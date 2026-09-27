//! Lexical (BM25) side of hybrid search, backed by the [`bm25`] crate.
//!
//! Unlike the dense side, `bm25::SearchEngine` supports true incremental
//! `upsert`/`remove` with no rebuild step, so this wrapper is a thin,
//! direct pass-through. Its own documentation notes that upserting after
//! construction lets the corpus's true average document length drift from
//! what the engine was built with, which makes BM25 scores gradually less
//! precise; [`LexicalIndex::from_chunks`] is how a caller re-fits that
//! statistic from the current corpus (the engine does this on every full
//! reindex, same as it does for the dense side's HNSW graph).
//!
//! TurboVault already has a full-text engine (Tantivy, behind the host's own
//! search tool), but the compiled-in plugin boundary has no passthrough to it
//! today, and adding one would be a separate, larger change to the plugin
//! contract itself. `bm25` is a small, pure-Rust, in-memory BM25
//! implementation with native incremental upsert/remove, which is a better
//! fit for a self-contained plugin than either hand-rolling the formula or
//! standing up a second on-disk Tantivy index just for this.

use bm25::{Document, Language, LanguageMode, SearchEngine, SearchEngineBuilder};

/// Lexical (BM25) index over chunk text.
pub struct LexicalIndex {
    engine: SearchEngine<u64>,
}

impl LexicalIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::from_chunks(std::iter::empty())
    }

    /// Build (or rebuild) an index from a known corpus, fitting BM25's
    /// average-document-length statistic to it.
    pub fn from_chunks(chunks: impl IntoIterator<Item = (u64, String)>) -> Self {
        let documents = chunks.into_iter().map(|(id, text)| Document::new(id, text));
        let engine = SearchEngineBuilder::<u64>::with_documents(
            LanguageMode::Fixed(Language::English),
            documents,
        )
        .build();
        Self { engine }
    }

    /// Insert or replace a chunk's text.
    pub fn upsert(&mut self, id: u64, text: &str) {
        self.engine.upsert(Document::new(id, text.to_string()));
    }

    /// Remove a chunk, if present.
    pub fn remove(&mut self, id: u64) {
        self.engine.remove(&id);
    }

    /// Number of chunks currently indexed.
    pub fn len(&self) -> usize {
        self.engine.iter().count()
    }

    /// Whether the index holds no chunks.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Search for the `limit` best-matching chunks. Returns `(chunk_id,
    /// bm25_score)` pairs, highest score first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<(u64, f32)> {
        self.engine
            .search(query, limit)
            .into_iter()
            .map(|result| (result.document.id, result.score))
            .collect()
    }
}

impl Default for LexicalIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_ranks_the_matching_document_first() {
        let index = LexicalIndex::from_chunks([
            (1, "the quick brown fox jumps over the lazy dog".to_string()),
            (
                2,
                "an entirely unrelated paragraph about gardening".to_string(),
            ),
        ]);
        let results = index.search("quick fox", 2);
        assert_eq!(results[0].0, 1);
        assert!(results[0].1 > 0.0);
    }

    #[test]
    fn removed_chunks_are_not_returned() {
        let mut index = LexicalIndex::from_chunks([(1, "hello world".to_string())]);
        assert_eq!(index.len(), 1);
        index.remove(1);
        assert!(index.is_empty());
        assert!(index.search("hello", 5).is_empty());
    }

    #[test]
    fn upsert_replaces_existing_content() {
        let mut index = LexicalIndex::from_chunks([(1, "alpha".to_string())]);
        index.upsert(1, "beta");
        assert!(index.search("alpha", 5).is_empty());
        assert_eq!(index.search("beta", 5).first().map(|(id, _)| *id), Some(1));
    }

    #[test]
    fn empty_index_returns_no_results() {
        let index = LexicalIndex::new();
        assert!(index.search("anything", 5).is_empty());
    }
}
