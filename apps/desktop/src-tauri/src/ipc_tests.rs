//! IPC tests: commands are invoked through Tauri's real IPC pipeline (mock
//! runtime, real `tauri.conf.json` and capabilities), so argument naming,
//! serialization and the permission system are all exercised.

use jarvis_core::{AppPaths, CoreOptions, JarvisCore};
use serde_json::{Value, json};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder};
use tauri::webview::InvokeRequest;
use tauri::{App, WebviewWindow, WebviewWindowBuilder};

use crate::{AppState, MAIN_WINDOW, with_commands};

struct Harness {
    _app: App<MockRuntime>,
    main: WebviewWindow<MockRuntime>,
    other: WebviewWindow<MockRuntime>,
    dir: tempfile::TempDir,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = AppPaths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        log_dir: dir.path().join("logs"),
    };
    // No HTTP providers: tests must not depend on servers running locally.
    let options = CoreOptions::new(paths, "0.0.0-test").with_provider_factory(None);
    let core = tauri::async_runtime::block_on(JarvisCore::initialize(options)).expect("core");

    let app = with_commands(mock_builder())
        .manage(AppState::new(core, None))
        .build(tauri::generate_context!())
        .expect("app");
    let main = WebviewWindowBuilder::new(&app, MAIN_WINDOW, Default::default())
        .build()
        .expect("main window");
    // A window that no capability applies to.
    let other = WebviewWindowBuilder::new(&app, "untrusted", Default::default())
        .build()
        .expect("other window");

    Harness {
        _app: app,
        main,
        other,
        dir,
    }
}

fn invoke(window: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
    let url = if cfg!(windows) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    get_ipc_response(
        window,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: url.parse().expect("url"),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|body| body.deserialize::<Value>().expect("json response"))
}

/// Commands that must be callable from the main window without arguments.
const READ_ONLY_COMMANDS: &[&str] = &[
    "get_app_info",
    "get_health",
    "get_platform_info",
    "get_system_metrics",
    "get_settings",
    "get_recent_events",
    "list_ai_models",
    "get_model_routes",
    "get_runtime_status",
    "get_hardware_info",
    "list_curated_models",
    "list_conversations",
    "list_facts",
    "list_projects",
    "list_episodes",
    "get_memory_status",
    "get_agent_status",
    "list_tools",
    "list_agent_tasks",
    "list_pending_approvals",
    "get_audit_log",
    "verify_audit_log",
    "get_voice_status",
    "get_vision_status",
];

#[test]
fn read_only_commands_are_allowed_for_the_main_window() {
    let h = harness();
    for cmd in READ_ONLY_COMMANDS {
        let result = invoke(&h.main, cmd, json!({}));
        assert!(result.is_ok(), "{cmd} failed: {result:?}");
    }
}

#[test]
fn every_command_is_denied_for_windows_without_a_capability() {
    let h = harness();
    let all = READ_ONLY_COMMANDS.iter().chain(&[
        "update_settings",
        "test_ai_provider",
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
        "cancel_chat",
        "report_ui_error",
        "get_conversation",
        "create_conversation",
        "update_conversation",
        "delete_conversation",
        "save_fact",
        "delete_fact",
        "save_project",
        "delete_project",
        "clear_episodes",
        "search_memory",
        "export_memory",
        "import_memory",
        "clear_memory",
        "start_agent_task",
        "get_agent_task",
        "cancel_agent_task",
        "pause_agent_task",
        "resume_agent_task",
        "clear_finished_agent_tasks",
        "resolve_approval",
        "add_workspace_folder",
        "install_voice_package",
        "remove_voice_package",
        "cancel_voice_download",
        "start_listening",
        "stop_listening",
        "stop_speaking",
        "speak_text",
        "list_windows",
        "capture_screen",
        "capture_window",
        "capture_selection",
        "attach_image_file",
        "attach_photo",
        "discard_capture",
        "attach_projector",
    ]);
    for cmd in all {
        let result = invoke(&h.other, cmd, json!({}));
        let error = result.expect_err(cmd);
        assert!(
            error.to_string().contains("not allowed"),
            "{cmd}: unexpected error {error}"
        );
    }
}

#[test]
fn unknown_commands_are_rejected() {
    let h = harness();
    assert!(invoke(&h.main, "delete_everything", json!({})).is_err());
}

#[test]
fn health_report_lists_components() {
    let h = harness();
    let report = invoke(&h.main, "get_health", json!({})).expect("health");
    assert_eq!(report["version"], "0.0.0-test");
    let ids: Vec<&str> = report["components"]
        .as_array()
        .expect("components")
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    for expected in [
        "config", "storage", "events", "platform", "ai", "runtime", "memory",
    ] {
        assert!(ids.contains(&expected), "missing component {expected}");
    }
    // Only the diagnostic provider is registered.
    assert_eq!(report["status"], "degraded");
}

#[test]
fn settings_roundtrip_and_validation_errors() {
    let h = harness();
    let mut settings = invoke(&h.main, "get_settings", json!({})).expect("settings");
    assert_eq!(settings["general"]["assistantLanguage"], "uk");

    settings["general"]["assistantLanguage"] = json!("en");
    let saved =
        invoke(&h.main, "update_settings", json!({ "settings": settings })).expect("update");
    assert_eq!(saved["general"]["assistantLanguage"], "en");
    let file = std::fs::read_to_string(h.dir.path().join("config").join("config.toml"))
        .expect("config file");
    assert!(file.contains("assistantLanguage = \"en\""), "{file}");

    let mut invalid = saved.clone();
    invalid["ai"]["requestTimeoutSecs"] = json!(1);
    let error = invoke(&h.main, "update_settings", json!({ "settings": invalid }))
        .expect_err("validation error");
    assert_eq!(error["code"], "validation");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("requestTimeoutSecs")
    );
}

#[test]
fn malformed_arguments_are_rejected() {
    let h = harness();
    let result = invoke(
        &h.main,
        "update_settings",
        json!({ "settings": { "ai": { "requestTimeoutSecs": "soon" } } }),
    );
    assert!(result.is_err());

    let result = invoke(&h.main, "cancel_chat", json!({ "requestId": "not-a-uuid" }));
    assert!(result.is_err());
}

#[test]
fn recent_events_include_startup() {
    let h = harness();
    let events = invoke(&h.main, "get_recent_events", json!({ "limit": 50 })).expect("events");
    let messages: Vec<&str> = events
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|e| e["message"].as_str())
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("core ready")),
        "{messages:?}"
    );
}

fn turn(conversation_id: &Value, content: &str) -> Value {
    json!({
        "request": {
            "conversationId": conversation_id,
            "content": content,
            "providerId": null,
            "model": null,
            "temperature": null,
            "maxOutputTokens": null
        },
        "onEvent": "__CHANNEL__:1"
    })
}

#[test]
fn messages_are_sent_in_saved_conversations() {
    let h = harness();
    let conversation = invoke(
        &h.main,
        "create_conversation",
        json!({ "projectId": null, "temporary": false }),
    )
    .expect("create conversation");
    assert_eq!(conversation["temporary"], false);
    let request_id =
        invoke(&h.main, "send_message", turn(&conversation["id"], "hello")).expect("send");
    assert!(
        request_id.as_str().is_some_and(|id| id.len() == 36),
        "{request_id}"
    );
    let detail = invoke(
        &h.main,
        "get_conversation",
        json!({ "id": conversation["id"] }),
    )
    .expect("detail");
    assert_eq!(detail["messages"][0]["content"], "hello");
    assert_eq!(detail["conversation"]["title"], "hello");
    let listed = invoke(&h.main, "list_conversations", json!({ "query": "hello" })).expect("list");
    assert_eq!(listed.as_array().map(Vec::len), Some(1));

    let cancelled = invoke(
        &h.main,
        "cancel_chat",
        json!({ "requestId": "00000000-0000-4000-8000-000000000000" }),
    )
    .expect("cancel");
    assert_eq!(cancelled, json!(false));
}

#[test]
fn invalid_chat_requests_return_validation_errors() {
    let h = harness();
    let conversation = invoke(
        &h.main,
        "create_conversation",
        json!({ "projectId": null, "temporary": true }),
    )
    .expect("create");
    let error = invoke(&h.main, "send_message", turn(&conversation["id"], "   "))
        .expect_err("empty message");
    assert_eq!(error["code"], "validation");
    let error = invoke(
        &h.main,
        "send_message",
        turn(&json!("00000000-0000-4000-8000-000000000000"), "hi"),
    )
    .expect_err("unknown conversation");
    assert_eq!(error["code"], "notFound");
}

#[test]
fn facts_projects_and_search() {
    let h = harness();
    let project = invoke(
        &h.main,
        "save_project",
        json!({ "input": { "id": null, "name": "Atlas", "description": "", "instructions": "Be brief." } }),
    )
    .expect("project");
    let fact = invoke(
        &h.main,
        "save_fact",
        json!({ "input": {
            "id": null,
            "content": "The Atlas deadline is Friday.",
            "projectId": project["id"],
            "pinned": true,
            "sourceConversationId": null
        }}),
    )
    .expect("fact");
    assert_eq!(fact["source"], "user");
    let facts = invoke(&h.main, "list_facts", json!({})).expect("facts");
    assert_eq!(facts.as_array().map(Vec::len), Some(1));
    let hits = invoke(
        &h.main,
        "search_memory",
        json!({ "query": { "text": "deadline", "kinds": [], "limit": 10 } }),
    )
    .expect("search");
    assert_eq!(hits[0]["kind"], "fact");
    let status = invoke(&h.main, "get_memory_status", json!({})).expect("status");
    assert_eq!(status["available"], true);
    assert_eq!(status["counts"]["facts"], 1);
    assert_eq!(status["embedding"]["state"], "unavailable");

    let error = invoke(&h.main, "save_fact", json!({ "input": {
        "id": null, "content": "", "projectId": null, "pinned": false, "sourceConversationId": null
    }}))
    .expect_err("empty fact");
    assert_eq!(error["code"], "validation");

    invoke(&h.main, "delete_fact", json!({ "id": fact["id"] })).expect("delete fact");
    invoke(&h.main, "delete_project", json!({ "id": project["id"] })).expect("delete project");
    let error = invoke(&h.main, "delete_fact", json!({ "id": fact["id"] })).expect_err("gone");
    assert_eq!(error["code"], "notFound");
}

#[test]
fn history_cursor_roundtrips_equal_timestamps_over_ipc() {
    use jarvis_core::jarvis_memory::Episode;
    use tauri::Manager;
    use uuid::Uuid;
    let h = harness();
    let state = h._app.state::<AppState>();
    let episodes = (1..=3)
        .map(|id| Episode {
            id: Uuid::from_u128(id),
            occurred_at_ms: 10_000,
            category: "test".into(),
            level: "info".into(),
            summary: "history".into(),
        })
        .collect();
    tauri::async_runtime::block_on(state.core().memory().record_episodes(episodes))
        .expect("episodes");
    let first = invoke(
        &h.main,
        "list_episodes",
        json!({
            "limit": 2, "before": { "occurredAtMs": 10_001, "id": Uuid::nil() }
        }),
    )
    .expect("first page");
    assert_eq!(first.as_array().expect("array").len(), 2);
    assert_eq!(first[0]["id"], Uuid::from_u128(3).to_string());
    let second = invoke(
        &h.main,
        "list_episodes",
        json!({
            "limit": 2, "before": { "occurredAtMs": first[1]["occurredAtMs"], "id": first[1]["id"] }
        }),
    )
    .expect("next page");
    assert_eq!(second.as_array().expect("array").len(), 1);
    assert_eq!(second[0]["id"], Uuid::from_u128(1).to_string());
    assert!(
        invoke(
            &h.main,
            "list_episodes",
            json!({
                "before": { "occurredAtMs": 10_000, "id": "invalid" }
            })
        )
        .is_err()
    );
}

#[test]
fn clearing_memory_needs_the_confirmation_phrase() {
    let h = harness();
    invoke(&h.main, "save_fact", json!({ "input": {
        "id": null, "content": "keep me", "projectId": null, "pinned": false, "sourceConversationId": null
    }}))
    .expect("fact");
    let error = invoke(&h.main, "clear_memory", json!({ "confirmation": "yes" }))
        .expect_err("wrong phrase");
    assert_eq!(error["code"], "validation");
    let status = invoke(&h.main, "get_memory_status", json!({})).expect("status");
    assert_eq!(status["counts"]["facts"], 1, "nothing was deleted");

    invoke(
        &h.main,
        "clear_memory",
        json!({ "confirmation": crate::memory_commands::CLEAR_CONFIRMATION }),
    )
    .expect("clear");
    let status = invoke(&h.main, "get_memory_status", json!({})).expect("status");
    assert_eq!(status["counts"]["facts"], 0);
    let removed = invoke(&h.main, "clear_episodes", json!({})).expect("clear episodes");
    assert!(removed.as_u64().is_some());
}

#[test]
fn ui_errors_are_recorded_as_events() {
    let h = harness();
    invoke(
        &h.main,
        "report_ui_error",
        json!({ "report": { "message": "boom", "context": "test" } }),
    )
    .expect("report");
    let events = invoke(&h.main, "get_recent_events", json!({})).expect("events");
    assert!(
        events
            .as_array()
            .expect("array")
            .iter()
            .any(|e| e["category"] == "ui" && e["message"].as_str().unwrap_or("").contains("boom"))
    );
}

#[test]
fn model_routes_cover_every_category() {
    let h = harness();
    let routes = invoke(&h.main, "get_model_routes", json!({})).expect("routes");
    let routes = routes.as_array().expect("array");
    assert_eq!(routes.len(), 6);
    let general = routes
        .iter()
        .find(|r| r["category"] == "general")
        .expect("general route");
    // Only the diagnostic provider exists in tests.
    assert_eq!(general["providerId"], "diagnostic-echo");
    assert_eq!(general["automatic"], true);
}

#[test]
fn listing_models_accepts_refresh() {
    let h = harness();
    let listing = invoke(&h.main, "list_ai_models", json!({ "refresh": true })).expect("models");
    assert_eq!(listing[0]["provider"]["kind"], "diagnostic");
}

#[test]
fn provider_test_validates_input_before_connecting() {
    let h = harness();
    let error = invoke(
        &h.main,
        "test_ai_provider",
        json!({ "config": {
            "id": "remote",
            "backend": "openAiCompatible",
            "displayName": "Remote",
            "baseUrl": "http://user:secret@example.com/v1",
            "enabled": true
        }}),
    )
    .expect_err("credentials in URL are rejected");
    assert_eq!(error["code"], "validation");
}

#[test]
fn runtime_status_reports_an_empty_installation() {
    let h = harness();
    let status = invoke(&h.main, "get_runtime_status", json!({})).expect("status");
    assert_eq!(status["binary"], Value::Null);
    assert_eq!(status["server"]["phase"], "stopped");
    assert_eq!(status["embeddingServer"]["phase"], "stopped");
    assert_eq!(status["embeddingLogTail"], json!([]));
    assert_eq!(status["models"], json!([]));
    assert!(
        status["modelsDir"]
            .as_str()
            .is_some_and(|d| d.ends_with("models"))
    );

    let curated = invoke(&h.main, "list_curated_models", json!({})).expect("catalog");
    assert!(curated.as_array().is_some_and(|c| !c.is_empty()));
    assert!(
        curated
            .as_array()
            .is_some_and(|c| c.iter().any(|m| m["role"] == "embeddings")),
        "the catalog offers an embedding model"
    );
    let hardware = invoke(&h.main, "get_hardware_info", json!({})).expect("hardware");
    assert!(hardware["logicalCpus"].as_u64().is_some_and(|n| n >= 1));
}

#[test]
fn runtime_commands_report_errors_without_touching_the_network() {
    let h = harness();
    for role in ["chat", "embeddings"] {
        let error = invoke(
            &h.main,
            "start_local_model",
            json!({ "modelId": "missing", "role": role }),
        )
        .expect_err("not installed");
        assert_eq!(error["code"], "notFound", "{role}");
    }
    invoke(
        &h.main,
        "start_local_model",
        json!({ "modelId": "missing", "role": "reranking" }),
    )
    .expect_err("unknown roles are rejected");

    let error = invoke(
        &h.main,
        "remove_model",
        json!({ "id": "missing", "deleteFile": false }),
    )
    .expect_err("unknown model");
    assert_eq!(error["code"], "notFound");

    let error = invoke(
        &h.main,
        "download_model",
        json!({ "request": { "repo": "not a repo", "file": "x.bin", "revision": null } }),
    )
    .expect_err("invalid request");
    assert_eq!(error["code"], "validation");

    let error = invoke(&h.main, "download_curated_model", json!({ "id": "nope" }))
        .expect_err("unknown catalog entry");
    assert_eq!(error["code"], "notFound");

    assert_eq!(
        invoke(&h.main, "cancel_download", json!({ "id": "nope" })).expect("cancel"),
        json!(false)
    );
    for role in ["chat", "embeddings"] {
        invoke(&h.main, "stop_local_model", json!({ "role": role }))
            .expect("stopping a stopped server is fine");
    }
}

#[test]
fn agent_commands_report_state_and_validate_input() {
    let h = harness();
    let status = invoke(&h.main, "get_agent_status", json!({})).expect("status");
    assert_eq!(status["enabled"], true);
    assert_eq!(status["persistent"], true);
    assert_eq!(status["workspaceFolders"], json!([]));

    let tools = invoke(&h.main, "list_tools", json!({})).expect("tools");
    let enabled = |name: &str| {
        tools
            .as_array()
            .and_then(|t| t.iter().find(|tool| tool["spec"]["name"] == name))
            .map(|tool| tool["enabled"].clone())
    };
    assert_eq!(enabled("read_file"), Some(json!(true)));
    assert_eq!(enabled("delete_path"), Some(json!(false)));
    assert_eq!(enabled("run_command"), Some(json!(false)));

    let request = |goal: &str| {
        json!({ "request": {
            "goal": goal, "providerId": null, "model": null,
            "maxSteps": null, "maxDurationSecs": null
        } })
    };
    let error = invoke(&h.main, "start_agent_task", request("   ")).expect_err("empty goal");
    assert_eq!(error["code"], "validation");
    let missing = json!({ "id": "00000000-0000-0000-0000-000000000000" });
    let error = invoke(&h.main, "get_agent_task", missing.clone()).expect_err("unknown task");
    assert_eq!(error["code"], "notFound");
    assert_eq!(
        invoke(&h.main, "pause_agent_task", missing).expect("pause"),
        json!(false)
    );
    assert_eq!(
        invoke(
            &h.main,
            "resolve_approval",
            json!({ "id": "00000000-0000-0000-0000-000000000000", "approved": true })
        )
        .expect("resolve"),
        json!(false)
    );
    assert_eq!(
        invoke(&h.main, "list_pending_approvals", json!({})).expect("approvals"),
        json!([])
    );

    // The diagnostic echo model does not speak the agent protocol: the task
    // ends after three invalid replies instead of looping.
    let task = invoke(&h.main, "start_agent_task", request("Say hello")).expect("start");
    let id = json!({ "id": task["id"] });
    let mut finished = Value::Null;
    for _ in 0..300 {
        let current = invoke(&h.main, "get_agent_task", id.clone()).expect("task");
        let status = current["status"].as_str().unwrap_or_default();
        if ["completed", "failed", "cancelled"].contains(&status) {
            finished = current;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(finished["status"], "failed", "{finished}");
    assert_eq!(
        finished["error"],
        "The model did not follow the action format"
    );
    assert_eq!(finished["providerId"], "diagnostic-echo");

    let listed = invoke(&h.main, "list_agent_tasks", json!({ "limit": 10 })).expect("list");
    assert_eq!(listed.as_array().map(Vec::len), Some(1));
    let verification = invoke(&h.main, "verify_audit_log", json!({})).expect("verify");
    assert_eq!(verification["intact"], true);
    assert_eq!(
        invoke(&h.main, "get_audit_log", json!({})).expect("audit"),
        json!([])
    );
    assert_eq!(
        invoke(&h.main, "clear_finished_agent_tasks", json!({})).expect("clear"),
        json!(1)
    );
}

#[test]
fn voice_commands_validate_input_without_downloading() {
    let h = harness();
    let status = invoke(&h.main, "get_voice_status", json!({})).expect("status");
    assert_eq!(status["state"]["micOpen"], false);
    assert_eq!(status["canListen"], false);
    assert!(status["packages"].is_array());

    let error = invoke(
        &h.main,
        "install_voice_package",
        json!({ "id": "no-such-voice" }),
    )
    .expect_err("unknown package");
    assert_eq!(error["code"], "validation");
    let error = invoke(&h.main, "speak_text", json!({ "text": "" })).expect_err("empty text");
    assert_eq!(error["code"], "validation");
    // Nothing is installed, so the microphone must stay closed.
    let error = invoke(&h.main, "start_listening", json!({})).expect_err("not installed");
    assert!(
        ["notFound", "unsupported"].contains(&error["code"].as_str().unwrap_or_default()),
        "{error}"
    );
    assert_eq!(
        invoke(&h.main, "cancel_voice_download", json!({ "id": "nothing" })).expect("cancel"),
        json!(false)
    );
    invoke(&h.main, "stop_listening", json!({ "submit": false })).expect("stop");
    invoke(&h.main, "stop_speaking", json!({})).expect("stop");
    let status = invoke(&h.main, "get_voice_status", json!({})).expect("status");
    assert_eq!(status["state"]["micOpen"], false);
}

#[test]
fn vision_commands_validate_input_without_capturing() {
    let h = harness();
    let status = invoke(&h.main, "get_vision_status", json!({})).expect("status");
    assert!(status["capabilities"]["notes"].is_array());
    let error =
        invoke(&h.main, "attach_photo", json!({ "data": "not an image" })).expect_err("garbage");
    assert_eq!(error["code"], "validation");
    assert_eq!(
        invoke(&h.main, "discard_capture", json!({ "id": "nothing" })).expect("discard"),
        json!(false)
    );
    // A message cannot point at a capture that does not exist.
    let conversation = invoke(
        &h.main,
        "create_conversation",
        json!({ "projectId": null, "temporary": false }),
    )
    .expect("conversation");
    let mut request = turn(&conversation["id"], "What is this?");
    request["request"]["attachments"] = json!(["missing"]);
    let error = invoke(&h.main, "send_message", request).expect_err("missing capture");
    assert_eq!(error["code"], "notFound");
}
