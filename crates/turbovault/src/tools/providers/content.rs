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

/// The `##` section headings of `markdown`, in order, leaving out any inside
/// a fenced code block (the guides show example notes that have their own).
fn section_headings(markdown: &str) -> Vec<&str> {
    let mut in_fence = false;
    markdown
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fence = !in_fence;
                return false;
            }
            !in_fence
        })
        .filter_map(|line| line.strip_prefix("## "))
        .map(str::trim)
        .collect()
}

/// What the OFM guide tools return: one guide, and where else to find it
/// and its siblings.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct OfmDocument {
    title: &'static str,
    /// The guide, as markdown.
    content: &'static str,
    /// Always `markdown`.
    format: &'static str,
    /// The guide's `##` headings. Not for the example note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sections: Option<Vec<&'static str>>,
    /// The example note only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    features_demonstrated: Option<Vec<&'static str>>,
    /// The example note only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    patterns_shown: Option<Vec<&'static str>>,
    alternatives: OfmAlternatives,
}

/// The resource holding the same text, and the tools for the other guides.
#[derive(serde::Serialize, schemars::JsonSchema, Default)]
struct OfmAlternatives {
    resource: &'static str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    complete_guide_tool: Option<&'static str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    syntax_guide_tool: Option<&'static str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quick_ref_tool: Option<&'static str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    examples_tool: Option<&'static str>,
}

fn to_value(document: OfmDocument) -> McpResult<serde_json::Value> {
    serde_json::to_value(document).map_err(|e| McpError::internal(e.to_string()))
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
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
        description = "The full guide to Obsidian Flavored Markdown as TurboVault reads it: links, embeds, callouts, frontmatter, tags and conventions, with what each is for. Long; get_ofm_quick_ref is the short version. The same text is the resource obsidian://syntax/complete-guide.",
        title = "OFM Syntax Guide",
        tags = ["read"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = OfmDocument,
    )]
    async fn get_ofm_syntax_guide(&self) -> McpResult<serde_json::Value> {
        let guide = crate::resources::OFM_SYNTAX_GUIDE;

        to_value(OfmDocument {
            title: "Obsidian Flavored Markdown - Syntax Guide",
            content: guide,
            format: "markdown",
            sections: Some(section_headings(guide)),
            features_demonstrated: None,
            patterns_shown: None,
            alternatives: OfmAlternatives {
                resource: "obsidian://syntax/complete-guide",
                quick_ref_tool: Some("get_ofm_quick_ref"),
                examples_tool: Some("get_ofm_examples"),
                ..OfmAlternatives::default()
            },
        })
    }

    /// Get quick reference for Obsidian Flavored Markdown (tool fallback)
    #[tool(
        description = "A one-page cheat sheet of Obsidian Flavored Markdown syntax: wikilinks, formatting, tasks, callouts, tables, embeds, frontmatter and block references. Read it before writing notes. The same text is the resource obsidian://syntax/quick-ref.",
        title = "OFM Quick Reference",
        tags = ["read"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = OfmDocument,
    )]
    async fn get_ofm_quick_ref(&self) -> McpResult<serde_json::Value> {
        let quick_ref = crate::resources::OFM_QUICK_REFERENCE;

        to_value(OfmDocument {
            title: "Obsidian Flavored Markdown - Quick Reference",
            content: quick_ref,
            format: "markdown",
            sections: Some(section_headings(quick_ref)),
            features_demonstrated: None,
            patterns_shown: None,
            alternatives: OfmAlternatives {
                resource: "obsidian://syntax/quick-ref",
                complete_guide_tool: Some("get_ofm_syntax_guide"),
                examples_tool: Some("get_ofm_examples"),
                ..OfmAlternatives::default()
            },
        })
    }

    /// Get example note demonstrating all OFM features (tool fallback)
    #[tool(
        description = "An example note that uses every Obsidian Flavored Markdown feature, to copy patterns from. The same text is the resource obsidian://examples/sample-note.",
        title = "OFM Example Note",
        tags = ["read"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = OfmDocument,
    )]
    async fn get_ofm_examples(&self) -> McpResult<serde_json::Value> {
        to_value(OfmDocument {
            title: "Obsidian Flavored Markdown - Complete Example Note",
            content: crate::resources::OFM_EXAMPLE_NOTE,
            format: "markdown",
            sections: None,
            features_demonstrated: Some(vec![
                "Basic Formatting",
                "Wikilinks & Aliases",
                "Embeds (notes, images, PDFs)",
                "Block References",
                "Callouts (all types)",
                "Task Lists",
                "Tables",
                "Code Blocks",
                "Preserved Obsidian syntax",
                "Real-World Patterns",
            ]),
            patterns_shown: Some(vec![
                "Daily Note Template",
                "Index/MOC Pattern",
                "Zettelkasten Pattern",
            ]),
            alternatives: OfmAlternatives {
                resource: "obsidian://examples/sample-note",
                syntax_guide_tool: Some("get_ofm_syntax_guide"),
                quick_ref_tool: Some("get_ofm_quick_ref"),
                ..OfmAlternatives::default()
            },
        })
    }
}
