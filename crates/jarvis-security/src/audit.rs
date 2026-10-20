//! Audit log of tool calls: what was requested, the decision and the
//! outcome. Entries form a hash chain (each hashes the previous one), so
//! changing or removing an entry is detected by [`AuditLog::verify`]. This
//! is tamper-evident, not tamper-proof: someone who can rewrite the whole
//! file can rebuild the chain.

use jarvis_storage::rusqlite::{self, OptionalExtension, params};
use jarvis_storage::{Database, StorageError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;
use uuid::Uuid;

use crate::types::RiskLevel;

const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const MAX_DETAIL_CHARS: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "security.ts")]
pub enum AuditEvent {
    /// The policy allowed the call without asking.
    Allowed,
    /// The user approved the call.
    Approved,
    /// The user denied the call.
    Denied,
    /// The policy refused the call.
    PolicyDenied,
    /// Nobody answered the approval request in time.
    TimedOut,
    /// The task was cancelled while waiting.
    Cancelled,
    /// The tool ran and succeeded.
    Succeeded,
    /// The tool ran and failed.
    Failed,
}

impl AuditEvent {
    fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::PolicyDenied => "policyDenied",
            Self::TimedOut => "timedOut",
            Self::Cancelled => "cancelled",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "allowed" => Self::Allowed,
            "approved" => Self::Approved,
            "denied" => Self::Denied,
            "policyDenied" => Self::PolicyDenied,
            "timedOut" => Self::TimedOut,
            "cancelled" => Self::Cancelled,
            "succeeded" => Self::Succeeded,
            _ => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "security.ts")]
pub struct AuditEntry {
    pub id: Uuid,
    pub occurred_at_ms: i64,
    pub task_id: Option<Uuid>,
    pub tool: String,
    /// What the call does, in one line.
    pub action: String,
    pub risk: RiskLevel,
    pub event: AuditEvent,
    /// Input, reason or error (truncated).
    pub detail: String,
}

/// Result of checking the hash chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "security.ts")]
pub struct AuditVerification {
    pub entries: u32,
    pub intact: bool,
    /// First entry whose hash does not match.
    pub broken_at: Option<Uuid>,
}

/// Fields of a new entry.
#[derive(Debug, Clone)]
pub struct NewAuditEntry {
    pub task_id: Option<Uuid>,
    pub tool: String,
    pub action: String,
    pub risk: RiskLevel,
    pub event: AuditEvent,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct AuditLog {
    db: Database,
}

fn risk_str(risk: RiskLevel) -> &'static str {
    risk.as_str()
}

fn parse_risk(value: &str) -> RiskLevel {
    match value {
        "low" => RiskLevel::Low,
        "medium" => RiskLevel::Medium,
        _ => RiskLevel::High,
    }
}

fn entry_hash(prev: &str, entry: &AuditEntry) -> String {
    // A JSON array gives an unambiguous encoding of the fields.
    let fields = serde_json::json!([
        prev,
        entry.id.to_string(),
        entry.occurred_at_ms,
        entry.task_id.map(|id| id.to_string()),
        entry.tool,
        entry.action,
        risk_str(entry.risk),
        entry.event.as_str(),
        entry.detail,
    ]);
    Sha256::digest(fields.to_string().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_DETAIL_CHARS {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(MAX_DETAIL_CHARS).collect();
        format!("{cut}…")
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<(AuditEntry, String, String)> {
    let parse_uuid = |text: String| {
        Uuid::parse_str(&text).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })
    };
    Ok((
        AuditEntry {
            id: parse_uuid(row.get(0)?)?,
            occurred_at_ms: row.get(1)?,
            task_id: row
                .get::<_, Option<String>>(2)?
                .map(parse_uuid)
                .transpose()?,
            tool: row.get(3)?,
            action: row.get(4)?,
            risk: parse_risk(&row.get::<_, String>(5)?),
            event: AuditEvent::parse(&row.get::<_, String>(6)?),
            detail: row.get(7)?,
        },
        row.get(8)?,
        row.get(9)?,
    ))
}

const COLUMNS: &str =
    "uid, occurred_at, task_id, tool, action, risk, event, detail, prev_hash, hash";

impl AuditLog {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Appends an entry to the chain.
    pub async fn record(&self, new: NewAuditEntry) -> Result<AuditEntry, StorageError> {
        let entry = AuditEntry {
            id: Uuid::new_v4(),
            occurred_at_ms: now_ms(),
            task_id: new.task_id,
            tool: new.tool,
            action: truncate(&new.action),
            risk: new.risk,
            event: new.event,
            detail: truncate(&new.detail),
        };
        let stored = entry.clone();
        self.db
            .call(move |conn| {
                let tx = conn.transaction()?;
                let prev: String = tx
                    .query_row(
                        "SELECT hash FROM audit_log ORDER BY id DESC LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .optional()?
                    .unwrap_or_else(|| GENESIS.to_owned());
                let hash = entry_hash(&prev, &stored);
                tx.execute(
                    &format!("INSERT INTO audit_log ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"),
                    params![
                        stored.id.to_string(),
                        stored.occurred_at_ms,
                        stored.task_id.map(|id| id.to_string()),
                        stored.tool,
                        stored.action,
                        risk_str(stored.risk),
                        stored.event.as_str(),
                        stored.detail,
                        prev,
                        hash
                    ],
                )?;
                tx.commit()?;
                Ok::<_, StorageError>(())
            })
            .await?;
        Ok(entry)
    }

    /// Newest first.
    pub async fn list(
        &self,
        limit: u32,
        before_ms: Option<i64>,
    ) -> Result<Vec<AuditEntry>, StorageError> {
        self.db
            .call(move |conn| {
                let mut statement = conn.prepare(&format!(
                    "SELECT {COLUMNS} FROM audit_log WHERE occurred_at < ?1
                     ORDER BY id DESC LIMIT ?2"
                ))?;
                let rows = statement
                    .query_map(
                        params![before_ms.unwrap_or(i64::MAX), limit.clamp(1, 1000)],
                        row_to_entry,
                    )?
                    .map(|row| row.map(|(entry, _, _)| entry))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, StorageError>(rows)
            })
            .await
    }

    /// Recomputes the chain from the first entry.
    pub async fn verify(&self) -> Result<AuditVerification, StorageError> {
        self.db
            .call(|conn| {
                let mut statement =
                    conn.prepare(&format!("SELECT {COLUMNS} FROM audit_log ORDER BY id"))?;
                let mut rows = statement.query([])?;
                let mut expected_prev = GENESIS.to_owned();
                let mut entries = 0u32;
                while let Some(row) = rows.next()? {
                    let (entry, prev, hash) = row_to_entry(row)?;
                    entries += 1;
                    if prev != expected_prev || entry_hash(&prev, &entry) != hash {
                        return Ok(AuditVerification {
                            entries,
                            intact: false,
                            broken_at: Some(entry.id),
                        });
                    }
                    expected_prev = hash;
                }
                Ok::<_, StorageError>(AuditVerification {
                    entries,
                    intact: true,
                    broken_at: None,
                })
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(event: AuditEvent, detail: &str) -> NewAuditEntry {
        NewAuditEntry {
            task_id: Some(Uuid::new_v4()),
            tool: "run_command".into(),
            action: "Run git status".into(),
            risk: RiskLevel::High,
            event,
            detail: detail.into(),
        }
    }

    #[tokio::test]
    async fn entries_form_a_verifiable_chain() {
        let db = Database::open_in_memory().expect("db");
        let log = AuditLog::new(db.clone());
        log.record(entry(AuditEvent::Approved, "{\"program\":\"git\"}"))
            .await
            .expect("record");
        let second = log
            .record(entry(AuditEvent::Succeeded, "exit 0"))
            .await
            .expect("record");
        log.record(entry(AuditEvent::Denied, &"x".repeat(5000)))
            .await
            .expect("record");

        let listed = log.list(10, None).await.expect("list");
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].event, AuditEvent::Denied, "newest first");
        assert!(listed[0].detail.chars().count() <= MAX_DETAIL_CHARS + 1);
        assert_eq!(
            log.verify().await.expect("verify"),
            AuditVerification {
                entries: 3,
                intact: true,
                broken_at: None
            }
        );

        // Rewriting an entry breaks the chain at that entry.
        let id = second.id.to_string();
        db.call(move |conn| {
            conn.execute(
                "UPDATE audit_log SET detail = 'exit 1' WHERE uid = ?1",
                [id],
            )?;
            Ok::<_, StorageError>(())
        })
        .await
        .expect("tamper");
        let result = log.verify().await.expect("verify");
        assert!(!result.intact);
        assert_eq!(result.broken_at, Some(second.id));
    }

    #[tokio::test]
    async fn deleting_an_entry_is_detected() {
        let db = Database::open_in_memory().expect("db");
        let log = AuditLog::new(db.clone());
        let first = log
            .record(entry(AuditEvent::Allowed, "a"))
            .await
            .expect("record");
        log.record(entry(AuditEvent::Succeeded, "b"))
            .await
            .expect("record");
        let id = first.id.to_string();
        db.call(move |conn| {
            conn.execute("DELETE FROM audit_log WHERE uid = ?1", [id])?;
            Ok::<_, StorageError>(())
        })
        .await
        .expect("delete");
        assert!(!log.verify().await.expect("verify").intact);
    }
}
