use crate::ipc::{self, IpcRequest, IpcResponse, MAX_FRAME_BYTES, WireError};
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

pub const NATIVE_HOST_FLAG: &str = "--native-messaging-host";

pub fn is_native_host_invocation(arguments: &[OsString]) -> bool {
    arguments
        .iter()
        .any(|argument| argument == NATIVE_HOST_FLAG)
        || (arguments.len() >= 2
            && Path::new(&arguments[0])
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json")))
}

#[derive(Debug, Eq, PartialEq)]
pub struct LaunchPlan {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

pub fn launch_plan(executable: &Path) -> LaunchPlan {
    LaunchPlan {
        program: executable.to_path_buf(),
        args: Vec::new(),
    }
}

pub fn should_start_gui(connect_error: &io::Error) -> bool {
    if matches!(
        connect_error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut
    ) {
        return true;
    }
    #[cfg(windows)]
    {
        matches!(
            connect_error.raw_os_error(),
            Some(code)
                if code == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND as i32
                    || code == windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32
                    || code == windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE as i32
        )
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn launch_gui(executable: &Path) -> io::Result<Child> {
    let plan = launch_plan(executable);
    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command.spawn()
}

pub fn run_native_host() -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    run_native_host_io(stdin.lock(), stdout.lock()).map_err(|error| error.to_string())
}

fn run_native_host_io<R: Read, W: Write>(mut input: R, mut output: W) -> io::Result<()> {
    loop {
        let body = match ipc::read_frame(&mut input, MAX_FRAME_BYTES) {
            Ok(body) => body,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(_error) => {
                let response = error_response(
                    "native-host",
                    "invalid_frame",
                    "Native Messaging frame 無效",
                );
                write_response(&mut output, &response)?;
                return Ok(());
            }
        };
        let response = process_message(&body);
        write_response(&mut output, &response)?;
    }
}

fn process_message(body: &[u8]) -> IpcResponse {
    let request_id = request_id_from_body(body);
    match serde_json::from_slice::<IpcRequest>(body) {
        Ok(request) => match forward_request(&request) {
            Ok(response) => response,
            Err(error) => error_response(
                &request_id,
                "bridge_unavailable",
                &redacted_error_message(&error),
            ),
        },
        Err(_) => error_response(&request_id, "invalid_json", "Native Messaging JSON 無效"),
    }
}

fn forward_request(request: &IpcRequest) -> io::Result<IpcResponse> {
    let timeout = Duration::from_millis(500);
    match ipc::call_pipe(request, timeout) {
        Ok(response) => Ok(response),
        Err(error) if should_start_gui(&error) => {
            let executable = std::env::current_exe()?;
            let _child = launch_gui(&executable)?;
            ipc::call_pipe_with_retry(
                request,
                Duration::from_millis(100),
                Duration::from_millis(100),
                50,
            )
        }
        Err(error) => Err(error),
    }
}

fn write_response<W: Write>(output: &mut W, response: &IpcResponse) -> io::Result<()> {
    let body = serde_json::to_vec(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    ipc::write_frame(output, &body)
}

fn request_id_from_body(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "native-host".to_owned())
}

fn error_response(request_id: &str, code: &str, message: &str) -> IpcResponse {
    IpcResponse::Error {
        request_id: request_id.to_owned(),
        error: WireError {
            code: code.to_owned(),
            message: message.to_owned(),
        },
    }
}

fn redacted_error_message(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut => {
            "Curl Downloader GUI 未能連線".into()
        }
        _ => "Curl Downloader Native host 無法完成請求".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io,
        path::{Path, PathBuf},
    };

    #[test]
    fn native_host_starts_gui_only_for_pipe_connection_failure() {
        let not_found = io::Error::new(io::ErrorKind::NotFound, "pipe missing");
        let invalid_data = io::Error::new(io::ErrorKind::InvalidData, "bad response");
        assert!(should_start_gui(&not_found));
        assert!(!should_start_gui(&invalid_data));
    }

    #[test]
    fn detects_firefox_native_host_arguments_without_custom_flag() {
        let firefox_arguments = vec![
            OsString::from(r"C:\Users\test\curl_downloader.json"),
            OsString::from("curl-downloader@kinkeil.local"),
        ];
        assert!(is_native_host_invocation(&firefox_arguments));
        assert!(is_native_host_invocation(&[OsString::from(
            NATIVE_HOST_FLAG
        )]));
        assert!(!is_native_host_invocation(&[]));
        assert!(!is_native_host_invocation(&[
            OsString::from("--unexpected"),
            OsString::from("argument")
        ]));
    }

    #[test]
    fn native_host_launch_plan_has_no_request_arguments() {
        let plan = launch_plan(Path::new(
            r"C:\Program Files\CurlDownloader\CurlDownloader.exe",
        ));
        assert_eq!(
            plan.program,
            PathBuf::from(r"C:\Program Files\CurlDownloader\CurlDownloader.exe")
        );
        assert!(plan.args.is_empty());
    }

    #[test]
    fn malformed_json_produces_one_redacted_response_frame() {
        let mut input = Vec::new();
        crate::ipc::write_frame(
            &mut input,
            br#"{"type":"enqueue","request_id":"request-1","password":"secret"}"#,
        )
        .unwrap();
        let mut output = Vec::new();
        run_native_host_io(input.as_slice(), &mut output).unwrap();
        let body = crate::ipc::read_frame(&mut output.as_slice(), MAX_FRAME_BYTES).unwrap();
        let response: IpcResponse = serde_json::from_slice(&body).unwrap();
        let encoded = String::from_utf8(body).unwrap();
        assert!(!encoded.contains("secret"));
        assert!(matches!(response, IpcResponse::Error { .. }));
    }
}
