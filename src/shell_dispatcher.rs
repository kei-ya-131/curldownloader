use crate::shell_foreground::{self, OpenTargetOutcome};
use std::io;
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
};

const SHELL_QUEUE_CAPACITY: usize = 8;

#[derive(Debug)]
pub struct ShellResult {
    pub path: PathBuf,
    pub result: Result<OpenTargetOutcome, String>,
}

pub struct ShellDispatcher {
    sender: SyncSender<PathBuf>,
    results: Receiver<ShellResult>,
    outstanding: Arc<Mutex<HashSet<PathBuf>>>,
}

impl ShellDispatcher {
    pub fn new() -> io::Result<Self> {
        Self::with_opener(|path| {
            shell_foreground::reveal_file_foreground(&path).map_err(|error| error.to_string())
        })
    }

    fn with_opener(
        opener: impl Fn(PathBuf) -> Result<OpenTargetOutcome, String> + Send + 'static,
    ) -> io::Result<Self> {
        let (sender, requests) = mpsc::sync_channel::<PathBuf>(SHELL_QUEUE_CAPACITY);
        let (results_sender, results) = mpsc::channel();
        let outstanding = Arc::new(Mutex::new(HashSet::new()));
        let worker_outstanding = Arc::clone(&outstanding);
        thread::Builder::new()
            .name("shell-open-worker".into())
            .spawn(move || {
                while let Ok(path) = requests.recv() {
                    let result = opener(path.clone());
                    if let Ok(mut paths) = worker_outstanding.lock() {
                        paths.remove(&path);
                    }
                    let _ = results_sender.send(ShellResult { path, result });
                }
            })?;
        Ok(Self {
            sender,
            results,
            outstanding,
        })
    }

    /// Queue a file-selection request. Duplicate paths share the in-flight request.
    pub fn open_location(&self, path: PathBuf) -> Result<bool, String> {
        let mut outstanding = self
            .outstanding
            .lock()
            .map_err(|_| "無法存取檔案總管工作佇列".to_owned())?;
        if !outstanding.insert(path.clone()) {
            return Ok(false);
        }
        match self.sender.try_send(path.clone()) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => {
                outstanding.remove(&path);
                Err("檔案總管工作佇列已滿，請稍後再試".into())
            }
            Err(TrySendError::Disconnected(_)) => {
                outstanding.remove(&path);
                Err("檔案總管背景工作已停止".into())
            }
        }
    }

    pub fn try_result(&self) -> Option<ShellResult> {
        self.results.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    fn wait_for_result(dispatcher: &ShellDispatcher) -> ShellResult {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = dispatcher.try_result() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "shell worker did not report result"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn requests_run_on_one_worker_and_duplicate_paths_are_deduplicated() {
        let (started, started_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let dispatcher = ShellDispatcher::with_opener(move |path| {
            started.send(path.clone()).unwrap();
            release_rx.recv().unwrap();
            Ok(OpenTargetOutcome::OpenedButNotFocused)
        })
        .unwrap();
        let path = PathBuf::from("C:\\Downloads");

        assert_eq!(dispatcher.open_location(path.clone()), Ok(true));
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            path
        );
        assert_eq!(dispatcher.open_location(path.clone()), Ok(false));
        release.send(()).unwrap();

        let result = wait_for_result(&dispatcher);
        assert_eq!(result.path, path);
        assert_eq!(result.result, Ok(OpenTargetOutcome::OpenedButNotFocused));
    }

    #[test]
    fn different_files_in_one_folder_are_separate_selection_requests() {
        let (started, started_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let dispatcher = ShellDispatcher::with_opener(move |path| {
            started.send(path).unwrap();
            release_rx.recv().unwrap();
            Ok(OpenTargetOutcome::Focused)
        })
        .unwrap();
        let first = PathBuf::from(r"C:\Downloadsirst.bin");
        let second = PathBuf::from(r"C:\Downloads\second.bin");
        assert_eq!(dispatcher.open_location(first.clone()), Ok(true));
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            first
        );
        assert_eq!(dispatcher.open_location(first.clone()), Ok(false));
        assert_eq!(dispatcher.open_location(second.clone()), Ok(true));
        release.send(()).unwrap();
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            second
        );
        release.send(()).unwrap();
        assert_eq!(wait_for_result(&dispatcher).path, first);
        assert_eq!(wait_for_result(&dispatcher).path, second);
    }

    #[test]
    fn worker_errors_are_returned_to_the_caller() {
        let dispatcher = ShellDispatcher::with_opener(|_| Err("Explorer failed".into())).unwrap();
        let path = PathBuf::from("C:\\Downloads");

        assert_eq!(dispatcher.open_location(path.clone()), Ok(true));
        let result = wait_for_result(&dispatcher);

        assert_eq!(result.path, path);
        assert_eq!(result.result, Err("Explorer failed".into()));
    }
}
