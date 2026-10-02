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

#[turbomcp::server(name = "obsidian-vault", version = "2.0.0")]
impl ExportProvider {
    // ==================== Export Operations ====================

    /// Export health report as JSON or CSV
    #[tool(
        description = "Export vault health analysis as structured data",
        tags = ["read", "export"],
        read_only = true,
    )]
    async fn export_health_report(&self, format: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let report = tools
            .export_health_report(&format)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "export_health_report",
            serde_json::json!({"content": report, "format": format}),
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }

    /// Export broken links as JSON or CSV
    #[tool(
        description = "Export broken links report as structured data",
        tags = ["read", "export"],
        read_only = true,
    )]
    async fn export_broken_links(&self, format: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let links = tools
            .export_broken_links(&format)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "export_broken_links",
            serde_json::json!({"content": links, "format": format}),
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }

    /// Export vault statistics as JSON or CSV
    #[tool(
        description = "Export comprehensive vault statistics as structured data",
        tags = ["read", "export"],
        read_only = true,
    )]
    async fn export_vault_stats(&self, format: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let stats = tools
            .export_vault_stats(&format)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "export_vault_stats",
            serde_json::json!({"content": stats, "format": format}),
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }

    /// Export full analysis report
    #[tool(
        description = "Export comprehensive vault analysis combining health, stats, links, and clusters",
        tags = ["read", "export"],
        read_only = true,
    )]
    async fn export_analysis_report(&self, format: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let report = tools
            .export_analysis_report(&format)
            .await
            .map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "export_analysis_report",
            serde_json::json!({"content": report, "format": format}),
        )
        .with_meta("format", serde_json::json!(format));

        response.to_json()
    }
}
