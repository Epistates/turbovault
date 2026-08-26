//! MCP tool implementations for Obsidian vault

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use turbomcp::prelude::*;
use turbovault_audit::{AuditFilter, AuditLog, OperationType, SnapshotStore};
use turbovault_core::ServerConfig;
use turbovault_core::config::{GitMergeStrategy as ConfigMergeStrategy, VaultConfig, WriteBackend};
use turbovault_core::error::Error;
use turbovault_core::prelude::MultiVaultManager;
use turbovault_tools::{
    AnalysisTools, AuditTools, BatchOperation, BatchTools, CommitLocks, DiffTools, DuplicateTools,
    ExportTools, FanoutInfo, FileTools, GitMergeStrategy, GraphTools, GroundingTools,
    MetadataTools, OkfTools, QualityTools, RelationshipTools, SearchEngine, SearchQuery,
    SearchTools, SimilarityEngine, SliceResult, SliceSpec, TemplateEngine, VaultLifecycleTools,
    VaultRepo, ViewerTools, WriteMode, obsidian_uri, slice_content,
};
use turbovault_vault::{ChangeListener, VaultManager};

mod providers;
pub use providers::ObsidianMcpServer;

#[cfg(feature = "plugin-api")]
mod plugin_host;

#[cfg(feature = "sql")]
use turbovault_tools::FrontmatterSqlEngine;

/// A frontmatter key-value filter for advanced search
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FrontmatterFilter {
    /// Frontmatter key to match (e.g. "type", "status", "project")
    pub key: String,
    /// Value to match against (substring match)
    pub value: String,
}

/// Helper to convert internal Error to McpError
fn to_mcp_error(e: Error) -> McpError {
    // Log full error for server-side debugging before sanitizing for client
    log::warn!("MCP error: {:?}", e);
    McpError::internal(e.to_string())
}

/// Extract count from serde_json::Value array (eliminates DRY violation)
#[inline]
fn extract_count(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(arr) => arr.len(),
        _ => 0,
    }
}

/// Render the OKF orientation block for `get_vault_context`: a compact signal
/// telling the agent whether this vault is an OKF bundle, where the
/// progressive-disclosure entry point is, and what its type vocabulary looks
/// like. Returns `null` when the vault is not an OKF bundle.
fn okf_context_block(bundle: &turbovault_core::okf::BundleInfo) -> serde_json::Value {
    if !bundle.is_okf_bundle {
        return serde_json::Value::Null;
    }
    serde_json::json!({
        "is_okf_bundle": true,
        "concept_docs": bundle.concept_docs,
        "concept_ratio": (bundle.concept_ratio * 100.0).round() / 100.0,
        "entry_point": bundle.has_root_index.then_some("index.md"),
        "has_root_log": bundle.has_root_log,
        "top_types": bundle.top_types.iter()
            .take(8)
            .map(|(t, n)| serde_json::json!({ "type": t, "count": n }))
            .collect::<Vec<_>>(),
        "guidance": if bundle.has_root_index {
            "This vault is an OKF bundle. Read index.md first and follow its links (progressive disclosure, OKF §6) instead of blind search."
        } else {
            "This vault is an OKF bundle but has no root index.md. Run generate_index to create progressive-disclosure entry points (OKF §6)."
        },
        "tools": ["okf_validate", "generate_index", "append_log_entry"],
    })
}

/// turbovault-0bh: auto-derive a commit subject from a batch's op tally.
/// Used as the fallback when the caller doesn't supply `commit_message`.
/// Example: `batch: 5 creates, 2 updates, 1 delete`.
fn derive_batch_message(operations: &[turbovault_tools::BatchOperation]) -> String {
    use turbovault_tools::BatchOperation;
    let mut creates = 0u32;
    let mut updates = 0u32;
    let mut deletes = 0u32;
    let mut moves = 0u32;
    let mut link_updates = 0u32;
    let mut edits = 0u32;
    let mut fm_updates = 0u32;
    let mut tag_updates = 0u32;
    let mut template_creates = 0u32;
    for op in operations {
        match op {
            BatchOperation::CreateNote { .. } => creates += 1,
            BatchOperation::WriteNote { .. } => updates += 1,
            BatchOperation::DeleteNote { .. } => deletes += 1,
            BatchOperation::MoveNote { .. } => moves += 1,
            BatchOperation::UpdateLinks { .. } => link_updates += 1,
            BatchOperation::EditNote { .. } => edits += 1,
            BatchOperation::UpdateFrontmatter { .. } => fm_updates += 1,
            BatchOperation::ManageTags { .. } => tag_updates += 1,
            BatchOperation::CreateFromTemplate { .. } => template_creates += 1,
        }
    }
    let pluralize = |n: u32, word: &str| -> String {
        if n == 1 {
            format!("1 {}", word)
        } else {
            format!("{} {}s", n, word)
        }
    };
    let mut parts = Vec::new();
    if creates > 0 {
        parts.push(pluralize(creates, "create"));
    }
    if updates > 0 {
        parts.push(pluralize(updates, "update"));
    }
    if deletes > 0 {
        parts.push(pluralize(deletes, "delete"));
    }
    if moves > 0 {
        parts.push(pluralize(moves, "move"));
    }
    if link_updates > 0 {
        parts.push(pluralize(link_updates, "link update"));
    }
    if edits > 0 {
        parts.push(pluralize(edits, "edit"));
    }
    if fm_updates > 0 {
        parts.push(pluralize(fm_updates, "frontmatter update"));
    }
    if tag_updates > 0 {
        parts.push(pluralize(tag_updates, "tag update"));
    }
    if template_creates > 0 {
        parts.push(pluralize(template_creates, "template create"));
    }
    if parts.is_empty() {
        // Should be unreachable — empty batches are rejected upstream.
        "batch_execute (0 ops)".to_string()
    } else {
        format!("batch: {}", parts.join(", "))
    }
}

/// Standardized response envelope for all tools (LLMX improvement)
/// Generic, non-cumbersome, forward-looking design
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct StandardResponse<T: serde::Serialize> {
    /// Which vault this operation was performed on
    pub vault: String,
    /// Operation name for context (e.g., "read_note", "search")
    pub operation: String,
    /// Was the operation successful?
    pub success: bool,
    /// The actual result data (any type)
    pub data: T,
    /// Count of items in result (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    /// How long the operation took in milliseconds
    pub took_ms: u64,
    /// Non-fatal warnings or notes (e.g., "Note had duplicate links")
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Suggested next operations based on result (e.g., ["write_note", "search"])
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub next_steps: Vec<String>,
    /// Flexible metadata object for forward-looking extensibility
    /// Can include: version, timestamp, correlation_id, suggestions, deprecation notices, etc.
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub meta: serde_json::Map<String, serde_json::Value>,
}

impl<T: serde::Serialize> StandardResponse<T> {
    /// Create a new standard response (accepts `Into<String>` for flexibility)
    pub fn new(vault: impl Into<String>, operation: impl Into<String>, data: T) -> Self {
        Self {
            vault: vault.into(),
            operation: operation.into(),
            success: true,
            data,
            count: None,
            took_ms: 0,
            warnings: vec![],
            next_steps: vec![],
            meta: serde_json::Map::new(),
        }
    }

    /// Set item count
    pub fn with_count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// Set operation time
    pub fn with_duration(mut self, ms: u64) -> Self {
        self.took_ms = ms;
        self
    }

    /// Add a warning
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    /// Add suggested next step
    pub fn with_next_step(mut self, step: impl Into<String>) -> Self {
        self.next_steps.push(step.into());
        self
    }

    /// Add metadata value (forward-looking extensibility)
    pub fn with_meta(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.meta.insert(key.into(), value);
        self
    }

    /// Set success flag
    pub fn with_success(mut self, success: bool) -> Self {
        self.success = success;
        self
    }

    /// Shorthand for serializing to JSON with consistent error handling
    pub fn to_json(self) -> McpResult<serde_json::Value> {
        serde_json::to_value(self).map_err(|e| McpError::internal(e.to_string()))
    }

    /// Add multiple next steps at once (reduces boilerplate)
    pub fn with_next_steps(mut self, steps: &[&str]) -> Self {
        for step in steps {
            self.next_steps.push(step.to_string());
        }
        self
    }

    /// Add common next step pattern for read operations
    pub fn with_read_next_steps(self) -> Self {
        self.with_next_steps(&["write_note", "get_backlinks"])
    }

    /// Add common next step pattern for write operations
    pub fn with_write_next_steps(self) -> Self {
        self.with_next_steps(&["read_note", "query_metadata"])
    }

    /// Add common next step pattern for search operations
    pub fn with_search_next_steps(self) -> Self {
        self.with_next_steps(&["advanced_search", "recommend_related"])
    }

    /// Add common next step pattern for analysis operations
    pub fn with_analysis_next_steps(self) -> Self {
        self.with_next_steps(&["quick_health_check", "full_health_analysis"])
    }
}

/// Shared implementation handler used by the focused provider facade.
#[derive(Clone)]
pub(super) struct CoreToolHandler {
    multi_vault_mgr: Arc<MultiVaultManager>,
    /// Cache of vault managers by vault name to persist state across calls
    vault_managers: Arc<RwLock<HashMap<String, Arc<VaultManager>>>>,
    /// Cache for persisting vault state across server restarts (project-aware)
    persistent_cache: Arc<RwLock<Option<turbovault_core::cache::VaultCache>>>,
    /// Audit logs per vault (keyed by vault name)
    audit_logs: Arc<RwLock<HashMap<String, Arc<AuditLog>>>>,
    /// Snapshot stores per vault (keyed by vault name)
    snapshot_stores: Arc<RwLock<HashMap<String, Arc<SnapshotStore>>>>,
    /// Similarity engines per vault (keyed by vault name, lazy-initialized)
    similarity_engines: Arc<RwLock<HashMap<String, Arc<SimilarityEngine>>>>,
    /// Search engines per vault (keyed by vault name, lazy-initialized)
    search_engines: Arc<RwLock<HashMap<String, Arc<SearchEngine>>>>,
    /// Shared per-vault `CommitLocks` registries for the git substrate
    /// (keyed by vault name, lazy-initialized). `VaultRepo` itself is `!Sync`
    /// (libgit2 raw pointers), so we cache the lock registry — which IS
    /// `Send + Sync` — and open a fresh `VaultRepo` per call via
    /// `open_with_locks(...)`. That keeps all in-process callers serialized
    /// on one commit-section mutex per worktree at trivial open cost.
    git_locks: Arc<RwLock<HashMap<String, Arc<CommitLocks>>>>,
    /// GWS.13 active fanouts, keyed by **base vault name**
    /// (the original vault, NOT the auto-registered fanout vault). At most
    /// one active fanout per base vault — `begin_fanout` errors loudly
    /// on a second concurrent attempt.
    active_fanouts: Arc<RwLock<HashMap<String, ActiveFanoutRecord>>>,
}

/// One row of `ObsidianMcpServer.active_fanouts`.
#[derive(Debug, Clone)]
struct ActiveFanoutRecord {
    fanout_id: String,
    info: FanoutInfo,
    /// Auto-registered transient vault name (e.g. `<base>-fanout-<fanout_id>`)
    /// that subagents `set_active_vault` to during the fanout.
    fanout_vault_name: String,
}

/// Shared setup for the `write_note` operation, which is implemented at two
/// provider boundaries — the MCP handler and the plugin `VaultHost`. Bundling
/// active-vault selection + commit-message resolution here keeps both from
/// re-deriving it.
pub(super) struct CompleteNoteWrite {
    pub(super) vault_name: String,
    pub(super) manager: Arc<VaultManager>,
    pub(super) message: String,
}

impl CoreToolHandler {
    /// Create a new server instance (vault-agnostic - no vault required at startup)
    pub fn new() -> Result<Self> {
        let config = ServerConfig {
            vaults: vec![],
            ..ServerConfig::default()
        };
        let mgr = MultiVaultManager::empty(config)?;
        Ok(Self {
            multi_vault_mgr: Arc::new(mgr),
            vault_managers: Arc::new(RwLock::new(HashMap::new())),
            persistent_cache: Arc::new(RwLock::new(None)),
            audit_logs: Arc::new(RwLock::new(HashMap::new())),
            snapshot_stores: Arc::new(RwLock::new(HashMap::new())),
            similarity_engines: Arc::new(RwLock::new(HashMap::new())),
            search_engines: Arc::new(RwLock::new(HashMap::new())),
            git_locks: Arc::new(RwLock::new(HashMap::new())),
            active_fanouts: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Initialize the persistent cache (should be called after server creation)
    pub async fn init_cache(&self) -> Result<()> {
        let cache = turbovault_core::cache::VaultCache::init().await?;
        let mut cache_lock = self.persistent_cache.write().await;
        *cache_lock = Some(cache);
        Ok(())
    }

    /// Get the multi-vault manager
    pub fn multi_vault(&self) -> Arc<MultiVaultManager> {
        self.multi_vault_mgr.clone()
    }

    /// turbovault-z5c: lift the `add_vault` MCP tool's auto-init logic
    /// to a public method so the CLI `--init` flag can actually
    /// initialize registered vaults at startup. Walks every registered
    /// vault, constructs its VaultManager (scans files + builds link
    /// graph), and caches it. Errors on the first failure.
    pub async fn initialize_registered_vaults(&self) -> Result<()> {
        let vaults = self
            .multi_vault_mgr
            .list_vaults()
            .await
            .map_err(|e| Error::config_error(format!("list_vaults: {}", e)))?;
        for vault_info in vaults {
            let name = vault_info.name.clone();
            // Skip if already cached (idempotent for sequential calls).
            {
                let cache = self.vault_managers.read().await;
                if cache.contains_key(&name) {
                    continue;
                }
            }
            self.activate_vault_manager(&name)
                .await
                .map_err(|e| Error::config_error(e.to_string()))?;
            log::info!("Initialized vault '{}' (--init)", name);
        }
        Ok(())
    }

    /// Helper to save vault state to cache
    async fn persist_vault_state(&self) -> Result<()> {
        if let Some(cache) = self.persistent_cache.read().await.as_ref() {
            // Get current vaults and active vault
            let vaults_lock = self.multi_vault_mgr.list_vaults().await?;
            let vault_configs: Vec<_> = vaults_lock.iter().map(|v| v.config.clone()).collect();
            let active_vault = self.multi_vault_mgr.get_active_vault().await;

            // Save to cache
            cache.save_vaults(&vault_configs, &active_vault).await?;
            log::debug!("Vault state persisted to cache");
        }
        Ok(())
    }

    /// turbovault-84k: scan every git-backend vault (or just `vault_filter`
    /// if specified) for `wip-*` worktrees that aren't backed by an entry
    /// in `active_fanouts`. Pure read; never mutates. Used by the startup
    /// warning and the `list_orphan_fanouts` MCP tool.
    pub async fn scan_orphan_fanouts(
        &self,
        vault_filter: Option<&str>,
    ) -> Result<Vec<OrphanFanoutEntry>> {
        let vaults = self.multi_vault_mgr.list_vaults().await?;
        let active_worktree_names: std::collections::HashSet<String> = {
            let guard = self.active_fanouts.read().await;
            guard
                .values()
                .map(|r| r.info.worktree_name.clone())
                .collect()
        };
        let mut out = Vec::new();
        for entry in vaults {
            let cfg = entry.config.clone();
            if let Some(filter) = vault_filter
                && cfg.name != filter
            {
                continue;
            }
            if !matches!(cfg.write_backend, WriteBackend::Git) {
                continue;
            }
            let locks = self.get_or_init_git_locks(&cfg.name).await;
            let cfg_name = cfg.name.clone();
            let path = cfg.path.clone();
            let active_clone = active_worktree_names.clone();
            let found: Result<Vec<OrphanFanoutEntry>> =
                tokio::task::spawn_blocking(move || -> Result<Vec<OrphanFanoutEntry>> {
                    let repo = VaultRepo::open_with_locks(&path, locks)
                        .map_err(|e| anyhow::anyhow!("open vault {}: {}", cfg_name, e))?;
                    Ok(repo
                        .list_orphan_fanouts()
                        .map_err(|e| anyhow::anyhow!("list_orphan_fanouts({}): {}", cfg_name, e))?
                        .into_iter()
                        .filter(|o| !active_clone.contains(&o.worktree_name))
                        .map(|o| OrphanFanoutEntry {
                            vault_name: cfg_name.clone(),
                            worktree_name: o.worktree_name,
                            wip_branch: o.wip_branch,
                            worktree_path: o.worktree_path,
                        })
                        .collect())
                })
                .await
                .map_err(|e| anyhow::anyhow!("scan_orphan_fanouts task: {}", e))?;
            out.extend(found?);
        }
        Ok(out)
    }

    /// turbovault-84k: best-effort `abandon_fanout_by_info` over every
    /// entry in `active_fanouts`. Called from the SIGTERM/SIGINT handler.
    /// Logs each result, then clears the registry. Survives individual
    /// failures so a slow vault can't block shutdown.
    pub async fn shutdown_fanouts_best_effort(&self) {
        let snapshot: Vec<(String, ActiveFanoutRecord)> = {
            let guard = self.active_fanouts.read().await;
            guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        if snapshot.is_empty() {
            return;
        }
        log::info!("shutdown: abandoning {} active fanout(s)", snapshot.len());
        let vaults = match self.multi_vault_mgr.list_vaults().await {
            Ok(list) => list,
            Err(e) => {
                log::warn!("shutdown: list_vaults: {}", e);
                return;
            }
        };
        for (base_vault, record) in snapshot {
            let Some(base_cfg) = vaults
                .iter()
                .find(|v| v.config.name == base_vault)
                .map(|v| v.config.clone())
            else {
                log::warn!(
                    "shutdown: base vault {} disappeared, skipping abandon",
                    base_vault
                );
                continue;
            };
            let locks = self.get_or_init_git_locks(&base_vault).await;
            let info = record.info.clone();
            let path = base_cfg.path.clone();
            let join = tokio::task::spawn_blocking(move || -> Result<()> {
                let repo = VaultRepo::open_with_locks(&path, locks)
                    .map_err(|e| anyhow::anyhow!("open base vault: {}", e))?;
                repo.abandon_fanout_by_info(&info)
                    .map_err(|e| anyhow::anyhow!("abandon_fanout_by_info: {}", e))
            })
            .await;
            match join {
                Ok(Ok(())) => log::info!(
                    "shutdown: abandoned fanout fanout_id={} on base vault {}",
                    record.fanout_id,
                    base_vault
                ),
                Ok(Err(e)) => log::warn!(
                    "shutdown: failed to abandon fanout fanout_id={} on {}: {}",
                    record.fanout_id,
                    base_vault,
                    e
                ),
                Err(e) => log::warn!("shutdown: abandon task panicked for {}: {}", base_vault, e),
            }
            if let Err(e) = self
                .multi_vault_mgr
                .remove_vault(&record.fanout_vault_name)
                .await
            {
                log::warn!(
                    "shutdown: failed to deregister fanout vault {}: {}",
                    record.fanout_vault_name,
                    e
                );
            }
        }
        self.active_fanouts.write().await.clear();
    }

    /// turbovault-84k: startup hook — scan + log a warning per orphan
    /// detected. Operator decides whether to clean up (manual `git
    /// worktree remove` / `git branch -D`, or call `list_orphan_fanouts`
    /// MCP tool for inspection).
    pub async fn log_orphan_fanouts_warnings(&self) {
        match self.scan_orphan_fanouts(None).await {
            Ok(orphans) if orphans.is_empty() => {}
            Ok(orphans) => {
                log::warn!(
                    "Startup: detected {} orphan fanout worktree(s). Inspect via list_orphan_fanouts MCP tool; clean up with `git worktree remove <name>` + `git branch -D wip/<id>`.",
                    orphans.len()
                );
                for o in &orphans {
                    log::warn!(
                        "  orphan: vault={} worktree={} branch={} path={}",
                        o.vault_name,
                        o.worktree_name,
                        o.wip_branch,
                        o.worktree_path.display()
                    );
                }
            }
            Err(e) => log::warn!("Startup orphan fanout scan failed: {}", e),
        }
    }
}

/// turbovault-84k: one fanout artifact found by [`ObsidianMcpServer::
/// scan_orphan_fanouts`]. Wraps the substrate's `OrphanFanout` with the
/// owning vault's name (which the substrate alone can't know).
#[derive(Debug, Clone, serde::Serialize)]
pub struct OrphanFanoutEntry {
    pub vault_name: String,
    pub worktree_name: String,
    pub wip_branch: String,
    pub worktree_path: PathBuf,
}

impl Default for CoreToolHandler {
    fn default() -> Self {
        Self::new().expect("Failed to create default CoreToolHandler")
    }
}

impl CoreToolHandler {
    /// Attach and cache audit state for a newly constructed vault manager.
    async fn initialize_audit_for_manager(&self, vault_name: &str, manager: &mut VaultManager) {
        let vault_path = manager.vault_path().clone();
        match AuditLog::new(&vault_path).await {
            Ok(audit_log) => {
                let audit_log = Arc::new(audit_log);
                let snapshot_store =
                    Arc::new(SnapshotStore::new(audit_log.snapshot_dir().to_path_buf()));
                manager.set_audit_log(audit_log.clone(), snapshot_store.clone());

                self.audit_logs
                    .write()
                    .await
                    .insert(vault_name.to_string(), audit_log);
                self.snapshot_stores
                    .write()
                    .await
                    .insert(vault_name.to_string(), snapshot_store);
            }
            Err(error) => {
                log::warn!(
                    "Failed to initialize audit log for {}: {} (audit trail disabled)",
                    vault_name,
                    error
                );
            }
        }
    }

    /// Get a vault manager for the currently active vault (cached)
    async fn get_active_vault_manager(&self) -> McpResult<Arc<VaultManager>> {
        let vault_name = self.get_active_vault_name().await?;

        // Check cache first
        {
            let cache = self.vault_managers.read().await;
            if let Some(manager) = cache.get(&vault_name) {
                return Ok(manager.clone());
            }
        }

        // Not in cache - build, wire and publish it
        self.activate_vault_manager(&vault_name).await
    }

    /// turbovault-qae.5.5: THE activation path — the ONE place that builds a
    /// `VaultManager` and publishes it into `vault_managers`.
    ///
    /// Build → audit → initialize → wire → publish is a single operation, so a
    /// caller cannot publish an unwired manager: it never gets its hands on a
    /// manager that is not already wired and cached. That is the structural
    /// form of the bite-3a (turbovault-qae.5.3) fix, which was previously a
    /// three-step dance open-coded at every insert site — a git-backend manager
    /// cached before its reindex machinery was wired makes
    /// `get_active_vault_manager` cache-hit the unwired manager forever, and
    /// search staleness never closes. Every new call site gets the wiring for
    /// free; there is no site left that could forget it.
    async fn activate_vault_manager(&self, vault_name: &str) -> McpResult<Arc<VaultManager>> {
        let vault_config = self
            .multi_vault_mgr
            .get_vault_config(vault_name)
            .await
            .map_err(|e| McpError::internal(format!("No config for vault '{vault_name}': {e}")))?;

        let mut server_config = ServerConfig::default();
        let mut vault_config = vault_config;
        vault_config.is_default = true; // Mark as default so VaultManager::new() can find it
        server_config.vaults = vec![vault_config];

        let mut manager = VaultManager::new(server_config).map_err(|e| {
            McpError::internal(format!(
                "Failed to create vault manager for '{vault_name}': {e}"
            ))
        })?;

        // Audit state attaches through `&mut` — it has to land between `new()`
        // and the `Arc`, which is exactly what used to force each call site
        // into its own multi-step publish. Owning the whole sequence here is
        // what resolves it.
        self.initialize_audit_for_manager(vault_name, &mut manager)
            .await;

        // Initialize vault (scan files and build link graph) on first access
        manager.initialize().await.map_err(|e| {
            McpError::internal(format!("Failed to initialize vault '{vault_name}': {e}"))
        })?;

        let manager = Arc::new(manager);

        // M4c (bite 3a, turbovault-qae.5.3): wire the manager-owned reindex
        // tasks + change-listener (git vaults; no-op on Direct) BEFORE
        // publishing to the cache, so no concurrent first-access caller can
        // cache-hit and observe an unwired manager. If we lose the
        // double-check race below, our wired manager is dropped and its `Drop`
        // aborts the tasks it started.
        self.wire_manager_reindex(vault_name, &manager).await;

        // Publish — double-check to handle concurrent initialization races
        let mut cache = self.vault_managers.write().await;
        // Another task may have activated between our read-check and here; first writer wins
        if let Some(existing) = cache.get(vault_name) {
            return Ok(existing.clone());
        }
        cache.insert(vault_name.to_string(), manager.clone());

        Ok(manager)
    }

    /// M4c (bite 3a, turbovault-qae.5.3): wiring for a freshly-built
    /// git-backend manager — start its OWN reindex tasks (drainer +
    /// HEAD-ref listener, manager-owned) and register the R7 change-listener
    /// that feeds THIS server's tantivy + similarity engines. write-substrate-
    /// layering M4e deleted the server's own duplicate reindex machinery
    /// (the old `GitFileTools` path), so this is now the ONLY reindex wiring
    /// for a git-backend vault. No-op on a Direct vault.
    async fn wire_manager_reindex(&self, vault_name: &str, manager: &Arc<VaultManager>) {
        let is_git = self
            .multi_vault_mgr
            .get_vault_config(vault_name)
            .await
            .map(|c| matches!(c.write_backend, WriteBackend::Git))
            .unwrap_or(false);
        if !is_git {
            return;
        }

        // Register the R7 change-listener BEFORE starting the drainer, so a
        // drain pass can't fire into an unset listener. The listener captures
        // ONLY the search + similarity engine maps — NOT a whole `self` clone,
        // which would hold `vault_managers` → the `Arc<VaultManager>` → this
        // listener: a reference cycle that defeats the manager's `Drop`-based
        // task teardown (the manager would never drop absent an explicit
        // `remove_vault`, undoing the `Weak<Self>` care the tasks take). Those
        // two maps never hold the manager, so there is no cycle; search /
        // similarity stay ABOVE the vault layer — the manager only invokes this
        // callback (R2 dependency inversion).
        let search_engines = Arc::clone(&self.search_engines);
        let similarity_engines = Arc::clone(&self.similarity_engines);
        let name = vault_name.to_string();
        let listener: ChangeListener = Arc::new(move |changed: Vec<(String, bool)>| {
            let search_engines = Arc::clone(&search_engines);
            let similarity_engines = Arc::clone(&similarity_engines);
            let name = name.clone();
            // The listener is a synchronous `Fn`; spawn the async index work.
            tokio::spawn(async move {
                if !changed.is_empty() {
                    let cached = { search_engines.read().await.get(&name).cloned() };
                    if let Some(engine) = cached
                        && let Err(e) = engine.apply_changes(changed).await
                    {
                        log::warn!(
                            "M4c change-listener: search apply failed for '{name}', evicting: {e}"
                        );
                        search_engines.write().await.remove(&name);
                    }
                }
                similarity_engines.write().await.remove(&name);
            });
        });
        manager.set_change_listener(listener);

        // Start the background drainer + HEAD-ref listener only after the
        // change-listener is registered.
        manager.ensure_reindex_started();
    }

    /// Get audit tools for the active vault
    async fn get_audit_tools(&self) -> McpResult<AuditTools> {
        let vault_name = self.get_active_vault_name().await?;
        // Ensure vault manager exists (triggers audit log creation)
        let _ = self.get_active_vault_manager().await?;

        let logs = self.audit_logs.read().await;
        let stores = self.snapshot_stores.read().await;

        let audit_log = logs.get(&vault_name).cloned().ok_or_else(|| {
            McpError::internal("Audit log not available for this vault".to_string())
        })?;
        let snapshot_store = stores
            .get(&vault_name)
            .cloned()
            .ok_or_else(|| McpError::internal("Snapshot store not available".to_string()))?;

        Ok(AuditTools::new(audit_log, snapshot_store))
    }

    /// Invalidate cached similarity engine for the active vault (call after any write operation)
    async fn invalidate_similarity_cache(&self) {
        if let Ok(vault_name) = self.get_active_vault_name().await {
            let mut cache = self.similarity_engines.write().await;
            cache.remove(&vault_name);
        }
    }

    /// Invalidate cached search engine for the active vault (call after any
    /// write operation).
    ///
    /// write-substrate-layering M4e: the GWS.14c git-backend skip is gone —
    /// `VaultManager`'s own change-listener (`wire_manager_reindex`) now
    /// feeds the cached search engine incrementally on every git-backend
    /// apply/drain, so this hammer eviction is redundant-but-harmless there
    /// (the next query rebuilds cold instead of reusing the listener's
    /// incremental update). On the direct backend (no change-listener wired)
    /// this remains the only path that keeps the cache coherent.
    async fn invalidate_search_cache(&self) {
        let Ok(vault_name) = self.get_active_vault_name().await else {
            return;
        };
        let mut cache = self.search_engines.write().await;
        cache.remove(&vault_name);
    }

    /// Get or build search engine for a given vault (cached, lazy-initialized)
    async fn get_search_engine(
        &self,
        vault_name: &str,
        manager: &Arc<VaultManager>,
    ) -> McpResult<Arc<SearchEngine>> {
        // Check cache first (read lock — fast path)
        {
            let cache = self.search_engines.read().await;
            if let Some(engine) = cache.get(vault_name) {
                return Ok(engine.clone());
            }
        }

        // Build new engine (this indexes the entire vault via tantivy)
        let engine = SearchEngine::new(manager.clone())
            .await
            .map_err(|e| McpError::internal(format!("Failed to build search engine: {}", e)))?;
        let engine = Arc::new(engine);

        // Cache it — double-check to handle concurrent callers
        {
            let mut cache = self.search_engines.write().await;
            if let Some(existing) = cache.get(vault_name) {
                return Ok(existing.clone());
            }
            cache.insert(vault_name.to_string(), engine.clone());
        }

        Ok(engine)
    }

    /// Get or build similarity engine for the active vault
    async fn get_similarity_engine(&self) -> McpResult<Arc<SimilarityEngine>> {
        let vault_name = self.get_active_vault_name().await?;

        // Check cache
        {
            let cache = self.similarity_engines.read().await;
            if let Some(engine) = cache.get(&vault_name) {
                return Ok(engine.clone());
            }
        }

        // Build new engine
        let manager = self.get_active_vault_manager().await?;
        let engine = SimilarityEngine::new(manager)
            .await
            .map_err(|e| McpError::internal(format!("Failed to build similarity engine: {}", e)))?;
        let engine = Arc::new(engine);

        {
            let mut cache = self.similarity_engines.write().await;
            cache.insert(vault_name, engine.clone());
        }

        Ok(engine)
    }

    /// Helper to get active vault name
    async fn get_active_vault_name(&self) -> McpResult<String> {
        let vault_name = self.multi_vault_mgr.get_active_vault().await;
        if vault_name.is_empty() {
            return Err(McpError::internal(
                "No active vault. Use add_vault() to register a vault.".to_string(),
            ));
        }
        Ok(vault_name)
    }

    /// turbovault-5nn: true when the active vault is on the git backend AND has
    /// `git.require_commit_message = true`. Only meaningful on git (the direct
    /// backend produces no commits).
    async fn active_vault_requires_commit_message(&self) -> bool {
        self.multi_vault_mgr
            .get_active_vault_config()
            .await
            .map(|c| {
                matches!(c.write_backend, WriteBackend::Git)
                    && c.git
                        .as_ref()
                        .map(|g| g.require_commit_message)
                        .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// write-substrate-layering M4d: true when the active vault is on the git
    /// backend. The manager already dispatches the substrate per write; this is
    /// only for the handlers' backend-dependent DEFAULTS (delete_note's
    /// force-by-default, move_note's update-backlinks-by-default) which differ
    /// by backend and must be preserved (R10).
    async fn active_vault_is_git(&self) -> McpResult<bool> {
        Ok(self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map(|c| matches!(c.write_backend, WriteBackend::Git))
            .unwrap_or(false))
    }

    /// turbovault-qae.5.2: check-only half of the commit-message gate —
    /// trims/filters the caller-supplied message (whitespace-only counts as
    /// missing) and, when none remains, refuses if the active vault requires
    /// one. Returns `Ok(None)` when no message was given and none is
    /// required, leaving the fallback to the caller. Single-write tools go
    /// through `resolve_commit_message` below; multi-write tools (e.g.
    /// `generate_index`, which can't collapse several auto-derived subjects
    /// into one fallback) call this directly.
    async fn require_commit_message(
        &self,
        commit_message: Option<String>,
    ) -> McpResult<Option<String>> {
        let provided = commit_message
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        match provided {
            Some(m) => Ok(Some(m)),
            None if self.active_vault_requires_commit_message().await => {
                Err(McpError::invalid_request(
                    "this vault requires an explicit commit message (git.require_commit_message = true); pass a non-empty `commit_message` for this operation".to_string(),
                ))
            }
            None => Ok(None),
        }
    }

    /// turbovault-5nn: resolve a mutation's commit subject. A caller-supplied
    /// message (trimmed; whitespace-only counts as missing) always wins. When
    /// none is given, the active vault's `git.require_commit_message` decides:
    /// `true` → refuse loudly so nothing is auto-derived; `false` → use
    /// `fallback` (the historical auto-derived subject).
    async fn resolve_commit_message(
        &self,
        commit_message: Option<String>,
        fallback: impl FnOnce() -> String,
    ) -> McpResult<String> {
        Ok(self
            .require_commit_message(commit_message)
            .await?
            .unwrap_or_else(fallback))
    }

    /// Resolve the shared state for a `write_note` (vault name, manager, commit
    /// message). Paired with [`Self::finish_complete_note_write`].
    async fn prepare_complete_note_write(
        &self,
        path: &str,
        commit_message: Option<String>,
        fallback_operation: &str,
    ) -> McpResult<CompleteNoteWrite> {
        // Resolve the manager first and take the vault name from it, so the
        // whole write sees ONE active-vault snapshot even if `set_active_vault`
        // races on another connection.
        let manager = self.get_active_vault_manager().await?;
        let vault_name = manager.vault_name().to_string();
        let message = self
            .resolve_commit_message(commit_message, || format!("{fallback_operation} {path}"))
            .await?;
        Ok(CompleteNoteWrite {
            vault_name,
            manager,
            message,
        })
    }

    /// Invalidate derived caches after a `write_note`.
    async fn finish_complete_note_write(&self) {
        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
    }

    /// Helper to get both vault name and manager (eliminates 31 repeated preambles)
    /// This is the most common pattern across all tools
    async fn get_vault_pair(&self) -> McpResult<(String, Arc<VaultManager>)> {
        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;
        Ok((vault_name, manager))
    }
    // ==================== Test support (turbovault-6fo.18) ====================
    //
    // Public wrappers that expose internals the e2e tests need to drive
    // the substrate as a real MCP handler would. These mirror the
    // private helpers; suffixed `_test` to discourage production use.

    /// Test-only: expose the active vault's `VaultManager` so e2e tests
    /// can read the link graph after a substrate write.
    pub async fn get_active_vault_manager_test(&self) -> McpResult<Arc<VaultManager>> {
        self.get_active_vault_manager().await
    }

    /// turbovault-5nn test-only: resolve a commit subject through the same
    /// require_commit_message gate the mutation tools use (eager `fallback`).
    pub async fn resolve_commit_message_test(
        &self,
        commit_message: Option<String>,
        fallback: String,
    ) -> McpResult<String> {
        self.resolve_commit_message(commit_message, || fallback)
            .await
    }

    /// Test-only: check whether a `CommitLocks` registry is cached for
    /// `vault_name`. Used by turbovault-1ne cleanup tests.
    pub async fn has_git_locks_test(&self, vault_name: &str) -> bool {
        self.git_locks.read().await.contains_key(vault_name)
    }

    /// Test-only: invoke the `remove_vault` MCP tool from integration
    /// tests. Used by turbovault-1ne tests.
    pub async fn remove_vault_test(&self, name: &str) -> McpResult<serde_json::Value> {
        self.remove_vault_with_cleanup(name).await
    }

    async fn remove_vault_with_cleanup(&self, name: &str) -> McpResult<serde_json::Value> {
        {
            let fanouts = self.active_fanouts.read().await;
            if let Some(record) = fanouts.get(name) {
                return Err(McpError::invalid_request(format!(
                    "vault {name} has an active fanout (fanout_id={}); abandon_fanout first",
                    record.fanout_id
                )));
            }
            if let Some(record) = fanouts
                .values()
                .find(|record| record.fanout_vault_name == name)
            {
                return Err(McpError::invalid_request(format!(
                    "vault {name} is a fanout worktree (fanout_id={}); abandon_fanout from the base vault first",
                    record.fanout_id
                )));
            }
        }

        VaultLifecycleTools::new(self.multi_vault_mgr.clone())
            .remove_vault(name)
            .await
            .map_err(to_mcp_error)?;
        self.search_engines.write().await.remove(name);
        self.similarity_engines.write().await.remove(name);
        self.vault_managers.write().await.remove(name);
        self.git_locks.write().await.remove(name);

        if let Err(error) = self.persist_vault_state().await {
            log::warn!("Failed to persist vault state after removal: {error}");
        }
        StandardResponse::new(
            name,
            "remove_vault",
            serde_json::json!({"status": "removed"}),
        )
        .with_next_step("list_vaults")
        .to_json()
    }

    /// Test-only: expose the lazy `CommitLocks` initializer for tests
    /// that need to drive substrate operations against the same lock
    /// registry the server uses. Used by turbovault-1ne tests.
    pub async fn get_or_init_git_locks_test(&self, vault_name: &str) -> Arc<CommitLocks> {
        self.get_or_init_git_locks(vault_name).await
    }

    /// Test-only: install an `active_fanouts` entry so tests can
    /// observe the remove_vault refusal without driving the full
    /// `begin_fanout` MCP wire path. Used by turbovault-1ne.
    pub async fn register_active_fanout_test(
        &self,
        base_vault: &str,
        fanout_id: &str,
        info: FanoutInfo,
        fanout_vault_name: &str,
    ) {
        let rec = ActiveFanoutRecord {
            fanout_id: fanout_id.to_string(),
            info,
            fanout_vault_name: fanout_vault_name.to_string(),
        };
        self.active_fanouts
            .write()
            .await
            .insert(base_vault.to_string(), rec);
    }

    /// Test-only: clear the `active_fanouts` entry for `base_vault`.
    /// Used by turbovault-1ne cleanup.
    pub async fn clear_active_fanout_test(&self, base_vault: &str) {
        self.active_fanouts.write().await.remove(base_vault);
    }

    /// turbovault-8df / TV-009: legacy audit MCP tools (audit_log /
    /// audit_stats / rollback_preview / rollback_note) are not wired to
    /// the git substrate's history yet. On a git-backend vault they
    /// would silently return empty / inert results, which is the worst
    /// failure shape — looks like "nothing happened" but the writes did
    /// happen via the substrate. Refuse loudly instead, naming the
    /// alternative (git log) so callers know where to look.
    ///
    /// Phase A of the architecture §15.2 cutover plan; Phase B (wire
    /// rollback_note/rollback_preview to git-history restore) is part
    /// of GWS.15 cutover or a follow-on (turbovault-8df remediation
    /// notes).
    async fn refuse_audit_on_git_backend(&self, tool_name: &str) -> McpResult<()> {
        let cfg = self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map_err(|e| McpError::internal(format!("No active vault config: {}", e)))?;
        if cfg.write_backend == WriteBackend::Git {
            return Err(McpError::invalid_request(format!(
                "{} is not wired for the git substrate (turbovault-8df). Use `git log` / `git revert` / `git show` from the vault directory directly — every substrate write is one commit with a clear subject. Audit ergonomics through MCP are a planned cutover deliverable; until then this tool returns nothing useful for write_backend=git.",
                tool_name
            )));
        }
        Ok(())
    }

    /// Compute a content-version token in the active vault's backend-native
    /// format. Returned by `read_note`'s `hash` field and accepted by
    /// `write_note`/`edit_note`/`delete_note`/`move_note`'s `expected_hash`
    /// param. The token a read RETURNS must be the token CAS ACCEPTS — that
    /// is the contract this helper closes for the git backend.
    ///
    /// - **Direct backend:** 64-char SHA-256 of the working-tree bytes (the
    ///   token `turbovault_vault::compute_hash` has always produced).
    /// - **Git backend:** 40-char hex git blob OID, matching `expect_blob`'s
    ///   tree-side precondition exactly.
    ///
    /// turbovault-6sj / TV-011 (pre-this-fix, `read_note` always returned the
    /// SHA-256, so a round-trip on the git backend failed with
    /// `expected_hash must be a 40-char git blob oid hex`).
    async fn hash_for_active_backend(&self, content: &str) -> McpResult<String> {
        let cfg = self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map_err(|e| McpError::internal(format!("No active vault config: {}", e)))?;
        match cfg.write_backend {
            WriteBackend::Direct => Ok(turbovault_vault::compute_hash(content)),
            WriteBackend::Git => VaultRepo::blob_oid_of(content.as_bytes())
                .map(|oid| oid.to_string())
                .map_err(|e| McpError::internal(format!("blob_oid_of failed: {}", e))),
        }
    }

    async fn get_or_init_git_locks(&self, vault_name: &str) -> Arc<CommitLocks> {
        if let Some(l) = self.git_locks.read().await.get(vault_name) {
            return Arc::clone(l);
        }
        let fresh = Arc::new(CommitLocks::new());
        self.git_locks
            .write()
            .await
            .insert(vault_name.to_string(), Arc::clone(&fresh));
        fresh
    }
}
