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
    AnalysisTools, AuditTools, BatchOperation, CachedRepo, CasCollisionFlush, CommitHook,
    CommitLocks, DiffTools, DuplicateTools, ExportTools, FanoutInfo, FileTools, GitMergeStrategy,
    GraphTools, MetadataTools, QualityTools, ReindexQueue, RelationshipTools, SearchEngine,
    SearchQuery, SearchTools, SimilarityEngine, TemplateEngine, VaultLifecycleTools, VaultRepo,
    WriteMode, WriteTools, obsidian_uri,
};
use turbovault_vault::VaultManager;

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

/// Obsidian MCP Server - Vault-agnostic, multi-vault capable
#[derive(Clone)]
pub struct ObsidianMcpServer {
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
    /// turbovault-a0l (PERF-1): per-vault cached `VaultRepo` handle, opened once
    /// (with the shared `CommitLocks` + reindex `CommitHook`) and reused for
    /// every write — eliding the ~140µs `Repository::open` that dominated the
    /// substrate op latency when done per call. Lazy-initialized in
    /// `get_or_init_git_repo`, which also serves as the write-backend
    /// validation. Cross-process CAS stays safe (libgit2 re-reads refs under
    /// `lock_ref`); torn down on `remove_vault`.
    git_repos: Arc<RwLock<HashMap<String, CachedRepo>>>,
    /// Per-vault GWS.14 reindex queues (keyed by vault name,
    /// lazy-initialized). Each git-backend `VaultRepo` open registers a
    /// `CommitHook` that pushes onto this queue; read tools that depend on
    /// derived state call `flush_reindex_for_active_vault` to drain through
    /// HEAD before answering.
    git_reindex_queues: Arc<RwLock<HashMap<String, Arc<ReindexQueue>>>>,
    /// Per-vault GWS.14a background drainer task handles (keyed by vault
    /// name). Spawned lazily on the first git-backend `get_active_write_tools`
    /// call; drains the reindex queue in the background so steady-state
    /// reads never pay catch-up. Idempotent: re-spawning is guarded by the
    /// HashMap occupancy check.
    git_drainers: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// turbovault-bou: per-vault HEAD-ref polling listener. Detects
    /// out-of-band ref advances (manual git pull/checkout, sibling
    /// turbovault instance committing) and pushes the new oid onto the
    /// reindex queue. Lazy-spawned at first git-backend write (alongside
    /// the drainer). One task per vault; the value is the JoinHandle so
    /// vault removal can abort it.
    git_ref_listeners: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
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

impl ObsidianMcpServer {
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
            git_repos: Arc::new(RwLock::new(HashMap::new())),
            git_reindex_queues: Arc::new(RwLock::new(HashMap::new())),
            git_drainers: Arc::new(RwLock::new(HashMap::new())),
            git_ref_listeners: Arc::new(RwLock::new(HashMap::new())),
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
            let vault_config = self
                .multi_vault_mgr
                .get_vault_config(&name)
                .await
                .map_err(|e| Error::config_error(format!("get_vault_config({name}): {e}")))?;
            let mut server_config = ServerConfig::default();
            let mut vault_cfg = vault_config;
            vault_cfg.is_default = true;
            server_config.vaults = vec![vault_cfg];
            let manager = VaultManager::new(server_config)
                .map_err(|e| Error::config_error(format!("VaultManager::new({name}): {e}")))?;
            manager
                .initialize()
                .await
                .map_err(|e| Error::config_error(format!("initialize({name}): {e}")))?;
            let manager = Arc::new(manager);
            self.vault_managers
                .write()
                .await
                .insert(name.clone(), manager);
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

impl Default for ObsidianMcpServer {
    fn default() -> Self {
        Self::new().expect("Failed to create default ObsidianMcpServer")
    }
}

#[turbomcp::server(name = "obsidian-vault", version = "1.4.0")]
impl ObsidianMcpServer {
    /// Get a vault manager for the currently active vault (cached)
    async fn get_active_vault_manager(&self) -> McpResult<Arc<VaultManager>> {
        let vault_name = self.multi_vault_mgr.get_active_vault().await;

        let vault_config = self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map_err(|e| McpError::internal(format!("No active vault: {}", e)))?;

        // Check cache first
        {
            let cache = self.vault_managers.read().await;
            if let Some(manager) = cache.get(&vault_name) {
                return Ok(manager.clone());
            }
        }

        // Not in cache - create and initialize
        let mut server_config = ServerConfig::default();
        let mut vault_config = vault_config;
        vault_config.is_default = true; // Mark as default so VaultManager::new() can find it
        server_config.vaults = vec![vault_config];

        let mut manager = VaultManager::new(server_config)
            .map_err(|e| McpError::internal(format!("Failed to create vault manager: {}", e)))?;

        // Initialize audit log for this vault
        let vault_path = manager.vault_path().clone();
        match AuditLog::new(&vault_path).await {
            Ok(audit_log) => {
                let audit_log = Arc::new(audit_log);
                let snapshot_store =
                    Arc::new(SnapshotStore::new(audit_log.snapshot_dir().to_path_buf()));
                manager.set_audit_log(audit_log.clone(), snapshot_store.clone());

                // Cache audit log and snapshot store
                let mut logs = self.audit_logs.write().await;
                logs.insert(vault_name.clone(), audit_log);
                let mut stores = self.snapshot_stores.write().await;
                stores.insert(vault_name.clone(), snapshot_store);
            }
            Err(e) => {
                log::warn!(
                    "Failed to initialize audit log for {}: {} (audit trail disabled)",
                    vault_name,
                    e
                );
            }
        }

        // Initialize vault (scan files and build link graph) on first access
        manager
            .initialize()
            .await
            .map_err(|e| McpError::internal(format!("Failed to initialize vault: {}", e)))?;

        let manager = Arc::new(manager);

        // Cache it — double-check to handle concurrent initialization races
        {
            let mut cache = self.vault_managers.write().await;
            // Another task may have initialized between our read-check and here; first writer wins
            if let Some(existing) = cache.get(&vault_name) {
                return Ok(existing.clone());
            }
            cache.insert(vault_name, manager.clone());
        }

        Ok(manager)
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
    /// **GWS.14c skip:** if the active vault is on the git backend, the
    /// reindex drainer will incrementally `apply_changes` to the cached
    /// engine on the next flush — evicting here would force a full
    /// cold-rebuild on the next query and defeat the optimization. Legacy
    /// backend still gets the hammer.
    async fn invalidate_search_cache(&self) {
        let Ok(vault_name) = self.get_active_vault_name().await else {
            return;
        };
        if let Ok(cfg) = self.multi_vault_mgr.get_active_vault_config().await
            && cfg.write_backend == WriteBackend::Git
        {
            return;
        }
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
    /// `git.require_commit_message = true`. Only meaningful on git (the legacy
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
        let provided = commit_message
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        match provided {
            Some(m) => Ok(m),
            None if self.active_vault_requires_commit_message().await => {
                Err(McpError::invalid_request(
                    "this vault requires an explicit commit message (git.require_commit_message = true); pass a non-empty `commit_message` for this operation".to_string(),
                ))
            }
            None => Ok(fallback()),
        }
    }

    /// Helper to get both vault name and manager (eliminates 31 repeated preambles)
    /// This is the most common pattern across all tools
    async fn get_vault_pair(&self) -> McpResult<(String, Arc<VaultManager>)> {
        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;
        Ok((vault_name, manager))
    }

    /// Same as `get_vault_pair` but FIRST flushes the GWS.14 reindex queue
    /// for the active vault (no-op on the Legacy backend or when the queue
    /// is empty). Called by every read tool that touches derived state
    /// (link graph, search/tantivy, similarity, quality reports). Read
    /// tools that consume only working-tree bytes (`read_note`,
    /// `get_notes_info`, `inspect_frontmatter`, ...) stay on
    /// `get_vault_pair` since flushing would be wasted work.
    async fn get_vault_pair_with_reindex(&self) -> McpResult<(String, Arc<VaultManager>)> {
        self.flush_reindex_for_active_vault().await?;
        self.get_vault_pair().await
    }

    /// Build the backend-dispatching write surface for the active vault.
    /// `Legacy` → wraps the cached `VaultManager`-backed tools. `Git` →
    /// wraps a cached `VaultRepo` (so all in-process callers share one
    /// commit-section mutex per vault). The MCP tool methods call this and
    /// never branch on the backend themselves; cutover (GWS.15) deletes the
    /// `Legacy` arm here.
    async fn get_active_write_tools(&self) -> McpResult<WriteTools> {
        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;
        let vault_config = self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map_err(|e| McpError::internal(format!("No active vault config: {}", e)))?;

        match vault_config.write_backend {
            WriteBackend::Legacy => Ok(WriteTools::legacy(manager)),
            WriteBackend::Git => {
                let locks = self.get_or_init_git_locks(&vault_name).await;
                let queue = self.get_or_init_reindex_queue(&vault_name).await;
                let queue_for_hook = Arc::clone(&queue);
                let hook: CommitHook = Arc::new(move |_parent, commit| queue_for_hook.push(commit));

                // turbovault-a0l (PERF-1): open the per-vault `VaultRepo` ONCE
                // and cache it (shared `CommitLocks` + reindex hook bound in),
                // so writes reuse it instead of paying ~140µs `Repository::open`
                // each call. This is ALSO the write-backend validation — a
                // non-git path fails here, surfaced from `get_active_write_tools`
                // exactly as the prior throwaway open did, but without re-opening
                // on every write (it used to open twice per write: validate +
                // apply).
                let cached_repo = self
                    .get_or_init_git_repo(
                        &vault_name,
                        vault_config.path.as_path(),
                        Arc::clone(&locks),
                        Arc::clone(&hook),
                    )
                    .await?;

                // GWS.14a: spawn the background drainer the first time this
                // vault sees a git-backend write. Idempotent.
                self.spawn_drainer_if_needed(
                    &vault_name,
                    vault_config.path.clone(),
                    Arc::clone(&manager),
                    Arc::clone(&queue),
                )
                .await;

                // turbovault-bou: HEAD-ref polling listener. Detects
                // out-of-band ref advances (cross-instance commits, manual
                // git pull, etc.) and pushes the new oid onto the same
                // reindex queue the drainer drains.
                self.spawn_ref_listener_if_needed(
                    &vault_name,
                    vault_config.path.clone(),
                    Arc::clone(&queue),
                )
                .await;

                // GWS.14b: build a flush callback fired BEFORE the substrate
                // returns a ConcurrencyError. Captures the vault_name + path
                // + manager bound at WriteTools construction so the flush
                // targets the correct vault even if `set_active_vault` shifts
                // between changeset start and the conflict surfacing.
                let server = self.clone();
                let flush_vault_name = vault_name.clone();
                let flush_vault_path = vault_config.path.clone();
                let flush_manager = Arc::clone(&manager);
                let flush_on_collision: CasCollisionFlush = Arc::new(move || {
                    let server = server.clone();
                    let vault_name = flush_vault_name.clone();
                    let path = flush_vault_path.clone();
                    let manager = Arc::clone(&flush_manager);
                    Box::pin(async move {
                        server
                            .flush_reindex_for_vault(&vault_name, &path, manager)
                            .await
                            .map_err(|e| {
                                Error::config_error(format!("GWS.14b CAS-collision flush: {}", e))
                            })
                    })
                });

                // turbovault-lri: thread the per-vault `include_ignored`
                // policy through. Default `true` preserves pre-lri
                // "always-write" behavior; vault configs that set
                // `include_ignored: false` get the gitignore-refusal
                // pass in `apply_txn`. `vault_config.git` is optional;
                // a missing section is treated as defaults
                // (`include_ignored == true`).
                let include_ignored = vault_config
                    .git
                    .as_ref()
                    .map(|g| g.include_ignored)
                    .unwrap_or(true);
                Ok(WriteTools::git_with_hook_and_flush(
                    manager,
                    vault_config.path,
                    locks,
                    hook,
                    flush_on_collision,
                )
                .with_include_ignored(include_ignored)
                .with_cached_repo(cached_repo))
            }
        }
    }

    // ==================== Test support (turbovault-6fo.18) ====================
    //
    // Public wrappers that expose internals the e2e tests need to drive
    // the substrate as a real MCP handler would. These mirror the
    // private helpers; suffixed `_test` to discourage production use.

    /// Test-only: expose `get_active_write_tools` so e2e tests can drive
    /// the same dispatcher the `#[tool]` handlers use.
    pub async fn get_active_write_tools_test(&self) -> McpResult<turbovault_tools::WriteTools> {
        self.get_active_write_tools().await
    }

    /// Test-only: expose the active vault's `VaultManager` so e2e tests
    /// can read the link graph after a substrate write.
    pub async fn get_active_vault_manager_test(&self) -> McpResult<Arc<VaultManager>> {
        self.get_active_vault_manager().await
    }

    /// Test-only: borrow the per-vault `ReindexQueue` for assertions
    /// about external-commit observation (turbovault-bou).
    pub async fn get_reindex_queue_test(&self, vault_name: &str) -> Option<Arc<ReindexQueue>> {
        self.git_reindex_queues
            .read()
            .await
            .get(vault_name)
            .cloned()
    }

    /// Test-only: drain pending reindex work via the same flush helper
    /// every derived-state read uses.
    pub async fn flush_reindex_for_active_vault_test(&self) -> McpResult<()> {
        self.flush_reindex_for_active_vault().await
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

    /// Test-only: spawn a HEAD-ref listener with a CUSTOM polling
    /// interval, bypassing the lazy-spawn-with-5s-default path. e2e
    /// tests use this to keep wall-clock test time short.
    pub async fn spawn_ref_listener_with_interval_test(
        &self,
        vault_name: &str,
        interval: std::time::Duration,
    ) {
        let cfg = self
            .multi_vault_mgr
            .get_vault_config(vault_name)
            .await
            .expect("vault config");
        let queue = self.get_or_init_reindex_queue(vault_name).await;
        let mut listeners = self.git_ref_listeners.write().await;
        // Abort any existing listener (the test wants ITS interval).
        if let Some(handle) = listeners.remove(vault_name) {
            handle.abort();
        }
        let queue_for_task = Arc::clone(&queue);
        let path_for_task = cfg.path.clone();
        let handle = tokio::spawn(async move {
            turbovault_tools::watch_ref_changes(path_for_task, queue_for_task, interval).await;
        });
        listeners.insert(vault_name.to_string(), handle);
    }

    /// Test-only: check whether a background drainer task is registered
    /// for `vault_name`. Used by turbovault-1ne cleanup tests.
    pub async fn has_git_drainer_test(&self, vault_name: &str) -> bool {
        self.git_drainers.read().await.contains_key(vault_name)
    }

    /// Test-only: check whether a HEAD-ref listener task is registered
    /// for `vault_name`. Used by turbovault-1ne cleanup tests.
    pub async fn has_git_ref_listener_test(&self, vault_name: &str) -> bool {
        self.git_ref_listeners.read().await.contains_key(vault_name)
    }

    /// Test-only: check whether a `CommitLocks` registry is cached for
    /// `vault_name`. Used by turbovault-1ne cleanup tests.
    pub async fn has_git_locks_test(&self, vault_name: &str) -> bool {
        self.git_locks.read().await.contains_key(vault_name)
    }

    /// Test-only: invoke the `remove_vault` MCP tool from integration
    /// tests. Used by turbovault-1ne tests.
    pub async fn remove_vault_test(&self, name: &str) -> McpResult<serde_json::Value> {
        self.remove_vault(name.to_string()).await
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
    /// - **Legacy backend:** 64-char SHA-256 of the working-tree bytes (the
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
            WriteBackend::Legacy => Ok(turbovault_vault::compute_hash(content)),
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

    async fn get_or_init_reindex_queue(&self, vault_name: &str) -> Arc<ReindexQueue> {
        if let Some(q) = self.git_reindex_queues.read().await.get(vault_name) {
            return Arc::clone(q);
        }
        let fresh = Arc::new(ReindexQueue::new());
        self.git_reindex_queues
            .write()
            .await
            .insert(vault_name.to_string(), Arc::clone(&fresh));
        fresh
    }

    /// turbovault-a0l (PERF-1): get (or lazily open) the cached `VaultRepo` for
    /// `vault_name`, opened once with the shared `CommitLocks` + reindex
    /// `CommitHook` and reused for every write — eliding the per-op
    /// `Repository::open`. Opening also validates the path IS a usable git repo,
    /// so this replaces the throwaway validation-open that used to fire on every
    /// `get_active_write_tools` call. `locks`/`hook` are consumed only on the
    /// first (opening) call; later calls return the cached handle and ignore
    /// them (the handle already carries the first-bound pair — stable, since the
    /// hook just pushes onto the per-vault reindex queue).
    async fn get_or_init_git_repo(
        &self,
        vault_name: &str,
        path: &std::path::Path,
        locks: Arc<CommitLocks>,
        hook: CommitHook,
    ) -> McpResult<CachedRepo> {
        if let Some(repo) = self.git_repos.read().await.get(vault_name) {
            return Ok(Arc::clone(repo));
        }
        // Open once (outside the lock so a slow open doesn't block readers).
        let repo = VaultRepo::open_with_locks_and_hook(path, locks, hook).map_err(|e| {
            McpError::internal(format!(
                "vault {} has write_backend=git but {:?} is not a usable git repo: {}",
                vault_name, path, e
            ))
        })?;
        let handle: CachedRepo = Arc::new(std::sync::Mutex::new(repo));
        // Double-checked insert: another task may have opened concurrently while
        // we held no lock — keep whichever landed first, drop our spare.
        let mut map = self.git_repos.write().await;
        let entry = map
            .entry(vault_name.to_string())
            .or_insert_with(|| Arc::clone(&handle));
        Ok(Arc::clone(entry))
    }

    /// Spawn a per-vault background drainer (GWS.14a) the first time a
    /// git-backend write surface is constructed for `vault_name`. Idempotent
    /// — subsequent calls early-out if a drainer is already running.
    ///
    /// The drainer:
    /// - Awaits the queue's `Notify` (poked on every `push`).
    /// - Falls through to a 100ms safety-net poll so a missed/lost wake
    ///   still eventually drains.
    /// - Calls `flush_reindex_for_vault` (the same drain path read tools
    ///   use), which spawn_blocks the libgit2 work and applies graph deltas
    ///   in the async task.
    /// - Logs and continues on per-iteration errors.
    ///
    /// The task is never explicitly aborted; it lives for the server's
    /// lifetime. Vault-removal cleanup is a follow-up if the leak becomes
    /// a problem in practice.
    async fn spawn_drainer_if_needed(
        &self,
        vault_name: &str,
        vault_path: PathBuf,
        manager: Arc<VaultManager>,
        queue: Arc<ReindexQueue>,
    ) {
        {
            let drainers = self.git_drainers.read().await;
            if drainers.contains_key(vault_name) {
                return;
            }
        }
        let mut drainers = self.git_drainers.write().await;
        // Double-check after re-acquiring the write lock (race window).
        if drainers.contains_key(vault_name) {
            return;
        }
        let server = self.clone();
        let name = vault_name.to_string();
        let queue_for_task = Arc::clone(&queue);
        let manager_for_task = Arc::clone(&manager);
        let path_for_task = vault_path.clone();
        let handle = tokio::spawn(async move {
            loop {
                // Re-arm Notify each iteration. Tokio's notify_one() stores a
                // single permit when no waiter is parked, so a push() that
                // races between drain-completion and the next .notified() is
                // captured by the next await.
                let notified = queue_for_task.notify().notified();
                tokio::pin!(notified);
                tokio::select! {
                    _ = &mut notified => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
                }
                if queue_for_task.pending_count() == 0 {
                    continue;
                }
                if let Err(e) = server
                    .flush_reindex_for_vault(&name, &path_for_task, Arc::clone(&manager_for_task))
                    .await
                {
                    log::warn!("GWS.14a background drainer for vault {}: {}", name, e);
                }
            }
        });
        drainers.insert(vault_name.to_string(), handle);
    }

    /// turbovault-bou: lazy-spawn the HEAD-ref polling listener for a
    /// git-backend vault. Idempotent (skips if already running). Default
    /// poll interval: 5s — fast enough for cross-instance dogfooding to
    /// notice within a few seconds, slow enough that the listener's
    /// `VaultRepo::open` overhead is negligible.
    async fn spawn_ref_listener_if_needed(
        &self,
        vault_name: &str,
        vault_path: PathBuf,
        queue: Arc<ReindexQueue>,
    ) {
        {
            let listeners = self.git_ref_listeners.read().await;
            if listeners.contains_key(vault_name) {
                return;
            }
        }
        let mut listeners = self.git_ref_listeners.write().await;
        // Double-check after re-acquiring the write lock (race window).
        if listeners.contains_key(vault_name) {
            return;
        }
        let queue_for_task = Arc::clone(&queue);
        let path_for_task = vault_path.clone();
        let handle = tokio::spawn(async move {
            turbovault_tools::watch_ref_changes(
                path_for_task,
                queue_for_task,
                std::time::Duration::from_secs(5),
            )
            .await;
        });
        listeners.insert(vault_name.to_string(), handle);
    }

    // ==================== GWS.13 Fanout helpers ====================

    /// Where on disk to put a fanout's scratch worktree. Must live OUTSIDE
    /// any existing vault's working tree (git refuses nested worktrees).
    /// The per-process pid disambiguates concurrent servers on the same box.
    fn fanout_scratch_path(fanout_id: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "turbovault-fanout-{}-{}",
            std::process::id(),
            fanout_id
        ))
    }

    /// Convert a YAML-config merge strategy to the substrate's enum.
    fn config_to_git_merge_strategy(s: ConfigMergeStrategy) -> GitMergeStrategy {
        match s {
            ConfigMergeStrategy::MergeCommit => GitMergeStrategy::MergeCommit,
            ConfigMergeStrategy::FastForward => GitMergeStrategy::FastForward,
        }
    }

    fn parse_merge_strategy(s: Option<&str>) -> McpResult<Option<GitMergeStrategy>> {
        match s {
            None => Ok(None),
            Some("merge-commit") | Some("merge_commit") => Ok(Some(GitMergeStrategy::MergeCommit)),
            Some("fast-forward") | Some("fast_forward") => Ok(Some(GitMergeStrategy::FastForward)),
            Some(other) => Err(McpError::invalid_request(format!(
                "invalid merge_strategy '{}': must be 'merge-commit' or 'fast-forward'",
                other
            ))),
        }
    }

    /// Resolve the base vault for the active fanout context:
    /// - If the active vault has an open fanout (keyed by its own name), use it.
    /// - Otherwise, if the active vault IS a fanout vault, walk back to base.
    /// - Else: no active fanout, return None.
    async fn resolve_active_fanout(&self) -> Option<(String, ActiveFanoutRecord)> {
        let active = self.get_active_vault_name().await.ok()?;
        let fanouts = self.active_fanouts.read().await;
        if let Some(rec) = fanouts.get(&active) {
            return Some((active, rec.clone()));
        }
        // active might be the FANOUT vault — find its base by reverse scan.
        for (base, rec) in fanouts.iter() {
            if rec.fanout_vault_name == active {
                return Some((base.clone(), rec.clone()));
            }
        }
        None
    }

    /// Drain the active vault's reindex queue through HEAD before a
    /// derived-state query (graph/search/analysis) reads. No-op for
    /// vaults on the legacy backend (no queue) or when the queue is
    /// empty.
    ///
    /// **Send/!Sync note:** `VaultRepo` is opened + dropped inside a
    /// single `spawn_blocking` task so its libgit2 handle never crosses
    /// an `await`. The drainer's graph work is async (tokio RwLock) and
    /// runs after the blocking phase produces the path-set.
    ///
    /// Called by `get_vault_pair_with_reindex` (which derived-state read
    /// tools use); legacy/eviction-only tools keep `get_vault_pair`.
    async fn flush_reindex_for_active_vault(&self) -> McpResult<()> {
        let vault_name = self.get_active_vault_name().await?;
        let vault_config = self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map_err(|e| McpError::internal(format!("No active vault config: {}", e)))?;
        if vault_config.write_backend != WriteBackend::Git {
            return Ok(());
        }
        let manager = self.get_active_vault_manager().await?;
        self.flush_reindex_for_vault(&vault_name, &vault_config.path, manager)
            .await
    }

    /// Flush by vault name + path + manager (no "active vault" resolution).
    /// The GWS.14b CAS-collision callback uses this so the closure can
    /// flush the vault it was constructed for even if the active vault
    /// has shifted by the time `apply_txn` returns the conflict error.
    async fn flush_reindex_for_vault(
        &self,
        vault_name: &str,
        vault_path: &Path,
        manager: Arc<VaultManager>,
    ) -> McpResult<()> {
        let queue = match self.git_reindex_queues.read().await.get(vault_name) {
            Some(q) => Arc::clone(q),
            None => return Ok(()), // never opened a git-backed write -> nothing pending
        };
        // turbovault-9zr: serialize the whole flush pass. The drain below pops
        // commits (in spawn_blocking) BEFORE applying them, so two concurrent
        // flushers (the background drainer + this read-path flush) must not
        // interleave — otherwise one sees pending==0 while the other has popped
        // but not yet applied, and a read observes a stale graph. Acquire the
        // lock BEFORE the pending check so an empty queue here means a peer
        // flush already fully applied (not just popped).
        let _flush_guard = queue.lock_flush().await;
        if queue.pending_count() == 0 {
            return Ok(());
        }
        let locks = self.get_or_init_git_locks(vault_name).await;
        let path = vault_path.to_path_buf();

        // The drainer's full body is async (graph + search invalidation are
        // tokio-locked), but it opens + consumes a !Sync VaultRepo. Run
        // open inside spawn_blocking, hand back the resolved diff per
        // commit, apply graph deltas in the async task.
        loop {
            // Quick exit when drained.
            if queue.pending_count() == 0 {
                break;
            }
            // Per-iteration clones are MOVED into spawn_blocking; the
            // outer `queue`/`locks`/`path` bindings stay valid for
            // subsequent iterations and for the post-blocking work.
            let path_for_blocking = path.clone();
            let locks_for_blocking = Arc::clone(&locks);
            let queue_for_blocking = Arc::clone(&queue);
            let drained = tokio::task::spawn_blocking(move || {
                // Open the repo locally so its !Sync handle never escapes
                // this thread. Drain the diff bookkeeping (sync); the
                // graph apply runs back in the async task.
                let repo = VaultRepo::open_with_locks(&path_for_blocking, locks_for_blocking)
                    .map_err(|e| McpError::internal(format!("flush_reindex open repo: {}", e)))?;
                let mut batches = Vec::new();
                while let Some(commit) = queue_for_blocking.pop_front() {
                    let parent = repo.git_commit_first_parent(commit).map_err(|e| {
                        McpError::internal(format!("flush_reindex first-parent: {}", e))
                    })?;
                    let changes = repo
                        .diff_path_statuses(parent, commit)
                        .map_err(|e| McpError::internal(format!("flush_reindex diff: {}", e)))?;
                    batches.push((commit, changes));
                }
                drop(repo);
                Ok::<_, McpError>(batches)
            })
            .await
            .map_err(|e| McpError::internal(format!("flush_reindex task: {}", e)))??;
            if drained.is_empty() {
                break;
            }
            // Collapse all pending changes into a path→latest-presence map
            // so the search engine writer (GWS.14c) commits ONCE per drain
            // pass instead of once per pending commit (writer create is
            // ~10ms; amortizes nicely).
            let mut collapsed_for_search: HashMap<String, bool> = HashMap::new();

            for (commit, changes) in drained {
                let vault_root = manager.vault_path().clone();
                let graph_handle = manager.link_graph();
                for (rel_path, present) in changes {
                    collapsed_for_search.insert(rel_path.clone(), present);
                    let full_path = vault_root.join(&rel_path);
                    if present {
                        match manager.parse_file(std::path::Path::new(&rel_path)).await {
                            Ok(vf) => {
                                let mut graph = graph_handle.write().await;
                                let _ = graph.remove_file(&full_path);
                                if let Err(e) = graph.add_file(&vf) {
                                    log::warn!("flush_reindex add_file({}): {}", rel_path, e);
                                }
                                if let Err(e) = graph.update_links(&vf) {
                                    log::warn!("flush_reindex update_links({}): {}", rel_path, e);
                                }
                            }
                            Err(e) => {
                                log::debug!("flush_reindex parse_file({}) skip: {}", rel_path, e);
                            }
                        }
                    } else {
                        let mut graph = graph_handle.write().await;
                        let _ = graph.remove_file(&full_path);
                    }
                }
                queue.advance_cursor(commit);
            }

            // GWS.14c: incrementally update the cached SearchEngine if any.
            // Skip if not cached — the first query will build fresh from
            // current state (same outcome, simpler reasoning).
            if !collapsed_for_search.is_empty() {
                let cached = {
                    let engines = self.search_engines.read().await;
                    engines.get(vault_name).cloned()
                };
                if let Some(engine) = cached {
                    let change_vec: Vec<(String, bool)> =
                        collapsed_for_search.into_iter().collect();
                    if let Err(e) = engine.apply_changes(change_vec).await {
                        log::warn!(
                            "GWS.14c search incremental apply failed; falling back to evict: {}",
                            e
                        );
                        self.invalidate_search_cache().await;
                    }
                }
            }
            // Similarity engine stays cache-evict for now — incremental
            // TF-IDF lives in a follow-up if needed (the corpus IDF table
            // is corpus-wide, so per-doc add/remove without recomputing
            // IDF drifts the scores).
            self.invalidate_similarity_cache().await;
        }
        Ok(())
    }

    // ==================== Vault Context (LLM Discovery) ====================

    /// Get comprehensive vault context in a single call (LLMX: replaces 4+ separate calls)
    #[tool(
        description = "Get complete vault context (vaults, stats, capabilities, markdown dialect) in a single discovery call",
        usage = "Use as first call after connecting to understand server state and capabilities. Essential for initial orientation",
        performance = "Fast (<10ms typical), no filesystem operations if no active vault",
        related = ["explain_vault", "list_vaults", "quick_health_check"],
        examples = ["Check available vaults", "Verify server readiness", "Get OFM syntax resources"]
    )]
    async fn get_vault_context(&self) -> McpResult<serde_json::Value> {
        let active_vault = self.multi_vault_mgr.get_active_vault().await;
        let vaults = self
            .multi_vault_mgr
            .list_vaults()
            .await
            .map_err(|e| McpError::internal(format!("Failed to list vaults: {}", e)))?;

        let current_stats = if !active_vault.is_empty() {
            let manager = self.get_active_vault_manager().await?;
            let tools = GraphTools::new(manager);
            let health = tools
                .quick_health_check()
                .await
                .map_err(|e| McpError::internal(e.to_string()))?;
            Some(health)
        } else {
            None
        };

        let context = serde_json::json!({
            "active_vault": active_vault,
            "all_vaults": vaults.iter().map(|v| serde_json::json!({
                "name": v.name,
                "path": v.path,
                "is_default": v.is_default,
            })).collect::<Vec<_>>(),
            "current_stats": current_stats,
            "ready": !active_vault.is_empty(),
            "markdown_dialect": {
                "name": "Obsidian Flavored Markdown (OFM)",
                "base": ["CommonMark", "GitHub Flavored Markdown"],
                "resources": {
                    "complete_guide": "obsidian://syntax/complete-guide",
                    "quick_ref": "obsidian://syntax/quick-ref",
                    "examples": "obsidian://examples/sample-note"
                },
                "tools": {
                    "complete_guide": "get_ofm_syntax_guide",
                    "quick_ref": "get_ofm_quick_ref",
                    "examples": "get_ofm_examples"
                },
                "note": "Use MCP resources if supported by client, otherwise use tools as fallback",
                "key_features": [
                    "Wikilinks: [[note]] and [[note|alias]]",
                    "Embeds: ![[image.png]] and ![[note#section]]",
                    "Block refs: [[note#^block-id]] and ^block-id",
                    "Callouts: > [!note] Title",
                    "Tags: #tag and #nested/tag",
                    "Task lists: - [ ], - [x], - [/], and - [-]",
                    "Tables and strikethrough"
                ],
                "important_notes": [
                    "Use wikilinks [[note]] for internal references, not markdown links",
                    "Highlights, comments, and math/LaTeX are Obsidian syntax but are not yet extracted as first-class parser nodes",
                    "No markdown formatting inside HTML tags",
                    "Block IDs should be unique within a note"
                ]
            },
            "tools": {
                "file_operations": ["read_note", "write_note", "edit_note", "delete_note", "move_note", "move_file", "get_notes_info"],
                "search": ["search", "advanced_search", "recommend_related", "find_notes_from_template"],
                "link_analysis": ["get_backlinks", "get_forward_links", "get_related_notes", "get_hub_notes", "get_dead_end_notes"],
                "analysis": ["quick_health_check", "full_health_analysis", "get_broken_links", "detect_cycles"],
                "vault_management": ["add_vault", "list_vaults", "set_active_vault", "get_active_vault"],
                "templates": ["list_templates", "get_template", "create_from_template", "find_notes_from_template"],
                "metadata": ["get_metadata_value", "query_metadata", "update_frontmatter", "manage_tags"],
                "batch": ["batch_execute"],
            }
        });

        let is_empty = active_vault.is_empty();
        let response = StandardResponse::new(
            if is_empty {
                "none".to_string()
            } else {
                active_vault
            },
            "get_vault_context",
            context,
        )
        .with_meta(
            "timestamp".to_string(),
            serde_json::json!(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            ),
        )
        .with_next_steps(if is_empty {
            &["add_vault", "list_vaults"]
        } else {
            &["search", "quick_health_check", "get_hub_notes"]
        });

        response.to_json()
    }

    // ==================== File Operations ====================

    /// Read the contents of a note
    #[tool(
        description = "Read complete markdown content of a note from active vault",
        usage = "Use before editing, analyzing, or displaying notes. Supports all Obsidian Flavored Markdown syntax including wikilinks [[note]], embeds ![[image.png]], and block references ^block-id",
        performance = "Fast (<10ms typical). Returns path, content, and content hash for conflict detection",
        related = ["write_note", "edit_note", "get_backlinks"],
        examples = ["daily/2024-01-15.md", "projects/website-redesign.md"]
    )]
    async fn read_note(&self, path: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = FileTools::new(manager);
        let content = tools.read_file(&path).await.map_err(to_mcp_error)?;

        // Backend-aware version token (turbovault-6sj / TV-011): git blob
        // OID on git backend, SHA-256 on legacy. Same token round-trips via
        // `expected_hash` to write/edit/delete/move.
        let hash = self.hash_for_active_backend(&content).await?;

        let uri = obsidian_uri(&vault_name, &path);
        StandardResponse::new(
            &vault_name,
            "read_note",
            serde_json::json!({"path": path, "content": content, "hash": hash, "uri": uri}),
        )
        .with_read_next_steps()
        .to_json()
    }

    /// Write or update a note with optional mode (overwrite, append, prepend)
    #[tool(
        description = "Write a note in active vault with mode control: 'overwrite' (default) replaces entire file, 'append' adds to end, 'prepend' adds after frontmatter. CAS-by-default (turbovault-947): on `mode: overwrite`, writing to an EXISTING file requires either `expected_hash` (from read_note) or `force: true` — without one of those, the call is refused loudly to prevent silent clobber. Writing to an ABSENT path implicitly carries an expect-absent precondition that fails the loser of a concurrent-create race. Pass `commit_message` to set a meaningful git commit subject (turbovault-0bh); defaults to `write_note <path>`.",
        usage = "Use for creating new notes or replacing existing ones (with CAS proof) or appending/prepending. Default safety on overwrite: pass expected_hash from a prior read_note OR pass force=true to acknowledge blind overwrite. Append/prepend modes preserve historical behavior for now. Pass commit_message to explain WHY the write was made; first line is the subject, double-newline starts a body.",
        performance = "Moderate (<50ms typical). Includes filesystem write and link graph update",
        related = ["read_note", "edit_note", "create_from_template"],
        examples = ["mode: overwrite (default)", "create absent path (no hash, no force needed)", "overwrite with expected_hash", "force: true blind overwrite (escape hatch)", "commit_message: 'add concept page for X'"]
    )]
    async fn write_note(
        &self,
        path: String,
        content: String,
        mode: Option<String>,
        expected_hash: Option<String>,
        force: Option<bool>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let vault_name = self.get_active_vault_name().await?;
        let write_mode = WriteMode::from_str_opt(mode.as_deref()).map_err(to_mcp_error)?;
        let tools = self.get_active_write_tools().await?;
        let force = force.unwrap_or(false);
        // turbovault-0bh: caller-supplied or auto-derived commit message
        // (verb=tool_name per TV-008).
        let msg = self
            .resolve_commit_message(commit_message, || format!("write_note {}", path))
            .await?;

        // ---- turbovault-947: CAS-by-default for `mode: overwrite` ----
        //
        // 1. `force == true`  → blind overwrite (escape hatch; existing behavior).
        // 2. `expected_hash` supplied → CAS-checked overwrite (existing behavior).
        // 3. Append / Prepend modes → preserved (historical behavior; their
        //    semantics are documented as "operate on existing content"
        //    elsewhere; CAS-default for these modes is a separate
        //    behavioral change deferred until dogfooding surfaces it).
        // 4. Overwrite + no force + no expected_hash:
        //       - target ABSENT → strict-create via `WriteTools::create_file`
        //         (substrate carries `expect_absent`; concurrent winner makes
        //         the loser's CAS fail loudly with ConcurrencyError).
        //       - target EXISTS → refuse loudly with a clear actionable
        //         message; no blind clobber.
        let cas_default_applies =
            !force && expected_hash.is_none() && write_mode == WriteMode::Overwrite;
        if cas_default_applies {
            let manager = self.get_active_vault_manager().await?;
            let full_path = manager.vault_path().join(&path);
            let exists = tokio::fs::try_exists(&full_path).await.unwrap_or(false);
            if exists {
                return Err(McpError::invalid_request(format!(
                    "write_note refused (turbovault-947 CAS-by-default): path '{}' exists and no expected_hash or force=true was supplied. Either: \
                     (a) call read_note(path) and pass the returned hash as expected_hash, or \
                     (b) pass force=true to acknowledge a blind overwrite. \
                     This prevents silent clobber when concurrent agents target the same file.",
                    path
                )));
            }
            tools
                .create_file_with_message(&path, &content, &msg)
                .await
                .map_err(to_mcp_error)?;
        } else {
            tools
                .write_file_with_mode_and_message(
                    &path,
                    &content,
                    write_mode,
                    expected_hash.as_deref(),
                    &msg,
                )
                .await
                .map_err(to_mcp_error)?;
        }

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        let mode_str = mode.as_deref().unwrap_or("overwrite");
        StandardResponse::new(
            vault_name,
            "write_note",
            serde_json::json!({"path": path, "status": "written", "bytes": content.len(), "mode": mode_str}),
        )
        .with_write_next_steps()
        .to_json()
    }

    /// Edit note using SEARCH/REPLACE blocks
    #[tool(
        description = "Apply targeted edits using SEARCH/REPLACE blocks (safer than full overwrite). Pass `commit_message` for a meaningful git commit subject (turbovault-0bh); defaults to `edit_note <path>`.",
        usage = "Use for precise modifications without reading/writing entire file. Requires exact match of search text. Supports optional content hash for conflict detection and dry_run mode for preview. Returns applied changes, rejected changes, and new hash. Pass commit_message to explain WHY the edit was made.",
        performance = "Fast (<30ms typical). More efficient than read+write cycle for small edits",
        related = ["read_note", "write_note"],
        examples = ["edits with SEARCH/REPLACE", "commit_message: 'fix stale link in concept-X'"]
    )]
    async fn edit_note(
        &self,
        path: String,
        edits: String,
        expected_hash: Option<String>,
        dry_run: Option<bool>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let vault_name = self.get_active_vault_name().await?;
        let tools = self.get_active_write_tools().await?;
        let dry_run = dry_run.unwrap_or(false);
        // turbovault-0bh: caller-supplied or auto-derived.
        let msg = self
            .resolve_commit_message(commit_message, || format!("edit_note {}", path))
            .await?;
        let result = tools
            .edit_file_with_message(&path, &edits, expected_hash.as_deref(), dry_run, &msg)
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        StandardResponse::new(
            vault_name,
            "edit_note",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_next_steps(&["read_note", "write_note"])
        .to_json()
    }

    /// Delete a note (confirmation-protected)
    #[tool(
        description = "Permanently delete a note from active vault (irreversible, confirmation-protected). Default-safe (turbovault-oz6): REFUSES if the target has inbound backlinks, naming the linkers and pointing at the workarounds. Pass `on_backlinks: \"rewrite-stale-callout\"` to atomically delete AND strikethrough-wrap every inbound `[[target]]` (and variants) in the SAME commit (~~[[target]]~~). Pass `force: true` to bypass the backlink check (today's blind-delete behavior; linkers become broken). Pass `commit_message` for a meaningful git commit subject.",
        usage = "Use to remove unwanted notes. REQUIRES confirm_path parameter matching path exactly to prevent accidental deletion. By default the delete is refused if any other notes link to this one — explicit caller decision required (force vs. rewrite-stale). Pass expected_hash to guard the target against a concurrent edit.",
        performance = "Default: O(1) backlink lookup. With on_backlinks=rewrite-stale-callout: linear in inbound-link count (each source is read + rewritten in the same atomic commit).",
        related = ["get_backlinks", "get_broken_links", "move_note"],
        examples = [
            "path: drafts/old-idea.md, confirm_path: drafts/old-idea.md (refused if backlinks exist)",
            "on_backlinks: rewrite-stale-callout — strikethrough every linker as part of the delete",
            "force: true — delete anyway, leave linkers broken (legacy behavior)",
            "commit_message: 'remove superseded concept page'"
        ]
    )]
    async fn delete_note(
        &self,
        path: String,
        confirm_path: String,
        expected_hash: Option<String>,
        commit_message: Option<String>,
        force: Option<bool>,
        on_backlinks: Option<String>,
    ) -> McpResult<serde_json::Value> {
        // Safety: confirm_path must match path exactly
        if path != confirm_path {
            return Err(McpError::invalid_request(format!(
                "Confirmation failed: confirm_path '{}' does not match path '{}'. Both must be identical to proceed with deletion.",
                confirm_path, path
            )));
        }

        let vault_name = self.get_active_vault_name().await?;
        let tools = self.get_active_write_tools().await?;
        // turbovault-0bh: caller-supplied or auto-derived.
        let msg = self
            .resolve_commit_message(commit_message, || format!("delete_note {}", path))
            .await?;
        let force = force.unwrap_or(false);

        // turbovault-oz6: backlink-aware refuse / rewrite logic. The
        // graph is kept coherent by the substrate's drainer + GWS.14
        // queue + the external-ref listener (bou).
        let on_backlinks = on_backlinks.as_deref();
        let (rewrite_path, _link_sources_updated): (bool, Vec<String>) = if force {
            (false, Vec::new())
        } else {
            let backlinks = tools
                .list_inbound_backlinks(&path)
                .await
                .map_err(to_mcp_error)?;
            match (backlinks.is_empty(), on_backlinks) {
                (true, _) => (false, Vec::new()),
                (false, Some("rewrite-stale-callout")) => (true, backlinks),
                (false, _) => {
                    let sample = backlinks
                        .iter()
                        .take(5)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ");
                    let more = if backlinks.len() > 5 {
                        format!(" (+{} more)", backlinks.len() - 5)
                    } else {
                        String::new()
                    };
                    return Err(McpError::invalid_request(format!(
                        "delete_note refused (turbovault-oz6): path '{}' has {} inbound backlink(s): [{}]{}. Either: \
                         (a) pass on_backlinks=\"rewrite-stale-callout\" to atomically strikethrough every linker as part of the delete, or \
                         (b) pass force=true to delete anyway and leave the linkers broken. \
                         This prevents silently shipping broken backlinks.",
                        path,
                        backlinks.len(),
                        sample,
                        more
                    )));
                }
            }
        };

        let updated_sources: Vec<String> = if rewrite_path {
            let result = tools
                .delete_file_with_link_rewrite_to_stale(&path, expected_hash.as_deref(), &msg)
                .await
                .map_err(to_mcp_error)?;
            result.link_sources_updated
        } else {
            tools
                .delete_file_with_hash_and_message(&path, expected_hash.as_deref(), &msg)
                .await
                .map_err(to_mcp_error)?;
            Vec::new()
        };

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        StandardResponse::new(
            vault_name,
            "delete_note",
            serde_json::json!({
                "path": path,
                "status": "deleted",
                "force": force,
                "on_backlinks": on_backlinks.unwrap_or("refuse"),
                "link_sources_updated": updated_sources,
            }),
        )
        .with_next_step("quick_health_check")
        .to_json()
    }

    /// Move or rename a note
    #[tool(
        description = "Move or rename a note within active vault. By default (turbovault-lqr) atomically rewrites every inbound wikilink to point at the new path in the SAME commit: `[[old]]`, `[[old|alias]]`, `[[old#section]]`, `[[old#^block-id]]`, `![[old]]` (and path-prefix forms) all become their new-target equivalents. Pass `update_backlinks: false` to skip the rewrite (rename only; links become broken — matches the pre-fork behavior). Pass `commit_message` for a meaningful git commit subject (turbovault-0bh).",
        usage = "Use to reorganize vault structure or rename notes. By default the inbound-wikilink rewrite is atomic: a concurrent change to any link source aborts the whole move with ConcurrencyError. Pass `update_backlinks: false` if the legacy rename-only behavior is wanted. Pass expected_hash to guard the source against a concurrent edit. Pass commit_message to explain WHY.",
        performance = "Moderate. Linear in inbound-link count: each backlink source is read + rewritten + included in the same atomic commit.",
        related = ["get_backlinks", "get_forward_links", "search"],
        examples = ["update_backlinks: true (default) — atomic rename + link rewrite", "update_backlinks: false — rename only, links dangle", "commit_message: 'rename concept page to canonical slug'"]
    )]
    async fn move_note(
        &self,
        from: String,
        to: String,
        expected_hash: Option<String>,
        commit_message: Option<String>,
        update_backlinks: Option<bool>,
    ) -> McpResult<serde_json::Value> {
        let vault_name = self.get_active_vault_name().await?;
        let tools = self.get_active_write_tools().await?;
        // turbovault-0bh: caller-supplied or auto-derived.
        let msg = self
            .resolve_commit_message(commit_message, || format!("move_note {} -> {}", from, to))
            .await?;
        let update_backlinks = update_backlinks.unwrap_or(true);

        let updated_sources: Vec<String> = if update_backlinks {
            // turbovault-78w (TV-002): drain the reindex queue BEFORE resolving
            // backlinks. move_file_with_link_updates reads the in-memory link
            // graph directly; with a pending reindex it sees a STALE graph,
            // silently misses inbound links (no edge yet), and omits them from
            // the move commit — leaving dangling links AND an incoherent
            // post-move graph. This is the same derived-state drain the read
            // tools perform via get_vault_pair_with_reindex.
            self.flush_reindex_for_active_vault().await?;

            // turbovault-lqr: atomic rename + inbound-wikilink rewrite.
            // Returns the list of source files whose links were rewritten.
            let result = tools
                .move_file_with_link_updates(&from, &to, expected_hash.as_deref(), &msg)
                .await
                .map_err(to_mcp_error)?;
            result.link_sources_updated
        } else {
            // Legacy rename-only path. Links will dangle.
            tools
                .move_file_with_hash_and_message(&from, &to, expected_hash.as_deref(), &msg)
                .await
                .map_err(to_mcp_error)?;
            Vec::new()
        };

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        StandardResponse::new(
            vault_name,
            "move_note",
            serde_json::json!({
                "from": from,
                "to": to,
                "status": "moved",
                "update_backlinks": update_backlinks,
                "link_sources_updated": updated_sources,
            }),
        )
        .with_next_steps(&["get_backlinks", "get_forward_links"])
        .to_json()
    }

    // ==================== Search & Links ====================

    /// Find all notes that link to this note
    #[tool(
        description = "Find all notes that link TO this note (incoming links)",
        usage = "Use to understand note importance in knowledge graph, discover related content, and analyze impact before deletion. Essential for bidirectional link analysis.",
        performance = "Fast retrieval from pre-built link graph (<50ms typical)",
        related = ["get_forward_links", "get_related_notes", "get_hub_notes"],
        examples = []
    )]
    async fn get_backlinks(&self, path: String) -> McpResult<serde_json::Value> {
        // turbovault-brs / TV-001: derived-state read — flush pending
        // reindex queue before answering so concurrent commits land in
        // the link graph first.
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = SearchTools::new(manager);
        let backlinks = tools.find_backlinks(&path).await.map_err(to_mcp_error)?;

        let count = backlinks.len();
        let response =
            StandardResponse::new(vault_name, "get_backlinks", serde_json::json!(backlinks))
                .with_count(count)
                .with_next_step("get_forward_links")
                .with_next_step("get_related_notes");

        let response = if count == 0 {
            response.with_warning("Note has no incoming links".to_string())
        } else {
            response
        };

        response.to_json()
    }

    /// Find all notes that this note links to
    #[tool(
        description = "Find all notes that this note links TO (outgoing links)",
        usage = "Use to understand note dependencies, validate link integrity, and explore connection patterns. Pair with get_backlinks for bidirectional link analysis.",
        performance = "Fast retrieval from pre-built link graph (<50ms typical)",
        related = ["get_backlinks", "get_related_notes", "get_broken_links"],
        examples = []
    )]
    async fn get_forward_links(&self, path: String) -> McpResult<serde_json::Value> {
        // turbovault-brs / TV-001: derived-state read — flush pending
        // reindex queue before answering.
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = SearchTools::new(manager);
        let links = tools
            .find_forward_links(&path)
            .await
            .map_err(to_mcp_error)?;

        let count = links.len();
        let response =
            StandardResponse::new(vault_name, "get_forward_links", serde_json::json!(links))
                .with_count(count)
                .with_next_step("get_backlinks")
                .with_next_step("get_related_notes");

        response.to_json()
    }

    /// Find related notes (by link proximity)
    #[tool(
        description = "Find notes connected within N hops in the link graph (default 2 hops)",
        usage = "Use to discover non-obvious relationships through graph traversal. Ideal for recommendations, cluster analysis, and exploring knowledge neighborhoods. Configurable max_hops parameter.",
        performance = "Graph traversal speed varies by depth: 2 hops <100ms typical, 3+ hops may take longer on large vaults",
        related = ["recommend_related", "get_hub_notes", "suggest_links"],
        examples = []
    )]
    async fn get_related_notes(
        &self,
        path: String,
        max_hops: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        // turbovault-brs / TV-001: derived-state (graph traversal) — flush
        // pending reindex queue before answering.
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = SearchTools::new(manager);
        let max_hops = max_hops.unwrap_or(2).min(5); // Cap at 5 hops to prevent runaway traversal
        let related = tools
            .find_related_notes(&path, max_hops)
            .await
            .map_err(to_mcp_error)?;

        let count = related.len();
        let response =
            StandardResponse::new(vault_name, "get_related_notes", serde_json::json!(related))
                .with_count(count)
                .with_meta("max_hops", serde_json::json!(max_hops));

        response.to_json()
    }

    // ==================== Analysis ====================

    /// Find hub notes (highly connected)
    #[tool(
        description = "Find the top N most connected notes in the vault (default 10). Returns notes ranked by total link count (incoming + outgoing). Hub notes are central to knowledge graph structure and often represent key concepts or index pages.",
        usage = "Identify knowledge centers, validate vault organization, discover MOCs (Maps of Content)",
        performance = "<50ms typical, scales linearly with vault size",
        related = ["get_centrality_ranking", "get_dead_end_notes", "explain_vault"],
        examples = []
    )]
    async fn get_hub_notes(&self, top_n: Option<usize>) -> McpResult<serde_json::Value> {
        let top_n = top_n.unwrap_or(10);
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = GraphTools::new(manager);
        let hubs = tools.get_hub_notes(top_n).await.map_err(to_mcp_error)?;

        let count = hubs.len();
        let response = StandardResponse::new(
            vault_name,
            "get_hub_notes",
            serde_json::to_value(&hubs).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_next_step("get_related_notes");

        response.to_json()
    }

    /// Find dead-end notes (incoming but no outgoing)
    #[tool(
        description = "Find notes with incoming links but NO outgoing links (knowledge dead-ends). Returns list of paths with backlink counts. Dead-ends may indicate incomplete notes, missing connections, or final destination topics.",
        usage = "Identify incomplete notes needing expansion, discover topics lacking context, prioritize linking work",
        performance = "<100ms typical, graph traversal O(N)",
        related = ["suggest_links", "get_hub_notes", "get_isolated_clusters"],
        examples = []
    )]
    async fn get_dead_end_notes(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = GraphTools::new(manager);
        let dead_ends = tools.get_dead_end_notes().await.map_err(to_mcp_error)?;

        let count = dead_ends.len();
        let response = StandardResponse::new(
            vault_name,
            "get_dead_end_notes",
            serde_json::json!(dead_ends),
        )
        .with_count(count);

        response.to_json()
    }

    /// Find isolated clusters in vault
    #[tool(
        description = "Find disconnected groups of notes (subgraphs with no connections to main graph). Returns clusters as arrays of paths. Isolated clusters may represent separate projects, orphaned content, or incomplete knowledge areas.",
        usage = "Improve vault connectivity, discover orphaned content, validate vault structure",
        performance = "<200ms typical, uses union-find algorithm O(N)",
        related = ["suggest_links", "get_dead_end_notes", "full_health_analysis"],
        examples = []
    )]
    async fn get_isolated_clusters(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = GraphTools::new(manager);
        let clusters = tools.get_isolated_clusters().await.map_err(to_mcp_error)?;

        let count = clusters.len();
        let response = StandardResponse::new(
            vault_name,
            "get_isolated_clusters",
            serde_json::json!(clusters),
        )
        .with_count(count);

        response.to_json()
    }

    // ==================== Health & Validation ====================

    /// Quick health check (0-100 score)
    #[tool(
        description = "Perform fast health assessment of active vault returning 0-100 score",
        usage = "Use as first diagnostic before deeper analysis. Score <60 suggests issues needing attention",
        performance = "Fast - optimized for speed with <100ms typical response using heuristics not exhaustive analysis",
        related = ["full_health_analysis", "get_broken_links", "detect_cycles"],
        examples = ["quick vault check", "is my vault healthy?", "vault health score"]
    )]
    async fn quick_health_check(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = GraphTools::new(manager);
        let health = tools.quick_health_check().await.map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "quick_health_check",
            serde_json::to_value(&health).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_next_step("full_health_analysis")
        .with_next_step(if health.is_healthy {
            "recommend_related"
        } else {
            "get_broken_links"
        });

        response.to_json()
    }

    /// Full health analysis with detailed report
    #[tool(
        description = "Comprehensive vault health report with detailed metrics including broken links, orphan analysis, link density, cluster analysis, and recommendations",
        usage = "Use when quick_health_check reveals issues or before major vault refactoring. Provides actionable insights for vault improvement",
        performance = "Slow - may take several seconds on large vaults. Significantly slower than quick_health_check due to exhaustive analysis",
        related = ["quick_health_check", "export_health_report", "explain_vault"],
        examples = ["detailed health analysis", "comprehensive vault check", "what are all my vault issues?"]
    )]
    async fn full_health_analysis(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = GraphTools::new(manager);
        let health = tools.full_health_analysis().await.map_err(to_mcp_error)?;

        let mut response = StandardResponse::new(
            vault_name,
            "full_health_analysis",
            serde_json::to_value(&health).map_err(|e| McpError::internal(e.to_string()))?,
        );

        // Add metadata about analysis
        response = response.with_meta("analysis_type", serde_json::json!("comprehensive"));

        // Suggest next actions based on health status
        if health.broken_links_count > 0 {
            response = response.with_next_step("get_broken_links");
        }
        if health.orphaned_notes_count > 0 {
            response = response.with_next_step("suggest_links");
        }

        response.to_json()
    }

    /// Get all broken links in vault
    #[tool(
        description = "Find all links pointing to non-existent notes with source path, target path, link text, and line number for each broken link",
        usage = "Use to identify notes to create or links to fix. Broken links harm navigation and indicate incomplete knowledge graph",
        performance = "Moderate - scans all notes and validates link targets, scales with vault size",
        related = ["suggest_links", "full_health_analysis", "export_broken_links"],
        examples = ["find broken links", "which links are broken?", "show missing note targets"]
    )]
    async fn get_broken_links(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = GraphTools::new(manager);
        let broken = tools.get_broken_links().await.map_err(to_mcp_error)?;

        let count = broken.len();
        let response =
            StandardResponse::new(vault_name, "get_broken_links", serde_json::json!(broken))
                .with_count(count);

        let response = if count > 0 {
            response
                .with_warning(format!("Found {} broken links", count))
                .with_next_step("export_broken_links")
        } else {
            response
        };

        response.to_json()
    }

    /// Detect cycles in link graph
    #[tool(
        description = "Detect circular reference chains in the link graph returning all cycles as arrays of paths",
        usage = "Use for graph topology analysis. Cycles aren't necessarily bad (many knowledge domains are naturally circular) but may indicate redundant structure or need for hub notes",
        performance = "Moderate - performs graph traversal to detect cycles, scales with vault complexity and link density",
        related = ["get_hub_notes", "full_health_analysis", "get_related_notes"],
        examples = ["find circular links", "detect reference cycles", "A→B→C→A patterns"]
    )]
    async fn detect_cycles(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = GraphTools::new(manager);
        let cycles = tools.detect_cycles().await.map_err(to_mcp_error)?;

        let count = cycles.len();
        let response =
            StandardResponse::new(vault_name, "detect_cycles", serde_json::json!(cycles))
                .with_count(count);

        let response = if count > 0 {
            response
                .with_warning("Cycles detected in link graph".to_string())
                .with_next_step("get_broken_links")
        } else {
            response
        };

        response.to_json()
    }

    /// **HOLISTIC VAULT OVERVIEW** - Complete gestalt view for LLMs (FIX 7: Single call replaces 5+ separate calls)
    /// Provides all essential vault structure info at once: organization, health, hubs, orphans, recommendations
    #[tool(
        description = "Generate holistic vault overview in a single comprehensive call",
        usage = "Use as comprehensive diagnostic or for presenting complete vault state. Replaces 5+ separate calls (scan + health + hubs + orphans + stats)",
        performance = "SLOW (1-5 seconds on large vaults) - aggregates multiple analyses. Use quick_health_check for fast diagnostics",
        related = ["get_vault_context", "full_health_analysis", "get_hub_notes", "quick_health_check"],
        examples = ["Get complete vault status before refactoring", "Present vault health to user", "Generate comprehensive diagnostic report"]
    )]
    async fn explain_vault(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let graph_tools = GraphTools::new(manager.clone());
        let analysis_tools = AnalysisTools::new(manager.clone());

        // Get all data efficiently (parallelizable)
        let files = manager.scan_vault().await.map_err(to_mcp_error)?;
        let health = graph_tools
            .quick_health_check()
            .await
            .map_err(to_mcp_error)?;
        let hubs = graph_tools.get_hub_notes(10).await.map_err(to_mcp_error)?;
        let dead_ends = graph_tools
            .get_dead_end_notes()
            .await
            .map_err(to_mcp_error)?;
        let stats = analysis_tools
            .get_vault_stats()
            .await
            .map_err(to_mcp_error)?;

        // Organize files by folder
        let mut folders: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for file in &files {
            if file.ends_with(".md") {
                let file_str = file.to_string_lossy().to_string();
                let parts: Vec<&str> = file_str.rsplitn(2, '/').collect();
                let folder = if parts.len() > 1 {
                    parts[1].to_string()
                } else {
                    "root".to_string()
                };
                folders.entry(folder).or_default().push(file_str);
            }
        }

        // Create holistic overview
        let overview = serde_json::json!({
            "vault_name": vault_name,
            "quick_facts": {
                "total_files": stats.total_files,
                "total_links": stats.total_links,
                "orphaned": stats.orphaned_files,
                "health_score": health.health_score,
                "is_healthy": health.is_healthy
            },
            "structure": {
                "folders": folders.keys().collect::<Vec<_>>(),
                "file_count_by_folder": folders.iter()
                    .map(|(k, v)| (k.clone(), v.len()))
                    .collect::<std::collections::HashMap<_, _>>(),
            },
            "key_insights": {
                "hub_notes": hubs.iter().take(5).map(|(path, count)| serde_json::json!({"path": path, "connections": count})).collect::<Vec<_>>(),
                "dead_ends": dead_ends.iter().take(5).cloned().collect::<Vec<_>>(),
                "average_links_per_file": stats.average_links_per_file,
            },
            "recommendations": {
                "action_1": if stats.orphaned_files > 0 {
                    format!("Link {} orphaned notes to main index or other hub notes", stats.orphaned_files)
                } else {
                    "Vault is well-connected".to_string()
                },
                "action_2": if health.broken_links_count > 0 {
                    format!("Fix {} broken links (use get_broken_links for details)", health.broken_links_count)
                } else {
                    "No broken links".to_string()
                },
                "action_3": if hubs.len() > 3 {
                    "Create hub pages for your top 3-5 topics".to_string()
                } else {
                    "Consider creating more cross-linking between topics".to_string()
                }
            }
        });

        let response = StandardResponse::new(vault_name, "explain_vault", overview)
            .with_meta(
                "view_type".to_string(),
                serde_json::json!("holistic_gestalt"),
            )
            .with_meta(
                "alternatives".to_string(),
                serde_json::json!([
                    "search() - Find notes by keyword",
                    "get_hub_notes() - See most connected notes",
                    "full_health_analysis() - Detailed health report",
                    "query_metadata() - Search by frontmatter"
                ]),
            )
            .with_next_steps(&[
                if stats.orphaned_files > 0 {
                    "get_dead_end_notes"
                } else {
                    "search"
                },
                if health.broken_links_count > 0 {
                    "get_broken_links"
                } else {
                    "get_hub_notes"
                },
            ]);

        response.to_json()
    }

    // ==================== Search (LLM Discovery) ====================

    /// Search vault by keyword
    #[tool(
        description = "Full-text search across all notes using Tantivy search engine with BM25 ranking",
        usage = "Use for discovering content by keywords. Case-insensitive, supports phrase queries with quotes. For filtered searches, use advanced_search",
        performance = "<100ms on 10k notes, <500ms on 100k notes",
        related = ["advanced_search", "recommend_related", "query_metadata"],
        examples = ["\"project alpha\"", "authentication", "urgent tasks"]
    )]
    async fn search(&self, query: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;
        let results = engine.search(&query).await.map_err(to_mcp_error)?;

        let result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let count = extract_count(&result_data);

        let response = StandardResponse::new(vault_name, "search", result_data)
            .with_count(count)
            .with_next_step("advanced_search")
            .with_next_step("recommend_related");

        response.to_json()
    }

    /// Advanced search with filters
    #[tool(
        description = "Enhanced search with tag, frontmatter, and path filters returning ranked results with match context",
        usage = "Use when search() returns too many results or you need filtered results. Supports tag filters, frontmatter key-value filters (AND logic), path exclusions, and custom result limits",
        performance = "Fast to Moderate - uses Tantivy search engine with BM25 ranking, additional filtering adds minimal overhead",
        related = ["search", "search_by_frontmatter", "query_metadata", "find_notes_from_template"],
        examples = [
            "search 'project' tags:['work', 'active']",
            "find notes tagged 'important'",
            "query with frontmatter_filters:[{key:'type', value:'task'}, {key:'status', value:'active'}]",
            "search 'meeting' exclude_paths:['archive/'] limit:20"
        ]
    )]
    async fn advanced_search(
        &self,
        query: String,
        tags: Option<Vec<String>>,
        frontmatter_filters: Option<Vec<FrontmatterFilter>>,
        exclude_paths: Option<Vec<String>>,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;

        let result_limit = limit.unwrap_or(10);
        let mut search_query = SearchQuery::new(query).limit(result_limit);

        if let Some(tags) = tags {
            search_query = search_query.with_tags(tags);
        }
        if let Some(filters) = frontmatter_filters {
            for f in filters {
                search_query = search_query.with_frontmatter(f.key, f.value);
            }
        }
        if let Some(excludes) = exclude_paths {
            search_query = search_query.exclude(excludes);
        }

        let results = engine
            .advanced_search(search_query)
            .await
            .map_err(to_mcp_error)?;
        let result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let count = extract_count(&result_data);

        let response = StandardResponse::new(vault_name, "advanced_search", result_data)
            .with_count(count)
            .with_next_step("search");

        response.to_json()
    }

    /// Search by frontmatter key-value pair
    #[tool(
        description = "Find notes where a frontmatter field matches a value. Returns up to 100 results ranked by relevance",
        usage = "Use for structured queries like finding all notes with type:'task' or status:'active'. For multiple filters combined with AND logic, use advanced_search with frontmatter_filters instead",
        performance = "Moderate - scans indexed content then filters by frontmatter, <200ms on 10k notes",
        related = ["advanced_search", "query_metadata", "get_metadata_value"],
        examples = ["key:'type' value:'task'", "key:'status' value:'active'", "key:'project' value:'alpha'"]
    )]
    async fn search_by_frontmatter(
        &self,
        key: String,
        value: String,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;

        let results = engine
            .search_by_frontmatter(&key, &value)
            .await
            .map_err(to_mcp_error)?;
        let result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let count = extract_count(&result_data);

        let response = StandardResponse::new(vault_name, "search_by_frontmatter", result_data)
            .with_count(count)
            .with_next_steps(&["advanced_search", "read_note"]);

        response.to_json()
    }

    /// Find related notes (recommendations engine)
    #[tool(
        description = "ML-powered note recommendations based on content similarity and link proximity with similarity scores and reasoning",
        usage = "Ideal for discovering non-obvious connections and suggesting reading paths. More sophisticated than get_related_notes which uses only graph structure",
        performance = "Slow - uses TF-IDF + graph features requiring content analysis and ML computations, may take seconds on large vaults",
        related = ["get_related_notes", "suggest_links", "search"],
        examples = ["recommend notes related to 'Machine Learning'", "find similar notes", "what should I read next?"]
    )]
    async fn recommend_related(&self, path: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;
        let results = engine
            .recommend_related(&path)
            .await
            .map_err(to_mcp_error)?;

        let result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let count = extract_count(&result_data);

        let response = StandardResponse::new(vault_name, "recommend_related", result_data)
            .with_count(count)
            .with_next_step("get_related_notes");

        response.to_json()
    }

    // ==================== SQL Frontmatter Queries (feature = "sql") ====================

    /// Inspect frontmatter schema
    #[tool(
        description = "Inspect the frontmatter schema across all vault notes — shows column names, types, nullability, and counts. Call this before writing SQL queries to discover available columns",
        usage = "Always call this first before query_frontmatter_sql so you know what columns exist. Returns the full schema of the 'files' table",
        performance = "Moderate - scans all vault files to collect schema metadata",
        related = ["query_frontmatter_sql", "query_metadata", "advanced_search"],
        examples = ["inspect schema to see available columns"]
    )]
    async fn inspect_frontmatter(&self) -> McpResult<serde_json::Value> {
        #[cfg(feature = "sql")]
        {
            let (vault_name, manager) = self.get_vault_pair().await?;
            let engine = FrontmatterSqlEngine::new(manager);
            let result = engine.inspect().await.map_err(to_mcp_error)?;

            let response = StandardResponse::new(vault_name, "inspect_frontmatter", result)
                .with_next_step("query_frontmatter_sql");

            response.to_json()
        }
        #[cfg(not(feature = "sql"))]
        {
            Err(McpError::internal(
                "SQL feature not enabled. Rebuild TurboVault with: cargo build --features sql"
                    .to_string(),
            ))
        }
    }

    /// Execute SQL query against frontmatter
    #[tool(
        description = "Execute arbitrary SQL against a 'files' table built from all vault note frontmatter. Each note becomes a row with 'path' + all frontmatter keys as columns. Supports WHERE, JOIN, GROUP BY, ORDER BY, LIMIT, subqueries, and aggregations",
        usage = "Use for complex structured queries that simple tools can't express. Call inspect_frontmatter first to discover available columns. The table is named 'files' — query it directly with standard SQL",
        performance = "Moderate to Slow - rebuilds in-memory table from vault on each call, then executes SQL. Proportional to vault size",
        related = ["inspect_frontmatter", "query_metadata", "advanced_search"],
        examples = [
            "SELECT path, status, type FROM files WHERE status = 'active' AND type = 'task'",
            "SELECT status, COUNT(*) as cnt FROM files GROUP BY status ORDER BY cnt DESC",
            "SELECT path FROM files WHERE tags IS NOT NULL ORDER BY path LIMIT 20"
        ]
    )]
    async fn query_frontmatter_sql(&self, sql: String) -> McpResult<serde_json::Value> {
        #[cfg(feature = "sql")]
        {
            let (vault_name, manager) = self.get_vault_pair().await?;
            let engine = FrontmatterSqlEngine::new(manager);
            let result = engine.query(&sql).await.map_err(to_mcp_error)?;

            let response = StandardResponse::new(vault_name, "query_frontmatter_sql", result)
                .with_next_steps(&["inspect_frontmatter", "read_note"]);

            response.to_json()
        }
        #[cfg(not(feature = "sql"))]
        {
            let _ = sql;
            Err(McpError::internal(
                "SQL feature not enabled. Rebuild TurboVault with: cargo build --features sql"
                    .to_string(),
            ))
        }
    }

    // ==================== Templates (LLM Note Creation) ====================

    /// List available templates
    #[tool(
        description = "List all available note templates in the active vault",
        usage = "Use to discover available templates before creating notes from templates",
        performance = "Instant (<5ms) - reads from in-memory template registry",
        related = ["get_template", "create_from_template", "find_notes_from_template"],
        examples = ["List all templates to find daily note template", "Check template fields before creation"]
    )]
    async fn list_templates(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);
        let templates = engine.list_templates();

        let count = templates.len();
        let response =
            StandardResponse::new(vault_name, "list_templates", serde_json::json!(templates))
                .with_count(count);

        response.to_json()
    }

    /// Get template details
    #[tool(
        description = "Get detailed information about a specific template including fields and preview",
        usage = "Use to understand template structure and required fields before creating notes",
        performance = "Instant (<5ms) - template lookup from in-memory registry",
        related = ["list_templates", "create_from_template", "find_notes_from_template"],
        examples = ["Get daily-note template to see required fields", "Preview meeting-notes template structure"]
    )]
    async fn get_template(&self, template_id: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);
        let template = engine
            .get_template(&template_id)
            .ok_or_else(|| McpError::internal(format!("Template {} not found", template_id)))?;

        let response = StandardResponse::new(
            vault_name,
            "get_template",
            serde_json::to_value(&template).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_next_step("create_from_template");

        response.to_json()
    }

    /// Create note from template
    #[tool(
        description = "Create a new note from a template with field substitution and frontmatter",
        usage = "Use for consistent note creation workflows with predefined structure and metadata",
        performance = "Fast (10-50ms) - template rendering + file write with directory creation",
        related = ["get_template", "list_templates", "write_note", "find_notes_from_template"],
        examples = ["Create daily note with date=2024-01-15", "Create meeting note with title and attendees", "Generate project note from template"]
    )]
    async fn create_from_template(
        &self,
        template_id: String,
        file_path: String,
        fields: String, // JSON string
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);

        let field_values: HashMap<String, String> = serde_json::from_str(&fields)
            .map_err(|e| McpError::invalid_request(format!("Invalid fields JSON: {}", e)))?;

        let (full_content, info) = engine
            .compute_from_template(&template_id, &file_path, field_values)
            .await
            .map_err(to_mcp_error)?;

        // turbovault-gje: route the new-file write through WriteTools so
        // the git backend records the template-rendered note as a commit
        // instead of bypassing the substrate via VaultManager.
        let write_tools = self.get_active_write_tools().await?;
        let msg = self
            .resolve_commit_message(commit_message, || {
                format!("create_from_template {} -> {}", template_id, file_path)
            })
            .await?;
        write_tools
            .create_file_with_message(&file_path, &full_content, &msg)
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        let response = StandardResponse::new(
            vault_name,
            "create_from_template",
            serde_json::to_value(&info).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_next_step("read_note")
        .with_next_step("find_notes_from_template");

        response.to_json()
    }

    /// Find notes created from template
    #[tool(
        description = "Find all notes created from a specific template via frontmatter tracking",
        usage = "Use to audit template usage, bulk update template-based notes, or analyze note patterns",
        performance = "Moderate (50-200ms) - scans vault frontmatter for template_id metadata",
        related = ["query_metadata", "get_template", "advanced_search", "create_from_template"],
        examples = ["Find all daily notes from template", "List meeting notes to bulk update", "Audit project note usage"]
    )]
    async fn find_notes_from_template(&self, template_id: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);
        let notes = engine
            .find_notes_from_template(&template_id)
            .await
            .map_err(to_mcp_error)?;

        let count = notes.len();
        let response = StandardResponse::new(
            vault_name,
            "find_notes_from_template",
            serde_json::json!(notes),
        )
        .with_count(count);

        response.to_json()
    }

    // ==================== Vault Lifecycle (Multi-Vault Management) ====================

    /// Create a new Obsidian vault
    #[tool(
        description = "Create a new Obsidian vault at specified filesystem path with optional template",
        usage = "Use for programmatic vault creation. Must call add_vault afterward to register with server",
        performance = "Fast (<50ms), creates .obsidian directory and config files",
        related = ["add_vault", "set_active_vault"],
        examples = ["template: basic", "template: zettelkasten", "template: projects"]
    )]
    async fn create_vault(
        &self,
        name: String,
        path: String,
        template: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let tools = VaultLifecycleTools::new(self.multi_vault_mgr.clone());
        let vault_info = tools
            .create_vault(&name, Path::new(&path), template.as_deref())
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            name.clone(),
            "create_vault",
            serde_json::to_value(&vault_info).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_next_step("add_vault")
        .with_next_step("set_active_vault");

        response.to_json()
    }

    /// Add an existing vault (automatically initializes it for better DX)
    #[tool(
        description = "Register an existing Obsidian vault with the MCP server and auto-initialize",
        usage = "Use as first step when working with existing vaults. Idempotent and safe to call multiple times",
        performance = "Depends on vault size: 100ms for small vaults, 1-5s for large (1000+ files) due to initialization",
        related = ["list_vaults", "set_active_vault", "get_vault_context"],
        examples = ["Add personal vault", "Register work vault", "Connect to shared knowledge base"]
    )]
    async fn add_vault(&self, name: String, path: String) -> McpResult<serde_json::Value> {
        let tools = VaultLifecycleTools::new(self.multi_vault_mgr.clone());
        let vault_info = tools
            .add_vault_from_path(&name, Path::new(&path))
            .await
            .map_err(to_mcp_error)?;

        // Auto-initialize the vault so it's ready to use immediately
        // This provides better DX - users don't need a separate initialize() call
        log::info!(
            "Automatically initializing vault '{}' for immediate use",
            name
        );

        // Get the vault manager and initialize it
        let vault_config = self
            .multi_vault_mgr
            .get_vault_config(&name)
            .await
            .map_err(|e| McpError::internal(format!("Failed to get vault config: {}", e)))?;

        let mut server_config = ServerConfig::default();
        let mut vault_cfg = vault_config;
        vault_cfg.is_default = true;
        server_config.vaults = vec![vault_cfg];

        let manager = VaultManager::new(server_config)
            .map_err(|e| McpError::internal(format!("Failed to create vault manager: {}", e)))?;

        manager
            .initialize()
            .await
            .map_err(|e| McpError::internal(format!("Failed to initialize vault: {}", e)))?;

        let manager = Arc::new(manager);

        // Cache the initialized manager
        {
            let mut cache = self.vault_managers.write().await;
            cache.insert(name.clone(), manager);
        }

        log::info!("Vault '{}' initialized and ready", name);

        let response = StandardResponse::new(
            name.clone(),
            "add_vault",
            serde_json::to_value(&vault_info).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_next_step("set_active_vault")
        .with_next_step("list_vaults");

        // CACHE PERSISTENCE: Save vault state to persistent cache
        if let Err(e) = self.persist_vault_state().await {
            log::warn!("Failed to persist vault state to cache: {}", e);
            // Not a fatal error - continue anyway
        }

        response.to_json()
    }

    /// Remove a vault from registration
    #[tool(
        description = "Unregister a vault from the MCP server (does NOT delete files)",
        usage = "Use when vault is no longer needed in current session. Not idempotent (fails if already removed)",
        performance = "Instant (<1ms), only removes from registry and clears cache",
        related = ["list_vaults", "add_vault"],
        examples = ["Remove temporary vault", "Cleanup after migration", "Close vault for maintenance"]
    )]
    async fn remove_vault(&self, name: String) -> McpResult<serde_json::Value> {
        // turbovault-1ne: refuse if the vault has an active fanout (as
        // base) OR IS itself a fanout vault — symmetric with the
        // `begin_fanout` nested-fanout refusal. Operator must
        // `abandon_fanout` first to clean state.
        {
            let fanouts = self.active_fanouts.read().await;
            if let Some(rec) = fanouts.get(&name) {
                return Err(McpError::invalid_request(format!(
                    "vault {} has an active fanout (fanout_id={}); abandon_fanout first",
                    name, rec.fanout_id
                )));
            }
            for rec in fanouts.values() {
                if rec.fanout_vault_name == name {
                    return Err(McpError::invalid_request(format!(
                        "vault {} is a fanout worktree (fanout_id={}); abandon_fanout on the base vault first",
                        name, rec.fanout_id
                    )));
                }
            }
        }

        let tools = VaultLifecycleTools::new(self.multi_vault_mgr.clone());
        tools.remove_vault(&name).await.map_err(to_mcp_error)?;

        // Clear all per-vault caches for the removed vault
        {
            let mut search_cache = self.search_engines.write().await;
            search_cache.remove(&name);
        }
        {
            let mut sim_cache = self.similarity_engines.write().await;
            sim_cache.remove(&name);
        }
        {
            let mut mgr_cache = self.vault_managers.write().await;
            mgr_cache.remove(&name);
        }

        // turbovault-1ne: tear down per-vault git-backend state. Abort
        // the background drainer + ref-listener tasks (they'd otherwise
        // poll forever against a removed vault), then drop the lock,
        // queue, and registry entries so a re-`add_vault` of the same
        // name starts clean.
        if let Some(handle) = self.git_drainers.write().await.remove(&name) {
            handle.abort();
        }
        if let Some(handle) = self.git_ref_listeners.write().await.remove(&name) {
            handle.abort();
        }
        self.git_reindex_queues.write().await.remove(&name);
        self.git_locks.write().await.remove(&name);
        // turbovault-a0l: drop the cached `VaultRepo` handle so a re-`add_vault`
        // of the same name re-opens fresh (and a stale handle to a removed vault
        // isn't held open).
        self.git_repos.write().await.remove(&name);
        // active_fanouts already empty for this name (refused above).

        let response = StandardResponse::new(
            name.clone(),
            "remove_vault",
            serde_json::json!({"status": "removed"}),
        )
        .with_next_step("list_vaults");

        // CACHE PERSISTENCE: Save updated vault state to cache
        if let Err(e) = self.persist_vault_state().await {
            log::warn!(
                "Failed to persist vault state after removal to cache: {}",
                e
            );
            // Not a fatal error - continue anyway
        }

        response.to_json()
    }

    /// List all registered vaults
    #[tool(
        description = "List all vaults registered with the MCP server. Each entry surfaces `write_backend` (\"legacy\" | \"git\") and `require_commit_message` (bool) at the top level (turbovault-17q) so you can tell which vaults are on the git substrate (GWS) and which require an explicit commit_message on mutations — without calling get_vault_config per vault.",
        usage = "Use to discover available vaults before setting active vault, and to learn each vault's write backend + commit-message requirement up front. Empty list means call add_vault first",
        performance = "Instant (<1ms), reads from in-memory registry",
        related = ["get_active_vault", "add_vault", "set_active_vault"],
        examples = ["Show all vaults", "Check available options", "Verify vault registration"]
    )]
    async fn list_vaults(&self) -> McpResult<serde_json::Value> {
        let tools = VaultLifecycleTools::new(self.multi_vault_mgr.clone());
        let vaults = tools.list_vaults().await.map_err(to_mcp_error)?;

        let count = vaults.len();
        let response = StandardResponse::new(
            String::new(), // No active vault for this operation
            "list_vaults",
            serde_json::to_value(&vaults).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count);

        response.to_json()
    }

    /// Get configuration for a specific vault
    #[tool(
        description = "Get detailed configuration for a specific vault",
        usage = "Use to inspect vault settings before operations or validate vault configuration",
        performance = "Instant (<1ms), reads from in-memory config",
        related = ["set_active_vault", "list_vaults"],
        examples = ["Check vault path", "Verify search settings", "Inspect custom config"]
    )]
    async fn get_vault_config(&self, name: String) -> McpResult<serde_json::Value> {
        let tools = VaultLifecycleTools::new(self.multi_vault_mgr.clone());
        let config = tools.get_vault_config(&name).await.map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            name.clone(),
            "get_vault_config",
            serde_json::to_value(&config).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_next_step("set_active_vault");

        response.to_json()
    }

    /// Set the active vault
    #[tool(
        description = "Switch the active vault for subsequent operations",
        usage = "Use when working with multiple vaults. All tools operate on the active vault. Idempotent",
        performance = "Instant (<1ms), updates in-memory state only",
        related = ["get_active_vault", "list_vaults", "get_vault_context"],
        examples = ["Switch to personal vault", "Activate work vault", "Change vault context"]
    )]
    async fn set_active_vault(&self, name: String) -> McpResult<serde_json::Value> {
        let tools = VaultLifecycleTools::new(self.multi_vault_mgr.clone());
        tools.set_active_vault(&name).await.map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            name.clone(),
            "set_active_vault",
            serde_json::json!({"status": "activated"}),
        )
        .with_next_step("get_vault_context")
        .with_next_step("quick_health_check");

        // CACHE PERSISTENCE: Save active vault state to cache
        if let Err(e) = self.persist_vault_state().await {
            log::warn!("Failed to persist active vault state to cache: {}", e);
            // Not a fatal error - continue anyway
        }

        response.to_json()
    }

    /// Get the currently active vault
    #[tool(
        description = "Get the name of the currently active vault",
        usage = "Use to verify vault context before operations. Returns empty string if none active",
        performance = "Instant (<1ms), reads from in-memory state",
        related = ["set_active_vault", "list_vaults", "get_vault_context"],
        examples = ["Check current vault", "Verify context", "Confirm active vault"]
    )]
    async fn get_active_vault(&self) -> McpResult<serde_json::Value> {
        let tools = VaultLifecycleTools::new(self.multi_vault_mgr.clone());
        let active = tools.get_active_vault().await.map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            active.clone(),
            "get_active_vault",
            serde_json::json!({"active_vault": active}),
        )
        .with_next_step("get_vault_context");

        response.to_json()
    }

    // ==================== Batch Operations ====================

    /// Execute batch file operations atomically
    #[tool(
        description = "Execute multiple file operations as ONE atomic substrate commit (turbovault-61k / TV-012: THIS is the all-or-nothing primitive. `begin_fanout` is worktree isolation for fanout, NOT atomic-batch — don't confuse them.) On git backend: every op carries optional per-op `expected_hash` (CAS), CreateNote implicitly carries `expect_absent`; a mismatch on ANY op aborts the whole batch and ZERO files change. Pass `commit_message` for a meaningful git commit subject (turbovault-0bh); defaults to an op-tally summary like `batch: 5 creates, 2 updates, 1 delete`.",
        usage = "Use for multi-file workflows requiring all-or-nothing. Substrate atomicity holds: zero ops commit if any op fails. Not idempotent. Pass commit_message to explain the batch's WHY. For parallel-subagent isolation (without atomicity), use `begin_fanout` instead.",
        performance = "Depends on operation count and types. Transactions add ~10-50ms overhead.",
        related = ["write_note", "delete_note", "move_note"],
        examples = [
            r#"[{"type":"write","path":"note1.md","content":"..."}]"#,
            r#"[{"type":"delete","path":"old.md"},{"type":"write","path":"new.md","content":"..."}]"#,
            r#"[{"type":"move","from":"a.md","to":"b.md"},{"type":"write","path":"index.md","content":"..."}]"#,
            r#"commit_message: 'ingest source X: 3 concept pages + 1 entity update'"#
        ]
    )]
    async fn batch_execute(
        &self,
        operations: Vec<BatchOperation>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let vault_name = self.get_active_vault_name().await?;

        if operations.is_empty() {
            return Err(McpError::internal(
                "Batch operations list cannot be empty".to_string(),
            ));
        }

        let op_count = operations.len();
        // turbovault-rxx: backend-aware `atomic` meta. Git
        // substrate is genuinely atomic (`batch_execute_failure_leaves_
        // no_partial_state` covers it). Legacy `BatchExecutor` is NOT
        // atomic and never was (the historic #213 "lie"); on legacy,
        // honesty = derive from result.success — true only when no op
        // failed; false otherwise (partial state may have landed).
        let backend = self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map(|c| c.write_backend)
            .unwrap_or(WriteBackend::Legacy);
        let tools = self.get_active_write_tools().await?;

        // turbovault-0g4.6/.7: a backlink-aware MoveNote or DeleteNote resolves
        // inbound links from the in-memory graph; on the git backend, drain any
        // pending reindex first so the graph is coherent (mirrors what move_note
        // /delete_note do for the single-op path — turbovault-78w). A stale graph
        // would silently miss inbound links. Cheap no-op when the queue is empty
        // / the batch has no backlink-aware op.
        let needs_backlink_graph = matches!(backend, WriteBackend::Git)
            && operations.iter().any(|op| match op {
                BatchOperation::MoveNote {
                    update_backlinks, ..
                } => update_backlinks.unwrap_or(true),
                BatchOperation::DeleteNote { on_backlinks, .. } => {
                    on_backlinks.as_deref() != Some("force")
                }
                _ => false,
            });
        if needs_backlink_graph {
            self.flush_reindex_for_active_vault().await?;
        }

        // turbovault-0bh: caller-supplied or op-tally derivation.
        let msg = self
            .resolve_commit_message(commit_message, || derive_batch_message(&operations))
            .await?;
        let result = tools
            .batch_execute_with_message(operations, &msg)
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        let atomic = match backend {
            WriteBackend::Git => true,
            WriteBackend::Legacy => result.success,
        };
        let response = StandardResponse::new(
            vault_name,
            "batch_execute",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(op_count)
        .with_meta("atomic", serde_json::json!(atomic))
        .with_meta(
            "backend",
            serde_json::json!(match backend {
                WriteBackend::Git => "git",
                WriteBackend::Legacy => "legacy",
            }),
        )
        .with_next_step("quick_health_check");

        response.to_json()
    }

    // ==================== Fanout Transactions (GWS.13) ====================

    /// Begin a fanout on the active vault.
    #[tool(
        description = "ISOLATION ONLY — NOT atomic-rollback (turbovault-61k / TV-012). Opens a scratch git worktree on a `wip/<fanout_id>` branch for the active git-backend vault and auto-registers a temporary vault pointing at it. N subagents `set_active_vault` to the fanout vault and write freely; their commits go to the wip branch without disturbing the base vault's working tree. A failed write INSIDE the fanout does NOT abort the rest — for all-or-nothing semantics use `batch_execute`. `commit_fanout` merges the wip branch back; `abandon_fanout` discards it.",
        usage = "Use to fan out subagent writes (e.g. parallel ingest of N sources) into a single visible reveal at merge-back. NOT a substitute for batch_execute's atomicity — fanout is git-worktree isolation, batch_execute is multi-file CAS. Default merge strategy comes from the vault's `git.merge_strategy` config (override at commit time).",
        performance = "Cheap (~5-10ms): one branch create + one git worktree add. Worktree shares the object DB with main; the working-tree files materialize into the scratch dir.",
        related = ["commit_fanout", "abandon_fanout", "batch_execute", "set_active_vault"],
        examples = ["begin_fanout() — open fanout for subagent ingest", "begin_fanout(merge_strategy: \"merge-commit\")", "(for atomic multi-op: prefer batch_execute, NOT begin_fanout)"]
    )]
    async fn begin_fanout(&self, merge_strategy: Option<String>) -> McpResult<serde_json::Value> {
        let base_vault = self.get_active_vault_name().await?;
        let base_cfg = self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map_err(|e| McpError::internal(format!("No active vault config: {}", e)))?;
        if base_cfg.write_backend != WriteBackend::Git {
            return Err(McpError::invalid_request(format!(
                "vault {} is on the legacy backend; fanouts require write_backend=git",
                base_vault
            )));
        }

        // Validate merge strategy upfront (so a bad value is caught at begin,
        // not at commit). Stored on the record for default-at-commit use.
        let _ = Self::parse_merge_strategy(merge_strategy.as_deref())?;

        // Refuse nested fanouts: one active per base vault.
        {
            let fanouts = self.active_fanouts.read().await;
            if fanouts.contains_key(&base_vault) {
                return Err(McpError::invalid_request(format!(
                    "vault {} already has an active fanout (fanout_id={}); commit_fanout or abandon_fanout first",
                    base_vault, fanouts[&base_vault].fanout_id
                )));
            }
            // Refuse if the active vault is itself a fanout vault (nested).
            for rec in fanouts.values() {
                if rec.fanout_vault_name == base_vault {
                    return Err(McpError::invalid_request(format!(
                        "active vault {} is already a fanout vault; nested fanouts are not supported",
                        base_vault
                    )));
                }
            }
        }

        let fanout_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let scratch_path = Self::fanout_scratch_path(&fanout_id);

        // Open VaultRepo on the base vault path (with its shared lock
        // registry) and call the stateless open_fanout_worktree.
        let locks = self.get_or_init_git_locks(&base_vault).await;
        let base_path = base_cfg.path.clone();
        let info_path = scratch_path.clone();
        let fanout_id_for_blocking = fanout_id.clone();
        let info = tokio::task::spawn_blocking(move || -> McpResult<FanoutInfo> {
            let repo = VaultRepo::open_with_locks(&base_path, locks)
                .map_err(|e| McpError::internal(format!("open base vault: {}", e)))?;
            repo.open_fanout_worktree(&fanout_id_for_blocking, &info_path)
                .map_err(|e| McpError::internal(format!("open_fanout_worktree: {}", e)))
        })
        .await
        .map_err(|e| McpError::internal(format!("begin_fanout task: {}", e)))??;

        // Auto-register the fanout vault.
        let fanout_vault_name = format!("{}-fanout-{}", base_vault, fanout_id);
        let fanout_cfg = VaultConfig::builder(&fanout_vault_name, scratch_path.clone())
            .write_backend(WriteBackend::Git)
            .build()
            .map_err(|e| McpError::internal(format!("fanout vault config: {}", e)))?;
        self.multi_vault_mgr
            .add_vault(fanout_cfg)
            .await
            .map_err(|e| McpError::internal(format!("add_vault for fanout: {}", e)))?;

        // Record the active fanout.
        self.active_fanouts.write().await.insert(
            base_vault.clone(),
            ActiveFanoutRecord {
                fanout_id: fanout_id.clone(),
                info,
                fanout_vault_name: fanout_vault_name.clone(),
            },
        );

        StandardResponse::new(
            base_vault.clone(),
            "begin_fanout",
            serde_json::json!({
                "fanout_id": fanout_id,
                "base_vault": base_vault,
                "fanout_vault": fanout_vault_name,
                "worktree_path": scratch_path.to_string_lossy(),
                "wip_branch": format!("wip/{}", fanout_id),
            }),
        )
        .with_next_steps(&["set_active_vault", "commit_fanout", "abandon_fanout"])
        .to_json()
    }

    /// Commit (merge back) the active fanout.
    #[tool(
        description = "Merge the active fanout's wip branch back into the base vault's main branch (turbovault-61k / TV-012: this is worktree merge-back, NOT atomic-commit-of-a-transactional batch; if you wanted batch atomicity, use `batch_execute` instead). One merge-commit by default; configurable. Cleans up the scratch worktree + wip branch + deregisters the fanout vault. Caller may call this with the base vault OR the fanout vault active — both resolve to the same transaction.",
        usage = "Pair with `begin_fanout`. `merge_strategy` overrides the vault config default at commit time; pass 'fast-forward' if you want main to advance directly to the wip tip (fails if main moved since begin). Pass `commit_message` to set the merge-commit subject (turbovault-b1q); defaults to an auto-derived 'merge fan-out ...' message. (Ignored for fast-forward, which creates no merge commit.)",
        performance = "Dominated by the merge (one tree merge + one materialize). Typical ~10-30ms.",
        related = ["begin_fanout", "abandon_fanout", "batch_execute"],
        examples = ["commit_fanout()", "commit_fanout(merge_strategy: \"fast-forward\")", "commit_fanout(commit_message: 'ingest source X: merge 12 concept pages')"]
    )]
    async fn commit_fanout(
        &self,
        merge_strategy: Option<String>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (base_vault, record) = self.resolve_active_fanout().await.ok_or_else(|| {
            McpError::invalid_request("no active fanout (call begin_fanout first)".to_string())
        })?;
        let base_cfg = self
            .multi_vault_mgr
            .list_vaults()
            .await
            .map_err(|e| McpError::internal(format!("list_vaults: {}", e)))?
            .into_iter()
            .find(|v| v.config.name == base_vault)
            .ok_or_else(|| McpError::internal(format!("base vault {} disappeared", base_vault)))?
            .config
            .clone();

        let strategy =
            Self::parse_merge_strategy(merge_strategy.as_deref())?.unwrap_or_else(|| {
                base_cfg
                    .git
                    .as_ref()
                    .map(|g| Self::config_to_git_merge_strategy(g.merge_strategy))
                    .unwrap_or(GitMergeStrategy::MergeCommit)
            });

        let locks = self.get_or_init_git_locks(&base_vault).await;
        let base_path = base_cfg.path.clone();
        let info = record.info.clone();
        let merge_result = tokio::task::spawn_blocking(move || -> McpResult<_> {
            let repo = VaultRepo::open_with_locks(&base_path, locks)
                .map_err(|e| McpError::internal(format!("open base vault: {}", e)))?;
            repo.merge_fanout_back(&info, strategy, commit_message.as_deref())
                .map_err(|e| McpError::internal(format!("merge_fanout_back: {}", e)))
        })
        .await
        .map_err(|e| McpError::internal(format!("commit_fanout task: {}", e)))??;

        // Deregister fanout vault + clear record. On error in either step we
        // keep going so we don't leave the server's in-memory state in a
        // wedged state.
        if let Err(e) = self
            .multi_vault_mgr
            .remove_vault(&record.fanout_vault_name)
            .await
        {
            log::warn!(
                "commit_fanout: failed to deregister fanout vault {}: {}",
                record.fanout_vault_name,
                e
            );
        }
        self.active_fanouts.write().await.remove(&base_vault);

        StandardResponse::new(
            base_vault,
            "commit_fanout",
            serde_json::json!({
                "fanout_id": record.fanout_id,
                "merge_commit": merge_result.merge_commit.map(|o| o.to_string()),
                "tip_before": merge_result.tip_before.to_string(),
                "tip_after": merge_result.tip_after.to_string(),
            }),
        )
        .with_next_step("quick_health_check")
        .to_json()
    }

    /// Abandon (discard) the active fanout.
    #[tool(
        description = "Discard the active fanout (turbovault-61k / TV-012: this is a worktree-discard, NOT a transactional-rollback of an atomic batch — `batch_execute` is the atomic primitive). Nothing lands on the base vault; the scratch worktree + wip branch are removed; the auto-registered fanout vault is deregistered. Safe no-op if there's no active fanout (returns ok with `was_active: false`).",
        usage = "Use to bail out of a fanout that no longer makes sense (subagent error, user cancel, conflicting plan). Symmetric counterpart to `commit_fanout`.",
        performance = "Fast: a few filesystem + git ref cleanups.",
        related = ["begin_fanout", "commit_fanout", "batch_execute"],
        examples = ["abandon_fanout()"]
    )]
    async fn abandon_fanout(&self) -> McpResult<serde_json::Value> {
        let Some((base_vault, record)) = self.resolve_active_fanout().await else {
            return StandardResponse::new(
                self.get_active_vault_name().await.unwrap_or_default(),
                "abandon_fanout",
                serde_json::json!({ "was_active": false }),
            )
            .to_json();
        };
        let base_cfg = self
            .multi_vault_mgr
            .list_vaults()
            .await
            .map_err(|e| McpError::internal(format!("list_vaults: {}", e)))?
            .into_iter()
            .find(|v| v.config.name == base_vault)
            .ok_or_else(|| McpError::internal(format!("base vault {} disappeared", base_vault)))?
            .config
            .clone();
        let locks = self.get_or_init_git_locks(&base_vault).await;
        let base_path = base_cfg.path.clone();
        let info = record.info.clone();

        tokio::task::spawn_blocking(move || -> McpResult<()> {
            let repo = VaultRepo::open_with_locks(&base_path, locks)
                .map_err(|e| McpError::internal(format!("open base vault: {}", e)))?;
            repo.abandon_fanout_by_info(&info)
                .map_err(|e| McpError::internal(format!("abandon_fanout_by_info: {}", e)))
        })
        .await
        .map_err(|e| McpError::internal(format!("abandon_fanout task: {}", e)))??;

        if let Err(e) = self
            .multi_vault_mgr
            .remove_vault(&record.fanout_vault_name)
            .await
        {
            log::warn!(
                "abandon_fanout: failed to deregister fanout vault {}: {}",
                record.fanout_vault_name,
                e
            );
        }
        self.active_fanouts.write().await.remove(&base_vault);

        StandardResponse::new(
            base_vault,
            "abandon_fanout",
            serde_json::json!({
                "was_active": true,
                "fanout_id": record.fanout_id,
            }),
        )
        .to_json()
    }

    /// turbovault-84k: enumerate fanout artifacts from prior sessions.
    #[tool(
        description = "List orphan fanout `wip-*` worktrees this server didn't open. Detects worktrees left behind by a crashed predecessor or by `commit/abandon_fanout` cleanup failures. Pure read; never mutates. Operator-driven cleanup: `git worktree remove <name>` + `git branch -D wip/<id>` from the vault root.",
        usage = "Diagnostic. Use after a server restart, or when `begin_fanout` errors with 'vault already has an active fanout' but no `commit/abandon_fanout` works.",
        performance = "Fast (~1-2ms per git-backend vault).",
        related = ["begin_fanout", "abandon_fanout"],
        examples = ["list_orphan_fanouts()", "list_orphan_fanouts(vault: \"my-vault\")"]
    )]
    async fn list_orphan_fanouts(&self, vault: Option<String>) -> McpResult<serde_json::Value> {
        let orphans = self
            .scan_orphan_fanouts(vault.as_deref())
            .await
            .map_err(|e| McpError::internal(format!("scan_orphan_fanouts: {}", e)))?;
        let active = self.get_active_vault_name().await.unwrap_or_default();
        StandardResponse::new(
            active,
            "list_orphan_fanouts",
            serde_json::json!({
                "count": orphans.len(),
                "orphans": orphans,
            }),
        )
        .to_json()
    }

    // ==================== Export Operations ====================

    /// Export health report as JSON or CSV
    #[tool(
        description = "Export vault health analysis as structured data",
        usage = "Use for external analysis, reporting, or archiving health metrics over time",
        performance = "Fast, <100ms typical",
        related = ["full_health_analysis", "export_analysis_report", "quick_health_check"],
        examples = ["format: json", "format: csv"]
    )]
    async fn export_health_report(&self, format: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let report = tools
            .export_health_report(&format)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "export_health_report",
            serde_json::json!({"content": report, "format": format}),
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }

    /// Export broken links as JSON or CSV
    #[tool(
        description = "Export broken links report as structured data",
        usage = "Use for bulk link fixing workflows or external tooling integration",
        performance = "Fast, <100ms typical",
        related = ["get_broken_links", "export_health_report", "full_health_analysis"],
        examples = ["format: json", "format: csv"]
    )]
    async fn export_broken_links(&self, format: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let links = tools
            .export_broken_links(&format)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "export_broken_links",
            serde_json::json!({"content": links, "format": format}),
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }

    /// Export vault statistics as JSON or CSV
    #[tool(
        description = "Export comprehensive vault statistics as structured data",
        usage = "Use for analytics dashboards, vault growth tracking, or external reporting",
        performance = "Fast, <100ms typical",
        related = ["quick_health_check", "export_analysis_report", "explain_vault"],
        examples = ["format: json", "format: csv"]
    )]
    async fn export_vault_stats(&self, format: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let stats = tools
            .export_vault_stats(&format)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "export_vault_stats",
            serde_json::json!({"content": stats, "format": format}),
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }

    /// Export full analysis report
    #[tool(
        description = "Export comprehensive vault analysis combining health, stats, links, and clusters",
        usage = "Use for full vault audits or migration preparation when complete data export is needed",
        performance = "Slow on large vaults (1-5s for 10k+ notes), combines multiple analyses",
        related = ["full_health_analysis", "export_vault_stats", "export_health_report"],
        examples = ["format: json", "format: csv"]
    )]
    async fn export_analysis_report(&self, format: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let report = tools
            .export_analysis_report(&format)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "export_analysis_report",
            serde_json::json!({"content": report, "format": format}),
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }

    // ==================== Metadata Operations ====================

    /// Query files by metadata pattern
    #[tool(
        description = "Query notes by frontmatter metadata pattern (equality, comparison, existence checks)",
        usage = "Use for tag-based organization, status tracking, or property-based filtering. Searches frontmatter YAML fields.",
        performance = "Fast on indexed fields (<100ms typical). Full vault scan for complex queries.",
        related = ["get_metadata_value", "advanced_search"],
        examples = [
            r#"status: "draft""#,
            "priority > 3",
            "tags contains 'project'",
            "author.name = 'Alice'",
            "created_at > '2024-01-01'"
        ]
    )]
    async fn query_metadata(&self, pattern: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);
        let results = tools.query_metadata(&pattern).await.map_err(to_mcp_error)?;

        let result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let count = extract_count(&result_data);

        let response = StandardResponse::new(vault_name, "query_metadata", result_data)
            .with_count(count)
            .with_meta("pattern", serde_json::json!(pattern));

        response.to_json()
    }

    /// Get metadata value from a file
    #[tool(
        description = "Extract specific metadata value from a note's frontmatter (supports dot notation for nested keys)",
        usage = "Use to read properties without parsing full note content. Faster than read_note when you only need metadata.",
        performance = "Very fast (<10ms typical), only parses frontmatter section.",
        related = ["query_metadata", "read_note"],
        examples = [
            "key: author",
            "key: tags",
            "key: author.name",
            "key: metadata.priority",
            "key: custom.nested.field"
        ]
    )]
    async fn get_metadata_value(&self, file: String, key: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);
        let value = tools
            .get_metadata_value(&file, &key)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "get_metadata_value",
            serde_json::json!({"file": file, "key": key, "value": value}),
        )
        .with_next_step("query_metadata");

        response.to_json()
    }

    /// Update frontmatter of a note without touching content
    #[tool(
        description = "Update YAML frontmatter of a note without modifying content body",
        usage = "Use to modify note metadata (status, tags, properties) while preserving content. Merge mode (default) deep-merges new keys into existing frontmatter. Replace mode replaces frontmatter entirely",
        performance = "Fast (<30ms typical). Reads file, modifies frontmatter, writes atomically",
        related = ["get_metadata_value", "query_metadata", "manage_tags"],
        examples = [
            r#"frontmatter: {"status": "published", "priority": 1}, merge: true"#,
            r#"frontmatter: {"tags": ["work", "urgent"]}, merge: false"#
        ]
    )]
    async fn update_frontmatter(
        &self,
        path: String,
        // turbovault-gje / TV-007: `HashMap<String, serde_json::Value>`
        // derives a JsonSchema with `type: object`, so MCP clients send a
        // structured object — NOT a string-coerced AnyValue. Replaces the
        // old `serde_json::Value` param that schemars emitted as Any,
        // which forced clients to send a stringified body the server
        // then rejected with "frontmatter must be a JSON object".
        frontmatter: HashMap<String, serde_json::Value>,
        merge: Option<bool>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);

        let fm_map: serde_json::Map<String, serde_json::Value> = frontmatter.into_iter().collect();
        let (new_content, info) = tools
            .compute_update_frontmatter(&path, fm_map, merge.unwrap_or(true))
            .await
            .map_err(to_mcp_error)?;

        // turbovault-gje: route the write through WriteTools so the git
        // backend records this mutation as a commit instead of bypassing
        // the substrate via VaultManager::write_file. Legacy backend
        // behavior is preserved (WriteTools::Legacy still calls
        // VaultManager directly).
        let write_tools = self.get_active_write_tools().await?;
        let msg = self
            .resolve_commit_message(commit_message, || format!("update_frontmatter {}", path))
            .await?;
        write_tools
            .write_file_with_mode_and_message(&path, &new_content, WriteMode::Overwrite, None, &msg)
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        StandardResponse::new(vault_name, "update_frontmatter", info)
            .with_next_steps(&["read_note", "query_metadata"])
            .to_json()
    }

    /// Manage tags on a note (add, remove, list)
    #[tool(
        description = "Add, remove, or list tags on a note. List returns both frontmatter and inline #tags. Add/remove only modify frontmatter tags array",
        usage = "Use for tag-based organization. 'list' discovers all tags (frontmatter + inline). 'add' creates tags array if missing. 'remove' leaves other tags intact. Tags are normalized (# prefix stripped)",
        performance = "Fast (<30ms typical). List requires parsing content for inline tags",
        related = ["update_frontmatter", "query_metadata", "advanced_search"],
        examples = [
            "operation: list (returns all tags)",
            r#"operation: add, tags: ["work", "urgent"]"#,
            r#"operation: remove, tags: ["draft"]"#
        ]
    )]
    async fn manage_tags(
        &self,
        path: String,
        operation: String,
        tags: Option<Vec<String>>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);

        let (maybe_write, info) = tools
            .compute_manage_tags(&path, &operation, tags.as_deref())
            .await
            .map_err(to_mcp_error)?;

        // turbovault-gje: route mutations through WriteTools so the git
        // backend records add/remove as commits. `list` is read-only —
        // `maybe_write` is `None` — and skips the write path entirely.
        if let Some(new_content) = maybe_write {
            let write_tools = self.get_active_write_tools().await?;
            let msg = self
                .resolve_commit_message(commit_message, || {
                    format!("manage_tags {} {}", operation, path)
                })
                .await?;
            write_tools
                .write_file_with_mode_and_message(
                    &path,
                    &new_content,
                    WriteMode::Overwrite,
                    None,
                    &msg,
                )
                .await
                .map_err(to_mcp_error)?;
            self.invalidate_similarity_cache().await;
            self.invalidate_search_cache().await;
        }

        StandardResponse::new(vault_name, "manage_tags", info)
            .with_next_steps(&["update_frontmatter", "query_metadata"])
            .to_json()
    }

    /// Get lightweight metadata for multiple files without reading content
    #[tool(
        description = "Get file metadata (size, modified time, has_frontmatter) for multiple notes without reading full content",
        usage = "Use to quickly assess file properties before deciding which notes to read. Much faster than read_note for metadata-only queries. Supports batch queries (up to 50 paths)",
        performance = "Very fast (<10ms typical). Only reads filesystem metadata and first 4 bytes per file",
        related = ["read_note", "query_metadata"],
        examples = [
            r#"paths: ["daily/2024-01-15.md", "projects/alpha.md"]"#,
            r#"paths: ["index.md"]"#
        ]
    )]
    async fn get_notes_info(&self, paths: Vec<String>) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = FileTools::new(manager);
        let results = tools.get_notes_info(&paths).await.map_err(to_mcp_error)?;

        let count = results.len();
        let result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;

        StandardResponse::new(vault_name, "get_notes_info", result_data)
            .with_count(count)
            .with_next_step("read_note")
            .to_json()
    }

    /// Move any file within vault (binary-safe, confirmation-protected)
    #[tool(
        description = "Move or rename any file (images, PDFs, attachments) within vault with double confirmation. Binary-safe, no content processing",
        usage = "Use for non-markdown files (images, PDFs, attachments). For markdown notes, use move_note instead (which updates link graph). Requires confirm_from and confirm_to matching from/to exactly",
        performance = "Fast (<20ms typical). Atomic rename, falls back to copy+delete for cross-filesystem moves",
        related = ["move_note", "delete_note"],
        examples = [
            "from: attachments/old.png, to: images/new.png, confirm_from: attachments/old.png, confirm_to: images/new.png"
        ]
    )]
    async fn move_file(
        &self,
        from: String,
        to: String,
        confirm_from: String,
        confirm_to: String,
        expected_hash: Option<String>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        // Safety: confirmations must match
        if from != confirm_from {
            return Err(McpError::invalid_request(format!(
                "Confirmation failed: confirm_from '{}' does not match from '{}'. Both must be identical.",
                confirm_from, from
            )));
        }
        if to != confirm_to {
            return Err(McpError::invalid_request(format!(
                "Confirmation failed: confirm_to '{}' does not match to '{}'. Both must be identical.",
                confirm_to, to
            )));
        }

        let vault_name = self.get_active_vault_name().await?;
        let tools = self.get_active_write_tools().await?;
        // turbovault-0bh: caller-supplied or auto-derived.
        let msg = self
            .resolve_commit_message(commit_message, || format!("move_file {} -> {}", from, to))
            .await?;
        tools
            .move_file_with_hash_and_message(&from, &to, expected_hash.as_deref(), &msg)
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        StandardResponse::new(
            vault_name,
            "move_file",
            serde_json::json!({"from": from, "to": to, "status": "moved"}),
        )
        .to_json()
    }

    // ==================== Relationship Operations ====================

    /// Suggest files to link
    #[tool(
        description = "AI-powered link suggestions for a note (returns top N candidates with reasoning)",
        usage = "Use to improve vault connectivity and discover non-obvious relationships. Analyzes content similarity, link patterns, and graph structure. ML-based, slower than simple queries.",
        performance = "200ms-2s depending on vault size. Uses TF-IDF + graph features. Consider limit parameter for faster results.",
        related = ["recommend_related", "get_dead_end_notes", "get_related_notes"],
        examples = [
            "file: daily/2024-01-15.md, limit: 5",
            "file: projects/research.md, limit: 10",
            "file: index.md (default limit: 5)"
        ]
    )]
    async fn suggest_links(
        &self,
        file: String,
        limit: Option<i32>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = RelationshipTools::new(manager);
        let limit = limit.unwrap_or(5) as usize;
        let suggestions = tools
            .suggest_links(&file, limit)
            .await
            .map_err(to_mcp_error)?;

        let result_data =
            serde_json::to_value(&suggestions).map_err(|e| McpError::internal(e.to_string()))?;
        let count = extract_count(&result_data);

        let response = StandardResponse::new(vault_name, "suggest_links", result_data)
            .with_count(count)
            .with_meta("limit", serde_json::json!(limit));

        response.to_json()
    }

    /// Get link strength between two files
    #[tool(
        description = "Calculate connection strength between two notes (0.0-1.0 score based on multiple factors)",
        usage = "Use to validate relationship importance or prioritize link maintenance. Considers direct links, shared links, content similarity, and co-citation.",
        performance = "Fast (<50ms typical), cached graph traversal.",
        related = ["suggest_links", "get_related_notes", "recommend_related"],
        examples = [
            "source: index.md, target: concepts/foo.md",
            "source: daily/2024-01-15.md, target: projects/research.md",
            "source: MOC.md, target: archive/old-note.md"
        ]
    )]
    async fn get_link_strength(
        &self,
        source: String,
        target: String,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = RelationshipTools::new(manager);
        let strength = tools
            .get_link_strength(&source, &target)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "get_link_strength",
            serde_json::json!({"source": source, "target": target, "strength": strength}),
        )
        .with_meta("metric", serde_json::json!("link_strength"));

        response.to_json()
    }

    /// Get centrality ranking
    #[tool(
        description = "Rank all notes by graph centrality metrics (betweenness, closeness, eigenvector)",
        usage = "Use for identifying key notes beyond simple link counts. Betweenness identifies bridge notes, closeness finds accessible notes, eigenvector reveals influence. More sophisticated than get_hub_notes.",
        performance = "Computationally expensive on large vaults. O(V³) for betweenness. May take several seconds for >1000 notes.",
        related = ["get_hub_notes", "explain_vault", "get_link_strength"],
        examples = [
            "Returns all notes ranked by betweenness (bridge importance)",
            "Returns all notes ranked by closeness (accessibility)",
            "Returns all notes ranked by eigenvector (influence)"
        ]
    )]
    async fn get_centrality_ranking(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = RelationshipTools::new(manager);
        let ranking = tools.get_centrality_ranking().await.map_err(to_mcp_error)?;

        let result_data =
            serde_json::to_value(&ranking).map_err(|e| McpError::internal(e.to_string()))?;
        let count = extract_count(&result_data);

        let response = StandardResponse::new(vault_name, "get_centrality_ranking", result_data)
            .with_count(count)
            .with_meta(
                "metrics",
                serde_json::json!(["betweenness", "closeness", "eigenvector"]),
            );

        response.to_json()
    }

    // ==================== Resources (OFM Knowledge Injection) ====================

    /// Complete Obsidian Flavored Markdown syntax guide
    #[resource("obsidian://syntax/complete-guide")]
    async fn ofm_complete_guide_resource(
        &self,
        _uri: String,
        _ctx: &RequestContext,
    ) -> McpResult<String> {
        Ok(crate::resources::OFM_SYNTAX_GUIDE.to_string())
    }

    /// Quick reference for Obsidian Flavored Markdown
    #[resource("obsidian://syntax/quick-ref")]
    async fn ofm_quick_reference_resource(
        &self,
        _uri: String,
        _ctx: &RequestContext,
    ) -> McpResult<String> {
        Ok(crate::resources::OFM_QUICK_REFERENCE.to_string())
    }

    /// Example note demonstrating all OFM features
    #[resource("obsidian://examples/sample-note")]
    async fn ofm_example_note_resource(
        &self,
        _uri: String,
        _ctx: &RequestContext,
    ) -> McpResult<String> {
        Ok(crate::resources::OFM_EXAMPLE_NOTE.to_string())
    }

    // ==================== OFM Documentation Tools (Resource Fallback) ====================

    /// Get Obsidian Flavored Markdown syntax guide (tool fallback for clients without resource support)
    #[tool(
        description = "Get Obsidian Flavored Markdown syntax guide for the OFM syntax TurboVault parses, classifies, or preserves",
        usage = "Use before writing notes to ensure correct syntax, or as reference for OFM extensions. Prefer resource obsidian://syntax/complete-guide if client supports resources",
        performance = "Instant, returns static documentation",
        related = ["get_ofm_quick_ref", "get_ofm_examples"],
        examples = []
    )]
    async fn get_ofm_syntax_guide(&self) -> McpResult<serde_json::Value> {
        let guide = crate::resources::OFM_SYNTAX_GUIDE.to_string();

        Ok(serde_json::json!({
            "title": "Obsidian Flavored Markdown - Syntax Guide",
            "content": guide,
            "format": "markdown",
            "sections": [
                "Overview",
                "Core Philosophy",
                "Supported Standards",
                "Markdown Extensions",
                "Usage Guidelines",
                "Best Practices"
            ],
            "alternatives": {
                "resource": "obsidian://syntax/complete-guide",
                "quick_ref_tool": "get_ofm_quick_ref",
                "examples_tool": "get_ofm_examples"
            }
        }))
    }

    /// Get quick reference for Obsidian Flavored Markdown (tool fallback)
    #[tool(
        description = "Get condensed OFM cheat sheet with common patterns and best practices",
        usage = "Use for quick syntax reminders during note writing. More concise than full guide. Prefer resource obsidian://syntax/quick-ref if client supports resources",
        performance = "Instant, returns static documentation",
        related = ["get_ofm_syntax_guide", "get_ofm_examples"],
        examples = []
    )]
    async fn get_ofm_quick_ref(&self) -> McpResult<serde_json::Value> {
        let quick_ref = crate::resources::OFM_QUICK_REFERENCE.to_string();

        Ok(serde_json::json!({
            "title": "Obsidian Flavored Markdown - Quick Reference",
            "content": quick_ref,
            "format": "markdown",
            "sections": [
                "Core Syntax",
                "Obsidian Extensions",
                "Key Differences",
                "Best Practices",
                "Common Patterns"
            ],
            "alternatives": {
                "resource": "obsidian://syntax/quick-ref",
                "complete_guide_tool": "get_ofm_syntax_guide",
                "examples_tool": "get_ofm_examples"
            }
        }))
    }

    /// Get example note demonstrating all OFM features (tool fallback)
    #[tool(
        description = "Get comprehensive example note demonstrating ALL OFM features with real-world patterns",
        usage = "Use as reference when creating complex notes or learning OFM syntax by example. Shows daily notes, Zettelkasten, and MOC patterns. Prefer resource obsidian://examples/sample-note if client supports resources",
        performance = "Instant, returns static example note",
        related = ["get_ofm_syntax_guide", "get_ofm_quick_ref"],
        examples = []
    )]
    async fn get_ofm_examples(&self) -> McpResult<serde_json::Value> {
        let examples = crate::resources::OFM_EXAMPLE_NOTE.to_string();

        Ok(serde_json::json!({
            "title": "Obsidian Flavored Markdown - Complete Example Note",
            "content": examples,
            "format": "markdown",
            "features_demonstrated": [
                "Basic Formatting",
                "Wikilinks & Aliases",
                "Embeds (notes, images, PDFs)",
                "Block References",
                "Callouts (all types)",
                "Task Lists",
                "Tables",
                "Code Blocks",
                "Preserved Obsidian syntax",
                "Real-World Patterns"
            ],
            "patterns_shown": [
                "Daily Note Template",
                "Index/MOC Pattern",
                "Zettelkasten Pattern"
            ],
            "alternatives": {
                "resource": "obsidian://examples/sample-note",
                "syntax_guide_tool": "get_ofm_syntax_guide",
                "quick_ref_tool": "get_ofm_quick_ref"
            }
        }))
    }

    // ─── DIFF TOOLS ──────────────────────────────────────────────────

    #[tool(
        description = "Compare two notes side-by-side showing unified diff, line-level and word-level changes, and similarity score",
        usage = "Use to understand differences between two notes, find duplicate content, or review changes. Returns unified diff format with added/removed/changed line counts and word-level inline changes",
        performance = "Fast (<50ms typical). Uses line-level then word-level diff for changed lines",
        related = ["read_note", "find_duplicates", "compare_notes"],
        examples = ["diff_notes(left='projects/plan-v1.md', right='projects/plan-v2.md')"]
    )]
    async fn diff_notes(&self, left: String, right: String) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = DiffTools::new(manager);
        let result = tools
            .diff_notes(&left, &right)
            .await
            .map_err(to_mcp_error)?;
        StandardResponse::new(
            &vault_name,
            "diff_notes",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["read_note", "edit_note", "compare_notes"])
        .to_json()
    }

    #[tool(
        description = "Compare current note with a previous version from the audit trail",
        usage = "Use to see what changed in a note over time. Specify operation_id from audit_log to identify the version to compare against",
        performance = "Fast (<50ms for diff, plus audit snapshot read time)",
        related = ["audit_log", "rollback_preview", "diff_notes"],
        examples = ["diff_note_version(path='notes/todo.md', operation_id='abc-123')"]
    )]
    async fn diff_note_version(
        &self,
        path: String,
        operation_id: String,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let audit_tools = self.get_audit_tools().await?;

        // Get the snapshot from the audit entry
        let entry = audit_tools
            .audit_log()
            .get_entry(&operation_id)
            .await
            .map_err(to_mcp_error)?
            .ok_or_else(|| {
                McpError::internal(format!("Audit entry not found: {}", operation_id))
            })?;

        let snapshot_id = entry
            .before_snapshot_id
            .as_ref()
            .or(entry.after_snapshot_id.as_ref())
            .ok_or_else(|| {
                McpError::internal("No snapshot available for this operation".to_string())
            })?;

        let snapshot_content = audit_tools
            .snapshot_store()
            .retrieve(snapshot_id)
            .await
            .map_err(to_mcp_error)?;

        // Read current content
        let current_content = manager
            .read_file(&std::path::PathBuf::from(&path))
            .await
            .map_err(to_mcp_error)?;

        let result = DiffTools::diff_content(
            &snapshot_content,
            &current_content,
            &format!(
                "{} (version {})",
                path,
                &operation_id[..8.min(operation_id.len())]
            ),
            &format!("{} (current)", path),
        );

        StandardResponse::new(
            &vault_name,
            "diff_note_version",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["audit_log", "rollback_note", "read_note"])
        .to_json()
    }

    // ─── QUALITY TOOLS ───────────────────────────────────────────────

    #[tool(
        description = "Evaluate note quality across readability, structure, completeness, and staleness dimensions (0-100 score per dimension plus composite)",
        usage = "Use to assess individual note quality and get specific improvement recommendations. Examines heading hierarchy, link density, vocabulary diversity, metadata completeness, and modification recency",
        performance = "Fast (<100ms per note). Parses content and checks graph for backlinks",
        related = ["vault_quality_report", "find_stale_notes", "full_health_analysis"],
        examples = ["evaluate_note_quality(path='projects/research.md')"]
    )]
    async fn evaluate_note_quality(&self, path: String) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = QualityTools::new(manager);
        let result = tools.evaluate_note(&path).await.map_err(to_mcp_error)?;
        StandardResponse::new(
            &vault_name,
            "evaluate_note_quality",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["vault_quality_report", "edit_note", "find_stale_notes"])
        .to_json()
    }

    #[tool(
        description = "Generate vault-wide quality report with score distribution, dimension averages, lowest/highest quality notes, and recommendations",
        usage = "Use for vault-wide quality assessment. Identifies notes needing improvement and provides aggregate metrics across readability, structure, completeness, and staleness",
        performance = "Moderate to slow (500ms-5s depending on vault size). Evaluates all notes",
        related = ["evaluate_note_quality", "find_stale_notes", "full_health_analysis", "explain_vault"],
        examples = ["vault_quality_report()", "vault_quality_report(bottom_n=20)"]
    )]
    async fn vault_quality_report(&self, bottom_n: Option<usize>) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = QualityTools::new(manager);
        let result = tools
            .vault_quality_report(bottom_n.unwrap_or(10))
            .await
            .map_err(to_mcp_error)?;
        StandardResponse::new(
            &vault_name,
            "vault_quality_report",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(result.total_notes)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["evaluate_note_quality", "find_stale_notes"])
        .to_json()
    }

    #[tool(
        description = "Find notes that have not been updated recently, sorted by staleness (most stale first)",
        usage = "Use to identify neglected content that may need review, updating, or archiving. Configurable threshold in days and result limit",
        performance = "Moderate (200ms-2s depending on vault size). Checks file modification times",
        related = ["evaluate_note_quality", "vault_quality_report", "query_metadata"],
        examples = ["find_stale_notes(threshold_days=90)", "find_stale_notes(threshold_days=30, limit=20)"]
    )]
    async fn find_stale_notes(
        &self,
        threshold_days: Option<u64>,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
        let tools = QualityTools::new(manager);
        let result = tools
            .find_stale_notes(threshold_days.unwrap_or(90), limit.unwrap_or(20))
            .await
            .map_err(to_mcp_error)?;
        let count = result.len();
        StandardResponse::new(
            &vault_name,
            "find_stale_notes",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["evaluate_note_quality", "read_note", "edit_note"])
        .to_json()
    }

    // ─── SIMILARITY TOOLS ────────────────────────────────────────────

    #[tool(
        description = "Find notes semantically similar to a query using TF-IDF cosine similarity (finds conceptual matches beyond exact keyword overlap)",
        usage = "Use when keyword search returns too few results or you want conceptual similarity. Returns similarity scores (0-1) and shared terms for explainability. More sophisticated than keyword search",
        performance = "Moderate (<500ms for 10k notes). Builds TF-IDF vectors on first call, cached for subsequent queries",
        related = ["search", "find_similar_notes", "recommend_related", "advanced_search"],
        examples = ["semantic_search(query='distributed systems architecture')", "semantic_search(query='machine learning concepts', limit=20)"]
    )]
    async fn semantic_search(
        &self,
        query: String,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;
        self.flush_reindex_for_active_vault().await?;
        let engine = self.get_similarity_engine().await?;
        let results = engine.semantic_search(&query, limit.unwrap_or(10));
        let count = results.len();
        StandardResponse::new(
            &vault_name,
            "semantic_search",
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["read_note", "find_similar_notes", "advanced_search"])
        .to_json()
    }

    #[tool(
        description = "Find notes most similar in content to a specific note using TF-IDF cosine similarity",
        usage = "Use to discover related notes for linking, find candidates for merging, or identify thematic clusters. More content-aware than graph-based get_related_notes",
        performance = "Moderate (<500ms for 10k notes). Uses pre-built TF-IDF vectors",
        related = ["semantic_search", "recommend_related", "get_related_notes", "find_duplicates"],
        examples = ["find_similar_notes(path='projects/research.md')", "find_similar_notes(path='ideas/concept.md', limit=20)"]
    )]
    async fn find_similar_notes(
        &self,
        path: String,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;
        self.flush_reindex_for_active_vault().await?;
        let engine = self.get_similarity_engine().await?;
        let results = engine.find_similar_notes(&path, limit.unwrap_or(10));
        let count = results.len();
        StandardResponse::new(
            &vault_name,
            "find_similar_notes",
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["read_note", "semantic_search", "get_backlinks"])
        .to_json()
    }

    // ─── DUPLICATE TOOLS ─────────────────────────────────────────────

    #[tool(
        description = "Find near-duplicate notes across vault using SimHash fingerprinting and TF-IDF cosine similarity verification",
        usage = "Use to identify redundant content, merge candidates, or detect copied notes. Default threshold 0.8 catches close duplicates; lower to 0.6 for looser matching. Two-stage: fast SimHash filtering then precise verification",
        performance = "Moderate (<2s for 10k notes). SimHash O(N^2) candidate filtering then TF-IDF verification",
        related = ["compare_notes", "find_similar_notes", "diff_notes"],
        examples = ["find_duplicates()", "find_duplicates(threshold=0.6, limit=50)"]
    )]
    async fn find_duplicates(
        &self,
        threshold: Option<f64>,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = DuplicateTools::new(manager);
        let result = tools
            .find_duplicates(threshold.unwrap_or(0.8), limit.unwrap_or(20))
            .await
            .map_err(to_mcp_error)?;
        let count = result.len();
        StandardResponse::new(
            &vault_name,
            "find_duplicates",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["compare_notes", "diff_notes", "read_note"])
        .to_json()
    }

    #[tool(
        description = "Compare two specific notes showing similarity score, shared terms, diff summary, and actionable recommendation",
        usage = "Use to assess whether two notes should be merged, linked, or kept separate. Returns similarity score (0-1), shared vocabulary, line-level diff statistics, and a recommendation",
        performance = "Moderate (<500ms). Builds TF-IDF vectors and computes diff",
        related = ["find_duplicates", "diff_notes", "find_similar_notes"],
        examples = ["compare_notes(left='projects/plan-v1.md', right='projects/plan-v2.md')"]
    )]
    async fn compare_notes(&self, left: String, right: String) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = DuplicateTools::new(manager);
        let result = tools
            .compare_notes(&left, &right)
            .await
            .map_err(to_mcp_error)?;
        StandardResponse::new(
            &vault_name,
            "compare_notes",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["diff_notes", "read_note", "find_duplicates"])
        .to_json()
    }

    // ─── AUDIT TOOLS ─────────────────────────────────────────────────

    #[tool(
        description = "View operation history for the active vault with optional filters by path, operation type (CREATE/UPDATE/DELETE/MOVE), and result limit",
        usage = "Use to review what changed in the vault, when, and get operation IDs for rollback. Returns chronological entries (newest first) with operation IDs, timestamps, paths, and content hashes",
        performance = "Fast (<100ms typical). Reads from append-only JSONL log file",
        related = ["rollback_note", "rollback_preview", "audit_stats", "diff_note_version"],
        examples = ["audit_log()", "audit_log(path='projects/', limit=20)", "audit_log(operation='DELETE')"]
    )]
    async fn audit_log(
        &self,
        path: Option<String>,
        operation: Option<String>,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("audit_log").await?;
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;
        let audit_tools = self.get_audit_tools().await?;

        let mut filter = AuditFilter::new().with_limit(limit.unwrap_or(50));
        if let Some(p) = path {
            filter = filter.with_path(p);
        }
        if let Some(op) = operation {
            let op_type = match op.to_uppercase().as_str() {
                "CREATE" => OperationType::Create,
                "UPDATE" => OperationType::Update,
                "DELETE" => OperationType::Delete,
                "MOVE" => OperationType::Move,
                "ROLLBACK" => OperationType::Rollback,
                _ => {
                    return Err(McpError::internal(format!(
                        "Unknown operation type: {}. Use CREATE, UPDATE, DELETE, MOVE, or ROLLBACK",
                        op
                    )));
                }
            };
            filter = filter.with_operation(op_type);
        }

        let entries = audit_tools.query_log(&filter).await.map_err(to_mcp_error)?;
        let count = entries.len();
        StandardResponse::new(
            &vault_name,
            "audit_log",
            serde_json::to_value(&entries).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["rollback_preview", "diff_note_version", "audit_stats"])
        .to_json()
    }

    #[tool(
        description = "Preview what a rollback would change without applying it (dry run). Shows unified diff between current content and rollback target",
        usage = "Always use before rollback_note to verify the change. Returns whether the rollback would create, delete, or modify the file, plus a diff preview",
        performance = "Fast (<50ms). Read-only operation",
        related = ["rollback_note", "audit_log", "diff_note_version"],
        examples = ["rollback_preview(operation_id='abc-123-def-456')"]
    )]
    async fn rollback_preview(&self, operation_id: String) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("rollback_preview").await?;
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let audit_tools = self.get_audit_tools().await?;
        let vault_path = manager.vault_path().clone();
        let result = audit_tools
            .rollback_preview(&operation_id, &vault_path)
            .await
            .map_err(to_mcp_error)?;
        let mut response = StandardResponse::new(
            &vault_name,
            "rollback_preview",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["rollback_note", "audit_log"]);
        for w in &result.warnings {
            response = response.with_warning(w.clone());
        }
        response.to_json()
    }

    #[tool(
        description = "Restore a note to its state before a specific operation (identified by operation_id from audit_log)",
        usage = "Use to undo unwanted changes. The rollback itself is recorded in the audit trail. Use rollback_preview first to verify. Cannot roll back MOVE or ROLLBACK operations",
        performance = "Moderate (<100ms). Reads snapshot, writes file atomically, records new audit entry",
        related = ["rollback_preview", "audit_log", "diff_note_version"],
        examples = ["rollback_note(operation_id='abc-123-def-456')"]
    )]
    async fn rollback_note(&self, operation_id: String) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("rollback_note").await?;
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let audit_tools = self.get_audit_tools().await?;
        let vault_path = manager.vault_path().clone();
        let result = audit_tools
            .rollback_execute(&operation_id, &vault_path)
            .await
            .map_err(to_mcp_error)?;

        // Invalidate similarity engine cache since vault content changed
        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;

        // Re-parse the rolled-back file so the link graph reflects the restored content
        let restored_path = std::path::PathBuf::from(&result.path);
        let full_path = vault_path.join(&restored_path);
        if full_path.exists()
            && tokio::fs::read_to_string(&full_path).await.is_ok()
            && let Ok(vault_file) = manager.parse_file(&restored_path).await
        {
            let graph = manager.link_graph();
            let mut graph_write = graph.write().await;
            let _ = graph_write.add_file(&vault_file);
            let _ = graph_write.update_links(&vault_file);
        }

        StandardResponse::new(
            &vault_name,
            "rollback_note",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["read_note", "audit_log"])
        .to_json()
    }

    #[tool(
        description = "Get audit trail statistics including operation counts by type, total snapshot storage used, and time range of recorded operations",
        usage = "Use for vault auditing overview. Shows operation breakdown (CREATE/UPDATE/DELETE/MOVE) and total snapshot disk usage",
        performance = "Fast (<50ms). Aggregates from log file",
        related = ["audit_log", "explain_vault", "vault_quality_report"],
        examples = ["audit_stats()"]
    )]
    async fn audit_stats(&self) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("audit_stats").await?;
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;
        let audit_tools = self.get_audit_tools().await?;
        let result = audit_tools.stats().await.map_err(to_mcp_error)?;
        StandardResponse::new(
            &vault_name,
            "audit_stats",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["audit_log", "explain_vault"])
        .to_json()
    }
}
