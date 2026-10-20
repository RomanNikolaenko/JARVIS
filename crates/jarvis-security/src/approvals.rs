//! Approvals: a tool call that needs the user's consent waits here until the
//! user approves or denies it, the request times out, or its task is
//! cancelled. Nothing is approved implicitly.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;
use uuid::Uuid;

use crate::types::RiskLevel;

/// A call waiting for the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "security.ts")]
pub struct PendingApproval {
    pub id: Uuid,
    pub task_id: Option<Uuid>,
    pub tool: String,
    pub risk: RiskLevel,
    /// What will happen, in one line.
    pub summary: String,
    /// Why approval is needed.
    pub reason: String,
    /// The tool's input, for inspection.
    pub details: String,
    pub requested_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "security.ts")]
pub enum ApprovalOutcome {
    Approved,
    Denied,
    TimedOut,
    Cancelled,
}

impl ApprovalOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::TimedOut => "timedOut",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Fields of a new approval request.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub task_id: Option<Uuid>,
    pub tool: String,
    pub risk: RiskLevel,
    pub summary: String,
    pub reason: String,
    pub details: String,
}

#[derive(Debug)]
struct Waiting {
    approval: PendingApproval,
    reply: oneshot::Sender<bool>,
}

#[derive(Debug)]
pub struct ApprovalBroker {
    waiting: Mutex<HashMap<Uuid, Waiting>>,
    pending: watch::Sender<Vec<PendingApproval>>,
}

impl Default for ApprovalBroker {
    fn default() -> Self {
        Self {
            waiting: Mutex::default(),
            pending: watch::Sender::new(Vec::new()),
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl ApprovalBroker {
    fn waiting(&self) -> MutexGuard<'_, HashMap<Uuid, Waiting>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn publish(&self) {
        let mut list: Vec<PendingApproval> = self
            .waiting()
            .values()
            .map(|w| w.approval.clone())
            .collect();
        list.sort_by_key(|a| a.requested_at_ms);
        self.pending.send_replace(list);
    }

    /// Waits for the user's decision. Returns `TimedOut` after `timeout` and
    /// `Cancelled` when `cancel` fires; both count as "not approved".
    pub async fn request(
        &self,
        request: ApprovalRequest,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> ApprovalOutcome {
        let id = Uuid::new_v4();
        let now = now_ms();
        let (reply, answer) = oneshot::channel();
        self.waiting().insert(
            id,
            Waiting {
                approval: PendingApproval {
                    id,
                    task_id: request.task_id,
                    tool: request.tool,
                    risk: request.risk,
                    summary: request.summary,
                    reason: request.reason,
                    details: request.details,
                    requested_at_ms: now,
                    expires_at_ms: now
                        .saturating_add(i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX)),
                },
                reply,
            },
        );
        self.publish();
        let outcome = tokio::select! {
            () = cancel.cancelled() => ApprovalOutcome::Cancelled,
            () = tokio::time::sleep(timeout) => ApprovalOutcome::TimedOut,
            answer = answer => match answer {
                Ok(true) => ApprovalOutcome::Approved,
                // Denied, or the broker dropped the request.
                _ => ApprovalOutcome::Denied,
            },
        };
        self.waiting().remove(&id);
        self.publish();
        outcome
    }

    /// Records the user's decision. Returns `false` if the request is gone
    /// (already decided, timed out or cancelled).
    pub fn resolve(&self, id: Uuid, approved: bool) -> bool {
        let waiting = self.waiting().remove(&id);
        let resolved = waiting.is_some_and(|w| w.reply.send(approved).is_ok());
        self.publish();
        resolved
    }

    pub fn pending(&self) -> Vec<PendingApproval> {
        self.pending.borrow().clone()
    }

    /// Changes whenever a request is added or removed.
    pub fn subscribe(&self) -> watch::Receiver<Vec<PendingApproval>> {
        self.pending.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            task_id: None,
            tool: "run_command".into(),
            risk: RiskLevel::High,
            summary: "Run git status".into(),
            reason: "high risk action".into(),
            details: "{}".into(),
        }
    }

    async fn wait_for_pending(broker: &ApprovalBroker) -> PendingApproval {
        let mut updates = broker.subscribe();
        loop {
            if let Some(first) = broker.pending().first() {
                return first.clone();
            }
            updates.changed().await.expect("broker alive");
        }
    }

    #[tokio::test]
    async fn the_user_decides() {
        let broker = Arc::new(ApprovalBroker::default());
        for approve in [true, false] {
            let waiting = Arc::clone(&broker);
            let task = tokio::spawn(async move {
                waiting
                    .request(
                        request(),
                        Duration::from_secs(30),
                        &CancellationToken::new(),
                    )
                    .await
            });
            let pending = wait_for_pending(&broker).await;
            assert_eq!(pending.summary, "Run git status");
            assert!(pending.expires_at_ms > pending.requested_at_ms);
            assert!(broker.resolve(pending.id, approve));
            let expected = if approve {
                ApprovalOutcome::Approved
            } else {
                ApprovalOutcome::Denied
            };
            assert_eq!(task.await.expect("join"), expected);
            assert!(broker.pending().is_empty());
            assert!(!broker.resolve(pending.id, true), "decided only once");
        }
    }

    #[tokio::test]
    async fn requests_expire_or_are_cancelled() {
        let broker = ApprovalBroker::default();
        let outcome = broker
            .request(
                request(),
                Duration::from_millis(20),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(outcome, ApprovalOutcome::TimedOut);

        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = broker
            .request(request(), Duration::from_secs(30), &cancel)
            .await;
        assert_eq!(outcome, ApprovalOutcome::Cancelled);
        assert!(broker.pending().is_empty());
    }
}
