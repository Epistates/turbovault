# turbovault-vector

Optional hybrid (dense + lexical) semantic search engine for TurboVault.

This crate is the engine only: it chunks note text, embeds and indexes it,
fuses dense and lexical rankings, and hands back what to persist. It does not
read notes, does not talk to a vault, and does not know what `PluginStore` is.
`crates/plugins/turbovault-plugin-vector` is the host that wires it to
TurboVault's compiled-in plugin boundary, reading notes through `VaultApi`,
persisting snapshots through `PluginStorage`, and advertising the
`vector_search_*` MCP tools.

Not published to crates.io yet (`publish = false`): this is a new, still-settling
engine with no external consumers.

## Stack

Issue [#29](https://github.com/Epistates/turbovault/issues/29) asked for an
opt-in upgrade over the existing TF-IDF `semantic_search` tool, off by
default, with a Tier 1 backend that is pure Rust and CPU-only so it never
pulls a heavier dependency (ONNX, a GPU runtime) into a build that does not
ask for it.

- **Embeddings: [`model2vec-rs`](https://github.com/MinishLab/model2vec-rs)**,
  static (lookup-table, mean-pooled) embeddings, no transformer forward pass,
  so encoding is fast enough on a CPU that a plugin can afford to do it inline
  rather than needing a separate always-on model server. Built with
  `default-features = false, features = ["local-only"]`: `local-only` compiles
  out the Hugging Face Hub client entirely, so this crate cannot reach the
  network no matter what a caller passes to `Model2VecEmbedder::load`, it
  only ever reads a local model directory. Point it at a locally-downloaded
  copy of a [Potion model](https://huggingface.co/minishlab) such as
  `minishlab/potion-base-8M` or `minishlab/potion-retrieval-32M` (256-dim).
- **Dense index: [`hnsw_rs`](https://github.com/jean-pierreBoth/hnswlib-rs)**,
  a pure-Rust HNSW implementation. Its public API has no point-removal method
  (confirmed against 0.3.4; this is a real gap relative to `usearch`, which
  the prototype this crate started from used instead, and which does support
  removal via tombstoning). [`DenseIndex`] works around that by keeping the
  actual vectors in a map as the source of truth and treating the HNSW graph
  as a disposable cache, rebuilt from that map whenever a search is served
  after an upsert or removal. See `src/dense.rs` for the reasoning on why
  that is an acceptable trade for a vault-sized corpus.
- **Lexical index: [`bm25`](https://github.com/Michael-JB/bm25)**, a small,
  pure-Rust, in-memory BM25 scorer with native incremental `upsert`/`remove`.
  TurboVault already has a full-text engine (Tantivy, behind the host's own
  `search` tool), but the compiled-in plugin boundary has no passthrough to
  it today, and adding one would be a separate, larger change to the plugin
  contract itself, out of scope here. `bm25` gives genuine BM25 ranking
  without that change and without hand-rolling the formula.
- **Fusion:** Reciprocal Rank Fusion over the two sides' rankings
  (`src/router.rs`), the combination the issue asked for as the strongest
  retrieval path.

None of the three crates above pull in a C toolchain dependency (`hnsw_rs`
and `bm25` are dependency trees of entirely pure-Rust crates; `model2vec-rs`
with the feature selection above drops its own optional C-touching pieces).
The workspace MSRV (1.90.0) comfortably covers all three (`model2vec-rs`
states 1.88; the others state no floor and build cleanly under 1.90 at the
versions pinned in the workspace `Cargo.toml`).

## Incremental indexing

Chunking splits note text on paragraph, then sentence, then hard character
boundaries (`chunk.rs::chunk_text`), and every chunk carries a SHA-256 content
hash. Re-indexing a note diffs its new chunk hashes against the previous set
(`chunk.rs::diff_chunks`): a chunk whose hash is unchanged reuses its stored
vector, and only new or changed chunks are sent to the embedder. A note whose
plain-text content hash has not moved at all short-circuits before chunking
even runs. This is the mechanism that makes editing one paragraph of a long
note cheap regardless of how large the rest of the note is.

## Persistence

[`IndexEngine`] persists nothing itself. [`IndexEngine::snapshot`] and
[`IndexEngine::snapshot_note`] hand back [`NoteRecord`]s (path, content hash,
and every chunk's position, hash, text, and vector) for a caller to store.
The plugin host stores one per note under its `PluginStore` namespace, plus a
small metadata key for the chunk-id allocator's high-water mark.
[`IndexEngine::restore`] rebuilds engine state, including the dense and
lexical indices, from a set of `NoteRecord`s with no calls to the embedder:
every vector already exists, which is the entire point of persisting them.

## Credit

The chunking algorithm, its content-hash diffing, and the Reciprocal Rank
Fusion math are close ports of ForrestThump's prototype in
[#29](https://github.com/Epistates/turbovault/issues/29) (`turbovault-vector`
on his fork, `forrest/prototype-vector-e2e`). What changed around them: the
prototype's stack (fastembed/ONNX + usearch + SQLite) is the heavier backend
issue #29 asked to keep behind a separate, not-yet-built optional feature
rather than the Tier 1 default, and its storage (a SQLite file, BM25 served by
the host's Tantivy index) predates the plugin API gaining `PluginStore` and a
reliable change feed, so persistence and reconciliation here are rebuilt
against the current plugin boundary rather than carried over.
