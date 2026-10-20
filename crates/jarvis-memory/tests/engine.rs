//! Memory Engine against a real SQLite database.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use jarvis_ai::ChatRole;
use jarvis_memory::{
    ContextRequest, ContextWindow, ConversationPatch, DEFAULT_TITLE, Embedder, FactInput,
    FactSource, HIGHLIGHT_START, MemoryEngine, MemoryError, MemoryKind, MessageRole, MessageStatus,
    NewMessage, ProjectInput, Retrieval, SearchQuery,
};
use uuid::Uuid;

/// Test-only embedder: words map to "concept" dimensions, synonyms share a
/// dimension, other words are hashed. Enough to tell semantic matches from
/// text matches; not a real model.
struct ConceptEmbedder;

fn concept(word: &str) -> usize {
    match word {
        "car" | "automobile" | "drive" | "машина" | "авто" => 0,
        "tesla" => 1,
        "red" | "червона" => 2,
        _ => {
            8 + word
                .bytes()
                .fold(7usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize))
                % 56
        }
    }
}

#[async_trait]
impl Embedder for ConceptEmbedder {
    fn model_key(&self) -> &str {
        "test/concepts"
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, MemoryError> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut vector = vec![0.0; 64];
                for word in text
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| w.len() > 2)
                {
                    vector[concept(&word.to_lowercase())] += 1.0;
                }
                vector
            })
            .collect())
    }
}

fn user(text: &str) -> NewMessage {
    NewMessage {
        id: Uuid::new_v4(),
        role: MessageRole::User,
        content: text.to_owned(),
        status: MessageStatus::Complete,
        provider_id: None,
        model: None,
    }
}

fn assistant(text: &str) -> NewMessage {
    NewMessage {
        role: MessageRole::Assistant,
        provider_id: Some("test".into()),
        model: Some("model".into()),
        ..user(text)
    }
}

fn fact(content: &str, pinned: bool, project_id: Option<Uuid>) -> FactInput {
    FactInput {
        id: None,
        content: content.to_owned(),
        project_id,
        pinned,
        source_conversation_id: None,
    }
}

fn request(conversation_id: Uuid, text: &str, retrieval: Retrieval) -> ContextRequest {
    ContextRequest {
        conversation_id,
        user_message: text.to_owned(),
        system_prompt: "You are JARVIS.".to_owned(),
        window: ContextWindow::new(Some(8192), None, None),
        retrieval,
    }
}

fn text_retrieval() -> Retrieval {
    Retrieval {
        facts: true,
        recall_conversations: true,
        embedder: None,
    }
}

#[tokio::test]
async fn conversations_persist_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory").join("jarvis.db");
    let id = {
        let memory = MemoryEngine::open(&path).await;
        assert!(memory.unavailable_reason().is_none());
        let conversation = memory.create_conversation(None, false).await.unwrap();
        assert_eq!(conversation.title, DEFAULT_TITLE);
        memory
            .append_message(
                conversation.id,
                user("Плануємо запуск   проєкту Атлас\nдеталі"),
            )
            .await
            .unwrap();
        memory
            .append_message(conversation.id, assistant("Добре, давай спланаємо."))
            .await
            .unwrap();
        memory
            .update_conversation(
                conversation.id,
                ConversationPatch {
                    pinned_note: Some("Говоримо про Атлас".into()),
                    ..ConversationPatch::default()
                },
            )
            .await
            .unwrap();
        conversation.id
    };

    let memory = MemoryEngine::open(&path).await;
    let list = memory.list_conversations(None, 50).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "Плануємо запуск проєкту Атлас");
    assert_eq!(list[0].message_count, 2);
    let detail = memory.conversation(id).await.unwrap();
    assert_eq!(detail.messages[1].role, MessageRole::Assistant);
    assert_eq!(detail.messages[1].model.as_deref(), Some("model"));
    assert_eq!(detail.conversation.pinned_note, "Говоримо про Атлас");
    // Search finds conversations by message text, inflected.
    let found = memory
        .list_conversations(Some("атласом"), 50)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
}

#[tokio::test]
async fn temporary_conversations_are_never_written() {
    let memory = MemoryEngine::in_memory().unwrap();
    let conversation = memory.create_conversation(None, true).await.unwrap();
    assert!(conversation.temporary);
    memory
        .append_message(conversation.id, user("secret plans"))
        .await
        .unwrap();
    assert_eq!(memory.counts().await.unwrap().conversations, 0);
    assert_eq!(memory.counts().await.unwrap().messages, 0);
    let list = memory.list_conversations(None, 10).await.unwrap();
    assert!(list[0].temporary);
    assert_eq!(list[0].title, "secret plans");
    assert!(
        memory
            .search(
                SearchQuery {
                    text: "secret".into(),
                    kinds: vec![],
                    limit: 10
                },
                None
            )
            .await
            .unwrap()
            .is_empty()
    );
    memory.delete_conversation(conversation.id).await.unwrap();
    assert!(
        memory
            .list_conversations(None, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn context_includes_pinned_and_relevant_facts_and_recalls() {
    let memory = MemoryEngine::in_memory().unwrap();
    memory
        .save_fact(
            fact("The user's name is Roman.", true, None),
            FactSource::User,
        )
        .await
        .unwrap();
    memory
        .save_fact(
            fact("The Atlas deadline is Friday.", false, None),
            FactSource::User,
        )
        .await
        .unwrap();
    memory
        .save_fact(fact("Likes green tea.", false, None), FactSource::User)
        .await
        .unwrap();
    let earlier = memory.create_conversation(None, false).await.unwrap();
    memory
        .append_message(earlier.id, user("Any news about Atlas?"))
        .await
        .unwrap();
    memory
        .append_message(earlier.id, assistant("The Atlas launch moved to March."))
        .await
        .unwrap();

    let current = memory.create_conversation(None, false).await.unwrap();
    let context = memory
        .prepare_context(request(
            current.id,
            "When is the Atlas deadline?",
            text_retrieval(),
        ))
        .await
        .unwrap();
    let system = &context.messages[0].content;
    assert_eq!(context.messages[0].role, ChatRole::System);
    assert!(system.contains("Roman"), "{system}");
    assert!(system.contains("deadline is Friday"), "{system}");
    assert!(system.contains("launch moved to March"), "{system}");
    assert!(!system.contains("green tea"), "{system}");
    assert_eq!(context.report.facts_included, 2);
    assert!(context.report.recalls_included >= 1);
    assert_eq!(
        context.messages.last().unwrap().content,
        "When is the Atlas deadline?"
    );

    // With retrieval off, only the conversation itself is used.
    let plain = memory
        .prepare_context(request(
            current.id,
            "When is the Atlas deadline?",
            Retrieval::default(),
        ))
        .await
        .unwrap();
    assert!(!plain.messages[0].content.contains("Roman"));
}

#[tokio::test]
async fn projects_scope_facts_and_recalls() {
    let memory = MemoryEngine::in_memory().unwrap();
    let project = memory
        .save_project(ProjectInput {
            id: None,
            name: "  Atlas   satellite ".into(),
            description: "Satellite tracking app".into(),
            instructions: "Use metric units.".into(),
        })
        .await
        .unwrap();
    assert_eq!(project.name, "Atlas satellite");
    memory
        .save_fact(
            fact("The orbit altitude is 550 km.", false, Some(project.id)),
            FactSource::User,
        )
        .await
        .unwrap();

    let outside = memory.create_conversation(None, false).await.unwrap();
    let context = memory
        .prepare_context(request(
            outside.id,
            "What is the orbit altitude?",
            text_retrieval(),
        ))
        .await
        .unwrap();
    assert!(!context.messages[0].content.contains("550 km"));

    let inside = memory
        .create_conversation(Some(project.id), false)
        .await
        .unwrap();
    let context = memory
        .prepare_context(request(
            inside.id,
            "What is the orbit altitude?",
            text_retrieval(),
        ))
        .await
        .unwrap();
    let system = &context.messages[0].content;
    assert!(system.contains("550 km"));
    assert!(system.contains("## Project: Atlas satellite"));
    assert!(system.contains("Use metric units."));
    assert_eq!(context.report.project.as_deref(), Some("Atlas satellite"));

    let listed = memory.list_projects().await.unwrap();
    assert_eq!(listed[0].fact_count, 1);
    assert_eq!(listed[0].conversation_count, 1);

    // Deleting the project deletes its facts and keeps its conversations.
    memory.delete_project(project.id).await.unwrap();
    assert!(memory.list_facts().await.unwrap().is_empty());
    let kept = memory.conversation(inside.id).await.unwrap();
    assert_eq!(kept.conversation.project_id, None);
}

#[tokio::test]
async fn semantic_recall_finds_what_text_search_misses() {
    let memory = MemoryEngine::in_memory().unwrap();
    memory
        .save_fact(
            fact("My automobile is a red Tesla.", false, None),
            FactSource::User,
        )
        .await
        .unwrap();
    let conversation = memory.create_conversation(None, false).await.unwrap();
    let question = "What car do I drive?";

    let text_only = memory
        .prepare_context(request(conversation.id, question, text_retrieval()))
        .await
        .unwrap();
    assert_eq!(text_only.report.facts_included, 0);

    let embedder: Arc<dyn Embedder> = Arc::new(ConceptEmbedder);
    assert_eq!(
        memory.index_pending(embedder.as_ref(), 64).await.unwrap(),
        1
    );
    assert_eq!(
        memory.index_pending(embedder.as_ref(), 64).await.unwrap(),
        0
    );
    assert_eq!(
        memory.embedding_counts("test/concepts").await.unwrap(),
        (1, 0)
    );
    let semantic = memory
        .prepare_context(request(
            conversation.id,
            question,
            Retrieval {
                embedder: Some(Arc::clone(&embedder)),
                ..text_retrieval()
            },
        ))
        .await
        .unwrap();
    assert_eq!(semantic.report.facts_included, 1);
    assert!(semantic.messages[0].content.contains("red Tesla"));

    let hits = memory
        .search(
            SearchQuery {
                text: "car".into(),
                kinds: vec![MemoryKind::Fact],
                limit: 5,
            },
            Some(embedder),
        )
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].matched_semantic);
    assert!(!hits[0].matched_text);
}

#[tokio::test]
async fn search_covers_all_kinds_with_highlights() {
    let memory = MemoryEngine::in_memory().unwrap();
    let conversation = memory.create_conversation(None, false).await.unwrap();
    memory
        .append_message(conversation.id, user("Deploy Atlas on Monday"))
        .await
        .unwrap();
    memory
        .save_fact(
            fact("Atlas uses PostgreSQL.", false, None),
            FactSource::User,
        )
        .await
        .unwrap();
    memory
        .record_episodes(vec![jarvis_memory::Episode {
            id: Uuid::new_v4(),
            occurred_at_ms: 1,
            category: "runtime".into(),
            level: "info".into(),
            summary: "Started the Atlas model".into(),
        }])
        .await
        .unwrap();

    let hits = memory
        .search(
            SearchQuery {
                text: "atlas".into(),
                kinds: vec![],
                limit: 10,
            },
            None,
        )
        .await
        .unwrap();
    let kinds: Vec<MemoryKind> = hits.iter().map(|h| h.kind).collect();
    assert!(kinds.contains(&MemoryKind::Message));
    assert!(kinds.contains(&MemoryKind::Fact));
    assert!(kinds.contains(&MemoryKind::Episode));
    assert!(hits.iter().all(|h| h.snippet.contains(HIGHLIGHT_START)));
    let message = hits.iter().find(|h| h.kind == MemoryKind::Message).unwrap();
    assert_eq!(message.conversation_id, Some(conversation.id));

    memory.delete_conversation(conversation.id).await.unwrap();
    let after = memory
        .search(
            SearchQuery {
                text: "atlas".into(),
                kinds: vec![MemoryKind::Message],
                limit: 10,
            },
            None,
        )
        .await
        .unwrap();
    assert!(after.is_empty(), "deleted messages are gone from search");
}

#[tokio::test]
async fn compaction_summaries_replace_old_messages() {
    let memory = MemoryEngine::in_memory().unwrap();
    let conversation = memory.create_conversation(None, false).await.unwrap();
    for i in 0..10 {
        memory
            .append_message(conversation.id, user(&format!("question {i}")))
            .await
            .unwrap();
        memory
            .append_message(conversation.id, assistant(&format!("answer {i}")))
            .await
            .unwrap();
    }
    let window = ContextWindow::new(Some(8192), None, None);
    let plan = memory
        .compaction_plan(conversation.id, window)
        .await
        .unwrap()
        .expect("plan");
    assert_eq!(plan.covered, 14);
    memory
        .store_summary(
            conversation.id,
            "The user asked ten numbered questions.".into(),
            plan.covers_until,
            Some("model".into()),
        )
        .await
        .unwrap();
    let detail = memory.conversation(conversation.id).await.unwrap();
    assert_eq!(detail.summary.as_ref().unwrap().covered_messages, 14);
    let context = memory
        .prepare_context(request(conversation.id, "next", Retrieval::default()))
        .await
        .unwrap();
    assert_eq!(context.report.summarized_messages, 14);
    assert_eq!(context.report.history_included, 6);
    assert!(
        context.messages[0]
            .content
            .contains("ten numbered questions")
    );
    assert!(
        memory
            .compaction_plan(conversation.id, window)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn export_clear_and_import_round_trip() {
    let memory = MemoryEngine::in_memory().unwrap();
    let project = memory
        .save_project(ProjectInput {
            id: None,
            name: "Atlas".into(),
            description: String::new(),
            instructions: String::new(),
        })
        .await
        .unwrap();
    let conversation = memory
        .create_conversation(Some(project.id), false)
        .await
        .unwrap();
    for i in 0..8 {
        memory
            .append_message(conversation.id, user(&format!("message {i}")))
            .await
            .unwrap();
    }
    let plan = memory
        .compaction_plan(conversation.id, ContextWindow::new(None, None, None))
        .await
        .unwrap()
        .unwrap();
    memory
        .store_summary(conversation.id, "summary".into(), plan.covers_until, None)
        .await
        .unwrap();
    memory
        .save_fact(
            FactInput {
                source_conversation_id: Some(conversation.id),
                ..fact("Atlas fact", true, Some(project.id))
            },
            FactSource::Conversation,
        )
        .await
        .unwrap();
    let before = memory.counts().await.unwrap();

    let export = memory.export().await.unwrap();
    let json = serde_json::to_string(&export).unwrap();
    memory.clear().await.unwrap();
    assert_eq!(memory.counts().await.unwrap().messages, 0);

    let outcome = memory
        .import(serde_json::from_str(&json).unwrap())
        .await
        .unwrap();
    assert_eq!(outcome.skipped, 0);
    assert_eq!(memory.counts().await.unwrap(), before);
    let restored = memory.conversation(conversation.id).await.unwrap();
    assert_eq!(restored.conversation.project_id, Some(project.id));
    assert_eq!(restored.summary.unwrap().covered_messages, 2);
    assert_eq!(
        memory.list_facts().await.unwrap()[0].source,
        FactSource::Import
    );

    let again = memory
        .import(serde_json::from_str(&json).unwrap())
        .await
        .unwrap();
    assert_eq!(again.imported.messages, 0);
    assert!(again.skipped > 0);
    assert_eq!(memory.counts().await.unwrap(), before);

    let mut foreign: serde_json::Value = serde_json::from_str(&json).unwrap();
    foreign["format"] = "something-else".into();
    assert!(matches!(
        memory
            .import(serde_json::from_value(foreign).unwrap())
            .await,
        Err(MemoryError::Invalid(_))
    ));
}

#[tokio::test]
async fn limits_are_enforced() {
    let memory = MemoryEngine::in_memory().unwrap();
    assert!(matches!(
        memory
            .save_fact(fact(&"x".repeat(2001), false, None), FactSource::User)
            .await,
        Err(MemoryError::Invalid(_))
    ));
    assert!(matches!(
        memory
            .save_fact(fact("   ", false, None), FactSource::User)
            .await,
        Err(MemoryError::Invalid(_))
    ));
    assert!(matches!(
        memory
            .save_project(ProjectInput {
                id: None,
                name: " ".into(),
                description: String::new(),
                instructions: String::new()
            })
            .await,
        Err(MemoryError::Invalid(_))
    ));
    assert!(matches!(
        memory
            .create_conversation(Some(Uuid::new_v4()), false)
            .await,
        Err(MemoryError::NotFound(_))
    ));
    let conversation = memory.create_conversation(None, false).await.unwrap();
    assert!(matches!(
        memory
            .update_conversation(
                conversation.id,
                ConversationPatch {
                    title: Some(String::new()),
                    ..ConversationPatch::default()
                }
            )
            .await,
        Err(MemoryError::Invalid(_))
    ));
}

#[tokio::test]
async fn temporary_chats_work_without_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jarvis.db");
    std::fs::write(&path, vec![0x42; 4096]).unwrap();
    let memory = MemoryEngine::open(&path).await;
    assert!(memory.unavailable_reason().is_some());
    assert!(matches!(
        memory.create_conversation(None, false).await,
        Err(MemoryError::Unavailable(_))
    ));
    let temporary = memory.create_conversation(None, true).await.unwrap();
    memory
        .append_message(temporary.id, user("hello"))
        .await
        .unwrap();
    let context = memory
        .prepare_context(request(temporary.id, "again", text_retrieval()))
        .await
        .unwrap();
    assert_eq!(context.messages.len(), 3);
}
