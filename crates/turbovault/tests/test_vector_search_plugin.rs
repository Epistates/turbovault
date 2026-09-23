//! End-to-end coverage for the `vector_search` compiled-in plugin, through
//! the real MCP plugin boundary: `ObsidianMcpServer::new_with_plugins`, a
//! real vault on disk, and the real `FilePluginStore`/`PluginVaultHost`
//! bridge `crates/turbovault/src/tools/plugin_host.rs` and
//! `plugin_storage.rs` provide.
//!
//! The one thing this deliberately does not exercise is `Model2VecEmbedder`
//! itself: it would need a real model directory, which means either a
//! network fetch or a checked-in model file, and the project's own test
//! policy is no network access and no downloaded models in tests. Every test
//! here builds `VectorSearchPlugin::with_embedder_factory` with
//! `turbovault_vector::embedding::testing::DeterministicEmbedder` instead —
//! everything *around* the embedder (tool namespacing, storage, the change
//! feed, reconciliation) is real.

#![cfg(feature = "vector-search")]

use std::sync::Arc;

use turbomcp::prelude::*;
use turbovault::ObsidianMcpServer;
use turbovault_core::VaultConfig;
use turbovault_plugin_vector::VectorSearchPlugin;
use turbovault_vector::EmbeddingEngine;
use turbovault_vector::embedding::testing::DeterministicEmbedder;

const DIMS: usize = 32;

fn fake_plugin() -> VectorSearchPlugin {
    VectorSearchPlugin::with_embedder_factory(|_config| {
        Ok(Arc::new(DeterministicEmbedder::new(DIMS)) as Arc<dyn EmbeddingEngine>)
    })
}

fn structured(result: ToolResult) -> serde_json::Value {
    result
        .structured_content
        .expect("tool result should contain structured content")
}

async fn harness() -> (tempfile::TempDir, ObsidianMcpServer) {
    let temp = tempfile::TempDir::new().expect("temp vault");
    let server = ObsidianMcpServer::new_with_plugins(vec![Arc::new(fake_plugin())])
        .expect("plugin composition");
    let config = VaultConfig::builder("vector-e2e", temp.path())
        .build()
        .expect("vault config");
    server
        .multi_vault()
        .add_vault(config)
        .await
        .expect("register vault");
    server
        .multi_vault()
        .set_active_vault("vector-e2e")
        .await
        .expect("select vault");
    (temp, server)
}

#[tokio::test]
async fn vector_search_tools_are_namespaced_and_searchable() {
    let (_temp, server) = harness().await;
    let ctx = RequestContext::new();

    let advertised = server.list_tools();
    for name in [
        "vector_search_search",
        "vector_search_reindex",
        "vector_search_status",
    ] {
        assert!(
            advertised.iter().any(|tool| tool.name == name),
            "missing {name}"
        );
    }

    server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "alpha.md", "content": "# Alpha\n\nA unique sentence about kangaroos."}),
            &ctx,
        )
        .await
        .expect("write alpha");
    server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "beta.md", "content": "# Beta\n\nAn unrelated note about the weather."}),
            &ctx,
        )
        .await
        .expect("write beta");

    let result = structured(
        server
            .call_tool(
                "vector_search_search",
                serde_json::json!({"query": "kangaroos", "limit": 5}),
                &ctx,
            )
            .await
            .expect("search"),
    );
    // The fake embedder is a crude byte-hash with no real semantics, so the
    // dense side can score an unrelated note above `min_similarity` by
    // chance; only the lexical (BM25) side reliably tells the two notes
    // apart here. Assert what hybrid mode guarantees: the lexical match
    // ranks first, not that dense noise contributed nothing.
    assert!(result["count"].as_u64().unwrap() >= 1);
    assert_eq!(result["results"][0]["path"], "alpha.md");
}

#[tokio::test]
async fn vector_search_status_reflects_index_growth_without_forcing_a_build() {
    let (_temp, server) = harness().await;
    let ctx = RequestContext::new();

    let before = structured(
        server
            .call_tool("vector_search_status", serde_json::json!({}), &ctx)
            .await
            .expect("status before anything is indexed"),
    );
    assert_eq!(before["indexed_notes"], 0);
    // `configured` reflects VectorConfig::model_path specifically (a
    // Model2Vec concept), not "will the embedder factory work" — the fake
    // factory ignores config entirely, so this is correctly `false` even
    // though search works. See engine_for's lazy build for what actually
    // gates whether search succeeds.
    assert_eq!(before["configured"], false);

    server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "note.md", "content": "Some searchable content."}),
            &ctx,
        )
        .await
        .expect("write");
    // vector_search_search reconciles before answering, so this alone builds
    // and populates the engine; no explicit reindex needed.
    server
        .call_tool(
            "vector_search_search",
            serde_json::json!({"query": "content"}),
            &ctx,
        )
        .await
        .expect("search triggers reconcile");

    let after = structured(
        server
            .call_tool("vector_search_status", serde_json::json!({}), &ctx)
            .await
            .expect("status after indexing"),
    );
    assert_eq!(after["indexed_notes"], 1);
    assert_eq!(after["dims"], DIMS);
}

#[tokio::test]
async fn vector_search_reindex_forces_a_full_rebuild() {
    let (_temp, server) = harness().await;
    let ctx = RequestContext::new();

    server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "note.md", "content": "Stable content."}),
            &ctx,
        )
        .await
        .expect("write");
    server
        .call_tool(
            "vector_search_search",
            serde_json::json!({"query": "content"}),
            &ctx,
        )
        .await
        .expect("initial index");

    let reindexed = structured(
        server
            .call_tool("vector_search_reindex", serde_json::json!({}), &ctx)
            .await
            .expect("reindex"),
    );
    assert_eq!(reindexed["reindexed_notes"], 1);

    let status = structured(
        server
            .call_tool("vector_search_status", serde_json::json!({}), &ctx)
            .await
            .expect("status"),
    );
    assert_eq!(status["indexed_notes"], 1);
}

#[tokio::test]
async fn vector_search_reacts_to_writes_made_through_a_different_tool() {
    // Exercises the background worker specifically: a write through the core
    // `write_note` tool (a different provider entirely) still reaches the
    // index via the shared hook bus, with no caller ever invoking a
    // vector_search tool to make it happen.
    let (_temp, server) = harness().await;
    let ctx = RequestContext::new();
    server.start_plugins().await.expect("start plugins");

    server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "external.md", "content": "Indexed via the change feed, not a direct call."}),
            &ctx,
        )
        .await
        .expect("write");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let status = structured(
            server
                .call_tool("vector_search_status", serde_json::json!({}), &ctx)
                .await
                .expect("status"),
        );
        if status["indexed_notes"].as_u64() == Some(1) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "background worker did not index the note via the hook bus in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// The other half of "a default build does not advertise these tools" (see
/// `crates/turbovault/src/cli.rs`'s `mounts_no_plugins_without_a_plugin_feature`,
/// which proves the production wiring mounts nothing without the feature).
/// This proves the complementary fact: even with the `vector-search` Cargo
/// feature compiled in, `ObsidianMcpServer::new()` — what every other test
/// in this crate builds against — mounts no plugins at all, so having the
/// feature available does not by itself change any existing test's or
/// caller's tool list. Only `new_with_plugins` (called from `cli.rs`, gated
/// the same way) does.
#[tokio::test]
async fn compiling_in_the_feature_does_not_by_itself_advertise_the_tools() {
    let server = ObsidianMcpServer::new().expect("provider composition");
    assert!(
        !server
            .list_tools()
            .iter()
            .any(|tool| tool.name.starts_with("vector_search_"))
    );
}
