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

#[turbomcp::server(name = "obsidian-vault", version = "1.6.0")]
impl RelationshipProvider {
    // ==================== Relationship Operations ====================

    /// Suggest files to link
    #[tool(
        description = "AI-powered link suggestions for a note (returns top N candidates with reasoning)",
        usage = "Use to improve vault connectivity and discover non-obvious relationships. Analyzes content similarity, link patterns, and graph structure. ML-based, slower than simple queries.",
        performance = "200ms-2s depending on vault size. Uses TF-IDF + graph features. Consider limit parameter for faster results.",
        related = ["recommend_related", "get_dead_end_notes", "get_related_notes"],
        examples = [
            "file: daily/2024-01-15.md, limit: 5",
            "file: projects/research.md, limit: 10",
            "file: index.md (default limit: 5)"
        ],
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn suggest_links(
        &self,
        file: String,
        limit: Option<i32>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
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
        usage = "Use to validate relationship importance or prioritize link maintenance. Considers direct links, shared links, content similarity, and co-citation.",
        performance = "Fast (<50ms typical), cached graph traversal.",
        related = ["suggest_links", "get_related_notes", "recommend_related"],
        examples = [
            "source: index.md, target: concepts/foo.md",
            "source: daily/2024-01-15.md, target: projects/research.md",
            "source: MOC.md, target: archive/old-note.md"
        ],
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn get_link_strength(
        &self,
        source: String,
        target: String,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
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
        usage = "Use for identifying key notes beyond simple link counts. Betweenness identifies bridge notes, closeness finds accessible notes, eigenvector reveals influence. More sophisticated than get_hub_notes.",
        performance = "Computationally expensive on large vaults. O(V³) for betweenness. May take several seconds for >1000 notes.",
        related = ["get_hub_notes", "explain_vault", "get_link_strength"],
        examples = [
            "Returns all notes ranked by betweenness (bridge importance)",
            "Returns all notes ranked by closeness (accessibility)",
            "Returns all notes ranked by eigenvector (influence)"
        ],
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn get_centrality_ranking(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair_with_reindex().await?;
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
