//! MCP tool implementations for Obsidian vault

use anyhow::Result;
use turbovault_core::ServerConfig;
use turbovault_core::error::Error;
use turbovault_core::prelude::MultiVaultManager;
use turbovault_tools::{
    AnalysisTools, BatchOperation, BatchTools, ExportTools, FileTools, GraphTools, MetadataTools,
    RelationshipTools, SearchEngine, SearchQuery, SearchTools, TemplateEngine, VaultLifecycleTools,
};
use turbovault_vault::VaultManager;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use turbomcp::prelude::*;

/// Helper to convert internal Error to McpError
fn to_mcp_error(e: Error) -> McpError {
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

#[turbomcp::server(
    name = "obsidian-vault",
    version = "1.0.0",
    transports = ["stdio", "http", "websocket", "tcp", "unix"]
)]
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

        let manager = VaultManager::new(server_config)
            .map_err(|e| McpError::internal(format!("Failed to create vault manager: {}", e)))?;

        // Initialize vault (scan files and build link graph) on first access
        manager
            .initialize()
            .await
            .map_err(|e| McpError::internal(format!("Failed to initialize vault: {}", e)))?;

        let manager = Arc::new(manager);

        // Cache it
        {
            let mut cache = self.vault_managers.write().await;
            cache.insert(vault_name, manager.clone());
        }

        Ok(manager)
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
                    "Embeds: ![[image.png]] and ![[note#section]]",
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
                "file_operations": ["read_note", "write_note", "delete_note", "move_note"],
                "search": ["search", "advanced_search", "recommend_related", "find_notes_from_template"],
                "link_analysis": ["get_backlinks", "get_forward_links", "get_related_notes", "get_hub_notes", "get_dead_end_notes"],
                "analysis": ["quick_health_check", "full_health_analysis", "get_broken_links", "detect_cycles"],
                "vault_management": ["add_vault", "list_vaults", "set_active_vault", "get_active_vault"],
                "templates": ["list_templates", "get_template", "create_from_template", "find_notes_from_template"],
                "metadata": ["get_metadata_value", "query_metadata"],
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

        // Compute hash for use with edit_file
        let hash = turbovault_vault::compute_hash(&content);

        StandardResponse::new(
            vault_name,
            "read_note",
            serde_json::json!({"path": path, "content": content, "hash": hash}),
        )
        .with_read_next_steps()
        .to_json()
    }

    /// Write or update a note
    #[tool(
        description = "Write or overwrite a note in active vault (creates if missing, replaces if exists)",
        usage = "Use for creating new notes or completely replacing existing ones. Accepts full markdown content with Obsidian Flavored Markdown syntax (wikilinks, callouts, block refs). Automatically creates parent directories and triggers link graph rebuild. For targeted edits, use edit_note instead",
        performance = "Moderate (<50ms typical). Includes filesystem write and link graph update",
        related = ["read_note", "edit_note", "create_from_template"],
        examples = ["meeting-notes/2024-01-15.md", "references/api-documentation.md"]
    )]
    async fn write_note(&self, path: String, content: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = FileTools::new(manager);
        tools
            .write_file(&path, &content)
            .await
            .map_err(to_mcp_error)?;

        StandardResponse::new(
            vault_name,
            "write_note",
            serde_json::json!({"path": path, "status": "written", "bytes": content.len()}),
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

        StandardResponse::new(
            vault_name,
            "edit_note",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_next_steps(&["read_note", "write_note"])
        .to_json()
    }

    /// Delete a note
    #[tool(
        description = "Permanently delete a note from active vault (irreversible)",
        usage = "Use to remove unwanted notes. Removes file from filesystem and updates link graph. Any links to this note become broken links. Use get_backlinks first to understand impact. Not idempotent (fails if already deleted)",
        performance = "Fast (<20ms typical). Includes filesystem delete and link graph update",
        related = ["get_backlinks", "get_broken_links", "move_note"],
        examples = ["drafts/old-idea.md", "archive/2023/deprecated-process.md"]
    )]
    async fn delete_note(&self, path: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = FileTools::new(manager);
        tools.delete_file(&path).await.map_err(to_mcp_error)?;

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
        description = "Move or rename a note within active vault with automatic link updates",
        usage = "Use to reorganize vault structure or rename notes. Updates all wikilinks pointing to this note across entire vault to preserve graph integrity. May break non-wikilink references (markdown links, embeds with paths). Returns old path, new path, and count of updated references",
        performance = "Variable (50-500ms depending on backlink count). Scans and updates all referencing files",
        related = ["get_backlinks", "get_forward_links", "search"],
        examples = []
    )]
    async fn move_note(&self, from: String, to: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = FileTools::new(manager);
        tools.move_file(&from, &to).await.map_err(to_mcp_error)?;

        StandardResponse::new(
            vault_name,
            "move_note",
            serde_json::json!({"from": from, "to": to, "status": "moved"}),
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
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = SearchTools::new(manager);
        let backlinks = tools.find_backlinks(&path).await.map_err(to_mcp_error)?;

        let count = backlinks.len();
        let response =
            StandardResponse::new(vault_name, "get_backlinks", serde_json::json!(backlinks))
                .with_count(count)
                .with_next_step("get_forward_links")
                .with_next_step("get_related_notes");

        if count == 0 {
            let response = response.with_warning("Note has no incoming links".to_string());
            serde_json::to_value(response)
        } else {
            serde_json::to_value(response)
        }
        .map_err(|e| McpError::internal(e.to_string()))
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
        let max_hops = max_hops.unwrap_or(2);
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
    async fn get_hub_notes(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager);
        let hubs = tools.get_hub_notes(10).await.map_err(to_mcp_error)?;

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
        let _file_tools = FileTools::new(manager.clone());
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
        examples = ["\"project alpha\"", "authentication", "TODO"]
    )]
    async fn search(&self, query: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = SearchEngine::new(manager).await.map_err(to_mcp_error)?;
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
        description = "Enhanced search with tag filtering and metadata constraints returning ranked results with match context",
        usage = "Use when search() returns too many results or you need tag-based filtering. Supports compound queries for precise targeting",
        performance = "Fast to Moderate - uses Tantivy search engine with BM25 ranking, additional filtering adds minimal overhead",
        related = ["search", "query_metadata", "find_notes_from_template"],
        examples = ["search 'project' tags:['work', 'active']", "find notes tagged 'important'", "query with metadata filters"]
    )]
    async fn advanced_search(
        &self,
        query: String,
        tags: Option<Vec<String>>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = SearchEngine::new(manager).await.map_err(to_mcp_error)?;

        let search_query = if let Some(tags) = tags {
            SearchQuery::new(query).with_tags(tags).limit(10)
        } else {
            SearchQuery::new(query).limit(10)
        };

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
        let engine = SearchEngine::new(manager).await.map_err(to_mcp_error)?;
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

        serde_json::to_value(response)
            .map_err(|e| McpError::internal(format!("Failed to serialize response: {}", e)))
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

        serde_json::to_value(response)
            .map_err(|e| McpError::internal(format!("Failed to serialize response: {}", e)))
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

        let response = StandardResponse::new(
            name.clone(),
            "remove_vault",
            serde_json::json!({"status": "removed"}),
        )
        .with_next_step("list_vaults");

        // CACHE PERSISTENCE: Save updated vault state to cache
        if let Err(e) = self.persist_vault_state().await {
            log::warn!("Failed to persist vault state after removal to cache: {}", e);
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

        serde_json::to_value(response)
            .map_err(|e| McpError::internal(format!("Failed to serialize response: {}", e)))
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

        serde_json::to_value(response)
            .map_err(|e| McpError::internal(format!("Failed to serialize response: {}", e)))
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
    async fn batch_execute(
        &self,
        operations: Vec<serde_json::Value>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;

        // Parse operations from JSON
        let mut ops = Vec::new();
        for op_json in operations {
            match serde_json::from_value::<BatchOperation>(op_json) {
                Ok(op) => ops.push(op),
                Err(e) => {
                    return Err(McpError::internal(format!(
                        "Invalid batch operation: {}",
                        e
                    )));
                }
            }
        }

        if ops.is_empty() {
            return Err(McpError::internal(
                "Batch operations list cannot be empty".to_string(),
            ));
        }

        let op_count = ops.len();
        let tools = BatchTools::new(manager);
        let result = tools.batch_execute(ops).await.map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "batch_execute",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(op_count)
        .with_meta("transactional", serde_json::json!(true))
        .with_next_step("quick_health_check");

        serde_json::to_value(response)
            .map_err(|e| McpError::internal(format!("Failed to serialize batch result: {}", e)))
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

        serde_json::to_value(response)
            .map_err(|e| McpError::internal(format!("Failed to serialize suggestions: {}", e)))
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

        serde_json::to_value(response)
            .map_err(|e| McpError::internal(format!("Failed to serialize ranking: {}", e)))
    }

    // ==================== Resources (OFM Knowledge Injection) ====================

    /// Complete Obsidian Flavored Markdown syntax guide
    #[resource("obsidian://syntax/complete-guide")]
    async fn ofm_complete_guide_resource(&self) -> McpResult<String> {
        Ok(include_str!("../resources/obsidian_flavored_markdown_system_prompt.md").to_string())
    }

    /// Quick reference for Obsidian Flavored Markdown
    #[resource("obsidian://syntax/quick-ref")]
    async fn ofm_quick_reference_resource(&self) -> McpResult<String> {
        Ok(include_str!("../resources/ofm_quick_reference.md").to_string())
    }

    /// Example note demonstrating all OFM features
    #[resource("obsidian://examples/sample-note")]
    async fn ofm_example_note_resource(&self) -> McpResult<String> {
        Ok(include_str!("../resources/ofm_example_note.md").to_string())
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
        let guide =
            include_str!("../resources/obsidian_flavored_markdown_system_prompt.md").to_string();

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
        let quick_ref = include_str!("../resources/ofm_quick_reference.md").to_string();

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
        let examples = include_str!("../resources/ofm_example_note.md").to_string();

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
}
