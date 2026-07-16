//! Integration tests for tool visibility filtering applied to ObsidianMcpServer.
//!
//! These tests exercise ToolVisibilitySettings -> VisibilityLayer end-to-end
//! without starting a network server.

use turbomcp::{McpHandler, RequestContext, VisibilityLayer};
use turbovault::ObsidianMcpServer;
use turbovault::tool_visibility::ToolVisibilitySettings;

fn make_server() -> ObsidianMcpServer {
    ObsidianMcpServer::new().expect("server creation must not fail without a vault")
}

fn ctx() -> RequestContext {
    RequestContext::new()
}

fn tool_names(layer: &impl McpHandler) -> Vec<String> {
    layer.list_tools().into_iter().map(|t| t.name).collect()
}

#[test]
fn server_advertises_package_version() {
    let server = make_server();
    assert_eq!(server.server_info().version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn mutating_tool_descriptions_match_current_safety_contract() {
    let server = make_server();
    let tools = server.list_tools();

    let batch = tools
        .iter()
        .find(|tool| tool.name == "batch_execute")
        .expect("batch_execute descriptor");
    let batch_description = batch.description.as_deref().unwrap_or_default();
    assert!(batch_description.contains("without rolling back"));
    assert!(!batch_description.contains("all-or-nothing"));

    let move_note = tools
        .iter()
        .find(|tool| tool.name == "move_note")
        .expect("move_note descriptor");
    let move_description = move_note.description.as_deref().unwrap_or_default();
    assert!(move_description.contains("Does NOT update wikilinks"));
}

#[tokio::test]
async fn batch_response_reports_partial_failure_contract() {
    let temp = tempfile::TempDir::new().expect("temp vault");
    let server = make_server();
    let config = turbovault_core::VaultConfig::builder("test", temp.path())
        .build()
        .expect("vault config");
    server
        .multi_vault()
        .add_vault(config)
        .await
        .expect("register vault");

    let result = server
        .call_tool(
            "batch_execute",
            serde_json::json!({
                "operations": [
                    {"type": "CreateNote", "path": "created.md", "content": "# Created"},
                    {"type": "DeleteNote", "path": "missing.md"}
                ]
            }),
            &ctx(),
        )
        .await
        .expect("batch tool returns a structured failed batch");

    let response = result
        .structured_content
        .expect("batch response should expose structured content");
    assert_eq!(response["success"], false);
    assert_eq!(response["data"]["success"], false);
    assert_eq!(response["data"]["executed"], 1);
    assert_eq!(response["data"]["failed_at"], 1);
    assert_eq!(response["meta"]["execution_mode"], "sequential_fail_fast");
    assert!(
        response["warnings"][0]
            .as_str()
            .unwrap_or_default()
            .contains("not rolled back")
    );
    assert!(temp.path().join("created.md").exists());
}

#[tokio::test]
async fn visibility_layer_empty_settings_exposes_all_tools() {
    let layer =
        ToolVisibilitySettings::default().apply_to_layer(VisibilityLayer::new(make_server()));
    let names = tool_names(&layer);
    // A minimal set of expected tools — confirm nothing was accidentally hidden.
    for expected in &[
        "read_note",
        "write_note",
        "delete_note",
        "search",
        "audit_log",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "expected tool {expected:?} to be listed with empty settings"
        );
    }
}

#[tokio::test]
async fn visibility_layer_disabled_name_blocks_tool() {
    let settings = ToolVisibilitySettings {
        disabled: vec!["delete_note".to_string()],
        ..Default::default()
    };
    let layer = settings.apply_to_layer(VisibilityLayer::new(make_server()));

    // Not in list.
    assert!(!tool_names(&layer).contains(&"delete_note".to_string()));

    // Direct call is rejected with ToolNotFound (-32001).
    let err = layer
        .call_tool("delete_note", serde_json::json!({"path": "x.md"}), &ctx())
        .await
        .expect_err("disabled tool must not be callable");
    assert_eq!(
        err.jsonrpc_code(),
        -32001,
        "expected ToolNotFound error code"
    );
}

#[tokio::test]
async fn visibility_layer_hidden_name_omits_from_list_but_allows_call() {
    let settings = ToolVisibilitySettings {
        hidden: vec!["full_health_analysis".to_string()],
        ..Default::default()
    };
    let layer = settings.apply_to_layer(VisibilityLayer::new(make_server()));

    // Hidden from listing.
    assert!(
        !tool_names(&layer).contains(&"full_health_analysis".to_string()),
        "hidden tool must not appear in list_tools"
    );

    // But still callable — it returns a vault-not-found error, not ToolNotFound.
    let result = layer
        .call_tool("full_health_analysis", serde_json::json!({}), &ctx())
        .await;
    match result {
        Ok(_) => {}
        Err(e) => {
            // Any error except ToolNotFound is acceptable; ToolNotFound (-32001) would
            // mean the layer is incorrectly blocking a merely-hidden tool.
            assert_ne!(
                e.jsonrpc_code(),
                -32001,
                "hidden tool must not return ToolNotFound; got: {e}"
            );
        }
    }
}

#[tokio::test]
async fn visibility_layer_disabled_tags_blocks_all_tagged_tools() {
    let settings = ToolVisibilitySettings {
        disabled_tags: vec!["delete".to_string()],
        ..Default::default()
    };
    let layer = settings.apply_to_layer(VisibilityLayer::new(make_server()));

    // delete_note carries the "delete" tag and must be absent.
    let names = tool_names(&layer);
    assert!(
        !names.contains(&"delete_note".to_string()),
        "delete_note must be absent when tag 'delete' is disabled"
    );
    // Non-delete tools must still be present.
    assert!(
        names.contains(&"read_note".to_string()),
        "read_note must remain visible when only 'delete' tag is disabled"
    );

    // Direct calls must be rejected.
    let err = layer
        .call_tool("delete_note", serde_json::json!({"path": "x.md"}), &ctx())
        .await
        .expect_err("tag-disabled tool must not be callable");
    assert_eq!(err.jsonrpc_code(), -32001);
}

#[tokio::test]
async fn visibility_layer_require_read_only_hides_write_tools() {
    let settings = ToolVisibilitySettings {
        require_read_only: true,
        ..Default::default()
    };
    let layer = settings.apply_to_layer(VisibilityLayer::new(make_server()));

    let names = tool_names(&layer);

    // Read-only tools should still appear.
    assert!(
        names.contains(&"read_note".to_string()),
        "read_only tool read_note must remain visible"
    );
    assert!(
        names.contains(&"search".to_string()),
        "read_only tool search must remain visible"
    );

    // Write tools (destructive=true / read_only not set) must be hidden.
    assert!(
        !names.contains(&"write_note".to_string()),
        "write_note must be hidden under require_read_only"
    );
    assert!(
        !names.contains(&"delete_note".to_string()),
        "delete_note must be hidden under require_read_only"
    );
}
