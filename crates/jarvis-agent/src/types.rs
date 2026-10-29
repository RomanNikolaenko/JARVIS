//! Agent task types shared with the UI (exported to TypeScript).

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

pub const DEFAULT_MAX_STEPS: u32 = 12;
pub const MAX_STEPS: u32 = 50;
pub const DEFAULT_MAX_DURATION_SECS: u32 = 600;
pub const MAX_DURATION_SECS: u32 = 3600;
pub const MAX_GOAL_CHARS: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "agents.ts")]
pub enum TaskStatus {
    Pending,
    Running,
    WaitingForApproval,
    Paused,
    Completed,
    /// An answer is available, but verification was unavailable or invalid.
    Unverified,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Unverified | Self::Failed | Self::Cancelled
        )
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::WaitingForApproval => "waitingForApproval",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Unverified => "unverified",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn parse(value: &str) -> Self {
        match value {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "waitingForApproval" => Self::WaitingForApproval,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "unverified" => Self::Unverified,
            "cancelled" => Self::Cancelled,
            _ => Self::Failed,
        }
    }
}

/// Bounds of one task. There is no unbounded agent loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "agents.ts")]
pub struct TaskLimits {
    /// Tool calls and answers the agent may make.
    pub max_steps: u32,
    /// Wall-clock time per run (waiting for approval included).
    pub max_duration_secs: u32,
}

impl Default for TaskLimits {
    fn default() -> Self {
        Self {
            max_steps: DEFAULT_MAX_STEPS,
            max_duration_secs: DEFAULT_MAX_DURATION_SECS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "agents.ts")]
pub enum StepKind {
    /// The planner's plan.
    Plan,
    /// A tool call.
    Action,
    /// The agent's answer.
    Answer,
    /// The verifier's check of the answer.
    Verification,
    /// Something the orchestrator noted (recovery, pause, limits).
    Note,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "agents.ts")]
pub enum StepOutcome {
    /// Durable intent saved before the tool starts; task ID + index identifies the operation.
    InProgress,
    /// Execution may have happened, but no durable result exists. Never replay automatically.
    Unknown,
    /// The verifier did not return a usable verdict.
    Unverified,
    Succeeded,
    Failed,
    /// Refused by the policy or the user.
    Denied,
    Info,
}

/// One entry of the task journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "agents.ts")]
pub struct TaskStep {
    pub index: u32,
    pub kind: StepKind,
    /// The agent's reasoning for the step, as it wrote it.
    pub thought: Option<String>,
    pub tool: Option<String>,
    /// Tool input (JSON).
    pub input: Option<String>,
    /// What the action does, in one line.
    pub summary: Option<String>,
    /// How the call was authorized, e.g. "approved by you".
    pub decision: Option<String>,
    pub outcome: StepOutcome,
    /// Result, answer or error (truncated).
    pub output: Option<String>,
    pub started_at_ms: i64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "agents.ts")]
pub struct AgentTask {
    pub id: Uuid,
    pub goal: String,
    pub status: TaskStatus,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub limits: TaskLimits,
    pub plan: Vec<String>,
    pub steps: Vec<TaskStep>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub finished_at_ms: Option<i64>,
}

/// A task without its journal, for lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "agents.ts")]
pub struct TaskSummary {
    pub id: Uuid,
    pub goal: String,
    pub status: TaskStatus,
    pub model: Option<String>,
    pub step_count: u32,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl From<&AgentTask> for TaskSummary {
    fn from(task: &AgentTask) -> Self {
        Self {
            id: task.id,
            goal: task.goal.clone(),
            status: task.status,
            model: task.model.clone(),
            step_count: task.steps.len() as u32,
            error: task.error.clone(),
            created_at_ms: task.created_at_ms,
            updated_at_ms: task.updated_at_ms,
        }
    }
}
