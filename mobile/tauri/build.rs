fn main() {
    let attrs = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(&["connect_server"]));
    if let Err(error) = tauri_build::try_build(attrs) {
        eprintln!("tauri-build failed: {error:#}");
        std::process::exit(1);
    }
}
