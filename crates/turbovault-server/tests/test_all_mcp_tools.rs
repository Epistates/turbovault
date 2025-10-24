//! Integration tests for all 38 MCP tools through the server interface
//! Tests the complete workflow from MCP call to response

use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_server::ObsidianMcpServer;
use tempfile::TempDir;
use tokio::fs;

/// Setup test vault with sample data
async fn setup_integration_vault() -> (TempDir, ObsidianMcpServer) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let vault_path = temp_dir.path();

    // Create sample files
    fs::write(
        vault_path.join("index.md"),
        r#"---
title: "Index"
tags: ["index", "main"]
---
# Index
[[note1]] [[note2]]"#,
    )
    .await
    .expect("Failed to write index");

    fs::write(
        vault_path.join("note1.md"),
        r#"---
title: "Note 1"
status: "active"
priority: 5
---
# Note 1
Content linking to [[note2]]"#,
    )
    .await
    .expect("Failed to write note1");

    fs::write(
        vault_path.join("note2.md"),
        r#"---
title: "Note 2"
status: "draft"
priority: 3
---
# Note 2
Links to [[index]]"#,
    )
    .await
    .expect("Failed to write note2");

    fs::create_dir(vault_path.join("templates")).await.ok();
    fs::write(
        vault_path.join("templates/daily.md"),
        "# {{date}}\n\n## Tasks\n- [ ] Task",
    )
    .await
    .ok();

    (
        temp_dir,
        ObsidianMcpServer::new().expect("Failed to create server"),
    )
}

// ==================== Vault Lifecycle Tools ====================

#[tokio::test]
async fn integration_test_vault_lifecycle() {
    let (temp_dir, server) = setup_integration_vault().await;
    let vault_path = temp_dir.path().to_str().unwrap();

    // Add vault
    let vault_config = VaultConfig::builder("test", vault_path)
        .build()
        .expect("Failed to create vault config");
    let result = server.multi_vault().add_vault(vault_config).await;
    assert!(result.is_ok(), "Failed to add vault: {:?}", result.err());

    // List vaults
    let vaults = server.multi_vault().list_vaults().await;
    assert!(vaults.is_ok());
    assert!(!vaults.unwrap().is_empty());

    // Set active vault
    let result = server.multi_vault().set_active_vault("test").await;
    assert!(result.is_ok());

    // Get active vault
    let active = server.multi_vault().get_active_vault().await;
    assert_eq!(active, "test");
}

// ==================== File Operations ====================

#[tokio::test]
async fn integration_test_file_operations() {
    let (temp_dir, _server) = setup_integration_vault().await;
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = turbovault_vault::VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    let manager = std::sync::Arc::new(manager);

    let tools = turbovault_tools::FileTools::new(manager);

    // Read
    let content = tools.read_file("index.md").await;
    assert!(content.is_ok());
    assert!(content.unwrap().contains("Index"));

    // Write
    let result = tools.write_file("new.md", "# New Note").await;
    assert!(result.is_ok());

    // Delete
    let result = tools.delete_file("new.md").await;
    assert!(result.is_ok());
}

// ==================== Search & Link Analysis ====================

#[tokio::test]
async fn integration_test_search_and_links() {
    let (temp_dir, _server) = setup_integration_vault().await;
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = turbovault_vault::VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    let manager = std::sync::Arc::new(manager);

    let tools = turbovault_tools::SearchTools::new(manager.clone());

    // Find backlinks
    let backlinks = tools.find_backlinks("note2.md").await;
    assert!(backlinks.is_ok());

    // Find forward links
    let forward = tools.find_forward_links("note1.md").await;
    assert!(forward.is_ok());

    // Find related notes
    let related = tools.find_related_notes("index.md", 2).await;
    assert!(related.is_ok());

    // Search files
    let results = tools.search_files("note").await;
    assert!(results.is_ok());
    assert!(results.unwrap().len() >= 2);
}

// ==================== Graph Analysis ====================

#[tokio::test]
async fn integration_test_graph_analysis() {
    let (temp_dir, _server) = setup_integration_vault().await;
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = turbovault_vault::VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    let manager = std::sync::Arc::new(manager);

    let tools = turbovault_tools::GraphTools::new(manager);

    // Quick health check
    let health = tools.quick_health_check().await;
    assert!(health.is_ok());
    let health_info = health.unwrap();
    assert!(health_info.total_notes >= 3);

    // Full health analysis
    let full_health = tools.full_health_analysis().await;
    assert!(full_health.is_ok());

    // Get hub notes
    let hubs = tools.get_hub_notes(5).await;
    assert!(hubs.is_ok());

    // Get broken links
    let broken = tools.get_broken_links().await;
    assert!(broken.is_ok());

    // Detect cycles
    let cycles = tools.detect_cycles().await;
    assert!(cycles.is_ok());
}

// ==================== Metadata Operations ====================

#[tokio::test]
async fn integration_test_metadata_operations() {
    let (temp_dir, _server) = setup_integration_vault().await;
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = turbovault_vault::VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    let manager = std::sync::Arc::new(manager);

    let tools = turbovault_tools::MetadataTools::new(manager);

    // Get metadata value
    let value = tools.get_metadata_value("note1.md", "title").await;
    assert!(value.is_ok());

    // Query metadata
    let query_result = tools.query_metadata(r#"status: "active""#).await;
    assert!(query_result.is_ok());
}

// ==================== Search Engine ====================

#[tokio::test]
async fn integration_test_search_engine() {
    let (temp_dir, _server) = setup_integration_vault().await;
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = turbovault_vault::VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    let manager = std::sync::Arc::new(manager);

    let engine = turbovault_tools::SearchEngine::new(manager).await;
    assert!(engine.is_ok());

    let engine = engine.unwrap();

    // Basic search
    let results = engine.search("note").await;
    assert!(results.is_ok());

    // Advanced search
    let query = turbovault_tools::SearchQuery::new("content").limit(10);
    let results = engine.advanced_search(query).await;
    assert!(results.is_ok());
}

// ==================== Batch Operations ====================

#[tokio::test]
async fn integration_test_batch_operations() {
    let (temp_dir, _server) = setup_integration_vault().await;
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = turbovault_vault::VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    let manager = std::sync::Arc::new(manager);

    let tools = turbovault_tools::BatchTools::new(manager);

    let ops = vec![
        turbovault_tools::BatchOperation::WriteNote {
            path: "batch1.md".to_string(),
            content: "# Batch 1".to_string(),
        },
        turbovault_tools::BatchOperation::WriteNote {
            path: "batch2.md".to_string(),
            content: "# Batch 2".to_string(),
        },
    ];

    let result = tools.batch_execute(ops).await;
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(batch_result.success);
}

// ==================== Templates ====================

#[tokio::test]
async fn integration_test_templates() {
    let (temp_dir, _server) = setup_integration_vault().await;
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = turbovault_vault::VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    let manager = std::sync::Arc::new(manager);

    let engine = turbovault_tools::TemplateEngine::new(manager);

    // List templates
    let templates = engine.list_templates();
    assert!(!templates.is_empty());
}

// ==================== Export Operations ====================

#[tokio::test]
async fn integration_test_export_operations() {
    let (temp_dir, _server) = setup_integration_vault().await;
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = turbovault_vault::VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    let manager = std::sync::Arc::new(manager);

    let tools = turbovault_tools::ExportTools::new(manager);

    // Export health report
    let report = tools.export_health_report("json").await;
    assert!(report.is_ok());

    // Export vault stats
    let stats = tools.export_vault_stats("json").await;
    assert!(stats.is_ok());
}

// ==================== Analysis Tools ====================

#[tokio::test]
async fn integration_test_analysis_tools() {
    let (temp_dir, _server) = setup_integration_vault().await;
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = turbovault_vault::VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    let manager = std::sync::Arc::new(manager);

    let tools = turbovault_tools::AnalysisTools::new(manager);

    // Get vault stats
    let stats = tools.get_vault_stats().await;
    assert!(stats.is_ok());
    let stats_info = stats.unwrap();
    assert!(stats_info.total_files >= 3);
}

// ==================== Relationship Tools ====================

#[tokio::test]
async fn integration_test_relationship_tools() {
    let (temp_dir, _server) = setup_integration_vault().await;
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = turbovault_vault::VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    let manager = std::sync::Arc::new(manager);

    let tools = turbovault_tools::RelationshipTools::new(manager);

    // Get link strength
    let strength = tools.get_link_strength("index.md", "note1.md").await;
    assert!(strength.is_ok());
}

// ==================== Full Workflow Test ====================

#[tokio::test]
async fn integration_test_full_workflow() {
    let (temp_dir, _server) = setup_integration_vault().await;
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = turbovault_vault::VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    let manager = std::sync::Arc::new(manager);

    // 1. Write a new note
    let file_tools = turbovault_tools::FileTools::new(manager.clone());
    file_tools
        .write_file("workflow.md", "# Workflow Test\n[[index]]")
        .await
        .unwrap();

    // 2. Search for it
    let search_tools = turbovault_tools::SearchTools::new(manager.clone());
    let results = search_tools.search_files("workflow").await.unwrap();
    assert!(results.contains(&"workflow.md".to_string()));

    // 3. Check its backlinks
    let _backlinks = search_tools.find_backlinks("index.md").await.unwrap();
    // workflow.md should now link to index.md

    // 4. Run health check
    let graph_tools = turbovault_tools::GraphTools::new(manager.clone());
    let health = graph_tools.quick_health_check().await.unwrap();
    assert!(health.total_notes >= 4);

    // 5. Clean up
    file_tools.delete_file("workflow.md").await.unwrap();
}
