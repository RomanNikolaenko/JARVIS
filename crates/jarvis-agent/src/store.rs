//! Tasks in the database: the whole task (plan and journal as JSON) is
//! saved after every step, so a task survives a restart.

use jarvis_storage::rusqlite::{self, OptionalExtension, Row, params};
use jarvis_storage::{Database, StorageError};
use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::types::{AgentTask, TaskStatus, TaskSummary};

const COLUMNS: &str = "id, goal, status, provider_id, model, limits, plan, steps, result, error,
     created_at, updated_at, finished_at";

#[derive(Debug, Clone)]
pub struct TaskStore {
    db: Database,
}

fn row_to_task(row: &Row<'_>) -> rusqlite::Result<AgentTask> {
    let json_error = |index: usize, err: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    };
    let id: String = row.get(0)?;
    Ok(AgentTask {
        id: Uuid::parse_str(&id).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })?,
        goal: row.get(1)?,
        status: TaskStatus::parse(&row.get::<_, String>(2)?),
        provider_id: row.get(3)?,
        model: row.get(4)?,
        limits: serde_json::from_str(&row.get::<_, String>(5)?).map_err(|e| json_error(5, e))?,
        plan: serde_json::from_str(&row.get::<_, String>(6)?).map_err(|e| json_error(6, e))?,
        steps: serde_json::from_str(&row.get::<_, String>(7)?).map_err(|e| json_error(7, e))?,
        result: row.get(8)?,
        error: row.get(9)?,
        created_at_ms: row.get(10)?,
        updated_at_ms: row.get(11)?,
        finished_at_ms: row.get(12)?,
    })
}

impl TaskStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Inserts or replaces the task.
    pub async fn save(&self, task: &AgentTask) -> Result<()> {
        let limits = serde_json::to_string(&task.limits).map_err(AgentError::internal)?;
        let plan = serde_json::to_string(&task.plan).map_err(AgentError::internal)?;
        let steps = serde_json::to_string(&task.steps).map_err(AgentError::internal)?;
        let task = task.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    &format!(
                        "INSERT OR REPLACE INTO agent_tasks ({COLUMNS})
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"
                    ),
                    params![
                        task.id.to_string(),
                        task.goal,
                        task.status.as_str(),
                        task.provider_id,
                        task.model,
                        limits,
                        plan,
                        steps,
                        task.result,
                        task.error,
                        task.created_at_ms,
                        task.updated_at_ms,
                        task.finished_at_ms
                    ],
                )?;
                Ok::<_, StorageError>(())
            })
            .await?;
        Ok(())
    }

    pub async fn get(&self, id: Uuid) -> Result<AgentTask> {
        self.db
            .call(move |conn| {
                Ok::<_, StorageError>(
                    conn.query_row(
                        &format!("SELECT {COLUMNS} FROM agent_tasks WHERE id = ?1"),
                        [id.to_string()],
                        row_to_task,
                    )
                    .optional()?,
                )
            })
            .await?
            .ok_or(AgentError::NotFound)
    }

    /// Newest first.
    pub async fn list(&self, limit: u32) -> Result<Vec<TaskSummary>> {
        let tasks = self
            .db
            .call(move |conn| {
                let mut statement = conn.prepare(&format!(
                    "SELECT {COLUMNS} FROM agent_tasks ORDER BY updated_at DESC LIMIT ?1"
                ))?;
                let rows = statement
                    .query_map([limit.clamp(1, 500)], row_to_task)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, StorageError>(rows)
            })
            .await?;
        Ok(tasks.iter().map(TaskSummary::from).collect())
    }

    /// Tasks that were running when JARVIS stopped; they become paused.
    pub async fn unfinished(&self) -> Result<Vec<AgentTask>> {
        Ok(self
            .db
            .call(|conn| {
                let mut statement = conn.prepare(&format!(
                    "SELECT {COLUMNS} FROM agent_tasks
                     WHERE status IN ('pending', 'running', 'waitingForApproval')"
                ))?;
                let rows = statement
                    .query_map([], row_to_task)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, StorageError>(rows)
            })
            .await?)
    }

    /// Deletes finished tasks; returns how many.
    pub async fn delete_finished(&self) -> Result<usize> {
        Ok(self
            .db
            .call(|conn| {
                Ok::<_, StorageError>(conn.execute(
                    "DELETE FROM agent_tasks WHERE status IN ('completed', 'failed', 'cancelled')",
                    [],
                )?)
            })
            .await?)
    }
}
