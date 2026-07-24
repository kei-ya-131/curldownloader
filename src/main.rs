#![windows_subsystem = "windows"]

use curl_downloader::{app::CurlDownloaderApp, native_host, single_instance};

fn main() -> eframe::Result {
    if std::env::args_os().any(|argument| argument == native_host::NATIVE_HOST_FLAG) {
        if let Err(error) = native_host::run_native_host() {
            eprintln!("Native Messaging host error: {error}");
        }
        return Ok(());
    }

    run_gui()
}

fn run_gui() -> eframe::Result {
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
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([860.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Curl Downloader",
        options,
        Box::new(|cc| Ok(Box::new(CurlDownloaderApp::new(cc)))),
    )
}
