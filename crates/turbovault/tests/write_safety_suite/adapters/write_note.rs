//! `write_note` adapter — a wholesale-replace op (design doc §4 default:
//! `ExpectAbsent`). A single-path op, so it rides the [`Op`] mold.
//!
//! `invoke` drives the aspirational `write` op on the tools-layer surface,
//! passing the [`Precondition`] directly. The tool layer does not take a
//! precondition yet, so this does not compile until the cutover (qae.9.1).
//!
//! SCOPE: only `WriteMode::Overwrite` is covered. **`append`/`prepend` are a
//! documented gap — turbovault-nbl.9**, deferred rather than guessed, for two
//! reasons found while scoping it:
//!
//! 1. An unresolved SPEC fork. WSS asserts clobber-safety, and an append never
//!    destroys existing bytes — so an append carrying a STALE `ExpectBlob` is not a
//!    clobber. Either it refuses like every other in-place op (consistent, and what
//!    the code does today) or it proceeds (nothing is lost). The ratified design
//!    says both: §4 calls append/prepend "in-place → `ExpectExists` + dirty-gated",
//!    while §9 lists "nbl.9 append/prepend CAS semantics" as an OPEN non-goal.
//!    Writing either answer into `wss-precondition-matrix.csv` would publish a
//!    contract we are not sure of — the one expensive mistake for an authoritative
//!    spec.
//! 2. The layers disagree on whether the mode even exists: `FileTools::
//!    write_file_with_mode` and the `write_note` wire param take a `WriteMode`, but
//!    `VaultManager::write_file` has no mode parameter and `BatchOperation` has no
//!    append/prepend variant. So the required LAYER coverage is unsettled too.
//!
//! Content-correctness of an append (does prepend land after the frontmatter?) is
//! NOT what this gap is about — that is a plain functional test's job, never a WSS
//! cell (see the README's scope boundary).

use crate::harness::backend::{
    Backend, BatchWorld, Layer, MSG, ManagerWorld, ToolsWorld, WireWorld, observe, observe_outcome,
};
use crate::harness::op::{Case, Op, OpAdapterMeta};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P, sentinel};
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

// Op-level surface: identity + shared `CASES` + the OK-effect check, stated once.
impl OpAdapterMeta for WriteNote {
    fn name(&self) -> &'static str {
        "write_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        ok_check(observed)
    }
}

// Tools-layer invoker: construct `FileTools` from the vault's manager and call it.
impl Op<ToolsWorld> for WriteNote {
    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let res = FileTools::new(w.vault().manager().clone())
            .write_file_with_mode(rel, CONTENT, WriteMode::Overwrite, pc, MSG)
            .await;
        observe(res, w.vault().read(rel))
    }
}

// Manager-layer invoker (qae.9.2 demo): call `VaultManager` directly. This one
// COMPILES today — the manager already takes a `Precondition` — so the manager
// arm can run pre-cutover. Shares `CASES`/`ok_check` via `OpAdapterMeta`.
impl Op<ManagerWorld> for WriteNote {
    async fn invoke(&self, w: &ManagerWorld, rel: &str, pc: Precondition) -> Observed {
        let res = w
            .vault()
            .manager()
            .write_file(std::path::Path::new(rel), CONTENT, pc, MSG)
            .await;
        observe(res, w.vault().read(rel))
    }
}

// Batch-layer invoker (qae.9.3): wrap the write in a ONE-op batch. The
// precondition picks the batch op that carries it — a strict create
// (`CreateNote`, expect_absent) for `ExpectAbsent`, an upsert (`WriteNote`) for
// `Blind`/`ExpectBlob`. `observe(w.apply_op(..))` is the same shape as the other
// arms; batch-of-one == standalone.
impl Op<BatchWorld> for WriteNote {
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
        observe(w.apply_op(op).await, w.vault().read(rel))
    }
}

// Wire-layer invoker (nbl.12): drive the real `write_note` MCP handler in-process
// via `w.call_tool`, encoding the precondition as the ratified sentinel
// `expected_hash` string. ASPIRATIONAL: the handler does not decode sentinels yet
// (qae.6.4 wire-decode commit), so the sentinel cells fail until it does — the wire
// arm drives that requirement.
impl Op<WireWorld> for WriteNote {
    async fn invoke(&self, w: &WireWorld, rel: &str, pc: Precondition) -> Observed {
        let params = serde_json::json!({
            "path": rel,
            "content": CONTENT,
            "expected_hash": sentinel(&pc),
        });
        let outcome = w.call_tool("write_note", params).await;
        observe_outcome(outcome, w.vault().read(rel))
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
