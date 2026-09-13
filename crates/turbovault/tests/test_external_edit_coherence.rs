//! Tools must agree with each other after somebody else edits the vault.
//!
//! The usual deployment has Obsidian open on the same directory an agent is
//! working through TurboVault, so external mutation is the normal state of the
//! world rather than an edge case. What made that damaging was never staleness
//! on its own, it was tools contradicting each other: `read_note` goes to disk
//! and is always current, while search and the link graph answered from derived
//! state that only this process's own writes ever updated. An agent could read
//! a note, see a phrase, search for the phrase, and be told it does not exist.
//!
//! These tests drive the public MCP surface and assert the derived views agree
//! with the file that is actually on disk.

use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;

/// Long enough to clear `RECONCILE_MIN_INTERVAL` (500ms), which is the bound
/// the gate promises. Tests wait it out rather than reaching past it, so what
/// they cover is the guarantee an agent actually gets.
const PAST_THE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(750);

fn ctx() -> RequestContext {
    RequestContext::new()
}

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

/// A registered vault with one note already written through the server, so the
/// search index and link graph are built and cached before anything external
/// happens.
async fn vault_with_warm_indexes() -> (TempDir, ObsidianMcpServer) {
    let temp = TempDir::new().expect("temporary vault");
    let server = ObsidianMcpServer::new().expect("provider composition");

    call(
        &server,
        "add_vault",
        json!({"name": "coherence", "path": temp.path().to_string_lossy()}),
    )
    .await;
    call(
        &server,
        "write_note",
        json!({"path": "a.md", "content": "# A\n\noriginaltoken\n"}),
    )
    .await;

    // Warm the caches so the tests exercise the invalidation path rather than a
    // cold build, which would be fresh for the wrong reason.
    let hits = search(&server, "originaltoken").await;
    assert_eq!(hits, 1, "the seeded token must be searchable to begin with");
    (temp, server)
}

async fn search(server: &ObsidianMcpServer, query: &str) -> usize {
    let response = call(server, "search", json!({"query": query})).await;
    response["count"].as_u64().unwrap_or(0) as usize
}

async fn forward_links(server: &ObsidianMcpServer, path: &str) -> Vec<String> {
    let response = call(server, "get_forward_links", json!({"path": path})).await;
    response["data"]
        .as_array()
        .map(|links| {
            links
                .iter()
                .map(|link| link.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The full scenario, as one session: an external editor rewrites a note, and
/// every view has to agree about it.
///
/// Asserted together rather than split apart because the failure being guarded
/// against is disagreement between them, which no single-view assertion can
/// catch.
#[tokio::test]
async fn every_view_agrees_after_an_external_edit() {
    let (temp, server) = vault_with_warm_indexes().await;

    // Somebody else rewrites the note: drops a word, adds a word, adds a link.
    std::fs::write(temp.path().join("b.md"), "# B\n").unwrap();
    std::fs::write(
        temp.path().join("a.md"),
        "# A\n\nreplacementtoken and a link to [[b]]\n",
    )
    .unwrap();
    tokio::time::sleep(PAST_THE_DEBOUNCE).await;

    // read_note goes to disk, so it was never the problem. It is the reference
    // the other views have to match.
    let note = call(&server, "read_note", json!({"path": "a.md"})).await;
    let content = note["data"]["content"].as_str().unwrap_or_default();
    assert!(content.contains("replacementtoken"));
    assert!(!content.contains("originaltoken"));

    assert_eq!(
        search(&server, "replacementtoken").await,
        1,
        "search must find text that read_note can see"
    );
    assert_eq!(
        search(&server, "originaltoken").await,
        0,
        "search must not report text that is no longer in the note"
    );

    let links = forward_links(&server, "a.md").await;
    assert_eq!(
        links.len(),
        1,
        "the link graph must see the link the note now has: {links:?}"
    );
    assert!(
        links[0].contains("b.md"),
        "unexpected link target: {links:?}"
    );
}

/// A note created outside the process becomes searchable.
///
/// This is the case a per-entry freshness check structurally cannot reach: it
/// can only re-check paths it already holds, and nothing holds a path nobody
/// has told it about.
#[tokio::test]
async fn a_note_created_externally_becomes_searchable() {
    let (temp, server) = vault_with_warm_indexes().await;

    std::fs::write(
        temp.path().join("appeared.md"),
        "# Appeared\n\nbrandnewtoken\n",
    )
    .unwrap();
    tokio::time::sleep(PAST_THE_DEBOUNCE).await;

    assert_eq!(
        search(&server, "brandnewtoken").await,
        1,
        "a note created outside the process must be discoverable"
    );

    // And the vault's own accounting agrees, not just the search index.
    let context = call(&server, "get_vault_context", json!({})).await;
    assert_eq!(
        context["data"]["current_stats"]["total_notes"], 2,
        "vault stats must count the note nobody told us about: {context:?}"
    );
}

/// A note deleted outside the process stops being reported.
///
/// The mirror of the create case, and the more damaging direction: an agent
/// acting on a search hit for a note that no longer exists gets a confusing
/// failure at read time instead of an honest empty result.
#[tokio::test]
async fn a_note_deleted_externally_leaves_the_index() {
    let (temp, server) = vault_with_warm_indexes().await;

    std::fs::remove_file(temp.path().join("a.md")).unwrap();
    tokio::time::sleep(PAST_THE_DEBOUNCE).await;

    assert_eq!(
        search(&server, "originaltoken").await,
        0,
        "a deleted note must not keep answering searches"
    );

    // Asserted as well as the search result because search results are checked
    // against disk on the way out, so that first assertion would hold even with
    // a stale index. Vault stats come straight off the link graph and are only
    // right if the deletion actually reached it.
    let context = call(&server, "get_vault_context", json!({})).await;
    assert_eq!(
        context["data"]["current_stats"]["total_notes"], 0,
        "the link graph must drop a note deleted underneath it: {context:?}"
    );
}

/// Nobody edited anything, so nothing may change underneath a reader.
///
/// The guard against the opposite failure: a sweep that mistook this process's
/// own writes, or an unchanged vault, for external activity would rebuild
/// indexes and republish change events on every read.
#[tokio::test]
async fn a_quiet_vault_stays_put() {
    let (_temp, server) = vault_with_warm_indexes().await;

    call(
        &server,
        "write_note",
        json!({"path": "c.md", "content": "# C\n\nownwritetoken\n"}),
    )
    .await;
    tokio::time::sleep(PAST_THE_DEBOUNCE).await;

    assert_eq!(
        search(&server, "ownwritetoken").await,
        1,
        "this process's own write must be searchable"
    );
    assert_eq!(
        search(&server, "originaltoken").await,
        1,
        "an untouched note must stay searchable"
    );
}
