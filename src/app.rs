pub use crate::window_control::focus_existing_main_window;
use crate::{
    controller::{self, AppEvent, ControllerCommand, ControllerHandle, LifecycleState},
    download::spawn_engine,
    ipc,
    model::{
        CURRENT_SCHEMA_VERSION, CurlSource, EngineCommand, FileDecision, GlobalSettings, NewTask,
        PersistedState, ProxyProtocol, ProxySettings, SegmentSnapshot, TaskId, TaskSnapshot,
        TaskStatus,
    },
    shell_foreground, storage, tray,
    window_control::{EguiMainWindow, MainWindowControl},
};
use eframe::egui;
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InspectorTab {
    Overview,
    Segments,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UrlDetailField {
    Original,
    Effective,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExpandedUrlKey {
    task_id: TaskId,
    field: UrlDetailField,
}

fn toggle_expanded_url(
    current: Option<ExpandedUrlKey>,
    key: ExpandedUrlKey,
) -> Option<ExpandedUrlKey> {
    if current == Some(key) {
        None
    } else {
        Some(key)
    }
}

fn is_url_expanded(current: Option<ExpandedUrlKey>, key: ExpandedUrlKey) -> bool {
    current == Some(key)
}

fn url_detail_wrap_mode(
    current: Option<ExpandedUrlKey>,
    key: ExpandedUrlKey,
) -> egui::TextWrapMode {
    if is_url_expanded(current, key) {
        egui::TextWrapMode::Wrap
    } else {
        egui::TextWrapMode::Truncate
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverviewCard {
    Progress,
    Basic,
    Storage,
    Proxy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverviewLayout {
    TwoColumn {
        left: [OverviewCard; 2],
        right: [OverviewCard; 1],
        below: [OverviewCard; 1],
    },
    OneColumn([OverviewCard; 4]),
}

// Two cards need roughly 400 px each plus the column gap and card margins;
// using a conservative threshold prevents the narrow inspector from drawing
// controls on top of one another.
const INSPECTOR_TWO_COLUMN_MIN_WIDTH: f32 = 840.0;

fn overview_layout(width: f32) -> OverviewLayout {
    if width >= INSPECTOR_TWO_COLUMN_MIN_WIDTH {
        OverviewLayout::TwoColumn {
            left: [OverviewCard::Progress, OverviewCard::Storage],
            right: [OverviewCard::Basic],
            below: [OverviewCard::Proxy],
        }
    } else {
        OverviewLayout::OneColumn([
            OverviewCard::Progress,
            OverviewCard::Basic,
            OverviewCard::Storage,
            OverviewCard::Proxy,
        ])
    }
}

fn format_segment_timestamp(timestamp: Option<u64>) -> String {
    let Some(timestamp) = timestamp else {
        return "未記錄".into();
    };
    let parts = unix_millis_parts(timestamp);
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::{
            Foundation::SYSTEMTIME, System::Time::SystemTimeToTzSpecificLocalTime,
        };
        let utc = SYSTEMTIME {
            wYear: parts.0 as u16,
            wMonth: parts.1,
            wDayOfWeek: 0,
            wDay: parts.2,
            wHour: parts.3,
            wMinute: parts.4,
            wSecond: parts.5,
            wMilliseconds: parts.6,
        };
        let mut local = utc;
        if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } != 0 {
            return format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
                local.wYear,
                local.wMonth,
                local.wDay,
                local.wHour,
                local.wMinute,
                local.wSecond,
                local.wMilliseconds
            );
        }
    }
    format_timestamp_parts(parts)
}

fn unix_millis_parts(timestamp: u64) -> (i32, u16, u16, u16, u16, u16, u16) {
    let total_seconds = timestamp / 1_000;
    let days = (total_seconds / 86_400) as i64;
    let seconds_of_day = total_seconds % 86_400;
    let milliseconds = (timestamp % 1_000) as u16;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (
        year as i32,
        month as u16,
        day as u16,
        (seconds_of_day / 3_600) as u16,
        ((seconds_of_day % 3_600) / 60) as u16,
        (seconds_of_day % 60) as u16,
        milliseconds,
    )
}

fn format_timestamp_parts(parts: (i32, u16, u16, u16, u16, u16, u16)) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} UTC",
        parts.0, parts.1, parts.2, parts.3, parts.4, parts.5, parts.6
    )
}

fn format_segment_duration(segment: &SegmentSnapshot) -> String {
    if segment.started_unix_ms.is_none() {
        return "未記錄".into();
    }
    format_active_duration(segment.active_millis)
}

fn format_active_duration(active_millis: u64) -> String {
    let total_seconds = active_millis / 1_000;
    if total_seconds >= 3_600 {
        format!(
            "{}小時 {:02}分 {:02}秒",
            total_seconds / 3_600,
            (total_seconds / 60) % 60,
            total_seconds % 60
        )
    } else if total_seconds >= 60 {
        format!("{}分 {:02}秒", total_seconds / 60, total_seconds % 60)
    } else {
        format!("{}秒", total_seconds)
    }
}

fn format_segment_average_speed(segment: &SegmentSnapshot) -> String {
    if segment.started_unix_ms.is_none() || segment.active_millis == 0 {
        return "未記錄".into();
    }
    let bps = segment.downloaded as f64 * 1_000.0 / segment.active_millis as f64;
    format_speed(bps)
}

fn segment_status_label(task_status: TaskStatus, segment: &SegmentSnapshot) -> &'static str {
    let expected = segment.end.saturating_sub(segment.start).saturating_add(1);
    if segment.completed_unix_ms.is_some()
        || (!segment.active && expected > 0 && segment.downloaded >= expected)
    {
        return "已完成";
    }
    if segment.active {
        return "下載中";
    }
    match task_status {
        TaskStatus::Paused | TaskStatus::Pausing => "已暫停",
        TaskStatus::Failed => "失敗",
        TaskStatus::Cancelled => "已取消",
        TaskStatus::Queued => "排隊中",
        TaskStatus::Probing => "探測中",
        TaskStatus::Downloading => "等待中",
        TaskStatus::Finalizing => "整合中",
        TaskStatus::NeedsProxyPassword => "等待密碼",
        TaskStatus::AwaitingFileDecision => "等待檔案決定",
        TaskStatus::Completed => "未完成",
    }
}
pub struct CurlDownloaderApp {
    engine_commands: Sender<EngineCommand>,
    controller: ControllerHandle,
    controller_state: controller::SharedControllerState,
    controller_events: Receiver<AppEvent>,
    window_control: Arc<dyn MainWindowControl>,
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
    ipc_stop: Arc<AtomicBool>,
    ipc_default_dir: Arc<Mutex<PathBuf>>,
    start_minimized: bool,
    hidden_to_tray: bool,
    inspector_tab: InspectorTab,
    expanded_url: Option<ExpandedUrlKey>,
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
        let manual_stop_path = storage::manual_stop_path(&state_path);
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
                        schema_version: CURRENT_SCHEMA_VERSION,
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
                    schema_version: CURRENT_SCHEMA_VERSION,
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
        let (engine_commands, engine_events) = engine.into_channels();
        let ipc_stop = Arc::new(AtomicBool::new(false));
        let ipc_default_dir = Arc::new(Mutex::new(last_download_dir.clone()));
        let mut start_minimized = start_minimized;
        let (tray, tray_receiver) = match tray::TrayController::create() {
            Ok(value) => value,
            Err(error) => {
                eprintln!("Windows 系統匣初始化失敗：{error}");
                start_minimized = effective_start_minimized(start_minimized, false);
                tray::TrayController::disabled()
            }
        };
        let window_control: Arc<dyn MainWindowControl> = if start_minimized {
            EguiMainWindow::new_minimized(cc.egui_ctx.clone())
        } else {
            EguiMainWindow::new(cc.egui_ctx.clone())
        };
        let initial_lifecycle = if start_minimized {
            LifecycleState::RunningHidden
        } else {
            LifecycleState::RunningVisible
        };
        let mut controller = controller::spawn_controller(
            initial_lifecycle,
            engine_commands.clone(),
            engine_events,
            tray,
            tray_receiver,
            Arc::clone(&window_control),
            manual_stop_path.clone(),
            Arc::clone(&ipc_stop),
        )
        .unwrap_or_else(|error| panic!("無法啟動背景控制器：{error}"));
        let controller_state = controller.state();
        let controller_events = controller.take_app_events();
        let ipc_thread = Some(ipc::spawn_server(
            engine_commands.clone(),
            Arc::clone(&ipc_default_dir),
            controller_state.clone(),
            controller.commands(),
            Arc::clone(&ipc_stop),
        ));
        controller_state.mark_ui_ready();
        let _ = controller_state.wait_ready(Duration::from_secs(2));

        cc.egui_ctx.set_fonts(chinese_font_definitions());
        cc.egui_ctx.set_theme(egui::ThemePreference::System);
        let tasks = controller_state.tasks();
        Self {
            engine_commands,
            controller,
            controller_state,
            controller_events,
            window_control,
            tasks: tasks.clone(),
            selected: tasks
                .iter()
                .max_by_key(|task| (task.created_unix_ms, task.id))
                .map(|task| task.id),
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
            ipc_stop,
            ipc_default_dir,
            start_minimized,
            hidden_to_tray: start_minimized,
            inspector_tab: InspectorTab::Overview,
            expanded_url: None,
            ipc_thread,
        }
    }

    fn apply_controller_events(&mut self, ctx: &egui::Context) -> bool {
        let mut restored = false;
        while let Ok(event) = self.controller_events.try_recv() {
            match event {
                AppEvent::SnapshotChanged => self.apply_tasks(self.controller_state.tasks()),
                AppEvent::ShowWindow => {
                    restored = self.restore_window(ctx) || restored;
                }
                AppEvent::ShowTask { task_id } => {
                    self.apply_tasks(self.controller_state.tasks());
                    if let Some(selected) = resolve_show_task(&self.tasks, task_id) {
                        self.selected = Some(selected);
                        self.draft = None;
                    }
                    restored = self.restore_window(ctx) || restored;
                }
                AppEvent::BatchProxyApplied { applied, skipped } => {
                    self.batch_proxy_message = Some(format_batch_proxy_result(applied, skipped));
                }
                AppEvent::Fatal(message) => self.fatal = Some(message),
            }
        }
        restored
    }

    fn apply_tasks(&mut self, tasks: Vec<TaskSnapshot>) {
        let previous_ids = self
            .tasks
            .iter()
            .map(|task| task.id)
            .collect::<HashSet<_>>();
        let newest_added = tasks
            .iter()
            .filter(|task| !previous_ids.contains(&task.id))
            .max_by_key(|task| (task.created_unix_ms, task.id))
            .map(|task| task.id);
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
        if let Some(newest_added) = newest_added {
            self.selected = Some(newest_added);
            self.draft = None;
        } else if self.selected.is_none() {
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

    fn restore_window(&mut self, ctx: &egui::Context) -> bool {
        self.hidden_to_tray = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        true
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
            .engine_commands
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
        let _ = self.engine_commands.send(EngineCommand::UpdateDraft {
            id: draft.id,
            url: draft.url.clone(),
            filename: draft.filename.clone(),
            target_dir: draft.target_dir.clone(),
            requested_segments: draft.segments,
            proxy,
        });
    }

    fn resolve_file_conflict(&mut self, id: TaskId, decision: FileDecision) {
        let (response, _ignored) = std::sync::mpsc::channel();
        let _ = self
            .engine_commands
            .send(EngineCommand::ResolveFileConflict {
                id,
                decision,
                response,
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
        let _ = self.engine_commands.send(EngineCommand::Add(NewTask {
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
        let _ = self.engine_commands.send(EngineCommand::AddBatch(tasks));
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
                            .engine_commands
                            .send(EngineCommand::SetMaxProcesses(self.max_processes));
                    }
                    if ui.button("▶ 開始全部").clicked() {
                        if let Some(draft) = self.draft.clone() {
                            self.send_draft(&draft);
                        }
                        let _ = self.engine_commands.send(EngineCommand::StartAll);
                    }
                    if ui.button("Ⅱ 暫停全部").clicked() {
                        let _ = self.engine_commands.send(EngineCommand::PauseAll);
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
                        let _ = self.engine_commands.send(EngineCommand::ClearHistory);
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
                if self.controller_state.lifecycle() == LifecycleState::ShuttingDown {
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
                if task.status == TaskStatus::AwaitingFileDecision {
                    if ui.button("覆蓋").clicked() {
                        self.resolve_file_conflict(task.id, FileDecision::Overwrite);
                    }
                    if ui.button("取消任務").clicked() {
                        self.resolve_file_conflict(task.id, FileDecision::Cancel);
                    }
                }
                if matches!(
                    task.status,
                    TaskStatus::Queued | TaskStatus::Paused | TaskStatus::Failed
                ) && ui.button("開始").clicked()
                {
                    self.flush_draft(task.id);
                    let _ = self.engine_commands.send(EngineCommand::Start(task.id));
                }
                if matches!(task.status, TaskStatus::Probing | TaskStatus::Downloading)
                    && ui.button("暫停").clicked()
                {
                    let _ = self.engine_commands.send(EngineCommand::Pause(task.id));
                }
                if !matches!(
                    task.status,
                    TaskStatus::Completed
                        | TaskStatus::Cancelled
                        | TaskStatus::AwaitingFileDecision
                ) && ui.button("取消").clicked()
                {
                    let _ = self.engine_commands.send(EngineCommand::Cancel(task.id));
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
                    let _ = self.engine_commands.send(EngineCommand::Remove(task.id));
                }
            });
        });
        if open_task {
            self.selected = Some(task.id);
            self.draft = None;
        }
    }

    fn show_segments_tab(&mut self, ui: &mut egui::Ui, task: &TaskSnapshot) -> Option<TaskDraft> {
        show_segment_history(ui, task);
        let can_edit = matches!(
            task.status,
            TaskStatus::Queued | TaskStatus::Paused | TaskStatus::Failed
        );
        let mut pending_update = None;
        if let Some(draft) = self.draft.as_mut() {
            let mut changed = false;
            ui.add_space(12.0);
            card_frame(ui).show(ui, |ui| {
                ui.heading("分段下載設定");
                changed |= ui
                    .add_enabled(
                        can_edit,
                        egui::Slider::new(&mut draft.segments, 1..=8).text("段"),
                    )
                    .changed();
                ui.weak("下載開始後會固定分段數；完成後仍會保留每段歷史資料。");
                if !can_edit {
                    ui.weak("下載已開始，分段數已鎖定。");
                }
            });
            if changed {
                pending_update = Some(draft.clone());
            }
        }
        pending_update
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
                        ui.add(
                            egui::Label::new(egui::RichText::new(&task.filename).heading()).wrap(),
                        )
                        .on_hover_text(&task.filename);
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
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(
                        &mut self.inspector_tab,
                        InspectorTab::Overview,
                        "任務總覽",
                    );
                    ui.selectable_value(
                        &mut self.inspector_tab,
                        InspectorTab::Segments,
                        "分段設定",
                    );
                });
                ui.separator();

                egui::ScrollArea::vertical()
                    .id_salt("inspector-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.inspector_tab == InspectorTab::Segments {
                            if let Some(draft) = self.show_segments_tab(ui, &task) {
                                self.send_draft(&draft);
                            }
                            return;
                        }

                        let mut pending_update = None;
                        let mut pending_last_dir = None;
                        let mut pending_password = None;
                        let mut pending_error = None;
                        if task.status == TaskStatus::AwaitingFileDecision {
                            ui.add_space(12.0);
                            card_frame(ui).show(ui, |ui| {
                                ui.heading("檔案已存在");
                                ui.label("目標位置已有同名檔案，請選擇如何處理。");
                                ui.horizontal_wrapped(|ui| {
                                    if ui.button("覆蓋（完成後替換）").clicked() {
                                        self.resolve_file_conflict(
                                            task.id,
                                            FileDecision::Overwrite,
                                        );
                                    }
                                    if ui.button("取消任務").clicked() {
                                        self.resolve_file_conflict(task.id, FileDecision::Cancel);
                                    }
                                });
                            });
                        }
                        let needs_password = task.status == TaskStatus::NeedsProxyPassword;
                        let can_edit = matches!(
                            task.status,
                            TaskStatus::Queued | TaskStatus::Paused | TaskStatus::Failed
                        );

                        if let Some(draft) = self.draft.as_mut() {
                            let overview = show_task_overview(
                                ui,
                                &task,
                                draft,
                                &mut self.expanded_url,
                                can_edit,
                                needs_password,
                            );
                            pending_update = if overview.changed {
                                Some(draft.clone())
                            } else {
                                None
                            };
                            pending_last_dir = overview.last_download_dir;
                            pending_password = overview.password;
                            pending_error = overview.error;
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
                                        ui.add(
                                            egui::Label::new(&error.diagnostic)
                                                .wrap()
                                                .selectable(true),
                                        );
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
                                .engine_commands
                                .send(EngineCommand::SetLastDownloadDir(path));
                        }
                        if let Some((id, password)) = pending_password {
                            let _ = self.engine_commands.send(EngineCommand::SetProxyPassword {
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
            self.window_control.hide();
            self.start_minimized = false;
            self.hidden_to_tray = true;
            let _ = self
                .controller
                .commands()
                .send(ControllerCommand::WindowHidden);
        }
        let restored = self.apply_controller_events(&ctx);
        if self.hidden_to_tray && !restored {
            self.window_control.hide();
        }
        if !restored
            && !self.hidden_to_tray
            && ctx.input(|input| input.viewport().minimized == Some(true))
        {
            self.hidden_to_tray = true;
            self.window_control.hide();
            let _ = self
                .controller
                .commands()
                .send(ControllerCommand::WindowHidden);
        }
        let lifecycle = self.controller_state.lifecycle();
        if lifecycle == LifecycleState::Stopped {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            if matches!(
                window_close_action(
                    matches!(
                        lifecycle,
                        LifecycleState::ShuttingDown | LifecycleState::Stopped
                    ),
                    false,
                ),
                WindowCloseAction::HideToTray
            ) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.hidden_to_tray = true;
                self.window_control.hide();
                let _ = self
                    .controller
                    .commands()
                    .send(ControllerCommand::WindowHidden);
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
        self.controller.shutdown_internal();
        self.controller.join();
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
        TaskStatus::Pausing
        | TaskStatus::Finalizing
        | TaskStatus::NeedsProxyPassword
        | TaskStatus::AwaitingFileDecision => visuals.warn_fg_color,
        TaskStatus::Completed => visuals.widgets.active.fg_stroke.color,
        TaskStatus::Failed => visuals.error_fg_color,
    }
}

#[derive(Default)]
struct StorageCardOutcome {
    changed: bool,
    last_download_dir: Option<PathBuf>,
    error: Option<String>,
}

#[derive(Default)]
struct ProxyCardOutcome {
    changed: bool,
    password: Option<(TaskId, String)>,
}

#[derive(Default)]
struct OverviewEditOutcome {
    changed: bool,
    last_download_dir: Option<PathBuf>,
    password: Option<(TaskId, String)>,
    error: Option<String>,
}

impl OverviewEditOutcome {
    fn merge_storage(&mut self, outcome: StorageCardOutcome) {
        self.changed |= outcome.changed;
        self.last_download_dir = outcome.last_download_dir;
        self.error = outcome.error;
    }

    fn merge_proxy(&mut self, outcome: ProxyCardOutcome) {
        self.changed |= outcome.changed;
        self.password = outcome.password;
    }
}

fn show_task_overview(
    ui: &mut egui::Ui,
    task: &TaskSnapshot,
    draft: &mut TaskDraft,
    expanded_url: &mut Option<ExpandedUrlKey>,
    can_edit: bool,
    needs_password: bool,
) -> OverviewEditOutcome {
    let mut outcome = OverviewEditOutcome::default();
    match overview_layout(ui.available_width()) {
        OverviewLayout::TwoColumn { .. } => {
            let mut storage = StorageCardOutcome::default();
            ui.columns(2, |columns| {
                show_progress_card(&mut columns[0], task);
                columns[0].add_space(12.0);
                storage = show_storage_card(&mut columns[0], draft, can_edit);
                show_basic_info_card(&mut columns[1], task, expanded_url);
            });
            ui.add_space(12.0);
            let proxy = show_proxy_card(ui, draft, can_edit, needs_password);
            outcome.merge_storage(storage);
            outcome.merge_proxy(proxy);
        }
        OverviewLayout::OneColumn(_) => {
            show_progress_card(ui, task);
            ui.add_space(12.0);
            show_basic_info_card(ui, task, expanded_url);
            ui.add_space(12.0);
            let storage = show_storage_card(ui, draft, can_edit);
            ui.add_space(12.0);
            let proxy = show_proxy_card(ui, draft, can_edit, needs_password);
            outcome.merge_storage(storage);
            outcome.merge_proxy(proxy);
        }
    }
    outcome
}

fn show_storage_card(
    ui: &mut egui::Ui,
    draft: &mut TaskDraft,
    can_edit: bool,
) -> StorageCardOutcome {
    let mut changed = false;
    let mut last_download_dir = None;
    let mut error = None;
    card_frame(ui).show(ui, |ui| {
        ui.heading("儲存位置");
        ui.label("來源 URL");
        changed |= ui
            .add_enabled(can_edit, egui::TextEdit::singleline(&mut draft.url))
            .changed();
        ui.label("檔名");
        changed |= ui
            .add_enabled(can_edit, egui::TextEdit::singleline(&mut draft.filename))
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
                    last_download_dir = Some(path);
                    changed = true;
                } else {
                    error = Some("下載目錄不存在".into());
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
                    last_download_dir = Some(path);
                    changed = true;
                }
            }
        });
        ui.weak("下載完成後，檔案會保存於此位置。");
    });
    StorageCardOutcome {
        changed,
        last_download_dir,
        error,
    }
}

fn show_proxy_card(
    ui: &mut egui::Ui,
    draft: &mut TaskDraft,
    can_edit: bool,
    needs_password: bool,
) -> ProxyCardOutcome {
    let mut changed = false;
    let mut password = None;
    card_frame(ui).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Proxy 設定");
            ui.weak("只套用於此任務的下載連線");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled(
                    can_edit,
                    egui::Checkbox::new(&mut draft.proxy.enabled, "啟用 Proxy"),
                );
            });
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
                changed |= ui.text_edit_singleline(&mut draft.proxy.host).changed();
                ui.label("連接埠");
                changed |= ui
                    .add(egui::DragValue::new(&mut draft.proxy.port).range(1..=65535))
                    .changed();
            });
        });
        if can_edit || needs_password {
            ui.horizontal_wrapped(|ui| {
                ui.label("帳號");
                if can_edit {
                    changed |= ui.text_edit_singleline(&mut draft.proxy.username).changed();
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
            if ui.button("設定密碼並開始").clicked() && !draft.password_input.is_empty() {
                password = Some((draft.id, draft.password_input.clone()));
                draft.password_input.clear();
            }
        }
    });
    ProxyCardOutcome { changed, password }
}

fn show_segment_history(ui: &mut egui::Ui, task: &TaskSnapshot) {
    ui.heading("分段下載歷史");
    ui.weak(format!(
        "已保存 {} 段；每段的範圍、進度、活動時間及平均速度會隨任務保存。",
        task.segments.len()
    ));
    ui.add_space(8.0);
    if task.segments.is_empty() {
        card_frame(ui).show(ui, |ui| {
            ui.weak("此任務沒有可顯示的分段歷史。舊版本未保存的資料會標示為未記錄。");
        });
        return;
    }

    for segment in &task.segments {
        let range = if segment.end >= segment.start {
            format!("位元組 {}–{}", segment.start, segment.end)
        } else {
            "位元組範圍未記錄".into()
        };
        let progress = format_segment_progress(task, segment);
        let status = segment_status_label(task.status, segment);
        card_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(
                    status_color(ui, task.status),
                    egui::RichText::new(format!("第 {} 段", segment.index + 1)).strong(),
                );
                ui.weak(status);
            });
            ui.add_space(4.0);
            show_detail_value(ui, "範圍", &range);
            show_detail_value(ui, "進度", &progress);
            show_detail_value(
                ui,
                "開始時間",
                &format_segment_timestamp(segment.started_unix_ms),
            );
            show_detail_value(
                ui,
                "完成時間",
                &format_segment_timestamp(segment.completed_unix_ms),
            );
            show_detail_value(ui, "活動下載時間", &format_segment_duration(segment));
            show_detail_value(ui, "平均速度", &format_segment_average_speed(segment));
        });
        ui.add_space(8.0);
    }
}

fn format_segment_progress(task: &TaskSnapshot, segment: &SegmentSnapshot) -> String {
    match segment_expected_size(task, segment) {
        Some(size) => format_progress_text(segment.downloaded, Some(size)),
        None if segment.downloaded == 0 => "尚未開始  ·  總大小未知".into(),
        None => format!("{}  ·  總大小未知", format_bytes(segment.downloaded)),
    }
}

fn segment_expected_size(task: &TaskSnapshot, segment: &SegmentSnapshot) -> Option<u64> {
    if task.total_size.is_some() && segment.end >= segment.start {
        return Some(segment.end - segment.start + 1);
    }
    if segment.completed_unix_ms.is_some() && segment.downloaded > 0 {
        return Some(segment.downloaded);
    }
    None
}

fn show_detail_value(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.weak(label);
    ui.add(egui::Label::new(value).wrap().selectable(true));
}
fn show_url_detail_value(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    key: ExpandedUrlKey,
    expanded_url: &mut Option<ExpandedUrlKey>,
) {
    ui.weak(label);
    let response = ui
        .add(
            egui::Label::new(value)
                .wrap_mode(url_detail_wrap_mode(*expanded_url, key))
                .selectable(true)
                .sense(egui::Sense::click()),
        )
        .on_hover_text(value);
    if response.clicked() {
        *expanded_url = toggle_expanded_url(*expanded_url, key);
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

fn show_basic_info_card(
    ui: &mut egui::Ui,
    task: &TaskSnapshot,
    expanded_url: &mut Option<ExpandedUrlKey>,
) {
    card_frame(ui).show(ui, |ui| {
        ui.heading("基本資料");
        show_detail_value(ui, "狀態", status_label(task.status));
        show_detail_value(ui, "下載工具", curl_source_label(task.curl_source));
        show_url_detail_value(
            ui,
            "來源 URL",
            &task.original_url,
            ExpandedUrlKey {
                task_id: task.id,
                field: UrlDetailField::Original,
            },
            expanded_url,
        );
        if let Some(effective_url) = &task.effective_url {
            if effective_url != &task.original_url {
                show_url_detail_value(
                    ui,
                    "實際 URL",
                    effective_url,
                    ExpandedUrlKey {
                        task_id: task.id,
                        field: UrlDetailField::Effective,
                    },
                    expanded_url,
                );
            }
        }
        show_detail_value(ui, "檔名", &task.filename);
        show_detail_value(ui, "儲存位置", &task.target_dir.display().to_string());
        show_detail_value(
            ui,
            "分段",
            &format!(
                "要求 {} 段／實際 {} 段",
                task.requested_segments, task.actual_segments
            ),
        );
        show_detail_value(ui, "Range 支援", range_support_label(task.range_support));
        show_detail_value(
            ui,
            "建立時間",
            &format_segment_timestamp(Some(task.created_unix_ms)),
        );
        show_detail_value(
            ui,
            "完成時間",
            &format_segment_timestamp(task.completed_unix_ms),
        );
    });
}

fn range_support_label(range_support: crate::model::RangeSupport) -> &'static str {
    match range_support {
        crate::model::RangeSupport::Unknown => "未知",
        crate::model::RangeSupport::Supported => "支援",
        crate::model::RangeSupport::Unsupported => "不支援",
    }
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
        TaskStatus::AwaitingFileDecision => "等待檔案決定",
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
        TaskStatus::AwaitingFileDecision => "!",
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
    let _ = shell_foreground::open_folder_foreground(location_directory(&path));
}

#[cfg(not(target_os = "windows"))]
fn open_location(_path: PathBuf) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SegmentSnapshot, TaskOrigin};

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

    #[test]
    fn wide_overview_uses_fixed_semantic_columns() {
        assert_eq!(
            overview_layout(840.0),
            OverviewLayout::TwoColumn {
                left: [OverviewCard::Progress, OverviewCard::Storage],
                right: [OverviewCard::Basic],
                below: [OverviewCard::Proxy],
            }
        );
    }

    #[test]
    fn narrow_overview_uses_stable_single_column_order() {
        assert_eq!(
            overview_layout(839.9),
            OverviewLayout::OneColumn([
                OverviewCard::Progress,
                OverviewCard::Basic,
                OverviewCard::Storage,
                OverviewCard::Proxy,
            ])
        );
    }
    #[test]
    fn url_detail_is_collapsed_by_default_and_toggles_on_click() {
        let key = ExpandedUrlKey {
            task_id: 42,
            field: UrlDetailField::Original,
        };

        assert!(!is_url_expanded(None, key));
        assert_eq!(
            url_detail_wrap_mode(None, key),
            egui::TextWrapMode::Truncate
        );

        let expanded = toggle_expanded_url(None, key);
        assert_eq!(expanded, Some(key));
        assert!(is_url_expanded(expanded, key));
        assert_eq!(
            url_detail_wrap_mode(expanded, key),
            egui::TextWrapMode::Wrap
        );

        assert_eq!(toggle_expanded_url(expanded, key), None);
        assert!(!is_url_expanded(None, key));
    }

    #[test]
    fn expanded_url_state_is_scoped_to_task_and_url_field() {
        let source = ExpandedUrlKey {
            task_id: 7,
            field: UrlDetailField::Original,
        };
        let effective = ExpandedUrlKey {
            task_id: 7,
            field: UrlDetailField::Effective,
        };
        let other_task = ExpandedUrlKey {
            task_id: 8,
            field: UrlDetailField::Original,
        };

        let expanded = toggle_expanded_url(None, source);
        assert!(is_url_expanded(expanded, source));
        assert!(!is_url_expanded(expanded, effective));
        assert!(!is_url_expanded(expanded, other_task));

        let other_field = toggle_expanded_url(expanded, effective);
        assert_eq!(other_field, Some(effective));
        assert!(!is_url_expanded(other_field, source));
        assert!(is_url_expanded(other_field, effective));
    }
    #[test]
    fn legacy_segment_history_displays_missing_timing_as_unrecorded() {
        let segment = SegmentSnapshot {
            index: 0,
            start: 0,
            end: 99,
            downloaded: 100,
            started_unix_ms: None,
            completed_unix_ms: None,
            active_millis: 0,
            active: false,
        };
        assert_eq!(format_segment_timestamp(None), "未記錄");
        assert_eq!(format_segment_duration(&segment), "未記錄");
        assert_eq!(format_segment_average_speed(&segment), "未記錄");
    }

    #[test]
    fn segment_status_prefers_completed_then_active_state() {
        let mut segment = SegmentSnapshot {
            index: 0,
            start: 0,
            end: 99,
            downloaded: 100,
            started_unix_ms: Some(1_000),
            completed_unix_ms: Some(2_000),
            active_millis: 1_000,
            active: true,
        };
        assert_eq!(
            segment_status_label(TaskStatus::Downloading, &segment),
            "已完成"
        );

        segment.completed_unix_ms = None;
        assert_eq!(
            segment_status_label(TaskStatus::Downloading, &segment),
            "下載中"
        );
        segment.active = false;
        segment.downloaded = 50;
        assert_eq!(segment_status_label(TaskStatus::Paused, &segment), "已暫停");
    }
    fn test_snapshot(id: TaskId, status: TaskStatus) -> TaskSnapshot {
        TaskSnapshot {
            id,
            original_url: "https://example.test/file.bin".into(),
            effective_url: None,
            filename: "file.bin".into(),
            target_dir: PathBuf::from("C:\\Downloads"),
            status,
            origin: TaskOrigin::Gui,
            requested_segments: 1,
            actual_segments: 1,
            segments: Vec::new(),
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
