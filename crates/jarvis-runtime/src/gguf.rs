//! Minimal GGUF metadata reader.
//!
//! Only the header and key/value metadata are read; tensors are not touched.
//! Model files are untrusted input, so every length is bounded.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

use crate::error::{Result, RuntimeError};
use crate::types::{GgufInfo, MemoryEstimate, PoolingType};

const MAGIC: &[u8; 4] = b"GGUF";
const MAX_KV_COUNT: u64 = 100_000;
const MAX_KEY_LEN: u64 = 64 * 1024;
const MAX_STRING_LEN: u64 = 16 * 1024 * 1024;
const MAX_ARRAY_LEN: u64 = 50_000_000;
const MAX_NESTING: u32 = 2;

/// Value types of the GGUF format.
mod ty {
    pub const U8: u32 = 0;
    pub const I8: u32 = 1;
    pub const U16: u32 = 2;
    pub const I16: u32 = 3;
    pub const U32: u32 = 4;
    pub const I32: u32 = 5;
    pub const F32: u32 = 6;
    pub const BOOL: u32 = 7;
    pub const STRING: u32 = 8;
    pub const ARRAY: u32 = 9;
    pub const U64: u32 = 10;
    pub const I64: u32 = 11;
    pub const F64: u32 = 12;
}

#[derive(Debug, Clone, PartialEq)]
enum Scalar {
    Int(i128),
    Text(String),
    Other,
}

/// Reads metadata from a GGUF file. Blocking.
pub fn read_gguf_info(path: &Path) -> Result<GgufInfo> {
    let file = File::open(path)
        .map_err(|err| RuntimeError::io(format!("opening {}", path.display()), err))?;
    parse(BufReader::new(file))
}

pub(crate) fn parse<R: Read>(mut reader: R) -> Result<GgufInfo> {
    let invalid = |message: &str| RuntimeError::InvalidModel(message.to_owned());

    let mut magic = [0u8; 4];
    reader
        .read_exact(&mut magic)
        .map_err(|_| invalid("file is too short"))?;
    if &magic != MAGIC {
        return Err(invalid("missing GGUF signature"));
    }
    let version = read_u32(&mut reader)?;
    if !(2..=3).contains(&version) {
        return Err(invalid(&format!("unsupported GGUF version {version}")));
    }
    let _tensor_count = read_u64(&mut reader)?;
    let kv_count = read_u64(&mut reader)?;
    if kv_count > MAX_KV_COUNT {
        return Err(invalid("too many metadata entries"));
    }

    let mut values: HashMap<String, Scalar> = HashMap::new();
    for _ in 0..kv_count {
        let key = read_string(&mut reader, MAX_KEY_LEN)?;
        let value_type = read_u32(&mut reader)?;
        let value = read_value(&mut reader, value_type, 0)?;
        if wanted(&key) {
            values.insert(key, value);
        }
    }

    let text = |key: &str| match values.get(key) {
        Some(Scalar::Text(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => None,
    };
    let int = |key: &str| match values.get(key) {
        Some(Scalar::Int(n)) => u32::try_from(*n).ok(),
        _ => None,
    };
    let architecture = text("general.architecture");
    let arch_int = |suffix: &str| {
        architecture
            .as_deref()
            .and_then(|arch| int(&format!("{arch}.{suffix}")))
    };

    Ok(GgufInfo {
        name: text("general.name"),
        size_label: text("general.size_label"),
        quantization: int("general.file_type").map(file_type_name),
        context_length: arch_int("context_length"),
        block_count: arch_int("block_count"),
        embedding_length: arch_int("embedding_length"),
        head_count: arch_int("attention.head_count"),
        head_count_kv: arch_int("attention.head_count_kv"),
        pooling: arch_int("pooling_type").and_then(PoolingType::from_gguf),
        architecture,
    })
}

fn wanted(key: &str) -> bool {
    matches!(
        key,
        "general.architecture" | "general.name" | "general.size_label" | "general.file_type"
    ) || [
        ".context_length",
        ".block_count",
        ".embedding_length",
        ".attention.head_count",
        ".attention.head_count_kv",
        ".pooling_type",
    ]
    .iter()
    .any(|suffix| key.ends_with(suffix))
}

fn read_value<R: Read>(reader: &mut R, value_type: u32, depth: u32) -> Result<Scalar> {
    Ok(match value_type {
        ty::U8 => Scalar::Int(read_array::<1, R>(reader)?[0].into()),
        ty::I8 => Scalar::Int(i8::from_le_bytes(read_array::<1, R>(reader)?).into()),
        ty::U16 => Scalar::Int(u16::from_le_bytes(read_array::<2, R>(reader)?).into()),
        ty::I16 => Scalar::Int(i16::from_le_bytes(read_array::<2, R>(reader)?).into()),
        ty::U32 => Scalar::Int(read_u32(reader)?.into()),
        ty::I32 => Scalar::Int(i32::from_le_bytes(read_array::<4, R>(reader)?).into()),
        ty::U64 => Scalar::Int(read_u64(reader)?.into()),
        ty::I64 => Scalar::Int(i64::from_le_bytes(read_array::<8, R>(reader)?).into()),
        ty::F32 | ty::BOOL | ty::F64 => {
            let size = match value_type {
                ty::F32 => 4,
                ty::BOOL => 1,
                _ => 8,
            };
            skip(reader, size)?;
            Scalar::Other
        }
        ty::STRING => Scalar::Text(read_string(reader, MAX_STRING_LEN)?),
        ty::ARRAY => {
            if depth >= MAX_NESTING {
                return Err(RuntimeError::InvalidModel(
                    "arrays nested too deeply".into(),
                ));
            }
            let item_type = read_u32(reader)?;
            let len = read_u64(reader)?;
            if len > MAX_ARRAY_LEN {
                return Err(RuntimeError::InvalidModel("metadata array too long".into()));
            }
            match fixed_size(item_type) {
                Some(size) => skip(reader, size * len)?,
                None => {
                    for _ in 0..len {
                        read_value(reader, item_type, depth + 1)?;
                    }
                }
            }
            Scalar::Other
        }
        other => {
            return Err(RuntimeError::InvalidModel(format!(
                "unknown metadata type {other}"
            )));
        }
    })
}

fn fixed_size(value_type: u32) -> Option<u64> {
    match value_type {
        ty::U8 | ty::I8 | ty::BOOL => Some(1),
        ty::U16 | ty::I16 => Some(2),
        ty::U32 | ty::I32 | ty::F32 => Some(4),
        ty::U64 | ty::I64 | ty::F64 => Some(8),
        _ => None,
    }
}

fn read_array<const N: usize, R: Read>(reader: &mut R) -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    reader
        .read_exact(&mut buf)
        .map_err(|_| RuntimeError::InvalidModel("unexpected end of file".into()))?;
    Ok(buf)
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32> {
    Ok(u32::from_le_bytes(read_array::<4, R>(reader)?))
}

fn read_u64<R: Read>(reader: &mut R) -> Result<u64> {
    Ok(u64::from_le_bytes(read_array::<8, R>(reader)?))
}

fn read_string<R: Read>(reader: &mut R, max: u64) -> Result<String> {
    let len = read_u64(reader)?;
    if len > max {
        return Err(RuntimeError::InvalidModel(
            "metadata string too long".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(len.min(4096)).unwrap_or(4096));
    reader
        .take(len)
        .read_to_end(&mut bytes)
        .map_err(|_| RuntimeError::InvalidModel("unexpected end of file".into()))?;
    if bytes.len() as u64 != len {
        return Err(RuntimeError::InvalidModel("unexpected end of file".into()));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn skip<R: Read>(reader: &mut R, len: u64) -> Result<()> {
    let copied = io::copy(&mut reader.take(len), &mut io::sink())
        .map_err(|_| RuntimeError::InvalidModel("unexpected end of file".into()))?;
    if copied != len {
        return Err(RuntimeError::InvalidModel("unexpected end of file".into()));
    }
    Ok(())
}

/// Name of a `general.file_type` (llama.cpp `llama_ftype`).
fn file_type_name(value: u32) -> String {
    let name = match value {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        7 => "Q8_0",
        8 => "Q5_0",
        9 => "Q5_1",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        19 => "IQ2_XXS",
        20 => "IQ2_XS",
        21 => "Q2_K_S",
        22 => "IQ3_XS",
        23 => "IQ3_XXS",
        24 => "IQ1_S",
        25 => "IQ4_NL",
        26 => "IQ3_S",
        27 => "IQ3_M",
        28 => "IQ2_S",
        29 => "IQ2_M",
        30 => "IQ4_XS",
        31 => "IQ1_M",
        32 => "BF16",
        36 => "TQ1_0",
        37 => "TQ2_0",
        38 => "MXFP4",
        other => return format!("type {other}"),
    };
    name.to_owned()
}

const OVERHEAD_BASE: u64 = 384 * 1024 * 1024;

/// Rough memory needed to run a model with `context` tokens: weights + f16
/// KV cache + compute buffers. Errs on the safe side.
pub fn estimate_memory(model_bytes: u64, info: &GgufInfo, context: u32) -> MemoryEstimate {
    let kv_cache_bytes = match (info.block_count, info.embedding_length, info.head_count) {
        (Some(layers), Some(embedding), Some(heads)) if heads > 0 => {
            let head_dim = u64::from(embedding) / u64::from(heads);
            let kv_heads = u64::from(info.head_count_kv.unwrap_or(heads));
            // K and V, f16 (2 bytes) per element.
            2 * u64::from(layers) * u64::from(context) * kv_heads * head_dim * 2
        }
        // Unknown architecture: assume a tenth of the weights.
        _ => model_bytes / 10,
    };
    let overhead_bytes = OVERHEAD_BASE + model_bytes / 20;
    MemoryEstimate {
        model_bytes,
        kv_cache_bytes,
        overhead_bytes,
        total_bytes: model_bytes + kv_cache_bytes + overhead_bytes,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a GGUF header for tests.
    pub(crate) fn sample_gguf(extra_kv: &[(&str, u32, Vec<u8>)]) -> Vec<u8> {
        fn string(s: &str) -> Vec<u8> {
            let mut out = (s.len() as u64).to_le_bytes().to_vec();
            out.extend_from_slice(s.as_bytes());
            out
        }
        let mut kvs: Vec<(String, u32, Vec<u8>)> = vec![
            ("general.architecture".into(), ty::STRING, string("qwen2")),
            (
                "general.name".into(),
                ty::STRING,
                string("Qwen2.5 0.5B Instruct"),
            ),
            ("general.size_label".into(), ty::STRING, string("0.5B")),
            (
                "general.file_type".into(),
                ty::U32,
                15u32.to_le_bytes().to_vec(),
            ),
            (
                "qwen2.context_length".into(),
                ty::U32,
                32_768u32.to_le_bytes().to_vec(),
            ),
            (
                "qwen2.block_count".into(),
                ty::U32,
                24u32.to_le_bytes().to_vec(),
            ),
            (
                "qwen2.embedding_length".into(),
                ty::U32,
                896u32.to_le_bytes().to_vec(),
            ),
            (
                "qwen2.attention.head_count".into(),
                ty::U32,
                14u32.to_le_bytes().to_vec(),
            ),
            (
                "qwen2.attention.head_count_kv".into(),
                ty::U32,
                2u32.to_le_bytes().to_vec(),
            ),
            (
                "general.rope_freq".into(),
                ty::F32,
                1.0f32.to_le_bytes().to_vec(),
            ),
        ];
        // A string array (like tokenizer tokens) that must be skipped.
        let mut tokens = ty::STRING.to_le_bytes().to_vec();
        tokens.extend_from_slice(&3u64.to_le_bytes());
        for token in ["<s>", "hello", "привіт"] {
            tokens.extend(string(token));
        }
        kvs.push(("tokenizer.ggml.tokens".into(), ty::ARRAY, tokens));
        // A numeric array skipped by length.
        let mut scores = ty::F32.to_le_bytes().to_vec();
        scores.extend_from_slice(&3u64.to_le_bytes());
        scores.extend_from_slice(&[0u8; 12]);
        kvs.push(("tokenizer.ggml.scores".into(), ty::ARRAY, scores));
        for (key, value_type, value) in extra_kv {
            kvs.push(((*key).to_owned(), *value_type, value.clone()));
        }

        let mut out = b"GGUF".to_vec();
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&(kvs.len() as u64).to_le_bytes());
        for (key, value_type, value) in kvs {
            out.extend(string(&key));
            out.extend_from_slice(&value_type.to_le_bytes());
            out.extend(value);
        }
        out
    }

    #[test]
    fn reads_model_metadata() {
        let info = parse(sample_gguf(&[]).as_slice()).expect("valid gguf");
        assert_eq!(info.architecture.as_deref(), Some("qwen2"));
        assert_eq!(info.name.as_deref(), Some("Qwen2.5 0.5B Instruct"));
        assert_eq!(info.size_label.as_deref(), Some("0.5B"));
        assert_eq!(info.quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(info.context_length, Some(32_768));
        assert_eq!(info.block_count, Some(24));
        assert_eq!(info.head_count_kv, Some(2));
        assert_eq!(info.pooling, None, "chat models declare no pooling");

        // Embedding models (e.g. bge-m3: `bert.pooling_type = 2`).
        let pooled = |value: u32| {
            let bytes =
                sample_gguf(&[("qwen2.pooling_type", ty::U32, value.to_le_bytes().to_vec())]);
            parse(bytes.as_slice()).expect("valid gguf").pooling
        };
        assert_eq!(pooled(2), Some(PoolingType::Cls));
        assert_eq!(pooled(1), Some(PoolingType::Mean));
        assert_eq!(pooled(4), Some(PoolingType::Rank));
        assert_eq!(pooled(77), None);
    }

    #[test]
    fn rejects_invalid_files() {
        assert!(matches!(
            parse(&b"GGML0000"[..]),
            Err(RuntimeError::InvalidModel(_))
        ));
        assert!(matches!(
            parse(&b"GG"[..]),
            Err(RuntimeError::InvalidModel(_))
        ));

        let mut old = sample_gguf(&[]);
        old[4..8].copy_from_slice(&1u32.to_le_bytes());
        assert!(parse(old.as_slice()).is_err());

        let truncated = sample_gguf(&[]);
        assert!(parse(&truncated[..truncated.len() - 5]).is_err());

        let mut huge = b"GGUF".to_vec();
        huge.extend_from_slice(&3u32.to_le_bytes());
        huge.extend_from_slice(&0u64.to_le_bytes());
        huge.extend_from_slice(&1u64.to_le_bytes());
        huge.extend_from_slice(&(MAX_KEY_LEN + 1).to_le_bytes());
        assert!(parse(huge.as_slice()).is_err());
    }

    #[test]
    fn estimates_memory_from_architecture() {
        let info = parse(sample_gguf(&[]).as_slice()).expect("valid gguf");
        let estimate = estimate_memory(491_400_032, &info, 4096);
        // 2 (K,V) * 24 layers * 4096 ctx * 2 kv heads * 64 head dim * 2 bytes
        assert_eq!(estimate.kv_cache_bytes, 2 * 24 * 4096 * 2 * 64 * 2);
        assert!(estimate.total_bytes > estimate.model_bytes + estimate.kv_cache_bytes);

        let unknown = estimate_memory(1_000_000_000, &GgufInfo::default(), 4096);
        assert_eq!(unknown.kv_cache_bytes, 100_000_000);
    }
}
