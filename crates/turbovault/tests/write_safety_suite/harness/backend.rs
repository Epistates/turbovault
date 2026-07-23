//! Backend + layer scaffolding (design doc §9).
//!
//! Three axes, cleanly separated:
//! - [`Backend`] (`Git` | `Direct`) → a [`Vault`]: the on-disk vault — state
//!   construction, reads, version tokens. **Layer-agnostic.**
//! - **Layer** (Tools | Manager | Wire) → a per-layer World implementing
//!   [`Layer`], each wrapping a [`Vault`] and exposing *its own* write surface.
//!   Only [`ToolsWorld`] exists in this bite; Manager (qae.9.2) and Wire (nbl.12)
//!   drop in as further `Layer` impls.
//! - **Op** → a shared `Case` table + a per-`(op, layer)` invoker
//!   (`impl SinglePathOp<W>`). An op that doesn't map to a layer simply has no
//!   invoker for that layer's World.
//!
//! The op invocation is NOT here — it lives in each op's invoker (`adapters/*`).
//! `ToolsWorld`'s tool calls are aspirational (they take a `Precondition` the
//! tool layer does not yet accept), so the suite does not compile until the
//! tool-signature cutover (qae.9.1).

use std::sync::{Arc, Once};

use tempfile::TempDir;
use turbovault_core::config::{ServerConfig, VaultConfig, WriteBackend};
use turbovault_vault::VaultManager;

use super::outcome::{Observed, ObservedError};
use super::state::{GitState, Oids};

/// Commit subject / message threaded to every op (the matrix never asserts it).
pub const MSG: &str = "wss";

/// Content a Direct "present" file holds (the Direct backend has no git states,
/// so its WORKDIR token is the sha256 of exactly these bytes).
const DIRECT_PRESENT: &str = "v1 committed\n";

/// Which write substrate the vault under test runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Every mutation is a git commit; the full 9-state working-tree grid applies.
    Git,
    /// Writes land directly on the filesystem; no HEAD/INDEX, so only the
    /// `{absent, present}` subset of states is representable (design §9).
    Direct,
}

impl Backend {
    /// Short label used in self-describing trial names.
    pub fn code(self) -> &'static str {
        match self {
            Backend::Git => "git",
            Backend::Direct => "direct",
        }
    }

    /// Whether this backend can represent `state` (git-only states are N/A on
    /// Direct). The runner filters trials by this before constructing a target.
    pub fn supports_state(self, state: GitState) -> bool {
        match self {
            Backend::Git => true,
            Backend::Direct => matches!(state, GitState::Absent | GitState::Untracked),
        }
    }
}

/// Make libgit2 hermetic: clear the global/system/XDG config search paths so the
/// substrate under test can't be perturbed by the developer's `~/.gitconfig`.
/// Two concrete hazards this closes: `core.autocrlf` (would rewrite line endings
/// → different blob oids → silently break the version-token contract) and
/// `core.excludesfile` (a global ignore could trip the substrate's lri gate).
/// Process-global (mirrors the substrate's own `init_libgit2_opts`); set once.
fn make_libgit2_hermetic() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for level in [
            git2::ConfigLevel::System,
            git2::ConfigLevel::Global,
            git2::ConfigLevel::XDG,
        ] {
            // SAFETY: process-global libgit2 option, set once before any repo is
            // opened on this thread; `""` clears the search path for that level.
            unsafe {
                let _ = git2::opts::set_search_path(level, "");
            }
        }
    });
}

/// The on-disk vault under test: backend-specific state construction, reads, and
/// version tokens. **Layer-agnostic** — every layer's World wraps one of these.
///
/// It holds a `VaultManager` used ONLY for state setup + token minting (the ops
/// go through the layer's own surface, not this manager — except the Manager
/// layer, which happens to reuse it). Minting the WORKDIR token via
/// `VaultManager::hash_bytes` guarantees an `ExpectBlob(WORKDIR)` round-trips
/// against the substrate on both backends (blob-oid on git, sha256 on direct).
pub struct Vault {
    pub dir: TempDir,
    backend: Backend,
    manager: Arc<VaultManager>,
}

impl Vault {
    pub fn new(backend: Backend) -> Vault {
        make_libgit2_hermetic();
        let dir = match backend {
            Backend::Git => super::state::new_seeded_repo(),
            Backend::Direct => TempDir::new().expect("tempdir"),
        };
        let path = dir.path().to_path_buf();
        let write_backend = match backend {
            Backend::Git => WriteBackend::Git,
            Backend::Direct => WriteBackend::Direct,
        };
        let mut cfg = ServerConfig::new();
        cfg.vaults.push(
            VaultConfig::builder("t", &path)
                .write_backend(write_backend)
                .build()
                .unwrap(),
        );
        let manager = Arc::new(VaultManager::new(cfg).unwrap());
        Vault {
            dir,
            backend,
            manager,
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The setup/token manager (also the ops surface for the Manager layer).
    pub fn manager(&self) -> &Arc<VaultManager> {
        &self.manager
    }

    /// Working-tree content of `rel` (`None` == absent).
    pub fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join(rel)).ok()
    }

    /// Build `state` for `rel` and resolve its version tokens; `None` if this
    /// backend cannot represent the state (the matrix's N/A rule, per backend).
    pub fn build_state(&self, rel: &str, state: GitState) -> Option<Oids> {
        match self.backend {
            Backend::Git => Some(super::state::build_state(self.dir.path(), rel, state)),
            Backend::Direct => {
                let path = self.dir.path().join(rel);
                match state {
                    GitState::Absent => {
                        let _ = std::fs::remove_file(&path);
                        Some(Oids {
                            head: None,
                            index: None,
                            workdir: None,
                        })
                    }
                    GitState::Untracked => {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent).unwrap();
                        }
                        std::fs::write(&path, DIRECT_PRESENT).unwrap();
                        let token = self.manager.hash_bytes(DIRECT_PRESENT.as_bytes()).ok();
                        Some(Oids {
                            head: None,
                            index: None,
                            workdir: token,
                        })
                    }
                    // The 7 git-only states have no Direct representation (§9).
                    _ => None,
                }
            }
        }
    }
}

/// A per-layer World: wraps a [`Vault`] and exposes the layer's write surface.
/// The runner is generic over this; each layer is exactly one impl.
pub trait Layer {
    /// Short label used to prefix trial names (`tools` / `manager` / `wire`).
    const LABEL: &'static str;
    fn new(backend: Backend) -> Self;
    fn vault(&self) -> &Vault;
}

/// The Tools layer: invokers construct the domain tool they need from
/// `vault().manager()`. The World knows nothing about which tools exist — adding
/// a new tool touches only its op's invoker, never this type.
pub struct ToolsWorld {
    vault: Vault,
}

impl Layer for ToolsWorld {
    const LABEL: &'static str = "tools";
    fn new(backend: Backend) -> Self {
        ToolsWorld {
            vault: Vault::new(backend),
        }
    }
    fn vault(&self) -> &Vault {
        &self.vault
    }
}

/// The Manager/ChangePlan layer (qae.9.2): invokers call `VaultManager` directly
/// via `vault().manager()`. Structurally identical to [`ToolsWorld`] — the layer
/// distinction lives entirely in the invokers, and only the 5 ops with a native
/// manager operation get a `SinglePathOp<ManagerWorld>` impl.
pub struct ManagerWorld {
    vault: Vault,
}

impl Layer for ManagerWorld {
    const LABEL: &'static str = "manager";
    fn new(backend: Backend) -> Self {
        ManagerWorld {
            vault: Vault::new(backend),
        }
    }
    fn vault(&self) -> &Vault {
        &self.vault
    }
}

/// The Batch layer (qae.9.3): invokers stand up `BatchTools` from
/// `vault().manager()` and drive each write as a `BatchOperation` through the
/// batch surface (`plan`/`apply_changes`/`batch_execute`). Structurally
/// identical to [`ManagerWorld`] — the layer distinction lives entirely in the
/// invokers. A batch-of-one proves per-op-in-batch behavior == the standalone op.
pub struct BatchWorld {
    vault: Vault,
}

impl Layer for BatchWorld {
    const LABEL: &'static str = "batch";
    fn new(backend: Backend) -> Self {
        BatchWorld {
            vault: Vault::new(backend),
        }
    }
    fn vault(&self) -> &Vault {
        &self.vault
    }
}

/// The Wire layer (nbl.12): a spawned MCP server + JSON-RPC client. **SKELETON** —
/// invokers would `call_tool(name, params)` over stdio; the vault is still needed
/// for state setup + reads. The server/client plumbing and the `Precondition`→wire
/// encoding (blocked on the M5.3 wire-param decision) are nbl.12; until then this
/// is a compiling placeholder so the shape is visible.
pub struct WireWorld {
    vault: Vault,
    // nbl.12: spawned `turbovault` server process + an MCP client over stdio.
}

impl Layer for WireWorld {
    const LABEL: &'static str = "wire";
    fn new(backend: Backend) -> Self {
        // nbl.12: also spawn the server + connect a client over this vault's dir.
        WireWorld {
            vault: Vault::new(backend),
        }
    }
    fn vault(&self) -> &Vault {
        &self.vault
    }
}

impl WireWorld {
    /// Invoke a tool over the wire and normalize the reply into an [`Observed`].
    pub async fn call_tool(&self, _tool: &str, _params: serde_json::Value) -> Observed {
        todo!("nbl.12: JSON-RPC call_tool + Precondition wire encoding (M5.3)")
    }
}

/// Normalize a substrate write `Result` (plus the post-op working-tree content)
/// into a layer-agnostic [`Observed`]. Each op's invoker calls this itself, so
/// op-specific result shaping (e.g. `move_note`'s dual-path assertion) stays in
/// the adapter.
pub fn observe(result: Result<(), turbovault_core::Error>, after: Option<String>) -> Observed {
    match result {
        Ok(()) => Observed::ok(after),
        Err(e) => Observed::failed(classify(&e), after),
    }
}

fn classify(e: &turbovault_core::Error) -> ObservedError {
    use turbovault_core::Error;
    match e {
        Error::ConcurrencyError { .. } => ObservedError::Concurrency,
        Error::FileNotFound { .. } => ObservedError::NotFound,
        Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound => ObservedError::NotFound,
        _ => ObservedError::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::outcome::Outcome;
    use crate::harness::precondition::{Precondition, PreconditionKind};

    /// The pipeline composes end-to-end: build_state → Vault → a real manager
    /// write → observe → `Outcome::assert`. Uses the manager (which already takes a
    /// `Precondition`), so it compiles + runs while the tool arm stays aspirational.
    #[tokio::test]
    async fn primitives_compose_end_to_end() {
        let vault = Vault::new(Backend::Git);
        vault.build_state("note.md", GitState::CleanCommitted);
        let before = vault.read("note.md");
        let res = vault
            .manager()
            .write_file(
                std::path::Path::new("note.md"),
                "NEW",
                Precondition::Blind,
                "wss",
            )
            .await;
        let observed = observe(res, vault.read("note.md"));
        Outcome::Ok.assert(&observed, before.as_deref());
        assert_eq!(
            observed.after_content.as_deref(),
            Some("NEW"),
            "the write must land in the working tree"
        );
    }

    /// Git vault builds every state with the expected tokens.
    #[test]
    fn git_vault_builds_state_with_tokens() {
        let vault = Vault::new(Backend::Git);
        let oids = vault
            .build_state("note.md", GitState::CommittedUnstaged)
            .expect("git builds all states");
        assert!(oids.head.is_some() && oids.workdir.is_some());
        match PreconditionKind::Workdir.resolve(&oids) {
            Some(Precondition::ExpectBlob(oid)) => assert_eq!(Some(&oid), oids.workdir.as_ref()),
            other => panic!("expected ExpectBlob, got {other:?}"),
        }
    }

    /// Direct vault represents only {absent, present}; git-only states are N/A,
    /// and a present file resolves a WORKDIR token (sha256).
    #[test]
    fn direct_vault_supports_absent_and_present_only() {
        let vault = Vault::new(Backend::Direct);
        assert!(
            vault
                .build_state("note.md", GitState::CommittedStaged)
                .is_none()
        );
        assert!(vault.build_state("note.md", GitState::Absent).is_some());
        let present = vault
            .build_state("note.md", GitState::Untracked)
            .expect("direct present");
        assert!(present.head.is_none() && present.index.is_none());
        assert!(
            present.workdir.is_some(),
            "direct present mints a sha256 token"
        );
        assert_eq!(vault.read("note.md").as_deref(), Some(DIRECT_PRESENT));
    }
}
