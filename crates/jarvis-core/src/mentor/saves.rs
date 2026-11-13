//! Speaking up when the user saves: a light poll of the project folder
//! (sizes and modification times only) that asks for a mentor turn once the
//! user's changes have settled, within limits that keep JARVIS from talking
//! on and on by itself.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::MentorPhase;
use super::snapshot::ProjectSnapshot;
use crate::JarvisCore;

/// Turns started by saving in a row, without the user saying anything,
/// before JARVIS waits for the user.
pub(crate) const MAX_IN_A_ROW: u32 = 3;

/// How often the folder is looked at and how long JARVIS waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MentorTiming {
    /// Between two looks at the project folder.
    pub poll: Duration,
    /// Files must stay unchanged this long before JARVIS reacts.
    pub settle: Duration,
    /// At least this long between two reactions.
    pub cooldown: Duration,
}

impl Default for MentorTiming {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(1),
            settle: Duration::from_secs(2),
            cooldown: Duration::from_secs(20),
        }
    }
}

/// The user saved these files; the UI starts a mentor turn for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct MentorNudge {
    pub conversation_id: Uuid,
    /// Relative paths, `/` separators, sorted.
    pub files: Vec<String>,
}

/// What the watch loop remembers between looks.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    conversation_id: Option<Uuid>,
    previous: ProjectSnapshot,
    last_change: Option<Instant>,
    last_nudge: Option<Instant>,
}

impl JarvisCore {
    /// The UI is answering, speaking or holding a phrase: no reaction now.
    pub fn mentor_set_busy(&self, busy: bool) {
        self.mentor.busy.store(busy, Ordering::Relaxed);
    }

    /// The user said something: JARVIS may speak up on saves again.
    pub fn mentor_owner_spoke(&self) {
        self.mentor.in_a_row.store(0, Ordering::Relaxed);
    }

    pub fn subscribe_mentor_nudges(&self) -> tokio::sync::broadcast::Receiver<MentorNudge> {
        self.mentor.nudges.subscribe()
    }

    /// One look at the project folder.
    pub(crate) async fn mentor_watch_tick(&self, tracker: &mut Tracker) {
        let (conversation_id, root, baseline) = {
            let session = self.mentor.session();
            match session.as_ref() {
                Some(s) => (s.conversation_id, s.root.clone(), s.watch_baseline.clone()),
                None => {
                    *tracker = Tracker::default();
                    return;
                }
            }
        };
        let scan_root = root.clone();
        let Ok(Ok(now)) =
            tokio::task::spawn_blocking(move || ProjectSnapshot::scan(&scan_root)).await
        else {
            return;
        };
        if tracker.conversation_id != Some(conversation_id) {
            // A change may have come just before this first look: it
            // settles from now.
            *tracker = Tracker {
                conversation_id: Some(conversation_id),
                previous: now,
                last_change: Some(Instant::now()),
                last_nudge: None,
            };
            return;
        }
        if !now.changes_since(&tracker.previous).is_empty() {
            tracker.last_change = Some(Instant::now());
        }
        tracker.previous = now.clone();

        let changes = now.changes_since(&baseline);
        if changes.is_empty() {
            return;
        }
        let timing = self.mentor.timing;
        let settled = tracker
            .last_change
            .is_none_or(|at| at.elapsed() >= timing.settle);
        let rested = tracker
            .last_nudge
            .is_none_or(|at| at.elapsed() >= timing.cooldown);
        let status = self.mentor.status();
        let free = status.phase == MentorPhase::Listening
            && status.pending_write.is_none()
            && !self.mentor.busy.load(Ordering::Relaxed)
            && self.mentor.in_a_row.load(Ordering::Relaxed) < MAX_IN_A_ROW;
        if !(settled && rested && free) {
            return;
        }
        let mut files: Vec<String> = changes
            .created
            .iter()
            .chain(&changes.changed)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        files.sort();
        {
            let mut session = self.mentor.session();
            match session
                .as_mut()
                .filter(|s| s.conversation_id == conversation_id)
            {
                Some(s) => s.watch_baseline = now,
                None => return,
            }
        }
        tracker.last_nudge = Some(Instant::now());
        // Only deletions: nothing to look at, but the baseline moves on.
        if files.is_empty() {
            return;
        }
        self.mentor.in_a_row.fetch_add(1, Ordering::Relaxed);
        let _ = self.mentor.nudges.send(MentorNudge {
            conversation_id,
            files,
        });
    }
}
