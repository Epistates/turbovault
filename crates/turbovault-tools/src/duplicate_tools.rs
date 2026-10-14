//! Duplicate and near-duplicate note detection
//!
//! Uses a two-stage approach:
//! 1. SimHash fingerprinting for fast O(1) approximate similarity checks
//! 2. TF-IDF cosine similarity for precise verification of candidates
//!
//! Candidates are every pair of notes whose fingerprints are within the
//! threshold's Hamming distance. They're found by comparing every pair, on
//! purpose: a 64-bit XOR and popcount per pair is about a nanosecond, and
//! split across cores that stays well under a second at 100,000 notes. The
//! usual shortcuts (banding the fingerprint, MinHash LSH) only find most
//! close pairs, and at the default threshold of 0.8 (13 differing bits) a
//! banded index barely narrows the search anyway. The fingerprints and the
//! TF-IDF vectors come from the similarity engine, which computes each note's
//! once.
//!
//! Also provides pairwise note comparison with diff summaries.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;

use crate::diff_tools::{DiffSummary, DiffTools};
use crate::similarity_engine::{NoteSummary, SimilarityEngine};

/// Duplicate detection tools
#[derive(Clone)]
pub struct DuplicateTools {
    /// The vault searched for duplicates.
    pub manager: Arc<VaultManager>,
}

/// A group of near-duplicate notes
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DuplicateGroup {
    /// How alike the pair is, 0.0 to 1.0, to four decimal places.
    pub similarity: f64,
    /// The notes that are alike.
    pub notes: Vec<DuplicateNote>,
}

/// A note within a duplicate group
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DuplicateNote {
    /// Vault-relative path.
    pub path: String,
    /// The note's title.
    pub title: String,
    /// Words of plain text.
    pub word_count: usize,
    /// When it was last modified, RFC 3339 in UTC, or `unknown`.
    pub modified_at: String,
}

/// Detailed comparison of two notes
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CompareResult {
    /// One note, vault-relative.
    pub left_path: String,
    /// The other note, vault-relative.
    pub right_path: String,
    /// How alike they are, 0.0 to 1.0, to four decimal places.
    pub similarity_score: f64,
    /// Terms the two notes share.
    pub shared_terms: Vec<String>,
    /// Line-level diff counts.
    pub diff_summary: DiffSummary,
    /// What to do about the pair, in words.
    pub recommendation: String,
}

impl DuplicateTools {
    /// Duplicate tools over `manager`'s vault.
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Find near-duplicate notes across the vault
    pub async fn find_duplicates(
        &self,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<DuplicateGroup>> {
        let engine = SimilarityEngine::new(self.manager.clone()).await?;
        Ok(self.find_duplicates_with(&engine, threshold, limit))
    }

    /// [`Self::find_duplicates`] with an engine already built for this vault.
    pub fn find_duplicates_with(
        &self,
        engine: &SimilarityEngine,
        threshold: f64,
        limit: usize,
    ) -> Vec<DuplicateGroup> {
        let notes = engine.note_summaries();
        let prints: Vec<u64> = notes.iter().map(|note| note.simhash).collect();

        // Convert threshold to hamming distance bound
        // SimHash with 64 bits: similarity ~= 1 - (hamming_distance / 64)
        // For threshold 0.8 → max hamming distance = 64 * (1 - 0.8) = ~13
        let max_hamming = ((1.0 - threshold) * 64.0).ceil() as u32;

        // Find candidate pairs via SimHash hamming distance: every pair, in
        // order (see the module docs).
        let mut candidate_pairs: Vec<(usize, usize, u32)> = (0..prints.len())
            .into_par_iter()
            .flat_map_iter(|i| {
                let print = prints[i];
                let prints = &prints;
                ((i + 1)..prints.len()).filter_map(move |j| {
                    let hamming = (print ^ prints[j]).count_ones();
                    (hamming <= max_hamming).then_some((i, j, hamming))
                })
            })
            .collect();

        // Sort by hamming distance (most similar first)
        candidate_pairs.sort_by_key(|&(_, _, h)| h);

        // Verify candidates and build groups
        let mut groups: Vec<DuplicateGroup> = Vec::new();
        let mut grouped: Vec<bool> = vec![false; notes.len()];

        for (i, j, hamming) in &candidate_pairs {
            if grouped[*i] && grouped[*j] {
                continue;
            }

            let path_i = notes[*i].path.to_string_lossy().to_string();
            let path_j = notes[*j].path.to_string_lossy().to_string();

            // Score this one pair. Ranking the whole vault from `path_i` to read
            // off `path_j` made every candidate pair cost a pass over every
            // note.
            let precise_score = engine
                .pair_similarity(&path_i, &path_j)
                // Fallback: compute from hamming distance
                .unwrap_or_else(|| 1.0 - (*hamming as f64 / 64.0));

            if precise_score >= threshold {
                grouped[*i] = true;
                grouped[*j] = true;

                let notes = vec![
                    make_duplicate_note(&notes[*i]),
                    make_duplicate_note(&notes[*j]),
                ];

                groups.push(DuplicateGroup {
                    similarity: (precise_score * 10000.0).round() / 10000.0,
                    notes,
                });
            }

            if groups.len() >= limit {
                break;
            }
        }

        // Sort by similarity descending
        groups.sort_by(|a, b| {
            let paths = |group: &DuplicateGroup| {
                group
                    .notes
                    .iter()
                    .map(|note| note.path.clone())
                    .collect::<Vec<_>>()
            };
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| paths(a).cmp(&paths(b)))
        });
        groups
    }

    /// Compare two specific notes in detail
    pub async fn compare_notes(&self, left: &str, right: &str) -> Result<CompareResult> {
        let engine = SimilarityEngine::new(self.manager.clone()).await?;
        self.compare_notes_with(&engine, left, right).await
    }

    /// [`Self::compare_notes`] with an engine already built for this vault.
    pub async fn compare_notes_with(
        &self,
        engine: &SimilarityEngine,
        left: &str,
        right: &str,
    ) -> Result<CompareResult> {
        // Get similarity score and shared terms: what a similarity ranking
        // from `left` lists for `right`, which never includes `left` itself.
        let (similarity_score, shared_terms) = if left == right {
            None
        } else {
            engine.pair_details(left, right)
        }
        .map(|(score, terms)| ((score * 10000.0).round() / 10000.0, terms))
        .unwrap_or((0.0, vec![]));

        // Get diff summary
        let diff_tools = DiffTools::new(self.manager.clone());
        let diff_result = diff_tools.diff_notes(left, right).await?;

        let recommendation = if similarity_score >= 0.9 {
            "Likely duplicate — consider merging these notes".to_string()
        } else if similarity_score >= 0.7 {
            "Highly similar — consider linking or consolidating overlapping content".to_string()
        } else if similarity_score >= 0.4 {
            "Related content — consider adding links between these notes".to_string()
        } else {
            "Different content — no action needed".to_string()
        };

        Ok(CompareResult {
            left_path: left.to_string(),
            right_path: right.to_string(),
            similarity_score: (similarity_score * 10000.0).round() / 10000.0,
            shared_terms,
            diff_summary: diff_result.summary,
            recommendation,
        })
    }
}

fn make_duplicate_note(note: &NoteSummary) -> DuplicateNote {
    let modified_at = chrono::DateTime::from_timestamp(note.modified_at as i64, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| "unknown".to_string());

    DuplicateNote {
        path: note.path.to_string_lossy().to_string(),
        title: note.title.clone(),
        word_count: note.word_count,
        modified_at,
    }
}

/// Compute a 64-bit SimHash fingerprint for text content
pub(crate) fn compute_simhash(text: &str) -> u64 {
    let shingles = generate_shingles(text, 3);
    if shingles.is_empty() {
        return 0;
    }

    let mut bit_sums = [0i64; 64];

    for shingle in &shingles {
        let hash = hash_shingle(shingle);
        for (i, sum) in bit_sums.iter_mut().enumerate() {
            if hash & (1u64 << i) != 0 {
                *sum += 1;
            } else {
                *sum -= 1;
            }
        }
    }

    let mut fingerprint = 0u64;
    for (i, &sum) in bit_sums.iter().enumerate() {
        if sum > 0 {
            fingerprint |= 1u64 << i;
        }
    }
    fingerprint
}

/// Generate word n-grams (shingles) from text
fn generate_shingles(text: &str, n: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().filter(|w| w.len() >= 2).collect();

    if words.len() < n {
        return words.iter().map(|w| w.to_lowercase()).collect();
    }

    words
        .windows(n)
        .map(|window| {
            window
                .iter()
                .map(|w| w.to_lowercase())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// Hash a shingle to a 64-bit value
fn hash_shingle(shingle: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    shingle.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simhash_identical() {
        let h1 = compute_simhash("The quick brown fox jumps over the lazy dog");
        let h2 = compute_simhash("The quick brown fox jumps over the lazy dog");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_simhash_similar() {
        let h1 = compute_simhash("The quick brown fox jumps over the lazy dog and then rests");
        let h2 = compute_simhash("The quick brown fox leaps over the lazy dog and then sleeps");
        let hamming = (h1 ^ h2).count_ones();
        // SimHash similarity improves with longer text; short texts may have higher distances
        assert!(
            hamming < 32,
            "Similar text should have hamming distance below 32: {}",
            hamming
        );
    }

    #[test]
    fn test_simhash_different() {
        let h1 = compute_simhash("The quick brown fox jumps over the lazy dog");
        let h2 = compute_simhash("Quantum computing enables exponential speedup in cryptography");
        let hamming = (h1 ^ h2).count_ones();
        assert!(
            hamming > 10,
            "Different text should have high hamming distance: {}",
            hamming
        );
    }

    #[test]
    fn test_generate_shingles() {
        let shingles = generate_shingles("one two three four", 3);
        assert_eq!(shingles.len(), 2);
        assert_eq!(shingles[0], "one two three");
        assert_eq!(shingles[1], "two three four");
    }

    #[test]
    fn test_simhash_empty() {
        assert_eq!(compute_simhash(""), 0);
    }
}
