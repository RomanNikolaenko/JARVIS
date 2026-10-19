//! User settings: model, validation and persistent storage.
//!
//! Settings live in a single TOML file (`config.toml`) in the platform config
//! directory. Writes are atomic (temp file + rename). A file that cannot be
//! parsed is never overwritten silently: JARVIS runs with defaults, reports a
//! degraded health status and backs the file up before the next save.

use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use jarvis_ai::ProviderId;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use ts_rs::TS;

use crate::error::CoreError;

/// Version of the settings schema written by this build.
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

pub const MIN_REQUEST_TIMEOUT_SECS: u32 = 10;
pub const MAX_REQUEST_TIMEOUT_SECS: u32 = 3600;
pub const MIN_IDLE_TIMEOUT_SECS: u32 = 5;
pub const MAX_IDLE_TIMEOUT_SECS: u32 = 600;
const MAX_MODEL_ID_LEN: usize = 256;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "core.ts")]
pub struct Settings {
    pub schema_version: u32,
    pub general: GeneralSettings,
    pub interface: InterfaceSettings,
    pub ai: AiSettings,
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
            privacy: PrivacySettings::default(),
            logging: LoggingSettings::default(),
        }
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
    /// Provider used when a request does not name one.
    pub default_provider_id: Option<ProviderId>,
    /// Model used when a request does not name one.
    pub default_model: Option<String>,
    /// Upper bound for a whole chat request.
    pub request_timeout_secs: u32,
    /// Maximum silence between two streamed chunks.
    pub idle_timeout_secs: u32,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            default_provider_id: None,
            default_model: None,
            request_timeout_secs: 300,
            idle_timeout_secs: 60,
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
        if let Some(model) = &ai.default_model
            && (model.trim().is_empty() || model.len() > MAX_MODEL_ID_LEN)
        {
            problems.push(format!(
                "ai.defaultModel must be 1..={MAX_MODEL_ID_LEN} characters"
            ));
        }
        if let Some(provider) = &ai.default_provider_id
            && !is_valid_provider_id(provider.as_str())
        {
            problems.push("ai.defaultProviderId has an invalid format".to_owned());
        }

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

fn is_valid_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
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
        settings.ai.default_model = Some("  ".into());
        settings.ai.default_provider_id = Some(ProviderId::new("Bad Id!"));

        let err = settings.validate().expect_err("invalid");
        let message = err.to_string();
        assert!(message.contains("requestTimeoutSecs"), "{message}");
        assert!(
            message.contains("idleTimeoutSecs must be between"),
            "{message}"
        );
        assert!(message.contains("defaultModel"), "{message}");
        assert!(message.contains("defaultProviderId"), "{message}");
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
        settings.ai.default_model = Some("llama3.2:3b".into());
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
