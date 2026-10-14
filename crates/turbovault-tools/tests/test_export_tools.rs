//! The exports report what the vault actually holds: its broken links, the
//! same health counts and score `vault_health` gives, and the vault's name.

use std::sync::Arc;
use tempfile::TempDir;
use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::ExportTools;
use turbovault_vault::VaultManager;

async fn vault(files: &[(&str, &str)]) -> (TempDir, Arc<VaultManager>) {
    let temp_dir = TempDir::new().unwrap();
    for (path, content) in files {
        let full = temp_dir.path().join(path);
        tokio::fs::create_dir_all(full.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(full, content).await.unwrap();
    }
    let mut config = ConfigProfile::Development.create_config();
    config.vaults.push(
        VaultConfig::builder("research", temp_dir.path())
            .build()
            .unwrap(),
    );
    let manager = VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    (temp_dir, Arc::new(manager))
}

fn notes() -> [(&'static str, &'static str); 3] {
    [
        ("notes/a.md", "# A\nSee [[b]] and [[missing note]].\n"),
        ("b.md", "# B\nBack to [[a]].\n"),
        ("orphan.md", "# Orphan\nNo links here.\n"),
    ]
}

#[tokio::test]
async fn broken_links_export_lists_each_broken_link() {
    let (_temp, manager) = vault(&notes()).await;
    let json = ExportTools::new(manager)
        .export_broken_links("json")
        .await
        .unwrap();
    let links: serde_json::Value = serde_json::from_str(&json).unwrap();
    let links = links.as_array().expect("a list of broken links");

    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0]["source_file"], "notes/a.md");
    assert_eq!(links[0]["target"], "missing note");
}

#[tokio::test]
async fn health_and_analysis_reports_count_what_the_vault_has() {
    let (_temp, manager) = vault(&notes()).await;
    let tools = ExportTools::new(manager);

    let health: serde_json::Value =
        serde_json::from_str(&tools.export_health_report("json").await.unwrap()).unwrap();
    assert_eq!(health["vault_name"], "research");
    assert_eq!(health["broken_links"], 1);
    assert_eq!(health["orphaned_notes"], 1);
    assert!(
        health["health_score"].as_u64().unwrap() < 100,
        "a vault with a broken link and an orphan isn't perfectly healthy: {health}"
    );

    let analysis: serde_json::Value =
        serde_json::from_str(&tools.export_analysis_report("json").await.unwrap()).unwrap();
    assert_eq!(analysis["vault_name"], "research");
    assert_eq!(analysis["broken_links_count"], 1);
    let recommendations = analysis["recommendations"].as_array().unwrap();
    assert!(
        recommendations
            .iter()
            .any(|r| r.as_str().unwrap().contains("1 broken link")),
        "{recommendations:?}"
    );

    let stats: serde_json::Value =
        serde_json::from_str(&tools.export_vault_stats("json").await.unwrap()).unwrap();
    assert_eq!(stats["vault_name"], "research");
    assert!(stats["total_words"].as_u64().unwrap() > 0);
}
