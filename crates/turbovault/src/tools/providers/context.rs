//! ContextProvider MCP capabilities.

use std::ops::Deref;

use turbovault_tools::HealthInfo;

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

/// Response payload for `get_vault_context`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct VaultContextPayload {
    active_vault: String,
    all_vaults: Vec<VaultSummary>,
    /// The active vault's link health; `null` with no active vault.
    current_stats: Option<HealthInfo>,
    /// How to read the vault when it is an OKF bundle; otherwise `null`.
    okf: Option<OkfContext>,
    ready: bool,
    /// The markdown the vault is written in. Omitted when concise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    markdown_dialect: Option<MarkdownDialect>,
    /// Tool names grouped by task. Omitted when concise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tools: Option<ToolGroups>,
}

/// A registered vault, as `get_vault_context` lists it.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct VaultSummary {
    name: String,
    path: std::path::PathBuf,
    is_default: bool,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct MarkdownDialect {
    name: &'static str,
    base: [&'static str; 2],
    resources: OfmPointers,
    tools: OfmPointers,
    note: &'static str,
    key_features: [&'static str; 7],
    important_notes: [&'static str; 4],
}

/// Where the OFM guide, quick reference and examples are.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct OfmPointers {
    complete_guide: &'static str,
    quick_ref: &'static str,
    examples: &'static str,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct ToolGroups {
    file_operations: [&'static str; 7],
    search: [&'static str; 4],
    link_analysis: [&'static str; 5],
    analysis: [&'static str; 4],
    vault_management: [&'static str; 4],
    templates: [&'static str; 4],
    metadata: [&'static str; 4],
    batch: [&'static str; 1],
}

const MARKDOWN_DIALECT: MarkdownDialect = MarkdownDialect {
    name: "Obsidian Flavored Markdown (OFM)",
    base: ["CommonMark", "GitHub Flavored Markdown"],
    resources: OfmPointers {
        complete_guide: "obsidian://syntax/complete-guide",
        quick_ref: "obsidian://syntax/quick-ref",
        examples: "obsidian://examples/sample-note",
    },
    tools: OfmPointers {
        complete_guide: "get_ofm_syntax_guide",
        quick_ref: "get_ofm_quick_ref",
        examples: "get_ofm_examples",
    },
    note: "Use MCP resources if supported by client, otherwise use tools as fallback",
    key_features: [
        "Wikilinks: [[note]] and [[note|alias]]",
        "Embeds: ![[image.png]] and ![[note#section]]",
        "Block refs: [[note#^block-id]] and ^block-id",
        "Callouts: > [!note] Title",
        "Tags: #tag and #nested/tag",
        "Task lists: - [ ], - [x], - [/], and - [-]",
        "Tables and strikethrough",
    ],
    important_notes: [
        "Use wikilinks [[note]] for internal references, not markdown links",
        "Highlights, comments, and math/LaTeX are Obsidian syntax but are not yet extracted as first-class parser nodes",
        "No markdown formatting inside HTML tags",
        "Block IDs should be unique within a note",
    ],
};

const TOOL_GROUPS: ToolGroups = ToolGroups {
    file_operations: [
        "read_note",
        "write_note",
        "edit_note",
        "delete_note",
        "move_note",
        "move_file",
        "get_notes_info",
    ],
    search: [
        "search",
        "advanced_search",
        "recommend_related",
        "find_notes_from_template",
    ],
    link_analysis: [
        "get_backlinks",
        "get_forward_links",
        "get_related_notes",
        "get_hub_notes",
        "get_dead_end_notes",
    ],
    analysis: [
        "quick_health_check",
        "full_health_analysis",
        "get_broken_links",
        "detect_cycles",
    ],
    vault_management: [
        "add_vault",
        "list_vaults",
        "set_active_vault",
        "get_active_vault",
    ],
    templates: [
        "list_templates",
        "get_template",
        "create_from_template",
        "find_notes_from_template",
    ],
    metadata: [
        "get_metadata_value",
        "query_metadata",
        "update_frontmatter",
        "manage_tags",
    ],
    batch: ["batch_execute"],
};

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl ContextProvider {
    // ==================== Vault Context (LLM Discovery) ====================

    /// Get comprehensive vault context in a single call (LLMX: replaces 4+ separate calls)
    #[tool(
        description = "Start here. In one call: the active vault and every registered vault, the active vault's note and link counts and health score, the markdown dialect it uses, and the tools grouped by task. If no vault is active, it says how to add one.",
        title = "Get Vault Context",
        tags = ["read"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<VaultContextPayload>,
    )]
    async fn get_vault_context(
        &self,
        #[description(
            "`detailed` (default) or `concise`. Concise leaves out `markdown_dialect` and `tools`, which are the same on every call; ask for them once."
        )]
        response_format: Option<ResponseFormat>,
    ) -> McpResult<serde_json::Value> {
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
            (Some(health), okf_context_block(&bundle))
        } else {
            (None, None)
        };

        let detailed = !ResponseFormat::is_concise(response_format);
        let context = VaultContextPayload {
            ready: !active_vault.is_empty(),
            active_vault: active_vault.clone(),
            all_vaults: vaults
                .iter()
                .map(|v| VaultSummary {
                    name: v.name.clone(),
                    path: v.path.clone(),
                    is_default: v.is_default,
                })
                .collect(),
            current_stats,
            okf: okf_block,
            markdown_dialect: detailed.then_some(MARKDOWN_DIALECT),
            tools: detailed.then_some(TOOL_GROUPS),
        };

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
