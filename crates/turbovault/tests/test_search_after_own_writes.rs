//! Search answers for TurboVault's own writes straight away, on the default
//! (direct) backend, without rebuilding its index.
//!
//! Every write used to evict the cached search index after the manager's
//! change-listener had already updated it in place, so the next search rebuilt
//! the index from every note in the vault. That eviction also hid whether the
//! listener kept search current on its own. With it gone, these are the tests
//! that say it does: each write is followed immediately by a search, with no
//! wait for the freshness gate's debounce, so only the listener can have made
//! the answer right.

use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;

async fn call(server: &ObsidianMcpServer, name: &str, arguments: Value) -> Value {
    let result = server
        .call_tool(name, arguments, &RequestContext::new())
        .await
        .unwrap_or_else(|error| panic!("tool {name:?} failed: {error}"));
    assert!(
        !result.is_error(),
        "tool {name:?} returned an error: {}",
        result.first_text().unwrap_or("no text")
    );
    result
        .structured_content
        .unwrap_or_else(|| panic!("tool {name:?} returned no structured content"))
}

async fn search(server: &ObsidianMcpServer, query: &str) -> usize {
    call(server, "search", json!({"query": query})).await["count"]
        .as_u64()
        .unwrap_or(0) as usize
}

/// A direct-backend vault whose search index is already built and cached.
async fn warm_vault() -> (TempDir, ObsidianMcpServer) {
    let temp = TempDir::new().unwrap();
    let server = ObsidianMcpServer::new().unwrap();
    call(
        &server,
        "add_vault",
        json!({"name": "own-writes", "path": temp.path().to_string_lossy()}),
    )
    .await;
    call(
        &server,
        "write_note",
        json!({"path": "seed.md", "content": "# Seed\n\nseedtoken\n"}),
    )
    .await;
    assert_eq!(search(&server, "seedtoken").await, 1);
    (temp, server)
}

#[tokio::test]
async fn a_new_note_is_searchable_immediately() {
    let (_temp, server) = warm_vault().await;
    call(
        &server,
        "write_note",
        json!({"path": "new.md", "content": "# New\n\nfreshtoken\n"}),
    )
    .await;
    assert_eq!(search(&server, "freshtoken").await, 1);
}

#[tokio::test]
async fn an_edit_replaces_the_old_text_in_search() {
    let (_temp, server) = warm_vault().await;
    call(
        &server,
        "edit_note",
        json!({
            "path": "seed.md",
            "edits": "<<<<<<< SEARCH\nseedtoken\n=======\nedittoken\n>>>>>>> REPLACE\n"
        }),
    )
    .await;
    assert_eq!(search(&server, "edittoken").await, 1);
    assert_eq!(search(&server, "seedtoken").await, 0);
}

#[tokio::test]
async fn a_moved_note_is_found_at_its_new_path_only() {
    let (_temp, server) = warm_vault().await;
    call(
        &server,
        "move_note",
        json!({"from": "seed.md", "to": "moved/seed.md"}),
    )
    .await;
    let hits = call(&server, "search", json!({"query": "seedtoken"})).await;
    let paths: Vec<&str> = hits["data"]
        .as_array()
        .or_else(|| hits["results"].as_array())
        .map(|results| results.iter().filter_map(|r| r["path"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(paths, vec!["moved/seed.md"], "search response: {hits}");
}

#[tokio::test]
async fn a_deleted_note_leaves_search() {
    let (_temp, server) = warm_vault().await;
    call(
        &server,
        "delete_note",
        json!({"path": "seed.md", "confirm_path": "seed.md"}),
    )
    .await;
    assert_eq!(search(&server, "seedtoken").await, 0);
}
