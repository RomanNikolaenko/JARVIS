//! Memory Engine: the five memory layers behind one API.
//!
//! | Layer        | Stored as                                   | Used for                     |
//! |--------------|---------------------------------------------|------------------------------|
//! | Working      | pinned note + recent turns of a conversation | every request                |
//! | Conversation | conversations, messages, summaries          | history, recall, compaction  |
//! | Semantic     | facts (global or per project)               | pinned and relevant facts    |
//! | Project      | projects (description, instructions)        | project context injection    |
//! | Episodic     | episodes (actions JARVIS performed)         | the Memory screen and search |
//!
//! Temporary conversations live only in this process. If the database
//! cannot be opened, the engine still serves temporary conversations and
//! reports the reason everywhere else.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jarvis_storage::Database;
use uuid::Uuid;

use crate::context::{
    self, AssembledContext, CompactionPlan, ContextInput, ContextWindow, MemoryItem, MemoryItemKind,
};
use crate::error::{MemoryError, Result};
use crate::store::{self, ConversationFilter, FactScope, MessageHit};
use crate::text::{self, TermMatch, TextQuery, format_date, truncate_chars};
use crate::types::{
    Conversation, ConversationDetail, ConversationPatch, ConversationSummary, Episode, Fact,
    FactInput, FactSource, MAX_FACT_CHARS, MAX_FACTS, MAX_MESSAGE_CHARS, MAX_NOTE_CHARS,
    MAX_PROJECT_DESCRIPTION_CHARS, MAX_PROJECT_INSTRUCTIONS_CHARS, MAX_PROJECT_NAME_CHARS,
    MAX_PROJECTS, MAX_TITLE_CHARS, MemoryCounts, MemoryKind, MessageRole, MessageStatus,
    NewMessage, Project, ProjectInput, SearchHit, SearchQuery, StoredMessage,
};
use crate::vector::{self, Embedder, SimilarityCuts, VectorIndex, VectorKind};

pub const DEFAULT_TITLE: &str = "New conversation";
const TITLE_CHARS: usize = 60;
/// Reciprocal rank fusion constant.
const RRF_K: f32 = 60.0;
/// Minimum share of query terms a text match must contain to be recalled.
const MIN_TERM_OVERLAP: f32 = 0.5;
const MAX_RELEVANT_FACTS: usize = 6;
const MAX_RECALLS: usize = 3;
const MAX_RECALLS_PER_CONVERSATION: usize = 2;
const RECALL_CHARS: usize = 400;
const EMBED_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SEARCH_CHARS: usize = 500;
const MAX_SEARCH_RESULTS: u32 = 100;
/// Text sent to the embedding model per item.
const EMBED_CHARS: usize = 2_000;

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Conversation title from its first message.
pub(crate) fn title_from(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        DEFAULT_TITLE.to_owned()
    } else {
        truncate_chars(&collapsed, TITLE_CHARS)
    }
}

/// What to retrieve for a request.
#[derive(Clone, Default)]
pub struct Retrieval {
    /// Facts (pinned and relevant).
    pub facts: bool,
    /// Excerpts of other conversations.
    pub recall_conversations: bool,
    /// Semantic search; without it retrieval is text-only.
    pub embedder: Option<Arc<dyn Embedder>>,
}

impl std::fmt::Debug for Retrieval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Retrieval")
            .field("facts", &self.facts)
            .field("recall_conversations", &self.recall_conversations)
            .field(
                "embedder",
                &self.embedder.as_ref().map(|e| e.model_key().to_owned()),
            )
            .finish()
    }
}

/// Input of [`MemoryEngine::prepare_context`].
#[derive(Debug, Clone)]
pub struct ContextRequest {
    pub conversation_id: Uuid,
    pub user_message: String,
    pub system_prompt: String,
    pub window: ContextWindow,
    pub retrieval: Retrieval,
}

#[derive(Debug)]
struct TemporaryConversation {
    conversation: Conversation,
    messages: Vec<StoredMessage>,
    summary: Option<ConversationSummary>,
}

#[derive(Debug)]
pub struct MemoryEngine {
    db: Option<Database>,
    unavailable: Option<String>,
    temporary: Mutex<HashMap<Uuid, TemporaryConversation>>,
    vectors: tokio::sync::Mutex<Option<VectorIndex>>,
    temporary_seq: AtomicI64,
}

impl MemoryEngine {
    /// Opens the database at `path`. Never fails: if the database cannot be
    /// opened, the engine works without it and reports why.
    pub async fn open(path: impl Into<PathBuf>) -> Self {
        match Database::open(path).await {
            Ok(db) => Self::with_database(Some(db), None),
            Err(err) => {
                tracing::error!("memory database unavailable: {err}");
                Self::with_database(None, Some(err.to_string()))
            }
        }
    }

    /// An engine on a private in-memory database (tests).
    pub fn in_memory() -> Result<Self> {
        Ok(Self::with_database(Some(Database::open_in_memory()?), None))
    }

    fn with_database(db: Option<Database>, unavailable: Option<String>) -> Self {
        Self {
            db,
            unavailable,
            temporary: Mutex::default(),
            vectors: tokio::sync::Mutex::default(),
            temporary_seq: AtomicI64::new(1),
        }
    }

    pub fn database(&self) -> Option<&Database> {
        self.db.as_ref()
    }

    /// Why the database is not available, if it is not.
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    fn db(&self) -> Result<Database> {
        self.db.clone().ok_or_else(|| {
            MemoryError::Unavailable(
                self.unavailable
                    .clone()
                    .unwrap_or_else(|| "the database is not open".to_owned()),
            )
        })
    }

    fn temporary(&self) -> MutexGuard<'_, HashMap<Uuid, TemporaryConversation>> {
        self.temporary
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn is_temporary(&self, id: Uuid) -> bool {
        self.temporary().contains_key(&id)
    }

    // ---- Conversations ----------------------------------------------------

    pub async fn create_conversation(
        &self,
        project_id: Option<Uuid>,
        temporary: bool,
    ) -> Result<Conversation> {
        if let Some(project) = project_id {
            self.project(project).await?;
        }
        let now = now_ms();
        let conversation = Conversation {
            id: Uuid::new_v4(),
            title: DEFAULT_TITLE.to_owned(),
            project_id,
            pinned_note: String::new(),
            temporary,
            message_count: 0,
            created_at_ms: now,
            updated_at_ms: now,
        };
        if temporary {
            self.temporary().insert(
                conversation.id,
                TemporaryConversation {
                    conversation: conversation.clone(),
                    messages: Vec::new(),
                    summary: None,
                },
            );
        } else {
            let stored = conversation.clone();
            self.db()?
                .call(move |conn| store::insert_conversation(conn, &stored))
                .await?;
        }
        Ok(conversation)
    }

    /// Temporary conversations first, then saved ones by last activity.
    /// `query` matches titles and message text.
    pub async fn list_conversations(
        &self,
        query: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Conversation>> {
        let query = query.map(str::trim).filter(|q| !q.is_empty());
        let mut list: Vec<Conversation> = self
            .temporary()
            .values()
            .filter(|t| {
                query.is_none_or(|q| {
                    t.conversation
                        .title
                        .to_lowercase()
                        .contains(&q.to_lowercase())
                })
            })
            .map(|t| t.conversation.clone())
            .collect();
        list.sort_by_key(|c| std::cmp::Reverse(c.updated_at_ms));
        if let Ok(db) = self.db() {
            let filter = ConversationFilter {
                project: None,
                fts: query.and_then(text::match_query),
                title_like: query.map(store::like_pattern),
                limit: limit.clamp(1, 1000),
            };
            list.extend(
                db.call(move |conn| store::list_conversations(conn, &filter))
                    .await?,
            );
        }
        Ok(list)
    }

    pub async fn conversation(&self, id: Uuid) -> Result<ConversationDetail> {
        if let Some(temporary) = self.temporary().get(&id) {
            return Ok(ConversationDetail {
                conversation: temporary.conversation.clone(),
                messages: temporary.messages.clone(),
                summary: temporary.summary.clone(),
            });
        }
        let detail = self
            .db()?
            .call(move |conn| {
                let Some(conversation) = store::get_conversation(conn, id)? else {
                    return Ok(None);
                };
                Ok::<_, MemoryError>(Some(ConversationDetail {
                    conversation,
                    messages: store::list_messages(conn, id)?,
                    summary: store::get_summary(conn, id)?,
                }))
            })
            .await?;
        detail.ok_or_else(|| not_found("conversation"))
    }

    pub async fn update_conversation(
        &self,
        id: Uuid,
        patch: ConversationPatch,
    ) -> Result<Conversation> {
        let title = patch
            .title
            .as_deref()
            .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "));
        if let Some(title) = &title {
            check_length("title", title, 1, MAX_TITLE_CHARS)?;
        }
        if let Some(note) = &patch.pinned_note {
            check_length("pinned note", note, 0, MAX_NOTE_CHARS)?;
        }
        let project = if patch.clear_project {
            Some(None)
        } else {
            patch.project_id.map(Some)
        };
        if let Some(Some(project_id)) = project {
            self.project(project_id).await?;
        }
        {
            let mut temporary = self.temporary();
            if let Some(entry) = temporary.get_mut(&id) {
                let conversation = &mut entry.conversation;
                if let Some(title) = title {
                    conversation.title = title;
                }
                if let Some(project) = project {
                    conversation.project_id = project;
                }
                if let Some(note) = patch.pinned_note {
                    conversation.pinned_note = note;
                }
                conversation.updated_at_ms = now_ms();
                return Ok(conversation.clone());
            }
        }
        let db = self.db()?;
        let updated = db
            .call(move |conn| {
                let changed = store::update_conversation(
                    conn,
                    id,
                    title.as_deref(),
                    project,
                    patch.pinned_note.as_deref(),
                    now_ms(),
                )?;
                if changed {
                    store::get_conversation(conn, id)
                } else {
                    Ok(None)
                }
            })
            .await?;
        updated.ok_or_else(|| not_found("conversation"))
    }

    pub async fn delete_conversation(&self, id: Uuid) -> Result<()> {
        if self.temporary().remove(&id).is_some() {
            return Ok(());
        }
        let deleted = self
            .db()?
            .call(move |conn| store::delete_conversation(conn, id))
            .await?;
        if !deleted {
            return Err(not_found("conversation"));
        }
        self.invalidate_vectors().await;
        Ok(())
    }

    /// Appends a message. Saved conversations update their title from the
    /// first user message.
    pub async fn append_message(
        &self,
        conversation_id: Uuid,
        message: NewMessage,
    ) -> Result<StoredMessage> {
        check_length("message", &message.content, 0, MAX_MESSAGE_CHARS)?;
        {
            let mut temporary = self.temporary();
            if let Some(entry) = temporary.get_mut(&conversation_id) {
                let now = now_ms();
                let stored = StoredMessage {
                    id: message.id,
                    seq: self.temporary_seq.fetch_add(1, Ordering::Relaxed),
                    role: message.role,
                    tokens: text::estimate_tokens(&message.content),
                    content: message.content,
                    status: message.status,
                    provider_id: message.provider_id,
                    model: message.model,
                    created_at_ms: now,
                };
                let conversation = &mut entry.conversation;
                if stored.role == MessageRole::User && conversation.title == DEFAULT_TITLE {
                    conversation.title = title_from(&stored.content);
                }
                conversation.message_count += 1;
                conversation.updated_at_ms = now;
                entry.messages.push(stored.clone());
                return Ok(stored);
            }
        }
        let db = self.db()?;
        db.call(move |conn| {
            if store::get_conversation(conn, conversation_id)?.is_none() {
                return Err(not_found("conversation"));
            }
            store::insert_message(conn, conversation_id, &message, now_ms(), DEFAULT_TITLE)
        })
        .await
    }

    pub async fn store_summary(
        &self,
        conversation_id: Uuid,
        content: String,
        covers_until: i64,
        model: Option<String>,
    ) -> Result<()> {
        let content = truncate_chars(content.trim(), 3_000);
        if content.is_empty() {
            return Err(MemoryError::Invalid("the summary is empty".to_owned()));
        }
        {
            let mut temporary = self.temporary();
            if let Some(entry) = temporary.get_mut(&conversation_id) {
                let covered = entry
                    .messages
                    .iter()
                    .filter(|m| m.seq <= covers_until)
                    .count();
                entry.summary = Some(ConversationSummary {
                    tokens: text::estimate_tokens(&content),
                    content,
                    covered_messages: covered as u32,
                    model,
                    updated_at_ms: now_ms(),
                    covers_until,
                });
                return Ok(());
            }
        }
        self.db()?
            .call(move |conn| {
                store::put_summary(
                    conn,
                    conversation_id,
                    &content,
                    covers_until,
                    model.as_deref(),
                    now_ms(),
                )
            })
            .await
    }

    // ---- Context ----------------------------------------------------------

    /// Builds the prompt for a new user message (not yet appended).
    pub async fn prepare_context(&self, request: ContextRequest) -> Result<AssembledContext> {
        let detail = self.conversation(request.conversation_id).await?;
        let project = match detail.conversation.project_id {
            Some(id) => self.project(id).await.ok(),
            None => None,
        };
        let memory = self
            .retrieve(
                &request.user_message,
                &detail.conversation,
                &request.retrieval,
            )
            .await;
        context::assemble(&ContextInput {
            system_prompt: &request.system_prompt,
            project: project.as_ref(),
            pinned_note: &detail.conversation.pinned_note,
            memory: &memory,
            summary: detail.summary.as_ref(),
            history: &detail.messages,
            user_message: &request.user_message,
            window: request.window,
        })
    }

    /// Plans a summarization of the conversation's older messages.
    pub async fn compaction_plan(
        &self,
        conversation_id: Uuid,
        window: ContextWindow,
    ) -> Result<Option<CompactionPlan>> {
        let detail = self.conversation(conversation_id).await?;
        Ok(context::compaction_plan(
            &detail.messages,
            detail.summary.as_ref(),
            window,
        ))
    }

    /// Pinned facts, relevant facts and excerpts of other conversations.
    /// Failures are logged: memory never breaks a chat.
    async fn retrieve(
        &self,
        query: &str,
        conversation: &Conversation,
        retrieval: &Retrieval,
    ) -> Vec<MemoryItem> {
        if !retrieval.facts && !retrieval.recall_conversations {
            return Vec::new();
        }
        match self.retrieve_inner(query, conversation, retrieval).await {
            Ok(items) => items,
            Err(err) => {
                tracing::warn!("memory retrieval failed: {err}");
                Vec::new()
            }
        }
    }

    async fn retrieve_inner(
        &self,
        query: &str,
        conversation: &Conversation,
        retrieval: &Retrieval,
    ) -> Result<Vec<MemoryItem>> {
        let db = self.db()?;
        let project = conversation.project_id;
        let text_query = TextQuery::new(query);
        let fts = text_query.match_expression();
        let query_vector = match &retrieval.embedder {
            Some(embedder) => embed_query(embedder.as_ref(), query).await,
            None => None,
        };

        let (want_facts, want_recall) = (retrieval.facts, retrieval.recall_conversations);
        let (pinned, fts_facts, fts_messages) = db
            .call(move |conn| {
                let scope = FactScope::Context(project);
                let pinned = if want_facts {
                    store::list_facts(conn, scope, true)?
                } else {
                    Vec::new()
                };
                let (facts, messages) = match &fts {
                    Some(q) => (
                        if want_facts {
                            store::fts_facts(conn, q, scope, 20)?
                        } else {
                            Vec::new()
                        },
                        if want_recall {
                            store::fts_messages(conn, q, 40)?
                        } else {
                            Vec::new()
                        },
                    ),
                    None => (Vec::new(), Vec::new()),
                };
                Ok::<_, MemoryError>((pinned, facts, messages))
            })
            .await?;

        // Text hits that may be recalled, judged against the best of them.
        let eligible = |hit: &MessageHit| {
            hit.conversation_id != conversation.id
                && hit.project_id == project
                && hit.status == MessageStatus::Complete
        };
        let fts_facts = text_query.judge(
            fts_facts
                .into_iter()
                .filter(|h| !h.item.fact.pinned)
                .collect(),
            |h| &h.item.fact.content,
        );
        let fts_messages = text_query.judge(
            fts_messages
                .into_iter()
                .map(|h| h.item)
                .filter(|h| eligible(h))
                .collect(),
            |h| &h.content,
        );

        let (mut fact_check, mut message_check) =
            (SemanticCheck::default(), SemanticCheck::default());
        let (recall_cut, vector_facts, vector_messages) = match &query_vector {
            Some((key, vector)) => {
                let cut = SimilarityCuts::for_model(key).recall;
                let index = self.vector_index(&db, key).await?;
                let index = index.as_ref();
                fact_check = SemanticCheck::new(
                    index,
                    VectorKind::Fact,
                    weak_ids(&fts_facts, |h| h.item.row_id),
                );
                message_check = SemanticCheck::new(
                    index,
                    VectorKind::Message,
                    weak_ids(&fts_messages, |h| h.row_id),
                );
                let search = |kind, limit| {
                    index
                        .map(|i| i.search(kind, vector, limit))
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|(_, s)| *s >= cut.min)
                        .collect::<Vec<_>>()
                };
                (
                    Some(cut),
                    if want_facts {
                        search(VectorKind::Fact, 20)
                    } else {
                        Vec::new()
                    },
                    if want_recall {
                        search(VectorKind::Message, 40)
                    } else {
                        Vec::new()
                    },
                )
            }
            None => (None, Vec::new(), Vec::new()),
        };
        // Applied to the hits that may be recalled, so the margin is measured
        // from the best of them, not from a fact of another project or a
        // message of this conversation.
        let cut = |hits: Vec<(i64, f32)>| match recall_cut {
            Some(cut) => cut.apply(hits),
            None => hits,
        };

        // Rows for vector-only candidates.
        let fact_ids: Vec<i64> = vector_facts.iter().map(|(id, _)| *id).collect();
        let message_ids: Vec<i64> = vector_messages.iter().map(|(id, _)| *id).collect();
        let (vector_fact_rows, vector_message_rows) = db
            .call(move |conn| {
                Ok::<_, MemoryError>((
                    store::facts_by_row_ids(conn, &fact_ids)?,
                    store::messages_by_row_ids(conn, &message_ids)?,
                ))
            })
            .await?;

        let mut items: Vec<MemoryItem> = pinned
            .iter()
            .map(|row| MemoryItem {
                kind: MemoryItemKind::PinnedFact,
                text: row.fact.content.clone(),
            })
            .collect();

        // Relevant facts (not pinned, in scope).
        let in_scope = |fact: &Fact| fact.project_id.is_none() || fact.project_id == project;
        let recallable_fact = |row_id: i64| {
            vector_fact_rows
                .iter()
                .find(|r| r.row_id == row_id && !r.fact.pinned && in_scope(&r.fact))
        };
        let vector_facts = cut(vector_facts
            .into_iter()
            .filter(|(id, _)| recallable_fact(*id).is_some())
            .collect());
        fact_check.set_matched(&vector_facts);
        let mut fused: HashMap<i64, (f32, Fact)> = HashMap::new();
        for (rank, (hit, _)) in fts_facts
            .iter()
            .filter(|(hit, judged)| {
                judged.overlap >= MIN_TERM_OVERLAP && fact_check.keeps(hit.item.row_id, *judged)
            })
            .enumerate()
        {
            let entry = fused
                .entry(hit.item.row_id)
                .or_insert((0.0, hit.item.fact.clone()));
            entry.0 += rrf(rank);
        }
        for (rank, (row_id, _)) in vector_facts.iter().enumerate() {
            if let Some(row) = recallable_fact(*row_id) {
                let entry = fused.entry(*row_id).or_insert((0.0, row.fact.clone()));
                entry.0 += rrf(rank);
            }
        }
        let mut facts: Vec<(f32, Fact)> = fused.into_values().collect();
        facts.sort_by(|a, b| b.0.total_cmp(&a.0));
        items.extend(
            facts
                .into_iter()
                .take(MAX_RELEVANT_FACTS)
                .map(|(_, fact)| MemoryItem {
                    kind: MemoryItemKind::Fact,
                    text: fact.content,
                }),
        );

        // Excerpts of other conversations in the same scope.
        let recallable_message = |row_id: i64| {
            vector_message_rows
                .iter()
                .find(|h| h.row_id == row_id && eligible(h))
        };
        let vector_messages = cut(vector_messages
            .into_iter()
            .filter(|(id, _)| recallable_message(*id).is_some())
            .collect());
        message_check.set_matched(&vector_messages);
        let mut fused: HashMap<i64, (f32, MessageHit)> = HashMap::new();
        for (rank, (hit, _)) in fts_messages
            .iter()
            .filter(|(hit, judged)| {
                judged.overlap >= MIN_TERM_OVERLAP && message_check.keeps(hit.row_id, *judged)
            })
            .enumerate()
        {
            let entry = fused.entry(hit.row_id).or_insert((0.0, hit.clone()));
            entry.0 += rrf(rank);
        }
        for (rank, (row_id, _)) in vector_messages.iter().enumerate() {
            if let Some(hit) = recallable_message(*row_id) {
                let entry = fused.entry(*row_id).or_insert((0.0, hit.clone()));
                entry.0 += rrf(rank);
            }
        }
        let mut recalls: Vec<(f32, MessageHit)> = fused.into_values().collect();
        recalls.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut per_conversation: HashMap<Uuid, usize> = HashMap::new();
        for (_, hit) in recalls {
            let count = per_conversation.entry(hit.conversation_id).or_default();
            if *count >= MAX_RECALLS_PER_CONVERSATION {
                continue;
            }
            *count += 1;
            let speaker = match hit.role {
                MessageRole::User => "User",
                MessageRole::Assistant => "JARVIS",
            };
            items.push(MemoryItem {
                kind: MemoryItemKind::Recall,
                text: format!(
                    "({}, conversation \"{}\") {speaker}: {}",
                    format_date(hit.created_at_ms),
                    hit.conversation_title,
                    truncate_chars(hit.content.trim(), RECALL_CHARS)
                ),
            });
            if items
                .iter()
                .filter(|i| i.kind == MemoryItemKind::Recall)
                .count()
                >= MAX_RECALLS
            {
                break;
            }
        }
        Ok(items)
    }

    // ---- Facts ------------------------------------------------------------

    pub async fn list_facts(&self) -> Result<Vec<Fact>> {
        let rows = self
            .db()?
            .call(|conn| store::list_facts(conn, FactScope::All, false))
            .await?;
        Ok(rows.into_iter().map(|r| r.fact).collect())
    }

    pub async fn save_fact(&self, input: FactInput, source: FactSource) -> Result<Fact> {
        let content = input.content.trim().to_owned();
        check_length("fact", &content, 1, MAX_FACT_CHARS)?;
        if let Some(project) = input.project_id {
            self.project(project).await?;
        }
        let db = self.db()?;
        let saved = db
            .call(move |conn| {
                let now = now_ms();
                let source_conversation = match input.source_conversation_id {
                    Some(id) if store::get_conversation(conn, id)?.is_some() => Some(id),
                    _ => None,
                };
                match input.id {
                    Some(id) => {
                        let Some(existing) = store::get_fact(conn, id)? else {
                            return Err(not_found("fact"));
                        };
                        let fact = Fact {
                            content,
                            project_id: input.project_id,
                            pinned: input.pinned,
                            updated_at_ms: now,
                            ..existing.fact
                        };
                        store::update_fact(conn, &fact)?;
                        Ok(fact)
                    }
                    None => {
                        if store::counts(conn)?.facts >= MAX_FACTS {
                            return Err(MemoryError::Invalid(format!(
                                "memory holds at most {MAX_FACTS} facts"
                            )));
                        }
                        let fact = Fact {
                            id: Uuid::new_v4(),
                            content,
                            project_id: input.project_id,
                            pinned: input.pinned,
                            source,
                            source_conversation_id: source_conversation,
                            created_at_ms: now,
                            updated_at_ms: now,
                        };
                        store::insert_fact(conn, &fact)?;
                        Ok(fact)
                    }
                }
            })
            .await?;
        Ok(saved)
    }

    pub async fn delete_fact(&self, id: Uuid) -> Result<()> {
        let deleted = self
            .db()?
            .call(move |conn| store::delete_fact(conn, id))
            .await?;
        if !deleted {
            return Err(not_found("fact"));
        }
        self.invalidate_vectors().await;
        Ok(())
    }

    // ---- Projects ---------------------------------------------------------

    pub async fn project(&self, id: Uuid) -> Result<Project> {
        self.db()?
            .call(move |conn| store::get_project(conn, id))
            .await?
            .ok_or_else(|| not_found("project"))
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        self.db()?.call(|conn| store::list_projects(conn)).await
    }

    pub async fn save_project(&self, input: ProjectInput) -> Result<Project> {
        let name = input.name.split_whitespace().collect::<Vec<_>>().join(" ");
        check_length("project name", &name, 1, MAX_PROJECT_NAME_CHARS)?;
        check_length(
            "project description",
            &input.description,
            0,
            MAX_PROJECT_DESCRIPTION_CHARS,
        )?;
        check_length(
            "project instructions",
            &input.instructions,
            0,
            MAX_PROJECT_INSTRUCTIONS_CHARS,
        )?;
        let db = self.db()?;
        db.call(move |conn| {
            let now = now_ms();
            let project = match input.id {
                Some(id) => {
                    let Some(existing) = store::get_project(conn, id)? else {
                        return Err(not_found("project"));
                    };
                    Project {
                        name,
                        description: input.description.trim().to_owned(),
                        instructions: input.instructions.trim().to_owned(),
                        updated_at_ms: now,
                        ..existing
                    }
                }
                None => {
                    if store::counts(conn)?.projects >= MAX_PROJECTS {
                        return Err(MemoryError::Invalid(format!(
                            "memory holds at most {MAX_PROJECTS} projects"
                        )));
                    }
                    Project {
                        id: Uuid::new_v4(),
                        name,
                        description: input.description.trim().to_owned(),
                        instructions: input.instructions.trim().to_owned(),
                        fact_count: 0,
                        conversation_count: 0,
                        created_at_ms: now,
                        updated_at_ms: now,
                    }
                }
            };
            store::upsert_project(conn, &project)?;
            store::get_project(conn, project.id)?.ok_or_else(|| not_found("project"))
        })
        .await
    }

    /// Deletes a project and its facts; its conversations are kept without
    /// a project.
    pub async fn delete_project(&self, id: Uuid) -> Result<()> {
        let deleted = self
            .db()?
            .call(move |conn| store::delete_project(conn, id))
            .await?;
        if !deleted {
            return Err(not_found("project"));
        }
        for entry in self.temporary().values_mut() {
            if entry.conversation.project_id == Some(id) {
                entry.conversation.project_id = None;
            }
        }
        self.invalidate_vectors().await;
        Ok(())
    }

    // ---- Episodes ---------------------------------------------------------

    pub async fn record_episodes(&self, episodes: Vec<Episode>) -> Result<()> {
        if episodes.is_empty() {
            return Ok(());
        }
        self.db()?
            .call(move |conn| {
                let tx = conn.transaction()?;
                store::insert_episodes(&tx, &episodes)?;
                tx.commit()?;
                Ok::<_, MemoryError>(())
            })
            .await
    }

    pub async fn list_episodes(&self, limit: u32, before_ms: Option<i64>) -> Result<Vec<Episode>> {
        self.db()?
            .call(move |conn| store::list_episodes(conn, limit.clamp(1, 1000), before_ms))
            .await
    }

    /// Deletes episodes older than `before_ms`, or all. Returns the count.
    pub async fn delete_episodes(&self, before_ms: Option<i64>) -> Result<usize> {
        self.db()?
            .call(move |conn| store::delete_episodes(conn, before_ms))
            .await
    }

    // ---- Search -----------------------------------------------------------

    /// Hybrid search: full-text (all kinds) fused with vector similarity
    /// (messages and facts) when an embedder is available. A text hit must
    /// share more than half as many terms as the best one, and a weak one
    /// (one word of a question) falls when the semantic side passes it over.
    pub async fn search(
        &self,
        query: SearchQuery,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Result<Vec<SearchHit>> {
        let text = query.text.trim().to_owned();
        check_length("search text", &text, 1, MAX_SEARCH_CHARS)?;
        let limit = query.limit.clamp(1, MAX_SEARCH_RESULTS);
        let wants = |kind| query.kinds.is_empty() || query.kinds.contains(&kind);
        let (want_messages, want_facts, want_episodes) = (
            wants(MemoryKind::Message),
            wants(MemoryKind::Fact),
            wants(MemoryKind::Episode),
        );
        let db = self.db()?;
        let text_query = TextQuery::new(&text);
        let fts = text_query.match_expression();
        let query_vector = match &embedder {
            Some(embedder) => embed_query(embedder.as_ref(), &text).await,
            None => None,
        };

        let (messages, facts, episodes) = db
            .call(move |conn| {
                let Some(q) = fts else {
                    return Ok::<_, MemoryError>((Vec::new(), Vec::new(), Vec::new()));
                };
                Ok((
                    if want_messages {
                        store::fts_messages(conn, &q, 50)?
                    } else {
                        Vec::new()
                    },
                    if want_facts {
                        store::fts_facts(conn, &q, FactScope::All, 50)?
                    } else {
                        Vec::new()
                    },
                    if want_episodes {
                        store::fts_episodes(conn, &q, 50)?
                    } else {
                        Vec::new()
                    },
                ))
            })
            .await?;
        let messages = text_query.judge(messages, |h| &h.item.content);
        let facts = text_query.judge(facts, |h| &h.item.fact.content);
        let episodes = text_query.judge(episodes, |h| &h.item.summary);

        // The semantic side first: it has a say on weak text hits.
        let mut semantic: Vec<(SearchHit, f32)> = Vec::new();
        let (mut message_check, mut fact_check) =
            (SemanticCheck::default(), SemanticCheck::default());
        if let Some((key, vector)) = query_vector {
            let (message_scores, fact_scores) = {
                let cut = SimilarityCuts::for_model(&key).search;
                let index = self.vector_index(&db, &key).await?;
                let index = index.as_ref();
                message_check = SemanticCheck::new(
                    index,
                    VectorKind::Message,
                    weak_ids(&messages, |h| h.item.row_id),
                );
                fact_check = SemanticCheck::new(
                    index,
                    VectorKind::Fact,
                    weak_ids(&facts, |h| h.item.row_id),
                );
                let search = |kind| {
                    cut.apply(
                        index
                            .map(|i| i.search(kind, &vector, 50))
                            .unwrap_or_default(),
                    )
                };
                (
                    if want_messages {
                        search(VectorKind::Message)
                    } else {
                        Vec::new()
                    },
                    if want_facts {
                        search(VectorKind::Fact)
                    } else {
                        Vec::new()
                    },
                )
            };
            message_check.set_matched(&message_scores);
            fact_check.set_matched(&fact_scores);
            let message_ids: Vec<i64> = message_scores.iter().map(|(id, _)| *id).collect();
            let fact_ids: Vec<i64> = fact_scores.iter().map(|(id, _)| *id).collect();
            let (message_rows, fact_rows) = db
                .call(move |conn| {
                    Ok::<_, MemoryError>((
                        store::messages_by_row_ids(conn, &message_ids)?,
                        store::facts_by_row_ids(conn, &fact_ids)?,
                    ))
                })
                .await?;
            for (rank, (row_id, _)) in message_scores.iter().enumerate() {
                if let Some(row) = message_rows.iter().find(|r| r.row_id == *row_id) {
                    let snippet = truncate_chars(row.content.trim(), 200);
                    semantic.push((message_hit(row.clone(), snippet), rrf(rank)));
                }
            }
            for (rank, (row_id, _)) in fact_scores.iter().enumerate() {
                if let Some(row) = fact_rows.iter().find(|r| r.row_id == *row_id) {
                    let snippet = truncate_chars(row.fact.content.trim(), 200);
                    semantic.push((fact_hit(row.fact.clone(), snippet), rrf(rank)));
                }
            }
        }

        let mut hits: HashMap<(MemoryKind, Uuid), SearchHit> = HashMap::new();
        let mut add = |hit: SearchHit, score: f32, semantic: bool| {
            let entry = hits
                .entry((hit.kind, hit.id))
                .or_insert(SearchHit { score: 0.0, ..hit });
            entry.score += score;
            if semantic {
                entry.matched_semantic = true;
            } else {
                entry.matched_text = true;
            }
        };
        // Text hits first, so a hit found both ways keeps its highlights.
        for (rank, (hit, _)) in messages
            .into_iter()
            .filter(|(hit, judged)| message_check.keeps(hit.item.row_id, *judged))
            .enumerate()
        {
            add(message_hit(hit.item, hit.snippet), rrf(rank), false);
        }
        for (rank, (hit, _)) in facts
            .into_iter()
            .filter(|(hit, judged)| fact_check.keeps(hit.item.row_id, *judged))
            .enumerate()
        {
            add(fact_hit(hit.item.fact, hit.snippet), rrf(rank), false);
        }
        // Episodes have no vectors, so their weak matches stand.
        for (rank, (hit, _)) in episodes.into_iter().enumerate() {
            let episode = hit.item;
            add(
                SearchHit {
                    kind: MemoryKind::Episode,
                    id: episode.id,
                    conversation_id: None,
                    conversation_title: None,
                    role: None,
                    snippet: hit.snippet,
                    score: 0.0,
                    matched_text: false,
                    matched_semantic: false,
                    created_at_ms: episode.occurred_at_ms,
                },
                rrf(rank),
                false,
            );
        }
        for (hit, score) in semantic {
            add(hit, score, true);
        }

        let mut hits: Vec<SearchHit> = hits.into_values().collect();
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(b.created_at_ms.cmp(&a.created_at_ms))
        });
        hits.truncate(limit as usize);
        Ok(hits)
    }

    // ---- Embeddings -------------------------------------------------------

    /// Embeds up to `batch` facts and messages that have no vector for the
    /// embedder's model yet. Returns how many were indexed.
    pub async fn index_pending(&self, embedder: &dyn Embedder, batch: u32) -> Result<usize> {
        let db = self.db()?;
        let key = embedder.model_key().to_owned();
        let lookup = key.clone();
        let items = db
            .call(move |conn| store::pending_embeddings(conn, &lookup, batch.clamp(1, 256)))
            .await?;
        if items.is_empty() {
            return Ok(0);
        }
        let texts = items
            .iter()
            .map(|(_, _, text)| truncate_chars(text, EMBED_CHARS))
            .collect();
        let vectors = embedder.embed(texts).await?;
        if vectors.len() != items.len() {
            return Err(MemoryError::Embedding(format!(
                "expected {} vectors, got {}",
                items.len(),
                vectors.len()
            )));
        }
        let mut stored = Vec::with_capacity(items.len());
        for ((kind, id, _), mut vector) in items.into_iter().zip(vectors) {
            // A vector that cannot be normalized is stored empty, so the item
            // is not retried forever; it simply never matches.
            let valid = vector::normalize(&mut vector);
            stored.push((kind, id, if valid { vector } else { Vec::new() }));
        }
        let count = stored.len();
        let rows: Vec<(VectorKind, i64, Vec<u8>)> = stored
            .iter()
            .map(|(kind, id, v)| (*kind, *id, vector::encode(v)))
            .collect();
        let write_key = key.clone();
        db.call(move |conn| {
            let tx = conn.transaction()?;
            for (kind, id, blob) in &rows {
                store::put_embedding(&tx, *kind, *id, &write_key, blob)?;
            }
            tx.commit()?;
            Ok::<_, MemoryError>(())
        })
        .await?;
        let mut index = self.vectors.lock().await;
        if let Some(index) = index.as_mut().filter(|i| i.model_key() == key) {
            for (kind, id, vector) in stored {
                if !vector.is_empty() {
                    index.insert(kind, id, vector);
                }
            }
        }
        Ok(count)
    }

    /// `(indexed, pending)` for an embedding model.
    pub async fn embedding_counts(&self, model_key: &str) -> Result<(u32, u32)> {
        let key = model_key.to_owned();
        self.db()?
            .call(move |conn| store::embedding_counts(conn, &key))
            .await
    }

    /// Removes vectors of every other embedding model.
    pub async fn retain_embeddings(&self, model_key: &str) -> Result<usize> {
        let key = model_key.to_owned();
        let removed = self
            .db()?
            .call(move |conn| store::delete_other_embeddings(conn, &key))
            .await?;
        if removed > 0 {
            self.invalidate_vectors().await;
        }
        Ok(removed)
    }

    async fn vector_index(
        &self,
        db: &Database,
        model_key: &str,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<VectorIndex>>> {
        let mut guard = self.vectors.lock().await;
        if guard.as_ref().is_none_or(|i| i.model_key() != model_key) {
            let key = model_key.to_owned();
            let rows = db.call(move |conn| store::load_vectors(conn, &key)).await?;
            let mut index = VectorIndex::new(model_key);
            for (kind, id, blob) in rows {
                if let Some(vector) = vector::decode(&blob) {
                    index.insert(kind, id, vector);
                }
            }
            *guard = Some(index);
        }
        Ok(guard)
    }

    async fn invalidate_vectors(&self) {
        *self.vectors.lock().await = None;
    }

    // ---- Whole memory -----------------------------------------------------

    pub async fn counts(&self) -> Result<MemoryCounts> {
        self.db()?.call(|conn| store::counts(conn)).await
    }

    /// Deletes all saved memory and compacts the file so the content does
    /// not linger on disk. Temporary conversations are not affected.
    pub async fn clear(&self) -> Result<()> {
        let db = self.db()?;
        db.call(store::clear_all).await?;
        db.compact().await?;
        self.invalidate_vectors().await;
        Ok(())
    }

    pub(crate) async fn invalidate_after_import(&self) {
        self.invalidate_vectors().await;
    }

    pub(crate) fn db_handle(&self) -> Result<Database> {
        self.db()
    }
}

fn rrf(rank: usize) -> f32 {
    1.0 / (RRF_K + rank as f32 + 1.0)
}

/// What the semantic side says about the weak text hits of one kind.
#[derive(Debug, Default)]
struct SemanticCheck {
    /// Weak text hits that have a vector, so the semantic side judged them.
    embedded: HashSet<i64>,
    /// Items that passed the similarity cut.
    matched: HashSet<i64>,
}

impl SemanticCheck {
    fn new(index: Option<&VectorIndex>, kind: VectorKind, weak: Vec<i64>) -> Self {
        Self {
            embedded: weak
                .into_iter()
                .filter(|id| index.is_some_and(|i| i.contains(kind, *id)))
                .collect(),
            matched: HashSet::new(),
        }
    }

    fn set_matched(&mut self, hits: &[(i64, f32)]) {
        self.matched = hits.iter().map(|(id, _)| *id).collect();
    }

    /// Whether a text hit stands. A weak one falls when the semantic side
    /// matched other items of its kind and passed over this one although it
    /// has a vector: "Where do I work?" finds the job fact by meaning, so
    /// "…works as a teacher" is a coincidence of one word. Without a query
    /// vector, a semantic match or a vector of the hit, it stands.
    fn keeps(&self, row_id: i64, judged: TermMatch) -> bool {
        !judged.weak
            || self.matched.is_empty()
            || !self.embedded.contains(&row_id)
            || self.matched.contains(&row_id)
    }
}

/// Row ids of the weak text hits.
fn weak_ids<T>(hits: &[(T, TermMatch)], row_id: impl Fn(&T) -> i64) -> Vec<i64> {
    hits.iter()
        .filter(|(_, judged)| judged.weak)
        .map(|(hit, _)| row_id(hit))
        .collect()
}

fn not_found(what: &str) -> MemoryError {
    MemoryError::NotFound(format!("the {what} does not exist"))
}

fn check_length(what: &str, value: &str, min: usize, max: usize) -> Result<()> {
    let chars = value.trim().chars().count();
    if chars < min {
        return Err(MemoryError::Invalid(format!(
            "the {what} must not be empty"
        )));
    }
    if value.chars().count() > max {
        return Err(MemoryError::Invalid(format!(
            "the {what} may contain at most {max} characters"
        )));
    }
    Ok(())
}

/// Embeds a query, normalized. Slow or failing embedders fall back to
/// text-only retrieval.
async fn embed_query(embedder: &dyn Embedder, text: &str) -> Option<(String, Vec<f32>)> {
    let request = embedder.embed(vec![truncate_chars(text, EMBED_CHARS)]);
    match tokio::time::timeout(EMBED_QUERY_TIMEOUT, request).await {
        Ok(Ok(mut vectors)) => {
            let mut vector = vectors.pop()?;
            vector::normalize(&mut vector).then(|| (embedder.model_key().to_owned(), vector))
        }
        Ok(Err(err)) => {
            tracing::warn!("query embedding failed, using text search only: {err}");
            None
        }
        Err(_) => {
            tracing::warn!("query embedding timed out, using text search only");
            None
        }
    }
}

fn message_hit(hit: MessageHit, snippet: String) -> SearchHit {
    SearchHit {
        kind: MemoryKind::Message,
        id: hit.id,
        conversation_id: Some(hit.conversation_id),
        conversation_title: Some(hit.conversation_title),
        role: Some(hit.role),
        snippet,
        score: 0.0,
        matched_text: false,
        matched_semantic: false,
        created_at_ms: hit.created_at_ms,
    }
}

fn fact_hit(fact: Fact, snippet: String) -> SearchHit {
    SearchHit {
        kind: MemoryKind::Fact,
        id: fact.id,
        conversation_id: fact.source_conversation_id,
        conversation_title: None,
        role: None,
        snippet,
        score: 0.0,
        matched_text: false,
        matched_semantic: false,
        created_at_ms: fact.updated_at_ms,
    }
}
