#![windows_subsystem = "windows"]

use curl_downloader::{
    app::CurlDownloaderApp, ipc, native_host, native_registration, single_instance, startup_policy,
    storage,
};

fn main() -> eframe::Result {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if native_host::is_native_host_invocation(&arguments) {
        if let Err(error) = native_host::run_native_host() {
            eprintln!("Native Messaging host error: {error}");
        }
        return Ok(());
    }

    let register_native = should_register_native(&arguments);
    run_gui(
        arguments.iter().any(|argument| argument == "--minimized"),
        register_native,
    )
}

fn initial_viewport_visible(_minimized: bool) -> bool {
    true
}
fn should_register_native(arguments: &[std::ffi::OsString]) -> bool {
    !arguments
        .iter()
        .any(|argument| argument == "--skip-native-registration")
}

fn run_gui(minimized: bool, register_native: bool) -> eframe::Result {
    if register_native
        && let Ok(executable) = std::env::current_exe()
        && let Err(error) = native_registration::ensure_registered(&executable)
    {
        eprintln!("Firefox Native host 自動註冊失敗：{error}");
    }

    let _instance = match single_instance::acquire() {
        Ok(Some(instance)) => instance,
        Ok(None) => {
            let request = ipc::show_window_request();
            let _ = ipc::call_pipe_with_retry(
                &request,
                std::time::Duration::from_millis(100),
                std::time::Duration::from_millis(50),
                40,
            );
            return Ok(());
        }
        Err(error) => {
            eprintln!("GUI 單例初始化失敗：{error}");
            return Ok(());
        }
    };
    if !minimized && let Ok(state_path) = storage::state_path() {
        let _ = startup_policy::clear_manual_stop(&storage::manual_stop_path(&state_path));
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Curl Downloader")
            .with_visible(initial_viewport_visible(minimized))
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([860.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Curl Downloader",
        options,
        Box::new(move |cc| Ok(Box::new(CurlDownloaderApp::new(cc, minimized)))),
    )
}

#[cfg(test)]
mod tests {
    use super::{initial_viewport_visible, should_register_native};

    #[test]
    fn lifecycle_probe_can_skip_native_registration() {
        let arguments = vec![
            std::ffi::OsString::from("--minimized"),
            std::ffi::OsString::from("--skip-native-registration"),
        ];
        assert!(!should_register_native(&arguments));
        assert!(should_register_native(&[std::ffi::OsString::from(
            "--minimized"
        )]));
    }

    #[test]
    fn minimized_gui_starts_a_viewport_before_hiding_to_tray() {
        assert!(initial_viewport_visible(true));
        assert!(initial_viewport_visible(false));
    }
}
