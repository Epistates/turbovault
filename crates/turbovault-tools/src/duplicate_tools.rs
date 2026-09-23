//! Duplicate and near-duplicate note detection
//!
//! Uses a two-stage approach:
//! 1. SimHash fingerprinting for fast O(1) approximate similarity checks
//! 2. TF-IDF cosine similarity for precise verification of candidates
//!
//! Also provides pairwise note comparison with diff summaries.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_core::prelude::*;
use turbovault_parser::to_plain_text;
use turbovault_vault::VaultManager;

use crate::diff_tools::{DiffSummary, DiffTools};
use crate::similarity_engine::SimilarityEngine;

/// Duplicate detection tools
#[derive(Clone)]
pub struct DuplicateTools {
    pub manager: Arc<VaultManager>,
}

/// A group of near-duplicate notes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub similarity: f64,
    pub notes: Vec<DuplicateNote>,
}

/// A note within a duplicate group
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateNote {
    pub path: String,
    pub title: String,
    pub word_count: usize,
    pub modified_at: String,
}

/// Detailed comparison of two notes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareResult {
    pub left_path: String,
    pub right_path: String,
    pub similarity_score: f64,
    pub shared_terms: Vec<String>,
    pub diff_summary: DiffSummary,
    pub recommendation: String,
}

/// SimHash fingerprint for a document
struct DocFingerprint {
    path: PathBuf,
    title: String,
    word_count: usize,
    modified_at: f64,
    fingerprint: u64,
}

impl DuplicateTools {
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Find near-duplicate notes across the vault
    pub async fn find_duplicates(
        &self,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<DuplicateGroup>> {
        let files = self.manager.scan_vault().await?;

        // Build SimHash fingerprints for all files
        let mut fingerprints: Vec<DocFingerprint> = Vec::new();

        for file_path in &files {
            if let Ok(vault_file) = self.manager.parse_file(file_path).await {
                let plain = to_plain_text(&vault_file.content);
                let fingerprint = compute_simhash(&plain);

                let rel_path = self.manager.relative_path(file_path);

                let title = vault_file
                    .headings
                    .first()
                    .map(|h| h.text.clone())
                    .unwrap_or_else(|| {
                        file_path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()
                    });

                fingerprints.push(DocFingerprint {
                    path: PathBuf::from(rel_path),
                    title,
                    word_count: plain.split_whitespace().count(),
                    modified_at: vault_file.metadata.modified_at,
                    fingerprint,
                });
            }
        }

        // Convert threshold to hamming distance bound
        // SimHash with 64 bits: similarity ~= 1 - (hamming_distance / 64)
        // For threshold 0.8 → max hamming distance = 64 * (1 - 0.8) = ~13
        let max_hamming = ((1.0 - threshold) * 64.0).ceil() as u32;

        // Find candidate pairs via SimHash hamming distance
        let mut candidate_pairs: Vec<(usize, usize, u32)> = Vec::new();

        for i in 0..fingerprints.len() {
            for j in (i + 1)..fingerprints.len() {
                let hamming =
                    (fingerprints[i].fingerprint ^ fingerprints[j].fingerprint).count_ones();
                if hamming <= max_hamming {
                    candidate_pairs.push((i, j, hamming));
                }
            }
        }

        // Sort by hamming distance (most similar first)
        candidate_pairs.sort_by_key(|&(_, _, h)| h);

        // Build similarity engine for precise verification
        let sim_engine = SimilarityEngine::new(self.manager.clone()).await?;

        // Verify candidates and build groups
        let mut groups: Vec<DuplicateGroup> = Vec::new();
        let mut grouped: Vec<bool> = vec![false; fingerprints.len()];

        for (i, j, _) in &candidate_pairs {
            if grouped[*i] && grouped[*j] {
                continue;
            }

            let path_i = fingerprints[*i].path.to_string_lossy().to_string();
            let path_j = fingerprints[*j].path.to_string_lossy().to_string();

            // Use similarity engine for precise score
            let results = sim_engine.find_similar_notes(&path_i, fingerprints.len());
            let precise_score = results
                .iter()
                .find(|r| r.path == path_j)
                .map(|r| r.score)
                .unwrap_or_else(|| {
                    // Fallback: compute from hamming distance
                    let hamming =
                        (fingerprints[*i].fingerprint ^ fingerprints[*j].fingerprint).count_ones();
                    1.0 - (hamming as f64 / 64.0)
                });

            if precise_score >= threshold {
                grouped[*i] = true;
                grouped[*j] = true;

                let notes = vec![
                    make_duplicate_note(&fingerprints[*i]),
                    make_duplicate_note(&fingerprints[*j]),
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
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(groups)
    }

    /// Compare two specific notes in detail
    pub async fn compare_notes(&self, left: &str, right: &str) -> Result<CompareResult> {
        // Get similarity score and shared terms
        let sim_engine = SimilarityEngine::new(self.manager.clone()).await?;
        let results = sim_engine.find_similar_notes(left, self.manager.scan_vault().await?.len());

        let (similarity_score, shared_terms) = results
            .iter()
            .find(|r| r.path == right)
            .map(|r| (r.score, r.shared_terms.clone()))
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

fn make_duplicate_note(fp: &DocFingerprint) -> DuplicateNote {
    let modified_at = chrono::DateTime::from_timestamp(fp.modified_at as i64, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| "unknown".to_string());

    DuplicateNote {
        path: fp.path.to_string_lossy().to_string(),
        title: fp.title.clone(),
        word_count: fp.word_count,
        modified_at,
    }
}

/// Compute a 64-bit SimHash fingerprint for text content
fn compute_simhash(text: &str) -> u64 {
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
