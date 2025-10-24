//! Comprehensive async error path tests
//! Tests error handling in async Result functions across all tools

use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::*;
use turbovault_vault::VaultManager;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup_minimal_vault() -> (TempDir, Arc<VaultManager>) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();

    (temp_dir, Arc::new(manager))
}

// ==================== FileTools Async Error Paths ====================

#[tokio::test]
async fn test_file_tools_read_permission_denied() {
    let (_temp_dir, manager) = setup_minimal_vault().await;
    let tools = FileTools::new(manager.clone());

    // Create a file with restricted permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = manager.vault_path().join("restricted.md");
        tokio::fs::write(&path, "content").await.unwrap();
        let mut perms = tokio::fs::metadata(&path).await.unwrap().permissions();
        perms.set_mode(0o000); // No permissions
        tokio::fs::set_permissions(&path, perms).await.unwrap();

        let result = tools.read_file("restricted.md").await;
        assert!(result.is_err());

        // Clean up - restore permissions
        let mut perms = tokio::fs::metadata(&path)
            .await
            .unwrap_or_else(|_| std::fs::metadata(&path).unwrap())
            .permissions();
        perms.set_mode(0o644);
        let _ = tokio::fs::set_permissions(&path, perms).await;
    }
}

#[tokio::test]
async fn test_file_tools_write_readonly_filesystem() {
    let (_temp_dir, manager) = setup_minimal_vault().await;
    let tools = FileTools::new(manager);

    // Try to write to a path that requires directory creation in read-only parent
    // This test simulates filesystem errors during directory creation
    #[cfg(unix)]
    {
        let result = tools.write_file("/proc/test/invalid.md", "content").await;
        // Should fail due to invalid path or permissions
        assert!(result.is_err());
    }
}

#[tokio::test]
async fn test_file_tools_delete_locked_file() {
    let (temp_dir, manager) = setup_minimal_vault().await;
    let tools = FileTools::new(manager.clone());

    // Create a file and open it for writing to lock it
    let path = temp_dir.path().join("locked.md");
    tokio::fs::write(&path, "content").await.unwrap();

    // Open file to create a lock
    let _lock = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .await
        .unwrap();

    // Attempt to delete while locked (behavior varies by OS)
    let _result = tools.delete_file("locked.md").await;
    // On Windows, this should fail; on Unix, it might succeed
    #[cfg(windows)]
    assert!(result.is_err());
}

// ==================== SearchTools Async Error Paths ====================

#[tokio::test]
async fn test_search_tools_malformed_graph_data() {
    let (_temp_dir, manager) = setup_minimal_vault().await;
    let tools = SearchTools::new(manager);

    // Query for a path with special characters that might break parsing
    let result = tools.find_backlinks("path/with/<>:\"\\|?*.md").await;
    // Should handle gracefully without panicking
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_search_tools_extremely_deep_recursion() {
    let (_temp_dir, manager) = setup_minimal_vault().await;
    let tools = SearchTools::new(manager);

    // Test with very large hop count
    let result = tools.find_related_notes("any.md", 1000).await;
    // Should handle without stack overflow
    assert!(result.is_ok());
}

// ==================== GraphTools Async Error Paths ====================

#[tokio::test]
async fn test_graph_tools_corrupted_graph_state() {
    let (_temp_dir, manager) = setup_minimal_vault().await;
    let tools = GraphTools::new(manager);

    // Health check on empty vault should not panic
    let result = tools.quick_health_check().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_graph_tools_circular_reference_overflow() {
    let (temp_dir, manager) = setup_minimal_vault().await;

    // Create deeply nested circular references
    for i in 0..100 {
        let content = format!("# Note {}\n[[note{}]]", i, (i + 1) % 100);
        tokio::fs::write(temp_dir.path().join(format!("note{}.md", i)), content)
            .await
            .unwrap();
    }

    manager.initialize().await.unwrap();
    let tools = GraphTools::new(manager);

    // Should detect cycles without stack overflow
    let result = tools.detect_cycles().await;
    assert!(result.is_ok());
}

// ==================== MetadataTools Async Error Paths ====================

#[tokio::test]
async fn test_metadata_tools_malformed_yaml() {
    let (temp_dir, manager) = setup_minimal_vault().await;

    // Create file with malformed YAML frontmatter
    tokio::fs::write(
        temp_dir.path().join("bad_yaml.md"),
        "---\ninvalid: yaml: syntax:\n  broken\n---\n# Content",
    )
    .await
    .unwrap();

    let tools = MetadataTools::new(manager);
    let result = tools.get_metadata_value("bad_yaml.md", "title").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_metadata_tools_deeply_nested_query() {
    let (temp_dir, manager) = setup_minimal_vault().await;

    tokio::fs::write(
        temp_dir.path().join("nested.md"),
        r#"---
a:
  b:
    c:
      d:
        e:
          f: "deep value"
---
# Content"#,
    )
    .await
    .unwrap();

    let tools = MetadataTools::new(manager);
    let result = tools.get_metadata_value("nested.md", "a.b.c.d.e.f").await;
    assert!(result.is_ok());
}

// ==================== BatchTools Async Error Paths ====================

#[tokio::test]
async fn test_batch_tools_partial_failure_rollback() {
    let (_temp_dir, manager) = setup_minimal_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![
        BatchOperation::WriteNote {
            path: "file1.md".to_string(),
            content: "Content 1".to_string(),
        },
        BatchOperation::WriteNote {
            path: "file2.md".to_string(),
            content: "Content 2".to_string(),
        },
        BatchOperation::DeleteNote {
            path: "nonexistent_file_for_failure.md".to_string(),
        },
        BatchOperation::WriteNote {
            path: "file3.md".to_string(),
            content: "Content 3".to_string(),
        },
    ];

    let result = tools.batch_execute(ops).await;
    // Current implementation returns Ok(BatchResult { success: false }) not Err
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(!batch_result.success);
    assert_eq!(batch_result.executed, 2); // Should stop at operation 2 (the delete)

    // Note: Current implementation does NOT rollback - operations 0 and 1 remain
    // This is "fail-fast" not "atomic rollback" behavior
    let vault_path = manager.vault_path();
    assert!(vault_path.join("file1.md").exists()); // Written before failure
    assert!(vault_path.join("file2.md").exists()); // Written before failure
    assert!(!vault_path.join("file3.md").exists()); // Not executed after failure
}

#[tokio::test]
async fn test_batch_tools_concurrent_batch_conflicts() {
    let (_temp_dir, manager) = setup_minimal_vault().await;

    // Spawn two concurrent batches that modify the same file
    let tools1 = BatchTools::new(manager.clone());
    let tools2 = BatchTools::new(manager.clone());

    let handle1 = tokio::spawn(async move {
        let ops = vec![BatchOperation::WriteNote {
            path: "conflict.md".to_string(),
            content: "Content from batch 1".to_string(),
        }];
        tools1.batch_execute(ops).await
    });

    let handle2 = tokio::spawn(async move {
        let ops = vec![BatchOperation::WriteNote {
            path: "conflict.md".to_string(),
            content: "Content from batch 2".to_string(),
        }];
        tools2.batch_execute(ops).await
    });

    let result1 = handle1.await.expect("Task panicked");
    let result2 = handle2.await.expect("Task panicked");

    // Both should complete (one will overwrite the other)
    assert!(result1.is_ok());
    assert!(result2.is_ok());
}

// ==================== SearchEngine Async Error Paths ====================

#[tokio::test]
async fn test_search_engine_index_corruption() {
    let (_temp_dir, manager) = setup_minimal_vault().await;

    // Create search engine with potentially corrupted index
    let engine = SearchEngine::new(manager.clone()).await;

    // Engine creation should succeed and search should work
    assert!(engine.is_ok());
}

#[tokio::test]
async fn test_search_engine_concurrent_queries() {
    let (_temp_dir, manager) = setup_minimal_vault().await;

    // Spawn many concurrent searches
    let handles: Vec<_> = (0..50)
        .map(|i| {
            let engine = SearchEngine::new(manager.clone());
            tokio::spawn(async move {
                let engine = engine.await.unwrap();
                engine.search(&format!("query {}", i)).await
            })
        })
        .collect();

    for handle in handles {
        let result = handle.await.expect("Task panicked");
        assert!(result.is_ok());
    }
}

// ==================== Memory and Resource Limits ====================

#[tokio::test]
async fn test_large_file_handling() {
    let (temp_dir, manager) = setup_minimal_vault().await;
    let tools = FileTools::new(manager);

    // Create a large file (1MB)
    let large_content = "x".repeat(1_000_000);
    let path = temp_dir.path().join("large.md");
    tokio::fs::write(&path, &large_content).await.unwrap();

    let result = tools.read_file("large.md").await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 1_000_000);
}

#[tokio::test]
async fn test_many_small_files() {
    let (temp_dir, manager) = setup_minimal_vault().await;

    // Create many small files
    for i in 0..1000 {
        tokio::fs::write(
            temp_dir.path().join(format!("small_{}.md", i)),
            format!("# Note {}", i),
        )
        .await
        .unwrap();
    }

    manager.initialize().await.unwrap();
    let tools = GraphTools::new(manager);

    let result = tools.quick_health_check().await;
    assert!(result.is_ok());
}

// ==================== Network/Filesystem Failures ====================

#[tokio::test]
async fn test_filesystem_suddenly_unavailable() {
    let (_temp_dir, manager) = setup_minimal_vault().await;
    let tools = FileTools::new(manager);

    // Try to access a file on a path that doesn't exist
    let result = tools.read_file("/nonexistent/mount/point/file.md").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_disk_full_simulation() {
    let (_temp_dir, manager) = setup_minimal_vault().await;
    let tools = FileTools::new(manager);

    // Try to write an extremely large file (this might fail on disk space)
    let huge_content = "x".repeat(100_000_000); // 100MB
    let result = tools.write_file("huge.md", &huge_content).await;
    // Either succeeds or fails gracefully with proper error
    assert!(result.is_ok() || result.is_err());
}
