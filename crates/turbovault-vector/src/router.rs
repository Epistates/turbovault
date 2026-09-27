//! Reciprocal Rank Fusion: combine two independently-ranked candidate lists
//! into one ranking, the way [`crate::engine::IndexEngine`] combines the
//! dense and lexical sides of a hybrid search.
//!
//! Kept as a small, standalone function over generic keys (rather than
//! chunk/note types) so it is trivial to test in isolation from embedding,
//! storage, or either underlying index. Ported from the RRF math in
//! ForrestThump's prototype (`turbovault-vector::router::SearchRouter::hybrid_search`
//! on `forrest/prototype-vector-e2e`), generalized from "BM25 ranks supplied
//! by the caller" to "either side's ranks", since this crate owns both sides
//! itself rather than receiving one from the host.

use std::collections::HashMap;
use std::hash::Hash;

/// One fused result: `key`, its combined score, and the 0-based rank it held
/// on each side that ranked it at all.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedResult<K> {
    /// The fused identifier (a note path, in this crate's use).
    pub key: K,
    /// Combined score. Higher is more relevant; not comparable across calls
    /// with different weights or `k`.
    pub score: f64,
    /// 0-based rank on the primary side, if it appeared there.
    pub primary_rank: Option<usize>,
    /// 0-based rank on the secondary side, if it appeared there.
    pub secondary_rank: Option<usize>,
}

/// Fuse two rank-ordered (best first) candidate lists via Reciprocal Rank
/// Fusion: `score(key) = primary_weight / (k + primary_rank) + secondary_weight
/// / (k + secondary_rank)`, treating a side a key is absent from as
/// contributing zero. A key present on both sides outranks one present on
/// only one, all else equal, which is the point of fusing rather than
/// picking a single side.
///
/// Only the rank within each input matters, not its own score scale, so a
/// cosine similarity and a BM25 score combine safely despite having
/// unrelated magnitudes.
pub fn reciprocal_rank_fusion<K: Eq + Hash + Clone>(
    primary: &[(K, f32)],
    secondary: &[(K, f32)],
    primary_weight: f64,
    secondary_weight: f64,
    k: f64,
) -> Vec<FusedResult<K>> {
    let primary_rank: HashMap<&K, usize> = primary
        .iter()
        .enumerate()
        .map(|(rank, (key, _))| (key, rank))
        .collect();
    let secondary_rank: HashMap<&K, usize> = secondary
        .iter()
        .enumerate()
        .map(|(rank, (key, _))| (key, rank))
        .collect();

    let mut keys: Vec<&K> = primary.iter().map(|(key, _)| key).collect();
    for (key, _) in secondary {
        if !primary_rank.contains_key(key) {
            keys.push(key);
        }
    }

    let mut fused: Vec<FusedResult<K>> = keys
        .into_iter()
        .map(|key| {
            let primary_rank = primary_rank.get(key).copied();
            let secondary_rank = secondary_rank.get(key).copied();
            let score = primary_rank
                .map(|rank| primary_weight / (k + rank as f64))
                .unwrap_or(0.0)
                + secondary_rank
                    .map(|rank| secondary_weight / (k + rank as f64))
                    .unwrap_or(0.0);
            FusedResult {
                key: key.clone(),
                score,
                primary_rank,
                secondary_rank,
            }
        })
        .collect();

    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_present_on_both_sides_outranks_single_side_matches() {
        let primary = vec![("a", 0.9), ("b", 0.5)];
        let secondary = vec![("b", 10.0), ("c", 5.0)];
        let fused = reciprocal_rank_fusion(&primary, &secondary, 0.6, 0.4, 60.0);
        assert_eq!(
            fused[0].key, "b",
            "b ranks on both sides, so it should fuse to the top"
        );
        assert_eq!(fused[0].primary_rank, Some(1));
        assert_eq!(fused[0].secondary_rank, Some(0));
    }

    #[test]
    fn preserves_every_key_from_both_sides_exactly_once() {
        let primary = vec![("a", 1.0), ("b", 1.0)];
        let secondary = vec![("b", 1.0), ("c", 1.0)];
        let fused = reciprocal_rank_fusion(&primary, &secondary, 0.5, 0.5, 60.0);
        let mut keys: Vec<&str> = fused.iter().map(|r| r.key).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn empty_secondary_falls_back_to_primary_order() {
        let primary = vec![("a", 1.0), ("b", 1.0), ("c", 1.0)];
        let fused = reciprocal_rank_fusion(&primary, &[], 0.6, 0.4, 60.0);
        assert_eq!(
            fused.iter().map(|r| r.key).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert!(fused.iter().all(|r| r.secondary_rank.is_none()));
    }

    #[test]
    fn both_empty_fuses_to_nothing() {
        let fused: Vec<FusedResult<&str>> = reciprocal_rank_fusion(&[], &[], 0.5, 0.5, 60.0);
        assert!(fused.is_empty());
    }

    #[test]
    fn zero_weight_on_a_side_ignores_its_ranking() {
        // Weighting the secondary side at 0 should reproduce plain primary order
        // even when the secondary ranking disagrees entirely.
        let primary = vec![("a", 1.0), ("b", 1.0)];
        let secondary = vec![("b", 1.0), ("a", 1.0)];
        let fused = reciprocal_rank_fusion(&primary, &secondary, 1.0, 0.0, 60.0);
        assert_eq!(fused[0].key, "a");
        assert_eq!(fused[1].key, "b");
    }
}
