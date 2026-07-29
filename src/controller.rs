use crate::model::{TaskId, TaskSnapshot};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicU8, Ordering},
};
use std::time::Duration;

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

#[derive(Clone)]
pub struct SharedControllerState {
    tasks: Arc<Mutex<Vec<TaskSnapshot>>>,
    lifecycle: Arc<AtomicU8>,
    ready: Arc<(Mutex<bool>, Condvar)>,
}

impl SharedControllerState {
    pub fn new(initial: LifecycleState) -> Self {
        Self {
            tasks: Arc::new(Mutex::new(Vec::new())),
            lifecycle: Arc::new(AtomicU8::new(initial as u8)),
            ready: Arc::new((Mutex::new(false), Condvar::new())),
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

    pub fn mark_ready(&self) {
        let (lock, condition) = &*self.ready;
        if let Ok(mut ready) = lock.lock() {
            *ready = true;
            condition.notify_all();
        }
    }

    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let (lock, condition) = &*self.ready;
        let Ok(ready) = lock.lock() else {
            return false;
        };
        if *ready {
            return true;
        }
        let Ok((ready, _)) = condition.wait_timeout(ready, timeout) else {
            return false;
        };
        *ready
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CurlSource, ProxyProtocol, ProxySnapshot, RangeSupport, TaskSnapshot, TaskStatus,
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
            requested_segments: 1,
            actual_segments: 1,
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
