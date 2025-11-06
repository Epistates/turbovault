//! Content validation tools

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_core::{
    CompositeValidator, ContentValidator, FrontmatterValidator, LinkValidator, Result, Severity,
    ValidationReport, Validator,
};
use turbovault_vault::VaultManager;

/// Validation tools context
pub struct ValidationTools {
    pub manager: Arc<VaultManager>,
}

/// Simplified validation issue for JSON serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationIssueInfo {
    pub severity: String,
    pub category: String,
    pub message: String,
    pub line: Option<usize>,
    pub suggestion: Option<String>,
}

/// Simplified validation report for JSON serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReportInfo {
    pub passed: bool,
    pub total_issues: usize,
    pub info_count: usize,
    pub warning_count: usize,
    pub error_count: usize,
    pub critical_count: usize,
    pub issues: Vec<ValidationIssueInfo>,
}

impl ValidationTools {
    /// Create new validation tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Validate a single note
    pub async fn validate_note(&self, path: &str) -> Result<ValidationReportInfo> {
        let file_path = PathBuf::from(path);
        let vault_file = self.manager.parse_file(&file_path).await?;

        let validator = CompositeValidator::default_rules();
        let report = validator.validate(&vault_file);

        Ok(Self::convert_report(report))
    }

    /// Validate note with custom rules
    pub async fn validate_note_with_rules(
        &self,
        path: &str,
        require_frontmatter: bool,
        required_fields: Vec<String>,
        check_links: bool,
        min_length: Option<usize>,
    ) -> Result<ValidationReportInfo> {
        let file_path = PathBuf::from(path);
        let vault_file = self.manager.parse_file(&file_path).await?;

        let mut validator = CompositeValidator::new();

        // Add frontmatter validator if required
        if require_frontmatter || !required_fields.is_empty() {
            let mut fm_validator = FrontmatterValidator::new();
            for field in required_fields {
                fm_validator = fm_validator.require_field(field);
            }
            validator = validator.add_validator(Box::new(fm_validator));
        }

        // Add link validator if requested
        if check_links {
            validator = validator.add_validator(Box::new(LinkValidator::new()));
        }

        // Add content validator if min length specified
        if let Some(min) = min_length {
            let content_validator = ContentValidator::new().min_length(min);
            validator = validator.add_validator(Box::new(content_validator));
        }

        let report = validator.validate(&vault_file);

        Ok(Self::convert_report(report))
    }

    /// Validate entire vault (batch validation)
    pub async fn validate_vault(&self) -> Result<ValidationReportInfo> {
        let files = self.manager.scan_vault().await?;

        let validator = CompositeValidator::default_rules();
        let mut combined_report = ValidationReport::new();

        for file_path in files {
            if let Ok(vault_file) = self.manager.parse_file(&file_path).await {
                let report = validator.validate(&vault_file);
                combined_report.merge(report);
            }
        }

        Ok(Self::convert_report(combined_report))
    }

    /// Validate vault with issue limit (for large vaults)
    pub async fn validate_vault_quick(&self, max_issues: usize) -> Result<ValidationReportInfo> {
        let files = self.manager.scan_vault().await?;

        let validator = CompositeValidator::default_rules();
        let mut combined_report = ValidationReport::new();

        for file_path in files {
            // Stop if we've hit the max issues
            if combined_report.total_issues() >= max_issues {
                break;
            }

            if let Ok(vault_file) = self.manager.parse_file(&file_path).await {
                let report = validator.validate(&vault_file);
                combined_report.merge(report);
            }
        }

        Ok(Self::convert_report(combined_report))
    }

    /// Convert ValidationReport to serializable format
    fn convert_report(report: ValidationReport) -> ValidationReportInfo {
        ValidationReportInfo {
            passed: report.passed,
            total_issues: report.total_issues(),
            info_count: report.summary.info_count,
            warning_count: report.summary.warning_count,
            error_count: report.summary.error_count,
            critical_count: report.summary.critical_count,
            issues: report
                .issues
                .into_iter()
                .map(|issue| ValidationIssueInfo {
                    severity: Self::severity_to_string(issue.severity),
                    category: issue.category,
                    message: issue.message,
                    line: issue.line,
                    suggestion: issue.suggestion,
                })
                .collect(),
        }
    }

    /// Convert Severity enum to string
    fn severity_to_string(severity: Severity) -> String {
        match severity {
            Severity::Info => "info".to_string(),
            Severity::Warning => "warning".to_string(),
            Severity::Error => "error".to_string(),
            Severity::Critical => "critical".to_string(),
        }
    }
}
