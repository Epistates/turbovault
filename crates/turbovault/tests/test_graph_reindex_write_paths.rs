//! TV-015 (turbovault-8v8) regression cover: the link graph must be current
//! the moment a write returns, on EVERY write path that changes link
//! structure — not just single `write_note`.
//!
//! The original dogfood report (filed against the pre-relayering build) had
//! two manifestations:
//!
//! 1. `batch_execute` `CreateNote` of a note carrying wikilinks left the
//!    note's OUTGOING edges out of the graph — `get_forward_links` on it came
//!    back empty until a subsequent single `write_note` touched the file.
//! 2. `move_note` rewrote inbound links on disk but did not reindex —
//!    post-move `get_forward_links(linker)` and `get_backlinks(renamed)` were
//!    both empty until the linker was touched.
//!
//! Plus TV-002's two residual re-verification cases, whose root cause the
//! ticket attributes to the same defect: a batch-created linker's inbound
//! edge being invisible to a later `move_note` (covered by
//! `batch_created_linker_is_visible_to_a_later_move_*`), and post-move
//! staleness (manifestation 2).
//!
//! Every assertion here runs with NO intervening `write_note` touch — that
//! touch is precisely the workaround the ticket reported, so admitting one
//! would defeat the test. Each scenario runs on BOTH write backends: a world
//! diverging here is a bug, not a per-backend expectation.

use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;
use turbovault_core::config::{VaultConfig, VaultGitConfig, WriteBackend};

const VAULT: &str = "tv015";

/// The ticket's repro note: four wikilink forms, all pointing at `target`.
const LINKER: &str = "# Linker\n\nplain [[target]]\nalias [[target|The Target]]\nheading [[target#Notes]]\nembed ![[target]]\n";
const TARGET: &str = "# Target\n\n## Notes\n\nplaceholder\n";

#[derive(Clone, Copy, Debug)]
enum Backend {
    Direct,
    Git,
}

fn ctx() -> RequestContext {
    RequestContext::new()
}

/// Call a tool through the real `#[tool]` handler dispatch and return its
/// `StandardResponse` JSON. Panics on any error — every call here succeeds.
async fn call(server: &ObsidianMcpServer, name: &str, arguments: Value) -> Value {
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

/// Register a vault on the requested backend. Git vaults are registered
/// through the multi-vault manager because the `add_vault` MCP tool only
/// exposes name+path = Direct (turbovault-xj8).
async fn setup(backend: Backend) -> (TempDir, ObsidianMcpServer) {
    let tmp = TempDir::new().unwrap();
    let builder = VaultConfig::builder(VAULT, tmp.path());
    let config = match backend {
        Backend::Direct => builder.build().unwrap(),
        Backend::Git => {
            init_git_repo(tmp.path());
            builder
                .write_backend(WriteBackend::Git)
                .git(VaultGitConfig::default())
                .build()
                .unwrap()
        }
    };
    let server = ObsidianMcpServer::new().unwrap();
    server.multi_vault().add_vault(config).await.unwrap();
    server.multi_vault().set_active_vault(VAULT).await.unwrap();
    (tmp, server)
}

/// Git repo with a born HEAD — the substrate needs a non-unborn baseline.
fn init_git_repo(path: &std::path::Path) {
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = git2::Repository::init_opts(path, &opts).unwrap();
    let tree_oid = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("Init", "init@example").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
}

/// `get_forward_links` / `get_backlinks` payload as vault-absolute path
/// strings.
async fn links(server: &ObsidianMcpServer, tool: &str, path: &str) -> Vec<String> {
    call(server, tool, json!({ "path": path }))
        .await
        .get("data")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{tool} returned no data array"))
        .iter()
        .map(|v| v.as_str().expect("link path is a string").to_string())
        .collect()
}

#[track_caller]
fn assert_contains(links: &[String], suffix: &str, what: &str) {
    assert!(
        links.iter().any(|p| p.ends_with(suffix)),
        "{what}: expected a link ending in {suffix:?}, got {links:?}"
    );
}

// ---------------------------------------------------------------------------
// Manifestation 1: batch_execute CreateNote must index outgoing edges.
// ---------------------------------------------------------------------------

async fn batch_created_note_is_graphed(backend: Backend) {
    let (_tmp, server) = setup(backend).await;

    // Target first, via the known-good single-write path, so the only
    // question under test is whether the BATCH-created linker is graphed.
    call(
        &server,
        "write_note",
        json!({ "path": "target.md", "content": TARGET }),
    )
    .await;

    call(
        &server,
        "batch_execute",
        json!({ "operations": [
            { "type": "CreateNote", "path": "linker.md", "content": LINKER },
        ]}),
    )
    .await;

    // No write_note touch of linker.md between the batch and these reads.
    assert_contains(
        &links(&server, "get_forward_links", "linker.md").await,
        "target.md",
        format!("{backend:?}: batch-created note's outgoing edge").as_str(),
    );
    assert_contains(
        &links(&server, "get_backlinks", "target.md").await,
        "linker.md",
        format!("{backend:?}: inbound edge from a batch-created note").as_str(),
    );
}

#[tokio::test]
#[serial_test::serial]
async fn batch_created_note_is_graphed_direct() {
    batch_created_note_is_graphed(Backend::Direct).await;
}

#[tokio::test]
#[serial_test::serial]
async fn batch_created_note_is_graphed_git() {
    batch_created_note_is_graphed(Backend::Git).await;
}

// ---------------------------------------------------------------------------
// Manifestation 2: move_note must reindex the moved note and every linker
// whose wikilinks it rewrote.
// ---------------------------------------------------------------------------

async fn move_note_reindexes_both_endpoints(backend: Backend) {
    let (_tmp, server) = setup(backend).await;

    call(
        &server,
        "write_note",
        json!({ "path": "target.md", "content": TARGET }),
    )
    .await;
    call(
        &server,
        "write_note",
        json!({ "path": "linker.md", "content": LINKER }),
    )
    .await;

    // `update_backlinks` defaults to the git backend only; pin it true so
    // both worlds run the same scenario (the ticket's repro was git-backed).
    let moved = call(
        &server,
        "move_note",
        json!({ "from": "target.md", "to": "renamed.md", "update_backlinks": true }),
    )
    .await;
    let updated = moved["data"]["link_sources_updated"]
        .as_array()
        .expect("link_sources_updated array");
    assert!(
        updated.iter().any(|v| v.as_str() == Some("linker.md")),
        "{backend:?}: move_note should have rewritten linker.md on disk, got {updated:?}"
    );

    // No write_note touch of linker.md between the move and these reads.
    assert_contains(
        &links(&server, "get_forward_links", "linker.md").await,
        "renamed.md",
        format!("{backend:?}: linker's outgoing edge after the target moved").as_str(),
    );
    assert_contains(
        &links(&server, "get_backlinks", "renamed.md").await,
        "linker.md",
        format!("{backend:?}: renamed target's inbound edge").as_str(),
    );
}

#[tokio::test]
#[serial_test::serial]
async fn move_note_reindexes_both_endpoints_direct() {
    move_note_reindexes_both_endpoints(Backend::Direct).await;
}

#[tokio::test]
#[serial_test::serial]
async fn move_note_reindexes_both_endpoints_git() {
    move_note_reindexes_both_endpoints(Backend::Git).await;
}

// ---------------------------------------------------------------------------
// TV-002 residual case 1: the two defects compose — a linker created by
// batch_execute must be discoverable as an inbound link by a later
// move_note, so its wikilinks get rewritten instead of silently broken.
// ---------------------------------------------------------------------------

async fn batch_created_linker_is_visible_to_a_later_move(backend: Backend) {
    let (tmp, server) = setup(backend).await;

    call(
        &server,
        "write_note",
        json!({ "path": "target.md", "content": TARGET }),
    )
    .await;
    call(
        &server,
        "batch_execute",
        json!({ "operations": [
            { "type": "CreateNote", "path": "linker.md", "content": LINKER },
        ]}),
    )
    .await;

    let moved = call(
        &server,
        "move_note",
        json!({ "from": "target.md", "to": "renamed.md", "update_backlinks": true }),
    )
    .await;
    let updated = moved["data"]["link_sources_updated"]
        .as_array()
        .expect("link_sources_updated array");
    assert!(
        updated.iter().any(|v| v.as_str() == Some("linker.md")),
        "{backend:?}: move_note missed the batch-created linker, got {updated:?}"
    );

    let on_disk = std::fs::read_to_string(tmp.path().join("linker.md")).unwrap();
    assert!(
        on_disk.contains("[[renamed]]"),
        "{backend:?}: linker.md still points at the old name: {on_disk:?}"
    );
    assert_contains(
        &links(&server, "get_backlinks", "renamed.md").await,
        "linker.md",
        format!("{backend:?}: batch-created linker after the move").as_str(),
    );
}

#[tokio::test]
#[serial_test::serial]
async fn batch_created_linker_is_visible_to_a_later_move_direct() {
    batch_created_linker_is_visible_to_a_later_move(Backend::Direct).await;
}

#[tokio::test]
#[serial_test::serial]
async fn batch_created_linker_is_visible_to_a_later_move_git() {
    batch_created_linker_is_visible_to_a_later_move(Backend::Git).await;
}
