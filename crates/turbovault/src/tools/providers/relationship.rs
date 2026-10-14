//! RelationshipProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct RelationshipProvider(CoreToolHandler);

impl RelationshipProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for RelationshipProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// `RelationshipTools` builds these results as JSON; the types below are the
// schemas they are advertised with, and the output schema test holds the two
// together.

/// What `suggest_links` returns.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct LinkSuggestions {
    file: String,
    /// This page of the suggestions, strongest first.
    suggestions: Vec<LinkSuggestion>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct LinkSuggestion {
    target: String,
    strength: f64,
    reasons: Vec<String>,
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl RelationshipProvider {
    // ==================== Relationship Operations ====================

    /// Suggest files to link
    #[tool(
        description = "Suggest notes that a note could link to, ranked by how many other notes link to both, each with its reasons. Use it when filling in a note's connections; search with `related_to` matches on content instead.",
        title = "Suggest Links",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<LinkSuggestions>,
    )]
    async fn suggest_links(
        &self,
        #[description(
            "Vault-relative path of the note to suggest links for, including the extension, such as `projects/Plan.md`."
        )]
        file: String,
        #[description("Most suggestions to return. Default 5, maximum 100.")] limit: Option<i32>,
        #[description(
            "How many top suggestions to skip, for the next page. Default 0, at most 1000."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = RelationshipTools::new(manager);
        let request = PageRequest::ranked(
            offset,
            limit.map(|limit| usize::try_from(limit).unwrap_or(0)),
            5,
            MAX_SEARCH_RESULTS,
        );
        let mut result_data = tools
            .suggest_links(&file, request.fetch())
            .await
            .map_err(to_mcp_error)?;
        let page = request.slice_field(&mut result_data, "suggestions", true);

        let response = StandardResponse::new(vault_name, "suggest_links", result_data)
            .with_page(page)
            .with_meta("limit", serde_json::json!(request.limit));

        response.to_json()
    }
}
