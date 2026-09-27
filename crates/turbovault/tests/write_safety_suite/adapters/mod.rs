//! Per-operation adapters (design doc §7).
//!
//! Each op's shared data — its `Case` table + its [`OpAdapterMeta`] impl (identity,
//! cases, `ok_effect`) — and its per-layer `Op<W>` invokers live in the op's own
//! module, layer-agnostic. The shared trial/runner machinery (`Case`, the `Op`
//! trait pair, `cell_trial`, `op_trials`) lives in [`crate::harness::op`].
//!
//! [`OpAdapterMeta`]: crate::harness::op::OpAdapterMeta

pub mod create_from_template;
pub mod delete_note;
pub mod edit_note;
pub mod manage_tags;
pub mod move_note;
pub mod update_frontmatter;
pub mod write_note;
