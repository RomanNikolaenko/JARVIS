//! Facts suggested from conversations: the pause, the request, the review
//! and the privacy rules, through the core with a scripted model.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use jarvis_ai::{
    AiError, AiProvider, ChatChunk, ChatRequest, ChatStream, DataLocality, FinishReason,
    ModelCapabilities, ModelInfo, ProviderDescriptor, ProviderId, ProviderKind,
};
use jarvis_core::jarvis_memory::AcceptFactSuggestion;
use jarvis_core::{AppPaths, ChatTurnRequest, CoreOptions, JarvisCore, SinkClosed};
use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

const PAUSE: Duration = Duration::from_millis(300);
/// For tests that save settings while a check is due (a file write that a
/// busy CI machine may stretch past a short pause).
const SLOW_PAUSE: Duration = Duration::from_millis(1500);
const EXTRACTION_MARK: &str = "the user stated about themselves";

#[derive(Debug, Default)]
struct Gate {
    entered: Notify,
    release: Notify,
}

/// Answers chat turns with a fixed text and fact requests with a script.
#[derive(Debug)]
struct Scripted {
    descriptor: ProviderDescriptor,
    requests: Mutex<Vec<ChatRequest>>,
    facts_reply: Mutex<String>,
    facts_fail: Mutex<bool>,
    /// Fact replies end at the output limit.
    facts_cut: Mutex<bool>,
    facts_gate: Mutex<Option<Arc<Gate>>>,
    /// Holds the model lookup of a chat turn.
    capability_gate: Mutex<Option<Arc<Gate>>>,
}

impl Scripted {
    fn new(locality: DataLocality) -> Arc<Self> {
        Arc::new(Self {
            descriptor: ProviderDescriptor {
                id: ProviderId::new("scripted"),
                display_name: "Scripted".into(),
                kind: if locality == DataLocality::ThirdPartyCloud {
                    ProviderKind::Cloud
                } else {
                    ProviderKind::LocalServer
                },
                locality,
            },
            requests: Mutex::default(),
            facts_reply: Mutex::new("{\"facts\": []}".to_owned()),
            facts_fail: Mutex::new(false),
            facts_cut: Mutex::new(false),
            facts_gate: Mutex::default(),
            capability_gate: Mutex::default(),
        })
    }

    fn reply_with(&self, reply: &str) {
        *self.facts_reply.lock().unwrap() = reply.to_owned();
    }

    /// Fact requests so far, as the text of their user message.
    fn extractions(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.messages[0].content.contains(EXTRACTION_MARK))
            .map(|r| r.messages[1].content.clone())
            .collect()
    }
}

#[async_trait]
impl AiProvider for Scripted {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn health_check(&self) -> Result<(), AiError> {
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(vec![ModelInfo {
            id: "scripted-model".into(),
            display_name: "Scripted model".into(),
            provider_id: self.descriptor.id.clone(),
            context_window: None,
            capabilities: ModelCapabilities {
                chat: true,
                streaming: true,
                ..ModelCapabilities::default()
            },
        }])
    }

    async fn capabilities(&self, model: &str) -> Result<ModelCapabilities, AiError> {
        let gate = self.capability_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        self.list_models()
            .await?
            .into_iter()
            .find(|info| info.id == model)
            .map(|info| info.capabilities)
            .ok_or_else(|| AiError::ModelNotFound {
                model: model.to_owned(),
            })
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError> {
        let extraction = request.messages[0].content.contains(EXTRACTION_MARK);
        self.requests.lock().unwrap().push(request);
        if !extraction {
            return Ok(chunks(
                "Recorded reply".to_owned(),
                None,
                FinishReason::Stop,
            ));
        }
        if *self.facts_fail.lock().unwrap() {
            return Err(AiError::Timeout);
        }
        let gate = self.facts_gate.lock().unwrap().take();
        let finish = if *self.facts_cut.lock().unwrap() {
            FinishReason::Length
        } else {
            FinishReason::Stop
        };
        Ok(chunks(
            self.facts_reply.lock().unwrap().clone(),
            gate,
            finish,
        ))
    }
}

fn chunks(text: String, gate: Option<Arc<Gate>>, finish_reason: FinishReason) -> ChatStream {
    stream::once(async move {
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        Ok(ChatChunk::Delta(text))
    })
    .chain(stream::once(async move {
        Ok(ChatChunk::Done {
            finish_reason,
            usage: None,
        })
    }))
    .boxed()
}

struct TestCore {
    core: Arc<JarvisCore>,
    _dir: tempfile::TempDir,
}

async fn start(provider: Arc<Scripted>, suggest: bool) -> TestCore {
    start_with(provider, suggest, PAUSE).await
}

/// A longer pause for tests that change settings while a check is due:
/// saving settings must not race the pause on a busy machine.
async fn start_with(provider: Arc<Scripted>, suggest: bool, pause: Duration) -> TestCore {
    let dir = tempfile::tempdir().unwrap();
    let core = open(dir.path(), provider, pause).await;
    set_suggest(&core, suggest).await;
    TestCore { core, _dir: dir }
}

async fn open(dir: &Path, provider: Arc<Scripted>, pause: Duration) -> Arc<JarvisCore> {
    let paths = AppPaths {
        config_dir: dir.join("config"),
        data_dir: dir.join("data"),
        log_dir: dir.join("logs"),
    };
    JarvisCore::initialize(
        CoreOptions::new(paths, "9.9.9")
            .with_static_providers(vec![provider])
            .with_provider_factory(None)
            .with_fact_suggestion_pause(pause),
    )
    .await
    .unwrap()
}

async fn set_suggest(core: &JarvisCore, on: bool) {
    let mut settings = core.settings();
    settings.memory.suggest_facts = on;
    core.update_settings(settings).await.unwrap();
}

/// Sends a message and returns once the reply is saved.
async fn send(core: &JarvisCore, conversation_id: Uuid, text: &str) {
    let before = core
        .memory()
        .conversation(conversation_id)
        .await
        .unwrap()
        .messages
        .len();
    let (tx, mut rx) = mpsc::unbounded_channel();
    core.send_message(
        ChatTurnRequest {
            conversation_id,
            content: text.to_owned(),
            provider_id: None,
            model: None,
            temperature: None,
            max_output_tokens: None,
            speak: false,
            attachments: Vec::new(),
        },
        move |event| tx.send(event).map_err(|_| SinkClosed),
    )
    .await
    .unwrap();
    while rx.recv().await.is_some() {}
    for _ in 0..200 {
        let count = core
            .memory()
            .conversation(conversation_id)
            .await
            .unwrap()
            .messages
            .len();
        if count >= before + 2 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the reply was not saved");
}

async fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..300 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn facts_are_suggested_after_a_pause_and_accepted_on_review() {
    let provider = Scripted::new(DataLocality::OnDevice);
    provider.reply_with(
        "{\"facts\": [\"Користувач працює вчителем.\", \"Користувач має кота Мурчика.\"]}",
    );
    let test = start(Arc::clone(&provider), true).await;
    let mut count = test.core.subscribe_fact_suggestions();
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    send(
        &test.core,
        conversation.id,
        "Я працюю вчителем і маю кота Мурчика.",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(provider.extractions().is_empty(), "not before the pause");

    tokio::time::timeout(Duration::from_secs(3), count.wait_for(|n| *n == 2))
        .await
        .expect("suggestions published")
        .unwrap();
    let request = &provider.extractions()[0];
    assert!(
        request.contains("User: Я працюю вчителем і маю кота Мурчика."),
        "{request}"
    );
    assert!(
        request.contains("JARVIS (context only): Recorded reply"),
        "{request}"
    );
    let suggestions = test.core.list_fact_suggestions().await.unwrap();
    assert_eq!(suggestions.len(), 2);
    assert!(test.core.memory().list_facts().await.unwrap().is_empty());

    let teacher = suggestions
        .iter()
        .find(|s| s.content.contains("вчителем"))
        .unwrap();
    let fact = test
        .core
        .accept_fact_suggestion(AcceptFactSuggestion {
            id: teacher.id,
            content: teacher.content.clone(),
            pinned: false,
            project_id: None,
        })
        .await
        .unwrap();
    assert_eq!(test.core.memory().list_facts().await.unwrap(), vec![fact]);
    assert_eq!(*count.borrow(), 1);
    let cat = suggestions.iter().find(|s| s.id != teacher.id).unwrap();
    test.core.reject_fact_suggestion(cat.id).await.unwrap();
    assert_eq!(*count.borrow(), 0);
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn activity_postpones_the_check() {
    let provider = Scripted::new(DataLocality::OnDevice);
    let test = start(Arc::clone(&provider), true).await;
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    send(&test.core, conversation.id, "Я живу у Львові.").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    send(&test.core, conversation.id, "І працюю програмістом.").await;
    let second = Instant::now();
    wait_for("the fact request", || !provider.extractions().is_empty()).await;
    assert!(
        second.elapsed() >= Duration::from_millis(250),
        "{:?}",
        second.elapsed()
    );
    let requests = provider.extractions();
    assert_eq!(requests.len(), 1, "one request for both messages");
    assert!(requests[0].contains("Я живу у Львові.") && requests[0].contains("програмістом"));
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_checked_when_off_temporary_or_unchanged() {
    let provider = Scripted::new(DataLocality::OnDevice);
    let test = start(Arc::clone(&provider), false).await;
    let saved = test.core.create_conversation(None, false).await.unwrap();
    send(&test.core, saved.id, "Я маю собаку.").await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(provider.extractions().is_empty(), "the setting is off");

    set_suggest(&test.core, true).await;
    let temporary = test.core.create_conversation(None, true).await.unwrap();
    send(&test.core, temporary.id, "Я маю папугу.").await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        provider.extractions().is_empty(),
        "temporary chats are skipped"
    );

    send(&test.core, saved.id, "Собаку звати Рекс.").await;
    wait_for("the fact request", || provider.extractions().len() == 1).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(provider.extractions().len(), 1, "nothing new to look at");
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_the_setting_off_discards_a_due_check() {
    let provider = Scripted::new(DataLocality::OnDevice);
    provider.reply_with("{\"facts\": [\"Користувач має кота.\"]}");
    let test = start_with(Arc::clone(&provider), true, SLOW_PAUSE).await;
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    send(&test.core, conversation.id, "Я маю кота.").await;
    set_suggest(&test.core, false).await;
    tokio::time::sleep(SLOW_PAUSE + Duration::from_millis(700)).await;
    assert!(provider.extractions().is_empty());

    set_suggest(&test.core, true).await;
    let gate = Arc::new(Gate::default());
    *provider.facts_gate.lock().unwrap() = Some(Arc::clone(&gate));
    send(&test.core, conversation.id, "Кота звати Мурчик.").await;
    tokio::time::timeout(SLOW_PAUSE + Duration::from_secs(3), gate.entered.notified())
        .await
        .expect("the fact request is in flight");
    set_suggest(&test.core, false).await;
    gate.release.notify_one();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(test.core.list_fact_suggestions().await.unwrap().is_empty());
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cloud_models_need_chat_permission() {
    let provider = Scripted::new(DataLocality::ThirdPartyCloud);
    let test = start_with(Arc::clone(&provider), true, SLOW_PAUSE).await;
    let mut settings = test.core.settings();
    settings.privacy.allow_cloud_providers = true;
    test.core.update_settings(settings).await.unwrap();
    let mut events = test.core.events().subscribe();
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    send(&test.core, conversation.id, "Я працюю лікарем.").await;
    let mut settings = test.core.settings();
    settings.privacy.allow_cloud_providers = false;
    test.core.update_settings(settings).await.unwrap();
    tokio::time::timeout(SLOW_PAUSE + Duration::from_secs(3), async {
        loop {
            let event = events.recv().await.expect("event");
            if event.message.starts_with("Blocked fact suggestions:") {
                break;
            }
        }
    })
    .await
    .expect("the refusal is reported");
    assert!(
        provider.extractions().is_empty(),
        "nothing reached the cloud"
    );
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_move_the_cursor_and_errors_keep_it() {
    let provider = Scripted::new(DataLocality::OnDevice);
    provider.reply_with("not json");
    let test = start(Arc::clone(&provider), true).await;
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    send(&test.core, conversation.id, "Перше повідомлення.").await;
    wait_for("the first request", || provider.extractions().len() == 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(test.core.list_fact_suggestions().await.unwrap().is_empty());

    *provider.facts_fail.lock().unwrap() = true;
    send(&test.core, conversation.id, "Друге повідомлення.").await;
    wait_for("the failed request", || provider.extractions().len() == 2).await;
    let failed = &provider.extractions()[1];
    assert!(
        !failed.contains("Перше"),
        "a garbled answer moved the cursor: {failed}"
    );

    *provider.facts_fail.lock().unwrap() = false;
    send(&test.core, conversation.id, "Третє повідомлення.").await;
    wait_for("the retry", || provider.extractions().len() == 3).await;
    let retry = &provider.extractions()[2];
    assert!(
        retry.contains("Друге повідомлення.") && retry.contains("Третє повідомлення."),
        "an error kept the cursor: {retry}"
    );
    test.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_the_conversation_or_clearing_memory_drops_suggestions() {
    let provider = Scripted::new(DataLocality::OnDevice);
    provider.reply_with("{\"facts\": [\"Користувач має кота.\"]}");
    let test = start(Arc::clone(&provider), true).await;
    let mut count = test.core.subscribe_fact_suggestions();

    let gone = test.core.create_conversation(None, false).await.unwrap();
    let gate = Arc::new(Gate::default());
    *provider.facts_gate.lock().unwrap() = Some(Arc::clone(&gate));
    send(&test.core, gone.id, "Я маю кота.").await;
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .expect("the fact request is in flight");
    test.core.delete_conversation(gone.id).await.unwrap();
    gate.release.notify_one();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(test.core.list_fact_suggestions().await.unwrap().is_empty());

    let kept = test.core.create_conversation(None, false).await.unwrap();
    send(&test.core, kept.id, "Я маю кота.").await;
    tokio::time::timeout(Duration::from_secs(3), count.wait_for(|n| *n == 1))
        .await
        .expect("one suggestion")
        .unwrap();
    test.core.clear_memory().await.unwrap();
    assert_eq!(*count.borrow(), 0);
    test.core.shutdown().await;
}

// Review finding: on a one-slot local server a running check made the next
// chat turn wait; activity now cancels it and the conversation is due again.
#[tokio::test(flavor = "multi_thread")]
async fn chat_activity_cancels_a_running_check_and_it_runs_again_later() {
    let provider = Scripted::new(DataLocality::OnDevice);
    provider.reply_with("{\"facts\": [\"Користувач має кота і собаку.\"]}");
    let test = start(Arc::clone(&provider), true).await;
    let mut count = test.core.subscribe_fact_suggestions();
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let gate = Arc::new(Gate::default());
    *provider.facts_gate.lock().unwrap() = Some(Arc::clone(&gate));
    send(&test.core, conversation.id, "Я маю кота.").await;
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .expect("the check is in flight");
    // The held check is never released: only the cancellation ends it.
    send(&test.core, conversation.id, "І собаку.").await;
    wait_for("the check to run again", || {
        provider.extractions().len() == 2
    })
    .await;
    let again = &provider.extractions()[1];
    assert!(
        again.contains("Я маю кота.") && again.contains("І собаку."),
        "the cancelled messages are looked at again: {again}"
    );
    tokio::time::timeout(Duration::from_secs(3), count.wait_for(|n| *n == 1))
        .await
        .expect("the second check keeps its suggestion")
        .unwrap();
    test.core.shutdown().await;
}

// Review finding: due conversations lived only in memory, so what was said
// right before closing JARVIS was never looked at.
#[tokio::test(flavor = "multi_thread")]
async fn recent_unchecked_conversations_are_looked_at_after_a_restart() {
    let provider = Scripted::new(DataLocality::OnDevice);
    let dir = tempfile::tempdir().unwrap();
    // JARVIS closes before the pause is over.
    let core = open(dir.path(), Arc::clone(&provider), Duration::from_secs(60)).await;
    set_suggest(&core, true).await;
    let conversation = core.create_conversation(None, false).await.unwrap();
    send(&core, conversation.id, "Я живу в Одесі.").await;
    core.shutdown().await;
    drop(core);
    assert!(provider.extractions().is_empty());

    let core = open(dir.path(), Arc::clone(&provider), PAUSE).await;
    wait_for("the check after the restart", || {
        provider.extractions().len() == 1
    })
    .await;
    assert!(provider.extractions()[0].contains("User: Я живу в Одесі."));
    tokio::time::sleep(Duration::from_millis(300)).await;
    core.shutdown().await;
    drop(core);

    let core = open(dir.path(), Arc::clone(&provider), PAUSE).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(provider.extractions().len(), 1, "looked at once");
    core.shutdown().await;
}

// Review finding #11: a turn that held its conversation past the pause and
// then failed released it without any activity, so due checks waited for
// the next message.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_turn_lets_due_checks_run() {
    let provider = Scripted::new(DataLocality::OnDevice);
    let test = start(Arc::clone(&provider), true).await;
    let due = test.core.create_conversation(None, false).await.unwrap();
    send(&test.core, due.id, "Я маю кота.").await;

    let other = test.core.create_conversation(None, false).await.unwrap();
    let gate = Arc::new(Gate::default());
    *provider.capability_gate.lock().unwrap() = Some(Arc::clone(&gate));
    let core = Arc::clone(&test.core);
    let failing = tokio::spawn(async move {
        core.send_message(
            ChatTurnRequest {
                conversation_id: other.id,
                content: "Привіт".to_owned(),
                provider_id: Some(ProviderId::new("scripted")),
                model: Some("missing-model".to_owned()),
                temperature: None,
                max_output_tokens: None,
                speak: false,
                attachments: Vec::new(),
            },
            |_| Ok(()),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .expect("the turn holds its conversation");
    // The pause ends while the turn is still running.
    tokio::time::sleep(PAUSE * 2).await;
    assert!(provider.extractions().is_empty());
    gate.release.notify_one();
    assert!(failing.await.unwrap().is_err(), "the model does not exist");
    wait_for("the due check after the failed turn", || {
        provider.extractions().len() == 1
    })
    .await;
    test.core.shutdown().await;
}

// Review finding #5: a reply cut at the output limit (a reasoning model may
// spend all of it inside <think>) moved the cursor past messages that were
// never looked at.
#[tokio::test(flavor = "multi_thread")]
async fn a_reply_cut_at_the_limit_keeps_the_cursor() {
    let provider = Scripted::new(DataLocality::OnDevice);
    provider.reply_with("<think>Let me look at every message of the user");
    *provider.facts_cut.lock().unwrap() = true;
    let test = start(Arc::clone(&provider), true).await;
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    send(&test.core, conversation.id, "Я маю кота.").await;
    wait_for("the cut check", || provider.extractions().len() == 1).await;

    *provider.facts_cut.lock().unwrap() = false;
    provider.reply_with("{\"facts\": [\"Користувач має кота і собаку.\"]}");
    send(&test.core, conversation.id, "І собаку.").await;
    wait_for("the next check", || provider.extractions().len() == 2).await;
    let next = &provider.extractions()[1];
    assert!(
        next.contains("Я маю кота."),
        "the cut check kept the cursor: {next}"
    );
    test.core.shutdown().await;
}
