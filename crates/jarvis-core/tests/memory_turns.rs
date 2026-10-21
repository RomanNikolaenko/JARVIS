//! Conversation turns through the core: context assembly, persistence,
//! privacy rules, compaction, export/import and the action history.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use jarvis_ai::{
    AiError, AiProvider, ChatChunk, ChatRequest, ChatRole, ChatStream, DataLocality, FinishReason,
    ModelCapabilities, ModelInfo, ProviderDescriptor, ProviderId, ProviderKind,
};
use jarvis_core::jarvis_memory::{FactInput, MessageRole, MessageStatus, SearchQuery};
use jarvis_core::{
    AppPaths, ChatStreamEvent, ChatTurnRequest, CoreOptions, ErrorCode, JarvisCore, SinkClosed,
};
use tokio::sync::mpsc;
use uuid::Uuid;

/// Records every request and replies with a fixed text.
#[derive(Debug)]
struct Recorder {
    descriptor: ProviderDescriptor,
    window: Option<u32>,
    delay: Duration,
    requests: Mutex<Vec<ChatRequest>>,
}

impl Recorder {
    fn new(locality: DataLocality) -> Arc<Self> {
        Arc::new(Self {
            descriptor: ProviderDescriptor {
                id: ProviderId::new("recorder"),
                display_name: "Recorder".into(),
                kind: if locality == DataLocality::ThirdPartyCloud {
                    ProviderKind::Cloud
                } else {
                    ProviderKind::LocalServer
                },
                locality,
            },
            window: None,
            delay: Duration::ZERO,
            requests: Mutex::default(),
        })
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// System prompt of the last chat request (not a summary request).
    fn last_system_prompt(&self) -> String {
        self.requests()
            .iter()
            .rev()
            .find(|r| !r.messages[0].content.contains("running summary"))
            .map(|r| r.messages[0].content.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl AiProvider for Recorder {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn health_check(&self) -> Result<(), AiError> {
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(vec![ModelInfo {
            id: "recorder-model".into(),
            display_name: "Recorder model".into(),
            provider_id: self.descriptor.id.clone(),
            context_window: self.window,
            capabilities: ModelCapabilities {
                chat: true,
                streaming: true,
                ..ModelCapabilities::default()
            },
        }])
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError> {
        let summary = request.messages[0].content.contains("running summary");
        self.requests.lock().unwrap().push(request);
        let delay = self.delay;
        let reply = if summary {
            "Summary: the user asked many questions."
        } else {
            "<think>pondering</think>Recorded reply"
        };
        Ok(stream::once(async move {
            tokio::time::sleep(delay).await;
            Ok(ChatChunk::Delta(reply.to_owned()))
        })
        .chain(stream::once(async {
            Ok(ChatChunk::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            })
        }))
        .boxed())
    }
}

struct TestCore {
    core: Arc<JarvisCore>,
    paths: AppPaths,
    _dir: tempfile::TempDir,
}

async fn start(provider: Arc<Recorder>) -> TestCore {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        log_dir: dir.path().join("logs"),
    };
    let core = restart(&paths, provider).await;
    TestCore {
        core,
        paths,
        _dir: dir,
    }
}

async fn restart(paths: &AppPaths, provider: Arc<Recorder>) -> Arc<JarvisCore> {
    JarvisCore::initialize(
        CoreOptions::new(paths.clone(), "9.9.9")
            .with_static_providers(vec![provider])
            .with_provider_factory(None),
    )
    .await
    .unwrap()
}

fn turn(conversation_id: Uuid, text: &str) -> ChatTurnRequest {
    ChatTurnRequest {
        conversation_id,
        content: text.to_owned(),
        provider_id: None,
        model: None,
        temperature: None,
        max_output_tokens: None,
        speak: false,
    }
}

/// Sends a message and returns its events once the reply has been saved.
async fn send(core: &JarvisCore, conversation_id: Uuid, text: &str) -> Vec<ChatStreamEvent> {
    let before = core
        .memory()
        .conversation(conversation_id)
        .await
        .unwrap()
        .messages
        .len();
    let (tx, mut rx) = mpsc::unbounded_channel();
    core.send_message(turn(conversation_id, text), move |event| {
        tx.send(event).map_err(|_| SinkClosed)
    })
    .await
    .unwrap();
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    // The reply is saved right after the stream ends.
    for _ in 0..100 {
        let count = core
            .memory()
            .conversation(conversation_id)
            .await
            .unwrap()
            .messages
            .len();
        if count >= before + 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    events
}

fn fact(content: &str, pinned: bool) -> FactInput {
    FactInput {
        id: None,
        content: content.to_owned(),
        project_id: None,
        pinned,
        source_conversation_id: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn turns_are_saved_and_survive_a_restart() {
    let provider = Recorder::new(DataLocality::OnDevice);
    let test = start(Arc::clone(&provider)).await;
    let conversation = test.core.create_conversation(None, false).await.unwrap();

    let events = send(&test.core, conversation.id, "Hello JARVIS").await;
    let ChatStreamEvent::Started {
        conversation_id,
        message_id,
        context,
        ..
    } = &events[0]
    else {
        panic!("first event is started: {events:?}");
    };
    assert_eq!(*conversation_id, Some(conversation.id));
    let context = context.as_ref().expect("context report");
    assert_eq!(context.window_tokens, 4096, "default window");
    assert_eq!(context.history_included, 0);
    assert!(matches!(
        events.last(),
        Some(ChatStreamEvent::Finished { .. })
    ));

    send(&test.core, conversation.id, "And again").await;
    let request = provider.requests().pop().unwrap();
    assert_eq!(request.messages[0].role, ChatRole::System);
    assert!(request.messages[0].content.contains("You are JARVIS"));
    assert!(request.messages[0].content.contains("Answer in Ukrainian"));
    // Earlier turns are sent, reasoning is stripped.
    assert_eq!(request.messages[1].content, "Hello JARVIS");
    assert_eq!(request.messages[2].content, "Recorded reply");
    assert_eq!(request.messages[3].content, "And again");

    test.core.shutdown().await;
    let core = restart(&test.paths, provider).await;
    let list = core.memory().list_conversations(None, 10).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "Hello JARVIS");
    let detail = core.memory().conversation(conversation.id).await.unwrap();
    assert_eq!(detail.messages.len(), 4);
    assert_eq!(detail.messages[1].role, MessageRole::Assistant);
    assert_eq!(Some(detail.messages[1].id), *message_id);
    assert_eq!(detail.messages[1].status, MessageStatus::Complete);
    assert_eq!(detail.messages[1].model.as_deref(), Some("recorder-model"));
    // The raw reply (with reasoning) is kept for display.
    assert!(detail.messages[1].content.starts_with("<think>"));
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn facts_and_other_conversations_reach_local_models() {
    let provider = Recorder::new(DataLocality::OnDevice);
    let test = start(Arc::clone(&provider)).await;
    test.core
        .save_fact(fact("The user's name is Roman.", true))
        .await
        .unwrap();
    let earlier = test.core.create_conversation(None, false).await.unwrap();
    send(
        &test.core,
        earlier.id,
        "Remember that the Atlas launch is in March",
    )
    .await;

    let current = test.core.create_conversation(None, false).await.unwrap();
    let events = send(&test.core, current.id, "When is the Atlas launch?").await;
    let system = provider.last_system_prompt();
    assert!(system.contains("Roman"), "{system}");
    assert!(system.contains("Atlas launch is in March"), "{system}");
    let ChatStreamEvent::Started { context, .. } = &events[0] else {
        panic!("started");
    };
    let context = context.as_ref().unwrap();
    assert_eq!(context.facts_included, 1);
    assert_eq!(context.recalls_included, 1);
    assert!(context.memory_skipped.is_none());

    let hits = test
        .core
        .search_memory(SearchQuery {
            text: "atlas".into(),
            kinds: vec![],
            limit: 10,
        })
        .await
        .unwrap();
    assert!(hits.len() >= 2);
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_memory_is_not_shared_with_cloud_models() {
    let provider = Recorder::new(DataLocality::ThirdPartyCloud);
    let test = start(Arc::clone(&provider)).await;
    let mut settings = test.core.settings();
    settings.privacy.allow_cloud_providers = true;
    test.core.update_settings(settings).await.unwrap();
    test.core
        .save_fact(fact("The user's name is Roman.", true))
        .await
        .unwrap();

    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let events = send(&test.core, conversation.id, "What is my name?").await;
    assert!(!provider.last_system_prompt().contains("Roman"));
    let ChatStreamEvent::Started { context, .. } = &events[0] else {
        panic!("started");
    };
    assert!(context.as_ref().unwrap().memory_skipped.is_some());

    let mut settings = test.core.settings();
    settings.privacy.share_memory_with_cloud = true;
    test.core.update_settings(settings).await.unwrap();
    send(&test.core, conversation.id, "What is my name now?").await;
    assert!(provider.last_system_prompt().contains("Roman"));
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_saving_off_nothing_is_written() {
    let provider = Recorder::new(DataLocality::OnDevice);
    let test = start(Arc::clone(&provider)).await;
    let saved = test.core.create_conversation(None, false).await.unwrap();
    let mut settings = test.core.settings();
    settings.memory.enabled = false;
    test.core.update_settings(settings).await.unwrap();

    let conversation = test.core.create_conversation(None, false).await.unwrap();
    assert!(conversation.temporary, "new chats are temporary");
    send(&test.core, conversation.id, "private thought").await;
    let counts = test.core.memory().counts().await.unwrap();
    assert_eq!(counts.messages, 0);
    assert_eq!(counts.conversations, 1, "only the chat saved before");

    let err = test
        .core
        .save_fact(fact("x", false))
        .await
        .expect_err("saving is off");
    assert_eq!(err.code(), ErrorCode::PermissionDenied);
    let (tx, _rx) = mpsc::unbounded_channel();
    let err = test
        .core
        .send_message(turn(saved.id, "continue"), move |event| {
            tx.send(event).map_err(|_| SinkClosed)
        })
        .await
        .expect_err("saved conversations do not continue");
    assert_eq!(err.code(), ErrorCode::PermissionDenied);

    test.core.shutdown().await;
    assert!(
        test.core
            .memory()
            .list_episodes(100, None)
            .await
            .unwrap()
            .is_empty(),
        "no action history while saving is off"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn long_conversations_are_summarized() {
    let provider = Arc::new(Recorder {
        window: Some(1024),
        ..Arc::try_unwrap(Recorder::new(DataLocality::OnDevice)).unwrap()
    });
    let test = start(Arc::clone(&provider)).await;
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let long = "Tell me more about the history of the project. ".repeat(12);
    for i in 0..8 {
        send(&test.core, conversation.id, &format!("{i}: {long}")).await;
    }
    let mut summary = None;
    for _ in 0..100 {
        summary = test
            .core
            .memory()
            .conversation(conversation.id)
            .await
            .unwrap()
            .summary;
        if summary.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let summary = summary.expect("a summary was written");
    assert!(summary.content.starts_with("Summary:"));
    assert!(summary.covered_messages >= 2);
    assert_eq!(summary.model.as_deref(), Some("recorder-model"));

    let events = send(&test.core, conversation.id, "What did we discuss?").await;
    let ChatStreamEvent::Started { context, .. } = &events[0] else {
        panic!("started");
    };
    let context = context.as_ref().unwrap();
    assert!(context.summarized_messages >= 2, "{context:?}");
    assert!(context.used_tokens <= context.budget_tokens);
    assert!(provider.last_system_prompt().contains("Summary:"));
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_reply_at_a_time_per_conversation() {
    let provider = Arc::new(Recorder {
        delay: Duration::from_millis(500),
        ..Arc::try_unwrap(Recorder::new(DataLocality::OnDevice)).unwrap()
    });
    let test = start(provider).await;
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    test.core
        .send_message(turn(conversation.id, "first"), move |event| {
            tx.send(event).map_err(|_| SinkClosed)
        })
        .await
        .unwrap();
    let (tx2, _rx2) = mpsc::unbounded_channel();
    let err = test
        .core
        .send_message(turn(conversation.id, "second"), move |event| {
            tx2.send(event).map_err(|_| SinkClosed)
        })
        .await
        .expect_err("busy");
    assert_eq!(err.code(), ErrorCode::LimitExceeded);
    let err = test
        .core
        .delete_conversation(conversation.id)
        .await
        .expect_err("busy");
    assert_eq!(err.code(), ErrorCode::LimitExceeded);
    while rx.recv().await.is_some() {}
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn export_clear_import_and_action_history() {
    let provider = Recorder::new(DataLocality::OnDevice);
    let test = start(Arc::clone(&provider)).await;
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    send(&test.core, conversation.id, "Remember the launch").await;
    test.core
        .save_fact(fact("Launch is in March", false))
        .await
        .unwrap();
    let before = test.core.memory().counts().await.unwrap();

    let file = test.paths.data_dir.join("export.json");
    let exported = test.core.export_memory(&file).await.unwrap();
    assert_eq!(exported.counts.conversations, 1);
    assert_eq!(exported.counts.messages, 2);
    assert!(file.is_file());

    test.core.clear_memory().await.unwrap();
    assert_eq!(test.core.memory().counts().await.unwrap().messages, 0);
    let imported = test.core.import_memory(&file).await.unwrap();
    assert_eq!(imported.counts.messages, 2);
    let after = test.core.memory().counts().await.unwrap();
    assert_eq!(after.conversations, before.conversations);
    assert_eq!(after.facts, before.facts);

    std::fs::write(&file, b"{ not json").unwrap();
    let err = test.core.import_memory(&file).await.expect_err("invalid");
    assert_eq!(err.code(), ErrorCode::Validation);

    // The action history is flushed on shutdown.
    test.core.shutdown().await;
    let episodes = test.core.memory().list_episodes(100, None).await.unwrap();
    let summaries: Vec<&str> = episodes.iter().map(|e| e.summary.as_str()).collect();
    assert!(
        summaries.iter().any(|s| s.contains("fact was saved")),
        "{summaries:?}"
    );
    assert!(
        summaries.iter().any(|s| s.contains("Memory imported")),
        "{summaries:?}"
    );
    assert!(
        summaries.iter().any(|s| s.contains("completed")),
        "{summaries:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn actions_right_before_shutdown_are_kept() {
    let test = start(Recorder::new(DataLocality::OnDevice)).await;
    test.core
        .events()
        .info(jarvis_events::EventCategory::Runtime, "last action");
    test.core.shutdown().await;
    let episodes = test.core.memory().list_episodes(10, None).await.unwrap();
    let summaries: Vec<&str> = episodes.iter().map(|e| e.summary.as_str()).collect();
    assert!(summaries.contains(&"last action"), "{summaries:?}");
    assert!(
        summaries.contains(&"JARVIS core shutting down"),
        "{summaries:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn temporary_chats_are_forgotten() {
    let provider = Recorder::new(DataLocality::OnDevice);
    let test = start(Arc::clone(&provider)).await;
    let conversation = test.core.create_conversation(None, true).await.unwrap();
    send(&test.core, conversation.id, "a secret").await;
    send(&test.core, conversation.id, "second").await;
    let request = provider.requests().pop().unwrap();
    assert_eq!(request.messages[1].content, "a secret", "history is used");
    assert_eq!(test.core.memory().counts().await.unwrap().messages, 0);
    test.core.shutdown().await;
    let core = restart(&test.paths, provider).await;
    assert!(
        core.memory()
            .list_conversations(None, 10)
            .await
            .unwrap()
            .is_empty()
    );
    core.shutdown().await;
}

/// A message sent as soon as the previous reply has streamed waits for that
/// reply to be saved instead of being refused, and the history stays in
/// order. (The reply is saved after its last event reaches the client.)
#[tokio::test(flavor = "multi_thread")]
async fn a_message_right_after_a_reply_waits_for_it_to_be_saved() {
    let test = start(Recorder::new(DataLocality::OnDevice)).await;
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    for i in 0..5 {
        let (tx, mut rx) = mpsc::unbounded_channel();
        test.core
            .send_message(
                turn(conversation.id, &format!("message {i}")),
                move |event| tx.send(event).map_err(|_| SinkClosed),
            )
            .await
            .unwrap_or_else(|err| panic!("message {i} was refused: {err}"));
        while rx.recv().await.is_some() {}
    }
    let mut messages = Vec::new();
    for _ in 0..100 {
        messages = test
            .core
            .memory()
            .conversation(conversation.id)
            .await
            .unwrap()
            .messages;
        if messages.len() == 10 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let roles: Vec<MessageRole> = messages.iter().map(|m| m.role).collect();
    let expected: Vec<MessageRole> = (0..5)
        .flat_map(|_| [MessageRole::User, MessageRole::Assistant])
        .collect();
    assert_eq!(roles, expected);
    test.core.shutdown().await;
}
