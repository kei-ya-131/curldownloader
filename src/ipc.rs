use crate::model::{
    ConfiguredTask, EngineCommand, ProxyProtocol, ProxySettings, TaskId, TaskSnapshot, TaskStatus,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread,
    time::{Duration, Instant},
};

pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const PIPE_NAME: &str = r"\\.\pipe\curl-downloader-v1";
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WireTaskSummary {
    pub task_id: TaskId,
    pub filename: String,
    pub status: String,
    pub downloaded: u64,
    pub total_size: Option<u64>,
    pub current_bps: f64,
    pub average_bps: f64,
    pub eta_seconds: Option<u64>,
    pub target_dir: String,
    pub file_available: bool,
    pub folder_available: bool,
}

pub fn build_task_summaries(tasks: &[TaskSnapshot]) -> Vec<WireTaskSummary> {
    let mut summaries = tasks
        .iter()
        .filter(|task| !matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled))
        .map(task_summary)
        .collect::<Vec<_>>();
    let mut completed = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .collect::<Vec<_>>();
    completed.sort_by(|left, right| {
        let left_key = (
            left.completed_unix_ms.unwrap_or(left.created_unix_ms),
            left.id,
        );
        let right_key = (
            right.completed_unix_ms.unwrap_or(right.created_unix_ms),
            right.id,
        );
        right_key.cmp(&left_key)
    });
    summaries.extend(completed.into_iter().take(10).map(task_summary));
    summaries
}

fn task_summary(task: &TaskSnapshot) -> WireTaskSummary {
    let target = task.target_dir.join(&task.filename);
    WireTaskSummary {
        task_id: task.id,
        filename: task.filename.clone(),
        status: wire_status_name(task.status).into(),
        downloaded: task.downloaded,
        total_size: task.total_size,
        current_bps: task.current_bps,
        average_bps: task.average_bps,
        eta_seconds: task.eta_seconds,
        target_dir: task.target_dir.to_string_lossy().into_owned(),
        file_available: task.status == TaskStatus::Completed && target.is_file(),
        folder_available: task.target_dir.is_dir(),
    }
}

fn wire_status_name(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Probing => "probing",
        TaskStatus::Downloading => "downloading",
        TaskStatus::Pausing => "pausing",
        TaskStatus::Paused => "paused",
        TaskStatus::NeedsProxyPassword => "needs_proxy_password",
        TaskStatus::Finalizing => "finalizing",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcRequest {
    Ping {
        request_id: String,
    },
    GetDefaults {
        request_id: String,
    },
    PickFolder {
        request_id: String,
    },
    ListTasks {
        request_id: String,
    },
    ShowWindow {
        request_id: String,
    },
    ShowTask {
        request_id: String,
        task_id: TaskId,
    },
    OpenFile {
        request_id: String,
        task_id: TaskId,
    },
    OpenFolder {
        request_id: String,
        task_id: TaskId,
    },
    Enqueue {
        request_id: String,
        url: String,
        filename: String,
        target_dir: String,
        proxy: WireProxy,
    },
}

impl IpcRequest {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Ping { request_id }
            | Self::GetDefaults { request_id }
            | Self::PickFolder { request_id }
            | Self::ListTasks { request_id }
            | Self::ShowWindow { request_id }
            | Self::ShowTask { request_id, .. }
            | Self::OpenFile { request_id, .. }
            | Self::OpenFolder { request_id, .. }
            | Self::Enqueue { request_id, .. } => request_id,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcResponse {
    Error {
        request_id: String,
        error: WireError,
    },
    Pong {
        request_id: String,
        ok: bool,
    },
    Defaults {
        request_id: String,
        target_dir: String,
    },
    Folder {
        request_id: String,
        ok: bool,
        target_dir: Option<String>,
        error: Option<WireError>,
    },
    EnqueueResult {
        request_id: String,
        ok: bool,
        task_id: Option<u64>,
        error: Option<WireError>,
    },
    TaskList {
        request_id: String,
        tasks: Vec<WireTaskSummary>,
    },
    ActionResult {
        request_id: String,
        ok: bool,
        error: Option<WireError>,
    },
}

impl IpcResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Error { request_id, .. }
            | Self::Pong { request_id, .. }
            | Self::Defaults { request_id, .. }
            | Self::Folder { request_id, .. }
            | Self::EnqueueResult { request_id, .. }
            | Self::TaskList { request_id, .. }
            | Self::ActionResult { request_id, .. } => request_id,
        }
    }
}

#[derive(Debug)]
pub enum UiCommand {
    ShowWindow,
    ShowTask { task_id: TaskId },
}

pub type SharedSnapshots = Arc<Mutex<Vec<TaskSnapshot>>>;
#[derive(Debug, Deserialize, Serialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WireProxy {
    pub enabled: bool,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub password: String,
}

impl WireProxy {
    pub fn direct() -> Self {
        Self {
            enabled: false,
            protocol: "http".into(),
            host: String::new(),
            port: 8080,
            username: String::new(),
            password: String::new(),
        }
    }

    pub fn into_proxy_settings(self) -> Result<ProxySettings, String> {
        let protocol = match self.protocol.as_str() {
            "http" => ProxyProtocol::Http,
            "https" => ProxyProtocol::Https,
            "socks5" => ProxyProtocol::Socks5,
            "socks5h" => ProxyProtocol::Socks5h,
            _ => return Err("Proxy 類型無效".into()),
        };
        let mut proxy = ProxySettings {
            enabled: self.enabled,
            protocol,
            host: self.host,
            port: self.port,
            username: self.username,
            ..ProxySettings::default()
        };
        proxy.set_password(self.password)?;
        proxy.validate()?;
        Ok(proxy)
    }
}

pub fn read_frame<R: Read>(reader: &mut R, max_len: usize) -> io::Result<Vec<u8>> {
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix)?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > max_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame size is invalid",
        ));
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    Ok(body)
}

pub fn write_frame<W: Write>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    let length = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "IPC frame is too large"))?;
    if length == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "IPC frame cannot be empty",
        ));
    }
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(body)?;
    writer.flush()
}

pub fn spawn_server(
    commands: Sender<EngineCommand>,
    last_download_dir: Arc<Mutex<PathBuf>>,
    snapshots: SharedSnapshots,
    ui_commands: Sender<UiCommand>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    spawn_server_with_repaint(
        commands,
        last_download_dir,
        snapshots,
        ui_commands,
        stop,
        Arc::new(|| {}),
    )
}

pub fn spawn_server_with_repaint(
    commands: Sender<EngineCommand>,
    last_download_dir: Arc<Mutex<PathBuf>>,
    snapshots: SharedSnapshots,
    ui_commands: Sender<UiCommand>,
    stop: Arc<AtomicBool>,
    repaint: Arc<dyn Fn() + Send + Sync>,
) -> thread::JoinHandle<()> {
    #[cfg(windows)]
    {
        let waker_stop = Arc::clone(&stop);
        let _ = thread::Builder::new()
            .name("native-bridge-stop-waker".into())
            .spawn(move || {
                while !waker_stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(25));
                }
                let request = IpcRequest::Ping {
                    request_id: "native-bridge-stop".into(),
                };
                let _ = call_pipe(&request, Duration::from_millis(250));
            });

        thread::Builder::new()
            .name("native-bridge-pipe".into())
            .spawn(move || {
                run_windows_server(
                    commands,
                    last_download_dir,
                    snapshots,
                    ui_commands,
                    stop,
                    repaint,
                )
            })
            .expect("無法啟動 Native Messaging Named Pipe 伺服器")
    }

    #[cfg(not(windows))]
    {
        let _ = (
            commands,
            last_download_dir,
            snapshots,
            ui_commands,
            stop,
            repaint,
        );
        thread::Builder::new()
            .name("native-bridge-pipe".into())
            .spawn(|| {})
            .expect("無法啟動 Native Messaging 佔位伺服器")
    }
}

pub fn call_pipe(request: &IpcRequest, timeout: Duration) -> io::Result<IpcResponse> {
    #[cfg(windows)]
    {
        call_windows_pipe(request, timeout)
    }

    #[cfg(not(windows))]
    {
        let _ = (request, timeout);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Named Pipe 只支援 Windows",
        ))
    }
}

pub fn call_pipe_with_retry(
    request: &IpcRequest,
    timeout: Duration,
    retry_delay: Duration,
    attempts: usize,
) -> io::Result<IpcResponse> {
    if attempts == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "至少需要嘗試一次 Named Pipe 連線",
        ));
    }

    let mut last_error = None;
    for attempt in 0..attempts {
        match call_pipe(request, timeout) {
            Ok(response) => return Ok(response),
            Err(error) if is_pipe_connection_error(&error) && attempt + 1 < attempts => {
                last_error = Some(error);
                thread::sleep(retry_delay);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.expect("Named Pipe 重試流程必須保留最後錯誤"))
}

fn is_pipe_connection_error(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(
            error.raw_os_error(),
            Some(code)
                if code == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND as i32
                    || code == windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32
                    || code == windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32
        )
    }

    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}

#[cfg(windows)]
fn run_windows_server(
    commands: Sender<EngineCommand>,
    last_download_dir: Arc<Mutex<PathBuf>>,
    snapshots: SharedSnapshots,
    ui_commands: Sender<UiCommand>,
    stop: Arc<AtomicBool>,
    repaint: Arc<dyn Fn() + Send + Sync>,
) {
    use std::{
        fs::File,
        os::windows::io::{FromRawHandle, RawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{FILE_FLAGS_AND_ATTRIBUTES, PIPE_ACCESS_DUPLEX},
        System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
            PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        },
    };

    let pipe_name = pipe_name_wide();
    let (security_attributes, _security_descriptor) = match named_pipe_security_attributes() {
        Ok(value) => value,
        Err(_) => return,
    };
    while !stop.load(Ordering::Acquire) {
        let handle = unsafe {
            CreateNamedPipeW(
                pipe_name.as_ptr(),
                PIPE_ACCESS_DUPLEX as FILE_FLAGS_AND_ATTRIBUTES,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                MAX_FRAME_BYTES as u32,
                MAX_FRAME_BYTES as u32,
                1_000,
                &security_attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            if stop.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(50));
            continue;
        }

        let connected = unsafe { ConnectNamedPipe(handle, ptr::null_mut()) != 0 };
        if !connected {
            let error = unsafe { GetLastError() };
            if error != ERROR_PIPE_CONNECTED {
                unsafe {
                    CloseHandle(handle);
                }
                if stop.load(Ordering::Acquire) {
                    break;
                }
                continue;
            }
        }

        let mut stream = unsafe { File::from_raw_handle(handle as RawHandle) };
        if let Ok(body) = read_frame(&mut stream, MAX_FRAME_BYTES) {
            if let Ok(request) = serde_json::from_slice::<IpcRequest>(&body) {
                let response = dispatch_request(
                    request,
                    &commands,
                    &last_download_dir,
                    &snapshots,
                    &ui_commands,
                    &repaint,
                );
                if let Ok(body) = serde_json::to_vec(&response) {
                    let _ = write_frame(&mut stream, &body);
                }
            }
        }
        drop(stream);
    }
}

pub fn show_window_request() -> IpcRequest {
    IpcRequest::ShowWindow {
        request_id: "gui-launch".into(),
    }
}

#[cfg(test)]
fn request_focus_task_id(request: &IpcRequest) -> Option<TaskId> {
    match request {
        IpcRequest::ShowTask { task_id, .. }
        | IpcRequest::OpenFile { task_id, .. }
        | IpcRequest::OpenFolder { task_id, .. } => Some(*task_id),
        _ => None,
    }
}

#[cfg(windows)]
fn focus_task_window(
    task_id: TaskId,
    snapshots: &SharedSnapshots,
    ui_commands: &Sender<UiCommand>,
    repaint: &Arc<dyn Fn() + Send + Sync>,
) -> bool {
    if !task_exists(snapshots, task_id) {
        return false;
    }
    if ui_commands.send(UiCommand::ShowTask { task_id }).is_err() {
        return false;
    }
    repaint();
    true
}

#[cfg(windows)]
fn dispatch_request(
    request: IpcRequest,
    commands: &Sender<EngineCommand>,
    last_download_dir: &Arc<Mutex<PathBuf>>,
    snapshots: &SharedSnapshots,
    ui_commands: &Sender<UiCommand>,
    repaint: &Arc<dyn Fn() + Send + Sync>,
) -> IpcResponse {
    match request {
        IpcRequest::Ping { request_id } => IpcResponse::Pong {
            request_id,
            ok: true,
        },
        IpcRequest::GetDefaults { request_id } => IpcResponse::Defaults {
            request_id,
            target_dir: last_download_dir
                .lock()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
        },
        IpcRequest::PickFolder { request_id } => {
            let current_dir = last_download_dir
                .lock()
                .map(|path| path.clone())
                .unwrap_or_default();
            let folder = rfd::FileDialog::new()
                .set_directory(current_dir)
                .pick_folder();
            IpcResponse::Folder {
                request_id,
                ok: folder.is_some(),
                target_dir: folder.map(|path| path.to_string_lossy().into_owned()),
                error: None,
            }
        }
        IpcRequest::ShowWindow { request_id } => {
            if ui_commands.send(UiCommand::ShowWindow).is_err() {
                action_error(
                    request_id,
                    "gui_unavailable",
                    "Curl Downloader GUI 未能接收操作。",
                )
            } else {
                repaint();
                action_success(request_id)
            }
        }
        IpcRequest::ListTasks { request_id } => IpcResponse::TaskList {
            request_id,
            tasks: snapshots
                .lock()
                .map(|tasks| build_task_summaries(&tasks))
                .unwrap_or_default(),
        },
        IpcRequest::ShowTask {
            request_id,
            task_id,
        } => {
            if !task_exists(snapshots, task_id) {
                action_error(request_id, "task_not_found", "找不到下載任務。")
            } else if !focus_task_window(task_id, snapshots, ui_commands, repaint) {
                action_error(
                    request_id,
                    "gui_unavailable",
                    "Curl Downloader GUI 未能接收操作。",
                )
            } else {
                action_success(request_id)
            }
        }
        IpcRequest::OpenFile {
            request_id,
            task_id,
        } => {
            let _ = focus_task_window(task_id, snapshots, ui_commands, repaint);
            open_task_file(request_id, task_id, snapshots)
        }
        IpcRequest::OpenFolder {
            request_id,
            task_id,
        } => {
            let _ = focus_task_window(task_id, snapshots, ui_commands, repaint);
            open_task_folder(request_id, task_id, snapshots)
        }
        IpcRequest::Enqueue {
            request_id,
            url,
            filename,
            target_dir,
            proxy,
        } => {
            let proxy = match proxy.into_proxy_settings() {
                Ok(proxy) => proxy,
                Err(message) => {
                    return IpcResponse::EnqueueResult {
                        request_id,
                        ok: false,
                        task_id: None,
                        error: Some(WireError {
                            code: "invalid_proxy".into(),
                            message,
                        }),
                    };
                }
            };
            let target_dir = PathBuf::from(target_dir);
            let (response_tx, response_rx) = std::sync::mpsc::channel();
            if commands
                .send(EngineCommand::AddConfigured {
                    task: ConfiguredTask {
                        url,
                        filename,
                        target_dir: target_dir.clone(),
                        requested_segments: 4,
                        proxy,
                    },
                    response: response_tx,
                })
                .is_err()
            {
                return enqueue_error(request_id, "engine_unavailable", "下載引擎未能接收任務");
            }
            match response_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(task_id)) => {
                    if let Ok(mut default_dir) = last_download_dir.lock() {
                        *default_dir = target_dir.clone();
                    }
                    IpcResponse::EnqueueResult {
                        request_id,
                        ok: true,
                        task_id: Some(task_id),
                        error: None,
                    }
                }
                Ok(Err(message)) => enqueue_error(request_id, "invalid_task", &message),
                Err(_) => enqueue_error(request_id, "engine_timeout", "下載引擎回應逾時"),
            }
        }
    }
}

#[cfg(windows)]
fn task_exists(snapshots: &SharedSnapshots, task_id: TaskId) -> bool {
    snapshots
        .lock()
        .map(|tasks| tasks.iter().any(|task| task.id == task_id))
        .unwrap_or(false)
}

#[cfg(windows)]
fn open_task_file(request_id: String, task_id: TaskId, snapshots: &SharedSnapshots) -> IpcResponse {
    let Some(task) = snapshots
        .lock()
        .ok()
        .and_then(|tasks| tasks.iter().find(|task| task.id == task_id).cloned())
    else {
        return action_error(request_id, "task_not_found", "找不到下載任務。");
    };
    if task.status != TaskStatus::Completed {
        return action_error(request_id, "file_unavailable", "下載尚未完成。");
    }
    let path = task.target_dir.join(&task.filename);
    if !path.is_file() {
        return action_error(request_id, "file_unavailable", "下載檔案不存在。");
    }
    match std::process::Command::new("explorer.exe").arg(path).spawn() {
        Ok(_) => action_success(request_id),
        Err(_) => action_error(request_id, "open_file_failed", "無法開啟下載檔案。"),
    }
}

#[cfg(windows)]
fn open_task_folder(
    request_id: String,
    task_id: TaskId,
    snapshots: &SharedSnapshots,
) -> IpcResponse {
    let Some(task) = snapshots
        .lock()
        .ok()
        .and_then(|tasks| tasks.iter().find(|task| task.id == task_id).cloned())
    else {
        return action_error(request_id, "task_not_found", "找不到下載任務。");
    };
    if !task.target_dir.is_dir() {
        return action_error(request_id, "folder_unavailable", "目標下載資料夾不存在。");
    }
    match std::process::Command::new("explorer.exe")
        .arg(task.target_dir)
        .spawn()
    {
        Ok(_) => action_success(request_id),
        Err(_) => action_error(request_id, "open_folder_failed", "無法開啟目標下載資料夾。"),
    }
}

#[cfg(windows)]
fn action_success(request_id: String) -> IpcResponse {
    IpcResponse::ActionResult {
        request_id,
        ok: true,
        error: None,
    }
}

#[cfg(windows)]
fn action_error(request_id: String, code: &str, message: &str) -> IpcResponse {
    IpcResponse::ActionResult {
        request_id,
        ok: false,
        error: Some(WireError {
            code: code.into(),
            message: message.into(),
        }),
    }
}
#[cfg(windows)]
fn enqueue_error(request_id: String, code: &str, message: &str) -> IpcResponse {
    IpcResponse::EnqueueResult {
        request_id,
        ok: false,
        task_id: None,
        error: Some(WireError {
            code: code.into(),
            message: message.into(),
        }),
    }
}

#[cfg(windows)]
fn call_windows_pipe(request: &IpcRequest, timeout: Duration) -> io::Result<IpcResponse> {
    use std::{
        fs::File,
        os::windows::io::{FromRawHandle, RawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{CreateFileW, OPEN_EXISTING},
    };

    let name = pipe_name_wide();
    let open_deadline = Instant::now() + timeout;
    let handle = loop {
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            break handle;
        }
        let error = io::Error::from_raw_os_error(unsafe { GetLastError() } as i32);
        if !is_pipe_connection_error(&error) || Instant::now() >= open_deadline {
            return Err(error);
        }
        thread::sleep(Duration::from_millis(10));
    };

    let mut stream = unsafe { File::from_raw_handle(handle as RawHandle) };
    let body = serde_json::to_vec(request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    write_frame(&mut stream, &body)?;

    let body = read_frame(&mut stream, MAX_FRAME_BYTES)?;
    let response = serde_json::from_slice::<IpcResponse>(&body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if response.request_id() != request.request_id() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Named Pipe 回應 request_id 不一致",
        ));
    }
    Ok(response)
}

#[cfg(windows)]
struct NamedPipeSecurityDescriptor {
    raw: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
}

#[cfg(windows)]
impl Drop for NamedPipeSecurityDescriptor {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                let _ = windows_sys::Win32::Foundation::LocalFree(self.raw);
            }
        }
    }
}

fn named_pipe_security_descriptor_sddl() -> &'static str {
    "D:(A;;GA;;;AU)"
}

#[cfg(windows)]
fn named_pipe_security_attributes() -> io::Result<(
    windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
    NamedPipeSecurityDescriptor,
)> {
    use std::{mem::size_of, ptr};
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;

    let sddl: Vec<u16> = named_pipe_security_descriptor_sddl()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut raw = ptr::null_mut();
    let mut descriptor_size = 0u32;
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut raw,
            &mut descriptor_size,
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }

    let descriptor = NamedPipeSecurityDescriptor { raw };
    let attributes = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: raw,
        bInheritHandle: 0,
    };
    Ok((attributes, descriptor))
}

#[cfg(windows)]
fn pipe_name_wide() -> Vec<u16> {
    PIPE_NAME.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_pipe_uses_an_authenticated_user_security_descriptor() {
        assert_eq!(named_pipe_security_descriptor_sddl(), "D:(A;;GA;;;AU)");
    }
    use crate::model::{
        CurlSource, DownloadTask, ProxyProtocol, ProxySnapshot, RangeSupport, TaskId, TaskSnapshot,
        TaskStatus,
    };

    #[test]
    fn frame_round_trip_handles_partial_reader() {
        let body = br#"{"type":"ping","request_id":"1"}"#;
        let mut encoded = Vec::new();
        write_frame(&mut encoded, body).unwrap();
        let mut reader = ChunkedReader::new(encoded, 2);
        assert_eq!(read_frame(&mut reader, MAX_FRAME_BYTES).unwrap(), body);
    }

    #[test]
    fn frame_reader_rejects_oversized_body_before_allocating_it() {
        let encoded = (u32::try_from(MAX_FRAME_BYTES + 1).unwrap()).to_le_bytes();
        let mut reader = std::io::Cursor::new(encoded);
        assert_eq!(
            read_frame(&mut reader, MAX_FRAME_BYTES).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn wire_proxy_conversion_uses_zeroizing_password_storage() {
        let wire = WireProxy {
            enabled: true,
            protocol: "socks5h".into(),
            host: "127.0.0.1".into(),
            port: 1080,
            username: "alice".into(),
            password: "secret".into(),
        };
        let proxy = wire.into_proxy_settings().unwrap();
        assert!(!format!("{proxy:?}").contains("secret"));
        assert_eq!(
            proxy.password.as_deref().map(String::as_str),
            Some("secret")
        );
    }

    #[test]
    fn file_and_folder_actions_request_the_main_window_to_front() {
        assert_eq!(
            request_focus_task_id(&IpcRequest::ShowTask {
                request_id: "1".into(),
                task_id: 7,
            }),
            Some(7)
        );
        assert_eq!(
            request_focus_task_id(&IpcRequest::OpenFile {
                request_id: "2".into(),
                task_id: 8,
            }),
            Some(8)
        );
        assert_eq!(
            request_focus_task_id(&IpcRequest::OpenFolder {
                request_id: "3".into(),
                task_id: 9,
            }),
            Some(9)
        );
        assert_eq!(
            request_focus_task_id(&IpcRequest::ListTasks {
                request_id: "4".into(),
            }),
            None
        );
    }

    #[test]
    fn duplicate_gui_launch_request_restores_main_window() {
        let request = show_window_request();
        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["type"], "show_window");
    }
    #[test]
    fn task_control_requests_use_stable_wire_names() {
        let list = serde_json::to_value(IpcRequest::ListTasks {
            request_id: "1".into(),
        })
        .unwrap();
        let show = serde_json::to_value(IpcRequest::ShowTask {
            request_id: "2".into(),
            task_id: 7,
        })
        .unwrap();
        let file = serde_json::to_value(IpcRequest::OpenFile {
            request_id: "3".into(),
            task_id: 7,
        })
        .unwrap();
        let folder = serde_json::to_value(IpcRequest::OpenFolder {
            request_id: "4".into(),
            task_id: 7,
        })
        .unwrap();
        assert_eq!(list["type"], "list_tasks");
        assert_eq!(show["type"], "show_task");
        assert_eq!(file["type"], "open_file");
        assert_eq!(folder["type"], "open_folder");
    }

    #[test]
    fn action_response_keeps_request_id_and_error_shape() {
        let response = IpcResponse::ActionResult {
            request_id: "9".into(),
            ok: false,
            error: Some(WireError {
                code: "task_not_found".into(),
                message: "找不到任務".into(),
            }),
        };
        let json = serde_json::to_value(response).unwrap();
        assert_eq!(json["type"], "action_result");
        assert_eq!(json["request_id"], "9");
        assert_eq!(json["error"]["code"], "task_not_found");
    }
    fn test_snapshot(
        id: TaskId,
        status: TaskStatus,
        created_unix_ms: u64,
        completed_unix_ms: Option<u64>,
    ) -> TaskSnapshot {
        TaskSnapshot {
            id,
            original_url: format!("https://example.test/{id}"),
            effective_url: None,
            filename: format!("file-{id}.bin"),
            target_dir: PathBuf::from(format!(r"C:\Downloads\{id}")),
            status,
            requested_segments: 4,
            actual_segments: 1,
            downloaded: id * 10,
            total_size: Some(1000),
            range_support: RangeSupport::Unknown,
            current_bps: 20.0,
            average_bps: 10.0,
            eta_seconds: Some(50),
            active_millis: 100,
            created_unix_ms,
            proxy: ProxySnapshot {
                enabled: false,
                protocol: ProxyProtocol::Http,
                host: String::new(),
                port: 8080,
                username: String::new(),
                requires_password: false,
            },
            error: None,
            curl_source: CurlSource::NotStarted,
            completed_unix_ms,
        }
    }

    #[test]
    fn task_summary_keeps_all_incomplete_tasks_and_only_ten_completed_tasks() {
        let mut tasks = vec![
            test_snapshot(1, TaskStatus::Downloading, 1, None),
            test_snapshot(2, TaskStatus::Paused, 2, None),
            test_snapshot(3, TaskStatus::Cancelled, 3, None),
        ];
        tasks.extend(
            (4..=15).map(|id| test_snapshot(id, TaskStatus::Completed, id, Some(id * 100))),
        );

        let summaries = build_task_summaries(&tasks);
        assert!(summaries.iter().any(|task| task.status == "downloading"));
        assert!(summaries.iter().any(|task| task.status == "paused"));
        assert!(!summaries.iter().any(|task| task.task_id == 3));
        let completed = summaries
            .iter()
            .filter(|task| task.status == "completed")
            .collect::<Vec<_>>();
        assert_eq!(completed.len(), 10);
        assert_eq!(completed.first().unwrap().task_id, 15);
        assert_eq!(completed.last().unwrap().task_id, 6);
    }

    #[test]
    fn task_summary_does_not_expose_url_or_proxy_fields() {
        let summary = build_task_summaries(&[test_snapshot(1, TaskStatus::Downloading, 1, None)])
            .pop()
            .unwrap();
        let json = serde_json::to_value(summary).unwrap();
        assert!(json.get("original_url").is_none());
        assert!(json.get("proxy").is_none());
        assert_eq!(json["status"], "downloading");
    }

    #[test]
    fn old_download_state_without_completion_time_remains_loadable() {
        let mut json = serde_json::to_value(DownloadTask::new(
            1,
            "https://example.test/file.bin",
            "file.bin".into(),
            PathBuf::from(r"C:\Downloads"),
        ))
        .unwrap();
        json.as_object_mut().unwrap().remove("completed_unix_ms");
        let task: DownloadTask = serde_json::from_value(json).unwrap();
        assert_eq!(task.completed_unix_ms, None);
    }
    struct ChunkedReader {
        bytes: Vec<u8>,
        offset: usize,
        chunk_size: usize,
    }

    impl ChunkedReader {
        fn new(bytes: Vec<u8>, chunk_size: usize) -> Self {
            Self {
                bytes,
                offset: 0,
                chunk_size,
            }
        }
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.offset == self.bytes.len() {
                return Ok(0);
            }
            let size = self
                .chunk_size
                .min(buffer.len())
                .min(self.bytes.len() - self.offset);
            buffer[..size].copy_from_slice(&self.bytes[self.offset..self.offset + size]);
            self.offset += size;
            Ok(size)
        }
    }
}
