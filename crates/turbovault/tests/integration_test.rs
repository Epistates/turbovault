//! Integration tests for TurboVault Server

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
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
        let graph = manager.link_graph();
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

        let t = find("Clean the kitchen #task_type_1");
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
        let t = find("Clean the baseboards #task_type_2");
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

        let t = find("Buy groceries #errands");
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

    // ==================== update_task Tests ====================

    /// Helper: replace a specific 1-indexed line in file content and write it back.
    /// Mirrors the logic in the update_task MCP tool.
    async fn apply_task_update(
        manager: &VaultManager,
        file_path: &Path,
        task: &turbovault_core::TaskItem,
    ) {
        let content = manager.read_file(file_path).await.expect("read file");
        let hash = turbovault_vault::compute_hash(&content);

        let line_sep = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut lines: Vec<String> = if line_sep == "\r\n" {
            content.split("\r\n").map(|s| s.to_string()).collect()
        } else {
            content.split('\n').map(|s| s.to_string()).collect()
        };

        let line_idx = task.position.line - 1; // 1-indexed → 0-indexed
        let original = &lines[line_idx];
        let indent_len = original.len() - original.trim_start().len();
        let indent = original[..indent_len].to_string();
        lines[line_idx] = format!("{}{}", indent, task.to_markdown_line());

        let new_content = lines.join(line_sep);
        manager
            .write_file(file_path, &new_content, Some(&hash))
            .await
            .expect("write file");
    }

    #[tokio::test]
    async fn test_update_task_mark_complete() {
        use chrono::NaiveDate;
        let (_temp, manager) = create_task_vault().await;
        let file_path = PathBuf::from("TestFolder/Tasks.md");

        // Find the task we want to update
        let vault_file = manager.parse_file(&file_path).await.expect("parse");
        let mut task = vault_file
            .tasks
            .iter()
            .find(|t| t.content == "Take out the trash")
            .expect("find task")
            .clone();

        assert!(!task.is_completed);
        assert!(task.done_date.is_none());

        // Mutate
        task.is_completed = true;
        task.done_date = NaiveDate::from_ymd_opt(2026, 5, 2);

        apply_task_update(&manager, &file_path, &task).await;

        // Re-parse and verify
        let vault_file2 = manager.parse_file(&file_path).await.expect("re-parse");
        let updated = vault_file2
            .tasks
            .iter()
            .find(|t| t.content == "Take out the trash")
            .expect("find updated task");

        assert!(updated.is_completed);
        assert_eq!(
            updated.done_date.map(|d| d.to_string()).as_deref(),
            Some("2026-05-02")
        );
        // Dates from the original task should be preserved
        assert_eq!(
            updated.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );
    }

    #[tokio::test]
    async fn test_update_task_change_priority() {
        let (_temp, manager) = create_task_vault().await;
        let file_path = PathBuf::from("TestFolder/Tasks.md");

        let vault_file = manager.parse_file(&file_path).await.expect("parse");
        let mut task = vault_file
            .tasks
            .iter()
            .find(|t| t.content == "Feed the cat")
            .expect("find task")
            .clone();

        assert_eq!(task.priority, TaskPriority::High);

        task.priority = TaskPriority::Lowest;
        apply_task_update(&manager, &file_path, &task).await;

        let vault_file2 = manager.parse_file(&file_path).await.expect("re-parse");
        let updated = vault_file2
            .tasks
            .iter()
            .find(|t| t.content == "Feed the cat")
            .expect("find updated task");

        assert_eq!(updated.priority, TaskPriority::Lowest);
        // Other fields should be unchanged
        assert_eq!(updated.recurrence.as_deref(), Some("every day"));
        assert!(!updated.is_completed);
    }

    #[tokio::test]
    async fn test_update_task_add_and_remove_tags() {
        let (_temp, manager) = create_task_vault().await;
        let file_path = PathBuf::from("TestFolder/Tasks.md");

        let vault_file = manager.parse_file(&file_path).await.expect("parse");
        let mut task = vault_file
            .tasks
            .iter()
            .find(|t| t.content == "Clean the kitchen #task_type_1")
            .expect("find task")
            .clone();

        assert!(task.tags.iter().any(|t| t == "task_type_1"));

        // Add a new tag and remove the existing one
        let bare = "urgent";
        task.content.push(' ');
        task.content.push('#');
        task.content.push_str(bare);
        task.tags.push(bare.to_string());

        // Remove task_type_1
        let remove = "task_type_1";
        let pattern = format!("#{}", remove);
        let lower = task.content.to_lowercase();
        if let Some(pos) = lower.find(&pattern) {
            let end = pos + pattern.len();
            let start = if pos > 0 && task.content.as_bytes().get(pos - 1) == Some(&b' ') {
                pos - 1
            } else {
                pos
            };
            task.content.drain(start..end);
            task.content = task.content.trim_end().to_string();
            task.tags.retain(|t| t != remove);
        }

        apply_task_update(&manager, &file_path, &task).await;

        let vault_file2 = manager.parse_file(&file_path).await.expect("re-parse");
        let updated = vault_file2
            .tasks
            .iter()
            .find(|t| t.tags.iter().any(|tag| tag == "urgent"))
            .expect("find updated task");

        assert!(updated.tags.iter().any(|t| t == "urgent"));
        assert!(!updated.tags.iter().any(|t| t == "task_type_1"));
    }

    #[tokio::test]
    async fn test_update_task_dataview_format_preserved() {
        use chrono::NaiveDate;
        use turbovault_core::TaskMetadataFormat;

        let (_temp, manager) = create_task_vault().await;
        let file_path = PathBuf::from("TestFolder/Tasks.md");

        let vault_file = manager.parse_file(&file_path).await.expect("parse");
        let mut task = vault_file
            .tasks
            .iter()
            .find(|t| t.content == "Buy groceries #errands")
            .expect("find task")
            .clone();

        // Confirm parser detected dataview format
        assert_eq!(task.metadata_format, TaskMetadataFormat::Dataview);

        // Update the due date
        task.due_date = NaiveDate::from_ymd_opt(2026, 6, 1);

        apply_task_update(&manager, &file_path, &task).await;

        // Read raw content to confirm dataview notation was used
        let new_content = manager.read_file(&file_path).await.expect("read");
        assert!(
            new_content.contains("[due:: 2026-06-01]"),
            "expected dataview [due:: ...] notation in: {new_content}"
        );
        assert!(
            !new_content.contains("📅 2026-06-01"),
            "unexpected emoji notation in dataview task"
        );
    }

    #[tokio::test]
    async fn test_update_task_clear_due_date() {
        let (_temp, manager) = create_task_vault().await;
        let file_path = PathBuf::from("TestFolder/Tasks.md");

        let vault_file = manager.parse_file(&file_path).await.expect("parse");
        let mut task = vault_file
            .tasks
            .iter()
            .find(|t| t.content == "Take out the trash")
            .expect("find task")
            .clone();

        assert!(task.due_date.is_some());

        task.due_date = None;
        apply_task_update(&manager, &file_path, &task).await;

        let vault_file2 = manager.parse_file(&file_path).await.expect("re-parse");
        let updated = vault_file2
            .tasks
            .iter()
            .find(|t| t.content == "Take out the trash")
            .expect("find updated task");

        assert!(
            updated.due_date.is_none(),
            "due_date should have been cleared"
        );
    }

    #[tokio::test]
    async fn test_update_task_description() {
        let (_temp, manager) = create_task_vault().await;
        let file_path = PathBuf::from("TestFolder/Tasks.md");

        let vault_file = manager.parse_file(&file_path).await.expect("parse");
        let mut task = vault_file
            .tasks
            .iter()
            .find(|t| t.content == "Buy groceries #errands")
            .expect("find task")
            .clone();

        // Change the description — inline tags in new content should be re-indexed
        task.content = "Buy oat milk and eggs #errands #urgent".to_string();
        task.tags = vec!["errands".to_string(), "urgent".to_string()];
        apply_task_update(&manager, &file_path, &task).await;

        let vault_file2 = manager.parse_file(&file_path).await.expect("re-parse");
        let updated = vault_file2
            .tasks
            .iter()
            .find(|t| t.content == "Buy oat milk and eggs #errands #urgent")
            .expect("find updated task");

        assert_eq!(updated.content, "Buy oat milk and eggs #errands #urgent");
        assert!(updated.tags.iter().any(|t| t == "errands"));
        assert!(updated.tags.iter().any(|t| t == "urgent"));
        // Metadata from the original task should be preserved
        assert_eq!(
            updated.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-05-01")
        );
    }

    #[tokio::test]
    async fn test_update_task_hash_guard_rejects_stale_write() {
        let (_temp, manager) = create_task_vault().await;
        let file_path = PathBuf::from("TestFolder/Tasks.md");

        // Read the file and capture hash
        let content = manager.read_file(&file_path).await.expect("read");
        let stale_hash = turbovault_vault::compute_hash(&content);

        // Modify the file so the hash becomes stale
        let modified = format!("{}\n<!-- touched -->", content);
        manager
            .write_file(&file_path, &modified, None)
            .await
            .expect("first write");

        // Attempt to write with the now-stale hash — should fail
        let result = manager
            .write_file(&file_path, &modified, Some(&stale_hash))
            .await;

        assert!(result.is_err(), "write with stale hash should fail");
    }

    // ==================== complete_task Tests ====================

    /// Simulate the complete_task tool: mark done + optionally spawn next occurrence.
    /// Returns the next-occurrence line if recurrence was detected and handled.
    async fn apply_task_complete(
        manager: &VaultManager,
        file_path: &Path,
        task_content: &str,
        done: chrono::NaiveDate,
    ) -> Option<String> {
        let vault_file = manager.parse_file(file_path).await.expect("parse");
        let mut task = vault_file
            .tasks
            .iter()
            .find(|t| t.content == task_content)
            .unwrap_or_else(|| panic!("task not found: {task_content:?}"))
            .clone();

        let content = manager.read_file(file_path).await.expect("read");
        let hash = turbovault_vault::compute_hash(&content);
        let line_sep = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut lines: Vec<String> = content.split(line_sep).map(|s| s.to_string()).collect();

        let line_idx = task.position.line - 1;
        let original = &lines[line_idx].clone();
        let indent_len = original.len() - original.trim_start().len();
        let indent = original[..indent_len].to_string();

        task.is_completed = true;
        task.done_date = Some(done);
        lines[line_idx] = format!("{}{}", indent, task.to_markdown_line());

        // Spawn next occurrence if recurring
        let mut spawned_line: Option<String> = None;
        if let Some(ref rec) = task.recurrence.clone() {
            let reference = task.due_date.unwrap_or(done);
            let next_due = match rec.as_str() {
                s if s.contains("every day") => Some(reference + chrono::Duration::days(1)),
                s if s.contains("every week") => Some(reference + chrono::Duration::days(7)),
                _ => None,
            };
            if let Some(nd) = next_due {
                let offset = nd.signed_duration_since(reference);
                let mut next = task.clone();
                next.is_completed = false;
                next.done_date = None;
                next.due_date = Some(nd);
                next.scheduled_date = task.scheduled_date.map(|d| d + offset);
                next.start_date = task.start_date.map(|d| d + offset);
                let line = format!("{}{}", indent, next.to_markdown_line());
                lines.insert(line_idx + 1, line.clone());
                spawned_line = Some(line);
            }
        }

        let new_content = lines.join(line_sep);
        manager
            .write_file(file_path, &new_content, Some(&hash))
            .await
            .expect("write");

        spawned_line
    }

    #[tokio::test]
    async fn test_complete_task_spawns_next_occurrence() {
        use chrono::NaiveDate;
        let (_temp, manager) = create_task_vault().await;
        let file_path = PathBuf::from("TestFolder/Tasks.md");

        // Verify initial state
        let vault_file = manager.parse_file(&file_path).await.expect("parse");
        assert_eq!(vault_file.tasks.len(), 8);
        let task = vault_file
            .tasks
            .iter()
            .find(|t| t.content == "Feed the cat")
            .expect("find task");
        assert!(!task.is_completed);
        assert_eq!(task.recurrence.as_deref(), Some("every day"));
        assert_eq!(
            task.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-04-30")
        );

        let done = NaiveDate::from_ymd_opt(2026, 5, 2).unwrap();
        let next_line = apply_task_complete(&manager, &file_path, "Feed the cat", done).await;

        assert!(
            next_line.is_some(),
            "expected next occurrence to be spawned"
        );

        // Re-parse: should now have 9 tasks (completed + spawned)
        let vault_file2 = manager.parse_file(&file_path).await.expect("re-parse");
        assert_eq!(
            vault_file2.tasks.len(),
            9,
            "expected 9 tasks after spawning next occurrence"
        );

        // Completed instance
        let completed = vault_file2
            .tasks
            .iter()
            .find(|t| t.content == "Feed the cat" && t.is_completed)
            .expect("completed task");
        assert_eq!(
            completed.done_date.map(|d| d.to_string()).as_deref(),
            Some("2026-05-02")
        );

        // Spawned pending instance — due date advances by 1 day from 2026-04-30
        let spawned = vault_file2
            .tasks
            .iter()
            .find(|t| t.content == "Feed the cat" && !t.is_completed)
            .expect("spawned next occurrence");
        assert!(!spawned.is_completed);
        assert_eq!(
            spawned.due_date.map(|d| d.to_string()).as_deref(),
            Some("2026-05-01")
        );
        assert_eq!(spawned.recurrence.as_deref(), Some("every day"));
        assert_eq!(spawned.priority, TaskPriority::High);
    }

    #[tokio::test]
    async fn test_complete_task_no_recurrence_preserves_count() {
        use chrono::NaiveDate;
        let (_temp, manager) = create_task_vault().await;
        let file_path = PathBuf::from("TestFolder/Tasks.md");

        let vault_file = manager.parse_file(&file_path).await.expect("parse");
        let count_before = vault_file.tasks.len();

        let task = vault_file
            .tasks
            .iter()
            .find(|t| t.content == "Buy groceries #errands")
            .expect("find task");
        assert!(task.recurrence.is_none());

        let done = NaiveDate::from_ymd_opt(2026, 5, 2).unwrap();
        let next_line =
            apply_task_complete(&manager, &file_path, "Buy groceries #errands", done).await;

        assert!(
            next_line.is_none(),
            "non-recurring task should not spawn next occurrence"
        );

        let vault_file2 = manager.parse_file(&file_path).await.expect("re-parse");
        assert_eq!(
            vault_file2.tasks.len(),
            count_before,
            "task count should not change for non-recurring tasks"
        );

        let updated = vault_file2
            .tasks
            .iter()
            .find(|t| t.content == "Buy groceries #errands")
            .expect("find updated");
        assert!(updated.is_completed);
        assert_eq!(
            updated.done_date.map(|d| d.to_string()).as_deref(),
            Some("2026-05-02")
        );
    }

    // ==================== get_overdue_tasks Tests ====================

    /// Collect overdue tasks as of a given date — mirrors get_overdue_tasks logic.
    async fn collect_overdue(
        manager: &VaultManager,
        as_of: chrono::NaiveDate,
    ) -> Vec<(String, chrono::NaiveDate)> {
        let files = manager.scan_vault().await.expect("scan");
        let mut overdue: Vec<(String, chrono::NaiveDate)> = Vec::new();
        for file_path in &files {
            if !file_path.to_string_lossy().ends_with(".md") {
                continue;
            }
            let vault_file = match manager.parse_file(file_path).await {
                Ok(vf) => vf,
                Err(_) => continue,
            };
            for task in vault_file.tasks {
                if task.is_completed {
                    continue;
                }
                if let Some(due) = task.due_date
                    && due < as_of
                {
                    overdue.push((task.content, due));
                }
            }
        }
        overdue.sort_by_key(|(_, d)| *d);
        overdue
    }

    #[tokio::test]
    async fn test_get_overdue_tasks_as_of_date() {
        use chrono::NaiveDate;
        let (_temp, manager) = create_task_vault().await;

        let as_of = NaiveDate::from_ymd_opt(2026, 5, 2).unwrap();
        let overdue = collect_overdue(&manager, as_of).await;

        // Pending tasks with due_date < 2026-05-02:
        //   "Take out the trash"          due 2026-04-30
        //   "Feed the cat"                due 2026-04-30
        //   "Clean the kitchen #..."      due 2026-04-30
        //   "Clean the baseboards #..."   due 2026-04-30
        //   "Buy groceries #errands"      due 2026-05-01
        let contents: Vec<&str> = overdue.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            overdue.len(),
            5,
            "expected 5 overdue tasks as of {as_of}, got: {contents:?}"
        );
        assert!(
            contents.contains(&"Take out the trash"),
            "missing 'Take out the trash'"
        );
        assert!(contents.contains(&"Feed the cat"), "missing 'Feed the cat'");
        assert!(
            contents.contains(&"Clean the kitchen #task_type_1"),
            "missing 'Clean the kitchen #task_type_1'"
        );
        assert!(
            contents.contains(&"Clean the baseboards #task_type_2"),
            "missing 'Clean the baseboards #task_type_2'"
        );
        assert!(
            contents.contains(&"Buy groceries #errands"),
            "missing 'Buy groceries #errands'"
        );

        // Completed or future-due tasks must NOT appear
        assert!(
            !contents.contains(&"Submit expense report"),
            "completed task should not be overdue"
        );
        assert!(
            !contents.contains(&"Write weekly report"),
            "future task (due 2026-05-10) should not be overdue as of {as_of}"
        );
        assert!(
            !contents.contains(&"Plan sprint"),
            "future task (due 2026-05-07) should not be overdue as of {as_of}"
        );
        assert!(contents.contains(&"Feed the cat"), "missing 'Feed the cat'");
        assert!(
            contents.contains(&"Clean the kitchen #task_type_1"),
            "missing 'Clean the kitchen #task_type_1'"
        );
        assert!(
            contents.contains(&"Clean the baseboards #task_type_2"),
            "missing 'Clean the baseboards #task_type_2'"
        );
        assert!(
            contents.contains(&"Buy groceries #errands"),
            "missing 'Buy groceries #errands'"
        );

        // Completed or future-due tasks must NOT appear
        assert!(
            !contents.contains(&"Submit expense report"),
            "completed task should not be overdue"
        );
        assert!(
            !contents.contains(&"Write weekly report"),
            "future task (due 2026-05-10) should not be overdue as of {as_of}"
        );
        assert!(
            !contents.contains(&"Plan sprint"),
            "future task (due 2026-05-07) should not be overdue as of {as_of}"
        );
    }

    #[tokio::test]
    async fn test_get_overdue_tasks_empty_before_all_due_dates() {
        use chrono::NaiveDate;
        let (_temp, manager) = create_task_vault().await;

        // All tasks in TASKS_MD have due dates in April–May 2026; nothing is overdue in January
        let as_of = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let overdue = collect_overdue(&manager, as_of).await;

        assert!(
            overdue.is_empty(),
            "expected no overdue tasks as of {as_of}, got: {:?}",
            overdue
                .iter()
                .map(|(c, d)| format!("{c} (due {d})"))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn test_get_overdue_tasks_sorted_oldest_first() {
        use chrono::NaiveDate;
        let (_temp, manager) = create_task_vault().await;

        let as_of = NaiveDate::from_ymd_opt(2026, 5, 2).unwrap();
        let overdue = collect_overdue(&manager, as_of).await;

        let dates: Vec<_> = overdue.iter().map(|(_, d)| *d).collect();
        let mut sorted = dates.clone();
        sorted.sort();
        assert_eq!(dates, sorted, "overdue tasks should be sorted oldest-first");
    }

    #[tokio::test]
    async fn test_list_tasks_mcp_tool() {
        let (_temp, manager) = create_task_vault().await;

        let files = manager.scan_vault().await.expect("scan vault");
        let mut all_tasks: Vec<serde_json::Value> = Vec::new();

        for file_path in files {
            if !file_path.to_string_lossy().ends_with(".md") {
                continue;
            }
            let vault_file = manager.parse_file(&file_path).await.expect("parse file");
            for task in vault_file.tasks {
                all_tasks.push(serde_json::json!({
                    "content": task.content,
                    "is_completed": task.is_completed,
                    "path": file_path.to_string_lossy().to_string(),
                    "priority": task.priority,
                    "tags": task.tags,
                }));
            }
        }

        assert_eq!(all_tasks.len(), 8, "expected 8 tasks");

        let find_task = |content: &str| {
            all_tasks
                .iter()
                .find(|t| t["content"] == content)
                .unwrap_or_else(|| panic!("task not found: {content}"))
        };

        let pending_task = find_task("Take out the trash");
        assert_eq!(pending_task["is_completed"], false);

        let completed_task = find_task("Submit expense report");
        assert_eq!(completed_task["is_completed"], true);

        let tag_task = find_task("Clean the kitchen #task_type_1");
        let tags = tag_task["tags"].as_array().expect("tags array");
        assert!(tags.iter().any(|t| t == "task_type_1"));
    }

    /// Collect tasks from the vault, applying the same status+tag filter logic as list_tasks.
    async fn collect_tasks_filtered(
        manager: &VaultManager,
        status_filter: Option<&str>,
        tag_filters: &[&str],
    ) -> Vec<turbovault_core::TaskItem> {
        let status = status_filter.unwrap_or("all").to_lowercase();
        let normalized_tags: Vec<String> = tag_filters
            .iter()
            .map(|t| t.trim_start_matches('#').to_lowercase())
            .collect();

        let files = manager.scan_vault().await.expect("scan vault");
        let mut results = Vec::new();
        for file_path in files {
            if !file_path.to_string_lossy().ends_with(".md") {
                continue;
            }
            let vault_file = match manager.parse_file(&file_path).await {
                Ok(vf) => vf,
                Err(_) => continue,
            };
            for task in vault_file.tasks {
                let passes_status = match status.as_str() {
                    "completed" | "done" => task.is_completed,
                    "pending" | "open" | "todo" | "incomplete" => !task.is_completed,
                    _ => true,
                };
                if !passes_status {
                    continue;
                }
                let passes_tags = normalized_tags.is_empty()
                    || normalized_tags
                        .iter()
                        .all(|req| task.tags.iter().any(|t| t.eq_ignore_ascii_case(req)));
                if passes_tags {
                    results.push(task);
                }
            }
        }
        results
    }

    #[tokio::test]
    async fn test_list_tasks_status_filter_pending() {
        let (_temp, manager) = create_task_vault().await;
        let tasks = collect_tasks_filtered(&manager, Some("pending"), &[]).await;
        assert_eq!(tasks.len(), 7, "expected 7 pending tasks");
        assert!(
            tasks.iter().all(|t| !t.is_completed),
            "all returned tasks should be pending"
        );
        // The one completed task must not appear
        assert!(
            tasks.iter().all(|t| t.content != "Submit expense report"),
            "completed task must not appear in pending filter"
        );
    }

    #[tokio::test]
    async fn test_list_tasks_status_filter_completed() {
        let (_temp, manager) = create_task_vault().await;
        let tasks = collect_tasks_filtered(&manager, Some("completed"), &[]).await;
        assert_eq!(tasks.len(), 1, "expected 1 completed task");
        assert_eq!(tasks[0].content, "Submit expense report");
        assert!(tasks[0].is_completed);
    }

    #[tokio::test]
    async fn test_list_tasks_status_filter_done_alias() {
        // "done" is an accepted alias for "completed"
        let (_temp, manager) = create_task_vault().await;
        let tasks = collect_tasks_filtered(&manager, Some("done"), &[]).await;
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].is_completed);
    }

    #[tokio::test]
    async fn test_list_tasks_tag_filter_single() {
        let (_temp, manager) = create_task_vault().await;
        let tasks = collect_tasks_filtered(&manager, None, &["errands"]).await;
        assert_eq!(tasks.len(), 1, "expected 1 task tagged #errands");
        assert_eq!(tasks[0].content, "Buy groceries #errands");
    }

    #[tokio::test]
    async fn test_list_tasks_tag_filter_hash_prefix_normalized() {
        // Passing "#errands" should produce the same result as "errands"
        let (_temp, manager) = create_task_vault().await;
        let tasks = collect_tasks_filtered(&manager, None, &["#errands"]).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].content, "Buy groceries #errands");
    }

    #[tokio::test]
    async fn test_list_tasks_tag_filter_and_logic() {
        // No single task in the fixture has both errands AND task_type_1 — AND logic means 0 results
        let (_temp, manager) = create_task_vault().await;
        let tasks = collect_tasks_filtered(&manager, None, &["errands", "task_type_1"]).await;
        assert!(
            tasks.is_empty(),
            "AND filter across disjoint tags should return no tasks"
        );
    }

    #[tokio::test]
    async fn test_list_tasks_combined_status_and_tag_filter() {
        let (_temp, manager) = create_task_vault().await;
        let tasks = collect_tasks_filtered(&manager, Some("pending"), &["errands"]).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].content, "Buy groceries #errands");
        assert!(!tasks[0].is_completed);
    }

    // ==================== list_task_tags Tests ====================

    async fn collect_all_task_tags(manager: &VaultManager) -> Vec<String> {
        let files = manager.scan_vault().await.expect("scan vault");
        let mut tag_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for file_path in files {
            if !file_path.to_string_lossy().ends_with(".md") {
                continue;
            }
            let vault_file = match manager.parse_file(&file_path).await {
                Ok(vf) => vf,
                Err(_) => continue,
            };
            for task in vault_file.tasks {
                for tag in task.tags {
                    tag_set.insert(tag.to_lowercase());
                }
            }
        }
        tag_set.into_iter().collect()
    }

    #[tokio::test]
    async fn test_list_task_tags_returns_deduplicated_sorted_tags() {
        let (_temp, manager) = create_task_vault().await;
        let tags = collect_all_task_tags(&manager).await;

        // TASKS_MD has: #task_type_1, #task_type_2, #errands
        assert!(tags.contains(&"errands".to_string()));
        assert!(tags.contains(&"task_type_1".to_string()));
        assert!(tags.contains(&"task_type_2".to_string()));

        // Each tag should appear exactly once
        let unique: std::collections::HashSet<_> = tags.iter().collect();
        assert_eq!(tags.len(), unique.len(), "tags should be deduplicated");

        // Should be sorted
        let mut sorted = tags.clone();
        sorted.sort();
        assert_eq!(tags, sorted, "tags should be sorted");
    }

    #[tokio::test]
    async fn test_list_task_tags_excludes_no_tag_tasks() {
        let (_temp, manager) = create_task_vault().await;
        let tags = collect_all_task_tags(&manager).await;
        // "Take out the trash" and "Submit expense report" have no tags — they should not contribute empty strings
        assert!(tags.iter().all(|t| !t.is_empty()));
    }

    // ==================== delete_task Tests ====================

    #[tokio::test]
    async fn test_delete_task_removes_line() {
        let (_temp, manager) = create_task_vault().await;
        let file_path = PathBuf::from("TestFolder/Tasks.md");

        let vault_file = manager.parse_file(&file_path).await.expect("parse");
        let count_before = vault_file.tasks.len();
        let target = vault_file
            .tasks
            .iter()
            .find(|t| t.content == "Buy groceries #errands")
            .expect("find target task")
            .clone();
        let target_line = target.position.line;

        // Delete by removing the line directly (mirrors delete_task logic)
        let content = manager.read_file(&file_path).await.expect("read");
        let line_sep = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut lines: Vec<&str> = content.split(line_sep).collect();
        lines.remove(target_line - 1);
        let new_content = lines.join(line_sep);
        manager
            .write_file(&file_path, &new_content, None)
            .await
            .expect("write");

        let vault_file2 = manager.parse_file(&file_path).await.expect("re-parse");
        assert_eq!(
            vault_file2.tasks.len(),
            count_before - 1,
            "task count should decrease by 1"
        );
        assert!(
            vault_file2
                .tasks
                .iter()
                .all(|t| t.content != "Buy groceries #errands"),
            "deleted task must not appear in re-parsed file"
        );
    }

    #[tokio::test]
    async fn test_delete_task_preserves_other_tasks() {
        let (_temp, manager) = create_task_vault().await;
        let file_path = PathBuf::from("TestFolder/Tasks.md");

        let vault_file = manager.parse_file(&file_path).await.expect("parse");
        let target = vault_file
            .tasks
            .iter()
            .find(|t| t.content == "Plan sprint")
            .expect("find target task")
            .clone();
        let target_line = target.position.line;

        let content = manager.read_file(&file_path).await.expect("read");
        let line_sep = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut lines: Vec<&str> = content.split(line_sep).collect();
        lines.remove(target_line - 1);
        let new_content = lines.join(line_sep);
        manager
            .write_file(&file_path, &new_content, None)
            .await
            .expect("write");

        let vault_file2 = manager.parse_file(&file_path).await.expect("re-parse");
        // Other tasks must still be present
        assert!(
            vault_file2
                .tasks
                .iter()
                .any(|t| t.content == "Feed the cat")
        );
        assert!(
            vault_file2
                .tasks
                .iter()
                .any(|t| t.content == "Submit expense report")
        );
    }
}
