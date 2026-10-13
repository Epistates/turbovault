//! Holds a tool result to the outputSchema the tool advertises.
//!
//! Shared by the tests that call tools, so every successful call they make
//! also checks that the result still matches its schema.

use serde_json::Value;
use turbomcp::McpHandler;
use turbovault::ObsidianMcpServer;

/// Panic unless `structured` (the `structuredContent` of a successful call to
/// `tool`) conforms to the outputSchema `tool` advertises in tools/list.
pub fn assert_matches_output_schema(server: &ObsidianMcpServer, tool: &str, structured: &Value) {
    let listed = server
        .list_tools()
        .into_iter()
        .find(|listed| listed.name == tool)
        .unwrap_or_else(|| panic!("{tool} is not in tools/list"));
    let schema = listed
        .output_schema
        .unwrap_or_else(|| panic!("{tool} advertises no outputSchema"));
    let schema = serde_json::to_value(schema).expect("outputSchema serializes");
    let validator = jsonschema::validator_for(&schema)
        .unwrap_or_else(|error| panic!("{tool}: outputSchema is not a valid schema: {error}"));
    let errors: Vec<String> = validator
        .iter_errors(structured)
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{tool}: result does not match its outputSchema:\n  {}\nresult: {}",
        errors.join("\n  "),
        serde_json::to_string(structured).unwrap_or_default()
    );
}
