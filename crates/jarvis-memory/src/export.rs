//! Export and import of saved memory as a versioned JSON document.
//!
//! Embeddings are not exported: they depend on the embedding model and are
//! recomputed after an import. Importing never overwrites: items whose id
//! already exists are skipped, and everything is applied in one transaction.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::engine::MemoryEngine;
use crate::error::{MemoryError, Result};
use crate::store::{self, ConversationFilter, FactScope};
use crate::types::{
    Conversation, Episode, Fact, FactSource, MAX_FACT_CHARS, MAX_FACTS, MAX_MESSAGE_CHARS,
    MAX_NOTE_CHARS, MAX_PROJECT_DESCRIPTION_CHARS, MAX_PROJECT_INSTRUCTIONS_CHARS,
    MAX_PROJECT_NAME_CHARS, MAX_PROJECTS, MAX_TITLE_CHARS, MemoryCounts, Project, StoredMessage,
};

pub const EXPORT_FORMAT: &str = "jarvis-memory";
pub const EXPORT_VERSION: u32 = 1;
const MAX_IMPORT_CONVERSATIONS: usize = 100_000;
const MAX_IMPORT_MESSAGES: usize = 1_000_000;
const MAX_IMPORT_EPISODES: usize = 1_000_000;
const MAX_SUMMARY_CHARS: usize = 3_000;
const MAX_EPISODE_CHARS: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryExport {
    pub format: String,
    pub version: u32,
    pub exported_at_ms: i64,
    pub projects: Vec<Project>,
    pub facts: Vec<Fact>,
    pub conversations: Vec<ExportedConversation>,
    pub episodes: Vec<Episode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedConversation {
    pub conversation: Conversation,
    /// Oldest first.
    pub messages: Vec<StoredMessage>,
    pub summary: Option<ExportedSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedSummary {
    pub content: String,
    /// Last message the summary covers.
    pub covers_until_message: Option<Uuid>,
    pub model: Option<String>,
    pub updated_at_ms: i64,
}

/// Counts of an import.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportOutcome {
    pub imported: MemoryCounts,
    pub skipped: u32,
}

impl MemoryEngine {
    /// A consistent snapshot of all saved memory.
    pub async fn export(&self) -> Result<MemoryExport> {
        let db = self.db_handle()?;
        db.call(|conn| {
            let tx = conn.transaction()?;
            let projects = store::list_projects(&tx)?;
            let facts = store::list_facts(&tx, FactScope::All, false)?
                .into_iter()
                .map(|row| row.fact)
                .collect();
            let filter = ConversationFilter {
                limit: u32::MAX,
                ..ConversationFilter::default()
            };
            let mut conversations = Vec::new();
            for conversation in store::list_conversations(&tx, &filter)? {
                let messages = store::list_messages(&tx, conversation.id)?;
                let summary = store::get_summary(&tx, conversation.id)?.map(|s| ExportedSummary {
                    covers_until_message: messages
                        .iter()
                        .rev()
                        .find(|m| m.seq <= s.covers_until)
                        .map(|m| m.id),
                    content: s.content,
                    model: s.model,
                    updated_at_ms: s.updated_at_ms,
                });
                conversations.push(ExportedConversation {
                    conversation,
                    messages,
                    summary,
                });
            }
            let episodes = store::list_episodes(&tx, u32::MAX, None)?;
            tx.commit()?;
            Ok(MemoryExport {
                format: EXPORT_FORMAT.to_owned(),
                version: EXPORT_VERSION,
                exported_at_ms: crate::engine::now_ms(),
                projects,
                facts,
                conversations,
                episodes,
            })
        })
        .await
    }

    /// Adds the content of an export. Items that already exist are skipped.
    pub async fn import(&self, data: MemoryExport) -> Result<ImportOutcome> {
        validate(&data)?;
        let db = self.db_handle()?;
        let outcome = db
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut outcome = ImportOutcome::default();

                for project in &data.projects {
                    if store::get_project(&tx, project.id)?.is_some() {
                        outcome.skipped += 1;
                        continue;
                    }
                    store::upsert_project(&tx, project)?;
                    outcome.imported.projects += 1;
                }
                let project_exists = |tx: &jarvis_storage::rusqlite::Connection, id: Option<Uuid>| {
                    Ok::<_, MemoryError>(match id {
                        Some(id) if store::get_project(tx, id)?.is_some() => Some(id),
                        _ => None,
                    })
                };

                for exported in &data.conversations {
                    let source = &exported.conversation;
                    if store::get_conversation(&tx, source.id)?.is_some() {
                        outcome.skipped += 1;
                        continue;
                    }
                    let conversation = Conversation {
                        project_id: project_exists(&tx, source.project_id)?,
                        temporary: false,
                        ..source.clone()
                    };
                    store::insert_conversation(&tx, &conversation)?;
                    outcome.imported.conversations += 1;
                    let mut covers_until = None;
                    for message in &exported.messages {
                        if store::message_exists(&tx, message.id)? {
                            outcome.skipped += 1;
                            continue;
                        }
                        let seq = store::insert_message_raw(&tx, conversation.id, message)?;
                        outcome.imported.messages += 1;
                        if let Some(summary) = &exported.summary
                            && summary.covers_until_message == Some(message.id)
                        {
                            covers_until = Some(seq);
                        }
                    }
                    if let (Some(summary), Some(covers_until)) = (&exported.summary, covers_until) {
                        store::put_summary(
                            &tx,
                            conversation.id,
                            &summary.content,
                            covers_until,
                            summary.model.as_deref(),
                            summary.updated_at_ms,
                        )?;
                    }
                }

                for fact in &data.facts {
                    if store::get_fact(&tx, fact.id)?.is_some() {
                        outcome.skipped += 1;
                        continue;
                    }
                    let source_conversation = match fact.source_conversation_id {
                        Some(id) if store::get_conversation(&tx, id)?.is_some() => Some(id),
                        _ => None,
                    };
                    store::insert_fact(
                        &tx,
                        &Fact {
                            content: fact.content.trim().to_owned(),
                            project_id: project_exists(&tx, fact.project_id)?,
                            source: FactSource::Import,
                            source_conversation_id: source_conversation,
                            ..fact.clone()
                        },
                    )?;
                    outcome.imported.facts += 1;
                }

                let added = store::insert_episodes(&tx, &data.episodes)?;
                outcome.imported.episodes = added as u32;
                outcome.skipped += (data.episodes.len() - added) as u32;

                let totals = store::counts(&tx)?;
                if totals.facts > MAX_FACTS || totals.projects > MAX_PROJECTS {
                    return Err(MemoryError::Invalid(format!(
                        "the import would exceed the limit of {MAX_FACTS} facts or {MAX_PROJECTS} projects"
                    )));
                }
                tx.commit()?;
                Ok(outcome)
            })
            .await?;
        self.invalidate_after_import().await;
        Ok(outcome)
    }
}

fn validate(data: &MemoryExport) -> Result<()> {
    let invalid = |message: String| Err(MemoryError::Invalid(message));
    if data.format != EXPORT_FORMAT {
        return invalid("the file is not a JARVIS memory export".to_owned());
    }
    if data.version != EXPORT_VERSION {
        return invalid(format!(
            "unsupported memory export version {} (this build reads version {EXPORT_VERSION})",
            data.version
        ));
    }
    let messages: usize = data.conversations.iter().map(|c| c.messages.len()).sum();
    if data.projects.len() > MAX_PROJECTS as usize
        || data.facts.len() > MAX_FACTS as usize
        || data.conversations.len() > MAX_IMPORT_CONVERSATIONS
        || messages > MAX_IMPORT_MESSAGES
        || data.episodes.len() > MAX_IMPORT_EPISODES
    {
        return invalid("the export is larger than JARVIS accepts".to_owned());
    }
    let too_long = |value: &str, max: usize| value.chars().count() > max;
    for project in &data.projects {
        if project.name.trim().is_empty()
            || too_long(&project.name, MAX_PROJECT_NAME_CHARS)
            || too_long(&project.description, MAX_PROJECT_DESCRIPTION_CHARS)
            || too_long(&project.instructions, MAX_PROJECT_INSTRUCTIONS_CHARS)
        {
            return invalid(format!("project \"{}\" is invalid", project.id));
        }
    }
    for fact in &data.facts {
        if fact.content.trim().is_empty() || too_long(&fact.content, MAX_FACT_CHARS) {
            return invalid(format!("fact \"{}\" is invalid", fact.id));
        }
    }
    for exported in &data.conversations {
        let conversation = &exported.conversation;
        if conversation.title.trim().is_empty()
            || too_long(&conversation.title, MAX_TITLE_CHARS)
            || too_long(&conversation.pinned_note, MAX_NOTE_CHARS)
            || exported
                .messages
                .iter()
                .any(|m| too_long(&m.content, MAX_MESSAGE_CHARS))
            || exported
                .summary
                .as_ref()
                .is_some_and(|s| too_long(&s.content, MAX_SUMMARY_CHARS))
        {
            return invalid(format!("conversation \"{}\" is invalid", conversation.id));
        }
    }
    if data
        .episodes
        .iter()
        .any(|e| too_long(&e.summary, MAX_EPISODE_CHARS) || too_long(&e.category, 64))
    {
        return invalid("an episode is invalid".to_owned());
    }
    Ok(())
}
