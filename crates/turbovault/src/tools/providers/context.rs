//! ContextProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct ContextProvider(CoreToolHandler);

impl ContextProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for ContextProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[turbomcp::server(name = "obsidian-vault", version = "1.6.0")]
impl ContextProvider {
    // ==================== Vault Context (LLM Discovery) ====================

    /// Get comprehensive vault context in a single call (LLMX: replaces 4+ separate calls)
    #[tool(
        description = "Get complete vault context (vaults, stats, capabilities, markdown dialect) in a single discovery call",
        usage = "Use as first call after connecting to understand server state and capabilities. Essential for initial orientation",
        performance = "Fast (<10ms typical), no filesystem operations if no active vault",
        related = ["explain_vault", "list_vaults", "quick_health_check"],
        examples = ["Check available vaults", "Verify server readiness", "Get OFM syntax resources"],
        tags = ["read"],
        read_only = true,
    )]
    async fn get_vault_context(&self) -> McpResult<serde_json::Value> {
        let active_vault = self.multi_vault_mgr.get_active_vault().await;
        let vaults = self
            .multi_vault_mgr
            .list_vaults()
            .await
            .map_err(|e| McpError::internal(format!("Failed to list vaults: {}", e)))?;

        let (current_stats, okf_block) = if !active_vault.is_empty() {
            let manager = self.get_active_vault_manager().await?;
            let health = GraphTools::new(manager.clone())
                .quick_health_check()
                .await
                .map_err(|e| McpError::internal(e.to_string()))?;
            // OKF orientation: tell the agent whether to navigate via index.md
            // (progressive disclosure) rather than blind search.
            let bundle = OkfTools::new(manager).bundle_info().await;
            (Some(health), Some(okf_context_block(&bundle)))
        } else {
            (None, None)
        };

        let context = serde_json::json!({
            "active_vault": active_vault,
            "all_vaults": vaults.iter().map(|v| serde_json::json!({
                "name": v.name,
                "path": v.path,
                "is_default": v.is_default,
            })).collect::<Vec<_>>(),
            "current_stats": current_stats,
            "okf": okf_block,
            "ready": !active_vault.is_empty(),
            "markdown_dialect": {
                "name": "Obsidian Flavored Markdown (OFM)",
                "base": ["CommonMark", "GitHub Flavored Markdown"],
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
                    "Tags: #tag and #nested/tag",
                    "Task lists: - [ ], - [x], - [/], and - [-]",
                    "Tables and strikethrough"
                ],
                "important_notes": [
                    "Use wikilinks [[note]] for internal references, not markdown links",
                    "Highlights, comments, and math/LaTeX are Obsidian syntax but are not yet extracted as first-class parser nodes",
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
}
