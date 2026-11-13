//! Mentor mode: JARVIS teaches the user by voice while they build a project
//! (see docs/superpowers/specs/2026-11-13-mentor-mode-design.md).

// The parts are wired into the session step by step (plan tasks 4-9).
#![allow(dead_code)]

pub mod commands;
pub(crate) mod prompt;
pub(crate) mod protocol;
pub(crate) mod snapshot;
mod typing;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jarvis_ai::{DataLocality, ProviderId};
use jarvis_events::EventCategory;
use jarvis_memory::ConversationPatch;
use jarvis_security::FileScope;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;
use uuid::Uuid;

use crate::{CoreError, JarvisCore};
use snapshot::ProjectSnapshot;

/// What the mentor is doing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub enum MentorPhase {
    #[default]
    Off,
    /// The microphone listens; every phrase goes to the mentor.
    Listening,
    /// The session is kept; the microphone is closed.
    Paused,
    /// JARVIS types an approved change into a project file.
    Typing,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct MentorStatus {
    pub conversation_id: Option<Uuid>,
    pub project_folder: Option<String>,
    pub phase: MentorPhase,
    /// The change waiting for the user's approval, in one line.
    pub pending_write: Option<String>,
    /// The file JARVIS is typing into.
    pub typing_file: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct MentorStartRequest {
    pub conversation_id: Uuid,
    /// The project folder: a workspace folder or a folder inside one.
    pub project_folder: String,
    /// The model to teach with; without them the chat's model is used.
    pub provider_id: Option<ProviderId>,
    pub model: Option<String>,
}

/// The running session.
#[derive(Debug)]
pub(crate) struct Session {
    pub(crate) conversation_id: Uuid,
    /// Canonical project folder.
    pub(crate) root: PathBuf,
    pub(crate) provider_id: Option<ProviderId>,
    pub(crate) model: Option<String>,
    /// The project as the mentor last saw it.
    pub(crate) baseline: ProjectSnapshot,
    /// The first turn also gets the project's file tree.
    pub(crate) tree_sent: bool,
    /// Stops the session's background work.
    pub(crate) cancel: CancellationToken,
}

/// Why a chat turn goes to the mentor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub enum MentorTrigger {
    /// The user said (or typed) something.
    Phrase,
    /// The user saved project files; the turn has no text of its own.
    FilesSaved,
}

/// What a mentor turn adds for the model and saves instead of the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnContext {
    /// Appended to the user's message sent to the model (not saved).
    pub(crate) extra: String,
    /// Saved as the user's message of a turn without text.
    pub(crate) note: Option<String>,
}

/// A finished reply of a mentor turn, for the blocks it carries.
#[derive(Debug)]
pub(crate) struct FinishedReply {
    pub(crate) conversation_id: Uuid,
    pub(crate) text: String,
}

/// Files listed for the mentor on the first turn.
const TREE_ENTRIES: usize = 300;
/// Share of the model's context window for changed files (percent).
const CHANGES_BUDGET_PERCENT: usize = 30;
/// Characters per token, roughly.
const CHARS_PER_TOKEN: usize = 3;
/// Window assumed when the model does not report one.
const DEFAULT_WINDOW_TOKENS: u32 = 4_096;

#[derive(Debug)]
pub(crate) struct MentorState {
    status: watch::Sender<MentorStatus>,
    session: Mutex<Option<Session>>,
    replies: mpsc::UnboundedSender<FinishedReply>,
    /// Taken by the task that handles finished replies.
    replies_rx: Mutex<Option<mpsc::UnboundedReceiver<FinishedReply>>>,
    /// Stops the typing in progress.
    typing: Mutex<Option<CancellationToken>>,
}

impl Default for MentorState {
    fn default() -> Self {
        let (replies, replies_rx) = mpsc::unbounded_channel();
        Self {
            status: watch::Sender::new(MentorStatus::default()),
            session: Mutex::default(),
            replies,
            replies_rx: Mutex::new(Some(replies_rx)),
            typing: Mutex::default(),
        }
    }
}

impl MentorState {
    pub(crate) fn session(&self) -> MutexGuard<'_, Option<Session>> {
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn update(&self, change: impl FnOnce(&mut MentorStatus)) {
        self.status.send_modify(change);
    }

    pub(crate) fn status(&self) -> MentorStatus {
        self.status.borrow().clone()
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<MentorStatus> {
        self.status.subscribe()
    }

    /// Hands a saved reply of a mentor turn to the reply task.
    pub(crate) fn reply_finished(&self, conversation_id: Uuid, text: String) {
        let _ = self.replies.send(FinishedReply {
            conversation_id,
            text,
        });
    }

    pub(crate) fn typing(&self) -> MutexGuard<'_, Option<CancellationToken>> {
        self.typing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn reply_sender(&self) -> mpsc::UnboundedSender<FinishedReply> {
        self.replies.clone()
    }

    /// The model the running session for this conversation uses.
    pub(crate) fn model_for(&self, conversation_id: Uuid) -> Option<(ProviderId, String)> {
        let session = self.session();
        let session = session
            .as_ref()
            .filter(|s| s.conversation_id == conversation_id)?;
        Some((session.provider_id.clone()?, session.model.clone()?))
    }

    /// Ends the session, if any; returns whether one was running.
    fn end(&self) -> bool {
        let ended = self.session().take();
        if let Some(session) = &ended {
            session.cancel.cancel();
        }
        self.status.send_replace(MentorStatus::default());
        ended.is_some()
    }
}

impl JarvisCore {
    /// Starts a lesson in a conversation: checks the folder and the
    /// permissions, remembers the project and opens the microphone for a
    /// conversation. A running session is replaced.
    pub async fn mentor_start(
        &self,
        request: MentorStartRequest,
    ) -> Result<MentorStatus, CoreError> {
        let settings = self.config.current();
        self.memory.conversation(request.conversation_id).await?;
        let root = self.mentor_project_root(&request.project_folder, &settings)?;

        let model = self
            .chat
            .resolve_model(
                request.provider_id.as_ref(),
                request.model.as_deref(),
                &settings,
            )
            .await?;
        if model.descriptor.locality == DataLocality::ThirdPartyCloud
            && !settings.privacy.allow_cloud_agents
        {
            return Err(CoreError::PrivacyBlocked(format!(
                "The mentor sends your project files to \"{}\". Turn on \"Let agents use cloud AI providers\" in Settings → Privacy, or choose a local model",
                model.descriptor.display_name
            )));
        }

        self.mentor.end();
        self.voice.stop_conversation();
        let baseline = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || ProjectSnapshot::scan(&root))
                .await
                .map_err(|err| CoreError::Internal(err.to_string()))?
                .map_err(|err| {
                    CoreError::Validation(format!("The project folder cannot be read: {err}"))
                })?
        };
        self.voice.start_conversation()?;
        self.memory
            .set_mentor_session(request.conversation_id, &root)
            .await?;

        let shown = root.display().to_string();
        *self.mentor.session() = Some(Session {
            conversation_id: request.conversation_id,
            root,
            provider_id: Some(model.descriptor.id.clone()),
            model: Some(model.model.clone()),
            baseline,
            tree_sent: false,
            cancel: CancellationToken::new(),
        });
        let text_only = (!self.voice.speaks_replies()).then(|| {
            "No voice is installed or replies are not read aloud: the mentor answers in text only (Settings → Voice)".to_owned()
        });
        self.mentor.update(|status| {
            *status = MentorStatus {
                conversation_id: Some(request.conversation_id),
                project_folder: Some(shown.clone()),
                phase: MentorPhase::Listening,
                pending_write: None,
                typing_file: None,
                last_error: text_only,
            };
        });
        self.events.info(
            EventCategory::Voice,
            format!("Mentor started for {shown}; the microphone listens"),
        );
        Ok(self.mentor.status())
    }

    /// Closes the microphone; the session stays.
    pub fn mentor_pause(&self) {
        if self.mentor.session().is_none() {
            return;
        }
        self.voice.stop_conversation();
        self.mentor.update(|s| s.phase = MentorPhase::Paused);
        self.events
            .info(EventCategory::Voice, "Mentor paused; the microphone is off");
    }

    /// Listens again after a pause.
    pub fn mentor_resume(&self) -> Result<MentorStatus, CoreError> {
        if self.mentor.session().is_none() {
            return Err(CoreError::Validation("No lesson is running".to_owned()));
        }
        self.voice.start_conversation()?;
        self.mentor.update(|s| {
            if s.phase == MentorPhase::Paused {
                s.phase = MentorPhase::Listening;
            }
        });
        self.events.info(
            EventCategory::Voice,
            "Mentor resumed; the microphone listens",
        );
        Ok(self.mentor.status())
    }

    /// Ends the lesson and closes the microphone.
    pub fn mentor_stop(&self) {
        if self.mentor.end() {
            self.voice.stop_conversation();
            self.events.info(
                EventCategory::Voice,
                "Mentor stopped; the microphone is off",
            );
        }
    }

    pub fn mentor_status(&self) -> MentorStatus {
        self.mentor.status()
    }

    pub fn subscribe_mentor(&self) -> watch::Receiver<MentorStatus> {
        self.mentor.subscribe()
    }

    /// The project context of a mentor turn in the running session's
    /// conversation, or `None` for any other turn. The turn's changes become
    /// the new baseline.
    pub(crate) async fn mentor_turn_context(
        &self,
        conversation_id: Uuid,
        trigger: MentorTrigger,
        window_tokens: Option<u32>,
    ) -> Result<Option<TurnContext>, CoreError> {
        let (root, baseline, tree_sent) = {
            let session = self.mentor.session();
            match session.as_ref() {
                Some(s) if s.conversation_id == conversation_id => {
                    (s.root.clone(), s.baseline.clone(), s.tree_sent)
                }
                _ => return Ok(None),
            }
        };
        let window = window_tokens.unwrap_or(DEFAULT_WINDOW_TOKENS) as usize;
        let budget = window * CHARS_PER_TOKEN * CHANGES_BUDGET_PERCENT / 100;
        let scan_root = root.clone();
        let (now, changes, rendered, tree) = tokio::task::spawn_blocking(move || {
            let now = ProjectSnapshot::scan(&scan_root)?;
            let changes = now.changes_since(&baseline);
            let rendered = (!changes.is_empty()).then(|| changes.render(&scan_root, budget));
            let tree = (!tree_sent).then(|| now.tree(TREE_ENTRIES));
            Ok::<_, std::io::Error>((now, changes, rendered, tree))
        })
        .await
        .map_err(|err| CoreError::Internal(err.to_string()))?
        .map_err(|err| {
            CoreError::Validation(format!("The project folder cannot be read: {err}"))
        })?;
        {
            let mut session = self.mentor.session();
            if let Some(s) = session
                .as_mut()
                .filter(|s| s.conversation_id == conversation_id)
            {
                s.baseline = now;
                s.tree_sent = true;
            }
        }

        let mut extra = String::new();
        if let Some(tree) = tree {
            extra.push_str(&format!(
                "\n\n<project_files>\nThe project folder ({}) has these files:\n{tree}</project_files>",
                root.display()
            ));
        }
        if let Some(rendered) = rendered {
            extra.push_str("\n\n");
            extra.push_str(&rendered);
        }
        let note = match trigger {
            MentorTrigger::Phrase => None,
            MentorTrigger::FilesSaved => {
                extra.push_str("\n\n");
                extra.push_str(prompt::FILES_SAVED_PROMPT);
                let names: Vec<String> = changes
                    .created
                    .iter()
                    .chain(&changes.changed)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .collect();
                Some(if names.is_empty() {
                    "[Saved files]".to_owned()
                } else {
                    format!("[Saved: {}]", names.join(", "))
                })
            }
        };
        Ok(Some(TurnContext { extra, note }))
    }

    /// Handles the blocks of finished mentor replies until the core stops.
    pub(crate) fn spawn_mentor_tasks(core: &Arc<Self>) {
        let Some(mut replies) = core
            .mentor
            .replies_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let weak = Arc::downgrade(core);
        tokio::spawn(async move {
            while let Some(reply) = replies.recv().await {
                let Some(core) = weak.upgrade() else { return };
                core.mentor_handle_reply(&core, reply).await;
            }
        });
    }

    async fn mentor_handle_reply(&self, core: &Arc<Self>, reply: FinishedReply) {
        let parsed = protocol::parse_reply(&reply.text);
        if let Some(plan) = parsed.plan {
            let patch = ConversationPatch {
                pinned_note: Some(plan),
                ..ConversationPatch::default()
            };
            if let Err(err) = self
                .memory
                .update_conversation(reply.conversation_id, patch)
                .await
            {
                tracing::warn!("cannot save the mentor's plan: {err}");
            }
        }
        if let Some(write) = parsed.write {
            // Typing takes a while; later replies are handled meanwhile.
            let core = Arc::clone(core);
            tokio::spawn(async move {
                core.mentor_write(reply.conversation_id, write).await;
            });
        }
    }

    /// Ends the session of a deleted conversation.
    pub(crate) fn mentor_conversation_deleted(&self, conversation_id: Uuid) {
        let running = self
            .mentor
            .session()
            .as_ref()
            .is_some_and(|s| s.conversation_id == conversation_id);
        if running {
            self.mentor_stop();
        }
    }

    /// The canonical project folder, which must be inside a workspace folder
    /// and outside JARVIS's own folders.
    fn mentor_project_root(
        &self,
        folder: &str,
        settings: &crate::Settings,
    ) -> Result<PathBuf, CoreError> {
        let path = PathBuf::from(folder.trim());
        if !path.is_dir() {
            return Err(CoreError::Validation(format!(
                "The project folder \"{folder}\" does not exist"
            )));
        }
        let scope = FileScope::new(
            settings.tools.workspace_folders.iter().map(PathBuf::from),
            [
                self.paths.config_dir.clone(),
                self.paths.data_dir.clone(),
                self.paths.log_dir.clone(),
            ],
        );
        scope.ensure_inside(&path).map_err(|err| {
            CoreError::Validation(format!(
                "The project folder must be in a workspace folder (Settings → Tools): {err}"
            ))
        })
    }
}
