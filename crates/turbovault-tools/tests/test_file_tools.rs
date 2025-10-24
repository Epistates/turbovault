//! Unit tests for FileTools

use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::FileTools;
use turbovault_vault::VaultManager;
use std::sync::Arc;
use tempfile::TempDir;

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
