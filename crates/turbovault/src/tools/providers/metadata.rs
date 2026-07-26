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

#[turbomcp::server(name = "obsidian-vault", version = "1.6.0")]
impl MetadataProvider {
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
        ],
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
        usage = "Use to read properties without parsing full note content. Faster than read_note when you only need metadata.",
        performance = "Very fast (<10ms typical), only parses frontmatter section.",
        related = ["query_metadata", "read_note"],
        examples = [
            "key: author",
            "key: tags",
            "key: author.name",
            "key: metadata.priority",
            "key: custom.nested.field"
        ],
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
        usage = "Use to modify note metadata (status, tags, properties) while preserving content. Merge mode (default) deep-merges new keys into existing frontmatter. Replace mode replaces frontmatter entirely",
        performance = "Fast (<30ms typical). Reads file, modifies frontmatter, writes atomically",
        related = ["get_metadata_value", "query_metadata", "manage_tags"],
        examples = [
            r#"frontmatter: {"status": "published", "priority": 1}, merge: true"#,
            r#"frontmatter: {"tags": ["work", "urgent"]}, merge: false"#
        ],
        tags = ["write", "frontmatter"],
        destructive = true,
    )]
    async fn update_frontmatter(
        &self,
        path: String,
        frontmatter: HashMap<String, serde_json::Value>,
        merge: Option<bool>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);
        let fm_map: serde_json::Map<String, serde_json::Value> = frontmatter.into_iter().collect();
        let message = self
            .resolve_commit_message(commit_message, || format!("update_frontmatter {path}"))
            .await?;
        let result = tools
            .update_frontmatter(&path, fm_map, merge.unwrap_or(true), &message)
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
        ],
        tags = ["write", "frontmatter"],
        destructive = true,
    )]
    async fn manage_tags(
        &self,
        path: String,
        operation: String,
        tags: Option<Vec<String>>,
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
                    turbovault_core::Precondition::for_in_place(None),
                    &message,
                )
                .await
                .map_err(to_mcp_error)?;
        }

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
        ],
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
        usage = "Use for non-markdown files (images, PDFs, attachments). For markdown notes, use move_note instead (which updates link graph). Requires confirm_from and confirm_to matching from/to exactly",
        performance = "Fast (<20ms typical). Atomic rename, falls back to copy+delete for cross-filesystem moves",
        related = ["move_note", "delete_note"],
        examples = [
            "from: attachments/old.png, to: images/new.png, confirm_from: attachments/old.png, confirm_to: images/new.png"
        ],
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
            .move_file_with_hash(&from, &to, expected_hash.as_deref(), &message)
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
}
