//! TF-IDF cosine similarity engine for semantic note search
//!
//! Builds TF-IDF document vectors for all vault notes and enables:
//! - Finding notes semantically similar to a query string
//! - Finding notes most similar to a given note
//! - Explainable results via shared term reporting
//!
//! # Memory model
//!
//! A note's terms are its words (unigrams) and each pair of adjacent words
//! (bigrams). Words are interned once for the whole vault; a term is a `u64`
//! key built from one word id or two, so a bigram is never stored as a
//! string, and its text is rebuilt from its words when a result names it.
//! Each term key is interned again to a dense id, and a note keeps only its
//! sorted `(term id, count)` pairs. Weights are not stored: a term's weight
//! in a note is `count / total * idf`, with the IDF looked up by the term's
//! document frequency, so a change to one note never rewrites another. Note
//! norms depend on every IDF, so they are recomputed on the first query after
//! a change, which costs one pass over the stored pairs.
//!
//! The engine is built from the vault manager's parsed note cache, and kept
//! current with [`SimilarityEngine::apply_changes`] rather than rebuilt.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use turbovault_core::prelude::*;
use turbovault_parser::to_plain_text;
use turbovault_vault::VaultManager;

use crate::duplicate_tools::compute_simhash;
use crate::search_engine::is_stopword;

/// Similarity search result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SimilarityResult {
    /// Vault-relative path of the similar note.
    pub path: String,
    /// Its title.
    pub title: String,
    /// Cosine similarity of the two notes' TF-IDF vectors, 0.0 to 1.0.
    pub score: f64,
    /// Terms the two notes share.
    pub shared_terms: Vec<String>,
    /// The first line of its plain text, up to 200 characters.
    pub preview: String,
}

/// What the engine knows about one note, besides its terms.
#[derive(Debug, Clone)]
pub(crate) struct NoteSummary {
    /// Vault-relative path, `/`-separated.
    pub(crate) path: PathBuf,
    /// The first heading, or the file name without its extension.
    pub(crate) title: String,
    /// SimHash of the note's plain text, for duplicate detection.
    pub(crate) simhash: u64,
    /// Whitespace-separated words in the note's plain text.
    pub(crate) word_count: usize,
    /// Seconds since the epoch, from the filesystem.
    pub(crate) modified_at: f64,
}

/// One note's terms and the text shown for it.
struct Document {
    summary: NoteSummary,
    preview: String,
    /// `(term id, count)`, sorted by term id.
    terms: Box<[(u32, u32)]>,
    /// Number of term occurrences, the TF denominator.
    total: u32,
}

/// TF-IDF cosine similarity engine
pub struct SimilarityEngine {
    manager: Arc<VaultManager>,
    corpus: RwLock<Corpus>,
}

/// A term key for word `w` alone. Bigram keys put a real word id in the
/// high half, and no word id is `u32::MAX`, so the two never meet.
fn unigram(word: u32) -> u64 {
    (u64::from(u32::MAX) << 32) | u64::from(word)
}

fn bigram(first: u32, second: u32) -> u64 {
    (u64::from(first) << 32) | u64::from(second)
}

#[derive(Default)]
struct Corpus {
    /// Word to word id, for every word any note has used.
    words: HashMap<Box<str>, u32>,
    word_names: Vec<Box<str>>,
    /// Term key to term id.
    term_ids: HashMap<u64, u32>,
    /// Term id to its key, so a result can name it.
    term_keys: Vec<u64>,
    /// Term id to the number of notes containing it.
    df: Vec<u32>,
    documents: Vec<Document>,
    by_path: HashMap<PathBuf, usize>,
    /// Each document's TF-IDF norm, valid while `norms_current`.
    norms: Vec<f64>,
    /// IDF by document frequency, valid while `norms_current`.
    idf_by_df: Vec<f64>,
    norms_current: bool,
}

/// A note's text reduced to what indexing needs, worked out off any lock.
struct Tokenized {
    summary: NoteSummary,
    preview: String,
    /// Filtered, lowercased words in order.
    words: Vec<String>,
}

fn tokenize_note(rel_path: PathBuf, file: &VaultFile) -> Tokenized {
    let plain = to_plain_text(&file.content);
    let title = file
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
    Tokenized {
        summary: NoteSummary {
            path: rel_path,
            title,
            simhash: compute_simhash(&plain),
            word_count: plain.split_whitespace().count(),
            modified_at: file.metadata.modified_at,
        },
        preview,
        words: words(&plain),
    }
}

impl Corpus {
    fn note_count(&self) -> usize {
        self.documents.len()
    }

    fn word_id(&mut self, word: String) -> u32 {
        if let Some(&id) = self.words.get(word.as_str()) {
            return id;
        }
        let id = self.word_names.len() as u32;
        let word: Box<str> = word.into_boxed_str();
        self.word_names.push(word.clone());
        self.words.insert(word, id);
        id
    }

    fn term_id(&mut self, key: u64) -> u32 {
        *self.term_ids.entry(key).or_insert_with(|| {
            self.term_keys.push(key);
            self.df.push(0);
            (self.term_keys.len() - 1) as u32
        })
    }

    /// The text a term key stands for, as the tokenizer wrote it.
    fn term_name(&self, key: u64) -> String {
        let second = &self.word_names[(key & u64::from(u32::MAX)) as usize];
        match (key >> 32) as u32 {
            u32::MAX => second.to_string(),
            first => format!("{}_{}", self.word_names[first as usize], second),
        }
    }

    /// Add a note (replacing any note at the same path).
    fn insert(&mut self, note: Tokenized) {
        self.remove(&note.summary.path);
        let ids: Vec<u32> = note.words.into_iter().map(|w| self.word_id(w)).collect();
        let mut counts: HashMap<u64, u32> = HashMap::new();
        for &w in &ids {
            *counts.entry(unigram(w)).or_insert(0) += 1;
        }
        for pair in ids.windows(2) {
            *counts.entry(bigram(pair[0], pair[1])).or_insert(0) += 1;
        }
        let total = (ids.len() + ids.len().saturating_sub(1)) as u32;
        let mut terms: Vec<(u32, u32)> = counts
            .into_iter()
            .map(|(key, count)| (self.term_id(key), count))
            .collect();
        terms.sort_unstable_by_key(|&(id, _)| id);
        for &(id, _) in &terms {
            self.df[id as usize] += 1;
        }
        self.by_path
            .insert(note.summary.path.clone(), self.documents.len());
        self.documents.push(Document {
            summary: note.summary,
            preview: note.preview,
            terms: terms.into_boxed_slice(),
            total,
        });
        self.norms_current = false;
    }

    fn remove(&mut self, path: &Path) {
        let Some(index) = self.by_path.remove(path) else {
            return;
        };
        let document = self.documents.swap_remove(index);
        for &(id, _) in document.terms.iter() {
            self.df[id as usize] -= 1;
        }
        if let Some(moved) = self.documents.get(index) {
            self.by_path.insert(moved.summary.path.clone(), index);
        }
        self.norms_current = false;
    }

    /// Bring the IDF table and every norm up to date.
    fn refresh_norms(&mut self) {
        let doc_count = self.note_count().max(1);
        self.idf_by_df = (0..=doc_count)
            .map(|df| {
                if df == 0 {
                    0.0
                } else {
                    (doc_count as f64 / df as f64).ln()
                }
            })
            .collect();
        let norms = self
            .documents
            .par_iter()
            .map(|document| {
                document
                    .terms
                    .iter()
                    .map(|&(id, count)| weight(&self.idf_by_df, &self.df, document, id, count))
                    .filter(|&w| w > 0.0)
                    .map(|w| w * w)
                    .sum::<f64>()
                    .sqrt()
            })
            .collect();
        self.norms = norms;
        self.norms_current = true;
    }

    fn idf(&self, id: u32) -> f64 {
        self.idf_by_df
            .get(self.df[id as usize] as usize)
            .copied()
            .unwrap_or(0.0)
    }

    /// The query's terms present in the corpus, with their TF-IDF weights,
    /// and the query norm. `None` when nothing in it carries weight.
    fn query_vector(&self, query: &str) -> Option<(Vec<(u32, f64)>, f64)> {
        let words = words(query);
        if words.is_empty() {
            return None;
        }
        let total = words.len() + words.len().saturating_sub(1);
        let ids: Vec<Option<u32>> = words
            .iter()
            .map(|w| self.words.get(w.as_str()).copied())
            .collect();
        let mut counts: HashMap<u64, u32> = HashMap::new();
        for id in ids.iter().flatten() {
            *counts.entry(unigram(*id)).or_insert(0) += 1;
        }
        for pair in ids.windows(2) {
            if let [Some(first), Some(second)] = pair {
                *counts.entry(bigram(*first, *second)).or_insert(0) += 1;
            }
        }
        let mut vector: Vec<(u32, f64)> = counts
            .into_iter()
            .filter_map(|(key, count)| {
                let id = *self.term_ids.get(&key)?;
                let w = count as f64 / total as f64 * self.idf(id);
                (w > 0.0).then_some((id, w))
            })
            .collect();
        vector.sort_unstable_by_key(|&(id, _)| id);
        let norm = vector.iter().map(|&(_, w)| w * w).sum::<f64>().sqrt();
        (norm >= f64::EPSILON).then_some((vector, norm))
    }

    /// A document's own vector, in the same form as a query's.
    fn document_vector(&self, index: usize) -> Vec<(u32, f64)> {
        let document = &self.documents[index];
        document
            .terms
            .iter()
            .map(|&(id, count)| (id, weight(&self.idf_by_df, &self.df, document, id, count)))
            .filter(|&(_, w)| w > 0.0)
            .collect()
    }

    /// The dot product of `vector` with document `index`, and the shared
    /// term ids. Both are sorted by term id, so this is one merge.
    fn dot(&self, vector: &[(u32, f64)], index: usize) -> (f64, Vec<(u32, f64)>) {
        let document = &self.documents[index];
        let mut dot = 0.0;
        let mut shared = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < vector.len() && j < document.terms.len() {
            let (qid, qw) = vector[i];
            let (did, count) = document.terms[j];
            match qid.cmp(&did) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    let dw = weight(&self.idf_by_df, &self.df, document, did, count);
                    if dw > 0.0 {
                        dot += qw * dw;
                        shared.push((did, dw));
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        (dot, shared)
    }

    /// Names of the shared terms, most heavily weighted in the document
    /// first, at most ten.
    fn shared_names(&self, mut shared: Vec<(u32, f64)>) -> Vec<String> {
        let mut named: Vec<(f64, String)> = shared
            .drain(..)
            .map(|(id, w)| (w, self.term_name(self.term_keys[id as usize])))
            .collect();
        named.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        named.truncate(10);
        named.into_iter().map(|(_, name)| name).collect()
    }

    fn rank(
        &self,
        vector: &[(u32, f64)],
        query_norm: f64,
        exclude: Option<usize>,
        limit: usize,
    ) -> Vec<SimilarityResult> {
        let mut scored: Vec<(f64, usize)> = (0..self.documents.len())
            .into_par_iter()
            .filter(|&index| Some(index) != exclude && self.norms[index] >= f64::EPSILON)
            .filter_map(|index| {
                let (dot, _) = self.dot(vector, index);
                (dot > 0.0).then(|| (dot / (query_norm * self.norms[index]), index))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    self.documents[a.1]
                        .summary
                        .path
                        .cmp(&self.documents[b.1].summary.path)
                })
        });
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|(score, index)| {
                let document = &self.documents[index];
                let (_, shared) = self.dot(vector, index);
                SimilarityResult {
                    path: document.summary.path.to_string_lossy().to_string(),
                    title: document.summary.title.clone(),
                    score: (score * 10000.0).round() / 10000.0,
                    shared_terms: self.shared_names(shared),
                    preview: document.preview.clone(),
                }
            })
            .collect()
    }
}

/// A term's TF-IDF weight in `document`.
fn weight(idf_by_df: &[f64], df: &[u32], document: &Document, id: u32, count: u32) -> f64 {
    let idf = idf_by_df
        .get(df[id as usize] as usize)
        .copied()
        .unwrap_or(0.0);
    count as f64 / document.total as f64 * idf
}

impl SimilarityEngine {
    /// Build TF-IDF vectors for every note in the vault, from the manager's
    /// parsed note cache (after the freshness gate).
    pub async fn new(manager: Arc<VaultManager>) -> Result<Self> {
        let notes = manager.notes_validated().await;
        let mut prepared: Vec<(PathBuf, Arc<VaultFile>)> = notes
            .into_iter()
            .filter(|file| turbovault_core::is_note(&file.path))
            .map(|file| (PathBuf::from(manager.relative_path(&file.path)), file))
            .collect();
        // Path order, so the corpus is laid out the same way every build.
        prepared.sort_by(|a, b| a.0.cmp(&b.0));

        let corpus = tokio::task::spawn_blocking(move || {
            let tokenized: Vec<Tokenized> = prepared
                .into_par_iter()
                .map(|(rel_path, file)| tokenize_note(rel_path, &file))
                .collect();
            let mut corpus = Corpus::default();
            for note in tokenized {
                corpus.insert(note);
            }
            corpus.refresh_norms();
            corpus
        })
        .await
        .map_err(|e| Error::other(format!("similarity index task failed: {e}")))?;

        Ok(Self {
            manager,
            corpus: RwLock::new(corpus),
        })
    }

    /// Bring the engine up to date with changed notes, `(vault-relative
    /// path, present)`, read from the manager's note cache as it is now, so
    /// applying the same report twice, or out of order, changes nothing.
    pub async fn apply_changes(&self, changes: Vec<(String, bool)>) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut updates: Vec<(PathBuf, Option<Arc<VaultFile>>)> = Vec::new();
        for (rel_path, _present) in changes {
            let full_path = self.manager.vault_path().join(&rel_path);
            if !turbovault_core::is_note(&full_path) {
                continue;
            }
            let rel = PathBuf::from(self.manager.relative_path(&full_path));
            updates.push((rel, self.manager.cached_note(&full_path).await));
        }
        let tokenized: Vec<(PathBuf, Option<Tokenized>)> = tokio::task::spawn_blocking(move || {
            updates
                .into_par_iter()
                .map(|(rel, file)| {
                    let note = file.map(|file| tokenize_note(rel.clone(), &file));
                    (rel, note)
                })
                .collect()
        })
        .await
        .map_err(|e| Error::other(format!("similarity update task failed: {e}")))?;

        let mut corpus = self.write();
        for (rel, note) in tokenized {
            match note {
                Some(note) => corpus.insert(note),
                None => corpus.remove(&rel),
            }
        }
        Ok(())
    }

    /// The corpus with norms current.
    fn read(&self) -> RwLockReadGuard<'_, Corpus> {
        {
            let corpus = self.corpus.read().unwrap_or_else(|p| p.into_inner());
            if corpus.norms_current {
                return corpus;
            }
        }
        self.write().refresh_norms();
        self.corpus.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Corpus> {
        // Every mutation leaves the corpus whole between statements, so a
        // panic elsewhere can't have left it half-updated.
        self.corpus.write().unwrap_or_else(|p| p.into_inner())
    }

    /// Cosine similarity between two notes, or `None` when a ranking from
    /// either one would not list the other: one is missing or has no terms,
    /// or they share no term at all.
    ///
    /// This is one note's score against one other. Getting it from
    /// [`Self::find_similar_notes`] means ranking the whole vault to read off a
    /// single entry.
    pub fn pair_similarity(&self, a: &str, b: &str) -> Option<f64> {
        self.pair_details(a, b).map(|(score, _)| score)
    }

    /// [`Self::pair_similarity`], with the terms `b` shares with `a`, most
    /// heavily weighted in `b` first, at most ten: the score and terms
    /// [`Self::find_similar_notes`] from `a` would list for `b`, before
    /// rounding.
    pub fn pair_details(&self, a: &str, b: &str) -> Option<(f64, Vec<String>)> {
        let corpus = self.read();
        let a = *corpus.by_path.get(Path::new(a))?;
        let b = *corpus.by_path.get(Path::new(b))?;
        let (a_norm, b_norm) = (corpus.norms[a], corpus.norms[b]);
        if a_norm < f64::EPSILON || b_norm < f64::EPSILON {
            return None;
        }
        let (dot, shared) = corpus.dot(&corpus.document_vector(a), b);
        (dot > 0.0).then(|| (dot / (a_norm * b_norm), corpus.shared_names(shared)))
    }

    /// Find notes similar to a query string
    pub fn semantic_search(&self, query: &str, limit: usize) -> Vec<SimilarityResult> {
        let corpus = self.read();
        match corpus.query_vector(query) {
            Some((vector, norm)) => corpus.rank(&vector, norm, None, limit),
            None => vec![],
        }
    }

    /// Find notes most similar to a given note
    pub fn find_similar_notes(&self, path: &str, limit: usize) -> Vec<SimilarityResult> {
        let corpus = self.read();
        let Some(&index) = corpus.by_path.get(Path::new(path)) else {
            return vec![];
        };
        let norm = corpus.norms[index];
        if norm < f64::EPSILON {
            return vec![];
        }
        corpus.rank(&corpus.document_vector(index), norm, Some(index), limit)
    }

    /// Every note the engine holds, in path order, with what duplicate
    /// detection needs of it.
    pub(crate) fn note_summaries(&self) -> Vec<NoteSummary> {
        let corpus = self.read();
        let mut summaries: Vec<NoteSummary> = corpus
            .documents
            .iter()
            .map(|document| document.summary.clone())
            .collect();
        summaries.sort_by(|a, b| a.path.cmp(&b.path));
        summaries
    }

    /// Get the document count (for diagnostics): notes with at least one term.
    pub fn document_count(&self) -> usize {
        self.read()
            .documents
            .iter()
            .filter(|document| document.total > 0)
            .count()
    }
}

/// The words a text is indexed by: split on whitespace and ASCII
/// punctuation, at least three bytes long, lowercased, stopwords dropped.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .filter(|w| !is_stopword(w))
        .collect()
}

/// Tokenize text into lowercase terms, filtering stopwords and short words.
/// Generates both unigrams and bigrams for better semantic capture. The
/// engine builds the same terms as keys; this spells them out for tests.
#[cfg(test)]
pub(crate) fn tokenize(text: &str) -> Vec<String> {
    let words = words(text);
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

    /// The engine as it was: a string-keyed TF-IDF map per note, rebuilt
    /// from every note, ranking by a scan. The new engine must give the
    /// same scores and the same results.
    struct Reference {
        docs: Vec<(String, HashMap<String, f64>, f64)>,
        idf: HashMap<String, f64>,
    }

    impl Reference {
        fn new(notes: &[(String, String)]) -> Self {
            let doc_count = notes.len().max(1);
            let mut df: HashMap<String, usize> = HashMap::new();
            let mut counted = Vec::new();
            for (path, plain) in notes {
                let mut counts: HashMap<String, usize> = HashMap::new();
                for token in tokenize(plain) {
                    *counts.entry(token).or_insert(0) += 1;
                }
                for term in counts.keys() {
                    *df.entry(term.clone()).or_insert(0) += 1;
                }
                counted.push((path.clone(), counts));
            }
            let idf: HashMap<String, f64> = df
                .into_iter()
                .map(|(t, c)| (t, (doc_count as f64 / c as f64).ln()))
                .collect();
            let docs = counted
                .into_iter()
                .filter_map(|(path, counts)| {
                    let total: usize = counts.values().sum();
                    (total > 0).then(|| {
                        let tfidf: HashMap<String, f64> = counts
                            .iter()
                            .map(|(t, c)| (t.clone(), *c as f64 / total as f64 * idf[t]))
                            .filter(|(_, w)| *w > 0.0)
                            .collect();
                        let norm = tfidf.values().map(|w| w * w).sum::<f64>().sqrt();
                        (path, tfidf, norm)
                    })
                })
                .collect();
            Self { docs, idf }
        }

        fn rank(
            &self,
            query: &HashMap<String, f64>,
            norm: f64,
            skip: Option<&str>,
        ) -> Vec<(String, f64, usize)> {
            let mut out: Vec<(String, f64, usize)> = self
                .docs
                .iter()
                .filter(|(path, _, n)| Some(path.as_str()) != skip && *n >= f64::EPSILON)
                .filter_map(|(path, tfidf, n)| {
                    let shared: Vec<&String> =
                        query.keys().filter(|t| tfidf.contains_key(*t)).collect();
                    let dot: f64 = shared.iter().map(|t| query[*t] * tfidf[*t]).sum();
                    (dot > 0.0).then(|| (path.clone(), dot / (norm * n), shared.len()))
                })
                .collect();
            out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then_with(|| a.0.cmp(&b.0)));
            out
        }

        fn search(&self, query: &str) -> Vec<(String, f64, usize)> {
            let tokens = tokenize(query);
            let mut counts: HashMap<String, usize> = HashMap::new();
            for t in &tokens {
                *counts.entry(t.clone()).or_insert(0) += 1;
            }
            let q: HashMap<String, f64> = counts
                .iter()
                .map(|(t, c)| {
                    (
                        t.clone(),
                        *c as f64 / tokens.len() as f64 * self.idf.get(t).copied().unwrap_or(0.0),
                    )
                })
                .filter(|(_, w)| *w > 0.0)
                .collect();
            let norm = q.values().map(|w| w * w).sum::<f64>().sqrt();
            if norm < f64::EPSILON {
                return vec![];
            }
            self.rank(&q, norm, None)
        }

        fn similar(&self, path: &str) -> Vec<(String, f64, usize)> {
            match self.docs.iter().find(|(p, _, _)| p == path) {
                Some((_, tfidf, norm)) => self.rank(tfidf, *norm, Some(path)),
                None => vec![],
            }
        }
    }

    /// Notes over a small vocabulary, so they share words and pairs.
    fn synthetic_notes(count: usize, seed: u64) -> Vec<(String, String)> {
        let vocab = [
            "ownership",
            "borrow",
            "checker",
            "memory",
            "garden",
            "tomato",
            "watering",
            "bread",
            "starter",
            "dough",
            "proof",
            "rust",
            "safety",
            "vines",
            "stake",
            "oven",
            "flour",
            "lifetime",
            "trait",
            "async",
        ];
        let mut state = seed;
        let mut next = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        (0..count)
            .map(|i| {
                let words = if i % 17 == 0 { 0 } else { 3 + next(25) };
                let text: Vec<&str> = (0..words).map(|_| vocab[next(vocab.len())]).collect();
                (
                    format!("n{i:03}.md"),
                    format!("# Note {i}\n\n{}\n", text.join(" ")),
                )
            })
            .collect()
    }

    async fn vault_with(notes: &[(String, String)]) -> (tempfile::TempDir, Arc<VaultManager>) {
        let temp = tempfile::TempDir::new().unwrap();
        for (path, content) in notes {
            std::fs::write(temp.path().join(path), content).unwrap();
        }
        let mut config = turbovault_core::ConfigProfile::Development.create_config();
        config.vaults.push(
            turbovault_core::VaultConfig::builder("sim", temp.path())
                .build()
                .unwrap(),
        );
        let manager = VaultManager::new(config).unwrap();
        manager.initialize().await.unwrap();
        (temp, Arc::new(manager))
    }

    /// The plain text the engine indexes each note by, for the reference.
    async fn plain_texts(manager: &VaultManager) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = manager
            .cached_notes()
            .await
            .iter()
            .map(|f| (manager.relative_path(&f.path), to_plain_text(&f.content)))
            .collect();
        out.sort();
        out
    }

    fn assert_same(found: &[SimilarityResult], expected: &[(String, f64, usize)], what: &str) {
        assert_eq!(found.len(), expected.len(), "{what}: result count");
        for (r, (path, score, shared)) in found.iter().zip(expected) {
            assert!(
                (r.score - (score * 10000.0).round() / 10000.0).abs() <= 1e-4 + 1e-12,
                "{what}: {} scored {} against {path} {score}",
                r.path,
                r.score
            );
            assert_eq!(
                r.shared_terms.len(),
                (*shared).min(10),
                "{what}: shared terms of {}",
                r.path
            );
        }
        // Same notes, whatever order exact ties fall in.
        let mut a: Vec<&str> = found.iter().map(|r| r.path.as_str()).collect();
        let mut b: Vec<&str> = expected.iter().map(|e| e.0.as_str()).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "{what}: result set");
    }

    #[tokio::test]
    async fn matches_the_string_keyed_engine() {
        let notes = synthetic_notes(150, 0x9e37_79b9_7f4a_7c15);
        let (_temp, manager) = vault_with(&notes).await;
        let engine = SimilarityEngine::new(Arc::clone(&manager)).await.unwrap();
        let reference = Reference::new(&plain_texts(&manager).await);

        for query in [
            "ownership borrow",
            "garden tomato watering",
            "oven",
            "missing words",
            "rust rust safety",
        ] {
            assert_same(
                &engine.semantic_search(query, 1000),
                &reference.search(query),
                query,
            );
        }
        for (path, _) in notes.iter().step_by(7) {
            assert_same(
                &engine.find_similar_notes(path, 1000),
                &reference.similar(path),
                path,
            );
        }
        for (a, _) in notes.iter().step_by(11) {
            for (path, score, _) in reference.similar(a) {
                let pair = engine
                    .pair_similarity(a, &path)
                    .expect("ranked pair has a score");
                assert!(
                    (pair - score).abs() < 1e-12,
                    "{a} vs {path}: {pair} != {score}"
                );
            }
        }
    }

    #[tokio::test]
    async fn applying_changes_matches_a_fresh_build() {
        let notes = synthetic_notes(80, 0x2545_f491_4f6c_dd1d);
        let (temp, manager) = vault_with(&notes).await;
        let engine = SimilarityEngine::new(Arc::clone(&manager)).await.unwrap();

        // Edit, delete and add notes outside the engine, then report them.
        let edited = synthetic_notes(10, 7);
        let mut changes = Vec::new();
        for (i, (_, content)) in edited.iter().enumerate() {
            let path = format!("n{:03}.md", i * 3);
            std::fs::write(temp.path().join(&path), content).unwrap();
            changes.push((path, true));
        }
        for i in [1usize, 2, 40] {
            let path = format!("n{i:03}.md");
            std::fs::remove_file(temp.path().join(&path)).unwrap();
            changes.push((path, false));
        }
        std::fs::write(
            temp.path().join("added.md"),
            "# Added\n\nrust ownership garden\n",
        )
        .unwrap();
        changes.push(("added.md".to_string(), true));
        manager.reconcile_now().await;
        engine.apply_changes(changes.clone()).await.unwrap();
        // Applying a report again changes nothing.
        engine.apply_changes(changes).await.unwrap();

        let fresh = SimilarityEngine::new(Arc::clone(&manager)).await.unwrap();
        let reference = Reference::new(&plain_texts(&manager).await);
        assert_eq!(engine.document_count(), fresh.document_count());
        for query in ["ownership borrow", "rust ownership garden", "bread starter"] {
            let expected = reference.search(query);
            assert_same(&engine.semantic_search(query, 1000), &expected, query);
            assert_same(&fresh.semantic_search(query, 1000), &expected, query);
        }
        for path in ["n000.md", "n005.md", "added.md"] {
            assert_same(
                &engine.find_similar_notes(path, 1000),
                &reference.similar(path),
                path,
            );
        }
    }
}
