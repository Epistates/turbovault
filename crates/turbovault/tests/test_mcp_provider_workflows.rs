//! Successful end-to-end workflows across the public MCP provider facade.
//!
//! These tests deliberately call tools by their stable public names. Lower-level
//! tool tests cover algorithms; this file protects provider composition, shared
//! state, argument decoding, and structured response contracts.

use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;

fn ctx() -> RequestContext {
    RequestContext::new()
}

async fn call(server: &ObsidianMcpServer, name: &str, arguments: Value) -> Value {
    let result = server
        .call_tool(name, arguments, &ctx())
        .await
        .unwrap_or_else(|error| panic!("public MCP tool {name:?} failed: {error}"));
    assert!(
        !result.is_error(),
        "public MCP tool {name:?} returned an error: {}",
        result
            .first_text()
            .unwrap_or("tool returned an error without text")
    );
    result
        .structured_content
        .unwrap_or_else(|| panic!("public MCP tool {name:?} returned no structured content"))
}

async fn call_error(server: &ObsidianMcpServer, name: &str, arguments: Value) -> String {
    match server.call_tool(name, arguments, &ctx()).await {
        Ok(result) if result.is_error() => result
            .first_text()
            .unwrap_or("tool returned an error without text")
            .to_string(),
        Ok(result) => panic!(
            "public MCP tool {name:?} unexpectedly succeeded: {:?}",
            result.structured_content
        ),
        Err(error) => error.to_string(),
    }
}

async fn registered_server(name: &str) -> (TempDir, ObsidianMcpServer) {
    let temp = TempDir::new().expect("temporary vault");
    let server = ObsidianMcpServer::new().expect("provider composition");

    let response = call(
        &server,
        "add_vault",
        json!({
            "name": name,
            "path": temp.path().to_string_lossy(),
        }),
    )
    .await;
    assert_eq!(response["success"], true);
    assert_eq!(response["vault"], name);

    (temp, server)
}

async fn write_note(server: &ObsidianMcpServer, path: &str, content: &str) -> Value {
    call(
        server,
        "write_note",
        json!({"path": path, "content": content}),
    )
    .await
}

const ALPHA: &str = r#"---
type: concept
status: draft
priority: 5
tags:
  - seed
---
# Alpha

Distributed systems coordinate independent nodes across unreliable networks.
Reliable consensus requires explicit failure handling and reproducible tests.

See [[beta]]. #architecture

# Citations

- Distributed Systems, third edition.
"#;

const BETA: &str = r#"---
type: concept
status: active
priority: 4
---
# Beta

Distributed systems coordinate machines while preserving useful guarantees.
Consensus protocols make failures observable and recovery predictable.

See [[alpha]].
"#;

const GAMMA: &str = r#"---
type: concept
status: draft
priority: 2
---
# Gamma

Operational runbooks describe repeatable recovery procedures for production.
Clear ownership reduces incident response time and prevents recurring failures.
"#;

async fn seed_analysis_vault(server: &ObsidianMcpServer) {
    for (path, content) in [("alpha.md", ALPHA), ("beta.md", BETA), ("gamma.md", GAMMA)] {
        let response = write_note(server, path, content).await;
        assert_eq!(response["success"], true, "failed to seed {path}");
    }
}

async fn seed_graph_vault(server: &ObsidianMcpServer) {
    let notes = [
        (
            "hub.md",
            "# Hub\n\n[[alpha]] [[beta]] [[leaf]] [[missing]]\n",
        ),
        ("alpha.md", "# Alpha\n\n[[hub]] [[beta]]\n"),
        ("beta.md", "# Beta\n\n[[hub]]\n"),
        ("leaf.md", "# Leaf\n"),
        ("islands/a.md", "# Island A\n\n[[islands/b]]\n"),
        ("islands/b.md", "# Island B\n\n[[islands/a]]\n"),
        ("orphan.md", "# Orphan\n"),
    ];

    for (path, content) in notes {
        let response = write_note(server, path, content).await;
        assert_eq!(response["success"], true, "failed to seed {path}");
    }
}

async fn seed_okf_vault(server: &ObsidianMcpServer) {
    let notes = [
        (
            "tables/orders.md",
            r#"---
type: table
title: Orders
description: Customer purchase records.
---
# Orders

Each order records a customer purchase and its current fulfillment state.
See [[customers]] for the owning customer.

# Schema

The primary key is the order identifier.

# Examples

An order can be pending or fulfilled.
"#,
        ),
        (
            "tables/customers.md",
            r#"---
type: table
title: Customers
description: Registered customer records.
---
# Customers

Each customer record identifies a registered account in the commerce system.

# Citations

- [Commerce data dictionary](https://example.com/data-dictionary)
"#,
        ),
        (
            "plain.md",
            "# Plain\n\nThis standalone document deliberately has no Open Knowledge Format type.\n",
        ),
    ];

    for (path, content) in notes {
        let response = write_note(server, path, content).await;
        assert_eq!(response["success"], true, "failed to seed {path}");
    }
}

async fn seed_relationship_vault(server: &ObsidianMcpServer) {
    let notes = [
        ("source.md", "# Source\n\n[[target]]\n"),
        ("target.md", "# Target\n\n[[source]]\n"),
        ("candidate-high.md", "# High candidate\n"),
        ("candidate-low.md", "# Low candidate\n"),
        (
            "references/ref-a.md",
            "# Reference A\n\n[[source]] [[target]] [[candidate-high]] [[candidate-low]]\n",
        ),
        (
            "references/ref-b.md",
            "# Reference B\n\n[[source]] [[candidate-high]]\n",
        ),
        ("orphan.md", "# Orphan\n"),
    ];

    for (path, content) in notes {
        let response = write_note(server, path, content).await;
        assert_eq!(response["success"], true, "failed to seed {path}");
    }
}

fn string_array_contains_path(value: &Value, suffix: &str) -> bool {
    value
        .as_array()
        .expect("path array")
        .iter()
        .filter_map(Value::as_str)
        .any(|path| path.ends_with(suffix))
}

#[tokio::test]
async fn metadata_discovery_and_export_work_through_the_public_facade() {
    let (_temp, server) = registered_server("metadata-export").await;
    seed_analysis_vault(&server).await;

    let value = call(
        &server,
        "get_metadata_value",
        json!({"file": "alpha.md", "key": "status"}),
    )
    .await;
    assert_eq!(value["data"]["value"], "draft");

    let update = call(
        &server,
        "update_frontmatter",
        json!({
            "path": "alpha.md",
            "frontmatter": {
                "status": "published",
                "owner": {"name": "Ada"}
            }
        }),
    )
    .await;
    assert_eq!(update["data"]["status"], "updated");
    assert_eq!(update["data"]["merge"], true);

    let nested = call(
        &server,
        "get_metadata_value",
        json!({"file": "alpha.md", "key": "owner.name"}),
    )
    .await;
    assert_eq!(nested["data"]["value"], "Ada");

    call(
        &server,
        "manage_tags",
        json!({
            "path": "alpha.md",
            "operation": "add",
            "tags": ["reviewed"]
        }),
    )
    .await;
    let tags = call(
        &server,
        "manage_tags",
        json!({"path": "alpha.md", "operation": "list"}),
    )
    .await;
    let all_tags = tags["data"]["all_tags"].as_array().expect("tag array");
    assert!(all_tags.contains(&json!("seed")));
    assert!(all_tags.contains(&json!("reviewed")));
    assert!(all_tags.contains(&json!("architecture")));

    let query = call(
        &server,
        "query_metadata",
        json!({"pattern": "status: \"published\""}),
    )
    .await;
    assert_eq!(query["count"], 1);
    assert_eq!(query["data"]["matched"], 1);
    assert_eq!(query["data"]["files"][0]["path"], "alpha.md");
    assert_eq!(query["meta"]["pattern"], "status: \"published\"");

    let no_matches = call(
        &server,
        "query_metadata",
        json!({"pattern": "status: \"missing\""}),
    )
    .await;
    assert_eq!(no_matches["count"], 0);
    assert_eq!(no_matches["data"]["matched"], 0);

    let invalid_query = call_error(
        &server,
        "query_metadata",
        json!({"pattern": "not a metadata query"}),
    )
    .await;
    assert!(invalid_query.contains("Unable to parse query pattern"));

    let invalid_frontmatter = call_error(
        &server,
        "update_frontmatter",
        json!({"path": "gamma.md", "frontmatter": ["not", "an", "object"]}),
    )
    .await;
    assert!(
        invalid_frontmatter.contains("map") || invalid_frontmatter.contains("object"),
        "unexpected schema error: {invalid_frontmatter}"
    );

    let replaced = call(
        &server,
        "update_frontmatter",
        json!({
            "path": "gamma.md",
            "frontmatter": {"status": "archived"},
            "merge": false
        }),
    )
    .await;
    assert_eq!(replaced["data"]["merge"], false);
    assert_eq!(replaced["data"]["keys_set"], json!(["status"]));
    let removed_key = call_error(
        &server,
        "get_metadata_value",
        json!({"file": "gamma.md", "key": "type"}),
    )
    .await;
    assert!(removed_key.contains("Key not found"));

    call(
        &server,
        "manage_tags",
        json!({
            "path": "alpha.md",
            "operation": "remove",
            "tags": ["reviewed"]
        }),
    )
    .await;
    let tags_after_remove = call(
        &server,
        "manage_tags",
        json!({"path": "alpha.md", "operation": "list"}),
    )
    .await;
    assert!(
        !tags_after_remove["data"]["all_tags"]
            .as_array()
            .expect("tag array after removal")
            .contains(&json!("reviewed"))
    );

    for (operation, expected) in [
        ("toggle", "Invalid tag operation"),
        ("add", "Tags array required"),
    ] {
        let error = call_error(
            &server,
            "manage_tags",
            json!({"path": "alpha.md", "operation": operation}),
        )
        .await;
        assert!(error.contains(expected), "unexpected tag error: {error}");
    }

    let search = call(
        &server,
        "search_by_frontmatter",
        json!({"key": "status", "value": "published"}),
    )
    .await;
    assert_eq!(search["count"], 1);

    let info = call(
        &server,
        "get_notes_info",
        json!({"paths": ["alpha.md", "beta.md", "../outside.md"]}),
    )
    .await;
    assert_eq!(info["count"], 3);
    assert_eq!(info["data"][0]["exists"], true);
    assert_eq!(info["data"][1]["exists"], true);
    assert_eq!(info["data"][2]["path"], "../outside.md");
    assert_eq!(info["data"][2]["exists"], false);

    for tool in [
        "export_health_report",
        "export_broken_links",
        "export_vault_stats",
        "export_analysis_report",
    ] {
        let export = call(&server, tool, json!({"format": "json"})).await;
        assert_eq!(export["data"]["format"], "json", "tool: {tool}");
        assert_eq!(export["meta"]["format"], "json", "tool: {tool}");
        let content = export["data"]["content"]
            .as_str()
            .unwrap_or_else(|| panic!("{tool} content should be a string"));
        serde_json::from_str::<Value>(content)
            .unwrap_or_else(|error| panic!("{tool} returned invalid JSON: {error}"));
    }
}

#[tokio::test]
async fn binary_file_move_enforces_confirmations_hashes_and_vault_boundaries() {
    let (temp, server) = registered_server("binary-move").await;
    let source = temp.path().join("attachments/blob.bin");
    tokio::fs::create_dir_all(source.parent().expect("attachment parent"))
        .await
        .expect("create attachment directory");
    let bytes = vec![0, 159, 146, 150, 255, 10];
    tokio::fs::write(&source, &bytes)
        .await
        .expect("write binary attachment");

    let bad_from = call_error(
        &server,
        "move_file",
        json!({
            "from": "attachments/blob.bin",
            "to": "assets/blob.bin",
            "confirm_from": "attachments/other.bin",
            "confirm_to": "assets/blob.bin"
        }),
    )
    .await;
    assert!(bad_from.contains("confirm_from"));
    assert!(source.is_file());

    let bad_to = call_error(
        &server,
        "move_file",
        json!({
            "from": "attachments/blob.bin",
            "to": "assets/blob.bin",
            "confirm_from": "attachments/blob.bin",
            "confirm_to": "assets/other.bin"
        }),
    )
    .await;
    assert!(bad_to.contains("confirm_to"));
    assert!(source.is_file());

    let stale = call_error(
        &server,
        "move_file",
        json!({
            "from": "attachments/blob.bin",
            "to": "assets/blob.bin",
            "confirm_from": "attachments/blob.bin",
            "confirm_to": "assets/blob.bin",
            "expected_hash": "0".repeat(64)
        }),
    )
    .await;
    assert!(stale.contains("File modified since last read"));
    assert!(source.is_file());

    let traversal = call_error(
        &server,
        "move_file",
        json!({
            "from": "attachments/blob.bin",
            "to": "../outside.bin",
            "confirm_from": "attachments/blob.bin",
            "confirm_to": "../outside.bin"
        }),
    )
    .await;
    assert!(traversal.to_lowercase().contains("traversal"));
    assert!(source.is_file());

    let moved = call(
        &server,
        "move_file",
        json!({
            "from": "attachments/blob.bin",
            "to": "assets/blob.bin",
            "confirm_from": "attachments/blob.bin",
            "confirm_to": "assets/blob.bin"
        }),
    )
    .await;
    assert_eq!(moved["data"]["from"], "attachments/blob.bin");
    assert_eq!(moved["data"]["to"], "assets/blob.bin");
    assert_eq!(moved["data"]["status"], "moved");
    assert!(!source.exists());
    assert_eq!(
        tokio::fs::read(temp.path().join("assets/blob.bin"))
            .await
            .expect("moved binary attachment"),
        bytes
    );

    let audit = call(
        &server,
        "audit_log",
        json!({"operation": "MOVE", "limit": 5}),
    )
    .await;
    assert_eq!(audit["count"], 1);
    assert_eq!(audit["data"][0]["path"], "attachments/blob.bin");
    assert_eq!(audit["data"][0]["new_path"], "assets/blob.bin");
}

#[tokio::test]
async fn file_lifecycle_and_concurrency_guards_work_through_the_public_facade() {
    let (_temp, server) = registered_server("file-workflow").await;
    let original = "---\ntitle: Work\n---\n# Work\n\nOriginal line.\n";

    let written = call(
        &server,
        "write_note",
        json!({"path": "notes/work.md", "content": original}),
    )
    .await;
    assert_eq!(written["data"]["status"], "written");
    assert_eq!(written["data"]["mode"], "overwrite");
    assert_eq!(written["data"]["bytes"], original.len());
    assert_eq!(
        written["next_steps"],
        json!(["read_note", "query_metadata"])
    );

    let first_read = call(&server, "read_note", json!({"path": "notes/work.md"})).await;
    assert_eq!(first_read["data"]["path"], "notes/work.md");
    assert_eq!(first_read["data"]["content"], original);
    assert_eq!(
        first_read["next_steps"],
        json!(["write_note", "get_backlinks"])
    );
    let initial_hash = first_read["data"]["hash"]
        .as_str()
        .expect("initial content hash")
        .to_string();
    assert_eq!(initial_hash.len(), 64);
    assert_eq!(
        first_read["data"]["uri"],
        "obsidian://open?vault=file-workflow&file=notes%2Fwork"
    );

    let appended = call(
        &server,
        "write_note",
        json!({
            "path": "notes/work.md",
            "content": "Appended line.",
            "mode": "append",
            "expected_hash": initial_hash,
        }),
    )
    .await;
    assert_eq!(appended["data"]["mode"], "append");
    assert_eq!(appended["data"]["bytes"], "Appended line.".len());

    let after_append = call(&server, "read_note", json!({"path": "notes/work.md"})).await;
    let appended_content = after_append["data"]["content"]
        .as_str()
        .expect("appended content");
    assert!(appended_content.ends_with("Original line.\n\nAppended line."));
    let appended_hash = after_append["data"]["hash"]
        .as_str()
        .expect("appended hash");

    call(
        &server,
        "write_note",
        json!({
            "path": "notes/work.md",
            "content": "Prepended line.",
            "mode": "prepend",
            "expected_hash": appended_hash,
        }),
    )
    .await;
    let after_prepend = call(&server, "read_note", json!({"path": "notes/work.md"})).await;
    let prepended_content = after_prepend["data"]["content"]
        .as_str()
        .expect("prepended content");
    assert!(prepended_content.starts_with("---\ntitle: Work\n---\nPrepended line.\n# Work"));
    assert!(prepended_content.ends_with("Appended line."));
    let pre_edit_hash = after_prepend["data"]["hash"]
        .as_str()
        .expect("pre-edit hash")
        .to_string();

    let edits = r#"<<<<<<< SEARCH
Original line.
=======
Updated line.
>>>>>>> REPLACE"#;
    let preview = call(
        &server,
        "edit_note",
        json!({
            "path": "notes/work.md",
            "edits": edits,
            "expected_hash": pre_edit_hash,
            "dry_run": true,
        }),
    )
    .await;
    assert_eq!(preview["data"]["success"], true);
    assert_eq!(preview["data"]["blocks_applied"], 1);
    assert!(preview["data"]["diff_preview"].is_string());
    assert_ne!(preview["data"]["old_hash"], preview["data"]["new_hash"]);
    let after_preview = call(&server, "read_note", json!({"path": "notes/work.md"})).await;
    assert!(
        after_preview["data"]["content"]
            .as_str()
            .expect("content after preview")
            .contains("Original line.")
    );

    let edited = call(
        &server,
        "edit_note",
        json!({
            "path": "notes/work.md",
            "edits": edits,
            "expected_hash": pre_edit_hash,
        }),
    )
    .await;
    assert_eq!(edited["data"]["success"], true);
    assert_eq!(edited["data"]["blocks_applied"], 1);
    assert!(edited["data"].get("diff_preview").is_none());
    assert_eq!(edited["next_steps"], json!(["read_note", "write_note"]));

    let after_edit = call(&server, "read_note", json!({"path": "notes/work.md"})).await;
    let edited_content = after_edit["data"]["content"]
        .as_str()
        .expect("edited content");
    assert!(edited_content.contains("Updated line."));
    assert!(!edited_content.contains("Original line."));
    let edited_hash = after_edit["data"]["hash"]
        .as_str()
        .expect("edited hash")
        .to_string();

    let moved = call(
        &server,
        "move_note",
        json!({
            "from": "notes/work.md",
            "to": "archive/work.md",
            "expected_hash": edited_hash,
        }),
    )
    .await;
    assert_eq!(moved["data"]["status"], "moved");
    assert_eq!(
        moved["next_steps"],
        json!(["get_backlinks", "get_forward_links"])
    );
    assert_eq!(
        moved["warnings"].as_array().expect("move warnings").len(),
        1
    );
    let archived = call(&server, "read_note", json!({"path": "archive/work.md"})).await;
    assert_eq!(archived["data"]["content"], edited_content);
    let archived_hash = archived["data"]["hash"]
        .as_str()
        .expect("archived hash")
        .to_string();

    let confirmation_error = call_error(
        &server,
        "delete_note",
        json!({
            "path": "archive/work.md",
            "confirm_path": "archive/other.md",
        }),
    )
    .await;
    assert!(confirmation_error.contains("Confirmation failed"));
    call(&server, "read_note", json!({"path": "archive/work.md"})).await;

    let stale_hash_error = call_error(
        &server,
        "delete_note",
        json!({
            "path": "archive/work.md",
            "confirm_path": "archive/work.md",
            "expected_hash": "0".repeat(64),
        }),
    )
    .await;
    assert!(stale_hash_error.contains("File modified since last read"));

    let deleted = call(
        &server,
        "delete_note",
        json!({
            "path": "archive/work.md",
            "confirm_path": "archive/work.md",
            "expected_hash": archived_hash,
        }),
    )
    .await;
    assert_eq!(deleted["data"]["status"], "deleted");
    assert_eq!(deleted["next_steps"], json!(["quick_health_check"]));
    let missing = call_error(&server, "read_note", json!({"path": "archive/work.md"})).await;
    assert!(missing.contains("No such file") || missing.contains("not found"));
}

#[tokio::test]
async fn file_provider_rejects_invalid_modes_stale_writes_and_unsafe_paths() {
    let (_temp, server) = registered_server("file-errors").await;
    write_note(&server, "guarded.md", "# Guarded\n").await;

    let invalid_mode = call_error(
        &server,
        "write_note",
        json!({
            "path": "guarded.md",
            "content": "replacement",
            "mode": "merge",
        }),
    )
    .await;
    assert!(invalid_mode.contains("Invalid write mode"));

    let stale_write = call_error(
        &server,
        "write_note",
        json!({
            "path": "guarded.md",
            "content": "replacement",
            "expected_hash": "0".repeat(64),
        }),
    )
    .await;
    assert!(stale_write.contains("File modified since last read"));

    for (tool, arguments) in [
        ("read_note", json!({"path": "../../outside.md"})),
        (
            "move_note",
            json!({"from": "guarded.md", "to": "../../outside.md"}),
        ),
    ] {
        let error = call_error(&server, tool, arguments).await;
        assert!(
            error.to_ascii_lowercase().contains("traversal"),
            "unexpected {tool} error: {error}"
        );
    }

    let intact = call(&server, "read_note", json!({"path": "guarded.md"})).await;
    assert_eq!(intact["data"]["content"], "# Guarded\n");
}

#[tokio::test]
async fn batch_preconditions_abort_before_public_side_effects() {
    let (temp, server) = registered_server("batch-preconditions").await;
    write_note(&server, "guarded.md", "current").await;
    let guarded = call(&server, "read_note", json!({"path": "guarded.md"})).await;
    let current_hash = guarded["data"]["hash"]
        .as_str()
        .expect("guarded hash")
        .to_string();

    let updated = call(
        &server,
        "batch_execute",
        json!({
            "operations": [{
                "type": "WriteNote",
                "path": "guarded.md",
                "content": "updated",
                "expected_hash": current_hash,
            }]
        }),
    )
    .await;
    assert_eq!(updated["success"], true);
    assert_eq!(updated["data"]["executed"], 1);

    // Since M4d the batch routes through `manager.apply_changes`, which aborts
    // the whole plan atomically on a stale per-op precondition (nothing
    // written) and reports it as a soft `success: false` BatchResult (R10 wire
    // shape preserved).
    let stale = call(
        &server,
        "batch_execute",
        json!({
            "operations": [
                {
                    "type": "CreateNote",
                    "path": "side-effect.md",
                    "content": "must not exist"
                },
                {
                    "type": "WriteNote",
                    "path": "guarded.md",
                    "content": "clobbered",
                    "expected_hash": "0".repeat(64)
                }
            ]
        }),
    )
    .await;
    assert_eq!(stale["success"], false);
    assert_eq!(stale["data"]["executed"], 0);
    assert!(
        stale["data"]["errors"][0]
            .as_str()
            .expect("precondition error")
            .contains("modified since last read")
    );
    assert!(!temp.path().join("side-effect.md").exists());
    assert_eq!(
        call(&server, "read_note", json!({"path": "guarded.md"})).await["data"]["content"],
        "updated"
    );
}

#[tokio::test]
async fn vault_lifecycle_works_through_the_public_facade() {
    let root = TempDir::new().expect("vault lifecycle root");
    let created_path = root.path().join("created");
    let existing_path = root.path().join("existing");
    tokio::fs::create_dir_all(existing_path.join(".obsidian"))
        .await
        .expect("existing vault structure");
    tokio::fs::write(existing_path.join("existing.md"), "# Existing\n")
        .await
        .expect("existing note");
    let server = ObsidianMcpServer::new().expect("provider composition");

    let empty = call(&server, "list_vaults", json!({})).await;
    assert_eq!(empty["count"], 0);
    assert_eq!(empty["data"], json!([]));
    let no_active = call(&server, "get_active_vault", json!({})).await;
    assert_eq!(no_active["data"]["active_vault"], "");

    let created = call(
        &server,
        "create_vault",
        json!({
            "name": "created",
            "path": created_path.to_string_lossy(),
            "template": "research",
        }),
    )
    .await;
    assert_eq!(created["vault"], "created");
    assert_eq!(created["data"]["name"], "created");
    assert_eq!(created["data"]["is_default"], true);
    assert_eq!(
        created["next_steps"],
        json!(["set_active_vault", "list_vaults"])
    );
    assert!(created_path.join(".obsidian").is_dir());
    for directory in ["Literature", "Theory", "Findings", "Hypotheses"] {
        assert!(created_path.join(directory).is_dir(), "missing {directory}");
    }

    let first_active = call(&server, "get_active_vault", json!({})).await;
    assert_eq!(first_active["data"]["active_vault"], "created");
    let config = call(&server, "get_vault_config", json!({"name": "created"})).await;
    assert_eq!(config["data"]["name"], "created");
    let canonical_created = created_path.canonicalize().expect("created canonical path");
    let configured_path = std::path::PathBuf::from(
        config["data"]["path"]
            .as_str()
            .expect("configured vault path"),
    )
    .canonicalize()
    .expect("configured canonical path");
    assert_eq!(configured_path, canonical_created);

    let ready = write_note(&server, "created.md", "# Created vault is ready\n").await;
    assert_eq!(ready["vault"], "created");

    let added = call(
        &server,
        "add_vault",
        json!({
            "name": "existing",
            "path": existing_path.to_string_lossy(),
        }),
    )
    .await;
    assert_eq!(added["vault"], "existing");
    assert_eq!(added["data"]["name"], "existing");
    assert_eq!(added["data"]["is_default"], false);

    let listed = call(&server, "list_vaults", json!({})).await;
    assert_eq!(listed["count"], 2);
    let names = listed["data"]
        .as_array()
        .expect("vault list")
        .iter()
        .filter_map(|vault| vault["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"created"));
    assert!(names.contains(&"existing"));

    let duplicate = call_error(
        &server,
        "add_vault",
        json!({
            "name": "existing",
            "path": existing_path.to_string_lossy(),
        }),
    )
    .await;
    assert!(duplicate.contains("already registered"));

    let activated = call(&server, "set_active_vault", json!({"name": "existing"})).await;
    assert_eq!(activated["data"]["status"], "activated");
    assert_eq!(
        activated["next_steps"],
        json!(["get_vault_context", "quick_health_check"])
    );
    let second_active = call(&server, "get_active_vault", json!({})).await;
    assert_eq!(second_active["data"]["active_vault"], "existing");
    let existing_note = call(&server, "read_note", json!({"path": "existing.md"})).await;
    assert_eq!(existing_note["data"]["content"], "# Existing\n");

    let removed_active = call(&server, "remove_vault", json!({"name": "existing"})).await;
    assert_eq!(removed_active["data"]["status"], "removed");
    assert_eq!(removed_active["next_steps"], json!(["list_vaults"]));
    let reassigned = call(&server, "get_active_vault", json!({})).await;
    assert_eq!(reassigned["data"]["active_vault"], "created");
    let created_note = call(&server, "read_note", json!({"path": "created.md"})).await;
    assert_eq!(
        created_note["data"]["content"],
        "# Created vault is ready\n"
    );

    call(&server, "remove_vault", json!({"name": "created"})).await;
    assert_eq!(call(&server, "list_vaults", json!({})).await["count"], 0);
    assert_eq!(
        call(&server, "get_active_vault", json!({})).await["data"]["active_vault"],
        ""
    );

    for (tool, arguments) in [
        ("remove_vault", json!({"name": "created"})),
        ("get_vault_config", json!({"name": "created"})),
        ("set_active_vault", json!({"name": "created"})),
    ] {
        let error = call_error(&server, tool, arguments).await;
        assert!(
            error.contains("not found"),
            "unexpected {tool} error: {error}"
        );
    }

    assert!(created_path.join("created.md").is_file());
    assert!(existing_path.join("existing.md").is_file());
}

#[tokio::test]
async fn create_vault_rejects_bad_inputs_before_touching_the_filesystem() {
    let root = TempDir::new().expect("invalid vault root");
    let server = ObsidianMcpServer::new().expect("provider composition");

    let invalid_template_path = root.path().join("invalid-template");
    let invalid_template = call_error(
        &server,
        "create_vault",
        json!({
            "name": "invalid-template",
            "path": invalid_template_path.to_string_lossy(),
            "template": "zettelkasten",
        }),
    )
    .await;
    assert!(invalid_template.contains("Unknown template"));
    assert!(!invalid_template_path.exists());

    for name in ["", "contains spaces", &"x".repeat(65)] {
        let path = root.path().join("invalid-name");
        let error = call_error(
            &server,
            "create_vault",
            json!({"name": name, "path": path.to_string_lossy()}),
        )
        .await;
        assert!(error.contains("Vault name"));
        assert!(!path.exists());
    }

    let file_path = root.path().join("not-a-directory");
    tokio::fs::write(&file_path, "not a vault")
        .await
        .expect("file fixture");
    let not_directory = call_error(
        &server,
        "create_vault",
        json!({"name": "file", "path": file_path.to_string_lossy()}),
    )
    .await;
    assert!(not_directory.contains("not a directory"));
}

#[tokio::test]
async fn templates_render_notes_and_preserve_template_provenance() {
    let (_temp, server) = registered_server("templates").await;

    let templates = call(&server, "list_templates", json!({})).await;
    assert_eq!(templates["count"], 3);
    let template_ids = templates["data"]
        .as_array()
        .expect("template array")
        .iter()
        .filter_map(|template| template["id"].as_str())
        .collect::<Vec<_>>();
    assert!(template_ids.contains(&"doc"));
    assert!(template_ids.contains(&"task"));
    assert!(template_ids.contains(&"research"));

    let doc_template = call(&server, "get_template", json!({"template_id": "doc"})).await;
    assert_eq!(doc_template["data"]["name"], "Documentation");
    assert_eq!(doc_template["data"]["category"], "documentation");
    assert_eq!(
        doc_template["data"]["fields"]
            .as_array()
            .expect("template fields")
            .len(),
        3
    );

    let created = call(
        &server,
        "create_from_template",
        json!({
            "template_id": "doc",
            "file_path": "guides/authentication.md",
            "fields": json!({
                "title": "Authentication Guide",
                "summary": "Explains the production authentication flow."
            }).to_string()
        }),
    )
    .await;
    assert_eq!(created["data"]["path"], "guides/authentication.md");
    assert_eq!(created["data"]["template_id"], "doc");
    assert_eq!(created["data"]["title"], "Authentication Guide");

    let rendered = call(
        &server,
        "read_note",
        json!({"path": "guides/authentication.md"}),
    )
    .await;
    let content = rendered["data"]["content"]
        .as_str()
        .expect("rendered template content");
    assert!(content.contains("template: doc"));
    assert!(content.contains("title: Authentication Guide"));
    assert!(content.contains("# Authentication Guide"));
    assert!(content.contains("Explains the production authentication flow."));

    let provenance = call(
        &server,
        "get_metadata_value",
        json!({"file": "guides/authentication.md", "key": "template"}),
    )
    .await;
    assert_eq!(provenance["data"]["value"], "doc");

    let matches = call(
        &server,
        "find_notes_from_template",
        json!({"template_id": "doc"}),
    )
    .await;
    assert_eq!(matches["count"], 1);
    assert!(
        matches["data"][0]
            .as_str()
            .expect("template match path")
            .ends_with("guides/authentication.md")
    );
}

#[tokio::test]
#[cfg(feature = "sql")]
async fn advanced_discovery_recommendations_and_sql_return_real_results() {
    let (_temp, server) = registered_server("advanced-discovery").await;
    seed_analysis_vault(&server).await;

    let filtered = call(
        &server,
        "advanced_search",
        json!({
            "query": "distributed systems",
            "tags": ["seed"],
            "frontmatter_filters": [
                {"key": "status", "value": "draft"},
                {"key": "type", "value": "concept"}
            ],
            "exclude_paths": ["gamma.md"],
            "limit": 5
        }),
    )
    .await;
    assert_eq!(filtered["count"], 1);
    assert!(
        filtered["data"][0]["path"]
            .as_str()
            .expect("advanced-search path")
            .ends_with("alpha.md")
    );
    assert_eq!(filtered["data"][0]["tags"], json!(["seed"]));

    let recommendations = call(&server, "recommend_related", json!({"path": "alpha.md"})).await;
    assert!(
        recommendations["count"]
            .as_u64()
            .expect("recommendation count")
            >= 1
    );
    assert!(
        recommendations["data"]
            .as_array()
            .expect("recommendation array")
            .iter()
            .all(|result| !result["path"]
                .as_str()
                .unwrap_or_default()
                .ends_with("alpha.md"))
    );

    let schema = call(&server, "inspect_frontmatter", json!({})).await;
    assert_eq!(schema["data"]["file_count"], 3);
    assert_eq!(schema["data"]["schema"]["status"]["type"], "string");
    assert_eq!(schema["data"]["schema"]["priority"]["type"], "number");
    assert!(schema["data"]["tables"]["files"].is_string());
    assert!(schema["data"]["tables"]["tags"].is_string());
    assert!(schema["data"]["tables"]["links"].is_string());

    let sql = call(
        &server,
        "query_frontmatter_sql",
        json!({
            "sql": "SELECT path, status, priority FROM files WHERE status = 'draft' ORDER BY path"
        }),
    )
    .await;
    assert_eq!(sql["data"]["file_count"], 3);
    assert_eq!(sql["data"]["tag_count"], 1);
    assert_eq!(sql["data"]["link_count"], 2);
    assert_eq!(sql["data"]["result"]["count"], 2);
    let rows = sql["data"]["result"]["rows"].as_array().expect("SQL rows");
    assert_eq!(rows[0]["path"], "alpha.md");
    assert_eq!(rows[0]["priority"], 5);
    assert_eq!(rows[1]["path"], "gamma.md");
    assert_eq!(rows[1]["priority"], 2);
}

#[tokio::test]
async fn graph_navigation_and_topology_work_through_the_public_facade() {
    let (_temp, server) = registered_server("graph-topology").await;
    seed_graph_vault(&server).await;

    let backlinks = call(&server, "get_backlinks", json!({"path": "beta.md"})).await;
    assert_eq!(backlinks["count"], 2);
    assert!(string_array_contains_path(&backlinks["data"], "hub.md"));
    assert!(string_array_contains_path(&backlinks["data"], "alpha.md"));
    assert_eq!(
        backlinks["next_steps"],
        json!(["get_forward_links", "get_related_notes"])
    );

    let no_backlinks = call(&server, "get_backlinks", json!({"path": "orphan.md"})).await;
    assert_eq!(no_backlinks["count"], 0);
    assert_eq!(
        no_backlinks["warnings"],
        json!(["Note has no incoming links"])
    );

    let forward = call(&server, "get_forward_links", json!({"path": "hub.md"})).await;
    assert_eq!(forward["count"], 3);
    for path in ["alpha.md", "beta.md", "leaf.md"] {
        assert!(string_array_contains_path(&forward["data"], path));
    }

    let related = call(
        &server,
        "get_related_notes",
        json!({"path": "hub.md", "max_hops": 99}),
    )
    .await;
    assert_eq!(related["meta"]["max_hops"], 5);
    assert_eq!(related["count"], 3);

    let default_related = call(&server, "get_related_notes", json!({"path": "hub.md"})).await;
    assert_eq!(default_related["meta"]["max_hops"], 2);

    let hubs = call(&server, "get_hub_notes", json!({"top_n": 1})).await;
    assert_eq!(hubs["count"], 1);
    assert!(
        hubs["data"][0][0]
            .as_str()
            .expect("hub path")
            .ends_with("hub.md")
    );
    assert_eq!(hubs["data"][0][1], 5);

    let dead_ends = call(&server, "get_dead_end_notes", json!({})).await;
    assert_eq!(dead_ends["count"], 1);
    assert!(string_array_contains_path(&dead_ends["data"], "leaf.md"));

    let clusters = call(&server, "get_isolated_clusters", json!({})).await;
    assert_eq!(clusters["count"], 1);
    let island = clusters["data"][0].as_array().expect("isolated cluster");
    assert!(
        island
            .iter()
            .filter_map(Value::as_str)
            .any(|path| path.ends_with("islands/a.md"))
    );
    assert!(
        island
            .iter()
            .filter_map(Value::as_str)
            .any(|path| path.ends_with("islands/b.md"))
    );

    let broken = call(&server, "get_broken_links", json!({})).await;
    assert_eq!(broken["count"], 1);
    assert_eq!(broken["data"][0]["target"], "missing");
    assert!(
        broken["data"][0]["source_file"]
            .as_str()
            .expect("broken-link source")
            .ends_with("hub.md")
    );
    assert_eq!(broken["warnings"], json!(["Found 1 broken links"]));
    assert_eq!(broken["next_steps"], json!(["export_broken_links"]));

    let cycles = call(&server, "detect_cycles", json!({})).await;
    assert!(cycles["count"].as_u64().expect("cycle count") >= 2);
    assert_eq!(cycles["warnings"], json!(["Cycles detected in link graph"]));
    assert_eq!(cycles["next_steps"], json!(["get_broken_links"]));
}

#[tokio::test]
async fn graph_health_and_explanation_return_actionable_public_responses() {
    let (temp, server) = registered_server("graph-health").await;
    seed_graph_vault(&server).await;

    let quick = call(&server, "quick_health_check", json!({})).await;
    assert_eq!(quick["data"]["total_notes"], 7);
    assert_eq!(quick["data"]["total_links"], 8);
    assert_eq!(quick["data"]["broken_links_count"], 1);
    assert_eq!(quick["data"]["orphaned_notes_count"], 1);
    assert_eq!(quick["next_steps"][0], "full_health_analysis");

    let full = call(&server, "full_health_analysis", json!({})).await;
    assert_eq!(full["data"]["total_notes"], 7);
    assert_eq!(full["data"]["broken_links_count"], 1);
    assert_eq!(full["data"]["orphaned_notes_count"], 1);
    assert_eq!(full["data"]["dead_end_notes_count"], 1);
    assert_eq!(full["meta"]["analysis_type"], "comprehensive");
    assert_eq!(
        full["next_steps"],
        json!(["get_broken_links", "suggest_links"])
    );

    let explanation = call(&server, "explain_vault", json!({})).await;
    assert_eq!(explanation["data"]["quick_facts"]["total_files"], 7);
    assert_eq!(explanation["data"]["quick_facts"]["total_links"], 8);
    assert_eq!(
        explanation["data"]["structure"]["file_count_by_folder"]["root"],
        5
    );
    assert_eq!(
        explanation["data"]["structure"]["file_count_by_folder"]["islands"],
        2
    );
    let folders = explanation["data"]["structure"]["folders"]
        .as_array()
        .expect("folder list");
    assert!(folders.contains(&json!("root")));
    assert!(folders.contains(&json!("islands")));
    assert!(
        !explanation
            .to_string()
            .contains(&temp.path().to_string_lossy().to_string()),
        "public explanation should not expose the absolute vault path"
    );
    assert_eq!(explanation["meta"]["view_type"], "holistic_gestalt");
    assert_eq!(
        explanation["next_steps"],
        json!(["get_dead_end_notes", "get_broken_links"])
    );
}

#[tokio::test]
async fn analysis_and_relationship_tools_share_the_seeded_vault() {
    let (_temp, server) = registered_server("analysis-relationship").await;
    seed_analysis_vault(&server).await;

    let diff = call(
        &server,
        "diff_notes",
        json!({"left": "alpha.md", "right": "beta.md"}),
    )
    .await;
    assert_eq!(diff["success"], true);
    assert!(diff["data"].is_object());

    let quality = call(
        &server,
        "evaluate_note_quality",
        json!({"path": "alpha.md"}),
    )
    .await;
    assert_eq!(quality["success"], true);
    assert!(quality["data"].is_object());

    let report = call(&server, "vault_quality_report", json!({"bottom_n": 2})).await;
    assert_eq!(report["count"], 3);

    let grounding = call(
        &server,
        "analyze_note_grounding",
        json!({"path": "gamma.md"}),
    )
    .await;
    assert_eq!(grounding["success"], true);
    assert!(grounding["data"].is_object());

    let semantic = call(
        &server,
        "semantic_search",
        json!({"query": "distributed systems consensus", "limit": 3}),
    )
    .await;
    assert!(semantic["count"].as_u64().expect("semantic count") >= 1);

    let similar = call(
        &server,
        "find_similar_notes",
        json!({"path": "alpha.md", "limit": 2}),
    )
    .await;
    assert!(similar["count"].as_u64().expect("similar-note count") >= 1);

    let duplicates = call(
        &server,
        "find_duplicates",
        json!({"threshold": 0.1, "limit": 5}),
    )
    .await;
    assert!(duplicates["data"].is_array());

    let comparison = call(
        &server,
        "compare_notes",
        json!({"left": "alpha.md", "right": "beta.md"}),
    )
    .await;
    assert!(comparison["data"].is_object());

    let strength = call(
        &server,
        "get_link_strength",
        json!({"source": "alpha.md", "target": "beta.md"}),
    )
    .await;
    assert_eq!(strength["data"]["source"], "alpha.md");
    assert_eq!(strength["data"]["target"], "beta.md");
    assert!(strength["data"]["strength"].is_object());

    let ranking = call(&server, "get_centrality_ranking", json!({})).await;
    assert_eq!(ranking["success"], true);
    assert!(ranking["data"].is_object());
}

#[tokio::test]
async fn relationship_scoring_and_suggestions_use_the_public_graph_contract() {
    let (temp, server) = registered_server("relationship-contract").await;
    seed_relationship_vault(&server).await;

    let strength = call(
        &server,
        "get_link_strength",
        json!({"source": "source.md", "target": "target.md"}),
    )
    .await;
    assert_eq!(strength["data"]["source"], "source.md");
    assert_eq!(strength["data"]["target"], "target.md");
    assert_eq!(strength["data"]["strength"]["strength"], 1.0);
    assert_eq!(
        strength["data"]["strength"]["components"],
        json!({
            "direct_links": 1,
            "backlinks": 1,
            "shared_references": 1
        })
    );
    assert_eq!(strength["meta"]["metric"], "link_strength");

    let suggestions = call(
        &server,
        "suggest_links",
        json!({"file": "source.md", "limit": 1}),
    )
    .await;
    assert_eq!(suggestions["count"], 1);
    assert_eq!(suggestions["meta"]["limit"], 1);
    assert_eq!(suggestions["data"]["file"], "source.md");
    assert_eq!(
        suggestions["data"]["suggestions"][0]["target"],
        "candidate-high.md"
    );
    assert_eq!(suggestions["data"]["suggestions"][0]["strength"], 0.6);

    let default_limit = call(&server, "suggest_links", json!({"file": "source.md"})).await;
    assert_eq!(default_limit["count"], 2);
    assert_eq!(default_limit["meta"]["limit"], 5);

    let negative_limit = call(
        &server,
        "suggest_links",
        json!({"file": "source.md", "limit": -1}),
    )
    .await;
    assert_eq!(negative_limit["count"], 0);
    assert_eq!(negative_limit["meta"]["limit"], 0);
    assert!(
        negative_limit["data"]["suggestions"]
            .as_array()
            .expect("negative-limit suggestions")
            .is_empty()
    );

    let ranking = call(&server, "get_centrality_ranking", json!({})).await;
    assert_eq!(ranking["count"], 7);
    assert_eq!(ranking["data"]["total_files"], 7);
    assert_eq!(
        ranking["meta"]["metrics"],
        json!(["betweenness", "closeness", "eigenvector"])
    );
    assert!(
        ranking["data"]["rankings"]
            .as_array()
            .expect("centrality rankings")
            .iter()
            .all(|entry| {
                !entry["file"]
                    .as_str()
                    .expect("ranked file")
                    .starts_with(temp.path().to_str().expect("temporary vault path"))
            })
    );

    for (tool, arguments) in [
        (
            "get_link_strength",
            json!({"source": "../outside.md", "target": "target.md"}),
        ),
        (
            "suggest_links",
            json!({"file": "../outside.md", "limit": 5}),
        ),
    ] {
        let error = call_error(&server, tool, arguments).await;
        assert!(
            error.to_lowercase().contains("traversal"),
            "unexpected {tool} error: {error}"
        );
    }

    let (_empty_temp, empty_server) = registered_server("relationship-empty").await;
    let empty_ranking = call(&empty_server, "get_centrality_ranking", json!({})).await;
    assert_eq!(empty_ranking["count"], 0);
    assert_eq!(empty_ranking["data"]["total_files"], 0);
    let empty_suggestions = call(
        &empty_server,
        "suggest_links",
        json!({"file": "missing.md"}),
    )
    .await;
    assert_eq!(empty_suggestions["count"], 0);
    assert_eq!(empty_suggestions["meta"]["limit"], 5);
}

#[tokio::test]
async fn okf_maintenance_grounding_visualization_and_staleness_are_public_workflows() {
    let (temp, server) = registered_server("okf-analysis").await;
    seed_okf_vault(&server).await;

    let ungrounded = call(&server, "find_ungrounded_notes", json!({"limit": 1})).await;
    assert_eq!(ungrounded["data"]["total_notes"], 3);
    assert_eq!(ungrounded["data"]["ungrounded_count"], 2);
    assert_eq!(ungrounded["count"], 2);
    assert_eq!(
        ungrounded["data"]["notes"]
            .as_array()
            .expect("ungrounded notes")
            .len(),
        1
    );

    let validation = call(&server, "okf_validate", json!({})).await;
    assert_eq!(validation["success"], false);
    assert_eq!(validation["count"], 3);
    assert_eq!(validation["data"]["total"], 3);
    assert_eq!(validation["data"]["conformant"], 2);
    assert_eq!(validation["data"]["non_conformant"], 1);
    assert_eq!(validation["data"]["type_distribution"]["table"], 2);
    assert_eq!(
        validation["data"]["non_conformant_paths"],
        json!(["plain.md"])
    );

    let subtree = call(&server, "okf_validate", json!({"subtree": "tables"})).await;
    assert_eq!(subtree["success"], true);
    assert_eq!(subtree["data"]["total"], 2);
    assert_eq!(subtree["data"]["non_conformant"], 0);

    let preview = call(
        &server,
        "generate_index",
        json!({"recursive": true, "dry_run": true}),
    )
    .await;
    assert_eq!(preview["data"]["dry_run"], true);
    assert_eq!(preview["count"], 2);
    assert!(
        preview["data"]["indexes"]
            .as_array()
            .expect("index previews")
            .iter()
            .all(|index| index["written"] == false)
    );
    assert!(!temp.path().join("index.md").exists());
    assert!(!temp.path().join("tables/index.md").exists());

    let generated = call(&server, "generate_index", json!({"recursive": true})).await;
    assert_eq!(generated["data"]["dry_run"], false);
    assert_eq!(generated["count"], 2);
    assert!(
        generated["data"]["indexes"]
            .as_array()
            .expect("generated indexes")
            .iter()
            .all(|index| index["written"] == true)
    );
    assert!(temp.path().join("index.md").is_file());
    assert!(temp.path().join("tables/index.md").is_file());

    let unchanged = call(&server, "generate_index", json!({"recursive": true})).await;
    assert!(
        unchanged["data"]["indexes"]
            .as_array()
            .expect("unchanged indexes")
            .iter()
            .all(|index| index["written"] == false)
    );

    let first_log = call(
        &server,
        "append_log_entry",
        json!({
            "directory": "tables",
            "kind": "Creation",
            "text": "Established the tables index.",
            "date": "2026-07-16"
        }),
    )
    .await;
    assert_eq!(first_log["data"]["path"], "tables/log.md");
    assert_eq!(first_log["data"]["created_file"], true);
    assert_eq!(first_log["data"]["created_section"], true);

    let second_log = call(
        &server,
        "append_log_entry",
        json!({
            "directory": "tables",
            "text": "Added customer ownership details.",
            "date": "2026-07-16"
        }),
    )
    .await;
    assert_eq!(second_log["data"]["created_file"], false);
    assert_eq!(second_log["data"]["created_section"], false);
    let log = tokio::fs::read_to_string(temp.path().join("tables/log.md"))
        .await
        .expect("generated OKF log");
    assert!(log.contains("* **Creation**: Established the tables index."));
    assert!(log.contains("* **Update**: Added customer ownership details."));

    let bad_date = call_error(
        &server,
        "append_log_entry",
        json!({"text": "Invalid date", "date": "07/16/2026"}),
    )
    .await;
    assert!(bad_date.contains("expected ISO YYYY-MM-DD"));

    let visualization = call(
        &server,
        "visualize",
        json!({"output": "reports/graph.html", "name": "Analysis Bundle"}),
    )
    .await;
    assert_eq!(visualization["data"]["output"], "reports/graph.html");
    assert_eq!(visualization["data"]["summary"]["name"], "Analysis Bundle");
    assert_eq!(visualization["data"]["summary"]["nodes"], 6);
    assert!(
        visualization["data"]["summary"]["html_bytes"]
            .as_u64()
            .expect("HTML byte count")
            > 1_000
    );
    let html = tokio::fs::read_to_string(temp.path().join("reports/graph.html"))
        .await
        .expect("generated visualization");
    assert!(html.contains("Analysis Bundle"));
    assert!(!html.contains(&temp.path().to_string_lossy().to_string()));

    let traversal = call_error(&server, "visualize", json!({"output": "../outside.html"})).await;
    assert!(traversal.to_lowercase().contains("traversal"));
    assert!(!temp.path().parent().unwrap().join("outside.html").exists());

    let stale = call(
        &server,
        "find_stale_notes",
        json!({"threshold_days": 0, "limit": 1}),
    )
    .await;
    assert_eq!(stale["count"], 1);
    let stale_path = stale["data"][0]["path"].as_str().expect("stale-note path");
    assert!(!stale_path.starts_with(temp.path().to_str().unwrap()));
}

#[tokio::test]
async fn audit_preview_diff_and_rollback_restore_the_previous_note() {
    let (_temp, server) = registered_server("audit-rollback").await;
    let original = "# Release plan\n\nVersion one keeps the safe rollout.\n";
    let updated = "# Release plan\n\nVersion two removes the safe rollout.\n";

    write_note(&server, "release.md", original).await;
    // Overwriting an existing note is a create-by-default refusal since M4d
    // (strict-create on both backends); pass force to make it a blind UPDATE.
    call(
        &server,
        "write_note",
        json!({"path": "release.md", "content": updated, "force": true}),
    )
    .await;

    let log = call(
        &server,
        "audit_log",
        json!({
            "path": "release.md",
            "operation": "UPDATE",
            "limit": 5
        }),
    )
    .await;
    assert_eq!(log["count"], 1);
    let update = &log["data"][0];
    assert_eq!(update["operation"], "UPDATE");
    let operation_id = update["id"].as_str().expect("audit operation ID");

    let version_diff = call(
        &server,
        "diff_note_version",
        json!({"path": "release.md", "operation_id": operation_id}),
    )
    .await;
    assert_eq!(version_diff["success"], true);
    assert!(version_diff["data"].is_object());

    let preview = call(
        &server,
        "rollback_preview",
        json!({"operation_id": operation_id}),
    )
    .await;
    assert_eq!(preview["data"]["operation"], "UPDATE");
    assert_eq!(preview["data"]["would_modify"], true);
    assert!(preview["data"]["diff_preview"].is_string());

    let rollback = call(
        &server,
        "rollback_note",
        json!({"operation_id": operation_id}),
    )
    .await;
    assert_eq!(rollback["data"]["success"], true);
    assert_eq!(rollback["data"]["path"], "release.md");

    let restored = call(&server, "read_note", json!({"path": "release.md"})).await;
    assert_eq!(restored["data"]["content"], original);

    let rollback_log = call(
        &server,
        "audit_log",
        json!({"operation": "ROLLBACK", "limit": 5}),
    )
    .await;
    assert_eq!(rollback_log["count"], 1);
    assert_eq!(
        rollback_log["data"][0]["metadata"]["rolled_back_operation"],
        operation_id
    );

    let stats = call(&server, "audit_stats", json!({})).await;
    assert!(
        stats["data"]["total_operations"]
            .as_u64()
            .expect("audit operation count")
            >= 3
    );
}

#[tokio::test]
async fn audit_create_delete_and_unsupported_rollbacks_keep_public_state_consistent() {
    let (_temp, server) = registered_server("audit-operation-matrix").await;
    write_note(&server, "target.md", "# Target\n").await;
    write_note(&server, "created.md", "# Created\n\n[[target]]\n").await;

    let creates = call(
        &server,
        "audit_log",
        json!({"path": "created.md", "operation": "create", "limit": 5}),
    )
    .await;
    assert_eq!(creates["count"], 1);
    let create_id = creates["data"][0]["id"]
        .as_str()
        .expect("create operation ID")
        .to_string();

    let zero_limit = call(&server, "audit_log", json!({"limit": 0})).await;
    assert_eq!(zero_limit["count"], 0);
    assert!(zero_limit["data"].as_array().unwrap().is_empty());

    let create_preview = call(
        &server,
        "rollback_preview",
        json!({"operation_id": create_id}),
    )
    .await;
    assert_eq!(create_preview["data"]["operation"], "CREATE");
    assert_eq!(create_preview["data"]["would_delete"], true);
    assert!(create_preview.get("warnings").is_none());

    let create_rollback = call(&server, "rollback_note", json!({"operation_id": create_id})).await;
    assert!(
        create_rollback["data"]["action_taken"]
            .as_str()
            .expect("create rollback action")
            .contains("Deleted file")
    );
    let missing_created = call_error(&server, "read_note", json!({"path": "created.md"})).await;
    assert!(missing_created.contains("not found") || missing_created.contains("No such file"));
    let health_after_delete = call(&server, "quick_health_check", json!({})).await;
    assert_eq!(health_after_delete["data"]["total_notes"], 1);

    write_note(&server, "deleted.md", "# Deleted\n\n[[target]]\n").await;
    call(
        &server,
        "delete_note",
        json!({"path": "deleted.md", "confirm_path": "deleted.md"}),
    )
    .await;
    let deletes = call(
        &server,
        "audit_log",
        json!({"path": "deleted.md", "operation": "DELETE", "limit": 5}),
    )
    .await;
    assert_eq!(deletes["count"], 1);
    let delete_id = deletes["data"][0]["id"]
        .as_str()
        .expect("delete operation ID")
        .to_string();

    let delete_preview = call(
        &server,
        "rollback_preview",
        json!({"operation_id": delete_id}),
    )
    .await;
    assert_eq!(delete_preview["data"]["operation"], "DELETE");
    assert_eq!(delete_preview["data"]["would_create"], true);
    call(&server, "rollback_note", json!({"operation_id": delete_id})).await;
    let restored = call(&server, "read_note", json!({"path": "deleted.md"})).await;
    assert_eq!(restored["data"]["content"], "# Deleted\n\n[[target]]\n");
    let restored_links = call(&server, "get_forward_links", json!({"path": "deleted.md"})).await;
    assert_eq!(restored_links["count"], 1);
    assert!(string_array_contains_path(
        &restored_links["data"],
        "target.md"
    ));
    let health_after_restore = call(&server, "quick_health_check", json!({})).await;
    assert_eq!(health_after_restore["data"]["total_notes"], 2);

    write_note(&server, "move.md", "# Move\n").await;
    call(
        &server,
        "move_note",
        json!({"from": "move.md", "to": "moved.md"}),
    )
    .await;
    let moves = call(
        &server,
        "audit_log",
        json!({"path": "move.md", "operation": "MOVE", "limit": 5}),
    )
    .await;
    assert_eq!(moves["count"], 1);
    let move_id = moves["data"][0]["id"]
        .as_str()
        .expect("move operation ID")
        .to_string();
    let move_preview = call(
        &server,
        "rollback_preview",
        json!({"operation_id": move_id}),
    )
    .await;
    assert_eq!(move_preview["data"]["would_modify"], false);
    assert_eq!(
        move_preview["warnings"],
        json!(["Move rollback not yet supported"])
    );
    let unsupported_move =
        call_error(&server, "rollback_note", json!({"operation_id": move_id})).await;
    assert!(unsupported_move.contains("MOVE operations is not supported"));

    let rollback_entries = call(
        &server,
        "audit_log",
        json!({"operation": "ROLLBACK", "limit": 5}),
    )
    .await;
    assert_eq!(rollback_entries["count"], 2);
    let rollback_id = rollback_entries["data"][0]["id"]
        .as_str()
        .expect("rollback operation ID");
    let rollback_preview = call(
        &server,
        "rollback_preview",
        json!({"operation_id": rollback_id}),
    )
    .await;
    assert_eq!(
        rollback_preview["warnings"],
        json!(["Cannot roll back a rollback operation"])
    );

    let unknown_operation = call_error(&server, "audit_log", json!({"operation": "COPY"})).await;
    assert!(unknown_operation.contains("Unknown operation type"));
    for tool in ["rollback_preview", "rollback_note"] {
        let missing = call_error(&server, tool, json!({"operation_id": "missing-operation"})).await;
        assert!(missing.contains("Audit entry not found"));
    }

    let stats = call(&server, "audit_stats", json!({})).await;
    assert_eq!(stats["data"]["total_operations"], 8);
    assert_eq!(stats["data"]["operations_by_type"]["CREATE"], 4);
    assert_eq!(stats["data"]["operations_by_type"]["DELETE"], 1);
    assert_eq!(stats["data"]["operations_by_type"]["MOVE"], 1);
    assert_eq!(stats["data"]["operations_by_type"]["ROLLBACK"], 2);
}
