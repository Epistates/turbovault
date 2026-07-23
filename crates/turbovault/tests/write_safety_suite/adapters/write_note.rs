//! `write_note` adapter — a wholesale-replace op (design doc §4 default:
//! `ExpectAbsent`). A single-path op, so it rides the [`SinglePathOp`] mold.
//!
//! `invoke` drives the aspirational `write` op on the tools-layer surface,
//! passing the [`Precondition`] directly. The tool layer does not take a
//! precondition yet, so this does not compile until the cutover (qae.9.1).

use super::{Case, SinglePathOp};
use crate::harness::backend::{Backend, BatchWorld, Layer, MSG, ManagerWorld, ToolsWorld, observe};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;
use turbovault_tools::BatchOperation;
use turbovault_tools::FileTools;
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

// Batch-layer invoker (qae.9.3): wrap the write in a ONE-op batch. The
// precondition picks the batch op that carries it — a strict create
// (`CreateNote`, expect_absent) for `ExpectAbsent`, an upsert (`WriteNote`) for
// `Blind`/`ExpectBlob`. Shares `CASES` + `ok_check`; batch-of-one == standalone.
impl SinglePathOp<BatchWorld> for WriteNote {
    fn name(&self) -> &'static str {
        "write_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &BatchWorld, rel: &str, pc: Precondition) -> Observed {
        let op = match pc {
            Precondition::ExpectAbsent => BatchOperation::CreateNote {
                path: rel.to_string(),
                content: CONTENT.to_string(),
                force: None,
            },
            Precondition::ExpectBlob(oid) => BatchOperation::WriteNote {
                path: rel.to_string(),
                content: CONTENT.to_string(),
                expected_hash: Some(oid),
            },
            // Blind (and the unreachable ExpectExists) → a no-precondition upsert.
            _ => BatchOperation::WriteNote {
                path: rel.to_string(),
                content: CONTENT.to_string(),
                expected_hash: None,
            },
        };
        w.run_batch_of_one(op, rel).await
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        ok_check(observed)
    }
}

/// The **full** write_note matrix, derived from the corrected CSV by collapsing
/// the `force × expected_hash` grid onto the single precondition axis (design
/// doc §3). Grouped by precondition; states in matrix column order. N/A cells
/// (token undefined for the state) and SKIP duplicates (WORKDIR == HEAD/INDEX)
/// are omitted.
///
/// `pending` = a cell current code gets wrong (the nbl.8 burndown); its reason is
/// the trial-name-derived nbl.8 tag (`--include-ignored` is the source of truth).
/// The `e---u`/Untracked cells split the git arm (pending burndown) from the
/// direct arm (already correct → active): a single shared `pending` flag can't be
/// right for both backends.
const CASES: &[Case] = &[
    // ── Blind (no precondition) → OK in every state (git dirty states pending) ─
    Case::new(P::Blind, S::Absent, O::Ok),
    Case::new(P::Blind, S::CleanCommitted, O::Ok),
    Case::pending(P::Blind, S::CommittedStaged, O::Ok),
    Case::pending(P::Blind, S::CommittedUnstaged, O::Ok),
    Case::pending(P::Blind, S::CommittedStagedUnstaged, O::Ok),
    Case::pending(P::Blind, S::NewStaged, O::Ok),
    Case::pending(P::Blind, S::IntentToAdd, O::Ok),
    Case::pending(P::Blind, S::NewStagedUnstaged, O::Ok),
    Case::pending(P::Blind, S::Untracked, O::Ok).on(Backend::Git),
    Case::new(P::Blind, S::Untracked, O::Ok).on(Backend::Direct),
    // ── ExpectAbsent (create-only) → OK on absent, else refuse ───────────────
    Case::new(P::Absent, S::Absent, O::Ok),
    Case::new(P::Absent, S::CleanCommitted, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Absent, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Absent, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Absent, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Absent, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Case::new(P::Absent, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
    // ── ExpectBlob(HEAD) — defined iff committed ─────────────────────────────
    Case::new(P::Head, S::CleanCommitted, O::Ok),
    Case::new(P::Head, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Head, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Head, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // ── ExpectBlob(INDEX) — defined iff staged ───────────────────────────────
    Case::pending(P::Index, S::CommittedStaged, O::Ok),
    Case::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::pending(P::Index, S::NewStaged, O::Ok),
    Case::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ── ExpectBlob(WORKDIR) — proving the on-disk bytes; SKIP where ==HEAD/INDEX ─
    Case::pending(P::Workdir, S::CommittedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::CommittedStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::IntentToAdd, O::Ok),
    Case::pending(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::Untracked, O::Ok).on(Backend::Git),
    Case::new(P::Workdir, S::Untracked, O::Ok).on(Backend::Direct),
    // ── ExpectBlob(WRONG) → refuse in every state ────────────────────────────
    Case::new(P::Wrong, S::Absent, O::ConcurrencyError),
    Case::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Case::new(P::Wrong, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
];
