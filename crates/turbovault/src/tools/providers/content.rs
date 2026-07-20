//! ContentProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct ContentProvider(CoreToolHandler);

impl ContentProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for ContentProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[turbomcp::server(name = "obsidian-vault", version = "1.6.0")]
impl ContentProvider {
    // ==================== Resources (OFM Knowledge Injection) ====================

    /// Complete Obsidian Flavored Markdown syntax guide
    #[resource("obsidian://syntax/complete-guide")]
    async fn ofm_complete_guide_resource(
        &self,
        _uri: String,
        _ctx: &RequestContext,
    ) -> McpResult<String> {
        Ok(crate::resources::OFM_SYNTAX_GUIDE.to_string())
    }

    /// Quick reference for Obsidian Flavored Markdown
    #[resource("obsidian://syntax/quick-ref")]
    async fn ofm_quick_reference_resource(
        &self,
        _uri: String,
        _ctx: &RequestContext,
    ) -> McpResult<String> {
        Ok(crate::resources::OFM_QUICK_REFERENCE.to_string())
    }

    /// Example note demonstrating all OFM features
    #[resource("obsidian://examples/sample-note")]
    async fn ofm_example_note_resource(
        &self,
        _uri: String,
        _ctx: &RequestContext,
    ) -> McpResult<String> {
        Ok(crate::resources::OFM_EXAMPLE_NOTE.to_string())
    }

    // ==================== OFM Documentation Tools (Resource Fallback) ====================

    /// Get Obsidian Flavored Markdown syntax guide (tool fallback for clients without resource support)
    #[tool(
        description = "Get Obsidian Flavored Markdown syntax guide for the OFM syntax TurboVault parses, classifies, or preserves",
        usage = "Use before writing notes to ensure correct syntax, or as reference for OFM extensions. Prefer resource obsidian://syntax/complete-guide if client supports resources",
        performance = "Instant, returns static documentation",
        related = ["get_ofm_quick_ref", "get_ofm_examples"],
        examples = [],
        tags = ["read"],
        read_only = true,
    )]
    async fn get_ofm_syntax_guide(&self) -> McpResult<serde_json::Value> {
        let guide = crate::resources::OFM_SYNTAX_GUIDE.to_string();

        Ok(serde_json::json!({
            "title": "Obsidian Flavored Markdown - Syntax Guide",
            "content": guide,
            "format": "markdown",
            "sections": [
                "Overview",
                "Core Philosophy",
                "Supported Standards",
                "Markdown Extensions",
                "Usage Guidelines",
                "Best Practices"
            ],
            "alternatives": {
                "resource": "obsidian://syntax/complete-guide",
                "quick_ref_tool": "get_ofm_quick_ref",
                "examples_tool": "get_ofm_examples"
            }
        }))
    }

    /// Get quick reference for Obsidian Flavored Markdown (tool fallback)
    #[tool(
        description = "Get condensed OFM cheat sheet with common patterns and best practices",
        usage = "Use for quick syntax reminders during note writing. More concise than full guide. Prefer resource obsidian://syntax/quick-ref if client supports resources",
        performance = "Instant, returns static documentation",
        related = ["get_ofm_syntax_guide", "get_ofm_examples"],
        examples = [],
        tags = ["read"],
        read_only = true,
    )]
    async fn get_ofm_quick_ref(&self) -> McpResult<serde_json::Value> {
        let quick_ref = crate::resources::OFM_QUICK_REFERENCE.to_string();

        Ok(serde_json::json!({
            "title": "Obsidian Flavored Markdown - Quick Reference",
            "content": quick_ref,
            "format": "markdown",
            "sections": [
                "Core Syntax",
                "Obsidian Extensions",
                "Key Differences",
                "Best Practices",
                "Common Patterns"
            ],
            "alternatives": {
                "resource": "obsidian://syntax/quick-ref",
                "complete_guide_tool": "get_ofm_syntax_guide",
                "examples_tool": "get_ofm_examples"
            }
        }))
    }

    /// Get example note demonstrating all OFM features (tool fallback)
    #[tool(
        description = "Get comprehensive example note demonstrating ALL OFM features with real-world patterns",
        usage = "Use as reference when creating complex notes or learning OFM syntax by example. Shows daily notes, Zettelkasten, and MOC patterns. Prefer resource obsidian://examples/sample-note if client supports resources",
        performance = "Instant, returns static example note",
        related = ["get_ofm_syntax_guide", "get_ofm_quick_ref"],
        examples = [],
        tags = ["read"],
        read_only = true,
    )]
    async fn get_ofm_examples(&self) -> McpResult<serde_json::Value> {
        let examples = crate::resources::OFM_EXAMPLE_NOTE.to_string();

        Ok(serde_json::json!({
            "title": "Obsidian Flavored Markdown - Complete Example Note",
            "content": examples,
            "format": "markdown",
            "features_demonstrated": [
                "Basic Formatting",
                "Wikilinks & Aliases",
                "Embeds (notes, images, PDFs)",
                "Block References",
                "Callouts (all types)",
                "Task Lists",
                "Tables",
                "Code Blocks",
                "Preserved Obsidian syntax",
                "Real-World Patterns"
            ],
            "patterns_shown": [
                "Daily Note Template",
                "Index/MOC Pattern",
                "Zettelkasten Pattern"
            ],
            "alternatives": {
                "resource": "obsidian://examples/sample-note",
                "syntax_guide_tool": "get_ofm_syntax_guide",
                "quick_ref_tool": "get_ofm_quick_ref"
            }
        }))
    }
}
