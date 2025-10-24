//! Integration tests for export functionality

use turbo_vault_core::prelude::*;
use turbo_vault_vault::VaultManager;
use turbo_vault_tools::ExportTools;
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
