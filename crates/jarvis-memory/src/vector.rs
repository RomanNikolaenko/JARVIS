//! Embedding vectors: storage encoding, brute-force similarity search and
//! the cuts that decide which hits match.
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

/// Which hits of a similarity search count as matches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimilarityCut {
    /// Below this cosine similarity nothing matches, so a query about
    /// something memory does not hold finds nothing.
    pub min: f32,
    /// Hits more than this below the best hit are dropped: the tail of
    /// texts that only share the query's style or a vague topic.
    pub margin: f32,
}

impl SimilarityCut {
    /// The hits that pass, in their order. The margin is measured from the
    /// best of `hits`, so filter out hits that cannot be used first.
    pub fn apply(self, hits: Vec<(i64, f32)>) -> Vec<(i64, f32)> {
        let best = hits
            .iter()
            .map(|(_, score)| *score)
            .fold(f32::NEG_INFINITY, f32::max);
        hits.into_iter()
            .filter(|(_, score)| *score >= self.min && *score >= best - self.margin)
            .collect()
    }
}

/// Cuts of one embedding model. Cosine similarities are not comparable
/// across models: each has its own level for unrelated texts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimilarityCuts {
    /// Search on the Memory screen: a ranked list the user reads.
    pub search: SimilarityCut,
    /// Facts and excerpts added to a chat's context: stricter, because a
    /// small model cannot tell an unrelated fact from a relevant one.
    pub recall: SimilarityCut,
}

/// Measured with BGE-M3 Q8_0 on Ukrainian and English facts (ADR 0009):
/// unrelated facts score up to 0.61 (median 0.39).
const BGE_M3_CUTS: SimilarityCuts = SimilarityCuts {
    search: SimilarityCut {
        min: 0.52,
        margin: 0.1,
    },
    recall: SimilarityCut {
        min: 0.55,
        margin: 0.1,
    },
};

/// Models that have not been measured keep the floors used before
/// calibration; the margin trims the tail below the best hit.
const DEFAULT_CUTS: SimilarityCuts = SimilarityCuts {
    search: SimilarityCut {
        min: 0.35,
        margin: 0.1,
    },
    recall: SimilarityCut {
        min: 0.5,
        margin: 0.1,
    },
};

impl SimilarityCuts {
    /// Cuts for an embedding model key (`provider/model`).
    pub fn for_model(model_key: &str) -> Self {
        if model_key.to_ascii_lowercase().contains("bge-m3") {
            BGE_M3_CUTS
        } else {
            DEFAULT_CUTS
        }
    }
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

    pub fn contains(&self, kind: VectorKind, id: i64) -> bool {
        self.vectors.contains_key(&(kind, id))
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

    /// Facts with these cosine similarities to the query `[1, 0]` (ids from
    /// 1, best first): the ids that pass `cut`.
    fn matches(scores: &[f32], cut: SimilarityCut) -> Vec<i64> {
        let mut index = VectorIndex::new("test");
        for (id, score) in (1..).zip(scores) {
            index.insert(
                VectorKind::Fact,
                id,
                vec![*score, (1.0 - score * score).sqrt()],
            );
        }
        cut.apply(index.search(VectorKind::Fact, &[1.0, 0.0], 20))
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    #[test]
    fn bge_m3_cuts_keep_relevant_facts_and_drop_unrelated_ones() {
        let cuts = SimilarityCuts::for_model("jarvis-runtime-embeddings/bge-m3-q8_0");
        // Similarities measured with BGE-M3 Q8_0 (ADR 0009); the first ids
        // are the relevant facts.
        // "What is my dog's name?": the dog fact, then unrelated facts, the
        // first of them above the floor but far below the best hit.
        let dog = [0.668, 0.544, 0.501, 0.483, 0.474];
        assert_eq!(matches(&dog, cuts.search), vec![1]);
        assert_eq!(matches(&dog, cuts.recall), vec![1]);
        // "Ким я працюю?" finds the English job fact; unrelated facts follow
        // closely, but below the floor.
        let job = [0.574, 0.514, 0.511, 0.508, 0.505];
        assert_eq!(matches(&job, cuts.search), vec![1]);
        assert_eq!(matches(&job, cuts.recall), vec![1]);
        // "алергія": a Ukrainian and an English allergy fact, both kept.
        let allergy = [0.720, 0.630, 0.466, 0.426];
        assert_eq!(matches(&allergy, cuts.search), vec![1, 2]);
        assert_eq!(matches(&allergy, cuts.recall), vec![1, 2]);
        // "Яка погода завтра у Львові?": memory holds nothing about it.
        let weather = [0.461, 0.429, 0.408, 0.406];
        assert!(matches(&weather, cuts.search).is_empty());
        assert!(matches(&weather, cuts.recall).is_empty());
        // A weak match is listed by search but not added to a chat.
        assert_eq!(matches(&[0.53, 0.41], cuts.search), vec![1]);
        assert!(matches(&[0.53, 0.41], cuts.recall).is_empty());
    }

    #[test]
    fn cuts_depend_on_the_embedding_model() {
        assert_eq!(
            SimilarityCuts::for_model("ollama/BGE-M3:latest"),
            BGE_M3_CUTS
        );
        assert_eq!(
            SimilarityCuts::for_model("ollama/nomic-embed-text:latest"),
            DEFAULT_CUTS
        );
        // Unmeasured models keep the old floors, plus the margin.
        let cuts = SimilarityCuts::for_model("ollama/nomic-embed-text:latest");
        assert_eq!(matches(&[0.8, 0.65, 0.4], cuts.search), vec![1]);
        assert_eq!(matches(&[0.48, 0.42, 0.4], cuts.search), vec![1, 2, 3]);
        assert!(matches(&[0.48, 0.42, 0.4], cuts.recall).is_empty());
    }

    #[test]
    fn the_margin_is_measured_from_the_best_remaining_hit() {
        let cut = DEFAULT_CUTS.recall;
        assert!(cut.apply(Vec::new()).is_empty());
        let hits = vec![(1, 0.9), (2, 0.7), (3, 0.65), (4, 0.45)];
        assert_eq!(cut.apply(hits.clone()), vec![(1, 0.9)]);
        // Without the best hit (e.g. a fact of another project), the next
        // ones are measured against each other.
        let usable = hits.into_iter().filter(|(id, _)| *id != 1).collect();
        assert_eq!(cut.apply(usable), vec![(2, 0.7), (3, 0.65)]);
    }
}
