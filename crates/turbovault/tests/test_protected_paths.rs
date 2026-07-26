//! The vault boundary is not the only boundary.
//!
//! A vault directory holds more than notes: `.obsidian/plugins/*/main.js` is
//! code Obsidian executes on load, `.git/hooks/*` is code git executes on
//! commit, and `.turbovault/` is the audit trail that records what the note
//! APIs did. Reaching any of those through `read_note`/`write_note` turns "edit
//! my notes" into code execution or a self-editable audit log, so both write
//! backends must refuse — the git substrate never passes through
//! `VaultManager::resolve_path`, so it needs its own coverage.

use turbomcp::prelude::*;
use turbovault::ObsidianMcpServer;
use turbovault_core::config::{VaultConfig, VaultGitConfig, WriteBackend};

const PROTECTED_PATHS: [&str; 3] = [
    ".obsidian/plugins/tasks/main.js",
    ".git/hooks/post-commit",
    ".turbovault/audit/operations.jsonl",
];

fn is_error(result: &McpResult<ToolResult>) -> bool {
    match result {
        Ok(tool_result) => tool_result.is_error.unwrap_or(false),
        Err(_) => true,
    }
}

async fn legacy_server() -> (tempfile::TempDir, ObsidianMcpServer) {
    let temp = tempfile::TempDir::new().expect("temp vault");
    let server = ObsidianMcpServer::new().expect("server");
    let config = VaultConfig::builder("protected", temp.path())
        .build()
        .expect("vault config");
    server
        .multi_vault()
        .add_vault(config)
        .await
        .expect("register vault");
    server
        .multi_vault()
        .set_active_vault("protected")
        .await
        .expect("select vault");
    (temp, server)
}

async fn git_server() -> (tempfile::TempDir, ObsidianMcpServer) {
    let temp = tempfile::TempDir::new().expect("temp vault");
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = git2::Repository::init_opts(temp.path(), &opts).expect("git init");
    std::fs::write(temp.path().join("seed.md"), "# Seed").expect("seed note");
    let tree_oid = {
        let mut index = repo.index().expect("index");
        index
            .add_path(std::path::Path::new("seed.md"))
            .expect("stage seed");
        index.write().expect("write index");
        index.write_tree().expect("write tree")
    };
    let tree = repo.find_tree(tree_oid).expect("tree");
    let signature = git2::Signature::now("Init", "init@example").expect("signature");
    repo.commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
        .expect("initial commit");

    let config = VaultConfig::builder("protected", temp.path())
        .write_backend(WriteBackend::Git)
        .git(VaultGitConfig::default())
        .build()
        .expect("vault config");
    let server = ObsidianMcpServer::new().expect("server");
    server
        .multi_vault()
        .add_vault(config)
        .await
        .expect("register vault");
    server
        .multi_vault()
        .set_active_vault("protected")
        .await
        .expect("select vault");
    (temp, server)
}

async fn assert_protected(temp: &tempfile::TempDir, server: &ObsidianMcpServer, backend: &str) {
    let ctx = RequestContext::new();

    for path in PROTECTED_PATHS {
        let write = server
            .call_tool(
                "write_note",
                serde_json::json!({"path": path, "content": "payload", "force": true}),
                &ctx,
            )
            .await;
        assert!(
            is_error(&write),
            "{backend}: write_note({path}) should be refused, got {write:?}"
        );
        assert!(
            !temp.path().join(path).exists(),
            "{backend}: {path} was written to disk"
        );

        let read = server
            .call_tool("read_note", serde_json::json!({"path": path}), &ctx)
            .await;
        assert!(
            is_error(&read),
            "{backend}: read_note({path}) should be refused, got {read:?}"
        );
    }

    // The policy must not swallow ordinary notes.
    let allowed = server
        .call_tool(
            "write_note",
            serde_json::json!({
                "path": "notes/ordinary.md",
                "content": "# Ordinary",
                "force": true,
            }),
            &ctx,
        )
        .await;
    assert!(
        !is_error(&allowed),
        "{backend}: an ordinary note must still be writable, got {allowed:?}"
    );
}

#[tokio::test]
async fn legacy_backend_refuses_protected_paths() {
    let (temp, server) = legacy_server().await;
    assert_protected(&temp, &server, "legacy").await;
}

#[tokio::test]
async fn git_backend_refuses_protected_paths() {
    let (temp, server) = git_server().await;
    assert_protected(&temp, &server, "git").await;
}

/// An existing config file under `.obsidian/` must not be readable through the
/// note API — the plugin config capability is the only sanctioned door, and it
/// is per-plugin and path-scoped.
#[tokio::test]
async fn existing_obsidian_config_is_not_readable_as_a_note() {
    let (temp, server) = legacy_server().await;
    std::fs::create_dir_all(temp.path().join(".obsidian/plugins/tasks")).expect("config dir");
    std::fs::write(
        temp.path().join(".obsidian/plugins/tasks/data.json"),
        br#"{"secret":"value"}"#,
    )
    .expect("config file");

    let ctx = RequestContext::new();
    let read = server
        .call_tool(
            "read_note",
            serde_json::json!({"path": ".obsidian/plugins/tasks/data.json"}),
            &ctx,
        )
        .await;
    assert!(is_error(&read), "config read should be refused: {read:?}");
}

/// Escaping the vault entirely stays refused — the new in-vault policy sits
/// beside the traversal check, it does not replace it.
#[tokio::test]
async fn vault_boundary_still_holds() {
    let (_temp, server) = legacy_server().await;
    let ctx = RequestContext::new();
    let read = server
        .call_tool(
            "read_note",
            serde_json::json!({"path": "../outside.md"}),
            &ctx,
        )
        .await;
    assert!(is_error(&read), "traversal should be refused: {read:?}");
}
