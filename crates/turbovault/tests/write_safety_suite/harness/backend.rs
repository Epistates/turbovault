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
//!   (`impl Op<W>`). An op that doesn't map to a layer simply has no
//!   invoker for that layer's World.
//!
//! The op invocation is NOT here — it lives in each op's invoker (`adapters/*`).
//! `ToolsWorld`'s tool calls are aspirational (they take a `Precondition` the
//! tool layer does not yet accept), so the suite does not compile until the
//! tool-signature cutover (qae.9.1).

use std::path::Path;
use std::sync::{Arc, Once};

use tempfile::TempDir;
use turbomcp::{McpHandler, RequestContext};
use turbovault::ObsidianMcpServer;
use turbovault_core::config::{ServerConfig, VaultConfig, WriteBackend};
use turbovault_tools::{BatchOperation, BatchTools};
use turbovault_vault::VaultManager;

use super::outcome::{Observed, ObservedError};
use super::precondition::Precondition;
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
            Self::Git => "git",
            Self::Direct => "direct",
        }
    }

    /// Whether this backend can represent `state` (git-only states are N/A on
    /// Direct). The runner filters trials by this before constructing a target.
    pub fn supports_state(self, state: GitState) -> bool {
        match self {
            Self::Git => true,
            Self::Direct => matches!(state, GitState::Absent | GitState::Untracked),
        }
    }

    /// The production write-substrate enum this test axis names.
    pub fn write_backend(self) -> WriteBackend {
        match self {
            Self::Git => WriteBackend::Git,
            Self::Direct => WriteBackend::Direct,
        }
    }

    /// The **single** place a test vault's config is built, so the harness cannot
    /// end up driving a substrate other than the one the cell names. That mapping
    /// used to be written twice — here and again in `WireWorld::new` for the
    /// server's own registration — and nothing checked the two agreed; a drift
    /// would have silently reproduced turbovault-74p (a git repo driven as Direct:
    /// writes land in the working tree but never commit) *inside the suite*, which
    /// every `Ok` cell would still have passed. `probe.rs`'s backend-identity probe
    /// now measures the real thing; this constructor removes the drift at source.
    ///
    /// `git` is left `None` on both arms: the manager reads `include_ignored` from
    /// it and falls back to `VaultGitConfig::default()` when absent (manager.rs),
    /// and that is the only field it consumes — so `None` and
    /// `Some(VaultGitConfig::default())` are equivalent, and the wire arm's former
    /// explicit default was redundant rather than different.
    pub fn vault_config(self, name: &str, path: &Path) -> VaultConfig {
        VaultConfig::builder(name, path)
            .write_backend(self.write_backend())
            .build()
            .expect("test vault config")
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
    pub fn new(backend: Backend) -> Self {
        make_libgit2_hermetic();
        let dir = match backend {
            Backend::Git => super::state::new_seeded_repo(),
            Backend::Direct => TempDir::new().expect("tempdir"),
        };
        let path = dir.path().to_path_buf();
        let mut cfg = ServerConfig::new();
        cfg.vaults.push(backend.vault_config("t", &path));
        let manager = Arc::new(VaultManager::new(cfg).unwrap());
        Self {
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
        Self {
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
/// manager operation get an `Op<ManagerWorld>` impl.
pub struct ManagerWorld {
    vault: Vault,
}

impl Layer for ManagerWorld {
    const LABEL: &'static str = "manager";
    fn new(backend: Backend) -> Self {
        Self {
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
///
/// Multi-op transaction-integrity (whole-op-list atomicity/rollback/collision/
/// empty-batch validation) is a DIFFERENT axis from WSS's per-write
/// clobber-safety and was moved out of WSS scope — see `turbovault-nbl.17` /
/// `docs/write-safety-suite/`.
pub struct BatchWorld {
    vault: Vault,
}

impl Layer for BatchWorld {
    const LABEL: &'static str = "batch";
    fn new(backend: Backend) -> Self {
        Self {
            vault: Vault::new(backend),
        }
    }
    fn vault(&self) -> &Vault {
        &self.vault
    }
}

impl BatchWorld {
    /// Apply a SINGLE [`BatchOperation`] through the batch translation path — the
    /// same `plan` fold + `apply_changes` that [`BatchTools::batch_execute`] runs
    /// internally, minus the soft-envelope wrapping that would stringify (and so
    /// erase) the structured error kind the matrix's `Outcome` assertions need.
    /// Returns the RAW substrate result so the adapter `observe()`s it exactly like
    /// every other world — the batch invoke reads
    /// `observe(w.apply_op(op).await, w.vault().read(rel))`, symmetric with the
    /// Tools/Manager arms. Proves batch-of-one == standalone: the one-op plan carries
    /// exactly the op's precondition, and the substrate's shared dirty gate + CAS
    /// decide the outcome identically to the standalone mutator.
    pub async fn apply_op(&self, op: BatchOperation) -> Result<(), turbovault_core::Error> {
        let mgr = self.vault().manager().clone();
        match BatchTools::new(mgr.clone()).plan(&[op]).await {
            Ok(mut plan) => {
                plan.message = MSG.to_string();
                mgr.apply_changes(&plan).await.map(|_| ())
            }
            Err(e) => Err(e),
        }
    }

    /// The `expected_hash` an in-place batch op should carry for a resolved
    /// precondition: the blob token for [`Precondition::ExpectBlob`], else `None` (a
    /// bare op — the fold's read + the substrate dirty gate enforce existence,
    /// matching the standalone `ExpectExists` default). Batch ops carry no
    /// first-class `ExpectExists`, so `None` is the faithful mapping.
    pub fn blob_token(pc: &Precondition) -> Option<String> {
        match pc {
            Precondition::ExpectBlob(oid) => Some(oid.clone()),
            _ => None,
        }
    }
}

/// The Wire layer (nbl.12): an **in-process** `ObsidianMcpServer` driven through
/// its real `call_tool` dispatch — the SAME path the JSON-RPC/stdio transport
/// routes to, minus the framing (the `test_mcp_e2e_git_substrate.rs` shape). It
/// exercises the load-bearing wire concerns — the `#[tool]` handler, JSON param
/// (de)serialization, and the `ConcurrencyError → McpError` mapping — git-capable
/// and cheap (no child process). This cell's vault is registered directly via a
/// `VaultConfig` (bypassing the `add_vault` tool, which is Direct-only —
/// turbovault-kdq).
///
/// The typed error kind is ERASED at the wire (every domain error flattens to an
/// `McpError` carrying only a Display string), so [`WireWorld::call_tool`]
/// classifies the outcome by MESSAGE SUBSTRING — which is precisely the wire
/// contract this arm exists to pin: that the kind survives as a legible string.
pub struct WireWorld {
    vault: Vault,
    server: ObsidianMcpServer,
    config: VaultConfig,
    /// Registration is async (`add_vault`/`set_active_vault`) so it can't happen
    /// in the sync `Layer::new`; do it once, lazily, on the first call.
    active: tokio::sync::OnceCell<()>,
}

impl Layer for WireWorld {
    const LABEL: &'static str = "wire";
    fn new(backend: Backend) -> Self {
        let vault = Vault::new(backend);
        // Same constructor the vault itself used — one mapping, no drift.
        let config = backend.vault_config("wss", vault.dir.path());
        Self {
            vault,
            server: ObsidianMcpServer::new().expect("wire server"),
            config,
            active: tokio::sync::OnceCell::new(),
        }
    }
    fn vault(&self) -> &Vault {
        &self.vault
    }
}

impl WireWorld {
    /// Register + activate this cell's vault on the server exactly once.
    async fn ensure_active(&self) {
        self.active
            .get_or_init(|| async {
                self.server
                    .multi_vault()
                    .add_vault(self.config.clone())
                    .await
                    .expect("wire add_vault");
                self.server
                    .multi_vault()
                    .set_active_vault("wss")
                    .await
                    .expect("wire set_active_vault");
            })
            .await;
    }

    /// Invoke a tool through the in-process server dispatch and classify the
    /// outcome. `Ok(())` == the tool succeeded; `Err(kind)` == a failure,
    /// classified from the wire message string. The op's invoker combines this
    /// with the post-op working-tree read via [`observe_outcome`].
    /// The parameter names `tool` DECLARES, read from the server's own catalog —
    /// the same `list_tools()` output the golden `providers/tool_catalog.json`
    /// pins. `None` == the server declares no such tool.
    pub fn declared_params(&self, tool: &str) -> Option<std::collections::BTreeSet<String>> {
        let listed = self.server.list_tools();
        let found = listed.iter().find(|t| t.name == tool)?;
        let schema = serde_json::to_value(found).ok()?;
        Some(
            schema
                .get("inputSchema")?
                .get("properties")?
                .as_object()?
                .keys()
                .cloned()
                .collect(),
        )
    }

    /// Panic unless every key in `params` is declared by `tool`.
    ///
    /// The wire is the ONLY world whose command shape isn't compiler-checked: the
    /// other worlds call Rust methods or build a typed `BatchOperation`, so a
    /// renamed field breaks the build, while here tool and param names are just
    /// strings in a JSON blob. An undeclared param is (at best) silently IGNORED,
    /// which makes the call behave as if no precondition were passed — so `Ok`
    /// cells still pass and only some refusal cells fail, with a thoroughly
    /// misleading message. Checking here covers every wire cell in the suite at
    /// once, with no list of params to keep in step (nbl.21).
    fn assert_declared(&self, tool: &str, params: &serde_json::Value) {
        let declared = self
            .declared_params(tool)
            .unwrap_or_else(|| panic!("wire: the server declares no tool named `{tool}`"));
        let sent = params
            .as_object()
            .unwrap_or_else(|| panic!("wire: params for `{tool}` must be a JSON object"));
        for key in sent.keys() {
            assert!(
                declared.contains(key),
                "wire: param `{key}` is not declared by tool `{tool}` \
                 (declared: {declared:?}) — a typo/rename would be silently ignored"
            );
        }
    }

    pub async fn call_tool(
        &self,
        tool: &str,
        params: serde_json::Value,
    ) -> Result<(), ObservedError> {
        self.assert_declared(tool, &params);
        self.ensure_active().await;
        match self
            .server
            .call_tool(tool, params, &RequestContext::new())
            .await
        {
            Ok(result) if !result.is_error() => Ok(()),
            Ok(result) => Err(classify_wire(result.first_text())),
            Err(e) => Err(classify_wire(Some(&e.to_string()))),
        }
    }
}

/// Classify a wire error message into an [`ObservedError`]. The typed kind is
/// erased at the wire boundary, so match the domain errors' `#[error(...)]`
/// text: `"Concurrent access conflict: …"` / `"File not found: …"`.
fn classify_wire(text: Option<&str>) -> ObservedError {
    let t = text.unwrap_or_default();
    // Mirror the typed `classify`: a not-found reaches the wire either as the
    // domain `FileNotFound` ("File not found: …") OR as a raw `Io(NotFound)`
    // ("… No such file or directory …") — both are the same NotFound outcome.
    if t.contains("Concurrent access conflict") {
        ObservedError::Concurrency
    } else if t.contains("File not found") || t.contains("No such file or directory") {
        ObservedError::NotFound
    } else {
        ObservedError::Other
    }
}

/// The Wire analogue of [`observe`]: build an [`Observed`] from an already-
/// classified wire outcome + the post-op working-tree content.
pub fn observe_outcome(outcome: Result<(), ObservedError>, after: Option<String>) -> Observed {
    match outcome {
        Ok(()) => Observed::ok(after),
        Err(kind) => Observed::failed(kind, after),
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

    /// The pipeline composes end-to-end: `build_state` → Vault → a real manager
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
