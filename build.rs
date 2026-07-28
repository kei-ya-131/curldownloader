fn main() {
    println!("cargo:rerun-if-changed=assets/curl-downloader.ico");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=app.rc");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/curl-downloader.ico");
        resource
            .compile()
            .expect("failed to embed Curl Downloader icon");
    }

    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => {
            let manifest = std::fs::canonicalize("app.manifest").expect("app.manifest must exist");
            println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
            println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
        }
        Ok("gnu") => {
            let out_dir = std::path::PathBuf::from(
                std::env::var_os("OUT_DIR").expect("OUT_DIR must be available"),
            );
            let resource = out_dir.join("app-manifest.o");
            let status = std::process::Command::new("windres")
                .args(["--input-format=rc", "--output-format=coff", "app.rc", "-o"])
                .arg(&resource)
                .status()
                .expect("GNU release needs windres from MinGW");
            assert!(status.success(), "windres failed with status {status}");
            println!("cargo:rustc-link-arg={}", resource.display());
        }
        _ => {}
    }
}
