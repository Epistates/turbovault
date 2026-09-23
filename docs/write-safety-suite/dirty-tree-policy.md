# Dirty-tree write policy

**Status:** ratified default (2026-08-14); implementation not started.
**Scope:** the git write backend only. Direct has no index and no HEAD, so "dirty"
is not a state it can be in — its only guard is the sha256 CAS, already expressed
by `ExpectBlob`.

## Summary

When an agent writes to a vault path that has uncommitted changes, TurboVault must
decide what happens to those changes. Today the answer is implicit in the code and
under-specified in the matrix: the write-safety suite asserts that such a write
returns `Ok`, but asserts *nothing* about what became of the human's work.

The ratified answer is **strict**: a write to a dirty path is refused. `Blind` is
the explicit force escape hatch, and is **disabled by default**. Both the policy
and the `Blind` switch are per-vault configuration, and the matrix verifies both
settings.

This is also the current behaviour, so the default is not a breaking change.

## Context / current state

Three code paths matter, and they do not currently agree with each other.

**The dirty gate** — `crates/turbovault-git/src/materialize.rs`,
`VaultRepo::ensure_worktree_matches_commit`, called from
`crates/turbovault-git/src/changeset.rs:138`. Two refusals:

1. repo-wide: any path with a staged change aborts the write, even a path the plan
   never mentions;
2. per touched path: working-tree bytes differing from HEAD aborts the write.

It consults no precondition at all.

**Precondition evaluation** — `crates/turbovault-git/src/occ.rs:50`,
`check_preconditions`, runs *after* the gate and resolves the observed blob from
the **base tree** (HEAD), not from the working tree. `Precondition::Blind` is a
no-op arm at `occ.rs:87`.

**Materialization** — `materialize` ends with `index.read_tree(&tree)` +
`index.write()`. The git2 0.21 doc is explicit: *"The current index contents will
be replaced by the specified tree."* Every write therefore rewrites the whole
index. This is why refusal (1) has to be repo-wide: without it, a write would
silently discard a staged change to an unrelated path.

### Why the matrix cannot currently settle the question

`Outcome::Ok` asserts that the op succeeded and that the new bytes landed. It does
not assert what happened to content that was already there. So the CSV saying
`Blind × etc-u → Ok` is true under "the displaced edit was destroyed", "it was
committed first", and "it was stashed" alike. The contract was never pinned.

## Goals

- Ratify what happens to uncommitted work when an agent writes over it.
- Make the matrix express the policy, so the answer is enforced rather than
  implied.
- Keep the escape hatch available, explicit, and off by default.

## Non-goals

- Multi-op transaction integrity (`turbovault-nbl.17`).
- `write_note` append/prepend semantics (`turbovault-nbl.9`).
- `BatchOperation::UpdateLinks` (`turbovault-0g4.8`).
- Any policy for the direct backend (see Scope).

## Invariants

- **The promise:** TurboVault aborts if the content on disk does not match what
  the caller said the file looks like.
- Every world runs an op's ONE shared `Case` table. A world that diverges FAILS
  its cell; there is no per-world pending flag.
- `docs/write-safety-suite/wss-precondition-matrix.csv` is the spec. A cell's
  `expected` value moves only when the CSV moves first, under review.
- A refusal leaves the working tree byte-for-byte intact.
- **New:** a write must not disturb any path it does not name — in content **or**
  in index state (`turbovault-nbl.22`).

## Design constraints

- The write substrate is merged upstream, so wire compatibility and default
  behaviour are maintainer-facing.
- Reviewer attention is the scarce resource; prefer the option whose failure mode
  is recoverable over the one that needs to be got right first time.
- Reference-implementation bias: choose the simplest, most reversible option and
  note it, rather than the most elegant.

## Alternatives considered

### Option 1 — Strict: refuse writes to a dirty path *(recommended, ratified)*

The write aborts with `ConcurrencyError`. Nothing is destroyed, committed, or
stashed. The human resolves the dirt and retries.

- **For:** the only option that preserves *intent* rather than merely bytes. It
  hands the human the reconciliation while they still have context. It is what
  `git checkout` does, and it is already the implemented behaviour. Being wrong is
  recoverable — refusal can be loosened later against evidence.
- **Against:** an agent that gets a bare refusal with no remedy will retry with
  `Blind`, so the error message and `turbovault-pzh` (commit/stage tools) are part
  of the contract, not polish. Users of auto-commit plugins live in short frequent
  dirty windows and will see sporadic refusals.

### Option 2 — Permissive with pre-commit

Commit the displaced content first (as its own commit, or folded into the agent's
commit), then apply the write.

- **For:** nothing is lost; the displaced work is in history where `git log` finds
  it.
- **Against:** it fabricates a commit the human did not author on their branch, and
  if the content was *staged* it consumes the commit they were about to make.
  Folding it into the agent's commit is worse: the commit message describes only
  the agent's edit, so authorship becomes a lie.

### Option 3 — Permissive with displaced-bytes capture

Before overwriting, write the working-tree bytes to a git object rooted by a ref
outside the branch (`refs/turbovault/displaced/...`), then write normally.

- **For:** branch history is untouched; no commit is fabricated.
- **Against:** **rejected on restore.** Recovering the content is a three-way merge
  (HEAD / human's version / agent's version) with real conflicts, on prose, where
  character-level diffing is semantically useless. It defers reconciliation to a
  worse moment — after the fact, when context is gone. `git stash` has the identical
  defect, which the maintainer identified independently.
- Note: staged content is *already* a durable blob (`git add` writes the object);
  only the index entry referencing it is destroyed. Unstaged content is the only
  content a write truly annihilates.

### What none of them do

Options 2 and 3 preserve **bytes**. Neither preserves the signal that `git add`
carries, which is simultaneously "keep this" and "**not yet**". Those two readings
point opposite ways, which is the strongest argument that an automated system must
not adjudicate it. Only refusal declines to decide.

## Recommendation

**Strict, with `Blind` available but disabled by default.**

`Blind` remains meaningful under strict — strict governs what happens when the
caller has *not* opted out. Stated precisely:

> Proof does not license writing over a dirty tree. Only explicit force does.

## Domain model and types

```rust
// turbovault-core: new
/// What a git-backed vault does when a write targets a path with uncommitted
/// changes. Direct vaults have no index or HEAD and are unaffected.
pub enum DirtyPolicy {
    /// Refuse the write. The default.
    Strict,
    /// Permit the write; displaced working-tree content is overwritten.
    Permissive,
}

// turbovault-core::config::VaultGitConfig: new fields, beside
// branch / author / merge_strategy / include_ignored / require_commit_message
pub struct VaultGitConfig {
    // ...existing...
    /// Default: DirtyPolicy::Strict
    pub dirty_policy: DirtyPolicy,
    /// Default: false. When false, a plan carrying Precondition::Blind is
    /// REFUSED rather than honoured — the force hatch must be opted into.
    pub allow_blind: bool,
}
```

`Precondition` itself is unchanged. What changes is that `Blind` acquires
*enforcement significance*: it is the only value that waives the gate, and it can
be switched off at the vault level.

## Seams and boundaries

| Boundary | What crosses it | What must not leak |
|---|---|---|
| `VaultManager` → `WriteSubstrate::apply` | `&ChangePlan` (changes + per-path `Precondition` + message) | policy decisions; the substrate reads policy from its own config |
| `GitSubstrate` → `VaultRepo::commit_changeset` | `&ChangePlan` + the vault's `DirtyPolicy` | git2 types above the substrate |
| gate → caller | `Error::concurrency` naming the dirty path **and the remedy** | raw git status detail |
| `BatchTools::plan` → `ChangePlan` | a decoded `Precondition` per touched path, **including `Blind`** | `Option<String>` "no hash" ambiguity |

## Call stacks and data flow

### Current flow — write to a dirty path

```txt
tool call (expected_hash sentinel)
  -> Precondition::from_wire(param, per-op default)
  -> FileTools/MetadataTools -> VaultManager::write_file
  -> ChangePlan { changes, preconditions, message }
  -> GitSubstrate::apply -> VaultRepo::commit_changeset      (changeset.rs:110)
     -> ensure_worktree_matches_commit(tip, changed)         (:138)  gate, precondition-blind
        -> staged anywhere?           -> Err(ConcurrencyError)
        -> touched path != HEAD bytes -> Err(ConcurrencyError)
     -> check_preconditions(base_tree, preconditions)        (:148)  compares vs HEAD, not worktree
     -> build_tree -> commit_tree
  -> materialize(commit, changed) -> index.read_tree(WHOLE TREE)
```

### Proposed flow — strict

```txt
     -> ensure_worktree_matches_commit(tip, changed, preconditions, policy)
        -> for each staged path NOT in the plan      -> Err(concurrency, names path + remedy)
        -> for each touched path:
             policy == Permissive && waived(path)    -> skip
             Blind && allow_blind                    -> skip           (the force hatch)
             Blind && !allow_blind                   -> Err(blind disabled for this vault)
             bytes != HEAD                           -> Err(concurrency, names path + remedy)
     -> check_preconditions(...)   // unchanged under strict
     -> materialize -> per-path index update (add_path / remove_path), NOT read_tree
```

Under strict the precondition-vs-worktree change (lever A) is **not required**: a
dirty path never reaches the CAS check, so comparing against HEAD is
indistinguishable from comparing against the worktree. That is what collapses the
burndown.

### Failure flow

`ConcurrencyError` is the single "changed underneath us" kind. Under strict its
message MUST name the offending path and the remedy, because an agent given a bare
refusal will retry with `Blind` — a strict policy with an unhelpful error trains
agents into the escape hatch. This is why `turbovault-pzh` is a dependency of the
policy, not a nice-to-have.

### Observability flow

Nothing records which precondition a write carried, so a blind write is invisible
after the fact (`turbovault-1o0`). Under this policy that gap matters more, because
`Blind` is now the sanctioned way to override a safety refusal. The precondition
kind should reach the audit entry (direct) and a commit trailer (git).

## Files to add / change / delete

**Add**

- `docs/write-safety-suite/dirty-tree-policy.md` — this document.

**Change**

- `crates/turbovault-core/src/config.rs` — `DirtyPolicy`, `VaultGitConfig.dirty_policy`, `.allow_blind`, serde defaults.
- `crates/turbovault-git/src/materialize.rs` — `ensure_worktree_matches_commit` takes preconditions + policy; split the two refusals; per-path index update replacing `read_tree` (`turbovault-a5j`).
- `crates/turbovault-git/src/changeset.rs:138` — pass preconditions + policy.
- `crates/turbovault-tools/src/batch_tools.rs:697,712,725` — record the decoded precondition **including `Blind`**; delete the now-false comment at `:739` (`turbovault-nbl.8.6`).
- `docs/write-safety-suite/wss-precondition-matrix.csv` — add the `policy` key; state the bystander sampling decision.
- `crates/turbovault/tests/write_safety_suite/` — policy axis; bystander cells.
- `docs/write-safety-suite/README.md` — policy axis in the model section.

**Explicitly not deleted**

- `Precondition::Blind` and its `occ.rs:87` no-op arm. Blind stays; only its
  availability becomes configurable.

## RGR TDD test plan

Vertical slices, red first, one behaviour each.

1. **Config defaults.** Red: a `VaultGitConfig` built with no git section yields `DirtyPolicy::Strict` and `allow_blind == false`. Guards the "default is today's behaviour" claim.
2. **Strict refuses a proof-carrying write to a dirty path.** Red: `ExpectBlob(WORKDIR)` on `etc-u` → `ConcurrencyError`, working tree byte-identical.
3. **Error names path and remedy.** Red: the message contains the offending path and the remedy verb. Cheap, and it is the thing standing between a refusal and an agent reaching for `Blind`.
4. **Blind refused when `allow_blind` is false.** Red: `Blind` on `etc-u` → error distinguishable from a CAS mismatch.
5. **Blind honoured when `allow_blind` is true.** Red: same cell → `Ok`, new bytes on disk.
6. **Permissive permits proof-carrying writes.** Red: policy `Permissive`, `ExpectBlob(WORKDIR)` on `etc-u` → `Ok`.
7. **Batch parity.** Red: each of the above through a one-op batch produces the identical `Observed` (this is `nbl.8.6`; batch currently cannot express `Blind` at all).
8. **Unrelated staged path survives a write** (`a5j` + `nbl.22`). Red: bystander staged, write to an unrelated path, bystander's `(e,t,c,s,u)` signature via `observe_git_state` unchanged. Fault-inject by narrowing the staged check and confirm it goes red.
9. **Matrix reconciliation.** `just wss-audit` clean; `just wss-report` newly-failing 0 under both policy settings.

## Risks and open questions

- **OPEN — cell growth.** The policy axis multiplies only the git rows, and only the `Workdir`/`Index`/`Exists` preconditions on dirty states. Exact new cell count not yet computed.
- **OPEN — does the harness build both policies per cell, or is policy a third CSV key with its own rows?** The `backend` column is the precedent; policy is the same shape.
- **OPEN — per-op default for an omitted batch `expected_hash`** (`nbl.8.6`). In-place ops want `ExpectExists`; `WriteNote` is an upsert whose omitted default is effectively `Blind` today. A wrong choice is self-detecting (the matrix reports a cell that SUCCEEDED where `ConcurrencyError` was required) but the choice is maintainer-facing.
- **RISK — `pzh` is load-bearing.** Without commit/stage tools, an MCP-only agent facing a strict refusal has no sanctioned remedy and will reach for `Blind`.
- **RISK — auto-commit users.** Obsidian-git's commit-on-idle produces frequent short dirty windows; strict will surface as intermittent refusals those users cannot explain.
- **RESOLVED — `Blind` under strict.** Available in both policies; strict governs the non-opted-out case.
- **RESOLVED — default.** Strict + `allow_blind: false`, matching current behaviour, so no migration.
