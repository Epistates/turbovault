//! The harness's own self-test layer (turbovault-nbl.19) — the probes that keep
//! the **suite** honest, as distinct from the matrix cells that keep **TurboVault**
//! honest.
//!
//! TurboVault's promise is to **ABORT** if the content on disk doesn't match what
//! you said the file looks like. WSS ensures this promise by setting up all
//! possible scenario combinations and making sure TurboVault aborts when it
//! *should* abort. Which means WSS is only as trustworthy as two things the
//! harness itself must get right — and each gets a probe here, one per axis the
//! matrix parameterizes over:
//!
//! 1. **Setup fidelity** ([`ProbeWorld`]) — the scenario a cell *claims* to set up
//!    is the scenario actually on disk, and the precondition it hands the op is the
//!    one the cell named. If `build_state(etcsu)` silently produced `etc-u`, every
//!    cell in that column would be exercising the wrong scenario and still "pass".
//!    [`ProbeWorld`] re-derives the state from git **independently** of
//!    `build_state` (via [`observe_git_state`]) and echoes back the resolved
//!    precondition.
//! 2. **Plumbing fidelity** ([`Probe`]) — every World actually reaches the write
//!    surface it names, and they all agree. The matrix asserts each world
//!    *separately* against a coarse expected [`Outcome`], so two worlds can diverge
//!    in ways no cell catches — a `classify_wire` mis-mapping, or a batch invoker
//!    swallowing an error kind. [`Probe`] is the minimal deterministic op, run on
//!    **all four** worlds over settled cells, asserting the observations are
//!    IDENTICAL.
//!
//! Scope: these probes check **clobber-safety plumbing, not content-correctness** —
//! WSS never asserts that written text is *formatted* right, only that a write
//! refuses-or-proceeds without silently losing an out-of-band change.
//!
//! They run in the default-harness `write_safety_suite` target (they test the
//! harness); the matrix cells run in `wss_matrix` (it tests TurboVault).
//!
//! SEAM (turbovault-nbl.13): [`ProbeWorld`] + the setup sweep are portable — they
//! need only `Vault`/`Layer`. [`Probe`]'s four invokers name the concrete worlds, so
//! they are the conformance checklist a new World (or a third-party backend) must
//! pass.

use crate::harness::backend::{
    Backend, BatchWorld, Layer, MSG, ManagerWorld, ToolsWorld, Vault, WireWorld, observe,
    observe_outcome,
};
use crate::harness::op::{Case, Op, OpAdapterMeta, REL};
use crate::harness::outcome::{Observed, Outcome};
use crate::harness::precondition::{Precondition, PreconditionKind, sentinel};
use crate::harness::state::{GitState, observe_git_state};
use turbovault_tools::{BatchOperation, FileTools, WriteMode};

// ── Probe 1: setup fidelity ──────────────────────────────────────────────────

/// What a cell's setup ACTUALLY looks like, measured rather than recalled: the
/// state re-derived from the working tree, plus the precondition the op would be
/// handed.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbedSetup {
    /// The state re-derived from disk (`None` == matches no canonical state).
    pub state: Option<GitState>,
    /// The precondition resolved for this cell — echoed back verbatim.
    pub precondition: Precondition,
    /// Working-tree content of the target (`None` == absent).
    pub content: Option<String>,
}

/// A World that writes **nothing**: its "invocation" only observes. It exists so
/// the state × precondition setup can be checked through the same `Vault`
/// construction path the real worlds use, without a mutation confusing the reading.
pub struct ProbeWorld {
    vault: Vault,
}

impl Layer for ProbeWorld {
    const LABEL: &'static str = "probe";
    fn new(backend: Backend) -> Self {
        ProbeWorld {
            vault: Vault::new(backend),
        }
    }
    fn vault(&self) -> &Vault {
        &self.vault
    }
}

impl ProbeWorld {
    /// Observe `rel`'s setup — the inspection-only counterpart to a real world's
    /// `invoke`. Takes the precondition by reference and echoes a clone, so the
    /// caller can compare it against what the cell asked for.
    pub fn probe(&self, rel: &str, precondition: &Precondition) -> ProbedSetup {
        ProbedSetup {
            state: observed_state(self.vault(), rel),
            precondition: precondition.clone(),
            content: self.vault().read(rel),
        }
    }
}

/// Re-derive `rel`'s state on either backend, independently of `build_state`.
/// Git measures the real `(e,t,c,s,u)` flags out of git plumbing; Direct has no
/// git at all, and its only representable states are absent and present — where
/// "present" is the `e---u` (Untracked) cell that `present_state(Direct)` names.
fn observed_state(vault: &Vault, rel: &str) -> Option<GitState> {
    match vault.backend() {
        Backend::Git => observe_git_state(vault.dir.path(), rel),
        Backend::Direct => Some(if vault.read(rel).is_some() {
            GitState::Untracked
        } else {
            GitState::Absent
        }),
    }
}

/// Whether `kind`'s token is *defined* in `state` — the matrix's N/A rule, stated
/// independently of `Oids` so the probe can catch a token that silently failed to
/// resolve. That failure mode is nastier than a wrong answer: an unresolved token
/// makes `resolve` return `None`, which does not FAIL a cell — it silently DELETES
/// it from the matrix (a coverage hole that looks like a clean run).
fn token_defined(kind: PreconditionKind, state: GitState) -> bool {
    match kind {
        PreconditionKind::Head => state.committed(),
        PreconditionKind::Index => state.staged(),
        PreconditionKind::Workdir => state.exists(),
        // Not token-backed: always constructible.
        PreconditionKind::Blind
        | PreconditionKind::Absent
        | PreconditionKind::Exists
        | PreconditionKind::Wrong => true,
    }
}

// ── Probe 2: plumbing fidelity ───────────────────────────────────────────────

/// The bytes the probe writes — distinct from every state generation, so a
/// successful write is observable as a real change.
const PROBE_CONTENT: &str = "wss-probe\n";

/// The minimal deterministic write op, implemented for **every** world. Its only
/// job is to prove a world's plumbing reaches the write surface and reports the
/// outcome faithfully; being minimal is the point — a divergence between worlds is
/// then attributable to the world, not to op-specific semantics.
#[derive(Clone, Copy)]
pub struct Probe;

impl OpAdapterMeta for Probe {
    fn name(&self) -> &'static str {
        "probe"
    }

    fn cases(&self) -> &'static [Case] {
        AGREEMENT_CASES
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        if observed.after_content.as_deref() == Some(PROBE_CONTENT) {
            Ok(())
        } else {
            Err(format!(
                "OK effect: expected probe content {PROBE_CONTENT:?}, got {:?}",
                observed.after_content
            ))
        }
    }
}

impl Op<ToolsWorld> for Probe {
    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let res = FileTools::new(w.vault().manager().clone())
            .write_file_with_mode(rel, PROBE_CONTENT, WriteMode::Overwrite, pc, MSG)
            .await;
        observe(res, w.vault().read(rel))
    }
}

impl Op<ManagerWorld> for Probe {
    async fn invoke(&self, w: &ManagerWorld, rel: &str, pc: Precondition) -> Observed {
        let res = w
            .vault()
            .manager()
            .write_file(std::path::Path::new(rel), PROBE_CONTENT, pc, MSG)
            .await;
        observe(res, w.vault().read(rel))
    }
}

impl Op<BatchWorld> for Probe {
    async fn invoke(&self, w: &BatchWorld, rel: &str, pc: Precondition) -> Observed {
        // Same precondition → batch-op mapping the write_note adapter uses: a
        // strict create carries ExpectAbsent, an upsert carries the blob (or none).
        let op = match pc {
            Precondition::ExpectAbsent => BatchOperation::CreateNote {
                path: rel.to_string(),
                content: PROBE_CONTENT.to_string(),
                force: None,
            },
            Precondition::ExpectBlob(oid) => BatchOperation::WriteNote {
                path: rel.to_string(),
                content: PROBE_CONTENT.to_string(),
                expected_hash: Some(oid),
            },
            _ => BatchOperation::WriteNote {
                path: rel.to_string(),
                content: PROBE_CONTENT.to_string(),
                expected_hash: None,
            },
        };
        observe(w.apply_op(op).await, w.vault().read(rel))
    }
}

impl Op<WireWorld> for Probe {
    async fn invoke(&self, w: &WireWorld, rel: &str, pc: Precondition) -> Observed {
        let params = serde_json::json!({
            "path": rel,
            "content": PROBE_CONTENT,
            "expected_hash": sentinel(&pc),
        });
        observe_outcome(w.call_tool("write_note", params).await, w.vault().read(rel))
    }
}

/// Cells every world must agree on **today** — deliberately only *settled*
/// behavior (each is an active, non-pending cell of the real `write_note` grid), so
/// a failure here means a world's plumbing broke, never that a burndown item is
/// outstanding. `present` differs by backend (`etc--` on git, `e---u` on direct),
/// so those cells are `.on()`-split — same expected outcome either way.
const AGREEMENT_CASES: &[Case] = &[
    // Blind → last-writer-wins: create and overwrite both succeed.
    Case::new(PreconditionKind::Blind, GitState::Absent, Outcome::Ok),
    Case::new(
        PreconditionKind::Blind,
        GitState::CleanCommitted,
        Outcome::Ok,
    )
    .on(Backend::Git),
    Case::new(PreconditionKind::Blind, GitState::Untracked, Outcome::Ok).on(Backend::Direct),
    // ExpectAbsent → the create guard: OK on absent, refuse on present.
    Case::new(PreconditionKind::Absent, GitState::Absent, Outcome::Ok),
    Case::new(
        PreconditionKind::Absent,
        GitState::CleanCommitted,
        Outcome::ConcurrencyError,
    )
    .on(Backend::Git),
    Case::new(
        PreconditionKind::Absent,
        GitState::Untracked,
        Outcome::ConcurrencyError,
    )
    .on(Backend::Direct),
    // ExpectBlob(HEAD) on a clean commit → the caller proved the bytes: OK.
    Case::new(
        PreconditionKind::Head,
        GitState::CleanCommitted,
        Outcome::Ok,
    )
    .on(Backend::Git),
    // ExpectBlob(WORKDIR) on direct → exercises the sha256 token round-trip.
    Case::new(PreconditionKind::Workdir, GitState::Untracked, Outcome::Ok).on(Backend::Direct),
    // ExpectBlob(WRONG) → never matches the tree: refuse everywhere.
    Case::new(
        PreconditionKind::Wrong,
        GitState::Absent,
        Outcome::ConcurrencyError,
    ),
    Case::new(
        PreconditionKind::Wrong,
        GitState::CleanCommitted,
        Outcome::ConcurrencyError,
    )
    .on(Backend::Git),
    Case::new(
        PreconditionKind::Wrong,
        GitState::Untracked,
        Outcome::ConcurrencyError,
    )
    .on(Backend::Direct),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Does this cell run on `backend`? Mirrors the runner's own filtering, so the
    /// probes cover exactly the cells the matrix would emit.
    fn runs_on(case: &Case, backend: Backend) -> bool {
        backend.supports_state(case.state) && case.only.is_none_or(|b| b == backend)
    }

    /// **Setup fidelity.** For every (precondition × state) the matrix can build on
    /// a backend: the state on disk is the state the cell named, and the resolved
    /// precondition is the one it asked for. Sweeps the FULL axes, not a subset.
    #[test]
    fn every_cell_is_set_up_as_it_claims() {
        for backend in [Backend::Git, Backend::Direct] {
            for state in GitState::ALL {
                if !backend.supports_state(state) {
                    continue;
                }
                for kind in PreconditionKind::ALL {
                    let w = ProbeWorld::new(backend);
                    let oids = w
                        .vault()
                        .build_state(REL, state)
                        .expect("a supported state must build");
                    let label = format!("{}::{}::{}", backend.code(), kind.code(), state.code());

                    // The N/A rule must hold exactly: a token resolves iff it is
                    // defined for this state. Getting this wrong silently drops a
                    // cell instead of failing one.
                    let resolved = kind.resolve(&oids);
                    assert_eq!(
                        resolved.is_some(),
                        token_defined(kind, state),
                        "{label}: precondition definedness disagrees with the N/A rule"
                    );
                    let Some(pc) = resolved else { continue };

                    let probed = w.probe(REL, &pc);
                    assert_eq!(
                        probed.state,
                        Some(state),
                        "{label}: built state is not the state the cell claims"
                    );
                    assert_eq!(
                        probed.precondition, pc,
                        "{label}: the op would receive a different precondition"
                    );
                    assert_eq!(
                        probed.content.is_some(),
                        state.exists(),
                        "{label}: target presence disagrees with the state's `e` flag"
                    );
                }
            }
        }
    }

    /// Build a cell on world `W` and run the probe op against it.
    async fn run_probe<W>(backend: Backend, case: Case) -> (Observed, Option<String>)
    where
        W: Layer,
        Probe: Op<W>,
    {
        let w = W::new(backend);
        let oids = w
            .vault()
            .build_state(REL, case.state)
            .expect("a probe cell's state must build on this backend");
        let pc = case
            .precondition
            .resolve(&oids)
            .expect("a probe cell's precondition must be defined in its state");
        let before = w.vault().read(REL);
        let observed = Probe.invoke(&w, REL, pc).await;
        (observed, before)
    }

    /// **Plumbing fidelity.** Every world reaches its write surface and reports the
    /// same thing: each satisfies the cell, AND all four observations are identical.
    /// The agreement half is what the matrix cannot check — it asserts each world
    /// against a coarse `Outcome` separately, so two worlds can differ while both
    /// still satisfy it.
    #[tokio::test]
    async fn every_world_agrees_on_a_settled_cell() {
        for backend in [Backend::Git, Backend::Direct] {
            for case in Probe.cases() {
                if !runs_on(case, backend) {
                    continue;
                }
                assert!(
                    !case.pending,
                    "a probe cell must be settled behavior, never pending"
                );
                let label = format!(
                    "{}::{}::{}::{:?}",
                    backend.code(),
                    case.precondition.code(),
                    case.state.code(),
                    case.expected
                );

                let (tools, before) = run_probe::<ToolsWorld>(backend, *case).await;
                let (manager, _) = run_probe::<ManagerWorld>(backend, *case).await;
                let (batch, _) = run_probe::<BatchWorld>(backend, *case).await;
                let (wire, _) = run_probe::<WireWorld>(backend, *case).await;

                for (world, observed) in [
                    ("tools", &tools),
                    ("manager", &manager),
                    ("batch", &batch),
                    ("wire", &wire),
                ] {
                    case.expected
                        .check(observed, before.as_deref())
                        .unwrap_or_else(|e| panic!("{label}: {world} world: {e}"));
                    if case.expected == Outcome::Ok {
                        // Op-level form: `ok_effect` exists on both traits, so the
                        // bare call would be ambiguous (E0034).
                        OpAdapterMeta::ok_effect(&Probe, observed)
                            .unwrap_or_else(|e| panic!("{label}: {world} world: {e}"));
                    }
                }

                assert_eq!(tools, manager, "{label}: tools vs manager DIVERGED");
                assert_eq!(tools, batch, "{label}: tools vs batch DIVERGED");
                assert_eq!(tools, wire, "{label}: tools vs wire DIVERGED");
            }
        }
    }
}
