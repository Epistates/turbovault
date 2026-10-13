//! Public contract tests for the legacy analysis facade.

use std::sync::Arc;

use tempfile::TempDir;
use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::AnalysisTools;
use turbovault_vault::VaultManager;

async fn setup_vault(files: &[(&str, &str)]) -> (TempDir, Arc<VaultManager>) {
    let temp = TempDir::new().expect("temporary vault");
    for (path, content) in files {
        let full_path = temp.path().join(path);
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("create note parent");
        }
        tokio::fs::write(full_path, content)
            .await
            .expect("write note fixture");
    }

    let mut config = ConfigProfile::Development.create_config();
    config.vaults.push(
        VaultConfig::builder("analysis", temp.path())
            .build()
            .expect("vault config"),
    );
    let manager = Arc::new(VaultManager::new(config).expect("vault manager"));
    manager.initialize().await.expect("initialize vault");

    (temp, manager)
}

#[tokio::test]
async fn analysis_reports_vault_statistics() {
    let (_temp, manager) = setup_vault(&[
        ("alpha.md", "# Alpha\n\n[[beta]]\n"),
        ("beta.md", "# Beta\n\n[[alpha]]\n"),
        ("nested/orphan.md", "# Orphan\n"),
    ])
    .await;
    let tools = AnalysisTools::new(manager);

    let stats = tools.get_vault_stats().await.expect("vault statistics");
    assert_eq!(stats.total_files, 3);
    assert_eq!(stats.total_links, 2);
    assert_eq!(stats.orphaned_files, 1);
    assert!((stats.average_links_per_file - (2.0 / 3.0)).abs() < f64::EPSILON);
}

#[tokio::test]
async fn an_empty_vault_has_no_statistics() {
    let (_empty, manager) = setup_vault(&[]).await;
    let stats = AnalysisTools::new(manager).get_vault_stats().await.unwrap();
    assert_eq!(stats.total_files, 0);
    assert_eq!(stats.total_links, 0);
}
