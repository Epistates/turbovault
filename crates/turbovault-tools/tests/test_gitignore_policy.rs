//! turbovault-lri — `.gitignore` policy enforcement when
//! `VaultGitConfig::include_ignored = false`.
//!
//! write-substrate-layering M4e: the policy now lives on
//! `turbovault_vault::GitSubstrate` (`run_plan`'s gitignore gate,
//! substrate.rs) rather than the deleted tool-layer `GitFileTools`. Drives
//! it the same way production code does: `FileTools::create_file` over a
//! `VaultManager` configured with `write_backend: git`.

use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use turbovault_core::config::{ServerConfig, VaultConfig, VaultGitConfig, WriteBackend};
use turbovault_tools::FileTools;
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
    let commit = repo
        .commit(Some("HEAD"), &sig, &sig, "seed", &tree, &[])
        .unwrap();

    // A commit built from an explicitly supplied tree does not guarantee that
    // libgit2 refreshes the on-disk index. Start each policy test from the same
    // clean HEAD/index/worktree invariant required by production writes.
    let mut index = repo.index().unwrap();
    index.read_tree(&tree).unwrap();
    index.write().unwrap();
    commit
}

fn make_manager(tmp: &TempDir, include_ignored: bool) -> Arc<VaultManager> {
    let mut cfg = ServerConfig::new();
    cfg.vaults.push(
        VaultConfig::builder("lri", tmp.path())
            .write_backend(WriteBackend::Git)
            .git(VaultGitConfig {
                include_ignored,
                ..Default::default()
            })
            .build()
            .unwrap(),
    );
    Arc::new(VaultManager::new(cfg).unwrap())
}

#[tokio::test]
async fn include_ignored_true_writes_gitignored_path() {
    let tmp = TempDir::new().unwrap();
    init_repo_with_gitignore(tmp.path(), "secrets/\n");
    let manager = make_manager(&tmp, true);

    // Default include_ignored=true: ignored paths commit normally.
    let tools = FileTools::new(manager);
    let result = tools
        .create_file("secrets/api.md", "TOKEN=xxx\n", "create ignored path")
        .await;
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
    let manager = make_manager(&tmp, false);

    let tools = FileTools::new(manager);
    let err = tools
        .create_file("secrets/api.md", "TOKEN=xxx\n", "create ignored path")
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
    let manager = make_manager(&tmp, false);

    let tools = FileTools::new(manager);
    let result = tools
        .create_file("notes/foo.md", "# Foo\n", "create non-ignored path")
        .await;
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
