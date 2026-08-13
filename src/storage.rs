use crate::model::{DownloadTask, PersistedState, TargetFingerprint};
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

pub fn manual_stop_path(state_path: &Path) -> PathBuf {
    crate::startup_policy::manual_stop_path(state_path)
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
    task_work_dir_for(&task.target_dir, task.id)
}

pub fn task_work_root(target_dir: &Path) -> PathBuf {
    target_dir.join(".curl-downloader")
}

pub fn task_work_dir_for(target_dir: &Path, id: u64) -> PathBuf {
    task_work_root(target_dir).join(id.to_string())
}

/// Create the task's temporary directory and keep the implementation folder
/// out of normal Explorer views on Windows.
pub fn ensure_task_work_dir(task: &DownloadTask) -> io::Result<PathBuf> {
    let root = task_work_root(&task.target_dir);
    fs::create_dir_all(&root)?;
    mark_hidden(&root)?;
    let dir = task_work_dir(task);
    fs::create_dir_all(&dir)?;
    mark_hidden(&dir)?;
    Ok(dir)
}

/// Remove a task's temporary directory.  Once the implementation root is
/// empty, remove it as well so completed/cancelled tasks leave no residue.
pub fn cleanup_task_work_dir(task: &DownloadTask) -> io::Result<()> {
    let root = task_work_root(&task.target_dir);
    let dir = task_work_dir(task);
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    if root.is_dir() {
        let mut entries = fs::read_dir(&root)?;
        if entries.next().transpose()?.is_none() {
            fs::remove_dir(&root)?;
        }
    }
    Ok(())
}

pub fn target_fingerprint(path: &Path) -> io::Result<Option<TargetFingerprint>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "目標路徑不是檔案",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let modified_unix_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    #[cfg(windows)]
    let file_identity = {
        // The stable Windows MetadataExt file-index APIs are still gated on
        // older toolchains.  Creation time is a fast, replacement-sensitive
        // identity fallback; the final digest check remains authoritative.
        metadata
            .created()
            .ok()
            .and_then(|created| created.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| {
                let value = duration.as_nanos();
                [value as u64, (value >> 64) as u64]
            })
    };
    #[cfg(unix)]
    let file_identity = {
        use std::os::unix::fs::MetadataExt;
        Some([metadata.dev(), metadata.ino()])
    };
    #[cfg(not(any(windows, unix)))]
    let file_identity = None;
    Ok(Some(TargetFingerprint {
        length: metadata.len(),
        modified_unix_nanos,
        file_identity,
        content_digest: None,
    }))
}

pub fn target_fingerprint_with_digest(path: &Path) -> io::Result<Option<TargetFingerprint>> {
    let Some(mut fingerprint) = target_fingerprint(path)? else {
        return Ok(None);
    };
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    fingerprint.content_digest = Some(hasher.finalize().into());
    Ok(Some(fingerprint))
}

#[cfg(windows)]
fn mark_hidden(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_HIDDEN, GetFileAttributesW, INVALID_FILE_ATTRIBUTES, SetFileAttributesW,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a null-terminated UTF-16 path that remains alive for
    // both Win32 calls.
    let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Err(io::Error::last_os_error());
    }
    if attributes & FILE_ATTRIBUTE_HIDDEN != 0 {
        return Ok(());
    }
    let updated = attributes | FILE_ATTRIBUTE_HIDDEN;
    // SAFETY: the path is valid for the duration of the call.
    if unsafe { SetFileAttributesW(wide.as_ptr(), updated) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn mark_hidden(_path: &Path) -> io::Result<()> {
    Ok(())
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
    fn manual_stop_path_shares_the_state_directory() {
        let state = PathBuf::from(r"C:\Portable\data\state.json");
        assert_eq!(
            manual_stop_path(&state),
            PathBuf::from(r"C:\Portable\data\manual-stop.json")
        );
    }

    #[test]
    fn saves_state_without_proxy_password() {
        let dir = test_dir("secret");
        let path = dir.join("state.json");
        let mut task = DownloadTask::new(1, "https://example.test/a", "a.bin".into(), dir.clone());
        task.proxy.enabled = true;
        task.proxy.set_password("never-write-me".into()).unwrap();
        let state = PersistedState {
            schema_version: CURRENT_SCHEMA_VERSION,
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
    fn legacy_schema_v1_segment_loads_and_resaves_with_current_schema() {
        let dir = test_dir("legacy-segment");
        let path = dir.join("state.json");
        let raw = serde_json::json!({
            "schema_version": 1,
            "settings": {
                "last_download_dir": dir,
                "max_curl_processes": 4,
                "next_task_id": 2
            },
            "tasks": [{
                "id": 1,
                "original_url": "https://example.test/a.bin",
                "effective_url": null,
                "filename": "a.bin",
                "target_dir": dir,
                "requested_segments": 1,
                "actual_segments": 1,
                "total_size": 100,
                "etag": null,
                "last_modified": null,
                "range_support": "Unknown",
                "proxy": {
                    "enabled": false,
                    "protocol": "Http",
                    "host": "",
                    "port": 8080,
                    "username": "",
                    "requires_password": false
                },
                "status": "Completed",
                "segments": [{
                    "index": 0,
                    "start": 0,
                    "end": 99,
                    "downloaded": 100
                }],
                "active_millis": 0,
                "created_unix_ms": 10,
                "completed_unix_ms": 1000,
                "last_error": null
            }]
        });
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

        let mut loaded = load_state(&path).unwrap();
        assert_eq!(loaded.schema_version, 1);
        assert_eq!(loaded.tasks[0].segments[0].active_millis, 0);
        assert_eq!(loaded.tasks[0].segments[0].started_unix_ms, None);
        assert_eq!(loaded.tasks[0].segments[0].completed_unix_ms, None);

        loaded.schema_version = CURRENT_SCHEMA_VERSION;
        save_state(&path, &loaded).unwrap();
        let upgraded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(upgraded["schema_version"], CURRENT_SCHEMA_VERSION);
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
        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
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
            schema_version: CURRENT_SCHEMA_VERSION,
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
