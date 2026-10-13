//! The optional `vault` argument every vault-scoped tool takes: a call names
//! the vault it runs against without changing the server's active vault.

use serde_json::{Value, json};
use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;

async fn call(server: &ObsidianMcpServer, name: &str, arguments: Value) -> Value {
    let result = server
        .call_tool(name, arguments, &RequestContext::new())
        .await
        .unwrap_or_else(|error| panic!("{name} failed: {error}"));
    assert!(
        !result.is_error(),
        "{name} returned an error: {}",
        result.first_text().unwrap_or_default()
    );
    result.structured_content.expect("structured content")
}

async fn call_error(server: &ObsidianMcpServer, name: &str, arguments: Value) -> String {
    match server
        .call_tool(name, arguments, &RequestContext::new())
        .await
    {
        Ok(result) if result.is_error() => result.first_text().unwrap_or_default().to_string(),
        Ok(_) => panic!("{name} unexpectedly succeeded"),
        Err(error) => error.to_string(),
    }
}

/// A server with vaults `alpha` and `beta`, `alpha` active.
async fn two_vaults() -> (TempDir, TempDir, ObsidianMcpServer) {
    let alpha = TempDir::new().unwrap();
    let beta = TempDir::new().unwrap();
    std::fs::write(alpha.path().join("Note.md"), "# In alpha\n").unwrap();
    std::fs::write(beta.path().join("Note.md"), "# In beta\n").unwrap();
    let server = ObsidianMcpServer::new().unwrap();
    for (name, dir) in [("alpha", &alpha), ("beta", &beta)] {
        call(
            &server,
            "add_vault",
            json!({"name": name, "path": dir.path().to_string_lossy()}),
        )
        .await;
    }
    call(&server, "set_active_vault", json!({"name": "alpha"})).await;
    (alpha, beta, server)
}

#[tokio::test]
async fn a_call_runs_against_the_vault_it_names() {
    let (_alpha, beta, server) = two_vaults().await;

    let read = call(
        &server,
        "read_note",
        json!({"path": "Note.md", "vault": "beta"}),
    )
    .await;
    assert_eq!(read["vault"], "beta");
    assert!(read.to_string().contains("In beta"), "{read}");

    call(
        &server,
        "write_note",
        json!({"path": "New.md", "content": "made in beta", "vault": "beta"}),
    )
    .await;
    assert!(beta.path().join("New.md").exists());

    // The active vault is untouched, and calls without `vault` still use it.
    assert_eq!(server.multi_vault().get_active_vault().await, "alpha");
    let read = call(&server, "read_note", json!({"path": "Note.md"})).await;
    assert!(read.to_string().contains("In alpha"), "{read}");
}

#[tokio::test]
async fn an_unknown_vault_is_refused_with_the_names_there_are() {
    let (_alpha, _beta, server) = two_vaults().await;
    let error = call_error(
        &server,
        "read_note",
        json!({"path": "Note.md", "vault": "gamma"}),
    )
    .await;
    assert!(error.contains("no vault named \"gamma\""), "{error}");
    assert!(error.contains("alpha") && error.contains("beta"), "{error}");
}

#[tokio::test]
async fn vault_scoped_tools_advertise_the_vault_argument() {
    let server = ObsidianMcpServer::new().unwrap();
    let tools = server.list_tools();
    let has_vault = |name: &str| {
        let tool = tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("no tool {name}"));
        tool.input_schema
            .properties
            .as_ref()
            .and_then(|properties| properties.get("vault"))
            .is_some()
    };
    for name in ["read_note", "write_note", "search", "get_vault_context"] {
        assert!(has_vault(name), "{name} should take `vault`");
    }
    for name in [
        "list_vaults",
        "add_vault",
        "remove_vault",
        "set_active_vault",
    ] {
        assert!(
            !has_vault(name),
            "{name} manages vaults and takes no `vault`"
        );
    }
}
