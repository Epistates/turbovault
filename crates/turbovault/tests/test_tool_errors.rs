//! A failed tool call tells the model what went wrong and what to do next.
//!
//! The error's kind travels in the result's `_meta`, so a client can tell a
//! missing note from bad input from a server fault; the text adds a next
//! step where there is one; and no absolute server path reaches the model.

use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;

/// A failed call's text, error kind and error data.
struct Failure {
    text: String,
    kind: Value,
    data: Value,
}

async fn call_failure(server: &ObsidianMcpServer, name: &str, arguments: Value) -> Failure {
    let result = server
        .call_tool(name, arguments, &RequestContext::new())
        .await
        .unwrap_or_else(|error| panic!("{name} should fail inside its result, got {error}"));
    assert!(result.is_error(), "{name} unexpectedly succeeded");
    let meta = result.meta.clone().unwrap_or_default();
    Failure {
        text: result.first_text().unwrap_or_default().to_string(),
        kind: meta
            .get("io.turbomcp/errorKind")
            .cloned()
            .unwrap_or(Value::Null),
        data: meta
            .get("io.turbomcp/errorData")
            .cloned()
            .unwrap_or(Value::Null),
    }
}

async fn call(server: &ObsidianMcpServer, name: &str, arguments: Value) -> Value {
    let result = server
        .call_tool(name, arguments, &RequestContext::new())
        .await
        .expect("tool call");
    assert!(
        !result.is_error(),
        "{name} failed: {:?}",
        result.first_text()
    );
    result.structured_content.expect("structured content")
}

async fn vault_with_notes(notes: &[(&str, &str)]) -> (TempDir, ObsidianMcpServer) {
    let temp = TempDir::new().unwrap();
    for (path, content) in notes {
        let full = temp.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    let server = ObsidianMcpServer::new().unwrap();
    call(
        &server,
        "add_vault",
        json!({"name": "errors", "path": temp.path().to_string_lossy()}),
    )
    .await;
    (temp, server)
}

fn assert_no_server_path(failure: &Failure, temp: &TempDir) {
    let root = temp.path().to_string_lossy().to_string();
    let real = temp
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    for text in [failure.text.clone(), failure.data.to_string()] {
        assert!(
            !text.contains(&root) && !text.contains(&real),
            "absolute vault path leaked: {text}"
        );
    }
}

#[tokio::test]
async fn a_missing_note_is_not_found_with_close_matches() {
    let (temp, server) = vault_with_notes(&[
        ("projects/Plan.md", "# Plan\n"),
        ("projects/Plans archive.md", "# Old\n"),
        ("Unrelated.md", "# Other\n"),
    ])
    .await;

    let failure = call_failure(&server, "read_note", json!({"path": "Plan.md"})).await;
    assert_eq!(failure.kind, "resource_not_found");
    assert_eq!(failure.data["category"], "not_found");
    assert_eq!(failure.data["path"], "Plan.md");
    assert_eq!(failure.data["suggestions"][0], "projects/Plan.md");
    assert!(
        !failure.data["suggestions"]
            .as_array()
            .unwrap()
            .contains(&json!("Unrelated.md"))
    );
    assert!(
        failure.text.contains("Did you mean: projects/Plan.md"),
        "{}",
        failure.text
    );
    assert!(failure.text.contains("Use search"), "{}", failure.text);
    assert_no_server_path(&failure, &temp);

    let typo = call_failure(&server, "read_note", json!({"path": "projects/Plam.md"})).await;
    assert_eq!(typo.data["suggestions"][0], "projects/Plan.md");
}

#[tokio::test]
async fn a_protected_path_says_what_lives_there() {
    let (temp, server) = vault_with_notes(&[(".obsidian/app.json", "{}")]).await;

    let failure = call_failure(&server, "read_note", json!({"path": ".obsidian/app.json"})).await;
    assert_eq!(failure.kind, "permission_denied");
    assert_eq!(failure.data["category"], "permission_denied");
    assert!(
        failure.text.contains("Obsidian's settings and plugin code"),
        "{}",
        failure.text
    );
    assert_no_server_path(&failure, &temp);
}

#[tokio::test]
async fn a_path_leaving_the_vault_is_invalid_input() {
    let (temp, server) = vault_with_notes(&[]).await;

    let failure = call_failure(&server, "read_note", json!({"path": "../outside.md"})).await;
    assert_eq!(failure.kind, "invalid_params");
    assert_eq!(failure.data["category"], "invalid_input");
    assert!(
        failure.text.contains("relative to the vault root"),
        "{}",
        failure.text
    );
    assert_no_server_path(&failure, &temp);
}

#[tokio::test]
async fn a_stale_hash_says_to_read_again() {
    let (_temp, server) = vault_with_notes(&[("note.md", "first\n")]).await;

    let failure = call_failure(
        &server,
        "write_note",
        json!({"path": "note.md", "content": "second\n", "expected_hash": "0".repeat(64)}),
    )
    .await;
    assert_eq!(failure.kind, "invalid_params");
    assert_eq!(failure.data["category"], "conflict");
    assert!(failure.text.contains("Read it again"), "{}", failure.text);
}

#[tokio::test]
async fn no_active_vault_says_how_to_choose_one() {
    let server = ObsidianMcpServer::new().unwrap();

    let failure = call_failure(&server, "read_note", json!({"path": "note.md"})).await;
    assert_eq!(failure.kind, "unavailable");
    assert!(
        failure.text.contains("set_active_vault"),
        "{}",
        failure.text
    );
}
