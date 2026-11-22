#!/usr/bin/env python3
"""Compare two saved criterion baselines and fail on a regression.

Usage: bench_compare.py <criterion-dir> <baseline> <candidate> [--threshold PCT]

Walks <criterion-dir> (normally target/criterion) for every benchmark that
has both baselines saved (`cargo bench -- --save-baseline <name>`), prints a
Markdown table of their mean times, and exits 1 if any candidate is more than
--threshold percent (default 25) slower than the baseline. A benchmark with no
baseline is listed as new and never fails.

Both sides must be measured on the same machine: the times are compared as
they are, and two machines differ by more than any threshold worth setting.

Either side may be saved more than once, as <name>-1, <name>-2 and so on; the
fastest run of each side is the one compared. Other load on a machine only
ever makes a run slower, so the fastest of a few runs is the closest to what
the code costs, and alternating the two sides spreads a busy spell over both.
A second table then lists every run, so the spread between runs of the same
code can be read next to the change. Stdlib only.
"""

import argparse
import json
import os
import re
import sys


def mean_ns(path):
    with open(os.path.join(path, "estimates.json")) as f:
        return json.load(f)["mean"]["point_estimate"]


def runs_ns(bench_dir, name):
    """The mean time of every saved run of `name` (`name` itself or
    `name-<n>`) for the benchmark in `bench_dir`, in the order of their names."""
    runs = re.compile(rf"{re.escape(name)}(-\d+)?")
    return [
        mean_ns(os.path.join(bench_dir, entry))
        for entry in sorted(os.listdir(bench_dir))
        if runs.fullmatch(entry)
        and os.path.isfile(os.path.join(bench_dir, entry, "estimates.json"))
    ]


def human(ns):
    for unit, scale in (("s", 1e9), ("ms", 1e6), ("µs", 1e3)):
        if ns >= scale:
            return f"{ns / scale:.2f} {unit}"
    return f"{ns:.0f} ns"


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("criterion_dir")
    ap.add_argument("baseline")
    ap.add_argument("candidate")
    ap.add_argument("--threshold", type=float, default=25.0,
                    help="percent slowdown that counts as a regression (default 25)")
    args = ap.parse_args()

    rows, runs, regressions = [], [], []
    for root, _, _ in os.walk(args.criterion_dir):
        if "report" in root.split(os.sep):
            continue
        new_runs = runs_ns(root, args.candidate)
        if not new_runs:
            continue
        name = os.path.relpath(root, args.criterion_dir)
        base_runs = runs_ns(root, args.baseline)
        runs.append((name, base_runs, new_runs))
        new = min(new_runs)
        if not base_runs:
            rows.append((name, "new", human(new), ""))
            continue
        base = min(base_runs)
        change = (new - base) / base * 100
        mark = ""
        if change > args.threshold:
            mark = "regression"
            regressions.append(name)
        elif change < -args.threshold:
            mark = "faster"
        rows.append((name, human(base), human(new), f"{change:+.1f}% {mark}".strip()))

    if not rows:
        sys.exit(f"no benchmarks with a saved '{args.candidate}' baseline under {args.criterion_dir}")

    print(f"| Benchmark | {args.baseline} | {args.candidate} | Change |")
    print("|---|---:|---:|---|")
    for row in sorted(rows):
        print("| " + " | ".join(row) + " |")
    if regressions:
        print(f"\n{len(regressions)} benchmark(s) more than {args.threshold:.0f}% slower: "
              + ", ".join(sorted(regressions)))
    if any(len(base) > 1 or len(new) > 1 for _, base, new in runs):
        print(f"\n| Every run | {args.baseline} | {args.candidate} |")
        print("|---|---|---|")
        for name, base, new in sorted(runs):
            print(f"| {name} | {', '.join(map(human, base))} | {', '.join(map(human, new))} |")

    sys.exit(1 if regressions else 0)


if __name__ == "__main__":
    main()
