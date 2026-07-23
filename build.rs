fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    if std::env::var_os("CARGO_CFG_TARGET_ENV").as_deref() == Some(std::ffi::OsStr::new("msvc")) {
        let manifest = std::fs::canonicalize("app.manifest").expect("app.manifest must exist");
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    }
}
