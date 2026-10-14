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

/// Which guide `get_ofm_guide` returns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OfmGuide {
    /// A one-page cheat sheet.
    #[default]
    QuickRef,
    /// The full guide.
    Syntax,
    /// An example note using every feature.
    Example,
}

/// What `get_ofm_guide` returns: one guide, and the resource holding the
/// same text.
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
    /// The MCP resource with the same text.
    resource: &'static str,
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

    // ==================== OFM Documentation Tool (Resource Fallback) ====================

    /// An Obsidian Flavored Markdown guide, for clients without resource support.
    #[tool(
        description = "A guide to Obsidian Flavored Markdown as TurboVault reads it. `quick_ref` (default): a one-page cheat sheet of wikilinks, formatting, tasks, callouts, tables, embeds, frontmatter and block references; read it before writing notes. `syntax`: the full guide, long. `example`: a note using every feature, to copy patterns from. The same texts are the resources obsidian://syntax/quick-ref, obsidian://syntax/complete-guide and obsidian://examples/sample-note.",
        title = "OFM Guide",
        tags = ["read"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = OfmDocument,
    )]
    async fn get_ofm_guide(
        &self,
        #[description("`quick_ref` (default), `syntax` or `example`.")] guide: Option<OfmGuide>,
    ) -> McpResult<serde_json::Value> {
        let document = match guide.unwrap_or_default() {
            OfmGuide::QuickRef => OfmDocument {
                title: "Obsidian Flavored Markdown - Quick Reference",
                content: crate::resources::OFM_QUICK_REFERENCE,
                format: "markdown",
                sections: Some(section_headings(crate::resources::OFM_QUICK_REFERENCE)),
                features_demonstrated: None,
                patterns_shown: None,
                resource: "obsidian://syntax/quick-ref",
            },
            OfmGuide::Syntax => OfmDocument {
                title: "Obsidian Flavored Markdown - Syntax Guide",
                content: crate::resources::OFM_SYNTAX_GUIDE,
                format: "markdown",
                sections: Some(section_headings(crate::resources::OFM_SYNTAX_GUIDE)),
                features_demonstrated: None,
                patterns_shown: None,
                resource: "obsidian://syntax/complete-guide",
            },
            OfmGuide::Example => OfmDocument {
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
                resource: "obsidian://examples/sample-note",
            },
        };
        serde_json::to_value(document).map_err(|e| McpError::internal(e.to_string()))
    }
}
