//! TF-IDF cosine similarity engine for semantic note search
//!
//! Builds TF-IDF document vectors for all vault notes and enables:
//! - Finding notes semantically similar to a query string
//! - Finding notes most similar to a given note
//! - Explainable results via shared term reporting

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_core::prelude::*;
use turbovault_parser::to_plain_text;
use turbovault_vault::VaultManager;

use crate::search_engine::is_stopword;

/// TF-IDF document vector for a single note
struct DocumentVector {
    path: PathBuf,
    title: String,
    preview: String,
    tfidf: HashMap<String, f64>,
    norm: f64,
}

/// Similarity search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarityResult {
    pub path: String,
    pub title: String,
    pub score: f64,
    pub shared_terms: Vec<String>,
    pub preview: String,
}

/// TF-IDF cosine similarity engine
pub struct SimilarityEngine {
    #[allow(dead_code)]
    manager: Arc<VaultManager>,
    documents: Vec<DocumentVector>,
    idf: HashMap<String, f64>,
    #[allow(dead_code)]
    doc_count: usize,
}

impl SimilarityEngine {
    /// Build TF-IDF vectors for all vault documents
    pub async fn new(manager: Arc<VaultManager>) -> Result<Self> {
        let files = manager.scan_vault().await?;
        let doc_count = files.len().max(1);

        // Pass 1: compute document frequencies
        let mut doc_freq: HashMap<String, usize> = HashMap::new();
        let mut parsed_docs: Vec<(PathBuf, String, String, HashMap<String, usize>)> = Vec::new();

        for file_path in &files {
            if let Ok(vault_file) = manager.parse_file(file_path).await {
                let plain = to_plain_text(&vault_file.content);
                let tokens = tokenize(&plain);
                let mut term_counts: HashMap<String, usize> = HashMap::new();

                for token in &tokens {
                    *term_counts.entry(token.clone()).or_insert(0) += 1;
                }

                // Count document frequency (each unique term in this doc)
                for term in term_counts.keys() {
                    *doc_freq.entry(term.clone()).or_insert(0) += 1;
                }

                let rel_path = PathBuf::from(manager.relative_path(file_path));
                let title = vault_file
                    .headings
                    .first()
                    .map(|h| h.text.clone())
                    .unwrap_or_else(|| {
                        rel_path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()
                    });

                let preview = plain
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect();

                parsed_docs.push((rel_path, title, preview, term_counts));
            }
        }

        // Compute IDF
        let idf: HashMap<String, f64> = doc_freq
            .into_iter()
            .map(|(term, count)| {
                let idf_val = (doc_count as f64 / count as f64).ln();
                (term, idf_val)
            })
            .collect();

        // Pass 2: build TF-IDF vectors
        let mut documents = Vec::with_capacity(parsed_docs.len());

        for (path, title, preview, term_counts) in parsed_docs {
            let total_terms: usize = term_counts.values().sum();
            if total_terms == 0 {
                continue;
            }

            let mut tfidf: HashMap<String, f64> = HashMap::new();
            let mut norm_sq = 0.0f64;

            for (term, count) in &term_counts {
                let tf = *count as f64 / total_terms as f64;
                let idf_val = idf.get(term).copied().unwrap_or(0.0);
                let tfidf_val = tf * idf_val;

                if tfidf_val > 0.0 {
                    tfidf.insert(term.clone(), tfidf_val);
                    norm_sq += tfidf_val * tfidf_val;
                }
            }

            let norm = norm_sq.sqrt();

            documents.push(DocumentVector {
                path,
                title,
                preview,
                tfidf,
                norm,
            });
        }

        Ok(Self {
            manager,
            documents,
            idf,
            doc_count,
        })
    }

    /// Find notes similar to a query string
    pub fn semantic_search(&self, query: &str, limit: usize) -> Vec<SimilarityResult> {
        let query_tokens = tokenize(query);
        if query_tokens.is_empty() {
            return vec![];
        }

        // Build query vector
        let mut query_counts: HashMap<String, usize> = HashMap::new();
        for token in &query_tokens {
            *query_counts.entry(token.clone()).or_insert(0) += 1;
        }

        let total_terms = query_tokens.len();
        let mut query_tfidf: HashMap<String, f64> = HashMap::new();
        let mut query_norm_sq = 0.0f64;

        for (term, count) in &query_counts {
            let tf = *count as f64 / total_terms as f64;
            let idf_val = self.idf.get(term).copied().unwrap_or(0.0);
            let tfidf_val = tf * idf_val;
            if tfidf_val > 0.0 {
                query_tfidf.insert(term.clone(), tfidf_val);
                query_norm_sq += tfidf_val * tfidf_val;
            }
        }

        let query_norm = query_norm_sq.sqrt();
        if query_norm < f64::EPSILON {
            return vec![];
        }

        self.rank_by_similarity(&query_tfidf, query_norm, None, limit)
    }

    /// Find notes most similar to a given note
    pub fn find_similar_notes(&self, path: &str, limit: usize) -> Vec<SimilarityResult> {
        let target_path = PathBuf::from(path);
        let target = self.documents.iter().find(|d| d.path == target_path);

        match target {
            Some(doc) => self.rank_by_similarity(&doc.tfidf, doc.norm, Some(path), limit),
            None => vec![],
        }
    }

    /// Rank all documents by cosine similarity to a query vector
    fn rank_by_similarity(
        &self,
        query_tfidf: &HashMap<String, f64>,
        query_norm: f64,
        exclude_path: Option<&str>,
        limit: usize,
    ) -> Vec<SimilarityResult> {
        let mut results: Vec<(f64, Vec<String>, &DocumentVector)> = Vec::new();

        for doc in &self.documents {
            // Skip the query note itself
            if let Some(excl) = exclude_path
                && doc.path.to_string_lossy() == excl
            {
                continue;
            }

            if doc.norm < f64::EPSILON {
                continue;
            }

            // Compute dot product and collect shared terms
            let mut dot_product = 0.0f64;
            let mut shared_terms = Vec::new();

            for (term, query_weight) in query_tfidf {
                if let Some(doc_weight) = doc.tfidf.get(term) {
                    dot_product += query_weight * doc_weight;
                    shared_terms.push(term.clone());
                }
            }

            if dot_product > 0.0 {
                let cosine_sim = dot_product / (query_norm * doc.norm);
                // Sort shared terms by their TF-IDF weight in the document (most important first)
                shared_terms.sort_by(|a, b| {
                    let wa = doc.tfidf.get(a).unwrap_or(&0.0);
                    let wb = doc.tfidf.get(b).unwrap_or(&0.0);
                    wb.partial_cmp(wa).unwrap_or(std::cmp::Ordering::Equal)
                });
                shared_terms.truncate(10);
                results.push((cosine_sim, shared_terms, doc));
            }
        }

        // Sort by similarity descending
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

        results
            .into_iter()
            .map(|(score, shared_terms, doc)| SimilarityResult {
                path: doc.path.to_string_lossy().to_string(),
                title: doc.title.clone(),
                score: (score * 10000.0).round() / 10000.0,
                shared_terms,
                preview: doc.preview.clone(),
            })
            .collect()
    }

    /// Get the document count (for diagnostics)
    pub fn document_count(&self) -> usize {
        self.documents.len()
    }
}

/// Tokenize text into lowercase terms, filtering stopwords and short words.
/// Generates both unigrams and bigrams for better semantic capture.
pub(crate) fn tokenize(text: &str) -> Vec<String> {
    let words: Vec<String> = text
        .split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .filter(|w| !is_stopword(w))
        .collect();

    let mut tokens = words.clone();

    // Add bigrams for better semantic matching
    for pair in words.windows(2) {
        tokens.push(format!("{}_{}", pair[0], pair[1]));
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_basic() {
        let tokens = tokenize("The quick brown fox jumps over the lazy dog");
        assert!(tokens.contains(&"quick".to_string()));
        assert!(tokens.contains(&"brown".to_string()));
        assert!(tokens.contains(&"fox".to_string()));
        // Should not contain stopwords
        assert!(!tokens.contains(&"the".to_string()));
        // "over" is 4 chars and not in stopword list, so it's included
        assert!(tokens.contains(&"over".to_string()));
    }

    #[test]
    fn test_tokenize_bigrams() {
        let tokens = tokenize("machine learning algorithms");
        assert!(tokens.contains(&"machine".to_string()));
        assert!(tokens.contains(&"learning".to_string()));
        assert!(tokens.contains(&"algorithms".to_string()));
        assert!(tokens.contains(&"machine_learning".to_string()));
        assert!(tokens.contains(&"learning_algorithms".to_string()));
    }

    #[test]
    fn test_tokenize_filters_short_words() {
        let tokens = tokenize("I am a ok fine yes no do go");
        // Most of these are <3 chars or stopwords
        assert!(!tokens.contains(&"am".to_string()));
        assert!(!tokens.contains(&"ok".to_string()));
        assert!(tokens.contains(&"fine".to_string()));
    }

    #[test]
    fn test_tokenize_empty() {
        let tokens = tokenize("");
        assert!(tokens.is_empty());
    }
}
