use serde_json::json;

/// Minimal StandardResponse replica for testing (matching src/tools.rs)
#[derive(serde::Serialize, Debug)]
pub struct StandardResponse<T: serde::Serialize> {
    pub vault: String,
    pub operation: String,
    pub success: bool,
    pub data: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    pub took_ms: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub next_steps: Vec<String>,
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub meta: serde_json::Map<String, serde_json::Value>,
}

impl<T: serde::Serialize> StandardResponse<T> {
    pub fn new(vault: impl Into<String>, operation: impl Into<String>, data: T) -> Self {
        Self {
            vault: vault.into(),
            operation: operation.into(),
            success: true,
            data,
            count: None,
            took_ms: 0,
            warnings: vec![],
            next_steps: vec![],
            meta: serde_json::Map::new(),
        }
    }

    pub fn with_count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    pub fn with_duration(mut self, ms: u64) -> Self {
        self.took_ms = ms;
        self
    }

    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    pub fn with_next_steps(mut self, steps: &[&str]) -> Self {
        for step in steps {
            self.next_steps.push(step.to_string());
        }
        self
    }

    pub fn with_meta(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.meta.insert(key.into(), value);
        self
    }

    pub fn to_json(self) -> serde_json::Value {
        serde_json::to_value(self).expect("Failed to serialize")
    }
}

#[test]
fn test_standard_response_basic_structure() {
    let response =
        StandardResponse::new("test-vault", "search", json!({"results": []})).with_count(0);

    let json = response.to_json();

    assert_eq!(json["vault"], "test-vault");
    assert_eq!(json["operation"], "search");
    assert_eq!(json["success"], true);
    assert_eq!(json["count"], 0);
    assert_eq!(json["took_ms"], 0);
}

#[test]
fn test_standard_response_count_only_if_set() {
    // Without count
    let response1 = StandardResponse::new("vault", "op", json!({})).to_json();
    assert!(response1["count"].is_null());

    // With count
    let response2 = StandardResponse::new("vault", "op", json!({}))
        .with_count(5)
        .to_json();
    assert_eq!(response2["count"], 5);
}

#[test]
fn test_standard_response_warnings_excluded_when_empty() {
    let response = StandardResponse::new("vault", "op", json!({})).to_json();
    assert!(
        response["warnings"].is_null()
            || response["warnings"]
                .as_array()
                .is_some_and(|a| a.is_empty())
    );
}

#[test]
fn test_standard_response_warnings_included_when_present() {
    let response = StandardResponse::new("vault", "op", json!({}))
        .with_warning("Note has duplicate links")
        .with_warning("File is too large")
        .to_json();

    assert_eq!(response["warnings"].as_array().unwrap().len(), 2);
    assert_eq!(response["warnings"][0], "Note has duplicate links");
    assert_eq!(response["warnings"][1], "File is too large");
}

#[test]
fn test_standard_response_next_steps_excluded_when_empty() {
    let response = StandardResponse::new("vault", "op", json!({})).to_json();
    assert!(
        response["next_steps"].is_null()
            || response["next_steps"]
                .as_array()
                .is_some_and(|a| a.is_empty())
    );
}

#[test]
fn test_standard_response_next_steps_included_when_present() {
    let response = StandardResponse::new("vault", "op", json!({}))
        .with_next_steps(&["write_note", "get_backlinks"])
        .to_json();

    assert_eq!(response["next_steps"].as_array().unwrap().len(), 2);
    assert_eq!(response["next_steps"][0], "write_note");
    assert_eq!(response["next_steps"][1], "get_backlinks");
}

#[test]
fn test_standard_response_meta_excluded_when_empty() {
    let response = StandardResponse::new("vault", "op", json!({})).to_json();
    assert!(
        response["meta"].is_null() || response["meta"].as_object().is_none_or(|m| m.is_empty())
    );
}

#[test]
fn test_standard_response_meta_included_when_present() {
    let response = StandardResponse::new("vault", "op", json!({}))
        .with_meta("view_type", json!("holistic"))
        .with_meta("request_id", json!("abc123"))
        .to_json();

    assert_eq!(response["meta"]["view_type"], "holistic");
    assert_eq!(response["meta"]["request_id"], "abc123");
}

#[test]
fn test_standard_response_all_fields() {
    let response = StandardResponse::new("my-vault", "explain_vault", json!({"data": "value"}))
        .with_count(42)
        .with_duration(125)
        .with_warning("Note: This is informational")
        .with_next_steps(&["action1", "action2"])
        .with_meta("type", json!("gestalt"))
        .to_json();

    // Verify all fields are present and correct
    assert_eq!(response["vault"], "my-vault");
    assert_eq!(response["operation"], "explain_vault");
    assert_eq!(response["success"], true);
    assert_eq!(response["count"], 42);
    assert_eq!(response["took_ms"], 125);
    assert_eq!(response["data"]["data"], "value");
    assert_eq!(response["warnings"][0], "Note: This is informational");
    assert_eq!(response["next_steps"][0], "action1");
    assert_eq!(response["next_steps"][1], "action2");
    assert_eq!(response["meta"]["type"], "gestalt");
}

#[test]
fn test_standard_response_flexible_data_types() {
    // Test with array
    let arr_response = StandardResponse::new("vault", "search", json!([1, 2, 3]))
        .with_count(3)
        .to_json();
    assert!(arr_response["data"].is_array());

    // Test with object
    let obj_response = StandardResponse::new("vault", "read", json!({"key": "value"})).to_json();
    assert!(obj_response["data"].is_object());

    // Test with string
    let str_response = StandardResponse::new("vault", "write", json!("success")).to_json();
    assert!(str_response["data"].is_string());

    // Test with null
    let null_response = StandardResponse::new("vault", "delete", json!(null)).to_json();
    assert!(null_response["data"].is_null());
}

#[test]
fn test_standard_response_serialization_format() {
    let response = StandardResponse::new("vault", "op", json!({"result": 42}))
        .with_count(1)
        .to_json();

    let json_str = response.to_string();

    // Verify it's valid JSON
    assert!(json_str.contains("\"vault\""));
    assert!(json_str.contains("\"operation\""));
    assert!(json_str.contains("\"success\""));
    assert!(json_str.contains("\"data\""));
    assert!(json_str.contains("\"count\""));
    assert!(json_str.contains("\"took_ms\""));
}

#[test]
fn test_standard_response_duration_timing() {
    let response = StandardResponse::new("vault", "op", json!({}))
        .with_duration(1234)
        .to_json();

    assert_eq!(response["took_ms"], 1234);
}

#[test]
fn test_next_steps_provide_context_aware_guidance() {
    // Simulating context-aware next steps like explain_vault does
    let orphaned_count = 16;

    let suggested_steps = if orphaned_count > 0 {
        vec!["get_dead_end_notes"]
    } else {
        vec!["search"]
    };

    let response = StandardResponse::new("vault", "explain_vault", json!({}))
        .with_next_steps(&suggested_steps)
        .to_json();

    assert_eq!(response["next_steps"][0], "get_dead_end_notes");
}
