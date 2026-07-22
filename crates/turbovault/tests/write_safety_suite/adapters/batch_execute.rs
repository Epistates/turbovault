//! `batch_execute` adapter — **multi-op** (N operations, all-or-nothing). The
//! other odd shape (besides `move_note`'s dual-path): a case is a whole op list
//! plus an atomicity expectation, not `(precondition, state)`. It drives the
//! tools-arm batch surface. THIN: the two atomicity cells that lock the shape.

use libtest_mimic::Trial;

use super::{cell_trial, present_state};
use crate::harness::backend::{Backend, Layer, MSG, ToolsWorld, observe};
use turbovault_tools::{BatchOperation, BatchTools};

fn create(path: &str, content: &str) -> BatchOperation {
    BatchOperation::CreateNote {
        path: path.to_string(),
        content: content.to_string(),
        force: None,
    }
}

pub fn trials(backend: Backend) -> Vec<Trial> {
    let label = ToolsWorld::LABEL;
    let b = backend.code();
    vec![
        // Atomic success: two creates on absent paths → both land.
        cell_trial(
            format!("{label}::{b}::batch_execute::two-creates::atomic-OK"),
            None,
            move || async move {
                let w = ToolsWorld::new(backend);
                let obs = observe(
                    BatchTools::new(w.vault().manager().clone())
                        .batch_execute(vec![create("a.md", "A"), create("b.md", "B")], MSG)
                        .await
                        .map(|_| ()),
                    None,
                );
                if obs.succeeded && w.vault().read("a.md").is_some() && w.vault().read("b.md").is_some()
                {
                    Ok(())
                } else {
                    Err(format!(
                        "batch did not create both (ok={}, a={}, b={})",
                        obs.succeeded,
                        w.vault().read("a.md").is_some(),
                        w.vault().read("b.md").is_some()
                    ))
                }
            },
        ),
        // Atomic abort: a collision in the 2nd op must roll back the 1st — the
        // first path must NOT exist after the batch fails. git is all-or-nothing;
        // direct multi-file is best-effort, so this cell's direct arm is the M5.2
        // atomicity delta (mark pending there, not here).
        cell_trial(
            format!("{label}::{b}::batch_execute::collision-aborts-atomically"),
            None,
            move || async move {
                let w = ToolsWorld::new(backend);
                let _ = w.vault().build_state("exists.md", present_state(backend));
                let _ = BatchTools::new(w.vault().manager().clone())
                    .batch_execute(vec![create("fresh.md", "F"), create("exists.md", "X")], MSG)
                    .await;
                if w.vault().read("fresh.md").is_none() {
                    Ok(())
                } else {
                    Err("NON-atomic: fresh.md was created despite the colliding op aborting".into())
                }
            },
        ),
    ]
}
