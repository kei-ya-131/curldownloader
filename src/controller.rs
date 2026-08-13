use crate::{
    model::{EngineCommand, EngineEvent, TaskId, TaskSnapshot},
    session_shutdown, startup_policy,
    tray::{TrayController, TrayEvent},
    window_control::MainWindowControl,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleState {
    Starting = 0,
    RunningHidden = 1,
    RunningVisible = 2,
    ShuttingDown = 3,
    Stopped = 4,
}

impl LifecycleState {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Starting,
            1 => Self::RunningHidden,
            2 => Self::RunningVisible,
            3 => Self::ShuttingDown,
            4 => Self::Stopped,
            _ => Self::Stopped,
        }
    }
}

#[derive(Default)]
struct ReadyState {
    engine: bool,
    ui: bool,
}

#[derive(Clone)]
pub struct SharedControllerState {
    tasks: Arc<Mutex<Vec<TaskSnapshot>>>,
    lifecycle: Arc<AtomicU8>,
    ready: Arc<(Mutex<ReadyState>, Condvar)>,
}

impl SharedControllerState {
    pub fn new(initial: LifecycleState) -> Self {
        Self {
            tasks: Arc::new(Mutex::new(Vec::new())),
            lifecycle: Arc::new(AtomicU8::new(initial as u8)),
            ready: Arc::new((Mutex::new(ReadyState::default()), Condvar::new())),
        }
    }

    pub fn replace_tasks(&self, tasks: Vec<TaskSnapshot>) {
        if let Ok(mut current) = self.tasks.lock() {
            *current = tasks;
        }
    }

    pub fn tasks(&self) -> Vec<TaskSnapshot> {
        self.tasks
            .lock()
            .map(|tasks| tasks.clone())
            .unwrap_or_default()
    }

    pub fn task_exists(&self, task_id: TaskId) -> bool {
        self.tasks
            .lock()
            .map(|tasks| tasks.iter().any(|task| task.id == task_id))
            .unwrap_or(false)
    }

    pub fn lifecycle(&self) -> LifecycleState {
        LifecycleState::from_byte(self.lifecycle.load(Ordering::Acquire))
    }

    pub fn set_lifecycle(&self, state: LifecycleState) {
        self.lifecycle.store(state as u8, Ordering::Release);
    }

    pub fn begin_shutdown(&self) -> bool {
        for current in [
            LifecycleState::Starting,
            LifecycleState::RunningHidden,
            LifecycleState::RunningVisible,
        ] {
            if self
                .lifecycle
                .compare_exchange(
                    current as u8,
                    LifecycleState::ShuttingDown as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
        }
        false
    }

    pub fn mark_engine_ready(&self) {
        let (lock, condition) = &*self.ready;
        if let Ok(mut ready) = lock.lock() {
            ready.engine = true;
            condition.notify_all();
        }
    }

    pub fn mark_ui_ready(&self) {
        let (lock, condition) = &*self.ready;
        if let Ok(mut ready) = lock.lock() {
            ready.ui = true;
            condition.notify_all();
        }
    }

    pub fn mark_ready(&self) {
        let (lock, condition) = &*self.ready;
        if let Ok(mut ready) = lock.lock() {
            ready.engine = true;
            ready.ui = true;
            condition.notify_all();
        }
    }

    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let (lock, condition) = &*self.ready;
        let Ok(ready) = lock.lock() else {
            return false;
        };
        if ready.engine && ready.ui {
            return true;
        }
        let Ok((ready, _)) = condition.wait_timeout(ready, timeout) else {
            return false;
        };
        ready.engine && ready.ui
    }
}

#[derive(Debug)]
pub enum ControllerCommand {
    ShowWindow,
    ShowTask { task_id: TaskId },
    WindowHidden,
    WindowVisible,
    ShutdownManual,
    ShutdownInternal,
}

#[derive(Debug)]
pub enum AppEvent {
    SnapshotChanged,
    ShowWindow,
    ShowTask { task_id: TaskId },
    BatchProxyApplied { applied: usize, skipped: usize },
    Fatal(String),
}

pub struct ControllerHandle {
    command_sender: Sender<ControllerCommand>,
    state: SharedControllerState,
    app_events: Option<Receiver<AppEvent>>,
    thread: Option<JoinHandle<()>>,
}

impl ControllerHandle {
    pub fn commands(&self) -> Sender<ControllerCommand> {
        self.command_sender.clone()
    }

    pub fn state(&self) -> SharedControllerState {
        self.state.clone()
    }

    pub fn take_app_events(&mut self) -> Receiver<AppEvent> {
        self.app_events
            .take()
            .expect("背景控制器 UI event receiver 已被取用")
    }

    pub fn shutdown_internal(&self) {
        let _ = self
            .command_sender
            .send(ControllerCommand::ShutdownInternal);
    }

    pub fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ControllerHandle {
    fn drop(&mut self) {
        self.shutdown_internal();
        self.join();
    }
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_controller(
    initial_lifecycle: LifecycleState,
    engine_commands: Sender<EngineCommand>,
    engine_events: Receiver<EngineEvent>,
    tray: TrayController,
    tray_events: Receiver<TrayEvent>,
    window: Arc<dyn MainWindowControl>,
    manual_stop_path: PathBuf,
    ipc_stop: Arc<AtomicBool>,
) -> Result<ControllerHandle, String> {
    let state = SharedControllerState::new(initial_lifecycle);
    let (command_sender, command_receiver) = mpsc::channel();
    let (app_event_sender, app_event_receiver) = mpsc::channel();
    let thread_state = state.clone();
    let thread_command_sender = engine_commands.clone();
    let thread = thread::Builder::new()
        .name("curl-downloader-controller".into())
        .spawn(move || {
            run_controller(
                thread_state,
                command_receiver,
                engine_events,
                thread_command_sender,
                tray,
                tray_events,
                window,
                manual_stop_path,
                ipc_stop,
                app_event_sender,
            )
        })
        .map_err(|error| format!("無法啟動背景控制器：{error}"))?;
    Ok(ControllerHandle {
        command_sender,
        state,
        app_events: Some(app_event_receiver),
        thread: Some(thread),
    })
}

#[allow(clippy::too_many_arguments)]
fn run_controller(
    state: SharedControllerState,
    command_receiver: Receiver<ControllerCommand>,
    engine_events: Receiver<EngineEvent>,
    engine_commands: Sender<EngineCommand>,
    _tray: TrayController,
    tray_events: Receiver<TrayEvent>,
    window: Arc<dyn MainWindowControl>,
    manual_stop_path: PathBuf,
    ipc_stop: Arc<AtomicBool>,
    app_events: Sender<AppEvent>,
) {
    let mut shutdown_deadline = None;
    let mut pending_show_task = None;
    loop {
        while let Ok(command) = command_receiver.try_recv() {
            handle_command(
                command,
                &state,
                &engine_commands,
                &window,
                &manual_stop_path,
                &ipc_stop,
                &app_events,
                &mut shutdown_deadline,
                &mut pending_show_task,
            );
        }
        while let Ok(event) = tray_events.try_recv() {
            match event {
                TrayEvent::ShowWindow => {
                    show_window(&state, &window, &app_events, None);
                }
                TrayEvent::CloseWindow => begin_shutdown(
                    true,
                    &state,
                    &engine_commands,
                    &manual_stop_path,
                    &ipc_stop,
                    &mut shutdown_deadline,
                ),
            }
        }

        match engine_events.recv_timeout(Duration::from_millis(25)) {
            Ok(EngineEvent::Snapshot(tasks)) => {
                state.replace_tasks(tasks);
                state.mark_engine_ready();
                if pending_show_task.is_some_and(|task_id| state.task_exists(task_id)) {
                    let task_id = pending_show_task.take().expect("task id was checked");
                    show_window(&state, &window, &app_events, Some(task_id));
                }
                let _ = app_events.send(AppEvent::SnapshotChanged);
            }
            Ok(EngineEvent::BatchProxyApplied { applied, skipped }) => {
                let _ = app_events.send(AppEvent::BatchProxyApplied { applied, skipped });
            }
            Ok(EngineEvent::Fatal(message)) => {
                let _ = app_events.send(AppEvent::Fatal(message));
            }
            Ok(EngineEvent::ShutdownComplete) => {
                state.set_lifecycle(LifecycleState::Stopped);
                window.request_close();
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if shutdown_deadline.is_some_and(|deadline| deadline.elapsed() >= Duration::from_secs(10)) {
            state.set_lifecycle(LifecycleState::Stopped);
            window.request_close();
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_command(
    command: ControllerCommand,
    state: &SharedControllerState,
    engine_commands: &Sender<EngineCommand>,
    window: &Arc<dyn MainWindowControl>,
    manual_stop_path: &Path,
    ipc_stop: &Arc<AtomicBool>,
    app_events: &Sender<AppEvent>,
    shutdown_deadline: &mut Option<Instant>,
    pending_show_task: &mut Option<TaskId>,
) {
    match command {
        ControllerCommand::ShowWindow => show_window(state, window, app_events, None),
        ControllerCommand::ShowTask { task_id } => {
            if state.task_exists(task_id) {
                show_window(state, window, app_events, Some(task_id));
            } else {
                *pending_show_task = Some(task_id);
            }
        }
        ControllerCommand::WindowHidden => {
            if state.lifecycle() == LifecycleState::RunningVisible {
                state.set_lifecycle(LifecycleState::RunningHidden);
            }
        }
        ControllerCommand::WindowVisible => {
            if matches!(
                state.lifecycle(),
                LifecycleState::RunningHidden | LifecycleState::Starting
            ) {
                state.set_lifecycle(LifecycleState::RunningVisible);
            }
        }
        ControllerCommand::ShutdownManual => begin_shutdown(
            true,
            state,
            engine_commands,
            manual_stop_path,
            ipc_stop,
            shutdown_deadline,
        ),
        ControllerCommand::ShutdownInternal => begin_shutdown(
            false,
            state,
            engine_commands,
            manual_stop_path,
            ipc_stop,
            shutdown_deadline,
        ),
    }
}

fn show_window(
    state: &SharedControllerState,
    window: &Arc<dyn MainWindowControl>,
    app_events: &Sender<AppEvent>,
    task_id: Option<TaskId>,
) {
    if matches!(
        state.lifecycle(),
        LifecycleState::ShuttingDown | LifecycleState::Stopped
    ) {
        return;
    }
    state.set_lifecycle(LifecycleState::RunningVisible);
    window.show_and_focus();
    let event = task_id.map_or(AppEvent::ShowWindow, |task_id| AppEvent::ShowTask {
        task_id,
    });
    let _ = app_events.send(event);
}

fn begin_shutdown(
    manual: bool,
    state: &SharedControllerState,
    engine_commands: &Sender<EngineCommand>,
    manual_stop_path: &Path,
    ipc_stop: &Arc<AtomicBool>,
    shutdown_deadline: &mut Option<Instant>,
) {
    if !state.begin_shutdown() {
        return;
    }
    if manual {
        let _ =
            startup_policy::record_manual_stop(manual_stop_path, startup_policy::unix_time_ms());
    }
    let _ = session_shutdown::signal_manual_shutdown();
    ipc_stop.store(true, Ordering::Release);
    let _ = engine_commands.send(EngineCommand::Shutdown);
    *shutdown_deadline = Some(Instant::now());
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CurlSource, ProxyProtocol, ProxySnapshot, RangeSupport, TaskOrigin, TaskSnapshot,
        TaskStatus,
    };
    use std::{path::PathBuf, time::Duration};

    fn snapshot(status: TaskStatus) -> TaskSnapshot {
        TaskSnapshot {
            id: 7,
            original_url: "https://example.test/file.bin".into(),
            effective_url: None,
            filename: "file.bin".into(),
            target_dir: PathBuf::from(r"C:\Downloads"),
            status,
            origin: TaskOrigin::Gui,
            requested_segments: 1,
            actual_segments: 1,
            segments: Vec::new(),
            downloaded: 1,
            total_size: Some(1),
            range_support: RangeSupport::Unknown,
            current_bps: 0.0,
            average_bps: 0.0,
            eta_seconds: None,
            active_millis: 0,
            created_unix_ms: 1,
            completed_unix_ms: None,
            proxy: ProxySnapshot {
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

    #[test]
    fn snapshot_updates_are_visible_without_a_gui_pump() {
        let state = SharedControllerState::new(LifecycleState::RunningHidden);
        state.replace_tasks(vec![snapshot(TaskStatus::Completed)]);
        assert!(state.task_exists(7));
        assert_eq!(state.tasks()[0].id, 7);
    }

    #[test]
    fn shutdown_transition_is_idempotent() {
        let state = SharedControllerState::new(LifecycleState::RunningHidden);
        assert!(state.begin_shutdown());
        assert!(!state.begin_shutdown());
        assert_eq!(state.lifecycle(), LifecycleState::ShuttingDown);
    }

    #[test]
    fn initial_snapshot_readiness_can_be_waited_for() {
        let state = SharedControllerState::new(LifecycleState::Starting);
        assert!(!state.wait_ready(Duration::from_millis(1)));
        state.mark_ready();
        assert!(state.wait_ready(Duration::from_millis(1)));
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::{
        ControllerCommand, ControllerHandle, LifecycleState, SharedControllerState,
        spawn_controller,
    };
    use crate::{
        model::{
            CurlSource, EngineCommand, EngineEvent, ProxyProtocol, ProxySnapshot, RangeSupport,
            TaskOrigin, TaskSnapshot, TaskStatus,
        },
        startup_policy,
        tray::{TrayController, TrayEvent},
        window_control::MainWindowControl,
    };
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc::{self, Receiver, Sender},
        },
        thread,
        time::{Duration, Instant},
    };

    #[derive(Default)]
    struct RecordingWindow {
        shown: AtomicUsize,
        closed: AtomicUsize,
    }

    impl MainWindowControl for RecordingWindow {
        fn show_and_focus(&self) {
            self.shown.fetch_add(1, Ordering::AcqRel);
        }

        fn hide(&self) {}

        fn request_close(&self) {
            self.closed.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct ControllerFixture {
        handle: ControllerHandle,
        commands: Sender<ControllerCommand>,
        engine_events: Sender<EngineEvent>,
        engine_commands: Receiver<EngineCommand>,
        tray_events: Sender<TrayEvent>,
        state: SharedControllerState,
        window: Arc<RecordingWindow>,
        manual_stop_path: PathBuf,
        ipc_stop: Arc<AtomicBool>,
    }

    impl ControllerFixture {
        fn hidden() -> Self {
            let (engine_commands, engine_commands_receiver) = mpsc::channel();
            let (engine_events_sender, engine_events_receiver) = mpsc::channel();
            let (tray, _tray_events_receiver) = TrayController::disabled();
            let (tray_events_sender, tray_events_receiver_for_controller) = mpsc::channel();
            let window = Arc::new(RecordingWindow::default());
            let ipc_stop = Arc::new(AtomicBool::new(false));
            let manual_stop_path = std::env::temp_dir().join(format!(
                "curl-downloader-controller-test-{}-{}.json",
                std::process::id(),
                startup_policy::unix_time_ms()
            ));
            let handle = spawn_controller(
                LifecycleState::RunningHidden,
                engine_commands,
                engine_events_receiver,
                tray,
                tray_events_receiver_for_controller,
                window.clone(),
                manual_stop_path.clone(),
                ipc_stop.clone(),
            )
            .expect("controller should start");
            let commands = handle.commands();
            let state = handle.state();
            Self {
                handle,
                commands,
                engine_events: engine_events_sender,
                engine_commands: engine_commands_receiver,
                tray_events: tray_events_sender,
                state,
                window,
                manual_stop_path,
                ipc_stop,
            }
        }

        fn snapshot(&self, id: u64, status: TaskStatus) -> TaskSnapshot {
            TaskSnapshot {
                id,
                original_url: "https://example.test/file.bin".into(),
                effective_url: None,
                filename: "file.bin".into(),
                target_dir: PathBuf::from(r"C:\Downloads"),
                status,
                origin: TaskOrigin::Gui,
                requested_segments: 1,
                actual_segments: 1,
                segments: Vec::new(),
                downloaded: u64::from(status == TaskStatus::Completed),
                total_size: Some(1),
                range_support: RangeSupport::Unknown,
                current_bps: 0.0,
                average_bps: 0.0,
                eta_seconds: None,
                active_millis: 0,
                created_unix_ms: 1,
                completed_unix_ms: (status == TaskStatus::Completed).then_some(2),
                proxy: ProxySnapshot {
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

        fn wait_for(&self, condition: impl Fn() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !condition() {
                assert!(Instant::now() < deadline, "controller condition timed out");
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Drop for ControllerFixture {
        fn drop(&mut self) {
            let _ = self.engine_events.send(EngineEvent::ShutdownComplete);
            self.handle.join();
            let _ = startup_policy::clear_manual_stop(&self.manual_stop_path);
        }
    }

    #[test]
    fn hidden_controller_publishes_engine_snapshots_immediately() {
        let fixture = ControllerFixture::hidden();
        fixture
            .engine_events
            .send(EngineEvent::Snapshot(vec![
                fixture.snapshot(9, TaskStatus::Completed),
            ]))
            .unwrap();
        fixture.wait_for(|| fixture.state.task_exists(9));
        assert_eq!(fixture.window.shown.load(Ordering::Acquire), 0);
    }

    #[test]
    fn show_command_uses_win32_control_without_waiting_for_ui() {
        let fixture = ControllerFixture::hidden();
        fixture
            .commands
            .send(ControllerCommand::ShowWindow)
            .unwrap();
        fixture.wait_for(|| fixture.window.shown.load(Ordering::Acquire) == 1);
        assert_eq!(fixture.state.lifecycle(), LifecycleState::RunningVisible);
    }

    #[test]
    fn show_task_waits_for_the_snapshot_after_enqueue() {
        let fixture = ControllerFixture::hidden();
        fixture
            .commands
            .send(ControllerCommand::ShowTask { task_id: 42 })
            .unwrap();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(fixture.window.shown.load(Ordering::Acquire), 0);
        fixture
            .engine_events
            .send(EngineEvent::Snapshot(vec![
                fixture.snapshot(42, TaskStatus::Queued),
            ]))
            .unwrap();
        fixture.wait_for(|| fixture.window.shown.load(Ordering::Acquire) == 1);
        assert_eq!(fixture.state.lifecycle(), LifecycleState::RunningVisible);
    }

    #[test]
    fn tray_close_records_one_manual_shutdown() {
        let fixture = ControllerFixture::hidden();
        fixture.tray_events.send(TrayEvent::CloseWindow).unwrap();
        fixture.tray_events.send(TrayEvent::CloseWindow).unwrap();
        assert!(matches!(
            fixture
                .engine_commands
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            EngineCommand::Shutdown
        ));
        assert!(
            fixture
                .engine_commands
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
        fixture.wait_for(|| fixture.state.lifecycle() == LifecycleState::ShuttingDown);
        assert!(
            startup_policy::read_manual_stop(&fixture.manual_stop_path)
                .unwrap()
                .is_some()
        );
        assert!(fixture.ipc_stop.load(Ordering::Acquire));
    }

    #[test]
    fn shutdown_command_stops_engine_and_records_manual_stop() {
        let fixture = ControllerFixture::hidden();
        fixture
            .commands
            .send(ControllerCommand::ShutdownManual)
            .unwrap();
        let engine_command = fixture
            .engine_commands
            .recv_timeout(Duration::from_secs(2))
            .expect("engine should receive shutdown");
        assert!(matches!(engine_command, EngineCommand::Shutdown));
        fixture.wait_for(|| fixture.state.lifecycle() == LifecycleState::ShuttingDown);
        fixture.wait_for(|| fixture.ipc_stop.load(Ordering::Acquire));
        assert!(
            startup_policy::read_manual_stop(&fixture.manual_stop_path)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn engine_shutdown_completion_closes_window_once() {
        let fixture = ControllerFixture::hidden();
        fixture
            .commands
            .send(ControllerCommand::ShutdownManual)
            .unwrap();
        assert!(matches!(
            fixture
                .engine_commands
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            EngineCommand::Shutdown
        ));
        fixture
            .engine_events
            .send(EngineEvent::ShutdownComplete)
            .unwrap();
        fixture.wait_for(|| fixture.state.lifecycle() == LifecycleState::Stopped);
        fixture.wait_for(|| fixture.window.closed.load(Ordering::Acquire) == 1);
        assert_eq!(fixture.window.closed.load(Ordering::Acquire), 1);
    }
}
