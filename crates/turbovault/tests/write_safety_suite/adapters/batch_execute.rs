//! `batch_execute` adapter (qae.9.3) — the BatchWorld layer's per-op isolation
//! shape (batch-of-one == standalone): every single-path op carries a
//! `SinglePathOp<BatchWorld>` invoker (in its own adapter module) that wraps the
//! op in a ONE-op batch and drives it through the batch translation path
//! ([`run_batch_of_one`]). `wss-batch-matrix.csv` states per-op behavior in a
//! batch is EXACTLY the standalone op, so those invokers reuse each op's
//! `cases()` table. The equalizer is the substrate's shared dirty gate + CAS: the
//! one-op plan carries exactly the op's precondition, so a batch-of-one and the
//! standalone mutator resolve to the same outcome.
//!
//! Multi-op transaction-integrity (whole-op-list atomicity/rollback/collision/
//! empty-batch validation) is a DIFFERENT axis from WSS's per-write
//! clobber-safety and was moved out of WSS scope — see `turbovault-nbl.17` /
//! `docs/write-safety-suite/`.

use crate::harness::backend::{BatchWorld, Layer, MSG, observe};
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
