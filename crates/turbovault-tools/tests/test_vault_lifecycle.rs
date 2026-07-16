//! Public contract tests for vault creation, registration, and validation.

use std::sync::Arc;

use tempfile::TempDir;
use turbovault_core::{MultiVaultManager, ServerConfig};
use turbovault_tools::VaultLifecycleTools;

fn lifecycle_tools() -> VaultLifecycleTools {
    let manager = MultiVaultManager::empty(ServerConfig::new()).expect("multi-vault manager");
    VaultLifecycleTools::new(Arc::new(manager))
}

#[tokio::test]
async fn every_template_creates_and_registers_its_expected_structure() {
    let root = TempDir::new().expect("template root");
    let tools = lifecycle_tools();
    let templates = [
        ("default", vec!["Areas", "Projects", "Resources", "Archive"]),
        (
            "research",
            vec!["Literature", "Theory", "Findings", "Hypotheses"],
        ),
        (
            "team",
            vec!["Team", "Projects", "Decisions", "Documentation"],
        ),
    ];

    for (template, directories) in templates {
        let path = root.path().join(template);
        let info = tools
            .create_vault(template, &path, Some(template))
            .await
            .unwrap_or_else(|error| panic!("create {template} template: {error}"));

        assert_eq!(info.name, template);
        assert_eq!(info.path, path);
        assert!(path.join(".obsidian").is_dir());
        for directory in directories {
            assert!(path.join(directory).is_dir(), "{template}/{directory}");
        }

        let validation = tools
            .validate_vault(template)
            .await
            .expect("validate created vault");
        assert_eq!(validation["is_valid"], true);
        assert_eq!(validation["issues"], serde_json::json!([]));
    }

    assert!(root.path().join("default/README.md").is_file());
    assert!(!root.path().join("research/README.md").exists());
    assert!(!root.path().join("team/README.md").exists());
    assert_eq!(tools.list_vaults().await.expect("list templates").len(), 3);
    assert_eq!(
        tools.get_active_vault().await.expect("active vault"),
        "default"
    );

    let research = tools
        .get_vault_config("research")
        .await
        .expect("research config");
    assert_eq!(research.path, root.path().join("research"));
    tools.set_active_vault("team").await.expect("activate team");
    assert_eq!(tools.get_active_vault().await.expect("active team"), "team");

    tools.remove_vault("team").await.expect("remove team");
    assert_eq!(
        tools.get_active_vault().await.expect("reassigned active"),
        "default"
    );
    assert!(root.path().join("team/.obsidian").is_dir());
}

#[tokio::test]
async fn default_creation_and_registered_directory_validation_report_real_state() {
    let root = TempDir::new().expect("validation root");
    let tools = lifecycle_tools();
    let default_path = root.path().join("implicit-default");

    tools
        .create_vault("implicit", &default_path, None)
        .await
        .expect("create implicit default template");
    assert!(default_path.join("Areas").is_dir());
    assert!(default_path.join("README.md").is_file());

    let plain_path = root.path().join("plain-directory");
    tokio::fs::create_dir(&plain_path)
        .await
        .expect("plain directory");
    tools
        .add_vault_from_path("plain", &plain_path)
        .await
        .expect("register plain directory");

    let missing_marker = tools
        .validate_vault("plain")
        .await
        .expect("validate missing marker");
    assert_eq!(missing_marker["is_valid"], false);
    assert_eq!(
        missing_marker["issues"],
        serde_json::json!(["Missing .obsidian directory"])
    );

    tokio::fs::remove_dir(&plain_path)
        .await
        .expect("remove registered directory");
    let inaccessible = tools
        .validate_vault("plain")
        .await
        .expect("validate inaccessible path");
    assert_eq!(inaccessible["is_valid"], false);
    let issues = inaccessible["issues"]
        .as_array()
        .expect("validation issues");
    assert_eq!(issues.len(), 2);
    assert!(issues.contains(&serde_json::json!("Missing .obsidian directory")));
    assert!(
        issues
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(|issue| issue.starts_with("Cannot access vault:"))
    );
}

#[tokio::test]
async fn lifecycle_rejects_invalid_names_templates_paths_and_duplicates() {
    let root = TempDir::new().expect("error root");
    let tools = lifecycle_tools();

    for name in ["", "contains spaces", &"x".repeat(65)] {
        let path = root.path().join("invalid-name");
        assert!(tools.create_vault(name, &path, None).await.is_err());
        assert!(!path.exists());
    }

    let invalid_template_path = root.path().join("invalid-template");
    assert!(
        tools
            .create_vault("invalid-template", &invalid_template_path, Some("unknown"))
            .await
            .is_err()
    );
    assert!(!invalid_template_path.exists());

    let missing_path = root.path().join("missing");
    let missing = tools
        .add_vault_from_path("missing", &missing_path)
        .await
        .expect_err("registration must not create a missing path");
    assert!(missing.to_string().contains("Use create_vault"));
    assert!(!missing_path.exists());

    let file_path = root.path().join("file");
    tokio::fs::write(&file_path, "not a directory")
        .await
        .expect("file fixture");
    assert!(tools.create_vault("file", &file_path, None).await.is_err());
    assert!(
        tools
            .add_vault_from_path("other-file", &file_path)
            .await
            .is_err()
    );

    let registered_path = root.path().join("registered");
    tokio::fs::create_dir(&registered_path)
        .await
        .expect("registered directory");
    tools
        .add_vault_from_path("registered", &registered_path)
        .await
        .expect("first registration");
    assert!(
        tools
            .add_vault_from_path("registered", &registered_path)
            .await
            .is_err()
    );
    assert!(
        tools
            .create_vault("registered", &root.path().join("duplicate"), None)
            .await
            .is_err()
    );

    assert!(tools.validate_vault("unknown").await.is_err());
    assert!(tools.get_vault_config("unknown").await.is_err());
    assert!(tools.set_active_vault("unknown").await.is_err());
    assert!(tools.remove_vault("unknown").await.is_err());
}
