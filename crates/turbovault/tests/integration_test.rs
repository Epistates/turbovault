//! Integration tests for TurboVault Server

#[cfg(test)]
mod tests {
    use turbovault_core::{ConfigProfile, VaultConfig};
    use turbovault::ObsidianMcpServer;
    use turbovault_vault::VaultManager;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::fs;

    /// Helper to create a test vault
    async fn create_test_vault() -> (TempDir, VaultManager) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let vault_path = temp_dir.path();

        // Create some test files
        fs::write(
            vault_path.join("index.md"),
            "# Index\n\n[[note1]], [[note2]]",
        )
        .await
        .expect("Failed to write index.md");

        fs::write(
            vault_path.join("note1.md"),
            "# Note 1\n\nThis links to [[note2]]",
        )
        .await
        .expect("Failed to write note1.md");

        fs::write(
            vault_path.join("note2.md"),
            "# Note 2\n\nThis links back to [[note1]] and [[index]]",
        )
        .await
        .expect("Failed to write note2.md");

        // Create vault manager
        let mut config = ConfigProfile::Development.create_config();
        let vault_config = VaultConfig::builder("default", vault_path)
            .build()
            .expect("Failed to create vault config");
        config.vaults.push(vault_config);

        let manager = VaultManager::new(config).expect("Failed to create vault manager");
        manager
            .initialize()
            .await
            .expect("Failed to initialize vault");

        (temp_dir, manager)
    }

    #[tokio::test]
    async fn test_server_creation() {
        let _server = ObsidianMcpServer::new();
        // Server should be creatable without vault (no assertion needed)
    }

    #[tokio::test]
    async fn test_server_initialization() {
        let (_temp, _manager) = create_test_vault().await;
        let _server = ObsidianMcpServer::new().expect("Failed to create server");
        // Server should initialize without vault (vault-agnostic design, no assertion needed)
    }

    #[tokio::test]
    async fn test_vault_path_resolution() {
        let (temp_dir, manager) = create_test_vault().await;
        let expected = temp_dir.path();
        let actual = manager.vault_path();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn test_scan_vault() {
        let (_temp, manager) = create_test_vault().await;
        let files = manager.scan_vault().await.expect("Failed to scan vault");
        assert!(files.len() >= 3, "Should find at least 3 markdown files");
    }

    #[tokio::test]
    async fn test_parse_file() {
        let (_temp, manager) = create_test_vault().await;
        let vault_file = manager
            .parse_file(&PathBuf::from("index.md"))
            .await
            .expect("Failed to parse file");
        assert_eq!(vault_file.path.file_name().unwrap(), "index.md");
    }

    #[tokio::test]
    async fn test_link_graph_access() {
        let (_temp, manager) = create_test_vault().await;
        let graph = manager.link_graph();
        let _read_guard = graph.read().await;
        // Should be able to acquire read lock on graph (no assertion needed)
    }

    // ==================== Export Tests ====================

    use turbovault_tools::ExportTools;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_export_health_report_json() {
        let (_temp, manager) = create_test_vault().await;
        let tools = ExportTools::new(Arc::new(manager));
        let report = tools.export_health_report("json").await.unwrap();

        assert!(report.contains("\"vault_name\""));
        assert!(report.contains("\"health_score\""));
    }

    #[tokio::test]
    async fn test_export_health_report_csv() {
        let (_temp, manager) = create_test_vault().await;
        let tools = ExportTools::new(Arc::new(manager));
        let report = tools.export_health_report("csv").await.unwrap();

        assert!(report.contains("timestamp,vault_name,health_score"));
    }

    #[tokio::test]
    async fn test_export_vault_stats() {
        let (_temp, manager) = create_test_vault().await;
        let tools = ExportTools::new(Arc::new(manager));
        let stats = tools.export_vault_stats("json").await.unwrap();

        assert!(stats.contains("\"total_files\""));
        assert!(stats.contains("\"total_links\""));
    }

    #[tokio::test]
    async fn test_export_analysis_report() {
        let (_temp, manager) = create_test_vault().await;
        let tools = ExportTools::new(Arc::new(manager));
        let report = tools.export_analysis_report("json").await.unwrap();

        assert!(report.contains("\"vault_name\""));
        assert!(report.contains("\"recommendations\""));
    }
}
