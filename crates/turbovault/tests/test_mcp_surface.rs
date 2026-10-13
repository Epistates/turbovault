//! The MCP surface beyond tools: notes as resources and the notifications
//! about them, workflow prompts, and completion. Called through the server's
//! `McpHandler`, the methods the router dispatches `resources/*`,
//! `prompts/*` and `completion/complete` to, with a session that records what
//! the server sends its client.

mod mcp_session;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;

use mcp_session::RecordingSession;

const UPDATED: &str = "notifications/resources/updated";
const LIST_CHANGED: &str = "notifications/resources/list_changed";

async fn server_with_vault() -> (TempDir, ObsidianMcpServer) {
    let vault = TempDir::new().unwrap();
    std::fs::create_dir_all(vault.path().join("projects")).unwrap();
    std::fs::write(
        vault.path().join("projects/Plan.md"),
        "# Plan\n\nSee [[Notes]].\n",
    )
    .unwrap();
    std::fs::write(vault.path().join("Notes.md"), "# Notes\n").unwrap();
    std::fs::write(vault.path().join("picture.png"), [0x89, b'P', b'N', b'G']).unwrap();
    std::fs::create_dir_all(vault.path().join(".obsidian")).unwrap();
    std::fs::write(vault.path().join(".obsidian/hidden.md"), "# secret\n").unwrap();

    let server = ObsidianMcpServer::new().unwrap();
    call(
        &server,
        "add_vault",
        json!({"name": "main", "path": vault.path().to_string_lossy()}),
    )
    .await;
    (vault, server)
}

async fn call(server: &ObsidianMcpServer, name: &str, arguments: Value) -> Value {
    let result = server
        .call_tool(name, arguments, &RequestContext::new())
        .await
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    assert!(
        !result.is_error(),
        "{name}: {}",
        result.first_text().unwrap_or_default()
    );
    result.structured_content.unwrap()
}

fn context(session: &Arc<RecordingSession>) -> RequestContext {
    RequestContext::new()
        .with_session(session.clone() as Arc<dyn turbomcp_core::session::McpSession>)
}

#[tokio::test]
async fn notes_are_listed_and_read_as_resources() {
    let (_vault, server) = server_with_vault().await;
    let session = RecordingSession::plain();

    let uris: Vec<String> = server
        .list_resources_for(&context(&session))
        .into_iter()
        .map(|resource| resource.uri)
        .collect();
    assert!(
        uris.contains(&"obsidian://note/main/projects/Plan.md".to_string()),
        "{uris:?}"
    );
    assert!(
        uris.contains(&"obsidian://note/main/Notes.md".to_string()),
        "{uris:?}"
    );
    assert!(
        !uris
            .iter()
            .any(|uri| uri.contains("picture.png") || uri.contains(".obsidian")),
        "only notes outside protected folders are resources: {uris:?}"
    );
    // The fixed guides are still there.
    assert!(uris.contains(&"obsidian://syntax/quick-ref".to_string()));

    let templates = server.list_resource_templates();
    assert_eq!(templates[0].uri_template, "obsidian://note/{vault}/{+path}");

    let read = server
        .read_resource(
            "obsidian://note/main/projects/Plan.md",
            &RequestContext::new(),
        )
        .await
        .unwrap();
    let text = read.first_text().unwrap();
    assert!(text.starts_with("# Plan"), "{text}");
    let contents = serde_json::to_value(&read.contents[0]).unwrap();
    assert_eq!(contents["mimeType"], "text/markdown");
}

#[tokio::test]
async fn reading_a_note_resource_keeps_read_notes_rules() {
    let (_vault, server) = server_with_vault().await;
    let ctx = RequestContext::new();
    for uri in [
        "obsidian://note/main/.obsidian/hidden.md",
        "obsidian://note/main/picture.png",
        "obsidian://note/main/missing.md",
        "obsidian://note/elsewhere/Notes.md",
        "obsidian://note/main/../outside.md",
    ] {
        assert!(
            server.read_resource(uri, &ctx).await.is_err(),
            "{uri} must not be readable"
        );
    }
}

#[tokio::test]
async fn a_subscriber_hears_about_writes_and_listers_about_new_notes() {
    let (_vault, server) = server_with_vault().await;
    let session = RecordingSession::plain();
    let ctx = context(&session);
    server.list_resources_for(&ctx);
    server
        .subscribe("obsidian://note/main/Notes.md", &ctx)
        .await
        .unwrap();

    let read = call(&server, "read_note", json!({"path": "Notes.md"})).await;
    let hash = read["data"]["hash"].as_str().unwrap().to_string();
    call(
        &server,
        "write_note",
        json!({"path": "Notes.md", "content": "# Notes\n\nMore.\n", "expected_hash": hash}),
    )
    .await;
    assert!(
        session
            .wait_for(UPDATED, Duration::from_secs(3), |params| {
                params["uri"] == "obsidian://note/main/Notes.md"
            })
            .await,
        "the subscriber is told the note changed: {:?}",
        session.notifications.lock().unwrap()
    );

    call(
        &server,
        "write_note",
        json!({"path": "New.md", "content": "# New\n"}),
    )
    .await;
    assert!(
        session
            .wait_for(LIST_CHANGED, Duration::from_secs(3), |_| true)
            .await,
        "a lister is told a note appeared"
    );
    // Only the subscribed note's updates are sent.
    assert!(
        session
            .sent(UPDATED)
            .iter()
            .all(|params| params["uri"] == "obsidian://note/main/Notes.md")
    );

    server
        .unsubscribe("obsidian://note/main/Notes.md", &ctx)
        .await
        .unwrap();
    let before = session.sent(UPDATED).len();
    let read = call(&server, "read_note", json!({"path": "Notes.md"})).await;
    let hash = read["data"]["hash"].as_str().unwrap().to_string();
    call(
        &server,
        "write_note",
        json!({"path": "Notes.md", "content": "# Notes\n\nAgain.\n", "expected_hash": hash}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(session.sent(UPDATED).len(), before, "unsubscribed");
}

#[tokio::test]
async fn a_subscriber_hears_about_edits_made_outside_turbovault() {
    let (vault, server) = server_with_vault().await;
    let session = RecordingSession::plain();
    server
        .subscribe("obsidian://note/main/projects/Plan.md", &context(&session))
        .await
        .unwrap();

    // Another editor saves the note; no tool call follows.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::write(
        vault.path().join("projects/Plan.md"),
        "# Plan\n\nEdited in Obsidian, longer than before.\n",
    )
    .unwrap();
    assert!(
        session
            .wait_for(UPDATED, Duration::from_secs(10), |params| {
                params["uri"] == "obsidian://note/main/projects/Plan.md"
            })
            .await,
        "the background sweep notices the edit"
    );
}

#[tokio::test]
async fn subscribing_to_something_that_is_not_a_resource_fails() {
    let (_vault, server) = server_with_vault().await;
    let session = RecordingSession::plain();
    assert!(
        server
            .subscribe("file:///etc/passwd", &context(&session))
            .await
            .is_err()
    );
    assert!(
        server
            .subscribe("obsidian://note/nope/a.md", &context(&session))
            .await
            .is_err(),
        "an unregistered vault"
    );
}

#[tokio::test]
async fn search_results_link_each_note_as_a_resource() {
    let (_vault, server) = server_with_vault().await;
    let result = server
        .call_tool("search", json!({"query": "plan"}), &RequestContext::new())
        .await
        .unwrap();
    let value = serde_json::to_value(&result).unwrap();
    let links: Vec<&Value> = value["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|content| content["type"] == "resource_link")
        .collect();
    assert!(!links.is_empty(), "{value}");
    assert_eq!(links[0]["uri"], "obsidian://note/main/projects/Plan.md");
    // The JSON text still comes first.
    assert_eq!(value["content"][0]["type"], "text");
}

#[tokio::test]
async fn workflow_prompts_list_and_build() {
    let (_vault, server) = server_with_vault().await;
    let names: Vec<String> = server.list_prompts().into_iter().map(|p| p.name).collect();
    for name in [
        "triage_vault",
        "repair_broken_links",
        "review_period",
        "summarize_note",
        "ground_draft",
    ] {
        assert!(names.contains(&name.to_string()), "{name}: {names:?}");
    }

    let prompt = server
        .get_prompt(
            "summarize_note",
            Some(json!({"path": "projects/Plan.md"})),
            &RequestContext::new(),
        )
        .await
        .unwrap();
    let text = prompt.messages[0].content.as_text().unwrap();
    assert!(
        text.contains("projects/Plan.md") && text.contains("get_backlinks"),
        "{text}"
    );

    assert!(
        server
            .get_prompt("summarize_note", None, &RequestContext::new())
            .await
            .is_err(),
        "a required argument is missing"
    );
}

#[tokio::test]
async fn prompt_arguments_and_note_template_variables_complete() {
    let (_vault, server) = server_with_vault().await;
    let ctx = RequestContext::new();
    let values = |result: Value| -> Vec<String> {
        result["completion"]["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect()
    };

    let periods = server
        .complete(
            json!({"ref": {"type": "ref/prompt", "name": "review_period"},
                   "argument": {"name": "period", "value": "w"}}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(values(periods), ["weekly"]);

    let paths = server
        .complete(
            json!({"ref": {"type": "ref/prompt", "name": "summarize_note"},
                   "argument": {"name": "path", "value": "proj"}}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(values(paths), ["projects/Plan.md"]);

    let folders = server
        .complete(
            json!({"ref": {"type": "ref/prompt", "name": "review_period"},
                   "argument": {"name": "folder", "value": ""}}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(values(folders), ["projects"]);

    let vaults = server
        .complete(
            json!({"ref": {"type": "ref/resource", "uri": "obsidian://note/{vault}/{+path}"},
                   "argument": {"name": "vault", "value": "m"}}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(values(vaults), ["main"]);

    let notes = server
        .complete(
            json!({"ref": {"type": "ref/resource", "uri": "obsidian://note/{vault}/{+path}"},
                   "argument": {"name": "path", "value": "N"},
                   "context": {"arguments": {"vault": "main"}}}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(values(notes), ["Notes.md"]);
}
