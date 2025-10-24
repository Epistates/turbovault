//! Export tools for vault analysis data

use turbovault_core::prelude::*;
use turbovault_export::{
    AnalysisReportExporter, BrokenLinkRecord, BrokenLinksExporter, HealthReportExporter,
    VaultStatsExporter, VaultStatsRecord, create_health_report,
};
use turbovault_vault::VaultManager;
use std::sync::Arc;

/// Export tools for vault analysis and reporting
pub struct ExportTools {
    pub manager: Arc<VaultManager>,
}

impl ExportTools {
    /// Create new export tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Export health report
    pub async fn export_health_report(&self, format: &str) -> Result<String> {
        let graph = self.manager.link_graph();
        let graph_read = graph.read().await;

        let stats = graph_read.stats();

        // Calculate metrics
        let total_notes = stats.total_files;
        let total_links = stats.total_links;
        let broken_links = 0; // Would need HealthAnalyzer for actual count
        let orphaned_notes = stats.orphaned_files;

        // Health score heuristic
        let health_score = if broken_links == 0 && orphaned_notes == 0 {
            100
        } else if broken_links == 0 {
            80
        } else {
            70
        };

        let report = create_health_report(
            "default",
            health_score,
            total_notes,
            total_links,
            broken_links,
            orphaned_notes,
        );

        match format {
            "json" => HealthReportExporter::to_json(&report),
            "csv" => HealthReportExporter::to_csv(&report),
            _ => Err(Error::config_error(
                "Invalid export format. Use 'json' or 'csv'".to_string(),
            )),
        }
    }

    /// Export broken links
    pub async fn export_broken_links(&self, format: &str) -> Result<String> {
        // In real implementation, would get actual broken links from HealthAnalyzer
        let links: Vec<BrokenLinkRecord> = vec![];

        match format {
            "json" => BrokenLinksExporter::to_json(&links),
            "csv" => BrokenLinksExporter::to_csv(&links),
            _ => Err(Error::config_error(
                "Invalid export format. Use 'json' or 'csv'".to_string(),
            )),
        }
    }

    /// Export vault statistics
    pub async fn export_vault_stats(&self, format: &str) -> Result<String> {
        let graph = self.manager.link_graph();
        let graph_read = graph.read().await;
        let stats = graph_read.stats();

        let stats_record = VaultStatsRecord {
            timestamp: chrono::Utc::now().to_rfc3339(),
            vault_name: "default".to_string(),
            total_files: stats.total_files,
            total_links: stats.total_links,
            orphaned_files: stats.orphaned_files,
            average_links_per_file: stats.average_links_per_file,
        };

        match format {
            "json" => VaultStatsExporter::to_json(&stats_record),
            "csv" => VaultStatsExporter::to_csv(&stats_record),
            _ => Err(Error::config_error(
                "Invalid export format. Use 'json' or 'csv'".to_string(),
            )),
        }
    }

    /// Export full analysis report
    pub async fn export_analysis_report(&self, format: &str) -> Result<String> {
        let graph = self.manager.link_graph();
        let graph_read = graph.read().await;
        let stats = graph_read.stats();

        let total_notes = stats.total_files;
        let total_links = stats.total_links;
        let broken_links = 0;
        let orphaned_notes = stats.orphaned_files;

        let health_score = if broken_links == 0 && orphaned_notes == 0 {
            100
        } else if broken_links == 0 {
            80
        } else {
            70
        };

        let health = create_health_report(
            "default",
            health_score,
            total_notes,
            total_links,
            broken_links,
            orphaned_notes,
        );

        let analysis_report = turbovault_export::AnalysisReport {
            timestamp: chrono::Utc::now().to_rfc3339(),
            vault_name: "default".to_string(),
            health,
            broken_links_count: broken_links,
            orphaned_notes_count: orphaned_notes,
            recommendations: vec![
                "Ensure all notes are linked for better connectivity".to_string(),
                "Review and fix broken links regularly".to_string(),
            ],
        };

        match format {
            "json" => AnalysisReportExporter::to_json(&analysis_report),
            "csv" => AnalysisReportExporter::to_csv(&analysis_report),
            _ => Err(Error::config_error(
                "Invalid export format. Use 'json' or 'csv'".to_string(),
            )),
        }
    }
}
