//! Integration test for search engine with the TEST vault

use turbovault_core::config::{ServerConfig, VaultConfig};
use turbovault_tools::search_engine::SearchEngine;
use turbovault_vault::VaultManager;
use std::path::PathBuf;
use std::sync::Arc;

fn create_test_config(vault_path: &PathBuf) -> ServerConfig {
    let mut config = ServerConfig::new();
    let vault_config = VaultConfig::builder("TEST", vault_path).build().unwrap();
    config.vaults.push(vault_config);
    config
}

#[tokio::test]
async fn test_search_engine_with_test_vault() {
    let vault_path = PathBuf::from("/Users/nickpaterno/work/TEST");

    println!("\n=== SEARCH ENGINE INTEGRATION TEST ===");
    println!("Vault path: {:?}", vault_path);

    // Create vault config
    let config = create_test_config(&vault_path);

    // Create vault manager
    let manager = Arc::new(VaultManager::new(config).expect("Failed to create vault manager"));

    // Scan vault files
    let files = manager.scan_vault().await.expect("Failed to scan vault");
    println!("Scanned {} files total", files.len());

    for (i, file) in files.iter().take(10).enumerate() {
        println!("  [{}] {:?}", i, file);
    }

    // Create search engine
    println!("\nCreating search engine...");
    let engine = SearchEngine::new(manager.clone())
        .await
        .expect("Failed to create search engine");

    println!("Search engine created");

    // Test basic search
    println!("\n--- Testing Search Queries ---");

    let queries = vec!["testing", "XYZABC123", "capabilities", "search"];

    for query in queries {
        println!("\nSearching for: '{}'", query);
        match engine.search(query).await {
            Ok(results) => {
                println!("  Found {} results", results.len());
                for (i, result) in results.iter().take(3).enumerate() {
                    println!("  [{}] {} (score: {:.2})", i, result.path, result.score);
                }
            }
            Err(e) => {
                println!("  Error: {}", e);
            }
        }
    }
}
