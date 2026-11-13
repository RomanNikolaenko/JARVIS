//! Mentor mode through the core: sessions, turns with project context,
//! confirmed typing and speaking up on save, with fake audio and models.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jarvis_ai::{
    AiError, AiProvider, ChatChunk, ChatRequest, ChatStream, DataLocality, FinishReason,
    ModelCapabilities, ModelInfo, ProviderDescriptor, ProviderId, ProviderKind,
};
use jarvis_core::jarvis_voice::testing::{self, FakeBackend};
use jarvis_core::{AppPaths, CoreOptions, ErrorCode, JarvisCore, MentorPhase, MentorStartRequest};
use uuid::Uuid;

/// A model that answers with scripted replies and records what it saw.
#[derive(Debug)]
struct Teacher {
    descriptor: ProviderDescriptor,
    replies: std::sync::Mutex<Vec<String>>,
    seen: std::sync::Mutex<Vec<ChatRequest>>,
}

impl Teacher {
    fn new(id: &str, locality: DataLocality) -> Arc<Self> {
        Arc::new(Self {
            descriptor: ProviderDescriptor {
                id: ProviderId::new(id),
                display_name: id.to_owned(),
                kind: if locality == DataLocality::ThirdPartyCloud {
                    ProviderKind::Cloud
                } else {
                    ProviderKind::LocalServer
                },
                locality,
            },
            replies: std::sync::Mutex::default(),
            seen: std::sync::Mutex::default(),
        })
    }
}

#[async_trait]
impl AiProvider for Teacher {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }
    async fn health_check(&self) -> Result<(), AiError> {
        Ok(())
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(vec![ModelInfo {
            id: "teacher".into(),
            display_name: "Teacher".into(),
            provider_id: self.descriptor.id.clone(),
            context_window: Some(8192),
            capabilities: ModelCapabilities {
                chat: true,
                streaming: true,
                vision: true,
                ..ModelCapabilities::default()
            },
        }])
    }
    async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError> {
        self.seen.lock().unwrap().push(request);
        let reply = {
            let mut replies = self.replies.lock().unwrap();
            if replies.is_empty() {
                "Добре.".to_owned()
            } else {
                replies.remove(0)
            }
        };
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(ChatChunk::Delta(reply)),
            Ok(ChatChunk::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            }),
        ])))
    }
}

struct TestCore {
    core: Arc<JarvisCore>,
    teacher: Arc<Teacher>,
    fake: FakeBackend,
    project: std::path::PathBuf,
    dir: tempfile::TempDir,
}

/// A core with voice "installed" (unless `voice` is false), a teacher model
/// and a project folder inside a workspace folder. `None` on platforms
/// without voice packages.
async fn start(locality: DataLocality, voice: bool) -> Option<TestCore> {
    start_with(locality, voice, jarvis_core::MentorTiming::default()).await
}

async fn start_with(
    locality: DataLocality,
    voice: bool,
    timing: jarvis_core::MentorTiming,
) -> Option<TestCore> {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        log_dir: dir.path().join("logs"),
    };
    let installed = testing::install_defaults(&paths.data_dir.join("voice"));
    if !installed {
        return None;
    }
    if !voice {
        std::fs::remove_dir_all(paths.data_dir.join("voice")).unwrap();
    }
    let workspace = dir.path().join("work");
    let project = workspace.join("shop");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/app.ts"), "export const shop = 1;\n").unwrap();
    let teacher = Teacher::new("teacher", locality);
    let fake = FakeBackend::default();
    let core = JarvisCore::initialize(
        CoreOptions::new(paths, "9.9.9")
            .with_provider_factory(None)
            .with_static_providers(vec![teacher.clone() as Arc<dyn AiProvider>])
            .with_voice_backend(Arc::new(fake.clone()))
            .with_mentor_timing(timing),
    )
    .await
    .unwrap();
    let mut settings = core.settings();
    settings.tools.workspace_folders = vec![workspace.to_string_lossy().into_owned()];
    settings.privacy.allow_cloud_providers = true;
    settings.tools.enabled.push("write_file".into());
    settings.tools.enabled.push("edit_file".into());
    core.update_settings(settings).await.unwrap();
    Some(TestCore {
        core,
        teacher,
        fake,
        project,
        dir,
    })
}

fn request(test: &TestCore, conversation: Uuid, folder: &std::path::Path) -> MentorStartRequest {
    MentorStartRequest {
        conversation_id: conversation,
        project_folder: folder.to_string_lossy().into_owned(),
        provider_id: Some(test.teacher.descriptor.id.clone()),
        model: Some("teacher".into()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn start_requires_a_workspace_folder() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let outside = test.dir.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let err = test
        .core
        .mentor_start(request(&test, conversation.id, &outside))
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Validation, "{err}");
    assert!(err.to_string().contains("workspace"), "{err}");
    assert_eq!(test.core.mentor_status().phase, MentorPhase::Off);
    assert!(!test.core.voice().state().mic_open);
}

#[tokio::test(flavor = "multi_thread")]
async fn cloud_mentor_needs_file_and_cloud_permissions() {
    let Some(test) = start(DataLocality::ThirdPartyCloud, true).await else {
        return;
    };
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let err = test
        .core
        .mentor_start(request(&test, conversation.id, &test.project))
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::PrivacyBlocked, "{err}");
    assert!(
        err.to_string()
            .contains("Let agents use cloud AI providers"),
        "{err}"
    );
    let mut settings = test.core.settings();
    settings.privacy.allow_cloud_agents = true;
    test.core.update_settings(settings).await.unwrap();
    test.core
        .mentor_start(request(&test, conversation.id, &test.project))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn start_without_speech_model_fails_with_settings_hint() {
    let Some(test) = start(DataLocality::OnDevice, false).await else {
        return;
    };
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let err = test
        .core
        .mentor_start(request(&test, conversation.id, &test.project))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Settings → Voice"), "{err}");
    assert_eq!(test.core.mentor_status().phase, MentorPhase::Off);
}

#[tokio::test(flavor = "multi_thread")]
async fn start_without_a_voice_runs_text_only() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    // Recognition stays; the Piper voice goes.
    let voice = test.dir.path().join("data/voice");
    for entry in std::fs::read_dir(&voice).unwrap() {
        let path = entry.unwrap().path();
        if path.to_string_lossy().contains("piper-uk_UA") {
            std::fs::remove_dir_all(&path).ok();
            std::fs::remove_file(&path).ok();
        }
    }
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let status = test
        .core
        .mentor_start(request(&test, conversation.id, &test.project))
        .await
        .unwrap();
    assert_eq!(status.phase, MentorPhase::Listening);
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("text"),
        "{status:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn start_pause_resume_stop() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    let status = test
        .core
        .mentor_start(request(&test, conversation.id, &test.project))
        .await
        .unwrap();
    assert_eq!(status.phase, MentorPhase::Listening);
    assert_eq!(status.conversation_id, Some(conversation.id));
    let folder = status.project_folder.expect("folder");
    assert!(folder.ends_with("shop"), "{folder}");
    wait_mic(&test, true).await;
    let session = test
        .core
        .memory()
        .mentor_session(conversation.id)
        .await
        .unwrap()
        .expect("saved session");
    assert!(session.project_folder.ends_with("shop"));

    test.core.mentor_pause();
    assert_eq!(test.core.mentor_status().phase, MentorPhase::Paused);
    wait_mic(&test, false).await;
    let status = test.core.mentor_resume().unwrap();
    assert_eq!(status.phase, MentorPhase::Listening);
    wait_mic(&test, true).await;

    test.core.mentor_stop();
    assert_eq!(test.core.mentor_status().phase, MentorPhase::Off);
    assert_eq!(test.core.mentor_status().conversation_id, None);
    wait_mic(&test, false).await;
    assert!(test.core.mentor_resume().is_err(), "nothing to resume");
    assert_eq!(test.fake.lock().mic_opened, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn second_start_replaces_the_first() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let first = test.core.create_conversation(None, false).await.unwrap();
    let second = test.core.create_conversation(None, false).await.unwrap();
    test.core
        .mentor_start(request(&test, first.id, &test.project))
        .await
        .unwrap();
    test.core
        .mentor_start(request(&test, second.id, &test.project))
        .await
        .unwrap();
    assert_eq!(test.core.mentor_status().conversation_id, Some(second.id));
    wait_mic(&test, true).await;
    // Deleting the conversation ends its session.
    test.core.delete_conversation(second.id).await.unwrap();
    assert_eq!(test.core.mentor_status().phase, MentorPhase::Off);
    wait_mic(&test, false).await;
}

async fn wait_mic(test: &TestCore, open: bool) {
    let mut state = test.core.voice().subscribe_state();
    tokio::time::timeout(
        Duration::from_secs(10),
        state.wait_for(|s| s.mic_open == open && s.conversation_active == open),
    )
    .await
    .expect("the microphone in time")
    .unwrap();
}

// ----- turns -----

use jarvis_core::{ChatStreamEvent, ChatTurnRequest, MentorTrigger, SinkClosed};

async fn mentor_turn(
    test: &TestCore,
    conversation: Uuid,
    text: &str,
    trigger: Option<MentorTrigger>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    test.core
        .send_message(
            ChatTurnRequest {
                conversation_id: conversation,
                content: text.to_owned(),
                provider_id: None,
                model: None,
                temperature: None,
                max_output_tokens: None,
                speak: false,
                attachments: Vec::new(),
                mentor: trigger,
            },
            move |event| tx.send(event).map_err(|_| SinkClosed),
        )
        .await
        .unwrap();
    while let Some(event) = rx.recv().await {
        if matches!(
            event,
            ChatStreamEvent::Finished { .. } | ChatStreamEvent::Failed { .. }
        ) {
            break;
        }
    }
}

/// The text of the last request the teacher saw: (system, last user).
fn last_request(test: &TestCore) -> (String, String) {
    let seen = test.teacher.seen.lock().unwrap();
    let request = seen.last().expect("a request");
    let system: Vec<&str> = request
        .messages
        .iter()
        .filter(|m| m.role == jarvis_ai::ChatRole::System)
        .map(|m| m.content.as_str())
        .collect();
    let user = request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == jarvis_ai::ChatRole::User)
        .expect("user message");
    (system.join("\n"), user.content.clone())
}

async fn started(test: &TestCore) -> Uuid {
    let conversation = test.core.create_conversation(None, false).await.unwrap();
    test.core
        .mentor_start(request(test, conversation.id, &test.project))
        .await
        .unwrap();
    conversation.id
}

/// The saved messages of a conversation once its reply is stored.
async fn saved(test: &TestCore, conversation: Uuid, count: usize) -> Vec<(String, String)> {
    for _ in 0..200 {
        let detail = test.core.memory().conversation(conversation).await.unwrap();
        if detail.messages.len() >= count {
            return detail
                .messages
                .iter()
                .map(|m| (format!("{:?}", m.role), m.content.clone()))
                .collect();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the messages were not saved");
}

#[tokio::test(flavor = "multi_thread")]
async fn mentor_turn_adds_changes_but_saves_only_the_phrase() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    mentor_turn(
        &test,
        conversation,
        "Починаємо магазин на Angular",
        Some(MentorTrigger::Phrase),
    )
    .await;
    let (system, user) = last_request(&test);
    assert!(
        system.contains("<write"),
        "the mentor prompt teaches the write block"
    );
    assert!(system.contains("<plan>"));
    assert!(user.starts_with("Починаємо магазин на Angular"), "{user}");
    assert!(
        user.contains("src/app.ts"),
        "the first turn lists the files: {user}"
    );

    std::fs::write(test.project.join("src/app.ts"), "export const shop = 2;\n").unwrap();
    mentor_turn(&test, conversation, "Що далі?", Some(MentorTrigger::Phrase)).await;
    let (_, user) = last_request(&test);
    assert!(
        user.contains("<project_file path=\"src/app.ts\">"),
        "{user}"
    );
    assert!(user.contains("export const shop = 2;"), "{user}");

    // Nothing changed: no project block.
    mentor_turn(
        &test,
        conversation,
        "Поясни ще раз",
        Some(MentorTrigger::Phrase),
    )
    .await;
    let (_, user) = last_request(&test);
    assert!(!user.contains("<project_file"), "{user}");

    let messages = saved(&test, conversation, 6).await;
    let users: Vec<&str> = messages
        .iter()
        .filter(|(role, _)| role == "User")
        .map(|(_, text)| text.as_str())
        .collect();
    assert_eq!(
        users,
        ["Починаємо магазин на Angular", "Що далі?", "Поясни ще раз"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn files_saved_turn_is_stored_as_a_short_note() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    std::fs::write(test.project.join("src/cart.ts"), "export class Cart {}\n").unwrap();
    mentor_turn(&test, conversation, "", Some(MentorTrigger::FilesSaved)).await;
    let (_, user) = last_request(&test);
    assert!(user.contains("export class Cart {}"), "{user}");
    assert!(user.to_lowercase().contains("saved"), "{user}");
    let messages = saved(&test, conversation, 2).await;
    assert_eq!(messages[0].1, "[Saved: src/cart.ts]");
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_block_becomes_the_pinned_note() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    test.teacher
        .replies
        .lock()
        .unwrap()
        .push("Почнемо.\n<plan>\nМета: магазин\n1. Кошик\n</plan>".to_owned());
    mentor_turn(&test, conversation, "Почнемо", Some(MentorTrigger::Phrase)).await;
    for _ in 0..200 {
        let detail = test.core.memory().conversation(conversation).await.unwrap();
        if !detail.conversation.pinned_note.is_empty() {
            assert_eq!(detail.conversation.pinned_note, "Мета: магазин\n1. Кошик");
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the plan was not pinned");
}

#[tokio::test(flavor = "multi_thread")]
async fn non_mentor_turns_are_unchanged() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    std::fs::write(test.project.join("src/app.ts"), "changed\n").unwrap();
    mentor_turn(&test, conversation, "Звичайне питання", None).await;
    let (system, user) = last_request(&test);
    assert!(!system.contains("<write"));
    assert_eq!(user, "Звичайне питання");
    // A mentor trigger for another conversation adds nothing either.
    let other = test.core.create_conversation(None, false).await.unwrap();
    mentor_turn(&test, other.id, "Інша розмова", Some(MentorTrigger::Phrase)).await;
    let (system, user) = last_request(&test);
    assert!(!system.contains("<write"));
    assert_eq!(user, "Інша розмова");
}

// ----- typing -----

use jarvis_core::jarvis_security::{PendingApproval, RiskLevel};

/// Makes the teacher answer the next turn with `reply` and sends a phrase.
async fn ask_to_write(test: &TestCore, conversation: Uuid, reply: &str) {
    test.teacher.replies.lock().unwrap().push(reply.to_owned());
    mentor_turn(
        test,
        conversation,
        "Напиши сам",
        Some(MentorTrigger::Phrase),
    )
    .await;
}

async fn approval(test: &TestCore) -> PendingApproval {
    for _ in 0..400 {
        if let Some(pending) = test.core.pending_approvals().into_iter().next() {
            return pending;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no approval was asked");
}

async fn until_not_typing(test: &TestCore) -> jarvis_core::MentorStatus {
    let mut status = test.core.subscribe_mentor();
    tokio::time::timeout(
        Duration::from_secs(20),
        status.wait_for(|s| s.phase != MentorPhase::Typing && s.pending_write.is_none()),
    )
    .await
    .expect("typing ends")
    .unwrap()
    .clone()
}

/// Waits for typing to start and then to end.
async fn typed(test: &TestCore) -> jarvis_core::MentorStatus {
    let mut status = test.core.subscribe_mentor();
    tokio::time::timeout(
        Duration::from_secs(20),
        status.wait_for(|s| s.phase == MentorPhase::Typing),
    )
    .await
    .expect("typing starts")
    .unwrap();
    until_not_typing(test).await
}

/// Waits until JARVIS has started typing into `path`, returning what the
/// file held then.
async fn first_typed(path: &std::path::Path, before: &str) -> String {
    for _ in 0..400 {
        let now = std::fs::read_to_string(path).unwrap_or_default();
        if now != before {
            return now;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nothing was typed");
}

fn lines(count: usize) -> String {
    (1..=count)
        .map(|i| format!("const line{i} = {i};\n"))
        .collect()
}

fn create_block(path: &str, code: &str) -> String {
    format!(
        "Пишу.\n<write path=\"{path}\" mode=\"create\">\n<say>Перша частина.</say>\n<code>\n{code}</code>\n</write>"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn approved_create_is_typed_in_steps() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    let code = lines(9);
    ask_to_write(&test, conversation, &create_block("src/cart.ts", &code)).await;
    let pending = approval(&test).await;
    assert_eq!(pending.risk, RiskLevel::Medium);
    assert!(
        pending.details.contains("const line9 = 9;"),
        "the whole change is shown"
    );
    assert!(test.core.mentor_status().pending_write.is_some());
    assert!(test.core.resolve_approval(pending.id, true));

    let path = test.project.join("src/cart.ts");
    let mut states = Vec::new();
    for _ in 0..600 {
        let now = std::fs::read_to_string(&path).unwrap_or_default();
        if states.last() != Some(&now) {
            states.push(now.clone());
        }
        if now == code {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(states.last(), Some(&code));
    assert!(states.len() >= 3, "typed in steps: {states:?}");
    let status = until_not_typing(&test).await;
    assert_eq!(status.phase, MentorPhase::Listening);
    assert!(status.last_error.is_none(), "{status:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn replace_types_only_the_new_fragment() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    let path = test.project.join("src/app.ts");
    std::fs::write(&path, "// head\nexport const shop = 1;\n// tail\n").unwrap();
    let reply = "<write path=\"src/app.ts\" mode=\"replace\">\n<find>\nexport const shop = 1;\n</find>\n<say>Нове значення.</say>\n<code>\nexport const shop = 2;\nexport const open = true;\n</code>\n</write>";
    ask_to_write(&test, conversation, reply).await;
    let pending = approval(&test).await;
    assert!(test.core.resolve_approval(pending.id, true));
    let status = typed(&test).await;
    assert!(status.last_error.is_none(), "{status:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "// head\nexport const shop = 2;\nexport const open = true;\n// tail\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_write_asks_nothing() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    std::fs::write(test.project.join("src/twice.ts"), "x = 1;\nx = 1;\n").unwrap();
    for reply in [
        "<write path=\"src/twice.ts\" mode=\"replace\"><find>x = 1;</find><say>a</say><code>\ny\n</code></write>",
        "<write path=\"../outside.ts\" mode=\"create\"><say>a</say><code>\ny\n</code></write>",
    ] {
        ask_to_write(&test, conversation, reply).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            test.core.pending_approvals().is_empty(),
            "asked for {reply}"
        );
        let status = test.core.mentor_status();
        assert!(status.last_error.is_some(), "{status:?}");
    }
    assert!(!test.dir.path().join("work/outside.ts").exists());
    // The mentor is told on its next turn.
    let messages = saved(&test, conversation, 6).await;
    assert!(
        messages
            .iter()
            .any(|(_, text)| text.contains("not written")),
        "{messages:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_write_leaves_the_file() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    ask_to_write(&test, conversation, &create_block("src/no.ts", &lines(2))).await;
    let pending = approval(&test).await;
    assert!(test.core.resolve_approval(pending.id, false));
    until_not_typing(&test).await;
    assert!(!test.project.join("src/no.ts").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn typing_stops_when_the_owner_edits_the_file() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    ask_to_write(
        &test,
        conversation,
        &create_block("src/long.ts", &lines(40)),
    )
    .await;
    let pending = approval(&test).await;
    assert!(test.core.resolve_approval(pending.id, true));
    let path = test.project.join("src/long.ts");
    first_typed(&path, "").await;
    std::fs::write(&path, "// typed by the user\n").unwrap();
    let status = until_not_typing(&test).await;
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("changed"),
        "{status:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "// typed by the user\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_command_keeps_what_was_typed() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    let code = lines(40);
    ask_to_write(&test, conversation, &create_block("src/stop.ts", &code)).await;
    let pending = approval(&test).await;
    assert!(test.core.resolve_approval(pending.id, true));
    let path = test.project.join("src/stop.ts");
    first_typed(&path, "").await;
    test.core.mentor_stop_typing();
    until_not_typing(&test).await;
    let kept = std::fs::read_to_string(&path).unwrap();
    assert!(!kept.is_empty() && kept != code, "{kept}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        kept,
        "no more typing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn asks_to_save_before_typing() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    ask_to_write(&test, conversation, &create_block("src/say.ts", &lines(2))).await;
    let pending = approval(&test).await;
    assert!(test.core.resolve_approval(pending.id, true));
    typed(&test).await;
    let spoken = test.fake.lock().synthesized.join(" ").to_lowercase();
    assert!(
        spoken.contains("збереж") || spoken.contains("save"),
        "{spoken}"
    );
    assert!(spoken.contains("перша частина"), "{spoken}");
}

#[tokio::test(flavor = "multi_thread")]
async fn overwrite_is_marked_high_risk() {
    let Some(test) = start(DataLocality::OnDevice, true).await else {
        return;
    };
    let conversation = started(&test).await;
    ask_to_write(&test, conversation, &create_block("src/app.ts", &lines(2))).await;
    let pending = approval(&test).await;
    assert_eq!(pending.risk, RiskLevel::High);
    assert!(test.core.resolve_approval(pending.id, false));
}

// ----- speaking up on save -----

use jarvis_core::{MentorNudge, MentorTiming};

const FAST: MentorTiming = MentorTiming {
    poll: Duration::from_millis(50),
    settle: Duration::from_millis(250),
    cooldown: Duration::from_millis(900),
};

async fn start_fast() -> Option<TestCore> {
    let test = start_with(DataLocality::OnDevice, true, FAST).await?;
    Some(test)
}

async fn next_nudge(
    nudges: &mut tokio::sync::broadcast::Receiver<MentorNudge>,
    within: Duration,
) -> Option<MentorNudge> {
    tokio::time::timeout(within, nudges.recv())
        .await
        .ok()
        .map(|n| n.unwrap())
}

fn touch(test: &TestCore, path: &str, text: &str) {
    let path = test.project.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn nudges_after_files_settle() {
    let Some(test) = start_fast().await else {
        return;
    };
    let conversation = started(&test).await;
    let mut nudges = test.core.subscribe_mentor_nudges();
    let saved_at = std::time::Instant::now();
    touch(&test, "src/app.ts", "export const shop = 3;\n");
    let nudge = next_nudge(&mut nudges, Duration::from_secs(5))
        .await
        .expect("a nudge");
    assert!(
        saved_at.elapsed() >= FAST.settle,
        "waited for the files to settle"
    );
    assert_eq!(nudge.conversation_id, conversation);
    assert_eq!(nudge.files, ["src/app.ts"]);
    // Nothing new: no more nudges.
    assert!(
        next_nudge(&mut nudges, Duration::from_millis(1500))
            .await
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_nudge_while_busy_paused_or_typing() {
    let Some(test) = start_fast().await else {
        return;
    };
    started(&test).await;
    let mut nudges = test.core.subscribe_mentor_nudges();
    test.core.mentor_set_busy(true);
    touch(&test, "src/a.ts", "a\n");
    assert!(
        next_nudge(&mut nudges, Duration::from_millis(800))
            .await
            .is_none()
    );
    test.core.mentor_set_busy(false);
    let nudge = next_nudge(&mut nudges, Duration::from_secs(3))
        .await
        .expect("after the answer");
    assert_eq!(nudge.files, ["src/a.ts"]);

    test.core.mentor_pause();
    touch(&test, "src/b.ts", "b\n");
    assert!(
        next_nudge(&mut nudges, Duration::from_millis(1500))
            .await
            .is_none()
    );
    test.core.mentor_resume().unwrap();
    let nudge = next_nudge(&mut nudges, Duration::from_secs(3))
        .await
        .expect("after the pause");
    assert_eq!(nudge.files, ["src/b.ts"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn at_most_one_nudge_per_cooldown() {
    let Some(test) = start_fast().await else {
        return;
    };
    started(&test).await;
    let mut nudges = test.core.subscribe_mentor_nudges();
    touch(&test, "src/one.ts", "1\n");
    let first = next_nudge(&mut nudges, Duration::from_secs(3))
        .await
        .expect("first");
    let at = std::time::Instant::now();
    touch(&test, "src/two.ts", "2\n");
    touch(&test, "src/three.ts", "3\n");
    let second = next_nudge(&mut nudges, Duration::from_secs(3))
        .await
        .expect("second");
    assert!(
        at.elapsed() >= FAST.cooldown - FAST.settle,
        "{:?}",
        at.elapsed()
    );
    assert_eq!(first.files, ["src/one.ts"]);
    assert_eq!(second.files, ["src/three.ts", "src/two.ts"], "gathered");
}

#[tokio::test(flavor = "multi_thread")]
async fn three_nudges_without_a_phrase_then_quiet_until_the_owner_speaks() {
    let Some(test) = start_fast().await else {
        return;
    };
    started(&test).await;
    let mut nudges = test.core.subscribe_mentor_nudges();
    for i in 0..3 {
        touch(&test, &format!("src/f{i}.ts"), "x\n");
        next_nudge(&mut nudges, Duration::from_secs(4))
            .await
            .expect("nudge");
    }
    touch(&test, "src/f3.ts", "x\n");
    assert!(
        next_nudge(&mut nudges, Duration::from_secs(2))
            .await
            .is_none(),
        "quiet"
    );
    test.core.mentor_owner_spoke();
    let nudge = next_nudge(&mut nudges, Duration::from_secs(3))
        .await
        .expect("again");
    assert_eq!(nudge.files, ["src/f3.ts"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn own_writes_never_nudge() {
    let Some(test) = start_fast().await else {
        return;
    };
    let conversation = started(&test).await;
    let mut nudges = test.core.subscribe_mentor_nudges();
    ask_to_write(&test, conversation, &create_block("src/made.ts", &lines(4))).await;
    let pending = approval(&test).await;
    assert!(test.core.resolve_approval(pending.id, true));
    typed(&test).await;
    assert!(test.project.join("src/made.ts").exists());
    assert!(
        next_nudge(&mut nudges, Duration::from_millis(1500))
            .await
            .is_none()
    );
}
