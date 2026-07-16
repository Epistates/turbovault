//! Public contract tests for content validation workflows.

use std::sync::Arc;

use tempfile::TempDir;
use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::ValidationTools;
use turbovault_vault::VaultManager;

async fn setup_vault(files: &[(&str, &str)]) -> (TempDir, Arc<VaultManager>) {
    let temp = TempDir::new().expect("temporary vault");
    for (path, content) in files {
        let full_path = temp.path().join(path);
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("create note parent");
        }
        tokio::fs::write(full_path, content)
            .await
            .expect("write note fixture");
    }

    let mut config = ConfigProfile::Development.create_config();
    config.vaults.push(
        VaultConfig::builder("validation", temp.path())
            .build()
            .expect("vault config"),
    );
    let manager = Arc::new(VaultManager::new(config).expect("vault manager"));
    manager.initialize().await.expect("initialize vault");

    (temp, manager)
}

#[tokio::test]
async fn default_validation_reports_link_warnings_and_information() {
    let (_temp, manager) = setup_vault(&[(
        "links.md",
        "# Links\n\nSee [[https://example.com]] and [[#Links]].\n",
    )])
    .await;
    let tools = ValidationTools::new(manager);

    let report = tools
        .validate_note("links.md")
        .await
        .expect("validate note");

    assert!(report.passed);
    assert_eq!(report.total_issues, 2);
    assert_eq!(report.info_count, 1);
    assert_eq!(report.warning_count, 1);
    assert_eq!(report.error_count, 0);
    assert_eq!(report.critical_count, 0);
    assert!(report.issues.iter().all(|issue| issue.category == "link"));
    assert!(report.issues.iter().all(|issue| issue.line.is_some()));
    assert!(report.issues.iter().all(|issue| issue.suggestion.is_some()));
}

#[tokio::test]
async fn custom_validation_enforces_each_requested_rule() {
    let note = r#"---
tags:
  - valid
  - 42
---
# Rules

See [[https://example.com]] and [[#Rules]].
"#;
    let (_temp, manager) = setup_vault(&[("rules.md", note)]).await;
    let tools = ValidationTools::new(manager);

    let report = tools
        .validate_note_with_rules("rules.md", true, vec!["title".to_string()], true, Some(500))
        .await
        .expect("validate custom rules");

    assert!(!report.passed);
    assert_eq!(report.total_issues, 5);
    assert_eq!(report.info_count, 1);
    assert_eq!(report.warning_count, 3);
    assert_eq!(report.error_count, 1);
    assert_eq!(report.critical_count, 0);
    assert!(report.issues.iter().any(|issue| {
        issue.severity == "error"
            && issue.category == "frontmatter"
            && issue.message == "Missing required field: title"
    }));
    assert!(report.issues.iter().any(|issue| {
        issue.severity == "warning"
            && issue.category == "content"
            && issue.message.contains("Content too short")
    }));
}

#[tokio::test]
async fn require_frontmatter_works_without_required_field_names() {
    let (_temp, manager) = setup_vault(&[("plain.md", "# Plain\n")]).await;
    let tools = ValidationTools::new(manager);

    let report = tools
        .validate_note_with_rules("plain.md", true, Vec::new(), false, None)
        .await
        .expect("validate frontmatter requirement");

    assert!(!report.passed);
    assert_eq!(report.error_count, 1);
    assert_eq!(report.total_issues, 1);
    assert_eq!(report.issues[0].category, "frontmatter");
    assert_eq!(report.issues[0].message, "File has no required frontmatter");
}

#[tokio::test]
async fn validation_without_custom_rules_returns_an_empty_report() {
    let (_temp, manager) = setup_vault(&[("plain.md", "Plain content\n")]).await;
    let tools = ValidationTools::new(manager);

    let report = tools
        .validate_note_with_rules("plain.md", false, Vec::new(), false, None)
        .await
        .expect("validate without rules");

    assert!(report.passed);
    assert_eq!(report.total_issues, 0);
    assert!(report.issues.is_empty());
}

#[tokio::test]
async fn vault_validation_aggregates_reports_and_honors_the_quick_limit() {
    let content = "# Links\n\nSee [[https://example.com]] and [[#Links]].\n";
    let (_temp, manager) =
        setup_vault(&[("first.md", content), ("nested/second.md", content)]).await;
    let tools = ValidationTools::new(manager);

    let full = tools.validate_vault().await.expect("validate vault");
    assert!(full.passed);
    assert_eq!(full.total_issues, 4);
    assert_eq!(full.warning_count, 2);
    assert_eq!(full.info_count, 2);

    let limited = tools
        .validate_vault_quick(1)
        .await
        .expect("quick validation");
    assert_eq!(limited.total_issues, 1);
    assert_eq!(
        limited.info_count + limited.warning_count + limited.error_count + limited.critical_count,
        1
    );

    let empty = tools
        .validate_vault_quick(0)
        .await
        .expect("zero-limit validation");
    assert!(empty.passed);
    assert_eq!(empty.total_issues, 0);
}

#[tokio::test]
async fn missing_note_returns_an_error() {
    let (_temp, manager) = setup_vault(&[]).await;
    let tools = ValidationTools::new(manager);

    assert!(tools.validate_note("missing.md").await.is_err());
    assert!(
        tools
            .validate_note_with_rules("missing.md", true, Vec::new(), true, Some(1))
            .await
            .is_err()
    );
}
