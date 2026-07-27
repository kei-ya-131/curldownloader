use std::{
    io,
    path::{Path, PathBuf},
};

use curl_downloader::native_registration::{
    EXTENSION_ID, HOST_NAME, RegistryWriter, manifest_path_for, register_with, render_manifest,
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
struct FakeRegistry {
    path: Option<PathBuf>,
}

impl RegistryWriter for FakeRegistry {
    fn set_manifest_path(&mut self, manifest_path: &Path) -> io::Result<()> {
        self.path = Some(manifest_path.to_path_buf());
        Ok(())
    }
}

#[test]
fn register_with_writes_manifest_and_registry_target() {
    let root = std::env::temp_dir().join(format!(
        "curl-downloader-native-registration-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let executable = root.join("CurlDownloader.exe");
    let app_data = root.join("AppData");
    let mut registry = FakeRegistry { path: None };
    let result = register_with(&executable, &app_data, &mut registry).unwrap();
    assert_eq!(registry.path, Some(result.manifest_path.clone()));
    assert_eq!(result.executable_path, executable);
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&result.manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["path"], executable.to_string_lossy().as_ref());
    std::fs::remove_dir_all(root).unwrap();
}
