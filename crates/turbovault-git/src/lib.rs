//! # turbovault-git — git-native write substrate
//!
//! Every vault mutation is a git commit built from plumbing — blobs written to
//! the object DB, a tree assembled in an **isolated/ephemeral index** (never the
//! shared `.git/index`), a `commit-tree`, and a **compare-and-swap ref advance**
//! — then materialized into the working tree. Git history is the rollback/audit
//! log; `update-ref` CAS is the cross-process serialization primitive.
//!
//! Design: `git-write-substrate-architecture.md`. This crate replaces the
//! direct `VaultManager` write path (mutators, batch executor, path-lock
//! registry, audit/snapshot rollback).
//!
//! Layers (built bottom-up):
//! - [`VaultRepo`] — repo handle + detection + branch/HEAD resolution (GWS.1).
//! - [`TreeChange`] + `build_tree`/`commit_tree` — object-DB plumbing (GWS.2).
//! - `cas_ref`/`commit_with_retry` — ref compare-and-swap + optimistic
//!   rebuild-on-conflict (GWS.3).
//! - (next) per-file precondition, materialization, changesets (GWS.4+).

#![forbid(unsafe_code)]

mod cas;
mod changeset;
mod error;
mod fanout;
mod locks;
mod materialize;
mod occ;
mod plumbing;
mod repo;
mod restore;

pub use changeset::{Changeset, ChangesetResult};
pub use error::{Error, Result};
pub use fanout::{FanoutInfo, FanoutWorktree, MergeBackResult, MergeStrategy, OrphanFanout};
pub use locks::CommitLocks;
pub use occ::Precondition;
pub use plumbing::TreeChange;
pub use repo::{CommitHook, VaultRepo};

/// Re-exported so substrate consumers (e.g. `turbovault-tools`) can talk about
/// blob/commit oids without taking a direct `git2` dep — preserves the
/// substrate's role as the only crate that knows about libgit2.
pub use git2::Oid;
