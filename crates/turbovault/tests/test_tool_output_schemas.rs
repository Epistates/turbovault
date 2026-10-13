//! Every tool advertises an outputSchema, and what it returns conforms to it.
//!
//! The schemas are derived from the Rust types the tools return, so most
//! drift is a compile error. Some results are still assembled as JSON (the
//! SQL engine, the metadata and relationship reports), and the envelope's
//! optional fields depend on what each call finds. This calls every tool on a
//! real vault and validates each result, so a schema cannot quietly stop
//! describing what its tool returns.

mod output_schema;

use std::collections::BTreeSet;

use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;

use output_schema::assert_matches_output_schema;

/// Calls tools and records which ones returned a result that matched.
struct Checked<'a> {
    server: &'a ObsidianMcpServer,
    seen: BTreeSet<String>,
}

impl Checked<'_> {
    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        let result = self
            .server
            .call_tool(name, arguments, &RequestContext::new())
            .await
            .unwrap_or_else(|error| panic!("{name} failed: {error}"));
        assert!(
            !result.is_error(),
            "{name} returned an error: {}",
            result.first_text().unwrap_or_default()
        );
        let structured = result
            .structured_content
            .clone()
            .unwrap_or_else(|| panic!("{name} returned no structured content"));
        assert_matches_output_schema(self.server, name, &structured);

        // The text block is the same result, compact.
        let text = result.first_text().expect("a text block");
        assert!(
            !text.contains("\n  "),
            "{name}: the text block is pretty-printed"
        );
        assert_eq!(
            serde_json::from_str::<Value>(text).expect("the text block is JSON"),
            structured,
            "{name}: the text block and structuredContent differ"
        );

        self.seen.insert(name.to_string());
        structured
    }
}

const ALPHA: &str = "---\ntype: concept\nstatus: draft\npriority: 5\ntags:\n  - seed\n---\n# Alpha\n\nDistributed systems coordinate independent nodes across unreliable networks.\n\nSee [[beta]] and [[missing]]. #architecture\n\n- [ ] follow up\n\n# Citations\n\n- Distributed Systems, third edition.\n";
const BETA: &str = "---\ntype: concept\nstatus: active\npriority: 4\n---\n# Beta\n\nDistributed systems coordinate machines while preserving useful guarantees.\n\nSee [[alpha]] and [[gamma]].\n";
const GAMMA: &str = "---\ntype: guide\nstatus: draft\n---\n# Gamma\n\nOperational runbooks describe repeatable recovery procedures.\n\nBack to [[beta]].\n";

fn git_repo(path: &std::path::Path) {
    let mut options = git2::RepositoryInitOptions::new();
    options.initial_head("main");
    let repo = git2::Repository::init_opts(path, &options).unwrap();
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let signature = git2::Signature::now("Init", "init@example").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
        .unwrap();
}

#[test]
fn every_tool_advertises_an_object_output_schema() {
    let server = ObsidianMcpServer::new().expect("provider composition");
    for tool in server.list_tools() {
        let schema = tool
            .output_schema
            .unwrap_or_else(|| panic!("{} advertises no outputSchema", tool.name));
        let schema = serde_json::to_value(schema).unwrap();
        assert_eq!(schema["type"], "object", "{}", tool.name);
        jsonschema::validator_for(&schema)
            .unwrap_or_else(|error| panic!("{}: invalid outputSchema: {error}", tool.name));
    }
}

#[tokio::test]
#[serial_test::serial]
async fn every_tool_result_matches_its_output_schema() {
    let vault = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let git_vault = TempDir::new().unwrap();
    git_repo(git_vault.path());
    std::fs::write(vault.path().join("picture.png"), [0x89, b'P', b'N', b'G']).unwrap();

    let server = ObsidianMcpServer::new().expect("provider composition");
    let mut checked = Checked {
        server: &server,
        seen: BTreeSet::new(),
    };

    // ---- Vaults ----
    checked
        .call(
            "add_vault",
            json!({"name": "main", "path": vault.path().to_string_lossy()}),
        )
        .await;
    checked
        .call(
            "create_vault",
            json!({"name": "made", "path": elsewhere.path().join("made").to_string_lossy()}),
        )
        .await;
    checked.call("list_vaults", json!({})).await;
    checked
        .call("get_vault_config", json!({"name": "main"}))
        .await;
    checked
        .call("set_active_vault", json!({"name": "main"}))
        .await;
    checked.call("get_active_vault", json!({})).await;
    checked.call("remove_vault", json!({"name": "made"})).await;

    // ---- Notes ----
    for (path, content) in [("alpha.md", ALPHA), ("beta.md", BETA), ("gamma.md", GAMMA)] {
        checked
            .call("write_note", json!({"path": path, "content": content}))
            .await;
    }
    checked.call("get_vault_context", json!({})).await;
    checked
        .call("get_vault_context", json!({"response_format": "concise"}))
        .await;
    checked.call("read_note", json!({"path": "alpha.md"})).await;
    checked
        .call("read_note", json!({"path": "alpha.md", "head_lines": 3}))
        .await;
    checked
        .call(
            "read_note",
            json!({"path": "alpha.md", "heading_level": 1, "last_sections": 1}),
        )
        .await;
    checked
        .call(
            "write_note",
            json!({"path": "scratch.md", "content": "# Scratch\n\nDraft text.\n"}),
        )
        .await;
    checked
        .call(
            "edit_note",
            json!({
                "path": "scratch.md",
                "edits": "<<<<<<< SEARCH\nDraft text.\n=======\nFinal text.\n>>>>>>> REPLACE",
            }),
        )
        .await;
    checked
        .call(
            "move_note",
            json!({"from": "scratch.md", "to": "moved/scratch.md"}),
        )
        .await;
    checked
        .call(
            "move_file",
            json!({
                "from": "picture.png",
                "to": "attachments/picture.png",
                "confirm_from": "picture.png",
                "confirm_to": "attachments/picture.png",
            }),
        )
        .await;
    checked
        .call(
            "delete_note",
            json!({"path": "moved/scratch.md", "confirm_path": "moved/scratch.md"}),
        )
        .await;
    checked
        .call(
            "get_notes_info",
            json!({"paths": ["alpha.md", "does-not-exist.md"]}),
        )
        .await;

    // ---- Canvases, bases, tasks and periodic notes ----
    checked
        .call(
            "write_canvas",
            json!({
                "path": "boards/Board.canvas",
                "canvas": {
                    "nodes": [
                        {"id": "f", "type": "file", "file": "beta.md", "x": 0, "y": 0, "width": 200, "height": 200},
                        {"id": "t", "type": "text", "text": "See [[alpha]].", "x": 0, "y": 300, "width": 200, "height": 100},
                    ],
                    "edges": [{"id": "e", "fromNode": "t", "toNode": "f", "label": "about"}],
                },
            }),
        )
        .await;
    checked
        .call("read_canvas", json!({"path": "boards/Board.canvas"}))
        .await;
    checked
        .call(
            "read_canvas",
            json!({"path": "boards/Board.canvas", "limit": 1}),
        )
        .await;
    std::fs::write(
        vault.path().join("Concepts.base"),
        "filters: 'type == \"concept\"'\nformulas:\n  rank: 'priority * 2'\nviews:\n  - type: table\n    name: Concepts\n    order: [file.name, status, formula.rank]\n    groupBy:\n      property: status\n      direction: ASC\n",
    )
    .unwrap();
    checked
        .call("query_base", json!({"path": "Concepts.base"}))
        .await;
    checked
        .call(
            "query_tasks",
            json!({"status": ["todo"], "path": "alpha.md"}),
        )
        .await;
    checked
        .call(
            "get_periodic_note",
            json!({"period": "daily", "date": "2026-10-13"}),
        )
        .await;
    checked
        .call(
            "get_periodic_note",
            json!({"period": "weekly", "date": "2026-10-13", "create": true}),
        )
        .await;

    // ---- Graph ----
    for tool in [
        "get_backlinks",
        "get_forward_links",
        "get_related_notes",
        "recommend_related",
        "evaluate_note_quality",
        "analyze_note_grounding",
        "find_similar_notes",
    ] {
        checked.call(tool, json!({"path": "beta.md"})).await;
    }
    for tool in [
        "get_hub_notes",
        "get_dead_end_notes",
        "get_isolated_clusters",
        "quick_health_check",
        "full_health_analysis",
        "get_broken_links",
        "detect_cycles",
        "explain_vault",
        "get_centrality_ranking",
        "vault_quality_report",
        "find_ungrounded_notes",
        "okf_validate",
        "find_stale_notes",
        "find_duplicates",
        "list_templates",
        "get_ofm_syntax_guide",
        "get_ofm_quick_ref",
        "get_ofm_examples",
        "audit_stats",
    ] {
        checked.call(tool, json!({})).await;
    }
    checked
        .call("explain_vault", json!({"response_format": "concise"}))
        .await;
    checked
        .call("suggest_links", json!({"file": "gamma.md"}))
        .await;
    checked
        .call(
            "get_link_strength",
            json!({"source": "alpha.md", "target": "beta.md"}),
        )
        .await;

    // ---- Search ----
    for format in ["detailed", "concise"] {
        checked
            .call(
                "search",
                json!({"query": "distributed", "response_format": format}),
            )
            .await;
    }
    checked
        .call(
            "advanced_search",
            json!({"query": "distributed", "tags": ["seed"], "response_format": "concise"}),
        )
        .await;
    checked
        .call(
            "search_by_frontmatter",
            json!({"key": "status", "value": "draft"}),
        )
        .await;
    checked
        .call("semantic_search", json!({"query": "distributed systems"}))
        .await;
    checked
        .call(
            "compare_notes",
            json!({"left": "alpha.md", "right": "beta.md"}),
        )
        .await;
    checked
        .call(
            "diff_notes",
            json!({"left": "alpha.md", "right": "beta.md"}),
        )
        .await;
    #[cfg(feature = "sql")]
    {
        checked.call("inspect_frontmatter", json!({})).await;
        checked
            .call(
                "query_frontmatter_sql",
                json!({"sql": "SELECT path, status FROM files ORDER BY path"}),
            )
            .await;
    }

    // ---- Metadata ----
    checked
        .call("query_metadata", json!({"pattern": "status: \"draft\""}))
        .await;
    checked
        .call(
            "get_metadata_value",
            json!({"file": "alpha.md", "key": "status"}),
        )
        .await;
    checked
        .call(
            "update_frontmatter",
            json!({"path": "gamma.md", "frontmatter": {"reviewed": true}}),
        )
        .await;
    checked
        .call(
            "manage_tags",
            json!({"path": "alpha.md", "operation": "list"}),
        )
        .await;
    checked
        .call(
            "manage_tags",
            json!({"path": "gamma.md", "operation": "add", "tags": ["ops"]}),
        )
        .await;

    // ---- Templates ----
    checked
        .call("get_template", json!({"template_id": "doc"}))
        .await;
    checked
        .call(
            "create_from_template",
            json!({
                "template_id": "doc",
                "file_path": "guides/auth.md",
                "fields": json!({"title": "Auth", "summary": "How sign-in works."}).to_string(),
            }),
        )
        .await;
    checked
        .call("find_notes_from_template", json!({"template_id": "doc"}))
        .await;

    // ---- Batch, OKF, exports ----
    checked
        .call(
            "batch_execute",
            json!({"operations": [{"type": "WriteNote", "path": "batch.md", "content": "# Batch\n"}]}),
        )
        .await;
    checked
        .call("generate_index", json!({"dry_run": true}))
        .await;
    checked
        .call(
            "append_log_entry",
            json!({"text": "Checked output schemas."}),
        )
        .await;
    checked.call("visualize", json!({})).await;
    for tool in [
        "export_health_report",
        "export_broken_links",
        "export_vault_stats",
        "export_analysis_report",
    ] {
        checked.call(tool, json!({"format": "json"})).await;
    }

    // ---- Audit ----
    checked
        .call(
            "write_note",
            json!({"path": "gamma.md", "content": GAMMA, "expected_hash": "blind"}),
        )
        .await;
    let log = checked
        .call(
            "audit_log",
            json!({"path": "gamma.md", "operation": "UPDATE", "limit": 1}),
        )
        .await;
    let operation_id = log["data"][0]["id"]
        .as_str()
        .expect("an audited update")
        .to_string();
    checked
        .call(
            "diff_note_version",
            json!({"path": "gamma.md", "operation_id": operation_id}),
        )
        .await;
    checked
        .call("rollback_preview", json!({"operation_id": operation_id}))
        .await;
    checked
        .call("rollback_note", json!({"operation_id": operation_id}))
        .await;

    // ---- Fanout, on a Git-backed vault ----
    checked
        .call(
            "add_vault",
            json!({
                "name": "git",
                "path": git_vault.path().to_string_lossy(),
                "write_backend": "git",
            }),
        )
        .await;
    checked
        .call("set_active_vault", json!({"name": "git"}))
        .await;
    checked.call("list_orphan_fanouts", json!({})).await;
    checked.call("abandon_fanout", json!({})).await;
    checked.call("begin_fanout", json!({})).await;
    checked.call("abandon_fanout", json!({})).await;
    let begun = checked.call("begin_fanout", json!({})).await;
    let fanout_vault = begun["data"]["fanout_vault"].as_str().unwrap().to_string();
    checked
        .call("set_active_vault", json!({"name": fanout_vault}))
        .await;
    checked
        .call(
            "write_note",
            json!({"path": "fan.md", "content": "# In the fanout\n"}),
        )
        .await;
    checked.call("commit_fanout", json!({})).await;

    // Every tool was called and matched.
    let mut missing: Vec<String> = server
        .list_tools()
        .into_iter()
        .map(|tool| tool.name)
        .filter(|name| !checked.seen.contains(name))
        .collect();
    #[cfg(not(feature = "sql"))]
    missing.retain(|name| name != "inspect_frontmatter" && name != "query_frontmatter_sql");
    missing.sort();
    assert!(missing.is_empty(), "never checked: {missing:?}");
}
