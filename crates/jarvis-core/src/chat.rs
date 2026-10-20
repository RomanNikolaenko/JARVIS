//! Chat orchestration: provider/model resolution, privacy policy, input
//! limits, streaming with timeouts, and cancellation.
//!
//! Requests are never retried automatically: a partially streamed answer
//! cannot be replayed transparently, and later (agent) requests may have side
//! effects. See ADR 0003.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures::StreamExt;
use jarvis_ai::{
    AiError, AiProvider, ChatChunk, ChatMessage, ChatRequest, ChatRole, FinishReason,
    GenerationOptions, ModelCapabilities, ModelInfo, ProviderDescriptor, ProviderId,
    ProviderRegistry, TokenUsage,
};
use jarvis_events::{EventBus, EventCategory, EventLevel};
use jarvis_memory::ContextReport;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;
use uuid::Uuid;

use crate::catalog::{ModelCatalog, ProviderModels};
use crate::config::{ConfigStore, MAX_MODEL_ID_LEN, Settings};
use crate::error::{CoreError, ErrorCode, ErrorPayload};
use crate::routing::{self, TaskCategory};

pub const MAX_MESSAGES: usize = 200;
pub const MAX_MESSAGE_CHARS: usize = 32_000;
pub const MAX_TOTAL_CHARS: usize = 256_000;
pub const MAX_CONCURRENT_CHATS: usize = 4;

/// A chat request as sent by the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct ChatStartRequest {
    /// Provider to use. Without it, the model router chooses (`TaskCategory::General`).
    pub provider_id: Option<ProviderId>,
    /// Model to use. Without it, the provider's first chat model or the
    /// router's choice is used.
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
}

/// Events streamed to the UI for one chat request. Every request that starts
/// successfully ends with exactly one `finished` or `failed` event, unless the
/// receiver disappears first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export_to = "core.ts")]
pub enum ChatStreamEvent {
    Started {
        request_id: Uuid,
        provider_id: ProviderId,
        model: String,
        /// Conversation the reply belongs to (conversation turns only).
        conversation_id: Option<Uuid>,
        /// Id the reply will be saved under (conversation turns only).
        message_id: Option<Uuid>,
        /// How the context was built (conversation turns only).
        context: Option<ContextReport>,
    },
    Delta {
        text: String,
    },
    Finished {
        finish_reason: FinishReason,
        usage: Option<TokenUsage>,
        elapsed_ms: u64,
    },
    Failed {
        error: ErrorPayload,
        elapsed_ms: u64,
    },
}

/// The receiver of a chat stream went away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkClosed;

/// Destination of [`ChatStreamEvent`]s (a Tauri channel in the desktop app).
pub trait ChatEventSink: Send + 'static {
    fn send(&mut self, event: ChatStreamEvent) -> Result<(), SinkClosed>;
}

impl<F> ChatEventSink for F
where
    F: FnMut(ChatStreamEvent) -> Result<(), SinkClosed> + Send + 'static,
{
    fn send(&mut self, event: ChatStreamEvent) -> Result<(), SinkClosed> {
        self(event)
    }
}

type ActiveRequests = Arc<Mutex<HashMap<Uuid, CancellationToken>>>;

fn lock(active: &ActiveRequests) -> MutexGuard<'_, HashMap<Uuid, CancellationToken>> {
    active.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Removes a request from the active set when its task ends, even on panic.
struct ActiveGuard {
    active: ActiveRequests,
    request_id: Uuid,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        lock(&self.active).remove(&self.request_id);
    }
}

#[derive(Debug)]
pub struct ChatService {
    registry: Arc<ProviderRegistry>,
    catalog: Arc<ModelCatalog>,
    config: Arc<ConfigStore>,
    events: EventBus,
    runtime: tokio::runtime::Handle,
    active: ActiveRequests,
    /// Parent of every request token; cancelled on shutdown.
    shutdown: CancellationToken,
}

/// Provider, model and capabilities chosen for a request.
pub(crate) struct ResolvedModel {
    pub provider: Arc<dyn AiProvider>,
    pub descriptor: ProviderDescriptor,
    pub model: String,
    pub capabilities: ModelCapabilities,
    /// Context window the provider reports for the model, if any.
    pub context_window: Option<u32>,
}

/// A reserved slot among the concurrent requests. Dropping it without
/// launching releases the slot.
pub(crate) struct Reservation {
    request_id: Uuid,
    token: CancellationToken,
    guard: ActiveGuard,
}

/// A request ready to run.
pub(crate) struct PreparedChat {
    pub model: ResolvedModel,
    pub messages: Vec<ChatMessage>,
    pub options: GenerationOptions,
    pub conversation_id: Option<Uuid>,
    pub message_id: Option<Uuid>,
    pub context: Option<ContextReport>,
}

impl ChatService {
    pub(crate) fn new(
        registry: Arc<ProviderRegistry>,
        catalog: Arc<ModelCatalog>,
        config: Arc<ConfigStore>,
        events: EventBus,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            registry,
            catalog,
            config,
            events,
            runtime,
            active: Arc::default(),
            shutdown: CancellationToken::new(),
        }
    }

    /// Validates and starts a chat request. Returns its id once the request is
    /// accepted; results are delivered to `sink` from a background task.
    pub async fn start(
        &self,
        request: ChatStartRequest,
        sink: impl ChatEventSink,
    ) -> Result<Uuid, CoreError> {
        validate_request(&request)?;
        let settings = self.config.current();
        let model = self
            .resolve_model(
                request.provider_id.as_ref(),
                request.model.as_deref(),
                &settings,
            )
            .await?;
        let reservation = self.reserve()?;
        Ok(self.launch(
            reservation,
            PreparedChat {
                model,
                messages: request.messages,
                options: GenerationOptions {
                    temperature: request.temperature,
                    max_output_tokens: request.max_output_tokens,
                },
                conversation_id: None,
                message_id: None,
                context: None,
            },
            sink,
        ))
    }

    /// Reserves one of the concurrent request slots.
    pub(crate) fn reserve(&self) -> Result<Reservation, CoreError> {
        let token = self.shutdown.child_token();
        let request_id = Uuid::new_v4();
        let mut active = lock(&self.active);
        if active.len() >= MAX_CONCURRENT_CHATS {
            return Err(CoreError::LimitExceeded(format!(
                "At most {MAX_CONCURRENT_CHATS} chat requests can run at the same time"
            )));
        }
        active.insert(request_id, token.clone());
        Ok(Reservation {
            request_id,
            token,
            guard: ActiveGuard {
                active: Arc::clone(&self.active),
                request_id,
            },
        })
    }

    /// Runs a prepared request in the background; events go to `sink`.
    pub(crate) fn launch(
        &self,
        reservation: Reservation,
        prepared: PreparedChat,
        sink: impl ChatEventSink,
    ) -> Uuid {
        let settings = self.config.current();
        let Reservation {
            request_id,
            token,
            guard,
        } = reservation;
        let job = ChatJob {
            request_id,
            provider: prepared.model.provider,
            provider_id: prepared.model.descriptor.id,
            streaming: prepared.model.capabilities.streaming,
            request: ChatRequest {
                model: prepared.model.model,
                messages: prepared.messages,
                options: prepared.options,
            },
            conversation_id: prepared.conversation_id,
            message_id: prepared.message_id,
            context: prepared.context,
            total_timeout: Duration::from_secs(u64::from(settings.ai.request_timeout_secs)),
            idle_timeout: Duration::from_secs(u64::from(settings.ai.idle_timeout_secs)),
            events: self.events.clone(),
        };
        self.runtime.spawn(async move {
            let _guard = guard;
            job.run(token, sink).await;
        });
        request_id
    }

    /// Requests cancellation. Returns `false` if the request is not running.
    pub fn cancel(&self, request_id: Uuid) -> bool {
        let token = lock(&self.active).get(&request_id).cloned();
        match token {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    pub fn active_requests(&self) -> usize {
        lock(&self.active).len()
    }

    /// Cancels all running requests and rejects new ones (used on shutdown).
    pub fn cancel_all(&self) {
        self.shutdown.cancel();
    }

    /// Chooses a chat-capable provider and model; blocked choices are
    /// reported as security events.
    pub(crate) async fn resolve_model(
        &self,
        provider_id: Option<&ProviderId>,
        model: Option<&str>,
        settings: &Settings,
    ) -> Result<ResolvedModel, CoreError> {
        let resolved = self
            .resolve(provider_id, model, settings)
            .await
            .inspect_err(|err| {
                if err.code() == ErrorCode::PermissionDenied {
                    self.events.warning(
                        EventCategory::Security,
                        format!("Blocked a chat request: {err}"),
                    );
                }
            })?;
        if !resolved.capabilities.chat {
            return Err(AiError::Unsupported {
                provider: resolved.descriptor.id,
                operation: "chat",
            }
            .into());
        }
        Ok(resolved)
    }

    /// Chooses provider and model: an explicit pair is used as is (after the
    /// privacy check, before any network call); otherwise the catalog and the
    /// model router decide.
    async fn resolve(
        &self,
        provider_id: Option<&ProviderId>,
        model: Option<&str>,
        settings: &Settings,
    ) -> Result<ResolvedModel, CoreError> {
        let allow_cloud = settings.privacy.allow_cloud_providers;

        if let (Some(provider_id), Some(model)) = (provider_id, model) {
            let provider = self.registry.get(provider_id)?;
            routing::check_privacy(provider.descriptor(), allow_cloud)?;
            let capabilities = provider.capabilities(model).await?;
            let catalog = self.catalog.get(&self.registry, false).await;
            let context_window = catalog
                .iter()
                .find(|p| p.provider.id == *provider_id)
                .and_then(|p| p.models.iter().find(|m| m.id == model))
                .and_then(|m| m.context_window);
            return Ok(ResolvedModel {
                descriptor: provider.descriptor().clone(),
                provider,
                model: model.to_owned(),
                capabilities,
                context_window,
            });
        }

        let catalog = self.catalog.get(&self.registry, false).await;
        let (provider_id, model) = match (provider_id, model) {
            (Some(provider_id), _) => first_chat_model(&catalog, provider_id, allow_cloud)?,
            (None, Some(model)) => find_model(&catalog, model, allow_cloud)?,
            (None, None) => {
                let selection = routing::select(
                    TaskCategory::General,
                    &settings.ai.routing,
                    &catalog,
                    allow_cloud,
                )?;
                (selection.provider.id, selection.model)
            }
        };
        let provider = self.registry.get(&provider_id)?;
        Ok(ResolvedModel {
            descriptor: provider.descriptor().clone(),
            provider,
            model: model.id,
            capabilities: model.capabilities,
            context_window: model.context_window,
        })
    }
}

fn first_chat_model(
    catalog: &[ProviderModels],
    provider_id: &ProviderId,
    allow_cloud: bool,
) -> Result<(ProviderId, ModelInfo), CoreError> {
    let entry = catalog
        .iter()
        .find(|p| p.provider.id == *provider_id)
        .ok_or_else(|| AiError::ProviderNotFound(provider_id.clone()))?;
    routing::check_privacy(&entry.provider, allow_cloud)?;
    if let Some(error) = &entry.error {
        return Err(AiError::Unavailable(error.clone()).into());
    }
    entry
        .models
        .iter()
        .find(|m| m.capabilities.chat)
        .map(|m| (entry.provider.id.clone(), m.clone()))
        .ok_or_else(|| {
            CoreError::NotFound(format!(
                "AI provider \"{}\" has no chat-capable models",
                entry.provider.display_name
            ))
        })
}

fn find_model(
    catalog: &[ProviderModels],
    model: &str,
    allow_cloud: bool,
) -> Result<(ProviderId, ModelInfo), CoreError> {
    catalog
        .iter()
        .filter(|p| p.error.is_none())
        .filter(|p| routing::check_privacy(&p.provider, allow_cloud).is_ok())
        .find_map(|p| {
            p.models
                .iter()
                .find(|m| m.id == model)
                .map(|m| (p.provider.id.clone(), m.clone()))
        })
        .ok_or_else(|| {
            AiError::ModelNotFound {
                model: model.to_owned(),
            }
            .into()
        })
}

fn validate_request(request: &ChatStartRequest) -> Result<(), CoreError> {
    let invalid = |message: String| Err(CoreError::Validation(message));

    if request.messages.is_empty() {
        return invalid("A chat request needs at least one message".to_owned());
    }
    if request.messages.len() > MAX_MESSAGES {
        return invalid(format!(
            "A chat request may contain at most {MAX_MESSAGES} messages"
        ));
    }
    if !request.messages.iter().any(|m| m.role == ChatRole::User) {
        return invalid("A chat request needs at least one user message".to_owned());
    }
    let mut total = 0usize;
    for message in &request.messages {
        let chars = message.content.chars().count();
        if chars > MAX_MESSAGE_CHARS {
            return invalid(format!(
                "A single message may contain at most {MAX_MESSAGE_CHARS} characters"
            ));
        }
        total += chars;
    }
    if total > MAX_TOTAL_CHARS {
        return invalid(format!(
            "A chat request may contain at most {MAX_TOTAL_CHARS} characters in total"
        ));
    }
    if let Some(temperature) = request.temperature
        && !(0.0..=2.0).contains(&temperature)
    {
        return invalid("Temperature must be between 0 and 2".to_owned());
    }
    if request.max_output_tokens == Some(0) {
        return invalid("maxOutputTokens must be greater than 0".to_owned());
    }
    if let Some(model) = &request.model
        && (model.trim().is_empty() || model.len() > MAX_MODEL_ID_LEN)
    {
        return invalid(format!(
            "Model id must be 1..={MAX_MODEL_ID_LEN} characters"
        ));
    }
    Ok(())
}

enum Outcome {
    Finished {
        finish_reason: FinishReason,
        usage: Option<TokenUsage>,
    },
    Cancelled,
    Failed(CoreError),
    SinkClosed,
}

struct ChatJob {
    request_id: Uuid,
    provider: Arc<dyn AiProvider>,
    provider_id: ProviderId,
    streaming: bool,
    request: ChatRequest,
    conversation_id: Option<Uuid>,
    message_id: Option<Uuid>,
    context: Option<ContextReport>,
    total_timeout: Duration,
    idle_timeout: Duration,
    events: EventBus,
}

impl ChatJob {
    async fn run(self, token: CancellationToken, mut sink: impl ChatEventSink) {
        let started = Instant::now();
        let short_id = short_id(self.request_id);
        if sink
            .send(ChatStreamEvent::Started {
                request_id: self.request_id,
                provider_id: self.provider_id.clone(),
                model: self.request.model.clone(),
                conversation_id: self.conversation_id,
                message_id: self.message_id,
                context: self.context.clone(),
            })
            .is_err()
        {
            return;
        }
        self.events.publish(
            EventLevel::Debug,
            EventCategory::Chat,
            format!(
                "Chat {short_id} started ({} / {})",
                self.provider_id, self.request.model
            ),
        );

        let outcome = tokio::select! {
            biased;
            () = token.cancelled() => Outcome::Cancelled,
            result = tokio::time::timeout(self.total_timeout, self.drive(&mut sink)) => {
                result.unwrap_or_else(|_| {
                    tracing::warn!(
                        request = %self.request_id,
                        "chat request exceeded the {}s limit",
                        self.total_timeout.as_secs()
                    );
                    Outcome::Failed(AiError::Timeout.into())
                })
            }
        };
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        match outcome {
            Outcome::Finished {
                finish_reason,
                usage,
            } => {
                let _ = sink.send(ChatStreamEvent::Finished {
                    finish_reason,
                    usage,
                    elapsed_ms,
                });
                self.events.info(
                    EventCategory::Chat,
                    format!(
                        "Chat {short_id} completed in {elapsed_ms} ms ({})",
                        self.request.model
                    ),
                );
            }
            Outcome::Cancelled => {
                let _ = sink.send(ChatStreamEvent::Finished {
                    finish_reason: FinishReason::Cancelled,
                    usage: None,
                    elapsed_ms,
                });
                self.events.info(
                    EventCategory::Chat,
                    format!("Chat {short_id} cancelled after {elapsed_ms} ms"),
                );
            }
            Outcome::Failed(error) => {
                let _ = sink.send(ChatStreamEvent::Failed {
                    error: error.to_payload(),
                    elapsed_ms,
                });
                self.events.warning(
                    EventCategory::Chat,
                    format!("Chat {short_id} failed: {error}"),
                );
            }
            Outcome::SinkClosed => {
                self.events.info(
                    EventCategory::Chat,
                    format!("Chat {short_id} stopped: the receiver went away"),
                );
            }
        }
    }

    async fn drive(&self, sink: &mut impl ChatEventSink) -> Outcome {
        if !self.streaming {
            return match self.provider.chat(self.request.clone()).await {
                Ok(response) => {
                    if sink
                        .send(ChatStreamEvent::Delta {
                            text: response.content,
                        })
                        .is_err()
                    {
                        return Outcome::SinkClosed;
                    }
                    Outcome::Finished {
                        finish_reason: response.finish_reason,
                        usage: response.usage,
                    }
                }
                Err(err) => Outcome::Failed(err.into()),
            };
        }

        let mut stream = match tokio::time::timeout(
            self.idle_timeout,
            self.provider.stream_chat(self.request.clone()),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(err)) => return Outcome::Failed(err.into()),
            Err(_) => return Outcome::Failed(AiError::Timeout.into()),
        };

        loop {
            match tokio::time::timeout(self.idle_timeout, stream.next()).await {
                Err(_) => return Outcome::Failed(AiError::Timeout.into()),
                Ok(None) => {
                    return Outcome::Finished {
                        finish_reason: FinishReason::Stop,
                        usage: None,
                    };
                }
                Ok(Some(Err(err))) => return Outcome::Failed(err.into()),
                Ok(Some(Ok(ChatChunk::Delta(text)))) => {
                    if text.is_empty() {
                        continue;
                    }
                    if sink.send(ChatStreamEvent::Delta { text }).is_err() {
                        return Outcome::SinkClosed;
                    }
                }
                Ok(Some(Ok(ChatChunk::Done {
                    finish_reason,
                    usage,
                }))) => {
                    return Outcome::Finished {
                        finish_reason,
                        usage,
                    };
                }
            }
        }
    }
}

fn short_id(id: Uuid) -> String {
    id.simple().to_string().chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_ai::EchoProvider;
    use tokio::sync::mpsc;

    fn request(text: &str) -> ChatStartRequest {
        ChatStartRequest {
            provider_id: None,
            model: None,
            messages: vec![ChatMessage::user(text)],
            temperature: None,
            max_output_tokens: None,
        }
    }

    fn service(chunk_delay: Duration) -> ChatService {
        let registry = Arc::new(ProviderRegistry::new());
        registry
            .register(Arc::new(EchoProvider::with_chunk_delay(chunk_delay)))
            .expect("register");
        let config = Arc::new(ConfigStore::in_memory(
            std::env::temp_dir().join("jarvis-unused-config.toml"),
            Settings::default(),
        ));
        ChatService::new(
            registry,
            Arc::default(),
            config,
            EventBus::default(),
            tokio::runtime::Handle::current(),
        )
    }

    fn channel_sink() -> (impl ChatEventSink, mpsc::UnboundedReceiver<ChatStreamEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let sink = move |event: ChatStreamEvent| tx.send(event).map_err(|_| SinkClosed);
        (sink, rx)
    }

    async fn collect(mut rx: mpsc::UnboundedReceiver<ChatStreamEvent>) -> Vec<ChatStreamEvent> {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    #[test]
    fn validation_rejects_bad_requests() {
        let mut empty = request("x");
        empty.messages.clear();
        assert!(validate_request(&empty).is_err());

        let mut system_only = request("x");
        system_only.messages = vec![ChatMessage::new(ChatRole::System, "rules")];
        assert!(validate_request(&system_only).is_err());

        let too_long = request(&"a".repeat(MAX_MESSAGE_CHARS + 1));
        assert!(validate_request(&too_long).is_err());

        let mut too_many = request("x");
        too_many.messages = vec![ChatMessage::user("x"); MAX_MESSAGES + 1];
        assert!(validate_request(&too_many).is_err());

        let mut hot = request("x");
        hot.temperature = Some(f32::NAN);
        assert!(validate_request(&hot).is_err());

        let mut zero_tokens = request("x");
        zero_tokens.max_output_tokens = Some(0);
        assert!(validate_request(&zero_tokens).is_err());

        assert!(validate_request(&request("hello")).is_ok());
    }

    #[tokio::test]
    async fn streams_echo_reply_and_finishes() {
        let chat = service(Duration::ZERO);
        let (sink, rx) = channel_sink();
        let id = chat.start(request("hi JARVIS"), sink).await.expect("start");
        let events = collect(rx).await;

        assert!(matches!(
            &events[0],
            ChatStreamEvent::Started { request_id, model, .. } if *request_id == id && model == EchoProvider::MODEL
        ));
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ChatStreamEvent::Delta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "[echo] hi JARVIS");
        assert!(matches!(
            events.last(),
            Some(ChatStreamEvent::Finished {
                finish_reason: FinishReason::Stop,
                ..
            })
        ));
        assert_eq!(chat.active_requests(), 0);
    }

    #[tokio::test]
    async fn cancellation_stops_the_stream() {
        let chat = service(Duration::from_secs(5));
        let (sink, rx) = channel_sink();
        let id = chat
            .start(request("one two three four"), sink)
            .await
            .expect("start");
        assert_eq!(chat.active_requests(), 1);
        assert!(chat.cancel(id));

        let events = collect(rx).await;
        assert!(matches!(
            events.last(),
            Some(ChatStreamEvent::Finished {
                finish_reason: FinishReason::Cancelled,
                ..
            })
        ));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ChatStreamEvent::Delta { .. }))
        );
        assert_eq!(chat.active_requests(), 0);
        assert!(!chat.cancel(id), "finished requests cannot be cancelled");
    }

    #[tokio::test]
    async fn rejects_unknown_provider() {
        let chat = service(Duration::ZERO);
        let (sink, _rx) = channel_sink();
        let mut req = request("hi");
        req.provider_id = Some(ProviderId::new("missing"));
        let err = chat.start(req, sink).await.expect_err("unknown provider");
        assert_eq!(err.code(), crate::error::ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn enforces_the_concurrency_limit() {
        let chat = service(Duration::from_secs(60));
        let mut receivers = Vec::new();
        for _ in 0..MAX_CONCURRENT_CHATS {
            let (sink, rx) = channel_sink();
            chat.start(request("slow"), sink).await.expect("start");
            receivers.push(rx);
        }
        let (sink, _rx) = channel_sink();
        let err = chat
            .start(request("one too many"), sink)
            .await
            .expect_err("limit");
        assert_eq!(err.code(), crate::error::ErrorCode::LimitExceeded);

        chat.cancel_all();
        for rx in receivers {
            let events = collect(rx).await;
            assert!(matches!(
                events.last(),
                Some(ChatStreamEvent::Finished {
                    finish_reason: FinishReason::Cancelled,
                    ..
                })
            ));
        }
    }

    #[test]
    fn stream_events_use_a_type_tag() {
        let event = ChatStreamEvent::Delta { text: "hi".into() };
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(json["type"], "delta");
        assert_eq!(json["text"], "hi");

        let finished = ChatStreamEvent::Finished {
            finish_reason: FinishReason::Stop,
            usage: None,
            elapsed_ms: 5,
        };
        let json = serde_json::to_value(&finished).expect("serialize");
        assert_eq!(json["type"], "finished");
        assert_eq!(json["finishReason"], "stop");
        assert_eq!(json["elapsedMs"], 5);
    }
}
