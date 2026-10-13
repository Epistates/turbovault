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
//!
//! Up to [`EXACT_SEARCH_LIMIT`] vectors there is no graph at all: a search
//! compares the query with every vector. An HNSW search is approximate, and
//! on a graph of a few dozen points it misses the true nearest often enough
//! to notice, while comparing a few thousand vectors takes about a
//! millisecond.

use std::collections::{HashMap, HashSet};

use hnsw_rs::prelude::{DistCosine, Hnsw};

use crate::error::{Result, VectorError};

/// Number of hierarchical layers `hnsw_rs` allocates for the graph. Not
/// exposed as config: this bounds table allocation, not search quality, and
/// the library's own defaults comfortably cover a vault-sized corpus.
const MAX_LAYER: usize = 16;

/// Up to this many vectors, a search is exact, comparing every one; past it,
/// it goes through the HNSW graph.
pub const EXACT_SEARCH_LIMIT: usize = 4096;

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
    /// Searches over at most this many vectors are exact.
    exact_limit: usize,
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
            exact_limit: EXACT_SEARCH_LIMIT,
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
        if self.vectors.len() <= self.exact_limit {
            // A graph built while the index was larger is dropped; one is
            // built again if it grows past the limit.
            if self.graph.take().is_some() {
                self.in_graph.clear();
                self.tombstones = 0;
                self.dirty = true;
            }
            return Ok(self.exact_search(query, top_k));
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

    /// The `top_k` nearest vectors by cosine similarity, comparing every
    /// one; ties go to the lower id, so the answer doesn't depend on map
    /// order. A zero vector is similar to nothing.
    fn exact_search(&self, query: &[f32], top_k: usize) -> Vec<(u64, f32)> {
        let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
        let query_norm = norm(query);
        let mut results: Vec<(u64, f32)> = self
            .vectors
            .iter()
            .map(|(id, vector)| {
                let denominator = query_norm * norm(vector);
                let similarity = if denominator > 0.0 {
                    vector.iter().zip(query).map(|(a, b)| a * b).sum::<f32>() / denominator
                } else {
                    0.0
                };
                (*id, similarity)
            })
            .collect();
        results.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        results.truncate(top_k);
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl DenseIndex {
        /// Search through the graph however few vectors there are.
        fn always_graph(mut self) -> Self {
            self.exact_limit = 0;
            self
        }
    }

    /// `count` vectors of `dims` values in [-1, 1), the same every run.
    fn spread(count: usize, dims: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut state = seed;
        let mut next = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
        };
        (0..count)
            .map(|_| (0..dims).map(|_| next()).collect())
            .collect()
    }

    fn unit_vector(dims: usize, hot: usize) -> Vec<f32> {
        let mut vector = vec![0.0f32; dims];
        vector[hot] = 1.0;
        vector
    }

    /// A stored point near the `hot` axis, tilted a little toward the next
    /// axis by an amount that grows with `hot`. Axis-aligned unit vectors
    /// are all exactly as far from each other, and on points that equal
    /// HNSW's neighbour selection keeps almost no edges, so a search can
    /// miss the nearest one. Real embeddings are never that uniform; these
    /// points aren't either.
    fn point(dims: usize, hot: usize) -> Vec<f32> {
        let mut vector = unit_vector(dims, hot);
        vector[(hot + 1) % dims] = 0.05 * (hot + 1) as f32;
        vector
    }

    /// The similarity of the nearest live vector to `query`, found by
    /// comparing every one: what an approximate search should match on a
    /// graph this small.
    fn exact_nearest(index: &DenseIndex, query: &[f32]) -> f32 {
        let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
        index
            .vectors
            .values()
            .map(|v| {
                let dot: f32 = v.iter().zip(query).map(|(a, b)| a * b).sum();
                dot / (norm(v) * norm(query))
            })
            .fold(f32::MIN, f32::max)
    }

    #[test]
    fn search_finds_the_nearest_axis_aligned_vector() {
        let mut index = DenseIndex::new(8, 16, 100, 50);
        for axis in 0..8 {
            index.upsert(axis as u64, point(8, axis)).unwrap();
        }
        let results = index.search(&unit_vector(8, 3), 1).unwrap();
        assert_eq!(results[0].0, 3);
        assert!(
            results[0].1 > 0.95,
            "expected a near match, got {}",
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

    /// Chunks added, replaced and removed between searches come back as the
    /// exact nearest live vectors, both after an in-place insert (with a
    /// tombstone in the graph) and after a replacement forces a rebuild.
    #[test]
    fn incremental_changes_find_the_exact_nearest() {
        let mut index = DenseIndex::new(8, 16, 100, 50);
        for axis in 0..8u64 {
            index.upsert(axis, point(8, axis as usize)).unwrap();
        }
        assert_eq!(index.search(&unit_vector(8, 2), 1).unwrap()[0].0, 2);

        let assert_exact = |index: &mut DenseIndex, stage: &str| {
            for axis in 0..8 {
                let query = unit_vector(8, axis);
                let found = index.search(&query, 2).unwrap();
                assert!(
                    found.iter().all(|r| r.0 != 2),
                    "{stage}: removed chunk returned"
                );
                // Ties (two equal vectors) can fall either way, so what must
                // agree is how near the nearest is.
                let exact = exact_nearest(index, &query);
                assert!(
                    (found[0].1 - exact).abs() < 1e-5,
                    "{stage}, nearest to axis {axis}: {found:?}, exact {exact}"
                );
            }
        };

        // A removal leaves a tombstone; a new id is inserted in place.
        index.remove(2);
        index.upsert(20, point(8, 2)).unwrap();
        assert_exact(&mut index, "in place");

        // New values under an id the graph holds force a rebuild.
        index.upsert(5, point(8, 6)).unwrap();
        assert_exact(&mut index, "rebuilt");
    }

    /// Past the exact limit, searches go through the graph. Chunks removed
    /// (left as tombstones) and inserted in place keep its recall: nearly
    /// every query still finds its true nearest, and a removed chunk never
    /// comes back.
    #[test]
    fn graph_search_keeps_its_recall_through_changes() {
        let points = spread(400, 16, 7);
        let mut index = DenseIndex::new(16, 16, 100, 50).always_graph();
        for (id, vector) in points.iter().take(300).enumerate() {
            index.upsert(id as u64, vector.clone()).unwrap();
        }
        index.search(&points[0], 1).unwrap();

        for id in 0..30u64 {
            index.remove(id * 10);
        }
        for (id, vector) in points.iter().enumerate().skip(300) {
            index.upsert(id as u64, vector.clone()).unwrap();
        }
        assert!(
            index.graph.is_some() && !index.dirty,
            "changes went in place"
        );

        let queries = spread(200, 16, 99);
        let mut hits = 0;
        for query in &queries {
            let found = index.search(query, 5).unwrap();
            assert!(
                found.iter().all(|(id, _)| id % 10 != 0 || *id >= 300),
                "a removed chunk came back: {found:?}"
            );
            if (found[0].1 - exact_nearest(&index, query)).abs() < 1e-5 {
                hits += 1;
            }
        }
        assert!(hits >= 190, "recall@1 {hits}/200");
    }

    #[test]
    fn a_small_index_drops_its_graph_and_searches_exactly() {
        let mut index = DenseIndex::new(8, 16, 100, 50);
        index.exact_limit = 4;
        for axis in 0..6u64 {
            index.upsert(axis, point(8, axis as usize)).unwrap();
        }
        index.search(&unit_vector(8, 0), 1).unwrap();
        assert!(index.graph.is_some());

        index.remove(4);
        index.remove(5);
        let found = index.search(&unit_vector(8, 3), 1).unwrap();
        assert!(index.graph.is_none());
        assert_eq!(found[0].0, 3);
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
