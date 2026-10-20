//! In-process event bus for JARVIS.
//!
//! The bus serves two purposes:
//!
//! 1. Decoupled notification between core components (publish/subscribe).
//! 2. A bounded, in-memory *system event log* that the UI can display.
//!
//! Events are user-facing, structured records ("chat request cancelled",
//! "settings saved"). They are intentionally separate from diagnostic logs,
//! which are written through `tracing`. Every published event is also mirrored
//! to `tracing` so log files contain the full picture.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use ts_rs::TS;

/// Default number of events kept in the in-memory history.
pub const DEFAULT_HISTORY_CAPACITY: usize = 500;
/// Default capacity of the broadcast channel used for live subscribers.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// Severity of a [`SystemEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "events.ts")]
pub enum EventLevel {
    Debug,
    Info,
    Warning,
    Error,
}

/// Functional area that produced a [`SystemEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "events.ts")]
pub enum EventCategory {
    Lifecycle,
    Config,
    Health,
    Ai,
    Runtime,
    Memory,
    Chat,
    Agents,
    Security,
    Platform,
    Ui,
}

/// A structured, user-facing system event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "events.ts")]
pub struct SystemEvent {
    /// Monotonically increasing identifier, unique within one application run.
    pub id: u64,
    /// Unix timestamp in milliseconds.
    pub timestamp_ms: u64,
    pub level: EventLevel,
    pub category: EventCategory,
    pub message: String,
}

/// Error returned by [`EventSubscription::recv`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    /// The subscriber fell behind and `skipped` events were dropped for it.
    /// The subscription remains usable.
    Lagged { skipped: u64 },
    /// The bus was dropped; no more events will arrive.
    Closed,
}

impl std::fmt::Display for RecvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lagged { skipped } => write!(f, "subscriber lagged behind by {skipped} events"),
            Self::Closed => f.write_str("event bus closed"),
        }
    }
}

impl std::error::Error for RecvError {}

/// A live subscription to the event bus.
#[derive(Debug)]
pub struct EventSubscription {
    receiver: broadcast::Receiver<SystemEvent>,
}

impl EventSubscription {
    /// Waits for the next event.
    pub async fn recv(&mut self) -> Result<SystemEvent, RecvError> {
        self.receiver.recv().await.map_err(|err| match err {
            broadcast::error::RecvError::Lagged(skipped) => RecvError::Lagged { skipped },
            broadcast::error::RecvError::Closed => RecvError::Closed,
        })
    }
}

#[derive(Debug)]
struct Inner {
    sender: broadcast::Sender<SystemEvent>,
    history: Mutex<VecDeque<SystemEvent>>,
    history_capacity: usize,
    next_id: AtomicU64,
}

/// Cheaply cloneable handle to the shared event bus.
#[derive(Debug, Clone)]
pub struct EventBus {
    inner: Arc<Inner>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(DEFAULT_HISTORY_CAPACITY, DEFAULT_CHANNEL_CAPACITY)
    }
}

impl EventBus {
    /// Creates a bus that keeps up to `history_capacity` events in memory and
    /// buffers up to `channel_capacity` events per live subscriber.
    ///
    /// Capacities of zero are raised to one.
    pub fn new(history_capacity: usize, channel_capacity: usize) -> Self {
        let history_capacity = history_capacity.max(1);
        let (sender, _) = broadcast::channel(channel_capacity.max(1));
        Self {
            inner: Arc::new(Inner {
                sender,
                history: Mutex::new(VecDeque::with_capacity(history_capacity)),
                history_capacity,
                next_id: AtomicU64::new(1),
            }),
        }
    }

    /// Publishes an event to the history and to all live subscribers.
    pub fn publish(
        &self,
        level: EventLevel,
        category: EventCategory,
        message: impl Into<String>,
    ) -> SystemEvent {
        let event = SystemEvent {
            id: self.inner.next_id.fetch_add(1, Ordering::Relaxed),
            timestamp_ms: unix_millis(),
            level,
            category,
            message: message.into(),
        };
        mirror_to_tracing(&event);

        {
            let mut history = self.history();
            if history.len() == self.inner.history_capacity {
                history.pop_front();
            }
            history.push_back(event.clone());
        }

        // An error here only means that nobody is currently subscribed.
        let _ = self.inner.sender.send(event.clone());
        event
    }

    pub fn info(&self, category: EventCategory, message: impl Into<String>) -> SystemEvent {
        self.publish(EventLevel::Info, category, message)
    }

    pub fn warning(&self, category: EventCategory, message: impl Into<String>) -> SystemEvent {
        self.publish(EventLevel::Warning, category, message)
    }

    pub fn error(&self, category: EventCategory, message: impl Into<String>) -> SystemEvent {
        self.publish(EventLevel::Error, category, message)
    }

    /// Subscribes to events published after this call.
    pub fn subscribe(&self) -> EventSubscription {
        EventSubscription {
            receiver: self.inner.sender.subscribe(),
        }
    }

    /// Returns up to `limit` most recent events, oldest first.
    pub fn recent(&self, limit: usize) -> Vec<SystemEvent> {
        let history = self.history();
        let skip = history.len().saturating_sub(limit);
        history.iter().skip(skip).cloned().collect()
    }

    /// Number of live subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.inner.sender.receiver_count()
    }

    fn history(&self) -> MutexGuard<'_, VecDeque<SystemEvent>> {
        // The history only holds plain data, so a poisoned lock is still consistent.
        self.inner
            .history
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn mirror_to_tracing(event: &SystemEvent) {
    let category = event.category;
    match event.level {
        EventLevel::Debug => {
            tracing::debug!(target: "jarvis_events", ?category, "{}", event.message)
        }
        EventLevel::Info => tracing::info!(target: "jarvis_events", ?category, "{}", event.message),
        EventLevel::Warning => {
            tracing::warn!(target: "jarvis_events", ?category, "{}", event.message)
        }
        EventLevel::Error => {
            tracing::error!(target: "jarvis_events", ?category, "{}", event.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_monotonic_and_history_is_bounded() {
        let bus = EventBus::new(3, 8);
        for i in 0..5 {
            bus.info(EventCategory::Lifecycle, format!("event {i}"));
        }

        let recent = bus.recent(10);
        assert_eq!(recent.len(), 3);
        let ids: Vec<u64> = recent.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![3, 4, 5]);
        assert_eq!(recent[2].message, "event 4");
    }

    #[test]
    fn recent_respects_limit_and_returns_newest() {
        let bus = EventBus::new(10, 8);
        for i in 0..4 {
            bus.info(EventCategory::Config, format!("event {i}"));
        }
        let recent = bus.recent(2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].message, "event 2");
        assert_eq!(recent[1].message, "event 3");
        assert!(bus.recent(0).is_empty());
    }

    #[test]
    fn zero_capacities_are_clamped() {
        let bus = EventBus::new(0, 0);
        bus.info(EventCategory::Ui, "a");
        bus.info(EventCategory::Ui, "b");
        assert_eq!(bus.recent(10).len(), 1);
    }

    #[tokio::test]
    async fn subscribers_receive_events_published_after_subscribing() {
        let bus = EventBus::default();
        bus.info(EventCategory::Lifecycle, "before");

        let mut sub = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        let published = bus.warning(EventCategory::Security, "after");

        let received = sub.recv().await.expect("event");
        assert_eq!(received, published);
        assert_eq!(received.level, EventLevel::Warning);
    }

    #[tokio::test]
    async fn slow_subscribers_observe_lag_and_keep_working() {
        let bus = EventBus::new(16, 2);
        let mut sub = bus.subscribe();
        for i in 0..5 {
            bus.info(EventCategory::Chat, format!("event {i}"));
        }

        assert_eq!(sub.recv().await, Err(RecvError::Lagged { skipped: 3 }));
        let next = sub.recv().await.expect("event after lag");
        assert_eq!(next.message, "event 3");
    }

    #[tokio::test]
    async fn subscription_closes_when_bus_is_dropped() {
        let bus = EventBus::default();
        let mut sub = bus.subscribe();
        drop(bus);
        assert_eq!(sub.recv().await, Err(RecvError::Closed));
    }

    #[test]
    fn serializes_with_camel_case_fields() {
        let bus = EventBus::default();
        let event = bus.error(EventCategory::Ai, "boom");
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(json["level"], "error");
        assert_eq!(json["category"], "ai");
        assert!(json.get("timestampMs").is_some());
    }
}
