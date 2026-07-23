# The Write-Safety Suite (WSS)

A backend-parameterized, matrix-driven test suite that pins the **desired
write-safety behavior** of every mutating vault operation across the full space
of *working-tree state × precondition*, on every write backend.

It is **aspirational-spec-first**: a cell asserts the behavior we *want*, so a
cell that isn't implemented yet fails — and is marked `pending` (an ignored
trial) rather than deleted. The pending set is the burndown; making a cell pass
is the work. "No committed test asserts broken behavior."

- Runner: `crates/turbovault/tests/wss_matrix.rs` (`libtest-mimic`, `harness = false`).
- Harness + adapters: `crates/turbovault/tests/write_safety_suite/`.
- Report: `just wss-report` (colored grid) · `just wss-report-html` · `just wss-test`.
- Source-of-truth matrices: [`wss-precondition-matrix.csv`](./wss-precondition-matrix.csv),
  [`wss-batch-matrix.csv`](./wss-batch-matrix.csv).

## The one question WSS answers

> Given a precondition and a working-tree state, does a **single write**
> refuse-or-proceed **without silently clobbering** an out-of-band change?

Everything else is out of scope — see [Scope boundary](#scope-boundary) below.
The universal rule the whole matrix encodes:

> The precondition is evaluated against the **working tree**. A dirty tree
> (staged, unstaged, or an untracked conflict) is **refused** unless the caller
> proved it read the current bytes (`ExpectBlob(WORKDIR)`) or explicitly opted
> out (`Blind`).

## Model — four axes, cleanly separated

| Axis | Type | What it is |
|---|---|---|
| **Backend** | `Backend { Git, Direct }` → a `Vault` | The on-disk vault: state construction + version tokens. Layer-agnostic. Git has the full 9-state grid; Direct has only `{absent, present}`. |
| **Layer** | a **World** per layer (`Layer` trait) | *How* you reach the write surface. One type per layer — `ToolsWorld`, `ManagerWorld`, `BatchWorld`, (`WireWorld`, planned). |
| **Op** | `SinglePathOp<W>` **invoker** | One impl per `(op, layer)`. Binds an op's shared `Case` table to a layer-specific invocation. An op that doesn't map to a layer simply has no invoker there. |
| **Cell** | `Case { precondition, state, expected, pending, only }` | One matrix cell. `pending` = ignored (burndown); `only` scopes a cell to one backend where git/direct diverge. |

A `Case` carries no failure-reason string: the cell's `(precondition, state,
expected)` *is* its identity, and the reporter derives the "why" dynamically —
so flipping a cell from pending to active is a mechanical `pending → new`.

### Layers (Worlds)

- **`ToolsWorld`** — invokers construct the domain tool (`FileTools` etc.) over
  the manager. The agent-facing-ish surface.
- **`ManagerWorld`** — invokers call the `VaultManager` mutators directly (the
  enforcement/SDK layer). Only ops with a native manager mutator get an invoker
  (write/edit/delete/move; not the compute-then-write ops). It mirrors
  `ToolsWorld` cell-for-cell today — the tools are pure delegators — so it is a
  *confirmation* net that the chokepoint adds no behavior of its own.
- **`BatchWorld`** — invokers wrap each op in a **one-op batch** and drive it
  through the batch translation path (`BatchTools::plan` + `apply_changes`).
  Proves *batch-of-one == standalone*: routing an op through the batch layer must
  not change its per-op clobber-safety. (It already caught one real divergence:
  git batch delete-of-absent short-circuits to an idempotent `Ok` while the
  standalone delete still refuses.)
- **`WireWorld`** *(planned)* — a spawned MCP server + JSON-RPC client, to verify
  the agent-facing wire contract (precondition encoding, error mapping).

## States, preconditions, outcomes

**States** (column code `[e]xists[t]racked[c]ommitted[s]taged[u]nstaged`): the 9
git working-tree states, built by backend-independent git plumbing. Direct
represents only `{absent, present}`.

**Preconditions** (HTTP conditional-request analogues):

| Variant | Meaning |
|---|---|
| `ExpectBlob(token)` | the path must currently hold exactly this blob (the token the caller read) |
| `ExpectAbsent` | the path must not exist (create-only) |
| `ExpectExists` | the path must exist, any content (in-place default) |
| `Blind` | no precondition; last-writer-wins |

**Outcomes**: `Ok` · `ConcurrencyError` · `NoFile` (`FileNotFound`) · `OpError`.
All "changed underneath us" failures — CAS mismatch, dirty-tree refusal — unify
to a **single** `ConcurrencyError`; they differ only by setup, not by kind. A
refusal must leave the working tree byte-for-byte intact (the no-clobber
invariant these tests exist to protect).

## Running & reading it

```
just wss-test                       # active cells (pending are ignored) — the gate
cargo test --test wss_matrix -- --ignored          # only the burndown cells
cargo test --test wss_matrix -- git::write_note     # filter by trial-name substring
just wss-report                     # per-op precondition×state grid, coloured by pass/fail
just wss-report-html                # the same as one self-contained HTML file
```

Trial names are `<layer>::<backend>::<op>::<PRECOND>::<state>::<expected>`. The
reporter is the reconcile oracle: it lists **un-pend candidates** (pending cells
that now pass — activate them) and **newly-failing** (active cells that broke).
Fixpoint = pending⇔failing, active⇔passing.

## <a name="scope-boundary"></a>Scope boundary — what WSS is *not*

WSS tests **per-write clobber-safety**. It deliberately does **not** test
**multi-op transaction integrity** — "does an N-op batch abort-and-roll-back, or
apply best-effort-partial?" Those are two orthogonal properties wearing one word
("batch"):

- **write-safety** (WSS): for *one* write, precondition × state → refuse-or-proceed
  without a silent clobber.
- **transaction-integrity** (a separate suite): for an *op-list* — atomicity,
  rollback vs partial, intra-batch same-path collision, empty-batch validation.

`wss-batch-matrix.csv` itself draws this line: *"PER-OP behavior = EXACTLY the
standalone op"* (that part is WSS — `BatchWorld`'s single-op isolation, which
reuses each op's standalone `Case` table) and separately *"what batch ADDS on
top: atomicity"* (that part is the transaction suite). Only the first lives here.

A best-effort partial batch is **not** a silent clobber: it is *reported*
(`failed_at`), and every constituent op still honored its own precondition (which
WSS already covers per-op). The torn-write is a broken atomicity *contract* — a
transaction suite's job.

Corollary: `BatchWorld`'s isolation invokers use `plan` + `apply_changes`, not
the `batch_execute` wire tool, on purpose. `batch_execute` returns
`Ok(BatchResult { success: false, errors: [e.to_string()] })` on failure — it
**stringifies the typed error kind** the `Outcome` assertions need. That
envelope (and its atomicity semantics) is the transaction suite's concern, not
WSS's.

## Source-of-truth matrices

- **`wss-precondition-matrix.csv`** — the per-op precondition × state grid with
  the desired outcome code per cell. The authoritative WSS spec.
- **`wss-batch-matrix.csv`** — the batch spec. Its per-op section ("batch-of-one
  == standalone") is realized by `BatchWorld`; its atomicity section is the
  transaction suite's (out of WSS scope).

Cells self-describe in their trial name, so a failure is legible without a
lookup; the CSVs are the human-facing source of truth the adapters encode.
