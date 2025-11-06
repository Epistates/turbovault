//! # Export System
//!
//! Provides data export functionality for vault analysis in multiple formats (JSON, CSV).
//! Enables downstream processing and reporting of vault metrics and analysis.
//!
//! ## Quick Start
//!
//! ```no_run
//! use turbovault_export::{create_health_report, HealthReportExporter};
//!
//! # fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Create a health report
//! let report = create_health_report(
//!     "my-vault",
//!     85,        // health score
//!     100,       // total notes
//!     150,       // total links
//!     2,         // broken links
//!     5,         // orphaned notes
//! );
//!
//! // Export as JSON
//! let json = HealthReportExporter::to_json(&report)?;
//! println!("JSON:\n{}", json);
//!
//! // Export as CSV
//! let csv = HealthReportExporter::to_csv(&report)?;
//! println!("CSV:\n{}", csv);
//! # Ok(())
//! # }
//! ```
//!
//! ## Export Formats
//!
//! The system supports two formats for all exporters:
//!
//! ### JSON Export
//! - Pretty-printed for readability
//! - Full structure preserved
//! - Suitable for API responses
//! - Nested arrays and objects supported
//!
//! ### CSV Export
//! - Flat structure (all values in one row)
//! - Header row included
//! - Quoted fields for safety
//! - Suitable for spreadsheets and databases
//!
//! ## Core Exporters
//!
//! ### HealthReportExporter
//!
//! Exports vault health analysis:
//! - Health score (0-100)
//! - Connected vs orphaned notes
//! - Broken link count
//! - Connectivity and link density metrics
//! - Status and recommendations
//!
//! ### BrokenLinksExporter
//!
//! Exports broken link analysis:
//! - Source file for each broken link
//! - Target that could not be resolved
//! - Line number in source file
//! - Suggested fixes
//!
//! ### VaultStatsExporter
//!
//! Exports vault statistics:
//! - Timestamp of analysis
//! - Total files and links
//! - Orphaned file count
//! - Average links per file
//!
//! ### AnalysisReportExporter
//!
//! Exports comprehensive analysis combining:
//! - Health report
//! - Broken links data
//! - Recommendations
//! - Full analysis context
//!
//! ## Data Models
//!
//! ### Health Metrics
//!
//! ```ignore
//! #[derive(Serialize, Deserialize)]
//! pub struct HealthReport {
//!     pub timestamp: String,
//!     pub vault_name: String,
//!     pub health_score: u8,
//!     pub total_notes: usize,
//!     pub total_links: usize,
//!     pub broken_links: usize,
//!     pub orphaned_notes: usize,
//!     pub connectivity_rate: f64,
//!     pub link_density: f64,
//!     pub status: String,
//!     pub recommendations: Vec<String>,
//! }
//! ```
//!
//! ### Broken Links
//!
//! Each broken link includes:
//! - Source file path
//! - Target reference (failed to resolve)
//! - Line number
//! - Suggested alternatives
//!
//! ## Integration with Analysis
//!
//! Export data is typically generated from:
//! - `turbovault_graph` health analysis (see <https://docs.rs/turbovault-graph>)
//! - `turbovault_tools` analysis tools (see <https://docs.rs/turbovault-tools>)
//! - Vault statistics computed at runtime
//!
//! Example integration:
//! ```no_run
//! use turbovault_export::{create_health_report, HealthReportExporter};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Get health from graph analysis
//! // let health = graph.analyze_health().await?;
//!
//! // Create report from health data
//! let report = create_health_report(
//!     "vault",
//!     80,
//!     50,
//!     100,
//!     1,
//!     2,
//! );
//!
//! // Export in desired format
//! // let json = HealthReportExporter::to_json(&report)?;
//! # Ok(())
//! # }
//! ```
//!
//! ## File I/O Patterns
//!
//! Exporters return strings (JSON or CSV). To save to files:
//!
//! ```ignore
//! use std::fs;
//!
//! let report = create_health_report(...);
//! let json = HealthReportExporter::to_json(&report)?;
//! fs::write("health-report.json", json)?;
//! ```
//!
//! ## Performance Considerations
//!
//! - JSON serialization is optimized with `serde`
//! - CSV generation uses string formatting (fast)
//! - All exporters run in-memory
//! - No I/O operations within exporters
//! - Suitable for batch processing large datasets

use chrono::Utc;
use serde::{Deserialize, Serialize};
use turbovault_core::prelude::*;
use turbovault_core::to_json_string;

/// Export format options
#[derive(Debug, Clone, Copy)]
pub enum ExportFormat {
    /// JSON format (pretty-printed)
    Json,
    /// CSV format (flattened)
    Csv,
}

/// Health report for export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub timestamp: String,
    pub vault_name: String,
    pub health_score: u8,
    pub total_notes: usize,
    pub total_links: usize,
    pub broken_links: usize,
    pub orphaned_notes: usize,
    pub connectivity_rate: f64,
    pub link_density: f64,
    pub status: String,
    pub recommendations: Vec<String>,
}

/// Broken link record for export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokenLinkRecord {
    pub source_file: String,
    pub target: String,
    pub line: usize,
    pub suggestions: Vec<String>,
}

/// Vault statistics for export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultStatsRecord {
    pub timestamp: String,
    pub vault_name: String,
    pub total_files: usize,
    pub total_links: usize,
    pub orphaned_files: usize,
    pub average_links_per_file: f64,
}

/// Full analysis report combining multiple metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub timestamp: String,
    pub vault_name: String,
    pub health: HealthReport,
    pub broken_links_count: usize,
    pub orphaned_notes_count: usize,
    pub recommendations: Vec<String>,
}

/// Health report exporter
pub struct HealthReportExporter;

impl HealthReportExporter {
    /// Export health report as JSON
    pub fn to_json(report: &HealthReport) -> Result<String> {
        to_json_string(report, "health report")
    }

    /// Export health report as CSV (single row)
    pub fn to_csv(report: &HealthReport) -> Result<String> {
        let csv = format!(
            "timestamp,vault_name,health_score,total_notes,total_links,broken_links,orphaned_notes,connectivity_rate,link_density,status\n\
             {},{},{},{},{},{},{},{:.3},{:.3},{}",
            report.timestamp,
            report.vault_name,
            report.health_score,
            report.total_notes,
            report.total_links,
            report.broken_links,
            report.orphaned_notes,
            report.connectivity_rate,
            report.link_density,
            report.status
        );

        Ok(csv)
    }
}

/// Broken links exporter
pub struct BrokenLinksExporter;

impl BrokenLinksExporter {
    /// Export broken links as JSON
    pub fn to_json(links: &[BrokenLinkRecord]) -> Result<String> {
        to_json_string(links, "broken links")
    }

    /// Export broken links as CSV
    pub fn to_csv(links: &[BrokenLinkRecord]) -> Result<String> {
        let mut csv = String::from("source_file,target,line,suggestions\n");

        for link in links {
            let suggestions = link.suggestions.join("|");
            csv.push_str(&format!(
                "\"{}\",\"{}\",{},\"{}\"\n",
                link.source_file, link.target, link.line, suggestions
            ));
        }

        Ok(csv)
    }
}

/// Vault statistics exporter
pub struct VaultStatsExporter;

impl VaultStatsExporter {
    /// Export vault stats as JSON
    pub fn to_json(stats: &VaultStatsRecord) -> Result<String> {
        to_json_string(stats, "vault stats")
    }

    /// Export vault stats as CSV
    pub fn to_csv(stats: &VaultStatsRecord) -> Result<String> {
        let csv = format!(
            "timestamp,vault_name,total_files,total_links,orphaned_files,average_links_per_file\n\
             {},{},{},{},{},{:.3}",
            stats.timestamp,
            stats.vault_name,
            stats.total_files,
            stats.total_links,
            stats.orphaned_files,
            stats.average_links_per_file
        );

        Ok(csv)
    }
}

/// Analysis report exporter
pub struct AnalysisReportExporter;

impl AnalysisReportExporter {
    /// Export analysis report as JSON
    pub fn to_json(report: &AnalysisReport) -> Result<String> {
        to_json_string(report, "analysis report")
    }

    /// Export analysis report as CSV (health metrics only, flattened)
    pub fn to_csv(report: &AnalysisReport) -> Result<String> {
        let csv = format!(
            "timestamp,vault_name,health_score,total_notes,total_links,broken_links,orphaned_notes,broken_links_count,recommendations\n\
             {},{},{},{},{},{},{},{},\"{}\"",
            report.timestamp,
            report.vault_name,
            report.health.health_score,
            report.health.total_notes,
            report.health.total_links,
            report.health.broken_links,
            report.health.orphaned_notes,
            report.broken_links_count,
            report.recommendations.join("|")
        );

        Ok(csv)
    }
}

/// Create a health report with recommendations
pub fn create_health_report(
    vault_name: &str,
    health_score: u8,
    total_notes: usize,
    total_links: usize,
    broken_links: usize,
    orphaned_notes: usize,
) -> HealthReport {
    let connectivity_rate = if total_notes > 0 {
        (total_notes - orphaned_notes) as f64 / total_notes as f64
    } else {
        0.0
    };

    let link_density = if total_notes > 1 {
        total_links as f64 / ((total_notes as f64) * (total_notes as f64 - 1.0))
    } else {
        0.0
    };

    let status = if health_score >= 80 {
        "Healthy".to_string()
    } else if health_score >= 60 {
        "Fair".to_string()
    } else if health_score >= 40 {
        "Needs Attention".to_string()
    } else {
        "Critical".to_string()
    };

    let mut recommendations = Vec::new();

    if broken_links > 0 {
        recommendations.push(format!(
            "Found {} broken links. Consider fixing or updating them.",
            broken_links
        ));
    }

    if orphaned_notes as f64 / total_notes as f64 > 0.1 {
        recommendations
            .push("Over 10% of notes are orphaned. Link them to improve connectivity.".to_string());
    }

    if link_density < 0.05 {
        recommendations.push(
            "Low link density. Consider adding more cross-references between notes.".to_string(),
        );
    }

    HealthReport {
        timestamp: Utc::now().to_rfc3339(),
        vault_name: vault_name.to_string(),
        health_score,
        total_notes,
        total_links,
        broken_links,
        orphaned_notes,
        connectivity_rate,
        link_density,
        status,
        recommendations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_report_creation() {
        let report = create_health_report("test", 85, 100, 150, 2, 5);
        assert_eq!(report.vault_name, "test");
        assert_eq!(report.health_score, 85);
        assert_eq!(report.status, "Healthy");
    }

    #[test]
    fn test_health_report_json_export() {
        let report = create_health_report("test", 85, 100, 150, 2, 5);
        let json = HealthReportExporter::to_json(&report).unwrap();
        assert!(json.contains("test"));
        assert!(json.contains("85"));
    }

    #[test]
    fn test_health_report_csv_export() {
        let report = create_health_report("test", 85, 100, 150, 2, 5);
        let csv = HealthReportExporter::to_csv(&report).unwrap();
        assert!(csv.contains("test"));
        assert!(csv.contains("85"));
    }

    #[test]
    fn test_broken_links_export() {
        let links = vec![BrokenLinkRecord {
            source_file: "file.md".to_string(),
            target: "missing.md".to_string(),
            line: 5,
            suggestions: vec!["existing.md".to_string()],
        }];

        let json = BrokenLinksExporter::to_json(&links).unwrap();
        assert!(json.contains("file.md"));
        assert!(json.contains("missing.md"));

        let csv = BrokenLinksExporter::to_csv(&links).unwrap();
        assert!(csv.contains("file.md"));
    }

    #[test]
    fn test_vault_stats_export() {
        let stats = VaultStatsRecord {
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            vault_name: "test".to_string(),
            total_files: 100,
            total_links: 150,
            orphaned_files: 5,
            average_links_per_file: 1.5,
        };

        let json = VaultStatsExporter::to_json(&stats).unwrap();
        assert!(json.contains("100"));

        let csv = VaultStatsExporter::to_csv(&stats).unwrap();
        assert!(csv.contains("100"));
    }
}
