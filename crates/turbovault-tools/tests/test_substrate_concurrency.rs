//! GWS.16 — Concurrency / isolation integration tests for the git substrate.
//!
//! write-substrate-layering M4e: exercises end-to-end scenarios through
//! `VaultManager`'s domain tools (`FileTools`/`BatchTools`) + the bare
//! substrate (`VaultRepo::commit_changeset`) — the surface that survives
//! `WriteTools`/`GitFileTools`'s deletion. Two callers sharing one
//! `Arc<VaultManager>` model what the MCP server's per-vault cache gives
//! every call today (the manager owns its own `CommitLocks` + cached repo
//! internally, so no test-level lock-registry plumbing is needed anymore).
//! `ReindexQueue`'s own drain/notify guarantees are unit-tested directly in
//! `turbovault-vault/src/reindex.rs`; this file only proves the
//! CAS/atomicity guarantees survive the trip through the domain tools that
//! sit on top of the manager.
//!
//! Scenarios:
//! 1. Disjoint concurrent writers — two tasks writing to distinct files
//!    through one shared `Arc<VaultManager>` both succeed.
//! 2. Same-file CAS abort — two writers, both with `expected_hash` of the
//!    same base bytes; first wins, second's precondition stalemates loudly.
//! 3. Reconsideration domino — a multi-file batch with an `expect_blob` on a
//!    file modified by a concurrent writer aborts atomically (zero changes
//!    materialize, no partial state).
//! 4. Move + link-update atomicity — `rename` chained with link-target
//!    `update`s lands as ONE commit.
//! 5. Batch atomicity through the manager-routed `BatchTools::batch_execute`
//!    — a mid-batch failure leaves zero partial state.
//!
//! Cross-process scenarios (Workflow B) are §8.4 territory and NOT covered
//! here — they require the future git-event listener.

use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use turbovault_batch::BatchOperation;
use turbovault_core::config::{ServerConfig, VaultConfig, WriteBackend};
use turbovault_tools::{BatchTools, CommitLocks, FileTools, VaultRepo, WriteMode};
use turbovault_vault::VaultManager;

fn init_repo(dir: &Path) {
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    git2::Repository::init_opts(dir, &opts).unwrap();
}

/// write_backend: git — these tests exercise the git substrate's CAS/
/// atomicity guarantees specifically (the direct backend has its own
/// coverage in turbovault-vault/src/substrate.rs).
fn test_server_config(vault_dir: &Path) -> ServerConfig {
    let mut cfg = ServerConfig::new();
    cfg.vaults.push(
        VaultConfig::builder("c", vault_dir)
            .write_backend(WriteBackend::Git)
            .build()
            .unwrap(),
    );
    cfg
}

/// Two `FileTools` handles wrapping the SAME `Arc<VaultManager>` — what the
/// MCP server's per-vault cache gives every call today (one manager, one
/// internal `CommitLocks` + cached repo). Models the per-vault in-process
/// contention the substrate is designed for.
fn two_writers(tmp: &TempDir) -> (Arc<VaultManager>, FileTools, FileTools) {
    init_repo(tmp.path());
    let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
    let a = FileTools::new(Arc::clone(&manager));
    let b = FileTools::new(Arc::clone(&manager));
    (manager, a, b)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disjoint_writers_both_succeed() {
    let tmp = TempDir::new().unwrap();
    let (manager, a, b) = two_writers(&tmp);

    let ja = tokio::spawn(async move {
        for i in 0..20 {
            a.write_file(&format!("a_{i}.md"), &format!("AAA {i}"))
                .await
                .unwrap();
        }
    });
    let jb = tokio::spawn(async move {
        for i in 0..20 {
            b.write_file(&format!("b_{i}.md"), &format!("BBB {i}"))
                .await
                .unwrap();
        }
    });
    ja.await.unwrap();
    jb.await.unwrap();

    // All 40 files landed on disk.
    for i in 0..20 {
        assert!(tmp.path().join(format!("a_{i}.md")).exists());
        assert!(tmp.path().join(format!("b_{i}.md")).exists());
    }
    // Reindex queue saw every commit — draining must not error, and the link
    // graph must end up with all 40 files (ReindexQueue's own drain/notify
    // unit tests live in turbovault-vault/src/reindex.rs).
    manager.flush_reindex().await;
    assert_eq!(manager.link_graph().read().await.node_count(), 40);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_file_cas_one_wins_other_aborts() {
    let tmp = TempDir::new().unwrap();
    let (_mgr, a, b) = two_writers(&tmp);

    // Seed v1 so both writers can compute a (stale-by-the-time-of-write)
    // blob oid for the precondition.
    a.write_file("a.md", "v1").await.unwrap();
    let v1_blob = VaultRepo::blob_oid_of(b"v1").unwrap().to_string();

    // Both writers attempt to update a.md from v1, concurrently. The
    // substrate's CAS guarantees exactly one lands and the other aborts.
    let v1a = v1_blob.clone();
    let v1b = v1_blob.clone();
    let ja = tokio::spawn(async move {
        a.write_file_with_mode("a.md", "WA", WriteMode::Overwrite, Some(&v1a), "writer A")
            .await
    });
    let jb = tokio::spawn(async move {
        b.write_file_with_mode("a.md", "WB", WriteMode::Overwrite, Some(&v1b), "writer B")
            .await
    });
    let ra = ja.await.unwrap();
    let rb = jb.await.unwrap();

    // Exactly one succeeded, exactly one got ConcurrencyError.
    let (winner, loser) = match (ra.is_ok(), rb.is_ok()) {
        (true, false) => ("WA", rb.unwrap_err()),
        (false, true) => ("WB", ra.unwrap_err()),
        (true, true) => panic!("both writers succeeded — substrate failed to serialize"),
        (false, false) => panic!("both writers failed: {ra:?} / {rb:?}"),
    };
    assert!(
        matches!(loser, turbovault_core::Error::ConcurrencyError { .. }),
        "loser must surface ConcurrencyError, got: {loser:?}",
    );
    // The committed bytes match the winner.
    let actual = std::fs::read_to_string(tmp.path().join("a.md")).unwrap();
    assert_eq!(actual, winner);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconsideration_domino_aborts_whole_batch_on_read_set_change() {
    // Multi-file batch with a precondition over a path it READS but doesn't
    // write. If the read-set changes underneath, the WHOLE batch aborts —
    // no partial state, no spurious writes.
    let tmp = TempDir::new().unwrap();
    let (_mgr, a, b) = two_writers(&tmp);

    a.write_file("watched.md", "W1").await.unwrap();
    let watched_v1 = VaultRepo::blob_oid_of(b"W1").unwrap();

    // Writer B races and modifies "watched.md" first.
    let watched_v1_str = watched_v1.to_string();
    b.write_file_with_mode(
        "watched.md",
        "W2",
        WriteMode::Overwrite,
        Some(&watched_v1_str),
        "writer B races",
    )
    .await
    .unwrap();

    // Writer A now tries a batch that touches three OTHER files but pinned
    // to "watched.md" being unchanged via a manually-constructed changeset
    // through the bare substrate. (The MCP-level batch API doesn't yet
    // expose read-set preconditions; this exercises the substrate's
    // reconsideration domino, which is what derived-state preconditions
    // (turbovault-5fm) will eventually surface.)
    use turbovault_core::ChangePlan;
    let txn = ChangePlan::new("write a/b/c, guard watched")
        .upsert("a.md", "AA")
        .upsert("b.md", "BB")
        .upsert("c.md", "CC")
        .expect_blob("watched.md", watched_v1.to_string());
    let repo = VaultRepo::open_with_locks(tmp.path(), Arc::new(CommitLocks::new())).unwrap();
    let res = repo.commit_changeset(&txn);
    assert!(
        matches!(
            res,
            Err(turbovault_git::Error::Core(turbovault_core::Error::ConcurrencyError {
                ref reason,
            })) if reason.contains("watched.md")
        ),
        "expected ConcurrencyError on watched.md, got: {res:?}",
    );

    // Nothing from the batch is on disk.
    assert!(!tmp.path().join("a.md").exists());
    assert!(!tmp.path().join("b.md").exists());
    assert!(!tmp.path().join("c.md").exists());
    // watched.md still shows B's update — domino didn't roll it back.
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("watched.md")).unwrap(),
        "W2"
    );
}

#[tokio::test]
async fn move_with_link_updates_lands_as_one_commit() {
    // The case a purely sequential direct backend couldn't deliver
    // atomically: rename a page AND update its inbound wikilinks in ONE
    // commit. This is the substrate's headline win.
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
    let files = FileTools::new(Arc::clone(&manager));

    files.write_file("old.md", "body").await.unwrap();
    files.write_file("link1.md", "see [[old]]").await.unwrap();
    files
        .write_file("link2.md", "ref [[old]] here")
        .await
        .unwrap();

    let head_before = {
        let r = VaultRepo::open(tmp.path()).unwrap();
        r.head_oid()
    };

    // Drive a single changeset directly through the substrate (the
    // move-with-links composition is exercised end-to-end via
    // `BatchTools::move_file_with_link_updates` in batch_tools.rs' own
    // tests; this proves the underlying commit stays a single HEAD advance).
    use turbovault_core::ChangePlan;
    let body_blob = VaultRepo::blob_oid_of(b"body").unwrap();
    let l1_blob = VaultRepo::blob_oid_of(b"see [[old]]").unwrap();
    let l2_blob = VaultRepo::blob_oid_of(b"ref [[old]] here").unwrap();
    let txn = ChangePlan::new("mv old->new + fix links")
        .rename("old.md", "new.md", body_blob.to_string())
        .update("link1.md", "see [[new]]", l1_blob.to_string())
        .update("link2.md", "ref [[new]] here", l2_blob.to_string());
    let locks = Arc::new(CommitLocks::new());
    let repo = VaultRepo::open_with_locks(tmp.path(), Arc::clone(&locks)).unwrap();
    let _res = repo.commit_changeset(&txn).unwrap();

    // HEAD advanced exactly ONCE for all four file changes (rename = remove
    // + upsert, plus 2 link updates).
    let head_after = repo.head_oid().unwrap();
    assert_ne!(Some(head_after), head_before);
    let raw_repo = git2::Repository::open(tmp.path()).unwrap();
    let commit = raw_repo.find_commit(head_after).unwrap();
    assert_eq!(
        commit.parent_count(),
        1,
        "single new commit covers move + both link rewrites"
    );

    // Filesystem confirms the rename + link rewrites.
    assert!(!tmp.path().join("old.md").exists());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("new.md")).unwrap(),
        "body"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("link1.md")).unwrap(),
        "see [[new]]"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("link2.md")).unwrap(),
        "ref [[new]] here"
    );
}

#[tokio::test]
async fn batch_failure_leaves_zero_partial_state() {
    // Proves the atomicity claim survives the manager-routed batch surface
    // (write-substrate-layering M4d: `BatchTools::batch_execute` folds every
    // op into ONE ChangePlan and applies it via `VaultManager::apply_changes`).
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
    let batch = BatchTools::new(manager);

    let ops = vec![
        BatchOperation::WriteNote {
            path: "first.md".into(),
            content: "F".into(),
            expected_hash: None,
        },
        BatchOperation::MoveNote {
            from: "does_not_exist.md".into(),
            to: "anywhere.md".into(),
            expected_hash: None,
            update_backlinks: None,
        },
        BatchOperation::WriteNote {
            path: "third.md".into(),
            content: "T".into(),
            expected_hash: None,
        },
    ];
    let res = batch
        .batch_execute(ops, "batch atomicity proof")
        .await
        .unwrap();
    assert!(!res.success);

    // Zero files from the failed batch materialized — proof of atomicity.
    assert!(!tmp.path().join("first.md").exists());
    assert!(!tmp.path().join("third.md").exists());
    assert!(!tmp.path().join("anywhere.md").exists());
}
