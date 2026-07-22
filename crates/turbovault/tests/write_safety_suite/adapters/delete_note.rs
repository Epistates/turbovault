//! `delete_note` adapter — an in-place op whose OK *effect* is that the target
//! is **gone** (not that content changed). Single-path → [`SinglePathOp`] mold,
//! overriding `ok_effect`.
//!
//! The oz6 backlink axis (refuse to delete a note with inbound links) is a
//! tool-layer behavior today, not substrate-layer, so at this layer it's a
//! deferred one-off (noted below), tracked with the substrate move of oz6.

use super::{Case, SinglePathOp};
use crate::harness::backend::{Layer, MSG, ToolsWorld, observe};
use turbovault_tools::FileTools;
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;

#[derive(Clone, Copy)]
pub struct DeleteNote;

impl SinglePathOp<ToolsWorld> for DeleteNote {
    fn name(&self) -> &'static str {
        "delete_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let res = FileTools::new(w.vault().manager().clone()).delete_file(rel, pc, MSG).await;
        observe(res, w.vault().read(rel))
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        if observed.after_content.is_none() {
            Ok(())
        } else {
            Err(format!(
                "OK effect: target still present after delete: {:?}",
                observed.after_content
            ))
        }
    }
}

/// The **full** delete_note matrix. In-place op → precondition axis
/// {Exists, Head, Index, Workdir, Wrong}. Two cells diverge from the other
/// in-place ops (edit/frontmatter/tags): `Exists`+absent is an **idempotent OK**
/// (the goal, absence, already holds — ratified) and `Wrong`+absent is a
/// `ConcurrencyError` (the caller asserted a blob that isn't there), *not*
/// `NoFile`. Head/Index/Workdir rows are identical to the other in-place ops.
/// `pending` set determined by running (design doc §6).
const CASES: &[Case] = &[
    // ── ExpectExists (in-place default, dirty-gated) ─────────────────────────
    // Deleting an absent target is an idempotent no-op success; clean deletes.
    Case::new(P::Exists, S::Absent, O::Ok),
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    // No content proof on a dirty tree must refuse — today no dirty gate, so it
    // deletes and discards the uncommitted content.
    Case::pending(
        P::Exists,
        S::CommittedStaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(
        P::Exists,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(
        P::Exists,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(P::Exists, S::NewStaged, O::ConcurrencyError, DIRTY_GATE),
    Case::pending(P::Exists, S::IntentToAdd, O::ConcurrencyError, DIRTY_GATE),
    Case::pending(
        P::Exists,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(P::Exists, S::Untracked, O::ConcurrencyError, DIRTY_GATE),
    // ── ExpectBlob(HEAD) — defined iff committed ─────────────────────────────
    Case::new(P::Head, S::CleanCommitted, O::Ok),
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
    Case::pending(P::Index, S::CommittedStaged, O::Ok, PRECOND_VS_HEAD),
    Case::pending(P::Index, S::NewStaged, O::Ok, PRECOND_VS_HEAD),
    Case::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ── ExpectBlob(WORKDIR) — proving on-disk bytes; SKIP where == HEAD/INDEX ─
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
    // ── ExpectBlob(WRONG) → refuse everywhere, incl. absent ──────────────────
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

// Burndown reasons.
const DIRTY_GATE: &str = "WSS: no dirty gate for delete (discards uncommitted content)";
const HEAD_CLOBBER: &str =
    "WSS: dirty-tree clobber — HEAD token passes vs HEAD, delete discards dirty content";
const PRECOND_VS_HEAD: &str = "WSS: precondition checked vs HEAD, not the working tree";
