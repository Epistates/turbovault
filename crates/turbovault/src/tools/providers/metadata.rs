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
        description = "Find notes whose frontmatter matches one simple condition, with each match's frontmatter. For several conditions, joins or sorting use query_frontmatter_sql.",
        title = "Query Metadata",
        tags = ["read", "frontmatter"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
    )]
    async fn query_metadata(
        &self,
        #[description(
            "One condition: `status: \"draft\"` (equals), `tags: contains(\"work\")` (text contains), `priority > 3` or `priority < 3` (numbers)."
        )]
        pattern: String,
        #[description("Most notes to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many matches to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);
        let results = tools.query_metadata(&pattern).await.map_err(to_mcp_error)?;

        let mut result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let page = request.slice_field(&mut result_data, "files", false);

        let response = StandardResponse::new(vault_name, "query_metadata", result_data)
            .with_page(page)
            .with_meta("pattern", serde_json::json!(pattern));

        response.to_json()
    }

    /// Get metadata value from a file
    #[tool(
        description = "Read one frontmatter value from a note. Use read_note to see all of a note's frontmatter.",
        title = "Get Metadata Value",
        tags = ["read", "frontmatter"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
    )]
    async fn get_metadata_value(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        file: String,
        #[description(
            "The frontmatter key. Use dots for nested keys, such as `author.name`."
        )]
        key: String,
    ) -> McpResult<serde_json::Value> {
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
        description = "Change a note's YAML frontmatter and leave its body untouched. By default the given keys are merged into the existing frontmatter; set merge to false to replace it entirely. For tags, manage_tags adds or removes single tags.",
        title = "Update Frontmatter",
        tags = ["write", "frontmatter"],
        read_only = false,
        destructive = true,
        idempotent = true,
        open_world = false,
    )]
    async fn update_frontmatter(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description(
            "The frontmatter keys and values to write, such as `{\"status\": \"done\", \"tags\": [\"work\"]}`. A key set to null is written as null."
        )]
        frontmatter: HashMap<String, serde_json::Value>,
        #[description(
            "True (default) to merge these keys into the existing frontmatter; false to replace the whole frontmatter with them."
        )]
        merge: Option<bool>,
        #[description(
            "The note's `hash` from your last read_note; the change is refused if the note changed since. Omit to change whatever is there now."
        )]
        expected_hash: Option<String>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the path."
        )]
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
        description = "List, add or remove a note's tags. `list` returns both its frontmatter tags and inline #tags; `add` and `remove` change only the frontmatter `tags` list, creating it if needed.",
        title = "Manage Tags",
        tags = ["write", "frontmatter"],
        read_only = false,
        destructive = true,
        idempotent = true,
        open_world = false,
    )]
    async fn manage_tags(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description("`list`, `add` or `remove`.")] operation: String,
        #[description(
            "Tags to add or remove, without `#`, such as `[\"work\", \"project/alpha\"]`. Not used by `list`."
        )]
        tags: Option<Vec<String>>,
        #[description(
            "The note's `hash` from your last read_note; an add or remove is refused if the note changed since. Omit to change whatever is there now."
        )]
        expected_hash: Option<String>,
        #[description(
            "Commit message for an add or remove on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived."
        )]
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
        description = "Check several notes at once without reading them: whether each exists, its size, when it was modified and whether it has frontmatter.",
        title = "Get Notes Info",
        tags = ["read"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
    )]
    async fn get_notes_info(
        &self,
        #[description(
            "Vault-relative paths of the notes, including extensions, such as `[\"projects/Plan.md\", \"Inbox.md\"]`."
        )]
        paths: Vec<String>,
    ) -> McpResult<serde_json::Value> {
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
        description = "Move or rename any file in the vault, such as an image or PDF, byte for byte. Links to it are not updated; to move a note and update the links to it, use move_note. A file already at the destination is replaced.",
        title = "Move File",
        tags = ["write"],
        read_only = false,
        destructive = true,
        idempotent = true,
        open_world = false,
    )]
    async fn move_file(
        &self,
        #[description(
            "Vault-relative path of the file to move, including the extension, such as `attachments/photo.png`."
        )]
        from: String,
        #[description("Vault-relative path to move it to, including the extension.")] to: String,
        #[description("The same path as `from`, repeated to confirm the move.")]
        confirm_from: String,
        #[description("The same path as `to`, repeated to confirm the move.")] confirm_to: String,
        #[description(
            "The file's hash from your last read; the move is refused if it changed since. Omit to move whatever is there now."
        )]
        expected_hash: Option<String>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the paths."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        // Safety: confirmations must match
        if from != confirm_from {
            return Err(McpError::invalid_params(format!(
                "Confirmation failed: confirm_from '{}' does not match from '{}'. Both must be identical.",
                confirm_from, from
            )));
        }
        if to != confirm_to {
            return Err(McpError::invalid_params(format!(
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
