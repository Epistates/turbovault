//! MCP tool implementations for Obsidian vault

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OnceCell, RwLock};
use turbomcp::prelude::*;
use turbovault_audit::{AuditFilter, AuditLog, OperationType, SnapshotStore};
use turbovault_core::ServerConfig;
use turbovault_core::error::Error;
use turbovault_core::prelude::MultiVaultManager;
use turbovault_tools::{
    AnalysisTools, AuditTools, BatchOperation, BatchTools, DiffTools, DuplicateTools, ExportTools,
    FileTools, GraphTools, MetadataTools, QualityTools, RelationshipTools, SearchEngine,
    SearchQuery, SearchTools, SimilarityEngine, TemplateEngine, VaultLifecycleTools, WriteMode,
    obsidian_uri,
};
use turbovault_vault::{
    EventEnvelope, EventFilter, SubscriptionHandle, SubscriptionRegistry, VaultEvent,
    VaultEventKind, VaultManager, VaultWatcher, WatcherConfig,
};

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

/// JSON-serializable mirror of [`EventFilter`] for the MCP tool layer.
///
/// We can't reuse `EventFilter` directly because `VaultEventKind`'s
/// existing `serde` attributes don't map cleanly to `JsonSchema`
/// without adding a `schemars` dep on `turbovault-vault`, so we keep a
/// thin DTO here and convert at the tool boundary.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct EventFilterDto {
    /// Gitignore-style glob patterns. Empty means "all paths match".
    /// Negation patterns (starting with `!`) exclude matches.
    #[serde(default)]
    pub globs: Vec<String>,
    /// If present, only events whose kind is listed are delivered.
    /// Valid values: "created", "modified", "deleted", "renamed".
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
}

impl EventFilterDto {
    /// Convert into the library-layer `EventFilter`, mapping kind
    /// strings to `VaultEventKind`. Unknown kinds are surfaced as an
    /// invalid-request error rather than silently dropped so clients
    /// catch typos immediately.
    fn into_filter(self) -> McpResult<EventFilter> {
        let kinds = self
            .kinds
            .map(|v| {
                v.into_iter()
                    .map(|s| match s.as_str() {
                        "created" => Ok(VaultEventKind::Created),
                        "modified" => Ok(VaultEventKind::Modified),
                        "deleted" => Ok(VaultEventKind::Deleted),
                        "renamed" => Ok(VaultEventKind::Renamed),
                        other => Err(McpError::invalid_request(format!(
                            "unknown event kind {:?}; expected one of created|modified|deleted|renamed",
                            other
                        ))),
                    })
                    .collect::<McpResult<Vec<_>>>()
            })
            .transpose()?;
        Ok(EventFilter {
            globs: self.globs,
            kinds,
        })
    }
}

/// Response shape for `subscribe_vault_events`.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct SubscribeResult {
    /// Opaque UUIDv4 handle. Pass to `fetch_vault_events` /
    /// `unsubscribe_vault_events`.
    pub handle: String,
    /// RFC-3339 UTC timestamp when the subscription was registered.
    pub created_at: String,
}

/// Wire-format envelope for a delivered vault event.
///
/// JSON shape intentionally flat — keeps MCP client code trivial.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct EventEnvelopeDto {
    /// Monotonically-increasing sequence number (per-subscription).
    pub seq: u64,
    /// One of: "created" | "modified" | "deleted" | "renamed".
    pub kind: &'static str,
    /// For renamed events this is the destination path; for all
    /// others this is the affected path. Paths are reported in the
    /// OS-native encoding the vault uses.
    pub path: String,
    /// Source path for "renamed" events; `None` for all other kinds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl From<EventEnvelope> for EventEnvelopeDto {
    fn from(env: EventEnvelope) -> Self {
        let (kind, path, from) = match env.event {
            VaultEvent::FileCreated(p) => ("created", path_to_string(&p), None),
            VaultEvent::FileModified(p) => ("modified", path_to_string(&p), None),
            VaultEvent::FileDeleted(p) => ("deleted", path_to_string(&p), None),
            VaultEvent::FileRenamed(from, to) => {
                ("renamed", path_to_string(&to), Some(path_to_string(&from)))
            }
        };
        Self {
            seq: env.seq,
            kind,
            path,
            from,
        }
    }
}

fn path_to_string(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Response shape for `fetch_vault_events`.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct FetchResultDto {
    /// Events delivered on this call, in dispatch order. May be empty
    /// on timeout.
    pub events: Vec<EventEnvelopeDto>,
    /// Pass this back as the next call's `since_seq` for seamless
    /// resume across restarts.
    pub next_seq: u64,
    /// Cumulative count of events dropped on this subscription (due
    /// to backpressure). Non-zero means the client should treat the
    /// current view as stale and re-read from the authoritative
    /// source.
    pub dropped: u64,
}

/// Response shape for `unsubscribe_vault_events`.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct UnsubResult {
    /// `true` if a subscription with that handle was found and
    /// removed; `false` if the handle was unknown (already removed,
    /// never existed, or reaped for inactivity).
    pub removed: bool,
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
    /// Subscription registry for pull-based vault-event streaming. Built
    /// lazily on the first `subscribe_vault_events` call because the
    /// server is vault-agnostic at startup — there's no path to watch
    /// until a vault is registered. `OnceCell::get_or_try_init` ensures
    /// a single construction even under concurrent subscribers. The
    /// registry is pinned to the active vault at first-subscribe time
    /// for the server's lifetime; switching to a different vault later
    /// continues using the original watcher (documented limitation).
    subscription_registry: Arc<OnceCell<Arc<SubscriptionRegistry>>>,
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
            subscription_registry: Arc::new(OnceCell::new()),
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

    /// Obtain the subscription registry, building it on first call.
    ///
    /// Uses the currently-active vault's path to construct a
    /// `VaultWatcher` rooted at that directory, then wraps it in a
    /// `SubscriptionRegistry`. Subsequent callers re-use the same
    /// registry via `OnceCell::get_or_try_init` — no redundant
    /// watchers, no race on construction.
    ///
    /// Returns an error if no vault is active or the watcher fails to
    /// start (e.g. path doesn't exist).
    async fn get_or_init_subscription_registry(
        &self,
    ) -> McpResult<Arc<SubscriptionRegistry>> {
        self.subscription_registry
            .get_or_try_init(|| async {
                // Resolve active vault's path. We don't hold any
                // VaultManager reference — the watcher just needs the
                // directory, independent of the vault's index state.
                let vault_config = self
                    .multi_vault_mgr
                    .get_active_vault_config()
                    .await
                    .map_err(|e| {
                        McpError::internal(format!(
                            "cannot subscribe to vault events without an active vault: {}",
                            e
                        ))
                    })?;

                // Build & start a watcher. We keep the same debounce /
                // markdown-only defaults the rest of the server uses
                // so subscription events are consistent with what
                // other tools observe.
                let (mut watcher, event_rx) = VaultWatcher::new(
                    vault_config.path.clone(),
                    WatcherConfig::default(),
                )
                .map_err(|e| {
                    McpError::internal(format!("failed to create vault watcher: {}", e))
                })?;
                watcher.start().await.map_err(|e| {
                    McpError::internal(format!("failed to start vault watcher: {}", e))
                })?;

                let registry = SubscriptionRegistry::new(Arc::new(watcher), event_rx);
                Ok::<_, McpError>(Arc::new(registry))
            })
            .await
            .cloned()
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

    /// Invalidate cached search engine for the active vault (call after any write operation)
    async fn invalidate_search_cache(&self) {
        if let Ok(vault_name) = self.get_active_vault_name().await {
            let mut cache = self.search_engines.write().await;
            cache.remove(&vault_name);
        }
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

    /// Helper to get both vault name and manager (eliminates 31 repeated preambles)
    /// This is the most common pattern across all tools
    async fn get_vault_pair(&self) -> McpResult<(String, Arc<VaultManager>)> {
        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;
        Ok((vault_name, manager))
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
                "base": ["CommonMark", "GitHub Flavored Markdown", "LaTeX"],
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
                    "Embeds: ![[image.png]] and ![[noteection]]",
                    "Block refs: [[note#^block-id]] and ^block-id",
                    "Callouts: > [!note] Title",
                    "Highlights: ==text==",
                    "Comments: %%hidden%%"
                ],
                "important_notes": [
                    "Use wikilinks [[note]] for internal references, not markdown links",
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

    // ==================== SPIKE: TurboMCP notification API probe ====================
    //
    // Goal of this spike: find out whether TurboMCP 3.0.11 can deliver a
    // custom-method notification (`notifications/vault/ping`) to a
    // WebSocket client from inside a #[tool] handler.
    //
    // Findings (see commit message):
    //   1. Tool handlers receive `turbomcp_core::RequestContext`, which has
    //      no session field, no `notify` method, and no `server_to_client`
    //      handle. The richer `turbomcp_server::RequestContext` (which has
    //      a private `session: Option<Arc<dyn McpSession>>`) is stripped
    //      by `to_core_context()` before reaching the handler. The macro
    //      (`turbomcp_macros::server::generate_server`) invokes
    //      `self.#fn_name(...)` with a `&turbomcp_core::context::
    //      RequestContext`, not the server one.
    //   2. `ServerNotification` in turbomcp-protocol is a closed enum —
    //      only `notifications/message`, `/progress`, `/resources/*`,
    //      `/tools/list_changed`, etc. No representation for custom
    //      methods like `notifications/vault/event`.
    //   3. The WebSocket transport
    //      (`turbomcp-server-3.0.10/src/transport/websocket.rs:241`)
    //      builds the context with `RequestContext::websocket()` and
    //      does NOT call `.with_session(...)`. STDIO does
    //      (`transport/line.rs:235`). Even if a handler could reach the
    //      session, WebSocket-served tool calls have no session to reach.
    //
    // This spike therefore can only demonstrate the limitation, not push
    // a custom notification through. It returns a stub response that
    // documents the gap so the feat branch can be planned around it
    // (return-to-caller encoding, or an upstream turbomcp patch).
    //
    // Manual verification procedure:
    //   1. Build with: `cargo build -p turbovault --features websocket`
    //   2. Run: `cargo run -p turbovault --features websocket -- \
    //              --transport websocket --port 3001`
    //   3. Connect: `wscat -c ws://127.0.0.1:3001/ws`
    //   4. Initialize:
    //        {"jsonrpc":"2.0","id":1,"method":"initialize","params":{
    //          "protocolVersion":"2025-06-18","capabilities":{},
    //          "clientInfo":{"name":"spike","version":"0"}}}
    //   5. Initialized notification:
    //        {"jsonrpc":"2.0","method":"notifications/initialized"}
    //   6. Call the spike tool:
    //        {"jsonrpc":"2.0","id":2,"method":"tools/call",
    //         "params":{"name":"ping_with_notification","arguments":{}}}
    //   7. Observe the response: it returns `gap_documented: true` and
    //      NO `notifications/vault/ping` line arrives out-of-band —
    //      because TurboMCP's WS transport can't deliver one.
    /// Spike: probe TurboMCP's server-to-client notification capability
    #[tool(
        description = "SPIKE: attempts to push a notifications/vault/ping message to the calling WS client and returns a structured report on whether the transport supports it",
        usage = "Diagnostic tool only. Returns the API-gap findings for the subscribe_vault_events design",
        performance = "Fast (<5ms)",
        related = [],
        examples = ["ping_with_notification()"]
    )]
    async fn ping_with_notification(
        &self,
        ctx: &turbomcp::RequestContext,
    ) -> McpResult<serde_json::Value> {
        // Empirical probe: try to reach a session handle from the core
        // RequestContext. There is no public API to do so — the methods
        // below are the complete set (`request_id`, `transport`,
        // `metadata`, `principal`). See turbomcp-core-3.0.10/src/context.rs.
        let probe = format!(
            "request_id={}, transport={:?}, metadata_keys={:?}",
            ctx.request_id,
            ctx.transport,
            ctx.metadata.keys().collect::<Vec<_>>()
        );
        // The ctx handed to us is `turbomcp_core::RequestContext`; it has
        // no `notify` / `session` / `server_to_client` accessor. We
        // document the gap here rather than faking success.
        let report = serde_json::json!({
            "attempted_method": "notifications/vault/ping",
            "payload": {
                "ts": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
                "note": "spike payload"
            },
            "gap_documented": true,
            "reasons": [
                "turbomcp_core::RequestContext (handed to #[tool] handlers) \
                 exposes no McpSession / ServerToClientRequests handle",
                "turbomcp-protocol::ServerNotification is a closed enum; \
                 no variant represents a custom method name like \
                 notifications/vault/event",
                "turbomcp-server::transport::websocket does not attach a \
                 SessionHandle to RequestContext (line 241: \
                 RequestContext::websocket() with no with_session call), \
                 so even if the plumbing existed at the handler level \
                 the WS transport has no session to route through"
            ],
            "ctx_probe": probe,
            "turbomcp_version": "3.0.11",
            "remediation_options": [
                "Patch turbomcp-server to install a SessionHandle in the \
                 WS transport AND expose ServerToClientRequests via the \
                 core RequestContext (upstream change)",
                "Fork turbomcp and add a JsonRpcNotification escape hatch \
                 that accepts a free-form method name",
                "Design turbovault around synchronous response delivery: \
                 clients poll a fetch_vault_events(handle) tool instead of \
                 receiving push notifications"
            ]
        });

        StandardResponse::new(
            "spike",
            "ping_with_notification",
            report,
        )
        .to_json()
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

        // Compute hash for use with edit_file
        let hash = turbovault_vault::compute_hash(&content);

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
        description = "Write a note in active vault with mode control: 'overwrite' (default) replaces entire file, 'append' adds to end, 'prepend' adds after frontmatter. Supports optimistic concurrency: pass expected_hash (from read_note) to prevent overwriting concurrent changes",
        usage = "Use for creating new notes, replacing existing ones, or appending/prepending content. Append mode is ideal for daily notes and journals. Prepend inserts after frontmatter if present. Accepts Obsidian Flavored Markdown. For targeted edits, use edit_note instead. Pass expected_hash to detect concurrent modifications",
        performance = "Moderate (<50ms typical). Includes filesystem write and link graph update",
        related = ["read_note", "edit_note", "create_from_template"],
        examples = ["mode: overwrite (default)", "mode: append (add to end)", "mode: prepend (add after frontmatter)", "expected_hash: <hash from read_note>"]
    )]
    async fn write_note(
        &self,
        path: String,
        content: String,
        mode: Option<String>,
        expected_hash: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let write_mode = WriteMode::from_str_opt(mode.as_deref()).map_err(to_mcp_error)?;
        let tools = FileTools::new(manager);
        tools
            .write_file_with_mode(&path, &content, write_mode, expected_hash.as_deref())
            .await
            .map_err(to_mcp_error)?;

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
        description = "Apply targeted edits using SEARCH/REPLACE blocks (safer than full overwrite)",
        usage = "Use for precise modifications without reading/writing entire file. Requires exact match of search text. Supports optional content hash for conflict detection and dry_run mode for preview. Returns applied changes, rejected changes, and new hash",
        performance = "Fast (<30ms typical). More efficient than read+write cycle for small edits",
        related = ["read_note", "write_note"],
        examples = []
    )]
    async fn edit_note(
        &self,
        path: String,
        edits: String,
        expected_hash: Option<String>,
        dry_run: Option<bool>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = FileTools::new(manager);
        let dry_run = dry_run.unwrap_or(false);
        let result = tools
            .edit_file(&path, &edits, expected_hash.as_deref(), dry_run)
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
        description = "Permanently delete a note from active vault (irreversible, confirmation-protected)",
        usage = "Use to remove unwanted notes. REQUIRES confirm_path parameter matching path exactly to prevent accidental deletion. Removes file from filesystem and updates link graph. Any links to this note become broken links. Use get_backlinks first to understand impact. Pass expected_hash for concurrency protection",
        performance = "Fast (<20ms typical). Includes filesystem delete and link graph update",
        related = ["get_backlinks", "get_broken_links", "move_note"],
        examples = ["path: drafts/old-idea.md, confirm_path: drafts/old-idea.md"]
    )]
    async fn delete_note(
        &self,
        path: String,
        confirm_path: String,
        expected_hash: Option<String>,
    ) -> McpResult<serde_json::Value> {
        // Safety: confirm_path must match path exactly
        if path != confirm_path {
            return Err(McpError::invalid_request(format!(
                "Confirmation failed: confirm_path '{}' does not match path '{}'. Both must be identical to proceed with deletion.",
                confirm_path, path
            )));
        }

        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = FileTools::new(manager);
        tools
            .delete_file_with_hash(&path, expected_hash.as_deref())
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        StandardResponse::new(
            vault_name,
            "delete_note",
            serde_json::json!({"path": path, "status": "deleted"}),
        )
        .with_next_step("quick_health_check")
        .to_json()
    }

    /// Move or rename a note
    #[tool(
        description = "Move or rename a note within active vault. Does NOT update wikilinks — use get_backlinks first to assess impact",
        usage = "Use to reorganize vault structure or rename notes. This performs a filesystem move only. Links pointing to the old path will become broken. Always call get_backlinks before moving to understand impact, then manually update references if needed. Pass expected_hash for concurrency protection",
        performance = "Fast (<20ms typical). Filesystem rename, falls back to copy+delete for cross-filesystem moves",
        related = ["get_backlinks", "get_forward_links", "search"],
        examples = []
    )]
    async fn move_note(
        &self,
        from: String,
        to: String,
        expected_hash: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = FileTools::new(manager);
        tools
            .move_file_with_hash(&from, &to, expected_hash.as_deref())
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        StandardResponse::new(
            vault_name,
            "move_note",
            serde_json::json!({"from": from, "to": to, "status": "moved"}),
        )
        .with_next_steps(&["get_backlinks", "get_forward_links"])
        .with_warning("Links pointing to the old path are now broken. Use get_backlinks and edit_note to update references.")
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);

        // Parse fields JSON
        let field_values: HashMap<String, String> = serde_json::from_str(&fields)
            .map_err(|e| McpError::invalid_request(format!("Invalid fields JSON: {}", e)))?;

        let result = engine
            .create_from_template(&template_id, &file_path, field_values)
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        let response = StandardResponse::new(
            vault_name,
            "create_from_template",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
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
        description = "List all vaults registered with the MCP server",
        usage = "Use to discover available vaults before setting active vault. Empty list means call add_vault first",
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
        description = "Execute multiple file operations atomically (all-or-nothing transaction)",
        usage = "Use for complex multi-file workflows requiring consistency. If any operation fails, all changes are rolled back. Not idempotent.",
        performance = "Depends on operation count and types. Transactions add ~10-50ms overhead.",
        related = ["write_note", "delete_note", "move_note"],
        examples = [
            r#"[{"type":"write","path":"note1.md","content":"..."}]"#,
            r#"[{"type":"delete","path":"old.md"},{"type":"write","path":"new.md","content":"..."}]"#,
            r#"[{"type":"move","from":"a.md","to":"b.md"},{"type":"write","path":"index.md","content":"..."}]"#
        ]
    )]
    async fn batch_execute(&self, operations: Vec<BatchOperation>) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;

        if operations.is_empty() {
            return Err(McpError::internal(
                "Batch operations list cannot be empty".to_string(),
            ));
        }

        let op_count = operations.len();
        let tools = BatchTools::new(manager);
        let result = tools
            .batch_execute(operations)
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        let response = StandardResponse::new(
            vault_name,
            "batch_execute",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(op_count)
        .with_meta("transactional", serde_json::json!(true))
        .with_next_step("quick_health_check");

        response.to_json()
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
        frontmatter: serde_json::Value,
        merge: Option<bool>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);

        let fm_map = match frontmatter {
            serde_json::Value::Object(map) => map,
            _ => {
                return Err(McpError::invalid_request(
                    "frontmatter must be a JSON object".to_string(),
                ));
            }
        };

        let result = tools
            .update_frontmatter(&path, fm_map, merge.unwrap_or(true))
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        StandardResponse::new(vault_name, "update_frontmatter", result)
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
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);

        let result = tools
            .manage_tags(&path, &operation, tags.as_deref())
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        StandardResponse::new(vault_name, "manage_tags", result)
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

        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = FileTools::new(manager);
        tools
            .move_file_with_hash(&from, &to, expected_hash.as_deref())
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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

    /// Get complete Obsidian Flavored Markdown syntax guide (tool fallback for clients without resource support)
    #[tool(
        description = "Get complete Obsidian Flavored Markdown syntax guide covering all OFM features",
        usage = "Use before writing notes to ensure correct syntax, or as reference for OFM extensions. Prefer resource obsidian://syntax/complete-guide if client supports resources",
        performance = "Instant, returns static documentation",
        related = ["get_ofm_quick_ref", "get_ofm_examples"],
        examples = []
    )]
    async fn get_ofm_syntax_guide(&self) -> McpResult<serde_json::Value> {
        let guide = crate::resources::OFM_SYNTAX_GUIDE.to_string();

        Ok(serde_json::json!({
            "title": "Obsidian Flavored Markdown - Complete Syntax Guide",
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
                "Math (LaTeX)",
                "Diagrams (Mermaid)",
                "Footnotes",
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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
        let (vault_name, manager) = self.get_vault_pair().await?;
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

    // ==================== Pull-based vault event stream ====================

    /// Register a subscription for vault file-system events.
    ///
    /// Pairs with `fetch_vault_events` + `unsubscribe_vault_events` to
    /// implement a pull-delivery event stream (rationale: TurboMCP 3.0.11
    /// cannot emit custom server-to-client notifications from `#[tool]`
    /// handlers; see PR #2 spike findings).
    #[tool(
        description = "Register to receive vault file-system events (created/modified/deleted/renamed). Returns an opaque handle; use fetch_vault_events to poll for events and unsubscribe_vault_events when done",
        usage = "Pass a filter to restrict events by path glob or event kind. Pull-based: events accumulate in a per-subscription 256-slot buffer; drain via fetch_vault_events on any cadence. If the client stalls for 15 minutes without polling, the server reaps the subscription automatically. Glob syntax is gitignore-like: `**` crosses path segments, `!pattern` excludes",
        performance = "Fast (<5ms typical). Spins up a file watcher lazily on the first subscribe",
        related = ["fetch_vault_events", "unsubscribe_vault_events"],
        examples = [
            "subscribe_vault_events()",
            "subscribe_vault_events(filter: { globs: [\"**/*.md\"] })",
            "subscribe_vault_events(filter: { globs: [\"02-KB-main/**/*.md\", \"!**/trash/**\"], kinds: [\"created\", \"modified\"] })"
        ]
    )]
    async fn subscribe_vault_events(
        &self,
        filter: Option<EventFilterDto>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;

        // Convert and validate the filter BEFORE building the registry
        // so the caller gets fast feedback on bad globs.
        let event_filter = filter
            .unwrap_or_default()
            .into_filter()?;

        let registry = self.get_or_init_subscription_registry().await?;
        let handle = registry
            .subscribe(event_filter, None)
            .await
            .map_err(to_mcp_error)?;

        // RFC-3339 UTC for created_at. chrono isn't in our deps; build
        // it from SystemTime + manual formatting to avoid a new crate.
        // Using the secs-since-epoch field plus a simple formatter
        // would be error-prone, so we fall back to a minimal %Y-%m-%dT%H:%M:%SZ
        // approximation built on std alone.
        let created_at = format_iso8601_now();

        let result = SubscribeResult {
            handle: handle.0,
            created_at,
        };

        StandardResponse::new(
            &vault_name,
            "subscribe_vault_events",
            serde_json::to_value(&result)
                .map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["fetch_vault_events", "unsubscribe_vault_events"])
        .to_json()
    }

    /// Long-poll for accumulated vault events on an existing subscription.
    #[tool(
        description = "Pull events from a subscription (returned by subscribe_vault_events). Long-polls up to timeout_ms (default 5000, max 30000) for the first event, then drains whatever else is queued up to max_events",
        usage = "Resume across restarts by passing since_seq equal to the previous response's next_seq — only newer events come back. A single subscription allows one fetcher at a time; concurrent fetch calls serialize behind the first. If `dropped` is non-zero, buffer overflowed and the client should treat its vault view as stale",
        performance = "Blocks up to timeout_ms; returns immediately once events arrive",
        related = ["subscribe_vault_events", "unsubscribe_vault_events"],
        examples = [
            "fetch_vault_events(handle: \"<uuid>\")",
            "fetch_vault_events(handle: \"<uuid>\", since_seq: 42, timeout_ms: 10000, max_events: 100)"
        ]
    )]
    async fn fetch_vault_events(
        &self,
        handle: String,
        since_seq: Option<u64>,
        timeout_ms: Option<u64>,
        max_events: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;

        // If no registry has been built yet, there are also no
        // subscriptions. Reject before we touch anything else — avoids
        // unnecessarily bootstrapping a watcher just to say "unknown
        // handle".
        let registry = self.subscription_registry.get().ok_or_else(|| {
            McpError::invalid_request(
                "no active subscriptions; call subscribe_vault_events first".to_string(),
            )
        })?;

        let sub_handle = SubscriptionHandle(handle);
        let timeout = timeout_ms.map(Duration::from_millis);
        let result = registry
            .fetch(&sub_handle, since_seq, timeout, max_events)
            .await
            .map_err(to_mcp_error)?;

        let dto = FetchResultDto {
            events: result.events.into_iter().map(Into::into).collect(),
            next_seq: result.next_seq,
            dropped: result.dropped,
        };

        let event_count = dto.events.len();
        StandardResponse::new(
            &vault_name,
            "fetch_vault_events",
            serde_json::to_value(&dto).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(event_count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["fetch_vault_events", "unsubscribe_vault_events"])
        .to_json()
    }

    /// Cancel an active vault-event subscription.
    #[tool(
        description = "Unregister a subscription created by subscribe_vault_events. Any events still buffered are discarded. Idempotent — unknown or already-removed handles return { removed: false }",
        usage = "Call this when the client is done streaming; otherwise the server reaps idle subscriptions automatically after 15 minutes of no fetches",
        performance = "Fast (<5ms)",
        related = ["subscribe_vault_events", "fetch_vault_events"],
        examples = ["unsubscribe_vault_events(handle: \"<uuid>\")"]
    )]
    async fn unsubscribe_vault_events(
        &self,
        handle: String,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;

        // No registry = nothing to unsubscribe; report removed=false.
        let removed = match self.subscription_registry.get() {
            Some(registry) => {
                registry
                    .unsubscribe(&SubscriptionHandle(handle))
                    .await
            }
            None => false,
        };

        let result = UnsubResult { removed };
        StandardResponse::new(
            &vault_name,
            "unsubscribe_vault_events",
            serde_json::to_value(&result)
                .map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .to_json()
    }
}

/// Format the current system time as a minimal RFC-3339 UTC string
/// (second precision). Used by `subscribe_vault_events` to stamp
/// `created_at` without pulling in `chrono` as a new dependency.
fn format_iso8601_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Safe on sane clocks; on a hypothetical pre-epoch clock we
    // return a clearly-invalid sentinel rather than panic.
    let secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => return "1970-01-01T00:00:00Z".to_string(),
    };
    // Convert Unix timestamp to Y/M/D H:M:S components. The math below
    // is the standard civil-from-days algorithm (Howard Hinnant).
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hour = (rem / 3_600) as u32;
    let minute = ((rem % 3_600) / 60) as u32;
    let second = (rem % 60) as u32;

    // Shift so days are counted from 0000-03-01 (Hinnant's epoch).
    let z = days + 719_468;
    let era = if z >= 0 { z / 146_097 } else { (z - 146_096) / 146_097 };
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = y + if m <= 2 { 1 } else { 0 };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, m, d, hour, minute, second
    )
}
