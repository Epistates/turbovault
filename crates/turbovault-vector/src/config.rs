//! Resolved vector-search configuration.
//!
//! Standalone: no dependency on a host's config crate. A caller starts from
//! [`VectorConfig::default`] and overrides fields from wherever its own
//! settings live (for the compiled-in plugin, that is a JSON file read
//! through `PluginStorage`).

use serde::{Deserialize, Serialize};

/// Resolved vector search configuration.
///
/// `model_path` has no default that resolves anywhere: it names a local
/// directory holding a Model2Vec model (tokenizer.json, model.safetensors,
/// config.json), and this crate never downloads one on its own. Leaving it
/// unset is a valid, deliberate default, not an oversight; a caller who wants
/// vector search working has to point it at a model they placed there
/// themselves. See the crate README for how to obtain one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VectorConfig {
    /// Local filesystem path to a Model2Vec model directory. Empty means
    /// "not configured"; [`crate::Model2VecEmbedder::load`] reports that as a
    /// config error rather than guessing or reaching the network.
    pub model_path: String,
    /// Maximum characters per chunk before a paragraph or sentence is
    /// hard-split. Matches [`crate::chunk::chunk_text`]'s `max_chars`.
    pub chunk_max_chars: usize,
    /// Characters of trailing context carried into the next chunk.
    pub chunk_overlap_chars: usize,
    /// Reciprocal Rank Fusion's `k` constant. Higher values flatten the
    /// influence of rank; 60 is the value the original RRF paper reports as
    /// robust across collections, and is what most hybrid-search
    /// implementations default to.
    pub rrf_k: f64,
    /// Weight given to the lexical (BM25) ranking in RRF, in `[0, 1]`. The
    /// dense ranking gets `1.0 - lexical_weight`.
    pub lexical_weight: f32,
    /// Multiply `limit` by this factor when pulling candidates from each side
    /// before fusing and truncating, so a note that is a middling lexical
    /// match but a strong dense one (or vice versa) is not dropped before
    /// fusion has a chance to rank it.
    pub search_overfetch_factor: usize,
    /// Minimum cosine similarity a dense candidate must clear to be considered.
    pub min_similarity: f32,
    /// HNSW `max_nb_connection` (graph degree per layer). Higher improves
    /// recall at the cost of memory and build time.
    pub ann_max_nb_connection: usize,
    /// HNSW `ef_construction`. Higher improves graph quality at the cost of
    /// build time.
    pub ann_ef_construction: usize,
    /// HNSW `ef_search`. Higher improves recall at the cost of query time.
    pub ann_ef_search: usize,
}

impl Default for VectorConfig {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            chunk_max_chars: 800,
            chunk_overlap_chars: 100,
            rrf_k: 60.0,
            lexical_weight: 0.4,
            search_overfetch_factor: 5,
            min_similarity: 0.2,
            ann_max_nb_connection: 16,
            ann_ef_construction: 200,
            ann_ef_search: 64,
        }
    }
}

impl VectorConfig {
    /// Whether a model path has been configured at all.
    pub fn is_configured(&self) -> bool {
        !self.model_path.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_unconfigured_but_otherwise_sane() {
        let config = VectorConfig::default();
        assert!(!config.is_configured());
        assert!(config.chunk_max_chars > config.chunk_overlap_chars);
        assert!((0.0..=1.0).contains(&config.lexical_weight));
    }

    #[test]
    fn round_trips_through_json() {
        let config = VectorConfig {
            model_path: "/models/potion-base-8m".to_string(),
            ..VectorConfig::default()
        };
        let encoded = serde_json::to_vec(&config).expect("serialize");
        let decoded: VectorConfig = serde_json::from_slice(&encoded).expect("deserialize");
        assert_eq!(decoded, config);
    }

    #[test]
    fn partial_json_fills_missing_fields_from_defaults() {
        let decoded: VectorConfig =
            serde_json::from_str(r#"{"model_path": "/models/x"}"#).expect("deserialize");
        assert_eq!(decoded.model_path, "/models/x");
        assert_eq!(
            decoded.chunk_max_chars,
            VectorConfig::default().chunk_max_chars
        );
    }
}
