//! Voice activity detection and utterance segmentation.
//!
//! Earshot scores 16 ms frames of 16 kHz audio (0 = no voice, 1 = voice).
//! The segmenter turns the scores into utterances: speech starts after a
//! few voiced frames in a row (with some audio before them kept, so the first
//! syllable is not cut), and ends after a stretch of silence.

use std::collections::VecDeque;

/// Sample rate of everything after capture.
pub const SAMPLE_RATE: u32 = 16_000;
/// Samples per VAD frame (16 ms at 16 kHz).
pub const FRAME: usize = 256;
const FRAME_MS: u32 = 16;

/// Scores one frame for the presence of speech.
pub trait SpeechScorer: Send {
    fn score(&mut self, frame: &[f32]) -> f32;
    fn reset(&mut self);
}

impl SpeechScorer for Box<dyn SpeechScorer> {
    fn score(&mut self, frame: &[f32]) -> f32 {
        (**self).score(frame)
    }

    fn reset(&mut self) {
        (**self).reset();
    }
}

/// The earshot neural VAD.
pub struct EarshotScorer(Box<earshot::Detector>);

impl Default for EarshotScorer {
    fn default() -> Self {
        Self(earshot::Detector::default_boxed())
    }
}

impl std::fmt::Debug for EarshotScorer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EarshotScorer")
    }
}

impl SpeechScorer for EarshotScorer {
    fn score(&mut self, frame: &[f32]) -> f32 {
        // Earshot asserts (in debug builds) that samples are within [-1, 1].
        let clamped: Vec<f32> = frame.iter().map(|s| s.clamp(-1.0, 1.0)).collect();
        self.0.predict_f32(&clamped)
    }

    fn reset(&mut self) {
        self.0.reset();
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmenterConfig {
    /// Score at or above which a frame counts as speech.
    pub threshold: f32,
    /// Voiced frames in a row that start an utterance.
    pub start_frames: usize,
    /// Silence that ends an utterance.
    pub end_silence_ms: u32,
    /// Audio kept from before the start.
    pub pre_roll_ms: u32,
    /// Shorter "utterances" (coughs, clicks) are dropped.
    pub min_speech_ms: u32,
    /// Longer ones are cut and delivered.
    pub max_utterance_ms: u32,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            start_frames: 4,
            end_silence_ms: 800,
            pre_roll_ms: 320,
            min_speech_ms: 250,
            max_utterance_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SegmentEvent {
    /// Speech began (the UI can show it at once).
    Started,
    /// A complete utterance, 16 kHz mono.
    Finished(Vec<f32>),
}

#[derive(Debug)]
pub struct Segmenter<S = EarshotScorer> {
    scorer: S,
    config: SegmenterConfig,
    pending: Vec<f32>,
    pre_roll: VecDeque<f32>,
    voiced_run: usize,
    in_speech: bool,
    utterance: Vec<f32>,
    speech_frames: usize,
    silence_frames: usize,
    level: f32,
}

impl Segmenter<EarshotScorer> {
    pub fn new(config: SegmenterConfig) -> Self {
        Self::with_scorer(EarshotScorer::default(), config)
    }
}

impl<S: SpeechScorer> Segmenter<S> {
    pub fn with_scorer(scorer: S, config: SegmenterConfig) -> Self {
        Self {
            scorer,
            config,
            pending: Vec::with_capacity(FRAME),
            pre_roll: VecDeque::new(),
            voiced_run: 0,
            in_speech: false,
            utterance: Vec::new(),
            speech_frames: 0,
            silence_frames: 0,
            level: 0.0,
        }
    }

    /// Loudness of the last frame (RMS, 0–1), for a level meter.
    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn in_speech(&self) -> bool {
        self.in_speech
    }

    /// Feeds 16 kHz mono samples.
    pub fn push(&mut self, samples: &[f32]) -> Vec<SegmentEvent> {
        let mut events = Vec::new();
        self.pending.extend_from_slice(samples);
        let mut start = 0;
        while self.pending.len() - start >= FRAME {
            let frame: Vec<f32> = self.pending[start..start + FRAME].to_vec();
            start += FRAME;
            self.frame(&frame, &mut events);
        }
        self.pending.drain(..start);
        events
    }

    /// Ends the current utterance now (e.g. push-to-talk released).
    pub fn flush(&mut self) -> Option<Vec<f32>> {
        if !self.in_speech {
            self.reset();
            return None;
        }
        let pending = std::mem::take(&mut self.pending);
        self.utterance.extend_from_slice(&pending);
        let utterance = self.take_utterance();
        self.reset();
        utterance
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.pre_roll.clear();
        self.voiced_run = 0;
        self.in_speech = false;
        self.utterance.clear();
        self.speech_frames = 0;
        self.silence_frames = 0;
        self.level = 0.0;
        self.scorer.reset();
    }

    fn frame(&mut self, frame: &[f32], events: &mut Vec<SegmentEvent>) {
        self.level = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        let voiced = self.scorer.score(frame) >= self.config.threshold;
        if !self.in_speech {
            self.pre_roll.extend(frame.iter().copied());
            let keep = (self.config.pre_roll_ms / FRAME_MS) as usize * FRAME
                + self.config.start_frames * FRAME;
            while self.pre_roll.len() > keep {
                self.pre_roll.pop_front();
            }
            self.voiced_run = if voiced { self.voiced_run + 1 } else { 0 };
            if self.voiced_run >= self.config.start_frames {
                self.in_speech = true;
                self.utterance = self.pre_roll.drain(..).collect();
                self.speech_frames = self.voiced_run;
                self.silence_frames = 0;
                events.push(SegmentEvent::Started);
            }
            return;
        }
        self.utterance.extend_from_slice(frame);
        if voiced {
            self.speech_frames += 1;
            self.silence_frames = 0;
        } else {
            self.silence_frames += 1;
        }
        let silence_ms = self.silence_frames as u32 * FRAME_MS;
        let length_ms = (self.utterance.len() / FRAME) as u32 * FRAME_MS;
        if silence_ms >= self.config.end_silence_ms || length_ms >= self.config.max_utterance_ms {
            if let Some(utterance) = self.take_utterance() {
                events.push(SegmentEvent::Finished(utterance));
            }
            self.in_speech = false;
            self.voiced_run = 0;
            self.pre_roll.clear();
        }
    }

    /// The utterance without most of its trailing silence, or `None` if it
    /// was too short to be speech.
    fn take_utterance(&mut self) -> Option<Vec<f32>> {
        let mut utterance = std::mem::take(&mut self.utterance);
        let speech_ms = self.speech_frames as u32 * FRAME_MS;
        let silence_frames = self.silence_frames;
        self.speech_frames = 0;
        self.silence_frames = 0;
        if speech_ms < self.config.min_speech_ms {
            return None;
        }
        // Keep ~200 ms after the last voiced frame.
        let trailing = silence_frames.saturating_sub(12) * FRAME;
        utterance.truncate(utterance.len().saturating_sub(trailing));
        Some(utterance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Treats frames with an amplitude above 0.1 as speech.
    struct LoudIsSpeech;

    impl SpeechScorer for LoudIsSpeech {
        fn score(&mut self, frame: &[f32]) -> f32 {
            if frame.iter().any(|s| s.abs() > 0.1) {
                1.0
            } else {
                0.0
            }
        }
        fn reset(&mut self) {}
    }

    fn segmenter() -> Segmenter<LoudIsSpeech> {
        Segmenter::with_scorer(LoudIsSpeech, SegmenterConfig::default())
    }

    fn audio(ms: u32, loud: bool) -> Vec<f32> {
        vec![if loud { 0.5 } else { 0.0 }; (ms * 16) as usize]
    }

    #[test]
    fn an_utterance_starts_with_pre_roll_and_ends_after_silence() {
        let mut s = segmenter();
        assert!(s.push(&audio(1000, false)).is_empty());
        let events = s.push(&audio(100, true));
        assert_eq!(events, vec![SegmentEvent::Started]);
        assert!(s.in_speech());
        let mut events = s.push(&audio(600, true));
        events.extend(s.push(&audio(900, false)));
        let Some(SegmentEvent::Finished(utterance)) = events.last() else {
            panic!("no utterance: {events:?}");
        };
        // pre-roll (~320 ms + start frames) + 700 ms speech + ~200 ms tail
        let ms = utterance.len() / 16;
        assert!((1150..=1400).contains(&ms), "{ms} ms");
        assert!(!s.in_speech());
    }

    #[test]
    fn short_noises_are_dropped_and_long_speech_is_cut() {
        let mut s = segmenter();
        let mut events = s.push(&audio(100, true)); // 100 ms < 250 ms minimum
        events.extend(s.push(&audio(1000, false)));
        assert_eq!(events, vec![SegmentEvent::Started]);

        let config = SegmenterConfig {
            max_utterance_ms: 2000,
            ..SegmenterConfig::default()
        };
        let mut s = Segmenter::with_scorer(LoudIsSpeech, config);
        let events = s.push(&audio(5000, true));
        let finished = events
            .iter()
            .filter(|e| matches!(e, SegmentEvent::Finished(_)))
            .count();
        assert_eq!(finished, 2, "{} events", events.len());
    }

    #[test]
    fn flush_delivers_the_current_utterance() {
        let mut s = segmenter();
        s.push(&audio(500, true));
        assert!((s.level() - 0.5).abs() < 1e-6);
        let utterance = s.flush().expect("utterance");
        assert_eq!(utterance.len(), 500 * 16);
        assert!(s.flush().is_none());
        assert_eq!(s.level(), 0.0);
    }

    #[test]
    fn earshot_does_not_take_silence_or_hum_for_speech() {
        let mut s = Segmenter::new(SegmenterConfig::default());
        let hum: Vec<f32> = (0..16_000 * 2)
            .map(|i| (2.0 * std::f32::consts::PI * 50.0 * i as f32 / 16_000.0).sin() * 0.05)
            .collect();
        let mut events = s.push(&vec![0.0; 16_000]);
        events.extend(s.push(&hum));
        assert!(events.is_empty(), "{events:?}");
    }
}
