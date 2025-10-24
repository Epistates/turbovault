//! Metadata query tools for finding and extracting file metadata

use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Metadata query filter
#[derive(Debug, Clone)]
pub enum QueryFilter {
    Equals(String, Value),
    GreaterThan(String, f64),
    LessThan(String, f64),
    Contains(String, String),
    And(Vec<QueryFilter>),
    Or(Vec<QueryFilter>),
}

impl QueryFilter {
    /// Check if metadata matches this filter
    fn matches(&self, metadata: &HashMap<String, Value>) -> bool {
        match self {
            QueryFilter::Equals(key, expected) => metadata.get(key) == Some(expected),
            QueryFilter::GreaterThan(key, threshold) => {
                if let Some(Value::Number(num)) = metadata.get(key)
                    && let Some(n) = num.as_f64()
                {
                    return n > *threshold;
                }
                false
            }
            QueryFilter::LessThan(key, threshold) => {
                if let Some(Value::Number(num)) = metadata.get(key)
                    && let Some(n) = num.as_f64()
                {
                    return n < *threshold;
                }
                false
            }
            QueryFilter::Contains(key, substring) => metadata
                .get(key)
                .and_then(|v| v.as_str())
                .map(|s| s.contains(substring))
                .unwrap_or(false),
            QueryFilter::And(filters) => filters.iter().all(|f| f.matches(metadata)),
            QueryFilter::Or(filters) => filters.iter().any(|f| f.matches(metadata)),
        }
    }
}

/// Parse simple query patterns
/// Examples:
/// - 'status: "draft"' → Equals("status", String("draft"))
/// - 'priority > 3' → GreaterThan("priority", 3.0)
/// - 'priority < 5' → LessThan("priority", 5.0)
/// - 'tags: contains("important")' → Contains("tags", "important")
fn parse_query(pattern: &str) -> Result<QueryFilter> {
    let pattern = pattern.trim();

    // Try: key: "value" (equals string)
    if let Some(colon_pos) = pattern.find(':') {
        let key = pattern[..colon_pos].trim();
        let rest = pattern[colon_pos + 1..].trim();

        // Check for string literal
        if rest.starts_with('"') && rest.ends_with('"') {
            let value = rest[1..rest.len() - 1].to_string();
            return Ok(QueryFilter::Equals(key.to_string(), Value::String(value)));
        }

        // Check for contains()
        if rest.starts_with("contains(") && rest.ends_with(")") {
            let inner = &rest[9..rest.len() - 1];
            if inner.starts_with('"') && inner.ends_with('"') {
                let substring = inner[1..inner.len() - 1].to_string();
                return Ok(QueryFilter::Contains(key.to_string(), substring));
            }
        }
    }

    // Try: key > number
    if let Some(gt_pos) = pattern.find(" > ") {
        let key = pattern[..gt_pos].trim();
        let rest = pattern[gt_pos + 3..].trim();
        if let Ok(num) = rest.parse::<f64>() {
            return Ok(QueryFilter::GreaterThan(key.to_string(), num));
        }
    }

    // Try: key < number
    if let Some(lt_pos) = pattern.find(" < ") {
        let key = pattern[..lt_pos].trim();
        let rest = pattern[lt_pos + 3..].trim();
        if let Ok(num) = rest.parse::<f64>() {
            return Ok(QueryFilter::LessThan(key.to_string(), num));
        }
    }

    Err(Error::config_error(format!(
        "Unable to parse query pattern: {}",
        pattern
    )))
}

/// Metadata tools for querying and extracting file metadata
pub struct MetadataTools {
    pub manager: Arc<VaultManager>,
}

impl MetadataTools {
    /// Create new metadata tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Query files by metadata pattern
    pub async fn query_metadata(&self, pattern: &str) -> Result<Value> {
        let filter = parse_query(pattern)?;

        // Get all markdown files
        let files = self.manager.scan_vault().await?;
        let mut matches = Vec::new();

        for file_path in files {
            if !file_path.ends_with(".md") {
                continue;
            }

            // Parse file to extract frontmatter
            match self.manager.parse_file(&file_path).await {
                Ok(vault_file) => {
                    if let Some(frontmatter) = vault_file.frontmatter
                        && filter.matches(&frontmatter.data)
                    {
                        let display_path = file_path
                            .strip_prefix(self.manager.vault_path())
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| file_path.to_string_lossy().to_string());

                        matches.push(json!({
                            "path": display_path,
                            "metadata": frontmatter.data
                        }));
                    }
                }
                Err(_) => {
                    // Skip files that can't be parsed
                    continue;
                }
            }
        }

        Ok(json!({
            "query": pattern,
            "matched": matches.len(),
            "files": matches
        }))
    }

    /// Get metadata value from a file by key (supports dot notation for nested keys)
    pub async fn get_metadata_value(&self, file: &str, key: &str) -> Result<Value> {
        // Resolve file path
        let file_path = PathBuf::from(file);

        // Parse file
        let vault_file = self.manager.parse_file(&file_path).await?;

        // Extract frontmatter
        let frontmatter = vault_file
            .frontmatter
            .ok_or_else(|| Error::not_found("No frontmatter in file".to_string()))?;

        // Handle nested keys: "a.b.c" → drill down
        let mut current: &Value = &Value::Object(serde_json::Map::from_iter(
            frontmatter.data.iter().map(|(k, v)| (k.clone(), v.clone())),
        ));

        for part in key.split('.') {
            current = current
                .get(part)
                .ok_or_else(|| Error::not_found(format!("Key not found: {}", key)))?;
        }

        Ok(json!({
            "file": file,
            "key": key,
            "value": current
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_query_equals_string() {
        let filter = parse_query(r#"status: "draft""#).unwrap();
        matches!(filter, QueryFilter::Equals(_, _));
    }

    #[test]
    fn test_parse_query_greater_than() {
        let filter = parse_query("priority > 3").unwrap();
        matches!(filter, QueryFilter::GreaterThan(_, _));
    }

    #[test]
    fn test_parse_query_less_than() {
        let filter = parse_query("priority < 5").unwrap();
        matches!(filter, QueryFilter::LessThan(_, _));
    }

    #[test]
    fn test_parse_query_contains() {
        let filter = parse_query(r#"tags: contains("important")"#).unwrap();
        matches!(filter, QueryFilter::Contains(_, _));
    }

    #[test]
    fn test_filter_matches_equals() {
        let mut metadata = HashMap::new();
        metadata.insert("status".to_string(), Value::String("draft".to_string()));

        let filter = QueryFilter::Equals("status".to_string(), Value::String("draft".to_string()));
        assert!(filter.matches(&metadata));

        let filter_no_match =
            QueryFilter::Equals("status".to_string(), Value::String("active".to_string()));
        assert!(!filter_no_match.matches(&metadata));
    }

    #[test]
    fn test_filter_matches_greater_than() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "priority".to_string(),
            Value::Number(serde_json::Number::from(5)),
        );

        let filter = QueryFilter::GreaterThan("priority".to_string(), 3.0);
        assert!(filter.matches(&metadata));

        let filter_no_match = QueryFilter::GreaterThan("priority".to_string(), 5.0);
        assert!(!filter_no_match.matches(&metadata));
    }

    #[test]
    fn test_filter_matches_contains() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "tags".to_string(),
            Value::String("important task".to_string()),
        );

        let filter = QueryFilter::Contains("tags".to_string(), "important".to_string());
        assert!(filter.matches(&metadata));

        let filter_no_match = QueryFilter::Contains("tags".to_string(), "urgent".to_string());
        assert!(!filter_no_match.matches(&metadata));
    }

    #[test]
    fn test_filter_matches_and() {
        let mut metadata = HashMap::new();
        metadata.insert("status".to_string(), Value::String("draft".to_string()));
        metadata.insert(
            "priority".to_string(),
            Value::Number(serde_json::Number::from(5)),
        );

        let filter = QueryFilter::And(vec![
            QueryFilter::Equals("status".to_string(), Value::String("draft".to_string())),
            QueryFilter::GreaterThan("priority".to_string(), 3.0),
        ]);
        assert!(filter.matches(&metadata));

        let filter_no_match = QueryFilter::And(vec![
            QueryFilter::Equals("status".to_string(), Value::String("draft".to_string())),
            QueryFilter::GreaterThan("priority".to_string(), 5.0),
        ]);
        assert!(!filter_no_match.matches(&metadata));
    }

    #[test]
    fn test_filter_matches_or() {
        let mut metadata = HashMap::new();
        metadata.insert("status".to_string(), Value::String("draft".to_string()));

        let filter = QueryFilter::Or(vec![
            QueryFilter::Equals("status".to_string(), Value::String("active".to_string())),
            QueryFilter::Equals("status".to_string(), Value::String("draft".to_string())),
        ]);
        assert!(filter.matches(&metadata));

        let filter_no_match = QueryFilter::Or(vec![
            QueryFilter::Equals("status".to_string(), Value::String("archived".to_string())),
            QueryFilter::Equals("status".to_string(), Value::String("active".to_string())),
        ]);
        assert!(!filter_no_match.matches(&metadata));
    }
}
