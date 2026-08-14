//! FileProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct FileProvider(CoreToolHandler);

impl FileProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for FileProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[turbomcp::server(name = "obsidian-vault", version = "1.6.0")]
impl FileProvider {
    // ==================== File Operations ====================

    /// Read the contents of a note
    #[tool(
        description = "Read complete markdown content of a note from active vault",
        usage = "Use before editing, analyzing, or displaying notes. Supports all Obsidian Flavored Markdown syntax including wikilinks [[note]], embeds ![[image.png]], and block references ^block-id",
        performance = "Fast (<10ms typical). Returns path, content, and content hash for conflict detection",
        related = ["write_note", "edit_note", "get_backlinks"],
        examples = ["daily/2024-01-15.md", "projects/website-redesign.md"],
        tags = ["read"],
        read_only = true,
    )]
    async fn read_note(&self, path: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = FileTools::new(manager);
        let content = tools.read_file(&path).await.map_err(to_mcp_error)?;

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
        description = "Write a note with overwrite/append/prepend mode and optimistic concurrency. Existing overwrite targets require expected_hash (or expected_hash=\"blind\" for an intentional blind overwrite). Git-backed vaults commit the mutation atomically.",
        usage = "Read existing notes first and pass the returned expected_hash. expected_hash accepts a hash or a sentinel (\"blind\"|\"absent\"|\"exists\"); use \"blind\" only for an intentional blind overwrite. commit_message controls the Git commit subject when using write_backend=git.",
        performance = "Moderate (<50ms typical). Includes filesystem write and link graph update",
        related = ["read_note", "edit_note", "create_from_template"],
        examples = ["mode: overwrite (default)", "mode: append (add to end)", "mode: prepend (add after frontmatter)", "expected_hash: <hash from read_note>"],
        tags = ["write"],
        destructive = true,
    )]
    async fn write_note(
        &self,
        path: String,
        content: String,
        mode: Option<String>,
        expected_hash: Option<String>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let write_mode = WriteMode::from_str_opt(mode.as_deref()).map_err(to_mcp_error)?;
        let prepared = self
            .prepare_complete_note_write(&path, commit_message, "write_note")
            .await?;
        let vault_name = prepared.vault_name;
        let manager = prepared.manager;
        let message = prepared.message;
        let files = FileTools::new(manager.clone());

        // Sentinel-or-oid `expected_hash` (qae.6.4), replacing the retired
        // `force`. The default when the param is omitted is mode-dependent: a full
        // Overwrite creates-by-default (`ExpectAbsent`, the turbovault-947
        // no-clobber backstop on both substrates); append/prepend are in-place
        // (`ExpectExists`). `expected_hash="blind"` is the explicit blind overwrite.
        let default = match write_mode {
            WriteMode::Overwrite => turbovault_core::Precondition::ExpectAbsent,
            _ => turbovault_core::Precondition::ExpectExists,
        };
        let precondition =
            turbovault_core::Precondition::from_wire(expected_hash.as_deref(), default);

        // Friendly pre-check for the create-by-default no-clobber case. A
        // create-on-present is the unified "changed underneath us" refusal
        // (`ConcurrencyError`), same kind as the substrate's `ExpectAbsent`
        // backstop — surfaced here with an actionable message.
        if precondition == turbovault_core::Precondition::ExpectAbsent
            && write_mode == WriteMode::Overwrite
            && tokio::fs::try_exists(manager.vault_path().join(&path))
                .await
                .unwrap_or(false)
        {
            return Err(to_mcp_error(turbovault_core::Error::concurrency_error(
                format!(
                    "'{path}' exists — pass expected_hash to overwrite, or expected_hash=\"blind\" for a blind overwrite."
                ),
            )));
        }
        files
            .write_file_with_mode(&path, &content, write_mode, precondition, &message)
            .await
            .map_err(to_mcp_error)?;

        self.finish_complete_note_write().await;
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
        examples = [],
        tags = ["write"],
        destructive = true,
    )]
    async fn edit_note(
        &self,
        path: String,
        edits: String,
        expected_hash: Option<String>,
        dry_run: Option<bool>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let dry_run = dry_run.unwrap_or(false);
        let message = self
            .resolve_commit_message(commit_message, || format!("edit_note {path}"))
            .await?;
        let result = FileTools::new(manager)
            .edit_file(
                &path,
                &edits,
                turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                dry_run,
                &message,
            )
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
        description = "Delete a note with confirmation, concurrency protection, and backlink safety. By default refuses when inbound links exist; use on_backlinks='rewrite-stale-callout' for an atomic delete+rewrite, or force=true to leave broken links.",
        usage = "confirm_path must exactly match path. Pass expected_hash from read_note. Git-backed rewrite mode updates every linker in the same atomic commit.",
        performance = "Fast (<20ms typical). Includes filesystem delete and link graph update",
        related = ["get_backlinks", "get_broken_links", "move_note"],
        examples = ["path: drafts/old-idea.md, confirm_path: drafts/old-idea.md"],
        tags = ["write", "delete"],
        destructive = true,
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
        let manager = self.get_active_vault_manager().await?;
        let is_git = self.active_vault_is_git().await?;
        let message = self
            .resolve_commit_message(commit_message, || format!("delete_note {path}"))
            .await?;
        let force = force.unwrap_or(!is_git);
        let files = FileTools::new(manager.clone());
        let backlinks = if force {
            Vec::new()
        } else {
            inbound_backlinks(&manager, &path).await?
        };

        let updated_sources = if backlinks.is_empty() || force {
            files
                .delete_file(
                    &path,
                    turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                    &message,
                )
                .await
                .map_err(to_mcp_error)?;
            Vec::new()
        } else if on_backlinks.as_deref() == Some("rewrite-stale-callout") {
            BatchTools::new(manager)
                .delete_file_with_link_rewrite_to_stale(&path, expected_hash.as_deref(), &message)
                .await
                .map_err(to_mcp_error)?
        } else {
            return Err(McpError::invalid_request(format!(
                "delete_note refused: '{path}' has {} inbound backlink(s): {}. Pass on_backlinks='rewrite-stale-callout' or force=true.",
                backlinks.len(),
                backlinks
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        StandardResponse::new(
            vault_name,
            "delete_note",
            serde_json::json!({"path": path, "status": "deleted", "link_sources_updated": updated_sources}),
        )
        .with_next_step("quick_health_check")
        .to_json()
    }

    /// Move or rename a note
    #[tool(
        description = "Move or rename a note. `expected_hash` guards the SOURCE, `dest_expected_hash` the DESTINATION (both accept a hash or a sentinel: \"absent\"|\"exists\"|\"blind\"; dest omitted = \"absent\", the no-clobber guard). Git-backed vaults update inbound wikilinks atomically by default; set update_backlinks=false for a rename-only operation.",
        usage = "Pass expected_hash from read_note. dest_expected_hash defaults to \"absent\" (refuse if the destination exists); pass \"blind\" to overwrite. With update_backlinks=true, a concurrent change to the source or any linker aborts the entire move with nothing committed.",
        performance = "Fast (<20ms typical). Filesystem rename, falls back to copy+delete for cross-filesystem moves",
        related = ["get_backlinks", "get_forward_links", "search"],
        examples = [],
        tags = ["write"],
        destructive = true,
    )]
    async fn move_note(
        &self,
        from: String,
        to: String,
        expected_hash: Option<String>,
        dest_expected_hash: Option<String>,
        commit_message: Option<String>,
        update_backlinks: Option<bool>,
    ) -> McpResult<serde_json::Value> {
        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;
        let is_git = self.active_vault_is_git().await?;
        let message = self
            .resolve_commit_message(commit_message, || format!("move_note {from} -> {to}"))
            .await?;
        let update_backlinks = update_backlinks.unwrap_or(is_git);
        let updated_sources = if update_backlinks {
            // `move_file_with_link_updates` flushes the reindex queue itself so
            // the backlink resolution reads a coherent link graph.
            BatchTools::new(manager)
                .move_file_with_link_updates(
                    &from,
                    &to,
                    expected_hash.as_deref(),
                    dest_expected_hash.as_deref(),
                    &message,
                )
                .await
                .map_err(to_mcp_error)?
        } else {
            FileTools::new(manager)
                .move_file(
                    &from,
                    &to,
                    turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                    // Sentinel-or-oid dest precondition (qae.6.4); omitted →
                    // `ExpectAbsent`, the clobber guard.
                    turbovault_core::Precondition::from_wire(
                        dest_expected_hash.as_deref(),
                        turbovault_core::Precondition::ExpectAbsent,
                    ),
                    &message,
                )
                .await
                .map_err(to_mcp_error)?;
            Vec::new()
        };

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        let response = StandardResponse::new(
            vault_name,
            "move_note",
            serde_json::json!({"from": from, "to": to, "status": "moved", "link_sources_updated": updated_sources}),
        )
        .with_next_steps(&["get_backlinks", "get_forward_links"]);
        if update_backlinks {
            response.to_json()
        } else {
            response
                .with_warning(
                    "Links pointing to the old path were not updated and may now be broken.",
                )
                .to_json()
        }
    }
}

/// Vault-relative source paths whose wikilinks target `path` (inbound
/// backlinks), resolved through the manager's self-flushing link graph
/// (`get_backlinks`). Backs `delete_note`'s refuse-by-default listing — the
/// M4d replacement for the old `WriteTools::list_inbound_backlinks`.
async fn inbound_backlinks(manager: &VaultManager, path: &str) -> McpResult<Vec<String>> {
    let full = manager
        .get_backlinks(std::path::Path::new(path))
        .await
        .map_err(to_mcp_error)?;
    let vault_root = manager.vault_path();
    Ok(full
        .into_iter()
        .filter_map(|p| {
            p.strip_prefix(vault_root)
                .unwrap_or(&p)
                .to_str()
                .map(str::to_string)
        })
        .collect())
}
