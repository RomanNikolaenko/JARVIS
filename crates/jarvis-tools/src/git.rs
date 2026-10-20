//! Read-only git: status, log, diff, show and branches. Repository
//! configuration cannot run code: fsmonitor, hooks, external diff drivers,
//! text conversion and pagers are switched off for every call, and optional
//! index locks are disabled so `status` does not write.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jarvis_security::{ActionRequest, Capability, RiskLevel};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::command::{base_command, capture, resolve_program};
use crate::util::{parse, to_args, truncate};
use crate::{PreparedCall, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};

const GIT_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(Git::new())]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum GitCommand {
    Status,
    Log,
    Diff,
    Show,
    Branch,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitArgs {
    command: GitCommand,
    /// Repository folder.
    #[serde(default)]
    repo: Option<String>,
    /// `log`: number of commits.
    #[serde(default)]
    limit: Option<u32>,
    /// `diff`: limit to this path; `show`: the revision.
    #[serde(default)]
    target: Option<String>,
    /// `diff`: staged changes.
    #[serde(default)]
    staged: bool,
}

#[derive(Debug)]
struct Git(ToolSpec);

impl Git {
    fn new() -> Self {
        Self(ToolSpec {
            name: "git".to_owned(),
            title: "Git (read-only)".to_owned(),
            description: "Reads a git repository in the workspace: `status`, `log` (`limit`), `diff` (`target` path, `staged`), `show` (`target` revision, e.g. HEAD~1) or `branch`. Never changes the repository.".to_owned(),
            parameters: json!({"type": "object", "required": ["command"], "properties": {
                "command": {"type": "string", "enum": ["status", "log", "diff", "show", "branch"]},
                "repo": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                "target": {"type": "string"},
                "staged": {"type": "boolean"}
            }}),
            capabilities: vec![Capability::FsRead],
            risk: RiskLevel::Low,
            idempotent: true,
        })
    }
}

/// Revisions: letters, digits and `._/~^@{}-`, not starting with `-`.
fn valid_revision(revision: &str) -> bool {
    !revision.is_empty()
        && revision.len() <= 200
        && !revision.starts_with('-')
        && revision
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/~^@{}-".contains(c))
}

fn git_args(args: &GitArgs, repo: &Path) -> Result<Vec<String>, ToolError> {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut command: Vec<String> = [
        "-c",
        "core.fsmonitor=false",
        "-c",
        &format!("core.hooksPath={null}"),
        "-c",
        "core.pager=cat",
        "-c",
        "diff.external=",
        "-c",
        "credential.helper=",
        "--no-pager",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    match args.command {
        GitCommand::Status => command.extend(["status", "--short", "--branch"].map(String::from)),
        GitCommand::Log => {
            let limit = args.limit.unwrap_or(20).clamp(1, 100);
            command.extend(["log", "--oneline", "--decorate", "-n"].map(String::from));
            command.push(limit.to_string());
        }
        GitCommand::Diff => {
            command.extend(
                [
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--stat",
                    "--patch",
                ]
                .map(String::from),
            );
            if args.staged {
                command.push("--cached".to_owned());
            }
            if let Some(target) = &args.target {
                let path = Path::new(target);
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    repo.join(path)
                };
                command.push("--".to_owned());
                command.push(path.display().to_string());
            }
        }
        GitCommand::Show => {
            let revision = args.target.as_deref().unwrap_or("HEAD");
            if !valid_revision(revision) {
                return Err(ToolError::InvalidInput(format!(
                    "invalid revision \"{revision}\""
                )));
            }
            command.extend(
                ["show", "--stat", "--no-ext-diff", "--no-textconv", revision].map(String::from),
            );
        }
        GitCommand::Branch => command.extend(["branch", "--list", "-vv"].map(String::from)),
    }
    Ok(command)
}

#[async_trait]
impl Tool for Git {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let mut args: GitArgs = parse(input)?;
        let repo = ctx.scope.resolve(args.repo.as_deref().unwrap_or("."))?;
        let mut paths = vec![repo.clone()];
        if args.command == GitCommand::Diff
            && let Some(target) = &args.target
        {
            paths.push(
                ctx.scope
                    .resolve(&repo.join(target).display().to_string())?,
            );
        }
        // Validates arguments early (e.g. revisions).
        git_args(&args, &repo)?;
        let summary = format!("git {:?} in {}", args.command, repo.display()).to_lowercase();
        args.repo = Some(repo.display().to_string());
        Ok(PreparedCall {
            action: ActionRequest {
                tool: self.0.name.clone(),
                capabilities: self.0.capabilities.clone(),
                risk: RiskLevel::Low,
                summary,
                paths,
                program: None,
                url: None,
            },
            args: to_args(&args)?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: GitArgs = parse(&call.args)?;
        let repo = ctx
            .scope
            .ensure_inside(Path::new(args.repo.as_deref().unwrap_or_default()))?;
        let git = resolve_program("git")
            .ok_or_else(|| ToolError::Failed("git was not found in PATH".to_owned()))?;
        let mut command = base_command(&git, &git_args(&args, &repo)?, &repo);
        command
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        let captured = capture(command, GIT_TIMEOUT, ctx).await?;
        if !captured.status.success() {
            return Err(ToolError::Failed(truncate(
                captured.stderr.trim(),
                ctx.limits.max_output_chars,
            )));
        }
        let output = if captured.stdout.trim().is_empty() {
            "(no output)".to_owned()
        } else {
            captured.stdout
        };
        Ok(ToolOutput {
            summary: format!("git {:?} in {}", args.command, repo.display()).to_lowercase(),
            content: truncate(&output, ctx.limits.max_output_chars),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_cannot_be_options() {
        assert!(valid_revision("HEAD~1"));
        assert!(valid_revision("main@{1}"));
        assert!(valid_revision("v1.2.3"));
        assert!(!valid_revision("--output=/tmp/x"));
        assert!(!valid_revision("HEAD; rm -rf /"));
        assert!(!valid_revision(""));
    }
}
