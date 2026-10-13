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

/// A cancellation the client already sent.
#[derive(Debug)]
struct Cancelled;

impl turbomcp_core::session::Cancellable for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn slow_tools_report_progress_to_a_client_that_asks() {
    let (_vault, server) = server_with_vault().await;
    for tool in [
        "explain_vault",
        "full_health_analysis",
        "vault_quality_report",
    ] {
        let session = RecordingSession::plain();
        let ctx = context(&session).with_progress_token(json!(format!("{tool}-token")));
        let result = server.call_tool(tool, json!({}), &ctx).await.unwrap();
        assert!(!result.is_error(), "{tool}");

        let progress = session.sent("notifications/progress");
        assert!(progress.len() >= 2, "{tool}: {progress:?}");
        let values: Vec<f64> = progress
            .iter()
            .map(|params| params["progress"].as_f64().unwrap())
            .collect();
        assert!(
            values.windows(2).all(|pair| pair[0] < pair[1]),
            "{tool}: progress only increases: {values:?}"
        );
        let last = progress.last().unwrap();
        assert_eq!(
            last["progress"], last["total"],
            "{tool}: the last update completes"
        );
        assert_eq!(last["progressToken"], json!(format!("{tool}-token")));
    }

    // No token, no progress.
    let session = RecordingSession::plain();
    server
        .call_tool("explain_vault", json!({}), &context(&session))
        .await
        .unwrap();
    assert!(session.sent("notifications/progress").is_empty());
}

#[tokio::test]
async fn a_cancelled_call_stops_before_it_changes_anything() {
    let (vault, server) = server_with_vault().await;
    let session = RecordingSession::plain();
    let ctx = context(&session).with_cancellation_token(Arc::new(Cancelled));

    let explained = server.call_tool("explain_vault", json!({}), &ctx).await;
    let cancelled = |result: Result<turbomcp::ToolResult, turbomcp::McpError>| match result {
        Ok(result) => {
            result.is_error()
                && result
                    .first_text()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains("cancel")
        }
        Err(error) => error.to_string().to_lowercase().contains("cancel"),
    };
    assert!(cancelled(explained));

    let batch = server
        .call_tool(
            "batch_execute",
            json!({"operations": [{"type": "CreateNote", "path": "Batch.md", "content": "# B\n"}]}),
            &ctx,
        )
        .await;
    assert!(cancelled(batch));
    assert!(
        !vault.path().join("Batch.md").exists(),
        "a cancelled batch writes nothing"
    );
}

fn delete_args(path: &str) -> Value {
    json!({"path": path, "confirm_path": path})
}

#[tokio::test]
async fn a_client_without_elicitation_deletes_as_before() {
    let (vault, server) = server_with_vault().await;
    let session = RecordingSession::plain();
    let result = server
        .call_tool("delete_note", delete_args("Notes.md"), &context(&session))
        .await
        .unwrap();
    assert!(
        !result.is_error(),
        "{}",
        result.first_text().unwrap_or_default()
    );
    assert!(!vault.path().join("Notes.md").exists());
    assert!(
        session.requests.lock().unwrap().is_empty(),
        "nobody is asked"
    );
}

#[tokio::test]
async fn the_user_confirms_a_delete_through_the_client() {
    let (vault, server) = server_with_vault().await;
    let session =
        RecordingSession::eliciting(json!({"action": "accept", "content": {"confirm": true}}));
    let result = server
        .call_tool("delete_note", delete_args("Notes.md"), &context(&session))
        .await
        .unwrap();
    assert!(
        !result.is_error(),
        "{}",
        result.first_text().unwrap_or_default()
    );
    assert!(!vault.path().join("Notes.md").exists());
    let requests = session.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "elicitation/create");
    assert!(
        requests[0].1["message"]
            .as_str()
            .unwrap()
            .contains("Notes.md")
    );
}

#[tokio::test]
async fn a_declined_or_unconfirmed_delete_changes_nothing() {
    for reply in [
        json!({"action": "decline"}),
        json!({"action": "cancel"}),
        json!({"action": "accept", "content": {"confirm": false}}),
    ] {
        let (vault, server) = server_with_vault().await;
        let session = RecordingSession::eliciting(reply.clone());
        let result = server
            .call_tool("delete_note", delete_args("Notes.md"), &context(&session))
            .await;
        let refused = match result {
            Ok(result) => result.is_error(),
            Err(_) => true,
        };
        assert!(refused, "{reply}");
        assert!(vault.path().join("Notes.md").exists(), "{reply}");
    }
}

#[tokio::test]
async fn read_only_tools_never_ask() {
    let (_vault, server) = server_with_vault().await;
    let session =
        RecordingSession::eliciting(json!({"action": "accept", "content": {"confirm": true}}));
    let ctx = context(&session);
    for (tool, arguments) in [
        ("read_note", json!({"path": "Notes.md"})),
        ("search", json!({"query": "plan"})),
        ("explain_vault", json!({})),
    ] {
        let result = server.call_tool(tool, arguments, &ctx).await.unwrap();
        assert!(!result.is_error(), "{tool}");
    }
    assert!(session.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_replacing_move_asks_and_a_plain_one_does_not() {
    let (vault, server) = server_with_vault().await;
    let session =
        RecordingSession::eliciting(json!({"action": "accept", "content": {"confirm": true}}));
    let ctx = context(&session);

    let result = server
        .call_tool(
            "move_note",
            json!({"from": "projects/Plan.md", "to": "projects/Moved.md"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(
        !result.is_error(),
        "{}",
        result.first_text().unwrap_or_default()
    );
    assert!(
        session.requests.lock().unwrap().is_empty(),
        "nothing replaced"
    );

    let result = server
        .call_tool(
            "move_note",
            json!({"from": "projects/Moved.md", "to": "Notes.md", "dest_expected_hash": "blind"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(
        !result.is_error(),
        "{}",
        result.first_text().unwrap_or_default()
    );
    assert_eq!(session.requests.lock().unwrap().len(), 1, "replacing asks");
    assert!(
        std::fs::read_to_string(vault.path().join("Notes.md"))
            .unwrap()
            .starts_with("# Plan")
    );
}

#[tokio::test]
async fn confirmation_can_be_turned_off() {
    let (vault, server) = server_with_vault().await;
    server.set_confirm_destructive(false);
    let session = RecordingSession::eliciting(json!({"action": "decline"}));
    let result = server
        .call_tool("delete_note", delete_args("Notes.md"), &context(&session))
        .await
        .unwrap();
    assert!(!result.is_error());
    assert!(!vault.path().join("Notes.md").exists());
    assert!(session.requests.lock().unwrap().is_empty());
}
