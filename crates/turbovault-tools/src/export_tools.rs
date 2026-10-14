//! Export tools for vault analysis data

use std::sync::Arc;
use turbovault_core::prelude::*;
use turbovault_export::{
    AnalysisReportExporter, BrokenLinkRecord, BrokenLinksExporter, HealthReportExporter,
    VaultStatsExporter, VaultStatsRecord, create_health_report,
};
use turbovault_graph::{HealthAnalyzer, HealthCounts, HealthReport};
use turbovault_parser::to_plain_text;
use turbovault_vault::VaultManager;

/// Export tools for vault analysis and reporting
pub struct ExportTools {
    /// The vault reports are made from.
    pub manager: Arc<VaultManager>,
}

impl ExportTools {
    /// Create new export tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// The vault's health counts and score, the same ones `vault_health`
    /// reports.
    async fn health_counts(&self) -> Result<(HealthCounts, u8)> {
        let graph = self.manager.link_graph_flushed().await;
        let graph = graph.read().await;
        let counts = HealthAnalyzer::with_files(&graph, graph.all_unresolved_links()).counts()?;
        Ok((counts, HealthReport::score(&counts)))
    }

    /// Export health report
    pub async fn export_health_report(&self, format: &str) -> Result<String> {
        let (counts, health_score) = self.health_counts().await?;
        let report = create_health_report(
            self.manager.vault_name(),
            health_score,
            counts.total_notes,
            counts.total_links,
            counts.broken_links,
            counts.orphaned_notes,
        );

        match format {
            "json" => HealthReportExporter::to_json(&report),
            "csv" => HealthReportExporter::to_csv(&report),
            _ => Err(Error::validation_error(
                "Invalid export format. Use 'json' or 'csv'".to_string(),
            )),
        }
    }

    /// Export broken links
    pub async fn export_broken_links(&self, format: &str) -> Result<String> {
        let links: Vec<BrokenLinkRecord> = {
            let graph = self.manager.link_graph_flushed().await;
            let graph = graph.read().await;
            HealthAnalyzer::with_files(&graph, graph.all_unresolved_links())
                .broken_links()?
                .into_iter()
                .map(|link| BrokenLinkRecord {
                    source_file: self.manager.relative_path(&link.source_file),
                    target: link.target,
                    line: link.line,
                    suggestions: link.suggestions,
                })
                .collect()
        };

        match format {
            "json" => BrokenLinksExporter::to_json(&links),
            "csv" => BrokenLinksExporter::to_csv(&links),
            _ => Err(Error::validation_error(
                "Invalid export format. Use 'json' or 'csv'".to_string(),
            )),
        }
    }

    /// Export vault statistics
    pub async fn export_vault_stats(&self, format: &str) -> Result<String> {
        // Readability over every note, from the shared note cache.
        let notes = self.manager.notes_validated().await;
        let mut total_words = 0usize;
        let mut total_readable_chars = 0usize;
        for note in &notes {
            let plain_text = to_plain_text(&note.content);
            total_words += plain_text.split_whitespace().count();
            total_readable_chars += plain_text.chars().count();
        }
        let note_count = notes.len();

        let graph = self.manager.link_graph_flushed().await;
        let stats = graph.read().await.stats();

        let avg_words_per_note = if note_count > 0 {
            total_words as f64 / note_count as f64
        } else {
            0.0
        };

        let stats_record = VaultStatsRecord {
            timestamp: chrono::Utc::now().to_rfc3339(),
            vault_name: self.manager.vault_name().to_string(),
            total_files: stats.total_files,
            total_links: stats.total_links,
            orphaned_files: stats.orphaned_files,
            average_links_per_file: stats.average_links_per_file,
            total_words,
            total_readable_chars,
            avg_words_per_note,
        };

        match format {
            "json" => VaultStatsExporter::to_json(&stats_record),
            "csv" => VaultStatsExporter::to_csv(&stats_record),
            _ => Err(Error::validation_error(
                "Invalid export format. Use 'json' or 'csv'".to_string(),
            )),
        }
    }

    /// Export full analysis report
    pub async fn export_analysis_report(&self, format: &str) -> Result<String> {
        let (counts, health_score) = self.health_counts().await?;
        let vault_name = self.manager.vault_name();
        let health = create_health_report(
            vault_name,
            health_score,
            counts.total_notes,
            counts.total_links,
            counts.broken_links,
            counts.orphaned_notes,
        );

        let mut recommendations = Vec::new();
        if counts.broken_links > 0 {
            recommendations.push(format!(
                "Fix {} broken link(s); export_broken_links lists them with suggested targets",
                counts.broken_links
            ));
        }
        if counts.orphaned_notes > 0 {
            recommendations.push(format!(
                "Link {} orphaned note(s) to related notes, or archive them",
                counts.orphaned_notes
            ));
        }
        if counts.isolated_clusters > 1 {
            recommendations.push(format!(
                "Connect {} isolated clusters to the rest of the vault",
                counts.isolated_clusters
            ));
        }

        let analysis_report = turbovault_export::AnalysisReport {
            timestamp: chrono::Utc::now().to_rfc3339(),
            vault_name: vault_name.to_string(),
            health,
            broken_links_count: counts.broken_links,
            orphaned_notes_count: counts.orphaned_notes,
            recommendations,
        };

        match format {
            "json" => AnalysisReportExporter::to_json(&analysis_report),
            "csv" => AnalysisReportExporter::to_csv(&analysis_report),
            _ => Err(Error::validation_error(
                "Invalid export format. Use 'json' or 'csv'".to_string(),
            )),
        }
    }
}
