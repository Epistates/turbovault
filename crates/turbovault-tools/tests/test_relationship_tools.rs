//! Public contract tests for relationship scoring and graph recommendations.

use std::sync::Arc;

use tempfile::TempDir;
use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::RelationshipTools;
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
        VaultConfig::builder("relationships", temp.path())
            .build()
            .expect("vault config"),
    );
    let manager = Arc::new(VaultManager::new(config).expect("vault manager"));
    manager.initialize().await.expect("initialize vault");

    (temp, manager)
}

async fn relationship_fixture() -> (TempDir, Arc<VaultManager>) {
    setup_vault(&[
        ("source.md", "# Source\n\n[[target]]\n"),
        ("target.md", "# Target\n\n[[source]]\n"),
        ("candidate-high.md", "# High candidate\n"),
        ("candidate-low.md", "# Low candidate\n"),
        (
            "references/ref-a.md",
            "# Reference A\n\n[[source]] [[target]] [[candidate-high]] [[candidate-low]]\n",
        ),
        (
            "references/ref-b.md",
            "# Reference B\n\n[[source]] [[candidate-high]]\n",
        ),
        ("orphan.md", "# Orphan\n"),
    ])
    .await
}

#[tokio::test]
async fn link_strength_uses_resolved_graph_paths_and_reports_components() {
    let (_temp, manager) = relationship_fixture().await;
    let tools = RelationshipTools::new(manager);

    let strength = tools
        .get_link_strength("source.md", "target.md")
        .await
        .expect("link strength");
    assert_eq!(strength["source"], "source.md");
    assert_eq!(strength["target"], "target.md");
    assert_eq!(strength["strength"], 1.0);
    assert_eq!(strength["components"]["direct_links"], 1);
    assert_eq!(strength["components"]["backlinks"], 1);
    assert_eq!(strength["components"]["shared_references"], 1);
    assert!(
        strength["interpretation"]
            .as_str()
            .expect("strength interpretation")
            .starts_with("Very strong")
    );

    let none = tools
        .get_link_strength("source.md", "orphan.md")
        .await
        .expect("unconnected strength");
    assert_eq!(none["strength"], 0.0);
    assert_eq!(none["interpretation"], "No connection");

    assert!(
        tools
            .get_link_strength("../outside.md", "target.md")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn suggestions_are_ranked_limited_relative_and_exclude_existing_links() {
    let (temp, manager) = relationship_fixture().await;
    let tools = RelationshipTools::new(manager);

    let suggestions = tools
        .suggest_links("source.md", 10)
        .await
        .expect("link suggestions");
    assert_eq!(suggestions["file"], "source.md");
    let candidates = suggestions["suggestions"]
        .as_array()
        .expect("suggestion array");
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0]["target"], "candidate-high.md");
    assert_eq!(candidates[0]["strength"], 0.6);
    assert_eq!(
        candidates[0]["reasons"],
        serde_json::json!(["2 shared backlinks"])
    );
    assert_eq!(candidates[1]["target"], "candidate-low.md");
    assert_eq!(candidates[1]["strength"], 0.3);
    assert!(candidates.iter().all(|candidate| {
        let path = candidate["target"].as_str().expect("suggestion path");
        path != "source.md"
            && path != "target.md"
            && !path.starts_with(temp.path().to_str().expect("temporary vault path"))
    }));

    let limited = tools
        .suggest_links("source.md", 1)
        .await
        .expect("limited suggestions");
    assert_eq!(limited["suggestions"].as_array().unwrap().len(), 1);

    let empty = tools
        .suggest_links("source.md", 0)
        .await
        .expect("zero-limit suggestions");
    assert!(empty["suggestions"].as_array().unwrap().is_empty());

    assert!(tools.suggest_links("../outside.md", 5).await.is_err());
}

#[tokio::test]
async fn centrality_handles_empty_graphs_and_labels_distinct_roles() {
    let (temp, manager) = setup_vault(&[
        (
            "connector.md",
            "# Connector\n\n[[authority]] [[leaf-1]] [[leaf-2]] [[leaf-3]]\n",
        ),
        ("authority.md", "# Authority\n"),
        ("leaf-1.md", "# Leaf 1\n\n[[authority]]\n"),
        ("leaf-2.md", "# Leaf 2\n\n[[authority]]\n"),
        ("leaf-3.md", "# Leaf 3\n\n[[authority]]\n"),
    ])
    .await;
    let tools = RelationshipTools::new(manager);

    let result = tools
        .get_centrality_ranking()
        .await
        .expect("centrality ranking");
    assert_eq!(result["total_files"], 5);
    let rankings = result["rankings"].as_array().expect("rankings");
    assert_eq!(rankings.len(), 5);
    assert!(rankings.iter().enumerate().all(|(index, entry)| {
        entry["rank"] == index + 1
            && entry["score"]
                .as_f64()
                .is_some_and(|score| (0.0..=1.0).contains(&score))
            && !entry["file"]
                .as_str()
                .expect("ranked path")
                .starts_with(temp.path().to_str().expect("temporary vault path"))
    }));
    assert!(rankings.iter().any(|entry| {
        entry["file"] == "authority.md" && entry["interpretation"] == "Authority file"
    }));
    assert!(rankings.iter().any(|entry| {
        entry["file"] == "connector.md" && entry["interpretation"] == "Highly connected"
    }));

    let (_empty, manager) = setup_vault(&[]).await;
    let empty = RelationshipTools::new(manager)
        .get_centrality_ranking()
        .await
        .expect("empty centrality ranking");
    assert_eq!(empty["total_files"], 0);
    assert!(empty["rankings"].as_array().unwrap().is_empty());
}
