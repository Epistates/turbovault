//! turbovault-kdq: runtime vault registration can select the write backend.
//!
//! Before this, `add_vault` / `create_vault` / `--vault` only ever produced
//! `Direct` vaults — the only route to a git-backed vault was a startup
//! `--config` yaml or the non-wire-exposed SDK. These tests pin the wire
//! contract of the new parameter from the outside: they drive the real
//! `#[tool]` handler through `ObsidianMcpServer::call_tool` (the same dispatch
//! turbomcp routes a JSON-RPC request to) and assert the *effect* — a vault
//! registered with `write_backend: "git"` commits its writes — rather than
//! just the config field.
//!
//! Back-compat is half of the contract: omitting the parameter MUST still
//! yield a `Direct` vault, which is why both directions live in one test.
//!
//! turbovault-74p rides along here: it is the same registration surface seen
//! from the other side — what happens when the caller does *not* choose.

use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;
use turbovault_core::config::{VaultConfig, WriteBackend};
use turbovault_tools::{VaultRepo, direct_over_git_repo_warning};

/// Call a tool through the real handler dispatch, panicking loudly on error.
async fn call(
    server: &ObsidianMcpServer,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let result = server
        .call_tool(name, arguments, &RequestContext::new())
        .await
        .unwrap_or_else(|error| panic!("tool {name:?} failed: {error}"));
    assert!(
        !result.is_error(),
        "tool {name:?} returned an error: {}",
        result
            .first_text()
            .unwrap_or("tool returned an error without text")
    );
    result
        .structured_content
        .unwrap_or_else(|| panic!("tool {name:?} returned no structured content"))
}

/// A git repo with HEAD born — the substrate's baseline requirement.
fn init_repo(path: &std::path::Path) {
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = git2::Repository::init_opts(path, &opts).unwrap();
    let tree_oid = {
        let mut idx = repo.index().unwrap();
        idx.write_tree().unwrap()
    };
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("Init", "init@example").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
}

fn head_oid(path: &std::path::Path) -> Option<git2::Oid> {
    VaultRepo::open(path).ok().and_then(|r| r.head_oid())
}

#[tokio::test]
#[serial_test::serial]
async fn add_vault_selects_the_git_backend_and_omitting_it_stays_direct() {
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let server = ObsidianMcpServer::new().unwrap();

    // Selected over the wire: the vault is registered on the git substrate…
    call(
        &server,
        "add_vault",
        serde_json::json!({
            "name": "gitvault",
            "path": repo.path(),
            "write_backend": "git",
        }),
    )
    .await;
    let config = server
        .multi_vault()
        .get_vault_config("gitvault")
        .await
        .unwrap();
    assert_eq!(config.write_backend, WriteBackend::Git);

    // …and the manager `add_vault` publishes actually commits its writes.
    server
        .multi_vault()
        .set_active_vault("gitvault")
        .await
        .unwrap();
    let head_before = head_oid(repo.path()).unwrap();
    call(
        &server,
        "write_note",
        serde_json::json!({ "path": "note.md", "content": "# Note\n" }),
    )
    .await;
    assert_ne!(
        head_oid(repo.path()).unwrap(),
        head_before,
        "a git-backend write must land a commit"
    );

    // Back-compat: the parameter omitted is exactly the pre-kdq behaviour.
    let plain = TempDir::new().unwrap();
    call(
        &server,
        "add_vault",
        serde_json::json!({ "name": "plainvault", "path": plain.path() }),
    )
    .await;
    assert_eq!(
        server
            .multi_vault()
            .get_vault_config("plainvault")
            .await
            .unwrap()
            .write_backend,
        WriteBackend::Direct
    );
}

/// turbovault-74p: a Direct vault over a git repo is a silent git bypass —
/// every write lands on the filesystem and never commits, so the working tree
/// drifts from HEAD with nothing to notice it by. Registration must say so.
///
/// The signal is tested through the pure warning function rather than by
/// capturing the log line: same predicate, no global logger fixture.
#[test]
fn registering_a_git_repo_as_direct_warns_while_a_plain_path_stays_quiet() {
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let shadow = VaultConfig::builder("shadow", repo.path()).build().unwrap();
    assert_eq!(shadow.write_backend, WriteBackend::Direct);

    let warning = direct_over_git_repo_warning(&shadow)
        .expect("a Direct vault over a git repository must warn");
    let lowercased = warning.to_lowercase();
    assert!(
        lowercased.contains("not be committed"),
        "the warning must state that writes will not be committed: {warning:?}"
    );
    assert!(
        warning.contains("write_backend"),
        "the warning must name the parameter that selects the git backend: {warning:?}"
    );

    // A Direct vault over a plain directory is the ordinary case: no signal.
    let plain = TempDir::new().unwrap();
    let ordinary = VaultConfig::builder("ordinary", plain.path())
        .build()
        .unwrap();
    assert!(
        direct_over_git_repo_warning(&ordinary).is_none(),
        "a non-git path must not warn"
    );
}
