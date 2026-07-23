use eframe::egui;

pub struct CurlDownloaderApp;

impl CurlDownloaderApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self
    }
}

impl eframe::App for CurlDownloaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Curl Downloader");
            ui.label("下載器初始化完成");
        });
    }
}
