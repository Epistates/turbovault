#!/usr/bin/env python3
"""WSS write-safety matrix reporter (turbovault-qae.9 / nbl.20).

Runs the `wss_matrix` libtest-mimic binary with the pending flag bypassed
(`--include-ignored`) — the source of truth for what actually passes/fails —
plus an `--ignored`-only pass to learn which cells are currently marked
`pending`. Renders ONE all-in-one table shaped like
`docs/write-safety-suite/wss-precondition-matrix.csv` (same state column
headers, with `world` and `backend` added as leading columns), then the burndown
summary and the two lists that drive the pending/active reconciliation:

  * un-pend candidates : cells marked `pending` that now PASS  -> should activate
  * newly-failing      : active cells that FAIL                -> should pend

Output is **Markdown with emoji status**, so a terminal run can be pasted
straight into a GitHub comment or PR body with no ANSI colour to strip and no
legend to remember — every symbol is defined in the legend it prints.

Stdlib only, zero external deps. `--html <path>` writes the same table as one
self-contained HTML file.

Column/row orders and the N/A rule mirror the Rust source of truth
(`harness/state.rs` GitState::ALL + its `(e,t,c,s,u)` codes,
`harness/precondition.rs` PreconditionKind, `Backend::supports_state`); keep
them in sync if those change.
"""

import argparse
import csv
import html
import os
import re
import subprocess
import sys
import unicodedata
from collections import defaultdict

# The authoritative WSS spec the Rust tables must agree with (`--audit`).
CSV_PATH = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                        "docs", "write-safety-suite", "wss-precondition-matrix.csv")

# ── Canonical orders — mirror the Rust enums + the CSV (see module docstring) ──
STATE_ORDER = ["-----", "etc--", "etcs-", "etc-u", "etcsu",
               "et-s-", "et--u", "et-su", "e---u"]
PRECOND_ORDER = ["BLIND", "ABSENT", "EXISTS", "HEAD", "INDEX", "WORKDIR", "WRONG"]
# Layering order (not alphabetical): the surface each world drives, outermost last.
WORLD_ORDER = ["tools", "manager", "batch", "wire"]
# Git first — it carries the full 9-state grid; Direct only {absent, present}.
BACKEND_ORDER = ["git", "direct"]
# CSV operation order.
OP_ORDER = ["write_note", "edit_note", "delete_note", "update_frontmatter",
            "manage_tags", "create_from_template", "move_note::src", "move_note::dest"]
STATE_SET, PRECOND_SET = set(STATE_ORDER), set(PRECOND_ORDER)

# The states `Backend::Direct` can represent (absent + present); the other seven
# are git-only, so a direct row has no cell there.
DIRECT_STATES = {"-----", "e---u"}

# Compact outcome labels shown inside a cell.
OUTCOME_ABBR = {"Ok": "Ok", "ConcurrencyError": "CE", "NoFile": "NF", "OpError": "OE"}

# Status glyphs. Every cell is <status><outcome>, so one symbol carries both the
# required behaviour and whether the code currently delivers it.
PASS, FAIL, PEND_FAIL, PEND_PASS = "✅", "❌", "⏳", "🔶"
NA, SKIP = "·", "⏩"

# Every status glyph MUST be East-Asian-Wide, so ONE padding rule covers them all.
# The trap this guards: SKIP was U+23ED (⏭), which has emoji *presentation* but
# East_Asian_Width=Neutral — terminals and fonts then disagree on its advance and
# the columns skew (observed in kitty). U+23E9 (⏩) is the same double-triangle
# family and is properly Wide. Keep this assert: it makes that regression loud.
STATUS_GLYPHS = (PASS, FAIL, PEND_FAIL, PEND_PASS, SKIP)
assert all(unicodedata.east_asian_width(g) in ("W", "F") for g in STATUS_GLYPHS), (
    "status glyphs must be East-Asian-Wide or terminal columns will not align"
)

TRIAL_RE = re.compile(r"^test (\S+)\s+\.\.\.\s+(ok|FAILED)\s*$")

BOLD, RESET = "\033[1m", "\033[0m"


class Cell:
    __slots__ = ("expected", "passed", "pending")

    def __init__(self, expected, passed, pending):
        self.expected = expected
        self.passed = passed
        self.pending = pending


def list_trials():
    """Every trial NAME, via libtest-mimic's `--list` — no cells are executed, so
    this is near-instant (the audit only needs to know which cells exist)."""
    out = run_matrix(["--list"])
    return [line.rsplit(": test", 1)[0]
            for line in out.splitlines() if line.endswith(": test")]


def load_csv_spec(path):
    """{(op, PRECOND, state): expected} from the source-of-truth CSV, skipping the
    cells it declares untestable: `N/A` (token undefined for the state) and
    `SKIP:X` (duplicate of another precondition). Op names normalise to the trial
    convention (`move_note | src` -> `move_note::src`), preconditions upper-case."""
    spec = {}
    with open(path) as f:
        for row in csv.reader(f):
            if len(row) < 2 + len(STATE_ORDER) or row[0].strip() in ("", "operation"):
                continue
            op = row[0].replace(" | ", "::").strip()
            precond = row[1].strip().upper()
            if precond not in PRECOND_SET:
                continue  # legend / notes rows
            for state, value in zip(STATE_ORDER, row[2:2 + len(STATE_ORDER)]):
                v = value.strip()
                if v in ("", "N/A") or v.startswith("SKIP"):
                    continue
                spec[(op, precond, state)] = v
    return spec


def audit_against_csv(spec, names):
    """Compare the CSV spec with the grid cells the suite actually emits, using the
    reference arm (`tools::git`: the only one with the full 9-state grid and every
    op). Returns (missing, extra, mismatched).

    Why this exists: the CSV is documented as the AUTHORITATIVE WSS spec, so a table
    that quietly drifts from it — a cell dropped, or asserting a different outcome —
    would make the docs lie. `split_name` filters non-grid trials, so op-specific
    one-offs (edit_note's SEARCH-not-found) are invisible here by construction and
    never count as `extra`.
    """
    actual = {}
    for name in names:
        parsed = split_name(name)
        if not parsed:
            continue
        world, backend, op, precond, state, expected = parsed
        if (world, backend) == ("tools", "git"):
            actual[(op, precond, state)] = expected
    missing = sorted(k for k in spec if k not in actual)
    extra = sorted(k for k in actual if k not in spec)
    mismatched = sorted((k, spec[k], actual[k]) for k in spec
                        if k in actual and actual[k] != spec[k])
    return missing, extra, mismatched


def run_matrix(extra_args):
    """Run the wss_matrix binary; return combined stdout+stderr (ignore exit code —
    --include-ignored intentionally exits non-zero because pending cells fail)."""
    cmd = ["cargo", "test", "--test", "wss_matrix", "--",
           "--test-threads=1"] + extra_args
    proc = subprocess.run(cmd, capture_output=True, text=True)
    return proc.stdout + proc.stderr


def parse_trials(output):
    """{trial_name: passed_bool} for every `test ... ok/FAILED` line."""
    out = {}
    for line in output.splitlines():
        m = TRIAL_RE.match(line)
        if m:
            out[m.group(1)] = (m.group(2) == "ok")
    return out


def split_name(name):
    """(world, backend, op, precond, state, expected) for a grid trial, else None.

    `move_note` carries a src/dest role segment, so its op reads `move_note::src`
    — matching the trial name, so a table cell can be grepped straight back to the
    failing trial. Non-grid trials (e.g. edit_note's SEARCH-not-found one-off)
    return None.
    """
    p = name.split("::")
    if len(p) == 7 and p[2] == "move_note":
        op, precond, state, expected = f"{p[2]}::{p[3]}", p[4], p[5], p[6]
    elif len(p) == 6:
        op, precond, state, expected = p[2], p[3], p[4], p[5]
    else:
        return None
    if precond not in PRECOND_SET or state not in STATE_SET:
        return None
    return p[0], p[1], op, precond, state, expected


def token_defined(precond, state):
    """Whether `precond`'s version token is DEFINED in `state` — the CSV's N/A rule,
    read off the `[e]xists[t]racked[c]ommitted[s]taged[u]nstaged` code exactly as
    the Rust harness does: HEAD iff committed, INDEX iff staged, WORKDIR iff exists;
    the non-token kinds are always defined."""
    if precond == "HEAD":
        return state[2] != "-"
    if precond == "INDEX":
        return state[3] != "-"
    if precond == "WORKDIR":
        return state[0] != "-"
    return True


def absent_reason(backend, precond, state):
    """Why a (precond, state) pair has no trial: `NA` when it is not constructible
    (this backend cannot build the state, or the token is undefined for it), else
    `SKIP` — the cell is a deliberate duplicate of another precondition whose token
    resolves equal here (the CSV's `SKIP:X`), so no test is emitted."""
    if backend == "direct" and state not in DIRECT_STATES:
        return NA
    if not token_defined(precond, state):
        return NA
    return SKIP


def cell_label(cell, backend, precond, state):
    """`<status><outcome>` for one table cell."""
    if cell is None:
        return absent_reason(backend, precond, state)
    code = OUTCOME_ABBR.get(cell.expected, cell.expected[:2])
    if cell.pending:
        return (PEND_PASS if cell.passed else PEND_FAIL) + code
    return (PASS if cell.passed else FAIL) + code


def build(truth, pending_set):
    """rows[(world, backend, op, precond)][state] = Cell, plus per-op totals and the
    two reconciliation lists."""
    rows = defaultdict(dict)
    totals = defaultdict(lambda: {"pass": 0, "fail": 0, "pending": 0})
    unpend, newly_failing = [], []
    for name, passed in sorted(truth.items()):
        pending = name in pending_set
        # per-op totals key: the plain op name (merges move's sub-sweeps + non-grid).
        parts = name.split("::")
        op_total = parts[2] if len(parts) > 2 else name
        t = totals[op_total]
        t["pass" if passed else "fail"] += 1
        if pending:
            t["pending"] += 1
        if pending and passed:
            unpend.append(name)
        if not pending and not passed:
            newly_failing.append(name)
        parsed = split_name(name)
        if parsed:
            world, backend, op, precond, state, expected = parsed
            rows[(world, backend, op, precond)][state] = Cell(expected, passed, pending)
    return rows, totals, unpend, newly_failing


def _rank(value, order):
    """Sort key: canonical position, then alphabetical for anything unlisted."""
    return (order.index(value), "") if value in order else (len(order), value)


def sorted_rows(rows):
    """Row keys in world -> backend -> operation -> precondition order."""
    return sorted(rows, key=lambda k: (_rank(k[0], WORLD_ORDER),
                                       _rank(k[1], BACKEND_ORDER),
                                       _rank(k[2], OP_ORDER),
                                       _rank(k[3], PRECOND_ORDER)))


def disp_width(text):
    """Display columns `text` occupies: East-Asian Wide/Fullwidth glyphs take two,
    everything else one. Ambiguous-width (`A`) counts as one, which is what a
    non-CJK terminal does — the only `A` glyph here is `·` (U+00B7)."""
    return sum(2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1
               for ch in text)


def _pad(text, width):
    """Pad to `width` DISPLAY columns, not characters."""
    return text + " " * max(0, width - disp_width(text))


LEGEND = f"""
Legend — every cell is <status><outcome>: the outcome WSS requires, and whether the
code currently delivers it.

  status   {PASS} active, passing — required behaviour holds
           {FAIL} active, FAILING — a regression; this list must be empty
           {PEND_FAIL} pending, failing — known burndown, expected (turbovault-nbl.8)
           {PEND_PASS} pending, PASSING — un-pend candidate: activate this cell
           {NA} no test: not constructible (backend cannot build the state, or the
             precondition's token is undefined for it — the CSV's N/A)
           {SKIP} no test: duplicate of another precondition whose token resolves
             equal in this state (the CSV's SKIP:X)

  outcome  Ok  succeeded and materialised
           CE  ConcurrencyError — refused with NO disk change (CAS mismatch or
               dirty-tree refusal; the matrix's DIRTY_ERR and CAS_FAIL unify here)
           NF  NoFile — in-place op on an absent target (FileNotFound)
           OE  OpError — op-specific refusal, no disk change (e.g. edit SEARCH
               matched nothing)

  precondition (checked against the WORKING TREE)
           BLIND    no precondition; last-writer-wins
           ABSENT   ExpectAbsent — create-only
           EXISTS   ExpectExists — must exist, any content (in-place default)
           HEAD     ExpectBlob(HEAD oid)    — defined iff committed
           INDEX    ExpectBlob(INDEX oid)   — defined iff staged
           WORKDIR  ExpectBlob(WORKDIR oid) — defined iff it exists
           WRONG    ExpectBlob(bogus oid)   — never matches

  state    [e]xists[t]racked[c]ommitted[s]taged[u]nstaged (columns, in CSV order)

Cells are addressable as trials: <world>::<backend>::<operation>::<PRECONDITION>::
<state>::<expected> — copy one into `cargo test --test wss_matrix -- <name>`.
"""


def render_markdown(rows, totals, unpend, newly_failing):
    """One Markdown table + legend + summary + the reconciliation lists."""
    head = ["world", "backend", "operation", "precondition"] + STATE_ORDER
    # Widths: pad the four ASCII lead columns for terminal readability; GitHub
    # ignores padding, so the same text renders as a table there.
    lead_w = [max(len(h), *(len(k[i]) for k in rows)) for i, h in enumerate(head[:4])] \
        if rows else [len(h) for h in head[:4]]
    cell_w = max(len(s) for s in STATE_ORDER)

    out = ["# WSS write-safety matrix", LEGEND]
    out.append("| " + " | ".join(
        [_pad(h, lead_w[i]) for i, h in enumerate(head[:4])]
        + [_pad(s, cell_w) for s in STATE_ORDER]) + " |")
    out.append("|" + "|".join(["-" * (w + 2) for w in lead_w]
                              + ["-" * (cell_w + 2)] * len(STATE_ORDER)) + "|")
    for key in sorted_rows(rows):
        world, backend, op, precond = key
        cells = [cell_label(rows[key].get(s), backend, precond, s) for s in STATE_ORDER]
        out.append("| " + " | ".join(
            [_pad(world, lead_w[0]), _pad(backend, lead_w[1]),
             _pad(op, lead_w[2]), _pad(precond, lead_w[3])]
            + [_pad(c, cell_w) for c in cells]) + " |")

    out.append("\n## Summary\n")
    out.append("| operation | pass | fail | pending |")
    out.append("|---|---:|---:|---:|")
    tp = tf = tpend = 0
    for op in sorted(totals, key=lambda o: _rank(o, OP_ORDER)):
        c = totals[op]
        tp += c["pass"]; tf += c["fail"]; tpend += c["pending"]
        out.append(f"| {op} | {c['pass']} | {c['fail']} | {c['pending']} |")
    out.append(f"| **TOTAL** | **{tp}** | **{tf}** | **{tpend}** |")

    out.append(f"\n## Un-pend candidates ({len(unpend)})\n")
    out.append("_Pending cells that PASS — activate them (`pending` -> `new`)._\n"
               if unpend else "_None._\n")
    out += [f"- {PEND_PASS} `{n}`" for n in unpend]
    out.append(f"\n## Newly-failing ({len(newly_failing)})\n")
    out.append("_Active cells that FAIL — a regression: fix the code or pend the cell._\n"
               if newly_failing else "_None._\n")
    out += [f"- {FAIL} `{n}`" for n in newly_failing]
    if not unpend and not newly_failing:
        out.append(f"\n{PASS} **FIXPOINT: pending == failing, active == passing.**")
    return "\n".join(out)


def render_html(rows, totals, unpend, newly_failing):
    """The same single table, as one self-contained HTML file."""
    css = """
    body{font:14px/1.5 system-ui,sans-serif;margin:2rem;color:#1a1a1a;background:#fff}
    h1{font-size:1.3rem}h2{margin:1.5rem 0 .4rem;font-size:1rem}
    table{border-collapse:collapse;margin-bottom:.5rem}
    th,td{border:1px solid #ccc;padding:2px 8px;text-align:center;font-family:ui-monospace,monospace}
    th{background:#f0f0f0;position:sticky;top:0}
    td.lead{text-align:left;background:#fafafa}
    pre{background:#f7f7f7;padding:.8rem;border-radius:4px;white-space:pre-wrap}
    ul{margin:.2rem 0 1rem;padding-left:1.2rem}
    .ok{color:#0a5;font-weight:bold}
    """
    out = ["<title>WSS write-safety matrix</title>", f"<style>{css}</style>",
           "<h1>WSS write-safety matrix</h1>",
           f"<pre>{html.escape(LEGEND.strip())}</pre>", "<table><tr>"]
    out += [f"<th>{html.escape(h)}</th>"
            for h in ["world", "backend", "operation", "precondition"] + STATE_ORDER]
    out.append("</tr>")
    for key in sorted_rows(rows):
        world, backend, op, precond = key
        out.append("<tr>" + "".join(
            f'<td class="lead">{html.escape(v)}</td>' for v in key))
        for s in STATE_ORDER:
            out.append(f"<td>{cell_label(rows[key].get(s), backend, precond, s)}</td>")
        out.append("</tr>")
    out.append("</table>")

    out.append("<h2>Summary</h2><table><tr><th>operation</th><th>pass</th>"
               "<th>fail</th><th>pending</th></tr>")
    tp = tf = tpend = 0
    for op in sorted(totals, key=lambda o: _rank(o, OP_ORDER)):
        c = totals[op]
        tp += c["pass"]; tf += c["fail"]; tpend += c["pending"]
        out.append(f"<tr><td class='lead'>{html.escape(op)}</td><td>{c['pass']}</td>"
                   f"<td>{c['fail']}</td><td>{c['pending']}</td></tr>")
    out.append(f"<tr><th>TOTAL</th><th>{tp}</th><th>{tf}</th><th>{tpend}</th></tr></table>")

    out.append(f"<h2>Un-pend candidates ({len(unpend)})</h2><ul>")
    out += [f"<li>{PEND_PASS} <code>{html.escape(n)}</code></li>" for n in unpend]
    out.append("</ul>")
    out.append(f"<h2>Newly-failing ({len(newly_failing)})</h2><ul>")
    out += [f"<li>{FAIL} <code>{html.escape(n)}</code></li>" for n in newly_failing]
    out.append("</ul>")
    if not unpend and not newly_failing:
        out.append(f"<p class='ok'>{PASS} FIXPOINT: pending == failing, "
                   "active == passing.</p>")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--html", metavar="PATH", help="write a self-contained HTML report")
    ap.add_argument("--audit", action="store_true",
                    help="check the Rust tables against the source-of-truth CSV "
                         "(fast: lists trials, runs none) and exit")
    args = ap.parse_args()

    if args.audit:
        spec = load_csv_spec(CSV_PATH)
        names = list_trials()
        if not names:
            sys.exit("no trials listed — did the wss_matrix binary build?")
        missing, extra, mismatched = audit_against_csv(spec, names)
        print(f"CSV spec cells (excluding N/A + SKIP): {len(spec)}")
        print(f"reference arm (tools::git) grid cells: "
              f"{len(spec) - len(missing) + len(extra)}")
        for label, items in (("MISSING (CSV requires a cell, the suite has none)", missing),
                             ("EXTRA (the suite tests a cell the CSV does not list)", extra)):
            print(f"\n{label}: {len(items)}")
            for k in items:
                print(f"  {FAIL} {'::'.join(k)}")
        print(f"\nOUTCOME MISMATCH (CSV vs the suite): {len(mismatched)}")
        for key, want, got in mismatched:
            print(f"  {FAIL} {'::'.join(key)}: CSV says {want}, suite asserts {got}")
        drift = len(missing) + len(extra) + len(mismatched)
        if not drift:
            print(f"\n{PASS} The suite matches the CSV spec exactly.")
        sys.exit(1 if drift else 0)

    truth = parse_trials(run_matrix(["--include-ignored"]))
    if not truth:
        sys.exit("no trials parsed — did the wss_matrix binary build/run?")

    # A run that dies partway still leaves `truth` non-empty, and the cells that
    # never reported would simply VANISH from the grid — the reporter would then
    # happily print a fixpoint and exit 0. For the reconcile oracle that is the
    # worst possible failure, so cross-check against the declared trial list
    # (`--list` executes nothing) and refuse to report on a partial run.
    expected = set(list_trials())
    missing = expected - set(truth)
    if missing:
        example = sorted(missing)[0]
        sys.exit(f"INCOMPLETE RUN: {len(missing)} of {len(expected)} trials produced no "
                 f"result (e.g. {example}). The matrix binary aborted mid-run — refusing "
                 f"to report, because missing cells would look like a clean grid.")

    pending_set = set(parse_trials(run_matrix(["--ignored"])))

    rows, totals, unpend, newly_failing = build(truth, pending_set)

    if args.html:
        with open(args.html, "w") as f:
            f.write(render_html(rows, totals, unpend, newly_failing))
        print(f"wrote {args.html}")
    else:
        print(render_markdown(rows, totals, unpend, newly_failing))

    # Non-zero exit iff the matrix is off its fixpoint, so CI/humans get a signal.
    sys.exit(1 if (unpend or newly_failing) else 0)


if __name__ == "__main__":
    main()
