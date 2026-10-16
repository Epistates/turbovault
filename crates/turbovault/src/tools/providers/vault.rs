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

/// What `list_vaults` returns.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct VaultList {
    /// The active vault's name, which tools use when a call names no vault.
    /// Absent when no vault is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_vault: Option<String>,
    vaults: Vec<VaultInfo>,
    /// The full configuration of the vault named in `name`, when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config: Option<VaultConfig>,
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl VaultProvider {
    // ==================== Vault Lifecycle (Multi-Vault Management) ====================

    /// Register a vault folder, creating it first if asked.
    // The `backend_opts` schema is a free-form object with no field names, so
    // this description is the only place a calling agent can discover the keys
    // it accepts. Spell them out here or they are undiscoverable.
    #[tool(
        description = "Register a vault folder with the server and index it so it's ready to use. With `create: true`, create the folder first, optionally from a starter `template`. Optional write_backend selects the write path: 'direct' (default) or 'git'. Optional backend_opts holds that backend's settings; 'direct' has none, 'git' accepts branch, author {name, email}, merge_strategy ('merge-commit' or 'fast-forward'), include_ignored, require_commit_message. Passing backend_opts with write_backend 'direct' is an error. The server may only allow vaults inside certain folders (allowed_vault_roots), and asks the user to confirm when the client supports it.",
        title = "Add Vault",
        tags = ["write", "admin"],
        read_only = false,
        destructive = false,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<VaultInfo>,
    )]
    // Each argument is one of the tool's MCP parameters; the macro reads the
    // input schema from this signature.
    #[allow(clippy::too_many_arguments)]
    async fn add_vault(
        &self,
        #[description(
            "A short name to refer to the vault by, without spaces, at most 64 characters, such as `notes`."
        )]
        name: String,
        #[description(
            "Absolute path of the vault folder, such as `/Users/me/Documents/Notes`. `~` is expanded."
        )]
        path: String,
        #[description(
            "Create the folder first; it must not exist yet. Default false: the folder must exist."
        )]
        create: Option<bool>,
        #[description(
            "With `create`: starter layout `default`, `research` or `team`. Omit for an empty vault."
        )]
        template: Option<String>,
        #[description(
            "How writes land: `direct` (default) writes files; `git` commits each change."
        )]
        write_backend: Option<String>,
        #[description(
            "Settings for the `git` backend: `branch`, `author` {name, email}, `merge_strategy` (`merge-commit` or `fast-forward`), `include_ignored`, `require_commit_message`."
        )]
        backend_opts: Option<HashMap<String, serde_json::Value>>,
        ctx: &RequestContext,
    ) -> McpResult<serde_json::Value> {
        let create = create.unwrap_or(false);
        if template.is_some() && !create {
            return Err(McpError::invalid_params(
                "template applies only when creating a vault; pass create: true".to_string(),
            ));
        }
        let (write_backend, backend_opts) = parse_backend_selection(write_backend, backend_opts)?;
        // Registering a folder gives the agent read and write access to it,
        // so the user is asked, as before a delete.
        let action = if create {
            format!(
                "Create the folder `{path}` and give the agent read and write access to it as the vault `{name}`?"
            )
        } else {
            format!(
                "Give the agent read and write access to the folder `{path}` as the vault `{name}`?"
            )
        };
        self.confirm_destructive(ctx, action).await?;
        let mut stages = Stages::new(ctx, 2);
        stages.next("Registering the vault").await?;
        let tools = self.lifecycle_tools();
        let vault_info = if create {
            tools
                .create_vault(
                    &name,
                    Path::new(&path),
                    template.as_deref(),
                    write_backend,
                    backend_opts,
                )
                .await
        } else {
            tools
                .add_vault_from_path(&name, Path::new(&path), write_backend, backend_opts)
                .await
        }
        .map_err(to_mcp_error)?;

        // Build, wire and publish through the single activation path, so the
        // vault is indexed and ready on return. Wiring matters most here:
        // `add_vault` is the primary runtime entry point and
        // `get_active_vault_manager` always cache-hits once a manager is
        // published, so an unwired manager published here would mean the
        // drainer / HEAD-ref listener never start and search-staleness never
        // closes for any git vault added at runtime (bite 3a, qae.5.3).
        // Registered now, so a cancellation no longer stops the call.
        stages.report("Scanning and indexing the notes").await;
        self.activate_vault_manager(&name).await?;
        stages.finish().await;
        tracing::info!("Vault '{}' registered and ready", name);

        // A vault registered at runtime survives a restart.
        if let Err(e) = self.persist_vault_state().await {
            tracing::warn!("Failed to persist vault state to cache: {}", e);
        }

        StandardResponse::new(name.clone(), "add_vault", vault_info)
            .with_next_step("set_active_vault")
            .with_next_step("list_vaults")
            .to_json()
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

    /// List the registered vaults
    #[tool(
        description = "List the vaults registered with the server: each one's name, path, write backend and whether it's the active vault, plus the active vault's name. Give `name` to also get that vault's full configuration (write backend settings, excluded paths and limits).",
        title = "List Vaults",
        tags = ["read", "admin"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<VaultList>,
    )]
    async fn list_vaults(
        &self,
        #[description(
            "Name of one vault to include the full configuration of. Omit for the list alone."
        )]
        name: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let tools = self.lifecycle_tools();
        let vaults = tools.list_vaults().await.map_err(to_mcp_error)?;
        let active_vault = tools
            .get_active_vault()
            .await
            .ok()
            .filter(|active| !active.is_empty());
        let config = match &name {
            Some(name) => Some(tools.get_vault_config(name).await.map_err(to_mcp_error)?),
            None => None,
        };

        let count = vaults.len();
        StandardResponse::new(
            name.unwrap_or_default(),
            "list_vaults",
            VaultList {
                active_vault,
                vaults,
                config,
            },
        )
        .with_count(count)
        .to_json()
    }

    /// Set the active vault
    #[tool(
        description = "Make a registered vault the active one: the vault every tool works on when a call names no `vault`.",
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
        .with_next_step("vault_health");

        // CACHE PERSISTENCE: Save active vault state to cache
        if let Err(e) = self.persist_vault_state().await {
            tracing::warn!("Failed to persist active vault state to cache: {}", e);
            // Not a fatal error - continue anyway
        }

        response.to_json()
    }
}
