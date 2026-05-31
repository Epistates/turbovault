//! turbovault-lri — `.gitignore` policy enforcement when
//! `VaultGitConfig::include_ignored = false`. Drives the same
//! `GitFileTools::write_file` chokepoint the MCP layer hits.

use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use turbovault_core::config::{ServerConfig, VaultConfig};
use turbovault_tools::{CommitLocks, GitFileTools};
use turbovault_vault::VaultManager;

fn init_repo_with_gitignore(dir: &Path, ignore_lines: &str) -> git2::Oid {
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = git2::Repository::init_opts(dir, &opts).unwrap();
    // Write .gitignore directly to the worktree so libgit2's matcher
    // picks it up. NOT committed yet — the matcher reads the worktree
    // copy.
    std::fs::write(dir.join(".gitignore"), ignore_lines).unwrap();
    // Seed commit so HEAD is born.
    let sig = git2::Signature::now("seed", "seed@example").unwrap();
    let tree_oid = {
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new(".gitignore")).unwrap();
        idx.write_tree().unwrap()
    };
    let tree = repo.find_tree(tree_oid).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "seed", &tree, &[])
        .unwrap()
}

fn make_tools(tmp: &TempDir) -> (Arc<VaultManager>, Arc<CommitLocks>) {
    let mut cfg = ServerConfig::new();
    cfg.vaults
        .push(VaultConfig::builder("lri", tmp.path()).build().unwrap());
    let manager = Arc::new(VaultManager::new(cfg).unwrap());
    let locks = Arc::new(CommitLocks::new());
    (manager, locks)
}

#[tokio::test]
async fn include_ignored_true_writes_gitignored_path() {
    let tmp = TempDir::new().unwrap();
    init_repo_with_gitignore(tmp.path(), "secrets/\n");
    let (manager, locks) = make_tools(&tmp);

    // Default include_ignored=true: ignored paths commit normally.
    let tools = GitFileTools::new(manager, tmp.path().to_path_buf(), locks);
    let result = tools.create_file("secrets/api.md", "TOKEN=xxx\n").await;
    assert!(
        result.is_ok(),
        "include_ignored=true should write gitignored path, got: {:?}",
        result.err()
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("secrets/api.md")).unwrap(),
        "TOKEN=xxx\n"
    );
}

#[tokio::test]
async fn include_ignored_false_refuses_gitignored_path() {
    let tmp = TempDir::new().unwrap();
    init_repo_with_gitignore(tmp.path(), "secrets/\n");
    let (manager, locks) = make_tools(&tmp);

    let tools =
        GitFileTools::new(manager, tmp.path().to_path_buf(), locks).with_include_ignored(false);
    let err = tools
        .create_file("secrets/api.md", "TOKEN=xxx\n")
        .await
        .unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("secrets/api.md") && msg.contains("gitignored"),
        "expected refuse-message naming the ignored path + 'gitignored', got: {}",
        msg
    );
    // And no file was committed / written.
    assert!(
        !tmp.path().join("secrets/api.md").exists(),
        "refused write should not have materialized"
    );
}

#[tokio::test]
async fn include_ignored_false_allows_non_ignored_path() {
    let tmp = TempDir::new().unwrap();
    init_repo_with_gitignore(tmp.path(), "secrets/\n");
    let (manager, locks) = make_tools(&tmp);

    let tools =
        GitFileTools::new(manager, tmp.path().to_path_buf(), locks).with_include_ignored(false);
    let result = tools.create_file("notes/foo.md", "# Foo\n").await;
    assert!(
        result.is_ok(),
        "include_ignored=false should still allow non-ignored paths, got: {:?}",
        result.err()
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("notes/foo.md")).unwrap(),
        "# Foo\n"
    );
}
