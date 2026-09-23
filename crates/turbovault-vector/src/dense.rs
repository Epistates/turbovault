//! Dense (ANN) side of hybrid search: an in-memory HNSW graph over chunk
//! embeddings, backed by [`hnsw_rs`].
//!
//! `hnsw_rs`'s `Hnsw` has no point-removal API (confirmed against 0.3.4's
//! public surface; upstream tracks this as a known gap, unlike `usearch`,
//! which the prototype this crate is based on used and which does support
//! removal via tombstoning). This index therefore keeps the actual vectors in
//! a plain map as the source of truth and treats the HNSW graph itself as a
//! disposable, rebuildable cache: an upsert or a removal marks the graph
//! dirty, and the next search rebuilds it from the current vector set before
//! querying. For a vault-sized corpus (thousands of chunks, not millions) a
//! full rebuild is milliseconds to low seconds, and it is paid at most once
//! per batch of changes rather than once per change, since the engine
//! reconciles a whole batch of edited notes before the next search.

use std::collections::HashMap;

use hnsw_rs::prelude::{DistCosine, Hnsw};

use crate::error::{Result, VectorError};

/// Number of hierarchical layers `hnsw_rs` allocates for the graph. Not
/// exposed as config: this bounds table allocation, not search quality, and
/// the library's own defaults comfortably cover a vault-sized corpus.
const MAX_LAYER: usize = 16;

/// Dense nearest-neighbour index over chunk embeddings.
pub struct DenseIndex {
    vectors: HashMap<u64, Vec<f32>>,
    graph: Option<Hnsw<'static, f32, DistCosine>>,
    dirty: bool,
    dims: usize,
    max_nb_connection: usize,
    ef_construction: usize,
    ef_search: usize,
}

impl DenseIndex {
    /// Construct an empty index for `dims`-wide vectors.
    pub fn new(
        dims: usize,
        max_nb_connection: usize,
        ef_construction: usize,
        ef_search: usize,
    ) -> Self {
        Self {
            vectors: HashMap::new(),
            graph: None,
            dirty: false,
            dims,
            max_nb_connection,
            ef_construction,
            ef_search,
        }
    }

    /// Insert or replace a chunk's vector. Marks the graph stale.
    pub fn upsert(&mut self, id: u64, vector: Vec<f32>) -> Result<()> {
        if vector.len() != self.dims {
            return Err(VectorError::Index(format!(
                "vector for chunk {id} has {} dimensions, expected {}",
                vector.len(),
                self.dims
            )));
        }
        self.vectors.insert(id, vector);
        self.dirty = true;
        Ok(())
    }

    /// Remove a chunk's vector, if present. Marks the graph stale.
    pub fn remove(&mut self, id: u64) {
        if self.vectors.remove(&id).is_some() {
            self.dirty = true;
        }
    }

    /// Number of vectors currently indexed.
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// Whether the index holds no vectors.
    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    fn rebuild_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        if self.vectors.is_empty() {
            self.graph = None;
        } else {
            let graph = Hnsw::<f32, DistCosine>::new(
                self.max_nb_connection,
                self.vectors.len(),
                MAX_LAYER,
                self.ef_construction,
                DistCosine {},
            );
            for (id, vector) in &self.vectors {
                graph.insert((vector.as_slice(), *id as usize));
            }
            self.graph = Some(graph);
        }
        self.dirty = false;
    }

    /// Search for the `top_k` nearest chunks to `query`, rebuilding the graph
    /// first if it is stale. Returns `(chunk_id, similarity)` pairs, higher
    /// similarity first; similarity is `1.0 - cosine_distance`, so `1.0` is
    /// an exact match.
    pub fn search(&mut self, query: &[f32], top_k: usize) -> Result<Vec<(u64, f32)>> {
        if query.len() != self.dims {
            return Err(VectorError::Index(format!(
                "query vector has {} dimensions, expected {}",
                query.len(),
                self.dims
            )));
        }
        self.rebuild_if_dirty();
        let Some(graph) = &self.graph else {
            return Ok(Vec::new());
        };
        let mut results: Vec<(u64, f32)> = graph
            .search(query, top_k, self.ef_search)
            .into_iter()
            .map(|neighbour| (neighbour.d_id as u64, 1.0 - neighbour.distance))
            .collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_vector(dims: usize, hot: usize) -> Vec<f32> {
        let mut vector = vec![0.0f32; dims];
        vector[hot] = 1.0;
        vector
    }

    #[test]
    fn search_finds_the_nearest_axis_aligned_vector() {
        let mut index = DenseIndex::new(8, 16, 100, 50);
        for axis in 0..8 {
            index.upsert(axis as u64, unit_vector(8, axis)).unwrap();
        }
        let results = index.search(&unit_vector(8, 3), 1).unwrap();
        assert_eq!(results[0].0, 3);
        assert!(
            results[0].1 > 0.99,
            "expected near-exact match, got {}",
            results[0].1
        );
    }

    #[test]
    fn removed_vectors_are_not_returned() {
        let mut index = DenseIndex::new(4, 16, 100, 50);
        index.upsert(1, unit_vector(4, 0)).unwrap();
        index.upsert(2, unit_vector(4, 1)).unwrap();
        index.remove(1);
        let results = index.search(&unit_vector(4, 0), 2).unwrap();
        assert!(results.iter().all(|(id, _)| *id != 1));
    }

    #[test]
    fn empty_index_returns_no_results() {
        let mut index = DenseIndex::new(4, 16, 100, 50);
        let results = index.search(&unit_vector(4, 0), 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn dimension_mismatch_is_rejected() {
        let mut index = DenseIndex::new(4, 16, 100, 50);
        assert!(index.upsert(1, vec![0.0, 1.0]).is_err());
        assert!(index.search(&[0.0, 1.0], 1).is_err());
    }
}
