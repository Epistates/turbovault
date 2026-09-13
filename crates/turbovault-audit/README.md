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
  the path, a timestamp, and an open `metadata` field the writer can use to say
  who it was. Query it with `AuditFilter`, or summarise with `AuditStats`.
- **`SnapshotStore`** keeps the pre-change bytes for a path, content-addressed,
  so a rollback has something to restore.
- **`RollbackEngine`** turns an entry back into a write. `RollbackPreview`
  answers what a rollback would do without doing it, which matters because a
  rollback is itself a vault mutation and gets audited like one.

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
