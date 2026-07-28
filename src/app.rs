use crate::{
    download::{EngineHandle, spawn_engine},
    ipc,
    model::{
        CurlSource, EngineCommand, EngineEvent, GlobalSettings, NewTask, PersistedState,
        ProxyProtocol, ProxySettings, TaskId, TaskSnapshot, TaskStatus,
    },
    storage,
    tray::{self, TrayController, TrayEvent},
};
use eframe::egui;
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
    },
    thread::JoinHandle,
    time::Duration,
};

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
    checked_tasks: HashSet<TaskId>,
    url_input: String,
    queue_search: String,
    input_error: Option<String>,
    batch_input: String,
    batch_error: Option<String>,
    show_batch_dialog: bool,
    batch_proxy_dialog: bool,
    batch_proxy: Option<BatchProxyDraft>,
    batch_proxy_message: Option<String>,
    fatal: Option<String>,
    draft: Option<TaskDraft>,
    last_download_dir: PathBuf,
    max_processes: u8,
    shutting_down: bool,
    ipc_stop: Arc<AtomicBool>,
    ipc_default_dir: Arc<Mutex<PathBuf>>,
    ipc_snapshots: ipc::SharedSnapshots,
    ipc_ui_receiver: Receiver<ipc::UiCommand>,
    pending_show_task: Option<TaskId>,
    start_minimized: bool,
    hidden_to_tray: bool,
    _tray: TrayController,
    tray_receiver: Receiver<TrayEvent>,
    ipc_thread: Option<JoinHandle<()>>,
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

#[derive(Clone)]
struct BatchProxyDraft {
    proxy: ProxySettings,
    password_input: String,
    show_password: bool,
}

impl CurlDownloaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>, start_minimized: bool) -> Self {
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
        let ipc_stop = Arc::new(AtomicBool::new(false));
        let ipc_default_dir = Arc::new(Mutex::new(last_download_dir.clone()));
        let ipc_snapshots = Arc::new(Mutex::new(Vec::<TaskSnapshot>::new()));
        let repaint_context = cc.egui_ctx.clone();
        let repaint: Arc<dyn Fn() + Send + Sync> =
            Arc::new(move || repaint_context.request_repaint());
        let mut start_minimized = start_minimized;
        let (tray, tray_receiver) = match tray::TrayController::create(Arc::clone(&repaint)) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("Windows 系統匣初始化失敗：{error}");
                start_minimized = effective_start_minimized(start_minimized, false);
                tray::TrayController::disabled()
            }
        };
        let (ipc_ui_sender, ipc_ui_receiver) = std::sync::mpsc::channel();
        let ipc_thread = Some(ipc::spawn_server_with_repaint(
            engine.commands.clone(),
            Arc::clone(&ipc_default_dir),
            Arc::clone(&ipc_snapshots),
            ipc_ui_sender,
            Arc::clone(&ipc_stop),
            repaint,
        ));

        cc.egui_ctx.set_fonts(chinese_font_definitions());
        cc.egui_ctx.set_theme(egui::ThemePreference::System);
        Self {
            engine,
            tasks: Vec::new(),
            selected: None,
            checked_tasks: HashSet::new(),
            url_input: String::new(),
            queue_search: String::new(),
            input_error: None,
            batch_input: String::new(),
            batch_error: None,
            show_batch_dialog: false,
            batch_proxy_dialog: false,
            batch_proxy: None,
            batch_proxy_message: None,
            fatal,
            draft: None,
            last_download_dir,
            max_processes,
            shutting_down: false,
            ipc_stop,
            ipc_default_dir,
            ipc_snapshots,
            ipc_ui_receiver,
            pending_show_task: None,
            start_minimized,
            hidden_to_tray: start_minimized,
            _tray: tray,
            tray_receiver,
            ipc_thread,
        }
    }

    fn begin_shutdown(&mut self, _ctx: &egui::Context) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.ipc_stop.store(true, Ordering::Release);
        let _ = self.engine.commands.send(EngineCommand::Shutdown);
    }
    fn pump_events(&mut self, ctx: &egui::Context) -> bool {
        let mut restored = false;
        while let Ok(event) = self.tray_receiver.try_recv() {
            match event {
                TrayEvent::ShowWindow => {
                    self.hidden_to_tray = false;
                    set_main_window_visibility(true);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    restored = true;
                }
                TrayEvent::CloseWindow => self.begin_shutdown(ctx),
            }
        }
        while let Ok(event) = self.engine.events.try_recv() {
            match event {
                EngineEvent::Snapshot(tasks) => {
                    if let Ok(mut snapshots) = self.ipc_snapshots.lock() {
                        *snapshots = tasks.clone();
                    }
                    self.tasks = tasks;
                    let draft_proxy_is_stale = self
                        .draft
                        .as_ref()
                        .and_then(|draft| {
                            self.tasks
                                .iter()
                                .find(|task| task.id == draft.id)
                                .map(|task| !draft_proxy_matches_snapshot(draft, task))
                        })
                        .unwrap_or(false);
                    if draft_proxy_is_stale {
                        self.draft = None;
                    }
                    if self.selected.is_none() {
                        self.selected = self.tasks.first().map(|task| task.id);
                    }
                    if let Some(selected) = self.selected {
                        if !self.tasks.iter().any(|task| task.id == selected) {
                            self.selected = self.tasks.first().map(|task| task.id);
                            self.draft = None;
                        }
                    }
                    self.checked_tasks.retain(|id| {
                        self.tasks
                            .iter()
                            .any(|task| task.id == *id && can_edit_proxy_in_bulk(task.status))
                    });
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
                EngineEvent::BatchProxyApplied { applied, skipped } => {
                    self.batch_proxy_message = Some(format_batch_proxy_result(applied, skipped));
                }
                EngineEvent::Fatal(message) => self.fatal = Some(message),
                EngineEvent::ShutdownComplete => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        while let Ok(command) = self.ipc_ui_receiver.try_recv() {
            match command {
                ipc::UiCommand::ShowWindow => {
                    self.hidden_to_tray = false;
                    set_main_window_visibility(true);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    restored = true;
                }
                ipc::UiCommand::ShowTask { task_id } => {
                    self.pending_show_task = Some(task_id);
                }
            }
        }
        if let Some(task_id) = self.pending_show_task
            && let Some(selected) = resolve_show_task(&self.tasks, task_id)
        {
            self.selected = Some(selected);
            self.draft = None;
            self.hidden_to_tray = false;
            set_main_window_visibility(true);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.pending_show_task = None;
            restored = true;
        }
        restored
    }

    fn selected_task(&self) -> Option<TaskSnapshot> {
        self.selected
            .and_then(|id| self.tasks.iter().find(|task| task.id == id).cloned())
    }

    fn select_all_editable(&mut self) {
        self.checked_tasks = self
            .tasks
            .iter()
            .filter(|task| can_edit_proxy_in_bulk(task.status))
            .map(|task| task.id)
            .collect();
    }

    fn open_batch_proxy_dialog(&mut self) {
        if self.checked_tasks.is_empty() {
            return;
        }
        let proxy = self
            .selected_task()
            .filter(|task| self.checked_tasks.contains(&task.id))
            .map(|task| proxy_from_snapshot(&task))
            .unwrap_or_default();
        self.batch_proxy = Some(BatchProxyDraft {
            proxy,
            password_input: String::new(),
            show_password: false,
        });
        self.batch_proxy_dialog = true;
    }

    fn apply_batch_proxy(&mut self) {
        let Some(draft) = self.batch_proxy.clone() else {
            return;
        };
        let mut proxy = draft.proxy;
        if draft.password_input.is_empty() {
            proxy.clear_password();
        } else if proxy.set_password(draft.password_input).is_err() {
            self.input_error = Some("Proxy 密碼含有不允許字元".into());
            return;
        }
        if !proxy.enabled {
            proxy.clear_password();
            proxy.requires_password = false;
        }
        let ids = self.checked_tasks.iter().copied().collect::<Vec<_>>();
        let _ = self
            .engine
            .commands
            .send(EngineCommand::UpdateProxy { ids, proxy });
        self.batch_proxy_dialog = false;
        self.batch_proxy = None;
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
        let panel_fill = ui.visuals().panel_fill;
        egui::Panel::top("top-bar")
            .frame(egui::Frame::new().fill(panel_fill).inner_margin(10))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Curl Downloader").strong().size(18.0));
                    let response = ui.add_sized(
                        [260.0, 30.0],
                        egui::TextEdit::singleline(&mut self.url_input)
                            .hint_text("貼上 HTTP/HTTPS 網址"),
                    );
                    if response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        self.add_url();
                    }
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new("＋ 新增下載").strong(),
                        ))
                        .clicked()
                    {
                        self.add_url();
                    }
                    if ui.button("☷ 批量新增").clicked() {
                        self.batch_error = None;
                        self.show_batch_dialog = true;
                    }
                    ui.separator();
                    ui.label("最大 curl");
                    if ui
                        .add(egui::DragValue::new(&mut self.max_processes).range(1..=16))
                        .changed()
                    {
                        let _ = self
                            .engine
                            .commands
                            .send(EngineCommand::SetMaxProcesses(self.max_processes));
                    }
                    if ui.button("▶ 開始全部").clicked() {
                        if let Some(draft) = self.draft.clone() {
                            self.send_draft(&draft);
                        }
                        let _ = self.engine.commands.send(EngineCommand::StartAll);
                    }
                    if ui.button("Ⅱ 暫停全部").clicked() {
                        let _ = self.engine.commands.send(EngineCommand::PauseAll);
                    }
                    ui.separator();
                    if ui.button("全選可編輯").clicked() {
                        self.select_all_editable();
                    }
                    if !self.checked_tasks.is_empty() {
                        ui.label(
                            egui::RichText::new(format!(
                                "已選取 {} 個任務",
                                self.checked_tasks.len()
                            ))
                            .strong(),
                        );
                        if ui.button("批量設定 Proxy").clicked() {
                            self.open_batch_proxy_dialog();
                        }
                        if ui.button("清除選取").clicked() {
                            self.checked_tasks.clear();
                        }
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
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if let Some(message) = &self.batch_proxy_message {
                    ui.colored_label(ui.visuals().text_color(), format!("✓ {message}"));
                }
                if let Some(message) = &self.fatal {
                    ui.colored_label(ui.visuals().warn_fg_color, message);
                }
                if self.shutting_down {
                    ui.colored_label(ui.visuals().hyperlink_color, "正在安全停止下載…");
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
                        ui.colored_label(ui.visuals().error_fg_color, error);
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
        if self.batch_proxy_dialog {
            let mut open = true;
            let mut apply = false;
            let mut cancel = false;
            egui::Window::new("批量設定 Proxy")
                .open(&mut open)
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label(format!(
                        "將套用至 {} 個已選取任務；下載中及已完成任務會略過。",
                        self.checked_tasks.len()
                    ));
                    let Some(draft) = self.batch_proxy.as_mut() else {
                        return;
                    };
                    ui.checkbox(&mut draft.proxy.enabled, "使用 Proxy");
                    egui::ComboBox::from_id_salt("batch-proxy-protocol")
                        .selected_text(draft.proxy.protocol.scheme())
                        .show_ui(ui, |ui| {
                            for protocol in [
                                ProxyProtocol::Http,
                                ProxyProtocol::Https,
                                ProxyProtocol::Socks5,
                                ProxyProtocol::Socks5h,
                            ] {
                                ui.selectable_value(
                                    &mut draft.proxy.protocol,
                                    protocol,
                                    protocol.scheme(),
                                );
                            }
                        });
                    ui.horizontal(|ui| {
                        ui.label("主機");
                        ui.text_edit_singleline(&mut draft.proxy.host);
                        ui.label("連接埠");
                        ui.add(egui::DragValue::new(&mut draft.proxy.port).range(1..=65535));
                    });
                    ui.horizontal(|ui| {
                        ui.label("帳號");
                        ui.text_edit_singleline(&mut draft.proxy.username);
                        ui.label("密碼");
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut draft.password_input)
                                .password(!draft.show_password),
                        );
                        draft.show_password = response.is_pointer_button_down_on();
                    });
                    ui.label("密碼留空會清除批量設定中的密碼；需要驗證的任務會等待重新輸入。");
                    ui.horizontal(|ui| {
                        if ui.button("套用").clicked() {
                            apply = true;
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });
            if apply {
                self.apply_batch_proxy();
            } else if cancel || !open {
                self.batch_proxy_dialog = false;
                self.batch_proxy = None;
            }
        }
    }

    fn show_queue(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("queue")
            .resizable(true)
            .default_size(350.0)
            .show(ui, |ui| {
                egui::Frame::new().inner_margin(12).show(ui, |ui| {
                    ui.heading("下載佇列");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.queue_search)
                            .hint_text("搜尋下載…")
                            .desired_width(ui.available_width()),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt("queue-scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let search = self.queue_search.trim().to_lowercase();
                            let matches_search = |task: &TaskSnapshot| {
                                search.is_empty()
                                    || task.filename.to_lowercase().contains(&search)
                                    || task.original_url.to_lowercase().contains(&search)
                            };
                            let active = self
                                .tasks
                                .iter()
                                .filter(|task| queue_group(task.status) == QueueGroup::Active)
                                .filter(|task| matches_search(task))
                                .cloned()
                                .collect::<Vec<_>>();
                            let completed = self
                                .tasks
                                .iter()
                                .filter(|task| queue_group(task.status) == QueueGroup::Completed)
                                .filter(|task| matches_search(task))
                                .cloned()
                                .collect::<Vec<_>>();
                            self.show_queue_section(ui, "下載佇列", &active);
                            ui.add_space(8.0);
                            self.show_queue_section(
                                ui,
                                &format!("已完成 ({})", completed.len()),
                                &completed,
                            );
                            if active.is_empty() && completed.is_empty() {
                                ui.weak("目前沒有符合的任務");
                            }
                        });
                    ui.separator();
                    ui.weak(format!("全部 {} 個任務", self.tasks.len()));
                });
            });
    }

    fn show_queue_section(&mut self, ui: &mut egui::Ui, title: &str, tasks: &[TaskSnapshot]) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(title).strong().size(16.0));
            ui.separator();
            ui.weak(format!("{} 個", tasks.len()));
        });
        for task in tasks {
            self.show_task_card(ui, task);
            ui.add_space(8.0);
        }
    }

    fn show_task_card(&mut self, ui: &mut egui::Ui, task: &TaskSnapshot) {
        let selected = self.selected == Some(task.id);
        let mut checked = self.checked_tasks.contains(&task.id);
        let mut open_task = false;
        task_card_frame(ui, selected).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        can_edit_proxy_in_bulk(task.status),
                        egui::Checkbox::new(&mut checked, "選取"),
                    )
                    .changed()
                {
                    if checked {
                        self.checked_tasks.insert(task.id);
                    } else {
                        self.checked_tasks.remove(&task.id);
                    }
                }
                ui.colored_label(
                    status_color(ui, task.status),
                    egui::RichText::new(status_icon(task.status))
                        .size(21.0)
                        .strong(),
                );
                let response = ui.selectable_label(
                    selected,
                    egui::RichText::new(&task.filename).strong().size(15.0),
                );
                if response.clicked() {
                    open_task = true;
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    status_color(ui, task.status),
                    egui::RichText::new(status_label(task.status)).strong(),
                );
                ui.weak(format!("{} 段", task.actual_segments.max(1)));
                ui.weak(curl_source_label(task.curl_source));
            });
            ui.add(
                egui::ProgressBar::new(progress_fraction(task))
                    .desired_height(14.0)
                    .fill(status_color(ui, task.status))
                    .text(format!("{:.1}%", progress_fraction(task) * 100.0)),
            );
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(progress_text(task)).strong());
                ui.label(egui::RichText::new(speed_text(task)).strong());
                ui.weak(format!("剩餘 {}", format_eta(task.eta_seconds)));
            });
            ui.horizontal_wrapped(|ui| {
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
                        .on_hover_text("在檔案總管開啟下載檔案所在位置")
                        .clicked()
                {
                    open_location(task.target_dir.join(&task.filename));
                }
                if matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled)
                    && ui.button("清除記錄").clicked()
                {
                    let _ = self.engine.commands.send(EngineCommand::Remove(task.id));
                }
            });
        });
        if open_task {
            self.selected = Some(task.id);
            self.draft = None;
        }
    }

    fn show_inspector(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(16))
            .show(ui, |ui| {
                let Some(task) = self.selected_task() else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.heading("選取任務以檢視詳細資料");
                        ui.weak("下載進度、速度及 Proxy 設定會顯示在這裡");
                    });
                    return;
                };
                self.ensure_draft(&task);
                ui.horizontal(|ui| {
                    ui.colored_label(
                        status_color(ui, task.status),
                        egui::RichText::new(status_icon(task.status))
                            .size(30.0)
                            .strong(),
                    );
                    ui.vertical(|ui| {
                        ui.heading(&task.filename);
                        ui.horizontal(|ui| {
                            ui.colored_label(
                                status_color(ui, task.status),
                                egui::RichText::new(status_label(task.status)).strong(),
                            );
                            ui.weak(format!("下載工具：{}", curl_source_label(task.curl_source)));
                        });
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.colored_label(
                        ui.visuals().hyperlink_color,
                        egui::RichText::new("▤  任務總覽").strong(),
                    );
                    ui.weak("分段設定");
                    ui.weak("記事");
                });
                ui.separator();

                egui::ScrollArea::vertical()
                    .id_salt("inspector-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut pending_update = None;
                        let mut pending_last_dir = None;
                        let mut pending_password = None;
                        let mut pending_error = None;
                        let needs_password = task.status == TaskStatus::NeedsProxyPassword;
                        let can_edit = matches!(
                            task.status,
                            TaskStatus::Queued | TaskStatus::Paused | TaskStatus::Failed
                        );

                        ui.columns(2, |columns| {
                            show_progress_card(&mut columns[0], &task);
                            show_basic_info_card(&mut columns[1], &task);
                        });
                        ui.add_space(12.0);

                        if let Some(draft) = self.draft.as_mut() {
                            let mut changed = false;
                            ui.columns(2, |columns| {
                                card_frame(&columns[0]).show(&mut columns[0], |ui| {
                                    ui.heading("儲存位置");
                                    ui.label("來源 URL");
                                    changed |= ui
                                        .add_enabled(
                                            can_edit,
                                            egui::TextEdit::singleline(&mut draft.url),
                                        )
                                        .changed();
                                    ui.label("檔名");
                                    changed |= ui
                                        .add_enabled(
                                            can_edit,
                                            egui::TextEdit::singleline(&mut draft.filename),
                                        )
                                        .changed();
                                    ui.horizontal(|ui| {
                                        ui.label("資料夾");
                                        let response = ui.add_enabled(
                                            can_edit,
                                            egui::TextEdit::singleline(&mut draft.target_dir_input),
                                        );
                                        if response.lost_focus() {
                                            let path = PathBuf::from(draft.target_dir_input.trim());
                                            if path.is_dir() {
                                                draft.target_dir = path.clone();
                                                pending_last_dir = Some(path);
                                                changed = true;
                                            } else {
                                                pending_error = Some("下載目錄不存在".into());
                                            }
                                        }
                                        if ui
                                            .add_enabled(can_edit, egui::Button::new("瀏覽…"))
                                            .clicked()
                                        {
                                            if let Some(path) = rfd::FileDialog::new()
                                                .set_directory(&draft.target_dir)
                                                .pick_folder()
                                            {
                                                draft.target_dir_input = path.display().to_string();
                                                draft.target_dir = path.clone();
                                                pending_last_dir = Some(path);
                                                changed = true;
                                            }
                                        }
                                    });
                                    ui.weak("下載完成後，檔案會保存於此位置。");
                                });
                                card_frame(&columns[1]).show(&mut columns[1], |ui| {
                                    ui.heading("分段設定");
                                    ui.label("分段數");
                                    changed |= ui
                                        .add_enabled(
                                            can_edit,
                                            egui::Slider::new(&mut draft.segments, 1..=8)
                                                .text("段"),
                                        )
                                        .changed();
                                    ui.weak("分段下載可提升穩定性及速度。");
                                });
                            });
                            ui.add_space(12.0);
                            card_frame(ui).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.heading("Proxy 設定");
                                    ui.weak("只套用於此任務的下載連線");
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.add_enabled(
                                                can_edit,
                                                egui::Checkbox::new(
                                                    &mut draft.proxy.enabled,
                                                    "啟用 Proxy",
                                                ),
                                            );
                                        },
                                    );
                                });
                                ui.add_enabled_ui(can_edit, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label("協定");
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
                                        ui.label("主機");
                                        changed |= ui
                                            .text_edit_singleline(&mut draft.proxy.host)
                                            .changed();
                                        ui.label("連接埠");
                                        changed |= ui
                                            .add(
                                                egui::DragValue::new(&mut draft.proxy.port)
                                                    .range(1..=65535),
                                            )
                                            .changed();
                                    });
                                });
                                if can_edit || needs_password {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label("帳號");
                                        if can_edit {
                                            changed |= ui
                                                .text_edit_singleline(&mut draft.proxy.username)
                                                .changed();
                                        } else {
                                            ui.label(&draft.proxy.username);
                                        }
                                        ui.label("密碼");
                                        let response = ui.add(
                                            egui::TextEdit::singleline(&mut draft.password_input)
                                                .password(!draft.show_password),
                                        );
                                        draft.show_password = response.is_pointer_button_down_on();
                                        if can_edit {
                                            changed |= response.changed();
                                        }
                                    });
                                }
                                if !can_edit && !needs_password {
                                    ui.weak("下載已開始，檔名、位置及 Proxy 設定已鎖定。");
                                }
                                if needs_password {
                                    ui.label("此任務需要 Proxy 密碼才能繼續。");
                                    if ui.button("設定密碼並開始").clicked()
                                        && !draft.password_input.is_empty()
                                    {
                                        pending_password =
                                            Some((draft.id, draft.password_input.clone()));
                                        draft.password_input.clear();
                                    }
                                }
                            });
                            if !needs_password && changed {
                                pending_update = Some(draft.clone());
                            }
                        }

                        if let Some(error) = &task.error {
                            ui.add_space(12.0);
                            card_frame(ui).show(ui, |ui| {
                                ui.colored_label(
                                    ui.visuals().error_fg_color,
                                    egui::RichText::new(format!(
                                        "! {}：{}",
                                        error.summary, error.action
                                    ))
                                    .strong(),
                                );
                                if !error.diagnostic.trim().is_empty() {
                                    ui.collapsing("詳細診斷（已清理敏感資料）", |ui| {
                                        ui.monospace(&error.diagnostic);
                                    });
                                }
                            });
                        }

                        if let Some(draft) = pending_update {
                            self.send_draft(&draft);
                        }
                        if let Some(path) = pending_last_dir {
                            self.last_download_dir = path.clone();
                            if let Ok(mut shared_dir) = self.ipc_default_dir.lock() {
                                *shared_dir = path.clone();
                            }
                            let _ = self
                                .engine
                                .commands
                                .send(EngineCommand::SetLastDownloadDir(path));
                        }
                        if let Some((id, password)) = pending_password {
                            let _ = self.engine.commands.send(EngineCommand::SetProxyPassword {
                                id,
                                password: zeroize::Zeroizing::new(password),
                            });
                        }
                        if let Some(error) = pending_error {
                            self.input_error = Some(error);
                        }
                    });
            });
    }
}

fn resolve_show_task(tasks: &[TaskSnapshot], task_id: TaskId) -> Option<TaskId> {
    tasks
        .iter()
        .find(|task| task.id == task_id)
        .map(|task| task.id)
}
fn effective_start_minimized(requested: bool, tray_available: bool) -> bool {
    requested && tray_available
}

fn window_matches_process(window_process_id: u32, target_process_id: Option<u32>) -> bool {
    match target_process_id {
        Some(target_process_id) => window_process_id == target_process_id,
        None => true,
    }
}

#[cfg(windows)]
fn set_window_visibility_for_process(visible: bool, target_process_id: Option<u32>) {
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{
            EnumWindows, GW_OWNER, GetWindow, GetWindowTextW, GetWindowThreadProcessId, SW_HIDE,
            SW_SHOW, SetForegroundWindow, ShowWindow,
        },
    };
    use windows_sys::core::BOOL;

    struct WindowSearch {
        process_id: Option<u32>,
        window: HWND,
    }

    unsafe extern "system" fn find_main_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam as *mut WindowSearch) };
        let mut process_id = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut process_id);
        }
        let mut title = [0u16; 256];
        let title_length = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
        let is_main_window = title_length > 0
            && String::from_utf16_lossy(&title[..title_length as usize]) == "Curl Downloader";
        if window_matches_process(process_id, search.process_id)
            && unsafe { GetWindow(hwnd, GW_OWNER) }.is_null()
            && is_main_window
        {
            search.window = hwnd;
            return 0;
        }
        1
    }

    let mut search = WindowSearch {
        process_id: target_process_id,
        window: std::ptr::null_mut(),
    };
    unsafe {
        EnumWindows(
            Some(find_main_window),
            (&mut search as *mut WindowSearch).cast::<()>() as LPARAM,
        );
        if !search.window.is_null() {
            ShowWindow(search.window, if visible { SW_SHOW } else { SW_HIDE });
            if visible {
                SetForegroundWindow(search.window);
            }
        }
    }
}

#[cfg(windows)]
fn set_main_window_visibility(visible: bool) {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    set_window_visibility_for_process(visible, Some(unsafe { GetCurrentProcessId() }));
}

#[cfg(windows)]
pub fn focus_existing_main_window() {
    set_window_visibility_for_process(true, None);
}

#[cfg(not(windows))]
fn set_main_window_visibility(_visible: bool) {}

#[cfg(not(windows))]
pub fn focus_existing_main_window() {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowCloseAction {
    HideToTray,
    Shutdown,
}

fn window_close_action(shutting_down: bool, tray_close_requested: bool) -> WindowCloseAction {
    if shutting_down || tray_close_requested {
        WindowCloseAction::Shutdown
    } else {
        WindowCloseAction::HideToTray
    }
}

impl eframe::App for CurlDownloaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.start_minimized {
            set_main_window_visibility(false);
            self.start_minimized = false;
            self.hidden_to_tray = true;
        }
        let restored = self.pump_events(&ctx);
        if !restored
            && !self.hidden_to_tray
            && ctx.input(|input| input.viewport().minimized == Some(true))
        {
            self.hidden_to_tray = true;
            set_main_window_visibility(false);
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            if matches!(
                window_close_action(self.shutting_down, false),
                WindowCloseAction::HideToTray
            ) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.hidden_to_tray = true;
                set_main_window_visibility(false);
            }
        }
        ctx.request_repaint_after(Duration::from_millis(200));
        self.show_top_bar(ui);
        self.show_queue(ui);
        self.show_inspector(ui);
    }
}

impl Drop for CurlDownloaderApp {
    fn drop(&mut self) {
        self.ipc_stop.store(true, Ordering::Release);
        if let Some(server) = self.ipc_thread.take() {
            let _ = server.join();
        }
    }
}

fn card_frame(ui: &egui::Ui) -> egui::Frame {
    let visuals = ui.visuals();
    egui::Frame::group(ui.style())
        .fill(visuals.faint_bg_color)
        .stroke(egui::Stroke::new(
            1.0,
            visuals.widgets.noninteractive.bg_stroke.color,
        ))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(12)
}

fn task_card_frame(ui: &egui::Ui, selected: bool) -> egui::Frame {
    let visuals = ui.visuals();
    if selected {
        card_frame(ui)
            .fill(visuals.selection.bg_fill)
            .stroke(egui::Stroke::new(1.5, visuals.selection.stroke.color))
    } else {
        card_frame(ui)
    }
}

fn status_color(ui: &egui::Ui, status: TaskStatus) -> egui::Color32 {
    let visuals = ui.visuals();
    match status {
        TaskStatus::Queued | TaskStatus::Paused | TaskStatus::Cancelled => {
            visuals.weak_text_color()
        }
        TaskStatus::Probing | TaskStatus::Downloading => visuals.hyperlink_color,
        TaskStatus::Pausing | TaskStatus::Finalizing | TaskStatus::NeedsProxyPassword => {
            visuals.warn_fg_color
        }
        TaskStatus::Completed => visuals.widgets.active.fg_stroke.color,
        TaskStatus::Failed => visuals.error_fg_color,
    }
}

fn show_progress_card(ui: &mut egui::Ui, task: &TaskSnapshot) {
    card_frame(ui).show(ui, |ui| {
        ui.heading("下載進度");
        ui.horizontal(|ui| {
            draw_progress_ring(ui, progress_fraction(task), status_color(ui, task.status));
            ui.vertical(|ui| {
                ui.label("已下載 / 總大小");
                ui.label(egui::RichText::new(progress_text(task)).size(16.0).strong());
                ui.add_space(8.0);
                ui.label("目前／平均速度");
                ui.label(egui::RichText::new(speed_text(task)).size(16.0).strong());
                ui.add_space(8.0);
                ui.label("剩餘時間");
                ui.label(egui::RichText::new(format_eta(task.eta_seconds)).size(16.0));
            });
        });
        ui.colored_label(
            status_color(ui, task.status),
            egui::RichText::new(format!(
                "{} {}",
                status_icon(task.status),
                status_label(task.status)
            ))
            .strong(),
        );
    });
}

fn show_basic_info_card(ui: &mut egui::Ui, task: &TaskSnapshot) {
    card_frame(ui).show(ui, |ui| {
        ui.heading("基本資料");
        egui::Grid::new(format!("basic-info-{}", task.id))
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                info_row(ui, "狀態", status_label(task.status));
                info_row(ui, "下載工具", curl_source_label(task.curl_source));
                ui.label("來源 URL");
                ui.label(&task.original_url);
                ui.end_row();
                info_row(ui, "檔名", &task.filename);
                info_row(ui, "儲存位置", &task.target_dir.display().to_string());
            });
    });
}

fn info_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.weak(label);
    ui.label(value);
    ui.end_row();
}

fn draw_progress_ring(ui: &mut egui::Ui, fraction: f32, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(142.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let center = rect.center();
    let radius = 49.0;
    let track = ui.visuals().widgets.noninteractive.bg_stroke.color;
    painter.circle_stroke(center, radius, egui::Stroke::new(10.0, track));
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        let points = (0..=64)
            .map(|step| {
                let angle = -std::f32::consts::FRAC_PI_2
                    + std::f32::consts::TAU * fraction * step as f32 / 64.0;
                center + egui::Vec2::angled(angle) * radius
            })
            .collect::<Vec<_>>();
        painter.add(egui::Shape::line(points, egui::Stroke::new(10.0, color)));
    }
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        format!("{:.0}%", fraction * 100.0),
        egui::FontId::proportional(25.0),
        ui.visuals().text_color(),
    );
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueGroup {
    Active,
    Completed,
}

fn status_icon(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "○",
        TaskStatus::Probing => "⌁",
        TaskStatus::Downloading => "↓",
        TaskStatus::Pausing | TaskStatus::Paused => "Ⅱ",
        TaskStatus::NeedsProxyPassword => "?",
        TaskStatus::Finalizing => "…",
        TaskStatus::Completed => "✓",
        TaskStatus::Failed => "!",
        TaskStatus::Cancelled => "×",
    }
}

fn queue_group(status: TaskStatus) -> QueueGroup {
    match status {
        TaskStatus::Completed | TaskStatus::Cancelled => QueueGroup::Completed,
        _ => QueueGroup::Active,
    }
}

fn format_batch_proxy_result(applied: usize, skipped: usize) -> String {
    match (applied, skipped) {
        (0, skipped) => format!("沒有任務套用 Proxy，略過 {skipped} 個不可修改任務"),
        (applied, 0) => format!("已設定 {applied} 個任務"),
        (applied, skipped) => {
            format!("已設定 {applied} 個任務，略過 {skipped} 個不可修改任務")
        }
    }
}

fn can_edit_proxy_in_bulk(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Queued
            | TaskStatus::Paused
            | TaskStatus::Failed
            | TaskStatus::NeedsProxyPassword
    )
}

fn proxy_from_snapshot(task: &TaskSnapshot) -> ProxySettings {
    ProxySettings {
        enabled: task.proxy.enabled,
        protocol: task.proxy.protocol,
        host: task.proxy.host.clone(),
        port: task.proxy.port,
        username: task.proxy.username.clone(),
        password: None,
        requires_password: task.proxy.requires_password,
    }
}

fn draft_proxy_matches_snapshot(draft: &TaskDraft, task: &TaskSnapshot) -> bool {
    draft.proxy.enabled == task.proxy.enabled
        && draft.proxy.protocol == task.proxy.protocol
        && draft.proxy.host == task.proxy.host
        && draft.proxy.port == task.proxy.port
        && draft.proxy.username == task.proxy.username
        && draft.proxy.requires_password == task.proxy.requires_password
}

fn curl_source_label(source: CurlSource) -> &'static str {
    match source {
        CurlSource::NotStarted => "尚未啟動",
        CurlSource::Local => "本機 curl",
        CurlSource::Embedded => "內置 curl",
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

    #[test]
    fn labels_curl_source_for_the_current_task() {
        assert_eq!(curl_source_label(CurlSource::NotStarted), "尚未啟動");
        assert_eq!(curl_source_label(CurlSource::Local), "本機 curl");
        assert_eq!(curl_source_label(CurlSource::Embedded), "內置 curl");
    }

    #[test]
    fn only_editable_tasks_can_receive_bulk_proxy_settings() {
        assert!(can_edit_proxy_in_bulk(TaskStatus::Queued));
        assert!(can_edit_proxy_in_bulk(TaskStatus::Paused));
        assert!(can_edit_proxy_in_bulk(TaskStatus::Failed));
        assert!(can_edit_proxy_in_bulk(TaskStatus::NeedsProxyPassword));
        assert!(!can_edit_proxy_in_bulk(TaskStatus::Probing));
        assert!(!can_edit_proxy_in_bulk(TaskStatus::Downloading));
        assert!(!can_edit_proxy_in_bulk(TaskStatus::Completed));
        assert!(!can_edit_proxy_in_bulk(TaskStatus::Cancelled));
    }

    #[test]
    fn maps_statuses_to_consistent_queue_icons_and_groups() {
        assert_eq!(status_icon(TaskStatus::Downloading), "↓");
        assert_eq!(status_icon(TaskStatus::Failed), "!");
        assert_eq!(status_icon(TaskStatus::Completed), "✓");
        assert_eq!(status_icon(TaskStatus::Cancelled), "×");
        assert_eq!(queue_group(TaskStatus::Downloading), QueueGroup::Active);
        assert_eq!(queue_group(TaskStatus::Failed), QueueGroup::Active);
        assert_eq!(queue_group(TaskStatus::Completed), QueueGroup::Completed);
        assert_eq!(queue_group(TaskStatus::Cancelled), QueueGroup::Completed);
    }

    #[test]
    fn formats_bulk_proxy_result_for_visible_feedback() {
        assert_eq!(format_batch_proxy_result(2, 0), "已設定 2 個任務");
        assert_eq!(
            format_batch_proxy_result(1, 1),
            "已設定 1 個任務，略過 1 個不可修改任務"
        );
    }

    #[test]
    fn show_task_command_selects_existing_task_and_ignores_unknown_task() {
        let tasks = vec![test_snapshot(7, TaskStatus::Downloading)];
        assert_eq!(resolve_show_task(&tasks, 7), Some(7));
        assert_eq!(resolve_show_task(&tasks, 999), None);
    }
    #[test]
    fn tray_failure_never_leaves_startup_invisible() {
        assert!(effective_start_minimized(true, true));
        assert!(!effective_start_minimized(true, false));
        assert!(!effective_start_minimized(false, false));
    }

    #[test]
    fn duplicate_gui_launch_can_target_existing_process_window() {
        assert!(window_matches_process(42, None));
        assert!(window_matches_process(42, Some(42)));
        assert!(!window_matches_process(42, Some(7)));
    }
    #[test]
    fn ordinary_window_close_hides_to_tray_instead_of_shutting_down() {
        assert_eq!(
            window_close_action(false, false),
            WindowCloseAction::HideToTray
        );
    }

    #[test]
    fn tray_close_is_the_explicit_shutdown_path() {
        assert_eq!(
            window_close_action(false, true),
            WindowCloseAction::Shutdown
        );
        assert_eq!(
            window_close_action(true, false),
            WindowCloseAction::Shutdown
        );
    }

    fn test_snapshot(id: TaskId, status: TaskStatus) -> TaskSnapshot {
        TaskSnapshot {
            id,
            original_url: "https://example.test/file.bin".into(),
            effective_url: None,
            filename: "file.bin".into(),
            target_dir: PathBuf::from("C:\\Downloads"),
            status,
            requested_segments: 1,
            actual_segments: 1,
            downloaded: 0,
            total_size: None,
            range_support: crate::model::RangeSupport::Unknown,
            current_bps: 0.0,
            average_bps: 0.0,
            eta_seconds: None,
            active_millis: 0,
            created_unix_ms: 0,
            completed_unix_ms: None,
            proxy: crate::model::ProxySnapshot {
                enabled: false,
                protocol: ProxyProtocol::Http,
                host: String::new(),
                port: 8080,
                username: String::new(),
                requires_password: false,
            },
            error: None,
            curl_source: CurlSource::NotStarted,
        }
    }
}
