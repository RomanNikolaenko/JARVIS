//! Telling the user's voice from JARVIS's own voice coming back from the
//! speakers: the speaker records how loud it just played
//! ([`PlaybackMeter`]).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Blocks older than this are forgotten.
const METER_HISTORY: Duration = Duration::from_secs(2);

/// How loud the speaker played recently, shared between the playback
/// callback and the microphone side.
#[derive(Debug, Clone, Default)]
pub struct PlaybackMeter(Arc<Mutex<VecDeque<(Instant, f32)>>>);

impl PlaybackMeter {
    fn blocks(&self) -> std::sync::MutexGuard<'_, VecDeque<(Instant, f32)>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records one block of samples just written to the speaker.
    pub fn record(&self, samples: &[f32]) {
        if !samples.is_empty() {
            self.record_rms(rms(samples));
        }
    }

    /// Records the RMS of a block just written to the speaker.
    pub fn record_rms(&self, rms: f32) {
        let now = Instant::now();
        let mut blocks = self.blocks();
        while blocks
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > METER_HISTORY)
        {
            blocks.pop_front();
        }
        blocks.push_back((now, rms));
    }

    /// The loudest block (RMS) played within `window`.
    pub fn loudest_within(&self, window: Duration) -> f32 {
        let now = Instant::now();
        self.blocks()
            .iter()
            .rev()
            .take_while(|(at, _)| now.duration_since(*at) <= window)
            .map(|(_, level)| *level)
            .fold(0.0, f32::max)
    }

    /// Forgets everything recorded.
    pub fn clear(&self) {
        self.blocks().clear();
    }
}

/// Root mean square of a block.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_reports_the_loudest_recent_block() {
        let meter = PlaybackMeter::default();
        meter.record(&[0.1; 256]);
        meter.record(&[-0.4; 256]);
        meter.record(&[]);
        let loudest = meter.loudest_within(Duration::from_millis(300));
        assert!((loudest - 0.4).abs() < 1e-4, "{loudest}");
        meter.clear();
        assert!(meter.loudest_within(Duration::from_millis(300)) < f32::EPSILON);
    }

    #[test]
    fn old_blocks_expire() {
        let meter = PlaybackMeter::default();
        meter.record(&[0.5; 256]);
        std::thread::sleep(Duration::from_millis(350));
        assert!(meter.loudest_within(Duration::from_millis(300)) < f32::EPSILON);
        assert!(meter.loudest_within(Duration::from_secs(1)) > 0.4);
    }
}
