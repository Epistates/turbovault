//! Metadata query tools for finding and extracting file metadata

use crate::file_tools::{deep_merge, reconstruct_content, split_frontmatter};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_core::Precondition;
use turbovault_core::prelude::*;
use turbovault_parser::parse_tags;
use turbovault_vault::VaultManager;

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
        let vault_files = self.manager.vault_files_validated().await;
        let mut matches = Vec::new();

        for vault_file in vault_files {
            if !vault_file
                .path
                .to_string_lossy()
                .to_lowercase()
                .ends_with(".md")
            {
                continue;
            }
            if let Some(frontmatter) = vault_file.frontmatter
                && filter.matches(&frontmatter.data)
            {
                let display_path = self.manager.relative_path(&vault_file.path);

                matches.push(json!({
                    "path": display_path,
                    "metadata": frontmatter.data
                }));
            }
        }

        Ok(json!({
            "query": pattern,
            "matched": matches.len(),
            "files": matches
        }))
    }

    /// Update frontmatter of a note without touching content
    ///
    /// # Arguments
    /// * `path` - Relative path to note in vault
    /// * `frontmatter` - JSON object with frontmatter keys to set
    /// * `merge` - If true, deep-merge into existing frontmatter. If false, replace entirely.
    pub async fn update_frontmatter(
        &self,
        path: &str,
        frontmatter: serde_json::Map<String, Value>,
        merge: bool,
    ) -> Result<Value> {
        let (new_content, info) = self
            .compute_update_frontmatter(path, frontmatter, merge)
            .await?;
        let file_path = PathBuf::from(path);
        self.manager
            .write_file(
                &file_path,
                &new_content,
                Precondition::for_in_place(None),
                &format!("update_frontmatter {path}"),
            )
            .await?;
        Ok(info)
    }

    /// turbovault-gje: pure compute variant of [`Self::update_frontmatter`]
    /// — does the read + deep merge + reconstruct work and returns the
    /// new content + summary JSON, but does NOT write. Lets the MCP
    /// layer route the write through `WriteTools` (substrate-aware)
    /// instead of the `VaultManager`-direct path that bypassed the git
    /// backend's commit machinery.
    pub async fn compute_update_frontmatter(
        &self,
        path: &str,
        frontmatter: serde_json::Map<String, Value>,
        merge: bool,
    ) -> Result<(String, Value)> {
        let file_path = PathBuf::from(path);
        let content = self.manager.read_file(&file_path).await?;

        let (existing_yaml, body) = split_frontmatter(&content);

        let existing_fm: serde_json::Map<String, Value> = if let Some(yaml_str) = &existing_yaml {
            yaml_serde::from_str(yaml_str).map_err(|e| {
                Error::config_error(format!("Failed to parse existing frontmatter YAML: {}", e))
            })?
        } else {
            serde_json::Map::new()
        };

        let final_fm = if merge {
            let overlay = Value::Object(frontmatter);
            let mut base = Value::Object(existing_fm);
            deep_merge(&mut base, overlay);
            match base {
                Value::Object(map) => map,
                _ => serde_json::Map::new(),
            }
        } else {
            frontmatter
        };

        let new_content = reconstruct_content(Some(&final_fm), &body);

        let info = json!({
            "path": path,
            "status": "updated",
            "merge": merge,
            "keys_set": final_fm.keys().collect::<Vec<_>>()
        });
        Ok((new_content, info))
    }

    /// Manage tags on a note (add, remove, list)
    ///
    /// - "list": Returns both frontmatter tags AND inline #tags from content
    /// - "add": Adds tags to frontmatter tags array (creates if missing)
    /// - "remove": Removes tags from frontmatter tags array
    pub async fn manage_tags(
        &self,
        path: &str,
        operation: &str,
        tags: Option<&[String]>,
    ) -> Result<Value> {
        let (maybe_write, info) = self.compute_manage_tags(path, operation, tags).await?;
        if let Some(new_content) = maybe_write {
            let file_path = PathBuf::from(path);
            self.manager
                .write_file(
                    &file_path,
                    &new_content,
                    Precondition::for_in_place(None),
                    &format!("manage_tags {path}"),
                )
                .await?;
        }
        Ok(info)
    }

    /// turbovault-gje: pure compute variant of [`Self::manage_tags`].
    /// Returns `(Some(new_content), info)` for add/remove (write needed)
    /// and `(None, info)` for `list` (read-only). Lets the MCP layer
    /// route writes through `WriteTools` instead of `VaultManager`.
    pub async fn compute_manage_tags(
        &self,
        path: &str,
        operation: &str,
        tags: Option<&[String]>,
    ) -> Result<(Option<String>, Value)> {
        let file_path = PathBuf::from(path);
        let content = self.manager.read_file(&file_path).await?;

        match operation {
            "list" => {
                // Get frontmatter tags
                let (yaml_str, body) = split_frontmatter(&content);
                let fm_tags: Vec<String> = if let Some(yaml) = &yaml_str {
                    let fm: serde_json::Map<String, Value> =
                        yaml_serde::from_str(yaml).map_err(|e| {
                            Error::config_error(format!("Failed to parse frontmatter YAML: {}", e))
                        })?;
                    extract_tags_from_value(fm.get("tags"))
                } else {
                    vec![]
                };

                // Get inline tags from content
                let parsed_tags = parse_tags(&body);
                let inline_tags: Vec<String> = parsed_tags.into_iter().map(|t| t.name).collect();

                // Deduplicate, preserving order
                let mut seen = HashSet::new();
                let mut all_tags = Vec::new();
                for tag in fm_tags.iter().chain(inline_tags.iter()) {
                    let normalized = tag.strip_prefix('#').unwrap_or(tag).to_string();
                    if seen.insert(normalized.clone()) {
                        all_tags.push(normalized);
                    }
                }

                Ok((
                    None,
                    json!({
                        "path": path,
                        "frontmatter_tags": fm_tags,
                        "inline_tags": inline_tags,
                        "all_tags": all_tags
                    }),
                ))
            }
            "add" => {
                let tags_to_add = tags.ok_or_else(|| {
                    Error::config_error("Tags array required for 'add' operation".to_string())
                })?;

                let (yaml_str, body) = split_frontmatter(&content);
                let mut fm: serde_json::Map<String, Value> = if let Some(yaml) = &yaml_str {
                    yaml_serde::from_str(yaml).map_err(|e| {
                        Error::config_error(format!("Failed to parse frontmatter YAML: {}", e))
                    })?
                } else {
                    serde_json::Map::new()
                };

                // Get or create tags array
                let mut existing_tags = extract_tags_from_value(fm.get("tags"));
                for tag in tags_to_add {
                    let normalized = tag.strip_prefix('#').unwrap_or(tag).to_string();
                    if !existing_tags.contains(&normalized) {
                        existing_tags.push(normalized);
                    }
                }

                fm.insert(
                    "tags".to_string(),
                    Value::Array(
                        existing_tags
                            .iter()
                            .map(|t| Value::String(t.clone()))
                            .collect(),
                    ),
                );

                let new_content = reconstruct_content(Some(&fm), &body);
                Ok((
                    Some(new_content),
                    json!({
                        "path": path,
                        "operation": "add",
                        "tags": existing_tags,
                        "status": "updated"
                    }),
                ))
            }
            "remove" => {
                let tags_to_remove = tags.ok_or_else(|| {
                    Error::config_error("Tags array required for 'remove' operation".to_string())
                })?;

                let (yaml_str, body) = split_frontmatter(&content);
                let mut fm: serde_json::Map<String, Value> = if let Some(yaml) = &yaml_str {
                    yaml_serde::from_str(yaml).map_err(|e| {
                        Error::config_error(format!("Failed to parse frontmatter YAML: {}", e))
                    })?
                } else {
                    return Ok((
                        None,
                        json!({
                            "path": path,
                            "operation": "remove",
                            "tags": [],
                            "status": "no_frontmatter"
                        }),
                    ));
                };

                let existing_tags = extract_tags_from_value(fm.get("tags"));
                let remove_set: HashSet<String> = tags_to_remove
                    .iter()
                    .map(|t| t.strip_prefix('#').unwrap_or(t).to_string())
                    .collect();

                let remaining: Vec<String> = existing_tags
                    .into_iter()
                    .filter(|t| !remove_set.contains(t))
                    .collect();

                fm.insert(
                    "tags".to_string(),
                    Value::Array(remaining.iter().map(|t| Value::String(t.clone())).collect()),
                );

                let new_content = reconstruct_content(Some(&fm), &body);
                Ok((
                    Some(new_content),
                    json!({
                        "path": path,
                        "operation": "remove",
                        "tags": remaining,
                        "status": "updated"
                    }),
                ))
            }
            other => Err(Error::config_error(format!(
                "Invalid tag operation '{}'. Must be 'add', 'remove', or 'list'",
                other
            ))),
        }
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

/// Extract tags from a frontmatter Value (handles both array and string forms)
/// Always normalizes by stripping leading `#` prefix
fn extract_tags_from_value(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .map(|s| s.strip_prefix('#').unwrap_or(s).to_string())
            })
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(|t| {
                let trimmed = t.trim();
                trimmed.strip_prefix('#').unwrap_or(trimmed).to_string()
            })
            .filter(|t| !t.is_empty())
            .collect(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_query_equals_string() {
        let filter = parse_query(r#"status: "draft""#).unwrap();
        assert!(matches!(filter, QueryFilter::Equals(_, _)));
    }

    #[test]
    fn test_parse_query_greater_than() {
        let filter = parse_query("priority > 3").unwrap();
        assert!(matches!(filter, QueryFilter::GreaterThan(_, _)));
    }

    #[test]
    fn test_parse_query_less_than() {
        let filter = parse_query("priority < 5").unwrap();
        assert!(matches!(filter, QueryFilter::LessThan(_, _)));
    }

    #[test]
    fn test_parse_query_contains() {
        let filter = parse_query(r#"tags: contains("important")"#).unwrap();
        assert!(matches!(filter, QueryFilter::Contains(_, _)));
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
    fn test_extract_tags_strips_hash_prefix() {
        let val = serde_json::json!(["#work", "personal", "#urgent"]);
        let tags = extract_tags_from_value(Some(&val));
        assert_eq!(tags, vec!["work", "personal", "urgent"]);
    }

    #[test]
    fn test_extract_tags_from_comma_string() {
        let val = serde_json::json!("#work, personal, #urgent");
        let tags = extract_tags_from_value(Some(&val));
        assert_eq!(tags, vec!["work", "personal", "urgent"]);
    }

    // ==================== extract_tags_from_value edge cases ====================

    #[test]
    fn test_extract_tags_from_value_none() {
        let tags = extract_tags_from_value(None);
        assert!(tags.is_empty());
    }

    #[test]
    fn test_extract_tags_from_value_null() {
        let val = Value::Null;
        let tags = extract_tags_from_value(Some(&val));
        assert!(tags.is_empty());
    }

    #[test]
    fn test_extract_tags_from_value_number() {
        let val = serde_json::json!(42);
        let tags = extract_tags_from_value(Some(&val));
        assert!(tags.is_empty());
    }

    #[test]
    fn test_extract_tags_from_value_empty_array() {
        let val = serde_json::json!([]);
        let tags = extract_tags_from_value(Some(&val));
        assert!(tags.is_empty());
    }

    #[test]
    fn test_extract_tags_from_value_array_with_non_string_elements() {
        let val = serde_json::json!([1, null, "valid", true]);
        let tags = extract_tags_from_value(Some(&val));
        assert_eq!(tags, vec!["valid"]);
    }

    #[test]
    fn test_extract_tags_from_value_comma_string_empty_segments() {
        let val = serde_json::json!(" , ,,");
        let tags = extract_tags_from_value(Some(&val));
        assert!(tags.is_empty());
    }

    #[test]
    fn test_extract_tags_from_value_comma_string_whitespace() {
        let val = serde_json::json!(" work , personal ");
        let tags = extract_tags_from_value(Some(&val));
        assert_eq!(tags, vec!["work", "personal"]);
    }

    // ==================== parse_query edge cases ====================

    #[test]
    fn test_parse_query_contains_missing_quotes() {
        // contains(important) without quotes should fail
        let result = parse_query(r#"tags: contains(important)"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_query_key_with_colon_in_value() {
        let filter = parse_query(r#"url: "http://example.com""#).unwrap();
        assert!(matches!(filter, QueryFilter::Equals(_, _)));
        if let QueryFilter::Equals(key, Value::String(val)) = filter {
            assert_eq!(key, "url");
            assert_eq!(val, "http://example.com");
        } else {
            panic!("Expected Equals filter");
        }
    }

    #[test]
    fn test_parse_query_leading_trailing_whitespace() {
        let filter = parse_query(r#"  status: "draft"  "#).unwrap();
        assert!(matches!(filter, QueryFilter::Equals(_, _)));
    }

    // ==================== QueryFilter::matches edge cases ====================

    #[test]
    fn test_filter_matches_greater_than_missing_key() {
        let metadata = HashMap::new();
        let filter = QueryFilter::GreaterThan("missing".to_string(), 3.0);
        assert!(!filter.matches(&metadata));
    }

    #[test]
    fn test_filter_matches_greater_than_non_numeric() {
        let mut metadata = HashMap::new();
        metadata.insert("priority".to_string(), Value::String("high".to_string()));
        let filter = QueryFilter::GreaterThan("priority".to_string(), 3.0);
        assert!(!filter.matches(&metadata));
    }

    #[test]
    fn test_filter_matches_less_than_boundary() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "priority".to_string(),
            Value::Number(serde_json::Number::from(5)),
        );
        // Exactly equal should NOT match LessThan
        let filter = QueryFilter::LessThan("priority".to_string(), 5.0);
        assert!(!filter.matches(&metadata));
    }

    #[test]
    fn test_filter_matches_contains_non_string_value() {
        let mut metadata = HashMap::new();
        metadata.insert("tags".to_string(), serde_json::json!(["a", "b"]));
        let filter = QueryFilter::Contains("tags".to_string(), "a".to_string());
        assert!(!filter.matches(&metadata)); // Array is not a string
    }

    #[test]
    fn test_filter_matches_and_empty() {
        let metadata = HashMap::new();
        let filter = QueryFilter::And(vec![]);
        assert!(filter.matches(&metadata)); // all() on empty is true
    }

    #[test]
    fn test_filter_matches_or_empty() {
        let metadata = HashMap::new();
        let filter = QueryFilter::Or(vec![]);
        assert!(!filter.matches(&metadata)); // any() on empty is false
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

    // ── query_metadata integration ───────────────────────────────────────────

    fn make_manager(vault_dir: &std::path::Path) -> std::sync::Arc<turbovault_vault::VaultManager> {
        use turbovault_core::{ServerConfig, VaultConfig};
        let mut config = ServerConfig::new();
        config
            .vaults
            .push(VaultConfig::builder("test", vault_dir).build().unwrap());
        std::sync::Arc::new(turbovault_vault::VaultManager::new(config).unwrap())
    }

    #[tokio::test]
    async fn test_query_metadata_returns_matching_files() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        std::fs::write(
            temp_dir.path().join("match.md"),
            "---\nstatus: \"active\"\n---\n# Match",
        )
        .unwrap();
        std::fs::write(
            temp_dir.path().join("no_match.md"),
            "---\nstatus: \"draft\"\n---\n# No match",
        )
        .unwrap();

        let manager = make_manager(temp_dir.path());
        manager.initialize().await.unwrap();
        let tools = MetadataTools::new(manager);

        let result = tools.query_metadata(r#"status: "active""#).await.unwrap();
        assert_eq!(result["matched"], 1);
        assert_eq!(result["files"][0]["metadata"]["status"], "active");
    }

    #[tokio::test]
    async fn test_query_metadata_returns_empty_on_no_match() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join("note.md"),
            "---\nstatus: \"draft\"\n---\n# Note",
        )
        .unwrap();

        let manager = make_manager(temp_dir.path());
        manager.initialize().await.unwrap();
        let tools = MetadataTools::new(manager);

        let result = tools
            .query_metadata(r#"status: "published""#)
            .await
            .unwrap();
        assert_eq!(result["matched"], 0);
    }

    #[tokio::test]
    async fn test_query_metadata_reflects_external_modification() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("note.md");
        std::fs::write(&path, "---\nstatus: \"draft\"\n---\n# Note").unwrap();

        let manager = make_manager(temp_dir.path());
        manager.initialize().await.unwrap();
        let tools = MetadataTools::new(manager);

        // Initially matches "draft", not "published".
        let before = tools.query_metadata(r#"status: "draft""#).await.unwrap();
        assert_eq!(before["matched"], 1);

        // External write — sleep to ensure mtime advances.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        std::fs::write(&path, "---\nstatus: \"published\"\n---\n# Note").unwrap();

        // After external modification, cache is invalidated and query reflects new state.
        let after_draft = tools.query_metadata(r#"status: "draft""#).await.unwrap();
        let after_published = tools
            .query_metadata(r#"status: "published""#)
            .await
            .unwrap();
        assert_eq!(after_draft["matched"], 0);
        assert_eq!(after_published["matched"], 1);
    }
}
