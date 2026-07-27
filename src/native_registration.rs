use serde::Serialize;
use std::path::{Path, PathBuf};

pub const HOST_NAME: &str = "curl_downloader";
pub const EXTENSION_ID: &str = "curl-downloader@kinkeil.local";

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
