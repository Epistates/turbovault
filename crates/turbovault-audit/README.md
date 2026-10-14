# turbovault-audit

[![Crates.io](https://img.shields.io/crates/v/turbovault-audit.svg)](https://crates.io/crates/turbovault-audit)
[![Docs.rs](https://docs.rs/turbovault-audit/badge.svg)](https://docs.rs/turbovault-audit)
[![License](https://img.shields.io/crates/l/turbovault-audit.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

Operation audit trail, snapshot storage, and rollback for TurboVault.

Records what changed a vault, keeps enough of the prior content to undo it, and
performs the undo. State lives under the vault's `.turbovault/` directory, which
the note APIs refuse to read or write, so an audit trail cannot be edited
through the same surface that produced it.

## What it provides

- **`AuditLog`** appends an `AuditEntry` per changed path: the operation type,
  the path, a timestamp, content hashes, an optional `context` (the tool,
  request, client, user and session that made the change) and an open `metadata`
  field. Each entry stores the hash of the one before it, appends are fsynced
  under a file lock, and `verify` checks the chain. Query it with
  `AuditFilter`, or summarise with `AuditStats`.
- **`SnapshotStore`** keeps the pre-change bytes for a path, content-addressed,
  so a rollback has something to restore. Retention by count or age is
  optional (`SnapshotRetention` in turbovault-core); the default keeps
  everything.
- **`RollbackEngine`** turns an entry back into a write. `plan` refuses when
  the note changed since the operation, unless forced, so a rollback never
  silently discards a later edit; the result records the content it replaced,
  so a rollback can itself be rolled back. `RollbackPreview` answers what a
  rollback would do without doing it.

## Provenance is advisory

`AuditEntry::metadata` carries whatever the writer put in a `ChangePlan`. It is
useful for attributing a change and for loop prevention in an agent that watches
the vault it writes to. It is **not** an authentication boundary: nothing
verifies the claim, and a change made outside the process carries no metadata at
all. Do not make a trust decision on it.

## Where entries come from

Callers do not usually construct entries. Every mutation in TurboVault flows
through one write chokepoint, and the direct substrate records the audit entry
there, so a new write path inherits the trail without opting in. On the Git
backend the commit history is the durable record and this trail complements it.
