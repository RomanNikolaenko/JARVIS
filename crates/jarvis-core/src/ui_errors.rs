//! Errors reported by the UI so they end up in the diagnostic log and the
//! system event log. Input is untrusted: it is truncated and rate limited.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const MAX_UI_ERROR_CHARS: usize = 2_000;
pub const UI_ERRORS_PER_WINDOW: u32 = 20;
pub const UI_ERROR_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct UiErrorReport {
    pub message: String,
    /// Where the error happened, e.g. a component or service name.
    pub context: Option<String>,
}

impl UiErrorReport {
    /// Returns a copy with all fields truncated to safe lengths.
    pub(crate) fn sanitized(self) -> Self {
        Self {
            message: truncate_chars(&self.message, MAX_UI_ERROR_CHARS),
            context: self.context.map(|c| truncate_chars(&c, 200)),
        }
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// Fixed-window rate limiter.
#[derive(Debug)]
pub(crate) struct RateLimiter {
    limit: u32,
    window: Duration,
    state: Mutex<(Instant, u32)>,
}

impl RateLimiter {
    pub(crate) fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            state: Mutex::new((Instant::now(), 0)),
        }
    }

    /// Returns `true` if the caller may proceed.
    pub(crate) fn try_acquire(&self) -> bool {
        self.try_acquire_at(Instant::now())
    }

    fn try_acquire_at(&self, now: Instant) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (window_start, count) = &mut *state;
        if now.duration_since(*window_start) >= self.window {
            *window_start = now;
            *count = 0;
        }
        if *count < self.limit {
            *count += 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_input_on_char_boundaries() {
        let report = UiErrorReport {
            message: "ї".repeat(MAX_UI_ERROR_CHARS + 10),
            context: Some("c".repeat(500)),
        }
        .sanitized();
        assert_eq!(report.message.chars().count(), MAX_UI_ERROR_CHARS + 1);
        assert!(report.message.ends_with('…'));
        assert_eq!(report.context.map(|c| c.chars().count()), Some(201));
    }

    #[test]
    fn short_input_is_unchanged() {
        assert_eq!(truncate_chars("hello", 10), "hello");
    }

    #[test]
    fn rate_limiter_resets_after_the_window() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        let start = Instant::now();
        assert!(limiter.try_acquire_at(start));
        assert!(limiter.try_acquire_at(start));
        assert!(!limiter.try_acquire_at(start + Duration::from_secs(1)));
        assert!(limiter.try_acquire_at(start + Duration::from_secs(61)));
    }
}
