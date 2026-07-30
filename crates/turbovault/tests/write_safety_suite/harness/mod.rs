//! Shared harness primitives for the WSS write-safety matrix (turbovault-nbl.1).
//!
//! Primitives here are backend-independent and know nothing about any operation
//! under test. Each mutating op is a thin adapter that composes these. See the
//! design doc §7.

// Shared by two test binaries: the default-harness `write_safety_suite`
// (self-tests) and the `harness = false` `wss_matrix` (cells). Each exercises a
// different slice of the harness, and `wss_matrix` compiles the self-test `mod
// tests` without running them — so per-target "unused" is expected here.
//
// BOTH lints are load-bearing; this was MEASURED (2026-07-29), don't drop either:
//   * `dead_code`      — 27 items warn, each in exactly ONE target: `probe.rs` is
//     unused by `wss_matrix`, while `op.rs`'s trial machinery (`op_trials`,
//     `cell_trial`, `run_single_cell`) is unused by `write_safety_suite`. Nothing
//     is dead in BOTH, i.e. this masks only the asymmetry, never real dead code.
//   * `unused_imports` — 5 warn, ALL from `wss_matrix`, all inside `#[cfg(test)]
//     mod tests`. They are NOT removable: deleting one (`outcome.rs`'s
//     `use super::*`) fails `write_safety_suite` with 14 errors, because that is
//     the target where those tests actually run. Same source, two compilations.
// Per-item `#[allow]`s would be 32 annotations for zero signal gained.
#![allow(dead_code, unused_imports)]

pub mod backend;
pub mod op;
pub mod outcome;
pub mod precondition;
pub mod probe;
pub mod state;
