//! `delete_note` adapter — an in-place op whose OK *effect* is that the target
//! is **gone** (not that content changed). Single-path → [`Op`] mold, overriding
//! `ok_effect` at the op level.
//!
//! The oz6 backlink axis (refuse to delete a note with inbound links) is a
//! tool-layer behavior today, not substrate-layer, so at this layer it's a
//! deferred one-off (noted below), tracked with the substrate move of oz6.

use crate::harness::backend::{
    Backend, BatchWorld, Layer, MSG, ManagerWorld, ToolsWorld, WireWorld, observe, observe_outcome,
};
use crate::harness::op::{Case, Op, OpAdapterMeta};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P, sentinel};
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

impl OpAdapterMeta for DeleteNote {
    fn name(&self) -> &'static str {
        "delete_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        ok_check(observed)
    }
}

impl Op<ToolsWorld> for DeleteNote {
    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let res = FileTools::new(w.vault().manager().clone())
            .delete_file(rel, pc, MSG)
            .await;
        observe(res, w.vault().read(rel))
    }
}

// Manager-layer invoker (qae.9.2): call `VaultManager::delete_file` directly.
// The tool `delete_file` is a thin delegator to this method (its oz6 backlink
// gate is a no-op here — `note.md` has no inbound links), so this arm shares
// `CASES` + `ok_check`.
impl Op<ManagerWorld> for DeleteNote {
    async fn invoke(&self, w: &ManagerWorld, rel: &str, pc: Precondition) -> Observed {
        let res = w
            .vault()
            .manager()
            .delete_file(std::path::Path::new(rel), pc, MSG)
            .await;
        observe(res, w.vault().read(rel))
    }
}

// Batch-layer invoker (qae.9.3): the delete as a ONE-op `DeleteNote` batch
// (`on_backlinks: None` = the "refuse-if-inbound-links" default — a no-op here,
// `note.md` has no linkers). `blob_token` carries `ExpectBlob`, else a bare
// remove. Shares the standalone `CASES` — the required behavior is
// layer-invariant. Where batch genuinely diverges from standalone (git batch
// delete-of-absent folds to an identity plan → idempotent Ok, while the
// standalone op still refuses), the shared cell simply DIVERGES: it is `pending`
// (the required idempotent-Ok isn't universally implemented), and the batch arm
// passing it early shows up as an un-pend candidate, not a blessed separate
// outcome. We never fork a per-world table to enshrine that divergence.
impl Op<BatchWorld> for DeleteNote {
    async fn invoke(&self, w: &BatchWorld, rel: &str, pc: Precondition) -> Observed {
        let op = BatchOperation::DeleteNote {
            path: rel.to_string(),
            expected_hash: BatchWorld::blob_token(&pc),
            on_backlinks: None,
        };
        observe(w.apply_op(op).await, w.vault().read(rel))
    }
}

// Wire-layer invoker (nbl.12): the real `delete_note` MCP handler in-process.
// `confirm_path` MUST equal `path` (the tool's delete-safety guard) or it refuses
// before the precondition; the precondition rides the sentinel `expected_hash`.
// Shares `CASES` + `ok_check`.
impl Op<WireWorld> for DeleteNote {
    async fn invoke(&self, w: &WireWorld, rel: &str, pc: Precondition) -> Observed {
        let params = serde_json::json!({
            "path": rel,
            "confirm_path": rel,
            "expected_hash": sentinel(&pc),
        });
        observe_outcome(
            w.call_tool("delete_note", params).await,
            w.vault().read(rel),
        )
    }
}

/// The **full** `delete_note` matrix. In-place op → precondition axis
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
