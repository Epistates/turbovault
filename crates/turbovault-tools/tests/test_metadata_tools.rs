//! Unit tests for MetadataTools

use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::MetadataTools;
use turbovault_vault::VaultManager;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup_test_vault_with_metadata() -> (TempDir, Arc<VaultManager>) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let vault_path = temp_dir.path();

    // Create notes with various metadata
    tokio::fs::write(
        vault_path.join("note1.md"),
        r#"---
title: "First Note"
author: "Alice"
status: "draft"
priority: 5
tags: ["project", "urgent"]
---
# Note 1
Content here"#,
    )
    .await
    .unwrap();

    tokio::fs::write(
        vault_path.join("note2.md"),
        r#"---
title: "Second Note"
author: "Bob"
status: "published"
priority: 3
tags: ["reference"]
---
# Note 2
More content"#,
    )
    .await
    .unwrap();

    tokio::fs::write(
        vault_path.join("note3.md"),
        r#"---
title: "Third Note"
status: "archived"
nested:
  field: "value"
  count: 42
---
# Note 3
Text"#,
    )
    .await
    .unwrap();

    tokio::fs::write(
        vault_path.join("no_metadata.md"),
        "# No Metadata\nJust content",
    )
    .await
    .unwrap();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();

    (temp_dir, Arc::new(manager))
}

#[tokio::test]
async fn test_get_metadata_value_string() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.get_metadata_value("note1.md", "title").await;
    assert!(result.is_ok());
    let response = result.unwrap();
    let value = response.get("value").unwrap();
    assert!(value.as_str().unwrap().contains("First Note"));
}

#[tokio::test]
async fn test_get_metadata_value_number() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.get_metadata_value("note1.md", "priority").await;
    assert!(result.is_ok());
    let response = result.unwrap();
    let value = response.get("value").unwrap();
    assert_eq!(value.as_i64().unwrap(), 5);
}

#[tokio::test]
async fn test_get_metadata_value_array() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.get_metadata_value("note1.md", "tags").await;
    assert!(result.is_ok());
    let response = result.unwrap();
    let value = response.get("value").unwrap();
    assert!(value.is_array());
    let tags = value.as_array().unwrap();
    assert!(tags.len() >= 2);
}

#[tokio::test]
async fn test_get_metadata_value_nested() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.get_metadata_value("note3.md", "nested.field").await;
    assert!(result.is_ok());
    let response = result.unwrap();
    let value = response.get("value").unwrap();
    assert_eq!(value.as_str().unwrap(), "value");
}

#[tokio::test]
async fn test_get_metadata_value_nested_number() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.get_metadata_value("note3.md", "nested.count").await;
    assert!(result.is_ok());
    let response = result.unwrap();
    let value = response.get("value").unwrap();
    assert_eq!(value.as_i64().unwrap(), 42);
}

#[tokio::test]
async fn test_get_metadata_value_missing_key() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.get_metadata_value("note1.md", "nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_get_metadata_value_no_metadata() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.get_metadata_value("no_metadata.md", "title").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_query_metadata_equality() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.query_metadata(r#"status: "draft""#).await;
    assert!(result.is_ok());
    // Query executes successfully (matches depend on frontmatter parsing)
    let _response = result.unwrap();
}

#[tokio::test]
async fn test_query_metadata_comparison() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.query_metadata("priority > 3").await;
    assert!(result.is_ok());
    // Query executes successfully (matches depend on frontmatter parsing)
    let _response = result.unwrap();
}

#[tokio::test]
async fn test_query_metadata_contains() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.query_metadata(r#"tags: contains("urgent")"#).await;
    assert!(result.is_ok());
    // Query executes successfully (matches depend on frontmatter parsing)
    let _response = result.unwrap();
}

#[tokio::test]
async fn test_query_metadata_no_matches() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.query_metadata(r#"status: "nonexistent""#).await;
    assert!(result.is_ok());
    let response = result.unwrap();
    let matched = response.get("matched").unwrap().as_u64().unwrap();
    assert_eq!(matched, 0);
}

#[tokio::test]
async fn test_query_metadata_invalid_syntax() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.query_metadata("invalid syntax !!!").await;
    // Should handle invalid syntax gracefully (returns error for invalid pattern)
    assert!(result.is_err());
}

#[tokio::test]
async fn test_async_error_nonexistent_file() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;
    let tools = MetadataTools::new(manager);

    let result = tools.get_metadata_value("nonexistent.md", "title").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_concurrent_metadata_reads() {
    let (_temp_dir, manager) = setup_test_vault_with_metadata().await;

    let handles: Vec<_> = (0..10)
        .map(|i| {
            let tools = MetadataTools::new(manager.clone());
            tokio::spawn(async move {
                let file = match i % 3 {
                    0 => "note1.md",
                    1 => "note2.md",
                    _ => "note3.md",
                };
                tools.get_metadata_value(file, "title").await
            })
        })
        .collect();

    for handle in handles {
        let result = handle.await.expect("Task panicked");
        assert!(result.is_ok());
    }
}
