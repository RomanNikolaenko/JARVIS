//! Approvals: a tool call that needs the user's consent waits here until the
//! user approves or denies it, the request times out, or its task is
//! cancelled. Nothing is approved implicitly.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;
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
    deadline: Instant,
    cancel: CancellationToken,
    reply: oneshot::Sender<ApprovalOutcome>,
}

/// A dropped request future must withdraw its permission request as well.
struct PendingRequest<'a> {
    broker: &'a ApprovalBroker,
    id: Uuid,
}

impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        let mut waiting = self.broker.waiting();
        if waiting.remove(&self.id).is_some() {
            self.broker.publish_waiting(&waiting);
        }
    }
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

    /// The caller keeps the waiting lock through publication: concurrent
    /// mutations cannot publish an older queue after a newer one.
    fn publish_waiting(&self, waiting: &HashMap<Uuid, Waiting>) {
        let mut list: Vec<PendingApproval> = waiting.values().map(|w| w.approval.clone()).collect();
        list.sort_by_key(|a| (a.requested_at_ms, a.id));
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
        if cancel.is_cancelled() {
            return ApprovalOutcome::Cancelled;
        }
        if timeout.is_zero() {
            return ApprovalOutcome::TimedOut;
        }
        let Some(deadline) = Instant::now().checked_add(timeout) else {
            return ApprovalOutcome::TimedOut;
        };
        let id = Uuid::new_v4();
        let now = now_ms();
        let (reply, answer) = oneshot::channel();
        let pending = PendingRequest { broker: self, id };
        {
            let mut waiting = self.waiting();
            waiting.insert(
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
                    deadline,
                    cancel: cancel.clone(),
                    reply,
                },
            );
            self.publish_waiting(&waiting);
        }
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => ApprovalOutcome::Cancelled,
            () = tokio::time::sleep_until(deadline) => ApprovalOutcome::TimedOut,
            answer = answer => answer.unwrap_or(ApprovalOutcome::Denied),
        };
        // A timely decision acknowledges delivery, not execution. If this
        // future resumes after cancellation or its deadline, permission has
        // expired even when a decision and timer became ready together.
        drop(pending);
        if cancel.is_cancelled() {
            ApprovalOutcome::Cancelled
        } else if Instant::now() >= deadline {
            ApprovalOutcome::TimedOut
        } else {
            outcome
        }
    }

    /// Records the user's decision. Returns `false` if the request is gone
    /// (already decided, timed out or cancelled), its waiter was dropped, or
    /// its deadline/cancellation fired before the waiter could be polled.
    /// `true` acknowledges a live decision, not execution of the action.
    pub fn resolve(&self, id: Uuid, approved: bool) -> bool {
        let mut waiting = self.waiting();
        let resolved = waiting.remove(&id).is_some_and(|w| {
            let outcome = if w.cancel.is_cancelled() {
                ApprovalOutcome::Cancelled
            } else if Instant::now() >= w.deadline {
                ApprovalOutcome::TimedOut
            } else if approved {
                ApprovalOutcome::Approved
            } else {
                ApprovalOutcome::Denied
            };
            let live = matches!(outcome, ApprovalOutcome::Approved | ApprovalOutcome::Denied);
            w.reply.send(outcome).is_ok() && live
        });
        self.publish_waiting(&waiting);
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
    use std::future::Future;
    use std::pin::Pin;
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

    /// Poll without spawning: the waiter cannot consume a decision or timeout
    /// until the test explicitly polls it again.
    async fn register(
        waiter: Pin<&mut impl Future<Output = ApprovalOutcome>>,
        broker: &ApprovalBroker,
    ) -> PendingApproval {
        tokio::select! {
            biased;
            outcome = waiter => panic!("approval returned before a decision: {outcome:?}"),
            () = tokio::task::yield_now() => {}
        }
        broker
            .pending()
            .first()
            .cloned()
            .expect("request registered")
    }

    #[tokio::test(start_paused = true)]
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

    #[tokio::test(start_paused = true)]
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

    #[tokio::test(start_paused = true)]
    async fn late_decisions_are_refused_before_the_expired_waiter_is_polled() {
        for approved in [true, false] {
            let broker = ApprovalBroker::default();
            let cancel = CancellationToken::new();
            let waiter = broker.request(request(), Duration::from_secs(30), &cancel);
            tokio::pin!(waiter);
            let pending = register(waiter.as_mut(), &broker).await;
            tokio::time::advance(Duration::from_secs(30)).await;
            assert!(!broker.resolve(pending.id, approved));
            assert_eq!(waiter.await, ApprovalOutcome::TimedOut);
            assert!(broker.pending().is_empty());
            assert!(!broker.resolve(pending.id, true));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_decisions_are_refused_before_the_waiter_is_polled() {
        for approved in [true, false] {
            let broker = ApprovalBroker::default();
            let cancel = CancellationToken::new();
            let waiter = broker.request(request(), Duration::from_secs(30), &cancel);
            tokio::pin!(waiter);
            let pending = register(waiter.as_mut(), &broker).await;
            cancel.cancel();
            assert!(!broker.resolve(pending.id, approved));
            assert_eq!(waiter.await, ApprovalOutcome::Cancelled);
            assert!(broker.pending().is_empty());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_or_deadline_wins_over_an_already_delivered_allow() {
        for cancelled in [true, false] {
            let broker = ApprovalBroker::default();
            let cancel = CancellationToken::new();
            let waiter = broker.request(request(), Duration::from_secs(30), &cancel);
            tokio::pin!(waiter);
            let pending = register(waiter.as_mut(), &broker).await;
            assert!(broker.resolve(pending.id, true));
            if cancelled {
                cancel.cancel();
            } else {
                tokio::time::advance(Duration::from_secs(30)).await;
            }
            assert_eq!(
                waiter.await,
                if cancelled {
                    ApprovalOutcome::Cancelled
                } else {
                    ApprovalOutcome::TimedOut
                }
            );
            assert!(broker.pending().is_empty());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_waiter_withdraws_its_request() {
        let broker = ApprovalBroker::default();
        let cancel = CancellationToken::new();
        let pending = {
            let waiter = broker.request(request(), Duration::from_secs(30), &cancel);
            tokio::pin!(waiter);
            register(waiter.as_mut(), &broker).await
        };
        assert!(broker.pending().is_empty());
        assert!(!broker.resolve(pending.id, true));
        assert!(!broker.resolve(pending.id, false));
    }

    #[tokio::test(start_paused = true)]
    async fn aborting_the_request_task_withdraws_only_its_approval() {
        let broker = Arc::new(ApprovalBroker::default());
        let first = Arc::clone(&broker);
        let task = tokio::spawn(async move {
            first
                .request(
                    request(),
                    Duration::from_secs(30),
                    &CancellationToken::new(),
                )
                .await
        });
        let first = wait_for_pending(&broker).await;
        let cancel = CancellationToken::new();
        let remaining = broker.request(request(), Duration::from_secs(30), &cancel);
        tokio::pin!(remaining);
        tokio::select! {
            biased;
            outcome = &mut remaining => panic!("approval returned before a decision: {outcome:?}"),
            () = tokio::task::yield_now() => {}
        }
        let second = broker
            .pending()
            .into_iter()
            .find(|approval| approval.id != first.id)
            .expect("second request registered");
        task.abort();
        assert!(task.await.expect_err("task aborted").is_cancelled());
        assert_eq!(broker.pending(), vec![second.clone()]);
        assert!(!broker.resolve(first.id, true));
        assert!(broker.resolve(second.id, false));
        assert_eq!(remaining.await, ApprovalOutcome::Denied);
    }

    #[tokio::test(start_paused = true)]
    async fn zero_time_or_existing_cancellation_never_publishes_an_approval() {
        let broker = ApprovalBroker::default();
        let cancel = CancellationToken::new();
        assert_eq!(
            broker.request(request(), Duration::ZERO, &cancel).await,
            ApprovalOutcome::TimedOut
        );
        cancel.cancel();
        for timeout in [Duration::ZERO, Duration::from_secs(30)] {
            assert_eq!(
                broker.request(request(), timeout, &cancel).await,
                ApprovalOutcome::Cancelled
            );
        }
        assert!(broker.pending().is_empty());
    }
}
