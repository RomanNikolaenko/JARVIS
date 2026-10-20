//! Agent tasks through the core: settings → policy, model routing and
//! privacy, approvals, the audit log, protected JARVIS folders, restarts.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use jarvis_ai::{
    AiError, AiProvider, ChatChunk, ChatRequest, ChatStream, DataLocality, FinishReason,
    ModelCapabilities, ModelInfo, ProviderDescriptor, ProviderId, ProviderKind,
};
use jarvis_core::jarvis_agent::{AgentTask, StepKind, StepOutcome, TaskStatus};
use jarvis_core::{AgentStartRequest, AppPaths, CoreOptions, ErrorCode, JarvisCore, Settings};
use uuid::Uuid;

/// Plays the agent: plans, then sends the scripted agent turns in order.
#[derive(Debug)]
struct AgentScript {
    descriptor: ProviderDescriptor,
    turns: Mutex<Vec<String>>,
    delay: Duration,
    requests: Mutex<Vec<ChatRequest>>,
}

impl AgentScript {
    fn new(locality: DataLocality, turns: &[&str]) -> Arc<Self> {
        Self::with_delay(locality, turns, Duration::ZERO)
    }

    fn with_delay(locality: DataLocality, turns: &[&str], delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            descriptor: ProviderDescriptor {
                id: ProviderId::new("script"),
                display_name: "Script".into(),
                kind: if locality == DataLocality::ThirdPartyCloud {
                    ProviderKind::Cloud
                } else {
                    ProviderKind::LocalServer
                },
                locality,
            },
            turns: Mutex::new(turns.iter().rev().map(|t| (*t).to_owned()).collect()),
            delay,
            requests: Mutex::default(),
        })
    }

    fn agent_requests(&self) -> Vec<ChatRequest> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| {
                r.messages[0]
                    .content
                    .starts_with("You are the JARVIS agent")
            })
            .cloned()
            .collect()
    }
}

#[async_trait]
impl AiProvider for AgentScript {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn health_check(&self) -> Result<(), AiError> {
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(vec![ModelInfo {
            id: "script-model".into(),
            display_name: "Script model".into(),
            provider_id: self.descriptor.id.clone(),
            context_window: Some(8192),
            capabilities: ModelCapabilities {
                chat: true,
                ..ModelCapabilities::default()
            },
        }])
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError> {
        let system = request.messages[0].content.clone();
        self.requests.lock().unwrap().push(request);
        let reply = if system.starts_with("You plan tasks") {
            r#"{"plan": ["Read the notes", "Write the summary"]}"#.to_owned()
        } else if system.starts_with("You check the work") {
            r#"{"ok": true}"#.to_owned()
        } else {
            self.turns
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| r#"{"final": "nothing left to do"}"#.to_owned())
        };
        let delay = self.delay;
        Ok(stream::once(async move {
            tokio::time::sleep(delay).await;
            Ok(ChatChunk::Delta(reply))
        })
        .chain(stream::once(async {
            Ok(ChatChunk::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            })
        }))
        .boxed())
    }
}

struct TestCore {
    core: Arc<JarvisCore>,
    paths: AppPaths,
    workspace: std::path::PathBuf,
    dir: tempfile::TempDir,
}

async fn initialize(paths: &AppPaths, provider: Arc<AgentScript>) -> Arc<JarvisCore> {
    JarvisCore::initialize(
        CoreOptions::new(paths.clone(), "9.9.9")
            .with_static_providers(vec![provider])
            .with_provider_factory(None),
    )
    .await
    .unwrap()
}

/// A core whose workspace folder holds `notes.txt`; `write_file` is on.
async fn start(provider: Arc<AgentScript>, configure: impl FnOnce(&mut Settings)) -> TestCore {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        log_dir: dir.path().join("logs"),
    };
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("notes.txt"), "Buy milk. Call Anna.").unwrap();
    let core = initialize(&paths, provider).await;
    let mut settings = core.settings();
    settings.tools.workspace_folders = vec![workspace.display().to_string()];
    settings.tools.enabled.push("write_file".to_owned());
    configure(&mut settings);
    core.update_settings(settings).await.unwrap();
    TestCore {
        core,
        paths,
        workspace,
        dir,
    }
}

fn goal(text: &str) -> AgentStartRequest {
    AgentStartRequest {
        goal: text.to_owned(),
        provider_id: None,
        model: None,
        max_steps: None,
        max_duration_secs: None,
    }
}

async fn wait_for(core: &JarvisCore, id: Uuid, done: impl Fn(&AgentTask) -> bool) -> AgentTask {
    for _ in 0..500 {
        let task = core.agent_task(id).await.unwrap();
        if done(&task) {
            return task;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "unexpected task state: {:#?}",
        core.agent_task(id).await.unwrap()
    );
}

const READ_NOTES: &str =
    r#"{"thought": "read first", "tool": "read_file", "input": {"path": "notes.txt"}}"#;
const WRITE_SUMMARY: &str =
    r#"{"tool": "write_file", "input": {"path": "summary.txt", "content": "2 tasks"}}"#;
const FINAL: &str = r#"{"final": "summary.txt lists 2 tasks"}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_task_reads_asks_before_writing_and_is_audited() {
    let provider = AgentScript::new(DataLocality::OnDevice, &[READ_NOTES, WRITE_SUMMARY, FINAL]);
    let t = start(Arc::clone(&provider), |_| {}).await;
    let status = t.core.agent_status();
    assert!(status.enabled && status.persistent);
    assert!(status.workspace_folders[0].available);
    assert!(
        t.core
            .list_tools()
            .iter()
            .any(|tool| tool.spec.name == "write_file" && tool.enabled)
    );

    let task = t
        .core
        .start_agent_task(goal("Summarize notes.txt into summary.txt"))
        .await
        .unwrap();
    assert_eq!(task.provider_id.as_deref(), Some("script"));
    assert_eq!(task.limits.max_steps, 12);

    let waiting = wait_for(&t.core, task.id, |t| {
        t.status == TaskStatus::WaitingForApproval
    })
    .await;
    assert_eq!(
        waiting
            .steps
            .iter()
            .filter(|s| s.kind == StepKind::Action)
            .count(),
        1
    );
    let pending = t.core.pending_approvals();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].tool, "write_file");
    assert!(!t.workspace.join("summary.txt").exists());
    assert!(t.core.resolve_approval(pending[0].id, true));
    assert!(
        !t.core.resolve_approval(pending[0].id, true),
        "decided once"
    );

    let done = wait_for(&t.core, task.id, |t| t.status.is_finished()).await;
    assert_eq!(done.status, TaskStatus::Completed, "{done:#?}");
    assert_eq!(done.result.as_deref(), Some("summary.txt lists 2 tasks"));
    assert_eq!(
        std::fs::read_to_string(t.workspace.join("summary.txt")).unwrap(),
        "2 tasks"
    );

    // The notes reached the model only as framed tool data.
    let requests = provider.agent_requests();
    let observation = &requests[1].messages.last().unwrap().content;
    assert!(observation.starts_with("<tool_result tool=\"read_file\" status=\"ok\">"));
    assert!(observation.contains("Buy milk"));
    assert_eq!(requests[0].options.temperature, Some(0.2));

    let audit = t.core.audit_log(50, None).await.unwrap();
    assert_eq!(audit.len(), 4, "{audit:#?}");
    assert!(t.core.verify_audit_log().await.unwrap().intact);
    let messages: Vec<String> = t
        .core
        .events()
        .recent(200)
        .into_iter()
        .map(|e| e.message)
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("Approval needed (medium risk)"))
    );
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("Agent task completed"))
    );

    let listed = t.core.list_agent_tasks(10).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(t.core.clear_finished_agent_tasks().await.unwrap(), 1);
    // Audit entries stay when tasks are deleted.
    assert_eq!(t.core.audit_log(50, None).await.unwrap().len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn cloud_models_need_their_own_permission_for_agents() {
    let provider = AgentScript::new(DataLocality::ThirdPartyCloud, &[FINAL]);
    let t = start(provider, |settings| {
        settings.privacy.allow_cloud_providers = true;
    })
    .await;
    let err = t.core.start_agent_task(goal("Anything")).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::PermissionDenied, "{err}");
    assert!(t.core.list_agent_tasks(10).await.unwrap().is_empty());

    let mut settings = t.core.settings();
    settings.privacy.allow_cloud_agents = true;
    t.core.update_settings(settings).await.unwrap();
    let task = t.core.start_agent_task(goal("Anything")).await.unwrap();
    let done = wait_for(&t.core, task.id, |t| t.status.is_finished()).await;
    assert_eq!(done.status, TaskStatus::Completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn jarvis_data_stays_protected_inside_a_workspace_folder() {
    let provider = AgentScript::new(
        DataLocality::OnDevice,
        &[
            r#"{"tool": "list_directory", "input": {"path": "data/memory"}}"#,
            r#"{"tool": "read_file", "input": {"path": "config/config.toml"}}"#,
            r#"{"final": "not allowed"}"#,
        ],
    );
    // The workspace is the folder that also holds JARVIS's data and settings.
    let t = start(provider, |_| {}).await;
    let mut settings = t.core.settings();
    settings.tools.workspace_folders = vec![t.dir.path().display().to_string()];
    t.core.update_settings(settings).await.unwrap();
    assert!(t.paths.data_dir.join("memory").join("jarvis.db").exists());

    let task = t
        .core
        .start_agent_task(goal("Read JARVIS's files"))
        .await
        .unwrap();
    let done = wait_for(&t.core, task.id, |t| t.status.is_finished()).await;
    let actions: Vec<_> = done
        .steps
        .iter()
        .filter(|s| s.kind == StepKind::Action)
        .collect();
    assert_eq!(actions.len(), 2);
    for action in actions {
        assert_eq!(action.outcome, StepOutcome::Denied, "{action:#?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_tool_settings_are_rejected() {
    let provider = AgentScript::new(DataLocality::OnDevice, &[]);
    let t = start(provider, |_| {}).await;
    let mut settings = t.core.settings();
    settings.tools.enabled.push("format_disk".to_owned());
    settings
        .tools
        .workspace_folders
        .push("relative/folder".to_owned());
    settings
        .tools
        .allowed_programs
        .push("cmd /c del".to_owned());
    settings.agents.max_steps = 0;
    let err = t.core.update_settings(settings).await.unwrap_err();
    let message = err.to_string();
    assert_eq!(err.code(), ErrorCode::Validation);
    for expected in [
        "format_disk",
        "relative/folder",
        "cmd /c del",
        "agents.maxSteps",
    ] {
        assert!(message.contains(expected), "{message}");
    }

    let mut settings = t.core.settings();
    settings.agents.enabled = false;
    t.core.update_settings(settings).await.unwrap();
    let err = t.core.start_agent_task(goal("Anything")).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::PermissionDenied);
}

#[tokio::test(flavor = "multi_thread")]
async fn running_tasks_pause_on_exit_and_resume_after_a_restart() {
    let slow = AgentScript::with_delay(
        DataLocality::OnDevice,
        &[READ_NOTES],
        Duration::from_secs(30),
    );
    let t = start(slow, |_| {}).await;
    let task = t
        .core
        .start_agent_task(goal("Read notes.txt"))
        .await
        .unwrap();
    wait_for(&t.core, task.id, |t| t.status == TaskStatus::Running).await;
    t.core.shutdown().await;
    let paused = t.core.agent_task(task.id).await.unwrap();
    assert_eq!(paused.status, TaskStatus::Paused);

    let fresh = AgentScript::new(DataLocality::OnDevice, &[READ_NOTES, FINAL]);
    let core = initialize(&t.paths, fresh).await;
    let restored = core.agent_task(task.id).await.unwrap();
    assert_eq!(restored.status, TaskStatus::Paused);
    core.resume_agent_task(task.id).await.unwrap();
    let done = wait_for(&core, task.id, |t| t.status.is_finished()).await;
    assert_eq!(done.status, TaskStatus::Completed, "{done:#?}");
    core.shutdown().await;
}
