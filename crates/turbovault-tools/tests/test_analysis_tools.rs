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
async fn analysis_reports_graph_statistics_cycles_and_relative_orphan_paths() {
    let (temp, manager) = setup_vault(&[
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

    let density = tools.get_link_density().await.expect("link density");
    assert!((density - (1.0 / 3.0)).abs() < f64::EPSILON);

    let metrics = tools
        .get_connectivity_metrics()
        .await
        .expect("connectivity metrics");
    assert_eq!(metrics["total_files"], 3);
    assert_eq!(metrics["total_links"], 2);
    assert_eq!(metrics["orphaned_files"], 1);
    assert_eq!(metrics["connected_files"], 2);
    assert!((metrics["connectivity_rate"].as_f64().unwrap() - (2.0 / 3.0)).abs() < f64::EPSILON);

    let orphans = tools.list_orphaned_notes().await.expect("orphaned notes");
    assert_eq!(orphans, vec!["nested/orphan.md"]);

    let cycles = tools.detect_cycles().await.expect("cycles");
    assert!(!cycles.is_empty());
    assert!(cycles.iter().any(|cycle| {
        cycle.iter().any(|path| path == "alpha.md") && cycle.iter().any(|path| path == "beta.md")
    }));
    assert!(cycles.iter().flatten().all(|path| {
        !path.starts_with(
            temp.path()
                .to_str()
                .expect("temporary vault path should be UTF-8"),
        )
    }));
}

#[tokio::test]
async fn empty_and_single_note_vaults_have_zero_density() {
    let (_empty, manager) = setup_vault(&[]).await;
    let tools = AnalysisTools::new(manager);

    assert_eq!(tools.get_link_density().await.expect("empty density"), 0.0);
    let empty_metrics = tools
        .get_connectivity_metrics()
        .await
        .expect("empty connectivity");
    assert_eq!(empty_metrics["total_files"], 0);
    assert_eq!(empty_metrics["connected_files"], 0);
    assert_eq!(empty_metrics["connectivity_rate"], 0.0);
    assert!(tools.list_orphaned_notes().await.unwrap().is_empty());
    assert!(tools.detect_cycles().await.unwrap().is_empty());

    let (_single, manager) = setup_vault(&[("only.md", "# Only\n")]).await;
    let tools = AnalysisTools::new(manager);
    assert_eq!(tools.get_link_density().await.expect("single density"), 0.0);
    assert_eq!(tools.list_orphaned_notes().await.unwrap(), vec!["only.md"]);
}
