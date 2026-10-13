//! Every result-count argument is clamped where the tool receives it.
//!
//! `advanced_search` with `limit: 0` panicked inside tantivy, and a huge
//! limit overflowed `limit * 2` or asked a collector to preallocate a vector
//! that size, which aborts the process rather than failing one call. Each
//! tool below must answer both extremes with an ordinary result.

use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;
use turbovault_core::config::VaultConfig;

async fn setup() -> (TempDir, ObsidianMcpServer) {
    let tmp = TempDir::new().unwrap();
    for i in 0..5 {
        std::fs::write(
            tmp.path().join(format!("note{i}.md")),
            format!(
                "# Note {i}\n\nShared words about vaults and links [[note{}]].\n",
                (i + 1) % 5
            ),
        )
        .unwrap();
    }
    let server = ObsidianMcpServer::new().unwrap();
    server
        .multi_vault()
        .add_vault(VaultConfig::builder("limits", tmp.path()).build().unwrap())
        .await
        .unwrap();
    server
        .multi_vault()
        .set_active_vault("limits")
        .await
        .unwrap();
    (tmp, server)
}

async fn call(server: &ObsidianMcpServer, name: &str, arguments: Value) -> Value {
    let result = server
        .call_tool(name, arguments.clone(), &RequestContext::new())
        .await
        .unwrap_or_else(|error| panic!("{name} {arguments}: {error}"));
    assert!(
        !result.is_error(),
        "{name} {arguments} returned an error: {}",
        result.first_text().unwrap_or_default()
    );
    result.structured_content.unwrap_or_default()
}

#[tokio::test]
async fn result_limits_are_clamped() {
    let (_tmp, server) = setup().await;
    let huge = json!(u64::MAX);
    let cases: Vec<(&str, Value)> = vec![
        ("advanced_search", json!({"query": "vaults"})),
        ("semantic_search", json!({"query": "vaults"})),
        ("find_similar_notes", json!({"path": "note0.md"})),
        ("find_duplicates", json!({})),
        ("find_stale_notes", json!({})),
        ("find_ungrounded_notes", json!({})),
        ("audit_log", json!({})),
    ];
    for (tool, base) in cases {
        for limit in [json!(0), huge.clone()] {
            let mut arguments = base.clone();
            arguments["limit"] = limit;
            call(&server, tool, arguments).await;
        }
    }
    for limit in [json!(0), huge.clone()] {
        call(&server, "vault_quality_report", json!({"bottom_n": limit})).await;
        call(&server, "get_hub_notes", json!({"top_n": limit})).await;
    }
    for limit in [json!(0), json!(i32::MAX), json!(i32::MIN)] {
        call(
            &server,
            "suggest_links",
            json!({"file": "note0.md", "limit": limit}),
        )
        .await;
    }
}

#[tokio::test]
async fn a_clamped_limit_still_returns_results() {
    let (_tmp, server) = setup().await;
    let response = call(
        &server,
        "advanced_search",
        json!({"query": "vaults", "limit": 0}),
    )
    .await;
    assert!(
        response["count"].as_u64().unwrap_or_default() >= 1,
        "limit 0 is read as 1, not as no results: {response}"
    );
}
