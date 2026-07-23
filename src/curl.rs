use crate::model::{ProxyProtocol, ProxySettings};
use sha2::{Digest, Sha256};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

pub const CURL_BYTES: &[u8] = include_bytes!("../assets/curl.exe");
pub const CURL_EXE_SHA256: &str =
    "8d28c1093e0b6345917d2c1710c67f78f61834d76ef983ea9fb631c75e20312f";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_PROCESS_ID: AtomicU64 = AtomicU64::new(1);

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub struct CurlCommandSpec {
    pub args: Vec<OsString>,
    pub stdin_config: Option<Zeroizing<String>>,
}

impl CurlCommandSpec {
    pub fn base(proxy: &ProxySettings) -> Result<Self, String> {
        proxy.validate()?;
        let mut args = vec!["--disable".into()];
        let mut stdin_config = None;
        if !proxy.enabled {
            args.extend(["--noproxy".into(), "*".into()]);
        } else {
            let host = if proxy.host.contains(':') {
                format!("[{}]", proxy.host.trim_matches(['[', ']']))
            } else {
                proxy.host.clone()
            };
            args.extend([
                "--proxy".into(),
                format!("{}://{}:{}", proxy.protocol.scheme(), host, proxy.port).into(),
                "--noproxy".into(),
                "".into(),
            ]);
            match proxy.protocol {
                ProxyProtocol::Http | ProxyProtocol::Https => args.push("--proxy-anyauth".into()),
                ProxyProtocol::Socks5 | ProxyProtocol::Socks5h => {
                    if !proxy.username.is_empty() || proxy.password.is_some() {
                        args.push("--socks5-basic".into());
                    }
                }
            }
            if !proxy.username.is_empty() || proxy.password.is_some() {
                args.extend(["--config".into(), "-".into()]);
                let user = escape_config(&proxy.username)?;
                let pass =
                    escape_config(proxy.password.as_deref().map(String::as_str).unwrap_or(""))?;
                stdin_config = Some(Zeroizing::new(format!("proxy-user = \"{user}:{pass}\"\n")));
            }
        }
        Ok(Self { args, stdin_config })
    }

    pub fn arguments_text(&self) -> String {
        self.args
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn escape_config(value: &str) -> Result<String, String> {
    if value.contains(['\0', '\r', '\n']) {
        return Err("Proxy 認證含有不允許字元".into());
    }
    Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub struct CurlRuntime {
    pub exe: PathBuf,
    root: PathBuf,
}

impl CurlRuntime {
    pub fn extract() -> io::Result<Self> {
        if sha256_hex(CURL_BYTES) != CURL_EXE_SHA256 {
            return Err(io::Error::other("內嵌 curl 校驗失敗"));
        }
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_millis();
        let runtime_id = NEXT_RUNTIME_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "CurlDownloader-{}-{millis}-{runtime_id}",
            std::process::id()
        ));
        fs::create_dir(&root)?;
        let exe = root.join("curl.exe");
        fs::write(&exe, CURL_BYTES)?;
        if sha256_hex(&fs::read(&exe)?) != CURL_EXE_SHA256 {
            return Err(io::Error::other("解壓 curl 校驗失敗"));
        }
        Ok(Self { exe, root })
    }

    pub fn spawn(&self, spec: &mut CurlCommandSpec, stdout: Stdio) -> io::Result<Child> {
        let process_id = NEXT_PROCESS_ID.fetch_add(1, Ordering::Relaxed);
        let child_exe = self.root.join(format!("curl-process-{process_id}.exe"));
        fs::copy(&self.exe, &child_exe)?;
        let mut command = Command::new(child_exe);
        command
            .args(&spec.args)
            .stdin(if spec.stdin_config.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(stdout)
            .stderr(Stdio::piped());
        #[cfg(target_os = "windows")]
        command.creation_flags(CREATE_NO_WINDOW);
        let mut child = command.spawn()?;
        if let (Some(config), Some(mut stdin)) = (spec.stdin_config.take(), child.stdin.take()) {
            stdin.write_all(config.as_bytes())?;
            stdin.flush()?;
        }
        Ok(child)
    }
}

impl Drop for CurlRuntime {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProbeMetadata {
    pub effective_url: String,
    pub http_code: u16,
    pub total_size: Option<u64>,
    pub range_support: crate::model::RangeSupport,
    pub content_disposition: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug)]
pub struct CurlOutcome {
    pub exit_code: i32,
    pub stderr: String,
    pub headers: String,
}

fn add_transfer_defaults(spec: &mut CurlCommandSpec) {
    spec.args.extend(
        [
            "--location",
            "--fail",
            "--show-error",
            "--silent",
            "--connect-timeout",
            "30",
            "--retry",
            "3",
            "--retry-delay",
            "1",
            "--retry-max-time",
            "60",
        ]
        .into_iter()
        .map(Into::into),
    );
}

pub fn build_head_probe(
    proxy: &ProxySettings,
    url: &str,
    headers: &Path,
) -> Result<CurlCommandSpec, String> {
    let mut spec = CurlCommandSpec::base(proxy)?;
    add_transfer_defaults(&mut spec);
    spec.args.extend([
        "--head".into(),
        "--dump-header".into(),
        headers.as_os_str().into(),
        "--output".into(),
        "NUL".into(),
        "--write-out".into(),
        "%{json}".into(),
        url.into(),
    ]);
    Ok(spec)
}

pub fn build_range_probe(
    proxy: &ProxySettings,
    url: &str,
    headers: &Path,
) -> Result<CurlCommandSpec, String> {
    let mut spec = CurlCommandSpec::base(proxy)?;
    add_transfer_defaults(&mut spec);
    spec.args.extend([
        "--range".into(),
        "0-0".into(),
        "--dump-header".into(),
        headers.as_os_str().into(),
        "--output".into(),
        "NUL".into(),
        "--write-out".into(),
        "%{json}".into(),
        url.into(),
    ]);
    Ok(spec)
}

fn add_if_range(spec: &mut CurlCommandSpec, validator: Option<&str>) -> Result<(), String> {
    if let Some(value) = validator {
        if value.contains(['\0', '\r', '\n']) {
            return Err("來源驗證值含有不允許字元".into());
        }
        spec.args
            .extend(["--header".into(), format!("If-Range: {value}").into()]);
    }
    Ok(())
}

pub fn build_single_transfer(
    proxy: &ProxySettings,
    url: &str,
    output: &Path,
    existing: u64,
    if_range: Option<&str>,
    headers: &Path,
) -> Result<CurlCommandSpec, String> {
    let mut spec = CurlCommandSpec::base(proxy)?;
    add_transfer_defaults(&mut spec);
    spec.args.extend([
        "--dump-header".into(),
        headers.as_os_str().into(),
        "--output".into(),
        output.as_os_str().into(),
    ]);
    if existing > 0 {
        spec.args.extend(["--continue-at".into(), "-".into()]);
    }
    add_if_range(&mut spec, if_range)?;
    spec.args.push(url.into());
    Ok(spec)
}

pub fn build_segment_transfer(
    proxy: &ProxySettings,
    url: &str,
    start: u64,
    end: u64,
    existing: u64,
    if_range: Option<&str>,
    headers: &Path,
) -> Result<CurlCommandSpec, String> {
    let length = end
        .checked_sub(start)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| "分段範圍無效".to_owned())?;
    if existing > length {
        return Err("分段部分檔超出範圍".into());
    }
    let adjusted_start = start
        .checked_add(existing)
        .ok_or_else(|| "分段續傳偏移無效".to_owned())?;
    let remaining = length - existing;
    let mut spec = CurlCommandSpec::base(proxy)?;
    add_transfer_defaults(&mut spec);
    add_if_range(&mut spec, if_range)?;
    spec.args.extend([
        "--range".into(),
        format!("{adjusted_start}-{end}").into(),
        "--max-filesize".into(),
        remaining.to_string().into(),
        "--dump-header".into(),
        headers.as_os_str().into(),
        "--output".into(),
        "-".into(),
        url.into(),
    ]);
    Ok(spec)
}

pub fn parse_probe(headers: &str, fallback_url: &str) -> Result<ProbeMetadata, String> {
    let normalized = headers.replace("\r\n", "\n");
    let blocks: Vec<&str> = normalized.split("\n\n").collect();
    let block = blocks
        .iter()
        .rev()
        .find(|candidate| candidate.trim_start().starts_with("HTTP/"))
        .ok_or_else(|| "找不到 HTTP 探測回應標頭".to_owned())?;
    let mut meta = ProbeMetadata {
        effective_url: fallback_url.to_owned(),
        ..ProbeMetadata::default()
    };
    let mut content_range = None;
    for (index, line) in block.lines().enumerate() {
        if index == 0 {
            let mut parts = line.split_whitespace();
            let _version = parts.next();
            meta.http_code = parts
                .next()
                .ok_or_else(|| "HTTP 狀態列缺少狀態碼".to_owned())?
                .parse()
                .map_err(|_| "HTTP 狀態碼無效".to_owned())?;
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => {
                if meta.total_size.is_none() {
                    meta.total_size = value.parse().ok();
                }
            }
            "content-range" => {
                content_range = parse_content_range(value);
                if let Some((_, _, total)) = content_range {
                    meta.total_size = Some(total);
                }
            }
            "content-disposition" => meta.content_disposition = Some(value.to_owned()),
            "etag" => meta.etag = Some(value.to_owned()),
            "last-modified" => meta.last_modified = Some(value.to_owned()),
            _ => {}
        }
    }
    if meta.http_code == 206 && content_range.is_some() && meta.total_size.is_some() {
        meta.range_support = crate::model::RangeSupport::Supported;
    } else {
        meta.range_support = crate::model::RangeSupport::Unsupported;
    }
    if let Some(json) = normalized.lines().find_map(|line| {
        let line = line.trim();
        line.starts_with('{')
            .then(|| serde_json::from_str::<serde_json::Value>(line).ok())
            .flatten()
    }) {
        if let Some(url) = json.get("url_effective").and_then(|value| value.as_str()) {
            meta.effective_url = url.to_owned();
        }
        if let Some(code) = json.get("http_code").and_then(|value| value.as_u64()) {
            meta.http_code = code as u16;
        }
    }
    Ok(meta)
}

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let rest = value.strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    #[test]
    fn embedded_curl_has_pinned_hash() {
        assert_eq!(sha256_hex(CURL_BYTES), CURL_EXE_SHA256);
    }

    #[test]
    fn direct_mode_ignores_environment_proxy() {
        let spec = CurlCommandSpec::base(&ProxySettings::default()).unwrap();
        assert_eq!(spec.args[0], "--disable");
        assert!(spec.arguments_text().contains("--noproxy *"));
    }

    #[test]
    fn password_only_enters_stdin_config() {
        let mut proxy = ProxySettings {
            enabled: true,
            protocol: ProxyProtocol::Socks5h,
            host: "proxy.test".into(),
            port: 1080,
            username: "alice".into(),
            ..ProxySettings::default()
        };
        proxy.set_password("s3cret".into()).unwrap();
        let spec = CurlCommandSpec::base(&proxy).unwrap();
        assert!(!spec.arguments_text().contains("s3cret"));
        assert!(
            spec.stdin_config
                .as_deref()
                .unwrap()
                .contains("alice:s3cret")
        );
    }

    #[test]
    fn socks_protocols_are_selected_per_task() {
        let mut proxy = ProxySettings {
            enabled: true,
            protocol: ProxyProtocol::Socks5,
            host: "127.0.0.1".into(),
            port: 1080,
            ..ProxySettings::default()
        };
        let socks5 = CurlCommandSpec::base(&proxy).unwrap();
        assert!(socks5.arguments_text().contains("socks5://127.0.0.1:1080"));
        proxy.protocol = ProxyProtocol::Socks5h;
        let socks5h = CurlCommandSpec::base(&proxy).unwrap();
        assert!(
            socks5h
                .arguments_text()
                .contains("socks5h://127.0.0.1:1080")
        );
    }

    #[test]
    fn parses_last_redirect_header_block() {
        let headers = "HTTP/1.1 302 Found\r\nLocation: /file\r\n\r\nHTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1000\r\nContent-Disposition: attachment; filename=real.bin\r\nETag: \"v1\"\r\n\r\n";
        let meta = parse_probe(headers, "https://example.test/file").unwrap();
        assert_eq!(meta.total_size, Some(1000));
        assert_eq!(meta.range_support, crate::model::RangeSupport::Supported);
        assert_eq!(
            meta.content_disposition.as_deref(),
            Some("attachment; filename=real.bin")
        );
    }

    #[test]
    fn segment_resume_uses_adjusted_range_without_continue_at() {
        let proxy = ProxySettings::default();
        let spec = build_segment_transfer(
            &proxy,
            "https://example.test/a",
            100,
            199,
            25,
            Some("\"v1\""),
            std::path::Path::new("headers.txt"),
        )
        .unwrap();
        let args = spec.arguments_text();
        assert!(args.contains("--range 125-199"));
        assert!(!args.contains("--continue-at"));
        assert!(args.contains("If-Range: \"v1\""));
    }
}
