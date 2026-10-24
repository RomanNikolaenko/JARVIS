//! `plugins.json`: plugins, approvals and per-tool choices.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jarvis_security::RiskLevel;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::launch::LaunchConfig;
use crate::types::SecretRequirement;

const STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreFile {
    version: u32,
    plugins: Vec<PluginRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum Source {
    /// A folder with a manifest.
    Folder { path: String },
    /// A server entered as a command.
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolChoice {
    pub enabled: bool,
    pub risk: RiskLevel,
    /// Hash of the tool's definition when the user lowered its risk.
    #[serde(default)]
    pub definition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    pub source: Source,
    /// Launch settings as approved (for manifest plugins, a copy of the
    /// manifest's; the fingerprint shows when the manifest changes).
    pub launch: LaunchConfig,
    #[serde(default)]
    pub secrets: Vec<SecretRequirement>,
    pub enabled: bool,
    #[serde(default)]
    pub approved_fingerprint: Option<String>,
    /// By the server's tool name.
    #[serde(default)]
    pub tools: BTreeMap<String, ToolChoice>,
    #[serde(default)]
    pub added_at_ms: u64,
}

impl PluginRecord {
    pub fn folder(&self) -> Option<PathBuf> {
        match &self.source {
            Source::Folder { path } => Some(PathBuf::from(path)),
            Source::Command => None,
        }
    }

    pub fn secret_names(&self) -> Vec<String> {
        self.secrets.iter().map(|s| s.name.clone()).collect()
    }
}

/// What loading found.
#[derive(Debug)]
pub(crate) struct Loaded {
    pub plugins: Vec<PluginRecord>,
    /// The file could not be read; it was moved to this path.
    pub moved_to: Option<(PathBuf, String)>,
}

/// Reads the store; a missing file is empty, an invalid one is moved aside
/// (never deleted) and replaced by an empty store.
pub(crate) async fn load(path: &Path) -> std::io::Result<Loaded> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded {
                plugins: Vec::new(),
                moved_to: None,
            });
        }
        Err(err) => return Err(err),
    };
    match serde_json::from_slice::<StoreFile>(&bytes) {
        Ok(file) if file.version == STORE_VERSION => Ok(Loaded {
            plugins: file.plugins,
            moved_to: None,
        }),
        Ok(file) => move_aside(path, format!("unknown version {}", file.version)).await,
        Err(err) => move_aside(path, err.to_string()).await,
    }
}

async fn move_aside(path: &Path, problem: String) -> std::io::Result<Loaded> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "plugins.json".into());
    name.push(format!(".invalid-{millis}.bak"));
    let backup = path.with_file_name(name);
    tokio::fs::rename(path, &backup).await?;
    Ok(Loaded {
        plugins: Vec::new(),
        moved_to: Some((backup, problem)),
    })
}

/// Writes the store atomically (temporary file, then rename).
pub(crate) async fn save(path: &Path, plugins: &[PluginRecord]) -> std::io::Result<()> {
    let file = StoreFile {
        version: STORE_VERSION,
        plugins: plugins.to_vec(),
    };
    let body = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
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
        let mut tmp = tokio::fs::File::create(&tmp_path).await?;
        tmp.write_all(&body).await?;
        tmp.sync_all().await?;
    }
    if let Err(err) = tokio::fs::rename(&tmp_path, path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> PluginRecord {
        PluginRecord {
            id: "demo".into(),
            name: "Demo".into(),
            version: None,
            description: None,
            author: None,
            homepage: None,
            source: Source::Command,
            launch: LaunchConfig {
                program: "node".into(),
                args: vec!["server.mjs".into()],
                cwd: None,
                env: BTreeMap::new(),
            },
            secrets: Vec::new(),
            enabled: true,
            approved_fingerprint: Some("abc".into()),
            tools: BTreeMap::from([(
                "echo".to_owned(),
                ToolChoice {
                    enabled: true,
                    risk: RiskLevel::Low,
                    definition: Some("hash".into()),
                },
            )]),
            added_at_ms: 1,
        }
    }

    #[tokio::test]
    async fn the_store_round_trips_and_keeps_invalid_files() {
        let Ok(dir) = tempfile::tempdir() else { return };
        let path = dir.path().join("config").join("plugins.json");
        let empty = load(&path).await.ok();
        assert_eq!(empty.map(|l| l.plugins.len()), Some(0));

        assert!(save(&path, &[record()]).await.is_ok());
        let loaded = load(&path).await.ok();
        assert_eq!(loaded.map(|l| l.plugins), Some(vec![record()]));

        std::fs::write(&path, b"{ not json").ok();
        let moved = load(&path).await.ok().and_then(|l| l.moved_to);
        let backup = moved.map(|(path, _)| path);
        assert!(backup.as_ref().is_some_and(|b| b.exists()));
        assert!(!path.exists());
    }
}
