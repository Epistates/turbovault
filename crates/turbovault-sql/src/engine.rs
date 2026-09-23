//! Core SQL engine: session management, table building, query execution

use crate::convert::{json_type_name, payload_to_json};
use gluesql::prelude::{Glue, MemoryStorage, Payload};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::instrument;
use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;

/// SQL-based frontmatter query engine backed by GlueSQL.
///
/// Use [`query`](Self::query) for one-shot queries or
/// [`session`](Self::session) to build tables once and run many queries.
pub struct FrontmatterSqlEngine {
    manager: Arc<VaultManager>,
}

/// A pre-built SQL session with `files`, `tags`, and `links` tables.
///
/// Created via [`FrontmatterSqlEngine::session`]. Reuse for multiple
/// queries to avoid rebuilding the in-memory tables each time.
pub struct SqlSession {
    glue: Glue<MemoryStorage>,
    pub file_count: usize,
    pub tag_count: usize,
    pub link_count: usize,
}

impl FrontmatterSqlEngine {
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Build all tables and return a reusable session.
    #[instrument(skip(self), name = "sql_session_build")]
    pub async fn session(&self) -> Result<SqlSession> {
        let storage = MemoryStorage::default();
        let mut glue = Glue::new(storage);

        // Create all three tables
        exec(&mut glue, "CREATE TABLE files").await?;
        exec(&mut glue, "CREATE TABLE tags (path TEXT, tag TEXT)").await?;
        exec(
            &mut glue,
            "CREATE TABLE links (source TEXT, target TEXT, link_type TEXT, is_valid BOOLEAN)",
        )
        .await?;

        let files = self.manager.scan_vault().await?;
        let mut file_count = 0usize;
        let mut tag_count = 0usize;

        for file_path in &files {
            if !file_path.to_string_lossy().to_lowercase().ends_with(".md") {
                continue;
            }

            let vault_file = match self.manager.parse_file(file_path).await {
                Ok(vf) => vf,
                Err(_) => continue,
            };

            file_count += 1;

            let rel_path = self.manager.relative_path(file_path);

            // --- files table (schemaless JSON) ---
            let mut row = serde_json::Map::new();
            row.insert("path".to_string(), json!(rel_path));

            if let Some(fm) = &vault_file.frontmatter {
                for (key, value) in &fm.data {
                    row.insert(key.clone(), value.clone());
                }

                // --- tags table (unnested from frontmatter) ---
                if let Some(tags_val) = fm.data.get("tags") {
                    let tag_strings = extract_tag_strings(tags_val);
                    for tag in &tag_strings {
                        let escaped_path = rel_path.replace('\'', "''");
                        let escaped_tag = tag.replace('\'', "''");
                        let sql =
                            format!("INSERT INTO tags VALUES ('{escaped_path}', '{escaped_tag}')");
                        if let Err(e) = exec(&mut glue, &sql).await {
                            log::warn!("Tag insert error for {rel_path}: {e}");
                        } else {
                            tag_count += 1;
                        }
                    }
                }
            }

            let json_str = serde_json::to_string(&Value::Object(row))
                .map_err(|e| Error::config_error(format!("JSON serialization error: {e}")))?;
            let escaped = json_str.replace('\'', "''");
            let insert_sql = format!("INSERT INTO files VALUES ('{escaped}')");

            if let Err(e) = exec(&mut glue, &insert_sql).await {
                log::warn!("Skipping {rel_path}: insert error: {e}");
            }
        }

        // --- links table (from link graph) ---
        let link_count = self.populate_links(&mut glue).await;

        Ok(SqlSession {
            glue,
            file_count,
            tag_count,
            link_count,
        })
    }

    /// One-shot: build tables, execute SQL, discard.
    #[instrument(skip(self), fields(sql = sql), name = "sql_query")]
    pub async fn query(&self, sql: &str) -> Result<Value> {
        let mut session = self.session().await?;
        session.query(sql).await
    }

    /// Inspect the frontmatter schema across all vault files.
    #[instrument(skip(self), name = "sql_inspect")]
    pub async fn inspect(&self) -> Result<Value> {
        let files = self.manager.scan_vault().await?;
        let mut schema: HashMap<String, SchemaInfo> = HashMap::new();
        let mut file_count = 0usize;
        let mut sample_paths: Vec<String> = Vec::new();

        for file_path in &files {
            if !file_path.to_string_lossy().to_lowercase().ends_with(".md") {
                continue;
            }

            let vault_file = match self.manager.parse_file(file_path).await {
                Ok(vf) => vf,
                Err(_) => continue,
            };

            file_count += 1;

            if sample_paths.len() < 3 {
                sample_paths.push(self.manager.relative_path(file_path));
            }

            if let Some(fm) = &vault_file.frontmatter {
                for (key, value) in &fm.data {
                    let info = schema.entry(key.clone()).or_insert_with(|| SchemaInfo {
                        type_name: "null".to_string(),
                        count: 0,
                        nullable: true,
                    });
                    info.count += 1;
                    let observed = json_type_name(value);
                    if info.type_name == "null" {
                        info.type_name = observed.to_string();
                    } else if info.type_name != observed && observed != "null" {
                        info.type_name = "mixed".to_string();
                    }
                }
            }
        }

        for info in schema.values_mut() {
            info.nullable = info.count < file_count;
        }

        let mut schema_json = serde_json::Map::new();
        schema_json.insert(
            "path".to_string(),
            json!({"type": "string", "nullable": false, "count": file_count}),
        );
        for (key, info) in &schema {
            schema_json.insert(
                key.clone(),
                json!({
                    "type": info.type_name,
                    "nullable": info.nullable,
                    "count": info.count
                }),
            );
        }

        Ok(json!({
            "file_count": file_count,
            "column_count": schema_json.len(),
            "schema": schema_json,
            "tables": {
                "files": "Schemaless — one row per note with path + all frontmatter keys as columns",
                "tags": "Structured (path TEXT, tag TEXT) — unnested from frontmatter tags arrays",
                "links": "Structured (source TEXT, target TEXT, link_type TEXT, is_valid BOOLEAN) — from vault link graph"
            },
            "sample_paths": sample_paths,
            "usage": "Call query_frontmatter_sql with SQL against the files, tags, or links tables"
        }))
    }

    /// Populate the `links` table from the vault link graph.
    async fn populate_links(&self, glue: &mut Glue<MemoryStorage>) -> usize {
        let graph = self.manager.link_graph();
        let graph_read = graph.read().await;
        let all_links = graph_read.all_links();
        let mut count = 0usize;

        for (source_path, links) in &all_links {
            let source_rel = self.manager.relative_path(source_path);
            let escaped_source = source_rel.replace('\'', "''");

            for link in links {
                let escaped_target = link.target.replace('\'', "''");
                let link_type = format!("{:?}", link.type_);
                let is_valid = link.is_valid;

                let sql = format!(
                    "INSERT INTO links VALUES ('{escaped_source}', '{escaped_target}', '{link_type}', {is_valid})"
                );
                if exec(glue, &sql).await.is_ok() {
                    count += 1;
                }
            }
        }

        count
    }
}

impl SqlSession {
    /// Execute a SQL query against the pre-built tables.
    pub async fn query(&mut self, sql: &str) -> Result<Value> {
        let payloads = self
            .glue
            .execute(sql)
            .await
            .map_err(|e| Error::config_error(format!("SQL error: {e}")))?;

        let result = if payloads.len() == 1 {
            payload_to_json(payloads.into_iter().next().unwrap())
        } else {
            Value::Array(payloads.into_iter().map(payload_to_json).collect())
        };

        Ok(json!({
            "file_count": self.file_count,
            "tag_count": self.tag_count,
            "link_count": self.link_count,
            "result": result
        }))
    }
}

struct SchemaInfo {
    type_name: String,
    count: usize,
    nullable: bool,
}

/// Extract tag strings from a frontmatter value (handles arrays and comma-separated strings).
fn extract_tag_strings(value: &Value) -> Vec<String> {
    match value {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.strip_prefix('#').unwrap_or(s).to_string())
            .collect(),
        Value::String(s) => s
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

/// Execute a SQL statement, mapping errors to `turbovault_core::Error`.
async fn exec(glue: &mut Glue<MemoryStorage>, sql: &str) -> Result<Vec<Payload>> {
    glue.execute(sql)
        .await
        .map_err(|e| Error::config_error(format!("SQL error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_schemaless_roundtrip() {
        let storage = MemoryStorage::default();
        let mut glue = Glue::new(storage);

        exec(&mut glue, "CREATE TABLE test").await.unwrap();
        exec(
            &mut glue,
            r#"INSERT INTO test VALUES ('{"path": "note.md", "status": "active", "priority": 3}')"#,
        )
        .await
        .unwrap();
        exec(
            &mut glue,
            r#"INSERT INTO test VALUES ('{"path": "other.md", "status": "draft"}')"#,
        )
        .await
        .unwrap();

        let payloads = glue
            .execute("SELECT path, status FROM test WHERE status = 'active'")
            .await
            .unwrap();

        assert_eq!(payloads.len(), 1);
        if let Payload::Select { labels, rows } = &payloads[0] {
            assert_eq!(labels, &["path", "status"]);
            assert_eq!(rows.len(), 1);
        } else {
            panic!("Expected Select payload");
        }
    }

    #[tokio::test]
    async fn test_aggregation() {
        let storage = MemoryStorage::default();
        let mut glue = Glue::new(storage);

        exec(&mut glue, "CREATE TABLE test").await.unwrap();
        exec(
            &mut glue,
            r#"INSERT INTO test VALUES ('{"status": "active"}')"#,
        )
        .await
        .unwrap();
        exec(
            &mut glue,
            r#"INSERT INTO test VALUES ('{"status": "active"}')"#,
        )
        .await
        .unwrap();
        exec(
            &mut glue,
            r#"INSERT INTO test VALUES ('{"status": "draft"}')"#,
        )
        .await
        .unwrap();

        let payloads = glue
            .execute("SELECT status, COUNT(*) as cnt FROM test GROUP BY status ORDER BY cnt DESC")
            .await
            .unwrap();

        if let Payload::Select { rows, .. } = &payloads[0] {
            assert_eq!(rows.len(), 2);
        } else {
            panic!("Expected Select payload");
        }
    }

    #[tokio::test]
    async fn test_structured_tags_table() {
        let storage = MemoryStorage::default();
        let mut glue = Glue::new(storage);

        exec(&mut glue, "CREATE TABLE tags (path TEXT, tag TEXT)")
            .await
            .unwrap();
        exec(&mut glue, "INSERT INTO tags VALUES ('note.md', 'work')")
            .await
            .unwrap();
        exec(
            &mut glue,
            "INSERT INTO tags VALUES ('note.md', 'important')",
        )
        .await
        .unwrap();
        exec(&mut glue, "INSERT INTO tags VALUES ('other.md', 'work')")
            .await
            .unwrap();

        let payloads = glue
            .execute("SELECT tag, COUNT(*) as cnt FROM tags GROUP BY tag ORDER BY cnt DESC")
            .await
            .unwrap();

        if let Payload::Select { labels, rows } = &payloads[0] {
            assert_eq!(labels, &["tag", "cnt"]);
            assert_eq!(rows.len(), 2); // work=2, important=1
        } else {
            panic!("Expected Select payload");
        }
    }

    #[tokio::test]
    async fn test_join_files_and_tags() {
        let storage = MemoryStorage::default();
        let mut glue = Glue::new(storage);

        exec(&mut glue, "CREATE TABLE files").await.unwrap();
        exec(&mut glue, "CREATE TABLE tags (path TEXT, tag TEXT)")
            .await
            .unwrap();

        exec(
            &mut glue,
            r#"INSERT INTO files VALUES ('{"path": "note.md", "status": "active"}')"#,
        )
        .await
        .unwrap();
        exec(
            &mut glue,
            r#"INSERT INTO files VALUES ('{"path": "other.md", "status": "draft"}')"#,
        )
        .await
        .unwrap();
        exec(&mut glue, "INSERT INTO tags VALUES ('note.md', 'work')")
            .await
            .unwrap();

        let payloads = glue
            .execute(
                "SELECT f.path, f.status FROM files f JOIN tags t ON f.path = t.path WHERE t.tag = 'work'",
            )
            .await
            .unwrap();

        if let Payload::Select { rows, .. } = &payloads[0] {
            assert_eq!(rows.len(), 1);
        } else {
            panic!("Expected Select payload");
        }
    }

    #[tokio::test]
    async fn test_links_table() {
        let storage = MemoryStorage::default();
        let mut glue = Glue::new(storage);

        exec(
            &mut glue,
            "CREATE TABLE links (source TEXT, target TEXT, link_type TEXT, is_valid BOOLEAN)",
        )
        .await
        .unwrap();
        exec(
            &mut glue,
            "INSERT INTO links VALUES ('note.md', 'other.md', 'WikiLink', true)",
        )
        .await
        .unwrap();
        exec(
            &mut glue,
            "INSERT INTO links VALUES ('note.md', 'missing.md', 'WikiLink', false)",
        )
        .await
        .unwrap();

        let payloads = glue
            .execute("SELECT source, target FROM links WHERE is_valid = false")
            .await
            .unwrap();

        if let Payload::Select { rows, .. } = &payloads[0] {
            assert_eq!(rows.len(), 1);
        } else {
            panic!("Expected Select payload");
        }
    }

    #[test]
    fn test_extract_tag_strings_array() {
        let val = json!(["#work", "personal", "#urgent"]);
        let tags = extract_tag_strings(&val);
        assert_eq!(tags, vec!["work", "personal", "urgent"]);
    }

    #[test]
    fn test_extract_tag_strings_csv() {
        let val = json!("#work, personal, #urgent");
        let tags = extract_tag_strings(&val);
        assert_eq!(tags, vec!["work", "personal", "urgent"]);
    }

    #[test]
    fn test_extract_tag_strings_empty() {
        assert!(extract_tag_strings(&json!(null)).is_empty());
        assert!(extract_tag_strings(&json!(42)).is_empty());
    }
}
