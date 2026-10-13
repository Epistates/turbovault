//! Integration tests for TurboVault Server

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::fs;
    use turbovault::ObsidianMcpServer;
    use turbovault_core::{ConfigProfile, TaskPriority, VaultConfig};
    use turbovault_vault::VaultManager;

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
        let graph = manager.link_graph_flushed().await;
        let _read_guard = graph.read().await;
        // Should be able to acquire read lock on graph (no assertion needed)
    }

    // ==================== Export Tests ====================

    use std::sync::Arc;
    use turbovault_tools::ExportTools;

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

    // ==================== Task Metadata Parsing Tests ====================

    /// Tasks.md content covering every metadata format the parser supports:
    /// emoji-only, dataview-only, mixed emoji+dataview, completed tasks,
    /// comma-separated dataview fields, and inline tags.
    const TASKS_MD: &str = "# Test Header
- [ ] Take out the trash 🔁 every day 🛫 2026-04-30 ⏳ 2026-04-30 📅 2026-04-30 🔺
- [ ] Feed the cat ⏫ 🔁 every day 🛫 2026-04-30 ⏳ 2026-04-30 📅 2026-04-30
- [ ] Clean the kitchen 🔁 every week 🛫 2026-04-30 ⏳ 2026-04-30 📅 2026-04-30 #task_type_1
- [ ] Clean the baseboards 🔁 every week 🔽 🛫  2026-04-30 ⏳ 2026-04-30 📅 2026-04-30 #task_type_2
- [ ] Buy groceries [due:: 2026-05-01] [priority:: medium] #errands
- [ ] Write weekly report 📅 2026-05-10 [scheduled:: 2026-05-08] 🔁 every week
- [x] Submit expense report ✅ 2026-04-29 📅 2026-04-28
- [ ] Plan sprint [priority:: high], [start:: 2026-05-01], [due:: 2026-05-07] [id:: sprint-42]
";

    async fn create_task_vault() -> (TempDir, VaultManager) {
        let temp_dir = TempDir::new().expect("temp dir");
        let vault_path = temp_dir.path();

        fs::create_dir_all(vault_path.join("TestFolder"))
            .await
            .expect("create TestFolder");
        fs::write(vault_path.join("TestFolder/Tasks.md"), TASKS_MD)
            .await
            .expect("write Tasks.md");

        let mut config = ConfigProfile::Development.create_config();
        let vault_config = VaultConfig::builder("test", vault_path)
            .build()
            .expect("vault config");
        config.vaults.push(vault_config);

        let manager = VaultManager::new(config).expect("vault manager");
        manager.initialize().await.expect("initialize");

        (temp_dir, manager)
    }

    #[tokio::test]
    async fn test_task_metadata_parsing() {
        let (_temp, manager) = create_task_vault().await;

        let vault_file = manager
            .parse_file(&PathBuf::from("TestFolder/Tasks.md"))
            .await
            .expect("parse Tasks.md");

        assert_eq!(vault_file.tasks.len(), 8, "expected 8 tasks in Tasks.md");

        let find = |desc: &str| {
            vault_file
                .tasks
                .iter()
                .find(|t| t.content == desc)
                .unwrap_or_else(|| panic!("task not found: {desc:?}"))
        };

        // --- Emoji format ---

        let t = find("Take out the trash");
        assert!(!t.is_completed);
        assert_eq!(t.priority, TaskPriority::Highest);
        assert_eq!(t.recurrence.as_deref(), Some("every day"));
        assert_eq!(
            t.start_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(
            t.scheduled_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(
            t.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );

        let t = find("Feed the cat");
        assert!(!t.is_completed);
        assert_eq!(t.priority, TaskPriority::High);
        assert_eq!(t.recurrence.as_deref(), Some("every day"));
        assert_eq!(
            t.start_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(
            t.scheduled_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(
            t.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );

        let t = find("Clean the kitchen");
        assert_eq!(t.priority, TaskPriority::Normal);
        assert_eq!(t.recurrence.as_deref(), Some("every week"));
        assert_eq!(
            t.start_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(
            t.scheduled_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(
            t.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(t.tags, vec!["task_type_1"]);

        // task_type_2 task also tests the double-space after 🛫 (🛫  2026-04-30)
        let t = find("Clean the baseboards");
        assert_eq!(t.priority, TaskPriority::Low);
        assert_eq!(t.recurrence.as_deref(), Some("every week"));
        assert_eq!(
            t.start_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(
            t.scheduled_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(
            t.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(t.tags, vec!["task_type_2"]);

        // --- Dataview format ---

        let t = find("Buy groceries");
        assert!(!t.is_completed);
        assert_eq!(t.priority, TaskPriority::Medium);
        assert_eq!(
            t.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-05-01")
        );
        assert_eq!(t.tags, vec!["errands"]);

        // --- Mixed emoji + Dataview ---

        let t = find("Write weekly report");
        assert!(!t.is_completed);
        assert_eq!(
            t.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-05-10")
        );
        assert_eq!(
            t.scheduled_date.map(|d| d.to_string()).as_deref(),
            Some("2026-05-08")
        );
        assert_eq!(t.recurrence.as_deref(), Some("every week"));

        // --- Completed task with done date ---

        let t = find("Submit expense report");
        assert!(t.is_completed);
        assert_eq!(
            t.done_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-29")
        );
        assert_eq!(
            t.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-28")
        );

        // --- Comma-separated Dataview fields with ID ---

        let t = find("Plan sprint");
        assert_eq!(t.priority, TaskPriority::High);
        assert_eq!(
            t.start_date.map(|d| d.to_string()).as_deref(),
            Some("2026-05-01")
        );
        assert_eq!(
            t.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-05-07")
        );
        assert_eq!(t.id.as_deref(), Some("sprint-42"));
    }
}
