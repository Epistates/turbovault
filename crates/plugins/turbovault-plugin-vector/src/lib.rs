//! `vector_search`: hybrid dense + lexical semantic search over the active
//! vault, as a compiled-in TurboVault plugin.
//!
//! Wraps [`turbovault_vector::IndexEngine`] behind the plugin boundary:
//! notes are read through [`VaultApi`], the index persists under this
//! plugin's [`PluginStorage`] namespace, and it is kept current by a
//! background worker that reconciles on every [`HookBus`] event (with an
//! initial pass in [`PluginProvider::start`] so a note edited while the
//! process was down is not missed) plus a resync every tool call, matching
//! how the host's own derived state stays fresh. Local tool names are
//! advertised namespaced as `vector_search_*`.
//!
//! See the crate README for what is and is not implemented yet (notably:
//! one engine per process, tied to whichever vault is active when it is
//! first built).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell};
use turbomcp_types::ToolInputSchema;
use turbovault_plugin_api::{
    HookBus, HookRecvError, NoteListing, Plugin, PluginContext, PluginDescriptor, PluginError,
    PluginProvider, PluginRequestContext, PluginResult, PluginStorage, ShutdownSignal, Tool,
    ToolResult, VaultApi, VaultDescriptor,
};
use turbovault_vector::{EmbeddingEngine, IndexEngine, Model2VecEmbedder, NoteRecord, VectorConfig, VectorError};

/// Key holding a JSON-encoded, partial [`VectorConfig`] override. Missing
/// fields fall back to the built-in defaults; the file itself is optional.
const CONFIG_KEY: &str = "config.json";
/// Key holding the reconcile cursor: `HashMap<path, NoteListing>` as last
/// observed, so a restart does not treat every note as newly changed.
const MANIFEST_KEY: &str = "manifest.json";
/// Key holding the chunk-id allocator's high-water mark.
const META_KEY: &str = "meta.json";
/// Directory under which each note's [`NoteRecord`] is stored, one key per
/// note so an edit to one note does not rewrite every other note's vectors.
///
/// No trailing slash: `PluginStorage::list`'s prefix is validated as a key
/// (via `validate_key`) unless it is empty, and a trailing `/` produces an
/// empty final segment, which `validate_key` rejects. A plain string prefix
/// still matches every `notes/...` key, since `list` does a `starts_with`
/// match rather than directory-aware matching.
const NOTES_DIR: &str = "notes";

fn note_key(path: &str) -> String {
    format!("{NOTES_DIR}/{path}.json")
}

fn verr(error: VectorError) -> PluginError {
    match error {
        // A config problem (e.g. no model_path set) is the caller's to fix,
        // not a server failure.
        VectorError::Config(message) => PluginError::invalid_input(message),
        VectorError::Embedding(message) | VectorError::Index(message) | VectorError::Snapshot(message) => {
            PluginError::internal(message)
        }
    }
}

/// Builds the embedder an [`EngineHandle`] embeds with, from the resolved
/// [`VectorConfig`]. A trait rather than a bare function pointer so a test
/// factory can close over state (a call counter, say) if it needs to.
type EmbedderFactory = Arc<dyn Fn(&VectorConfig) -> PluginResult<Arc<dyn EmbeddingEngine>> + Send + Sync>;

fn model2vec_embedder_factory(config: &VectorConfig) -> PluginResult<Arc<dyn EmbeddingEngine>> {
    Ok(Arc::new(Model2VecEmbedder::load(&config.model_path).map_err(verr)?))
}

/// Compiled-in factory for the `vector_search` plugin.
pub struct VectorSearchPlugin {
    embedder_factory: EmbedderFactory,
}

impl VectorSearchPlugin {
    /// The production plugin: embeds with a real, locally-loaded Model2Vec
    /// model (see [`turbovault_vector::Model2VecEmbedder`]).
    pub fn new() -> Self {
        Self {
            embedder_factory: Arc::new(model2vec_embedder_factory),
        }
    }

    /// Build with a caller-supplied embedder factory instead of loading a
    /// real model. For tests: this is how the plugin boundary (tool
    /// namespacing, storage, the change feed) gets exercised end to end
    /// without a model file to hand and without any network access — see
    /// `turbovault_vector::embedding::testing` for a ready-made fake.
    pub fn with_embedder_factory(
        factory: impl Fn(&VectorConfig) -> PluginResult<Arc<dyn EmbeddingEngine>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            embedder_factory: Arc::new(factory),
        }
    }
}

impl Default for VectorSearchPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for VectorSearchPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::new(
            "vector_search",
            "Vector Search",
            env!("CARGO_PKG_VERSION"),
            "Hybrid dense + lexical semantic search over the active vault",
        )
    }

    fn build(&self, context: PluginContext) -> PluginResult<Arc<dyn PluginProvider>> {
        let handle = Arc::new(EngineHandle {
            vault: context.vault,
            storage: context.storage,
            embedder_factory: Arc::clone(&self.embedder_factory),
            engine: OnceCell::new(),
            seen: Mutex::new(HashMap::new()),
        });
        Ok(Arc::new(VectorSearchProvider {
            handle,
            hooks: context.hooks,
            shutdown: context.shutdown,
            worker: Mutex::new(None),
        }))
    }
}

/// Lazily-built engine plus the storage and reconciliation state around it.
///
/// One engine per process. Built lazily (loading a model is not free, and
/// `Plugin::build` is synchronous) against whichever vault is active the
/// first time anything asks for it; [`Self::engine_for`] fails loudly rather
/// than silently searching the wrong vault's index if the active vault later
/// changes. See the crate README for the reasoning and the multi-vault
/// follow-up this leaves open.
struct EngineHandle {
    vault: VaultApi,
    storage: PluginStorage,
    embedder_factory: EmbedderFactory,
    engine: OnceCell<(String, IndexEngine)>,
    /// Reconcile cursor: what this plugin last saw for each note, so
    /// [`Self::reconcile`] only re-reads notes whose listing moved.
    seen: Mutex<HashMap<String, NoteListing>>,
}

impl EngineHandle {
    /// The engine, built and restored from persisted state on first use.
    /// Returns an error if the active vault has changed since the engine was
    /// built, rather than silently answering from the wrong vault's index.
    async fn engine_for(&self, active: &VaultDescriptor) -> PluginResult<&IndexEngine> {
        let (built_for, engine) = self
            .engine
            .get_or_try_init(|| async {
                let config = self.load_config(&active.name).await?;
                let embedder = (self.embedder_factory)(&config)?;
                let index_engine = IndexEngine::new(embedder, config);
                self.restore(&active.name, &index_engine).await?;
                Ok::<(String, IndexEngine), PluginError>((active.name.clone(), index_engine))
            })
            .await?;
        if built_for != &active.name {
            return Err(PluginError::unavailable(format!(
                "vector_search built its index against vault {built_for:?} and does not yet \
                 support switching to {:?} without a server restart",
                active.name
            )));
        }
        Ok(engine)
    }

    /// Convenience for a call site that has not already resolved the active
    /// vault.
    async fn engine(&self) -> PluginResult<&IndexEngine> {
        let active = self.vault.active_vault().await?;
        self.engine_for(&active).await
    }

    async fn load_config(&self, vault: &str) -> PluginResult<VectorConfig> {
        match self.storage.get(vault, CONFIG_KEY).await? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| PluginError::internal(format!("invalid {CONFIG_KEY}: {error}"))),
            None => Ok(VectorConfig::default()),
        }
    }

    /// Load every persisted note record plus the chunk-id counter and the
    /// reconcile cursor, and rebuild `index_engine`'s in-memory state from
    /// them. Calls the embedder for nothing.
    async fn restore(&self, vault: &str, index_engine: &IndexEngine) -> PluginResult<()> {
        let keys = self.storage.list(vault, NOTES_DIR).await?;
        let mut notes = Vec::with_capacity(keys.len());
        for key in &keys {
            let Some(bytes) = self.storage.get(vault, key).await? else {
                continue;
            };
            match serde_json::from_slice::<NoteRecord>(&bytes) {
                Ok(note) => notes.push(note),
                Err(error) => {
                    tracing::warn!("vector_search: dropping corrupt snapshot {key:?}: {error}");
                }
            }
        }

        let next_chunk_id = match self.storage.get(vault, META_KEY).await? {
            Some(bytes) => serde_json::from_slice::<turbovault_vector::IndexMeta>(&bytes)
                .map(|meta| meta.next_chunk_id)
                .unwrap_or(1),
            None => 1,
        };
        index_engine.restore(notes, next_chunk_id).await.map_err(verr)?;

        if let Some(bytes) = self.storage.get(vault, MANIFEST_KEY).await?
            && let Ok(manifest) = serde_json::from_slice::<HashMap<String, NoteListing>>(&bytes)
        {
            *self.seen.lock().await = manifest;
        }
        Ok(())
    }

    /// Persist one note's current record (or delete its key, if the engine
    /// no longer has it) and, if anything changed, the manifest and counter
    /// alongside it. Written in that order deliberately: if the process dies
    /// between the note write and the manifest write, the note is simply
    /// re-checked (and, via its own content hash, a no-op) on the next
    /// reconcile — the other order could permanently skip a real change.
    async fn persist_note(&self, vault: &str, path: &str, index_engine: &IndexEngine) -> PluginResult<()> {
        match index_engine.snapshot_note(path).await {
            Some(record) => {
                let bytes = serde_json::to_vec(&record)
                    .map_err(|error| PluginError::internal(error.to_string()))?;
                self.storage.put(vault, &note_key(path), &bytes).await?;
            }
            None => self.storage.delete(vault, &note_key(path)).await?,
        }
        Ok(())
    }

    async fn persist_manifest(&self, vault: &str) -> PluginResult<()> {
        let seen = self.seen.lock().await;
        let bytes = serde_json::to_vec(&*seen).map_err(|error| PluginError::internal(error.to_string()))?;
        drop(seen);
        self.storage.put(vault, MANIFEST_KEY, &bytes).await
    }

    async fn persist_meta(&self, vault: &str, index_engine: &IndexEngine) -> PluginResult<()> {
        let meta = turbovault_vector::IndexMeta {
            model: index_engine.model_name().to_string(),
            dims: index_engine.dims(),
            next_chunk_id: index_engine.next_chunk_id(),
        };
        let bytes = serde_json::to_vec(&meta).map_err(|error| PluginError::internal(error.to_string()))?;
        self.storage.put(vault, META_KEY, &bytes).await
    }

    /// Re-read every note whose `list_notes_detailed` listing moved since
    /// last seen, remove notes that vanished, and persist what changed.
    /// `IndexEngine::update_note` skips unchanged content at chunk
    /// granularity, so a listing that looked changed but was not (a touch,
    /// a re-save with no edits) costs one read and no re-embedding.
    ///
    /// Returns the number of notes that were actually re-indexed or removed.
    async fn reconcile(&self) -> PluginResult<usize> {
        let active = self.vault.active_vault().await?;
        let index_engine = self.engine_for(&active).await?;
        let current = self.vault.list_notes_detailed(&active.name).await?;
        let current: HashMap<String, NoteListing> =
            current.into_iter().map(|listing| (listing.path.clone(), listing)).collect();

        let seen = self.seen.lock().await;
        let to_check: Vec<String> = current
            .iter()
            .filter(|(path, listing)| !seen.get(*path).is_some_and(|previous| listing.looks_unchanged_from(previous)))
            .map(|(path, _)| path.clone())
            .collect();
        let gone: Vec<String> = seen.keys().filter(|path| !current.contains_key(*path)).cloned().collect();
        drop(seen);

        if to_check.is_empty() && gone.is_empty() {
            return Ok(0);
        }

        let mut changed = 0usize;
        if !to_check.is_empty() {
            for snapshot in self.vault.read_notes(&active.name, &to_check).await? {
                let did_change = index_engine
                    .update_note(&snapshot.path, &snapshot.content)
                    .await
                    .map_err(verr)?;
                if did_change {
                    changed += 1;
                    self.persist_note(&active.name, &snapshot.path, index_engine).await?;
                }
                if let Some(listing) = current.get(&snapshot.path) {
                    self.seen.lock().await.insert(snapshot.path.clone(), listing.clone());
                }
            }
        }
        for path in &gone {
            if index_engine.remove_note(path).await {
                changed += 1;
                self.storage.delete(&active.name, &note_key(path)).await?;
            }
            self.seen.lock().await.remove(path);
        }

        if changed > 0 || !gone.is_empty() {
            self.persist_manifest(&active.name).await?;
            self.persist_meta(&active.name, index_engine).await?;
        }
        Ok(changed)
    }

    /// Drop the engine's in-memory state and every persisted note record and
    /// cursor, so the next reconcile treats the whole vault as unseen and
    /// re-embeds everything. For a config or model change, or recovering
    /// from a snapshot a caller no longer trusts.
    async fn hard_reset(&self) -> PluginResult<usize> {
        let active = self.vault.active_vault().await?;
        let index_engine = self.engine_for(&active).await?;
        index_engine.clear().await.map_err(verr)?;

        let stale_keys = self.storage.list(&active.name, NOTES_DIR).await?;
        for key in &stale_keys {
            self.storage.delete(&active.name, key).await?;
        }
        self.storage.delete(&active.name, META_KEY).await?;
        self.seen.lock().await.clear();
        self.persist_manifest(&active.name).await?;

        self.reconcile().await
    }
}

pub struct VectorSearchProvider {
    handle: Arc<EngineHandle>,
    hooks: HookBus,
    shutdown: ShutdownSignal,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl VectorSearchProvider {
    async fn search(&self, arguments: &Value) -> PluginResult<ToolResult> {
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| PluginError::invalid_input("query is required and must not be empty"))?;
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .map(|limit| limit.clamp(1, 100) as usize)
            .unwrap_or(10);
        let hybrid = arguments.get("hybrid").and_then(Value::as_bool).unwrap_or(true);

        // Reconcile before serving, the same contract the host's own derived
        // reads follow: a search answers from state at least as fresh as the
        // last debounced pass, not from whatever the background worker
        // happened to have applied already.
        self.handle.reconcile().await?;
        let index_engine = self.handle.engine().await?;
        let results = index_engine.search(query, limit, hybrid).await.map_err(verr)?;
        ok_json(json!({
            "query": query,
            "hybrid": hybrid,
            "count": results.len(),
            "results": results,
        }))
    }

    async fn reindex(&self) -> PluginResult<ToolResult> {
        let reindexed = self.handle.hard_reset().await?;
        ok_json(json!({ "reindexed_notes": reindexed }))
    }

    async fn status(&self) -> PluginResult<ToolResult> {
        if let Some((_, index_engine)) = self.handle.engine.get() {
            let stats = index_engine.stats().await;
            let config = index_engine.config();
            return ok_json(json!({
                "configured": true,
                "model": stats.model,
                "dims": stats.dims,
                "indexed_notes": stats.indexed_notes,
                "indexed_chunks": stats.indexed_chunks,
                "chunk_max_chars": config.chunk_max_chars,
                "chunk_overlap_chars": config.chunk_overlap_chars,
                "lexical_weight": config.lexical_weight,
                "rrf_k": config.rrf_k,
            }));
        }

        // Not built yet: report the resolved config (including any override
        // in `config.json`) without forcing a model load, so `status` stays
        // cheap and safe to call before vector search is configured at all.
        let active = self.handle.vault.active_vault().await?;
        let config = self.handle.load_config(&active.name).await?;
        ok_json(json!({
            "configured": config.is_configured(),
            "model_path": config.model_path,
            "chunk_max_chars": config.chunk_max_chars,
            "chunk_overlap_chars": config.chunk_overlap_chars,
            "lexical_weight": config.lexical_weight,
            "rrf_k": config.rrf_k,
            "indexed_notes": 0,
            "indexed_chunks": 0,
            "note": if config.is_configured() {
                "model not loaded yet; call vector_search_search or vector_search_reindex to load it"
            } else {
                "not configured; set model_path in .turbovault/plugins/vector_search/config.json \
                 to a local Model2Vec model directory"
            },
        }))
    }
}

#[async_trait]
impl PluginProvider for VectorSearchProvider {
    fn tools(&self) -> Vec<Tool> {
        vec![
            tool(
                "search",
                "Hybrid semantic search over the vault; returns the most relevant note chunks",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Natural-language query" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "description": "Max results (default 10)" },
                        "hybrid": { "type": "boolean", "description": "Fuse BM25 lexical ranking in via Reciprocal Rank Fusion (default true); false for dense-only" }
                    },
                    "required": ["query"]
                }),
            ),
            tool(
                "reindex",
                "Force a full re-embed of the vault (normally indexing is incremental, driven by edits)",
                json!({ "type": "object", "properties": {} }),
            ),
            tool(
                "status",
                "Report index status: model, dimensions, indexed note/chunk counts, resolved config",
                json!({ "type": "object", "properties": {} }),
            ),
        ]
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        _context: PluginRequestContext,
    ) -> PluginResult<ToolResult> {
        match name {
            "search" => self.search(&arguments).await,
            "reindex" => self.reindex().await,
            "status" => self.status().await,
            other => Err(PluginError::not_found(format!("unknown tool {other:?}"))),
        }
    }

    async fn start(&self) -> PluginResult<()> {
        // Prime the index before entering the event loop: a note edited
        // while the process was down should be searchable on the first
        // query, not only after the first live event arrives.
        if let Err(error) = self.handle.reconcile().await {
            tracing::warn!("vector_search: initial reconcile failed: {error}");
        }

        let handle = Arc::clone(&self.handle);
        let shutdown = self.shutdown.clone();
        let mut events = self
            .hooks
            .subscribe()
            .map_err(|error| PluginError::unavailable(error.to_string()))?;

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    event = events.recv() => match event {
                        // Every event, of whatever kind, is answered the same
                        // way: reconcile from listings. That is what makes
                        // this correct for `ResyncRequired` too, with no
                        // special case for it.
                        Ok(_envelope) => {
                            if let Err(error) = handle.reconcile().await {
                                tracing::warn!("vector_search: reconcile failed: {error}");
                            }
                        }
                        Err(HookRecvError::Lagged { .. }) => {
                            if let Err(error) = handle.reconcile().await {
                                tracing::warn!("vector_search: reconcile after lag failed: {error}");
                            }
                        }
                        Err(HookRecvError::Closed) => break,
                        Err(HookRecvError::Empty) => continue,
                    },
                }
            }
        });
        *self.worker.lock().await = Some(task);
        Ok(())
    }

    async fn shutdown(&self) {
        if let Some(task) = self.worker.lock().await.take() {
            let _ = task.await;
        }
    }
}

fn tool(name: &str, description: &str, schema: Value) -> Tool {
    Tool::new(name, description).with_schema(ToolInputSchema::from_value(schema))
}

fn ok_json(value: Value) -> PluginResult<ToolResult> {
    ToolResult::json(&value).map_err(|error| PluginError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use turbovault_plugin_api::{
        HookBus, NoteSnapshot, PluginIdentity, PluginStore, ShutdownTrigger, VaultHost,
        WriteNoteRequest, WriteReceipt,
    };
    use turbovault_vector::embedding::testing::{CountingEmbedder, DeterministicEmbedder};

    const VAULT: &str = "test-vault";
    const DIMS: usize = 32;

    /// In-memory [`PluginStore`], namespaced by `(vault, key)` exactly like
    /// the production `FilePluginStore`, so a test can share one across two
    /// `EngineHandle`s to simulate a restart.
    #[derive(Default)]
    struct FakeStore {
        data: StdMutex<HashMap<(String, String), Vec<u8>>>,
    }

    #[async_trait]
    impl PluginStore for FakeStore {
        async fn get(&self, vault: &str, key: &str) -> PluginResult<Option<Vec<u8>>> {
            Ok(self.data.lock().unwrap().get(&(vault.to_string(), key.to_string())).cloned())
        }

        async fn put(&self, vault: &str, key: &str, value: &[u8]) -> PluginResult<()> {
            self.data
                .lock()
                .unwrap()
                .insert((vault.to_string(), key.to_string()), value.to_vec());
            Ok(())
        }

        async fn delete(&self, vault: &str, key: &str) -> PluginResult<()> {
            self.data.lock().unwrap().remove(&(vault.to_string(), key.to_string()));
            Ok(())
        }

        async fn list(&self, vault: &str, prefix: &str) -> PluginResult<Vec<String>> {
            let data = self.data.lock().unwrap();
            let mut keys: Vec<String> = data
                .keys()
                .filter(|(entry_vault, key)| entry_vault == vault && key.starts_with(prefix))
                .map(|(_, key)| key.clone())
                .collect();
            keys.sort();
            Ok(keys)
        }
    }

    /// In-memory [`VaultHost`], with an explicit "clock" so a test can
    /// control exactly what `list_notes_detailed` reports without racing a
    /// real filesystem's timestamp resolution.
    #[derive(Default)]
    struct FakeVault {
        notes: StdMutex<HashMap<String, (String, u64)>>, // path -> (content, observed_at)
        clock: AtomicU64,
    }

    impl FakeVault {
        fn put_note(&self, path: &str, content: &str) {
            let observed_at = self.clock.fetch_add(1, Ordering::SeqCst);
            self.notes.lock().unwrap().insert(path.to_string(), (content.to_string(), observed_at));
        }

        fn remove_note(&self, path: &str) {
            self.notes.lock().unwrap().remove(path);
        }
    }

    #[async_trait]
    impl VaultHost for FakeVault {
        async fn active_vault(&self) -> PluginResult<VaultDescriptor> {
            Ok(VaultDescriptor::new(VAULT, "direct"))
        }

        async fn list_notes(&self, _vault: &str) -> PluginResult<Vec<String>> {
            Ok(self.notes.lock().unwrap().keys().cloned().collect())
        }

        async fn list_notes_detailed(&self, _vault: &str) -> PluginResult<Vec<NoteListing>> {
            Ok(self
                .notes
                .lock()
                .unwrap()
                .iter()
                .map(|(path, (content, observed_at))| {
                    NoteListing::new(path.clone(), content.len() as u64, Some(*observed_at))
                })
                .collect())
        }

        async fn read_note(&self, _vault: &str, path: &str) -> PluginResult<NoteSnapshot> {
            let notes = self.notes.lock().unwrap();
            let (content, _) = notes.get(path).ok_or_else(|| PluginError::not_found(path.to_string()))?;
            Ok(NoteSnapshot::new(VAULT, path, content.clone(), "v1"))
        }

        async fn read_notes(&self, _vault: &str, paths: &[String]) -> PluginResult<Vec<NoteSnapshot>> {
            let notes = self.notes.lock().unwrap();
            Ok(paths
                .iter()
                .filter_map(|path| notes.get(path).map(|(content, _)| NoteSnapshot::new(VAULT, path, content.clone(), "v1")))
                .collect())
        }

        async fn write_note(&self, _request: WriteNoteRequest) -> PluginResult<WriteReceipt> {
            Err(PluginError::unavailable("FakeVault is read-only"))
        }

        async fn read_config(&self, _vault: &str, _relative_path: &str) -> PluginResult<Option<Vec<u8>>> {
            Ok(None)
        }
    }

    fn vault_api(host: Arc<FakeVault>) -> VaultApi {
        let identity = PluginIdentity::new("vector_search", Default::default()).expect("identity");
        VaultApi::new(host, identity)
    }

    fn handle_with(store: Arc<FakeStore>, vault: Arc<FakeVault>, embedder_calls: Arc<CountingEmbedder>) -> EngineHandle {
        EngineHandle {
            vault: vault_api(vault),
            storage: PluginStorage::new(store),
            embedder_factory: Arc::new(move |_config: &VectorConfig| Ok(Arc::clone(&embedder_calls) as Arc<dyn EmbeddingEngine>)),
            engine: OnceCell::new(),
            seen: Mutex::new(HashMap::new()),
        }
    }

    #[tokio::test]
    async fn reconcile_indexes_new_notes_and_persists_them() {
        let store = Arc::new(FakeStore::default());
        let vault = Arc::new(FakeVault::default());
        vault.put_note("alpha.md", "Alpha unique text.");
        vault.put_note("beta.md", "Beta unique text.");
        let embedder = Arc::new(CountingEmbedder::new(DIMS));
        let handle = handle_with(Arc::clone(&store), vault, embedder);

        let changed = handle.reconcile().await.expect("reconcile");
        assert_eq!(changed, 2);

        let keys = store.list(VAULT, NOTES_DIR).await.expect("list");
        assert_eq!(keys, vec![note_key("alpha.md"), note_key("beta.md")]);
        assert!(store.get(VAULT, MANIFEST_KEY).await.expect("get").is_some());
        assert!(store.get(VAULT, META_KEY).await.expect("get").is_some());

        // A second reconcile with nothing changed must not touch either
        // index or re-persist anything.
        let changed_again = handle.reconcile().await.expect("second reconcile");
        assert_eq!(changed_again, 0);
    }

    #[tokio::test]
    async fn restart_restores_the_index_without_re_embedding() {
        // The fake embedder hashes whole inputs into a bag of features with
        // no notion of "contains as a substring" (unlike a real model), so a
        // query has to match the indexed text itself to land above
        // min_similarity on the dense-only path this test exercises.
        const NOTE_CONTENT: &str = "Some searchable unique content.";
        let store = Arc::new(FakeStore::default());
        let vault = Arc::new(FakeVault::default());
        vault.put_note("note.md", NOTE_CONTENT);

        let embedder = Arc::new(CountingEmbedder::new(DIMS));
        let handle = handle_with(Arc::clone(&store), Arc::clone(&vault), Arc::clone(&embedder));
        handle.reconcile().await.expect("initial reconcile");
        let embedded_before_restart = embedder.count();
        assert!(embedded_before_restart > 0);

        // A fresh handle sharing the same store stands in for a process
        // restart: everything on disk survives, nothing in memory does.
        let restarted_embedder = Arc::new(CountingEmbedder::new(DIMS));
        let restarted = handle_with(store, vault, Arc::clone(&restarted_embedder));
        let active = restarted.vault.active_vault().await.expect("active vault");
        let index_engine = restarted.engine_for(&active).await.expect("engine");
        let stats = index_engine.stats().await;
        assert_eq!(stats.indexed_notes, 1);
        assert_eq!(stats.indexed_chunks, 1);
        assert_eq!(restarted_embedder.count(), 0, "restore must not call the embedder");

        let hits = index_engine.search(NOTE_CONTENT, 5, false).await.expect("search");
        assert_eq!(hits[0].path, "note.md");
    }

    #[tokio::test]
    async fn removed_notes_are_dropped_from_index_and_storage() {
        let store = Arc::new(FakeStore::default());
        let vault = Arc::new(FakeVault::default());
        vault.put_note("note.md", "Content to be removed.");
        let embedder = Arc::new(CountingEmbedder::new(DIMS));
        let handle = handle_with(Arc::clone(&store), Arc::clone(&vault), embedder);
        handle.reconcile().await.expect("index it first");
        assert!(store.get(VAULT, &note_key("note.md")).await.unwrap().is_some());

        vault.remove_note("note.md");
        let changed = handle.reconcile().await.expect("reconcile after removal");
        assert_eq!(changed, 1);
        assert!(store.get(VAULT, &note_key("note.md")).await.unwrap().is_none());

        let active = handle.vault.active_vault().await.unwrap();
        let index_engine = handle.engine_for(&active).await.unwrap();
        assert_eq!(index_engine.stats().await.indexed_notes, 0);
    }

    #[tokio::test]
    async fn hard_reset_forces_a_full_re_embed() {
        let store = Arc::new(FakeStore::default());
        let vault = Arc::new(FakeVault::default());
        vault.put_note("note.md", "Stable content that never changes.");
        let embedder = Arc::new(CountingEmbedder::new(DIMS));
        let handle = handle_with(Arc::clone(&store), vault, Arc::clone(&embedder));
        handle.reconcile().await.expect("initial index");
        let first_pass = embedder.count();
        assert!(first_pass > 0);

        // Content did not change, so a plain reconcile would not re-embed;
        // hard_reset must re-embed anyway.
        assert_eq!(handle.reconcile().await.unwrap(), 0);
        let reindexed = handle.hard_reset().await.expect("hard reset");
        assert_eq!(reindexed, 1);
        assert_eq!(embedder.count(), first_pass * 2);
    }

    #[tokio::test]
    async fn search_tool_rejects_an_empty_query() {
        let provider = VectorSearchProvider {
            handle: Arc::new(handle_with(
                Arc::new(FakeStore::default()),
                Arc::new(FakeVault::default()),
                Arc::new(CountingEmbedder::new(DIMS)),
            )),
            hooks: HookBus::new(16),
            shutdown: ShutdownTrigger::new().signal(),
            worker: Mutex::new(None),
        };
        let error = provider
            .call_tool("search", json!({"query": "   "}), PluginRequestContext::new("req"))
            .await
            .expect_err("empty query must be rejected");
        assert_eq!(error.code, turbovault_plugin_api::PluginErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn status_reports_before_and_after_the_engine_is_built() {
        let vault = Arc::new(FakeVault::default());
        vault.put_note("note.md", "Searchable content for status reporting.");
        let provider = VectorSearchProvider {
            handle: Arc::new(handle_with(
                Arc::new(FakeStore::default()),
                vault,
                Arc::new(CountingEmbedder::new(DIMS)),
            )),
            hooks: HookBus::new(16),
            shutdown: ShutdownTrigger::new().signal(),
            worker: Mutex::new(None),
        };

        let before = provider
            .call_tool("status", json!({}), PluginRequestContext::new("req"))
            .await
            .expect("status before build");
        let before = before.structured_content.expect("structured content");
        assert_eq!(before["indexed_notes"], 0);

        provider
            .call_tool("search", json!({"query": "content"}), PluginRequestContext::new("req"))
            .await
            .expect("search builds the engine");

        let after = provider
            .call_tool("status", json!({}), PluginRequestContext::new("req"))
            .await
            .expect("status after build");
        let after = after.structured_content.expect("structured content");
        assert_eq!(after["indexed_notes"], 1);
        assert_eq!(after["dims"], DIMS);
    }

    #[tokio::test]
    async fn worker_reconciles_on_a_hook_event_and_stops_on_shutdown() {
        let vault = Arc::new(FakeVault::default());
        vault.put_note("note.md", "Indexed before the worker starts.");
        let hooks = HookBus::new(16);
        let trigger = ShutdownTrigger::new();
        let provider = VectorSearchProvider {
            handle: Arc::new(handle_with(
                Arc::new(FakeStore::default()),
                Arc::clone(&vault),
                Arc::new(CountingEmbedder::new(DIMS)),
            )),
            hooks: hooks.clone(),
            shutdown: trigger.signal(),
            worker: Mutex::new(None),
        };

        provider.start().await.expect("start");
        vault.put_note("second.md", "Indexed after a hook event.");
        hooks
            .publish(
                VAULT,
                turbovault_plugin_api::HookEvent::FileCreated { path: "second.md".to_string() },
                None,
                None,
                turbovault_plugin_api::EventAttribution::ExternalOrUnknown,
            )
            .expect("publish");

        // The worker's reconcile is async and not itself observable from
        // here, so poll for it instead of asserting instantly. Bounded by
        // yield count rather than a wall-clock deadline, and checking engine
        // state directly rather than through `call_tool`: nothing here waits
        // on real I/O or a timer (the fakes resolve immediately), and a cheap
        // check keeps the bound meaningful even when the host is under heavy
        // unrelated load, instead of the iteration budget being consumed by
        // JSON serialization work on every poll.
        let mut indexed = 0;
        for _ in 0..1_000 {
            if let Some((_, index_engine)) = provider.handle.engine.get() {
                indexed = index_engine.stats().await.indexed_notes;
                if indexed == 2 {
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(indexed, 2, "worker did not reconcile the hook event");

        // The real host signals the trigger, then awaits each plugin's
        // shutdown (see `ObsidianMcpServer::shutdown`); `PluginProvider::shutdown`
        // only awaits cleanup; it does not itself fire the signal the worker
        // loop is blocked on.
        trigger.shutdown();
        provider.shutdown().await;
        assert!(provider.worker.lock().await.is_none());
    }

    #[tokio::test]
    async fn plugin_descriptor_is_a_stable_namespace() {
        let plugin = VectorSearchPlugin::with_embedder_factory(|_| {
            Ok(Arc::new(DeterministicEmbedder::new(DIMS)) as Arc<dyn EmbeddingEngine>)
        });
        assert_eq!(plugin.descriptor().id, "vector_search");
    }
}
