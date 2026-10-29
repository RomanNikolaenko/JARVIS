use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

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
    "set_provider_api_key",
    "has_provider_api_key",
    "delete_provider_api_key",
    "cancel_chat",
    "report_ui_error",
    "get_runtime_status",
    "get_hardware_info",
    "list_curated_models",
    "check_runtime_release",
    "install_runtime",
    "select_runtime_binary",
    "import_model",
    "download_curated_model",
    "download_model",
    "cancel_download",
    "remove_model",
    "start_local_model",
    "stop_local_model",
    "send_message",
    "list_conversations",
    "get_conversation",
    "create_conversation",
    "update_conversation",
    "delete_conversation",
    "list_facts",
    "save_fact",
    "delete_fact",
    "list_projects",
    "save_project",
    "delete_project",
    "list_episodes",
    "clear_episodes",
    "search_memory",
    "get_memory_status",
    "export_memory",
    "import_memory",
    "clear_memory",
    "get_agent_status",
    "list_tools",
    "start_agent_task",
    "list_agent_tasks",
    "get_agent_task",
    "cancel_agent_task",
    "pause_agent_task",
    "resume_agent_task",
    "clear_finished_agent_tasks",
    "list_pending_approvals",
    "resolve_approval",
    "get_audit_log",
    "verify_audit_log",
    "add_workspace_folder",
    "get_voice_status",
    "install_voice_package",
    "remove_voice_package",
    "cancel_voice_download",
    "start_listening",
    "stop_listening",
    "stop_speaking",
    "speak_text",
    "get_vision_status",
    "list_windows",
    "capture_screen",
    "capture_window",
    "capture_selection",
    "attach_image_file",
    "attach_photo",
    "discard_capture",
    "attach_projector",
    "list_plugins",
    "get_plugin_logs",
    "pick_plugin_folder",
    "review_mcp_server",
    "review_installed_plugin",
    "install_plugin",
    "set_plugin_enabled",
    "restart_plugin",
    "refresh_plugin_tools",
    "set_plugin_tool",
    "set_plugin_secret",
    "clear_plugin_secret",
    "remove_plugin",
    "get_update_status",
    "check_for_update",
    "install_update",
];

fn main() {
    if let Err(err) = validate_main_window_capability() {
        panic!("invalid Tauri capability: {err:#}");
    }

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

/// Ensures every registered IPC command is granted exactly once to the main
/// window and that the capability does not retain permissions for removed
/// commands. This turns capability drift into a build error.
fn validate_main_window_capability() -> Result<(), Box<dyn Error>> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_owned());
    let capability_path = Path::new(&manifest_dir).join("capabilities/main-window.json");
    println!("cargo:rerun-if-changed={}", capability_path.display());

    let raw = fs::read_to_string(&capability_path)?;
    let capability: serde_json::Value = serde_json::from_str(&raw)?;

    if capability
        .get("identifier")
        .and_then(serde_json::Value::as_str)
        != Some("main-window")
    {
        return Err(invalid_data("capability identifier must be `main-window`").into());
    }

    let windows = string_array(&capability, "windows")?;
    if windows.as_slice() != ["main"] {
        return Err(invalid_data(format!(
            "capability must apply only to the `main` window, found {windows:?}"
        ))
        .into());
    }

    let permissions = string_array(&capability, "permissions")?;
    let actual = permissions.iter().cloned().collect::<BTreeSet<_>>();
    if actual.len() != permissions.len() {
        return Err(invalid_data("capability contains duplicate permissions").into());
    }

    let mut expected = COMMANDS
        .iter()
        .map(|command| format!("allow-{}", command.replace('_', "-")))
        .collect::<BTreeSet<_>>();
    expected.insert("core:event:allow-listen".to_owned());
    expected.insert("core:event:allow-unlisten".to_owned());

    let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
    let unexpected = actual.difference(&expected).cloned().collect::<Vec<_>>();
    if !missing.is_empty() || !unexpected.is_empty() {
        return Err(invalid_data(format!(
            "permissions do not match registered commands; missing={missing:?}, unexpected={unexpected:?}"
        ))
        .into());
    }

    Ok(())
}

fn string_array(document: &serde_json::Value, field: &str) -> Result<Vec<String>, io::Error> {
    let values = document
        .get(field)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_data(format!("`{field}` must be an array")))?;

    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| invalid_data(format!("every `{field}` item must be a string")))
        })
        .collect()
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.into())
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
