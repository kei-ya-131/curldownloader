use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf, sync::mpsc::Sender};
use url::Host;
use zeroize::Zeroizing;

pub type TaskId = u64;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProxyProtocol {
    #[default]
    Http,
    Https,
    Socks5,
    Socks5h,
}

impl ProxyProtocol {
    pub fn scheme(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks5 => "socks5",
            Self::Socks5h => "socks5h",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::Http | Self::Https => 8080,
            Self::Socks5 | Self::Socks5h => 1080,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ProxySettings {
    pub enabled: bool,
    pub protocol: ProxyProtocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(skip, default)]
    pub password: Option<Zeroizing<String>>,
    pub requires_password: bool,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            protocol: ProxyProtocol::Http,
            host: String::new(),
            port: 8080,
            username: String::new(),
            password: None,
            requires_password: false,
        }
    }
}

impl fmt::Debug for ProxySettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxySettings")
            .field("enabled", &self.enabled)
            .field("protocol", &self.protocol)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("requires_password", &self.requires_password)
            .finish()
    }
}

impl ProxySettings {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.host.is_empty() || self.host.contains(['\0', '\r', '\n']) {
            return Err("Proxy 主機含有不允許字元".into());
        }
        if self.host.contains("://")
            || self.host.contains(['/', '?', '#', '@'])
            || Host::parse(self.host.trim_matches(['[', ']'])).is_err()
        {
            return Err("Proxy 主機格式無效".into());
        }
        if self.port == 0 {
            return Err("Proxy 連接埠必須介乎 1 至 65535".into());
        }
        if self.username.contains(['\0', '\r', '\n', ':']) {
            return Err("Proxy 帳號含有不允許字元".into());
        }
        if self
            .password
            .as_deref()
            .is_some_and(|password| password.contains(['\0', '\r', '\n']))
        {
            return Err("Proxy 密碼含有不允許字元".into());
        }
        Ok(())
    }

    pub fn set_password(&mut self, password: String) -> Result<(), String> {
        if password.contains(['\0', '\r', '\n']) {
            return Err("Proxy 密碼含有不允許字元".into());
        }
        self.requires_password = !password.is_empty();
        self.password = (!password.is_empty()).then(|| Zeroizing::new(password));
        Ok(())
    }

    pub fn set_password_secret(&mut self, password: Zeroizing<String>) -> Result<(), String> {
        if password.contains(['\0', '\r', '\n']) {
            return Err("Proxy 密碼含有不允許字元".into());
        }
        self.requires_password = !password.is_empty();
        self.password = (!password.is_empty()).then_some(password);
        Ok(())
    }

    pub fn clear_password(&mut self) {
        self.password = None;
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TaskStatus {
    Queued,
    Probing,
    Downloading,
    Pausing,
    Paused,
    NeedsProxyPassword,
    Finalizing,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum RangeSupport {
    #[default]
    Unknown,
    Supported,
    Unsupported,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SegmentState {
    pub index: u8,
    pub start: u64,
    pub end: u64,
    pub downloaded: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ErrorKind {
    Input,
    Proxy,
    Network,
    Http,
    Disk,
    Curl,
    SourceChanged,
    Internal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskError {
    pub kind: ErrorKind,
    pub summary: String,
    pub code: Option<i32>,
    pub diagnostic: String,
    pub action: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DownloadTask {
    pub id: TaskId,
    pub original_url: String,
    pub effective_url: Option<String>,
    pub filename: String,
    pub target_dir: PathBuf,
    pub requested_segments: u8,
    pub actual_segments: u8,
    pub total_size: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub range_support: RangeSupport,
    pub proxy: ProxySettings,
    pub status: TaskStatus,
    pub segments: Vec<SegmentState>,
    pub active_millis: u64,
    pub created_unix_ms: u64,
    pub last_error: Option<TaskError>,
}

impl DownloadTask {
    pub fn new(id: TaskId, url: &str, filename: String, target_dir: PathBuf) -> Self {
        Self {
            id,
            original_url: url.into(),
            effective_url: None,
            filename,
            target_dir,
            requested_segments: 4,
            actual_segments: 1,
            total_size: None,
            etag: None,
            last_modified: None,
            range_support: RangeSupport::Unknown,
            proxy: ProxySettings::default(),
            status: TaskStatus::Queued,
            segments: Vec::new(),
            active_millis: 0,
            created_unix_ms: 0,
            last_error: None,
        }
    }

    pub fn recover_after_load(&mut self) {
        self.proxy.password = None;
        self.status = if self.proxy.enabled && self.proxy.requires_password {
            TaskStatus::NeedsProxyPassword
        } else if self.status == TaskStatus::Completed {
            TaskStatus::Completed
        } else {
            TaskStatus::Paused
        };
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GlobalSettings {
    pub last_download_dir: PathBuf,
    pub max_curl_processes: u8,
    pub next_task_id: TaskId,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PersistedState {
    pub schema_version: u32,
    pub settings: GlobalSettings,
    pub tasks: Vec<DownloadTask>,
}

#[derive(Clone, Debug)]
pub struct ProxySnapshot {
    pub enabled: bool,
    pub protocol: ProxyProtocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub requires_password: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CurlSource {
    #[default]
    NotStarted,
    Local,
    Embedded,
}

#[derive(Clone, Debug)]
pub struct TaskSnapshot {
    pub id: TaskId,
    pub original_url: String,
    pub effective_url: Option<String>,
    pub filename: String,
    pub target_dir: PathBuf,
    pub status: TaskStatus,
    pub requested_segments: u8,
    pub actual_segments: u8,
    pub downloaded: u64,
    pub total_size: Option<u64>,
    pub range_support: RangeSupport,
    pub current_bps: f64,
    pub average_bps: f64,
    pub eta_seconds: Option<u64>,
    pub active_millis: u64,
    pub proxy: ProxySnapshot,
    pub error: Option<TaskError>,
    pub curl_source: CurlSource,
}

pub struct NewTask {
    pub url: String,
    pub target_dir: PathBuf,
}

pub struct ConfiguredTask {
    pub url: String,
    pub filename: String,
    pub target_dir: PathBuf,
    pub requested_segments: u8,
    pub proxy: ProxySettings,
}

pub enum EngineCommand {
    Add(NewTask),
    AddBatch(Vec<NewTask>),
    AddConfigured {
        task: ConfiguredTask,
        response: Sender<Result<TaskId, String>>,
    },
    Start(TaskId),
    Pause(TaskId),
    Cancel(TaskId),
    Remove(TaskId),
    ClearHistory,
    StartAll,
    PauseAll,
    UpdateDraft {
        id: TaskId,
        url: String,
        filename: String,
        target_dir: PathBuf,
        requested_segments: u8,
        proxy: ProxySettings,
    },
    UpdateProxy {
        ids: Vec<TaskId>,
        proxy: ProxySettings,
    },
    SetProxyPassword {
        id: TaskId,
        password: Zeroizing<String>,
    },
    SetLastDownloadDir(PathBuf),
    SetMaxProcesses(u8),
    Shutdown,
}

pub enum EngineEvent {
    Snapshot(Vec<TaskSnapshot>),
    BatchProxyApplied { applied: usize, skipped: usize },
    Fatal(String),
    ShutdownComplete,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_proxy_and_redacts_password() {
        let mut proxy = ProxySettings::default();
        proxy.enabled = true;
        proxy.host = "127.0.0.1".into();
        proxy.port = 8080;
        proxy.username = "alice".into();
        proxy.set_password("secret".into()).unwrap();
        assert!(proxy.validate().is_ok());
        assert!(!format!("{proxy:?}").contains("secret"));
    }

    #[test]
    fn rejects_proxy_config_injection() {
        let proxy = ProxySettings {
            enabled: true,
            host: "proxy.local\noutput=stolen".into(),
            port: 8080,
            ..ProxySettings::default()
        };
        assert_eq!(proxy.validate().unwrap_err(), "Proxy 主機含有不允許字元");
    }

    #[test]
    fn restored_authenticated_task_requires_password() {
        let mut task = DownloadTask::new(
            7,
            "https://example.test/a.bin",
            "a.bin".into(),
            "C:\\Downloads".into(),
        );
        task.proxy.enabled = true;
        task.proxy.requires_password = true;
        task.recover_after_load();
        assert_eq!(task.status, TaskStatus::NeedsProxyPassword);
    }
}
