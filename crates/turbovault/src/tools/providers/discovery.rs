//! DiscoveryProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct DiscoveryProvider(CoreToolHandler);

impl DiscoveryProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for DiscoveryProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[turbomcp::server(name = "obsidian-vault", version = "2.0.0")]
impl DiscoveryProvider {
    // ==================== Search (LLM Discovery) ====================

    /// Search vault by keyword
    #[tool(
        description = "Full-text search across all notes using Tantivy search engine with BM25 ranking",
        tags = ["read", "search"],
        read_only = true,
    )]
    async fn search(&self, query: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;
        let results = engine.search(&query).await.map_err(to_mcp_error)?;

        let result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let count = extract_count(&result_data);

        let response = StandardResponse::new(vault_name, "search", result_data)
            .with_count(count)
            .with_next_step("advanced_search")
            .with_next_step("recommend_related");

        response.to_json()
    }

    /// Advanced search with filters
    #[tool(
        description = "Enhanced search with tag, frontmatter, and path filters returning ranked results with match context",
        tags = ["read", "search"],
        read_only = true,
    )]
    async fn advanced_search(
        &self,
        query: String,
        tags: Option<Vec<String>>,
        frontmatter_filters: Option<Vec<FrontmatterFilter>>,
        exclude_paths: Option<Vec<String>>,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;

        let result_limit = limit.unwrap_or(10);
        let mut search_query = SearchQuery::new(query).limit(result_limit);

        if let Some(tags) = tags {
            search_query = search_query.with_tags(tags);
        }
        if let Some(filters) = frontmatter_filters {
            for f in filters {
                search_query = search_query.with_frontmatter(f.key, f.value);
            }
        }
        if let Some(excludes) = exclude_paths {
            search_query = search_query.exclude(excludes);
        }

        let results = engine
            .advanced_search(search_query)
            .await
            .map_err(to_mcp_error)?;
        let result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let count = extract_count(&result_data);

        let response = StandardResponse::new(vault_name, "advanced_search", result_data)
            .with_count(count)
            .with_next_step("search");

        response.to_json()
    }

    /// Search by frontmatter key-value pair
    #[tool(
        description = "Find notes where a frontmatter field matches a value. Returns up to 100 results ranked by relevance",
        tags = ["read", "search"],
        read_only = true,
    )]
    async fn search_by_frontmatter(
        &self,
        key: String,
        value: String,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;

        let results = engine
            .search_by_frontmatter(&key, &value)
            .await
            .map_err(to_mcp_error)?;
        let result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let count = extract_count(&result_data);

        let response = StandardResponse::new(vault_name, "search_by_frontmatter", result_data)
            .with_count(count)
            .with_next_steps(&["advanced_search", "read_note"]);

        response.to_json()
    }

    /// Find related notes (recommendations engine)
    #[tool(
        description = "ML-powered note recommendations based on content similarity and link proximity with similarity scores and reasoning",
        tags = ["read", "search"],
        read_only = true,
    )]
    async fn recommend_related(&self, path: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;
        let results = engine
            .recommend_related(&path)
            .await
            .map_err(to_mcp_error)?;

        let result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let count = extract_count(&result_data);

        let response = StandardResponse::new(vault_name, "recommend_related", result_data)
            .with_count(count)
            .with_next_step("get_related_notes");

        response.to_json()
    }

    // ==================== SQL Frontmatter Queries (feature = "sql") ====================

    /// Inspect frontmatter schema
    #[tool(
        description = "Inspect the frontmatter schema across all vault notes — shows column names, types, nullability, and counts. Call this before writing SQL queries to discover available columns",
        tags = ["read", "frontmatter"],
        read_only = true,
    )]
    async fn inspect_frontmatter(&self) -> McpResult<serde_json::Value> {
        #[cfg(feature = "sql")]
        {
            let (vault_name, manager) = self.get_vault_pair().await?;
            let engine = FrontmatterSqlEngine::new(manager);
            let result = engine.inspect().await.map_err(to_mcp_error)?;

            let response = StandardResponse::new(vault_name, "inspect_frontmatter", result)
                .with_next_step("query_frontmatter_sql");

            response.to_json()
        }
        #[cfg(not(feature = "sql"))]
        {
            Err(McpError::internal(
                "SQL feature not enabled. Rebuild TurboVault with: cargo build --features sql"
                    .to_string(),
            ))
        }
    }

    /// Execute SQL query against frontmatter
    #[tool(
        description = "Execute arbitrary SQL against a 'files' table built from all vault note frontmatter. Each note becomes a row with 'path' + all frontmatter keys as columns. Supports WHERE, JOIN, GROUP BY, ORDER BY, LIMIT, subqueries, and aggregations",
        tags = ["read", "frontmatter", "sql"],
        read_only = true,
    )]
    async fn query_frontmatter_sql(&self, sql: String) -> McpResult<serde_json::Value> {
        #[cfg(feature = "sql")]
        {
            let (vault_name, manager) = self.get_vault_pair().await?;
            let engine = FrontmatterSqlEngine::new(manager);
            let result = engine.query(&sql).await.map_err(to_mcp_error)?;

            let response = StandardResponse::new(vault_name, "query_frontmatter_sql", result)
                .with_next_steps(&["inspect_frontmatter", "read_note"]);

            response.to_json()
        }
        #[cfg(not(feature = "sql"))]
        {
            let _ = sql;
            Err(McpError::internal(
                "SQL feature not enabled. Rebuild TurboVault with: cargo build --features sql"
                    .to_string(),
            ))
        }
    }
}
