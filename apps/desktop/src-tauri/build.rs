use std::path::PathBuf;

/// IPC commands exposed to the webview. Declaring them in the app manifest
/// makes Tauri generate `allow-<command>` permissions and deny every command
/// that is not granted by a capability (see `capabilities/`).
///
/// Keep in sync with `tauri::generate_handler!` in `src/lib.rs`; the IPC tests
/// fail if a registered command is not covered by the main window capability.
const COMMANDS: &[&str] = &[
    "get_app_info",
    "get_health",
    "get_platform_info",
    "get_system_metrics",
    "get_settings",
    "update_settings",
    "get_recent_events",
    "list_ai_models",
    "get_model_routes",
    "test_ai_provider",
    "start_chat",
    "cancel_chat",
    "report_ui_error",
];

fn main() {
    let mut attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));

    if embed_windows_manifest_via_linker() {
        // The manifest is embedded by the linker instead (see below), so the
        // resource compiler must not add a second one.
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    }

    if let Err(err) = tauri_build::try_build(attributes) {
        panic!("tauri-build failed: {err:#}");
    }
}

/// On Windows/MSVC, embeds the application manifest (Common Controls v6) with
/// linker flags so that it applies to *every* linked target, including test
/// binaries. Without it, test executables that link Tauri fail to start with
/// `STATUS_ENTRYPOINT_NOT_FOUND` (`TaskDialogIndirect` lives in comctl32 v6).
fn embed_windows_manifest_via_linker() -> bool {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return false;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_owned());
    let manifest = PathBuf::from(manifest_dir).join("windows-app-manifest.xml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    true
}
