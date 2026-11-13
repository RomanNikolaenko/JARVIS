//! Typing an approved change into a project file slowly, in step with the
//! spoken explanation, so the user's editor shows it appear.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jarvis_memory::{MessageRole, MessageStatus, NewMessage};
use jarvis_security::{
    ApprovalOutcome, ApprovalRequest, AuditEvent, FileScope, NewAuditEntry, PolicyDecision,
    RiskLevel,
};
use jarvis_tools::ToolContext;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::MentorPhase;
use super::protocol::{WriteBlock, WriteChange};
use crate::JarvisCore;
use crate::config::AssistantLanguage;

/// Time between two writes to the file.
const STEP: Duration = Duration::from_millis(150);
/// Lines typed in one write.
const LINES_PER_STEP: usize = 3;

/// How a typing run ended, for the status and the conversation.
enum Ended {
    Done,
    Stopped(String),
}

impl JarvisCore {
    /// Stops typing; what was typed stays in the file.
    pub fn mentor_stop_typing(&self) {
        if let Some(cancel) = self.mentor.typing().take() {
            cancel.cancel();
        }
    }

    /// Checks a write block from the mentor, asks the user and types it.
    pub(crate) async fn mentor_write(
        self: &Arc<Self>,
        conversation_id: Uuid,
        block: Result<WriteBlock, String>,
    ) {
        let (root, session_cancel) = {
            let session = self.mentor.session();
            match session
                .as_ref()
                .filter(|s| s.conversation_id == conversation_id)
            {
                Some(s) => (s.root.clone(), s.cancel.clone()),
                None => return,
            }
        };
        let block = match block {
            Ok(block) => block,
            Err(reason) => {
                self.mentor_refuse(conversation_id, "the code", &reason)
                    .await;
                return;
            }
        };
        let shown = block.path.clone();
        match self.mentor_prepare_write(&root, &block) {
            Ok(write) => {
                self.mentor_approve_and_type(
                    conversation_id,
                    &root,
                    &block,
                    write,
                    &session_cancel,
                )
                .await;
            }
            Err(reason) => self.mentor_refuse(conversation_id, &shown, &reason).await,
        }
    }

    /// The change as a tool call, checked by the tool and the policy.
    fn mentor_prepare_write(
        &self,
        root: &Path,
        block: &WriteBlock,
    ) -> Result<PreparedWrite, String> {
        let path = root.join(&block.path);
        let original = if path.exists() {
            Some(
                std::fs::read_to_string(&path)
                    .map_err(|err| format!("the file cannot be read as text: {err}"))?,
            )
        } else {
            None
        };
        let code = block.code();
        let (tool_name, input, prefix, suffix) = match &block.change {
            WriteChange::Create => (
                "write_file",
                json!({"path": path, "content": code, "overwrite": original.is_some()}),
                String::new(),
                String::new(),
            ),
            WriteChange::Replace { find } => {
                let Some(original) = &original else {
                    return Err("the file does not exist".to_owned());
                };
                let count = original.matches(find.as_str()).count();
                if count != 1 {
                    return Err(format!(
                        "the fragment to replace occurs {count} times in the file, not once"
                    ));
                }
                let start = original.find(find.as_str()).unwrap_or_default();
                (
                    "edit_file",
                    json!({"path": path, "find": find, "replace": code}),
                    original[..start].to_owned(),
                    original[start + find.len()..].to_owned(),
                )
            }
        };

        let settings = self.config.current();
        let protected = [
            self.paths.config_dir.clone(),
            self.paths.data_dir.clone(),
            self.paths.log_dir.clone(),
        ];
        // The mentor is not an agent: the tool list in Settings → Tools is
        // its switch, whatever the agents setting says.
        let mut tool_settings = settings.clone();
        tool_settings.agents.enabled = true;
        let agent = crate::agents::agent_settings(&tool_settings, &protected, &BTreeSet::new());
        let scope = FileScope::new(
            settings.tools.workspace_folders.iter().map(PathBuf::from),
            protected.iter().cloned(),
        );
        let tool = self
            .agents
            .tools
            .get(tool_name)
            .ok_or_else(|| format!("the tool {tool_name} is missing"))?;
        let ctx = ToolContext::new(
            scope.clone(),
            agent.tool_limits,
            Arc::clone(&self.agents.processes),
        );
        let prepared = tool.prepare(&input, &ctx).map_err(|err| err.to_string())?;
        if let PolicyDecision::Deny { reason } = agent.policy.evaluate(&prepared.action) {
            return Err(format!(
                "{reason} (turn on \"{tool_name}\" in Settings → Tools to let the mentor write)"
            ));
        }
        let details = match &block.change {
            WriteChange::Create => format!("{}\n\n{code}", prepared.action.summary),
            WriteChange::Replace { find } => format!(
                "{}\n\nReplace:\n{find}\nWith:\n{code}",
                prepared.action.summary
            ),
        };
        Ok(PreparedWrite {
            path,
            scope,
            original,
            prefix,
            suffix,
            tool: tool_name,
            risk: prepared.action.risk,
            summary: prepared.action.summary,
            details,
            timeout: agent.approval_timeout,
        })
    }

    async fn mentor_approve_and_type(
        self: &Arc<Self>,
        conversation_id: Uuid,
        root: &Path,
        block: &WriteBlock,
        write: PreparedWrite,
        session_cancel: &CancellationToken,
    ) {
        self.mentor
            .update(|s| s.pending_write = Some(write.summary.clone()));
        let outcome = self
            .agents
            .approvals
            .request(
                ApprovalRequest {
                    task_id: None,
                    tool: write.tool.to_owned(),
                    risk: write.risk,
                    summary: write.summary.clone(),
                    reason: "The mentor wants to type this change into your project".to_owned(),
                    details: write.details.clone(),
                },
                write.timeout,
                session_cancel,
            )
            .await;
        self.mentor.update(|s| s.pending_write = None);
        let event = match outcome {
            ApprovalOutcome::Approved => AuditEvent::Approved,
            ApprovalOutcome::Denied => AuditEvent::Denied,
            ApprovalOutcome::TimedOut => AuditEvent::TimedOut,
            ApprovalOutcome::Cancelled => AuditEvent::Cancelled,
        };
        self.mentor_audit(&write, event, "mentor").await;
        if outcome != ApprovalOutcome::Approved {
            self.mentor_note(
                conversation_id,
                format!(
                    "[The change to {} was not written: not approved]",
                    block.path
                ),
            )
            .await;
            return;
        }

        let typing = session_cancel.child_token();
        *self.mentor.typing() = Some(typing.clone());
        let before = self.mentor.status().phase;
        self.mentor.update(|s| {
            s.phase = MentorPhase::Typing;
            s.typing_file = Some(block.path.clone());
            s.last_error = None;
        });
        let ended = self.mentor_type(root, block, &write, &typing).await;
        self.mentor.typing().take();
        let (event, detail) = match &ended {
            Ended::Done => (AuditEvent::Succeeded, "typed".to_owned()),
            Ended::Stopped(reason) => (AuditEvent::Failed, reason.clone()),
        };
        self.mentor_audit(&write, event, &detail).await;
        self.mentor.update(|s| {
            if s.phase == MentorPhase::Typing {
                s.phase = if before == MentorPhase::Paused {
                    MentorPhase::Paused
                } else {
                    MentorPhase::Listening
                };
            }
            s.typing_file = None;
            s.last_error = match &ended {
                Ended::Done => None,
                Ended::Stopped(reason) => {
                    Some(format!("Typing into {} stopped: {reason}", block.path))
                }
            };
        });
        if let Ended::Stopped(reason) = ended {
            self.mentor_note(
                conversation_id,
                format!(
                    "[Typing into {} stopped before the end: {reason}]",
                    block.path
                ),
            )
            .await;
        }
    }

    async fn mentor_type(
        &self,
        root: &Path,
        block: &WriteBlock,
        write: &PreparedWrite,
        cancel: &CancellationToken,
    ) -> Ended {
        let speaks = self.voice.speaks_replies();
        if speaks {
            let ask = match self.config.current().general.assistant_language {
                AssistantLanguage::Uk => "Збережи файл і не друкуй у ньому, поки я пишу.",
                AssistantLanguage::En => "Save the file and do not type in it while I write.",
            };
            tokio::select! {
                () = cancel.cancelled() => return Ended::Stopped("stopped by the user".into()),
                () = self.voice.say_and_wait(ask) => {}
            }
        }
        let relative = PathBuf::from(&block.path);
        let mut last = write.original.clone();
        let mut typed = String::new();
        for part in &block.parts {
            if speaks && !part.say.is_empty() {
                tokio::select! {
                    () = cancel.cancelled() => return Ended::Stopped("stopped by the user".into()),
                    () = self.voice.say_and_wait(&part.say) => {}
                }
            }
            let lines: Vec<&str> = part.code.split_inclusive('\n').collect();
            for chunk in lines.chunks(LINES_PER_STEP) {
                if cancel.is_cancelled() {
                    return Ended::Stopped("stopped by the user".into());
                }
                if let Err(reason) = check_unchanged(&write.path, last.as_deref()) {
                    return Ended::Stopped(reason);
                }
                typed.extend(chunk.iter().copied());
                let content = format!("{}{typed}{}", write.prefix, write.suffix);
                if let Err(reason) = write_file(&write.scope, &write.path, &content) {
                    return Ended::Stopped(reason);
                }
                last = Some(content);
                if let Some(session) = self.mentor.session().as_mut() {
                    session.baseline.mark_written(root, &relative);
                    session.watch_baseline.mark_written(root, &relative);
                }
                tokio::select! {
                    () = cancel.cancelled() => return Ended::Stopped("stopped by the user".into()),
                    () = tokio::time::sleep(STEP) => {}
                }
            }
        }
        Ended::Done
    }

    async fn mentor_refuse(&self, conversation_id: Uuid, what: &str, reason: &str) {
        self.mentor.update(|s| {
            s.last_error = Some(format!("The change to {what} was not written: {reason}"));
        });
        self.mentor_note(
            conversation_id,
            format!("[The change to {what} was not written: {reason}]"),
        )
        .await;
    }

    /// Tells the mentor (and the user) what happened, as a short note in the
    /// conversation.
    async fn mentor_note(&self, conversation_id: Uuid, text: String) {
        let note = NewMessage {
            id: Uuid::new_v4(),
            role: MessageRole::Assistant,
            content: text,
            status: MessageStatus::Complete,
            provider_id: None,
            model: None,
        };
        if let Err(err) = self.memory.append_message(conversation_id, note).await {
            tracing::warn!("cannot save a mentor note: {err}");
        }
    }

    async fn mentor_audit(&self, write: &PreparedWrite, event: AuditEvent, detail: &str) {
        let entry = NewAuditEntry {
            task_id: None,
            tool: write.tool.to_owned(),
            action: write.summary.clone(),
            risk: write.risk,
            event,
            detail: detail.to_owned(),
        };
        if let Err(err) = self.agents.audit.record(entry).await {
            tracing::warn!("cannot audit a mentor write: {err}");
        }
    }
}

/// A checked change, ready for approval.
struct PreparedWrite {
    path: PathBuf,
    scope: FileScope,
    /// The file before the change (`None`: it does not exist).
    original: Option<String>,
    /// Text kept before and after the typed code.
    prefix: String,
    suffix: String,
    tool: &'static str,
    risk: RiskLevel,
    summary: String,
    details: String,
    timeout: Duration,
}

/// Fails when the file is not what JARVIS last left there: the user edited
/// it, and their text must not be overwritten.
fn check_unchanged(path: &Path, last: Option<&str>) -> Result<(), String> {
    let now = std::fs::read_to_string(path).ok();
    if now.as_deref() == last {
        Ok(())
    } else {
        Err("the file was changed while JARVIS was typing; your text was kept".to_owned())
    }
}

fn write_file(scope: &FileScope, path: &Path, content: &str) -> Result<(), String> {
    let path = scope.ensure_inside(path).map_err(|err| err.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create the folder: {err}"))?;
    }
    std::fs::write(&path, content).map_err(|err| format!("cannot write the file: {err}"))
}
