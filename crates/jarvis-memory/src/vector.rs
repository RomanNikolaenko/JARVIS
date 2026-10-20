//! Embedding vectors: storage encoding and brute-force similarity search.
//!
//! Vectors are L2-normalized when stored, so cosine similarity is a dot
//! product. Search scans all vectors of one embedding model in memory, which
//! is fast enough for tens of thousands of items on a desktop; an
//! approximate index can replace it behind the same interface.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::error::MemoryError;

/// Produces embedding vectors for texts.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Identifies the vector space (e.g. `ollama/nomic-embed-text`): vectors
    /// with different keys are never compared.
    fn model_key(&self) -> &str;

    /// One vector per text, in order.
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, MemoryError>;
}

/// Kinds of items that carry embeddings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VectorKind {
    Message,
    Fact,
}

impl VectorKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Fact => "fact",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "message" => Some(Self::Message),
            "fact" => Some(Self::Fact),
            _ => None,
        }
    }
}

/// Scales `v` to unit length. Returns `false` for a zero or non-finite
/// vector, which cannot be compared.
pub fn normalize(v: &mut [f32]) -> bool {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return false;
    }
    v.iter_mut().for_each(|x| *x /= norm);
    true
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Little-endian `f32` bytes.
pub fn encode(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn decode(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let (chunks, _) = bytes.as_chunks::<4>();
    Some(chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

/// In-memory vectors of one embedding model.
#[derive(Debug, Default)]
pub struct VectorIndex {
    model_key: String,
    vectors: HashMap<(VectorKind, i64), Vec<f32>>,
}

impl VectorIndex {
    pub fn new(model_key: impl Into<String>) -> Self {
        Self {
            model_key: model_key.into(),
            vectors: HashMap::new(),
        }
    }

    pub fn model_key(&self) -> &str {
        &self.model_key
    }

    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    /// Adds a normalized vector.
    pub fn insert(&mut self, kind: VectorKind, id: i64, vector: Vec<f32>) {
        self.vectors.insert((kind, id), vector);
    }

    pub fn remove(&mut self, kind: VectorKind, id: i64) {
        self.vectors.remove(&(kind, id));
    }

    /// Most similar items of `kind`, best first, with cosine similarity.
    pub fn search(&self, kind: VectorKind, query: &[f32], limit: usize) -> Vec<(i64, f32)> {
        let mut scored: Vec<(i64, f32)> = self
            .vectors
            .iter()
            .filter(|((k, _), v)| *k == kind && v.len() == query.len())
            .map(|((_, id), v)| (*id, dot(v, query)))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(limit);
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_round_trip_and_rank_by_cosine() {
        let mut v = vec![3.0, 4.0];
        assert!(normalize(&mut v));
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert_eq!(decode(&encode(&v)), Some(v.clone()));
        assert_eq!(decode(&[1, 2, 3]), None);
        assert!(!normalize(&mut [0.0, 0.0]));
        assert!(!normalize(&mut [f32::NAN, 1.0]));

        let mut index = VectorIndex::new("test");
        index.insert(VectorKind::Fact, 1, vec![1.0, 0.0]);
        index.insert(VectorKind::Fact, 2, vec![0.0, 1.0]);
        index.insert(VectorKind::Message, 3, vec![1.0, 0.0]);
        index.insert(VectorKind::Fact, 4, vec![1.0, 0.0, 0.0]);
        let hits = index.search(VectorKind::Fact, &[0.8, 0.6], 5);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), vec![1, 2]);
        assert!((hits[0].1 - 0.8).abs() < 1e-6);
        index.remove(VectorKind::Fact, 1);
        assert_eq!(index.search(VectorKind::Fact, &[1.0, 0.0], 1)[0].0, 2);
    }
}
