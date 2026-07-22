//! `write_note` adapter — a wholesale-replace op (design doc §4 default:
//! `ExpectAbsent`). A single-path op, so it rides the [`SinglePathOp`] mold.
//!
//! `invoke` drives the aspirational `write` op on the tools-layer surface,
//! passing the [`Precondition`] directly. The tool layer does not take a
//! precondition yet, so this does not compile until the cutover (qae.9.1).

use super::{Case, SinglePathOp};
use crate::harness::backend::{Layer, MSG, ManagerWorld, ToolsWorld, observe};
use turbovault_tools::FileTools;
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;
use turbovault_tools::WriteMode;

/// The bytes a successful write leaves — distinct from the state's own
/// generations so an `Ok` is observable as a real change.
const CONTENT: &str = "wss-written\n";

#[derive(Clone, Copy)]
pub struct WriteNote;

/// Shared OK-effect check for every layer's invoker (op-specific, layer-agnostic).
fn ok_check(observed: &Observed) -> Result<(), String> {
    if observed.after_content.as_deref() == Some(CONTENT) {
        Ok(())
    } else {
        Err(format!(
            "OK effect: expected written content {CONTENT:?}, got {:?}",
            observed.after_content
        ))
    }
}

// Tools-layer invoker: construct `FileTools` from the vault's manager and call it.
impl SinglePathOp<ToolsWorld> for WriteNote {
    fn name(&self) -> &'static str {
        "write_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let res = FileTools::new(w.vault().manager().clone())
            .write_file_with_mode(rel, CONTENT, WriteMode::Overwrite, pc, MSG)
            .await;
        observe(res, w.vault().read(rel))
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        ok_check(observed)
    }
}

// Manager-layer invoker (qae.9.2 demo): call `VaultManager` directly. This one
// COMPILES today — the manager already takes a `Precondition` — so the manager
// arm can run pre-cutover. Not wired into `main` yet (qae.9.2 wires the arm); it
// exists here to show the SAME op carrying a second, layer-specific invoker,
// sharing `CASES` and `ok_check`.
impl SinglePathOp<ManagerWorld> for WriteNote {
    fn name(&self) -> &'static str {
        "write_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &ManagerWorld, rel: &str, pc: Precondition) -> Observed {
        let res = w
            .vault()
            .manager()
            .write_file(std::path::Path::new(rel), CONTENT, pc, MSG)
            .await;
        observe(res, w.vault().read(rel))
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        ok_check(observed)
    }
}

// Burndown reasons (shared by the cells that pin each defect).
const PRECOND_VS_HEAD: &str = "WSS: precondition checked vs HEAD, not the working tree";
const HEAD_CLOBBER: &str =
    "WSS: dirty-tree clobber — HEAD token passes vs HEAD, materialize discards uncommitted content";
const ABSENT_CLOBBER: &str =
    "WSS: expect_absent checks HEAD, so an uncommitted-but-present file is clobbered, not refused";

/// The **full** write_note matrix, derived from the corrected CSV by collapsing
/// the `force × expected_hash` grid onto the single precondition axis (design
/// doc §3). Grouped by precondition; states in matrix column order. N/A cells
/// (token undefined for the state) are simply omitted; SKIP duplicates
/// (WORKDIR == HEAD/INDEX) are omitted too. `pending` = a cell current code gets
/// wrong (the burndown).
const CASES: &[Case] = &[
    // ── Blind (no precondition) → OK in every state ──────────────────────────
    Case::new(P::Blind, S::Absent, O::Ok),
    Case::new(P::Blind, S::CleanCommitted, O::Ok),
    Case::new(P::Blind, S::CommittedStaged, O::Ok),
    Case::new(P::Blind, S::CommittedUnstaged, O::Ok),
    Case::new(P::Blind, S::CommittedStagedUnstaged, O::Ok),
    Case::new(P::Blind, S::NewStaged, O::Ok),
    Case::new(P::Blind, S::IntentToAdd, O::Ok),
    Case::new(P::Blind, S::NewStagedUnstaged, O::Ok),
    Case::new(P::Blind, S::Untracked, O::Ok),
    // ── ExpectAbsent (create-only) → OK on absent, else refuse ───────────────
    Case::new(P::Absent, S::Absent, O::Ok),
    Case::new(P::Absent, S::CleanCommitted, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // uncommitted-but-present: HEAD has no entry, so expect_absent wrongly passes.
    Case::pending(P::Absent, S::NewStaged, O::ConcurrencyError, ABSENT_CLOBBER),
    Case::pending(
        P::Absent,
        S::IntentToAdd,
        O::ConcurrencyError,
        ABSENT_CLOBBER,
    ),
    Case::pending(
        P::Absent,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        ABSENT_CLOBBER,
    ),
    Case::pending(P::Absent, S::Untracked, O::ConcurrencyError, ABSENT_CLOBBER),
    // ── ExpectBlob(HEAD) — defined iff committed ─────────────────────────────
    Case::new(P::Head, S::CleanCommitted, O::Ok),
    // dirty: HEAD token matches HEAD-tree, so it passes and materialize clobbers.
    Case::pending(
        P::Head,
        S::CommittedStaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    Case::pending(
        P::Head,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    Case::pending(
        P::Head,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    // ── ExpectBlob(INDEX) — defined iff staged ───────────────────────────────
    // INDEX == workdir (no unstaged) → proving it should pass; today vs HEAD → refuse.
    Case::pending(P::Index, S::CommittedStaged, O::Ok, PRECOND_VS_HEAD),
    Case::pending(P::Index, S::NewStaged, O::Ok, PRECOND_VS_HEAD),
    // INDEX != workdir (unstaged on top) → correctly refuses today.
    Case::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ── ExpectBlob(WORKDIR) — proving the on-disk bytes; SKIP where ==HEAD/INDEX ─
    // All should be OK (you proved the current bytes); today checked vs HEAD.
    Case::pending(P::Workdir, S::CommittedUnstaged, O::Ok, PRECOND_VS_HEAD),
    Case::pending(
        P::Workdir,
        S::CommittedStagedUnstaged,
        O::Ok,
        PRECOND_VS_HEAD,
    ),
    Case::pending(P::Workdir, S::IntentToAdd, O::Ok, PRECOND_VS_HEAD),
    Case::pending(P::Workdir, S::NewStagedUnstaged, O::Ok, PRECOND_VS_HEAD),
    Case::pending(P::Workdir, S::Untracked, O::Ok, PRECOND_VS_HEAD),
    // ── ExpectBlob(WRONG) → refuse in every state ────────────────────────────
    Case::new(P::Wrong, S::Absent, O::ConcurrencyError),
    Case::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::Untracked, O::ConcurrencyError),
];
