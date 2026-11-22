fn main() {
    println!("cargo:rerun-if-env-changed=MINARBOT_DESKTOP_RUNTIME_SHA256");
    println!("cargo:rerun-if-env-changed=MINARBOT_DESKTOP_FIXTURE_PORT");
    let attrs = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(&["connect_server"]));
    if let Err(error) = tauri_build::try_build(attrs) {
        eprintln!("tauri-build failed: {error:#}");
        std::process::exit(1);
    }
}
