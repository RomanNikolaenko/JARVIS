//! Minimal WAV (RIFF) encoding and decoding: 16-bit PCM out, 16/32-bit PCM
//! and 32-bit float in. Enough for whisper.cpp input and sherpa-onnx output.

use crate::error::{Result, VoiceError};

/// Encodes mono samples in [-1, 1] as 16-bit PCM WAV.
pub fn encode_pcm16(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Decoded audio, mixed down to mono.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Decodes a WAV file (PCM 16/32 bit or IEEE float 32 bit, any channel count).
pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    let invalid = |why: &str| VoiceError::InvalidAudio(why.to_owned());
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(invalid("not a WAV file"));
    }
    let mut offset = 12;
    let mut format: Option<(u16, u16, u32, u16)> = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        let body_start = offset + 8;
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = &bytes[body_start..body_end];
        match id {
            b"fmt " => {
                if body.len() < 16 {
                    return Err(invalid("short fmt chunk"));
                }
                let tag = u16::from_le_bytes([body[0], body[1]]);
                let channels = u16::from_le_bytes([body[2], body[3]]);
                let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                let bits = u16::from_le_bytes([body[14], body[15]]);
                // WAVE_FORMAT_EXTENSIBLE: the real tag is in the sub-format GUID.
                let tag = if tag == 0xFFFE && body.len() >= 26 {
                    u16::from_le_bytes([body[24], body[25]])
                } else {
                    tag
                };
                format = Some((tag, channels, rate, bits));
            }
            b"data" => {
                let (tag, channels, rate, bits) =
                    format.ok_or_else(|| invalid("data before fmt"))?;
                if channels == 0 || rate == 0 {
                    return Err(invalid("zero channels or sample rate"));
                }
                let interleaved: Vec<f32> = match (tag, bits) {
                    (1, 16) => body
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0)
                        .collect(),
                    (1, 32) => body
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| i32::from_le_bytes(*b) as f32 / 2_147_483_648.0)
                        .collect(),
                    (3, 32) => body
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b))
                        .collect(),
                    _ => return Err(invalid("unsupported sample format")),
                };
                return Ok(Decoded {
                    samples: crate::resample::downmix(&interleaved, usize::from(channels)),
                    sample_rate: rate,
                });
            }
            _ => {}
        }
        // Chunks are padded to an even size.
        offset = body_start.saturating_add(size + (size & 1));
    }
    Err(invalid("no data chunk"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm16_roundtrip() {
        let samples = vec![0.0, 0.5, -0.5, 1.0, -1.0];
        let bytes = encode_pcm16(&samples, 16_000);
        assert_eq!(bytes.len(), 44 + samples.len() * 2);
        let decoded = decode(&bytes).expect("decode");
        assert_eq!(decoded.sample_rate, 16_000);
        for (a, b) in decoded.samples.iter().zip(&samples) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn float_stereo_with_extra_chunks() {
        let mut bytes = Vec::new();
        let frames: [f32; 4] = [0.2, 0.4, -0.2, -0.4];
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"LIST");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&[1, 2, 3, 0]); // odd size + padding byte
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&22_050u32.to_le_bytes());
        bytes.extend_from_slice(&(22_050u32 * 8).to_le_bytes());
        bytes.extend_from_slice(&8u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        for f in frames {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        let decoded = decode(&bytes).expect("decode");
        assert_eq!(decoded.sample_rate, 22_050);
        assert_eq!(decoded.samples.len(), 2);
        assert!((decoded.samples[0] - 0.3).abs() < 1e-6);
        assert!((decoded.samples[1] + 0.3).abs() < 1e-6);
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(decode(b"not audio").is_err());
        assert!(decode(b"RIFF\0\0\0\0WAVE").is_err());
    }
}
