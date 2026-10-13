//! ExportProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct ExportProvider(CoreToolHandler);

impl ExportProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for ExportProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Response payload for the export tools: the document, and its format.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct ExportPayload {
    /// The JSON or CSV document, as text.
    content: String,
    format: String,
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl ExportProvider {
    // ==================== Export Operations ====================

    /// Export health report as JSON or CSV
    #[tool(
        description = "The vault's link-health report (score, counts, broken links, orphans) as one JSON or CSV document in `content`, to save or hand to another tool. To act on the findings, the get_* graph tools return them as data.",
        title = "Export Health Report",
        tags = ["read", "export"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<ExportPayload>,
    )]
    async fn export_health_report(
        &self,
        ctx: &RequestContext,
        #[description("`json` or `csv`.")] format: String,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let mut stages = Stages::new(ctx, 1);
        stages.next("Analyzing the vault").await?;
        let report = tools
            .export_health_report(&format)
            .await
            .map_err(to_mcp_error)?;
        stages.finish().await;

        let response = StandardResponse::new(
            vault_name,
            "export_health_report",
            ExportPayload {
                content: report,
                format: format.clone(),
            },
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }

    /// Export broken links as JSON or CSV
    #[tool(
        description = "Every broken link in the vault as one JSON or CSV document in `content`, to save or share. get_broken_links returns them page by page as data.",
        title = "Export Broken Links",
        tags = ["read", "export"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<ExportPayload>,
    )]
    async fn export_broken_links(
        &self,
        ctx: &RequestContext,
        #[description("`json` or `csv`.")] format: String,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let mut stages = Stages::new(ctx, 1);
        stages.next("Analyzing the vault").await?;
        let links = tools
            .export_broken_links(&format)
            .await
            .map_err(to_mcp_error)?;
        stages.finish().await;

        let response = StandardResponse::new(
            vault_name,
            "export_broken_links",
            ExportPayload {
                content: links,
                format: format.clone(),
            },
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }

    /// Export vault statistics as JSON or CSV
    #[tool(
        description = "Vault statistics (note, link, orphan and word counts, and averages per note) as one JSON or CSV document in `content`, to save or share.",
        title = "Export Vault Statistics",
        tags = ["read", "export"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<ExportPayload>,
    )]
    async fn export_vault_stats(
        &self,
        ctx: &RequestContext,
        #[description("`json` or `csv`.")] format: String,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let mut stages = Stages::new(ctx, 1);
        stages.next("Analyzing the vault").await?;
        let stats = tools
            .export_vault_stats(&format)
            .await
            .map_err(to_mcp_error)?;
        stages.finish().await;

        let response = StandardResponse::new(
            vault_name,
            "export_vault_stats",
            ExportPayload {
                content: stats,
                format: format.clone(),
            },
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }

    /// Export full analysis report
    #[tool(
        description = "A combined analysis report (health, statistics, links and clusters) as one JSON or CSV document in `content`. The largest export; use it for a full snapshot to save.",
        title = "Export Analysis Report",
        tags = ["read", "export"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<ExportPayload>,
    )]
    async fn export_analysis_report(
        &self,
        ctx: &RequestContext,
        #[description("`json` or `csv`.")] format: String,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let mut stages = Stages::new(ctx, 1);
        stages.next("Analyzing the vault").await?;
        let report = tools
            .export_analysis_report(&format)
            .await
            .map_err(to_mcp_error)?;
        stages.finish().await;

        let response = StandardResponse::new(
            vault_name,
            "export_analysis_report",
            ExportPayload {
                content: report,
                format: format.clone(),
            },
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }
}
