//! A profile's config reaches the vaults.
//!
//! Every vault manager used to be built from `ServerConfig::default()`, so
//! nothing `--profile` chose could reach a vault (#88). The server now builds
//! them from the config it was constructed with.

use turbomcp::prelude::*;
use turbovault::ObsidianMcpServer;
use turbovault_core::ServerConfig;
use turbovault_core::config::VaultConfig;

#[tokio::test]
async fn vault_managers_start_from_the_server_config() {
    let temp = tempfile::TempDir::new().expect("temp vault");
    std::fs::write(temp.path().join("big.md"), "x".repeat(64)).expect("seed note");

    let config = ServerConfig {
        max_file_size: 16,
        ..ServerConfig::default()
    };
    let server = ObsidianMcpServer::with_config(config).expect("server");
    let vault = VaultConfig::builder("profiled", temp.path())
        .build()
        .expect("vault config");
    server
        .multi_vault()
        .add_vault(vault)
        .await
        .expect("register");
    server
        .multi_vault()
        .set_active_vault("profiled")
        .await
        .expect("select");

    let result = server
        .call_tool(
            "read_note",
            serde_json::json!({"path": "big.md"}),
            &RequestContext::new(),
        )
        .await;
    let refused = match &result {
        Ok(tool_result) => tool_result.is_error.unwrap_or(false),
        Err(_) => true,
    };
    assert!(
        refused,
        "a 64-byte note is over the server's 16-byte limit: {result:?}"
    );
}
