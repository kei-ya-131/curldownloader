use crate::{
    download::{EngineHandle, spawn_engine},
    model::{
        EngineCommand, EngineEvent, GlobalSettings, NewTask, PersistedState, ProxyProtocol,
        ProxySettings, TaskId, TaskSnapshot, TaskStatus,
    },
    storage,
};
use eframe::egui;
use std::{path::PathBuf, sync::Arc, time::Duration};

pub const CHINESE_FONT_NAME: &str = "NotoSansTC-VF";
const CHINESE_FONT_BYTES: &[u8] = include_bytes!("../assets/NotoSansTC-VF.ttf");

pub fn chinese_font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        CHINESE_FONT_NAME.to_owned(),
        Arc::new(egui::FontData::from_static(CHINESE_FONT_BYTES)),
    );
    for family in fonts.families.values_mut() {
        family.insert(0, CHINESE_FONT_NAME.to_owned());
    }
    fonts
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.2} KiB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.2} MiB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{:.2} GiB", bytes as f64 / 1_073_741_824.0)
    }
}

pub fn format_speed(bps: f64) -> String {
    format!("{}/s", format_bytes(bps.max(0.0) as u64))
}

pub fn format_eta(seconds: Option<u64>) -> String {
    match seconds {
        None => "—".into(),
        Some(seconds) if seconds >= 3600 => {
            format!("{}小時 {:02}分", seconds / 3600, seconds % 3600 / 60)
        }
        Some(seconds) if seconds >= 60 => format!("{}分 {:02}秒", seconds / 60, seconds % 60),
        Some(seconds) => format!("{seconds}秒"),
    }
}

pub struct CurlDownloaderApp {
    engine: EngineHandle,
    tasks: Vec<TaskSnapshot>,
    selected: Option<TaskId>,
    url_input: String,
    input_error: Option<String>,
    batch_input: String,
    batch_error: Option<String>,
    show_batch_dialog: bool,
    fatal: Option<String>,
    draft: Option<TaskDraft>,
    last_download_dir: PathBuf,
    max_processes: u8,
    shutting_down: bool,
}

#[derive(Clone)]
struct TaskDraft {
    id: TaskId,
    url: String,
    filename: String,
    target_dir: PathBuf,
    target_dir_input: String,
    segments: u8,
    proxy: ProxySettings,
    password_input: String,
    show_password: bool,
}

impl CurlDownloaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let state_path = storage::state_path().unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("CurlDownloader")
                .join("state.json")
        });
        let default_dir = storage::default_download_dir()
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());
        let (state, fatal) = match storage::load_state(&state_path) {
            Ok(mut state) => {
                state.settings.last_download_dir = if state.settings.last_download_dir.is_dir() {
                    state.settings.last_download_dir.clone()
                } else {
                    default_dir.clone()
                };
                (state, None)
            }
            Err(error) if state_path.exists() => {
                let _ = storage::quarantine_corrupt(&state_path);
                (
                    PersistedState {
                        schema_version: 1,
                        settings: GlobalSettings {
                            last_download_dir: default_dir.clone(),
                            max_curl_processes: 4,
                            next_task_id: 1,
                        },
                        tasks: Vec::new(),
                    },
                    Some(format!("狀態檔已隔離：{error}")),
                )
            }
            Err(_) => (
                PersistedState {
                    schema_version: 1,
                    settings: GlobalSettings {
                        last_download_dir: default_dir.clone(),
                        max_curl_processes: 4,
                        next_task_id: 1,
                    },
                    tasks: Vec::new(),
                },
                None,
            ),
        };
        let max_processes = state.settings.max_curl_processes;
        let last_download_dir = state.settings.last_download_dir.clone();
        let engine = spawn_engine(state_path, state)
            .unwrap_or_else(|error| panic!("無法啟動下載引擎：{error}"));

        let mut visuals = egui::Visuals::dark();
        visuals.selection.bg_fill = egui::Color32::from_rgb(40, 100, 190);
        cc.egui_ctx.set_fonts(chinese_font_definitions());
        cc.egui_ctx.set_visuals(visuals);
        Self {
            engine,
            tasks: Vec::new(),
            selected: None,
            url_input: String::new(),
            input_error: None,
            batch_input: String::new(),
            batch_error: None,
            show_batch_dialog: false,
            fatal,
            draft: None,
            last_download_dir,
            max_processes,
            shutting_down: false,
        }
    }

    fn pump_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.engine.events.try_recv() {
            match event {
                EngineEvent::Snapshot(tasks) => {
                    self.tasks = tasks;
                    if self.selected.is_none() {
                        self.selected = self.tasks.first().map(|task| task.id);
                    }
                    if let Some(selected) = self.selected {
                        if !self.tasks.iter().any(|task| task.id == selected) {
                            self.selected = self.tasks.first().map(|task| task.id);
                            self.draft = None;
                        }
                    }
                    if self
                        .selected
                        .and_then(|id| self.tasks.iter().find(|task| task.id == id))
                        .is_some_and(|task| {
                            matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled)
                        })
                    {
                        if let Some(draft) = self.draft.as_mut() {
                            draft.proxy.clear_password();
                            draft.password_input.clear();
                        }
                    }
                }
                EngineEvent::Fatal(message) => self.fatal = Some(message),
                EngineEvent::ShutdownComplete => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn selected_task(&self) -> Option<TaskSnapshot> {
        self.selected
            .and_then(|id| self.tasks.iter().find(|task| task.id == id).cloned())
    }

    fn ensure_draft(&mut self, task: &TaskSnapshot) {
        if self.draft.as_ref().is_some_and(|draft| draft.id == task.id) {
            return;
        }
        let mut proxy = ProxySettings::default();
        proxy.enabled = task.proxy.enabled;
        proxy.protocol = task.proxy.protocol;
        proxy.host = task.proxy.host.clone();
        proxy.port = task.proxy.port;
        proxy.username = task.proxy.username.clone();
        proxy.requires_password = task.proxy.requires_password;
        self.draft = Some(TaskDraft {
            id: task.id,
            url: task.original_url.clone(),
            filename: task.filename.clone(),
            target_dir: task.target_dir.clone(),
            target_dir_input: task.target_dir.display().to_string(),
            segments: task.requested_segments,
            proxy,
            password_input: String::new(),
            show_password: false,
        });
    }

    fn send_draft(&mut self, draft: &TaskDraft) {
        let mut proxy = draft.proxy.clone();
        if !draft.password_input.is_empty() {
            let _ = proxy.set_password(draft.password_input.clone());
        }
        let _ = self.engine.commands.send(EngineCommand::UpdateDraft {
            id: draft.id,
            url: draft.url.clone(),
            filename: draft.filename.clone(),
            target_dir: draft.target_dir.clone(),
            requested_segments: draft.segments,
            proxy,
        });
    }

    fn flush_draft(&mut self, id: TaskId) {
        if let Some(draft) = self.draft.as_ref().filter(|draft| draft.id == id).cloned() {
            self.send_draft(&draft);
        }
    }

    fn add_url(&mut self) {
        let value = self.url_input.trim().to_owned();
        let Ok(url) = url::Url::parse(&value) else {
            self.input_error = Some("網址格式無效".into());
            return;
        };
        if !matches!(url.scheme(), "http" | "https") {
            self.input_error = Some("只支援 HTTP 或 HTTPS 網址".into());
            return;
        }
        let _ = self.engine.commands.send(EngineCommand::Add(NewTask {
            url: value,
            target_dir: self.last_download_dir.clone(),
        }));
        self.url_input.clear();
        self.input_error = None;
    }

    fn add_batch(&mut self) {
        let urls = match parse_batch_urls(&self.batch_input) {
            Ok(urls) => urls,
            Err(error) => {
                self.batch_error = Some(error);
                return;
            }
        };
        let tasks = urls
            .into_iter()
            .map(|url| NewTask {
                url,
                target_dir: self.last_download_dir.clone(),
            })
            .collect();
        let _ = self.engine.commands.send(EngineCommand::AddBatch(tasks));
        self.batch_input.clear();
        self.batch_error = None;
        self.show_batch_dialog = false;
    }

    fn show_top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top-bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let response = ui.text_edit_singleline(&mut self.url_input);
                if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    self.add_url();
                }
                if ui.button("新增下載").clicked() {
                    self.add_url();
                }
                if ui.button("批量新增").clicked() {
                    self.batch_error = None;
                    self.show_batch_dialog = true;
                }
                ui.label("最大 curl：");
                if ui
                    .add(egui::DragValue::new(&mut self.max_processes).range(1..=16))
                    .changed()
                {
                    let _ = self
                        .engine
                        .commands
                        .send(EngineCommand::SetMaxProcesses(self.max_processes));
                }
                if ui.button("全部開始").clicked() {
                    if let Some(draft) = self.draft.clone() {
                        self.send_draft(&draft);
                    }
                    let _ = self.engine.commands.send(EngineCommand::StartAll);
                }
                if ui.button("全部暫停").clicked() {
                    let _ = self.engine.commands.send(EngineCommand::PauseAll);
                }
                if ui
                    .button("清除已完成")
                    .on_hover_text("清除已完成及已取消的任務記錄，不會刪除已下載檔案")
                    .clicked()
                {
                    let _ = self.engine.commands.send(EngineCommand::ClearHistory);
                }
            });
            if let Some(error) = &self.input_error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            if let Some(message) = &self.fatal {
                ui.colored_label(egui::Color32::YELLOW, message);
            }
            if self.shutting_down {
                ui.colored_label(egui::Color32::LIGHT_BLUE, "正在安全停止下載…");
            }
        });
        if self.show_batch_dialog {
            let mut open = true;
            let mut submit = false;
            let mut cancel = false;
            egui::Window::new("批量新增下載")
                .open(&mut open)
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label("每行輸入一個 HTTP/HTTPS 網址；新增後會先排隊，確認每個任務設定後再按開始。明文密碼不會保存。",);
                    ui.add(
                        egui::TextEdit::multiline(&mut self.batch_input)
                            .desired_rows(10)
                            .desired_width(620.0),
                    );
                    if let Some(error) = &self.batch_error {
                        ui.colored_label(egui::Color32::LIGHT_RED, error);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("批量新增").clicked() {
                            submit = true;
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });
            if submit {
                self.add_batch();
            } else if cancel || !open {
                self.show_batch_dialog = false;
            }
        }
    }

    fn show_queue(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("queue")
            .resizable(true)
            .default_size(470.0)
            .show(ui, |ui| {
                ui.heading("下載佇列");
                for task in self.tasks.clone() {
                    let selected = self.selected == Some(task.id);
                    let response = ui.selectable_label(
                        selected,
                        egui::RichText::new(&task.filename).strong().size(15.0),
                    );
                    if response.clicked() {
                        self.selected = Some(task.id);
                        self.draft = None;
                    }
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(status_label(task.status)).strong());
                        ui.small(format!("{} 段", task.actual_segments.max(1)));
                    });
                    let bar_width = ui.available_width();
                    ui.add_sized(
                        [bar_width, 26.0],
                        egui::ProgressBar::new(progress_fraction(&task))
                            .text(format!("{:.1}%", progress_fraction(&task) * 100.0)),
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(progress_text(&task))
                                .strong()
                                .size(14.0),
                        );
                        ui.label(egui::RichText::new(speed_text(&task)).strong().size(14.0));
                        ui.label(format!("剩餘 {}", format_eta(task.eta_seconds)));
                    });
                    ui.horizontal(|ui| {
                        if matches!(
                            task.status,
                            TaskStatus::Queued | TaskStatus::Paused | TaskStatus::Failed
                        ) && ui.button("開始").clicked()
                        {
                            self.flush_draft(task.id);
                            let _ = self.engine.commands.send(EngineCommand::Start(task.id));
                        }
                        if matches!(task.status, TaskStatus::Probing | TaskStatus::Downloading)
                            && ui.button("暫停").clicked()
                        {
                            let _ = self.engine.commands.send(EngineCommand::Pause(task.id));
                        }
                        if !matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled)
                            && ui.button("取消").clicked()
                        {
                            let _ = self.engine.commands.send(EngineCommand::Cancel(task.id));
                        }
                        if task.status == TaskStatus::Completed
                            && ui
                                .button("開啟位置")
                                .on_hover_text("在檔案總管選取檔案")
                                .clicked()
                        {
                            open_location(task.target_dir.join(task.filename));
                        }
                        if matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled)
                            && ui.button("清除記錄").clicked()
                        {
                            let _ = self.engine.commands.send(EngineCommand::Remove(task.id));
                        }
                    });
                    ui.separator();
                }
            });
    }

    fn show_inspector(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            let Some(task) = self.selected_task() else {
                ui.heading("選取任務以檢視詳細資料");
                return;
            };
            self.ensure_draft(&task);
            let needs_password = task.status == TaskStatus::NeedsProxyPassword;
            ui.heading(&task.filename);
            ui.label(format!("狀態：{}", status_label(task.status)));
            if let Some(error) = &task.error {
                ui.colored_label(
                    egui::Color32::LIGHT_RED,
                    format!("{}：{}", error.summary, error.action),
                );
                if !error.diagnostic.trim().is_empty() {
                    ui.collapsing("詳細診斷（已清理敏感資料）", |ui| {
                        ui.monospace(&error.diagnostic);
                    });
                }
            }
            let mut pending_update = None;
            if let Some(draft) = self.draft.as_mut() {
                let mut changed = false;
                let can_edit = matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Paused | TaskStatus::Failed
                );
                ui.add_enabled_ui(can_edit, |ui| {
                    ui.label("網址");
                    if ui.text_edit_singleline(&mut draft.url).lost_focus() {
                        changed = true;
                    }
                    ui.label("檔名");
                    if ui.text_edit_singleline(&mut draft.filename).lost_focus() {
                        changed = true;
                    }
                    ui.label("下載目錄");
                    if ui
                        .text_edit_singleline(&mut draft.target_dir_input)
                        .lost_focus()
                    {
                        let path = PathBuf::from(draft.target_dir_input.trim());
                        if path.is_dir() {
                            draft.target_dir = path.clone();
                            self.last_download_dir = path.clone();
                            let _ = self
                                .engine
                                .commands
                                .send(EngineCommand::SetLastDownloadDir(path));
                            changed = true;
                        } else {
                            self.input_error = Some("下載目錄不存在".into());
                        }
                    }
                    if ui
                        .button("選擇資料夾…")
                        .on_hover_text("選擇此任務的下載位置")
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_directory(&draft.target_dir)
                            .pick_folder()
                        {
                            draft.target_dir_input = path.display().to_string();
                            draft.target_dir = path.clone();
                            self.last_download_dir = path.clone();
                            let _ = self
                                .engine
                                .commands
                                .send(EngineCommand::SetLastDownloadDir(path));
                            changed = true;
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.label("分段");
                        changed |= ui
                            .add(egui::Slider::new(&mut draft.segments, 1..=8))
                            .changed();
                    });
                    ui.separator();
                    ui.checkbox(&mut draft.proxy.enabled, "使用 Proxy");
                    egui::ComboBox::from_id_salt("proxy-protocol")
                        .selected_text(draft.proxy.protocol.scheme())
                        .show_ui(ui, |ui| {
                            for protocol in [
                                ProxyProtocol::Http,
                                ProxyProtocol::Https,
                                ProxyProtocol::Socks5,
                                ProxyProtocol::Socks5h,
                            ] {
                                changed |= ui
                                    .selectable_value(
                                        &mut draft.proxy.protocol,
                                        protocol,
                                        protocol.scheme(),
                                    )
                                    .changed();
                            }
                        });
                    ui.horizontal(|ui| {
                        ui.label("主機");
                        changed |= ui.text_edit_singleline(&mut draft.proxy.host).changed();
                        ui.label("連接埠");
                        changed |= ui
                            .add(egui::DragValue::new(&mut draft.proxy.port).range(1..=65535))
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label("帳號");
                        changed |= ui.text_edit_singleline(&mut draft.proxy.username).changed();
                        ui.label("密碼");
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut draft.password_input)
                                .password(!draft.show_password),
                        );
                        draft.show_password = response.is_pointer_button_down_on();
                        if draft.show_password {
                            ui.label("顯示中");
                        }
                    });
                });
                if !can_edit && !needs_password {
                    ui.label("下載已開始，檔名、位置及 Proxy 設定已鎖定。");
                }
                if needs_password {
                    ui.label("此任務需要 Proxy 密碼才能繼續。");
                    if ui.button("設定密碼並開始").clicked() && !draft.password_input.is_empty()
                    {
                        let _ = self.engine.commands.send(EngineCommand::SetProxyPassword {
                            id: draft.id,
                            password: zeroize::Zeroizing::new(draft.password_input.clone()),
                        });
                        draft.password_input.clear();
                    }
                } else if changed {
                    pending_update = Some(draft.clone());
                }
            }
            if let Some(draft) = pending_update {
                self.send_draft(&draft);
            }
        });
    }
}

impl eframe::App for CurlDownloaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump_events(&ctx);
        if ctx.input(|input| input.viewport().close_requested()) && !self.shutting_down {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.shutting_down = true;
            let _ = self.engine.commands.send(EngineCommand::Shutdown);
        }
        ctx.request_repaint_after(Duration::from_millis(200));
        self.show_top_bar(ui);
        self.show_queue(ui);
        self.show_inspector(ui);
    }
}

fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "排隊中",
        TaskStatus::Probing => "探測中",
        TaskStatus::Downloading => "下載中",
        TaskStatus::Pausing => "暫停中",
        TaskStatus::Paused => "已暫停",
        TaskStatus::NeedsProxyPassword => "需要 Proxy 密碼",
        TaskStatus::Finalizing => "整合中",
        TaskStatus::Completed => "已完成",
        TaskStatus::Failed => "失敗",
        TaskStatus::Cancelled => "已取消",
    }
}

fn progress_fraction(task: &TaskSnapshot) -> f32 {
    if task.status == TaskStatus::Completed {
        return 1.0;
    }
    task.total_size
        .map(|total| {
            if total == 0 {
                0.0
            } else {
                (task.downloaded as f64 / total as f64).clamp(0.0, 1.0) as f32
            }
        })
        .unwrap_or(0.0)
}

fn progress_text(task: &TaskSnapshot) -> String {
    format_progress_text(task.downloaded, task.total_size)
}

fn format_progress_text(downloaded: u64, total: Option<u64>) -> String {
    match total {
        Some(total) => {
            let fraction = if total == 0 {
                0.0
            } else {
                (downloaded as f64 / total as f64).clamp(0.0, 1.0)
            };
            format!(
                "{:.1}%  ·  {} / {}",
                fraction * 100.0,
                format_bytes(downloaded),
                format_bytes(total)
            )
        }
        None if downloaded == 0 => "尚未開始".into(),
        None => format!("{}  ·  總大小未知", format_bytes(downloaded)),
    }
}

fn speed_text(task: &TaskSnapshot) -> String {
    format_speed_text(task.status, task.current_bps, task.average_bps)
}

fn format_speed_text(status: TaskStatus, current_bps: f64, average_bps: f64) -> String {
    if matches!(status, TaskStatus::Downloading | TaskStatus::Probing) {
        format!("目前速度 {}", format_speed(current_bps))
    } else {
        format!("平均速度 {}", format_speed(average_bps))
    }
}

fn parse_batch_urls(input: &str) -> Result<Vec<String>, String> {
    let urls = input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if urls.is_empty() {
        return Err("請至少輸入一個網址".into());
    }
    for url in &urls {
        let parsed = url::Url::parse(url).map_err(|_| format!("網址格式無效：{url}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(format!("只支援 HTTP 或 HTTPS 網址：{url}"));
        }
    }
    Ok(urls)
}

fn location_directory(path: &std::path::Path) -> &std::path::Path {
    path.parent().unwrap_or(path)
}

#[cfg(target_os = "windows")]
fn open_location(path: PathBuf) {
    let _ = std::process::Command::new("explorer.exe")
        .arg(location_directory(&path))
        .spawn();
}

#[cfg(not(target_os = "windows"))]
fn open_location(_path: PathBuf) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_and_speed() {
        assert_eq!(format_bytes(1_572_864), "1.50 MiB");
        assert_eq!(format_speed(1_048_576.0), "1.00 MiB/s");
    }

    #[test]
    fn formats_eta() {
        assert_eq!(format_eta(Some(65)), "1分 05秒");
        assert_eq!(format_eta(None), "—");
    }

    #[test]
    fn installs_traditional_chinese_font_as_high_priority_fallback() {
        let fonts = chinese_font_definitions();
        let font = fonts
            .font_data
            .get(CHINESE_FONT_NAME)
            .expect("Traditional Chinese font must be bundled");
        assert!(font.font.len() > 100_000);
        assert!(
            fonts
                .families
                .values()
                .all(|family| family.first().is_some_and(|name| name == CHINESE_FONT_NAME))
        );
    }

    #[test]
    fn parses_one_url_per_line_for_batch_downloads() {
        assert_eq!(
            parse_batch_urls("\n https://example.test/a.bin \n\nhttp://example.test/b.bin\n")
                .unwrap(),
            vec![
                "https://example.test/a.bin".to_owned(),
                "http://example.test/b.bin".to_owned()
            ]
        );
    }

    #[test]
    fn rejects_invalid_batch_url() {
        assert_eq!(
            parse_batch_urls("https://example.test/a.bin\nftp://example.test/b.bin").unwrap_err(),
            "只支援 HTTP 或 HTTPS 網址：ftp://example.test/b.bin"
        );
    }

    #[test]
    fn opens_the_parent_directory_for_a_completed_file() {
        let file = PathBuf::from("C:\\Downloads\\completed.bin");
        assert_eq!(
            location_directory(&file),
            PathBuf::from("C:\\Downloads").as_path()
        );
    }

    #[test]
    fn formats_progress_as_the_primary_download_summary() {
        assert_eq!(
            format_progress_text(512, Some(1024)),
            "50.0%  ·  512 B / 1.00 KiB"
        );
        assert_eq!(format_progress_text(0, None), "尚未開始");
    }

    #[test]
    fn uses_average_speed_for_terminal_downloads() {
        assert_eq!(
            format_speed_text(TaskStatus::Completed, 0.0, 2_048.0),
            "平均速度 2.00 KiB/s"
        );
        assert_eq!(
            format_speed_text(TaskStatus::Downloading, 1_024.0, 512.0),
            "目前速度 1.00 KiB/s"
        );
    }
}
