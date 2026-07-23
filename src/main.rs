#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use curl_downloader::app::CurlDownloaderApp;

fn main() -> eframe::Result {
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
