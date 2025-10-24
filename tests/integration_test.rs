//! Integration tests for turbovault

use turbo_vault_core::prelude::*;
use turbo_vault_vault::VaultManager;
use turbo_vault_tools::{ExportTools, MetadataTools};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

// ==================== Export Tests ====================

#[tokio::test]
async fn test_export_health_report_json() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let tools = ExportTools::new(manager);
    let report = tools.export_health_report("json").await.unwrap();

    assert!(report.contains("\"vault_name\""));
    assert!(report.contains("\"health_score\""));
}

#[tokio::test]
async fn test_export_health_report_csv() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let tools = ExportTools::new(manager);
    let report = tools.export_health_report("csv").await.unwrap();

    assert!(report.contains("timestamp,vault_name,health_score"));
}

#[tokio::test]
async fn test_export_vault_stats_json() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let tools = ExportTools::new(manager);
    let stats = tools.export_vault_stats("json").await.unwrap();

    assert!(stats.contains("\"total_files\""));
    assert!(stats.contains("\"total_links\""));
}

#[tokio::test]
async fn test_export_vault_stats_csv() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let tools = ExportTools::new(manager);
    let stats = tools.export_vault_stats("csv").await.unwrap();

    assert!(stats.contains("timestamp,vault_name,total_files"));
}

#[tokio::test]
async fn test_export_broken_links_json() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let tools = ExportTools::new(manager);
    let links = tools.export_broken_links("json").await.unwrap();

    // Empty list is valid
    assert!(links.contains("[]"));
}

#[tokio::test]
async fn test_export_analysis_report_json() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let tools = ExportTools::new(manager);
    let report = tools.export_analysis_report("json").await.unwrap();

    assert!(report.contains("\"vault_name\""));
    assert!(report.contains("\"recommendations\""));
}

#[tokio::test]
async fn test_export_invalid_format() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let tools = ExportTools::new(manager);
    let result = tools.export_health_report("invalid").await;

    assert!(result.is_err());
}

// ==================== Metadata Query Tests ====================

#[tokio::test]
async fn test_query_metadata_equals() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    // Create test files with different statuses
    manager
        .write_file(
            &PathBuf::from("draft1.md"),
            "---\nstatus: draft\nauthor: Jane\n---\n# Draft Note 1\nContent here",
        )
        .await
        .unwrap();

    manager
        .write_file(
            &PathBuf::from("draft2.md"),
            "---\nstatus: draft\nauthor: Bob\n---\n# Draft Note 2\nContent here",
        )
        .await
        .unwrap();

    manager
        .write_file(
            &PathBuf::from("published.md"),
            "---\nstatus: published\nauthor: Jane\n---\n# Published Note\nContent here",
        )
        .await
        .unwrap();

    // Reinitialize to pick up new files
    manager.initialize().await.unwrap();

    let tools = MetadataTools::new(manager.clone());
    let result = tools.query_metadata(r#"status: "draft""#).await.unwrap();

    let matched = result.get("matched").and_then(|v| v.as_u64()).unwrap_or(0);
    assert_eq!(matched, 2, "Should match exactly 2 draft files");
}

#[tokio::test]
async fn test_query_metadata_numeric_gt() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    manager
        .write_file(&PathBuf::from("p5.md"), "---\npriority: 5\n---\n")
        .await
        .unwrap();
    manager
        .write_file(&PathBuf::from("p3.md"), "---\npriority: 3\n---\n")
        .await
        .unwrap();
    manager
        .write_file(&PathBuf::from("p1.md"), "---\npriority: 1\n---\n")
        .await
        .unwrap();

    manager.initialize().await.unwrap();

    let tools = MetadataTools::new(manager.clone());
    let result = tools.query_metadata("priority > 3").await.unwrap();

    let matched = result.get("matched").and_then(|v| v.as_u64()).unwrap_or(0);
    assert_eq!(matched, 1, "Should match exactly 1 file with priority > 3");
}

#[tokio::test]
async fn test_query_metadata_contains() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    manager
        .write_file(
            &PathBuf::from("imp1.md"),
            "---\ntags: important task\n---\n",
        )
        .await
        .unwrap();
    manager
        .write_file(
            &PathBuf::from("imp2.md"),
            "---\ntags: urgent and important\n---\n",
        )
        .await
        .unwrap();
    manager
        .write_file(&PathBuf::from("normal.md"), "---\ntags: routine\n---\n")
        .await
        .unwrap();

    manager.initialize().await.unwrap();

    let tools = MetadataTools::new(manager.clone());
    let result = tools.query_metadata(r#"tags: contains("important")"#).await.unwrap();

    let matched = result.get("matched").and_then(|v| v.as_u64()).unwrap_or(0);
    assert_eq!(matched, 2, "Should match 2 files containing 'important'");
}

#[tokio::test]
async fn test_query_metadata_no_matches() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    manager
        .write_file(&PathBuf::from("file1.md"), "---\nstatus: active\n---\n")
        .await
        .unwrap();

    manager.initialize().await.unwrap();

    let tools = MetadataTools::new(manager.clone());
    let result = tools
        .query_metadata(r#"status: "nonexistent""#)
        .await
        .unwrap();

    let matched = result.get("matched").and_then(|v| v.as_u64()).unwrap_or(0);
    assert_eq!(matched, 0, "Should match 0 files");
}

#[tokio::test]
async fn test_get_metadata_value_simple() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    manager
        .write_file(
            &PathBuf::from("test.md"),
            "---\nauthor: Jane Doe\ntitle: Test Note\n---\nContent",
        )
        .await
        .unwrap();

    manager.initialize().await.unwrap();

    let tools = MetadataTools::new(manager.clone());
    let result = tools.get_metadata_value("test.md", "author").await.unwrap();

    let value = result.get("value").and_then(|v| v.as_str()).unwrap();
    assert_eq!(value, "Jane Doe");
}

#[tokio::test]
async fn test_get_metadata_value_nested() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    manager
        .write_file(
            &PathBuf::from("test.md"),
            "---\nproject:\n  status: active\n  name: MyProject\n---\nContent",
        )
        .await
        .unwrap();

    manager.initialize().await.unwrap();

    let tools = MetadataTools::new(manager.clone());
    let result = tools
        .get_metadata_value("test.md", "project.status")
        .await
        .unwrap();

    let value = result.get("value").and_then(|v| v.as_str()).unwrap();
    assert_eq!(value, "active");
}

#[tokio::test]
async fn test_get_metadata_value_missing_key() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    manager
        .write_file(&PathBuf::from("test.md"), "---\nauthor: Jane\n---\nContent")
        .await
        .unwrap();

    manager.initialize().await.unwrap();

    let tools = MetadataTools::new(manager.clone());
    let result = tools.get_metadata_value("test.md", "nonexistent_key").await;

    assert!(result.is_err(), "Should return error for missing key");
}

#[tokio::test]
async fn test_get_metadata_value_no_frontmatter() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    // Write file without frontmatter
    manager
        .write_file(&PathBuf::from("no_front.md"), "# Just Content\nNo frontmatter here")
        .await
        .unwrap();

    manager.initialize().await.unwrap();

    let tools = MetadataTools::new(manager.clone());
    let result = tools.get_metadata_value("no_front.md", "author").await;

    assert!(result.is_err(), "Should return error for file without frontmatter");
}

// ==================== Edit File Tests ====================

#[tokio::test]
async fn test_edit_file_simple_replacement() {
    use turbo_vault_vault::compute_hash;

    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = VaultManager::new(config).unwrap();

    // Create initial file
    let path = PathBuf::from("test.md");
    let initial_content = "# Hello World\n\nThis is a test file.\n\nGoodbye.";
    manager.write_file(&path, initial_content).await.unwrap();

    // Create edit blocks
    let edits = r#"<<<<<<< SEARCH
This is a test file.
=======
This file has been edited!
>>>>>>> REPLACE"#;

    // Apply edit
    let hash = compute_hash(initial_content);
    let result = manager
        .edit_file(&path, edits, Some(&hash), false)
        .await
        .unwrap();

    assert!(result.success);
    assert_eq!(result.blocks_applied, 1);
    assert_ne!(result.old_hash, result.new_hash);

    // Verify content changed
    let new_content = manager.read_file(&path).await.unwrap();
    assert!(new_content.contains("This file has been edited!"));
    assert!(!new_content.contains("This is a test file."));
}

#[tokio::test]
async fn test_edit_file_dry_run() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = VaultManager::new(config).unwrap();

    let path = PathBuf::from("dry_run.md");
    let initial = "# Test\n\nOriginal content";
    manager.write_file(&path, initial).await.unwrap();

    let edits = r#"<<<<<<< SEARCH
Original content
=======
Modified content
>>>>>>> REPLACE"#;

    // Dry run
    let result = manager.edit_file(&path, edits, None, true).await.unwrap();

    assert!(result.success);
    assert!(result.diff_preview.is_some());

    // Verify file NOT changed
    let content = manager.read_file(&path).await.unwrap();
    assert_eq!(content, initial);
}
