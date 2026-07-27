use serde::Serialize;
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

pub const HOST_NAME: &str = "curl_downloader";
pub const EXTENSION_ID: &str = "curl-downloader@kinkeil.local";
const REGISTRY_KEY: &str = r"Software\Mozilla\NativeMessagingHosts\curl_downloader";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct NativeHostRegistration {
    pub manifest_path: PathBuf,
    pub executable_path: PathBuf,
}

#[derive(Serialize)]
struct NativeHostManifest<'a> {
    name: &'static str,
    description: &'static str,
    path: &'a str,
    #[serde(rename = "type")]
    manifest_type: &'static str,
    allowed_extensions: [&'static str; 1],
}

pub trait RegistryWriter {
    fn set_manifest_path(&mut self, manifest_path: &Path) -> io::Result<()>;
}

pub fn manifest_path_for(app_data: &Path) -> PathBuf {
    app_data
        .join("CurlDownloader")
        .join("firefox-native-host")
        .join(format!("{HOST_NAME}.json"))
}

pub fn render_manifest(executable: &Path) -> Result<String, serde_json::Error> {
    let executable = executable.to_string_lossy();
    serde_json::to_string_pretty(&NativeHostManifest {
        name: HOST_NAME,
        description: "Curl Downloader Firefox Native Messaging host",
        path: &executable,
        manifest_type: "stdio",
        allowed_extensions: [EXTENSION_ID],
    })
}

pub fn register_with<R: RegistryWriter>(
    executable: &Path,
    app_data: &Path,
    registry: &mut R,
) -> io::Result<NativeHostRegistration> {
    let manifest_path = manifest_path_for(app_data);
    let support_directory = manifest_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Native host manifest path 無效",
        )
    })?;
    fs::create_dir_all(support_directory)?;
    let manifest = render_manifest(executable)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    fs::write(&manifest_path, manifest)?;
    registry.set_manifest_path(&manifest_path)?;
    Ok(NativeHostRegistration {
        manifest_path,
        executable_path: executable.to_path_buf(),
    })
}

pub fn ensure_registered(executable: &Path) -> io::Result<NativeHostRegistration> {
    #[cfg(windows)]
    {
        let app_data = env::var_os("APPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "找不到 APPDATA"))?;
        let mut registry = CurrentUserRegistry;
        register_with(executable, &app_data, &mut registry)
    }

    #[cfg(not(windows))]
    {
        let _ = env::var_os("APPDATA");
        Ok(NativeHostRegistration {
            manifest_path: PathBuf::new(),
            executable_path: executable.to_path_buf(),
        })
    }
}

#[cfg(windows)]
struct CurrentUserRegistry;

#[cfg(windows)]
impl RegistryWriter for CurrentUserRegistry {
    fn set_manifest_path(&mut self, manifest_path: &Path) -> io::Result<()> {
        use winreg::{RegKey, enums::HKEY_CURRENT_USER};

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .create_subkey(REGISTRY_KEY)
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))?;
        let value = manifest_path.to_string_lossy().into_owned();
        key.set_value("", &value)
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))
    }
}
