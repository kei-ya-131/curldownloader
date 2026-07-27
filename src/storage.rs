use crate::model::{DownloadTask, PersistedState};
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const PORTABLE_MARKER: &str = "portable.flag";

pub fn state_path() -> io::Result<PathBuf> {
    if let Some(executable) = current_portable_executable() {
        return Ok(portable_state_path(&executable));
    }

    let base = env::var_os("APPDATA")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "找不到 APPDATA"))?;
    Ok(PathBuf::from(base)
        .join("CurlDownloader")
        .join("state.json"))
}

pub fn portable_state_path(executable: &Path) -> PathBuf {
    executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("data")
        .join("state.json")
}

fn current_portable_executable() -> Option<PathBuf> {
    let executable = env::current_exe().ok()?;
    let root = executable.parent()?;
    let environment_enabled = env::var_os("CURL_DOWNLOADER_PORTABLE")
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    (environment_enabled || root.join(PORTABLE_MARKER).is_file()).then_some(executable)
}

pub fn default_download_dir() -> io::Result<PathBuf> {
    let home = PathBuf::from(
        env::var_os("USERPROFILE")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "找不到 USERPROFILE"))?,
    );
    let downloads = home.join("Downloads");
    Ok(if downloads.is_dir() { downloads } else { home })
}

pub fn save_state(path: &Path, state: &PersistedState) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "狀態路徑沒有父目錄"))?;
    fs::create_dir_all(parent)?;

    let temporary = path.with_extension("json.tmp");
    let backup = path.with_extension("json.bak");
    let bytes = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    let mut file = fs::File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);

    if path.exists() {
        let _ = fs::remove_file(&backup);
        fs::rename(path, &backup)?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err(error);
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

pub fn load_state(path: &Path) -> io::Result<PersistedState> {
    let backup = path.with_extension("json.bak");
    if !path.exists() && backup.exists() {
        fs::rename(&backup, path)?;
    }
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

pub fn quarantine_corrupt(path: &Path) -> io::Result<PathBuf> {
    let target = path.with_extension(format!("json.corrupt-{}", std::process::id()));
    fs::rename(path, &target)?;
    Ok(target)
}

pub fn task_work_dir(task: &DownloadTask) -> PathBuf {
    task.target_dir
        .join(".curl-downloader")
        .join(task.id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    #[test]
    fn portable_state_path_lives_next_to_executable() {
        let root = test_dir("portable-state");
        let executable = root.join("CurlDownloader.exe");
        assert_eq!(
            portable_state_path(&executable),
            root.join("data").join("state.json")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saves_state_without_proxy_password() {
        let dir = test_dir("secret");
        let path = dir.join("state.json");
        let mut task = DownloadTask::new(1, "https://example.test/a", "a.bin".into(), dir.clone());
        task.proxy.enabled = true;
        task.proxy.set_password("never-write-me".into()).unwrap();
        let state = PersistedState {
            schema_version: 1,
            settings: GlobalSettings {
                last_download_dir: dir.clone(),
                max_curl_processes: 4,
                next_task_id: 2,
            },
            tasks: vec![task],
        };
        save_state(&path, &state).unwrap();
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(!json.contains("never-write-me"));
        assert!(load_state(&path).unwrap().tasks[0].proxy.password.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restores_backup_when_primary_state_is_missing() {
        let dir = test_dir("backup");
        let path = dir.join("state.json");
        let backup = path.with_extension("json.bak");
        let state = minimal_state(&dir);
        std::fs::write(&backup, serde_json::to_vec(&state).unwrap()).unwrap();

        let loaded = load_state(&path).unwrap();
        assert_eq!(loaded.schema_version, 1);
        assert!(path.exists());
        assert!(!backup.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn quarantines_malformed_state_file() {
        let dir = test_dir("corrupt");
        let path = dir.join("state.json");
        std::fs::write(&path, b"{not-json").unwrap();

        let quarantined = quarantine_corrupt(&path).unwrap();
        assert!(!path.exists());
        assert!(quarantined.exists());
        assert!(
            quarantined
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("state.json.corrupt-")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn minimal_state(dir: &std::path::Path) -> PersistedState {
        PersistedState {
            schema_version: 1,
            settings: GlobalSettings {
                last_download_dir: dir.to_path_buf(),
                max_curl_processes: 4,
                next_task_id: 1,
            },
            tasks: Vec::new(),
        }
    }

    fn test_dir(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("curl-downloader-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
