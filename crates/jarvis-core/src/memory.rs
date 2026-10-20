//! Memory integration of the core: conversation turns (context assembly,
//! persistence, compaction), embeddings for semantic search, the action
//! history (episodic memory) and the memory API used by the UI.
//!
//! Privacy rules:
//! - with saving off (`memory.enabled = false`) nothing is written: new chats
//!   are temporary, facts and the action history are not recorded;
//! - saved memory (facts, excerpts of other conversations, embeddings) is
//!   sent only to local or LAN models unless the user allows sharing it
//!   with cloud models;
//! - summaries are made by the model that serves the conversation, so the
//!   data goes nowhere the conversation did not already go.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use jarvis_ai::{
    AiProvider, ChatRequest, DataLocality, EmbeddingRequest, FinishReason, GenerationOptions,
    ProviderBackend, ProviderId, ProviderKind,
};
use jarvis_events::{EventCategory, EventLevel, RecvError, SystemEvent};
use jarvis_memory::{
    ContextRequest, ContextWindow, Conversation, ConversationPatch, Embedder, EmbeddingState,
    EmbeddingStatus, Episode, Fact, FactInput, FactSource, MemoryEngine, MemoryError, MemoryExport,
    MemoryStatus, MessageRole, MessageStatus, NewMessage, Project, ProjectInput, Retrieval,
    SearchHit, SearchQuery, TransferReport, text,
};
use jarvis_runtime::{RUNTIME_PROVIDER_ID, ServerPhase, ServerRole};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;
use uuid::Uuid;

use crate::chat::{ChatEventSink, ChatStreamEvent, PreparedChat, ResolvedModel, SinkClosed};
use crate::config::Settings;
use crate::error::CoreError;
use crate::{JarvisCore, routing};

pub const MAX_TURN_CHARS: usize = 32_000;
/// Largest memory export JARVIS reads.
pub const MAX_IMPORT_BYTES: u64 = 256 * 1024 * 1024;
const INDEX_BATCH: u32 = 32;
const MAX_INDEX_BATCHES: usize = 200;
const INDEX_INTERVAL: Duration = Duration::from_secs(60);
const EPISODE_FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const EPISODE_BATCH: usize = 50;
const PRUNE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
/// Ollama's default context length; it silently truncates longer prompts
/// because JARVIS does not raise `num_ctx`.
const OLLAMA_DEFAULT_CONTEXT: u32 = 4096;
const SUMMARY_MAX_TOKENS: u32 = 400;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// A user message in a conversation, as sent by the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct ChatTurnRequest {
    pub conversation_id: Uuid,
    pub content: String,
    /// Provider to use; without it the model router chooses.
    pub provider_id: Option<ProviderId>,
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
}

/// State of the memory background work.
#[derive(Debug, Default)]
pub(crate) struct MemoryState {
    /// One turn at a time per conversation.
    turns: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
    compacting: Mutex<HashSet<Uuid>>,
    embedding: Mutex<EmbeddingRuntime>,
    index_notify: Notify,
    shutdown: CancellationToken,
    recorder: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[derive(Debug, Default, Clone)]
struct EmbeddingRuntime {
    model_key: Option<String>,
    indexing: bool,
    error: Option<String>,
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl MemoryState {
    fn turn_lock(&self, conversation_id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(guard(&self.turns).entry(conversation_id).or_default())
    }

    fn busy(&self, conversation_id: Uuid) -> bool {
        guard(&self.turns)
            .get(&conversation_id)
            .is_some_and(|lock| lock.try_lock().is_err())
    }

    fn forget(&self, conversation_id: Uuid) {
        guard(&self.turns).remove(&conversation_id);
    }
}

/// The base system prompt (persona, language, date).
pub(crate) fn system_prompt(settings: &Settings) -> String {
    let language = match settings.general.assistant_language {
        crate::config::AssistantLanguage::Uk => "Ukrainian",
        crate::config::AssistantLanguage::En => "English",
    };
    let today = text::format_date(crate::unix_millis().try_into().unwrap_or(i64::MAX));
    format!(
        "You are JARVIS, a personal AI assistant running on the user's computer. Be helpful, \
         accurate and concise; say so when you do not know something. Answer in {language} \
         unless the user writes in another language or asks otherwise. Today is {today} (UTC)."
    )
}

// ---------------------------------------------------------------------------
// Turn recording
// ---------------------------------------------------------------------------

enum TurnFinish {
    Completed,
    /// Stopped by the user, failed midway, or the UI went away.
    Interrupted,
    Failed,
}

struct TurnOutcome {
    text: String,
    finish: TurnFinish,
}

impl TurnOutcome {
    /// How the reply is saved; `None` when there is nothing to save.
    fn status(&self) -> Option<MessageStatus> {
        let has_text = !self.text.trim().is_empty();
        match self.finish {
            TurnFinish::Completed if has_text => Some(MessageStatus::Complete),
            TurnFinish::Interrupted | TurnFinish::Failed if has_text => {
                Some(MessageStatus::Cancelled)
            }
            _ => None,
        }
    }
}

/// Forwards events to the UI and keeps the reply text for saving. Dropping
/// it before a terminal event (receiver gone) reports an interrupted turn.
struct RecordingSink<S> {
    inner: S,
    text: String,
    done: Option<oneshot::Sender<TurnOutcome>>,
}

impl<S> RecordingSink<S> {
    fn finish(&mut self, finish: TurnFinish) {
        if let Some(done) = self.done.take() {
            let _ = done.send(TurnOutcome {
                text: std::mem::take(&mut self.text),
                finish,
            });
        }
    }
}

impl<S: ChatEventSink> ChatEventSink for RecordingSink<S> {
    fn send(&mut self, event: ChatStreamEvent) -> Result<(), SinkClosed> {
        let finish = match &event {
            ChatStreamEvent::Delta { text } => {
                self.text.push_str(text);
                None
            }
            ChatStreamEvent::Finished {
                finish_reason: FinishReason::Cancelled,
                ..
            } => Some(TurnFinish::Interrupted),
            ChatStreamEvent::Finished { .. } => Some(TurnFinish::Completed),
            ChatStreamEvent::Failed { .. } => Some(TurnFinish::Failed),
            ChatStreamEvent::Started { .. } => None,
        };
        let result = self.inner.send(event);
        if let Some(finish) = finish {
            self.finish(finish);
        }
        result
    }
}

impl<S> Drop for RecordingSink<S> {
    fn drop(&mut self) {
        self.finish(TurnFinish::Interrupted);
    }
}

// ---------------------------------------------------------------------------
// Embeddings through an AI provider
// ---------------------------------------------------------------------------

struct ProviderEmbedder {
    provider: Arc<dyn AiProvider>,
    model: String,
    key: String,
}

#[async_trait]
impl Embedder for ProviderEmbedder {
    fn model_key(&self) -> &str {
        &self.key
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, MemoryError> {
        self.provider
            .embeddings(EmbeddingRequest {
                model: self.model.clone(),
                input: texts,
            })
            .await
            .map(|response| response.vectors)
            .map_err(|err| MemoryError::Embedding(err.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Core API
// ---------------------------------------------------------------------------

impl JarvisCore {
    pub fn memory(&self) -> &Arc<MemoryEngine> {
        &self.memory
    }

    /// Sends a user message in a conversation: builds the context, saves the
    /// message, streams the reply to `sink` and saves it when it ends.
    pub async fn send_message(
        &self,
        request: ChatTurnRequest,
        sink: impl ChatEventSink,
    ) -> Result<Uuid, CoreError> {
        let content = request.content.trim().to_owned();
        if content.is_empty() {
            return Err(CoreError::Validation("The message is empty".to_owned()));
        }
        if content.chars().count() > MAX_TURN_CHARS {
            return Err(CoreError::Validation(format!(
                "A message may contain at most {MAX_TURN_CHARS} characters"
            )));
        }
        if let Some(temperature) = request.temperature
            && !(0.0..=2.0).contains(&temperature)
        {
            return Err(CoreError::Validation(
                "Temperature must be between 0 and 2".to_owned(),
            ));
        }
        if request.max_output_tokens == Some(0) {
            return Err(CoreError::Validation(
                "maxOutputTokens must be greater than 0".to_owned(),
            ));
        }

        let settings = self.config.current();
        let conversation_id = request.conversation_id;
        if !self.memory.is_temporary(conversation_id) && !settings.memory.enabled {
            return Err(MemoryError::Disabled(
                "Saving is turned off, so this saved conversation cannot continue. Start a temporary chat instead.".to_owned(),
            )
            .into());
        }
        let turn = self
            .memory_state
            .turn_lock(conversation_id)
            .try_lock_owned()
            .map_err(|_| {
                CoreError::LimitExceeded(
                    "JARVIS is still answering in this conversation".to_owned(),
                )
            })?;

        let model = self
            .chat
            .resolve_model(
                request.provider_id.as_ref(),
                request.model.as_deref(),
                &settings,
            )
            .await?;
        let window = ContextWindow::new(
            self.model_window(&model, &settings),
            settings.memory.context_limit_tokens,
            request.max_output_tokens,
        );
        let share_memory = model.descriptor.locality != DataLocality::ThirdPartyCloud
            || settings.privacy.share_memory_with_cloud;
        let retrieval = if share_memory
            && (settings.memory.use_facts || settings.memory.recall_conversations)
        {
            Retrieval {
                facts: settings.memory.use_facts,
                recall_conversations: settings.memory.recall_conversations,
                embedder: self.resolve_embedder(&settings).await,
            }
        } else {
            Retrieval::default()
        };
        let mut assembled = self
            .memory
            .prepare_context(ContextRequest {
                conversation_id,
                user_message: content.clone(),
                system_prompt: system_prompt(&settings),
                window,
                retrieval,
            })
            .await?;
        if !share_memory {
            assembled.report.memory_skipped = Some(
                "Saved memory is not shared with cloud models (Settings → Privacy)".to_owned(),
            );
        }

        let reservation = self.chat.reserve()?;
        self.memory
            .append_message(
                conversation_id,
                NewMessage {
                    id: Uuid::new_v4(),
                    role: MessageRole::User,
                    content,
                    status: MessageStatus::Complete,
                    provider_id: None,
                    model: None,
                },
            )
            .await?;

        let assistant_id = Uuid::new_v4();
        let (done, outcome) = oneshot::channel();
        let sink = RecordingSink {
            inner: sink,
            text: String::new(),
            done: Some(done),
        };
        let needs_compaction = assembled.report.needs_compaction;
        let provider = Arc::clone(&model.provider);
        let provider_id = model.descriptor.id.clone();
        let model_name = model.model.clone();
        let request_id = self.chat.launch(
            reservation,
            PreparedChat {
                model,
                messages: assembled.messages,
                options: GenerationOptions {
                    temperature: request.temperature,
                    max_output_tokens: request.max_output_tokens,
                },
                conversation_id: Some(conversation_id),
                message_id: Some(assistant_id),
                context: Some(assembled.report),
            },
            sink,
        );

        let memory = Arc::clone(&self.memory);
        let state = Arc::clone(&self.memory_state);
        let events = self.events.clone();
        let auto_summarize = settings.memory.auto_summarize;
        let timeout = Duration::from_secs(u64::from(settings.ai.request_timeout_secs));
        tokio::spawn(async move {
            if let Ok(outcome) = outcome.await
                && let Some(status) = outcome.status()
            {
                let saved = memory
                    .append_message(
                        conversation_id,
                        NewMessage {
                            id: assistant_id,
                            role: MessageRole::Assistant,
                            content: outcome.text,
                            status,
                            provider_id: Some(provider_id.to_string()),
                            model: Some(model_name.clone()),
                        },
                    )
                    .await;
                if let Err(err) = saved {
                    tracing::warn!("cannot save the reply: {err}");
                }
            }
            drop(turn);
            state.index_notify.notify_one();
            if auto_summarize && needs_compaction {
                compact(
                    &memory,
                    &state,
                    &events,
                    conversation_id,
                    provider,
                    model_name,
                    window,
                    timeout,
                )
                .await;
            }
        });
        Ok(request_id)
    }

    /// Context window to plan with: the running llama.cpp context for the
    /// embedded runtime, at most Ollama's default for Ollama, otherwise what
    /// the provider reports.
    fn model_window(&self, model: &ResolvedModel, settings: &Settings) -> Option<u32> {
        if model.descriptor.id.as_str() == RUNTIME_PROVIDER_ID {
            return self
                .runtime
                .server_state(ServerRole::Chat)
                .context_size
                .or(model.context_window);
        }
        let ollama = settings
            .ai
            .providers
            .iter()
            .any(|p| p.id == model.descriptor.id && p.backend == ProviderBackend::Ollama);
        if ollama {
            return Some(
                model
                    .context_window
                    .unwrap_or(OLLAMA_DEFAULT_CONTEXT)
                    .min(OLLAMA_DEFAULT_CONTEXT),
            );
        }
        model.context_window
    }

    /// The embedding model for semantic search, if one is usable under the
    /// privacy settings.
    async fn resolve_embedder(&self, settings: &Settings) -> Option<Arc<dyn Embedder>> {
        let allow_cloud =
            settings.privacy.allow_cloud_providers && settings.privacy.share_memory_with_cloud;
        let catalog = self.catalog.get(&self.providers, false).await;
        let usable = |entry: &crate::ProviderModels| {
            entry.error.is_none()
                && entry.provider.kind != ProviderKind::Diagnostic
                && routing::check_privacy(&entry.provider, allow_cloud).is_ok()
        };
        let (provider_id, model) = match &settings.memory.embedding_model {
            // An explicit choice is trusted even if the provider does not
            // report the capability (OpenAI-compatible servers do not).
            Some(choice) => {
                let entry = catalog
                    .iter()
                    .find(|p| p.provider.id == choice.provider_id && usable(p))?;
                let model = entry.models.iter().find(|m| m.id == choice.model)?;
                (entry.provider.id.clone(), model.id.clone())
            }
            None => catalog.iter().filter(|p| usable(p)).find_map(|p| {
                p.models
                    .iter()
                    .find(|m| m.capabilities.embeddings)
                    .map(|m| (p.provider.id.clone(), m.id.clone()))
            })?,
        };
        let provider = self.providers.get(&provider_id).ok()?;
        Some(Arc::new(ProviderEmbedder {
            key: format!("{provider_id}/{model}"),
            provider,
            model,
        }))
    }

    /// Runs the indexer soon (e.g. after the embedding model changed).
    pub(crate) fn notify_indexer(&self) {
        self.memory_state.index_notify.notify_one();
    }

    /// Embeds facts and messages that have no vector for the current model.
    async fn run_indexing(&self) {
        let settings = self.config.current();
        if !settings.memory.enabled || self.memory.database().is_none() {
            return;
        }
        let Some(embedder) = self.resolve_embedder(&settings).await else {
            *guard(&self.memory_state.embedding) = EmbeddingRuntime::default();
            return;
        };
        let key = embedder.model_key().to_owned();
        let changed = {
            let mut state = guard(&self.memory_state.embedding);
            let changed = state.model_key.as_deref() != Some(key.as_str());
            *state = EmbeddingRuntime {
                model_key: Some(key.clone()),
                indexing: true,
                error: None,
            };
            changed
        };
        if changed && let Err(err) = self.memory.retain_embeddings(&key).await {
            tracing::warn!("cannot drop old embeddings: {err}");
        }
        let mut error = None;
        for _ in 0..MAX_INDEX_BATCHES {
            if self.memory_state.shutdown.is_cancelled() {
                break;
            }
            match self
                .memory
                .index_pending(embedder.as_ref(), INDEX_BATCH)
                .await
            {
                Ok(0) => break,
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!("embedding with {key} failed: {err}");
                    error = Some(err.to_string());
                    break;
                }
            }
        }
        let mut state = guard(&self.memory_state.embedding);
        state.indexing = false;
        state.error = error;
    }

    /// Starts the indexer, the action-history recorder and its retention.
    pub(crate) fn spawn_memory_tasks(core: &Arc<Self>) {
        let state = Arc::clone(&core.memory_state);

        let weak = Arc::downgrade(core);
        let indexer_state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = indexer_state.shutdown.cancelled() => break,
                    () = indexer_state.index_notify.notified() => {}
                    () = tokio::time::sleep(INDEX_INTERVAL) => {}
                }
                let Some(core) = weak.upgrade() else { break };
                core.run_indexing().await;
            }
        });

        let weak = Arc::downgrade(core);
        let mut subscription = core.events.subscribe();
        let recorder_state = Arc::clone(&state);
        let recorder = tokio::spawn(async move {
            let mut buffer = Vec::new();
            let mut tick = tokio::time::interval(EPISODE_FLUSH_INTERVAL);
            loop {
                tokio::select! {
                    () = recorder_state.shutdown.cancelled() => {
                        // Keep what was published right before shutdown.
                        while let Some(Ok(event)) = subscription.recv().now_or_never() {
                            buffer.extend(episode_from(&event));
                        }
                        flush_episodes(&weak, &mut buffer).await;
                        break;
                    }
                    event = subscription.recv() => match event {
                        Ok(event) => {
                            buffer.extend(episode_from(&event));
                            if buffer.len() >= EPISODE_BATCH {
                                flush_episodes(&weak, &mut buffer).await;
                            }
                        }
                        Err(RecvError::Lagged { .. }) => {}
                        Err(RecvError::Closed) => {
                            flush_episodes(&weak, &mut buffer).await;
                            break;
                        }
                    },
                    _ = tick.tick() => flush_episodes(&weak, &mut buffer).await,
                }
            }
        });
        *guard(&state.recorder) = Some(recorder);

        let weak = Arc::downgrade(core);
        let pruner_state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                if let Some(core) = weak.upgrade() {
                    core.prune_episodes().await;
                }
                tokio::select! {
                    () = pruner_state.shutdown.cancelled() => break,
                    () = tokio::time::sleep(PRUNE_INTERVAL) => {}
                }
            }
        });
    }

    async fn prune_episodes(&self) {
        let days = self.config.current().memory.episode_retention_days;
        if days == 0 || self.memory.database().is_none() {
            return;
        }
        let now = i64::try_from(crate::unix_millis()).unwrap_or(i64::MAX);
        let cutoff = now - i64::from(days) * DAY_MS;
        match self.memory.delete_episodes(Some(cutoff)).await {
            Ok(0) => {}
            Ok(count) => tracing::info!("removed {count} actions older than {days} days"),
            Err(err) => tracing::warn!("cannot prune the action history: {err}"),
        }
    }

    /// Stops the memory tasks; the action history is flushed first.
    pub(crate) async fn stop_memory_tasks(&self) {
        self.memory_state.shutdown.cancel();
        let recorder = guard(&self.memory_state.recorder).take();
        if let Some(recorder) = recorder {
            let _ = tokio::time::timeout(SHUTDOWN_FLUSH_TIMEOUT, recorder).await;
        }
    }

    // ---- Conversations ----------------------------------------------------

    /// Creates a conversation. With saving off it is always temporary.
    pub async fn create_conversation(
        &self,
        project_id: Option<Uuid>,
        temporary: bool,
    ) -> Result<Conversation, CoreError> {
        let temporary = temporary || !self.config.current().memory.enabled;
        Ok(self
            .memory
            .create_conversation(project_id, temporary)
            .await?)
    }

    pub async fn update_conversation(
        &self,
        id: Uuid,
        patch: ConversationPatch,
    ) -> Result<Conversation, CoreError> {
        if !self.memory.is_temporary(id) {
            self.require_saving()?;
        }
        Ok(self.memory.update_conversation(id, patch).await?)
    }

    pub async fn delete_conversation(&self, id: Uuid) -> Result<(), CoreError> {
        if self.memory_state.busy(id) {
            return Err(CoreError::LimitExceeded(
                "Stop the reply before deleting the conversation".to_owned(),
            ));
        }
        let temporary = self.memory.is_temporary(id);
        self.memory.delete_conversation(id).await?;
        self.memory_state.forget(id);
        if !temporary {
            self.events
                .info(EventCategory::Memory, "A conversation was deleted");
        }
        Ok(())
    }

    // ---- Facts and projects -----------------------------------------------

    pub async fn save_fact(&self, input: FactInput) -> Result<Fact, CoreError> {
        self.require_saving()?;
        let new = input.id.is_none();
        let source = if input.source_conversation_id.is_some() {
            FactSource::Conversation
        } else {
            FactSource::User
        };
        let fact = self.memory.save_fact(input, source).await?;
        self.events.info(
            EventCategory::Memory,
            if new {
                "A fact was saved"
            } else {
                "A fact was updated"
            },
        );
        self.memory_state.index_notify.notify_one();
        Ok(fact)
    }

    pub async fn delete_fact(&self, id: Uuid) -> Result<(), CoreError> {
        self.memory.delete_fact(id).await?;
        self.events
            .info(EventCategory::Memory, "A fact was deleted");
        Ok(())
    }

    pub async fn save_project(&self, input: ProjectInput) -> Result<Project, CoreError> {
        self.require_saving()?;
        let new = input.id.is_none();
        let project = self.memory.save_project(input).await?;
        self.events.info(
            EventCategory::Memory,
            if new {
                "A project was created"
            } else {
                "A project was updated"
            },
        );
        Ok(project)
    }

    pub async fn delete_project(&self, id: Uuid) -> Result<(), CoreError> {
        self.memory.delete_project(id).await?;
        self.events.info(
            EventCategory::Memory,
            "A project and its facts were deleted",
        );
        Ok(())
    }

    fn require_saving(&self) -> Result<(), CoreError> {
        if self.config.current().memory.enabled {
            Ok(())
        } else {
            Err(
                MemoryError::Disabled("Saving is turned off in Settings → Memory".to_owned())
                    .into(),
            )
        }
    }

    // ---- History, search and status ---------------------------------------

    /// Deletes the whole action history.
    pub async fn clear_episodes(&self) -> Result<usize, CoreError> {
        Ok(self.memory.delete_episodes(None).await?)
    }

    pub async fn search_memory(&self, query: SearchQuery) -> Result<Vec<SearchHit>, CoreError> {
        let settings = self.config.current();
        let embedder = self.resolve_embedder(&settings).await;
        Ok(self.memory.search(query, embedder).await?)
    }

    pub async fn memory_status(&self) -> MemoryStatus {
        let settings = self.config.current();
        let runtime = guard(&self.memory_state.embedding).clone();
        let database = self.memory.database();
        let counts = self.memory.counts().await.unwrap_or_default();
        let (indexed, pending) = match &runtime.model_key {
            Some(key) => self.memory.embedding_counts(key).await.unwrap_or_default(),
            None => (0, 0),
        };
        let embedding = EmbeddingStatus {
            state: match (&runtime.model_key, runtime.indexing) {
                (None, _) => EmbeddingState::Unavailable,
                (Some(_), true) => EmbeddingState::Indexing,
                (Some(_), false) => EmbeddingState::Ready,
            },
            model: runtime.model_key.clone(),
            indexed,
            pending,
            detail: runtime.error.clone().or_else(|| {
                runtime
                    .model_key
                    .is_none()
                    .then(|| self.no_embedder_detail())
            }),
        };
        MemoryStatus {
            available: database.is_some(),
            error: self.memory.unavailable_reason().map(str::to_owned),
            saving_enabled: settings.memory.enabled,
            database_path: database
                .and_then(|db| db.path())
                .map(|p| p.display().to_string()),
            size_bytes: match database {
                Some(db) => db.size_bytes().await,
                None => 0,
            },
            schema_version: database.map_or(0, |db| db.report().schema_version),
            counts,
            embedding,
        }
    }

    /// Why search is text-only, pointing at the embedding server of the
    /// runtime when it is the reason.
    fn no_embedder_detail(&self) -> String {
        let server = self.runtime.server_state(ServerRole::Embeddings);
        match server.phase {
            ServerPhase::Starting => "The embedding model is loading; search is text-only until it is ready".to_owned(),
            ServerPhase::Failed => format!(
                "The embedding model failed ({}); search is text-only",
                server.error.as_deref().unwrap_or("unknown error")
            ),
            _ => "No embedding model is available, so search is text-only. Start the embedding model on the Runtime screen or use a provider with an embedding model".to_owned(),
        }
    }

    // ---- Export, import, clear ----------------------------------------------

    /// Writes all saved memory to `path` (JSON).
    pub async fn export_memory(&self, path: &Path) -> Result<TransferReport, CoreError> {
        let export = self.memory.export().await?;
        let counts = jarvis_memory::MemoryCounts {
            conversations: export.conversations.len() as u32,
            messages: export
                .conversations
                .iter()
                .map(|c| c.messages.len() as u32)
                .sum(),
            facts: export.facts.len() as u32,
            projects: export.projects.len() as u32,
            episodes: export.episodes.len() as u32,
        };
        let json = serde_json::to_vec_pretty(&export)
            .map_err(|err| CoreError::Internal(format!("cannot serialize memory: {err}")))?;
        let tmp = path.with_extension("jarvis-export.tmp");
        tokio::fs::write(&tmp, json)
            .await
            .map_err(|err| CoreError::io(format!("writing {}", tmp.display()), err))?;
        tokio::fs::rename(&tmp, path)
            .await
            .map_err(|err| CoreError::io(format!("writing {}", path.display()), err))?;
        self.events.info(
            EventCategory::Memory,
            format!(
                "Memory exported: {} conversations, {} facts, {} projects",
                counts.conversations, counts.facts, counts.projects
            ),
        );
        Ok(TransferReport {
            path: path.display().to_string(),
            counts,
            skipped: 0,
        })
    }

    /// Adds the content of a memory export; existing items are skipped.
    pub async fn import_memory(&self, path: &Path) -> Result<TransferReport, CoreError> {
        self.require_saving()?;
        let size = tokio::fs::metadata(path)
            .await
            .map_err(|err| CoreError::io(format!("reading {}", path.display()), err))?
            .len();
        if size > MAX_IMPORT_BYTES {
            return Err(CoreError::LimitExceeded(format!(
                "The file is larger than {} MB",
                MAX_IMPORT_BYTES / 1024 / 1024
            )));
        }
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|err| CoreError::io(format!("reading {}", path.display()), err))?;
        let data: MemoryExport = serde_json::from_slice(&bytes).map_err(|err| {
            MemoryError::Invalid(format!("the file is not a JARVIS memory export: {err}"))
        })?;
        let outcome = self.memory.import(data).await?;
        let counts = outcome.imported;
        self.events.info(
            EventCategory::Memory,
            format!(
                "Memory imported: {} conversations, {} facts, {} projects ({} items already present)",
                counts.conversations, counts.facts, counts.projects, outcome.skipped
            ),
        );
        self.memory_state.index_notify.notify_one();
        Ok(TransferReport {
            path: path.display().to_string(),
            counts,
            skipped: outcome.skipped,
        })
    }

    /// Deletes all saved memory.
    pub async fn clear_memory(&self) -> Result<(), CoreError> {
        if guard(&self.memory_state.turns)
            .values()
            .any(|lock| lock.try_lock().is_err())
        {
            return Err(CoreError::LimitExceeded(
                "Stop the running replies before clearing memory".to_owned(),
            ));
        }
        self.memory.clear().await?;
        *guard(&self.memory_state.embedding) = EmbeddingRuntime::default();
        self.events
            .warning(EventCategory::Memory, "All saved memory was deleted");
        Ok(())
    }
}

/// Summarizes the older part of a conversation with the model that serves
/// it. At most one summary per conversation runs at a time.
#[allow(clippy::too_many_arguments)]
async fn compact(
    memory: &MemoryEngine,
    state: &MemoryState,
    events: &jarvis_events::EventBus,
    conversation_id: Uuid,
    provider: Arc<dyn AiProvider>,
    model: String,
    window: ContextWindow,
    timeout: Duration,
) {
    if !guard(&state.compacting).insert(conversation_id) {
        return;
    }
    let result = async {
        let Some(plan) = memory.compaction_plan(conversation_id, window).await? else {
            return Ok::<_, CoreError>(None);
        };
        let request = ChatRequest {
            model: model.clone(),
            messages: plan.messages,
            options: GenerationOptions {
                temperature: Some(0.2),
                max_output_tokens: Some(SUMMARY_MAX_TOKENS),
            },
        };
        let response = tokio::time::timeout(timeout, provider.chat(request))
            .await
            .map_err(|_| CoreError::Ai(jarvis_ai::AiError::Timeout))??;
        let summary = text::strip_reasoning(&response.content).trim().to_owned();
        if summary.is_empty() {
            return Ok(None);
        }
        memory
            .store_summary(
                conversation_id,
                summary,
                plan.covers_until,
                Some(model.clone()),
            )
            .await?;
        Ok(Some(plan.covered))
    }
    .await;
    guard(&state.compacting).remove(&conversation_id);
    match result {
        Ok(Some(covered)) => {
            events.info(
                EventCategory::Memory,
                format!(
                    "A conversation was summarized: {covered} older messages condensed ({model})"
                ),
            );
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!("conversation summary failed: {err}");
        }
    };
}

/// Actions worth keeping in the action history.
fn episode_from(event: &SystemEvent) -> Option<Episode> {
    if event.level < EventLevel::Info {
        return None;
    }
    let category = match event.category {
        EventCategory::Lifecycle => "lifecycle",
        EventCategory::Runtime => "runtime",
        EventCategory::Memory => "memory",
        EventCategory::Chat => "chat",
        EventCategory::Security => "security",
        _ => return None,
    };
    let level = match event.level {
        EventLevel::Debug | EventLevel::Info => "info",
        EventLevel::Warning => "warning",
        EventLevel::Error => "error",
    };
    Some(Episode {
        id: Uuid::new_v4(),
        occurred_at_ms: i64::try_from(event.timestamp_ms).unwrap_or(i64::MAX),
        category: category.to_owned(),
        level: level.to_owned(),
        summary: text::truncate_chars(&event.message, 1_000),
    })
}

async fn flush_episodes(core: &Weak<JarvisCore>, buffer: &mut Vec<Episode>) {
    if buffer.is_empty() {
        return;
    }
    let batch = std::mem::take(buffer);
    let Some(core) = core.upgrade() else { return };
    if !core.config.current().memory.enabled || core.memory.database().is_none() {
        return;
    }
    // Logged, not published: an event here would record itself.
    if let Err(err) = core.memory.record_episodes(batch).await {
        tracing::warn!("cannot save the action history: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_decide_what_is_saved() {
        let outcome = |text: &str, finish| TurnOutcome {
            text: text.to_owned(),
            finish,
        };
        assert_eq!(
            outcome("hi", TurnFinish::Completed).status(),
            Some(MessageStatus::Complete)
        );
        assert_eq!(
            outcome("partial", TurnFinish::Interrupted).status(),
            Some(MessageStatus::Cancelled)
        );
        assert_eq!(outcome(" ", TurnFinish::Interrupted).status(), None);
        assert_eq!(outcome("", TurnFinish::Failed).status(), None);
    }

    #[test]
    fn only_actions_become_episodes() {
        let event = |level, category| SystemEvent {
            id: 1,
            timestamp_ms: 5,
            level,
            category,
            message: "m".into(),
        };
        assert!(episode_from(&event(EventLevel::Info, EventCategory::Runtime)).is_some());
        assert!(episode_from(&event(EventLevel::Debug, EventCategory::Chat)).is_none());
        assert!(episode_from(&event(EventLevel::Info, EventCategory::Config)).is_none());
        let warning = episode_from(&event(EventLevel::Warning, EventCategory::Security)).unwrap();
        assert_eq!(warning.level, "warning");
        assert_eq!(warning.category, "security");
    }
}
