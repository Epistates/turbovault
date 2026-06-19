//! turbovault-3i5: wire-level (black-box, over-stdio) MCP e2e tests.
//!
//! Playwright-for-MCP: each test spawns the REAL `turbovault` binary as a
//! child process and drives it over JSON-RPC/stdio with the first-party
//! `turbomcp-client`. Nothing is called in-process — assertions are made
//! purely against `CallToolResult`s coming back over the wire.
//!
//! This complements `test_mcp_e2e_git_substrate.rs` (turbovault-6fo.18 /
//! GWS.17), which deliberately stops one layer below the wire (it calls the
//! `WriteTools` dispatcher in-process via `get_active_write_tools_test()`).
//! This suite exercises what that one skips: tool schema/registration, JSON
//! argument (de)serialization, MCP response framing, AND — crucially — the
//! REAL lazy-reindex flush-on-query path. There is no `flush_*_test`
//! backdoor reachable over the wire, so every derived-state assertion below
//! goes through a read tool (`get_backlinks` / `get_forward_links` /
//! `search`) whose handler drains the per-vault `ReindexQueue` via
//! `get_vault_pair_with_reindex` before answering (turbovault-brs / GWS.14).
//!
//! The git-backed vault is registered through a temp `--config` YAML, the
//! only over-wire route to `write_backend: git` (the `add_vault` MCP tool
//! exposes name+path only = Legacy; turbovault-xj8).
//!
//! Serialized (`#[serial_test::serial]`): each test spawns its own server
//! process, but the test process shares libgit2's process-wide init.
//!
//! This suite surfaced two derived-index coherence bugs on the git backend,
//! both now fixed (all scenarios run; none ignored):
//! - turbovault-9zr: multi-file commits didn't reindex the link graph — fixed
//!   by re-resolving dangling links on file add + serializing flush passes.
//! - turbovault-2ag: edit didn't reindex search — fixed by keying search docs
//!   by vault-relative path and making the path field a raw STRING so
//!   `delete_term` matches (was a tokenized TEXT field).

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp_client::Client;
use turbomcp_protocol::types::CallToolResult;
use turbomcp_transport::child_process::{ChildProcessConfig, ChildProcessTransport};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Spawn `turbovault --config <yaml>` over stdio with a freshly-initialized
/// git vault registered as `write_backend: git`, complete the MCP
/// `initialize` handshake, and make the vault active.
///
/// Returns the vault + config `TempDir`s (kept alive for the test's
/// lifetime) and the connected client. Dropping the returned tuple drops the
/// client first (killing the server via `kill_on_drop`), then the temp dirs.
async fn setup_wire_vault() -> (TempDir, TempDir, Client<ChildProcessTransport>) {
    let vault = TempDir::new().expect("vault tempdir");
    init_git_repo(vault.path());

    // Config lives OUTSIDE the vault so it isn't an untracked file inside the
    // working tree the substrate manages.
    let cfg_dir = TempDir::new().expect("config tempdir");
    let cfg_path = cfg_dir.path().join("turbovault.yaml");
    let yaml = format!(
        "vaults:\n  - name: gvault\n    path: {}\n    is_default: true\n    \
         write_backend: git\n    git:\n      branch: main\n      \
         merge_strategy: fast-forward\n",
        vault.path().display()
    );
    std::fs::write(&cfg_path, yaml).expect("write config yaml");

    let config = ChildProcessConfig {
        command: env!("CARGO_BIN_EXE_turbovault").to_string(),
        args: vec!["--config".to_string(), cfg_path.display().to_string()],
        kill_on_drop: true,
        ..Default::default()
    };
    let client = Client::new(ChildProcessTransport::new(config));
    client
        .initialize()
        .await
        .expect("MCP initialize handshake over stdio");

    // The YAML marks gvault `is_default: true`; set it active explicitly so
    // the test is robust to default-selection changes.
    call(&client, "set_active_vault", json!({ "name": "gvault" })).await;

    (vault, cfg_dir, client)
}

/// Init a git repo at `path` with `main` as HEAD and one empty initial
/// commit, so HEAD is born (the substrate needs a non-unborn baseline).
fn init_git_repo(path: &Path) {
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = git2::Repository::init_opts(path, &opts).expect("git init");
    let tree_oid = {
        let mut idx = repo.index().expect("index");
        idx.write_tree().expect("write_tree")
    };
    let tree = repo.find_tree(tree_oid).expect("find_tree");
    let sig = git2::Signature::now("Init", "init@example").expect("signature");
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .expect("initial commit");
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

/// Call a tool over the wire, assert it did not error, and return the
/// decoded `StandardResponse` JSON (the `{ vault, operation, success, data,
/// count, ... }` envelope turbovault tools emit).
async fn call(client: &Client<ChildProcessTransport>, name: &str, args: Value) -> Value {
    let result = call_raw(client, name, args)
        .await
        .unwrap_or_else(|e| panic!("call_tool {name} returned a protocol error: {e}"));
    assert_ne!(
        result.is_error,
        Some(true),
        "tool {name} returned is_error: {}",
        serde_json::to_value(&result).unwrap_or(Value::Null)
    );
    extract(&result)
}

/// Raw tool call: returns the protocol `Result` so callers can assert on
/// failure modes (used by the TV-006 regression test).
async fn call_raw(
    client: &Client<ChildProcessTransport>,
    name: &str,
    args: Value,
) -> turbomcp_protocol::Result<CallToolResult> {
    let map: HashMap<String, Value> = match args {
        Value::Object(m) => m.into_iter().collect(),
        Value::Null => HashMap::new(),
        other => panic!("tool args must be a JSON object, got: {other}"),
    };
    client.call_tool(name, Some(map), None).await
}

/// Decode the tool payload from a `CallToolResult`: prefer `structuredContent`,
/// else parse the first text `content` block as JSON.
fn extract(result: &CallToolResult) -> Value {
    let v = serde_json::to_value(result).expect("serialize CallToolResult");
    if let Some(sc) = v.get("structuredContent")
        && !sc.is_null()
    {
        return sc.clone();
    }
    let text = v
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .find_map(|b| b.get("text").and_then(Value::as_str))
        })
        .unwrap_or_else(|| panic!("no text content block in tool result: {v}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("tool text content was not JSON ({e}): {text}"))
}

/// True if the response's `data` payload mentions `needle` anywhere (link
/// list items are structs whose path/target we don't want to pin exactly).
fn data_mentions(resp: &Value, needle: &str) -> bool {
    resp.get("data")
        .map(|d| d.to_string().contains(needle))
        .unwrap_or(false)
}

/// The `count` field of a `StandardResponse` (0 if absent).
fn count_of(resp: &Value) -> u64 {
    resp.get("count").and_then(Value::as_u64).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Graph + search reflect commits (the primary interest)
// ---------------------------------------------------------------------------

/// write_note commits land in the link graph: a page's wikilink shows up as a
/// forward link, and the target gains the backlink — observed entirely
/// through the over-wire read tools (which drain the reindex queue).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_write_note_updates_link_graph() {
    let (_vault, _cfg, client) = setup_wire_vault().await;

    call(
        &client,
        "write_note",
        json!({ "path": "concepts/beta.md", "content": "# Beta\n" }),
    )
    .await;
    call(
        &client,
        "write_note",
        json!({ "path": "concepts/alpha.md", "content": "# Alpha\n\nlinks to [[beta]]\n" }),
    )
    .await;

    let fwd = call(
        &client,
        "get_forward_links",
        json!({ "path": "concepts/alpha.md" }),
    )
    .await;
    assert!(
        data_mentions(&fwd, "beta"),
        "alpha's forward links should include beta after commit: {fwd}"
    );

    let back = call(
        &client,
        "get_backlinks",
        json!({ "path": "concepts/beta.md" }),
    )
    .await;
    assert!(
        data_mentions(&back, "alpha"),
        "beta's backlinks should include alpha after commit: {back}"
    );
}

/// move_note + update_backlinks is one atomic commit; afterwards the inbound
/// linker is rewritten to the new slug and the backlink follows. Regression
/// guard for turbovault-78w: move_note self-flushes its reindex queue before
/// resolving backlinks, so a pending reindex no longer causes a silent skip —
/// no manual pre-flush is needed here (it was required before the fix).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_move_note_rewrites_links() {
    let (_vault, _cfg, client) = setup_wire_vault().await;

    call(
        &client,
        "write_note",
        json!({ "path": "old.md", "content": "# Old\n" }),
    )
    .await;
    call(
        &client,
        "write_note",
        json!({ "path": "linker.md", "content": "see [[old]] here\n" }),
    )
    .await;

    // No manual drain: move_note flushes its own reindex queue (turbovault-78w)
    // so it sees linker->old and rewrites it even with the writes' reindex still
    // pending. Before the fix this silently found nothing to rewrite.
    let moved = call(
        &client,
        "move_note",
        json!({ "from": "old.md", "to": "renamed.md", "update_backlinks": true }),
    )
    .await;
    assert!(
        data_mentions(&moved, "linker"),
        "move_note must report linker.md in link_sources_updated: {moved}"
    );

    let linker = call(&client, "read_note", json!({ "path": "linker.md" })).await;
    let content = linker["data"]["content"]
        .as_str()
        .expect("read_note returns data.content");
    assert!(
        content.contains("[[renamed]]"),
        "linker should be rewritten to [[renamed]]: {content:?}"
    );
    assert!(
        !content.contains("[[old]]"),
        "linker should no longer reference [[old]]: {content:?}"
    );

    let back = call(&client, "get_backlinks", json!({ "path": "renamed.md" })).await;
    assert!(
        data_mentions(&back, "linker"),
        "renamed.md's backlinks should include linker after the move commit: {back}"
    );
}

/// delete_note drops the page from the search index: a unique token is found
/// before the delete commit and gone after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_delete_note_drops_from_search() {
    let (_vault, _cfg, client) = setup_wire_vault().await;

    call(
        &client,
        "write_note",
        json!({ "path": "doomed.md", "content": "# Doomed\n\nzqxwvut unique marker\n" }),
    )
    .await;

    let before = call(&client, "search", json!({ "query": "zqxwvut" })).await;
    assert!(
        count_of(&before) >= 1,
        "search should find the unique token before delete: {before}"
    );

    call(
        &client,
        "delete_note",
        json!({ "path": "doomed.md", "confirm_path": "doomed.md" }),
    )
    .await;

    let after = call(&client, "search", json!({ "query": "zqxwvut" })).await;
    assert_eq!(
        count_of(&after),
        0,
        "search should not find the token after delete commit: {after}"
    );
}

/// batch_execute applies multiple creates in one commit; both files are in the
/// link graph afterwards (atomic, all-or-none reveal). Regression guard for
/// turbovault-9zr: a `[[two]]` link from a file added alongside its target in
/// one commit now resolves to an edge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_batch_execute_atomic_multi_file() {
    let (_vault, _cfg, client) = setup_wire_vault().await;

    let operations = json!([
        { "type": "CreateNote", "path": "b/one.md", "content": "# One\n\nlinks [[two]]\n" },
        { "type": "CreateNote", "path": "b/two.md", "content": "# Two\n" },
    ]);
    call(
        &client,
        "batch_execute",
        json!({ "operations": operations }),
    )
    .await;

    let fwd = call(&client, "get_forward_links", json!({ "path": "b/one.md" })).await;
    assert!(
        data_mentions(&fwd, "two"),
        "one.md forward links should include two after batch commit: {fwd}"
    );
    let back = call(&client, "get_backlinks", json!({ "path": "b/two.md" })).await;
    assert!(
        data_mentions(&back, "one"),
        "two.md backlinks should include one after batch commit: {back}"
    );
}

/// edit_note body changes should be reflected in the search index: the new
/// term findable, the old term gone after the edit commit.
///
/// IGNORED — executable repro for turbovault-2ag: edit (content modify)
/// doesn't update the tantivy index on the git backend (delete→search does).
/// The edit commits fine (content correct on disk). Remove `#[ignore]` once
/// 2ag is fixed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_edit_note_updates_search_index() {
    let (_vault, _cfg, client) = setup_wire_vault().await;

    call(
        &client,
        "write_note",
        json!({ "path": "s.md", "content": "# S\n\nalphaword content here\n" }),
    )
    .await;
    assert!(
        count_of(&call(&client, "search", json!({ "query": "alphaword" })).await) >= 1,
        "alphaword should be findable before the edit"
    );

    let edits =
        "<<<<<<< SEARCH\nalphaword content here\n=======\nbetaword content here\n>>>>>>> REPLACE\n";
    call(
        &client,
        "edit_note",
        json!({ "path": "s.md", "edits": edits }),
    )
    .await;

    assert!(
        count_of(&call(&client, "search", json!({ "query": "betaword" })).await) >= 1,
        "betaword should be findable after the edit commit"
    );
    assert_eq!(
        count_of(&call(&client, "search", json!({ "query": "alphaword" })).await),
        0,
        "alphaword should be gone from the index after the edit commit"
    );
}

// ---------------------------------------------------------------------------
// TV-006 regression (turbovault-u9w): ambiguous `=======` divider
// ---------------------------------------------------------------------------

/// TV-006 / turbovault-u9w: when a SEARCH/REPLACE block's body legitimately
/// contains a literal `=======` line, the block has two `=======` lines and
/// is ambiguous. The parser MUST fail loud (not silently truncate) and the
/// file MUST be left untouched. This locks the fix at the MCP surface.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_edit_note_rejects_ambiguous_divider_tv006() {
    let (_vault, _cfg, client) = setup_wire_vault().await;

    let original = "# Div\n\nbefore the rule\n=======\nafter the rule\n";
    call(
        &client,
        "write_note",
        json!({ "path": "div.md", "content": original }),
    )
    .await;

    // SEARCH body spans the literal `=======` line -> two dividers -> ambiguous.
    let edits = "<<<<<<< SEARCH\nbefore the rule\n=======\nafter the rule\n=======\nreplaced\n>>>>>>> REPLACE\n";
    let result = call_raw(
        &client,
        "edit_note",
        json!({ "path": "div.md", "edits": edits }),
    )
    .await;

    let errored = match &result {
        Err(_) => true,
        Ok(r) => r.is_error == Some(true),
    };
    assert!(
        errored,
        "TV-006 regression: edit_note silently accepted an ambiguous `=======` block \
         instead of failing loud (result: {:?})",
        result
            .as_ref()
            .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
    );

    let msg = match &result {
        Err(e) => e.to_string(),
        Ok(r) => serde_json::to_value(r).unwrap_or(Value::Null).to_string(),
    };
    assert!(
        msg.contains("=======") || msg.to_lowercase().contains("divider"),
        "expected a divider-ambiguity error, got: {msg}"
    );

    // The file must be untouched — no silent truncation, no stray `=======`.
    let read = call(&client, "read_note", json!({ "path": "div.md" })).await;
    let content = read["data"]["content"]
        .as_str()
        .expect("read_note returns data.content");
    assert!(
        content.contains("before the rule")
            && content.contains("after the rule")
            && content.contains("======="),
        "TV-006 regression: original content was mutated by a rejected edit: {content:?}"
    );
    assert!(
        !content.contains("replaced"),
        "TV-006 regression: rejected REPLACE body leaked into the file: {content:?}"
    );
}

/// turbovault-cuy: the fanout transaction lifecycle over the WIRE —
/// begin_fanout -> set_active_vault(fanout) -> write_note -> commit_fanout
/// -> the write merges back into the base vault's working tree. Previously only
/// the substrate-side open_fanout_worktree + the refuse path were tested; the
/// full MCP round-trip (the substrate's headline feature) had ZERO wire coverage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_fanout_begin_write_commit_lands_on_base() {
    let (vault, _cfg, client) = setup_wire_vault().await;

    let begun = call(&client, "begin_fanout", json!({})).await;
    let fanout_vault = begun["data"]["fanout_vault"]
        .as_str()
        .expect("begin_fanout returns data.fanout_vault")
        .to_string();

    call(&client, "set_active_vault", json!({ "name": fanout_vault })).await;
    call(
        &client,
        "write_note",
        json!({ "path": "fan.md", "content": "# In fanout\n" }),
    )
    .await;
    call(&client, "commit_fanout", json!({})).await;

    // Merge-back materialized fan.md into the BASE vault's working tree.
    let on_disk = std::fs::read_to_string(vault.path().join("fan.md"))
        .expect("fan.md must be merged into the base working tree after commit_fanout");
    assert!(on_disk.contains("In fanout"), "got: {on_disk:?}");
}

/// turbovault-cuy: abandon_fanout discards the fanout — a write made inside
/// the fanout never reaches the base vault.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_fanout_begin_write_abandon_discards() {
    let (vault, _cfg, client) = setup_wire_vault().await;

    let begun = call(&client, "begin_fanout", json!({})).await;
    let fanout_vault = begun["data"]["fanout_vault"].as_str().unwrap().to_string();

    call(&client, "set_active_vault", json!({ "name": fanout_vault })).await;
    call(
        &client,
        "write_note",
        json!({ "path": "ghost.md", "content": "discarded\n" }),
    )
    .await;
    call(&client, "abandon_fanout", json!({})).await;

    assert!(
        !vault.path().join("ghost.md").exists(),
        "ghost.md must NOT reach the base vault after abandon_fanout"
    );
}

/// turbovault-ct1: batch_execute atomic ABORT over the wire — a stale per-op CAS
/// precondition rolls the WHOLE batch back, zero files change. Previously only
/// the in-process e2e covered this; never the MCP wire shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_batch_execute_stale_cas_aborts_atomically() {
    let (vault, _cfg, client) = setup_wire_vault().await;
    call(
        &client,
        "write_note",
        json!({ "path": "s1.md", "content": "v1\n" }),
    )
    .await;

    // op[1] carries a bogus (stale) expected_hash -> the whole batch must abort.
    let _resp = call(
        &client,
        "batch_execute",
        json!({
            "operations": [
                { "type": "CreateNote", "path": "ghost.md", "content": "x\n" },
                { "type": "WriteNote", "path": "s1.md", "content": "v2\n",
                  "expected_hash": "0000000000000000000000000000000000000001" }
            ]
        }),
    )
    .await;

    // Atomicity over the wire: zero files change.
    assert!(
        !vault.path().join("ghost.md").exists(),
        "ghost.md must not be created on a stale-CAS abort"
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("s1.md")).unwrap(),
        "v1\n",
        "s1.md must be unchanged"
    );
}

/// turbovault-ct1: write_note with a STALE expected_hash surfaces a loud
/// conflict over the wire and leaves the file untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_write_note_stale_expected_hash_conflicts() {
    let (vault, _cfg, client) = setup_wire_vault().await;
    call(
        &client,
        "write_note",
        json!({ "path": "a.md", "content": "v1\n" }),
    )
    .await;

    let result = call_raw(
        &client,
        "write_note",
        json!({ "path": "a.md", "content": "v2\n",
                "expected_hash": "0000000000000000000000000000000000000001" }),
    )
    .await;
    let errored = match &result {
        Err(_) => true,
        Ok(r) => r.is_error == Some(true),
    };
    assert!(
        errored,
        "a stale expected_hash must surface a loud conflict: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("a.md")).unwrap(),
        "v1\n",
        "the file must be untouched after a rejected stale write"
    );
}

/// turbovault-uag: audit_log (and the rollback/audit family) loudly REFUSE on a
/// git-backend vault — the deliberate "refuse, don't return a silent empty
/// audit" contract, previously untested.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_audit_log_refused_on_git_backend() {
    let (_vault, _cfg, client) = setup_wire_vault().await;
    let result = call_raw(&client, "audit_log", json!({})).await;
    let errored = match &result {
        Err(_) => true,
        Ok(r) => r.is_error == Some(true),
    };
    assert!(
        errored,
        "audit_log must refuse on a git-backend vault: {result:?}"
    );
}

/// turbovault-uag: rollback_preview / rollback_note also refuse on a git-backend
/// vault (same refuse-don't-silently-empty contract as audit_log). The refusal
/// fires before any arg use, so a dummy operation_id reaches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn wire_rollback_refused_on_git_backend() {
    let (_vault, _cfg, client) = setup_wire_vault().await;
    for tool in ["rollback_preview", "rollback_note"] {
        let result = call_raw(&client, tool, json!({ "operation_id": "x" })).await;
        let errored = match &result {
            Err(_) => true,
            Ok(r) => r.is_error == Some(true),
        };
        assert!(
            errored,
            "{tool} must refuse on a git-backend vault: {result:?}"
        );
    }
}
