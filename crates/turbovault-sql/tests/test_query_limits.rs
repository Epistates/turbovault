//! `query_frontmatter_sql` is a read-only tool that runs caller-written SQL.
//! It has to stay read-only, return a bounded result, and not hold a runtime
//! worker for as long as a query cares to run.

use std::sync::Arc;
use tempfile::TempDir;
use turbovault_core::{ServerConfig, VaultConfig};
use turbovault_sql::{FrontmatterSqlEngine, MAX_RESULT_ROWS};
use turbovault_vault::VaultManager;

async fn engine(notes: usize) -> (TempDir, FrontmatterSqlEngine) {
    let temp = TempDir::new().unwrap();
    for i in 0..notes {
        std::fs::write(
            temp.path().join(format!("note{i}.md")),
            format!("---\nstatus: active\nrank: {i}\n---\n# Note {i}\n"),
        )
        .unwrap();
    }
    let mut config = ServerConfig::new();
    config
        .vaults
        .push(VaultConfig::builder("sql", temp.path()).build().unwrap());
    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();
    (temp, FrontmatterSqlEngine::new(manager))
}

#[tokio::test]
async fn only_queries_run() {
    let (_temp, engine) = engine(2).await;
    for sql in [
        "DELETE FROM files",
        "INSERT INTO tags VALUES ('a.md', 'x')",
        "UPDATE tags SET tag = 'y'",
        "CREATE TABLE extra (id INTEGER)",
        "DROP TABLE files",
        "SELECT * FROM files; DELETE FROM files",
    ] {
        let error = engine.query(sql).await.expect_err(sql);
        assert!(error.to_string().contains("SELECT"), "{sql}: {error}");
    }
}

#[tokio::test]
async fn select_star_returns_rows() {
    let (_temp, engine) = engine(3).await;
    let result = engine
        .query("SELECT * FROM files WHERE status = 'active'")
        .await
        .unwrap();
    assert_eq!(result["result"]["count"], 3, "{result}");
    assert_eq!(result["result"]["rows"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn results_are_capped() {
    // 40 notes joined with themselves is 1,600 rows.
    let (_temp, engine) = engine(40).await;
    let result = engine
        .query("SELECT a.path, b.path AS other FROM files a JOIN files b")
        .await
        .unwrap();
    let rows = result["result"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), MAX_RESULT_ROWS, "{}", result["result"]["count"]);
    assert_eq!(result["result"]["truncated"], true);
    assert_eq!(result["result"]["total_rows"], 1600);
}
