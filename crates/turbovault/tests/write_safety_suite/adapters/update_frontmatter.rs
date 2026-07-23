//! `update_frontmatter` adapter — an in-place op (design doc §4 default:
//! `ExpectExists`, dirty-gated). Single-path → [`SinglePathOp`] mold.
//!
//! `invoke` drives the aspirational `update_frontmatter` op on the
//! tools-layer surface, passing the [`Precondition`] directly; the tool layer
//! does not take one yet (cutover: qae.9.1). The dirty-gate / precond-vs-workdir
//! cells are `pending` (the nbl.8 burndown).

use super::{Case, SinglePathOp};
use crate::harness::backend::{Backend, Layer, MSG, ToolsWorld, observe};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;
use turbovault_tools::MetadataTools;

const KEY: &str = "wss_touched";

#[derive(Clone, Copy)]
pub struct UpdateFrontmatter;

impl SinglePathOp<ToolsWorld> for UpdateFrontmatter {
    fn name(&self) -> &'static str {
        "update_frontmatter"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let mut fm = serde_json::Map::new();
        fm.insert(KEY.to_string(), serde_json::json!(true));
        let res = MetadataTools::new(w.vault().manager().clone())
            .update_frontmatter(rel, fm, true, pc, MSG)
            .await
            .map(|_| ());
        observe(res, w.vault().read(rel))
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        if observed
            .after_content
            .as_deref()
            .is_some_and(|c| c.contains(KEY))
        {
            Ok(())
        } else {
            Err(format!(
                "OK effect: frontmatter key {KEY:?} not present: {:?}",
                observed.after_content
            ))
        }
    }
}

/// The **full** update_frontmatter matrix. In-place op → precondition axis
/// {Exists, Head, Index, Workdir, Wrong}; desired outcomes are identical to
/// `edit_note`'s (same matrix rows). `pending` = a cell current code gets wrong
/// (the nbl.8 burndown), with a trial-name-derived reason; `--include-ignored` is
/// the source of truth. The `e---u`/Untracked cells split the git arm (burndown)
/// from the direct arm (already correct → active).
const CASES: &[Case] = &[
    // ── ExpectExists (in-place default, dirty-gated) ─────────────────────────
    Case::new(P::Exists, S::Absent, O::NoFile),
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    Case::pending(
        P::Exists,
        S::CommittedStaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Exists,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Exists,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Exists,
        S::NewStaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Exists,
        S::IntentToAdd,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Exists,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Exists,
        S::Untracked,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    // ── ExpectBlob(HEAD) — defined iff committed ─────────────────────────────
    Case::new(P::Head, S::CleanCommitted, O::Ok),
    Case::pending(
        P::Head,
        S::CommittedStaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Head,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Head,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    // ── ExpectBlob(INDEX) — defined iff staged ───────────────────────────────
    Case::pending(
        P::Index,
        S::CommittedStaged,
        O::Ok,
        "nbl.8: dirty gate must honor Blind/WORKDIR opt-out",
    ),
    Case::pending(
        P::Index,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Index,
        S::NewStaged,
        O::Ok,
        "nbl.8: dirty gate must honor Blind/WORKDIR opt-out",
    ),
    Case::pending(
        P::Index,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    // ── ExpectBlob(WORKDIR) — proving on-disk bytes; SKIP where == HEAD/INDEX ─
    Case::pending(
        P::Workdir,
        S::CommittedUnstaged,
        O::Ok,
        "nbl.8: dirty gate must honor Blind/WORKDIR opt-out",
    ),
    Case::pending(
        P::Workdir,
        S::CommittedStagedUnstaged,
        O::Ok,
        "nbl.8: dirty gate must honor Blind/WORKDIR opt-out",
    ),
    Case::pending(
        P::Workdir,
        S::IntentToAdd,
        O::Ok,
        "nbl.8: dirty gate must honor Blind/WORKDIR opt-out",
    ),
    Case::pending(
        P::Workdir,
        S::NewStagedUnstaged,
        O::Ok,
        "nbl.8: dirty gate must honor Blind/WORKDIR opt-out",
    ),
    Case::pending(
        P::Workdir,
        S::Untracked,
        O::Ok,
        "nbl.8: dirty gate must honor Blind/WORKDIR opt-out",
    )
    .on(Backend::Git),
    Case::new(P::Workdir, S::Untracked, O::Ok).on(Backend::Direct),
    // ── ExpectBlob(WRONG) → refuse everywhere; NoFile on absent ──────────────
    Case::new(P::Wrong, S::Absent, O::NoFile),
    Case::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Case::pending(
        P::Wrong,
        S::CommittedStaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Wrong,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Wrong,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Wrong,
        S::NewStaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Wrong,
        S::IntentToAdd,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Wrong,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    ),
    Case::pending(
        P::Wrong,
        S::Untracked,
        O::ConcurrencyError,
        "nbl.8: refusal not yet unified to ConcurrencyError",
    )
    .on(Backend::Git),
    Case::new(P::Wrong, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
];
