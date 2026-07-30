use crate::{
    ipc::{self, IpcRequest, IpcResponse, MAX_FRAME_BYTES, WireError},
    session_shutdown, single_instance, startup_policy,
};
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
    pub creation_flags: u32,
}

#[cfg(windows)]
fn detached_gui_creation_flags() -> u32 {
    use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};
    CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW
}

#[cfg(not(windows))]
fn detached_gui_creation_flags() -> u32 {
    0
}

pub fn launch_plan(executable: &Path) -> LaunchPlan {
    LaunchPlan {
        program: executable.to_path_buf(),
        args: Vec::new(),
        creation_flags: detached_gui_creation_flags(),
    }
}

pub fn minimized_launch_plan(executable: &Path) -> LaunchPlan {
    LaunchPlan {
        program: executable.to_path_buf(),
        args: vec![OsString::from("--minimized")],
        creation_flags: detached_gui_creation_flags(),
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
                    || code == windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE as i32
        )
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn launch_gui(executable: &Path, minimized: bool) -> io::Result<Child> {
    let plan = if minimized {
        minimized_launch_plan(executable)
    } else {
        launch_plan(executable)
    };
    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(plan.creation_flags);
    }
    command.spawn()
}

pub fn run_native_host() -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    run_native_host_io_with_hook(stdin.lock(), stdout.lock(), |_| {
        let _ = session_shutdown::spawn_native_exit_monitor();
    })
    .map_err(|error| error.to_string())
}

#[cfg(test)]
fn run_native_host_io<R: Read, W: Write>(input: R, output: W) -> io::Result<()> {
    run_native_host_io_with_hook(input, output, |_| {})
}

fn run_native_host_io_with_hook<R: Read, W: Write, F: FnMut(&IpcResponse)>(
    input: R,
    output: W,
    after_response: F,
) -> io::Result<()> {
    run_native_host_io_with_processor(input, output, process_message, after_response)
}

fn run_native_host_io_with_processor<
    R: Read,
    W: Write,
    P: FnMut(&[u8]) -> IpcResponse,
    F: FnMut(&IpcResponse),
>(
    mut input: R,
    mut output: W,
    mut process: P,
    mut after_response: F,
) -> io::Result<()> {
    let mut hook_called = false;
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
        let response = process(&body);
        write_response(&mut output, &response)?;
        let manually_stopped = matches!(
            &response,
            IpcResponse::Error { error, .. } if error.code == "manually_stopped"
        );
        if !hook_called && !manually_stopped {
            after_response(&response);
            hook_called = true;
        }
    }
}

fn native_start_policy(body: &[u8]) -> startup_policy::NativeStartPolicy {
    serde_json::from_slice(body).unwrap_or_default()
}

fn denied_start_error_kind(stopped_at: Option<u64>) -> io::ErrorKind {
    if stopped_at.is_some() {
        io::ErrorKind::Interrupted
    } else {
        io::ErrorKind::WouldBlock
    }
}
fn forward_error_code(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::Interrupted => "manually_stopped",
        io::ErrorKind::WouldBlock => "gui_not_running",
        _ => "bridge_unavailable",
    }
}

fn process_message(body: &[u8]) -> IpcResponse {
    let request_id = request_id_from_body(body);
    match serde_json::from_slice::<IpcRequest>(body) {
        Ok(request) => {
            let policy = native_start_policy(body);
            match forward_request(&request, policy) {
                Ok(response) => response,
                Err(error) => error_response(
                    &request_id,
                    forward_error_code(&error),
                    &redacted_error_message(&error),
                ),
            }
        }
        Err(_) => error_response(&request_id, "invalid_json", "Native Messaging JSON 無效"),
    }
}

fn forward_request(
    request: &IpcRequest,
    policy: startup_policy::NativeStartPolicy,
) -> io::Result<IpcResponse> {
    let timeout = Duration::from_millis(500);
    match ipc::call_pipe(request, timeout) {
        Ok(response) => Ok(response),
        Err(error) if should_start_gui(&error) => {
            let state_path = crate::storage::state_path()?;
            let stop_path = crate::storage::manual_stop_path(&state_path);
            let stopped_at = startup_policy::read_manual_stop(&stop_path)?;
            if !policy.permits_start(stopped_at) {
                return Err(io::Error::new(
                    denied_start_error_kind(stopped_at),
                    "GUI 啟動不獲允許",
                ));
            }
            startup_policy::clear_manual_stop(&stop_path)?;

            let _start_guard = if !single_instance::is_running() {
                let guard = single_instance::acquire_start_guard().map_err(io::Error::other)?;
                if guard.is_some() && !single_instance::is_running() {
                    let executable = std::env::current_exe()?;
                    let _child = launch_gui(&executable, true)?;
                }
                guard
            } else {
                None
            };
            ipc::call_pipe_with_retry_until(
                request,
                Duration::from_millis(100),
                Duration::from_millis(100),
                50,
                || match startup_policy::read_manual_stop(&stop_path) {
                    Ok(stopped_at) => policy.permits_start(stopped_at),
                    Err(_) => false,
                },
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
        io::ErrorKind::Interrupted => "Curl Downloader 已由使用者關閉".into(),
        io::ErrorKind::WouldBlock => "Curl Downloader GUI 尚未啟動".into(),
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

    #[cfg(windows)]
    #[test]
    fn does_not_restart_gui_when_named_pipe_is_busy() {
        let busy =
            io::Error::from_raw_os_error(windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32);
        assert!(!should_start_gui(&busy));
    }

    #[cfg(windows)]
    #[test]
    fn does_not_restart_gui_when_existing_pipe_denies_access() {
        let denied = io::Error::from_raw_os_error(
            windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32,
        );
        assert!(!should_start_gui(&denied));
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
    fn minimized_launch_plan_marks_extension_started_gui() {
        let plan = minimized_launch_plan(Path::new(
            r"C:\Program Files\CurlDownloader\CurlDownloader.exe",
        ));
        assert_eq!(
            plan.program,
            PathBuf::from(r"C:\Program Files\CurlDownloader\CurlDownloader.exe")
        );
        assert_eq!(plan.args, vec![OsString::from("--minimized")]);
    }

    #[test]
    fn native_host_restart_uses_current_gui_executable_minimized() {
        let executable = Path::new(r"C:\Tools\CurlDownloader.exe");
        let plan = minimized_launch_plan(executable);
        assert_eq!(plan.program, executable);
        assert_eq!(plan.args, vec![OsString::from("--minimized")]);
    }

    #[cfg(windows)]
    #[test]
    fn firefox_started_gui_breaks_away_from_native_host_job() {
        use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};

        let plan = minimized_launch_plan(Path::new(r"C:\Tools\CurlDownloader.exe"));
        assert_ne!(plan.creation_flags & CREATE_BREAKAWAY_FROM_JOB, 0);
        assert_ne!(plan.creation_flags & CREATE_NO_WINDOW, 0);
    }
    #[test]
    fn native_policy_uses_snake_case_and_legacy_camel_case_fields() {
        let snake = native_start_policy(
            br#"{"type":"list_tasks","auto_start":true,"start_intent_unix_ms":200}"#,
        );
        assert!(snake.auto_start);
        assert_eq!(snake.start_intent_unix_ms, Some(200));

        let camel = native_start_policy(
            br#"{"type":"list_tasks","autoStart":true,"startIntentUnixMs":201}"#,
        );
        assert_eq!(camel.start_intent_unix_ms, Some(201));
    }

    #[test]
    fn native_host_response_hook_runs_once_after_first_non_manual_response() {
        let mut input = Vec::new();
        crate::ipc::write_frame(&mut input, br#"{"type":"ping","request_id":"request-1"}"#)
            .unwrap();
        crate::ipc::write_frame(&mut input, br#"{"type":"ping","request_id":"request-2"}"#)
            .unwrap();
        crate::ipc::write_frame(&mut input, br#"{"type":"ping","request_id":"request-3"}"#)
            .unwrap();
        let mut output = Vec::new();
        let mut calls = 0;
        let mut response_number = 0;
        run_native_host_io_with_processor(
            input.as_slice(),
            &mut output,
            |_body| {
                response_number += 1;
                if response_number == 1 {
                    error_response("request-1", "manually_stopped", "GUI 啟動不獲允許")
                } else {
                    IpcResponse::Pong {
                        request_id: format!("request-{response_number}"),
                        ok: true,
                    }
                }
            },
            |_response| calls += 1,
        )
        .unwrap();
        assert_eq!(calls, 1);
    }
    #[test]
    fn manually_stopped_error_has_a_stable_wire_code() {
        assert_eq!(
            forward_error_code(&io::Error::new(io::ErrorKind::Interrupted, "stopped")),
            "manually_stopped"
        );
    }

    #[test]
    fn denied_start_reports_manual_stop_only_when_marker_exists() {
        assert_eq!(
            denied_start_error_kind(Some(100)),
            io::ErrorKind::Interrupted
        );
        assert_eq!(denied_start_error_kind(None), io::ErrorKind::WouldBlock);
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
