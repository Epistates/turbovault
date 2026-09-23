//! Ties chunking, embedding, the dense index, and the lexical index together
//! into one incrementally-updatable hybrid search engine.
//!
//! [`IndexEngine`] does no I/O of its own: a caller feeds it note content
//! through [`IndexEngine::update_note`] and [`IndexEngine::remove_note`], and
//! reads back what to persist through [`IndexEngine::snapshot`]. Deciding
//! *which* notes are worth re-reading, and actually reading and writing
//! anything, is the caller's job — for the compiled-in plugin, that is
//! `turbovault-plugin-vector`, driven by the change feed and
//! `list_notes_detailed`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tokio::sync::Mutex;

use crate::chunk::{chunk_text, content_hash, diff_chunks};
use crate::config::VectorConfig;
use crate::dense::DenseIndex;
use crate::embedding::EmbeddingEngine;
use crate::error::{Result, VectorError};
use crate::lexical::LexicalIndex;
use crate::router::{FusedResult, reciprocal_rank_fusion};
use crate::store::{ChunkRecord, NoteRecord};

/// Characters of chunk text kept in a [`SearchHit::preview`].
const PREVIEW_CHARS: usize = 200;

/// One hybrid search hit: the best-matching chunk from one note.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    /// Vault-relative path of the matching note.
    pub path: String,
    /// A prefix of the matching chunk's text.
    pub preview: String,
    /// Human-readable position, e.g. `"chunk 2 of 5"`.
    pub chunk_position: String,
    /// Fused (or, in dense-only mode, cosine similarity) score. Higher is
    /// more relevant; not comparable across calls with different config.
    pub score: f64,
    /// 0-based rank on the dense side, if it matched there.
    pub dense_rank: Option<usize>,
    /// 0-based rank on the lexical side, if it matched there (always `None`
    /// when the search was not run in hybrid mode).
    pub lexical_rank: Option<usize>,
}

/// Snapshot of engine-wide counters, for the plugin's `status` tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndexStats {
    /// [`EmbeddingEngine::model_name`] of the loaded embedder.
    pub model: String,
    /// Dimensionality of every stored vector.
    pub dims: usize,
    /// Number of notes with at least one indexed chunk.
    pub indexed_notes: usize,
    /// Total number of indexed chunks across all notes.
    pub indexed_chunks: usize,
}

struct EngineState {
    notes: HashMap<String, NoteRecord>,
    /// Reverse lookup from chunk id to owning note path, so a search hit
    /// (which only has chunk ids from the dense/lexical indices) can find its
    /// note without scanning every note's chunk list.
    chunk_owner: HashMap<u64, String>,
    dense: DenseIndex,
    lexical: LexicalIndex,
}

impl EngineState {
    fn new(dims: usize, config: &VectorConfig) -> Self {
        Self {
            notes: HashMap::new(),
            chunk_owner: HashMap::new(),
            dense: DenseIndex::new(
                dims,
                config.ann_max_nb_connection,
                config.ann_ef_construction,
                config.ann_ef_search,
            ),
            lexical: LexicalIndex::new(),
        }
    }
}

/// Incrementally-updatable hybrid (dense + lexical) search engine over a
/// vault's notes.
pub struct IndexEngine {
    embedder: Arc<dyn EmbeddingEngine>,
    config: VectorConfig,
    state: Mutex<EngineState>,
    next_chunk_id: AtomicU64,
}

impl IndexEngine {
    /// Construct an empty engine.
    pub fn new(embedder: Arc<dyn EmbeddingEngine>, config: VectorConfig) -> Self {
        let dims = embedder.dimensions();
        let state = Mutex::new(EngineState::new(dims, &config));
        Self {
            embedder,
            config,
            state,
            next_chunk_id: AtomicU64::new(1),
        }
    }

    /// The loaded embedder's model name.
    pub fn model_name(&self) -> &str {
        self.embedder.model_name()
    }

    /// The loaded embedder's vector width.
    pub fn dims(&self) -> usize {
        self.embedder.dimensions()
    }

    /// Rebuild engine state from a persisted snapshot. Calls the embedder for
    /// nothing: every vector comes from `notes`, which is the whole point of
    /// persisting them.
    pub async fn restore(&self, notes: Vec<NoteRecord>, next_chunk_id: u64) -> Result<()> {
        let dims = self.embedder.dimensions();
        let mut dense = DenseIndex::new(
            dims,
            self.config.ann_max_nb_connection,
            self.config.ann_ef_construction,
            self.config.ann_ef_search,
        );
        let mut chunk_owner = HashMap::new();
        let mut lexical_chunks = Vec::new();
        for note in &notes {
            for chunk in &note.chunks {
                if chunk.vector.len() != dims {
                    return Err(VectorError::Snapshot(format!(
                        "chunk {} in {:?} has {} dimensions, expected {dims}; the model changed \
                         since this snapshot was written, or it is corrupt",
                        chunk.id,
                        note.path,
                        chunk.vector.len()
                    )));
                }
                dense.upsert(chunk.id, chunk.vector.clone())?;
                lexical_chunks.push((chunk.id, chunk.text.clone()));
                chunk_owner.insert(chunk.id, note.path.clone());
            }
        }
        let lexical = LexicalIndex::from_chunks(lexical_chunks);

        let mut state = self.state.lock().await;
        state.notes = notes
            .into_iter()
            .map(|note| (note.path.clone(), note))
            .collect();
        state.chunk_owner = chunk_owner;
        state.dense = dense;
        state.lexical = lexical;
        drop(state);
        self.next_chunk_id
            .store(next_chunk_id.max(1), Ordering::SeqCst);
        Ok(())
    }

    /// Update (or create) one note's chunks from its raw markdown content.
    /// Only chunks whose content actually changed are re-embedded; unchanged
    /// chunks reuse their stored vector. Returns `false` without touching
    /// either index when the note's plain-text content hash matches what is
    /// already stored (the common case on a reconcile pass: most notes
    /// listed as "maybe changed" by size/mtime are not).
    pub async fn update_note(&self, path: &str, content: &str) -> Result<bool> {
        let plain = turbovault_parser::to_plain_text(content);
        let file_hash = content_hash(plain.as_bytes());

        let mut state = self.state.lock().await;
        if state
            .notes
            .get(path)
            .is_some_and(|existing| existing.content_hash == file_hash)
        {
            return Ok(false);
        }

        let ranges = chunk_text(
            &plain,
            self.config.chunk_max_chars,
            self.config.chunk_overlap_chars,
        );
        let chunk_texts: Vec<&str> = ranges
            .iter()
            .map(|&(start, end)| &plain[start..end])
            .collect();
        let new_hashes: Vec<String> = chunk_texts
            .iter()
            .map(|text| content_hash(text.as_bytes()))
            .collect();

        let existing_chunks: &[ChunkRecord] = state
            .notes
            .get(path)
            .map(|existing| existing.chunks.as_slice())
            .unwrap_or_default();
        let old_hashes: Vec<(u64, String)> = existing_chunks
            .iter()
            .map(|chunk| (chunk.id, chunk.content_hash.clone()))
            .collect();
        let old_vectors: HashMap<u64, Vec<f32>> = existing_chunks
            .iter()
            .map(|chunk| (chunk.id, chunk.vector.clone()))
            .collect();
        let (reuse, stale) = diff_chunks(&old_hashes, &new_hashes);

        let embed_positions: Vec<usize> = reuse
            .iter()
            .enumerate()
            .filter_map(|(index, id)| id.is_none().then_some(index))
            .collect();
        let texts_to_embed: Vec<String> = embed_positions
            .iter()
            .map(|&index| chunk_texts[index].to_string())
            .collect();
        let new_vectors = if texts_to_embed.is_empty() {
            Vec::new()
        } else {
            self.embedder.embed(&texts_to_embed).await?
        };
        if new_vectors.len() != texts_to_embed.len() {
            return Err(VectorError::Embedding(format!(
                "embedder returned {} vectors for {} inputs",
                new_vectors.len(),
                texts_to_embed.len()
            )));
        }
        let mut new_vectors = embed_positions.into_iter().zip(new_vectors);
        let mut next_new = new_vectors.next();

        let total_chunks = ranges.len() as u32;
        let mut new_chunks = Vec::with_capacity(ranges.len());
        for (index, (&(start, end), hash)) in ranges.iter().zip(new_hashes.iter()).enumerate() {
            let (id, vector) = match reuse[index] {
                Some(existing_id) => {
                    let vector = old_vectors.get(&existing_id).cloned().ok_or_else(|| {
                        VectorError::Index(format!(
                            "chunk {existing_id} was marked reusable but had no stored vector"
                        ))
                    })?;
                    (existing_id, vector)
                }
                None => {
                    let (position, vector) = next_new.take().ok_or_else(|| {
                        VectorError::Index(
                            "ran out of freshly-embedded vectors mid-chunk".to_string(),
                        )
                    })?;
                    debug_assert_eq!(position, index);
                    next_new = new_vectors.next();
                    (self.next_chunk_id.fetch_add(1, Ordering::SeqCst), vector)
                }
            };
            new_chunks.push(ChunkRecord {
                id,
                chunk_index: index as u32,
                total_chunks,
                start_byte: start as u64,
                end_byte: end as u64,
                content_hash: hash.clone(),
                text: chunk_texts[index].to_string(),
                vector,
            });
        }

        for id in &stale {
            state.dense.remove(*id);
            state.lexical.remove(*id);
            state.chunk_owner.remove(id);
        }
        for chunk in &new_chunks {
            state.dense.upsert(chunk.id, chunk.vector.clone())?;
            state.lexical.upsert(chunk.id, &chunk.text);
            state.chunk_owner.insert(chunk.id, path.to_string());
        }
        state.notes.insert(
            path.to_string(),
            NoteRecord {
                path: path.to_string(),
                content_hash: file_hash,
                chunks: new_chunks,
            },
        );
        Ok(true)
    }

    /// Remove a note and every one of its chunks. Returns whether the note
    /// was present.
    pub async fn remove_note(&self, path: &str) -> bool {
        let mut state = self.state.lock().await;
        let Some(note) = state.notes.remove(path) else {
            return false;
        };
        for chunk in &note.chunks {
            state.dense.remove(chunk.id);
            state.lexical.remove(chunk.id);
            state.chunk_owner.remove(&chunk.id);
        }
        true
    }

    /// Hybrid or dense-only search. `hybrid` fuses BM25 lexical ranking in
    /// with Reciprocal Rank Fusion; without it this is plain cosine-ranked
    /// dense search. Results are deduplicated to the single best-matching
    /// chunk per note.
    pub async fn search(&self, query: &str, limit: usize, hybrid: bool) -> Result<Vec<SearchHit>> {
        let limit = limit.max(1);
        let overfetch = limit
            .saturating_mul(self.config.search_overfetch_factor.max(1))
            .max(limit);

        // The embedder is a separate Arc from `state`, so this runs before
        // taking the lock: a slow embed of the query text should not block
        // concurrent updates or other searches any longer than necessary.
        let query_vector = self
            .embedder
            .embed(std::slice::from_ref(&query.to_string()))
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();

        let mut state = self.state.lock().await;
        let min_similarity = self.config.min_similarity;
        let dense_hits = state.dense.search(&query_vector, overfetch)?;
        let (dense_ranked, dense_chunk_of) = dedup_best_per_note(
            dense_hits
                .into_iter()
                .filter(|&(_, score)| score >= min_similarity),
            &state.chunk_owner,
        );

        let (lexical_ranked, lexical_chunk_of) = if hybrid {
            let lexical_hits = state.lexical.search(query, overfetch);
            dedup_best_per_note(lexical_hits.into_iter(), &state.chunk_owner)
        } else {
            (Vec::new(), HashMap::new())
        };

        let fused: Vec<FusedResult<String>> = if hybrid {
            reciprocal_rank_fusion(
                &dense_ranked,
                &lexical_ranked,
                1.0 - self.config.lexical_weight as f64,
                self.config.lexical_weight as f64,
                self.config.rrf_k,
            )
        } else {
            dense_ranked
                .iter()
                .enumerate()
                .map(|(rank, (path, score))| FusedResult {
                    key: path.clone(),
                    score: f64::from(*score),
                    primary_rank: Some(rank),
                    secondary_rank: None,
                })
                .collect()
        };

        let mut hits = Vec::with_capacity(limit.min(fused.len()));
        for result in fused.into_iter().take(limit) {
            let Some(&chunk_id) = dense_chunk_of
                .get(&result.key)
                .or_else(|| lexical_chunk_of.get(&result.key))
            else {
                continue;
            };
            let Some(note) = state.notes.get(&result.key) else {
                continue;
            };
            let Some(chunk) = note.chunks.iter().find(|chunk| chunk.id == chunk_id) else {
                continue;
            };
            hits.push(SearchHit {
                path: result.key,
                preview: preview_of(&chunk.text),
                chunk_position: format!(
                    "chunk {} of {}",
                    chunk.chunk_index + 1,
                    chunk.total_chunks
                ),
                score: result.score,
                dense_rank: result.primary_rank,
                lexical_rank: result.secondary_rank,
            });
        }
        Ok(hits)
    }

    /// Every currently-indexed note, and the chunk-id allocator's next value,
    /// for a caller to persist.
    pub async fn snapshot(&self) -> (Vec<NoteRecord>, u64) {
        let state = self.state.lock().await;
        (
            state.notes.values().cloned().collect(),
            self.next_chunk_id.load(Ordering::SeqCst),
        )
    }

    /// One note's current record, for a caller persisting incrementally
    /// (write only what changed rather than the whole snapshot).
    pub async fn snapshot_note(&self, path: &str) -> Option<NoteRecord> {
        self.state.lock().await.notes.get(path).cloned()
    }

    /// Engine-wide counters for the plugin's `status` tool.
    pub async fn stats(&self) -> IndexStats {
        let state = self.state.lock().await;
        IndexStats {
            model: self.embedder.model_name().to_string(),
            dims: self.embedder.dimensions(),
            indexed_notes: state.notes.len(),
            indexed_chunks: state.notes.values().map(|note| note.chunks.len()).sum(),
        }
    }
}

/// Deduplicate a stream of `(chunk_id, score)` hits, already sorted best
/// first, to the single best chunk per owning note. Returns the note-ranked
/// `(path, score)` list (in the input's order, so still best-first) and a
/// `path -> chunk_id` map for looking the winning chunk back up.
fn dedup_best_per_note(
    hits: impl Iterator<Item = (u64, f32)>,
    chunk_owner: &HashMap<u64, String>,
) -> (Vec<(String, f32)>, HashMap<String, u64>) {
    let mut ranked = Vec::new();
    let mut best_chunk = HashMap::new();
    for (chunk_id, score) in hits {
        let Some(path) = chunk_owner.get(&chunk_id) else {
            continue;
        };
        if best_chunk.contains_key(path) {
            continue;
        }
        best_chunk.insert(path.clone(), chunk_id);
        ranked.push((path.clone(), score));
    }
    (ranked, best_chunk)
}

/// A prefix of `text`, truncated on a char boundary.
fn preview_of(text: &str) -> String {
    match text.char_indices().nth(PREVIEW_CHARS) {
        Some((byte_index, _)) => text[..byte_index].to_string(),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::testing::{CountingEmbedder, DeterministicEmbedder};

    fn config() -> VectorConfig {
        VectorConfig {
            chunk_max_chars: 30,
            chunk_overlap_chars: 0,
            ..VectorConfig::default()
        }
    }

    const S1: &str = "Alpha unique text here.";
    const S2: &str = "Beta unique text here.";
    const S3: &str = "Gamma unique text here.";
    const S2_CHANGED: &str = "Beta changed text here.";

    fn three_sentences() -> String {
        format!("{S1} {S2} {S3}")
    }

    #[tokio::test]
    async fn initial_index_embeds_every_chunk() {
        let embedder = Arc::new(CountingEmbedder::new(16));
        let engine = IndexEngine::new(embedder.clone(), config());
        let changed = engine
            .update_note("note.md", &three_sentences())
            .await
            .unwrap();
        assert!(changed);
        assert_eq!(
            embedder.count(),
            3,
            "all three sentences should be embedded once"
        );
    }

    #[tokio::test]
    async fn re_embeds_only_the_changed_chunk() {
        let embedder = Arc::new(CountingEmbedder::new(16));
        let engine = IndexEngine::new(embedder.clone(), config());
        engine
            .update_note("note.md", &three_sentences())
            .await
            .unwrap();
        assert_eq!(embedder.count(), 3);

        let changed_text = format!("{S1} {S2_CHANGED} {S3}");
        let changed = engine.update_note("note.md", &changed_text).await.unwrap();
        assert!(changed);
        assert_eq!(
            embedder.count(),
            4,
            "only the one changed sentence should be re-embedded"
        );
    }

    #[tokio::test]
    async fn identical_content_is_a_no_op() {
        let embedder = Arc::new(CountingEmbedder::new(16));
        let engine = IndexEngine::new(embedder.clone(), config());
        engine
            .update_note("note.md", &three_sentences())
            .await
            .unwrap();
        assert_eq!(embedder.count(), 3);

        let changed = engine
            .update_note("note.md", &three_sentences())
            .await
            .unwrap();
        assert!(!changed, "unchanged content must not re-chunk or re-embed");
        assert_eq!(embedder.count(), 3);
    }

    #[tokio::test]
    async fn remove_note_drops_its_chunks_from_search() {
        let embedder = Arc::new(DeterministicEmbedder::new(16));
        let engine = IndexEngine::new(embedder, config());
        engine
            .update_note("note.md", &three_sentences())
            .await
            .unwrap();
        assert!(!engine.search(S1, 5, false).await.unwrap().is_empty());

        assert!(engine.remove_note("note.md").await);
        assert!(engine.search(S1, 5, false).await.unwrap().is_empty());
        assert!(
            !engine.remove_note("note.md").await,
            "second removal reports absence"
        );
    }

    #[tokio::test]
    async fn search_finds_the_note_containing_the_query_text() {
        let embedder = Arc::new(DeterministicEmbedder::new(32));
        let engine = IndexEngine::new(embedder, config());
        engine.update_note("alpha.md", S1).await.unwrap();
        engine.update_note("beta.md", S2).await.unwrap();

        let hits = engine.search(S1, 1, false).await.unwrap();
        assert_eq!(hits[0].path, "alpha.md");
    }

    #[tokio::test]
    async fn hybrid_search_still_finds_an_exact_lexical_match() {
        let embedder = Arc::new(DeterministicEmbedder::new(32));
        let engine = IndexEngine::new(embedder, config());
        engine.update_note("alpha.md", S1).await.unwrap();
        engine.update_note("beta.md", S2).await.unwrap();

        let hits = engine.search("unique", 2, true).await.unwrap();
        assert_eq!(hits.len(), 2, "both notes share the word 'unique'");
    }

    #[tokio::test]
    async fn snapshot_round_trips_through_restore() {
        let embedder = Arc::new(DeterministicEmbedder::new(16));
        let engine = IndexEngine::new(embedder.clone(), config());
        engine
            .update_note("note.md", &three_sentences())
            .await
            .unwrap();
        let (notes, next_id) = engine.snapshot().await;
        assert_eq!(notes.len(), 1);

        let restored = IndexEngine::new(embedder, config());
        restored.restore(notes, next_id).await.unwrap();
        let stats = restored.stats().await;
        assert_eq!(stats.indexed_notes, 1);
        assert_eq!(stats.indexed_chunks, 3);

        let hits = restored.search(S1, 1, false).await.unwrap();
        assert_eq!(hits[0].path, "note.md");
    }

    #[tokio::test]
    async fn restore_rejects_a_dimension_mismatch() {
        let embedder = Arc::new(DeterministicEmbedder::new(16));
        let engine = IndexEngine::new(embedder, config());
        let bad_note = NoteRecord {
            path: "note.md".to_string(),
            content_hash: "x".to_string(),
            chunks: vec![ChunkRecord {
                id: 1,
                chunk_index: 0,
                total_chunks: 1,
                start_byte: 0,
                end_byte: 1,
                content_hash: "x".to_string(),
                text: "x".to_string(),
                vector: vec![0.0, 1.0], // 2 dims, engine expects 16
            }],
        };
        assert!(engine.restore(vec![bad_note], 2).await.is_err());
    }
}
