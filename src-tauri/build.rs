fn main() {
    const COMMANDS: &[&str] = &["check_desktop_update", "install_desktop_update"];

    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));
    tauri_build::try_build(attributes).expect("failed to run tauri build script");
}
