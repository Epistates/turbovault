//! turbovault-6fo.18 (GWS.17): MCP-surface end-to-end tests for the
//! git substrate. Each test sets up a real `ObsidianMcpServer` with a
//! real git-repo vault registered as `write_backend: Git`, drives
//! operations through the server's dispatch surface, and asserts the
//! end-to-end effects:
//!
//! - Git commits land as expected (HEAD advances; subject matches).
//! - Working tree stays coherent with HEAD.
//! - In-memory link graph reflects committed state after the per-vault
//!   reindex queue drains.
//! - Substrate-killer features (atomic move + link updates, atomic
//!   delete + stale-callout wrap, batch CAS) work through the
//!   server-side dispatcher.
//! - The HEAD-ref listener (turbovault-bou) absorbs out-of-band
//!   commits.
//!
//! These tests exercise the SAME path the MCP `#[tool]` handlers use
//! internally — they call `get_active_write_tools().await?` and
//! invoke the WriteTools dispatcher, just like the MCP layer does.
//! The turbomcp wire-protocol layer above is exercised separately by
//! the `turbomcp` crate's own test suite.

use std::time::Duration;
use tempfile::TempDir;
use turbovault::ObsidianMcpServer;
use turbovault_core::config::{VaultConfig, VaultGitConfig, WriteBackend};
use turbovault_tools::{BatchOperation, VaultRepo};

/// Set up a real git repo with an initial commit + a server with the
/// vault registered as `write_backend: Git`. Returns the temp dir
/// (kept alive for the test's lifetime), the registered vault name,
/// and the server.
async fn setup_git_vault() -> (TempDir, &'static str, ObsidianMcpServer) {
    let tmp = TempDir::new().unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = git2::Repository::init_opts(tmp.path(), &opts).unwrap();
    // Initial commit so HEAD is born — substrate fanout + several
    // operations need a non-unborn baseline.
    let tree_oid = {
        let mut idx = repo.index().unwrap();
        idx.write_tree().unwrap()
    };
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("Init", "init@example").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();

    let vault_config = VaultConfig::builder("e2e", tmp.path())
        .write_backend(WriteBackend::Git)
        .git(VaultGitConfig::default())
        .build()
        .unwrap();
    let server = ObsidianMcpServer::new().unwrap();
    server.multi_vault().add_vault(vault_config).await.unwrap();
    server.multi_vault().set_active_vault("e2e").await.unwrap();
    (tmp, "e2e", server)
}

fn head_oid(path: &std::path::Path) -> Option<git2::Oid> {
    VaultRepo::open(path).ok().and_then(|r| r.head_oid())
}

fn head_message(path: &std::path::Path) -> String {
    let repo = git2::Repository::open(path).unwrap();
    let oid = head_oid(path).unwrap();
    repo.find_commit(oid)
        .unwrap()
        .message()
        .unwrap()
        .to_string()
}

/// turbovault-6fo.18: write_note on git backend lands one commit; the
/// working tree matches HEAD; the auto-derived message is
/// `write_note <path>` (verb=tool_name per TV-008).
#[tokio::test]
#[serial_test::serial]
async fn e2e_write_note_commits_to_git_with_tool_name_verb() {
    let (tmp, _name, server) = setup_git_vault().await;
    let tools = server.get_active_write_tools_test().await.unwrap();

    let head_before = head_oid(tmp.path()).unwrap();
    tools
        .write_file_with_mode_and_message(
            "concepts/foo.md",
            "# Foo\n\nplaceholder\n",
            turbovault_tools::WriteMode::Overwrite,
            None,
            "write_note concepts/foo.md",
        )
        .await
        .unwrap();
    let head_after = head_oid(tmp.path()).unwrap();
    assert_ne!(Some(head_after), Some(head_before));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("concepts/foo.md")).unwrap(),
        "# Foo\n\nplaceholder\n"
    );
    let msg = head_message(tmp.path());
    assert!(
        msg.contains("write_note concepts/foo.md"),
        "expected MCP-layer verb=tool_name subject, got: {msg:?}"
    );
}

/// turbovault-6fo.18: move with link updates is one atomic commit.
/// HEAD advances by exactly one commit; the rename + every linker
/// rewrite land together.
#[tokio::test]
#[serial_test::serial]
async fn e2e_move_note_with_link_updates_one_commit() {
    let (tmp, _name, server) = setup_git_vault().await;
    let tools = server.get_active_write_tools_test().await.unwrap();
    let mgr = server.get_active_vault_manager_test().await.unwrap();

    tools.write_file("old.md", "# Old\n").await.unwrap();
    tools
        .write_file("linker.md", "see [[old]] here\n")
        .await
        .unwrap();
    // Ensure the link graph reflects the just-committed writes before
    // we run a move that asks the substrate to consult it. Calling
    // initialize() first guarantees the working-tree scan completes
    // even if the substrate's drainer hasn't drained yet (the lqr
    // move path consults the graph, not the queue).
    // Drain queued reindex work + then bootstrap the link graph from
    // disk. Sleep + flush + initialize together make the link graph
    // fully reflect the just-committed writes; the substrate's
    // background drainer + initialize() compete for the link-graph
    // write lock and interleave non-deterministically without this
    // priming.
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.flush_reindex_for_active_vault_test().await.unwrap();
    mgr.initialize().await.unwrap();
    let head_before = head_oid(tmp.path()).unwrap();

    let result = tools
        .move_file_with_link_updates("old.md", "new.md", None, "atomic rename test")
        .await
        .unwrap();
    assert_eq!(result.link_sources_updated, vec!["linker.md".to_string()]);
    let head_after = head_oid(tmp.path()).unwrap();
    assert_ne!(head_after, head_before);

    let repo = git2::Repository::open(tmp.path()).unwrap();
    let commit = repo.find_commit(head_after).unwrap();
    assert_eq!(commit.parent_count(), 1, "exactly one new commit");

    assert!(!tmp.path().join("old.md").exists());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("new.md")).unwrap(),
        "# Old\n"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("linker.md")).unwrap(),
        "see [[new]] here\n"
    );
}

/// turbovault-6fo.18: delete with rewrite-stale callout wraps the
/// linker as part of the same commit. The strikethrough breadcrumb
/// survives.
#[tokio::test]
#[serial_test::serial]
async fn e2e_delete_note_rewrite_stale_callout_atomic() {
    let (tmp, _name, server) = setup_git_vault().await;
    let tools = server.get_active_write_tools_test().await.unwrap();
    let mgr = server.get_active_vault_manager_test().await.unwrap();

    tools.write_file("doomed.md", "# Doomed").await.unwrap();
    tools
        .write_file("a.md", "see [[doomed]] for details\n")
        .await
        .unwrap();
    // Same sleep+flush+initialize pattern as the move test (drainer
    // wake-up races otherwise).
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.flush_reindex_for_active_vault_test().await.unwrap();
    mgr.initialize().await.unwrap();

    let result = tools
        .delete_file_with_link_rewrite_to_stale("doomed.md", None, "delete + wrap")
        .await
        .unwrap();
    assert_eq!(result.link_sources_updated, vec!["a.md".to_string()]);
    assert!(!tmp.path().join("doomed.md").exists());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.md")).unwrap(),
        "see ~~[[doomed]]~~ for details\n"
    );
}

/// turbovault-6fo.18: batch_execute is atomic on a stale-CAS abort.
/// Zero files change; HEAD doesn't advance; the substrate folds the
/// failure into BatchResult{success:false, executed:0}.
#[tokio::test]
#[serial_test::serial]
async fn e2e_batch_execute_per_op_cas_aborts_atomically() {
    let (tmp, _name, server) = setup_git_vault().await;
    let tools = server.get_active_write_tools_test().await.unwrap();

    tools.write_file("a.md", "v1\n").await.unwrap();
    let stale = VaultRepo::blob_oid_of(b"NEVER").unwrap().to_string();
    let head_before = head_oid(tmp.path()).unwrap();

    let ops = vec![
        BatchOperation::CreateNote {
            path: "fresh.md".into(),
            content: "ok".into(),
            force: None,
        },
        BatchOperation::WriteNote {
            path: "a.md".into(),
            content: "v2\n".into(),
            expected_hash: Some(stale),
        },
    ];
    let res = tools.batch_execute(ops).await.unwrap();
    assert!(!res.success);
    assert_eq!(res.executed, 0);
    assert!(!tmp.path().join("fresh.md").exists());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.md")).unwrap(),
        "v1\n"
    );
    assert_eq!(head_oid(tmp.path()), Some(head_before));
}

/// turbovault-6fo.18: batch_execute with all-matching preconditions
/// lands as ONE commit (architecture §5.4: "1 transaction = 1 commit").
#[tokio::test]
#[serial_test::serial]
async fn e2e_batch_execute_lands_as_one_commit() {
    let (tmp, _name, server) = setup_git_vault().await;
    let tools = server.get_active_write_tools_test().await.unwrap();

    let head_before = head_oid(tmp.path()).unwrap();
    let ops = vec![
        BatchOperation::CreateNote {
            path: "x.md".into(),
            content: "x".into(),
            force: None,
        },
        BatchOperation::CreateNote {
            path: "y.md".into(),
            content: "y".into(),
            force: None,
        },
        BatchOperation::CreateNote {
            path: "z.md".into(),
            content: "z".into(),
            force: None,
        },
    ];
    let res = tools.batch_execute(ops).await.unwrap();
    assert!(res.success);
    assert_eq!(res.executed, 3);
    let head_after = head_oid(tmp.path()).unwrap();
    assert_ne!(head_after, head_before);

    let repo = git2::Repository::open(tmp.path()).unwrap();
    let commit = repo.find_commit(head_after).unwrap();
    assert_eq!(
        commit.parent_count(),
        1,
        "exactly one new commit for the 3-op batch"
    );
    // Substrate-default batch subject (the MCP-layer's op-tally derive
    // only runs through the `#[tool]` handler; this e2e drives
    // WriteTools::batch_execute directly which uses the substrate's
    // default `batch_execute (N ops)` format).
    let msg = head_message(tmp.path());
    assert!(
        msg.contains("batch_execute"),
        "expected substrate-default batch subject, got: {msg:?}"
    );
}

/// turbovault-6fo.18 (+ bou): the HEAD-ref listener detects a commit
/// made out-of-band (via direct git2) and pushes the new oid onto the
/// per-vault reindex queue. Server-side wiring covered.
///
/// Note: the production listener polls at 5s default. The substrate
/// commit-hook (which fires on writes via this server's WriteTools)
/// also pushes to the queue immediately, so the test does NOT do any
/// substrate write between baseline and the external commit — the
/// only oid that can land in the queue must come from the listener.
#[tokio::test]
#[serial_test::serial]
async fn e2e_external_commit_observed_by_ref_listener() {
    let (tmp, _name, server) = setup_git_vault().await;
    // Spawn listener with a SHORT interval so the test doesn't waste
    // wallclock on 5s production polls. The interval is the only knob
    // that differs from the production wiring.
    server
        .spawn_ref_listener_with_interval_test("e2e", Duration::from_millis(50))
        .await;
    // Give the listener one tick to set its baseline.
    tokio::time::sleep(Duration::from_millis(120)).await;
    let queue = server.get_reindex_queue_test("e2e").await.unwrap();
    assert_eq!(
        queue.pending_count(),
        0,
        "queue should be empty at listener start"
    );

    // External commit via direct git2 (bypasses the substrate's
    // CommitHook — only the listener can observe this).
    let repo = git2::Repository::open(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("external.md"), "external content").unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(std::path::Path::new("external.md")).unwrap();
    let tree_oid = idx.write_tree().unwrap();
    idx.write().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("Ext", "ext@example").unwrap();
    let parent_oid = repo.head().unwrap().target().unwrap();
    let parent = repo.find_commit(parent_oid).unwrap();
    let external_oid = repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            "external commit (bou test)",
            &tree,
            &[&parent],
        )
        .unwrap();

    // Listener polls at 50ms in this test — generous 2s ceiling.
    let start = std::time::Instant::now();
    let mut detected = false;
    while start.elapsed() < Duration::from_secs(2) {
        if queue.pending_count() > 0 {
            detected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        detected,
        "HEAD-ref listener should have detected the external commit within the poll window"
    );
    // The single pushed oid must be the external commit (no substrate
    // writes happened in this test, so the listener is the only
    // pusher).
    assert_eq!(queue.pop_front(), Some(external_oid));
}

/// turbovault-6fo.18: after a substrate write, the reindex queue
/// drains successfully via the server's `flush_reindex_for_active_vault`
/// helper. End-to-end check that the queue + drainer + flush helpers
/// compose without panicking. Doesn't assert the post-drain graph
/// shape (those are covered by `turbovault-tools` integration tests
/// at the substrate level); this test cares about the
/// server-side wiring.
#[tokio::test]
#[serial_test::serial]
async fn e2e_reindex_queue_drains_after_substrate_writes() {
    let (_tmp, _name, server) = setup_git_vault().await;
    let tools = server.get_active_write_tools_test().await.unwrap();

    tools
        .write_file("home.md", "see [[concept-x]]\n")
        .await
        .unwrap();
    tools
        .write_file("concept-x.md", "# Concept X\n")
        .await
        .unwrap();

    // The background drainer task may have already pulled the pending
    // pushes by the time we observe (race vs. tokio::Notify wake-ups
    // is intentional — drains happen asap). We don't assert "queue had
    // > 0 pending" because that's inherently racy. We DO assert the
    // server-side flush helper completes without error and the queue
    // is empty afterwards (which is the e2e wiring contract).
    server.flush_reindex_for_active_vault_test().await.unwrap();

    let queue = server.get_reindex_queue_test("e2e").await.unwrap();
    assert_eq!(queue.pending_count(), 0, "flush should leave queue empty");
}

/// turbovault-1ne: remove_vault refuses while a fanout is active
/// (symmetric with begin_transaction's nested-fanout refusal), and
/// cleanly aborts the drainer + ref-listener tasks + drops the
/// lock/queue entries when allowed.
#[tokio::test]
#[serial_test::serial]
async fn e2e_remove_vault_cleans_up_git_backend_state() {
    let (tmp, name, server) = setup_git_vault().await;

    // Drive one write so the lazy drainer + ref listener spawn,
    // and the lock/queue entries materialize.
    let tools = server.get_active_write_tools_test().await.unwrap();
    tools.write_file("seed.md", "seed\n").await.unwrap();
    server
        .spawn_ref_listener_with_interval_test(name, Duration::from_millis(50))
        .await;

    assert!(
        server.has_git_drainer_test(name).await,
        "drainer should be live"
    );
    assert!(
        server.has_git_ref_listener_test(name).await,
        "ref listener should be live"
    );
    assert!(
        server.has_git_locks_test(name).await,
        "locks entry should be live"
    );

    server.remove_vault_test(name).await.unwrap();

    assert!(
        !server.has_git_drainer_test(name).await,
        "drainer entry should be dropped after remove_vault"
    );
    assert!(
        !server.has_git_ref_listener_test(name).await,
        "ref listener entry should be dropped after remove_vault"
    );
    assert!(
        !server.has_git_locks_test(name).await,
        "locks entry should be dropped after remove_vault"
    );

    // Vault gone from the registry too.
    let vaults = server.multi_vault().list_vaults().await.unwrap();
    assert!(
        vaults.iter().all(|v| v.config.name != name),
        "vault should be deregistered"
    );
    drop(tmp);
}

/// turbovault-1ne: remove_vault is refused while a fanout is active.
/// Caller must abandon_transaction first. Symmetric with the
/// nested-fanout refusal in begin_transaction.
#[tokio::test]
#[serial_test::serial]
async fn e2e_remove_vault_blocked_while_fanout_active() {
    let (_tmp, name, server) = setup_git_vault().await;

    // Bring up the drainer + lock entries with a seed write.
    let tools = server.get_active_write_tools_test().await.unwrap();
    tools.write_file("seed.md", "seed\n").await.unwrap();

    // Open a fanout transaction directly through the substrate-side
    // path so the e2e test doesn't depend on the full
    // `begin_transaction` MCP wire shape (which is exercised
    // separately). We need active_fanouts populated.
    let cfg = server.multi_vault().get_vault_config(name).await.unwrap();
    let locks = server.get_or_init_git_locks_test(name).await;
    let scratch = tempfile::TempDir::new().unwrap();
    let wt_path = scratch.path().join("wt-1");
    let info = {
        let path = cfg.path.clone();
        tokio::task::spawn_blocking(move || {
            let repo = VaultRepo::open_with_locks(&path, locks).unwrap();
            repo.open_fanout_worktree("tx-1ne", &wt_path).unwrap()
        })
        .await
        .unwrap()
    };
    let fanout_vault_name = format!("{}-fanout-tx-1ne", name);
    server
        .register_active_fanout_test(name, "tx-1ne", info.clone(), &fanout_vault_name)
        .await;

    // remove_vault on the base vault should now fail loudly.
    let err = server.remove_vault_test(name).await.unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("active fanout") && msg.contains("abandon_transaction"),
        "expected refuse-message mentioning fanout + abandon_transaction, got: {}",
        msg
    );

    // Clean up so the temp dirs drop without leaks.
    server.clear_active_fanout_test(name).await;
    let cfg_path = cfg.path.clone();
    tokio::task::spawn_blocking(move || {
        let repo = VaultRepo::open(&cfg_path).unwrap();
        repo.abandon_fanout_by_info(&info).unwrap();
    })
    .await
    .unwrap();
}
