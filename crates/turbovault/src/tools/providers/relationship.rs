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

#[turbomcp::server(name = "obsidian-vault", version = "2.0.0")]
impl RelationshipProvider {
    // ==================== Relationship Operations ====================

    /// Suggest files to link
    #[tool(
        description = "AI-powered link suggestions for a note (returns top N candidates with reasoning)",
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn suggest_links(
        &self,
        file: String,
        limit: Option<i32>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = RelationshipTools::new(manager);
        let limit = usize::try_from(limit.unwrap_or(5)).unwrap_or(0);
        let suggestions = tools
            .suggest_links(&file, limit)
            .await
            .map_err(to_mcp_error)?;

        let result_data =
            serde_json::to_value(&suggestions).map_err(|e| McpError::internal(e.to_string()))?;
        let count = result_data["suggestions"].as_array().map_or(0, Vec::len);

        let response = StandardResponse::new(vault_name, "suggest_links", result_data)
            .with_count(count)
            .with_meta("limit", serde_json::json!(limit));

        response.to_json()
    }

    /// Get link strength between two files
    #[tool(
        description = "Calculate connection strength between two notes (0.0-1.0 score based on multiple factors)",
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn get_link_strength(
        &self,
        source: String,
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
        description = "Rank all notes by graph centrality metrics (betweenness, closeness, eigenvector)",
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn get_centrality_ranking(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = RelationshipTools::new(manager);
        let ranking = tools.get_centrality_ranking().await.map_err(to_mcp_error)?;

        let result_data =
            serde_json::to_value(&ranking).map_err(|e| McpError::internal(e.to_string()))?;
        let count = result_data["rankings"].as_array().map_or(0, Vec::len);

        let response = StandardResponse::new(vault_name, "get_centrality_ranking", result_data)
            .with_count(count)
            .with_meta(
                "metrics",
                serde_json::json!(["betweenness", "closeness", "eigenvector"]),
            );

        response.to_json()
    }
}
