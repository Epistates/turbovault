//! Dense (ANN) side of hybrid search: an in-memory HNSW graph over chunk
//! embeddings, backed by [`hnsw_rs`].
//!
//! `hnsw_rs`'s `Hnsw` has no point-removal API (confirmed against 0.3.4's
//! public surface). This index keeps the actual vectors in a plain map as the
//! source of truth and keeps the graph current around that gap:
//!
//! - A new chunk is inserted into the existing graph, so an edit costs its
//!   own chunks, not a rebuild of every vector in the vault.
//! - A removed chunk stays in the graph as a tombstone, and searches skip it
//!   with a filter, so the top `k` live chunks still come back.
//! - Replacing a vector already in the graph (the same id with new values),
//!   or tombstones passing a quarter of the graph, rebuilds it, inserting in
//!   parallel.

use std::collections::{HashMap, HashSet};

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
    /// Ids the graph holds a point for, live or tombstoned.
    in_graph: HashSet<u64>,
    /// Points in the graph whose vector has since been removed.
    tombstones: usize,
    /// The graph must be rebuilt before the next search.
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
            in_graph: HashSet::new(),
            tombstones: 0,
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
        match &self.graph {
            // A point under this id already has its old values in the graph,
            // which can't remove it.
            Some(_) if self.in_graph.contains(&id) => self.dirty = true,
            Some(graph) if !self.dirty => {
                graph.insert_slice((vector.as_slice(), id as usize));
                self.in_graph.insert(id);
            }
            _ => self.dirty = true,
        }
        self.vectors.insert(id, vector);
        Ok(())
    }

    /// Remove a chunk's vector, if present. It stays in the graph, skipped
    /// by searches, until the next rebuild.
    pub fn remove(&mut self, id: u64) {
        if self.vectors.remove(&id).is_some() && self.in_graph.contains(&id) {
            self.tombstones += 1;
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
        let worn = self.tombstones * 4 > self.in_graph.len();
        if !self.dirty && !worn && (self.graph.is_some() || self.vectors.is_empty()) {
            return;
        }
        self.in_graph.clear();
        self.tombstones = 0;
        if self.vectors.is_empty() {
            self.graph = None;
        } else {
            let graph = Hnsw::<f32, DistCosine>::new(
                self.max_nb_connection,
                // A capacity hint; later inserts grow past it.
                self.vectors.len().max(1024),
                MAX_LAYER,
                self.ef_construction,
                DistCosine {},
            );
            let points: Vec<(&[f32], usize)> = self
                .vectors
                .iter()
                .map(|(id, vector)| (vector.as_slice(), *id as usize))
                .collect();
            graph.parallel_insert_slice(&points);
            self.in_graph.extend(self.vectors.keys().copied());
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
        let vectors = &self.vectors;
        let live = |id: &usize| vectors.contains_key(&(*id as u64));
        let filter: Option<&dyn hnsw_rs::prelude::FilterT> =
            (self.tombstones > 0).then_some(&live as &dyn hnsw_rs::prelude::FilterT);
        let mut results: Vec<(u64, f32)> = graph
            .search_filter(query, top_k, self.ef_search, filter)
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

    /// Chunks added, replaced and removed between searches come back exactly
    /// as a freshly built index would return them.
    #[test]
    fn incremental_changes_match_a_rebuild() {
        let mut index = DenseIndex::new(8, 16, 100, 50);
        for axis in 0..8u64 {
            index.upsert(axis, unit_vector(8, axis as usize)).unwrap();
        }
        assert_eq!(index.search(&unit_vector(8, 2), 1).unwrap()[0].0, 2);

        index.remove(2);
        index.upsert(20, unit_vector(8, 2)).unwrap();
        index.upsert(5, unit_vector(8, 6)).unwrap();
        let mut fresh = DenseIndex::new(8, 16, 100, 50);
        for (id, vector) in &index.vectors {
            fresh.upsert(*id, vector.clone()).unwrap();
        }
        for axis in 0..8 {
            let found = index.search(&unit_vector(8, axis), 2).unwrap();
            let expected = fresh.search(&unit_vector(8, axis), 2).unwrap();
            assert!(found.iter().all(|r| r.0 != 2), "removed chunk returned");
            // Ties (several exact matches, or none) can fall either way, so
            // what must agree is how near the nearest is.
            assert!(
                (found[0].1 - expected[0].1).abs() < 1e-5,
                "nearest to axis {axis}: {found:?} vs {expected:?}"
            );
        }
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
