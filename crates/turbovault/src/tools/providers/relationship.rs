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

/// Cut the array under `key` in `data` to `request`'s page.
fn page_array(
    data: &mut serde_json::Value,
    key: &str,
    request: &PageRequest,
    ranked: bool,
) -> Page {
    let items = match data.get_mut(key).map(serde_json::Value::take) {
        Some(serde_json::Value::Array(items)) => items,
        _ => Vec::new(),
    };
    let (items, page) = if ranked {
        request.slice_ranked(items)
    } else {
        request.slice(items)
    };
    data[key] = serde_json::Value::Array(items);
    page
}

#[turbomcp::server(name = "obsidian-vault", version = "2.0.0")]
impl RelationshipProvider {
    // ==================== Relationship Operations ====================

    /// Suggest files to link
    #[tool(
        description = "Suggest notes that a note could link to, ranked by how many other notes link to both, each with its reasons. Use it when filling in a note's connections; recommend_related matches on content instead.",
        title = "Suggest Links",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
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
        let page = page_array(&mut result_data, "suggestions", &request, true);

        let response = StandardResponse::new(vault_name, "suggest_links", result_data)
            .with_page(page)
            .with_meta("limit", serde_json::json!(request.limit));

        response.to_json()
    }

    /// Get link strength between two files
    #[tool(
        description = "Score how strongly two notes are connected, from 0.0 to 1.0, from links between them in each direction and notes that link to both, with the counts behind it and a plain-language reading.",
        title = "Get Link Strength",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
    )]
    async fn get_link_strength(
        &self,
        #[description(
            "Vault-relative path of the first note, including the extension, such as `projects/Plan.md`."
        )]
        source: String,
        #[description("Vault-relative path of the second note, including the extension.")]
        target: String,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = RelationshipTools::new(manager);
        let strength = tools
            .get_link_strength(&source, &target)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "get_link_strength",
            serde_json::json!({"source": source, "target": target, "strength": strength}),
        )
        .with_meta("metric", serde_json::json!("link_strength"));

        response.to_json()
    }

    /// Get centrality ranking
    #[tool(
        description = "Rank notes by how central they are in the link graph, most central first, with betweenness, closeness and eigenvector-style scores and a label such as `Central hub`. Returns the top 50 unless asked for more; get_hub_notes is a simpler ranking by link count.",
        title = "Get Centrality Ranking",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
    )]
    async fn get_centrality_ranking(
        &self,
        #[description("Most notes to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many top-ranked notes to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = RelationshipTools::new(manager);
        let mut result_data = tools.get_centrality_ranking().await.map_err(to_mcp_error)?;
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let page = page_array(&mut result_data, "rankings", &request, false);

        let response = StandardResponse::new(vault_name, "get_centrality_ranking", result_data)
            .with_page(page)
            .with_meta(
                "metrics",
                serde_json::json!(["betweenness", "closeness", "eigenvector"]),
            );

        response.to_json()
    }
}
