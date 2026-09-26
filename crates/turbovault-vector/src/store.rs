//! Persisted schema for the index. This crate does not perform I/O itself
//! (see the crate-level docs for why); these types are what a caller
//! serializes (with `serde_json`, matching the convention the plugin
//! boundary already uses for `EventCursor`) into whatever storage it has,
//! and what [`crate::IndexEngine::restore`] rebuilds engine state from.

use serde::{Deserialize, Serialize};

/// One indexed chunk: its position within its note, its content hash for
/// change detection, its text (kept in full, not just a preview, so the
/// lexical index can be rebuilt from a persisted snapshot without re-reading
/// the vault), and its dense embedding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ChunkRecord {
    /// Stable id, unique within the engine, used as the key in both the
    /// dense and lexical indices.
    pub id: u64,
    /// Zero-based position among this note's chunks.
    pub chunk_index: u32,
    /// Total number of chunks this note currently has.
    pub total_chunks: u32,
    /// Byte offset of this chunk's start within the note's plain-text form.
    pub start_byte: u64,
    /// Byte offset of this chunk's end within the note's plain-text form.
    pub end_byte: u64,
    /// Content hash of this chunk's text, used by [`crate::chunk::diff_chunks`]
    /// to decide whether it needs re-embedding.
    pub content_hash: String,
    /// The chunk's full text.
    pub text: String,
    /// The chunk's dense embedding.
    pub vector: Vec<f32>,
}

/// All of one note's chunks.
///
/// Deciding *which* notes are worth re-reading from the vault at all is the
/// caller's job: the plugin boundary already has a purpose-built type for
/// that (`turbovault_plugin_api::NoteListing`, compared with
/// `looks_unchanged_from` against `list_notes_detailed`), so this crate does
/// not duplicate it. `content_hash` here is a second, cheaper filter *after*
/// that: it is the hash of the plain text [`crate::engine::IndexEngine`]
/// actually chunked, so a note whose size or mtime moved but whose content
/// did not (a touch, a re-save with no edits) still short-circuits before
/// re-chunking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NoteRecord {
    /// Vault-relative path.
    pub path: String,
    /// Content hash of the plain text these chunks were cut from.
    pub content_hash: String,
    /// This note's chunks, in order.
    pub chunks: Vec<ChunkRecord>,
}

/// Engine-wide metadata: which model produced the stored vectors (so a
/// config change that swaps models is detectable rather than silently mixing
/// embedding spaces), and the chunk-id allocator's high-water mark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct IndexMeta {
    /// [`crate::EmbeddingEngine::model_name`] of the model that produced
    /// every vector currently stored.
    pub model: String,
    /// Dimensionality every stored vector has.
    pub dims: usize,
    /// Next unused chunk id. Persisted so a restart never reissues an id
    /// still referenced by a chunk it has not yet re-read.
    pub next_chunk_id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_records_round_trip_through_json() {
        let record = NoteRecord {
            path: "inbox.md".to_string(),
            content_hash: "notehash".to_string(),
            chunks: vec![ChunkRecord {
                id: 7,
                chunk_index: 0,
                total_chunks: 1,
                start_byte: 0,
                end_byte: 5,
                content_hash: "abc".to_string(),
                text: "hello".to_string(),
                vector: vec![0.1, 0.2],
            }],
        };
        let encoded = serde_json::to_vec(&record).unwrap();
        let decoded: NoteRecord = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, record);
    }
}
