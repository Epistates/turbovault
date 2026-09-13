# turbovault-git

[![Crates.io](https://img.shields.io/crates/v/turbovault-git.svg)](https://crates.io/crates/turbovault-git)
[![Docs.rs](https://docs.rs/turbovault-git/badge.svg)](https://docs.rs/turbovault-git)
[![License](https://img.shields.io/crates/l/turbovault-git.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

Git-native write substrate for TurboVault: every mutation is a commit.

A vault backed by this crate gets the guarantees git already has. A write either
lands whole or does not land, concurrent writers cannot silently clobber each
other, and the history of who changed what is the repository itself rather than
a log beside it.

## How a write happens

Blobs are written, assembled into an isolated index, turned into a tree, then a
commit, and the branch ref is moved by compare-and-swap. Only after the ref
moves is the working tree materialized to match.

The CAS on the ref is the part that matters. Two writers racing the same vault
both build their commits, and the loser's ref update fails rather than
overwriting. The caller re-reads and retries against what actually landed, so
the loser's change is never silently dropped.

## What it provides

- **`VaultRepo`** opens a vault as a repository and is the entry point for
  commits. `CommitHook` observes commits as they are made.
- **`CommitLocks`** serialises in-process writers to one repository, so the CAS
  handles cross-process races and the lock registry handles the cheap local
  case.
- **`FanoutWorktree`** runs a multi-note operation on an isolated branch and
  merges it back (`MergeStrategy`, `MergeBackResult`). `OrphanFanout` finds
  worktrees a crashed session left behind.
- **`ChangesetResult`**, **`TreeChange`** and **`PathChange`** describe what a
  commit did, which is what the caller needs to update its indexes.

## Requirements and scope

The vault path must already be a git repository. This crate never runs the `git`
binary; it uses `git2`/libgit2 directly, so there is no shell to inject into.
Remotes are out of scope: it commits locally, and pushing or pulling stays the
operator's business.
