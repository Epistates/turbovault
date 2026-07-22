//! `manage_tags` adapter — an in-place op (design doc §4 default: `ExpectExists`,
//! dirty-gated). Single-path → [`SinglePathOp`] mold; a sibling of `edit_note` /
//! `update_frontmatter`.
//!
//! `invoke` drives the aspirational `manage_tags` op on the tools-layer surface,
//! passing the [`Precondition`] directly; the tool layer does not take one yet
//! (cutover: qae.9.1). The dirty-gate / precond-vs-workdir cells are `pending`
//! (nbl.8 burndown).

use super::{Case, SinglePathOp};
use crate::harness::backend::{Layer, MSG, ToolsWorld, observe};
use turbovault_tools::MetadataTools;
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;

const TAG: &str = "wss-tag";

#[derive(Clone, Copy)]
pub struct ManageTags;

impl SinglePathOp<ToolsWorld> for ManageTags {
    fn name(&self) -> &'static str {
        "manage_tags"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let tags = [TAG.to_string()];
        let res = MetadataTools::new(w.vault().manager().clone())
            .manage_tags(rel, "add", Some(&tags[..]), pc, MSG)
            .await
            .map(|_| ());
        observe(res, w.vault().read(rel))
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        if observed
            .after_content
            .as_deref()
            .is_some_and(|c| c.contains(TAG))
        {
            Ok(())
        } else {
            Err(format!(
                "OK effect: tag {TAG:?} not present: {:?}",
                observed.after_content
            ))
        }
    }
}

/// The **full** manage_tags matrix — same in-place shape and desired outcomes as
/// `edit_note` / `update_frontmatter`. `pending` = a cell whose aspirational
/// behavior the nbl.6 signature cutover does NOT yet deliver (behavior
/// unchanged); un-pending is the nbl.8 burndown. Pending set is exactly the
/// other in-place ops' (same substrate primitives).
const CASES: &[Case] = &[
    // ── ExpectExists (in-place default, dirty-gated) ─────────────────────────
    Case::new(P::Exists, S::Absent, O::NoFile),
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
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
    // ── ExpectBlob(WRONG) → refuse everywhere; NoFile on absent ──────────────
    Case::new(P::Wrong, S::Absent, O::NoFile),
    Case::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::Untracked, O::ConcurrencyError),
];

// Burndown reasons (nbl.8) — the aspirational behavior the cutover defers.
const DIRTY_GATE: &str = "WSS: no dirty gate for in-place tag edit (writes uncommitted bytes)";
const HEAD_CLOBBER: &str =
    "WSS: dirty-tree clobber — HEAD token passes vs HEAD, write applies to dirty bytes";
const PRECOND_VS_HEAD: &str = "WSS: precondition checked vs HEAD, not the working tree";
