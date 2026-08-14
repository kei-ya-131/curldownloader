use crate::{
    controller::{ControllerCommand, SharedControllerState},
    model::{
        ConfiguredTask, EngineCommand, FileDecision, ProxyProtocol, ProxySettings, TaskId,
        TaskOrigin, TaskSnapshot, TaskStatus,
    },
    request_context::{self, SourceAuthorization, WireRequestContext},
    shell_foreground::{self, OpenTargetOutcome},
};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    io::{self, Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::Sender,
    },
    thread,
    time::{Duration, Instant},
};

pub const MAX_FRAME_BYTES: usize = 64 * 1024;
const MAX_PIPE_CLIENTS: usize = 32;
pub const PIPE_NAME: &str = r"\\.\pipe\curl-downloader-v1";
const TEST_SHUTDOWN_MANUAL_ENV: &str = "CURL_DOWNLOADER_TEST_SHUTDOWN_MANUAL";
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WireTaskSummary {
    pub task_id: TaskId,
    pub filename: String,
    pub origin: String,
    pub status: String,
    pub authorization: String,
    pub reauthorization_requested: bool,
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
        origin: wire_origin_name(task.origin).into(),
        status: wire_status_name(task.status).into(),
        authorization: wire_authorization_name(task.authorization).into(),
        reauthorization_requested: task.reauthorization_requested,
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
        TaskStatus::NeedsFirefoxAuthorization => "needs_firefox_authorization",
        TaskStatus::AwaitingFileDecision => "awaiting_file_decision",
        TaskStatus::Finalizing => "finalizing",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Unknown => "paused",
    }
}

fn wire_authorization_name(authorization: SourceAuthorization) -> &'static str {
    match authorization {
        SourceAuthorization::Public => "public",
        SourceAuthorization::Encrypted => "encrypted",
        SourceAuthorization::NeedsFirefox => "needs_firefox_authorization",
        SourceAuthorization::DecryptionFailed => "decryption_failed",
        SourceAuthorization::ProtectedCleared => "protected_cleared",
    }
}

fn wire_origin_name(origin: TaskOrigin) -> &'static str {
    match origin {
        TaskOrigin::Gui => "gui",
        TaskOrigin::Firefox => "firefox",
    }
}

const DEFAULT_REQUESTED_SEGMENTS: u8 = 4;

fn default_requested_segments() -> u8 {
    DEFAULT_REQUESTED_SEGMENTS
}

fn validate_requested_segments(value: u8) -> Result<u8, String> {
    if (1..=8).contains(&value) {
        Ok(value)
    } else {
        Err("下載線程數量必須介乎 1 至 8".into())
    }
}

#[derive(Deserialize, Serialize)]
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
    ShutdownManual {
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
    CancelTask {
        request_id: String,
        task_id: TaskId,
    },
    RefreshFirefoxAuthorization {
        request_id: String,
        task_id: TaskId,
        request_context: WireRequestContext,
    },
    Enqueue {
        request_id: String,
        url: String,
        filename: String,
        target_dir: String,
        #[serde(default = "default_requested_segments")]
        requested_segments: u8,
        proxy: WireProxy,
        #[serde(default)]
        request_context: Option<WireRequestContext>,
    },
    ResolveFileConflict {
        request_id: String,
        task_id: TaskId,
        decision: String,
    },
}

impl fmt::Debug for IpcRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Ping { .. } => "ping",
            Self::GetDefaults { .. } => "get_defaults",
            Self::PickFolder { .. } => "pick_folder",
            Self::ListTasks { .. } => "list_tasks",
            Self::ShowWindow { .. } => "show_window",
            Self::ShutdownManual { .. } => "shutdown_manual",
            Self::ShowTask { .. } => "show_task",
            Self::OpenFile { .. } => "open_file",
            Self::OpenFolder { .. } => "open_folder",
            Self::CancelTask { .. } => "cancel_task",
            Self::RefreshFirefoxAuthorization { .. } => "refresh_firefox_authorization",
            Self::Enqueue { .. } => "enqueue",
            Self::ResolveFileConflict { .. } => "resolve_file_conflict",
        };
        f.debug_struct("IpcRequest")
            .field("type", &kind)
            .field("request_id", &self.request_id())
            .finish()
    }
}

impl IpcRequest {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Ping { request_id }
            | Self::GetDefaults { request_id }
            | Self::PickFolder { request_id }
            | Self::ListTasks { request_id }
            | Self::ShowWindow { request_id }
            | Self::ShutdownManual { request_id }
            | Self::ShowTask { request_id, .. }
            | Self::OpenFile { request_id, .. }
            | Self::OpenFolder { request_id, .. }
            | Self::CancelTask { request_id, .. }
            | Self::RefreshFirefoxAuthorization { request_id, .. }
            | Self::Enqueue { request_id, .. }
            | Self::ResolveFileConflict { request_id, .. } => request_id,
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
        #[serde(default)]
        awaiting_file_decision: bool,
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

#[derive(Debug, Deserialize, Serialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
}

#[derive(Deserialize, Serialize)]
pub struct WireProxy {
    pub enabled: bool,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub password: String,
}

impl fmt::Debug for WireProxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireProxy")
            .field("enabled", &self.enabled)
            .field("protocol", &self.protocol)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
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

#[cfg(windows)]
fn read_named_pipe_frame(
    reader: &mut impl Read,
    handle: windows_sys::Win32::Foundation::HANDLE,
    max_len: usize,
    timeout: Duration,
) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    fn wait_for_bytes(
        handle: windows_sys::Win32::Foundation::HANDLE,
        required: u32,
        deadline: Instant,
    ) -> io::Result<()> {
        loop {
            let mut available = 0u32;
            let ok = unsafe {
                PeekNamedPipe(
                    handle,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if available >= required {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Native pipe frame read timed out",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    let deadline = Instant::now() + timeout;
    wait_for_bytes(handle, 4, deadline)?;
    let mut prefix = [0u8; 4];
    reader.read_exact(&mut prefix)?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > max_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame size is invalid",
        ));
    }
    wait_for_bytes(handle, length as u32, deadline)?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(body)
}

pub fn spawn_server(
    commands: Sender<EngineCommand>,
    last_download_dir: Arc<Mutex<PathBuf>>,
    state: SharedControllerState,
    controller: Sender<ControllerCommand>,
    stop: Arc<AtomicBool>,
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
            .spawn(move || run_windows_server(commands, last_download_dir, state, controller, stop))
            .expect("無法啟動 Native Messaging Named Pipe 伺服器")
    }

    #[cfg(not(windows))]
    {
        let _ = (commands, last_download_dir, state, controller, stop);
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
    call_pipe_with_retry_until(request, timeout, retry_delay, attempts, || true)
}

pub fn call_pipe_with_retry_until<F>(
    request: &IpcRequest,
    timeout: Duration,
    retry_delay: Duration,
    attempts: usize,
    mut should_continue: F,
) -> io::Result<IpcResponse>
where
    F: FnMut() -> bool,
{
    if attempts == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "至少需要嘗試一次 Named Pipe 連線",
        ));
    }

    let mut last_error = None;
    for attempt in 0..attempts {
        if !should_continue() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Curl Downloader 已由使用者關閉",
            ));
        }
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
    state: SharedControllerState,
    controller: Sender<ControllerCommand>,
    stop: Arc<AtomicBool>,
) {
    use std::{
        fs::File,
        os::windows::io::{AsRawHandle, FromRawHandle, RawHandle},
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
    let active_clients = Arc::new(AtomicUsize::new(0));
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

        if active_clients.load(Ordering::Acquire) >= MAX_PIPE_CLIENTS {
            unsafe {
                CloseHandle(handle);
            }
            continue;
        }
        active_clients.fetch_add(1, Ordering::AcqRel);

        let stream = unsafe { File::from_raw_handle(handle as RawHandle) };
        // Keep accepting new instances while a client is being authenticated
        // or is timing out on a partial frame.  A single slow client must not
        // monopolize the Native Messaging bridge.
        let client_commands = commands.clone();
        let client_last_download_dir = Arc::clone(&last_download_dir);
        let client_state = state.clone();
        let client_controller = controller.clone();
        let client_count = Arc::clone(&active_clients);
        if thread::Builder::new()
            .name("native-bridge-client".into())
            .spawn(move || {
                let mut stream = stream;
                let pipe = stream.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
                if let Ok(body) = read_named_pipe_frame(
                    &mut stream,
                    pipe,
                    MAX_FRAME_BYTES,
                    Duration::from_secs(5),
                ) {
                    if let Ok(request) = serde_json::from_slice::<IpcRequest>(&body) {
                        let trusted_client = named_pipe_client_is_trusted(pipe);
                        let response = dispatch_request(
                            request,
                            &client_commands,
                            &client_last_download_dir,
                            &client_state,
                            &client_controller,
                            trusted_client,
                        );
                        if let Ok(body) = serde_json::to_vec(&response) {
                            let _ = write_frame(&mut stream, &body);
                        }
                    }
                }
                client_count.fetch_sub(1, Ordering::AcqRel);
            })
            .is_err()
        {
            active_clients.fetch_sub(1, Ordering::AcqRel);
        }
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
        IpcRequest::ShowTask { task_id, .. } => Some(*task_id),
        _ => None,
    }
}

fn request_show_task(task_id: TaskId, controller: &Sender<ControllerCommand>) -> bool {
    controller
        .send(ControllerCommand::ShowTask { task_id })
        .is_ok()
}

fn dispatch_request(
    request: IpcRequest,
    commands: &Sender<EngineCommand>,
    last_download_dir: &Arc<Mutex<PathBuf>>,
    state: &SharedControllerState,
    controller: &Sender<ControllerCommand>,
    trusted_client: bool,
) -> IpcResponse {
    if !trusted_client {
        return match request {
            IpcRequest::Enqueue { request_id, .. } => enqueue_error(
                request_id,
                "unauthorized_client",
                "Native host client 未獲授權。",
            ),
            IpcRequest::ResolveFileConflict { request_id, .. }
            | IpcRequest::ShowWindow { request_id }
            | IpcRequest::ShowTask { request_id, .. }
            | IpcRequest::OpenFile { request_id, .. }
            | IpcRequest::OpenFolder { request_id, .. }
            | IpcRequest::CancelTask { request_id, .. }
            | IpcRequest::RefreshFirefoxAuthorization { request_id, .. }
            | IpcRequest::PickFolder { request_id }
            | IpcRequest::GetDefaults { request_id }
            | IpcRequest::ListTasks { request_id }
            | IpcRequest::ShutdownManual { request_id }
            | IpcRequest::Ping { request_id } => action_error(
                request_id,
                "unauthorized_client",
                "Native host client 未獲授權。",
            ),
        };
    }
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
            if controller.send(ControllerCommand::ShowWindow).is_err() {
                action_error(
                    request_id,
                    "gui_unavailable",
                    "Curl Downloader GUI 未能接收操作。",
                )
            } else {
                action_success(request_id)
            }
        }
        IpcRequest::ShutdownManual { request_id } => {
            if std::env::var_os(TEST_SHUTDOWN_MANUAL_ENV).is_none() {
                action_error(request_id, "unsupported", "測試關閉命令未啟用。")
            } else if controller.send(ControllerCommand::ShutdownManual).is_err() {
                action_error(
                    request_id,
                    "gui_unavailable",
                    "Curl Downloader GUI 未能接收操作。",
                )
            } else {
                action_success(request_id)
            }
        }
        IpcRequest::ListTasks { request_id } => IpcResponse::TaskList {
            request_id,
            tasks: build_task_summaries(&state.tasks()),
        },
        IpcRequest::ShowTask {
            request_id,
            task_id,
        } => {
            if !request_show_task(task_id, controller) {
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
        } => open_task_file(request_id, task_id, state),
        IpcRequest::OpenFolder {
            request_id,
            task_id,
        } => open_task_folder(request_id, task_id, state),
        IpcRequest::CancelTask {
            request_id,
            task_id,
        } => {
            let (response_tx, response_rx) = std::sync::mpsc::channel();
            if commands
                .send(EngineCommand::CancelWithResponse {
                    id: task_id,
                    response: response_tx,
                })
                .is_err()
            {
                return action_error(request_id, "engine_unavailable", "下載引擎未能接收操作。");
            }
            match response_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(())) => action_success(request_id),
                Ok(Err(message)) => action_error(request_id, "cancel_failed", &message),
                Err(_) => action_error(request_id, "engine_timeout", "下載引擎回應逾時。"),
            }
        }
        IpcRequest::RefreshFirefoxAuthorization {
            request_id,
            task_id,
            request_context,
        } => {
            let prepared = match request_context::prepare(request_context) {
                Ok(prepared) => prepared,
                Err(_) => {
                    return action_error(
                        request_id,
                        "invalid_request_context",
                        "Firefox 授權資料無效或超出安全限制。",
                    );
                }
            };
            let (response_tx, response_rx) = std::sync::mpsc::channel();
            if commands
                .send(EngineCommand::RefreshFirefoxAuthorization {
                    id: task_id,
                    request_context: prepared,
                    response: response_tx,
                })
                .is_err()
            {
                return action_error(request_id, "engine_unavailable", "下載引擎未能接收操作。");
            }
            match response_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(())) => action_success(request_id),
                Ok(Err(message)) => {
                    let code = if message.contains("來源已變更") {
                        "source_changed"
                    } else {
                        "reauthorization_failed"
                    };
                    action_error(request_id, code, &message)
                }
                Err(_) => action_error(request_id, "engine_timeout", "下載引擎回應逾時。"),
            }
        }
        IpcRequest::ResolveFileConflict {
            request_id,
            task_id,
            decision,
        } => {
            let decision = match decision.as_str() {
                "overwrite" => FileDecision::Overwrite,
                "cancel" => FileDecision::Cancel,
                _ => return action_error(request_id, "invalid_decision", "檔案衝突決定無效。"),
            };
            let (response_tx, response_rx) = std::sync::mpsc::channel();
            if commands
                .send(EngineCommand::ResolveFileConflict {
                    id: task_id,
                    decision,
                    response: response_tx,
                })
                .is_err()
            {
                return action_error(request_id, "engine_unavailable", "下載引擎未能接收操作。");
            }
            match response_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(())) => action_success(request_id),
                Ok(Err(message)) => action_error(request_id, "conflict_failed", &message),
                Err(_) => action_error(request_id, "engine_timeout", "下載引擎回應逾時。"),
            }
        }
        IpcRequest::Enqueue {
            request_id,
            url,
            filename,
            target_dir,
            requested_segments,
            proxy,
            request_context,
        } => {
            let requested_segments = match validate_requested_segments(requested_segments) {
                Ok(value) => value,
                Err(message) => return enqueue_error(request_id, "invalid_task", &message),
            };
            let proxy = match proxy.into_proxy_settings() {
                Ok(proxy) => proxy,
                Err(message) => {
                    return IpcResponse::EnqueueResult {
                        request_id,
                        ok: false,
                        task_id: None,
                        awaiting_file_decision: false,
                        error: Some(WireError {
                            code: "invalid_proxy".into(),
                            message,
                        }),
                    };
                }
            };
            let request_context = match request_context.map(request_context::prepare).transpose() {
                Ok(context) => context,
                Err(_) => {
                    return enqueue_error(
                        request_id,
                        "invalid_request_context",
                        "Firefox 授權資料無效或超出安全限制。",
                    );
                }
            };
            let target_dir = PathBuf::from(target_dir);
            let (response_tx, response_rx) = std::sync::mpsc::channel();
            if commands
                .send(EngineCommand::AddConfiguredWithOrigin {
                    task: ConfiguredTask {
                        url,
                        filename,
                        target_dir: target_dir.clone(),
                        requested_segments,
                        proxy,
                        request_id: Some(request_id.clone()),
                        request_context,
                    },
                    origin: TaskOrigin::Firefox,
                    response: response_tx,
                })
                .is_err()
            {
                return enqueue_error(request_id, "engine_unavailable", "下載引擎未能接收任務");
            }
            match response_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(acceptance)) => {
                    if let Ok(mut default_dir) = last_download_dir.lock() {
                        *default_dir = target_dir;
                    }
                    IpcResponse::EnqueueResult {
                        request_id,
                        ok: true,
                        task_id: Some(acceptance.task_id),
                        awaiting_file_decision: acceptance.awaiting_file_decision,
                        error: None,
                    }
                }
                Ok(Err(message)) => enqueue_error(request_id, "invalid_task", &message),
                Err(_) => enqueue_error(request_id, "engine_timeout", "下載引擎回應逾時"),
            }
        }
    }
}

#[cfg(test)]
fn dispatch_request_for_test(
    request: IpcRequest,
    state: &SharedControllerState,
    controller: &Sender<ControllerCommand>,
) -> IpcResponse {
    let (commands, _receiver) = std::sync::mpsc::channel();
    let last_download_dir = Arc::new(Mutex::new(PathBuf::from(r"C:\Downloads")));
    dispatch_request(
        request,
        &commands,
        &last_download_dir,
        state,
        controller,
        true,
    )
}

#[cfg(test)]
fn dispatch_untrusted_request_for_test(request: IpcRequest) -> IpcResponse {
    let (commands, _receiver) = std::sync::mpsc::channel();
    let state = SharedControllerState::new(crate::controller::LifecycleState::RunningHidden);
    let controller = std::sync::mpsc::channel().0;
    let last_download_dir = Arc::new(Mutex::new(PathBuf::from(r"C:\Downloads")));
    dispatch_request(
        request,
        &commands,
        &last_download_dir,
        &state,
        &controller,
        false,
    )
}
fn action_from_open_outcome(
    request_id: String,
    outcome: io::Result<OpenTargetOutcome>,
    open_failed_code: &'static str,
    open_failed_message: &'static str,
) -> IpcResponse {
    match outcome {
        Ok(OpenTargetOutcome::Focused) => action_success(request_id),
        Ok(OpenTargetOutcome::OpenedButNotFocused) => action_error(
            request_id,
            "target_not_foreground",
            "目標已開啟，但未能置前。",
        ),
        Err(_) => action_error(request_id, open_failed_code, open_failed_message),
    }
}
fn open_task_file(
    request_id: String,
    task_id: TaskId,
    state: &SharedControllerState,
) -> IpcResponse {
    let Some(task) = state.tasks().into_iter().find(|task| task.id == task_id) else {
        return action_error(request_id, "task_not_found", "找不到下載任務。");
    };
    if task.status != TaskStatus::Completed {
        return action_error(request_id, "file_unavailable", "下載尚未完成。");
    }
    let path = task.target_dir.join(&task.filename);
    if !path.is_file() {
        return action_error(request_id, "file_unavailable", "下載檔案不存在。");
    }
    action_from_open_outcome(
        request_id,
        shell_foreground::open_file_foreground(&path),
        "open_file_failed",
        "無法開啟下載檔案。",
    )
}

fn open_task_folder(
    request_id: String,
    task_id: TaskId,
    state: &SharedControllerState,
) -> IpcResponse {
    let Some(task) = state.tasks().into_iter().find(|task| task.id == task_id) else {
        return action_error(request_id, "task_not_found", "找不到下載任務。");
    };
    if !task.target_dir.is_dir() {
        return action_error(request_id, "folder_unavailable", "目標下載資料夾不存在。");
    }
    action_from_open_outcome(
        request_id,
        shell_foreground::open_folder_foreground(&task.target_dir),
        "open_folder_failed",
        "無法開啟目標下載資料夾。",
    )
}

fn action_success(request_id: String) -> IpcResponse {
    IpcResponse::ActionResult {
        request_id,
        ok: true,
        error: None,
    }
}

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
fn enqueue_error(request_id: String, code: &str, message: &str) -> IpcResponse {
    IpcResponse::EnqueueResult {
        request_id,
        ok: false,
        task_id: None,
        awaiting_file_decision: false,
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

    if !named_pipe_server_is_trusted(handle) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Named Pipe 伺服器未獲授權",
        ));
    }

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
fn named_pipe_server_is_trusted(pipe: windows_sys::Win32::Foundation::HANDLE) -> bool {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            Pipes::GetNamedPipeServerProcessId,
            Threading::{
                GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        },
    };

    // A pipe name alone is not an authentication mechanism: another process
    // in the same user session can create the name before the GUI does.  Bind
    // the client to the actual GUI process and its single-instance mutex.
    // The debug-only test override is restricted to the test pipe suffix and
    // is never compiled into the release artifact.
    let test_override = cfg!(debug_assertions)
        && std::env::var_os("CURL_DOWNLOADER_PIPE_SUFFIX")
            .is_some_and(|suffix| suffix.to_string_lossy().starts_with("test-"));
    if !test_override && !crate::single_instance::is_running() {
        return false;
    }
    let mut server_pid = 0u32;
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut server_pid) } == 0
        || server_pid == 0
        || (server_pid == unsafe { GetCurrentProcessId() } && !test_override)
    {
        return false;
    }
    let process: HANDLE = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, server_pid) };
    if process.is_null() {
        return false;
    }
    let trusted = (|| {
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) } == 0
        {
            return false;
        }
        buffer.truncate(length as usize);
        let server_path = std::ffi::OsString::from_wide(&buffer);
        let Ok(client_path) = std::env::current_exe() else {
            return false;
        };
        if !server_path
            .to_string_lossy()
            .eq_ignore_ascii_case(&client_path.to_string_lossy())
        {
            return false;
        }
        process_user_sid(process) == current_process_user_sid()
    })();
    unsafe {
        CloseHandle(process);
    }
    trusted
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

fn named_pipe_security_descriptor_sddl(user_sid: &str) -> String {
    format!("D:(A;;GA;;;{user_sid})")
}

#[cfg(windows)]
fn named_pipe_client_is_trusted(pipe: windows_sys::Win32::Foundation::HANDLE) -> bool {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            Pipes::GetNamedPipeClientProcessId,
            Threading::{
                GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        },
    };

    let mut client_pid = 0u32;
    if unsafe { GetNamedPipeClientProcessId(pipe, &mut client_pid) } == 0 || client_pid == 0 {
        return false;
    }
    let process: HANDLE = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, client_pid) };
    if process.is_null() {
        return false;
    }
    let trusted = (|| {
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) } == 0
        {
            return false;
        }
        buffer.truncate(length as usize);
        let client_path = std::ffi::OsString::from_wide(&buffer);
        let Ok(server_path) = std::env::current_exe() else {
            return false;
        };
        if !client_path
            .to_string_lossy()
            .eq_ignore_ascii_case(&server_path.to_string_lossy())
        {
            return false;
        }
        if process_user_sid(process) != current_process_user_sid() {
            return false;
        }
        #[cfg(feature = "smoke-test-native-auth")]
        if std::env::var_os("CURL_DOWNLOADER_TEST_NATIVE_CLIENT").is_some() {
            return true;
        }
        if client_pid == unsafe { GetCurrentProcessId() } {
            return true;
        }
        parent_process_is_firefox(client_pid)
    })();
    unsafe {
        CloseHandle(process);
    }
    trusted
}

#[cfg(windows)]
fn current_process_user_sid() -> Option<String> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    process_user_sid(unsafe { GetCurrentProcess() })
}

#[cfg(windows)]
fn process_user_sid(process: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, GetLastError, LocalFree},
        Security::Authorization::ConvertSidToStringSidW,
        Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser},
        System::Threading::OpenProcessToken,
    };

    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let result = (|| {
        let mut required = 0u32;
        let _ = unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut required) };
        if required == 0 && unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
        let mut bytes = vec![0u8; required as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                bytes.as_mut_ptr().cast(),
                bytes.len() as u32,
                &mut required,
            )
        } == 0
        {
            return None;
        }
        let user = unsafe { &*bytes.as_ptr().cast::<TOKEN_USER>() };
        let mut sid_string = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_string) } == 0 {
            return None;
        }
        let value = unsafe {
            let mut length = 0usize;
            while *sid_string.add(length) != 0 {
                length += 1;
            }
            OsString::from_wide(std::slice::from_raw_parts(sid_string, length))
                .to_string_lossy()
                .into_owned()
        };
        unsafe {
            let _ = LocalFree(sid_string.cast());
        }
        Some(value)
    })();
    unsafe {
        CloseHandle(token);
    }
    result
}

#[cfg(windows)]
fn parent_process_is_firefox(pid: u32) -> bool {
    use std::{mem::size_of, os::windows::ffi::OsStringExt};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return false;
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut parent_pid = None;
    let mut found = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while found {
        if entry.th32ProcessID == pid {
            parent_pid = Some(entry.th32ParentProcessID);
            break;
        }
        found = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }
    let Some(parent_pid) = parent_pid else {
        return false;
    };
    let parent = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, parent_pid) };
    if parent.is_null() {
        return false;
    }
    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;
    let ok =
        unsafe { QueryFullProcessImageNameW(parent, 0, buffer.as_mut_ptr(), &mut length) } != 0;
    unsafe {
        CloseHandle(parent);
    }
    if !ok {
        return false;
    }
    buffer.truncate(length as usize);
    let path = std::ffi::OsString::from_wide(&buffer);
    let Some(name) = std::path::Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return false;
    };
    if !name.eq_ignore_ascii_case("firefox.exe") {
        return false;
    }
    verify_signed_windows_binary(std::path::Path::new(&path))
}

#[cfg(windows)]
fn verify_signed_windows_binary(path: &std::path::Path) -> bool {
    use std::{mem::size_of, os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::Security::Cryptography::{
        CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW,
    };
    use windows_sys::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO, WTD_CHOICE_FILE,
        WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
        WTHelperGetProvCertFromChain, WTHelperGetProvSignerFromChain,
        WTHelperProvDataFromStateData, WinVerifyTrust,
    };

    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: wide_path.as_ptr(),
        hFile: ptr::null_mut(),
        pgKnownSubject: ptr::null_mut(),
    };
    let mut trust_data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: windows_sys::Win32::Security::WinTrust::WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let result = unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &mut action,
            (&mut trust_data as *mut WINTRUST_DATA).cast(),
        )
    };
    let mozilla_signer = if result == 0 && !trust_data.hWVTStateData.is_null() {
        let provider = unsafe { WTHelperProvDataFromStateData(trust_data.hWVTStateData) };
        if provider.is_null() {
            false
        } else {
            let signer = unsafe { WTHelperGetProvSignerFromChain(provider, 0, 0, 0) };
            if signer.is_null()
                || unsafe { (*signer).csCertChain } == 0
                || unsafe { (*signer).pasCertChain.is_null() }
            {
                false
            } else {
                let certificate = unsafe { WTHelperGetProvCertFromChain(signer, 0) };
                if certificate.is_null() || unsafe { (*certificate).pCert.is_null() } {
                    false
                } else {
                    let mut name = vec![0u16; 256];
                    let length = unsafe {
                        CertGetNameStringW(
                            (*certificate).pCert,
                            CERT_NAME_SIMPLE_DISPLAY_TYPE,
                            0,
                            ptr::null(),
                            name.as_mut_ptr(),
                            name.len() as u32,
                        )
                    };
                    if length <= 1 {
                        false
                    } else {
                        name.truncate(length.saturating_sub(1) as usize);
                        String::from_utf16_lossy(&name)
                            .trim()
                            .eq_ignore_ascii_case("mozilla corporation")
                    }
                }
            }
        }
    } else {
        false
    };
    trust_data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        let _ = WinVerifyTrust(
            ptr::null_mut(),
            &mut action,
            (&mut trust_data as *mut WINTRUST_DATA).cast(),
        );
    }
    result == 0 && mozilla_signer && firefox_version_metadata(path)
}

#[cfg(windows)]
fn firefox_version_metadata(path: &std::path::Path) -> bool {
    use std::{os::windows::ffi::OsStrExt, ptr, slice};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut ignored = 0u32;
    let size = unsafe { GetFileVersionInfoSizeW(wide_path.as_ptr(), &mut ignored) };
    if size == 0 {
        return false;
    }
    let mut data = vec![0u8; size as usize];
    if unsafe { GetFileVersionInfoW(wide_path.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return false;
    }

    fn query_string(data: &[u8], key: &str) -> Option<String> {
        let key_wide: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
        let mut value = ptr::null_mut();
        let mut length = 0u32;
        if unsafe {
            VerQueryValueW(
                data.as_ptr().cast(),
                key_wide.as_ptr(),
                &mut value,
                &mut length,
            )
        } == 0
            || value.is_null()
            || length == 0
        {
            return None;
        }
        let utf16 = unsafe { slice::from_raw_parts(value.cast::<u16>(), length as usize) };
        Some(
            String::from_utf16_lossy(utf16)
                .trim_end_matches('\0')
                .to_ascii_lowercase(),
        )
    }

    let translations = unsafe {
        let key_wide: Vec<u16> = "\\VarFileInfo\\Translation"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut value = ptr::null_mut();
        let mut length = 0u32;
        if VerQueryValueW(
            data.as_ptr().cast(),
            key_wide.as_ptr(),
            &mut value,
            &mut length,
        ) != 0
            && !value.is_null()
        {
            slice::from_raw_parts(value.cast::<u16>(), (length as usize) / 2)
                .chunks_exact(2)
                .map(|pair| (pair[0], pair[1]))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    };
    let translations = if translations.is_empty() {
        vec![(0x0409, 0x04b0)]
    } else {
        translations
    };

    translations.into_iter().any(|(language, code_page)| {
        let company_key = format!("\\StringFileInfo\\{language:04x}{code_page:04x}\\CompanyName");
        let product_key = format!("\\StringFileInfo\\{language:04x}{code_page:04x}\\ProductName");
        let company = query_string(&data, &company_key);
        let product = query_string(&data, &product_key);
        company.is_some_and(|value| value.contains("mozilla"))
            && product.is_some_and(|value| value.contains("firefox"))
    })
}

#[cfg(not(windows))]
fn named_pipe_client_is_trusted(_pipe: ()) -> bool {
    true
}

#[cfg(windows)]
fn named_pipe_security_attributes() -> io::Result<(
    windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
    NamedPipeSecurityDescriptor,
)> {
    use std::{mem::size_of, ptr};
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;

    let user_sid = current_process_user_sid().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "無法取得目前 Windows 使用者 SID",
        )
    })?;
    let sddl_text = if cfg!(debug_assertions)
        && std::env::var_os("CURL_DOWNLOADER_PIPE_SUFFIX")
            .is_some_and(|suffix| suffix.to_string_lossy().starts_with("test-"))
    {
        // The in-process integration test shares a pipe with the test runner;
        // keep this debug-only ACL broad while the PID/path/SID check still
        // authenticates the peer.  Release builds always use the per-user ACL.
        "D:(A;;GA;;;WD)".to_owned()
    } else {
        named_pipe_security_descriptor_sddl(&user_sid)
    };
    let sddl: Vec<u16> = sddl_text.encode_utf16().chain(std::iter::once(0)).collect();
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
    let suffix = std::env::var("CURL_DOWNLOADER_PIPE_SUFFIX")
        .ok()
        .filter(|suffix| {
            !suffix.is_empty()
                && suffix
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
        .map(|suffix| format!("-{suffix}"))
        .unwrap_or_default();
    let user_suffix = current_process_user_sid()
        .map(|sid| format!("-{sid}"))
        .unwrap_or_default();
    let name = format!("{PIPE_NAME}{suffix}{user_suffix}");
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_enqueue_without_segments_defaults_to_four() {
        let request: IpcRequest = serde_json::from_value(serde_json::json!({
            "type": "enqueue",
            "request_id": "legacy",
            "url": "https://example.test/file.bin",
            "filename": "file.bin",
            "target_dir": "C:\\Downloads",
            "proxy": WireProxy::direct()
        }))
        .unwrap();
        let IpcRequest::Enqueue {
            requested_segments, ..
        } = request
        else {
            panic!("expected enqueue request");
        };
        assert_eq!(requested_segments, 4);
    }

    #[test]
    fn invalid_request_context_has_a_stable_redacted_error() {
        let state = SharedControllerState::new(LifecycleState::RunningHidden);
        let (controller_tx, _controller_rx) = std::sync::mpsc::channel();
        let response = dispatch_request_for_test(
            IpcRequest::Enqueue {
                request_id: "invalid-context".into(),
                url: "https://example.test/file.bin".into(),
                filename: "file.bin".into(),
                target_dir: r"C:\Downloads".into(),
                requested_segments: 1,
                proxy: WireProxy::direct(),
                request_context: Some(WireRequestContext {
                    headers: vec![WireRequestHeader::new("X-Test", "bad\r\nvalue")],
                    source_page_url: Some("https://app.test/page".into()),
                    initial_url: "https://example.test/file.bin".into(),
                    final_url: "https://example.test/file.bin".into(),
                    incognito: false,
                    cookie_store_id: Some("firefox-default".into()),
                }),
            },
            &state,
            &controller_tx,
        );
        assert!(matches!(
            response,
            IpcResponse::EnqueueResult {
                ok: false,
                error: Some(WireError { code, message }),
                ..
            } if code == "invalid_request_context" && !message.contains("bad")
        ));
    }

    #[test]
    fn requested_segments_accepts_only_one_through_eight() {
        assert_eq!(validate_requested_segments(1), Ok(1));
        assert_eq!(validate_requested_segments(8), Ok(8));
        assert!(validate_requested_segments(0).is_err());
        assert!(validate_requested_segments(9).is_err());
    }
    #[test]
    fn pipe_retry_can_be_cancelled_before_opening_another_host_connection() {
        let request = IpcRequest::Ping {
            request_id: "manual-stop".into(),
        };
        let error = call_pipe_with_retry_until(
            &request,
            Duration::from_millis(1),
            Duration::from_millis(1),
            5,
            || false,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn named_pipe_security_descriptor_is_available_to_local_clients() {
        assert_eq!(
            named_pipe_security_descriptor_sddl("S-1-5-21-test"),
            "D:(A;;GA;;;S-1-5-21-test)"
        );
    }

    #[test]
    fn untrusted_pipe_clients_cannot_enqueue_tasks() {
        let response = dispatch_untrusted_request_for_test(IpcRequest::Enqueue {
            request_id: "untrusted".into(),
            url: "https://example.test/file.bin".into(),
            filename: "file.bin".into(),
            target_dir: r"C:\Downloads".into(),
            requested_segments: 1,
            proxy: WireProxy::direct(),
            request_context: None,
        });
        assert!(matches!(
            response,
            IpcResponse::EnqueueResult {
                ok: false,
                error: Some(WireError { code, .. }),
                ..
            } if code == "unauthorized_client"
        ));
    }

    use crate::request_context::WireRequestHeader;
    use crate::{
        controller::{ControllerCommand, LifecycleState, SharedControllerState},
        model::{
            CurlSource, DownloadTask, ProxyProtocol, ProxySnapshot, RangeSupport, TaskId,
            TaskSnapshot, TaskStatus,
        },
        shell_foreground::OpenTargetOutcome,
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
    fn only_task_cards_request_the_main_window_to_front() {
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
            None
        );
        assert_eq!(
            request_focus_task_id(&IpcRequest::OpenFolder {
                request_id: "3".into(),
                task_id: 9,
            }),
            None
        );
        assert_eq!(
            request_focus_task_id(&IpcRequest::ListTasks {
                request_id: "4".into(),
            }),
            None
        );
    }

    #[test]
    fn open_actions_do_not_restore_gui_before_shell_operation() {
        let (controller_tx, controller_rx) = std::sync::mpsc::channel();
        let state = SharedControllerState::new(LifecycleState::RunningHidden);
        state.replace_tasks(vec![test_snapshot(7, TaskStatus::Completed, 1, Some(2))]);

        let response = dispatch_request_for_test(
            IpcRequest::OpenFile {
                request_id: "open".into(),
                task_id: 7,
            },
            &state,
            &controller_tx,
        );
        assert!(matches!(
            response,
            IpcResponse::ActionResult { ok: false, .. }
        ));
        assert!(controller_rx.try_recv().is_err());
    }

    #[test]
    fn show_and_task_actions_route_to_the_background_controller() {
        let (controller_tx, controller_rx) = std::sync::mpsc::channel();
        let state = SharedControllerState::new(LifecycleState::RunningHidden);
        state.replace_tasks(vec![test_snapshot(7, TaskStatus::Completed, 1, Some(2))]);

        let show = dispatch_request_for_test(
            IpcRequest::ShowWindow {
                request_id: "show".into(),
            },
            &state,
            &controller_tx,
        );
        assert!(matches!(show, IpcResponse::ActionResult { ok: true, .. }));
        assert!(matches!(
            controller_rx.recv().unwrap(),
            ControllerCommand::ShowWindow
        ));

        let task = dispatch_request_for_test(
            IpcRequest::ShowTask {
                request_id: "task".into(),
                task_id: 7,
            },
            &state,
            &controller_tx,
        );
        assert!(matches!(task, IpcResponse::ActionResult { ok: true, .. }));
        assert!(matches!(
            controller_rx.recv().unwrap(),
            ControllerCommand::ShowTask { task_id: 7 }
        ));
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
    fn opened_but_not_foreground_has_stable_wire_error() {
        let response = action_from_open_outcome(
            "7".into(),
            Ok(OpenTargetOutcome::OpenedButNotFocused),
            "open_file_failed",
            "無法開啟下載檔案。",
        );
        let json = serde_json::to_value(response).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["code"], "target_not_foreground");
        assert_eq!(json["error"]["message"], "目標已開啟，但未能置前。");
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
            authorization: SourceAuthorization::Public,
            reauthorization_requested: false,
            origin: TaskOrigin::Gui,
            requested_segments: 4,
            actual_segments: 1,
            segments: Vec::new(),
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
