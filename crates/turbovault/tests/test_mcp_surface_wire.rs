//! Resources, prompts and completion over the wire: the real binary, spawned
//! with a vault and driven over stdio by `turbomcp-client`, so the router's
//! handling of `resources/*`, `prompts/*` and `completion/complete` (paging,
//! capability negotiation, serialization) is exercised end to end.

use std::collections::HashMap;

use serde_json::json;
use tempfile::TempDir;
use turbomcp_client::Client;
use turbomcp_transport::child_process::{ChildProcessConfig, ChildProcessTransport};

async fn connect() -> (TempDir, TempDir, Client<ChildProcessTransport>) {
    let vault = TempDir::new().unwrap();
    std::fs::create_dir_all(vault.path().join("projects")).unwrap();
    std::fs::write(vault.path().join("projects/Plan.md"), "# Plan\n").unwrap();
    std::fs::write(vault.path().join("My Notes.md"), "# Notes\n").unwrap();
    let cache = TempDir::new().unwrap();

    let config = ChildProcessConfig {
        command: env!("CARGO_BIN_EXE_turbovault").to_string(),
        args: vec![
            "--vault".to_string(),
            vault.path().display().to_string(),
            "--init".to_string(),
        ],
        // Keep the spawned server's vault registry out of the user's cache.
        environment: Some(vec![(
            "XDG_CACHE_HOME".to_string(),
            cache.path().display().to_string(),
        )]),
        kill_on_drop: true,
        ..Default::default()
    };
    let client = Client::new(ChildProcessTransport::new(config));
    client.initialize().await.expect("initialize over stdio");
    (vault, cache, client)
}

#[tokio::test]
#[serial_test::serial]
async fn wire_notes_are_resources_and_prompts_complete() {
    let (_vault, _cache, client) = connect().await;

    let resources = serde_json::to_value(client.list_resources().await.unwrap()).unwrap();
    let uris: Vec<&str> = resources
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|resource| resource["uri"].as_str())
        .filter(|uri| uri.starts_with("obsidian://note/"))
        .collect();
    assert_eq!(uris.len(), 2, "{resources}");
    let plan = uris
        .iter()
        .find(|uri| uri.ends_with("/projects/Plan.md"))
        .expect("Plan.md is listed")
        .to_string();
    assert!(
        uris.iter().any(|uri| uri.ends_with("/My%20Notes.md")),
        "a space is escaped: {uris:?}"
    );

    let read = serde_json::to_value(client.read_resource(&plan).await.unwrap()).unwrap();
    assert_eq!(read["contents"][0]["text"], "# Plan\n", "{read}");
    assert_eq!(read["contents"][0]["mimeType"], "text/markdown");

    let templates = serde_json::to_value(client.list_resource_templates().await.unwrap()).unwrap();
    assert!(
        templates
            .as_array()
            .unwrap()
            .iter()
            .any(|template| template["uriTemplate"] == "obsidian://note/{vault}/{+path}"),
        "{templates}"
    );

    let prompts = serde_json::to_value(client.list_prompts().await.unwrap()).unwrap();
    assert!(
        prompts
            .as_array()
            .unwrap()
            .iter()
            .any(|prompt| prompt["name"] == "summarize_note"),
        "{prompts}"
    );
    let mut arguments = HashMap::new();
    arguments.insert("path".to_string(), json!("projects/Plan.md"));
    let prompt = serde_json::to_value(
        client
            .get_prompt("summarize_note", Some(arguments))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        prompt["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("projects/Plan.md"),
        "{prompt}"
    );

    let completed = serde_json::to_value(
        client
            .complete_prompt("summarize_note", "path", "proj", None)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        completed["completion"]["values"],
        json!(["projects/Plan.md"]),
        "{completed}"
    );

    let subscribed: Result<_, _> = client.subscribe(&plan).await;
    assert!(
        subscribed.is_ok(),
        "subscribing is supported: {subscribed:?}"
    );
}
