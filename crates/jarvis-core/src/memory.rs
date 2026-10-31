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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use jarvis_ai::{
    AiProvider, ChatRequest, ChatRole, DataLocality, EmbeddingRequest, FinishReason,
    GenerationOptions, ProviderBackend, ProviderId, ProviderKind,
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
use crate::config::{ConfigStore, Settings};
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
    /// Read the reply aloud (requests made by voice). Takes effect only
    /// when speech synthesis is installed and `voice.speakReplies` is on.
    #[serde(default)]
    pub speak: bool,
    /// Ids of captured images to show the model (see `capture_screen`).
    /// The model must support vision.
    #[serde(default)]
    pub attachments: Vec<String>,
}

/// State of the memory background work.
#[derive(Debug, Default)]
pub(crate) struct MemoryState {
    /// One turn at a time per conversation.
    turns: Mutex<HashMap<Uuid, Arc<TurnSlot>>>,
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

/// How long a new turn waits for the previous reply to be saved.
const TURN_HANDOVER: Duration = Duration::from_secs(10);

/// The turn of one conversation.
#[derive(Debug, Default)]
struct TurnSlot {
    lock: Arc<tokio::sync::Mutex<()>>,
    /// The reply has ended (the client already has its last event) and is
    /// being saved: the next turn waits for it instead of failing. Set
    /// before the last event is forwarded, cleared by the next owner.
    finishing: AtomicBool,
}

impl TurnSlot {
    /// `true` when no reply runs: none is running, or the one that ended
    /// was saved within the handover time.
    async fn settle(&self) -> bool {
        if self.lock.try_lock().is_ok() {
            return true;
        }
        self.finishing.load(Ordering::Acquire)
            && tokio::time::timeout(TURN_HANDOVER, self.lock.lock())
                .await
                .is_ok()
    }
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl MemoryState {
    fn turn_slot(&self, conversation_id: Uuid) -> Arc<TurnSlot> {
        Arc::clone(guard(&self.turns).entry(conversation_id).or_default())
    }

    /// Whether a reply is running in the conversation.
    async fn busy(&self, conversation_id: Uuid) -> bool {
        let slot = guard(&self.turns).get(&conversation_id).cloned();
        match slot {
            Some(slot) => !slot.settle().await,
            None => false,
        }
    }

    /// Whether a reply is running in any conversation.
    async fn any_busy(&self) -> bool {
        let slots: Vec<Arc<TurnSlot>> = guard(&self.turns).values().cloned().collect();
        for slot in slots {
            if !slot.settle().await {
                return true;
            }
        }
        false
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
    slot: Arc<TurnSlot>,
}

impl<S> RecordingSink<S> {
    fn finish(&mut self, finish: TurnFinish) {
        self.slot.finishing.store(true, Ordering::Release);
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
        // Before the client learns that the reply ended, so a message it
        // sends right away waits for the reply to be saved.
        if finish.is_some() {
            self.slot.finishing.store(true, Ordering::Release);
        }
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
    config: Arc<ConfigStore>,
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
        let settings = self.config.current();
        routing::check_data_privacy(self.provider.descriptor(), &settings.privacy, true, false)
            .map_err(|err| MemoryError::Embedding(err.to_string()))?;
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
        let still_answering = || {
            CoreError::LimitExceeded("JARVIS is still answering in this conversation".to_owned())
        };
        let slot = self.memory_state.turn_slot(conversation_id);
        let turn = match Arc::clone(&slot.lock).try_lock_owned() {
            Ok(turn) => turn,
            // The previous reply has ended and is being saved.
            Err(_) if slot.finishing.load(Ordering::Acquire) => {
                tokio::time::timeout(TURN_HANDOVER, Arc::clone(&slot.lock).lock_owned())
                    .await
                    .map_err(|_| still_answering())?
            }
            Err(_) => return Err(still_answering()),
        };
        slot.finishing.store(false, Ordering::Release);

        let attachments = self.attachments(&request.attachments)?;
        let model = if attachments.images.is_empty() {
            self.chat
                .resolve_model(
                    request.provider_id.as_ref(),
                    request.model.as_deref(),
                    &settings,
                )
                .await?
        } else {
            let model = self
                .chat
                .resolve_vision_model(
                    request.provider_id.as_ref(),
                    request.model.as_deref(),
                    &settings,
                )
                .await?;
            self.check_vision_model(&model, &settings)?;
            model
        };
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
        if !attachments.images.is_empty()
            && let Some(current) = assembled
                .messages
                .iter_mut()
                .rev()
                .find(|m| m.role == ChatRole::User)
        {
            current.images = attachments.images;
        }
        if !share_memory {
            assembled.report.memory_skipped = Some(
                "Saved memory is not shared with cloud models (Settings → Privacy)".to_owned(),
            );
        }
        // Context preparation can wait for storage and semantic retrieval.
        // Refuse newly revoked sharing permissions before recording this turn.
        self.chat.authorize_request(
            model.provider.as_ref(),
            &assembled.messages,
            Some(&assembled.report),
        )?;

        // Images are not saved; the conversation keeps a note instead.
        let saved_content = if attachments.notes.is_empty() {
            content
        } else {
            format!("{content}\n\n{}", attachments.notes.join("\n"))
        };
        let reservation = self.chat.reserve()?;
        self.memory
            .append_message(
                conversation_id,
                NewMessage {
                    id: Uuid::new_v4(),
                    role: MessageRole::User,
                    content: saved_content,
                    status: MessageStatus::Complete,
                    provider_id: None,
                    model: None,
                },
            )
            .await?;

        let assistant_id = Uuid::new_v4();
        let (done, outcome) = oneshot::channel();
        // A new spoken request also interrupts the previous reply.
        let speech = (request.speak && self.voice.speaks_replies()).then(|| self.voice.speak());
        let sink = RecordingSink {
            inner: crate::voice::SpeakingSink {
                inner: sink,
                speech,
            },
            text: String::new(),
            done: Some(done),
            slot,
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

        for id in &attachments.ids {
            self.captures.remove(id);
        }

        let memory = Arc::clone(&self.memory);
        let state = Arc::clone(&self.memory_state);
        let config = Arc::clone(&self.config);
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
                    &config,
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
            config: Arc::clone(&self.config),
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
        if self.memory_state.busy(id).await {
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
        if self.memory_state.any_busy().await {
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
    config: &ConfigStore,
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
        let settings = config.current();
        if !settings.memory.enabled || !settings.memory.auto_summarize {
            return Ok(None);
        }
        // Summaries contain this conversation, not facts or other chats, so
        // only its destination needs cloud permission.
        routing::check_data_privacy(provider.descriptor(), &settings.privacy, false, false)
            .inspect_err(|err| {
                events.warning(
                    EventCategory::Security,
                    format!("Blocked a conversation summary: {err}"),
                );
            })?;
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
    use std::sync::atomic::AtomicUsize;

    use futures::{StreamExt, stream};
    use jarvis_ai::{
        AiError, ChatChunk, ChatStream, EmbeddingResponse, ModelInfo, ProviderConfig,
        ProviderDescriptor,
    };

    #[derive(Debug)]
    struct PrivacyRecorder {
        descriptor: ProviderDescriptor,
        embeddings: AtomicUsize,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl PrivacyRecorder {
        fn new(locality: DataLocality) -> Arc<Self> {
            Arc::new(Self {
                descriptor: ProviderDescriptor {
                    id: ProviderId::new("memory-provider"),
                    display_name: "Memory provider".into(),
                    kind: if locality == DataLocality::ThirdPartyCloud {
                        ProviderKind::Cloud
                    } else {
                        ProviderKind::LocalServer
                    },
                    locality,
                },
                embeddings: AtomicUsize::new(0),
                requests: Mutex::default(),
            })
        }
    }

    #[async_trait]
    impl AiProvider for PrivacyRecorder {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }

        async fn health_check(&self) -> Result<(), AiError> {
            Ok(())
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
            Ok(vec![])
        }

        async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError> {
            guard(&self.requests).push(request);
            Ok(stream::iter([
                Ok(ChatChunk::Delta(
                    "A summary of the current conversation".into(),
                )),
                Ok(ChatChunk::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                }),
            ])
            .boxed())
        }

        async fn embeddings(
            &self,
            request: EmbeddingRequest,
        ) -> Result<EmbeddingResponse, AiError> {
            self.embeddings.fetch_add(1, Ordering::SeqCst);
            Ok(EmbeddingResponse {
                vectors: vec![vec![1.0, 0.0]; request.input.len()],
            })
        }
    }

    #[tokio::test]
    async fn cached_embedders_check_actual_destination_and_current_sharing_permissions() {
        for revoke_cloud in [true, false] {
            let dir = tempfile::tempdir().expect("temporary settings");
            let mut settings = Settings::default();
            settings.privacy.allow_cloud_providers = true;
            settings.privacy.share_memory_with_cloud = true;
            let config = Arc::new(ConfigStore::in_memory(
                dir.path().join("settings.toml"),
                settings.clone(),
            ));
            let provider = PrivacyRecorder::new(DataLocality::ThirdPartyCloud);
            let embedder = ProviderEmbedder {
                config: Arc::clone(&config),
                provider: provider.clone(),
                model: "embedding-model".into(),
                key: "memory-provider/embedding-model".into(),
            };
            assert!(embedder.embed(vec!["Allowed fact".into()]).await.is_ok());
            assert_eq!(provider.embeddings.load(Ordering::SeqCst), 1);

            // Repointing the configured ID does not change the cached cloud handle.
            settings.ai.providers = vec![ProviderConfig {
                id: provider.descriptor.id.clone(),
                backend: ProviderBackend::Ollama,
                display_name: "Replacement local".into(),
                base_url: "http://127.0.0.1:11434".into(),
                enabled: true,
                native_tool_models: vec![],
            }];
            if revoke_cloud {
                settings.privacy.allow_cloud_providers = false;
            } else {
                settings.privacy.share_memory_with_cloud = false;
            }
            config
                .update(settings.clone())
                .await
                .expect("revoke sharing");
            let error = embedder
                .embed(vec!["A newly private fact".into()])
                .await
                .expect_err("cached cloud destination must not receive it");
            assert!(matches!(error, MemoryError::Embedding(_)));
            assert_eq!(provider.embeddings.load(Ordering::SeqCst), 1);

            settings.privacy.allow_cloud_providers = true;
            settings.privacy.share_memory_with_cloud = true;
            config.update(settings).await.expect("restore sharing");
            assert!(embedder.embed(vec!["Allowed again".into()]).await.is_ok());
            assert_eq!(provider.embeddings.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn local_embedders_do_not_require_cloud_sharing_permissions() {
        for locality in [DataLocality::OnDevice, DataLocality::LocalNetwork] {
            let provider = PrivacyRecorder::new(locality);
            let embedder = ProviderEmbedder {
                config: Arc::new(ConfigStore::in_memory(
                    std::env::temp_dir().join("jarvis-unused-memory-config.toml"),
                    Settings::default(),
                )),
                provider: provider.clone(),
                model: "embedding-model".into(),
                key: "memory-provider/embedding-model".into(),
            };
            assert!(embedder.embed(vec!["Local fact".into()]).await.is_ok());
            assert_eq!(provider.embeddings.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn deferred_compaction_rechecks_current_permissions_before_dispatch() {
        for revoked in ["cloud", "automatic summaries", "saving"] {
            let dir = tempfile::tempdir().expect("temporary settings");
            let mut settings = Settings::default();
            settings.privacy.allow_cloud_providers = true;
            // A summary of the current chat is allowed without sharing other memory.
            assert!(!settings.privacy.share_memory_with_cloud);
            let config = ConfigStore::in_memory(dir.path().join("settings.toml"), settings.clone());
            let provider = PrivacyRecorder::new(DataLocality::ThirdPartyCloud);
            let memory = MemoryEngine::in_memory().expect("memory database");
            let conversation = memory
                .create_conversation(None, false)
                .await
                .expect("conversation");
            for index in 0..12 {
                memory
                    .append_message(
                        conversation.id,
                        NewMessage {
                            id: Uuid::new_v4(),
                            role: if index % 2 == 0 {
                                MessageRole::User
                            } else {
                                MessageRole::Assistant
                            },
                            content: format!(
                                "{index}: {}",
                                "Detailed private conversation history. ".repeat(4)
                            ),
                            status: MessageStatus::Complete,
                            provider_id: None,
                            model: None,
                        },
                    )
                    .await
                    .expect("seed conversation");
            }
            let window = ContextWindow::new(Some(1024), None, Some(128));
            assert!(
                memory
                    .compaction_plan(conversation.id, window)
                    .await
                    .expect("compaction plan")
                    .is_some()
            );
            match revoked {
                "cloud" => settings.privacy.allow_cloud_providers = false,
                "automatic summaries" => settings.memory.auto_summarize = false,
                _ => settings.memory.enabled = false,
            }
            config
                .update(settings.clone())
                .await
                .expect("revoke before deferred summary");
            let state = MemoryState::default();
            let events = jarvis_events::EventBus::default();
            compact(
                &memory,
                &state,
                &events,
                &config,
                conversation.id,
                provider.clone(),
                "summary-model".into(),
                window,
                Duration::from_secs(1),
            )
            .await;
            assert!(guard(&provider.requests).is_empty(), "{revoked}");
            assert!(
                memory
                    .conversation(conversation.id)
                    .await
                    .expect("conversation")
                    .summary
                    .is_none()
            );
            assert!(
                guard(&state.compacting).is_empty(),
                "summary reservation released"
            );

            settings.privacy.allow_cloud_providers = true;
            settings.memory.auto_summarize = true;
            settings.memory.enabled = true;
            config.update(settings).await.expect("restore permissions");
            compact(
                &memory,
                &state,
                &events,
                &config,
                conversation.id,
                provider.clone(),
                "summary-model".into(),
                window,
                Duration::from_secs(1),
            )
            .await;
            assert_eq!(guard(&provider.requests).len(), 1, "{revoked}");
            assert!(
                memory
                    .conversation(conversation.id)
                    .await
                    .expect("conversation")
                    .summary
                    .is_some()
            );
        }
    }

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
