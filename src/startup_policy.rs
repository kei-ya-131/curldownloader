use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct NativeStartPolicy {
    #[serde(default, alias = "autoStart")]
    pub auto_start: bool,
    #[serde(default, alias = "startIntentUnixMs")]
    pub start_intent_unix_ms: Option<u64>,
}

impl NativeStartPolicy {
    pub fn permits_start(self, stopped_unix_ms: Option<u64>) -> bool {
        if !self.auto_start {
            return false;
        }
        match stopped_unix_ms {
            None => true,
            Some(stopped) => self
                .start_intent_unix_ms
                .is_some_and(|intent| intent > stopped),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct ManualStopState {
    stopped_unix_ms: u64,
}

pub fn manual_stop_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("manual-stop.json")
}

pub fn record_manual_stop(path: &Path, stopped_unix_ms: u64) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "手動停止檔沒有父目錄"))?;
    fs::create_dir_all(parent)?;
    let bytes =
        serde_json::to_vec(&ManualStopState { stopped_unix_ms }).map_err(io::Error::other)?;
    fs::write(path, bytes)?;
    Ok(())
}

pub fn read_manual_stop(path: &Path) -> io::Result<Option<u64>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<ManualStopState>(&bytes)
            .map(|state| Some(state.stopped_unix_ms))
            .map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn clear_manual_stop(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_never_starts_and_only_new_intent_overrides_manual_stop() {
        let background = NativeStartPolicy {
            auto_start: false,
            start_intent_unix_ms: None,
        };
        assert!(!background.permits_start(Some(100)));

        let stale = NativeStartPolicy {
            auto_start: true,
            start_intent_unix_ms: Some(100),
        };
        assert!(!stale.permits_start(Some(100)));

        let fresh = NativeStartPolicy {
            auto_start: true,
            start_intent_unix_ms: Some(101),
        };
        assert!(fresh.permits_start(Some(100)));
        assert!(fresh.permits_start(None));
    }

    #[test]
    fn passive_start_can_launch_idle_gui_but_never_override_manual_stop() {
        let passive = NativeStartPolicy {
            auto_start: true,
            start_intent_unix_ms: None,
        };
        assert!(passive.permits_start(None));
        assert!(!passive.permits_start(Some(100)));

        let explicit = NativeStartPolicy {
            auto_start: true,
            start_intent_unix_ms: Some(101),
        };
        assert!(explicit.permits_start(Some(100)));
    }

    #[test]
    fn manual_stop_file_contains_only_the_stop_time() {
        let root = std::env::temp_dir().join(format!(
            "curl-downloader-stop-policy-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let path = root.join("manual-stop.json");

        record_manual_stop(&path, 1234).unwrap();
        assert_eq!(read_manual_stop(&path).unwrap(), Some(1234));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"stopped_unix_ms":1234}"#
        );
        clear_manual_stop(&path).unwrap();
        assert_eq!(read_manual_stop(&path).unwrap(), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
