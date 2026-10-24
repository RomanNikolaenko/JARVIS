//! SQL for the memory tables. Every function takes a connection and runs on
//! the database's blocking thread (see [`jarvis_storage::Database::call`]).

use jarvis_storage::rusqlite::{self, Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use crate::error::Result;
use crate::text::estimate_tokens;
use crate::types::{
    Conversation, ConversationSummary, Episode, EpisodeCursor, Fact, FactSource, MemoryCounts,
    MessageRole, MessageStatus, NewMessage, Project, StoredMessage,
};
use crate::vector::VectorKind;

fn parse_uuid(text: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(text).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn uuid_at(row: &Row<'_>, index: usize) -> rusqlite::Result<Uuid> {
    parse_uuid(&row.get::<_, String>(index)?)
}

fn opt_uuid_at(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<Uuid>> {
    row.get::<_, Option<String>>(index)?
        .map(|s| parse_uuid(&s))
        .transpose()
}

fn opt_str(id: Option<Uuid>) -> Option<String> {
    id.map(|id| id.to_string())
}

// ---------------------------------------------------------------------------
// Conversations and messages
// ---------------------------------------------------------------------------

const CONVERSATION_COLUMNS: &str =
    "c.id, c.title, c.project_id, c.pinned_note, c.created_at, c.updated_at,
     (SELECT count(*) FROM messages m WHERE m.conversation_id = c.id)";

fn conversation_row(row: &Row<'_>) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: uuid_at(row, 0)?,
        title: row.get(1)?,
        project_id: opt_uuid_at(row, 2)?,
        pinned_note: row.get(3)?,
        temporary: false,
        created_at_ms: row.get(4)?,
        updated_at_ms: row.get(5)?,
        message_count: row.get(6)?,
    })
}

pub fn insert_conversation(conn: &Connection, conversation: &Conversation) -> Result<()> {
    conn.execute(
        "INSERT INTO conversations (id, title, project_id, pinned_note, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            conversation.id.to_string(),
            conversation.title,
            opt_str(conversation.project_id),
            conversation.pinned_note,
            conversation.created_at_ms,
            conversation.updated_at_ms
        ],
    )?;
    Ok(())
}

pub fn get_conversation(conn: &Connection, id: Uuid) -> Result<Option<Conversation>> {
    Ok(conn
        .query_row(
            &format!("SELECT {CONVERSATION_COLUMNS} FROM conversations c WHERE c.id = ?1"),
            [id.to_string()],
            conversation_row,
        )
        .optional()?)
}

/// Which conversations to list.
#[derive(Debug, Clone, Default)]
pub struct ConversationFilter {
    /// `None`: all; `Some(None)`: without project; `Some(Some(id))`: in project.
    pub project: Option<Option<Uuid>>,
    /// FTS5 expression matched against message text (titles are matched
    /// with `title_like`).
    pub fts: Option<String>,
    pub title_like: Option<String>,
    pub limit: u32,
}

pub fn list_conversations(
    conn: &Connection,
    filter: &ConversationFilter,
) -> Result<Vec<Conversation>> {
    let mut sql = format!("SELECT {CONVERSATION_COLUMNS} FROM conversations c WHERE 1 = 1");
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(project) = &filter.project {
        values.push(opt_str(*project).into());
        sql.push_str(&format!(" AND c.project_id IS ?{}", values.len()));
    }
    match (&filter.fts, &filter.title_like) {
        (Some(fts), Some(like)) => {
            values.push(like.clone().into());
            let like_index = values.len();
            values.push(fts.clone().into());
            sql.push_str(&format!(
                " AND (c.title LIKE ?{like_index} ESCAPE '\\' OR c.id IN (
                     SELECT m.conversation_id FROM messages m
                     WHERE m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?{})))",
                values.len()
            ));
        }
        (None, Some(like)) => {
            values.push(like.clone().into());
            sql.push_str(&format!(" AND c.title LIKE ?{} ESCAPE '\\'", values.len()));
        }
        (Some(fts), None) => {
            values.push(fts.clone().into());
            sql.push_str(&format!(
                " AND c.id IN (SELECT m.conversation_id FROM messages m
                     WHERE m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?{}))",
                values.len()
            ));
        }
        (None, None) => {}
    }
    values.push(i64::from(filter.limit.max(1)).into());
    sql.push_str(&format!(
        " ORDER BY c.updated_at DESC LIMIT ?{}",
        values.len()
    ));
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(values), conversation_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// `LIKE` pattern matching `text` anywhere, with wildcards escaped.
pub fn like_pattern(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

pub fn update_conversation(
    conn: &Connection,
    id: Uuid,
    title: Option<&str>,
    project: Option<Option<Uuid>>,
    pinned_note: Option<&str>,
    now: i64,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE conversations SET
             title = coalesce(?2, title),
             project_id = CASE WHEN ?3 THEN ?4 ELSE project_id END,
             pinned_note = coalesce(?5, pinned_note),
             updated_at = ?6
         WHERE id = ?1",
        params![
            id.to_string(),
            title,
            project.is_some(),
            project.flatten().map(|p| p.to_string()),
            pinned_note,
            now
        ],
    )?;
    Ok(changed > 0)
}

pub fn delete_conversation(conn: &Connection, id: Uuid) -> Result<bool> {
    Ok(conn.execute("DELETE FROM conversations WHERE id = ?1", [id.to_string()])? > 0)
}

const MESSAGE_COLUMNS: &str =
    "id, uid, role, content, status, provider_id, model, tokens, created_at";

fn message_row(row: &Row<'_>) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        seq: row.get(0)?,
        id: uuid_at(row, 1)?,
        role: MessageRole::parse(&row.get::<_, String>(2)?),
        content: row.get(3)?,
        status: MessageStatus::parse(&row.get::<_, String>(4)?),
        provider_id: row.get(5)?,
        model: row.get(6)?,
        tokens: row.get(7)?,
        created_at_ms: row.get(8)?,
    })
}

/// Appends a message and updates the conversation's `updated_at` (and its
/// title, when it is still the default).
pub fn insert_message(
    conn: &mut Connection,
    conversation_id: Uuid,
    message: &NewMessage,
    now: i64,
    default_title: &str,
) -> Result<StoredMessage> {
    let tokens = estimate_tokens(&message.content);
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO messages (uid, conversation_id, role, content, status, provider_id, model, tokens, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            message.id.to_string(),
            conversation_id.to_string(),
            message.role.as_str(),
            message.content,
            message.status.as_str(),
            message.provider_id,
            message.model,
            tokens,
            now
        ],
    )?;
    let seq = tx.last_insert_rowid();
    if message.role == MessageRole::User {
        tx.execute(
            "UPDATE conversations SET title = ?2 WHERE id = ?1 AND title = ?3",
            params![
                conversation_id.to_string(),
                crate::engine::title_from(&message.content),
                default_title
            ],
        )?;
    }
    tx.execute(
        "UPDATE conversations SET updated_at = ?2 WHERE id = ?1",
        params![conversation_id.to_string(), now],
    )?;
    tx.commit()?;
    Ok(StoredMessage {
        id: message.id,
        seq,
        role: message.role,
        content: message.content.clone(),
        status: message.status,
        provider_id: message.provider_id.clone(),
        model: message.model.clone(),
        tokens,
        created_at_ms: now,
    })
}

pub fn message_exists(conn: &Connection, id: Uuid) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM messages WHERE uid = ?1",
            [id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Inserts a message as is (import); returns its sequence number.
pub fn insert_message_raw(
    conn: &Connection,
    conversation_id: Uuid,
    message: &StoredMessage,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO messages (uid, conversation_id, role, content, status, provider_id, model, tokens, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            message.id.to_string(),
            conversation_id.to_string(),
            message.role.as_str(),
            message.content,
            message.status.as_str(),
            message.provider_id,
            message.model,
            estimate_tokens(&message.content),
            message.created_at_ms
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_messages(conn: &Connection, conversation_id: Uuid) -> Result<Vec<StoredMessage>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages WHERE conversation_id = ?1 ORDER BY id"
    ))?;
    let rows = statement
        .query_map([conversation_id.to_string()], message_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_summary(
    conn: &Connection,
    conversation_id: Uuid,
) -> Result<Option<ConversationSummary>> {
    Ok(conn
        .query_row(
            "SELECT s.content, s.covers_until, s.model, s.tokens, s.updated_at,
                    (SELECT count(*) FROM messages m
                     WHERE m.conversation_id = s.conversation_id AND m.id <= s.covers_until)
             FROM summaries s WHERE s.conversation_id = ?1",
            [conversation_id.to_string()],
            |row| {
                Ok(ConversationSummary {
                    content: row.get(0)?,
                    covers_until: row.get(1)?,
                    model: row.get(2)?,
                    tokens: row.get(3)?,
                    updated_at_ms: row.get(4)?,
                    covered_messages: row.get(5)?,
                })
            },
        )
        .optional()?)
}

pub fn put_summary(
    conn: &Connection,
    conversation_id: Uuid,
    content: &str,
    covers_until: i64,
    model: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO summaries (conversation_id, content, covers_until, model, tokens, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (conversation_id) DO UPDATE SET
             content = excluded.content, covers_until = excluded.covers_until,
             model = excluded.model, tokens = excluded.tokens, updated_at = excluded.updated_at",
        params![
            conversation_id.to_string(),
            content,
            covers_until,
            model,
            estimate_tokens(content),
            now
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

const PROJECT_COLUMNS: &str =
    "p.id, p.name, p.description, p.instructions, p.created_at, p.updated_at,
     (SELECT count(*) FROM facts f WHERE f.project_id = p.id),
     (SELECT count(*) FROM conversations c WHERE c.project_id = p.id)";

fn project_row(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: uuid_at(row, 0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        instructions: row.get(3)?,
        created_at_ms: row.get(4)?,
        updated_at_ms: row.get(5)?,
        fact_count: row.get(6)?,
        conversation_count: row.get(7)?,
    })
}

pub fn get_project(conn: &Connection, id: Uuid) -> Result<Option<Project>> {
    Ok(conn
        .query_row(
            &format!("SELECT {PROJECT_COLUMNS} FROM projects p WHERE p.id = ?1"),
            [id.to_string()],
            project_row,
        )
        .optional()?)
}

pub fn list_projects(conn: &Connection) -> Result<Vec<Project>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {PROJECT_COLUMNS} FROM projects p ORDER BY p.name COLLATE NOCASE"
    ))?;
    let rows = statement
        .query_map([], project_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn upsert_project(conn: &Connection, project: &Project) -> Result<()> {
    conn.execute(
        "INSERT INTO projects (id, name, description, instructions, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (id) DO UPDATE SET
             name = excluded.name, description = excluded.description,
             instructions = excluded.instructions, updated_at = excluded.updated_at",
        params![
            project.id.to_string(),
            project.name,
            project.description,
            project.instructions,
            project.created_at_ms,
            project.updated_at_ms
        ],
    )?;
    Ok(())
}

pub fn delete_project(conn: &Connection, id: Uuid) -> Result<bool> {
    Ok(conn.execute("DELETE FROM projects WHERE id = ?1", [id.to_string()])? > 0)
}

// ---------------------------------------------------------------------------
// Facts
// ---------------------------------------------------------------------------

const FACT_COLUMNS: &str = "f.id, f.uid, f.content, f.project_id, f.pinned, f.source,
     f.source_conversation_id, f.created_at, f.updated_at";

/// A fact with its row id (used by the full-text and vector indexes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactRow {
    pub row_id: i64,
    pub fact: Fact,
}

fn fact_row(row: &Row<'_>) -> rusqlite::Result<FactRow> {
    Ok(FactRow {
        row_id: row.get(0)?,
        fact: Fact {
            id: uuid_at(row, 1)?,
            content: row.get(2)?,
            project_id: opt_uuid_at(row, 3)?,
            pinned: row.get(4)?,
            source: FactSource::parse(&row.get::<_, String>(5)?),
            source_conversation_id: opt_uuid_at(row, 6)?,
            created_at_ms: row.get(7)?,
            updated_at_ms: row.get(8)?,
        },
    })
}

/// Which facts apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactScope {
    All,
    /// Global facts plus those of the project (the context of a conversation).
    Context(Option<Uuid>),
}

impl FactScope {
    fn sql(self) -> (&'static str, Option<String>) {
        match self {
            Self::All => ("1 = 1", None),
            Self::Context(project) => (
                "(f.project_id IS NULL OR f.project_id IS ?1)",
                opt_str(project),
            ),
        }
    }
}

pub fn get_fact(conn: &Connection, id: Uuid) -> Result<Option<FactRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {FACT_COLUMNS} FROM facts f WHERE f.uid = ?1"),
            [id.to_string()],
            fact_row,
        )
        .optional()?)
}

pub fn list_facts(conn: &Connection, scope: FactScope, pinned_only: bool) -> Result<Vec<FactRow>> {
    let (condition, project) = scope.sql();
    let pinned = if pinned_only { " AND f.pinned = 1" } else { "" };
    let mut statement = conn.prepare(&format!(
        "SELECT {FACT_COLUMNS} FROM facts f WHERE {condition}{pinned}
         ORDER BY f.pinned DESC, f.updated_at DESC"
    ))?;
    let rows = if condition.contains("?1") {
        statement
            .query_map([project], fact_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        statement
            .query_map([], fact_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(rows)
}

pub fn insert_fact(conn: &Connection, fact: &Fact) -> Result<i64> {
    conn.execute(
        "INSERT INTO facts (uid, content, project_id, pinned, source, source_conversation_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            fact.id.to_string(),
            fact.content,
            opt_str(fact.project_id),
            fact.pinned,
            fact.source.as_str(),
            opt_str(fact.source_conversation_id),
            fact.created_at_ms,
            fact.updated_at_ms
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_fact(conn: &Connection, fact: &Fact) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE facts SET content = ?2, project_id = ?3, pinned = ?4, updated_at = ?5 WHERE uid = ?1",
        params![
            fact.id.to_string(),
            fact.content,
            opt_str(fact.project_id),
            fact.pinned,
            fact.updated_at_ms
        ],
    )?;
    Ok(changed > 0)
}

pub fn delete_fact(conn: &Connection, id: Uuid) -> Result<bool> {
    Ok(conn.execute("DELETE FROM facts WHERE uid = ?1", [id.to_string()])? > 0)
}

pub fn facts_by_row_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<FactRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let mut statement = conn.prepare(&format!(
        "SELECT {FACT_COLUMNS} FROM facts f WHERE f.id IN ({placeholders})"
    ))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(ids), fact_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// A full-text match with its BM25 rank (lower is better) and highlighted
/// excerpt.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked<T> {
    pub item: T,
    pub rank: f64,
    pub snippet: String,
}

pub fn fts_facts(
    conn: &Connection,
    query: &str,
    scope: FactScope,
    limit: u32,
) -> Result<Vec<Ranked<FactRow>>> {
    let (condition, project) = scope.sql();
    // Parameters: ?1 project (if used), ?2 query, ?3 limit.
    let condition = condition.replace("?1", "?3");
    let mut statement = conn.prepare(&format!(
        "SELECT {FACT_COLUMNS}, bm25(facts_fts), snippet(facts_fts, 0, char(2), char(3), '…', 16)
         FROM facts_fts JOIN facts f ON f.id = facts_fts.rowid
         WHERE facts_fts MATCH ?1 AND {condition}
         ORDER BY bm25(facts_fts) LIMIT ?2"
    ))?;
    let map = |row: &Row<'_>| {
        Ok(Ranked {
            item: fact_row(row)?,
            rank: row.get(9)?,
            snippet: row.get(10)?,
        })
    };
    let rows = if condition.contains("?3") {
        statement
            .query_map(params![query, limit, project], map)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        statement
            .query_map(params![query, limit], map)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Message search
// ---------------------------------------------------------------------------

/// A message found by search, with its conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageHit {
    pub row_id: i64,
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub conversation_title: String,
    pub project_id: Option<Uuid>,
    pub role: MessageRole,
    pub content: String,
    pub status: MessageStatus,
    pub created_at_ms: i64,
}

const MESSAGE_HIT_COLUMNS: &str = "m.id, m.uid, m.conversation_id, c.title, c.project_id, m.role,
     m.content, m.status, m.created_at";

fn message_hit_row(row: &Row<'_>) -> rusqlite::Result<MessageHit> {
    Ok(MessageHit {
        row_id: row.get(0)?,
        id: uuid_at(row, 1)?,
        conversation_id: uuid_at(row, 2)?,
        conversation_title: row.get(3)?,
        project_id: opt_uuid_at(row, 4)?,
        role: MessageRole::parse(&row.get::<_, String>(5)?),
        content: row.get(6)?,
        status: MessageStatus::parse(&row.get::<_, String>(7)?),
        created_at_ms: row.get(8)?,
    })
}

pub fn fts_messages(conn: &Connection, query: &str, limit: u32) -> Result<Vec<Ranked<MessageHit>>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {MESSAGE_HIT_COLUMNS}, bm25(messages_fts),
                snippet(messages_fts, 0, char(2), char(3), '…', 16)
         FROM messages_fts
         JOIN messages m ON m.id = messages_fts.rowid
         JOIN conversations c ON c.id = m.conversation_id
         WHERE messages_fts MATCH ?1
         ORDER BY bm25(messages_fts) LIMIT ?2"
    ))?;
    let map = |row: &Row<'_>| {
        Ok(Ranked {
            item: message_hit_row(row)?,
            rank: row.get(9)?,
            snippet: row.get(10)?,
        })
    };
    let rows = statement
        .query_map(params![query, limit], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn messages_by_row_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<MessageHit>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let mut statement = conn.prepare(&format!(
        "SELECT {MESSAGE_HIT_COLUMNS} FROM messages m
         JOIN conversations c ON c.id = m.conversation_id
         WHERE m.id IN ({placeholders})"
    ))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(ids), message_hit_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Episodes
// ---------------------------------------------------------------------------

fn episode_row(row: &Row<'_>) -> rusqlite::Result<Episode> {
    Ok(Episode {
        id: uuid_at(row, 0)?,
        occurred_at_ms: row.get(1)?,
        category: row.get(2)?,
        level: row.get(3)?,
        summary: row.get(4)?,
    })
}

/// Inserts episodes whose id is new; returns how many were added. Callers
/// wrap batches in a transaction.
pub fn insert_episodes(conn: &Connection, episodes: &[Episode]) -> Result<usize> {
    let mut statement = conn.prepare(
        "INSERT OR IGNORE INTO episodes (uid, occurred_at, category, level, summary)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut added = 0;
    for episode in episodes {
        added += statement.execute(params![
            episode.id.to_string(),
            episode.occurred_at_ms,
            episode.category,
            episode.level,
            episode.summary
        ])?;
    }
    Ok(added)
}

pub fn list_episodes(
    conn: &Connection,
    limit: u32,
    before: Option<EpisodeCursor>,
) -> Result<Vec<Episode>> {
    let mut statement = conn.prepare(if before.is_some() {
        "SELECT uid, occurred_at, category, level, summary FROM episodes
         WHERE (occurred_at, uid) < (?1, ?2)
         ORDER BY occurred_at DESC, uid DESC LIMIT ?3"
    } else {
        "SELECT uid, occurred_at, category, level, summary FROM episodes
         ORDER BY occurred_at DESC, uid DESC LIMIT ?1"
    })?;
    let mut rows = match before {
        Some(cursor) => {
            statement.query(params![cursor.occurred_at_ms, cursor.id.to_string(), limit])?
        }
        None => statement.query([limit])?,
    };
    let mut episodes = Vec::new();
    while let Some(row) = rows.next()? {
        episodes.push(episode_row(row)?);
    }
    Ok(episodes)
}

pub fn fts_episodes(conn: &Connection, query: &str, limit: u32) -> Result<Vec<Ranked<Episode>>> {
    let mut statement = conn.prepare(
        "SELECT e.uid, e.occurred_at, e.category, e.level, e.summary, bm25(episodes_fts),
                snippet(episodes_fts, 0, char(2), char(3), '…', 16)
         FROM episodes_fts JOIN episodes e ON e.id = episodes_fts.rowid
         WHERE episodes_fts MATCH ?1 ORDER BY bm25(episodes_fts) LIMIT ?2",
    )?;
    let map = |row: &Row<'_>| {
        Ok(Ranked {
            item: episode_row(row)?,
            rank: row.get(5)?,
            snippet: row.get(6)?,
        })
    };
    let rows = statement
        .query_map(params![query, limit], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Deletes episodes older than `before_ms` (all with `None`). Returns the count.
pub fn delete_episodes(conn: &Connection, before_ms: Option<i64>) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM episodes WHERE occurred_at < ?1",
        [before_ms.unwrap_or(i64::MAX)],
    )?)
}

// ---------------------------------------------------------------------------
// Embeddings
// ---------------------------------------------------------------------------

/// Facts and non-empty messages without a vector for `model_key`, facts
/// first, newest first.
pub fn pending_embeddings(
    conn: &Connection,
    model_key: &str,
    limit: u32,
) -> Result<Vec<(VectorKind, i64, String)>> {
    let mut statement = conn.prepare(
        "SELECT kind, id, content FROM (
             SELECT 'fact' AS kind, f.id AS id, f.content AS content FROM facts f
             WHERE NOT EXISTS (SELECT 1 FROM embeddings e
                               WHERE e.item_kind = 'fact' AND e.item_id = f.id AND e.model_key = ?1)
             UNION ALL
             SELECT 'message', m.id, m.content FROM messages m
             WHERE length(m.content) > 0 AND NOT EXISTS (SELECT 1 FROM embeddings e
                               WHERE e.item_kind = 'message' AND e.item_id = m.id AND e.model_key = ?1)
         ) ORDER BY kind, id DESC LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![model_key, limit], |row| {
            let kind: String = row.get(0)?;
            Ok((kind, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(kind, id, text)| VectorKind::parse(&kind).map(|k| (k, id, text)))
        .collect())
}

pub fn put_embedding(
    conn: &Connection,
    kind: VectorKind,
    id: i64,
    model_key: &str,
    vector: &[u8],
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO embeddings (item_kind, item_id, model_key, vector) VALUES (?1, ?2, ?3, ?4)",
        params![kind.as_str(), id, model_key, vector],
    )?;
    Ok(())
}

pub fn load_vectors(conn: &Connection, model_key: &str) -> Result<Vec<(VectorKind, i64, Vec<u8>)>> {
    let mut statement =
        conn.prepare("SELECT item_kind, item_id, vector FROM embeddings WHERE model_key = ?1")?;
    let rows = statement
        .query_map([model_key], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(kind, id, blob)| VectorKind::parse(&kind).map(|k| (k, id, blob)))
        .collect())
}

/// `(indexed, pending)` item counts for `model_key`.
pub fn embedding_counts(conn: &Connection, model_key: &str) -> Result<(u32, u32)> {
    Ok(conn.query_row(
        "SELECT (SELECT count(*) FROM embeddings WHERE model_key = ?1),
                (SELECT count(*) FROM facts f WHERE NOT EXISTS (SELECT 1 FROM embeddings e
                     WHERE e.item_kind = 'fact' AND e.item_id = f.id AND e.model_key = ?1))
              + (SELECT count(*) FROM messages m WHERE length(m.content) > 0 AND NOT EXISTS (
                     SELECT 1 FROM embeddings e
                     WHERE e.item_kind = 'message' AND e.item_id = m.id AND e.model_key = ?1))",
        [model_key],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

/// Drops vectors of other embedding models (after the model was changed).
pub fn delete_other_embeddings(conn: &Connection, model_key: &str) -> Result<usize> {
    Ok(conn.execute("DELETE FROM embeddings WHERE model_key <> ?1", [model_key])?)
}

// ---------------------------------------------------------------------------
// Whole database
// ---------------------------------------------------------------------------

pub fn counts(conn: &Connection) -> Result<MemoryCounts> {
    Ok(conn.query_row(
        "SELECT (SELECT count(*) FROM conversations), (SELECT count(*) FROM messages),
                (SELECT count(*) FROM facts), (SELECT count(*) FROM projects),
                (SELECT count(*) FROM episodes)",
        [],
        |row| {
            Ok(MemoryCounts {
                conversations: row.get(0)?,
                messages: row.get(1)?,
                facts: row.get(2)?,
                projects: row.get(3)?,
                episodes: row.get(4)?,
            })
        },
    )?)
}

/// Deletes all memory. Full-text indexes are rebuilt empty.
pub fn clear_all(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(
        "DELETE FROM embeddings;
         DELETE FROM summaries;
         DELETE FROM messages;
         DELETE FROM conversations;
         DELETE FROM facts;
         DELETE FROM projects;
         DELETE FROM episodes;
         INSERT INTO messages_fts (messages_fts) VALUES ('rebuild');
         INSERT INTO facts_fts (facts_fts) VALUES ('rebuild');
         INSERT INTO episodes_fts (episodes_fts) VALUES ('rebuild');",
    )?;
    tx.commit()?;
    Ok(())
}
