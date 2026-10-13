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

/// Which report `export` writes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExportReport {
    /// Score, counts, broken links and orphans.
    Health,
    /// Every broken link.
    BrokenLinks,
    /// Note, link, orphan and word counts, and averages per note.
    Stats,
    /// Health, statistics, links and clusters together: the largest.
    Analysis,
}

/// Response payload for `export`: the document, and its format.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct ExportPayload {
    /// The JSON or CSV document, as text.
    content: String,
    format: String,
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl ExportProvider {
    // ==================== Export Operations ====================

    /// Export a vault report as JSON or CSV.
    #[tool(
        description = "One report about the vault as a single JSON or CSV document in `content`, to save or hand to another tool. `health`: link-health score, counts, broken links and orphans. `broken_links`: every broken link. `stats`: note, link, orphan and word counts and averages. `analysis`: health, statistics, links and clusters together, the largest. To act on findings, vault_health and graph_report return them as data.",
        title = "Export Report",
        tags = ["read", "export"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<ExportPayload>,
    )]
    async fn export(
        &self,
        ctx: &RequestContext,
        #[description("Which report: `health`, `broken_links`, `stats` or `analysis`.")]
        report: ExportReport,
        #[description("`json` or `csv`.")] format: String,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ExportTools::new(manager);
        let mut stages = Stages::new(ctx, 1);
        stages.next("Analyzing the vault").await?;
        let content = match report {
            ExportReport::Health => tools.export_health_report(&format).await,
            ExportReport::BrokenLinks => tools.export_broken_links(&format).await,
            ExportReport::Stats => tools.export_vault_stats(&format).await,
            ExportReport::Analysis => tools.export_analysis_report(&format).await,
        }
        .map_err(to_mcp_error)?;
        stages.finish().await;

        StandardResponse::new(
            vault_name,
            "export",
            ExportPayload {
                content,
                format: format.clone(),
            },
        )
        .with_meta("format", serde_json::json!(format))
        .to_json()
    }
}
