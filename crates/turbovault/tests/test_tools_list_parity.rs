//! Parity / regression suite for the MCP tool surface (`tools/list` + `call_tool`).
//!
//! Pins the **public core** contract that must stay identical across the tools.rs
//! decomposition ([#28](https://github.com/Epistates/turbovault/issues/28)), Phase 1
//! of the plugin plan ([#34](https://github.com/Epistates/turbovault/issues/34)):
//!
//! - Exact flat tool names (no missing/extra; no CompositeHandler prefix leaks)
//! - Tags + read-only / destructive annotations (visibility filtering)
//! - Input schema property names (parameter contracts)
//! - Every listed tool remains dispatchable via `call_tool` (not ToolNotFound)
//!
//! Plugin verticals (#34 Phase 3) may later add namespaced tools (`tasks_*`);
//! this suite guards the **core** catalog only.
//!
//! When intentionally adding, removing, or renaming a core tool, update
//! [`EXPECTED_TOOLS`] in the same PR.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;

/// JSON-RPC / MCP error code for unknown tools.
const TOOL_NOT_FOUND: i32 = -32001;

/// Tag categories that visibility filtering and docs rely on; must remain present
/// somewhere in the catalog (per-tool tags are checked exactly below).
const REQUIRED_TAG_CATEGORIES: &[&str] = &[
    "read",
    "write",
    "delete",
    "graph",
    "search",
    "health",
    "admin",
    "template",
    "frontmatter",
    "export",
    "batch",
    "audit",
    "semantic",
    "sql",
];

/// Expected public tool catalog: name → (tags, read_only, destructive, input properties).
///
/// Order does not matter; assertions sort by name.
/// `input_properties` must match the JSON Schema property keys emitted for the tool.
/// Core names stay flat (`read_note`); namespacing is for plugins only (#34).
const EXPECTED_TOOLS: &[ExpectedTool] = &[
    // Discovery / vault context
    ExpectedTool::new("get_vault_context", &["read"], true, false, &[]),
    // File ops
    ExpectedTool::new("read_note", &["read"], true, false, &["path"]),
    ExpectedTool::new(
        "write_note",
        &["write"],
        false,
        true,
        &["path", "content", "mode", "expected_hash"],
    ),
    ExpectedTool::new(
        "edit_note",
        &["write"],
        false,
        true,
        &["path", "edits", "expected_hash", "dry_run"],
    ),
    ExpectedTool::new(
        "delete_note",
        &["write", "delete"],
        false,
        true,
        &["path", "confirm_path", "expected_hash"],
    ),
    ExpectedTool::new(
        "move_note",
        &["write"],
        false,
        true,
        &["from", "to", "expected_hash"],
    ),
    // Graph / links
    ExpectedTool::new("get_backlinks", &["read", "graph"], true, false, &["path"]),
    ExpectedTool::new(
        "get_forward_links",
        &["read", "graph"],
        true,
        false,
        &["path"],
    ),
    ExpectedTool::new(
        "get_related_notes",
        &["read", "graph"],
        true,
        false,
        &["path", "max_hops"],
    ),
    ExpectedTool::new("get_hub_notes", &["read", "graph"], true, false, &["top_n"]),
    ExpectedTool::new("get_dead_end_notes", &["read", "graph"], true, false, &[]),
    ExpectedTool::new(
        "get_isolated_clusters",
        &["read", "graph"],
        true,
        false,
        &[],
    ),
    ExpectedTool::new(
        "suggest_links",
        &["read", "graph"],
        true,
        false,
        &["file", "limit"],
    ),
    ExpectedTool::new(
        "get_link_strength",
        &["read", "graph"],
        true,
        false,
        &["source", "target"],
    ),
    ExpectedTool::new(
        "get_centrality_ranking",
        &["read", "graph"],
        true,
        false,
        &[],
    ),
    ExpectedTool::new("detect_cycles", &["read", "graph"], true, false, &[]),
    // Health
    ExpectedTool::new("quick_health_check", &["read", "health"], true, false, &[]),
    ExpectedTool::new(
        "full_health_analysis",
        &["read", "health"],
        true,
        false,
        &[],
    ),
    ExpectedTool::new("get_broken_links", &["read", "health"], true, false, &[]),
    ExpectedTool::new(
        "evaluate_note_quality",
        &["read", "health"],
        true,
        false,
        &["path"],
    ),
    ExpectedTool::new(
        "vault_quality_report",
        &["read", "health"],
        true,
        false,
        &["bottom_n"],
    ),
    ExpectedTool::new(
        "find_stale_notes",
        &["read", "health"],
        true,
        false,
        &["threshold_days", "limit"],
    ),
    // Search / semantic
    ExpectedTool::new("search", &["read", "search"], true, false, &["query"]),
    ExpectedTool::new(
        "advanced_search",
        &["read", "search"],
        true,
        false,
        &[
            "query",
            "tags",
            "frontmatter_filters",
            "exclude_paths",
            "limit",
        ],
    ),
    ExpectedTool::new(
        "search_by_frontmatter",
        &["read", "search"],
        true,
        false,
        &["key", "value"],
    ),
    ExpectedTool::new(
        "recommend_related",
        &["read", "search"],
        true,
        false,
        &["path"],
    ),
    ExpectedTool::new(
        "semantic_search",
        &["read", "search", "semantic"],
        true,
        false,
        &["query", "limit"],
    ),
    ExpectedTool::new(
        "find_similar_notes",
        &["read", "search", "semantic"],
        true,
        false,
        &["path", "limit"],
    ),
    ExpectedTool::new(
        "find_duplicates",
        &["read", "search", "semantic"],
        true,
        false,
        &["threshold", "limit"],
    ),
    ExpectedTool::new(
        "compare_notes",
        &["read", "semantic"],
        true,
        false,
        &["left", "right"],
    ),
    // Frontmatter / metadata / SQL
    ExpectedTool::new(
        "inspect_frontmatter",
        &["read", "frontmatter"],
        true,
        false,
        &[],
    ),
    ExpectedTool::new(
        "query_frontmatter_sql",
        &["read", "frontmatter", "sql"],
        true,
        false,
        &["sql"],
    ),
    ExpectedTool::new(
        "query_metadata",
        &["read", "frontmatter"],
        true,
        false,
        &["pattern"],
    ),
    ExpectedTool::new(
        "get_metadata_value",
        &["read", "frontmatter"],
        true,
        false,
        &["file", "key"],
    ),
    ExpectedTool::new(
        "update_frontmatter",
        &["write", "frontmatter"],
        false,
        true,
        &["path", "frontmatter", "merge"],
    ),
    ExpectedTool::new(
        "manage_tags",
        &["write", "frontmatter"],
        false,
        true,
        &["path", "operation", "tags"],
    ),
    // Templates
    ExpectedTool::new("list_templates", &["read", "template"], true, false, &[]),
    ExpectedTool::new(
        "get_template",
        &["read", "template"],
        true,
        false,
        &["template_id"],
    ),
    ExpectedTool::new(
        "create_from_template",
        &["write", "template"],
        false,
        true,
        &["template_id", "file_path", "fields"],
    ),
    ExpectedTool::new(
        "find_notes_from_template",
        &["read", "template"],
        true,
        false,
        &["template_id"],
    ),
    // Vault lifecycle / admin
    ExpectedTool::new(
        "create_vault",
        &["write", "admin"],
        false,
        false,
        &["name", "path", "template"],
    ),
    ExpectedTool::new(
        "add_vault",
        &["write", "admin"],
        false,
        false,
        &["name", "path"],
    ),
    ExpectedTool::new("remove_vault", &["write", "admin"], false, false, &["name"]),
    ExpectedTool::new("list_vaults", &["read", "admin"], true, false, &[]),
    ExpectedTool::new(
        "get_vault_config",
        &["read", "admin"],
        true,
        false,
        &["name"],
    ),
    ExpectedTool::new(
        "set_active_vault",
        &["write", "admin"],
        false,
        false,
        &["name"],
    ),
    ExpectedTool::new("get_active_vault", &["read", "admin"], true, false, &[]),
    // Batch / export
    ExpectedTool::new(
        "batch_execute",
        &["write", "batch"],
        false,
        true,
        &["operations"],
    ),
    ExpectedTool::new(
        "export_health_report",
        &["read", "export"],
        true,
        false,
        &["format"],
    ),
    ExpectedTool::new(
        "export_broken_links",
        &["read", "export"],
        true,
        false,
        &["format"],
    ),
    ExpectedTool::new(
        "export_vault_stats",
        &["read", "export"],
        true,
        false,
        &["format"],
    ),
    ExpectedTool::new(
        "export_analysis_report",
        &["read", "export"],
        true,
        false,
        &["format"],
    ),
    // Misc read / analysis
    ExpectedTool::new("explain_vault", &["read"], true, false, &[]),
    ExpectedTool::new("get_notes_info", &["read"], true, false, &["paths"]),
    ExpectedTool::new(
        "move_file",
        &["write"],
        false,
        true,
        &["from", "to", "confirm_from", "confirm_to", "expected_hash"],
    ),
    ExpectedTool::new("get_ofm_syntax_guide", &["read"], true, false, &[]),
    ExpectedTool::new("get_ofm_quick_ref", &["read"], true, false, &[]),
    ExpectedTool::new("get_ofm_examples", &["read"], true, false, &[]),
    ExpectedTool::new("diff_notes", &["read"], true, false, &["left", "right"]),
    // Audit
    ExpectedTool::new(
        "diff_note_version",
        &["read", "audit"],
        true,
        false,
        &["path", "operation_id"],
    ),
    ExpectedTool::new(
        "audit_log",
        &["read", "audit"],
        true,
        false,
        &["path", "operation", "limit"],
    ),
    ExpectedTool::new(
        "rollback_preview",
        &["read", "audit"],
        true,
        false,
        &["operation_id"],
    ),
    ExpectedTool::new(
        "rollback_note",
        &["write", "audit"],
        false,
        true,
        &["operation_id"],
    ),
    ExpectedTool::new("audit_stats", &["read", "audit"], true, false, &[]),
];

#[derive(Clone, Copy, Debug)]
struct ExpectedTool {
    name: &'static str,
    tags: &'static [&'static str],
    read_only: bool,
    destructive: bool,
    input_properties: &'static [&'static str],
}

impl ExpectedTool {
    const fn new(
        name: &'static str,
        tags: &'static [&'static str],
        read_only: bool,
        destructive: bool,
        input_properties: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            tags,
            read_only,
            destructive,
            input_properties,
        }
    }
}

fn make_server() -> ObsidianMcpServer {
    ObsidianMcpServer::new().expect("server creation must not fail without a vault")
}

fn tags_from_meta(
    meta: &Option<std::collections::HashMap<String, serde_json::Value>>,
) -> Vec<String> {
    meta.as_ref()
        .and_then(|m| m.get("tags"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn schema_property_names(tool: &turbomcp::Tool) -> BTreeSet<String> {
    tool.input_schema
        .properties_as_object()
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

/// Single pass over `tools/list`: names, dups, tags, annotations, descriptions,
/// input schema keys, tag categories, and server identity.
///
/// One server construction covers the whole listing contract (#28 acceptance).
#[test]
fn tools_list_catalog_parity() {
    // Fixture must not contain duplicate names (would make set-equality lie).
    let mut fixture_names = HashSet::new();
    for tool in EXPECTED_TOOLS {
        assert!(
            fixture_names.insert(tool.name),
            "duplicate name in EXPECTED_TOOLS fixture: {}",
            tool.name
        );
    }

    let expected: BTreeMap<&str, ExpectedTool> =
        EXPECTED_TOOLS.iter().map(|t| (t.name, *t)).collect();

    let server = make_server();

    let info = server.server_info();
    assert_eq!(info.name, "obsidian-vault");
    assert_eq!(info.version, "1.4.0");

    let tools = server.list_tools();

    // Duplicates: set equality would collapse them, so check list length first.
    let mut seen = HashSet::new();
    let mut dupes = Vec::new();
    for tool in &tools {
        if !seen.insert(tool.name.as_str()) {
            dupes.push(tool.name.clone());
        }
    }
    assert!(
        dupes.is_empty(),
        "duplicate tool names in tools/list: {dupes:?}"
    );

    let listed_names: BTreeSet<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    let expected_names: BTreeSet<&str> = expected.keys().copied().collect();
    let missing: Vec<_> = expected_names.difference(&listed_names).copied().collect();
    let extra: Vec<_> = listed_names.difference(&expected_names).copied().collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "tools/list name set diverged.\n  missing: {missing:?}\n  extra: {extra:?}\n  \
         (extras like graph_read_note usually mean a non-empty CompositeHandler prefix on core)"
    );

    let mut failures = Vec::new();
    let mut runtime_tags: BTreeSet<String> = BTreeSet::new();

    for tool in &tools {
        // Exact set match above guarantees this; still defensive for clear errors.
        let Some(exp) = expected.get(tool.name.as_str()) else {
            failures.push(format!("unexpected tool: {}", tool.name));
            continue;
        };

        let actual_tags = tags_from_meta(&tool.meta);
        for tag in &actual_tags {
            runtime_tags.insert(tag.clone());
        }
        let expected_tags: Vec<String> = exp.tags.iter().map(|s| (*s).to_string()).collect();
        if actual_tags != expected_tags {
            failures.push(format!(
                "{}: tags actual={actual_tags:?} expected={expected_tags:?}",
                tool.name
            ));
        }

        let ro = tool
            .annotations
            .as_ref()
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false);
        let dest = tool
            .annotations
            .as_ref()
            .and_then(|a| a.destructive_hint)
            .unwrap_or(false);
        if ro != exp.read_only {
            failures.push(format!(
                "{}: readOnlyHint actual={ro} expected={}",
                tool.name, exp.read_only
            ));
        }
        if dest != exp.destructive {
            failures.push(format!(
                "{}: destructiveHint actual={dest} expected={}",
                tool.name, exp.destructive
            ));
        }

        match &tool.description {
            Some(d) if !d.trim().is_empty() => {}
            Some(_) => failures.push(format!("{}: description is empty", tool.name)),
            None => failures.push(format!("{}: description is missing", tool.name)),
        }

        let actual_props = schema_property_names(tool);
        let expected_props: BTreeSet<String> = exp
            .input_properties
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        if actual_props != expected_props {
            let prop_missing: Vec<_> = expected_props.difference(&actual_props).cloned().collect();
            let prop_extra: Vec<_> = actual_props.difference(&expected_props).cloned().collect();
            failures.push(format!(
                "{}: inputSchema props missing={prop_missing:?} extra={prop_extra:?}",
                tool.name
            ));
        }
    }

    for category in REQUIRED_TAG_CATEGORIES {
        if !runtime_tags.iter().any(|t| t == *category) {
            failures.push(format!(
                "tag category {category:?} missing from tools/list (visibility/docs rely on it)"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "tools/list catalog parity failures:\n  - {}",
        failures.join("\n  - ")
    );
}

/// Routing parity: every public tool name must be recognized by `call_tool`.
///
/// Domain errors (no vault, missing args, etc.) are fine. ToolNotFound (-32001)
/// means the tool was dropped from the composite dispatch table — a different
/// path from `list_tools`, so this is intentionally separate.
#[tokio::test]
async fn every_listed_tool_is_dispatchable_not_tool_not_found() {
    let server = make_server();
    let names: Vec<String> = server.list_tools().into_iter().map(|t| t.name).collect();
    assert!(
        !names.is_empty(),
        "list_tools returned no tools; run tools_list_catalog_parity for details"
    );

    let ctx = RequestContext::new();
    let mut not_found = Vec::new();

    for name in &names {
        let result = server.call_tool(name, serde_json::json!({}), &ctx).await;
        match result {
            Ok(_) => {}
            Err(e) if e.jsonrpc_code() == TOOL_NOT_FOUND => not_found.push(name.clone()),
            Err(_) => {
                // Tool was found and executed (validation/domain error is fine).
            }
        }
    }

    assert!(
        not_found.is_empty(),
        "tools returned ToolNotFound (-32001); dispatch incomplete after decomposition: {not_found:?}"
    );
}
