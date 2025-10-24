//! Content validation system.
//!
//! Provides validators for markdown content, frontmatter, links, and other
//! vault elements. Extensible validator trait allows custom validation rules.

use crate::models::{Frontmatter, Link, VaultFile};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Severity level for validation issues
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Severity {
    /// Informational message (not a problem)
    Info,
    /// Warning (should be addressed but not critical)
    Warning,
    /// Error (should be fixed)
    Error,
    /// Critical error (must be fixed)
    Critical,
}

impl Severity {
    /// Check if this severity is considered a failure
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Error | Self::Critical)
    }
}

/// A validation issue found in content
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    /// Severity of the issue
    pub severity: Severity,
    /// Category of the issue
    pub category: String,
    /// Human-readable message
    pub message: String,
    /// Location in the file (line number, optional)
    pub line: Option<usize>,
    /// Suggested fix (optional)
    pub suggestion: Option<String>,
}

impl ValidationIssue {
    /// Create a new validation issue
    pub fn new(
        severity: Severity,
        category: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            category: category.into(),
            message: message.into(),
            line: None,
            suggestion: None,
        }
    }

    /// Set the line number
    pub fn with_line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }

    /// Set a suggested fix
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }
}

/// Result of validating content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Whether validation passed (no errors/critical issues)
    pub passed: bool,
    /// All issues found
    pub issues: Vec<ValidationIssue>,
    /// Summary counts by severity
    pub summary: ValidationSummary,
}

/// Summary of validation results
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ValidationSummary {
    pub info_count: usize,
    pub warning_count: usize,
    pub error_count: usize,
    pub critical_count: usize,
}

impl ValidationReport {
    /// Create a new validation report
    pub fn new() -> Self {
        Self {
            passed: true,
            issues: Vec::new(),
            summary: ValidationSummary::default(),
        }
    }

    /// Add an issue to the report
    pub fn add_issue(&mut self, issue: ValidationIssue) {
        // Update summary
        match issue.severity {
            Severity::Info => self.summary.info_count += 1,
            Severity::Warning => self.summary.warning_count += 1,
            Severity::Error => {
                self.summary.error_count += 1;
                self.passed = false;
            }
            Severity::Critical => {
                self.summary.critical_count += 1;
                self.passed = false;
            }
        }

        self.issues.push(issue);
    }

    /// Merge another report into this one
    pub fn merge(&mut self, other: ValidationReport) {
        for issue in other.issues {
            self.add_issue(issue);
        }
    }

    /// Get issues by severity
    pub fn issues_by_severity(&self, severity: Severity) -> Vec<&ValidationIssue> {
        self.issues
            .iter()
            .filter(|i| i.severity == severity)
            .collect()
    }

    /// Check if there are any failures
    pub fn has_failures(&self) -> bool {
        !self.passed
    }

    /// Total issue count
    pub fn total_issues(&self) -> usize {
        self.issues.len()
    }
}

impl Default for ValidationReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait for content validators
pub trait Validator {
    /// Validate content and return a report
    fn validate(&self, file: &VaultFile) -> ValidationReport;

    /// Name of this validator
    fn name(&self) -> &str;
}

/// Validates frontmatter structure and required fields
#[derive(Debug, Clone)]
pub struct FrontmatterValidator {
    required_fields: HashSet<String>,
}

impl FrontmatterValidator {
    /// Create a new frontmatter validator
    pub fn new() -> Self {
        Self {
            required_fields: HashSet::new(),
        }
    }

    /// Require a specific field to be present
    pub fn require_field(mut self, field: impl Into<String>) -> Self {
        self.required_fields.insert(field.into());
        self
    }

    /// Validate a frontmatter object
    fn validate_frontmatter(&self, frontmatter: &Frontmatter) -> ValidationReport {
        let mut report = ValidationReport::new();

        // Check required fields
        for field in &self.required_fields {
            if !frontmatter.data.contains_key(field) {
                report.add_issue(
                    ValidationIssue::new(
                        Severity::Error,
                        "frontmatter",
                        format!("Missing required field: {}", field),
                    )
                    .with_suggestion(format!("Add '{}:' to frontmatter", field)),
                );
            }
        }

        // Validate tags format if present
        if let Some(tags_value) = frontmatter.data.get("tags") {
            match tags_value {
                serde_json::Value::Array(arr) => {
                    for (idx, tag) in arr.iter().enumerate() {
                        if !tag.is_string() {
                            report.add_issue(ValidationIssue::new(
                                Severity::Warning,
                                "frontmatter",
                                format!("Tag at index {} is not a string", idx),
                            ));
                        }
                    }
                }
                serde_json::Value::String(_) => {
                    // Single string tag is OK
                }
                _ => {
                    report.add_issue(ValidationIssue::new(
                        Severity::Warning,
                        "frontmatter",
                        "Tags should be an array of strings or a single string",
                    ));
                }
            }
        }

        report
    }
}

impl Default for FrontmatterValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl Validator for FrontmatterValidator {
    fn validate(&self, file: &VaultFile) -> ValidationReport {
        if let Some(ref frontmatter) = file.frontmatter {
            self.validate_frontmatter(frontmatter)
        } else if !self.required_fields.is_empty() {
            let mut report = ValidationReport::new();
            report.add_issue(ValidationIssue::new(
                Severity::Error,
                "frontmatter",
                "File has no frontmatter but required fields are specified",
            ));
            report
        } else {
            ValidationReport::new()
        }
    }

    fn name(&self) -> &str {
        "FrontmatterValidator"
    }
}

/// Validates link syntax and format
#[derive(Debug, Clone)]
pub struct LinkValidator {
    check_fragments: bool,
}

impl LinkValidator {
    /// Create a new link validator
    pub fn new() -> Self {
        Self {
            check_fragments: true,
        }
    }

    /// Enable or disable fragment validation
    pub fn check_fragments(mut self, check: bool) -> Self {
        self.check_fragments = check;
        self
    }

    /// Validate a single link
    fn validate_link(&self, link: &Link, line: usize) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();

        // Check for empty target
        if link.target.is_empty() {
            issues.push(
                ValidationIssue::new(Severity::Error, "link", "Empty link target")
                    .with_line(line)
                    .with_suggestion("Provide a target for the link or remove it"),
            );
        }

        // Check for suspicious characters in wikilinks
        if link.target.contains("http://") || link.target.contains("https://") {
            issues.push(
                ValidationIssue::new(
                    Severity::Warning,
                    "link",
                    format!("URL in wikilink syntax: {}", link.target),
                )
                .with_line(line)
                .with_suggestion("Use markdown link syntax [text](url) for external links"),
            );
        }

        // Check for fragments without base
        if self.check_fragments && link.target.starts_with('#') && link.target.len() > 1 {
            issues.push(
                ValidationIssue::new(
                    Severity::Info,
                    "link",
                    format!("Fragment-only link: {}", link.target),
                )
                .with_line(line)
                .with_suggestion("Fragment links reference headings in the current file"),
            );
        }

        issues
    }
}

impl Default for LinkValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl Validator for LinkValidator {
    fn validate(&self, file: &VaultFile) -> ValidationReport {
        let mut report = ValidationReport::new();

        for link in &file.links {
            let line = link.position.line;

            for issue in self.validate_link(link, line) {
                report.add_issue(issue);
            }
        }

        report
    }

    fn name(&self) -> &str {
        "LinkValidator"
    }
}

/// Validates file content structure
#[derive(Debug, Clone)]
pub struct ContentValidator {
    min_length: Option<usize>,
    max_length: Option<usize>,
    require_heading: bool,
}

impl ContentValidator {
    /// Create a new content validator
    pub fn new() -> Self {
        Self {
            min_length: None,
            max_length: None,
            require_heading: false,
        }
    }

    /// Set minimum content length
    pub fn min_length(mut self, min: usize) -> Self {
        self.min_length = Some(min);
        self
    }

    /// Set maximum content length
    pub fn max_length(mut self, max: usize) -> Self {
        self.max_length = Some(max);
        self
    }

    /// Require at least one heading
    pub fn require_heading(mut self) -> Self {
        self.require_heading = true;
        self
    }
}

impl Default for ContentValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl Validator for ContentValidator {
    fn validate(&self, file: &VaultFile) -> ValidationReport {
        let mut report = ValidationReport::new();

        let content_len = file.content.len();

        // Check minimum length
        if let Some(min) = self.min_length
            && content_len < min
        {
            report.add_issue(
                ValidationIssue::new(
                    Severity::Warning,
                    "content",
                    format!(
                        "Content too short: {} bytes (minimum: {})",
                        content_len, min
                    ),
                )
                .with_suggestion("Add more content to the note"),
            );
        }

        // Check maximum length
        if let Some(max) = self.max_length
            && content_len > max
        {
            report.add_issue(
                ValidationIssue::new(
                    Severity::Warning,
                    "content",
                    format!("Content too long: {} bytes (maximum: {})", content_len, max),
                )
                .with_suggestion("Consider splitting into multiple notes"),
            );
        }

        // Check for heading if required
        if self.require_heading && file.headings.is_empty() {
            report.add_issue(
                ValidationIssue::new(Severity::Warning, "content", "No headings found")
                    .with_suggestion("Add at least one heading (# Title)"),
            );
        }

        report
    }

    fn name(&self) -> &str {
        "ContentValidator"
    }
}

/// Composite validator that runs multiple validators
pub struct CompositeValidator {
    validators: Vec<Box<dyn Validator>>,
}

impl CompositeValidator {
    /// Create a new composite validator
    pub fn new() -> Self {
        Self {
            validators: Vec::new(),
        }
    }

    /// Add a validator
    pub fn add_validator(mut self, validator: Box<dyn Validator>) -> Self {
        self.validators.push(validator);
        self
    }

    /// Create a default validator with common rules
    pub fn default_rules() -> Self {
        Self::new()
            .add_validator(Box::new(FrontmatterValidator::new()))
            .add_validator(Box::new(LinkValidator::new()))
            .add_validator(Box::new(ContentValidator::new()))
    }
}

impl Default for CompositeValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl Validator for CompositeValidator {
    fn validate(&self, file: &VaultFile) -> ValidationReport {
        let mut report = ValidationReport::new();

        for validator in &self.validators {
            let sub_report = validator.validate(file);
            report.merge(sub_report);
        }

        report
    }

    fn name(&self) -> &str {
        "CompositeValidator"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourcePosition;
    use crate::models::{FileMetadata, LinkType};
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn create_test_file() -> VaultFile {
        VaultFile {
            path: PathBuf::from("test.md"),
            content: "# Test\nSome content".to_string(),
            metadata: FileMetadata {
                path: PathBuf::from("test.md"),
                size: 20,
                created_at: 0.0,
                modified_at: 0.0,
                checksum: "abc123".to_string(),
                is_attachment: false,
            },
            frontmatter: None,
            headings: Vec::new(),
            links: Vec::new(),
            backlinks: HashSet::new(),
            blocks: Vec::new(),
            tags: Vec::new(),
            callouts: Vec::new(),
            tasks: Vec::new(),
            is_parsed: true,
            parse_error: None,
            last_parsed: Some(0.0),
        }
    }

    #[test]
    fn test_validation_issue_creation() {
        let issue = ValidationIssue::new(Severity::Error, "test", "Test message");
        assert_eq!(issue.severity, Severity::Error);
        assert_eq!(issue.category, "test");
        assert_eq!(issue.message, "Test message");
        assert!(issue.line.is_none());
        assert!(issue.suggestion.is_none());
    }

    #[test]
    fn test_validation_issue_with_line() {
        let issue = ValidationIssue::new(Severity::Error, "test", "Test").with_line(42);
        assert_eq!(issue.line, Some(42));
    }

    #[test]
    fn test_validation_issue_with_suggestion() {
        let issue = ValidationIssue::new(Severity::Error, "test", "Test").with_suggestion("Fix it");
        assert_eq!(issue.suggestion, Some("Fix it".to_string()));
    }

    #[test]
    fn test_severity_is_failure() {
        assert!(!Severity::Info.is_failure());
        assert!(!Severity::Warning.is_failure());
        assert!(Severity::Error.is_failure());
        assert!(Severity::Critical.is_failure());
    }

    #[test]
    fn test_validation_report_creation() {
        let report = ValidationReport::new();
        assert!(report.passed);
        assert_eq!(report.issues.len(), 0);
        assert_eq!(report.summary.error_count, 0);
    }

    #[test]
    fn test_validation_report_add_issue() {
        let mut report = ValidationReport::new();
        report.add_issue(ValidationIssue::new(Severity::Warning, "test", "Warning"));
        assert!(report.passed);
        assert_eq!(report.summary.warning_count, 1);

        report.add_issue(ValidationIssue::new(Severity::Error, "test", "Error"));
        assert!(!report.passed);
        assert_eq!(report.summary.error_count, 1);
    }

    #[test]
    fn test_validation_report_merge() {
        let mut report1 = ValidationReport::new();
        report1.add_issue(ValidationIssue::new(Severity::Warning, "test", "Warning"));

        let mut report2 = ValidationReport::new();
        report2.add_issue(ValidationIssue::new(Severity::Error, "test", "Error"));

        report1.merge(report2);
        assert!(!report1.passed);
        assert_eq!(report1.summary.warning_count, 1);
        assert_eq!(report1.summary.error_count, 1);
        assert_eq!(report1.total_issues(), 2);
    }

    #[test]
    fn test_frontmatter_validator_no_requirements() {
        let validator = FrontmatterValidator::new();
        let file = create_test_file();
        let report = validator.validate(&file);
        assert!(report.passed);
    }

    #[test]
    fn test_frontmatter_validator_missing_required_field() {
        let validator = FrontmatterValidator::new().require_field("title");
        let file = create_test_file();
        let report = validator.validate(&file);
        assert!(!report.passed);
        assert_eq!(report.summary.error_count, 1);
    }

    #[test]
    fn test_frontmatter_validator_with_required_field() {
        use std::collections::HashMap;

        let validator = FrontmatterValidator::new().require_field("title");
        let mut file = create_test_file();
        let mut data = HashMap::new();
        data.insert("title".to_string(), serde_json::json!("Test Title"));
        let frontmatter = Frontmatter {
            data,
            position: SourcePosition::start(),
        };
        file.frontmatter = Some(frontmatter);

        let report = validator.validate(&file);
        assert!(report.passed);
    }

    #[test]
    fn test_link_validator_empty_target() {
        let validator = LinkValidator::new();
        let mut file = create_test_file();
        file.links.push(Link {
            type_: LinkType::WikiLink,
            source_file: PathBuf::from("test.md"),
            target: "".to_string(),
            display_text: None,
            position: SourcePosition::start(),
            resolved_target: None,
            is_valid: false,
        });

        let report = validator.validate(&file);
        assert!(!report.passed);
        assert_eq!(report.summary.error_count, 1);
    }

    #[test]
    fn test_link_validator_url_in_wikilink() {
        let validator = LinkValidator::new();
        let mut file = create_test_file();
        file.links.push(Link {
            type_: LinkType::WikiLink,
            source_file: PathBuf::from("test.md"),
            target: "https://example.com".to_string(),
            display_text: None,
            position: SourcePosition::start(),
            resolved_target: None,
            is_valid: false,
        });

        let report = validator.validate(&file);
        assert!(report.passed); // Warning, not error
        assert_eq!(report.summary.warning_count, 1);
    }

    #[test]
    fn test_link_validator_fragment_only() {
        let validator = LinkValidator::new();
        let mut file = create_test_file();
        file.links.push(Link {
            type_: LinkType::WikiLink,
            source_file: PathBuf::from("test.md"),
            target: "#heading".to_string(),
            display_text: None,
            position: SourcePosition::start(),
            resolved_target: None,
            is_valid: false,
        });

        let report = validator.validate(&file);
        assert!(report.passed);
        assert_eq!(report.summary.info_count, 1);
    }

    #[test]
    fn test_content_validator_min_length() {
        let validator = ContentValidator::new().min_length(100);
        let file = create_test_file();
        let report = validator.validate(&file);
        assert!(report.passed); // Warning, not error
        assert_eq!(report.summary.warning_count, 1);
    }

    #[test]
    fn test_content_validator_max_length() {
        let validator = ContentValidator::new().max_length(10);
        let file = create_test_file();
        let report = validator.validate(&file);
        assert!(report.passed); // Warning, not error
        assert_eq!(report.summary.warning_count, 1);
    }

    #[test]
    fn test_content_validator_require_heading() {
        let validator = ContentValidator::new().require_heading();
        let mut file = create_test_file();
        file.headings.clear(); // Remove headings

        let report = validator.validate(&file);
        assert!(report.passed); // Warning, not error
        assert_eq!(report.summary.warning_count, 1);
    }

    #[test]
    fn test_composite_validator() {
        let validator = CompositeValidator::new()
            .add_validator(Box::new(FrontmatterValidator::new().require_field("title")))
            .add_validator(Box::new(LinkValidator::new()))
            .add_validator(Box::new(ContentValidator::new().min_length(100)));

        let file = create_test_file();
        let report = validator.validate(&file);

        // Should have issues from multiple validators
        assert!(!report.passed); // Frontmatter error
        assert!(report.summary.error_count > 0);
        assert!(report.summary.warning_count > 0);
    }

    #[test]
    fn test_validation_report_issues_by_severity() {
        let mut report = ValidationReport::new();
        report.add_issue(ValidationIssue::new(Severity::Warning, "test", "W1"));
        report.add_issue(ValidationIssue::new(Severity::Error, "test", "E1"));
        report.add_issue(ValidationIssue::new(Severity::Warning, "test", "W2"));

        let warnings = report.issues_by_severity(Severity::Warning);
        assert_eq!(warnings.len(), 2);

        let errors = report.issues_by_severity(Severity::Error);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_validator_name() {
        let frontmatter = FrontmatterValidator::new();
        assert_eq!(frontmatter.name(), "FrontmatterValidator");

        let link = LinkValidator::new();
        assert_eq!(link.name(), "LinkValidator");

        let content = ContentValidator::new();
        assert_eq!(content.name(), "ContentValidator");
    }
}
