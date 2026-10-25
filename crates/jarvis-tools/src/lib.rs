//! Tools JARVIS agents can use, behind one interface.
//!
//! A call has two phases: [`Tool::prepare`] validates the input, resolves
//! paths inside the workspace and describes the concrete action
//! ([`ActionRequest`]) **without side effects**; the Security Policy Engine
//! decides on that description; only then [`Tool::run`] executes the
//! prepared call. Tools re-check paths right before using them.
//!
//! Tools run with the user's rights; there is no sandbox. Tools from
//! plugins (MCP servers) join the registry in groups at run time; they
//! implement the same trait and are subject to the same policy.

mod command;
mod files;
mod git;
mod system;
mod util;
mod web;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use jarvis_security::{ActionRequest, Capability, FileScope, RiskLevel, ScopeError};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

pub use command::resolve_program;
pub use system::ProcessRegistry;
pub use web::html_to_text;

/// Description of a tool for the model and the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "tools.ts")]
pub struct ToolSpec {
    /// Stable identifier, e.g. `read_file`.
    pub name: String,
    pub title: String,
    /// What the tool does, written for the model.
    pub description: String,
    /// JSON Schema of the input object.
    #[ts(type = "unknown")]
    pub parameters: serde_json::Value,
    pub capabilities: Vec<Capability>,
    /// Usual risk; a call may be riskier (e.g. overwriting a file).
    pub risk: RiskLevel,
    /// Running it twice does the same as once (reads).
    pub idempotent: bool,
    /// Name of the plugin that provides the tool; `None` for built-in tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin: Option<String>,
}

/// A validated call, ready for the policy and then for [`Tool::run`].
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCall {
    pub action: ActionRequest,
    /// Normalized arguments (resolved paths), private to the tool.
    pub args: serde_json::Value,
}

/// Result of a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// One line for the UI.
    pub summary: String,
    /// Text for the model (already truncated).
    pub content: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error(transparent)]
    Scope(#[from] ScopeError),
    #[error("{0}")]
    Failed(String),
    #[error("the tool did not finish within {0} s")]
    TimedOut(u64),
    #[error("cancelled")]
    Cancelled,
    #[error("I/O error while {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl ToolError {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// Bounds for tool work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolLimits {
    pub command_timeout: Duration,
    /// Characters of output given to the model.
    pub max_output_chars: usize,
    pub max_read_bytes: u64,
    pub max_write_bytes: usize,
    pub max_listed: usize,
    pub max_scanned_files: usize,
    pub max_fetch_bytes: usize,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            command_timeout: Duration::from_secs(60),
            max_output_chars: 16_000,
            max_read_bytes: 512 * 1024,
            max_write_bytes: 1024 * 1024,
            max_listed: 500,
            max_scanned_files: 5_000,
            max_fetch_bytes: 2 * 1024 * 1024,
        }
    }
}

/// What a tool may use while it runs.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub scope: FileScope,
    pub limits: ToolLimits,
    pub cancel: CancellationToken,
    pub processes: Arc<ProcessRegistry>,
}

impl ToolContext {
    pub fn new(scope: FileScope, limits: ToolLimits, processes: Arc<ProcessRegistry>) -> Self {
        Self {
            scope,
            limits,
            cancel: CancellationToken::new(),
            processes,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync + std::fmt::Debug {
    fn spec(&self) -> &ToolSpec;

    /// Validates the input and describes the action. No side effects.
    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError>;

    /// Executes a call that the policy accepted.
    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError>;
}

/// The tools JARVIS knows, by name: built-in tools, fixed at start, and
/// groups of tools added and replaced at run time (one group per plugin).
#[derive(Debug, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    groups: RwLock<ToolGroups>,
}

/// Tools by group, then by name.
type ToolGroups = BTreeMap<String, BTreeMap<String, Arc<dyn Tool>>>;

impl ToolRegistry {
    /// All built-in tools.
    pub fn builtin() -> Self {
        let mut registry = Self::default();
        for tool in files::tools()
            .into_iter()
            .chain(system::tools())
            .chain(command::tools())
            .chain(web::tools())
            .chain(git::tools())
        {
            registry.register(tool);
        }
        registry
    }

    /// Adds a tool; a tool with the same name is replaced.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name.clone(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some(tool) = self.tools.get(name) {
            return Some(Arc::clone(tool));
        }
        self.groups()
            .values()
            .find_map(|group| group.get(name).cloned())
    }

    /// Built-in tools, then the tools of every group.
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = self.builtin_specs();
        specs.extend(
            self.groups()
                .values()
                .flat_map(|group| group.values().map(|t| t.spec().clone())),
        );
        specs
    }

    pub fn builtin_specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec().clone()).collect()
    }

    /// Names of the tools in groups (all of them are turned on: a group
    /// holds only the tools the user enabled).
    pub fn group_tool_names(&self) -> BTreeSet<String> {
        self.groups()
            .values()
            .flat_map(|group| group.keys().cloned())
            .collect()
    }

    /// Replaces the tools of a group. Fails, changing nothing, when a name
    /// is used by a built-in tool, by another group or twice.
    pub fn set_group(&self, group: &str, tools: Vec<Arc<dyn Tool>>) -> Result<(), String> {
        let mut groups = self.groups.write().unwrap_or_else(PoisonError::into_inner);
        let mut named = BTreeMap::new();
        for tool in tools {
            let name = tool.spec().name.clone();
            let taken = self.tools.contains_key(&name)
                || groups
                    .iter()
                    .any(|(other, tools)| other != group && tools.contains_key(&name));
            if taken || named.contains_key(&name) {
                return Err(format!("the tool name \"{name}\" is already used"));
            }
            named.insert(name, tool);
        }
        groups.insert(group.to_owned(), named);
        Ok(())
    }

    pub fn remove_group(&self, group: &str) {
        self.groups
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(group);
    }

    fn groups(&self) -> std::sync::RwLockReadGuard<'_, ToolGroups> {
        self.groups.read().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Tools that only read, enabled by default.
pub const DEFAULT_ENABLED: &[&str] = &[
    "list_directory",
    "read_file",
    "search_files",
    "system_info",
    "list_processes",
    "git",
];
