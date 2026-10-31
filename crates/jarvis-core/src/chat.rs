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
        self.authorize_request(model.provider.as_ref(), &request.messages, None)?;
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
            config: Arc::clone(&self.config),
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

    /// Rechecks the actual destination and payload after asynchronous preparation.
    pub(crate) fn authorize_request(
        &self,
        provider: &dyn AiProvider,
        messages: &[ChatMessage],
        context: Option<&ContextReport>,
    ) -> Result<(), CoreError> {
        authorize_chat_request(&self.config, provider, messages, context, &self.events)
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
        self.authorize_request(resolved.provider.as_ref(), &[], None)?;
        if !resolved.capabilities.chat {
            return Err(AiError::Unsupported {
                provider: resolved.descriptor.id,
                operation: "chat",
            }
            .into());
        }
        Ok(resolved)
    }

    /// Like [`Self::resolve_model`] for a message with images: automatic
    /// routing uses the vision route.
    pub(crate) async fn resolve_vision_model(
        &self,
        provider_id: Option<&ProviderId>,
        model: Option<&str>,
        settings: &Settings,
    ) -> Result<ResolvedModel, CoreError> {
        if provider_id.is_some() || model.is_some() {
            return self.resolve_model(provider_id, model, settings).await;
        }
        let catalog = self.catalog.get(&self.registry, false).await;
        let selection = routing::select(
            TaskCategory::Vision,
            &settings.ai.routing,
            &catalog,
            settings.privacy.allow_cloud_providers,
        )?;
        let provider = self.registry.get(&selection.provider.id)?;
        self.authorize_request(provider.as_ref(), &[], None)?;
        Ok(ResolvedModel {
            descriptor: provider.descriptor().clone(),
            provider,
            model: selection.model.id,
            capabilities: selection.model.capabilities,
            context_window: selection.model.context_window,
        })
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

fn authorize_chat_request(
    config: &ConfigStore,
    provider: &dyn AiProvider,
    messages: &[ChatMessage],
    context: Option<&ContextReport>,
    events: &EventBus,
) -> Result<(), CoreError> {
    let shares_memory = context.is_some_and(|report| {
        report.memory_tokens > 0 || report.facts_included > 0 || report.recalls_included > 0
    });
    let shares_captures = messages.iter().any(|message| !message.images.is_empty());
    routing::check_data_privacy(
        provider.descriptor(),
        &config.current().privacy,
        shares_memory,
        shares_captures,
    )
    .inspect_err(|err| {
        events.warning(
            EventCategory::Security,
            format!("Blocked a chat request: {err}"),
        );
    })
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
    config: Arc<ConfigStore>,
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
        if let Err(err) = authorize_chat_request(
            &self.config,
            self.provider.as_ref(),
            &self.request.messages,
            self.context.as_ref(),
            &self.events,
        ) {
            return Outcome::Failed(err);
        }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use jarvis_ai::{ChatStream, DataLocality, EchoProvider, ImageData, ProviderKind};
    use tokio::sync::{Notify, mpsc};

    #[derive(Debug, Default)]
    struct DiscoveryGate {
        entered: Notify,
        release: Notify,
    }

    #[derive(Debug)]
    struct PrivacyProvider {
        descriptor: ProviderDescriptor,
        streaming: bool,
        calls: AtomicUsize,
        capabilities_gate: Option<Arc<DiscoveryGate>>,
        catalog_gate: Option<Arc<DiscoveryGate>>,
    }

    impl PrivacyProvider {
        fn new(locality: DataLocality, streaming: bool) -> Self {
            Self {
                descriptor: ProviderDescriptor {
                    id: ProviderId::new("privacy-test"),
                    display_name: "Privacy test".into(),
                    kind: if locality == DataLocality::ThirdPartyCloud {
                        ProviderKind::Cloud
                    } else {
                        ProviderKind::LocalServer
                    },
                    locality,
                },
                streaming,
                calls: AtomicUsize::default(),
                capabilities_gate: None,
                catalog_gate: None,
            }
        }

        fn caps(&self) -> ModelCapabilities {
            ModelCapabilities {
                chat: true,
                streaming: self.streaming,
                vision: true,
                ..ModelCapabilities::default()
            }
        }
    }

    #[async_trait]
    impl AiProvider for PrivacyProvider {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }

        async fn health_check(&self) -> Result<(), AiError> {
            Ok(())
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
            if let Some(gate) = &self.catalog_gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            Ok(vec![ModelInfo {
                id: "private-model".into(),
                display_name: "Private model".into(),
                provider_id: self.descriptor.id.clone(),
                capabilities: self.caps(),
                context_window: Some(4096),
            }])
        }

        async fn capabilities(&self, _: &str) -> Result<ModelCapabilities, AiError> {
            if let Some(gate) = &self.capabilities_gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            Ok(self.caps())
        }

        async fn stream_chat(&self, _: ChatRequest) -> Result<ChatStream, AiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(futures::stream::iter([
                Ok(ChatChunk::Delta("allowed reply".into())),
                Ok(ChatChunk::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                }),
            ])
            .boxed())
        }
    }

    fn privacy_service(provider: Arc<PrivacyProvider>) -> (ChatService, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temporary settings");
        let registry = Arc::new(ProviderRegistry::new());
        registry
            .register(provider)
            .expect("register privacy provider");
        let mut settings = Settings::default();
        settings.privacy.allow_cloud_providers = true;
        settings.privacy.share_memory_with_cloud = true;
        settings.privacy.share_captures_with_cloud = true;
        let config = Arc::new(ConfigStore::in_memory(
            dir.path().join("config.toml"),
            settings,
        ));
        (
            ChatService::new(
                registry,
                Arc::default(),
                config,
                EventBus::default(),
                tokio::runtime::Handle::current(),
            ),
            dir,
        )
    }

    fn prepared_privacy_request(
        provider: Arc<PrivacyProvider>,
        shares_memory: bool,
        shares_captures: bool,
    ) -> PreparedChat {
        let message = ChatMessage::user("Private current conversation");
        let message = if shares_captures {
            message.with_images(vec![ImageData {
                media_type: "image/jpeg".into(),
                base64: "aW1hZ2U=".into(),
            }])
        } else {
            message
        };
        PreparedChat {
            model: ResolvedModel {
                descriptor: provider.descriptor.clone(),
                capabilities: provider.caps(),
                context_window: Some(4096),
                provider,
                model: "private-model".into(),
            },
            messages: vec![message],
            options: GenerationOptions::default(),
            conversation_id: None,
            message_id: None,
            context: Some(ContextReport {
                memory_tokens: u32::from(shares_memory),
                facts_included: u32::from(shares_memory),
                // Current conversation history and summary are independent of
                // the permission for recalled saved memory.
                history_tokens: 50,
                summary_tokens: 20,
                ..ContextReport::default()
            }),
        }
    }

    #[tokio::test]
    async fn cloud_permission_is_rechecked_after_explicit_automatic_and_vision_discovery() {
        for discovery in ["explicit", "automatic", "vision"] {
            let gate = Arc::new(DiscoveryGate::default());
            let mut provider = PrivacyProvider::new(DataLocality::ThirdPartyCloud, true);
            if discovery == "explicit" {
                provider.capabilities_gate = Some(Arc::clone(&gate));
            } else {
                provider.catalog_gate = Some(Arc::clone(&gate));
            }
            let provider = Arc::new(provider);
            let (chat, _dir) = privacy_service(Arc::clone(&provider));
            let chat = Arc::new(chat);
            let pending_chat = Arc::clone(&chat);
            let pending = tokio::spawn(async move {
                if discovery == "vision" {
                    pending_chat
                        .resolve_vision_model(None, None, &pending_chat.config.current())
                        .await
                        .map(|_| ())
                } else {
                    let mut req = request("Private message awaiting discovery");
                    if discovery == "explicit" {
                        req.provider_id = Some(ProviderId::new("privacy-test"));
                        req.model = Some("private-model".into());
                    }
                    let (sink, _rx) = channel_sink();
                    pending_chat.start(req, sink).await.map(|_| ())
                }
            });
            tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
                .await
                .expect("discovery entered");
            let mut settings = chat.config.current();
            settings.privacy.allow_cloud_providers = false;
            chat.config.update(settings).await.expect("revoke cloud");
            gate.release.notify_one();
            let err = tokio::time::timeout(Duration::from_secs(5), pending)
                .await
                .expect("discovery completed")
                .expect("request task")
                .expect_err("revoked privacy must fail");
            assert_eq!(
                err.code(),
                ErrorCode::PermissionDenied,
                "{discovery}: {err}"
            );
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
            assert_eq!(chat.active_requests(), 0);
        }
    }

    #[tokio::test]
    async fn prepared_cloud_requests_recheck_each_permission_against_the_held_provider() {
        use jarvis_ai::{ProviderBackend, ProviderConfig};

        for streaming in [false, true] {
            for permission in ["cloud", "memory", "captures"] {
                let provider = Arc::new(PrivacyProvider::new(
                    DataLocality::ThirdPartyCloud,
                    streaming,
                ));
                let (chat, _dir) = privacy_service(Arc::clone(&provider));
                let prepared = prepared_privacy_request(Arc::clone(&provider), true, true);
                let reservation = chat.reserve().expect("reserve");
                let mut settings = chat.config.current();
                // The same ID now describes a local replacement. The prepared
                // request still holds the original cloud adapter.
                settings.ai.providers = vec![ProviderConfig {
                    id: ProviderId::new("privacy-test"),
                    backend: ProviderBackend::Ollama,
                    display_name: "Local replacement".into(),
                    base_url: "http://127.0.0.1:11434".into(),
                    enabled: true,
                    native_tool_models: vec![],
                }];
                match permission {
                    "cloud" => settings.privacy.allow_cloud_providers = false,
                    "memory" => settings.privacy.share_memory_with_cloud = false,
                    _ => settings.privacy.share_captures_with_cloud = false,
                }
                chat.config
                    .update(settings)
                    .await
                    .expect("revoke permission");
                chat.registry
                    .replace_all(vec![Arc::new(PrivacyProvider::new(
                        DataLocality::OnDevice,
                        streaming,
                    ))])
                    .expect("replace provider");
                let (sink, rx) = channel_sink();
                chat.launch(reservation, prepared, sink);
                let events = collect(rx).await;
                assert!(matches!(
                    events.first(),
                    Some(ChatStreamEvent::Started { .. })
                ));
                assert!(
                    matches!(events.last(), Some(ChatStreamEvent::Failed { error, .. }) if error.code == ErrorCode::PermissionDenied)
                );
                assert_eq!(
                    events.len(),
                    2,
                    "streaming={streaming}, revoked={permission}"
                );
                assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
                assert_eq!(chat.active_requests(), 0);
            }
        }
    }

    #[tokio::test]
    async fn cloud_text_with_current_history_needs_no_saved_memory_or_capture_permission() {
        for streaming in [false, true] {
            let provider = Arc::new(PrivacyProvider::new(
                DataLocality::ThirdPartyCloud,
                streaming,
            ));
            let (chat, _dir) = privacy_service(Arc::clone(&provider));
            let mut settings = chat.config.current();
            settings.privacy.share_memory_with_cloud = false;
            settings.privacy.share_captures_with_cloud = false;
            chat.config.update(settings).await.expect("limit sharing");
            let prepared = prepared_privacy_request(Arc::clone(&provider), false, false);
            let (sink, rx) = channel_sink();
            chat.launch(chat.reserve().expect("reserve"), prepared, sink);
            let events = collect(rx).await;
            assert!(matches!(
                events.last(),
                Some(ChatStreamEvent::Finished { .. })
            ));
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
            assert_eq!(chat.active_requests(), 0);
        }
    }

    #[tokio::test]
    async fn local_and_lan_payloads_remain_allowed_with_all_cloud_sharing_disabled() {
        for locality in [DataLocality::OnDevice, DataLocality::LocalNetwork] {
            for streaming in [false, true] {
                let provider = Arc::new(PrivacyProvider::new(locality, streaming));
                let (chat, _dir) = privacy_service(Arc::clone(&provider));
                let mut settings = chat.config.current();
                settings.privacy = Default::default();
                chat.config
                    .update(settings)
                    .await
                    .expect("disable cloud sharing");
                let prepared = prepared_privacy_request(Arc::clone(&provider), true, true);
                let (sink, rx) = channel_sink();
                chat.launch(chat.reserve().expect("reserve"), prepared, sink);
                let events = collect(rx).await;
                assert!(matches!(
                    events.last(),
                    Some(ChatStreamEvent::Finished { .. })
                ));
                assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
                assert_eq!(chat.active_requests(), 0);
            }
        }
    }

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
