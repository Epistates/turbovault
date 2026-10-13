//! Every tool tells an agent what it needs to call it well.
//!
//! An agent picks a tool and fills its arguments from the catalog alone, so
//! each tool must say whether it is safe to call (the four behavior hints and
//! a title) and each parameter must say what it takes.

use turbomcp::McpHandler;
use turbovault::ObsidianMcpServer;

#[test]
fn every_tool_is_fully_annotated() {
    let server = ObsidianMcpServer::new().expect("server");
    let tools = server.list_tools();
    assert!(tools.len() >= 74, "catalog shrank to {}", tools.len());

    for tool in &tools {
        let name = &tool.name;
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{name} has no annotations"));
        assert!(
            annotations.title.as_deref().is_some_and(|t| !t.is_empty()),
            "{name} has no title"
        );
        assert!(annotations.read_only_hint.is_some(), "{name}: readOnlyHint");
        assert!(
            annotations.destructive_hint.is_some(),
            "{name}: destructiveHint"
        );
        assert!(
            annotations.idempotent_hint.is_some(),
            "{name}: idempotentHint"
        );
        assert!(
            annotations.open_world_hint.is_some(),
            "{name}: openWorldHint"
        );
        if annotations.read_only_hint == Some(true) {
            assert_eq!(
                annotations.destructive_hint,
                Some(false),
                "{name} is read-only, so it can't be destructive"
            );
        }
    }
}

#[test]
fn every_parameter_and_tool_is_described() {
    let server = ObsidianMcpServer::new().expect("server");

    for tool in server.list_tools() {
        let name = &tool.name;
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(description.len() > 20, "{name} needs a real description");
        assert!(
            !description.contains('\u{2014}'),
            "{name}'s description has an em dash"
        );

        let schema = serde_json::to_value(&tool.input_schema).expect("schema");
        let properties = schema["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        for (param, property) in properties {
            let text = property["description"].as_str().unwrap_or_default();
            assert!(!text.trim().is_empty(), "{name}.{param} has no description");
            assert!(
                !text.contains('\u{2014}'),
                "{name}.{param}'s description has an em dash"
            );
        }
    }
}
