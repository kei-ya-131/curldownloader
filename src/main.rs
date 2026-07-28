#![windows_subsystem = "windows"]

use curl_downloader::{app::CurlDownloaderApp, native_host, native_registration, single_instance};

fn main() -> eframe::Result {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if native_host::is_native_host_invocation(&arguments) {
        if let Err(error) = native_host::run_native_host() {
            eprintln!("Native Messaging host error: {error}");
        }
        return Ok(());
    }

    run_gui(arguments.iter().any(|argument| argument == "--minimized"))
}

fn run_gui(minimized: bool) -> eframe::Result {
    if let Ok(executable) = std::env::current_exe()
        && let Err(error) = native_registration::ensure_registered(&executable)
    {
        eprintln!("Firefox Native host 自動註冊失敗：{error}");
    }

    let _instance = match single_instance::acquire() {
        Ok(Some(instance)) => instance,
        Ok(None) => return Ok(()),
        Err(error) => {
            eprintln!("GUI 單例初始化失敗：{error}");
            return Ok(());
        }
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Curl Downloader")
            .with_visible(!minimized)
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
