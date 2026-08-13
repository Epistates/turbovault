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
- Report: `just wss-report` (one Markdown table + legend, paste-able into a GitHub
  comment) · `just wss-report-html` · `just wss-test`.
- Source-of-truth matrices: [`wss-precondition-matrix.csv`](./wss-precondition-matrix.csv),
  [`wss-batch-matrix.csv`](./wss-batch-matrix.csv).

## The one question WSS answers

**The promise.** TurboVault's promise is to **ABORT** if the content on disk
doesn't match what you said the file looks like. WSS ensures this promise by
setting up all possible scenario combinations and making sure TurboVault aborts
when it *should* abort.

**The same thing, as a scope boundary.** WSS tests **clobber-safety** — did the op
refuse-or-proceed without silently losing an out-of-band change? — and **not
content-correctness** — is the written text formatted right? Those are orthogonal
axes; content-correctness is a plain functional test's job, never a WSS cell.

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
| **Backend** | `Backend { Git, Direct }` → a `Vault` | The on-disk vault: state construction + version tokens. Layer-agnostic. Git runs the full 9-state grid; the harness currently *builds* only `{absent, present}` on Direct — a harness limit, not a property of the backend. |
| **Layer** | a **World** per layer (`Layer` trait) | *How* you reach the write surface. One type per layer — `ToolsWorld`, `ManagerWorld`, `BatchWorld`, `WireWorld`. |
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
  not change its per-op clobber-safety. It runs the op's **same shared `Case`
  table**; where batch genuinely diverges from standalone (e.g. git batch
  delete-of-absent short-circuits to an idempotent `Ok` while the standalone
  delete still refuses), the shared cell simply diverges — a failure or an
  un-pend candidate — instead of being blessed in a per-world table.
- **`WireWorld`** — an **in-process** `ObsidianMcpServer` driven through its real
  `call_tool` dispatch: the `#[tool]` handler, JSON param (de)serialization, and the
  `ConcurrencyError → McpError` mapping, with no child process. It verifies the
  agent-facing wire contract (sentinel precondition encoding, error mapping). The
  typed error kind is erased at the wire boundary, so this arm classifies outcomes by
  **message substring** — which is precisely the contract it exists to pin: that the
  kind survives as a legible string. Its vault is registered directly via a
  `VaultConfig`, bypassing the Direct-only `add_vault` tool.

## States, preconditions, outcomes

**States** (column code `[e]xists[t]racked[c]ommitted[s]taged[u]nstaged`): the 9
git working-tree states, built by backend-independent git plumbing. The CSV
specifies all nine on both backends; the harness builds only `{absent, present}`
on Direct today. Direct is **git-blind** — its version token is
the sha256 of the file's bytes, so staged/committed are invisible and all eight
"present" states collapse to one behaviour.

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
just wss-report                     # ONE table: world×backend×op×precondition rows,
                                    #   state columns, emoji status + a legend
just wss-report-html                # the same table as one self-contained HTML file
just wss-audit                      # do the Case tables still match the CSV spec?
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

## <a name="known-gaps"></a>Known gaps (deliberate, tracked)

Two write surfaces are **not** covered, in both cases because the *required
behavior* is genuinely unsettled — encoding a guess in the matrix would publish a
contract nobody ratified, which is worse than a documented hole:

- **`write_note` `append`/`prepend` modes** (`turbovault-nbl.9`). Only
  `Overwrite` has cells. WSS asserts clobber-safety, and an append never destroys
  existing bytes — so an append carrying a *stale* `ExpectBlob` is arguably not a
  clobber at all. It could refuse like every other in-place op (consistent with the
  design's §4 "in-place → `ExpectExists` + dirty-gated") or proceed (nothing is
  lost); the design's §9 explicitly leaves "append/prepend CAS semantics" open. The
  layers also disagree on whether the mode exists — `FileTools::write_file_with_mode`
  and the `write_note` wire param take a `WriteMode`, but `VaultManager::write_file`
  and `BatchOperation` have none.
- **`BatchOperation::UpdateLinks`** (`turbovault-0g4.8`). The only batch variant
  with no arm, pending a decision on whether it survives at all (it is a naive,
  non-OFM-aware `str::replace`). If it is removed, the gap closes by deletion.

Neither is a *coverage* oversight: `just wss-audit` proves the tables match the CSV
spec exactly for everything the spec does cover.

## Authoring rules — how to change WSS

WSS is **aspirational**: a cell states the behavior we *want*, so the suite is
written against the **API we need, not the API we have**.

1. **Every world runs the op's ONE shared `Case` table.** Tools, Manager, Batch,
   Wire all bind the same cells for an op (per-role for dual-path ops: one src,
   one dest table). There are **no per-world tables.**
2. **A world diverging from the required outcome is a BUG — the test MUST fail
   for that world.** That failure is the finding. Do **not** fork a per-world
   table to record the divergent behavior; that enshrines the bug and defeats the
   suite. (This is why `move`'s batch arm and `delete`'s batch arm share the
   standalone tables even where batch behaves differently.)
3. **Write against the API you need; let it not compile.** If the required
   behavior needs a method/param/type that doesn't exist yet, use it anyway.
   **Compilation failure is intentional** — it is the signal the API must be
   built. (Example: batch `move_note` needs first-class src+dest preconditions on
   `BatchOperation::MoveNote`; the test names them and won't compile until the
   substrate gains them.)
4. **`pending` = required-but-unimplemented AND out-of-scope-to-fix-now.** A
   shared property of the *cell* — it keeps agents off out-of-scope work and
   keeps the gate legible. Never a per-world lever to hide divergence.
5. **`Case::on(Backend)` — two legitimate splits, and they are not
   interchangeable.**
   - **Same requirement, one backend lags.** Copy the cell, `.on(Git)` /
     `.on(Direct)`, carrying the **same `expected`** and differing only in the
     `pending` flag. The working backend is guarded against regression; the
     lagging one stays out of scope.
   - **Different requirement, because the backends genuinely disagree.** The
     copies carry **different `expected`**. Legal ONLY when the CSV says so — it
     is keyed by `backend`, and `just wss-audit` checks both arms against it.
     That is the guard: a divergence cannot be hand-written into a `Case`, it has
     to be written into a reviewed spec first, so this can never become a licence
     to turn a red cell green.

   The distinguishing question is **is the difference in the requirement or in
   the code?** Recording a code lag as a spec difference is the bug the first
   form exists to prevent. `e---u` is the second form: on git it is a dirty
   untracked tree, on direct it is merely a present file, and direct is
   git-blind, so there is nothing to lose and nothing to refuse.

### Commit & gate discipline

- **WSS test changes go in their own commit, separate from the behavior/API fix**
  that satisfies them. Combining the two is a bad smell: it obscures whether the
  code was written to match the test or the test to match the code. The failing
  test lands first (it may be red or non-compiling), the fix follows — and the
  red commit proves the behavior-under-test is real signal.
- **A green `just test` / `just wss-test` is NOT required for a WSS commit.** The
  reviewer's sign-off is the gate. Fixpoint/green is not the success criterion
  when *changing* the suite; it is the target the *fix* commit then drives toward.

## Source-of-truth matrices

- **`wss-precondition-matrix.csv`** — the per-op precondition × state grid with
  the desired outcome code per cell, keyed by **`backend`**. The authoritative WSS
  spec.

  It carries a `backend` column and deliberately **no `world` column**: backend is
  the one axis along which the spec may legitimately vary (git and direct disagree
  because direct is git-blind), whereas a world that diverges *fails its cell*, so
  a world key would encode a degree of freedom the spec does not have.

  It describes **the real world, not the harness**. The direct rows specify all
  nine states even though the harness currently builds only `{absent, present}`
  there — a limitation of `Vault::new(Direct)`, not a fact about the backend, since
  a direct vault can sit inside a git repo perfectly well. `just wss-audit` prints
  those cells as a counted **NOT BUILT** total rather than skipping them silently,
  so the gap cannot read as coverage. The CSV's own `note:` row records that
  decision and why the cells are specified regardless.
- **`wss-batch-matrix.csv`** — the batch spec. Its per-op section ("batch-of-one
  == standalone") is realized by `BatchWorld`; its atomicity section is the
  transaction suite's (out of WSS scope).

Cells self-describe in their trial name, so a failure is legible without a
lookup; the CSVs are the human-facing source of truth the adapters encode.
