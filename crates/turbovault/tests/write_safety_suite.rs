//! WSS write-safety matrix suite (aspirational).
//!
//! Backend-parameterized, matrix-driven tests asserting the *desired*
//! write-safety behavior of every mutating TurboVault operation across the
//! full space of working-tree state × precondition. Tests assert desired
//! behavior, so many fail (or are `#[ignore]`d) until the behavior lands. The
//! per-cell outcomes are encoded directly in each adapter's `Case` table, and
//! every cell self-describes in its trial name.
//!
//! This target (default harness) runs the harness's OWN unit tests.

// An integration-test file is its own crate root, so `mod harness;` would look
// for a sibling `tests/harness.rs` (which cargo would compile as a stray test
// binary). `#[path]` keeps the harness in its own subtree; nested `mod`s inside
// harness.rs then resolve relative to `write_safety_suite/harness/`.
// This target (default harness) runs the harness's OWN unit tests — the meta
// tests that keep the primitives honest. The matrix cells run in the sibling
// `wss_matrix` target (harness=false), one named trial per cell.
#[path = "write_safety_suite/harness/mod.rs"]
mod harness;
