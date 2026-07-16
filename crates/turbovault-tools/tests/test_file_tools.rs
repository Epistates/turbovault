//! Unit tests for FileTools

use std::sync::Arc;
use tempfile::TempDir;
use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::FileTools;
use turbovault_vault::VaultManager;

async fn setup_test_vault() -> (TempDir, Arc<VaultManager>) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let vault_path = temp_dir.path();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path)
        .build()
        .expect("Failed to create vault config");
    config.vaults.push(vault_config);

    let manager = VaultManager::new(config).expect("Failed to create vault manager");
    manager
        .initialize()
        .await
        .expect("Failed to initialize vault");

    (temp_dir, Arc::new(manager))
}

#[tokio::test]
async fn test_read_file_success() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // Create a test file
    let content = "# Test Note\nHello World";
    tokio::fs::write(temp_dir.path().join("test.md"), content)
        .await
        .expect("Failed to write test file");

    // Read it back
    let result = tools.read_file("test.md").await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), content);
}

#[tokio::test]
async fn test_read_file_not_found() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    let result = tools.read_file("nonexistent.md").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_write_file_success() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    let content = "# New Note\nContent here";
    let result = tools.write_file("new.md", content).await;
    assert!(result.is_ok());

    // Verify it was written
    let read_result = tools.read_file("new.md").await;
    assert!(read_result.is_ok());
    assert_eq!(read_result.unwrap(), content);
}

#[tokio::test]
async fn test_write_file_creates_directories() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    let content = "# Nested Note";
    let result = tools.write_file("folder/subfolder/note.md", content).await;
    assert!(result.is_ok());

    // Verify it was created
    let read_result = tools.read_file("folder/subfolder/note.md").await;
    assert!(read_result.is_ok());
}

#[tokio::test]
async fn test_delete_file_success() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // Create a file
    tokio::fs::write(temp_dir.path().join("delete.md"), "content")
        .await
        .expect("Failed to create file");

    let result = tools.delete_file("delete.md").await;
    assert!(result.is_ok());

    // Verify it was deleted
    let read_result = tools.read_file("delete.md").await;
    assert!(read_result.is_err());
}

#[tokio::test]
async fn test_delete_file_not_found() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    let result = tools.delete_file("nonexistent.md").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_move_file_success() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // Create source file
    let content = "# Moving Note";
    tokio::fs::write(temp_dir.path().join("source.md"), content)
        .await
        .expect("Failed to create source file");

    let result = tools.move_file("source.md", "destination.md").await;
    assert!(result.is_ok());

    // Verify source is gone
    let source_result = tools.read_file("source.md").await;
    assert!(source_result.is_err());

    // Verify destination exists
    let dest_result = tools.read_file("destination.md").await;
    assert!(dest_result.is_ok());
    assert_eq!(dest_result.unwrap(), content);
}

#[tokio::test]
async fn test_move_file_does_not_rewrite_incoming_wikilinks() {
    let (temp_dir, manager) = setup_test_vault().await;
    tokio::fs::write(temp_dir.path().join("target.md"), "# Target")
        .await
        .unwrap();
    tokio::fs::write(
        temp_dir.path().join("source.md"),
        "Links to [[target]] and must remain explicit.",
    )
    .await
    .unwrap();
    manager.initialize().await.unwrap();

    let tools = FileTools::new(manager);
    tools
        .move_file("target.md", "archive/target.md")
        .await
        .unwrap();

    let source = tokio::fs::read_to_string(temp_dir.path().join("source.md"))
        .await
        .unwrap();
    assert!(source.contains("[[target]]"));
    assert!(!source.contains("[[archive/target]]"));
}

#[tokio::test]
async fn test_move_file_with_directory_creation() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // Create source file
    tokio::fs::write(temp_dir.path().join("source.md"), "content")
        .await
        .expect("Failed to create source file");

    let result = tools.move_file("source.md", "new/folder/dest.md").await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_copy_file_success() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // Create source file
    let content = "# Copy Test";
    tokio::fs::write(temp_dir.path().join("original.md"), content)
        .await
        .expect("Failed to create source file");

    let result = tools.copy_file("original.md", "copy.md").await;
    assert!(result.is_ok());

    // Verify both exist
    let original_result = tools.read_file("original.md").await;
    assert!(original_result.is_ok());

    let copy_result = tools.read_file("copy.md").await;
    assert!(copy_result.is_ok());
    assert_eq!(original_result.unwrap(), copy_result.unwrap());
}

#[tokio::test]
async fn test_edit_file_success() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // Create initial file
    let initial_content = "# Title\nOriginal content\nMore text";
    tokio::fs::write(temp_dir.path().join("edit.md"), initial_content)
        .await
        .expect("Failed to create file");

    // Edit with SEARCH/REPLACE block
    let edits = r#"
<<<<<<< SEARCH
Original content
=======
Updated content
>>>>>>> REPLACE
"#;

    let result = tools.edit_file("edit.md", edits, None, false).await;
    assert!(result.is_ok());

    // Verify the edit was applied
    let new_content = tools.read_file("edit.md").await.unwrap();
    assert!(new_content.contains("Updated content"));
    assert!(!new_content.contains("Original content"));
}

#[tokio::test]
async fn test_edit_file_dry_run() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // Create initial file
    let initial_content = "# Title\nOriginal content";
    tokio::fs::write(temp_dir.path().join("dryrun.md"), initial_content)
        .await
        .expect("Failed to create file");

    let edits = r#"
<<<<<<< SEARCH
Original content
=======
Changed content
>>>>>>> REPLACE
"#;

    let result = tools.edit_file("dryrun.md", edits, None, true).await;
    assert!(result.is_ok());

    // Verify file was NOT changed (dry run)
    let content = tools.read_file("dryrun.md").await.unwrap();
    assert_eq!(content, initial_content);
}

#[tokio::test]
async fn test_edit_file_with_hash_validation() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // Create initial file
    let initial_content = "# Title\nContent";
    tokio::fs::write(temp_dir.path().join("hash.md"), initial_content)
        .await
        .expect("Failed to create file");

    // Get hash using the vault's compute_hash function
    let expected_hash = turbovault_vault::compute_hash(initial_content);

    let edits = r#"
<<<<<<< SEARCH
Content
=======
New Content
>>>>>>> REPLACE
"#;

    // Should succeed with correct hash
    let result = tools
        .edit_file("hash.md", edits, Some(&expected_hash), false)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_edit_file_with_wrong_hash() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // Create initial file
    tokio::fs::write(temp_dir.path().join("wronghash.md"), "Content")
        .await
        .expect("Failed to create file");

    let edits = r#"
<<<<<<< SEARCH
Content
=======
New Content
>>>>>>> REPLACE
"#;

    // Should fail with wrong hash
    let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
    let result = tools
        .edit_file("wronghash.md", edits, Some(wrong_hash), false)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_path_traversal_prevention_read() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    let result = tools.read_file("../../etc/passwd").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_path_traversal_prevention_write() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    let result = tools.write_file("../../tmp/evil.md", "content").await;
    assert!(result.is_err());
}

// ==================== Path Traversal Tests ====================

#[tokio::test]
async fn test_delete_file_path_traversal_rejected() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);
    let result = tools.delete_file("../../etc/passwd").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_move_file_destination_path_traversal() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);
    tokio::fs::write(temp_dir.path().join("source.md"), "content")
        .await
        .unwrap();
    let result = tools.move_file("source.md", "../../tmp/evil.md").await;
    assert!(result.is_err());
    // Source should still exist (move should have been rejected before any IO)
    assert!(temp_dir.path().join("source.md").exists());
}

#[tokio::test]
async fn test_move_file_source_not_found() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);
    let result = tools.move_file("nonexistent.md", "dest.md").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_copy_file_destination_path_traversal() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);
    tokio::fs::write(temp_dir.path().join("source.md"), "content")
        .await
        .unwrap();
    let result = tools.copy_file("source.md", "../../tmp/evil.md").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_copy_file_source_not_found() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);
    let result = tools.copy_file("nonexistent.md", "dest.md").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_copy_file_creates_nested_dirs() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);
    tokio::fs::write(temp_dir.path().join("source.md"), "content")
        .await
        .unwrap();
    let result = tools
        .copy_file("source.md", "deep/nested/dir/copy.md")
        .await;
    assert!(result.is_ok());
    let read = tools.read_file("deep/nested/dir/copy.md").await;
    assert!(read.is_ok());
    assert_eq!(read.unwrap(), "content");
}

// ==================== get_notes_info Tests ====================

#[tokio::test]
async fn test_get_notes_info_all_exist() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    tokio::fs::write(temp_dir.path().join("a.md"), "---\ntitle: A\n---\n# A")
        .await
        .unwrap();
    tokio::fs::write(temp_dir.path().join("b.md"), "# B no frontmatter")
        .await
        .unwrap();

    let paths = vec!["a.md".to_string(), "b.md".to_string()];
    let results = tools.get_notes_info(&paths).await.unwrap();
    assert_eq!(results.len(), 2);

    assert!(results[0].exists);
    assert!(results[0].size_bytes.unwrap() > 0);
    assert!(results[0].modified_at.is_some());
    assert_eq!(results[0].has_frontmatter, Some(true));

    assert!(results[1].exists);
    assert_eq!(results[1].has_frontmatter, Some(false));
}

#[tokio::test]
async fn test_get_notes_info_mixed_exist_and_missing() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    tokio::fs::write(temp_dir.path().join("exists.md"), "content")
        .await
        .unwrap();

    let paths = vec!["exists.md".to_string(), "missing.md".to_string()];
    let results = tools.get_notes_info(&paths).await.unwrap();

    assert!(results[0].exists);
    assert!(!results[1].exists);
    assert!(results[1].size_bytes.is_none());
    assert!(results[1].modified_at.is_none());
    assert!(results[1].has_frontmatter.is_none());
}

#[tokio::test]
async fn test_get_notes_info_path_traversal_returns_not_found() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    let paths = vec!["../../etc/passwd".to_string()];
    let results = tools.get_notes_info(&paths).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(!results[0].exists);
}

#[tokio::test]
async fn test_get_notes_info_empty_input() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    let results = tools.get_notes_info(&[]).await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn test_get_notes_info_tiny_file_no_frontmatter() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // 2-byte file, too small for frontmatter marker
    tokio::fs::write(temp_dir.path().join("tiny.md"), "ab")
        .await
        .unwrap();

    let paths = vec!["tiny.md".to_string()];
    let results = tools.get_notes_info(&paths).await.unwrap();
    assert!(results[0].exists);
    assert_eq!(results[0].has_frontmatter, Some(false));
    assert_eq!(results[0].size_bytes, Some(2));
}

#[tokio::test]
async fn test_get_notes_info_crlf_frontmatter() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    tokio::fs::write(
        temp_dir.path().join("crlf.md"),
        "---\r\ntitle: Test\r\n---\r\n# Body",
    )
    .await
    .unwrap();

    let paths = vec!["crlf.md".to_string()];
    let results = tools.get_notes_info(&paths).await.unwrap();
    assert_eq!(results[0].has_frontmatter, Some(true));
}

// ==================== write_file_with_mode Tests ====================

#[tokio::test]
async fn test_write_mode_append_empty_file() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // Create empty file first
    tools.write_file("empty.md", "").await.unwrap();
    tools
        .write_file_with_mode(
            "empty.md",
            "appended content",
            turbovault_tools::WriteMode::Append,
            None,
        )
        .await
        .unwrap();

    let result = tools.read_file("empty.md").await.unwrap();
    assert_eq!(result, "appended content");
}

#[tokio::test]
async fn test_write_mode_append_nonempty_file() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    tools.write_file("existing.md", "line1").await.unwrap();
    tools
        .write_file_with_mode(
            "existing.md",
            "line2",
            turbovault_tools::WriteMode::Append,
            None,
        )
        .await
        .unwrap();

    let result = tools.read_file("existing.md").await.unwrap();
    assert_eq!(result, "line1\nline2");
}

#[tokio::test]
async fn test_write_mode_prepend_with_frontmatter() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    tools
        .write_file("fm.md", "---\ntitle: Test\n---\n# Body\nContent")
        .await
        .unwrap();
    tools
        .write_file_with_mode(
            "fm.md",
            "INSERTED",
            turbovault_tools::WriteMode::Prepend,
            None,
        )
        .await
        .unwrap();

    let result = tools.read_file("fm.md").await.unwrap();
    // Frontmatter should come first, then INSERTED, then body
    assert!(result.starts_with("---\n"));
    let fm_end = result.find("---\n").unwrap(); // opening
    let second_dashes = result[fm_end + 4..].find("---\n").unwrap() + fm_end + 4;
    let after_fm = &result[second_dashes + 4..];
    assert!(after_fm.starts_with("INSERTED"));
    assert!(after_fm.contains("# Body"));
}

#[tokio::test]
async fn test_write_mode_prepend_without_frontmatter() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    tools
        .write_file("nofm.md", "# Body\nContent")
        .await
        .unwrap();
    tools
        .write_file_with_mode(
            "nofm.md",
            "INSERTED",
            turbovault_tools::WriteMode::Prepend,
            None,
        )
        .await
        .unwrap();

    let result = tools.read_file("nofm.md").await.unwrap();
    assert!(result.starts_with("INSERTED\n"));
    assert!(result.contains("# Body"));
}

#[tokio::test]
async fn test_write_mode_prepend_empty_file() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    tools.write_file("empty.md", "").await.unwrap();
    tools
        .write_file_with_mode(
            "empty.md",
            "new content",
            turbovault_tools::WriteMode::Prepend,
            None,
        )
        .await
        .unwrap();

    let result = tools.read_file("empty.md").await.unwrap();
    assert_eq!(result, "new content");
}

#[tokio::test]
async fn test_write_mode_prepend_malformed_frontmatter() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager);

    // Opens with --- but never closes
    tools
        .write_file("malformed.md", "---\ntitle: Test\nno closing\n# Body")
        .await
        .unwrap();
    tools
        .write_file_with_mode(
            "malformed.md",
            "INSERTED",
            turbovault_tools::WriteMode::Prepend,
            None,
        )
        .await
        .unwrap();

    let result = tools.read_file("malformed.md").await.unwrap();
    // Should prepend before everything since frontmatter is malformed
    assert!(result.starts_with("INSERTED\n"));
}

#[tokio::test]
async fn test_concurrent_writes() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = FileTools::new(manager.clone());

    // Spawn multiple concurrent writes
    let handles: Vec<_> = (0..10)
        .map(|i| {
            let tools_clone = FileTools::new(manager.clone());
            tokio::spawn(async move {
                let path = format!("concurrent_{}.md", i);
                let content = format!("Content {}", i);
                tools_clone.write_file(&path, &content).await
            })
        })
        .collect();

    // Wait for all writes
    for handle in handles {
        let result = handle.await.expect("Task panicked");
        assert!(result.is_ok());
    }

    // Verify all files exist
    for i in 0..10 {
        let path = format!("concurrent_{}.md", i);
        let result = tools.read_file(&path).await;
        assert!(result.is_ok());
    }
}
