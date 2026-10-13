//! VaultProvider MCP capabilities.

use std::collections::HashMap;
use std::ops::Deref;

use turbovault_core::VaultInfo;
use turbovault_core::config::{VaultGitConfig, WriteBackend};

use super::super::*;

/// turbovault-kdq: resolve the optional `write_backend` + `backend_opts`
/// registration arguments into the typed pair the builder wants. Omitting
/// `write_backend` yields `Direct` — the pre-kdq behaviour, unchanged.
///
/// Whether the pair is *coherent* is not decided here: `VaultLifecycleTools`
/// is the funnel every registration passes through (the CLI shorthand
/// included), so the direct-plus-options refusal lives there.
fn parse_backend_selection(
    write_backend: Option<String>,
    backend_opts: Option<HashMap<String, serde_json::Value>>,
) -> McpResult<(WriteBackend, Option<VaultGitConfig>)> {
    let backend = match write_backend {
        Some(raw) => raw.parse::<WriteBackend>().map_err(to_mcp_error)?,
        None => WriteBackend::default(),
    };
    let backend_opts = backend_opts
        .map(|settings| {
            serde_json::from_value::<VaultGitConfig>(serde_json::Value::Object(
                settings.into_iter().collect(),
            ))
        })
        .transpose()
        .map_err(|error| McpError::invalid_params(format!("Invalid backend_opts: {error}")))?;
    Ok((backend, backend_opts))
}

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

/// Response payload for `get_active_vault`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct ActiveVaultPayload {
    active_vault: String,
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl VaultProvider {
    // ==================== Vault Lifecycle (Multi-Vault Management) ====================

    /// Create a new Obsidian vault
    // The `backend_opts` schema is a free-form object with no field names, so
    // this description is the only place a calling agent can discover the keys
    // it accepts. Spell them out here or they are undiscoverable.
    #[tool(
        description = "Create a new vault folder, optionally from a starter template, and register it. Optional write_backend selects the write path: 'direct' (default) or 'git'. Optional backend_opts holds that backend's settings; 'direct' has none, 'git' accepts branch, author {name, email}, merge_strategy ('merge-commit' or 'fast-forward'), include_ignored, require_commit_message. Passing backend_opts with write_backend 'direct' is an error. The server may only allow vaults inside certain folders (allowed_vault_roots).",
        title = "Create Vault",
        tags = ["write", "admin"],
        read_only = false,
        destructive = false,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<VaultInfo>,
    )]
    async fn create_vault(
        &self,
        #[description(
            "A short name to refer to the vault by, without spaces, at most 64 characters, such as `research`."
        )]
        name: String,
        #[description(
            "Absolute path of the folder to create, such as `/Users/me/Documents/Research`. `~` is expanded."
        )]
        path: String,
        #[description("Starter layout: `default`, `research` or `team`. Omit for an empty vault.")]
        template: Option<String>,
        #[description(
            "How writes land: `direct` (default) writes files; `git` commits each change."
        )]
        write_backend: Option<String>,
        #[description(
            "Settings for the `git` backend: `branch`, `author` {name, email}, `merge_strategy` (`merge-commit` or `fast-forward`), `include_ignored`, `require_commit_message`."
        )]
        backend_opts: Option<HashMap<String, serde_json::Value>>,
    ) -> McpResult<serde_json::Value> {
        let (write_backend, backend_opts) = parse_backend_selection(write_backend, backend_opts)?;
        let tools = self.lifecycle_tools();
        let vault_info = tools
            .create_vault(
                &name,
                Path::new(&path),
                template.as_deref(),
                write_backend,
                backend_opts,
            )
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(name.clone(), "create_vault", vault_info)
            .with_next_step("set_active_vault")
            .with_next_step("list_vaults");

        response.to_json()
    }

    /// Add an existing vault (automatically initializes it for better DX)
    #[tool(
        description = "Register an existing vault folder with the server and index it so it's ready to use. Optional write_backend selects the write path: 'direct' (default) or 'git'. Optional backend_opts holds that backend's settings; 'direct' has none, 'git' accepts branch, author {name, email}, merge_strategy ('merge-commit' or 'fast-forward'), include_ignored, require_commit_message. Passing backend_opts with write_backend 'direct' is an error. The server may only allow vaults inside certain folders (allowed_vault_roots).",
        title = "Add Vault",
        tags = ["write", "admin"],
        read_only = false,
        destructive = false,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<VaultInfo>,
    )]
    async fn add_vault(
        &self,
        #[description(
            "A short name to refer to the vault by, without spaces, at most 64 characters, such as `notes`."
        )]
        name: String,
        #[description(
            "Absolute path of the existing vault folder, such as `/Users/me/Documents/Notes`. `~` is expanded."
        )]
        path: String,
        #[description(
            "How writes land: `direct` (default) writes files; `git` commits each change."
        )]
        write_backend: Option<String>,
        #[description(
            "Settings for the `git` backend: `branch`, `author` {name, email}, `merge_strategy` (`merge-commit` or `fast-forward`), `include_ignored`, `require_commit_message`."
        )]
        backend_opts: Option<HashMap<String, serde_json::Value>>,
    ) -> McpResult<serde_json::Value> {
        let (write_backend, backend_opts) = parse_backend_selection(write_backend, backend_opts)?;
        let tools = self.lifecycle_tools();
        let vault_info = tools
            .add_vault_from_path(&name, Path::new(&path), write_backend, backend_opts)
            .await
            .map_err(to_mcp_error)?;

        // Auto-initialize the vault so it's ready to use immediately
        // This provides better DX - users don't need a separate initialize() call
        log::info!(
            "Automatically initializing vault '{}' for immediate use",
            name
        );

        // Build, wire and publish through the single activation path. Wiring
        // matters most here: `add_vault` is the primary runtime entry point and
        // `get_active_vault_manager` always cache-hits once a manager is
        // published — so an unwired manager published here would mean the
        // drainer / HEAD-ref listener never start and search-staleness never
        // closes for any git vault added at runtime (bite 3a, qae.5.3).
        self.activate_vault_manager(&name).await?;

        log::info!("Vault '{}' initialized and ready", name);

        let response = StandardResponse::new(name.clone(), "add_vault", vault_info)
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
        description = "Unregister a vault from the server. Its files are left untouched.",
        title = "Remove Vault",
        tags = ["write", "admin"],
        read_only = false,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<StatusPayload>,
    )]
    async fn remove_vault(
        &self,
        #[description("Name of the vault to unregister, as list_vaults shows it.")] name: String,
    ) -> McpResult<serde_json::Value> {
        self.remove_vault_with_cleanup(&name).await
    }

    /// List all registered vaults
    #[tool(
        description = "List the vaults registered with the server: each one's name, path, write backend and whether it's the active vault.",
        title = "List Vaults",
        tags = ["read", "admin"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<Vec<VaultInfo>>,
    )]
    async fn list_vaults(&self) -> McpResult<serde_json::Value> {
        let tools = self.lifecycle_tools();
        let vaults = tools.list_vaults().await.map_err(to_mcp_error)?;

        let count = vaults.len();
        let response = StandardResponse::new(
            String::new(), // No active vault for this operation
            "list_vaults",
            vaults,
        )
        .with_count(count);

        response.to_json()
    }

    /// Get configuration for a specific vault
    #[tool(
        description = "Show one vault's full configuration: path, write backend and its settings, excluded paths and limits.",
        title = "Get Vault Config",
        tags = ["read", "admin"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<VaultConfig>,
    )]
    async fn get_vault_config(
        &self,
        #[description("Name of the vault, as list_vaults shows it.")] name: String,
    ) -> McpResult<serde_json::Value> {
        let tools = self.lifecycle_tools();
        let config = tools.get_vault_config(&name).await.map_err(to_mcp_error)?;

        let response = StandardResponse::new(name.clone(), "get_vault_config", config)
            .with_next_step("set_active_vault");

        response.to_json()
    }

    /// Set the active vault
    #[tool(
        description = "Make a registered vault the active one. Every note, search and graph tool works on the active vault.",
        title = "Set Active Vault",
        tags = ["write", "admin"],
        read_only = false,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<StatusPayload>,
    )]
    async fn set_active_vault(
        &self,
        #[description("Name of the vault to make active, as list_vaults shows it.")] name: String,
    ) -> McpResult<serde_json::Value> {
        let tools = self.lifecycle_tools();
        tools.set_active_vault(&name).await.map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            name.clone(),
            "set_active_vault",
            StatusPayload {
                status: "activated",
            },
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
        description = "Name the active vault, the one the note, search and graph tools work on.",
        title = "Get Active Vault",
        tags = ["read", "admin"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<ActiveVaultPayload>,
    )]
    async fn get_active_vault(&self) -> McpResult<serde_json::Value> {
        let tools = self.lifecycle_tools();
        let active = tools.get_active_vault().await.map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            active.clone(),
            "get_active_vault",
            ActiveVaultPayload {
                active_vault: active,
            },
        )
        .with_next_step("get_vault_context");

        response.to_json()
    }
}
