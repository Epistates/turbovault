//! List-returning tools hand back a page and say where the next one starts.

use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;

async fn call(server: &ObsidianMcpServer, name: &str, arguments: Value) -> Value {
    let result = server
        .call_tool(name, arguments, &RequestContext::new())
        .await
        .expect("tool call");
    assert!(
        !result.is_error(),
        "{name} failed: {:?}",
        result.first_text()
    );
    result.structured_content.expect("structured content")
}

/// A hub note linked from `count` notes, `linker-000.md` onward.
async fn hub_vault(count: usize) -> (TempDir, ObsidianMcpServer) {
    let temp = TempDir::new().unwrap();
    std::fs::write(temp.path().join("hub.md"), "# Hub\n").unwrap();
    for i in 0..count {
        std::fs::write(
            temp.path().join(format!("linker-{i:03}.md")),
            "See [[hub]].\n",
        )
        .unwrap();
    }
    let server = ObsidianMcpServer::new().unwrap();
    call(
        &server,
        "add_vault",
        json!({"name": "pages", "path": temp.path().to_string_lossy()}),
    )
    .await;
    (temp, server)
}

#[tokio::test]
async fn backlinks_page_through_every_link_once() {
    let (temp, server) = hub_vault(7).await;

    let first = call(
        &server,
        "get_backlinks",
        json!({"path": "hub.md", "limit": 3}),
    )
    .await;
    let page = &first["meta"]["pagination"];
    assert_eq!(page["total"], 7);
    assert_eq!(page["returned"], 3);
    assert_eq!(page["truncated"], true);
    assert_eq!(page["next_offset"], 3);
    assert_eq!(first["count"], 3);
    assert!(
        first["warnings"][0]
            .as_str()
            .is_some_and(|w| w.contains("offset 3")),
        "{}",
        first["warnings"]
    );

    let mut seen: Vec<String> = Vec::new();
    let mut offset = 0;
    loop {
        let page = call(
            &server,
            "get_backlinks",
            json!({"path": "hub.md", "limit": 3, "offset": offset}),
        )
        .await;
        for path in page["data"].as_array().unwrap() {
            seen.push(path.as_str().unwrap().to_string());
        }
        match page["meta"]["pagination"]["next_offset"].as_u64() {
            Some(next) => offset = next as usize,
            None => break,
        }
    }
    seen.sort();
    let expected: Vec<String> = (0..7).map(|i| format!("linker-{i:03}.md")).collect();
    assert_eq!(seen, expected, "vault-relative, each once");
    assert!(
        !seen
            .iter()
            .any(|p| p.contains(&*temp.path().to_string_lossy()))
    );
}

#[tokio::test]
async fn a_short_list_is_not_truncated() {
    let (_temp, server) = hub_vault(2).await;

    let all = call(&server, "get_backlinks", json!({"path": "hub.md"})).await;
    let page = &all["meta"]["pagination"];
    assert_eq!(page["total"], 2);
    assert_eq!(page["truncated"], false);
    assert!(page.get("next_offset").is_none());
    assert!(all.get("warnings").is_none());
}

#[tokio::test]
async fn centrality_returns_the_top_of_the_ranking_by_default() {
    let (_temp, server) = hub_vault(60).await;

    let ranking = call(&server, "get_centrality_ranking", json!({})).await;
    assert_eq!(ranking["data"]["rankings"].as_array().unwrap().len(), 50);
    assert_eq!(ranking["data"]["total_files"], 61);
    assert_eq!(ranking["meta"]["pagination"]["total"], 61);
    assert_eq!(ranking["data"]["rankings"][0]["file"], "hub.md");
}

#[tokio::test]
async fn a_ranked_search_pages_without_a_total() {
    let (_temp, server) = hub_vault(5).await;

    let results = call(&server, "search", json!({"query": "hub", "limit": 2})).await;
    let page = &results["meta"]["pagination"];
    assert!(page.get("total").is_none());
    assert_eq!(page["returned"], 2);
    assert_eq!(page["next_offset"], 2);
}
