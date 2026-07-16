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

#[turbomcp::server(name = "obsidian-vault", version = "1.5.0")]
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
        examples = ["path: drafts/old-idea.md, confirm_path: drafts/old-idea.md"],
        tags = ["write", "delete"],
        destructive = true,
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
        examples = [],
        tags = ["write"],
        destructive = true,
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
}
