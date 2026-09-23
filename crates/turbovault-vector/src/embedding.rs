//! Text-to-vector embedding backends.

use crate::error::{Result, VectorError};

/// Backend that turns text into dense vectors.
///
/// A trait rather than a concrete type so a heavier backend (fastembed/ONNX,
/// say) can be added later behind its own Cargo feature without touching
/// anything that already calls this one. [`Model2VecEmbedder`] is the only
/// implementation this crate ships today; see the crate README for why.
#[async_trait::async_trait]
pub trait EmbeddingEngine: Send + Sync {
    /// Embed a batch of texts, one vector per input, in the same order.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// The dimensionality every vector this engine produces has.
    fn dimensions(&self) -> usize;
    /// A stable name for the loaded model, for status reporting.
    fn model_name(&self) -> &str;
}

/// Static-embedding backend using [`model2vec_rs`] (Potion / Model2Vec
/// models): a lookup table of per-token vectors, mean-pooled per input, with
/// no transformer forward pass. Pure Rust, CPU-only, and — because this crate
/// depends on it with `default-features = false, features = ["local-only"]`
/// — incapable of reaching the network; it only ever reads a local model
/// directory.
#[derive(Debug)]
pub struct Model2VecEmbedder {
    model: model2vec_rs::model::StaticModel,
    dims: usize,
    model_path: String,
}

impl Model2VecEmbedder {
    /// Load a Model2Vec model from a local directory (`tokenizer.json`,
    /// `model.safetensors`, `config.json`, in the layout `model2vec-rs`
    /// resolves on its own).
    pub fn load(model_path: &str) -> Result<Self> {
        if model_path.trim().is_empty() {
            return Err(VectorError::Config(
                "vector search model_path is not set; point it at a local Model2Vec model \
                 directory (see the turbovault-vector README for how to obtain one)"
                    .to_string(),
            ));
        }
        let model = model2vec_rs::model::StaticModel::from_pretrained(model_path, None, None, None)
            .map_err(|error| {
                VectorError::Embedding(format!("failed to load model at {model_path:?}: {error}"))
            })?;
        // model2vec-rs exposes no direct dimensions() accessor; the mean-pool
        // always produces a fixed-width vector regardless of token count, so
        // probing with an empty string is a cheap and always-valid way to
        // learn it (see model2vec_rs::model::StaticModel::pool_ids).
        let dims = model.encode_single("").len();
        if dims == 0 {
            return Err(VectorError::Embedding(format!(
                "model at {model_path:?} produced a zero-length embedding"
            )));
        }
        Ok(Self {
            model,
            dims,
            model_path: model_path.to_string(),
        })
    }
}

#[async_trait::async_trait]
impl EmbeddingEngine for Model2VecEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // StaticModel::encode is CPU-bound (tokenize + mean-pool over a
        // lookup table, no I/O) and StaticModel is cheap to clone (it shares
        // an Arc'd inner). Run it on the blocking pool so a large batch does
        // not stall the caller's async task.
        let model = self.model.clone();
        let owned = texts.to_vec();
        tokio::task::spawn_blocking(move || model.encode(&owned))
            .await
            .map_err(|error| VectorError::Embedding(format!("embedding task panicked: {error}")))
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn model_name(&self) -> &str {
        &self.model_path
    }
}

/// Test-only embedding backends: no model loading, no network, deterministic
/// output, so tests can exercise chunking, incremental re-embedding, and
/// hybrid search without a model file to hand.
#[cfg(any(test, feature = "test-util"))]
pub mod testing {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{EmbeddingEngine, Result};

    /// Hash-based embedder: the same text always produces the same vector,
    /// and different text (almost always) produces a different one, which is
    /// enough to exercise ranking and incremental re-embedding without any
    /// real semantics.
    pub struct DeterministicEmbedder {
        dims: usize,
    }

    impl DeterministicEmbedder {
        /// Construct an embedder producing `dims`-wide vectors.
        pub fn new(dims: usize) -> Self {
            Self { dims }
        }

        fn embed_one(&self, text: &str) -> Vec<f32> {
            // A simple rolling hash per dimension, then L2-normalized so
            // cosine similarity behaves the way a real embedding's would.
            let mut vector = vec![0.0f32; self.dims];
            let mut state: u64 = 1469598103934665603; // FNV offset basis
            for byte in text.bytes() {
                state ^= u64::from(byte);
                state = state.wrapping_mul(1099511628211); // FNV prime
                let slot = (state as usize) % self.dims;
                vector[slot] += 1.0;
            }
            let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
            if norm > 0.0 {
                for value in &mut vector {
                    *value /= norm;
                }
            }
            vector
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingEngine for DeterministicEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|text| self.embed_one(text)).collect())
        }

        fn dimensions(&self) -> usize {
            self.dims
        }

        fn model_name(&self) -> &str {
            "deterministic-test-embedder"
        }
    }

    /// Embedder that additionally counts how many texts it has embedded
    /// across its lifetime, so a test can assert that an incremental update
    /// re-embedded exactly the chunks it should have and nothing else.
    pub struct CountingEmbedder {
        inner: DeterministicEmbedder,
        count: Arc<AtomicUsize>,
    }

    impl CountingEmbedder {
        /// Construct a counting embedder producing `dims`-wide vectors.
        pub fn new(dims: usize) -> Self {
            Self {
                inner: DeterministicEmbedder::new(dims),
                count: Arc::new(AtomicUsize::new(0)),
            }
        }

        /// Number of texts embedded so far.
        pub fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingEngine for CountingEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.count.fetch_add(texts.len(), Ordering::SeqCst);
            self.inner.embed(texts).await
        }

        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }

        fn model_name(&self) -> &str {
            "counting-test-embedder"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::DeterministicEmbedder;
    use super::*;

    #[tokio::test]
    async fn deterministic_embedder_is_stable_and_normalized() {
        let embedder = DeterministicEmbedder::new(16);
        let first = embedder.embed(&["hello world".to_string()]).await.unwrap();
        let second = embedder.embed(&["hello world".to_string()]).await.unwrap();
        assert_eq!(first, second, "same text must embed to the same vector");

        let norm: f32 = first[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-4,
            "expected a unit vector, got norm {norm}"
        );
    }

    #[tokio::test]
    async fn deterministic_embedder_distinguishes_different_text() {
        let embedder = DeterministicEmbedder::new(32);
        let vectors = embedder
            .embed(&["alpha".to_string(), "beta".to_string()])
            .await
            .unwrap();
        assert_ne!(vectors[0], vectors[1]);
    }

    #[test]
    fn empty_model_path_is_a_config_error_not_a_panic() {
        let error = Model2VecEmbedder::load("").expect_err("empty path must be rejected");
        assert!(matches!(error, VectorError::Config(_)));
    }
}
