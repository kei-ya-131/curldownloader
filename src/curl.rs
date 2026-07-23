use crate::model::{ProxyProtocol, ProxySettings};
use sha2::{Digest, Sha256};
use std::os::windows::process::CommandExt;
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

pub const CURL_BYTES: &[u8] = include_bytes!("../assets/curl.exe");
pub const CURL_EXE_SHA256: &str =
    "8d28c1093e0b6345917d2c1710c67f78f61834d76ef983ea9fb631c75e20312f";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
                    args.push("--socks5-basic".into())
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
        let root =
            std::env::temp_dir().join(format!("CurlDownloader-{}-{millis}", std::process::id()));
        fs::create_dir(&root)?;
        let exe = root.join("curl.exe");
        fs::write(&exe, CURL_BYTES)?;
        if sha256_hex(&fs::read(&exe)?) != CURL_EXE_SHA256 {
            return Err(io::Error::other("解壓 curl 校驗失敗"));
        }
        Ok(Self { exe, root })
    }

    pub fn spawn(&self, spec: &mut CurlCommandSpec, stdout: Stdio) -> io::Result<Child> {
        let mut command = Command::new(&self.exe);
        command
            .args(&spec.args)
            .stdin(if spec.stdin_config.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(stdout)
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW);
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
}
