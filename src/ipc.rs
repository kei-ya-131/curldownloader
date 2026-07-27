use crate::model::{ProxyProtocol, ProxySettings};
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

use crate::model::{ConfiguredTask, EngineCommand};

pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const PIPE_NAME: &str = r"\\.\pipe\curl-downloader-v1";

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
}

impl IpcResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Error { request_id, .. }
            | Self::Pong { request_id, .. }
            | Self::Defaults { request_id, .. }
            | Self::Folder { request_id, .. }
            | Self::EnqueueResult { request_id, .. } => request_id,
        }
    }
}

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
            .spawn(move || run_windows_server(commands, last_download_dir, stop))
            .expect("無法啟動 Native Messaging Named Pipe 伺服器")
    }

    #[cfg(not(windows))]
    {
        let _ = (commands, last_download_dir, stop);
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
    stop: Arc<AtomicBool>,
) {
    use std::{
        fs::File,
        os::windows::io::{FromRawHandle, RawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE},
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{FILE_FLAGS_AND_ATTRIBUTES, PIPE_ACCESS_DUPLEX},
        System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
            PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        },
    };

    let pipe_name = pipe_name_wide();
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
                ptr::null::<SECURITY_ATTRIBUTES>(),
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
                let response = dispatch_request(request, &commands, &last_download_dir);
                if let Ok(body) = serde_json::to_vec(&response) {
                    let _ = write_frame(&mut stream, &body);
                }
            }
        }
        drop(stream);
    }
}

#[cfg(windows)]
fn dispatch_request(
    request: IpcRequest,
    commands: &Sender<EngineCommand>,
    last_download_dir: &Arc<Mutex<PathBuf>>,
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
fn pipe_name_wide() -> Vec<u16> {
    PIPE_NAME.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
