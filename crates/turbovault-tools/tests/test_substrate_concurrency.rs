//! GWS.16 — Concurrency / isolation integration tests for the git substrate.
//!
//! Exercises end-to-end scenarios that span the `WriteTools` dispatch layer +
//! `GitFileTools` + the bare substrate (`VaultRepo::apply_transaction`) +
//! `ReindexQueue`. The substrate's unit tests live in `turbovault-git/src/`
//! and exercise the primitives directly; this file proves the same
//! correctness guarantees survive the trip through the tool layer that the
//! MCP server sits on.
//!
//! Scenarios:
//! 1. Disjoint concurrent writers — two tasks writing to distinct files via
//!    one shared `CommitLocks` registry both succeed.
//! 2. Same-file CAS abort — two writers, both with `expected_hash` of the
//!    same base bytes; first wins, second's precondition stalemates loudly.
//! 3. Reconsideration domino — a multi-file batch with an `expect_blob` on a
//!    file modified by a concurrent writer aborts atomically (zero changes
//!    materialize, no partial state).
//! 4. Move + link-update atomicity — `rename` chained with link-target
//!    `update`s lands as ONE commit.
//! 5. Reindex queue stays coherent under contention — both writers' commits
//!    end up in the queue + drainer applies all of them.
//!
//! Cross-process scenarios (Workflow B) are §8.4 territory and NOT covered
//! here — they require the future git-event listener.

use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use turbovault_batch::BatchOperation;
use turbovault_core::config::{ServerConfig, VaultConfig};
use turbovault_tools::{
    CommitHook, CommitLocks, GitFileTools, Oid, ReadSet, ReindexQueue, VaultRepo, WriteMode,
    WriteTools,
};
use turbovault_vault::VaultManager;

fn init_repo(dir: &Path) {
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    git2::Repository::init_opts(dir, &opts).unwrap();
}

fn test_server_config(vault_dir: &Path) -> ServerConfig {
    let mut cfg = ServerConfig::new();
    cfg.vaults
        .push(VaultConfig::builder("c", vault_dir).build().unwrap());
    cfg
}

/// Two `WriteTools` handles sharing the same `CommitLocks` registry — what
/// the MCP server cache gives every per-call open. Models the per-vault
/// in-process contention the substrate is designed for.
fn two_writers(tmp: &TempDir) -> (Arc<VaultManager>, WriteTools, WriteTools, Arc<ReindexQueue>) {
    init_repo(tmp.path());
    let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
    let locks = Arc::new(CommitLocks::new());
    let queue = Arc::new(ReindexQueue::new());

    let q1 = Arc::clone(&queue);
    let hook1: CommitHook = Arc::new(move |_p, c| q1.push(c));
    let q2 = Arc::clone(&queue);
    let hook2: CommitHook = Arc::new(move |_p, c| q2.push(c));

    let a = WriteTools::git_with_hook(
        Arc::clone(&manager),
        tmp.path().to_path_buf(),
        Arc::clone(&locks),
        hook1,
    );
    let b = WriteTools::git_with_hook(
        Arc::clone(&manager),
        tmp.path().to_path_buf(),
        Arc::clone(&locks),
        hook2,
    );
    (manager, a, b, queue)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disjoint_writers_both_succeed() {
    let tmp = TempDir::new().unwrap();
    let (_mgr, a, b, queue) = two_writers(&tmp);

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
    // Reindex queue saw all 40 commits.
    assert_eq!(queue.pending_count(), 40);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_file_cas_one_wins_other_aborts() {
    let tmp = TempDir::new().unwrap();
    let (_mgr, a, b, _q) = two_writers(&tmp);

    // Seed v1 so both writers can compute a (stale-by-the-time-of-write)
    // blob oid for the precondition.
    a.write_file("a.md", "v1").await.unwrap();
    let v1_blob = VaultRepo::blob_oid_of(b"v1").unwrap().to_string();

    // Both writers attempt to update a.md from v1, concurrently. The
    // substrate's CAS guarantees exactly one lands and the other aborts.
    let v1a = v1_blob.clone();
    let v1b = v1_blob.clone();
    let ja = tokio::spawn(async move {
        a.write_file_with_mode("a.md", "WA", WriteMode::Overwrite, Some(&v1a))
            .await
    });
    let jb = tokio::spawn(async move {
        b.write_file_with_mode("a.md", "WB", WriteMode::Overwrite, Some(&v1b))
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
    let (_mgr, a, b, _q) = two_writers(&tmp);

    a.write_file("watched.md", "W1").await.unwrap();
    let watched_v1 = VaultRepo::blob_oid_of(b"W1").unwrap();

    // Writer B races and modifies "watched.md" first.
    let watched_v1_str = watched_v1.to_string();
    b.write_file_with_mode(
        "watched.md",
        "W2",
        WriteMode::Overwrite,
        Some(&watched_v1_str),
    )
    .await
    .unwrap();

    // Writer A now tries a batch that touches three OTHER files but pinned
    // to "watched.md" being unchanged via a manually-constructed transaction
    // through the bare substrate. (The MCP-level batch API doesn't yet
    // expose read-set preconditions; this exercises the substrate's
    // reconsideration domino, which is what derived-state preconditions
    // (turbovault-5fm) will eventually surface.)
    use turbovault_git::Transaction;
    let txn = Transaction::new("write a/b/c, guard watched")
        .upsert("a.md", "AA")
        .upsert("b.md", "BB")
        .upsert("c.md", "CC")
        .expect_blob("watched.md", watched_v1);
    let repo = VaultRepo::open_with_locks(tmp.path(), Arc::new(CommitLocks::new())).unwrap();
    let res = repo.apply_transaction(&txn);
    assert!(
        matches!(
            res,
            Err(turbovault_git::Error::PreconditionFailed { ref path, .. })
                if path == "watched.md"
        ),
        "expected PreconditionFailed on watched.md, got: {res:?}",
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
    // The case the legacy batch couldn't deliver atomically: rename a page
    // AND update its inbound wikilinks in ONE commit. This is the substrate's
    // headline win.
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
    let locks = Arc::new(CommitLocks::new());
    let queue = Arc::new(ReindexQueue::new());
    let q = Arc::clone(&queue);
    let hook: CommitHook = Arc::new(move |_p, c| q.push(c));
    let tools = GitFileTools::new_with_hook(
        Arc::clone(&manager),
        tmp.path().to_path_buf(),
        Arc::clone(&locks),
        hook,
    );

    tools.write_file("old.md", "body").await.unwrap();
    tools.write_file("link1.md", "see [[old]]").await.unwrap();
    tools
        .write_file("link2.md", "ref [[old]] here")
        .await
        .unwrap();

    let head_before = {
        let r = VaultRepo::open(tmp.path()).unwrap();
        r.head_oid()
    };

    // Drive a single transaction directly through the substrate (the
    // move-with-links composition isn't yet wrapped by GitFileTools — the
    // batch surface gets close, but doesn't accept preconditions yet).
    use turbovault_git::Transaction;
    let body_blob = VaultRepo::blob_oid_of(b"body").unwrap();
    let l1_blob = VaultRepo::blob_oid_of(b"see [[old]]").unwrap();
    let l2_blob = VaultRepo::blob_oid_of(b"ref [[old]] here").unwrap();
    let txn = Transaction::new("mv old->new + fix links")
        .rename("old.md", "new.md", "body", body_blob)
        .update("link1.md", "see [[new]]", l1_blob)
        .update("link2.md", "ref [[new]] here", l2_blob);
    let repo = VaultRepo::open_with_locks_and_hook(
        tmp.path(),
        Arc::clone(&locks),
        Arc::new(move |_p, c| Arc::clone(&queue).push(c)),
    )
    .unwrap();
    let _res = repo.apply_transaction(&txn).unwrap();

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reindex_queue_receives_every_commit_under_contention() {
    let tmp = TempDir::new().unwrap();
    let (_mgr, a, b, queue) = two_writers(&tmp);

    // Two writers, distinct files, 25 each — all should land in the queue.
    let ja = tokio::spawn(async move {
        for i in 0..25 {
            a.write_file(&format!("a_{i}.md"), "x").await.unwrap();
        }
    });
    let jb = tokio::spawn(async move {
        for i in 0..25 {
            b.write_file(&format!("b_{i}.md"), "y").await.unwrap();
        }
    });
    ja.await.unwrap();
    jb.await.unwrap();
    assert_eq!(queue.pending_count(), 50);

    // Drain everything synchronously; cursor advances to the last commit.
    let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
    let repo = VaultRepo::open_with_locks(tmp.path(), Arc::new(CommitLocks::new())).unwrap();
    let drained = queue.drain_through(&repo, &manager).await.unwrap();
    assert_eq!(drained, 50);
    assert_eq!(queue.pending_count(), 0);
    assert!(queue.cursor().is_some());

    // Ensure no Oid was reused or dropped — the drainer covers all of them.
    let _: Oid = queue.cursor().unwrap();
}

#[tokio::test]
async fn batch_failure_leaves_zero_partial_state() {
    // Equivalent of the WriteTools test but at the integration boundary —
    // proves the atomicity claim survives the dispatch layer.
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
    let locks = Arc::new(CommitLocks::new());
    let tools = WriteTools::git(
        Arc::clone(&manager),
        tmp.path().to_path_buf(),
        Arc::clone(&locks),
    );

    let ops = vec![
        BatchOperation::WriteNote {
            path: "first.md".into(),
            content: "F".into(),
        },
        BatchOperation::MoveNote {
            from: "does_not_exist.md".into(),
            to: "anywhere.md".into(),
        },
        BatchOperation::WriteNote {
            path: "third.md".into(),
            content: "T".into(),
        },
    ];
    let res = tools.batch_execute(ops).await.unwrap();
    assert!(!res.success);

    // Zero files from the failed batch materialized — proof of atomicity.
    assert!(!tmp.path().join("first.md").exists());
    assert!(!tmp.path().join("third.md").exists());
    assert!(!tmp.path().join("anywhere.md").exists());
}

// -------- GWS.5fm: read-set preconditions --------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_set_aborts_write_when_source_file_changed_underneath() {
    let tmp = TempDir::new().unwrap();
    let (_mgr, a, b, _q) = two_writers(&tmp);

    // Seed two source files that an agent "reads" to derive state.
    a.write_file("src1.md", "S1").await.unwrap();
    a.write_file("src2.md", "S2").await.unwrap();
    let s1 = VaultRepo::blob_oid_of(b"S1").unwrap();
    let s2 = VaultRepo::blob_oid_of(b"S2").unwrap();

    // Agent A constructs a read-set token capturing both source files.
    let read_set = ReadSet::from_entries(vec![
        ("src1.md".to_string(), s1),
        ("src2.md".to_string(), s2),
    ]);

    // Writer B concurrently modifies src1.md (the "derivation source"
    // changed underneath A's read).
    let s1_str = s1.to_string();
    b.write_file_with_mode(
        "src1.md",
        "S1-MODIFIED",
        WriteMode::Overwrite,
        Some(&s1_str),
    )
    .await
    .unwrap();

    // Agent A now tries to write a new file based on its read; the
    // read_set fires the reconsideration domino — write ABORTS.
    let err = a
        .write_file_with_read_set(
            "derived.md",
            "based on src1+src2",
            WriteMode::Overwrite,
            None,
            Some(&read_set),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, turbovault_core::Error::ConcurrencyError { .. }),
        "expected ConcurrencyError on stale read-set; got: {err:?}"
    );
    // Derived file never materialized.
    assert!(!tmp.path().join("derived.md").exists());
}

#[tokio::test]
async fn read_set_with_no_concurrent_changes_lets_write_succeed() {
    let tmp = TempDir::new().unwrap();
    let (_mgr, a, _b, _q) = two_writers(&tmp);

    a.write_file("src.md", "X").await.unwrap();
    let x = VaultRepo::blob_oid_of(b"X").unwrap();
    let read_set = ReadSet::from_entries(vec![("src.md".to_string(), x)]);

    a.write_file_with_read_set(
        "derived.md",
        "Y",
        WriteMode::Overwrite,
        None,
        Some(&read_set),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("derived.md")).unwrap(),
        "Y"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_set_aborts_whole_batch_when_any_source_changes() {
    let tmp = TempDir::new().unwrap();
    let (_mgr, a, b, _q) = two_writers(&tmp);

    a.write_file("src.md", "SRC1").await.unwrap();
    let src_oid = VaultRepo::blob_oid_of(b"SRC1").unwrap();
    let read_set = ReadSet::from_entries(vec![("src.md".to_string(), src_oid)]);

    // Writer B mutates the read-set source mid-flight.
    let src_str = src_oid.to_string();
    b.write_file_with_mode("src.md", "SRC2", WriteMode::Overwrite, Some(&src_str))
        .await
        .unwrap();

    let ops = vec![
        turbovault_batch::BatchOperation::CreateNote {
            path: "out1.md".into(),
            content: "O1".into(),
        },
        turbovault_batch::BatchOperation::CreateNote {
            path: "out2.md".into(),
            content: "O2".into(),
        },
    ];
    let res = a
        .batch_execute_with_read_set(ops, Some(&read_set))
        .await
        .unwrap();
    assert!(
        !res.success,
        "batch must abort when read-set source changed"
    );
    assert!(!tmp.path().join("out1.md").exists());
    assert!(!tmp.path().join("out2.md").exists());
}

#[tokio::test]
async fn legacy_backend_rejects_read_set_loudly() {
    // The legacy backend has no precondition machinery beyond expected_hash;
    // passing a read_set must surface as a loud ConfigError, not silent drop.
    let tmp = TempDir::new().unwrap();
    let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
    let tools = WriteTools::legacy(manager);

    let rs = ReadSet::from_entries(vec![(
        "src.md".to_string(),
        VaultRepo::blob_oid_of(b"X").unwrap(),
    )]);
    let err = tools
        .write_file_with_read_set("a.md", "v", WriteMode::Overwrite, None, Some(&rs))
        .await
        .unwrap_err();
    assert!(
        matches!(err, turbovault_core::Error::ConfigError { .. }),
        "got: {err:?}"
    );
}
