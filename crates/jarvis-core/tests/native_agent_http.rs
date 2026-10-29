//! Actual HTTP adapters and core settings/routing, with scripted local model
//! servers. These are integration contracts, not evidence of live-model quality.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../../jarvis-ai/tests/support/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use jarvis_ai::{ProviderBackend, ProviderConfig, ProviderId};
use jarvis_core::jarvis_agent::{AgentTask, StepKind, StepOutcome, TaskStatus};
use jarvis_core::jarvis_security::AuditEvent;
use jarvis_core::{AgentStartRequest, AppPaths, CoreOptions, JarvisCore};
use serde_json::{Value, json};
use support::{MockServer, Reply, Request};

#[derive(Clone, Copy)]
enum Scenario {
    Write,
    ResumeWrite,
    Hang,
    InvalidCalls,
    Unverified,
}

fn respond(request: &Request, ollama: bool, scenario: Scenario) -> Reply {
    match request.path.as_str() {
        "/api/version" => return Reply::Json(200, json!({"version":"contract-test"})),
        "/api/tags" => return Reply::Json(200, json!({"models":[{"name":"tool-model"}]})),
        "/api/show" => return Reply::Json(200, json!({"capabilities":["completion","tools"]})),
        "/v1/models" => return Reply::Json(200, json!({"data":[{"id":"tool-model"}]})),
        _ => {}
    }
    let body = request.json();
    assert_eq!(
        request.path,
        if ollama {
            "/api/chat"
        } else {
            "/v1/chat/completions"
        }
    );
    if body["stream"] == true {
        let system = body["messages"][0]["content"].as_str().unwrap();
        let content = if system.starts_with("You plan tasks") {
            r#"{"plan":["Write the requested file","Report the outcome"]}"#
        } else {
            assert!(
                system.starts_with("You check the work"),
                "native execution must not silently fall back to text"
            );
            if matches!(scenario, Scenario::Unverified) {
                return Reply::Json(400, json!({"error":{"message":"verifier unavailable"}}));
            }
            r#"{"ok":true}"#
        };
        return if ollama {
            Reply::stream(
                "application/x-ndjson",
                &format!(
                    "{}\n{}\n",
                    json!({"message":{"content":content},"done":false}),
                    json!({"done":true})
                ),
                19,
            )
        } else {
            Reply::stream(
                "text/event-stream",
                &format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"delta":{"content":content},"finish_reason":"stop"}]})
                ),
                19,
            )
        };
    }
    assert_eq!(body["stream"], false);
    let tools = body["tools"]
        .as_array()
        .expect("native tools sent by the core");
    let tool = tools
        .iter()
        .find(|tool| {
            tool["function"]["description"]
                .as_str()
                .is_some_and(|s| s.starts_with("write_file:"))
        })
        .expect("write_file advertised");
    if matches!(scenario, Scenario::Hang) {
        return Reply::Stream {
            content_type: "application/json",
            chunks: vec![],
            hang: true,
        };
    }
    let observation = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "tool");
    // On a resumed task the model deliberately proposes the interrupted
    // action again. The core must obtain a fresh approval before executing it.
    let observation = observation.filter(|message| {
        !(matches!(scenario, Scenario::ResumeWrite)
            && message["content"]
                .as_str()
                .is_some_and(|text| text.contains("Not run:")))
    });
    let message: Value = if let Some(observation) = observation {
        let content = observation["content"].as_str().unwrap();
        json!({"role":"assistant","content":if content.contains("status=\"denied\"") { "Write was denied" } else { "Write succeeded" }})
    } else {
        let name = if matches!(scenario, Scenario::InvalidCalls) {
            json!("not_advertised")
        } else {
            tool["function"]["name"].clone()
        };
        let input = json!({"path":"result.txt","content":"written once"});
        if ollama {
            json!({"role":"assistant","content":"","tool_calls":[{"function":{"name":name,"arguments":input}}]})
        } else {
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"wire-call-1","type":"function","function":{"name":name,"arguments":input.to_string()}}]})
        }
    };
    Reply::Json(
        200,
        if ollama {
            json!({"done":true,"message":message})
        } else {
            json!({"choices":[{"message":message,"finish_reason":"stop"}]})
        },
    )
}

struct Harness {
    core: Arc<JarvisCore>,
    paths: AppPaths,
    server: MockServer,
    workspace: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new(ollama: bool, scenario: Scenario) -> Self {
        let server = MockServer::start(move |request| respond(request, ollama, scenario)).await;
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
            log_dir: dir.path().join("logs"),
        };
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let core = JarvisCore::initialize(CoreOptions::new(paths.clone(), "test"))
            .await
            .unwrap();
        let mut settings = core.settings();
        settings.ai.providers = vec![ProviderConfig {
            id: ProviderId::new("native-contract"),
            backend: if ollama {
                ProviderBackend::Ollama
            } else {
                ProviderBackend::OpenAiCompatible
            },
            display_name: "Native contract".into(),
            base_url: server.url(if ollama { "" } else { "/v1" }),
            enabled: true,
            native_tool_models: if ollama {
                vec![]
            } else {
                vec!["tool-model".into()]
            },
        }];
        settings.tools.workspace_folders = vec![workspace.display().to_string()];
        settings.tools.enabled = vec!["write_file".into()];
        core.update_settings(settings).await.unwrap();
        Self {
            core,
            paths,
            server,
            workspace,
            _dir: dir,
        }
    }

    async fn start(&self) -> uuid::Uuid {
        self.core
            .start_agent_task(AgentStartRequest {
                goal: "Write result.txt".into(),
                provider_id: Some("native-contract".into()),
                model: Some("tool-model".into()),
                max_steps: Some(6),
                max_duration_secs: Some(30),
            })
            .await
            .unwrap()
            .id
    }

    async fn approval(&self, task_id: uuid::Uuid) -> uuid::Uuid {
        // The task status is checkpointed before the broker publishes the
        // request. Wait for the request itself, as the desktop dialog does.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(request) = self
                    .core
                    .pending_approvals()
                    .iter()
                    .find(|p| p.task_id == Some(task_id))
                {
                    return request.id;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("approval request published")
    }

    async fn wait(&self, id: uuid::Uuid, predicate: impl Fn(&AgentTask) -> bool) -> AgentTask {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let task = self.core.agent_task(id).await.unwrap();
                if predicate(&task) {
                    return task;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("task reaches expected state")
    }
}

#[tokio::test]
async fn native_http_write_obeys_approval_and_audits_the_actual_outcome() {
    for ollama in [true, false] {
        for approved in [true, false] {
            let h = Harness::new(ollama, Scenario::Write).await;
            let id = h.start().await;
            h.wait(id, |task| task.status == TaskStatus::WaitingForApproval)
                .await;
            assert!(!h.workspace.join("result.txt").exists());
            let approval_id = h.approval(id).await;
            assert!(h.core.resolve_approval(approval_id, approved));
            assert!(!h.core.resolve_approval(approval_id, true));
            let task = h.wait(id, |task| task.status.is_finished()).await;
            assert_eq!(task.status, TaskStatus::Completed, "{task:?}");
            let actions: Vec<_> = task
                .steps
                .iter()
                .filter(|s| s.kind == StepKind::Action)
                .collect();
            assert_eq!(actions.len(), 1);
            assert_eq!(
                actions[0].outcome,
                if approved {
                    StepOutcome::Succeeded
                } else {
                    StepOutcome::Denied
                }
            );
            assert_eq!(h.workspace.join("result.txt").exists(), approved);
            if approved {
                assert_eq!(
                    std::fs::read_to_string(h.workspace.join("result.txt")).unwrap(),
                    "written once"
                );
            }
            assert_eq!(
                task.result.as_deref(),
                Some(if approved {
                    "Write succeeded"
                } else {
                    "Write was denied"
                })
            );
            assert!(h.core.verify_audit_log().await.unwrap().intact);
            let audit = h.core.audit_log(20, None).await.unwrap();
            assert_eq!(audit.len(), if approved { 2 } else { 1 });
            assert!(audit.iter().any(|entry| entry.event
                == if approved {
                    AuditEvent::Succeeded
                } else {
                    AuditEvent::Denied
                }));
            assert!(
                audit
                    .iter()
                    .all(|entry| entry.task_id == Some(id) && entry.tool == "write_file")
            );
            assert!(h.core.pending_approvals().is_empty());
            h.core.shutdown().await;
        }
    }
}

#[tokio::test]
async fn restarting_during_native_approval_requires_a_new_decision_before_writing() {
    for ollama in [true, false] {
        for approved in [true, false] {
            let mut h = Harness::new(ollama, Scenario::ResumeWrite).await;
            let id = h.start().await;
            let old_approval = h.approval(id).await;
            h.core.shutdown().await;
            let paused = h.core.agent_task(id).await.unwrap();
            assert_eq!(paused.status, TaskStatus::Paused);
            assert_eq!(paused.steps.last().unwrap().outcome, StepOutcome::Denied);
            assert!(!h.core.resolve_approval(old_approval, true));
            assert!(!h.workspace.join("result.txt").exists());
            let before_resume = h
                .server
                .requests()
                .iter()
                .filter(|r| r.json()["tools"].is_array())
                .count();
            h.core = JarvisCore::initialize(CoreOptions::new(h.paths.clone(), "test"))
                .await
                .unwrap();
            assert_eq!(h.core.agent_task(id).await.unwrap(), paused);
            assert!(h.core.pending_approvals().is_empty());
            assert!(!h.core.resolve_approval(old_approval, true));
            assert_eq!(
                h.server
                    .requests()
                    .iter()
                    .filter(|r| r.json()["tools"].is_array())
                    .count(),
                before_resume
            );
            h.core.resume_agent_task(id).await.unwrap();
            let new_approval = h.approval(id).await;
            assert_ne!(new_approval, old_approval);
            assert!(!h.core.resolve_approval(old_approval, true));
            assert!(!h.workspace.join("result.txt").exists());
            assert!(h.core.resolve_approval(new_approval, approved));
            let done = h.wait(id, |task| task.status.is_finished()).await;
            assert_eq!(done.status, TaskStatus::Completed, "{done:#?}");
            assert_eq!(h.workspace.join("result.txt").exists(), approved);
            let actions: Vec<_> = done
                .steps
                .iter()
                .filter(|s| s.kind == StepKind::Action)
                .collect();
            assert_eq!(actions.len(), 2);
            assert_eq!(actions[0].outcome, StepOutcome::Denied);
            assert_eq!(
                actions[1].outcome,
                if approved {
                    StepOutcome::Succeeded
                } else {
                    StepOutcome::Denied
                }
            );
            let audit = h.core.audit_log(20, None).await.unwrap();
            assert_eq!(
                audit
                    .iter()
                    .filter(|e| e.event == AuditEvent::Succeeded)
                    .count(),
                usize::from(approved)
            );
            assert!(h.core.verify_audit_log().await.unwrap().intact);
            assert!(h.core.pending_approvals().is_empty());
            h.core.shutdown().await;
        }
    }
}

#[tokio::test]
async fn restarting_the_core_retains_native_results_without_replaying_tools() {
    for ollama in [true, false] {
        let h = Harness::new(ollama, Scenario::Write).await;
        let id = h.start().await;
        h.wait(id, |task| task.status == TaskStatus::WaitingForApproval)
            .await;
        assert!(h.core.resolve_approval(h.approval(id).await, true));
        let done = h.wait(id, |task| task.status.is_finished()).await;
        assert_eq!(done.status, TaskStatus::Completed);
        h.core.shutdown().await;
        let native_requests = h
            .server
            .requests()
            .iter()
            .filter(|r| r.json()["tools"].is_array())
            .count();
        let reopened = JarvisCore::initialize(CoreOptions::new(h.paths.clone(), "test"))
            .await
            .unwrap();
        assert_eq!(reopened.agent_task(id).await.unwrap(), done);
        assert_eq!(
            reopened.list_agent_tasks(10).await.unwrap()[0].status,
            TaskStatus::Completed
        );
        assert_eq!(
            reopened.settings().ai.providers,
            h.core.settings().ai.providers
        );
        assert!(reopened.verify_audit_log().await.unwrap().intact);
        assert!(reopened.pending_approvals().is_empty());
        assert_eq!(
            h.server
                .requests()
                .iter()
                .filter(|r| r.json()["tools"].is_array())
                .count(),
            native_requests
        );
        assert_eq!(
            std::fs::read_to_string(h.workspace.join("result.txt")).unwrap(),
            "written once"
        );
        reopened.shutdown().await;
    }
}

#[tokio::test]
async fn cancelling_a_native_http_request_closes_the_connection_and_runs_no_tool() {
    for ollama in [true, false] {
        let h = Harness::new(ollama, Scenario::Hang).await;
        let id = h.start().await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !h
                .server
                .requests()
                .iter()
                .any(|r| r.json()["tools"].is_array())
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let disconnected = h.server.disconnects();
        assert!(h.core.cancel_agent_task(id).await.unwrap());
        let task = h.wait(id, |task| task.status.is_finished()).await;
        assert_eq!(task.status, TaskStatus::Cancelled);
        tokio::time::timeout(Duration::from_secs(5), async {
            while h.server.disconnects() <= disconnected {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("HTTP request cancelled");
        assert!(!h.workspace.join("result.txt").exists());
        assert!(h.core.pending_approvals().is_empty());
        assert!(h.core.audit_log(20, None).await.unwrap().is_empty());
        h.core.shutdown().await;
    }
}

#[tokio::test]
async fn cancelling_native_approval_withdraws_it_and_rejects_a_late_allow() {
    for ollama in [true, false] {
        let h = Harness::new(ollama, Scenario::Write).await;
        let id = h.start().await;
        h.wait(id, |task| task.status == TaskStatus::WaitingForApproval)
            .await;
        let approval_id = h.approval(id).await;
        assert!(h.core.cancel_agent_task(id).await.unwrap());
        let task = h.wait(id, |task| task.status.is_finished()).await;
        assert_eq!(task.status, TaskStatus::Cancelled);
        assert!(h.core.pending_approvals().is_empty());
        assert!(!h.core.resolve_approval(approval_id, true));
        assert!(!h.workspace.join("result.txt").exists());
        assert_eq!(
            h.server
                .requests()
                .iter()
                .filter(|r| r.json()["tools"].is_array())
                .count(),
            1
        );
        assert!(h.core.verify_audit_log().await.unwrap().intact);
        h.core.shutdown().await;
    }
}

#[tokio::test]
async fn invalid_native_calls_fail_without_execution_or_text_fallback() {
    for ollama in [true, false] {
        let h = Harness::new(ollama, Scenario::InvalidCalls).await;
        let id = h.start().await;
        let task = h.wait(id, |task| task.status.is_finished()).await;
        assert_eq!(task.status, TaskStatus::Failed);
        assert!(!h.workspace.join("result.txt").exists());
        assert!(h.core.pending_approvals().is_empty());
        assert!(h.core.audit_log(20, None).await.unwrap().is_empty());
        assert_eq!(
            h.server
                .requests()
                .iter()
                .filter(|r| r.json()["tools"].is_array())
                .count(),
            3
        );
        h.core.shutdown().await;
    }
}

#[tokio::test]
async fn verifier_http_failure_preserves_effect_and_answer_as_unverified() {
    for ollama in [true, false] {
        let h = Harness::new(ollama, Scenario::Unverified).await;
        let id = h.start().await;
        h.wait(id, |task| task.status == TaskStatus::WaitingForApproval)
            .await;
        assert!(h.core.resolve_approval(h.approval(id).await, true));
        let task = h.wait(id, |task| task.status.is_finished()).await;
        assert_eq!(task.status, TaskStatus::Unverified);
        assert_eq!(task.steps.last().unwrap().outcome, StepOutcome::Unverified);
        assert_eq!(task.result.as_deref(), Some("Write succeeded"));
        assert_eq!(
            std::fs::read_to_string(h.workspace.join("result.txt")).unwrap(),
            "written once"
        );
        assert!(h.core.verify_audit_log().await.unwrap().intact);
        h.core.shutdown().await;
    }
}
