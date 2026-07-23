#!/usr/bin/env python3
"""WSS write-safety matrix reporter (turbovault-qae.9).

Runs the `wss_matrix` libtest-mimic binary with the pending flag bypassed
(`--include-ignored`) — the source of truth for what actually passes/fails —
plus an `--ignored`-only pass to learn which cells are currently marked
`pending`. Renders, per (layer, backend, op), a precondition x state grid
coloured by result, and prints a burndown summary plus the two lists that drive
the pending/active reconciliation:

  * un-pend candidates : cells marked `pending` that now PASS  -> should activate
  * newly-failing      : active cells that FAIL                -> should pend

Stdlib only, zero external deps. Terminal ANSI by default; `--html <path>`
writes one self-contained HTML file.

State/precondition column orders mirror the Rust source of truth
(`harness/state.rs` GitState::ALL and `harness/precondition.rs`
PreconditionKind); keep them in sync if those enums change.
"""

import argparse
import html
import re
import subprocess
import sys
from collections import defaultdict

# Column/row orders — mirror the Rust enums (see module docstring).
STATE_ORDER = ["-----", "etc--", "etcs-", "etc-u", "etcsu",
               "et-s-", "et--u", "et-su", "e---u"]
PRECOND_ORDER = ["BLIND", "ABSENT", "EXISTS", "HEAD", "INDEX", "WORKDIR", "WRONG"]
STATE_SET, PRECOND_SET = set(STATE_ORDER), set(PRECOND_ORDER)

# Compact outcome labels shown inside a grid cell.
OUTCOME_ABBR = {"Ok": "OK", "ConcurrencyError": "CE", "NoFile": "NF", "OpError": "OE"}

TRIAL_RE = re.compile(r"^test (\S+)\s+\.\.\.\s+(ok|FAILED)\s*$")

# ANSI
GREEN, RED, GREY, BOLD, RESET = "\033[32m", "\033[31m", "\033[90m", "\033[1m", "\033[0m"


class Cell:
    __slots__ = ("expected", "passed", "pending")

    def __init__(self, expected, passed, pending):
        self.expected = expected
        self.passed = passed
        self.pending = pending


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
    """(op_key, precond, state, expected) for a grid trial, else None.

    op_key groups move_note's src/dest sub-sweeps separately; batch_execute and
    edit_note's one-off carry no precondition/state grid, so they return None.
    """
    p = name.split("::")
    if len(p) == 7 and p[2] == "move_note":
        op, precond, state, expected = f"{p[2]}/{p[3]}", p[4], p[5], p[6]
    elif len(p) == 6:
        op, precond, state, expected = p[2], p[3], p[4], p[5]
    else:
        return None
    if precond not in PRECOND_SET or state not in STATE_SET:
        return None
    return (p[0], p[1], op), precond, state, expected


def build(truth, pending_set):
    """grids[(layer,backend,op)][(state,precond)] = Cell, plus per-op totals and
    the two reconciliation lists."""
    grids = defaultdict(dict)
    totals = defaultdict(lambda: {"pass": 0, "fail": 0, "pending": 0})
    unpend, newly_failing = [], []
    for name, passed in sorted(truth.items()):
        pending = name in pending_set
        # per-op totals key: the plain op name (merge move sub-sweeps + non-grid).
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
            key, precond, state, expected = parsed
            grids[key][(state, precond)] = Cell(expected, passed, pending)
    return grids, totals, unpend, newly_failing


def render_terminal(grids, totals, unpend, newly_failing):
    lines = []
    for key in sorted(grids):
        layer, backend, op = key
        grid = grids[key]
        states = [s for s in STATE_ORDER if any((s, p) in grid for p in PRECOND_ORDER)]
        preconds = [p for p in PRECOND_ORDER if any((s, p) in grid for s in STATE_ORDER)]
        lines.append(f"\n{BOLD}{layer}::{backend}::{op}{RESET}")
        header = "  state \\ pc │" + "".join(f" {p:^7} │" for p in preconds)
        lines.append(header)
        lines.append("  " + "─" * (len(header) - 2))
        for s in states:
            row = [f"  {s:^9} │"]
            for p in preconds:
                cell = grid.get((s, p))
                if cell is None:
                    row.append(f" {GREY}{'·':^5}{RESET} │")
                else:
                    lab = OUTCOME_ABBR.get(cell.expected, cell.expected[:5])
                    lab += "*" if cell.pending else " "
                    color = GREEN if cell.passed else RED
                    row.append(f" {color}{lab:^5}{RESET} │")
            lines.append("".join(row))
    lines.append(f"\n{BOLD}Summary{RESET}  (* = cell marked pending; "
                 f"colour = actual pass/fail under --include-ignored)")
    lines.append(f"  {'op':<22} {'pass':>5} {'fail':>5} {'pending':>8}")
    tp = tf = tpend = 0
    for op in sorted(totals):
        c = totals[op]
        tp += c["pass"]; tf += c["fail"]; tpend += c["pending"]
        lines.append(f"  {op:<22} {c['pass']:>5} {c['fail']:>5} {c['pending']:>8}")
    lines.append(f"  {'─'*22} {'─'*5} {'─'*5} {'─'*8}")
    lines.append(f"  {'TOTAL':<22} {tp:>5} {tf:>5} {tpend:>8}")

    lines.append(f"\n{BOLD}Un-pend candidates{RESET} "
                 f"(pending cells that PASS — should be activated): {len(unpend)}")
    for n in unpend:
        lines.append(f"  {GREEN}{n}{RESET}")
    lines.append(f"\n{BOLD}Newly-failing{RESET} "
                 f"(active cells that FAIL — should be pended): {len(newly_failing)}")
    for n in newly_failing:
        lines.append(f"  {RED}{n}{RESET}")
    if not unpend and not newly_failing:
        lines.append(f"\n  {GREEN}{BOLD}FIXPOINT: pending == failing, active == passing.{RESET}")
    return "\n".join(lines)


def render_html(grids, totals, unpend, newly_failing):
    css = """
    body{font:14px/1.5 system-ui,sans-serif;margin:2rem;color:#1a1a1a;background:#fff}
    h2{margin:1.5rem 0 .4rem;font-size:1rem}
    table{border-collapse:collapse;margin-bottom:.5rem}
    th,td{border:1px solid #ccc;padding:2px 8px;text-align:center;font-family:ui-monospace,monospace}
    th{background:#f0f0f0}
    td.na{color:#bbb}
    td.pass{background:#d7f5dd;color:#0a5}
    td.fail{background:#fbdcdc;color:#c00}
    td.pending::after{content:" *";color:#888}
    .sum td,.sum th{text-align:right;font-family:system-ui,sans-serif}
    ul{margin:.2rem 0 1rem;padding-left:1.2rem}
    li.pass{color:#0a5}li.fail{color:#c00}
    .ok{color:#0a5;font-weight:bold}
    """
    out = ["<title>WSS write-safety matrix</title>",
           f"<style>{css}</style>", "<h1>WSS write-safety matrix</h1>",
           "<p>Colour = actual pass/fail under <code>--include-ignored</code>; "
           "<code>*</code> = cell marked <code>pending</code>.</p>"]
    for key in sorted(grids):
        layer, backend, op = key
        grid = grids[key]
        states = [s for s in STATE_ORDER if any((s, p) in grid for p in PRECOND_ORDER)]
        preconds = [p for p in PRECOND_ORDER if any((s, p) in grid for s in STATE_ORDER)]
        out.append(f"<h2>{html.escape(layer)}::{html.escape(backend)}::{html.escape(op)}</h2>")
        out.append("<table><tr><th>state \\ pc</th>"
                   + "".join(f"<th>{html.escape(p)}</th>" for p in preconds) + "</tr>")
        for s in states:
            row = [f"<tr><th>{html.escape(s)}</th>"]
            for p in preconds:
                cell = grid.get((s, p))
                if cell is None:
                    row.append('<td class="na">·</td>')
                else:
                    cls = "pass" if cell.passed else "fail"
                    if cell.pending:
                        cls += " pending"
                    row.append(f'<td class="{cls}">{html.escape(OUTCOME_ABBR.get(cell.expected, cell.expected))}</td>')
            out.append("".join(row) + "</tr>")
        out.append("</table>")

    out.append("<h2>Summary</h2><table class='sum'><tr><th>op</th><th>pass</th>"
               "<th>fail</th><th>pending</th></tr>")
    tp = tf = tpend = 0
    for op in sorted(totals):
        c = totals[op]
        tp += c["pass"]; tf += c["fail"]; tpend += c["pending"]
        out.append(f"<tr><th>{html.escape(op)}</th><td>{c['pass']}</td>"
                   f"<td>{c['fail']}</td><td>{c['pending']}</td></tr>")
    out.append(f"<tr><th>TOTAL</th><td>{tp}</td><td>{tf}</td><td>{tpend}</td></tr></table>")

    out.append(f"<h2>Un-pend candidates ({len(unpend)})</h2><ul>")
    out += [f'<li class="pass">{html.escape(n)}</li>' for n in unpend]
    out.append("</ul>")
    out.append(f"<h2>Newly-failing ({len(newly_failing)})</h2><ul>")
    out += [f'<li class="fail">{html.escape(n)}</li>' for n in newly_failing]
    out.append("</ul>")
    if not unpend and not newly_failing:
        out.append("<p class='ok'>FIXPOINT: pending == failing, active == passing.</p>")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--html", metavar="PATH", help="write a self-contained HTML report")
    args = ap.parse_args()

    truth = parse_trials(run_matrix(["--include-ignored"]))
    if not truth:
        sys.exit("no trials parsed — did the wss_matrix binary build/run?")
    pending_set = set(parse_trials(run_matrix(["--ignored"])))

    grids, totals, unpend, newly_failing = build(truth, pending_set)

    if args.html:
        with open(args.html, "w") as f:
            f.write(render_html(grids, totals, unpend, newly_failing))
        print(f"wrote {args.html}")
    else:
        print(render_terminal(grids, totals, unpend, newly_failing))

    # Non-zero exit iff the matrix is off its fixpoint, so CI/humans get a signal.
    sys.exit(1 if (unpend or newly_failing) else 0)


if __name__ == "__main__":
    main()
