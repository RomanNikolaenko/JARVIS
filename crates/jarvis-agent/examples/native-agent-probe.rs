//! A bounded, read-only native-tool smoke test against a local model server.
//! See docs/NATIVE-TOOLS-VALIDATION.md for usage and pass criteria.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use jarvis_agent::{
    AgentEnvironment, AgentError, AgentManager, AgentModel, AgentSettings, NewTask, StepKind,
    StepOutcome, TaskLimits, TaskStatus, TaskStore,
};
use jarvis_ai::{
    AiProvider, ChatMessage, ChatRequest, DataLocality, GenerationOptions, HttpProviderFactory,
    ProviderBackend, ProviderConfig, ProviderFactory, ProviderId, ToolChatRequest,
    ToolChatResponse, ToolDefinition, ToolMessage,
};
use jarvis_security::{
    ApprovalBroker, AuditEvent, AuditLog, FileScope, PolicyConfig, SecurityPolicy,
};
use jarvis_storage::Database;
use jarvis_tools::{ProcessRegistry, ToolLimits, ToolRegistry};
use uuid::Uuid;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const TASK_SECONDS: u32 = 180;

struct ProbeModel {
    provider: Arc<dyn AiProvider>,
    model: String,
    native_turns: AtomicUsize,
}

fn options() -> GenerationOptions {
    GenerationOptions {
        temperature: Some(0.0),
        max_output_tokens: Some(2048),
    }
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T, jarvis_ai::AiError>>,
) -> jarvis_agent::Result<T> {
    tokio::time::timeout(REQUEST_TIMEOUT, future)
        .await
        .map_err(|_| AgentError::Model("probe request timed out".into()))?
        .map_err(|error| AgentError::Model(error.to_string()))
}

#[async_trait]
impl AgentModel for ProbeModel {
    fn provider_id(&self) -> Option<String> {
        Some(self.provider.descriptor().id.to_string())
    }
    fn model(&self) -> String {
        self.model.clone()
    }
    fn context_chars(&self) -> usize {
        12_000
    }
    fn supports_native_tools(&self) -> bool {
        true
    }
    async fn complete(&self, messages: Vec<ChatMessage>) -> jarvis_agent::Result<String> {
        bounded(self.provider.chat(ChatRequest {
            model: self.model.clone(),
            messages,
            options: options(),
        }))
        .await
        .map(|response| response.content)
    }
    async fn complete_tools(
        &self,
        messages: Vec<ToolMessage>,
        tools: Vec<ToolDefinition>,
    ) -> jarvis_agent::Result<ToolChatResponse> {
        self.native_turns.fetch_add(1, Ordering::Relaxed);
        bounded(self.provider.chat_with_tools(ToolChatRequest {
            model: self.model.clone(),
            messages,
            tools,
            options: options(),
        }))
        .await
    }
}

fn configuration(args: &[String]) -> Result<(ProviderConfig, String), String> {
    let [backend, base_url, model] = args else {
        return Err(
            "usage: native-agent-probe <ollama|openai-compatible> <base-url> <model-id>".into(),
        );
    };
    let backend = match backend.as_str() {
        "ollama" => ProviderBackend::Ollama,
        "openai-compatible" => ProviderBackend::OpenAiCompatible,
        _ => return Err("supported backends: ollama, openai-compatible".into()),
    };
    if model.trim().is_empty() {
        return Err("a model ID is required".into());
    }
    let config = ProviderConfig {
        id: ProviderId::new("native-probe"),
        backend,
        display_name: "Native agent probe".into(),
        base_url: base_url.clone(),
        enabled: true,
        native_tool_models: if backend == ProviderBackend::OpenAiCompatible {
            vec![model.clone()]
        } else {
            vec![]
        },
    };
    config.validate()?;
    if config.locality()? != DataLocality::OnDevice {
        return Err("this probe only accepts an on-device server URL".into());
    }
    Ok((config, model.clone()))
}

async fn probe(provider: Arc<dyn AiProvider>, model: String) -> Result<String, String> {
    bounded(provider.health_check())
        .await
        .map_err(|e| e.to_string())?;
    let capabilities = bounded(provider.capabilities(&model))
        .await
        .map_err(|e| e.to_string())?;
    if !provider.supports_native_tools() || !capabilities.chat || !capabilities.tool_calling {
        return Err("the selected model/provider does not report native tool support".into());
    }
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let marker = format!("JARVIS_PROBE_{}", Uuid::new_v4().simple());
    std::fs::write(directory.path().join("probe.txt"), &marker).map_err(|e| e.to_string())?;
    let database = Database::open_in_memory().map_err(|e| e.to_string())?;
    let audit = AuditLog::new(database.clone());
    let policy = SecurityPolicy::new(PolicyConfig {
        enabled_tools: ["read_file".to_owned()].into_iter().collect(),
        scope: FileScope::new([directory.path().to_path_buf()], []),
        allowed_programs: Default::default(),
        allow_network: false,
        auto_approve_low: true,
        auto_approve_medium: false,
    });
    let manager = AgentManager::new(AgentEnvironment {
        store: TaskStore::new(database),
        tools: Arc::new(ToolRegistry::builtin()),
        approvals: Arc::new(ApprovalBroker::default()),
        audit: Some(audit.clone()),
        processes: Arc::new(ProcessRegistry::default()),
        settings: Arc::new(move || AgentSettings {
            policy: policy.clone(),
            tool_limits: ToolLimits::default(),
            approval_timeout: Duration::from_secs(1),
        }),
        max_concurrent: 1,
    });
    let model = Arc::new(ProbeModel {
        provider,
        model,
        native_turns: AtomicUsize::new(0),
    });
    let task = manager.start(NewTask {
        goal: "Read probe.txt using the read_file tool. Return the exact marker stored in that file. Do not guess it.".into(),
        limits: TaskLimits { max_steps: 4, max_duration_secs: TASK_SECONDS },
    }, model.clone()).await.map_err(|e| e.to_string())?;
    let finished = tokio::time::timeout(Duration::from_secs(u64::from(TASK_SECONDS) + 10), async {
        loop {
            let current = manager.get(task.id).await.map_err(|e| e.to_string())?;
            if current.status.is_finished() {
                return Ok::<_, String>(current);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    // Wait for owned work to stop before the temporary workspace disappears.
    manager.shutdown().await;
    let finished = finished.map_err(|_| "probe task timed out".to_owned())??;
    let calls = model.native_turns.load(Ordering::Relaxed);
    let read = finished.steps.iter().any(|step| {
        step.kind == StepKind::Action
            && step.tool.as_deref() == Some("read_file")
            && step.outcome == StepOutcome::Succeeded
            && step
                .output
                .as_deref()
                .is_some_and(|output| output.contains(&marker))
    });
    let entries = audit.list(100, None).await.map_err(|e| e.to_string())?;
    let audited = entries
        .iter()
        .any(|entry| entry.tool == "read_file" && entry.event == AuditEvent::Succeeded);
    let intact = audit.verify().await.map_err(|e| e.to_string())?.intact;
    if finished.status != TaskStatus::Completed
        || calls < 2
        || !read
        || !audited
        || !intact
        || !finished
            .result
            .as_deref()
            .is_some_and(|answer| answer.contains(&marker))
    {
        return Err(format!(
            "native probe failed: status={:?}, native_turns={calls}, journal_read={read}, audited={audited}, audit_intact={intact}, error={:?}",
            finished.status, finished.error
        ));
    }
    Ok(format!(
        "PASS: {calls} native turns; fixture read, correlated result, final marker and audit verified"
    ))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let (config, model) = match configuration(&args) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let provider = match HttpProviderFactory::new().create(&config, None) {
        Ok(provider) => provider,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    match probe(provider, model).await {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("FAIL: {error}");
            ExitCode::FAILURE
        }
    }
}

// Reuse the provider contract server so both probe protocols are tested
// through HTTP, decoding, orchestration and real scoped file access.
#[cfg(test)]
#[path = "../../jarvis-ai/tests/support/mod.rs"]
mod support;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]
    use super::*;
    use serde_json::{Value, json};
    use support::{MockServer, Reply, Request};

    fn reply(request: &Request, ollama: bool, skip_read: bool) -> Reply {
        match request.path.as_str() {
            "/api/version" => return Reply::Json(200, json!({"version":"test"})),
            "/api/show" => return Reply::Json(200, json!({"capabilities":["completion","tools"]})),
            "/v1/models" => return Reply::Json(200, json!({"data":[{"id":"probe-model"}]})),
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
                r#"{"plan":["Read the file","Answer"]}"#
            } else {
                r#"{"ok":true}"#
            };
            let (kind, wire) = if ollama {
                (
                    "application/x-ndjson",
                    format!(
                        "{}\n{}\n",
                        json!({"message":{"content":content},"done":false}),
                        json!({"done":true})
                    ),
                )
            } else {
                (
                    "text/event-stream",
                    format!(
                        "data: {}\n\ndata: [DONE]\n\n",
                        json!({"choices":[{"delta":{"content":content},"finish_reason":"stop"}]})
                    ),
                )
            };
            return Reply::stream(kind, &wire, 31);
        }
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        let tool = &body["tools"][0]["function"];
        assert!(
            tool["description"]
                .as_str()
                .unwrap()
                .starts_with("read_file:")
        );
        let result = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "tool");
        let message: Value = if skip_read {
            json!({"role":"assistant","content":"I guessed the file contents"})
        } else if let Some(result) = result {
            json!({"role":"assistant","content":result["content"]})
        } else if ollama {
            json!({"role":"assistant","content":"","tool_calls":[{"function":{"name":tool["name"],"arguments":{"path":"probe.txt"}}}]})
        } else {
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"live-call-1","type":"function","function":{"name":tool["name"],"arguments":"{\"path\":\"probe.txt\"}"}}]})
        };
        Reply::Json(
            200,
            if ollama {
                json!({"done":true,"done_reason":"stop","message":message})
            } else {
                json!({"choices":[{"finish_reason":if message.get("tool_calls").is_some() { "tool_calls" } else { "stop" },"message":message}]})
            },
        )
    }

    async fn run_server(ollama: bool, skip_read: bool) -> Result<String, String> {
        let server = MockServer::start(move |request| reply(request, ollama, skip_read)).await;
        let backend = if ollama {
            "ollama"
        } else {
            "openai-compatible"
        };
        let (config, model) = configuration(&[
            backend.into(),
            server.url(if ollama { "" } else { "/v1" }),
            "probe-model".into(),
        ])
        .unwrap();
        let provider = HttpProviderFactory::new().create(&config, None).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(10), probe(provider, model))
            .await
            .expect("bounded probe");
        assert!(server.requests().iter().all(|request| {
            !request.body.contains("JARVIS_PROBE_")
                || request.json()["messages"]
                    .as_array()
                    .is_some_and(|messages| {
                        messages.iter().any(|message| message["role"] == "tool")
                            || messages[0]["content"]
                                .as_str()
                                .is_some_and(|s| s.starts_with("You check the work"))
                    })
        }));
        result
    }

    #[tokio::test]
    async fn native_probe_exercises_both_http_protocols_and_real_tool_execution() {
        for ollama in [true, false] {
            assert!(
                run_server(ollama, false)
                    .await
                    .expect("probe succeeds")
                    .starts_with("PASS:")
            );
        }
    }

    #[tokio::test]
    async fn a_completed_task_without_a_native_read_cannot_pass() {
        for ollama in [true, false] {
            let error = run_server(ollama, true)
                .await
                .expect_err("must actually read");
            assert!(error.contains("journal_read=false"));
        }
    }

    #[test]
    fn probe_rejects_remote_endpoints_credentials_and_invalid_arguments() {
        for url in [
            "https://example.com/v1",
            "http://192.168.1.2:1234/v1",
            "http://user:secret@localhost/v1",
        ] {
            assert!(
                configuration(&["openai-compatible".into(), url.into(), "model".into()]).is_err()
            );
        }
        assert!(configuration(&[]).is_err());
        assert!(
            configuration(&["unknown".into(), "http://localhost".into(), "model".into()]).is_err()
        );
        assert!(
            configuration(&[
                "ollama".into(),
                "http://localhost:11434".into(),
                "model".into()
            ])
            .is_ok()
        );
    }
}
