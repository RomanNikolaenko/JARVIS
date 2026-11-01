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

/// Audible frames (about 0.5 s) that only teach the gate the echo level.
const LEARN_FRAMES: usize = 31;
/// The user must be this many times louder than the expected echo.
const MARGIN: f32 = 2.0;
/// Recent frames that decide a barge-in, and how many must be speech.
const DECIDING_FRAMES: usize = 16;
const NEEDED_FRAMES: usize = 12;
/// Playback quieter than this does not reach the microphone.
const AUDIBLE: f32 = 0.005;
/// The lowest noise floor.
const MIN_FLOOR: f32 = 0.003;

/// Decides, frame by frame, whether the microphone hears the user over
/// JARVIS's own voice.
///
/// It learns the coupling — microphone level ÷ playback level — from the
/// first half second of playback (the median, so a word said right then
/// does not spoil it), then follows its upper envelope on frames it does
/// not take for the user: quickly upwards (the volume went up), slowly
/// downwards. A frame counts as the user when the VAD hears speech and it
/// is louder than twice the expected echo (and than the noise floor);
/// without playback the VAD decides alone.
#[derive(Debug, Default)]
pub struct EchoGate {
    learning: Vec<f32>,
    coupling: Option<f32>,
    quietest: Option<f32>,
    recent: VecDeque<bool>,
}

impl EchoGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// One 16 ms microphone frame: its RMS, whether the VAD calls it speech,
    /// and the loudest playback of the last 300 ms. Returns
    /// `Some(frames_back)` on barge-in: the frames from the first speech
    /// frame of the deciding window to this one, inclusive.
    pub fn push(&mut self, frame_rms: f32, speech: bool, reference: f32) -> Option<usize> {
        let quietest = match self.quietest {
            Some(q) if frame_rms >= q => q + (frame_rms - q) * 0.001,
            _ => frame_rms,
        };
        self.quietest = Some(quietest);
        let floor = (2.0 * quietest).max(MIN_FLOOR);
        let audible = reference > AUDIBLE;
        let ratio = frame_rms / reference.max(AUDIBLE);

        let candidate = match self.coupling {
            None if audible => {
                self.learning.push(ratio);
                if self.learning.len() >= LEARN_FRAMES {
                    self.learning.sort_by(f32::total_cmp);
                    self.coupling = Some(self.learning[self.learning.len() / 2]);
                }
                false
            }
            coupling => {
                let coupling = coupling.unwrap_or(0.0);
                let candidate = speech
                    && frame_rms > floor
                    && (!audible || frame_rms > MARGIN * coupling * reference);
                if audible && !candidate {
                    let step = if ratio > coupling { 0.2 } else { 0.01 };
                    self.coupling = Some(coupling + step * (ratio - coupling));
                }
                candidate
            }
        };

        self.recent.push_back(candidate);
        if self.recent.len() > DECIDING_FRAMES {
            self.recent.pop_front();
        }
        if self.recent.iter().filter(|c| **c).count() < NEEDED_FRAMES {
            return None;
        }
        let first = self.recent.iter().position(|c| *c).unwrap_or(0);
        let frames_back = self.recent.len() - first;
        self.recent.clear();
        Some(frames_back)
    }

    /// Starts over, for a new reply.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames of 16 ms in the 300 ms reference window.
    const WINDOW_FRAMES: usize = 19;

    /// JARVIS speaking: syllables with a short pause every two seconds.
    fn playback(frame: usize) -> f32 {
        let t = frame as f32 * 0.016;
        if t % 2.0 > 1.7 {
            0.0
        } else {
            0.05 + 0.25 * (std::f32::consts::TAU * 3.0 * t).sin().abs()
        }
    }

    /// A deterministic pseudo-random number in [0, 1).
    fn noise(seed: &mut u64) -> f32 {
        *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (*seed >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Feeds `frames` frames and returns the first barge-in as (frame,
    /// frames_back). `gain` is the echo path, `user` the user's level.
    fn run(
        frames: usize,
        playing: impl Fn(usize) -> f32,
        gain: impl Fn(usize) -> f32,
        user: impl Fn(usize) -> f32,
    ) -> Option<(usize, usize)> {
        let mut gate = EchoGate::new();
        let mut seed = 7;
        for f in 0..frames {
            // The meter: the loudest playback of the last 300 ms.
            let reference = (f.saturating_sub(WINDOW_FRAMES - 1)..=f)
                .map(&playing)
                .fold(0.0, f32::max);
            // The echo arrives 5 frames (80 ms) later, a little uneven.
            let echo = gain(f) * playing(f.saturating_sub(5)) * (0.85 + 0.3 * noise(&mut seed))
                + 0.001 * noise(&mut seed);
            let voice = user(f);
            let mic = (echo * echo + voice * voice).sqrt();
            let speech = voice > 0.0 || echo > 0.01;
            if let Some(back) = gate.push(mic, speech, reference) {
                return Some((f, back));
            }
        }
        None
    }

    const THIRTY_SECONDS: usize = 1875;

    #[test]
    fn echo_alone_never_triggers() {
        for gain in [0.05, 0.5, 2.0] {
            assert_eq!(
                run(THIRTY_SECONDS, playback, |_| gain, |_| 0.0),
                None,
                "gain {gain}"
            );
        }
    }

    #[test]
    fn volume_up_mid_reply_does_not_trigger() {
        let gain = |f: usize| if f < 190 { 0.5 } else { 1.0 };
        assert_eq!(run(THIRTY_SECONDS, playback, gain, |_| 0.0), None);
    }

    #[test]
    fn speech_over_the_echo_triggers() {
        let user = |f: usize| if f >= 250 { 0.45 } else { 0.0 };
        let (at, back) = run(THIRTY_SECONDS, playback, |_| 0.5, user).expect("barge-in");
        assert!((250 + 11..=250 + 18).contains(&at), "{at}");
        assert!((12..=16).contains(&back), "{back}");
    }

    #[test]
    fn speech_in_a_pause_triggers() {
        // JARVIS stops at frame 300; the user speaks quietly at 330.
        let playing = |f: usize| if f < 300 { playback(f) } else { 0.0 };
        let user = |f: usize| if f >= 330 { 0.05 } else { 0.0 };
        let (at, _) = run(THIRTY_SECONDS, playing, |_| 0.5, user).expect("barge-in");
        assert!((330 + 11..=330 + 18).contains(&at), "{at}");
    }

    #[test]
    fn headphones_trigger_on_any_speech() {
        let user = |f: usize| if f >= 100 { 0.05 } else { 0.0 };
        let (at, _) = run(THIRTY_SECONDS, playback, |_| 0.0, user).expect("barge-in");
        assert!((100 + 11..=100 + 18).contains(&at), "{at}");
    }

    #[test]
    fn nothing_triggers_while_learning() {
        // Loud speech at the very start is not taken for the user yet.
        let early = |f: usize| if f < 10 { 0.45 } else { 0.0 };
        assert_eq!(run(31, playback, |_| 0.5, early), None);
        let later = |f: usize| if (10..200).contains(&f) { 0.0 } else { 0.45 };
        let (at, _) = run(THIRTY_SECONDS, playback, |_| 0.5, later).expect("barge-in");
        assert!((200 + 11..=200 + 18).contains(&at), "{at}");
    }

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
