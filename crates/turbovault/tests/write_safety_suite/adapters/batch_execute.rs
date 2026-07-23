//! `batch_execute` adapter (qae.9.3) — the BatchWorld layer's two shapes:
//!
//! 1. **Per-op isolation** (batch-of-one == standalone): every single-path op
//!    carries a `SinglePathOp<BatchWorld>` invoker (in its own adapter module)
//!    that wraps the op in a ONE-op batch and drives it through the batch
//!    translation path ([`run_batch_of_one`]). `wss-batch-matrix.csv` states
//!    per-op behavior in a batch is EXACTLY the standalone op, so those invokers
//!    reuse each op's `cases()` table. The equalizer is the substrate's shared
//!    dirty gate + CAS: the one-op plan carries exactly the op's precondition, so
//!    a batch-of-one and the standalone mutator resolve to the same outcome.
//!
//! 2. **Multi-op atomicity** (this file's [`trials`]): whole-op-list scenarios —
//!    a case is an op list + an atomicity expectation, not `(precondition,
//!    state)`. Specced by `wss-batch-matrix.csv`: empty batch → loud validation
//!    failure; all-ok distinct paths → one atomic commit; any op refuses →
//!    nothing applied; same-path collision → refused loudly.

use libtest_mimic::Trial;

use super::{cell_trial, present_state};
use crate::harness::backend::{Backend, BatchWorld, Layer, MSG, observe};
use crate::harness::outcome::Observed;
use crate::harness::precondition::Precondition;
use turbovault_tools::{BatchOperation, BatchTools};

/// Drive a SINGLE [`BatchOperation`] through the batch translation path — the
/// same `plan` fold + `apply_changes` that [`BatchTools::batch_execute`] runs
/// internally, minus the soft-envelope wrapping that would stringify (and so
/// erase) the structured error kind the matrix's `Outcome` assertions need.
/// Proves batch-of-one == standalone: the one-op plan carries exactly the op's
/// precondition, and the substrate's shared dirty gate + CAS decide the outcome
/// identically to the standalone mutator.
pub async fn run_batch_of_one(w: &BatchWorld, op: BatchOperation, rel: &str) -> Observed {
    let mgr = w.vault().manager().clone();
    let res = match BatchTools::new(mgr.clone()).plan(&[op]).await {
        Ok(mut plan) => {
            plan.message = MSG.to_string();
            mgr.apply_changes(&plan).await.map(|_| ())
        }
        Err(e) => Err(e),
    };
    observe(res, w.vault().read(rel))
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

/// A strict-create batch op on `path` (the multi-op scenarios' building block).
fn create(path: &str, content: &str) -> BatchOperation {
    BatchOperation::CreateNote {
        path: path.to_string(),
        content: content.to_string(),
        force: None,
    }
}

/// The multi-op atomicity scenarios (whole-op-list shapes) — `wss-batch-matrix`.
pub fn trials(backend: Backend) -> Vec<Trial> {
    let label = BatchWorld::LABEL;
    let b = backend.code();
    vec![
        // Empty batch → loud validation failure (a soft `success:false` envelope,
        // NOT a precondition Outcome), nothing written.
        cell_trial(
            format!("{label}::{b}::batch_execute::empty::loud-failure"),
            false,
            move || async move {
                let w = BatchWorld::new(backend);
                let r = BatchTools::new(w.vault().manager().clone())
                    .batch_execute(vec![], MSG)
                    .await
                    .map_err(|e| {
                        format!("empty batch hard-errored instead of soft failure: {e}")
                    })?;
                if r.success {
                    return Err("empty batch reported success".into());
                }
                if r.executed != 0 {
                    return Err(format!("empty batch executed {} ops", r.executed));
                }
                if r.errors.is_empty() {
                    return Err("empty batch surfaced no error".into());
                }
                Ok(())
            },
        ),
        // All ops Ok on distinct absent paths → success, all applied in ONE commit.
        cell_trial(
            format!("{label}::{b}::batch_execute::all-ok-distinct::atomic-OK"),
            false,
            move || async move {
                let w = BatchWorld::new(backend);
                let r = BatchTools::new(w.vault().manager().clone())
                    .batch_execute(vec![create("a.md", "A"), create("b.md", "B")], MSG)
                    .await
                    .map_err(|e| format!("batch hard-errored: {e}"))?;
                if r.success && w.vault().read("a.md").is_some() && w.vault().read("b.md").is_some()
                {
                    Ok(())
                } else {
                    Err(format!(
                        "batch did not create both (ok={}, a={}, b={})",
                        r.success,
                        w.vault().read("a.md").is_some(),
                        w.vault().read("b.md").is_some()
                    ))
                }
            },
        ),
        // One op refuses (strict-create over a present path) → the whole batch
        // aborts, the would-succeed sibling is NOT applied (all-or-nothing). The
        // refusal is a precondition failure, which the direct backend's atomic
        // precondition GATE catches before any write — so this holds on both
        // backends (best-effort `failed_at` reporting, S2/qae.6.3, isn't needed
        // here; that's only for mid-apply non-precondition failures).
        cell_trial(
            format!("{label}::{b}::batch_execute::one-refusal-aborts-all"),
            false,
            move || async move {
                let w = BatchWorld::new(backend);
                let _ = w.vault().build_state("exists.md", present_state(backend));
                let r = BatchTools::new(w.vault().manager().clone())
                    .batch_execute(vec![create("fresh.md", "F"), create("exists.md", "X")], MSG)
                    .await
                    .map_err(|e| format!("batch hard-errored: {e}"))?;
                if r.success {
                    return Err("batch reported success despite a refusing op".into());
                }
                if w.vault().read("fresh.md").is_some() {
                    return Err("NON-atomic: fresh.md created despite the batch aborting".into());
                }
                Ok(())
            },
        ),
        // Two ops on the SAME path → refused loudly before execution (intra-batch
        // collision, bead 0g4.5), nothing written. Backend-agnostic — the
        // collision is caught in `plan()`.
        cell_trial(
            format!("{label}::{b}::batch_execute::same-path-collision-refused"),
            false,
            move || async move {
                let w = BatchWorld::new(backend);
                let r = BatchTools::new(w.vault().manager().clone())
                    .batch_execute(vec![create("dup.md", "X"), create("dup.md", "Y")], MSG)
                    .await
                    .map_err(|e| format!("batch hard-errored: {e}"))?;
                if r.success {
                    return Err("same-path collision reported success".into());
                }
                if w.vault().read("dup.md").is_some() {
                    return Err("intra-batch collision wrote dup.md".into());
                }
                Ok(())
            },
        ),
    ]
}
