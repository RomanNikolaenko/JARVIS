//! Agents in the core: the Security Policy Engine built from settings, the
//! model adapter (routing and privacy rules), task management and the
//! audit log.
//!
//! Agent tasks and the audit log live in the memory database. If it cannot
//! be opened they are kept in memory only, and the status says so.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jarvis_agent::{
    AgentEnvironment, AgentError, AgentManager, AgentModel, AgentTask, NewTask, SettingsSource,
    StepOutcome, TaskLimits, TaskStatus, TaskStore, TaskSummary,
};
use jarvis_ai::{
    AiProvider, ChatMessage, ChatRequest, DataLocality, GenerationOptions, ProviderId,
};
use jarvis_events::{EventBus, EventCategory};
use jarvis_memory::MemoryEngine;
use jarvis_security::{
    ApprovalBroker, AuditEntry, AuditLog, AuditVerification, FileScope, PendingApproval,
    PolicyConfig, SecurityPolicy,
};
use jarvis_storage::Database;
use jarvis_tools::{ProcessRegistry, ToolLimits, ToolRegistry, ToolSpec};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};
use ts_rs::TS;
use uuid::Uuid;

use crate::chat::ResolvedModel;
use crate::config::{ConfigStore, Settings};
use crate::error::CoreError;
use crate::{AppPaths, JarvisCore};

/// Tasks that may run at the same time.
const MAX_CONCURRENT_TASKS: usize = 2;
/// Output tokens of one agent turn (a JSON action or the final answer).
const AGENT_MAX_OUTPUT_TOKENS: u32 = 1024;
/// Context assumed when the provider does not report one.
const DEFAULT_CONTEXT_TOKENS: u32 = 4096;

/// A new agent task as the UI sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct AgentStartRequest {
    pub goal: String,
    /// Provider and model; without them the router chooses (the
    /// `toolCalling` choice if set, otherwise the general one).
    pub provider_id: Option<ProviderId>,
    pub model: Option<String>,
    /// Limits; unset uses the settings.
    pub max_steps: Option<u32>,
    pub max_duration_secs: Option<u32>,
}

/// A tool and whether it is turned on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct ToolInfo {
    pub spec: ToolSpec,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct WorkspaceFolder {
    pub path: String,
    /// The folder exists and can be used.
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct LaunchedProgram {
    pub pid: u32,
    pub program: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct AgentStatus {
    pub enabled: bool,
    /// False when tasks and the audit log cannot be saved (the memory
    /// database is unavailable); they are then lost on exit.
    pub persistent: bool,
    pub running_tasks: u32,
    pub workspace_folders: Vec<WorkspaceFolder>,
    /// Programs agents started that still run.
    pub launched_programs: Vec<LaunchedProgram>,
}

/// The agent subsystem owned by the core.
#[derive(Debug)]
pub(crate) struct AgentSystem {
    pub manager: Arc<AgentManager>,
    pub approvals: Arc<ApprovalBroker>,
    pub audit: AuditLog,
    pub tools: Arc<ToolRegistry>,
    pub processes: Arc<ProcessRegistry>,
    pub persistent: bool,
}

impl AgentSystem {
    pub(crate) async fn open(
        memory: &MemoryEngine,
        config: Arc<ConfigStore>,
        paths: &AppPaths,
        events: &EventBus,
    ) -> Result<Self, CoreError> {
        let (db, persistent) = match memory.database() {
            Some(db) => (db.clone(), true),
            None => {
                events.error(
                    EventCategory::Agents,
                    "Agent tasks and the audit log are kept in memory only: the database is unavailable",
                );
                let db = Database::open_in_memory().map_err(AgentError::from)?;
                (db, false)
            }
        };
        let tools = Arc::new(ToolRegistry::builtin());
        let approvals = Arc::new(ApprovalBroker::default());
        let audit = AuditLog::new(db.clone());
        let processes = Arc::new(ProcessRegistry::default());
        // JARVIS's own settings, data (databases, models) and logs are never
        // reachable through tools, even inside a workspace folder.
        let protected = vec![
            paths.config_dir.clone(),
            paths.data_dir.clone(),
            paths.log_dir.clone(),
        ];
        // Tools of running plugins are registered only while the user has
        // them turned on.
        let registry = Arc::clone(&tools);
        let settings: SettingsSource = Arc::new(move || {
            agent_settings(&config.current(), &protected, &registry.group_tool_names())
        });
        let manager = AgentManager::new(AgentEnvironment {
            store: TaskStore::new(db),
            tools: Arc::clone(&tools),
            approvals: Arc::clone(&approvals),
            audit: Some(audit.clone()),
            processes: Arc::clone(&processes),
            settings,
            max_concurrent: MAX_CONCURRENT_TASKS,
        });
        let interrupted = manager.recover().await?;
        if interrupted > 0 {
            events.warning(
                EventCategory::Agents,
                format!(
                    "{interrupted} agent task(s) were interrupted by the last exit and are paused"
                ),
            );
        }
        Ok(Self {
            manager,
            approvals,
            audit,
            tools,
            processes,
            persistent,
        })
    }
}

/// The policy and limits for the current settings.
pub(crate) fn agent_settings(
    settings: &Settings,
    protected: &[PathBuf],
    plugin_tools: &BTreeSet<String>,
) -> jarvis_agent::AgentSettings {
    let tools = &settings.tools;
    let scope = FileScope::new(
        tools.workspace_folders.iter().map(PathBuf::from),
        protected.iter().cloned(),
    );
    let allowed_programs = tools
        .allowed_programs
        .iter()
        .map(|program| {
            let name = program.trim().to_lowercase();
            [".exe", ".cmd", ".bat"]
                .iter()
                .find_map(|ext| name.strip_suffix(ext))
                .map_or_else(|| name.clone(), str::to_owned)
        })
        .collect();
    jarvis_agent::AgentSettings {
        policy: SecurityPolicy::new(PolicyConfig {
            enabled_tools: if settings.agents.enabled {
                tools.enabled.iter().chain(plugin_tools).cloned().collect()
            } else {
                BTreeSet::new()
            },
            scope,
            allowed_programs,
            allow_network: tools.allow_network,
            auto_approve_low: tools.auto_approve_read_only,
            auto_approve_medium: tools.auto_approve_changes,
        }),
        tool_limits: ToolLimits {
            command_timeout: Duration::from_secs(u64::from(tools.command_timeout_secs)),
            ..ToolLimits::default()
        },
        approval_timeout: Duration::from_secs(u64::from(settings.agents.approval_timeout_secs)),
    }
}

/// A chat model used by an agent.
#[derive(Debug)]
struct CoreAgentModel {
    config: Arc<ConfigStore>,
    provider: Arc<dyn AiProvider>,
    provider_id: String,
    model: String,
    context_chars: usize,
    timeout: Duration,
    native_tools: bool,
}

impl CoreAgentModel {
    /// The adapter can outlive registry replacement while a task is paused.
    /// Authorize its own destination, not the current provider sharing its ID.
    fn authorize_request(&self) -> jarvis_agent::Result<()> {
        let settings = self.config.current();
        if !settings.agents.enabled {
            return Err(AgentError::Model(
                "Agents are turned off in settings".into(),
            ));
        }
        if self.provider.descriptor().locality == DataLocality::ThirdPartyCloud {
            if !settings.privacy.allow_cloud_providers {
                return Err(AgentError::Model(
                    "Sending data to cloud providers is disabled in privacy settings".into(),
                ));
            }
            if !settings.privacy.allow_cloud_agents {
                return Err(AgentError::Model(
                    "Cloud models for agents are disabled in privacy settings".into(),
                ));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl AgentModel for CoreAgentModel {
    fn supports_native_tools(&self) -> bool {
        self.native_tools
    }

    async fn complete_tools(
        &self,
        messages: Vec<jarvis_ai::ToolMessage>,
        tools: Vec<jarvis_ai::ToolDefinition>,
    ) -> jarvis_agent::Result<jarvis_ai::ToolChatResponse> {
        self.authorize_request()?;
        let request = jarvis_ai::ToolChatRequest {
            model: self.model.clone(),
            messages,
            tools,
            options: GenerationOptions {
                temperature: Some(0.2),
                max_output_tokens: Some(AGENT_MAX_OUTPUT_TOKENS),
            },
        };
        match tokio::time::timeout(self.timeout, self.provider.chat_with_tools(request)).await {
            Ok(result) => result.map_err(|err| AgentError::Model(err.to_string())),
            Err(_) => Err(AgentError::Model("the model did not answer in time".into())),
        }
    }

    fn provider_id(&self) -> Option<String> {
        Some(self.provider_id.clone())
    }

    fn model(&self) -> String {
        self.model.clone()
    }

    fn context_chars(&self) -> usize {
        self.context_chars
    }

    async fn complete(&self, messages: Vec<ChatMessage>) -> jarvis_agent::Result<String> {
        self.authorize_request()?;
        let request = ChatRequest {
            model: self.model.clone(),
            messages,
            options: GenerationOptions {
                temperature: Some(0.2),
                max_output_tokens: Some(AGENT_MAX_OUTPUT_TOKENS),
            },
        };
        match tokio::time::timeout(self.timeout, self.provider.chat(request)).await {
            Ok(Ok(response)) => Ok(response.content),
            Ok(Err(err)) => Err(AgentError::Model(err.to_string())),
            Err(_) => Err(AgentError::Model(
                "the model did not answer in time".to_owned(),
            )),
        }
    }
}

impl JarvisCore {
    fn agents(&self) -> &AgentSystem {
        &self.agents
    }

    fn require_agents_enabled(&self) -> Result<(), CoreError> {
        if !self.config.current().agents.enabled {
            return Err(CoreError::PermissionDenied(
                "Agents are turned off in settings".to_owned(),
            ));
        }
        Ok(())
    }

    /// Chooses the model for an agent and applies the privacy rules: agents
    /// send file contents and program output to the model, so a cloud model
    /// needs its own permission.
    async fn agent_model(
        &self,
        provider_id: Option<&ProviderId>,
        model: Option<&str>,
    ) -> Result<Arc<dyn AgentModel>, CoreError> {
        let settings = self.config.current();
        let resolved: ResolvedModel = match (provider_id, model) {
            (None, None) => match &settings.ai.routing.tool_calling {
                Some(choice) => {
                    self.chat
                        .resolve_model(Some(&choice.provider_id), Some(&choice.model), &settings)
                        .await?
                }
                None => self.chat.resolve_model(None, None, &settings).await?,
            },
            (provider_id, model) => {
                self.chat
                    .resolve_model(provider_id, model, &settings)
                    .await?
            }
        };
        // Capability/catalog discovery can wait for the server. Check the
        // current switches again before this adapter can receive a journal.
        self.require_agents_enabled()?;
        let settings = self.config.current();
        crate::routing::check_privacy(&resolved.descriptor, settings.privacy.allow_cloud_providers)
            .inspect_err(|err| {
                self.events.warning(
                    EventCategory::Security,
                    format!("Blocked an agent task: {err}"),
                );
            })?;
        if resolved.descriptor.locality == DataLocality::ThirdPartyCloud
            && !settings.privacy.allow_cloud_agents
        {
            let message = format!(
                "Agents may not use the cloud model {} ({}): allow cloud models for agents in Privacy settings",
                resolved.model, resolved.descriptor.display_name
            );
            self.events.warning(
                EventCategory::Security,
                format!("Blocked an agent task: {message}"),
            );
            return Err(CoreError::PermissionDenied(message));
        }
        let mut window = resolved.context_window.unwrap_or(DEFAULT_CONTEXT_TOKENS);
        if let Some(limit) = settings.memory.context_limit_tokens {
            window = window.min(limit);
        }
        // About three characters per token, minus room for the answer.
        let context_chars =
            (window.saturating_sub(AGENT_MAX_OUTPUT_TOKENS).max(1024) as usize).saturating_mul(3);
        Ok(Arc::new(CoreAgentModel {
            config: Arc::clone(&self.config),
            native_tools: resolved.capabilities.tool_calling
                && resolved.provider.supports_native_tools(),
            provider: resolved.provider,
            provider_id: resolved.descriptor.id.to_string(),
            model: resolved.model,
            context_chars,
            timeout: Duration::from_secs(u64::from(settings.ai.request_timeout_secs)),
        }))
    }

    pub fn list_tools(&self) -> Vec<ToolInfo> {
        let enabled = self.config.current().tools.enabled;
        self.agents()
            .tools
            .builtin_specs()
            .into_iter()
            .map(|spec| ToolInfo {
                enabled: enabled.contains(&spec.name),
                spec,
            })
            .collect()
    }

    pub fn agent_status(&self) -> AgentStatus {
        let settings = self.config.current();
        let workspace_folders = settings
            .tools
            .workspace_folders
            .iter()
            .map(|path| WorkspaceFolder {
                available: std::path::Path::new(path).is_dir(),
                path: path.clone(),
            })
            .collect();
        AgentStatus {
            enabled: settings.agents.enabled,
            persistent: self.agents().persistent,
            running_tasks: u32::try_from(self.agents().manager.running_count()).unwrap_or(u32::MAX),
            workspace_folders,
            launched_programs: self
                .agents()
                .processes
                .list()
                .into_iter()
                .map(|(pid, program)| LaunchedProgram { pid, program })
                .collect(),
        }
    }

    pub async fn start_agent_task(
        &self,
        request: AgentStartRequest,
    ) -> Result<AgentTask, CoreError> {
        let settings = self.config.current();
        if !settings.agents.enabled {
            return Err(CoreError::PermissionDenied(
                "Agents are turned off in settings".to_owned(),
            ));
        }
        let model = self
            .agent_model(request.provider_id.as_ref(), request.model.as_deref())
            .await?;
        let limits = TaskLimits {
            max_steps: request.max_steps.unwrap_or(settings.agents.max_steps),
            max_duration_secs: request
                .max_duration_secs
                .unwrap_or(settings.agents.max_duration_secs),
        };
        let task = self
            .agents()
            .manager
            .start(
                NewTask {
                    goal: request.goal,
                    limits,
                },
                model,
            )
            .await?;
        self.events.info(
            EventCategory::Agents,
            format!(
                "Agent task started with {}: {}",
                task.model.as_deref().unwrap_or("a model"),
                clip(&task.goal, 120)
            ),
        );
        Ok(task)
    }

    /// Continues a paused task only with its recorded provider and model.
    /// Its journal must never be sent to an automatically chosen replacement.
    pub async fn resume_agent_task(&self, id: Uuid) -> Result<(), CoreError> {
        self.require_agents_enabled()?;
        let task = self.agents().manager.get(id).await?;
        if task
            .steps
            .iter()
            .any(|step| matches!(step.outcome, StepOutcome::InProgress | StepOutcome::Unknown))
        {
            return Err(AgentError::Invalid(
                "An action has an unknown result. Check its effects manually before starting a new task; it cannot be resumed.".into(),
            ).into());
        }
        if task.status != TaskStatus::Paused {
            return Err(AgentError::Invalid("only paused tasks can be resumed".into()).into());
        }
        let (Some(provider_id), Some(model_id)) = (
            task.provider_id
                .as_deref()
                .filter(|id| !id.trim().is_empty()),
            task.model.as_deref().filter(|id| !id.trim().is_empty()),
        ) else {
            return Err(CoreError::Validation(
                "The task has no recorded provider and model. Review its journal and start a new task; it cannot be resumed automatically.".into(),
            ));
        };
        let model = self
            .agent_model(Some(&ProviderId::from(provider_id)), Some(model_id))
            .await?;
        // Discovery may have waited for a model server while the user changed
        // the agent switch. Recheck before the manager can unpause or launch.
        self.require_agents_enabled()?;
        self.agents().manager.resume(id, model).await?;
        self.events.info(
            EventCategory::Agents,
            format!("Agent task resumed: {}", clip(&task.goal, 120)),
        );
        Ok(())
    }

    pub async fn cancel_agent_task(&self, id: Uuid) -> Result<bool, CoreError> {
        Ok(self.agents().manager.cancel(id).await?)
    }

    pub fn pause_agent_task(&self, id: Uuid) -> bool {
        self.agents().manager.pause(id)
    }

    pub async fn agent_task(&self, id: Uuid) -> Result<AgentTask, CoreError> {
        Ok(self.agents().manager.get(id).await?)
    }

    pub async fn list_agent_tasks(&self, limit: u32) -> Result<Vec<TaskSummary>, CoreError> {
        Ok(self.agents().manager.list(limit).await?)
    }

    /// Deletes finished tasks with known results (their audit entries stay).
    pub async fn clear_finished_agent_tasks(&self) -> Result<usize, CoreError> {
        let removed = self.agents().manager.delete_finished().await?;
        self.events.info(
            EventCategory::Agents,
            format!("{removed} finished agent task(s) deleted"),
        );
        Ok(removed)
    }

    /// A user explicitly reviewed the effects and selected this journal for deletion.
    pub async fn delete_reviewed_agent_task(&self, id: Uuid) -> Result<usize, CoreError> {
        let removed = self.agents().manager.delete_reviewed(id).await?;
        self.events.info(
            EventCategory::Agents,
            format!("Reviewed agent task journal deleted: {id}"),
        );
        Ok(removed)
    }

    pub fn pending_approvals(&self) -> Vec<PendingApproval> {
        self.agents().approvals.pending()
    }

    /// The user's decision on an action. `false` if it is no longer pending.
    pub fn resolve_approval(&self, id: Uuid, approved: bool) -> bool {
        self.agents().approvals.resolve(id, approved)
    }

    pub async fn audit_log(
        &self,
        limit: u32,
        before_id: Option<Uuid>,
    ) -> Result<Vec<AuditEntry>, CoreError> {
        Ok(self
            .agents()
            .audit
            .list(limit, before_id)
            .await
            .map_err(AgentError::from)?)
    }

    pub async fn verify_audit_log(&self) -> Result<AuditVerification, CoreError> {
        let verification = self
            .agents()
            .audit
            .verify()
            .await
            .map_err(AgentError::from)?;
        if !verification.intact {
            self.events.error(
                EventCategory::Security,
                "The audit log was changed outside JARVIS: its hash chain is broken",
            );
        }
        Ok(verification)
    }

    /// Every change of an agent task.
    pub fn subscribe_agent_tasks(&self) -> broadcast::Receiver<AgentTask> {
        self.agents().manager.subscribe()
    }

    /// Actions waiting for the user's approval.
    pub fn subscribe_approvals(&self) -> watch::Receiver<Vec<PendingApproval>> {
        self.agents().approvals.subscribe()
    }

    /// Reports finished tasks and approval requests as system events.
    pub(crate) fn spawn_agent_events(core: &Arc<Self>) {
        let mut updates = core.subscribe_agent_tasks();
        let events = core.events.clone();
        tokio::spawn(async move {
            loop {
                let task = match updates.recv().await {
                    Ok(task) => task,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let goal = clip(&task.goal, 120);
                match task.status {
                    jarvis_agent::TaskStatus::Completed => {
                        events.info(
                            EventCategory::Agents,
                            format!("Agent task completed: {goal}"),
                        );
                    }
                    jarvis_agent::TaskStatus::Failed => {
                        events.warning(
                            EventCategory::Agents,
                            format!(
                                "Agent task failed: {goal} ({})",
                                task.error.as_deref().unwrap_or("unknown error")
                            ),
                        );
                    }
                    jarvis_agent::TaskStatus::Unverified => {
                        events.warning(
                            EventCategory::Agents,
                            format!("Agent answer could not be verified: {goal}"),
                        );
                    }
                    jarvis_agent::TaskStatus::Cancelled => {
                        events.info(
                            EventCategory::Agents,
                            format!("Agent task cancelled: {goal}"),
                        );
                    }
                    _ => {}
                }
            }
        });

        let mut approvals = core.subscribe_approvals();
        let events = core.events.clone();
        tokio::spawn(async move {
            let mut known: Vec<Uuid> = Vec::new();
            while approvals.changed().await.is_ok() {
                let pending = approvals.borrow_and_update().clone();
                for approval in pending.iter().filter(|a| !known.contains(&a.id)) {
                    events.warning(
                        EventCategory::Security,
                        format!(
                            "Approval needed ({} risk): {}",
                            approval.risk.as_str(),
                            approval.summary
                        ),
                    );
                }
                known = pending.iter().map(|a| a.id).collect();
            }
        });
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use jarvis_ai::{
        AiError, ChatStream, ModelInfo, ProviderBackend, ProviderConfig, ProviderDescriptor,
        ProviderKind, ToolChatRequest, ToolChatResponse,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct NativeProvider {
        descriptor: ProviderDescriptor,
        delay: Duration,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AiProvider for NativeProvider {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }
        fn supports_native_tools(&self) -> bool {
            true
        }
        async fn health_check(&self) -> Result<(), AiError> {
            Ok(())
        }
        async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
            Ok(vec![])
        }
        async fn stream_chat(&self, _: ChatRequest) -> Result<ChatStream, AiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("native generation must not fall back to text")
        }
        async fn chat_with_tools(
            &self,
            request: ToolChatRequest,
        ) -> Result<ToolChatResponse, AiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.model, "native-model");
            assert_eq!(
                request.options.max_output_tokens,
                Some(AGENT_MAX_OUTPUT_TOKENS)
            );
            assert_eq!(request.options.temperature, Some(0.2));
            tokio::time::sleep(self.delay).await;
            Ok(ToolChatResponse {
                content: "done".into(),
                calls: vec![],
            })
        }
    }

    fn model(delay: Duration) -> CoreAgentModel {
        CoreAgentModel {
            config: Arc::new(ConfigStore::in_memory(
                std::env::temp_dir().join("jarvis-unused-agent-config.toml"),
                Settings::default(),
            )),
            provider: Arc::new(NativeProvider {
                descriptor: jarvis_ai::EchoProvider::new().descriptor().clone(),
                delay,
                calls: Arc::default(),
            }),
            provider_id: "native".into(),
            model: "native-model".into(),
            context_chars: 4096,
            timeout: Duration::from_secs(1),
            native_tools: true,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn native_adapter_preserves_generation_options_and_returns_response() {
        let result = model(Duration::ZERO).complete_tools(vec![], vec![]).await;
        assert!(matches!(result, Ok(response) if response.content == "done"));
    }

    #[tokio::test(start_paused = true)]
    async fn native_adapter_times_out_without_text_fallback() {
        let result = model(Duration::from_secs(60))
            .complete_tools(vec![], vec![])
            .await;
        assert!(matches!(result, Err(AgentError::Model(message)) if message.contains("in time")));
    }

    #[tokio::test]
    async fn cached_adapters_recheck_their_own_destination_before_text_and_native_requests() {
        for permission in ["agents", "cloud providers", "cloud agents"] {
            let dir = tempfile::tempdir().expect("temporary settings");
            let mut settings = Settings::default();
            settings.privacy.allow_cloud_providers = true;
            settings.privacy.allow_cloud_agents = true;
            let config = Arc::new(ConfigStore::in_memory(
                dir.path().join("config.toml"),
                settings.clone(),
            ));
            let calls = Arc::new(AtomicUsize::new(0));
            let mut cached = model(Duration::ZERO);
            cached.config = Arc::clone(&config);
            cached.provider = Arc::new(NativeProvider {
                descriptor: ProviderDescriptor {
                    id: ProviderId::new("native"),
                    display_name: "Original cloud".into(),
                    kind: ProviderKind::Cloud,
                    locality: DataLocality::ThirdPartyCloud,
                },
                delay: Duration::ZERO,
                calls: Arc::clone(&calls),
            });
            // The same configured ID now points locally. The cached model must
            // still recognize that its own provider sends to the old cloud.
            settings.ai.providers = vec![ProviderConfig {
                id: ProviderId::new("native"),
                backend: ProviderBackend::Ollama,
                display_name: "Replacement local".into(),
                base_url: "http://127.0.0.1:11434".into(),
                enabled: true,
                native_tool_models: vec![],
            }];
            match permission {
                "agents" => settings.agents.enabled = false,
                "cloud providers" => settings.privacy.allow_cloud_providers = false,
                _ => settings.privacy.allow_cloud_agents = false,
            }
            config.update(settings).await.expect("revoke permission");
            assert!(cached.complete(vec![]).await.is_err(), "{permission}");
            assert!(
                cached.complete_tools(vec![], vec![]).await.is_err(),
                "{permission}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{permission}");
        }
    }

    #[test]
    fn disabled_agents_refuse_builtin_and_plugin_tools() {
        use jarvis_security::{ActionRequest, Capability, PolicyDecision, RiskLevel};
        let mut settings = Settings::default();
        settings.tools.enabled = vec!["read_file".into()];
        let plugins = BTreeSet::from(["plugin/tool".into()]);
        let actions = [
            ("read_file", Capability::FsRead),
            ("plugin/tool", Capability::PluginTool),
        ]
        .map(|(tool, capability)| ActionRequest {
            tool: tool.into(),
            capabilities: vec![capability],
            risk: RiskLevel::Low,
            summary: "Check the agent switch".into(),
            paths: vec![],
            program: None,
            url: None,
        });
        let enabled = agent_settings(&settings, &[], &plugins);
        for action in &actions {
            assert!(!matches!(
                enabled.policy.evaluate(action),
                PolicyDecision::Deny { .. }
            ));
        }
        settings.agents.enabled = false;
        let disabled = agent_settings(&settings, &[], &plugins);
        for action in &actions {
            assert!(matches!(
                disabled.policy.evaluate(action),
                PolicyDecision::Deny { .. }
            ));
        }
    }
}
