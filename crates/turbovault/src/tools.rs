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
    GraphTools, GroundingTools, MetadataTools, OkfTools, QualityTools, ReindexQueue,
    RelationshipTools, SearchEngine, SearchQuery, SearchTools, SimilarityEngine, TemplateEngine,
    VaultLifecycleTools, VaultRepo, ViewerTools, WriteMode, WriteTools, obsidian_uri,
};
use turbovault_vault::VaultManager;

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

        self.initialize_audit_for_manager(&vault_name, &mut manager)
            .await;

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
        if let Some(handle) = self.git_drainers.write().await.remove(name) {
            handle.abort();
        }
        if let Some(handle) = self.git_ref_listeners.write().await.remove(name) {
            handle.abort();
        }
        self.git_reindex_queues.write().await.remove(name);
        self.git_locks.write().await.remove(name);
        self.git_repos.write().await.remove(name);

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
            let drained =
                Self::drain_pending_diffs(path.clone(), Arc::clone(&locks), Arc::clone(&queue))
                    .await?;
            if drained.is_empty() {
                break;
            }
            // Collapse all pending changes into a path→latest-presence map
            // so the search engine writer (GWS.14c) commits ONCE per drain
            // pass instead of once per pending commit (writer create is
            // ~10ms; amortizes nicely).
            let mut collapsed_for_search: HashMap<String, bool> = HashMap::new();

            for (commit, changes) in drained {
                for (rel_path, present) in changes {
                    collapsed_for_search.insert(rel_path.clone(), present);
                    Self::apply_one_path(&manager, &rel_path, present).await;
                }
                queue.advance_cursor(commit);
            }

            // GWS.14c: incrementally update the cached SearchEngine, then evict
            // the similarity cache (incremental TF-IDF is a follow-up — the
            // corpus-wide IDF table drifts under per-doc add/remove).
            self.apply_collapsed_to_search(vault_name, collapsed_for_search)
                .await;
            self.invalidate_similarity_cache().await;
        }
        Ok(())
    }

    /// v3b.2: drain the pending reindex queue into per-commit diffs inside
    /// `spawn_blocking` — the `VaultRepo` handle is `!Sync`, so it must never
    /// cross an await. Returns one `(commit, [(path, present)])` entry per
    /// drained commit. Extracted from `flush_reindex_for_vault`.
    async fn drain_pending_diffs(
        path: PathBuf,
        locks: Arc<CommitLocks>,
        queue: Arc<ReindexQueue>,
    ) -> McpResult<Vec<(turbovault_tools::Oid, Vec<(String, bool)>)>> {
        tokio::task::spawn_blocking(move || {
            // Open the repo locally so its !Sync handle never escapes this
            // thread. Drain the diff bookkeeping (sync); the graph apply runs
            // back in the async caller.
            let repo = VaultRepo::open_with_locks(&path, locks)
                .map_err(|e| McpError::internal(format!("flush_reindex open repo: {}", e)))?;
            let mut batches = Vec::new();
            while let Some(commit) = queue.pop_front() {
                // hq8: unify with ReindexQueue::drain_through (tlx.1) — a commit
                // the ref-watcher enqueued may no longer be reachable. A
                // first-parent (or diff) failure must SKIP that commit, not
                // propagate and brick the whole read-path flush.
                let parent = match repo.git_commit_first_parent(commit) {
                    Ok(p) => p,
                    Err(e) => {
                        log::warn!(
                            "flush_reindex: skipping commit {} after first-parent error: {}",
                            commit,
                            e
                        );
                        continue;
                    }
                };
                match repo.diff_path_statuses(parent, commit) {
                    Ok(changes) => batches.push((commit, changes)),
                    Err(e) => {
                        log::warn!(
                            "flush_reindex: skipping commit {} after diff error: {}",
                            commit,
                            e
                        );
                    }
                }
            }
            drop(repo);
            Ok::<_, McpError>(batches)
        })
        .await
        .map_err(|e| McpError::internal(format!("flush_reindex task: {}", e)))?
    }

    /// v3b.2: apply one `(path, present)` change to the link graph — extracted
    /// from `flush_reindex_for_vault`'s inner loop to flatten its nesting. A
    /// present path is re-parsed and its node/links rebuilt; an absent path is
    /// removed. Parse failures are logged and skipped (a later commit may have
    /// deleted the file).
    async fn apply_one_path(manager: &Arc<VaultManager>, rel_path: &str, present: bool) {
        let full_path = manager.vault_path().join(rel_path);
        let graph_handle = manager.link_graph();
        if present {
            match manager.parse_file(std::path::Path::new(rel_path)).await {
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

    /// v3b.2: GWS.14c incremental search update for one drain pass. Skips when
    /// no `SearchEngine` is cached (next query builds fresh); on apply error
    /// falls back to a full cache evict. Extracted from
    /// `flush_reindex_for_vault`.
    async fn apply_collapsed_to_search(&self, vault_name: &str, collapsed: HashMap<String, bool>) {
        if collapsed.is_empty() {
            return;
        }
        let cached = {
            let engines = self.search_engines.read().await;
            engines.get(vault_name).cloned()
        };
        if let Some(engine) = cached {
            let change_vec: Vec<(String, bool)> = collapsed.into_iter().collect();
            if let Err(e) = engine.apply_changes(change_vec).await {
                log::warn!(
                    "GWS.14c search incremental apply failed; falling back to evict: {}",
                    e
                );
                self.invalidate_search_cache().await;
            }
        }
    }
}
