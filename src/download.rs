use crate::{
    curl::{self, CurlCommandSpec, CurlOutcome, CurlRuntime},
    filename,
    model::*,
    storage,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

pub struct EngineHandle {
    pub commands: Sender<EngineCommand>,
    pub events: Receiver<EngineEvent>,
    metrics: Arc<EngineMetrics>,
}

impl EngineHandle {
    pub fn max_observed_processes(&self) -> usize {
        self.metrics.max.load(Ordering::Acquire)
    }

    pub fn last_command_line(&self) -> String {
        self.metrics
            .last_command
            .lock()
            .map(|line| line.clone())
            .unwrap_or_default()
    }
}

#[derive(Default)]
struct EngineMetrics {
    active: AtomicUsize,
    max: AtomicUsize,
    last_command: Mutex<String>,
}

impl EngineMetrics {
    fn started(&self) {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        let mut previous = self.max.load(Ordering::Acquire);
        while active > previous {
            match self
                .max
                .compare_exchange(previous, active, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(current) => previous = current,
            }
        }
    }

    fn finished(&self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn spawn_engine(state_path: PathBuf, state: PersistedState) -> Result<EngineHandle, String> {
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let metrics = Arc::new(EngineMetrics::default());
    let thread_metrics = Arc::clone(&metrics);
    thread::Builder::new()
        .name("download-engine".into())
        .spawn(move || Engine::new(state_path, state, event_tx, thread_metrics).run(command_rx))
        .map_err(|error| error.to_string())?;
    Ok(EngineHandle {
        commands: command_tx,
        events: event_rx,
        metrics,
    })
}

pub fn split_ranges(total: u64, requested: u8) -> Vec<(u64, u64)> {
    if total == 0 {
        return Vec::new();
    }
    let count = u64::from(requested.max(1)).min(total);
    let base = total / count;
    let extra = total % count;
    let mut start = 0;
    (0..count)
        .map(|index| {
            let length = base + u64::from(index < extra);
            let range = (start, start + length - 1);
            start += length;
            range
        })
        .collect()
}

pub fn resume_offset(start: u64, end: u64, existing: u64) -> io::Result<Option<(u64, u64)>> {
    let length = end
        .checked_sub(start)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "分段範圍無效"))?;
    if existing > length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "分段部分檔超出範圍",
        ));
    }
    if existing == length {
        return Ok(None);
    }
    let adjusted = start
        .checked_add(existing)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "分段續傳偏移無效"))?;
    Ok(Some((adjusted, end)))
}

#[derive(Default)]
pub struct ProgressMeter {
    samples: VecDeque<(u64, u64)>,
    first_ms: Option<u64>,
    paused_at: Option<u64>,
    active_ms: u64,
    last_wall_ms: Option<u64>,
    latest_bytes: u64,
}

impl ProgressMeter {
    pub fn sample(&mut self, now_ms: u64, bytes: u64) {
        if let Some(previous) = self.last_wall_ms {
            if self.paused_at.is_none() {
                self.active_ms = self
                    .active_ms
                    .saturating_add(now_ms.saturating_sub(previous));
            }
        }
        self.last_wall_ms = Some(now_ms);
        self.first_ms.get_or_insert(self.active_ms);
        self.latest_bytes = bytes;
        self.samples.push_back((self.active_ms, bytes));
        while self
            .samples
            .front()
            .is_some_and(|(timestamp, _)| self.active_ms.saturating_sub(*timestamp) > 2_000)
        {
            self.samples.pop_front();
        }
    }

    pub fn pause(&mut self, now_ms: u64) {
        if self.paused_at.is_none() {
            self.paused_at = Some(now_ms);
        }
    }

    pub fn resume(&mut self, now_ms: u64) {
        if self.paused_at.take().is_some() {
            self.last_wall_ms = Some(now_ms);
        }
    }

    pub fn current_bps(&self) -> f64 {
        match (self.samples.front(), self.samples.back()) {
            (Some((first_time, first_bytes)), Some((last_time, last_bytes)))
                if last_time > first_time =>
            {
                last_bytes.saturating_sub(*first_bytes) as f64 * 1000.0
                    / (last_time - first_time) as f64
            }
            _ => 0.0,
        }
    }

    pub fn average_bps(&self) -> f64 {
        let Some(first) = self.first_ms else {
            return 0.0;
        };
        let active = self.active_ms.saturating_sub(first);
        if active == 0 {
            0.0
        } else {
            self.latest_bytes as f64 * 1000.0 / active as f64
        }
    }

    pub fn eta_seconds(&self, total: Option<u64>) -> Option<u64> {
        let speed = self.current_bps();
        let remaining = total?.saturating_sub(self.latest_bytes);
        (speed >= 1.0).then(|| (remaining as f64 / speed).ceil() as u64)
    }
}

pub fn validate_segment(path: &Path, start: u64, end: u64) -> io::Result<()> {
    let expected = end
        .checked_sub(start)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "分段範圍無效"))?;
    let actual = fs::metadata(path)?.len();
    if actual == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("分段大小錯誤：預期 {expected}，實際 {actual}"),
        ))
    }
}

pub fn merge_segments(parts: &[PathBuf], output: &Path, expected: u64) -> io::Result<()> {
    let mut writer = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?,
    );
    for part in parts {
        io::copy(&mut BufReader::new(File::open(part)?), &mut writer)?;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    if fs::metadata(output)?.len() != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "整合後檔案大小錯誤",
        ));
    }
    Ok(())
}

pub fn finalize_file(merged: &Path, target: &Path) -> io::Result<()> {
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "目標檔案已存在",
        ));
    }
    fs::rename(merged, target)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobKind {
    HeadProbe,
    RangeProbe,
    Single,
    Segment,
}

struct ActiveJob {
    task_id: TaskId,
    segment: Option<u8>,
    kind: JobKind,
    child: Child,
    header_path: PathBuf,
    metadata_path: Option<PathBuf>,
    stop: bool,
}

struct Engine {
    state_path: PathBuf,
    settings: GlobalSettings,
    tasks: Vec<DownloadTask>,
    active: HashMap<u64, ActiveJob>,
    next_job_id: u64,
    queue: VecDeque<TaskId>,
    range_probe: HashSet<TaskId>,
    pending_start: HashSet<TaskId>,
    meters: HashMap<TaskId, ProgressMeter>,
    runtime: Option<CurlRuntime>,
    curl_source: CurlSource,
    events: Sender<EngineEvent>,
    metrics: Arc<EngineMetrics>,
    shutting_down: bool,
}

impl Engine {
    fn new(
        state_path: PathBuf,
        mut state: PersistedState,
        events: Sender<EngineEvent>,
        metrics: Arc<EngineMetrics>,
    ) -> Self {
        for task in &mut state.tasks {
            if task.status != TaskStatus::Completed {
                task.recover_after_load();
                if let Err(error) = reconcile_task(task) {
                    task.last_error = Some(error);
                    task.status = TaskStatus::Failed;
                }
            }
        }
        Self {
            state_path,
            settings: state.settings,
            tasks: state.tasks,
            active: HashMap::new(),
            next_job_id: 1,
            queue: VecDeque::new(),
            range_probe: HashSet::new(),
            pending_start: HashSet::new(),
            meters: HashMap::new(),
            runtime: None,
            curl_source: CurlSource::NotStarted,
            events,
            metrics,
            shutting_down: false,
        }
    }

    fn run(mut self, commands: Receiver<EngineCommand>) {
        let mut last_tick = Instant::now();
        self.publish_snapshot();
        loop {
            while let Ok(command) = commands.try_recv() {
                self.handle_command(command);
            }
            self.poll_jobs();
            if self.shutting_down {
                if self.active.is_empty() {
                    self.refresh_progress();
                    for task in &mut self.tasks {
                        if !matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled) {
                            task.status = if task.proxy.enabled && task.proxy.requires_password {
                                TaskStatus::NeedsProxyPassword
                            } else {
                                TaskStatus::Paused
                            };
                        }
                    }
                    self.clear_passwords();
                    let _ = self.persist();
                    let _ = self.events.send(EngineEvent::ShutdownComplete);
                    return;
                }
                thread::sleep(Duration::from_millis(25));
                continue;
            }
            self.schedule_round_robin();
            if last_tick.elapsed() >= Duration::from_millis(500) {
                self.refresh_progress();
                self.publish_snapshot();
                last_tick = Instant::now();
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn handle_command(&mut self, command: EngineCommand) {
        match command {
            EngineCommand::Add(new_task) => self.add_task(new_task),
            EngineCommand::AddBatch(tasks) => {
                for task in tasks {
                    self.add_task(task);
                }
            }
            EngineCommand::AddConfigured { task, response } => {
                let result = self.add_configured_task(task);
                let should_start = result.is_ok();
                let _ = response.send(result);
                if should_start {
                    if let Some(id) = self.tasks.last().map(|task| task.id) {
                        self.request_start(id);
                    }
                }
            }
            EngineCommand::Start(id) => self.request_start(id),
            EngineCommand::Pause(id) => self.pause_task(id),
            EngineCommand::Cancel(id) => self.cancel_task(id),
            EngineCommand::Remove(id) => self.remove_task(id),
            EngineCommand::ClearHistory => self.clear_history(),
            EngineCommand::StartAll => {
                for id in self
                    .tasks
                    .iter()
                    .filter(|task| {
                        matches!(
                            task.status,
                            TaskStatus::Queued | TaskStatus::Paused | TaskStatus::Failed
                        )
                    })
                    .map(|task| task.id)
                    .collect::<Vec<_>>()
                {
                    self.request_start(id);
                }
            }
            EngineCommand::PauseAll => {
                for id in self
                    .tasks
                    .iter()
                    .filter(|task| {
                        matches!(task.status, TaskStatus::Probing | TaskStatus::Downloading)
                    })
                    .map(|task| task.id)
                    .collect::<Vec<_>>()
                {
                    self.pause_task(id);
                }
            }
            EngineCommand::UpdateDraft {
                id,
                url,
                filename,
                target_dir,
                requested_segments,
                proxy,
            } => self.update_draft(id, url, filename, target_dir, requested_segments, proxy),
            EngineCommand::UpdateProxy { ids, proxy } => self.update_proxy(ids, proxy),
            EngineCommand::SetProxyPassword { id, password } => {
                if let Some(task) = self.task_mut(id) {
                    if task.proxy.set_password_secret(password).is_ok() {
                        task.status = TaskStatus::Queued;
                        let _ = self.persist();
                        self.request_start(id);
                    }
                }
            }
            EngineCommand::SetLastDownloadDir(path) => {
                if path.is_dir() {
                    self.settings.last_download_dir = path;
                    let _ = self.persist();
                }
            }
            EngineCommand::SetMaxProcesses(value) => {
                self.settings.max_curl_processes = value.clamp(1, 16);
                let _ = self.persist();
            }
            EngineCommand::Shutdown => {
                self.shutting_down = true;
                self.queue.clear();
                self.pending_start.clear();
                self.stop_all();
            }
        }
    }

    fn add_task(&mut self, new_task: NewTask) {
        let Ok(url) = url::Url::parse(&new_task.url) else {
            let _ = self.events.send(EngineEvent::Fatal("網址格式無效".into()));
            return;
        };
        if !matches!(url.scheme(), "http" | "https") {
            let _ = self
                .events
                .send(EngineEvent::Fatal("只支援 HTTP 或 HTTPS 網址".into()));
            return;
        }
        let id = self.settings.next_task_id;
        self.settings.next_task_id = self.settings.next_task_id.saturating_add(1);
        let filename = filename::suggest_filename(None, &url, id);
        let mut task = DownloadTask::new(id, &new_task.url, filename, new_task.target_dir);
        task.created_unix_ms = current_unix_ms();
        let _ = fs::create_dir_all(storage::task_work_dir(&task));
        self.tasks.push(task);
        let _ = self.persist();
        self.publish_snapshot();
    }

    fn add_configured_task(&mut self, new_task: ConfiguredTask) -> Result<TaskId, String> {
        let parsed = url::Url::parse(&new_task.url).map_err(|_| "網址格式無效".to_owned())?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("只支援 HTTP 或 HTTPS 網址".into());
        }
        if !new_task.target_dir.is_dir() {
            return Err("下載目錄不存在或無法存取".into());
        }
        new_task.proxy.validate()?;

        let id = self.settings.next_task_id;
        let filename = filename::sanitize_filename(&new_task.filename, id);
        let mut task = DownloadTask::new(id, &new_task.url, filename, new_task.target_dir);
        task.requested_segments = new_task.requested_segments.clamp(1, 8);
        task.proxy = new_task.proxy;
        task.created_unix_ms = current_unix_ms();
        fs::create_dir_all(storage::task_work_dir(&task))
            .map_err(|error| format!("無法建立下載工作目錄：{error}"))?;

        self.settings.next_task_id = self.settings.next_task_id.saturating_add(1);
        let id = task.id;
        self.tasks.push(task);
        self.persist()
            .map_err(|error| format!("無法保存下載任務：{error}"))?;
        self.publish_snapshot();
        Ok(id)
    }

    fn update_draft(
        &mut self,
        id: TaskId,
        url: String,
        filename_value: String,
        target_dir: PathBuf,
        requested_segments: u8,
        proxy: ProxySettings,
    ) {
        let Ok(parsed) = url::Url::parse(&url) else {
            self.fail_task(
                id,
                task_error(ErrorKind::Input, "網址格式無效", "網址無法解析", "修改網址"),
            );
            return;
        };
        if !matches!(parsed.scheme(), "http" | "https") {
            self.fail_task(
                id,
                task_error(
                    ErrorKind::Input,
                    "只支援 HTTP 或 HTTPS 網址",
                    "不支援的網址協定",
                    "修改網址",
                ),
            );
            return;
        }
        if let Err(error) = proxy.validate() {
            self.fail_task(
                id,
                task_error(ErrorKind::Proxy, &error, &error, "修正 Proxy 設定"),
            );
            return;
        }
        let source_changed = self.task(id).is_some_and(|task| {
            task.original_url != url || proxy_configuration_changed(&task.proxy, &proxy)
        });
        if source_changed {
            self.stop_task_jobs(id);
            self.queue.retain(|queued| *queued != id);
            self.pending_start.remove(&id);
        }
        let needs_password = proxy.enabled && proxy.requires_password && proxy.password.is_none();
        if !source_changed && needs_password {
            self.stop_task_jobs(id);
            self.queue.retain(|queued| *queued != id);
        }
        let Some(task) = self.task_mut(id) else {
            return;
        };
        task.original_url = url;
        task.filename = filename_value;
        task.target_dir = target_dir;
        task.requested_segments = requested_segments.clamp(1, 8);
        task.proxy = proxy;
        if source_changed {
            task.effective_url = None;
            task.total_size = None;
            task.etag = None;
            task.last_modified = None;
            task.range_support = RangeSupport::Unknown;
            task.segments.clear();
            if task.proxy.enabled && task.proxy.requires_password && task.proxy.password.is_none() {
                task.status = TaskStatus::NeedsProxyPassword;
            } else {
                task.status = TaskStatus::Queued;
            }
        } else if needs_password {
            task.status = TaskStatus::NeedsProxyPassword;
        }
        let _ = self.persist();
    }

    fn update_proxy(&mut self, ids: Vec<TaskId>, proxy: ProxySettings) {
        if let Err(error) = proxy.validate() {
            let _ = self
                .events
                .send(EngineEvent::Fatal(format!("Proxy 設定無效：{error}")));
            return;
        }
        let mut applied = 0;
        let mut skipped = 0;
        for id in ids {
            let Some(status) = self.task(id).map(|task| task.status) else {
                skipped += 1;
                continue;
            };
            if !matches!(
                status,
                TaskStatus::Queued
                    | TaskStatus::Paused
                    | TaskStatus::Failed
                    | TaskStatus::NeedsProxyPassword
            ) {
                skipped += 1;
                continue;
            }
            let source_changed = self
                .task(id)
                .is_some_and(|task| proxy_configuration_changed(&task.proxy, &proxy));
            if source_changed {
                self.stop_task_jobs(id);
                self.queue.retain(|queued| *queued != id);
                self.pending_start.remove(&id);
                self.range_probe.remove(&id);
            }
            let needs_password =
                proxy.enabled && proxy.requires_password && proxy.password.is_none();
            if let Some(task) = self.task_mut(id) {
                task.proxy = proxy.clone();
                task.last_error = None;
                if source_changed {
                    task.effective_url = None;
                    task.total_size = None;
                    task.etag = None;
                    task.last_modified = None;
                    task.range_support = RangeSupport::Unknown;
                    task.segments.clear();
                    task.status = if needs_password {
                        TaskStatus::NeedsProxyPassword
                    } else {
                        TaskStatus::Queued
                    };
                } else if needs_password {
                    task.status = TaskStatus::NeedsProxyPassword;
                }
            }
            if !source_changed && needs_password {
                self.stop_task_jobs(id);
                self.queue.retain(|queued| *queued != id);
            }
            applied += 1;
        }
        let _ = self.persist();
        let _ = self
            .events
            .send(EngineEvent::BatchProxyApplied { applied, skipped });
        self.publish_snapshot();
    }

    fn request_start(&mut self, id: TaskId) {
        let Some(status) = self.task(id).map(|task| task.status) else {
            return;
        };
        match status {
            TaskStatus::Probing => {
                self.pending_start.insert(id);
            }
            TaskStatus::Queued | TaskStatus::Paused | TaskStatus::Failed => {
                if self.task(id).is_some_and(|task| task.total_size.is_none()) {
                    self.begin_probe(id);
                } else {
                    self.start_task(id);
                }
            }
            TaskStatus::NeedsProxyPassword => {}
            _ => {}
        }
    }

    fn begin_probe(&mut self, id: TaskId) {
        let Some(task) = self.task(id) else {
            return;
        };
        if task.proxy.enabled && task.proxy.requires_password && task.proxy.password.is_none() {
            if let Some(task) = self.task_mut(id) {
                task.status = TaskStatus::NeedsProxyPassword;
            }
            let _ = self.persist();
            self.publish_snapshot();
            return;
        }
        self.stop_task_jobs(id);
        self.queue.retain(|queued| *queued != id);
        self.range_probe.insert(id);
        self.pending_start.insert(id);
        if let Some(task) = self.task_mut(id) {
            task.status = TaskStatus::Probing;
        }
        self.queue.push_back(id);
        let _ = self.persist();
    }

    fn pause_task(&mut self, id: TaskId) {
        self.stop_task_jobs(id);
        self.pending_start.remove(&id);
        self.queue.retain(|queued| *queued != id);
        if let Some(task) = self.task_mut(id) {
            if !matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled) {
                task.status = TaskStatus::Paused;
            }
        }
        let _ = self.persist();
    }

    fn cancel_task(&mut self, id: TaskId) {
        self.stop_task_jobs(id);
        self.queue.retain(|queued| *queued != id);
        self.pending_start.remove(&id);
        if let Some(task) = self.task_mut(id) {
            let work = storage::task_work_dir(task);
            let _ = fs::remove_dir_all(work);
            task.proxy.clear_password();
            task.status = TaskStatus::Cancelled;
        }
        let _ = self.persist();
    }

    fn remove_task(&mut self, id: TaskId) {
        self.cancel_task(id);
        self.tasks.retain(|task| task.id != id);
        let _ = self.persist();
        self.publish_snapshot();
    }

    fn clear_history(&mut self) {
        let removed = self
            .tasks
            .iter()
            .filter(|task| matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled))
            .cloned()
            .collect::<Vec<_>>();
        for task in &removed {
            self.stop_task_jobs(task.id);
            self.queue.retain(|queued| *queued != task.id);
            self.pending_start.remove(&task.id);
            self.range_probe.remove(&task.id);
            self.meters.remove(&task.id);
            let _ = fs::remove_dir_all(storage::task_work_dir(task));
        }
        self.tasks
            .retain(|task| !matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled));
        let _ = self.persist();
        self.publish_snapshot();
    }

    fn start_task(&mut self, id: TaskId) {
        let Some(index) = self.task_index(id) else {
            return;
        };
        let task = &mut self.tasks[index];
        if task.proxy.enabled && task.proxy.requires_password && task.proxy.password.is_none() {
            task.status = TaskStatus::NeedsProxyPassword;
            return;
        }
        if task.target_dir.is_dir() == false {
            self.fail_task(
                id,
                task_error(
                    ErrorKind::Disk,
                    "下載目錄不存在",
                    "目標目錄無法存取",
                    "選擇其他目錄",
                ),
            );
            return;
        }
        let target = task.target_dir.join(&task.filename);
        if target.exists() {
            self.fail_task(
                id,
                task_error(
                    ErrorKind::Disk,
                    "目標檔案已存在",
                    "為避免靜默覆寫而停止",
                    "更改檔名或移除現有檔案",
                ),
            );
            return;
        }
        let work = storage::task_work_dir(task);
        if let Err(error) = fs::create_dir_all(&work) {
            self.fail_task(
                id,
                task_error(
                    ErrorKind::Disk,
                    "無法建立下載工作目錄",
                    &error.to_string(),
                    "選擇其他目錄",
                ),
            );
            return;
        }
        let total = task.total_size;
        let previous_segments = task.segments.clone();
        if task.range_support == RangeSupport::Supported && task.requested_segments > 1 {
            let Some(total) = total else {
                task.range_support = RangeSupport::Unsupported;
                task.actual_segments = 1;
                task.segments = vec![SegmentState {
                    index: 0,
                    start: 0,
                    end: 0,
                    downloaded: 0,
                }];
                task.status = TaskStatus::Downloading;
                self.queue.push_back(id);
                return;
            };
            let ranges = split_ranges(total, task.requested_segments);
            task.actual_segments = ranges.len() as u8;
            task.segments = ranges
                .into_iter()
                .enumerate()
                .map(|(index, (start, end))| SegmentState {
                    index: index as u8,
                    start,
                    end,
                    downloaded: previous_segments
                        .iter()
                        .find(|previous| previous.start == start && previous.end == end)
                        .map(|previous| previous.downloaded)
                        .unwrap_or(0),
                })
                .collect();
        } else {
            task.actual_segments = 1;
            let end = total.map(|value| value.saturating_sub(1)).unwrap_or(0);
            task.segments = vec![SegmentState {
                index: 0,
                start: 0,
                end,
                downloaded: previous_segments
                    .first()
                    .filter(|previous| previous.start == 0 && previous.end == end)
                    .map(|previous| previous.downloaded)
                    .unwrap_or(0),
            }];
        }
        task.status = TaskStatus::Downloading;
        self.meters.entry(id).or_default();
        self.queue.push_back(id);
        let _ = self.persist();
    }

    fn schedule_round_robin(&mut self) {
        let limit = usize::from(self.settings.max_curl_processes.clamp(1, 16));
        while self.active.len() < limit {
            let Some(id) = self.queue.pop_front() else {
                break;
            };
            let Some((status, actual_segments)) = self
                .task(id)
                .map(|task| (task.status, task.actual_segments))
            else {
                continue;
            };
            let single_or_probe = status == TaskStatus::Probing || actual_segments <= 1;
            if single_or_probe && self.task_has_active(id) {
                continue;
            }
            let has_more = match status {
                TaskStatus::Probing | TaskStatus::Downloading => self.start_next_job(id),
                _ => false,
            };
            if has_more {
                self.queue.push_back(id);
            }
            if !has_more && self.active.len() >= limit {
                break;
            }
        }
    }

    fn start_next_job(&mut self, id: TaskId) -> bool {
        let Some(task) = self.task(id).cloned() else {
            return false;
        };
        let work = storage::task_work_dir(&task);
        let (kind, segment) = if task.status == TaskStatus::Probing {
            if self.range_probe.contains(&id) {
                (JobKind::RangeProbe, None)
            } else {
                (JobKind::HeadProbe, None)
            }
        } else if task.actual_segments <= 1 {
            if task.segments.first().is_some_and(|segment| {
                segment.downloaded > 0
                    && task
                        .total_size
                        .is_some_and(|total| segment.downloaded >= total)
            }) {
                self.finalize_task(id);
                return false;
            }
            (JobKind::Single, Some(0))
        } else {
            let Some(segment) = task.segments.iter().find(|segment| {
                let expected = segment.end.saturating_sub(segment.start).saturating_add(1);
                segment.downloaded < expected && !self.segment_has_active(task.id, segment.index)
            }) else {
                if !self.task_has_incomplete_segments(id) {
                    self.finalize_task(id);
                }
                return false;
            };
            (JobKind::Segment, Some(segment.index))
        };
        let result = self.spawn_job(&task, work, kind, segment);
        if let Err(error) = result {
            let diagnostic = error.to_string();
            self.fail_task(
                id,
                task_error(ErrorKind::Curl, "無法啟動 curl", &diagnostic, "重試下載"),
            );
            return false;
        }
        true
    }

    fn spawn_job(
        &mut self,
        task: &DownloadTask,
        work: PathBuf,
        kind: JobKind,
        segment: Option<u8>,
    ) -> io::Result<()> {
        let suffix = match kind {
            JobKind::HeadProbe => "head",
            JobKind::RangeProbe => "range",
            JobKind::Single => "single",
            JobKind::Segment => "segment",
        };
        let header_path = if kind == JobKind::Segment {
            work.join(format!("headers-{suffix}-{}.txt", segment.unwrap_or(0)))
        } else {
            work.join(format!("headers-{suffix}.txt"))
        };
        let mut metadata_path = None;
        let mut spec: CurlCommandSpec;
        let stdout = match kind {
            JobKind::HeadProbe => {
                let path = work.join("probe-head.json");
                metadata_path = Some(path.clone());
                spec = curl::build_head_probe(&task.proxy, &task.original_url, &header_path)
                    .map_err(io::Error::other)?;
                Stdio::from(File::create(path)?)
            }
            JobKind::RangeProbe => {
                let path = work.join("probe-range.json");
                metadata_path = Some(path.clone());
                spec = curl::build_range_probe(&task.proxy, &task.original_url, &header_path)
                    .map_err(io::Error::other)?;
                Stdio::from(File::create(path)?)
            }
            JobKind::Single => {
                let output = work.join("payload.part");
                let existing = fs::metadata(&output)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                spec = curl::build_single_transfer(
                    &task.proxy,
                    task.effective_url.as_deref().unwrap_or(&task.original_url),
                    &output,
                    existing,
                    task.etag.as_deref().or(task.last_modified.as_deref()),
                    &header_path,
                )
                .map_err(io::Error::other)?;
                Stdio::null()
            }
            JobKind::Segment => {
                let segment_index = segment.ok_or_else(|| io::Error::other("缺少分段索引"))?;
                let segment_state = task
                    .segments
                    .iter()
                    .find(|state| state.index == segment_index)
                    .ok_or_else(|| io::Error::other("找不到分段"))?;
                let output = work.join(format!("segment-{segment_index}.part"));
                let existing = fs::metadata(&output)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                spec = curl::build_segment_transfer(
                    &task.proxy,
                    task.effective_url.as_deref().unwrap_or(&task.original_url),
                    segment_state.start,
                    segment_state.end,
                    existing,
                    task.etag.as_deref().or(task.last_modified.as_deref()),
                    &header_path,
                )
                .map_err(io::Error::other)?;
                Stdio::from(OpenOptions::new().create(true).append(true).open(output)?)
            }
        };
        let child = {
            let runtime = self.ensure_runtime()?;
            runtime.spawn(&mut spec, stdout)?
        };
        if let Ok(mut command_line) = self.metrics.last_command.lock() {
            *command_line = spec.arguments_text();
        }
        let job_id = self.next_job_id;
        self.next_job_id = self.next_job_id.saturating_add(1);
        self.active.insert(
            job_id,
            ActiveJob {
                task_id: task.id,
                segment,
                kind,
                child,
                header_path,
                metadata_path,
                stop: false,
            },
        );
        self.metrics.started();
        Ok(())
    }

    fn ensure_runtime(&mut self) -> io::Result<&CurlRuntime> {
        if self.runtime.is_none() {
            let runtime = CurlRuntime::extract()?;
            self.curl_source = runtime.source();
            self.runtime = Some(runtime);
        }
        Ok(self
            .runtime
            .as_ref()
            .expect("curl runtime must be initialized"))
    }

    fn poll_jobs(&mut self) {
        let job_ids = self.active.keys().copied().collect::<Vec<_>>();
        for job_id in job_ids {
            let mut exit_code = None;
            if let Some(job) = self.active.get_mut(&job_id) {
                if job.stop {
                    let _ = job.child.kill();
                }
                if let Ok(Some(status)) = job.child.try_wait() {
                    exit_code = Some(status.code().unwrap_or(-1));
                }
            }
            let Some(exit_code) = exit_code else { continue };
            let Some(mut job) = self.active.remove(&job_id) else {
                continue;
            };
            self.metrics.finished();
            let mut stderr = String::new();
            if let Some(mut pipe) = job.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let headers = fs::read_to_string(&job.header_path).unwrap_or_default();
            let headers = if let Some(path) = job.metadata_path.as_ref() {
                format!(
                    "{headers}\n{}",
                    fs::read_to_string(path).unwrap_or_default()
                )
            } else {
                headers
            };
            self.finish_job(
                job,
                CurlOutcome {
                    exit_code,
                    stderr,
                    headers,
                },
            );
        }
    }

    fn finish_job(&mut self, job: ActiveJob, outcome: CurlOutcome) {
        if job.stop {
            return;
        }
        match job.kind {
            JobKind::HeadProbe | JobKind::RangeProbe => self.finish_probe(job, outcome),
            JobKind::Single => self.finish_single(job, outcome),
            JobKind::Segment => self.finish_segment(job, outcome),
        }
    }

    fn finish_probe(&mut self, job: ActiveJob, outcome: CurlOutcome) {
        let fallback = job.kind == JobKind::HeadProbe && !self.range_probe.contains(&job.task_id);
        let parsed = curl::parse_probe(
            &outcome.headers,
            self.task(job.task_id)
                .and_then(|task| task.effective_url.as_deref())
                .unwrap_or_else(|| {
                    self.task(job.task_id)
                        .map(|task| task.original_url.as_str())
                        .unwrap_or("")
                }),
        );
        if outcome.exit_code != 0 || parsed.is_err() {
            if fallback {
                self.range_probe.insert(job.task_id);
                self.queue.push_back(job.task_id);
            } else {
                let kind = self
                    .task(job.task_id)
                    .map(|task| transfer_error_kind(task, outcome.exit_code, &outcome.headers))
                    .unwrap_or(ErrorKind::Network);
                self.fail_task(
                    job.task_id,
                    task_error(
                        kind,
                        "無法取得來源資訊",
                        &outcome.stderr,
                        "檢查網址或 Proxy 後重試",
                    ),
                );
            }
            return;
        }
        let meta = parsed.unwrap();
        if job.kind == JobKind::HeadProbe && meta.range_support != RangeSupport::Supported {
            self.range_probe.insert(job.task_id);
            self.queue.push_back(job.task_id);
            return;
        }
        self.range_probe.remove(&job.task_id);
        if let Some(task) = self.task_mut(job.task_id) {
            task.last_error = None;
            task.effective_url = Some(meta.effective_url);
            task.total_size = meta.total_size;
            task.range_support = meta.range_support;
            task.etag = meta.etag;
            task.last_modified = meta.last_modified;
            if let Some(server_name) = meta.content_disposition.as_deref() {
                if let Ok(url) = url::Url::parse(&task.original_url) {
                    let generated = filename::suggest_filename(None, &url, task.id);
                    if task.filename == generated {
                        task.filename =
                            filename::suggest_filename(Some(server_name), &url, task.id);
                    }
                }
            }
            task.status = TaskStatus::Queued;
        }
        let start = self.pending_start.remove(&job.task_id);
        if start {
            self.start_task(job.task_id);
        } else {
            self.queue.push_back(job.task_id);
        }
        let _ = self.persist();
    }

    fn finish_single(&mut self, job: ActiveJob, outcome: CurlOutcome) {
        if outcome.exit_code != 0 {
            let kind = self
                .task(job.task_id)
                .map(|task| transfer_error_kind(task, outcome.exit_code, &outcome.headers))
                .unwrap_or(ErrorKind::Network);
            self.fail_task(
                job.task_id,
                task_error(kind, "下載失敗", &outcome.stderr, "重試下載"),
            );
            return;
        }
        let Some(task) = self.task(job.task_id).cloned() else {
            return;
        };
        let path = storage::task_work_dir(&task).join("payload.part");
        let length = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if task.total_size.is_some_and(|total| total != length) {
            self.fail_task(
                job.task_id,
                task_error(
                    ErrorKind::SourceChanged,
                    "下載大小與來源不符",
                    "單流回應長度不符合探測結果",
                    "從零重試",
                ),
            );
            return;
        }
        if let Some(task) = self.task_mut(job.task_id) {
            if let Some(segment) = task.segments.first_mut() {
                segment.downloaded = length;
            }
            task.last_error = None;
        }
        self.finalize_task(job.task_id);
    }

    fn finish_segment(&mut self, job: ActiveJob, outcome: CurlOutcome) {
        if outcome.exit_code != 0 {
            let kind = self
                .task(job.task_id)
                .map(|task| transfer_error_kind(task, outcome.exit_code, &outcome.headers))
                .unwrap_or(ErrorKind::Network);
            self.fail_task(
                job.task_id,
                task_error(kind, "分段下載失敗", &outcome.stderr, "重試下載"),
            );
            return;
        }
        let Some(task) = self.task(job.task_id).cloned() else {
            return;
        };
        let Some(segment_index) = job.segment else {
            return;
        };
        let Some(segment) = task
            .segments
            .iter()
            .find(|segment| segment.index == segment_index)
        else {
            return;
        };
        let path = storage::task_work_dir(&task).join(format!("segment-{segment_index}.part"));
        if let Err(error) = validate_segment(&path, segment.start, segment.end) {
            self.fail_task(
                job.task_id,
                task_error(
                    ErrorKind::SourceChanged,
                    "分段內容不符",
                    &error.to_string(),
                    "刪除部分資料並從零重試",
                ),
            );
            return;
        }
        if !outcome.headers.contains(" 206 ") {
            self.fail_task(
                job.task_id,
                task_error(
                    ErrorKind::SourceChanged,
                    "來源未回傳分段內容",
                    "伺服器忽略 Range 要求",
                    "改用單流或從零重試",
                ),
            );
            return;
        }
        if let Some(task) = self.task_mut(job.task_id) {
            if let Some(segment) = task
                .segments
                .iter_mut()
                .find(|segment| segment.index == segment_index)
            {
                segment.downloaded = segment.end.saturating_sub(segment.start).saturating_add(1);
            }
            task.last_error = None;
        }
        if !self.task_has_incomplete_segments(job.task_id) {
            self.finalize_task(job.task_id);
        } else {
            self.queue.push_back(job.task_id);
        }
    }

    fn finalize_task(&mut self, id: TaskId) {
        let Some(task) = self.task(id).cloned() else {
            return;
        };
        let work = storage::task_work_dir(&task);
        let target = task.target_dir.join(&task.filename);
        if target.exists() {
            self.fail_task(
                id,
                task_error(
                    ErrorKind::Disk,
                    "目標檔案已存在",
                    "為避免靜默覆寫而停止",
                    "更改檔名或移除現有檔案",
                ),
            );
            return;
        }
        if let Some(task) = self.task_mut(id) {
            task.status = TaskStatus::Finalizing;
        }
        let result = if task.actual_segments > 1 {
            let parts = task
                .segments
                .iter()
                .map(|segment| work.join(format!("segment-{}.part", segment.index)))
                .collect::<Vec<_>>();
            let merged = work.join("merged.part");
            merge_segments(&parts, &merged, task.total_size.unwrap_or(0))
                .and_then(|_| finalize_file(&merged, &target))
        } else {
            finalize_file(&work.join("payload.part"), &target)
        };
        if let Err(error) = result {
            self.fail_task(
                id,
                task_error(
                    ErrorKind::Disk,
                    "整合檔案失敗",
                    &error.to_string(),
                    "保留部分資料後重試",
                ),
            );
            return;
        }
        let _ = fs::remove_dir_all(work);
        if let Some(task) = self.task_mut(id) {
            task.proxy.clear_password();
            task.last_error = None;
            task.status = TaskStatus::Completed;
        }
        let _ = self.persist();
        self.publish_snapshot();
    }

    fn stop_all(&mut self) {
        for job in self.active.values_mut() {
            job.stop = true;
        }
    }

    fn stop_task_jobs(&mut self, id: TaskId) {
        for job in self.active.values_mut().filter(|job| job.task_id == id) {
            job.stop = true;
        }
    }

    fn fail_task(&mut self, id: TaskId, error: TaskError) {
        self.stop_task_jobs(id);
        self.queue.retain(|queued| *queued != id);
        self.pending_start.remove(&id);
        if let Some(task) = self.task_mut(id) {
            let mut error = error;
            error.diagnostic = sanitize_diagnostic(&error.diagnostic, &task.proxy);
            task.last_error = Some(error);
            task.status = TaskStatus::Failed;
        }
        let _ = self.persist();
        self.publish_snapshot();
    }

    fn task_has_active(&self, id: TaskId) -> bool {
        self.active.values().any(|job| job.task_id == id)
    }

    fn segment_has_active(&self, id: TaskId, segment: u8) -> bool {
        self.active
            .values()
            .any(|job| job.task_id == id && job.segment == Some(segment))
    }

    fn task_has_incomplete_segments(&self, id: TaskId) -> bool {
        self.task(id).is_some_and(|task| {
            task.segments.iter().any(|segment| {
                segment.downloaded < segment.end.saturating_sub(segment.start).saturating_add(1)
            })
        })
    }

    fn task_index(&self, id: TaskId) -> Option<usize> {
        self.tasks.iter().position(|task| task.id == id)
    }

    fn task(&self, id: TaskId) -> Option<&DownloadTask> {
        self.tasks.iter().find(|task| task.id == id)
    }

    fn task_mut(&mut self, id: TaskId) -> Option<&mut DownloadTask> {
        self.tasks.iter_mut().find(|task| task.id == id)
    }

    fn refresh_progress(&mut self) {
        let now = current_unix_ms();
        for task in &mut self.tasks {
            if matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled) {
                continue;
            }
            let work = storage::task_work_dir(task);
            if task.actual_segments > 1 {
                for segment in &mut task.segments {
                    let path = work.join(format!("segment-{}.part", segment.index));
                    segment.downloaded = fs::metadata(path)
                        .map(|metadata| metadata.len())
                        .unwrap_or(0);
                }
            } else if let Some(segment) = task.segments.first_mut() {
                segment.downloaded = fs::metadata(work.join("payload.part"))
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
            }
            let downloaded = task.segments.iter().map(|segment| segment.downloaded).sum();
            self.meters
                .entry(task.id)
                .or_default()
                .sample(now, downloaded);
        }
    }

    fn publish_snapshot(&self) {
        let snapshots = self
            .tasks
            .iter()
            .map(|task| {
                let downloaded = task.segments.iter().map(|segment| segment.downloaded).sum();
                let meter = self.meters.get(&task.id);
                TaskSnapshot {
                    id: task.id,
                    original_url: task.original_url.clone(),
                    effective_url: task.effective_url.clone(),
                    filename: task.filename.clone(),
                    target_dir: task.target_dir.clone(),
                    status: task.status,
                    requested_segments: task.requested_segments,
                    actual_segments: task.actual_segments,
                    downloaded,
                    total_size: task.total_size,
                    range_support: task.range_support,
                    current_bps: meter.map(ProgressMeter::current_bps).unwrap_or(0.0),
                    average_bps: meter.map(ProgressMeter::average_bps).unwrap_or(0.0),
                    eta_seconds: meter.and_then(|meter| meter.eta_seconds(task.total_size)),
                    active_millis: task.active_millis,
                    proxy: ProxySnapshot {
                        enabled: task.proxy.enabled,
                        protocol: task.proxy.protocol,
                        host: task.proxy.host.clone(),
                        port: task.proxy.port,
                        username: task.proxy.username.clone(),
                        requires_password: task.proxy.requires_password,
                    },
                    error: task.last_error.clone(),
                    curl_source: self.curl_source,
                }
            })
            .collect();
        let _ = self.events.send(EngineEvent::Snapshot(snapshots));
    }

    fn persist(&self) -> io::Result<()> {
        storage::save_state(
            &self.state_path,
            &PersistedState {
                schema_version: 1,
                settings: self.settings.clone(),
                tasks: self.tasks.clone(),
            },
        )
    }

    fn clear_passwords(&mut self) {
        for task in &mut self.tasks {
            task.proxy.clear_password();
        }
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn proxy_configuration_changed(left: &ProxySettings, right: &ProxySettings) -> bool {
    left.enabled != right.enabled
        || left.protocol != right.protocol
        || left.host != right.host
        || left.port != right.port
        || left.username != right.username
        || left.requires_password != right.requires_password
        || left.password.as_deref() != right.password.as_deref()
}

fn sanitize_diagnostic(raw: &str, proxy: &ProxySettings) -> String {
    let mut text = raw.to_owned();
    if !proxy.username.is_empty() {
        text = text.replace(&proxy.username, "<proxy-user>");
    }
    if let Some(password) = proxy.password.as_deref() {
        text = text.replace(password.as_str(), "<proxy-password>");
    }
    text.chars().take(2_000).collect()
}

fn transfer_error_kind(task: &DownloadTask, exit_code: i32, headers: &str) -> ErrorKind {
    if task.proxy.enabled && matches!(exit_code, 5 | 6 | 7 | 35 | 56 | 97) {
        return ErrorKind::Proxy;
    }
    if headers.contains(" 407 ") {
        return ErrorKind::Proxy;
    }
    if headers.lines().any(|line| {
        line.starts_with("HTTP/")
            && line
                .split_whitespace()
                .nth(1)
                .and_then(|code| code.parse::<u16>().ok())
                .is_some_and(|code| code >= 400)
    }) {
        return ErrorKind::Http;
    }
    ErrorKind::Network
}

fn task_error(kind: ErrorKind, summary: &str, diagnostic: &str, action: &str) -> TaskError {
    TaskError {
        kind,
        summary: summary.to_owned(),
        code: None,
        diagnostic: diagnostic.chars().take(2_000).collect(),
        action: action.to_owned(),
    }
}

fn reconcile_task(task: &mut DownloadTask) -> Result<(), TaskError> {
    let work = storage::task_work_dir(task);
    if task.actual_segments > 1 {
        for segment in &mut task.segments {
            let path = work.join(format!("segment-{}.part", segment.index));
            segment.downloaded = fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let expected = segment.end.saturating_sub(segment.start).saturating_add(1);
            if segment.downloaded > expected {
                return Err(task_error(
                    ErrorKind::Disk,
                    "部分檔大小超出分段",
                    "部分檔可能已損壞",
                    "刪除部分資料並重新下載",
                ));
            }
        }
    } else if let Some(segment) = task.segments.first_mut() {
        let path = work.join("payload.part");
        segment.downloaded = fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let expected = segment.end.saturating_sub(segment.start).saturating_add(1);
        if task
            .total_size
            .is_some_and(|_| segment.downloaded > expected)
        {
            return Err(task_error(
                ErrorKind::Disk,
                "部分檔大小超出下載大小",
                "部分檔可能已損壞",
                "刪除部分資料並重新下載",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_cover_file_once_even_when_small() {
        assert_eq!(split_ranges(3, 8), vec![(0, 0), (1, 1), (2, 2)]);
        let ranges = split_ranges(10, 4);
        assert_eq!(ranges, vec![(0, 2), (3, 5), (6, 7), (8, 9)]);
    }

    #[test]
    fn speed_excludes_paused_time() {
        let mut meter = ProgressMeter::default();
        meter.sample(0, 0);
        meter.sample(2_000, 2_000);
        meter.pause(2_000);
        meter.resume(12_000);
        meter.sample(14_000, 4_000);
        assert_eq!(meter.average_bps(), 1000.0);
        assert_eq!(meter.current_bps(), 1000.0);
    }

    #[test]
    fn resume_reports_completion_and_rejects_oversize_part() {
        assert_eq!(resume_offset(100, 199, 100).unwrap(), None);
        assert!(resume_offset(100, 199, 101).is_err());
    }

    #[test]
    fn merge_preserves_segment_order_and_finalization_rejects_collision() {
        let dir = test_dir("merge");
        let first = dir.join("segment-0.part");
        let second = dir.join("segment-1.part");
        let merged = dir.join("merged.part");
        let target = dir.join("file.bin");
        std::fs::write(&first, b"abc").unwrap();
        std::fs::write(&second, b"def").unwrap();
        merge_segments(&[second.clone(), first.clone()], &merged, 6).unwrap();
        assert_eq!(std::fs::read(&merged).unwrap(), b"defabc");
        std::fs::write(&target, b"existing").unwrap();
        assert!(finalize_file(&merged, &target).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn zero_speed_and_unknown_size_have_no_eta() {
        let meter = ProgressMeter::default();
        assert_eq!(meter.current_bps(), 0.0);
        assert_eq!(meter.eta_seconds(None), None);
    }

    fn test_dir(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("curl-downloader-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
