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
            .with_voice_backend(Arc::new(fake.clone())),
    )
    .await
    .unwrap();
    let mut settings = core.settings();
    settings.tools.workspace_folders = vec![workspace.to_string_lossy().into_owned()];
    settings.privacy.allow_cloud_providers = true;
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
