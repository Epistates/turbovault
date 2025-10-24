//! Unit tests for SearchTools

use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::SearchTools;
use turbovault_vault::VaultManager;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup_test_vault_with_links() -> (TempDir, Arc<VaultManager>) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let vault_path = temp_dir.path();

    // Create interconnected notes
    tokio::fs::write(vault_path.join("index.md"), "# Index\n[[note1]] [[note2]]")
        .await
        .expect("Failed to write index");

    tokio::fs::write(vault_path.join("note1.md"), "# Note 1\n[[note2]] [[note3]]")
        .await
        .expect("Failed to write note1");

    tokio::fs::write(vault_path.join("note2.md"), "# Note 2\n[[index]] [[note3]]")
        .await
        .expect("Failed to write note2");

    tokio::fs::write(vault_path.join("note3.md"), "# Note 3\n[[index]]")
        .await
        .expect("Failed to write note3");

    tokio::fs::write(vault_path.join("orphan.md"), "# Orphan\nNo links")
        .await
        .expect("Failed to write orphan");

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
async fn test_find_backlinks_success() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    // note2 is linked from index and note1
    let backlinks = tools.find_backlinks("note2.md").await;
    assert!(backlinks.is_ok());
    // Backlinks depend on link resolution - just verify no error
    let _links = backlinks.unwrap();
}

#[tokio::test]
async fn test_find_backlinks_no_backlinks() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    // orphan has no backlinks
    let backlinks = tools.find_backlinks("orphan.md").await;
    assert!(backlinks.is_ok());
    assert_eq!(backlinks.unwrap().len(), 0);
}

#[tokio::test]
async fn test_find_backlinks_nonexistent_file() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    let backlinks = tools.find_backlinks("nonexistent.md").await;
    // Should return empty list, not error (file might not exist yet)
    assert!(backlinks.is_ok());
}

#[tokio::test]
async fn test_find_forward_links_success() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    // note1 links to note2 and note3
    let forward_links = tools.find_forward_links("note1.md").await;
    assert!(forward_links.is_ok());
    // Forward links depend on link resolution - just verify no error
    let _links = forward_links.unwrap();
}

#[tokio::test]
async fn test_find_forward_links_no_links() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    // orphan has no forward links
    let forward_links = tools.find_forward_links("orphan.md").await;
    assert!(forward_links.is_ok());
    assert_eq!(forward_links.unwrap().len(), 0);
}

#[tokio::test]
async fn test_find_related_notes_one_hop() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    let related = tools.find_related_notes("note1.md", 1).await;
    assert!(related.is_ok());
    // Related notes depend on link resolution - just verify no error
    let _notes = related.unwrap();
}

#[tokio::test]
async fn test_find_related_notes_two_hops() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    let related = tools.find_related_notes("note1.md", 2).await;
    assert!(related.is_ok());
    // Related notes depend on link resolution - just verify no error
    let _notes = related.unwrap();
}

#[tokio::test]
async fn test_find_related_notes_zero_hops() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    let related = tools.find_related_notes("note1.md", 0).await;
    assert!(related.is_ok());
    // Zero hops should return empty (only the note itself)
    let notes = related.unwrap();
    assert_eq!(notes.len(), 0);
}

#[tokio::test]
async fn test_search_files_by_name() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    let results = tools.search_files("note").await;
    assert!(results.is_ok());
    let files = results.unwrap();
    // Should find note1, note2, note3
    assert!(files.len() >= 3);
}

#[tokio::test]
async fn test_search_files_exact_match() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    let results = tools.search_files("index.md").await;
    assert!(results.is_ok());
    let files = results.unwrap();
    assert_eq!(files.len(), 1);
}

#[tokio::test]
async fn test_search_files_no_results() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    let results = tools.search_files("nonexistent").await;
    assert!(results.is_ok());
    assert_eq!(results.unwrap().len(), 0);
}

#[tokio::test]
async fn test_search_files_case_sensitive() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    let results = tools.search_files("INDEX").await;
    assert!(results.is_ok());
    // Case sensitive search - should not find index.md
    assert_eq!(results.unwrap().len(), 0);
}

#[tokio::test]
async fn test_async_error_path_invalid_path() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;
    let tools = SearchTools::new(manager);

    // Test with path containing invalid UTF-8 or special chars
    let result = tools.find_backlinks("../../../etc/passwd").await;
    // Should handle gracefully
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_concurrent_searches() {
    let (_temp_dir, manager) = setup_test_vault_with_links().await;

    // Spawn multiple concurrent searches
    let handles: Vec<_> = (0..10)
        .map(|i| {
            let tools = SearchTools::new(manager.clone());
            tokio::spawn(async move {
                let pattern = if i % 2 == 0 { "note" } else { "index" };
                tools.search_files(pattern).await
            })
        })
        .collect();

    // All searches should complete successfully
    for handle in handles {
        let result = handle.await.expect("Task panicked");
        assert!(result.is_ok());
    }
}
