//! `delete_note` adapter — an in-place op whose OK *effect* is that the target
//! is **gone** (not that content changed). Single-path → [`SinglePathOp`] mold,
//! overriding `ok_effect`.
//!
//! The oz6 backlink axis (refuse to delete a note with inbound links) is a
//! tool-layer behavior today, not substrate-layer, so at this layer it's a
//! deferred one-off (noted below), tracked with the substrate move of oz6.

use super::batch_execute::{blob_token, run_batch_of_one};
use super::{Case, SinglePathOp};
use crate::harness::backend::{Backend, BatchWorld, Layer, MSG, ManagerWorld, ToolsWorld, observe};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;
use turbovault_tools::BatchOperation;
use turbovault_tools::FileTools;

#[derive(Clone, Copy)]
pub struct DeleteNote;

/// Shared OK-effect check for every layer's invoker: a successful delete leaves
/// the target **gone** (op-specific, layer-agnostic).
fn ok_check(observed: &Observed) -> Result<(), String> {
    if observed.after_content.is_none() {
        Ok(())
    } else {
        Err(format!(
            "OK effect: target still present after delete: {:?}",
            observed.after_content
        ))
    }
}

impl SinglePathOp<ToolsWorld> for DeleteNote {
    fn name(&self) -> &'static str {
        "delete_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let res = FileTools::new(w.vault().manager().clone())
            .delete_file(rel, pc, MSG)
            .await;
        observe(res, w.vault().read(rel))
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        ok_check(observed)
    }
}

// Manager-layer invoker (qae.9.2): call `VaultManager::delete_file` directly.
// The tool `delete_file` is a thin delegator to this method (its oz6 backlink
// gate is a no-op here — `note.md` has no inbound links), so this arm shares
// `CASES` + `ok_check`.
impl SinglePathOp<ManagerWorld> for DeleteNote {
    fn name(&self) -> &'static str {
        "delete_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &ManagerWorld, rel: &str, pc: Precondition) -> Observed {
        let res = w
            .vault()
            .manager()
            .delete_file(std::path::Path::new(rel), pc, MSG)
            .await;
        observe(res, w.vault().read(rel))
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        ok_check(observed)
    }
}

// Batch-layer invoker (qae.9.3): the delete as a ONE-op `DeleteNote` batch
// (`on_backlinks: None` = the "refuse-if-inbound-links" default — a no-op here,
// `note.md` has no linkers). `blob_token` carries `ExpectBlob`, else a bare
// remove. Uses `BATCH_CASES`, which diverges from the standalone table at ONE
// cell: `Exists`/absent. A bare batch remove of an absent path folds to an
// identity plan (nothing to remove) → the substrate's no-op-commit short-circuit
// returns Ok — i.e. batch delete-of-absent is the idempotent OK the standalone
// `delete_file(ExpectExists)` still refuses (its pending burndown cell). Batch
// gets it right, so that cell is `new(Ok)` here.
impl SinglePathOp<BatchWorld> for DeleteNote {
    fn name(&self) -> &'static str {
        "delete_note"
    }

    fn cases(&self) -> &'static [Case] {
        BATCH_CASES
    }

    async fn invoke(&self, w: &BatchWorld, rel: &str, pc: Precondition) -> Observed {
        let op = BatchOperation::DeleteNote {
            path: rel.to_string(),
            expected_hash: blob_token(&pc),
            on_backlinks: None,
        };
        run_batch_of_one(w, op, rel).await
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        ok_check(observed)
    }
}

/// The **full** delete_note matrix. In-place op → precondition axis
/// {Exists, Head, Index, Workdir, Wrong}. Two cells diverge from the other
/// in-place ops (edit/frontmatter/tags): `Exists`+absent is an **idempotent OK**
/// (the goal, absence, already holds — ratified) and `Wrong`+absent is a
/// `ConcurrencyError` (the caller asserted a blob that isn't there), *not*
/// `NoFile`. Head/Index/Workdir rows are identical to the other in-place ops.
/// `pending` = a cell current code gets wrong (the nbl.8 burndown), with a
/// trial-name-derived reason; `--include-ignored` is the source of truth. The
/// `e---u`/Untracked cells split the git arm (burndown) from the direct arm
/// (already correct → active).
const CASES: &[Case] = &[
    // ── ExpectExists (in-place default, dirty-gated) ─────────────────────────
    // delete-of-absent should be an idempotent OK, but current code refuses it.
    Case::pending(P::Exists, S::Absent, O::Ok),
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    Case::new(P::Exists, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Exists, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Exists, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Exists, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Case::pending(P::Exists, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
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
    // ── ExpectBlob(WORKDIR) — proving on-disk bytes; SKIP where == HEAD/INDEX ─
    Case::pending(P::Workdir, S::CommittedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::CommittedStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::IntentToAdd, O::Ok),
    Case::pending(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::Untracked, O::Ok).on(Backend::Git),
    Case::new(P::Workdir, S::Untracked, O::Ok).on(Backend::Direct),
    // ── ExpectBlob(WRONG) → refuse everywhere, incl. absent ──────────────────
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

/// The batch-layer delete matrix. Identical to [`CASES`] except the one cell
/// where a batch delete genuinely diverges from the standalone op: `Exists` on
/// an **absent** target. A bare batch `DeleteNote` of an absent path folds to an
/// identity plan (nothing to remove), which the git substrate's no-op-commit
/// short-circuit reports as `Ok` — the idempotent delete-of-absent the standalone
/// `delete_file(ExpectExists)` still refuses (its pending burndown cell). So the
/// git arm is active-`Ok` here. Direct has no no-op short-circuit — a remove of
/// an absent path still errors — so its arm stays `pending` (matching standalone).
/// Every other cell coincides with the standalone table (the shared dirty gate +
/// CAS equalize them), so this reuses those rows verbatim.
const BATCH_CASES: &[Case] = &[
    // ── ExpectExists (in-place default, dirty-gated) ─────────────────────────
    // The divergent cell: batch delete-of-absent is idempotent OK on git, still
    // refused on direct (see the doc comment above).
    Case::new(P::Exists, S::Absent, O::Ok).on(Backend::Git),
    Case::pending(P::Exists, S::Absent, O::Ok).on(Backend::Direct),
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    Case::new(P::Exists, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Exists, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Exists, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Exists, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Case::pending(P::Exists, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
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
    // ── ExpectBlob(WORKDIR) — proving on-disk bytes; SKIP where == HEAD/INDEX ─
    Case::pending(P::Workdir, S::CommittedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::CommittedStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::IntentToAdd, O::Ok),
    Case::pending(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::Untracked, O::Ok).on(Backend::Git),
    Case::new(P::Workdir, S::Untracked, O::Ok).on(Backend::Direct),
    // ── ExpectBlob(WRONG) → refuse everywhere, incl. absent ──────────────────
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
