//! Sample-rate conversion for speech: a windowed-sinc FIR resampler.
//!
//! Microphones deliver 44.1 or 48 kHz; speech recognition wants 16 kHz, and
//! synthesized speech (16–24 kHz) has to be played at the device rate. The
//! low-pass cut-off follows the lower of the two rates, so downsampling does
//! not alias. Quality is aimed at speech, not music.

use std::collections::VecDeque;
use std::f64::consts::PI;

/// Zero crossings of the sinc on each side of the centre tap.
const HALF_TAPS: usize = 16;

/// Streaming resampler with a fixed ratio.
#[derive(Debug, Clone)]
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Normalized cut-off (1.0 = the input Nyquist frequency).
    cutoff: f64,
    /// Half-width of the filter in input samples.
    width: usize,
    history: VecDeque<f32>,
    /// Position of the next output sample, relative to `history[0]`.
    position: f64,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        let from = f64::from(from_rate.max(1));
        let to = f64::from(to_rate.max(1));
        let cutoff = (to / from).min(1.0) * 0.95;
        let width = ((HALF_TAPS as f64) / cutoff).ceil() as usize;
        Self {
            step: from / to,
            cutoff,
            width,
            history: VecDeque::new(),
            // The first output sample is centred on the first input sample
            // once enough history (zero padding) precedes it.
            position: width as f64,
        }
        .primed()
    }

    fn primed(mut self) -> Self {
        self.history.extend(std::iter::repeat_n(0.0, self.width));
        self
    }

    /// Whether the rates are equal (no resampling needed).
    pub fn is_identity(&self) -> bool {
        (self.step - 1.0).abs() < f64::EPSILON
    }

    /// Resamples the next block of a stream.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.is_identity() {
            return input.to_vec();
        }
        self.history.extend(input.iter().copied());
        let mut output = Vec::with_capacity((input.len() as f64 / self.step) as usize + 1);
        while self.position + self.width as f64 + 1.0 <= self.history.len() as f64 {
            output.push(self.sample_at(self.position));
            self.position += self.step;
        }
        // Drop input that no future output needs.
        let consumed = (self.position as usize).saturating_sub(self.width);
        self.history.drain(..consumed.min(self.history.len()));
        self.position -= consumed as f64;
        output
    }

    /// Flushes the delayed tail (call once at the end of a clip).
    pub fn finish(&mut self) -> Vec<f32> {
        let padding = vec![0.0; self.width + 1];
        self.process(&padding)
    }

    fn sample_at(&self, position: f64) -> f32 {
        let centre = position.floor() as isize;
        let start = centre - self.width as isize + 1;
        let end = centre + self.width as isize;
        let mut sum = 0.0;
        for index in start..=end {
            let Some(&sample) = usize::try_from(index)
                .ok()
                .and_then(|i| self.history.get(i))
            else {
                continue;
            };
            let distance = position - index as f64;
            sum += f64::from(sample) * self.kernel(distance);
        }
        sum as f32
    }

    /// Blackman-windowed sinc low-pass, scaled to unity DC gain.
    fn kernel(&self, distance: f64) -> f64 {
        let x = distance * self.cutoff;
        let sinc = if x.abs() < 1e-9 {
            1.0
        } else {
            (PI * x).sin() / (PI * x)
        };
        let span = self.width as f64;
        let t = (distance / span).clamp(-1.0, 1.0);
        let window = 0.42 + 0.5 * (PI * t).cos() + 0.08 * (2.0 * PI * t).cos();
        self.cutoff * sinc * window
    }
}

/// Resamples a whole clip.
pub fn resample(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate {
        return input.to_vec();
    }
    let mut resampler = Resampler::new(from_rate, to_rate);
    let mut output = resampler.process(input);
    output.extend(resampler.finish());
    let expected =
        (input.len() as f64 * f64::from(to_rate) / f64::from(from_rate)).round() as usize;
    output.truncate(expected);
    output
}

/// Mixes interleaved frames down to mono.
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, seconds: f64) -> Vec<f32> {
        let count = (f64::from(rate) * seconds) as usize;
        (0..count)
            .map(|i| (2.0 * PI * freq * i as f64 / f64::from(rate)).sin() as f32 * 0.5)
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
    }

    /// Amplitude of `freq` in `samples` (single-bin DFT).
    fn tone_level(samples: &[f32], freq: f64, rate: u32) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &s) in samples.iter().enumerate() {
            let phase = 2.0 * PI * freq * i as f64 / f64::from(rate);
            re += f64::from(s) * phase.cos();
            im += f64::from(s) * phase.sin();
        }
        2.0 * (re * re + im * im).sqrt() / samples.len() as f64
    }

    #[test]
    fn speech_band_tones_survive_downsampling() {
        let input = sine(1000.0, 48_000, 0.5);
        let output = resample(&input, 48_000, 16_000);
        assert_eq!(output.len(), 8_000);
        let body = &output[1000..7000];
        let level = tone_level(body, 1000.0, 16_000);
        assert!((level - 0.5).abs() < 0.03, "level {level}");
    }

    #[test]
    fn frequencies_above_the_new_nyquist_are_removed() {
        // 12 kHz cannot exist at 16 kHz; without filtering it would alias to 4 kHz.
        let input = sine(12_000.0, 48_000, 0.5);
        let output = resample(&input, 48_000, 16_000);
        let body = &output[1000..7000];
        assert!(rms(body) < 0.01, "rms {}", rms(body));
        assert!(tone_level(body, 4000.0, 16_000) < 0.01);
    }

    #[test]
    fn non_integer_ratios_and_upsampling() {
        let input = sine(440.0, 44_100, 0.5);
        let down = resample(&input, 44_100, 16_000);
        assert_eq!(down.len(), 8_000);
        assert!((tone_level(&down[1000..7000], 440.0, 16_000) - 0.5).abs() < 0.03);

        let voice = sine(300.0, 22_050, 0.4);
        let up = resample(&voice, 22_050, 48_000);
        assert_eq!(
            up.len(),
            (voice.len() as f64 * 48_000.0 / 22_050.0).round() as usize
        );
        let body = &up[2000..up.len() - 2000];
        assert!((tone_level(body, 300.0, 48_000) - 0.5).abs() < 0.03);
    }

    #[test]
    fn streaming_matches_whole_clip_processing() {
        let input = sine(700.0, 48_000, 0.3);
        let whole = resample(&input, 48_000, 16_000);
        let mut resampler = Resampler::new(48_000, 16_000);
        let mut streamed = Vec::new();
        for chunk in input.chunks(480) {
            streamed.extend(resampler.process(chunk));
        }
        streamed.extend(resampler.finish());
        streamed.truncate(whole.len());
        assert_eq!(streamed.len(), whole.len());
        for (a, b) in streamed.iter().zip(&whole) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn identity_and_downmix() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample(&input, 16_000, 16_000), input);
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(downmix(&[0.3, 0.4], 1), vec![0.3, 0.4]);
    }
}
