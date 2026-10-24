//! The plugin manifest (`jarvis-plugin.json`, version 1) and the checks
//! shared with MCP servers entered by the user.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::{PluginError, Result};
use crate::types::SecretRequirement;

pub const MANIFEST_FILE: &str = "jarvis-plugin.json";
pub const MANIFEST_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_ID: usize = 32;
const MAX_NAME: usize = 64;
const MAX_TEXT: usize = 500;
const MAX_ARGS: usize = 64;
const MAX_ARG: usize = 4096;
const MAX_ENV: usize = 64;
const MAX_ENV_VALUE: usize = 4096;
const MAX_SECRETS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub manifest_version: u32,
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
    pub server: ManifestServer,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestServer {
    /// A program name found in `PATH`, or a path relative to the plugin
    /// folder.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<SecretRequirement>,
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(PluginError::Invalid(format!(
                "{MANIFEST_FILE} is larger than {} KB",
                MAX_MANIFEST_BYTES / 1024
            )));
        }
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|err| PluginError::Invalid(format!("{MANIFEST_FILE}: {err}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if self.manifest_version != MANIFEST_VERSION {
            return Err(PluginError::Invalid(format!(
                "manifestVersion {} is not supported (expected {MANIFEST_VERSION})",
                self.manifest_version
            )));
        }
        check_id(&self.id)?;
        check_name(&self.name)?;
        for (field, value, max) in [
            ("version", &self.version, 32),
            ("description", &self.description, MAX_TEXT),
            ("author", &self.author, 100),
            ("homepage", &self.homepage, 200),
        ] {
            if let Some(value) = value {
                check_text(field, value, max)?;
            }
        }
        if let Some(homepage) = &self.homepage
            && !(homepage.starts_with("https://") || homepage.starts_with("http://"))
        {
            return Err(PluginError::Invalid(
                "homepage must be an http(s) URL".into(),
            ));
        }
        if self.server.command.trim().is_empty() || self.server.command.len() > 260 {
            return Err(PluginError::Invalid(
                "server.command is empty or too long".into(),
            ));
        }
        check_launch(
            &self.server.args,
            self.server.env.iter(),
            self.server.secrets.iter().map(|s| s.name.as_str()),
        )?;
        for secret in &self.server.secrets {
            if let Some(description) = &secret.description {
                check_text("secret description", description, MAX_TEXT)?;
            }
        }
        Ok(())
    }
}

/// `a-z`, `0-9` and `-`, 1–32 characters, not starting with `-`.
pub fn check_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && id.len() <= MAX_ID
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(PluginError::Invalid(format!(
            "id \"{id}\" must be 1–{MAX_ID} characters a-z, 0-9 or -"
        )))
    }
}

pub fn check_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(PluginError::Invalid("the name is empty".into()));
    }
    check_text("name", name, MAX_NAME)
}

fn check_text(field: &str, value: &str, max: usize) -> Result<()> {
    if value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(PluginError::Invalid(format!(
            "{field} must be at most {max} characters on one line"
        )));
    }
    Ok(())
}

/// An id derived from a name (for servers entered as commands).
pub fn id_from_name(name: &str) -> String {
    let mut id = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            id.push(c);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    let id: String = id.trim_end_matches('-').chars().take(MAX_ID).collect();
    let id = id.trim_end_matches('-').to_owned();
    if id.is_empty() {
        "mcp-server".to_owned()
    } else {
        id
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 128
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Arguments, variables and secret names of a server.
pub fn check_launch<'a>(
    args: &[String],
    env: impl Iterator<Item = (&'a String, &'a String)>,
    secrets: impl Iterator<Item = &'a str>,
) -> Result<()> {
    if args.len() > MAX_ARGS {
        return Err(PluginError::Invalid(format!(
            "at most {MAX_ARGS} arguments"
        )));
    }
    if args.iter().any(|a| a.len() > MAX_ARG || a.contains('\0')) {
        return Err(PluginError::Invalid(
            "an argument is too long or invalid".into(),
        ));
    }
    let mut names = BTreeMap::new();
    for (name, value) in env {
        if !valid_env_name(name) {
            return Err(PluginError::Invalid(format!(
                "\"{name}\" is not a valid environment variable name"
            )));
        }
        if value.len() > MAX_ENV_VALUE || value.contains('\0') {
            return Err(PluginError::Invalid(format!(
                "the value of {name} is too long"
            )));
        }
        names.insert(name.to_uppercase(), ());
    }
    let mut secret_count = 0;
    for name in secrets {
        secret_count += 1;
        if !valid_env_name(name) {
            return Err(PluginError::Invalid(format!(
                "\"{name}\" is not a valid environment variable name"
            )));
        }
        if names.insert(name.to_uppercase(), ()).is_some() {
            return Err(PluginError::Invalid(format!("{name} is set twice")));
        }
    }
    if names.len() > MAX_ENV || secret_count > MAX_SECRETS {
        return Err(PluginError::Invalid(format!(
            "at most {MAX_ENV} variables and {MAX_SECRETS} secrets"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(server: &str) -> String {
        format!(
            r#"{{"manifestVersion": 1, "id": "text-tools", "name": "Text tools", "server": {server}}}"#
        )
    }

    #[test]
    fn valid_manifests_parse() {
        let parsed = Manifest::parse(
            manifest(
                r#"{"command": "node", "args": ["server.mjs"], "env": {"LOCALE": "en"},
                    "secrets": [{"name": "API_KEY", "description": "Key"}]}"#,
            )
            .as_bytes(),
        );
        let parsed = parsed.ok();
        assert_eq!(parsed.as_ref().map(|m| m.id.as_str()), Some("text-tools"));
        assert_eq!(parsed.map(|m| m.server.secrets.len()), Some(1));
    }

    #[test]
    fn invalid_manifests_are_refused() {
        for server in [
            r#"{"command": ""}"#,
            r#"{"command": "node", "extra": 1}"#,
            r#"{"command": "node", "env": {"1BAD": "x"}}"#,
            r#"{"command": "node", "env": {"KEY": "x"}, "secrets": [{"name": "KEY"}]}"#,
        ] {
            assert!(
                Manifest::parse(manifest(server).as_bytes()).is_err(),
                "{server}"
            );
        }
        let wrong_id = r#"{"manifestVersion": 1, "id": "Text Tools", "name": "x", "server": {"command": "node"}}"#;
        assert!(Manifest::parse(wrong_id.as_bytes()).is_err());
        let wrong_version =
            r#"{"manifestVersion": 2, "id": "x", "name": "x", "server": {"command": "node"}}"#;
        assert!(Manifest::parse(wrong_version.as_bytes()).is_err());
    }

    #[test]
    fn ids_come_from_names() {
        assert_eq!(id_from_name("GitHub MCP (official)"), "github-mcp-official");
        assert_eq!(id_from_name("  ---  "), "mcp-server");
        assert_eq!(id_from_name("Файли"), "mcp-server");
        assert!(id_from_name(&"long name ".repeat(10)).len() <= 32);
    }
}
