# turbovault-plugin-vector

`vector_search`: hybrid dense + lexical semantic search over the active
vault, as a compiled-in TurboVault plugin.

Wraps [`turbovault-vector`](../../turbovault-vector)'s engine behind the
stable plugin boundary (`turbovault-plugin-api`): notes are read through
`VaultApi`, the index persists under this plugin's `PluginStorage`
namespace (`.turbovault/plugins/vector_search/` on a Direct-backed vault),
and it stays current via a background worker that reconciles on every
`HookBus` event, plus a resync on every tool call. Local tool names
(`search`, `reindex`, `status`) are advertised namespaced as
`vector_search_search`, `vector_search_reindex`, `vector_search_status`.

Not published to crates.io yet (`publish = false`), matching
`turbovault-vector`.

## Enabling it

Off by default. Build `turbovault` with the `vector-search` feature:

```bash
cargo build --release --features vector-search
```

This pulls in `turbovault-plugin-vector` (and transitively
`turbovault-vector`, `model2vec-rs`, `hnsw_rs`, `bm25`) and mounts
`VectorSearchPlugin` alongside the core tool set. A build without the
feature is byte-for-byte the same as before this crate existed: no new
dependency, no new tool, no new code path exercised.

## Configuring a model

`turbovault-vector` never downloads a model. Point it at a local directory
holding a [Potion model](https://huggingface.co/minishlab) (or any
Model2Vec-compatible model): `tokenizer.json`, `model.safetensors`,
`config.json`.

Write `.turbovault/plugins/vector_search/config.json` inside the vault (any
field you omit keeps its default):

```json
{
  "model_path": "/absolute/path/to/potion-base-8M",
  "chunk_max_chars": 800,
  "chunk_overlap_chars": 100,
  "lexical_weight": 0.4
}
```

Call `vector_search_status` to see the resolved configuration (and whether
a model has been loaded yet) without forcing a load. `vector_search_search`
and `vector_search_reindex` load the model on first use.

## Tools

- **`vector_search_search`** — `{ query: string, limit?: integer (default
  10, max 100), hybrid?: boolean (default true) }`. Reconciles first, then
  searches; `hybrid` fuses BM25 lexical ranking in via Reciprocal Rank
  Fusion, `false` for dense-only.
- **`vector_search_reindex`** — `{}`. Forces a full re-embed: drops every
  persisted chunk and the reconcile cursor, then walks the vault from
  scratch. Normal indexing is incremental and driven by edits; this is for
  a changed model or recovering from a snapshot you no longer trust.
- **`vector_search_status`** — `{}`. Model, dimensions, indexed note/chunk
  counts, and the resolved config. Safe to call before anything is
  configured.

## Known limitations

- **One engine per process, tied to whichever vault is active when it is
  first built.** If the active vault changes afterward, every tool call
  returns a clear `unavailable` error rather than silently searching the
  wrong vault's index. Multi-vault-aware indexing (one engine per vault,
  built and evicted as the active vault changes) is a larger follow-up, not
  implemented here. This matches the scope ForrestThump's prototype set for
  itself ("single active vault; default config").
- **The dense index rebuilds from scratch on every stale search**, because
  `hnsw_rs` has no point-removal API. See `turbovault-vector`'s README and
  `src/dense.rs` for why that is an acceptable trade for a vault-sized
  corpus, and where it would stop being one.

## Credit

Reconcile-on-demand against `list_notes_detailed` plus the change feed, and
the overall shape of "wrap the engine behind `VaultApi` and `PluginStorage`,
advertise `search`/`reindex`/`status`", follow ForrestThump's prototype in
[#29](https://github.com/Epistates/turbovault/issues/29)
(`turbovault-plugin-vector` on `forrest/prototype-vector-e2e`). That
prototype predates the plugin API gaining `PluginStorage` (#42) and a
reliable change feed (#43), so the actual persistence and reconciliation code
here is new against the current plugin boundary rather than ported.
