//! Output formatting for different transport types
//!
//! Provides human-readable, JSON, and text output formats for HTTP/WebSocket/TCP transports.
//! STDIO transport always uses JSON per MCP protocol specification.

use serde_json::Value;
use std::fmt;
use std::str::FromStr;

/// Output format preference for HTTP/WebSocket/TCP transports
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// JSON format (default, also required for STDIO transport)
    #[default]
    Json,
    /// Human-readable format with pretty-printed output
    Human,
    /// Plain text format for terminal output
    Text,
}

impl FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "json" => Ok(OutputFormat::Json),
            "human" => Ok(OutputFormat::Human),
            "text" => Ok(OutputFormat::Text),
            _ => Err(format!(
                "Unknown output format '{}'. Valid options: json, human, text",
                s
            )),
        }
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OutputFormat::Json => write!(f, "json"),
            OutputFormat::Human => write!(f, "human"),
            OutputFormat::Text => write!(f, "text"),
        }
    }
}

/// Formatter for converting responses to different formats
pub struct ResponseFormatter;

impl ResponseFormatter {
    /// Format a JSON response according to the output format preference
    pub fn format(response: &Value, format: OutputFormat) -> String {
        match format {
            OutputFormat::Json => Self::format_json(response),
            OutputFormat::Human => Self::format_human(response),
            OutputFormat::Text => Self::format_text(response),
        }
    }

    /// Format as JSON (pretty-printed)
    fn format_json(response: &Value) -> String {
        serde_json::to_string_pretty(response).unwrap_or_else(|_| response.to_string())
    }

    /// Format as human-readable output
    fn format_human(response: &Value) -> String {
        let mut output = String::new();

        // Extract key information from standard response structure
        if let Some(obj) = response.as_object() {
            // Vault name
            if let Some(vault) = obj.get("vault").and_then(|v| v.as_str()) {
                output.push_str(&format!("📦 Vault: {}\n", vault));
            }

            // Operation
            if let Some(op) = obj.get("operation").and_then(|v| v.as_str()) {
                output.push_str(&format!("⚙️  Operation: {}\n", op));
            }

            // Success indicator
            if let Some(success) = obj.get("success").and_then(|v| v.as_bool()) {
                let status = if success { "✅ Success" } else { "❌ Failed" };
                output.push_str(&format!("Status: {}\n", status));
            }

            output.push('\n');

            // Data section
            if let Some(data) = obj.get("data") {
                output.push_str("📊 Data:\n");
                output.push_str(&Self::format_value_indented(data, 2));
            }

            // Warnings
            if let Some(warnings) = obj.get("warnings").and_then(|v| v.as_array())
                && !warnings.is_empty()
            {
                output.push_str("\n⚠️  Warnings:\n");
                for warning in warnings {
                    if let Some(msg) = warning.as_str() {
                        output.push_str(&format!("  • {}\n", msg));
                    }
                }
            }

            // Next steps
            if let Some(steps) = obj.get("next_steps").and_then(|v| v.as_array())
                && !steps.is_empty()
            {
                output.push_str("\n👉 Next Steps:\n");
                for (i, step) in steps.iter().enumerate() {
                    if let Some(s) = step.as_str() {
                        output.push_str(&format!("  {}. {}\n", i + 1, s));
                    }
                }
            }

            // Performance metric
            if let Some(took) = obj.get("took_ms").and_then(|v| v.as_u64()) {
                output.push_str(&format!("\n⏱️  Took: {}ms\n", took));
            }

            // Count if present
            if let Some(count) = obj.get("count").and_then(|v| v.as_u64()) {
                output.push_str(&format!("Count: {}\n", count));
            }
        }

        if output.is_empty() {
            Self::format_json(response)
        } else {
            output
        }
    }

    /// Format as plain text
    fn format_text(response: &Value) -> String {
        let mut output = String::new();

        if let Some(obj) = response.as_object() {
            // Minimal output - just the key facts
            if let Some(success) = obj.get("success").and_then(|v| v.as_bool()) {
                output.push_str(if success {
                    "✓ Success\n"
                } else {
                    "✗ Failed\n"
                });
            }

            if let Some(op) = obj.get("operation").and_then(|v| v.as_str()) {
                output.push_str(&format!("{}\n", op));
            }

            // Brief data summary
            if let Some(data) = obj.get("data") {
                match data {
                    Value::Object(map) => {
                        for (key, value) in map.iter().take(5) {
                            output.push_str(&format!("{}: ", key));
                            match value {
                                Value::String(s) => output.push_str(&format!("{}\n", s)),
                                Value::Number(n) => output.push_str(&format!("{}\n", n)),
                                Value::Bool(b) => output.push_str(&format!("{}\n", b)),
                                Value::Array(arr) => {
                                    output.push_str(&format!("[{} items]\n", arr.len()))
                                }
                                _ => output.push_str("...\n"),
                            }
                        }
                    }
                    Value::Array(arr) => {
                        output.push_str(&format!("[{} items]\n", arr.len()));
                    }
                    Value::String(s) => output.push_str(&format!("{}\n", s)),
                    _ => {}
                }
            }

            if let Some(took) = obj.get("took_ms").and_then(|v| v.as_u64()) {
                output.push_str(&format!("({} ms)\n", took));
            }
        }

        if output.is_empty() {
            Self::format_json(response)
        } else {
            output
        }
    }

    /// Helper to format a value with indentation
    fn format_value_indented(value: &Value, indent: usize) -> String {
        let indent_str = " ".repeat(indent);

        match value {
            Value::Object(map) => {
                let mut result = String::new();
                for (key, val) in map.iter() {
                    result.push_str(&format!("{}{}: ", indent_str, key));
                    match val {
                        Value::String(s) => result.push_str(&format!("{}\n", s)),
                        Value::Number(n) => result.push_str(&format!("{}\n", n)),
                        Value::Bool(b) => result.push_str(&format!("{}\n", b)),
                        Value::Array(arr) => {
                            result.push_str(&format!("[{} items]\n", arr.len()));
                            for (i, item) in arr.iter().take(3).enumerate() {
                                result.push_str(&format!("{}  [{}] {}\n", indent_str, i, item));
                            }
                            if arr.len() > 3 {
                                result.push_str(&format!(
                                    "{}  ... and {} more\n",
                                    indent_str,
                                    arr.len() - 3
                                ));
                            }
                        }
                        Value::Object(_) => {
                            result.push_str(&format!(
                                "{}\n",
                                Self::format_value_indented(val, indent + 2)
                            ));
                        }
                        Value::Null => result.push_str("null\n"),
                    }
                }
                result
            }
            Value::Array(arr) => {
                let mut result = String::from("[\n");
                for (i, item) in arr.iter().take(5).enumerate() {
                    result.push_str(&format!("{}  [{}] {}\n", indent_str, i, item));
                }
                if arr.len() > 5 {
                    result.push_str(&format!("{}  ... and {} more\n", indent_str, arr.len() - 5));
                }
                result.push_str(&format!("{}]\n", indent_str));
                result
            }
            Value::String(s) => format!("{}\n", s),
            other => format!("{}\n", other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_output_format_parse() {
        assert_eq!(OutputFormat::from_str("json").unwrap(), OutputFormat::Json);
        assert_eq!(OutputFormat::from_str("JSON").unwrap(), OutputFormat::Json);
        assert_eq!(
            OutputFormat::from_str("human").unwrap(),
            OutputFormat::Human
        );
        assert_eq!(OutputFormat::from_str("text").unwrap(), OutputFormat::Text);
        assert!(OutputFormat::from_str("invalid").is_err());
    }

    #[test]
    fn test_format_json() {
        let response = json!({
            "success": true,
            "data": {"test": "value"}
        });
        let formatted = ResponseFormatter::format(&response, OutputFormat::Json);
        assert!(formatted.contains("\"success\": true"));
    }

    #[test]
    fn test_format_human() {
        let response = json!({
            "vault": "personal",
            "operation": "read_note",
            "success": true,
            "data": {"content": "test"},
            "took_ms": 42
        });
        let formatted = ResponseFormatter::format(&response, OutputFormat::Human);
        assert!(formatted.contains("Vault: personal"));
        assert!(formatted.contains("✅ Success"));
        assert!(formatted.contains("42ms"));
    }

    #[test]
    fn test_format_text() {
        let response = json!({
            "success": true,
            "operation": "search",
            "data": [1, 2, 3]
        });
        let formatted = ResponseFormatter::format(&response, OutputFormat::Text);
        assert!(formatted.contains("✓ Success"));
        assert!(formatted.contains("search"));
    }
}
