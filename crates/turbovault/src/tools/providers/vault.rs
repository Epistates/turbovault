//! VaultProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct VaultProvider(CoreToolHandler);

impl VaultProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for VaultProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[turbomcp::server(name = "obsidian-vault", version = "1.5.0")]
impl VaultProvider {
    // ==================== Vault Lifecycle (Multi-Vault Management) ====================

    /// Create a new Obsidian vault
    #[tool(
        description = "Create and register a new Obsidian vault at the specified filesystem path with an optional template",
        usage = "Use for programmatic vault creation. The new vault is registered immediately; use set_active_vault if another vault is currently active",
        performance = "Fast (<50ms), creates .obsidian directory and config files",
        related = ["set_active_vault", "list_vaults"],
        examples = ["template: default", "template: research", "template: team"],
        tags = ["write", "admin"],
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
        .with_next_step("set_active_vault")
        .with_next_step("list_vaults");

        response.to_json()
    }

    /// Add an existing vault (automatically initializes it for better DX)
    #[tool(
        description = "Register an existing Obsidian vault with the MCP server and auto-initialize",
        usage = "Use as first step when working with existing vaults. Idempotent and safe to call multiple times",
        performance = "Depends on vault size: 100ms for small vaults, 1-5s for large (1000+ files) due to initialization",
        related = ["list_vaults", "set_active_vault", "get_vault_context"],
        examples = ["Add personal vault", "Register work vault", "Connect to shared knowledge base"],
        tags = ["write", "admin"],
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

        let mut manager = VaultManager::new(server_config)
            .map_err(|e| McpError::internal(format!("Failed to create vault manager: {}", e)))?;

        self.initialize_audit_for_manager(&name, &mut manager).await;

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
        examples = ["Remove temporary vault", "Cleanup after migration", "Close vault for maintenance"],
        tags = ["write", "admin"],
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
        examples = ["Show all vaults", "Check available options", "Verify vault registration"],
        tags = ["read", "admin"],
        read_only = true,
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
        examples = ["Check vault path", "Verify search settings", "Inspect custom config"],
        tags = ["read", "admin"],
        read_only = true,
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
        examples = ["Switch to personal vault", "Activate work vault", "Change vault context"],
        tags = ["write", "admin"],
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
        examples = ["Check current vault", "Verify context", "Confirm active vault"],
        tags = ["read", "admin"],
        read_only = true,
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
}
