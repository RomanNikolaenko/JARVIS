//! User settings: model, validation and persistent storage.
//!
//! Settings live in a single TOML file (`config.toml`) in the platform config
//! directory. Writes are atomic (temp file + rename). A file that cannot be
//! parsed is never overwritten silently: JARVIS runs with defaults, reports a
//! degraded health status and backs the file up before the next save.

use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use jarvis_ai::{EchoProvider, ProviderConfig};
use jarvis_runtime::{RUNTIME_EMBEDDINGS_PROVIDER_ID, RUNTIME_PROVIDER_ID, RuntimeSettings};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use ts_rs::TS;

use crate::error::CoreError;
use crate::routing::{ModelRef, ModelRouting};

/// Version of the settings schema written by this build.
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

pub const MIN_REQUEST_TIMEOUT_SECS: u32 = 10;
pub const MAX_REQUEST_TIMEOUT_SECS: u32 = 3600;
pub const MIN_IDLE_TIMEOUT_SECS: u32 = 5;
pub const MAX_IDLE_TIMEOUT_SECS: u32 = 600;
pub const MAX_PROVIDERS: usize = 16;
pub(crate) const MAX_MODEL_ID_LEN: usize = 256;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "core.ts")]
pub struct Settings {
    pub schema_version: u32,
    pub general: GeneralSettings,
    pub interface: InterfaceSettings,
    pub ai: AiSettings,
    /// Embedded llama.cpp runtime.
    pub runtime: RuntimeSettings,
    pub memory: MemorySettings,
    /// Tools agents may use and what they need approval for.
    pub tools: ToolSettings,
    pub agents: AgentsSettings,
    pub privacy: PrivacySettings,
    pub logging: LoggingSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            general: GeneralSettings::default(),
            interface: InterfaceSettings::default(),
            ai: AiSettings::default(),
            runtime: RuntimeSettings::default(),
            memory: MemorySettings::default(),
            tools: ToolSettings::default(),
            agents: AgentsSettings::default(),
            privacy: PrivacySettings::default(),
            logging: LoggingSettings::default(),
        }
    }
}

pub const MIN_CONTEXT_LIMIT_TOKENS: u32 = 1024;
pub const MAX_CONTEXT_LIMIT_TOKENS: u32 = 1_048_576;
pub const MAX_EPISODE_RETENTION_DAYS: u32 = 3650;

/// Long-term memory and context management.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "core.ts")]
pub struct MemorySettings {
    /// Save conversations, facts and the action history. When off, new
    /// chats are temporary and nothing is written to disk.
    pub enabled: bool,
    /// Add pinned and relevant facts to requests.
    pub use_facts: bool,
    /// Add excerpts of earlier conversations to requests.
    pub recall_conversations: bool,
    /// Summarize the older part of long conversations with the chat model.
    pub auto_summarize: bool,
    /// Model for semantic search; unset picks a local model that supports
    /// embeddings. Without one, search is text-only.
    pub embedding_model: Option<ModelRef>,
    /// Upper bound for the context window of a request, in tokens; unset
    /// uses the model's window.
    pub context_limit_tokens: Option<u32>,
    /// Days to keep the action history (0 keeps it forever).
    pub episode_retention_days: u32,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            use_facts: true,
            recall_conversations: true,
            auto_summarize: true,
            embedding_model: None,
            context_limit_tokens: None,
            episode_retention_days: 90,
        }
    }
}

impl MemorySettings {
    fn problems(&self, providers: &[ProviderConfig]) -> Vec<String> {
        let mut problems = Vec::new();
        if let Some(limit) = self.context_limit_tokens
            && !(MIN_CONTEXT_LIMIT_TOKENS..=MAX_CONTEXT_LIMIT_TOKENS).contains(&limit)
        {
            problems.push(format!(
                "memory.contextLimitTokens must be between {MIN_CONTEXT_LIMIT_TOKENS} and {MAX_CONTEXT_LIMIT_TOKENS}"
            ));
        }
        if self.episode_retention_days > MAX_EPISODE_RETENTION_DAYS {
            problems.push(format!(
                "memory.episodeRetentionDays must be at most {MAX_EPISODE_RETENTION_DAYS}"
            ));
        }
        if let Some(model_ref) = &self.embedding_model {
            let known = is_reserved_provider_id(model_ref.provider_id.as_str())
                || providers.iter().any(|p| p.id == model_ref.provider_id);
            if !known {
                problems.push(format!(
                    "memory.embeddingModel: unknown provider \"{}\"",
                    model_ref.provider_id
                ));
            }
            let model = model_ref.model.trim();
            if model.is_empty() || model.len() > MAX_MODEL_ID_LEN {
                problems.push(format!(
                    "memory.embeddingModel: model id must be 1..={MAX_MODEL_ID_LEN} characters"
                ));
            }
        }
        problems
    }
}

pub const MAX_WORKSPACE_FOLDERS: usize = 16;
pub const MAX_ALLOWED_PROGRAMS: usize = 32;
pub const MIN_COMMAND_TIMEOUT_SECS: u32 = 5;
pub const MAX_COMMAND_TIMEOUT_SECS: u32 = 600;
pub const MIN_APPROVAL_TIMEOUT_SECS: u32 = 30;
pub const MAX_APPROVAL_TIMEOUT_SECS: u32 = 3600;
const MAX_FOLDER_LEN: usize = 1024;

/// Settings of the Security Policy Engine for tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "core.ts")]
pub struct ToolSettings {
    /// Tools that may be used at all.
    pub enabled: Vec<String>,
    /// Folders file tools may use (absolute paths); nothing outside them is
    /// reachable. JARVIS's own data and settings stay off limits.
    pub workspace_folders: Vec<String>,
    /// Programs `run_command` and `launch_program` may start (plain names
    /// such as `git`, no paths).
    pub allowed_programs: Vec<String>,
    /// Whether `fetch_url` may load public web pages.
    pub allow_network: bool,
    /// Run read-only actions inside the workspace without asking.
    pub auto_approve_read_only: bool,
    /// Run medium-risk actions (create or edit files, copy, move, fetch or
    /// open pages) without asking. High-risk actions always ask.
    pub auto_approve_changes: bool,
    /// Time limit of one program run by `run_command`.
    pub command_timeout_secs: u32,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            enabled: jarvis_tools::DEFAULT_ENABLED
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            workspace_folders: Vec::new(),
            allowed_programs: Vec::new(),
            allow_network: false,
            auto_approve_read_only: true,
            auto_approve_changes: false,
            command_timeout_secs: 60,
        }
    }
}

impl ToolSettings {
    fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let known: Vec<String> = jarvis_tools::ToolRegistry::builtin()
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect();
        for name in &self.enabled {
            if !known.contains(name) {
                problems.push(format!("tools.enabled: unknown tool \"{name}\""));
            }
        }
        if self.workspace_folders.len() > MAX_WORKSPACE_FOLDERS {
            problems.push(format!(
                "tools.workspaceFolders: at most {MAX_WORKSPACE_FOLDERS} folders"
            ));
        }
        for folder in &self.workspace_folders {
            if folder.len() > MAX_FOLDER_LEN || !Path::new(folder).is_absolute() {
                problems.push(format!(
                    "tools.workspaceFolders: \"{folder}\" is not an absolute path"
                ));
            }
        }
        if self.allowed_programs.len() > MAX_ALLOWED_PROGRAMS {
            problems.push(format!(
                "tools.allowedPrograms: at most {MAX_ALLOWED_PROGRAMS} programs"
            ));
        }
        for program in &self.allowed_programs {
            if !jarvis_security::is_plain_program_name(program) {
                problems.push(format!(
                    "tools.allowedPrograms: \"{program}\" must be a plain program name"
                ));
            }
        }
        if !(MIN_COMMAND_TIMEOUT_SECS..=MAX_COMMAND_TIMEOUT_SECS)
            .contains(&self.command_timeout_secs)
        {
            problems.push(format!(
                "tools.commandTimeoutSecs must be between {MIN_COMMAND_TIMEOUT_SECS} and {MAX_COMMAND_TIMEOUT_SECS}"
            ));
        }
        problems
    }
}

/// Agent tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "core.ts")]
pub struct AgentsSettings {
    /// Whether agent tasks can be started.
    pub enabled: bool,
    /// Default number of steps (tool calls and answers) per task.
    pub max_steps: u32,
    /// Default time limit per task run.
    pub max_duration_secs: u32,
    /// How long an action waits for approval before it counts as refused.
    pub approval_timeout_secs: u32,
}

impl Default for AgentsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_steps: jarvis_agent::DEFAULT_MAX_STEPS,
            max_duration_secs: jarvis_agent::DEFAULT_MAX_DURATION_SECS,
            approval_timeout_secs: 300,
        }
    }
}

impl AgentsSettings {
    fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if !(1..=jarvis_agent::MAX_STEPS).contains(&self.max_steps) {
            problems.push(format!(
                "agents.maxSteps must be between 1 and {}",
                jarvis_agent::MAX_STEPS
            ));
        }
        if !(30..=jarvis_agent::MAX_DURATION_SECS).contains(&self.max_duration_secs) {
            problems.push(format!(
                "agents.maxDurationSecs must be between 30 and {}",
                jarvis_agent::MAX_DURATION_SECS
            ));
        }
        if !(MIN_APPROVAL_TIMEOUT_SECS..=MAX_APPROVAL_TIMEOUT_SECS)
            .contains(&self.approval_timeout_secs)
        {
            problems.push(format!(
                "agents.approvalTimeoutSecs must be between {MIN_APPROVAL_TIMEOUT_SECS} and {MAX_APPROVAL_TIMEOUT_SECS}"
            ));
        }
        problems
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub enum AssistantLanguage {
    #[default]
    Uk,
    En,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "core.ts")]
pub struct GeneralSettings {
    /// Language JARVIS speaks and answers in.
    pub assistant_language: AssistantLanguage,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "core.ts")]
pub struct InterfaceSettings {
    /// Disable non-essential animations regardless of the OS preference.
    pub reduced_motion: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "core.ts")]
pub struct AiSettings {
    /// AI servers JARVIS connects to, in priority order.
    pub providers: Vec<ProviderConfig>,
    /// Explicit model choices per task category; unset means automatic.
    pub routing: ModelRouting,
    /// Upper bound for a whole chat request.
    pub request_timeout_secs: u32,
    /// Maximum silence between two streamed chunks. Also bounds the wait
    /// for the first token, which includes model loading.
    pub idle_timeout_secs: u32,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            providers: ProviderConfig::defaults(),
            routing: ModelRouting::default(),
            request_timeout_secs: 300,
            idle_timeout_secs: 120,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "core.ts")]
pub struct PrivacySettings {
    /// Whether conversation data may be sent to third-party cloud providers.
    /// Off by default (local first).
    pub allow_cloud_providers: bool,
    /// Whether saved memory (facts, excerpts of other conversations,
    /// embeddings) may be sent to cloud providers. Off by default; the
    /// current conversation itself goes wherever the chat goes.
    pub share_memory_with_cloud: bool,
    /// Whether agents may use cloud models. Agents send file contents and
    /// program output to the model; this also needs `allowCloudProviders`.
    /// Off by default.
    pub allow_cloud_agents: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "core.ts")]
pub struct LoggingSettings {
    pub level: LogLevel,
}

impl Settings {
    /// Validates value ranges and cross-field constraints.
    pub fn validate(&self) -> Result<(), CoreError> {
        let mut problems = Vec::new();

        if self.schema_version != SETTINGS_SCHEMA_VERSION {
            problems.push(format!(
                "unsupported settings schema version {} (expected {SETTINGS_SCHEMA_VERSION})",
                self.schema_version
            ));
        }

        let ai = &self.ai;
        if !(MIN_REQUEST_TIMEOUT_SECS..=MAX_REQUEST_TIMEOUT_SECS).contains(&ai.request_timeout_secs)
        {
            problems.push(format!(
                "ai.requestTimeoutSecs must be between {MIN_REQUEST_TIMEOUT_SECS} and {MAX_REQUEST_TIMEOUT_SECS}"
            ));
        }
        if !(MIN_IDLE_TIMEOUT_SECS..=MAX_IDLE_TIMEOUT_SECS).contains(&ai.idle_timeout_secs) {
            problems.push(format!(
                "ai.idleTimeoutSecs must be between {MIN_IDLE_TIMEOUT_SECS} and {MAX_IDLE_TIMEOUT_SECS}"
            ));
        }
        if ai.idle_timeout_secs > ai.request_timeout_secs {
            problems.push("ai.idleTimeoutSecs must not exceed ai.requestTimeoutSecs".to_owned());
        }
        validate_providers(&ai.providers, &mut problems);
        validate_routing(&ai.routing, &ai.providers, &mut problems);
        problems.extend(self.runtime.problems());
        problems.extend(self.memory.problems(&ai.providers));
        problems.extend(self.tools.problems());
        problems.extend(self.agents.problems());

        if problems.is_empty() {
            Ok(())
        } else {
            Err(CoreError::Validation(format!(
                "invalid settings: {}",
                problems.join("; ")
            )))
        }
    }
}

/// Ids used by built-in providers (diagnostic echo, embedded runtime).
pub fn is_reserved_provider_id(id: &str) -> bool {
    id == EchoProvider::ID || id == RUNTIME_PROVIDER_ID || id == RUNTIME_EMBEDDINGS_PROVIDER_ID
}

fn validate_providers(providers: &[ProviderConfig], problems: &mut Vec<String>) {
    if providers.len() > MAX_PROVIDERS {
        problems.push(format!(
            "at most {MAX_PROVIDERS} AI providers can be configured"
        ));
    }
    for (index, provider) in providers.iter().enumerate() {
        if let Err(problem) = provider.validate() {
            problems.push(format!("ai.providers: {problem}"));
        }
        if is_reserved_provider_id(provider.id.as_str()) {
            problems.push(format!("ai.providers: id \"{}\" is reserved", provider.id));
        }
        if providers[..index].iter().any(|p| p.id == provider.id) {
            problems.push(format!("ai.providers: duplicate id \"{}\"", provider.id));
        }
    }
}

fn validate_routing(
    routing: &ModelRouting,
    providers: &[ProviderConfig],
    problems: &mut Vec<String>,
) {
    for (category, model_ref) in routing.entries() {
        let known = is_reserved_provider_id(model_ref.provider_id.as_str())
            || providers.iter().any(|p| p.id == model_ref.provider_id);
        if !known {
            problems.push(format!(
                "ai.routing.{}: unknown provider \"{}\"",
                category.key(),
                model_ref.provider_id
            ));
        }
        let model = model_ref.model.trim();
        if model.is_empty() || model.len() > MAX_MODEL_ID_LEN {
            problems.push(format!(
                "ai.routing.{}: model id must be 1..={MAX_MODEL_ID_LEN} characters",
                category.key()
            ));
        }
    }
}

/// Where the currently active settings came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Loaded from an existing, valid file.
    File,
    /// No file existed; defaults were written to disk.
    CreatedDefaults,
    /// The file could not be used. Defaults are active and the file has not
    /// been modified. `error` explains why.
    Fallback { error: String },
}

#[derive(Debug)]
struct ConfigState {
    settings: Settings,
    source: ConfigSource,
}

/// Owns the settings file and the in-memory copy of the settings.
#[derive(Debug)]
pub struct ConfigStore {
    path: PathBuf,
    state: RwLock<ConfigState>,
    /// Serialises writers so concurrent saves cannot interleave on disk.
    write_lock: tokio::sync::Mutex<()>,
}

impl ConfigStore {
    /// Loads settings from `path`, creating the file with defaults if it does
    /// not exist. Never fails because of file *contents*; see [`ConfigSource`].
    pub async fn load(path: PathBuf) -> Self {
        let (settings, source) = match tokio::fs::read_to_string(&path).await {
            Ok(text) => match parse_settings(&text) {
                Ok(settings) => (settings, ConfigSource::File),
                Err(error) => {
                    tracing::warn!(path = %path.display(), "settings file rejected: {error}");
                    (Settings::default(), ConfigSource::Fallback { error })
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let defaults = Settings::default();
                match write_atomic(&path, &defaults).await {
                    Ok(()) => (defaults, ConfigSource::CreatedDefaults),
                    Err(err) => {
                        tracing::warn!(path = %path.display(), "cannot create settings file: {err}");
                        (
                            defaults,
                            ConfigSource::Fallback {
                                error: err.to_string(),
                            },
                        )
                    }
                }
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), "cannot read settings file: {err}");
                (
                    Settings::default(),
                    ConfigSource::Fallback {
                        error: format!("cannot read settings file: {err}"),
                    },
                )
            }
        };

        Self {
            path,
            state: RwLock::new(ConfigState { settings, source }),
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// In-memory store for tests; [`ConfigStore::update`] still writes to `path`.
    #[cfg(test)]
    pub(crate) fn in_memory(path: PathBuf, settings: Settings) -> Self {
        Self {
            path,
            state: RwLock::new(ConfigState {
                settings,
                source: ConfigSource::File,
            }),
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A snapshot of the current settings.
    pub fn current(&self) -> Settings {
        self.state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .settings
            .clone()
    }

    pub fn source(&self) -> ConfigSource {
        self.state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .source
            .clone()
    }

    /// Validates and persists new settings, then makes them active.
    ///
    /// If the existing file was rejected at load time it is first renamed to
    /// `config.toml.invalid-<unix-ms>.bak` so the user's data is preserved.
    pub async fn update(&self, settings: Settings) -> Result<Settings, CoreError> {
        settings.validate()?;
        let _guard = self.write_lock.lock().await;

        if matches!(self.source(), ConfigSource::Fallback { .. })
            && tokio::fs::try_exists(&self.path).await.unwrap_or(false)
        {
            let backup = backup_path(&self.path);
            tokio::fs::rename(&self.path, &backup)
                .await
                .map_err(|err| CoreError::io("backing up the rejected settings file", err))?;
            tracing::warn!(backup = %backup.display(), "rejected settings file backed up");
        }

        write_atomic(&self.path, &settings)
            .await
            .map_err(|err| CoreError::io("saving settings", err))?;

        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        state.settings = settings.clone();
        state.source = ConfigSource::File;
        Ok(settings)
    }
}

fn parse_settings(text: &str) -> Result<Settings, String> {
    let settings: Settings =
        toml::from_str(text).map_err(|err| format!("invalid TOML: {}", err.message()))?;
    if settings.schema_version > SETTINGS_SCHEMA_VERSION {
        return Err(format!(
            "settings were written by a newer JARVIS version (schema {})",
            settings.schema_version
        ));
    }
    settings.validate().map_err(|err| err.to_string())?;
    Ok(settings)
}

fn backup_path(path: &Path) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "config.toml".into());
    name.push(format!(".invalid-{millis}.bak"));
    path.with_file_name(name)
}

async fn write_atomic(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let body = toml::to_string_pretty(settings).map_err(std::io::Error::other)?;
    let contents = format!(
        "# JARVIS settings. Edit while JARVIS is closed, or use the Settings screen.\n{body}"
    );

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut tmp_name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    tmp_name.push(".tmp");
    let tmp_path = path.with_file_name(tmp_name);

    {
        let mut file = tokio::fs::File::create(&tmp_path).await?;
        file.write_all(contents.as_bytes()).await?;
        file.sync_all().await?;
    }
    // `rename` replaces the destination atomically on both Windows and Unix.
    if let Err(err) = tokio::fs::rename(&tmp_path, path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::ModelRef;
    use jarvis_ai::ProviderId;

    fn config_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("config").join("config.toml")
    }

    #[test]
    fn defaults_are_valid() {
        Settings::default()
            .validate()
            .expect("defaults must validate");
    }

    #[test]
    fn validation_reports_all_problems() {
        let mut settings = Settings::default();
        settings.ai.request_timeout_secs = 1;
        settings.ai.idle_timeout_secs = 5000;
        settings.ai.routing.general = Some(ModelRef {
            provider_id: ProviderId::new("nowhere"),
            model: "  ".into(),
        });

        let err = settings.validate().expect_err("invalid");
        let message = err.to_string();
        assert!(message.contains("requestTimeoutSecs"), "{message}");
        assert!(
            message.contains("idleTimeoutSecs must be between"),
            "{message}"
        );
        assert!(
            message.contains("unknown provider \"nowhere\""),
            "{message}"
        );
        assert!(
            message.contains("ai.routing.general: model id"),
            "{message}"
        );
    }

    #[test]
    fn validation_rejects_bad_provider_lists() {
        let mut settings = Settings::default();
        let mut duplicate = settings.ai.providers[0].clone();
        duplicate.base_url = "http://user:secret@127.0.0.1:11434".into();
        settings.ai.providers.push(duplicate);
        let mut reserved = settings.ai.providers[0].clone();
        reserved.id = ProviderId::new(EchoProvider::ID);
        settings.ai.providers.push(reserved);

        let message = settings.validate().expect_err("invalid").to_string();
        assert!(message.contains("duplicate id \"ollama\""), "{message}");
        assert!(
            message.contains("must not contain credentials"),
            "{message}"
        );
        assert!(message.contains("is reserved"), "{message}");
    }

    #[test]
    fn provider_lists_roundtrip_through_toml() {
        let mut settings = Settings::default();
        settings.ai.providers[1].enabled = false;
        settings.ai.routing.coding = Some(ModelRef {
            provider_id: ProviderId::new("ollama"),
            model: "qwen2.5-coder:7b".into(),
        });
        let text = toml::to_string_pretty(&settings).expect("serialize");
        assert!(text.contains("[[ai.providers]]"), "{text}");
        assert_eq!(parse_settings(&text).expect("parse"), settings);
    }

    #[test]
    fn partial_files_fill_in_defaults() {
        let settings = parse_settings("schemaVersion = 1\n[ai]\nidleTimeoutSecs = 30\n")
            .expect("partial settings parse");
        assert_eq!(settings.ai.idle_timeout_secs, 30);
        assert_eq!(settings.ai.request_timeout_secs, 300);
        assert_eq!(settings.general.assistant_language, AssistantLanguage::Uk);
    }

    #[test]
    fn rejects_newer_schema_versions() {
        let err = parse_settings("schemaVersion = 99\n").expect_err("newer schema");
        assert!(err.contains("newer JARVIS version"), "{err}");
    }

    #[tokio::test]
    async fn creates_defaults_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = config_path(&dir);

        let store = ConfigStore::load(path.clone()).await;
        assert_eq!(store.source(), ConfigSource::CreatedDefaults);
        assert_eq!(store.current(), Settings::default());

        let written = std::fs::read_to_string(&path).expect("file written");
        assert_eq!(
            parse_settings(&written).expect("parse"),
            Settings::default()
        );
    }

    #[tokio::test]
    async fn update_persists_and_reloads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = config_path(&dir);
        let store = ConfigStore::load(path.clone()).await;

        let mut settings = store.current();
        settings.general.assistant_language = AssistantLanguage::En;
        settings.privacy.allow_cloud_providers = true;
        settings.ai.routing.general = Some(ModelRef {
            provider_id: ProviderId::new("ollama"),
            model: "llama3.2:3b".into(),
        });
        store.update(settings.clone()).await.expect("update");

        let reloaded = ConfigStore::load(path).await;
        assert_eq!(reloaded.source(), ConfigSource::File);
        assert_eq!(reloaded.current(), settings);
    }

    #[tokio::test]
    async fn invalid_update_is_rejected_without_side_effects() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::load(config_path(&dir)).await;

        let mut invalid = store.current();
        invalid.ai.request_timeout_secs = 0;
        assert!(matches!(
            store.update(invalid).await,
            Err(CoreError::Validation(_))
        ));
        assert_eq!(store.current(), Settings::default());
    }

    #[tokio::test]
    async fn broken_file_is_preserved_and_backed_up_on_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = config_path(&dir);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "this is = = not toml").expect("write broken file");

        let store = ConfigStore::load(path.clone()).await;
        assert!(matches!(store.source(), ConfigSource::Fallback { .. }));
        assert_eq!(store.current(), Settings::default());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "this is = = not toml",
            "a rejected file must not be overwritten at load time"
        );

        store.update(Settings::default()).await.expect("save");
        assert_eq!(store.source(), ConfigSource::File);

        let backups: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("list")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".bak"))
            .collect();
        assert_eq!(backups.len(), 1, "exactly one backup expected");
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).expect("read backup"),
            "this is = = not toml"
        );
    }
}
