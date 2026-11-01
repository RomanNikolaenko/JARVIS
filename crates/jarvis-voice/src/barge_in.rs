//! Noticing the user speaking over a spoken reply.

use std::collections::VecDeque;
use std::time::Duration;

use crate::echo::{EchoGate, PlaybackMeter, rms};
use crate::vad::{FRAME, SAMPLE_RATE, SegmenterConfig, SpeechScorer};

/// Playback this recent may still be heard (output and input latency, the
/// room).
const ECHO_WINDOW: Duration = Duration::from_millis(300);
/// Microphone audio kept for the start of the phrase.
const HISTORY_SAMPLES: usize = SAMPLE_RATE as usize * 3 / 2;
/// The phrase starts this long before the first speech frame.
const PRE_ROLL_SAMPLES: usize = SAMPLE_RATE as usize * 3 / 10;

/// Listens to the microphone while JARVIS speaks and reports the user's
/// speech, with the audio from shortly before it began.
pub struct BargeInDetector {
    scorer: Box<dyn SpeechScorer>,
    meter: PlaybackMeter,
    gate: EchoGate,
    threshold: f32,
    pending: Vec<f32>,
    history: VecDeque<f32>,
}

impl std::fmt::Debug for BargeInDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BargeInDetector")
            .field("gate", &self.gate)
            .finish_non_exhaustive()
    }
}

impl BargeInDetector {
    pub fn new(scorer: Box<dyn SpeechScorer>, meter: PlaybackMeter) -> Self {
        Self {
            scorer,
            meter,
            gate: EchoGate::new(),
            threshold: SegmenterConfig::default().threshold,
            pending: Vec::with_capacity(FRAME * 2),
            history: VecDeque::with_capacity(HISTORY_SAMPLES),
        }
    }

    /// Takes 16 kHz microphone audio. On barge-in returns the audio from
    /// 300 ms before the user's first speech frame up to now.
    pub fn push(&mut self, samples: &[f32]) -> Option<Vec<f32>> {
        self.pending.extend_from_slice(samples);
        let mut used = 0;
        while self.pending.len() - used >= FRAME {
            let frame = &self.pending[used..used + FRAME];
            used += FRAME;
            self.history.extend(frame);
            while self.history.len() > HISTORY_SAMPLES {
                self.history.pop_front();
            }
            let speech = self.scorer.score(frame) >= self.threshold;
            let reference = self.meter.loudest_within(ECHO_WINDOW);
            if let Some(frames_back) = self.gate.push(rms(frame), speech, reference) {
                let take = (frames_back * FRAME + PRE_ROLL_SAMPLES).min(self.history.len());
                let mut phrase: Vec<f32> = self
                    .history
                    .iter()
                    .skip(self.history.len() - take)
                    .copied()
                    .collect();
                // What arrived after this frame belongs to the phrase too.
                phrase.extend_from_slice(&self.pending[used..]);
                self.pending.clear();
                self.history.clear();
                return Some(phrase);
            }
        }
        self.pending.drain(..used);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames with any sample louder than 0.1 are speech.
    struct Loud;

    impl SpeechScorer for Loud {
        fn score(&mut self, frame: &[f32]) -> f32 {
            if frame.iter().any(|s| s.abs() > 0.1) {
                1.0
            } else {
                0.0
            }
        }
        fn reset(&mut self) {}
    }

    /// Feeds `audio` in 20 ms chunks, as the microphone does; `playing`
    /// is recorded into the meter before each chunk.
    fn feed(
        detector: &mut BargeInDetector,
        meter: &PlaybackMeter,
        audio: &[f32],
        playing: f32,
    ) -> Option<Vec<f32>> {
        for chunk in audio.chunks(320) {
            if playing > 0.0 {
                meter.record(&[playing; 256]);
            }
            if let Some(phrase) = detector.push(chunk) {
                return Some(phrase);
            }
        }
        None
    }

    #[test]
    fn returns_the_phrase_from_its_start() {
        let meter = PlaybackMeter::default();
        let mut detector = BargeInDetector::new(Box::new(Loud), meter.clone());
        let mut audio = vec![0.0; 16_000];
        audio.extend(vec![0.5; 8_000]);
        let phrase = feed(&mut detector, &meter, &audio, 0.0).expect("barge-in");
        let silence = phrase.iter().take_while(|s| **s == 0.0).count();
        // The phrase starts up to 300 ms (4800 samples) before the speech.
        assert!(silence > 0 && silence <= 4_800 + 256, "{silence}");
        let speech = phrase.len() - silence;
        assert!((12 * 256..=16 * 256 + 320).contains(&speech), "{speech}");
    }

    #[test]
    fn quiet_echo_is_ignored() {
        let meter = PlaybackMeter::default();
        let mut detector = BargeInDetector::new(Box::new(Loud), meter.clone());
        // Two seconds of JARVIS at 0.4 heard back at 0.2.
        assert_eq!(feed(&mut detector, &meter, &vec![0.2; 32_000], 0.4), None);
    }
}
