//! MetadataProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct MetadataProvider(CoreToolHandler);

impl MetadataProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for MetadataProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[turbomcp::server(name = "obsidian-vault", version = "2.0.0")]
impl MetadataProvider {
    // ==================== Metadata Operations ====================

    /// Query files by metadata pattern
    #[tool(
        description = "Query notes by frontmatter metadata pattern (equality, comparison, existence checks)",
        tags = ["read", "frontmatter"],
        read_only = true,
    )]
    async fn query_metadata(&self, pattern: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);
        let results = tools.query_metadata(&pattern).await.map_err(to_mcp_error)?;

        let result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let count = result_data["files"].as_array().map_or(0, Vec::len);

        let response = StandardResponse::new(vault_name, "query_metadata", result_data)
            .with_count(count)
            .with_meta("pattern", serde_json::json!(pattern));

        response.to_json()
    }

    /// Get metadata value from a file
    #[tool(
        description = "Extract specific metadata value from a note's frontmatter (supports dot notation for nested keys)",
        tags = ["read", "frontmatter"],
        read_only = true,
    )]
    async fn get_metadata_value(&self, file: String, key: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);
        let value = tools
            .get_metadata_value(&file, &key)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(vault_name, "get_metadata_value", value)
            .with_next_step("query_metadata");

        response.to_json()
    }

    /// Update frontmatter of a note without touching content
    #[tool(
        description = "Update YAML frontmatter of a note without modifying content body",
        tags = ["write", "frontmatter"],
        destructive = true,
    )]
    async fn update_frontmatter(
        &self,
        path: String,
        frontmatter: HashMap<String, serde_json::Value>,
        merge: Option<bool>,
        expected_hash: Option<String>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);
        let fm_map: serde_json::Map<String, serde_json::Value> = frontmatter.into_iter().collect();
        let message = self
            .resolve_commit_message(commit_message, || format!("update_frontmatter {path}"))
            .await?;
        let result = tools
            .update_frontmatter(
                &path,
                fm_map,
                merge.unwrap_or(true),
                // Sentinel-or-oid `expected_hash` (qae.6.4); omitted → the
                // in-place default `ExpectExists`.
                turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                &message,
            )
            .await
            .map_err(to_mcp_error)?;

        self.after_write_one(
            &vault_name,
            VaultChange::Modified { path: path.clone() },
            WriteAttribution::host("update_frontmatter"),
        )
        .await;
        StandardResponse::new(vault_name, "update_frontmatter", result)
            .with_next_steps(&["read_note", "query_metadata"])
            .to_json()
    }

    /// Manage tags on a note (add, remove, list)
    #[tool(
        description = "Add, remove, or list tags on a note. List returns both frontmatter and inline #tags. Add/remove only modify frontmatter tags array",
        tags = ["write", "frontmatter"],
        destructive = true,
    )]
    async fn manage_tags(
        &self,
        path: String,
        operation: String,
        tags: Option<Vec<String>>,
        expected_hash: Option<String>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager.clone());

        // Compute first so the commit-message gate (and its fallback) is only
        // applied when the operation actually writes — `list` and a no-op
        // `remove` on a note without frontmatter are read-only (None), and a
        // read must never demand a commit_message on a require-message vault.
        let (maybe_content, result) = tools
            .compute_manage_tags(&path, &operation, tags.as_deref())
            .await
            .map_err(to_mcp_error)?;

        // `list` is a read; only add/remove produce content to store, so only
        // those report a change.
        let mutated = maybe_content.is_some();
        if let Some(content) = maybe_content {
            let message = self
                .resolve_commit_message(commit_message, || {
                    format!("manage_tags {operation} {path}")
                })
                .await?;
            manager
                .write_file(
                    std::path::Path::new(&path),
                    &content,
                    // Sentinel-or-oid `expected_hash` (qae.6.4); omitted →
                    // in-place default `ExpectExists`.
                    turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                    &message,
                )
                .await
                .map_err(to_mcp_error)?;
        }

        if mutated {
            self.after_write_one(
                &vault_name,
                VaultChange::Modified { path: path.clone() },
                WriteAttribution::host("manage_tags"),
            )
            .await;
        }
        StandardResponse::new(vault_name, "manage_tags", result)
            .with_next_steps(&["update_frontmatter", "query_metadata"])
            .to_json()
    }

    /// Get lightweight metadata for multiple files without reading content
    #[tool(
        description = "Get file metadata (size, modified time, has_frontmatter) for multiple notes without reading full content",
        tags = ["read"],
        read_only = true,
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
        tags = ["write"],
        destructive = true,
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
        let manager = self.get_active_vault_manager().await?;
        let message = self
            .resolve_commit_message(commit_message, || format!("move_file {from} -> {to}"))
            .await?;
        FileTools::new(manager)
            .move_file(
                &from,
                &to,
                turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                turbovault_core::Precondition::Blind,
                &message,
            )
            .await
            .map_err(to_mcp_error)?;

        self.after_write_one(
            &vault_name,
            VaultChange::Renamed {
                from: from.clone(),
                to: to.clone(),
            },
            WriteAttribution::host("move_file"),
        )
        .await;
        StandardResponse::new(
            vault_name,
            "move_file",
            serde_json::json!({"from": from, "to": to, "status": "moved"}),
        )
        .to_json()
    }
}
