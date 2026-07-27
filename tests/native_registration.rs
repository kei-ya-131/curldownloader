use std::path::{Path, PathBuf};

use curl_downloader::native_registration::{
    EXTENSION_ID, HOST_NAME, manifest_path_for, render_manifest,
};

#[test]
fn manifest_path_uses_per_user_firefox_host_directory() {
    let app_data = PathBuf::from(r"C:\Users\tester\AppData\Roaming");
    assert_eq!(
        manifest_path_for(&app_data),
        PathBuf::from(
            r"C:\Users\tester\AppData\Roaming\CurlDownloader\firefox-native-host\curl_downloader.json"
        )
    );
}

#[test]
fn rendered_manifest_contains_current_executable_and_fixed_identity() {
    let executable = Path::new(r"C:\Tools\CurlDownloader\CurlDownloader.exe");
    let value: serde_json::Value =
        serde_json::from_str(&render_manifest(executable).unwrap()).unwrap();
    assert_eq!(value["name"], HOST_NAME);
    assert_eq!(value["path"], executable.to_string_lossy().as_ref());
    assert_eq!(value["type"], "stdio");
    assert_eq!(value["allowed_extensions"][0], EXTENSION_ID);
}
