//! turbovault-6fo.18 (GWS.17): MCP-surface end-to-end tests for the
//! git substrate. Each test sets up a real `ObsidianMcpServer` with a
//! real git-repo vault registered as `write_backend: Git`, drives
//! operations through the server's `call_tool` dispatch (the SAME path
//! turbomcp routes a wire request to, minus the JSON-RPC/stdio framing),
//! and asserts the end-to-end effects:
//!
//! - Git commits land as expected (HEAD advances; subject matches).
//! - Working tree stays coherent with HEAD.
//! - Substrate-killer features (atomic move + link updates, atomic
//!   delete + stale-callout wrap, batch CAS) work through the real
//!   `#[tool]` handlers.
//!
//! write-substrate-layering M4e: previously this suite drove the deleted
//! `WriteTools` dispatcher in-process via a test-only shim
//! (`get_active_write_tools_test`) — one layer below the server, per design
//! decision 8 ("known-bad"). It now calls `ObsidianMcpServer::call_tool`
//! directly (the same public method `test_mcp_provider_workflows.rs` uses),
//! so every scenario here exercises the real handler. The turbomcp
//! wire-protocol layer above (JSON-RPC framing, child-process spawn) is
//! exercised separately by `test_mcp_wire_e2e.rs`; scenarios already covered
//! there byte-for-byte were deleted from this file rather than duplicated.
//! Scenarios that depended on the deleted server-side reindex test shims
//! (`get_reindex_queue_test` / `has_git_drainer_test` /
//! `has_git_ref_listener_test` / `flush_reindex_for_active_vault_test`) were
//! also deleted — that machinery's coverage now lives in
//! `turbovault-vault/src/reindex.rs`'s own unit tests (the manager owns
//! reindex since bite 3a).

use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;
use turbovault_core::config::{VaultConfig, VaultGitConfig, WriteBackend};
use turbovault_tools::VaultRepo;

fn ctx() -> RequestContext {
    RequestContext::new()
}

/// Call a tool in-process through the real `#[tool]` handler dispatch and
/// return its structured `StandardResponse` JSON. Panics loudly on any
/// error — every call in this file is expected to succeed.
async fn call(
    server: &ObsidianMcpServer,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let result = server
        .call_tool(name, arguments, &ctx())
        .await
        .unwrap_or_else(|error| panic!("tool {name:?} failed: {error}"));
    assert!(
        !result.is_error(),
        "tool {name:?} returned an error: {}",
        result
            .first_text()
            .unwrap_or("tool returned an error without text")
    );
    result
        .structured_content
        .unwrap_or_else(|| panic!("tool {name:?} returned no structured content"))
}

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

    // Registered directly through the multi-vault manager rather than the
    // `add_vault` MCP tool. Since turbovault-kdq that tool *can* select
    // `write_backend: git` (see test_vault_registration_backend.rs); this
    // suite keeps the direct route so its subject stays the substrate, not
    // the registration surface.
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

    let head_before = head_oid(tmp.path()).unwrap();
    call(
        &server,
        "write_note",
        serde_json::json!({ "path": "concepts/foo.md", "content": "# Foo\n\nplaceholder\n" }),
    )
    .await;
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

/// turbovault-5nn: with `git.require_commit_message = true`, a mutation called
/// WITHOUT a caller message (or with a blank one) is refused; a real message is
/// accepted (trimmed).
#[tokio::test]
#[serial_test::serial]
async fn e2e_require_commit_message_gate() {
    let tmp = TempDir::new().unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = git2::Repository::init_opts(tmp.path(), &opts).unwrap();
    {
        let mut idx = repo.index().unwrap();
        let tree_oid = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("Init", "init@example").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }
    let git_cfg = VaultGitConfig {
        require_commit_message: true,
        ..VaultGitConfig::default()
    };
    let vault_config = VaultConfig::builder("req", tmp.path())
        .write_backend(WriteBackend::Git)
        .git(git_cfg)
        .build()
        .unwrap();
    let server = ObsidianMcpServer::new().unwrap();
    server.multi_vault().add_vault(vault_config).await.unwrap();
    server.multi_vault().set_active_vault("req").await.unwrap();

    // Missing message → refused.
    let err = server
        .resolve_commit_message_test(None, "write_note x.md".to_string())
        .await
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("commit message"),
        "expected require-commit-message refusal, got: {err:?}"
    );

    // Blank / whitespace-only message → also refused.
    assert!(
        server
            .resolve_commit_message_test(Some("   ".to_string()), "fallback".to_string())
            .await
            .is_err(),
        "whitespace-only message must be treated as missing"
    );

    // Real message → accepted, trimmed.
    let msg = server
        .resolve_commit_message_test(Some("  real subject  ".to_string()), "fallback".to_string())
        .await
        .unwrap();
    assert_eq!(msg, "real subject");
}

/// turbovault-5nn: a vault with the default (require_commit_message = false)
/// auto-derives the fallback subject when no message is given.
#[tokio::test]
#[serial_test::serial]
async fn e2e_commit_message_optional_by_default() {
    let (_tmp, _name, server) = setup_git_vault().await;
    let msg = server
        .resolve_commit_message_test(None, "write_note x.md".to_string())
        .await
        .unwrap();
    assert_eq!(
        msg, "write_note x.md",
        "default vault auto-derives the subject"
    );
}

/// turbovault-6fo.18: move_note with update_backlinks is one atomic commit.
/// HEAD advances by exactly one commit; the rename + the linker rewrite land
/// together. `move_note` self-flushes its own reindex queue (turbovault-78w)
/// before resolving backlinks, so no manual drain is needed here.
#[tokio::test]
#[serial_test::serial]
async fn e2e_move_note_with_link_updates_one_commit() {
    let (tmp, _name, server) = setup_git_vault().await;

    call(
        &server,
        "write_note",
        serde_json::json!({ "path": "old.md", "content": "# Old\n" }),
    )
    .await;
    call(
        &server,
        "write_note",
        serde_json::json!({ "path": "linker.md", "content": "see [[old]] here\n" }),
    )
    .await;
    let head_before = head_oid(tmp.path()).unwrap();

    let result = call(
        &server,
        "move_note",
        serde_json::json!({ "from": "old.md", "to": "new.md", "update_backlinks": true }),
    )
    .await;
    assert_eq!(
        result["data"]["link_sources_updated"],
        serde_json::json!(["linker.md"])
    );
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

/// turbovault-6fo.18: delete_note with on_backlinks='rewrite-stale-callout'
/// wraps the linker as part of the same commit. The strikethrough
/// breadcrumb survives.
#[tokio::test]
#[serial_test::serial]
async fn e2e_delete_note_rewrite_stale_callout_atomic() {
    let (tmp, _name, server) = setup_git_vault().await;

    call(
        &server,
        "write_note",
        serde_json::json!({ "path": "doomed.md", "content": "# Doomed" }),
    )
    .await;
    call(
        &server,
        "write_note",
        serde_json::json!({ "path": "a.md", "content": "see [[doomed]] for details\n" }),
    )
    .await;

    let result = call(
        &server,
        "delete_note",
        serde_json::json!({
            "path": "doomed.md",
            "confirm_path": "doomed.md",
            "on_backlinks": "rewrite-stale-callout",
        }),
    )
    .await;
    assert_eq!(
        result["data"]["link_sources_updated"],
        serde_json::json!(["a.md"])
    );
    assert!(!tmp.path().join("doomed.md").exists());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.md")).unwrap(),
        "see ~~[[doomed]]~~ for details\n"
    );
}

/// turbovault-6fo.18: batch_execute with all-matching preconditions lands
/// as ONE commit (architecture §5.4: "1 changeset = 1 commit"), carrying
/// the MCP layer's auto-derived op-tally subject.
#[tokio::test]
#[serial_test::serial]
async fn e2e_batch_execute_lands_as_one_commit() {
    let (tmp, _name, server) = setup_git_vault().await;

    let head_before = head_oid(tmp.path()).unwrap();
    let operations = serde_json::json!([
        { "type": "CreateNote", "path": "x.md", "content": "x" },
        { "type": "CreateNote", "path": "y.md", "content": "y" },
        { "type": "CreateNote", "path": "z.md", "content": "z" },
    ]);
    let res = call(
        &server,
        "batch_execute",
        serde_json::json!({ "operations": operations }),
    )
    .await;
    assert_eq!(res["success"], true);
    assert_eq!(res["data"]["executed"], 3);

    let head_after = head_oid(tmp.path()).unwrap();
    assert_ne!(head_after, head_before);
    let repo = git2::Repository::open(tmp.path()).unwrap();
    let commit = repo.find_commit(head_after).unwrap();
    assert_eq!(
        commit.parent_count(),
        1,
        "exactly one new commit for the 3-op batch"
    );
    let msg = head_message(tmp.path());
    assert!(
        msg.contains("batch:") && msg.contains("create"),
        "expected the MCP layer's auto-derived batch subject, got: {msg:?}"
    );
}

/// turbovault-1ne: remove_vault cleanly drops the fanout `CommitLocks` entry
/// once no fanout is active (`git_repos`/`git_reindex_queues`/`git_drainers`/
/// `git_ref_listeners` were the server's own now-deleted reindex duplicate;
/// `git_locks` alone survives write-substrate-layering M4e — fanout is now
/// its ONLY populator: ordinary writes route straight through the manager,
/// which owns its own internal `CommitLocks` independent of this map).
#[tokio::test]
#[serial_test::serial]
async fn e2e_remove_vault_cleans_up_git_backend_state() {
    let (tmp, name, server) = setup_git_vault().await;

    // begin_fanout is what populates `git_locks` now (fanout.rs calls
    // `get_or_init_git_locks`); abandon it immediately so remove_vault isn't
    // blocked by an active fanout.
    call(&server, "begin_fanout", serde_json::json!({})).await;
    call(&server, "abandon_fanout", serde_json::json!({})).await;

    assert!(
        server.has_git_locks_test(name).await,
        "locks entry should be live"
    );

    server.remove_vault_test(name).await.unwrap();

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
/// Caller must abandon_fanout first. Symmetric with the
/// nested-fanout refusal in begin_fanout.
#[tokio::test]
#[serial_test::serial]
async fn e2e_remove_vault_blocked_while_fanout_active() {
    let (_tmp, name, server) = setup_git_vault().await;

    // Bring up the lock entry with a seed write.
    call(
        &server,
        "write_note",
        serde_json::json!({ "path": "seed.md", "content": "seed\n" }),
    )
    .await;

    // Open a fanout worktree directly through the substrate-side
    // path so the e2e test doesn't depend on the full
    // `begin_fanout` MCP wire shape (which is exercised
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
        msg.contains("active fanout") && msg.contains("abandon_fanout"),
        "expected refuse-message mentioning fanout + abandon_fanout, got: {}",
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

/// turbovault-gje: update_frontmatter previously bypassed the git substrate
/// by calling VaultManager::write_file directly from the tool layer. Now
/// that the handler is a thin call into `MetadataTools` over the manager
/// (write-substrate-layering M4d), drive it through the REAL handler and
/// verify it produces a real git commit.
#[tokio::test]
#[serial_test::serial]
async fn e2e_update_frontmatter_routed_through_substrate() {
    let (tmp, _name, server) = setup_git_vault().await;

    call(
        &server,
        "write_note",
        serde_json::json!({ "path": "notes/sample.md", "content": "---\ntags: [a]\n---\nbody\n" }),
    )
    .await;
    let head_after_seed = head_oid(tmp.path()).unwrap();

    call(
        &server,
        "update_frontmatter",
        serde_json::json!({
            "path": "notes/sample.md",
            "frontmatter": { "title": "hello" },
            "merge": true,
        }),
    )
    .await;

    let head_after_update = head_oid(tmp.path()).unwrap();
    assert_ne!(
        head_after_update, head_after_seed,
        "update_frontmatter should advance HEAD"
    );
    let msg = head_message(tmp.path());
    assert!(
        msg.contains("update_frontmatter"),
        "commit subject should reflect tool, got: {}",
        msg
    );
    let on_disk = std::fs::read_to_string(tmp.path().join("notes/sample.md")).unwrap();
    assert!(
        on_disk.contains("title: hello") || on_disk.contains("title:hello"),
        "new frontmatter not visible on disk, got: {}",
        on_disk
    );
}

/// turbovault-gje: same routing proof for create_from_template — the
/// template path used to call manager.write_file directly. Drive it
/// through the real handler with a built-in template ("doc") and verify
/// it now commits.
#[tokio::test]
#[serial_test::serial]
async fn e2e_create_from_template_routed_through_substrate() {
    let (tmp, _name, server) = setup_git_vault().await;
    let head_before = head_oid(tmp.path()).unwrap();

    call(
        &server,
        "create_from_template",
        serde_json::json!({
            "template_id": "doc",
            "file_path": "templated/n.md",
            "fields": serde_json::to_string(&serde_json::json!({
                "title": "hello",
                "summary": "a summary",
            })).unwrap(),
        }),
    )
    .await;

    let head_after = head_oid(tmp.path()).unwrap();
    assert_ne!(
        Some(head_after),
        Some(head_before),
        "create_from_template should advance HEAD"
    );
    let msg = head_message(tmp.path());
    assert!(
        msg.contains("create_from_template"),
        "commit subject should reflect tool, got: {}",
        msg
    );
    assert!(tmp.path().join("templated/n.md").exists());
}

// Deliberately not migrated (write-substrate-layering M4e):
// - e2e_batch_execute_per_op_cas_aborts_atomically — byte-for-byte covered
//   by `wire_batch_execute_stale_cas_aborts_atomically` in
//   test_mcp_wire_e2e.rs (the real child-process wire path).
// - e2e_external_commit_observed_by_ref_listener /
//   e2e_reindex_queue_drains_after_substrate_writes — drove the server's
//   OWN now-deleted reindex duplicate (`get_reindex_queue_test` /
//   `flush_reindex_for_active_vault_test` / the HEAD-ref listener test
//   shim). The manager now owns reindex end-to-end (bite 3a); the
//   underlying `watch_ref_changes`/drain guarantees are unit-tested in
//   `turbovault-vault/src/reindex.rs`, and every derived-state assertion
//   in the wire suite already exercises the manager's self-flushing reads.
