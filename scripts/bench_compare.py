#!/usr/bin/env python3
"""Compare two saved criterion baselines and fail on a regression.

Usage: bench_compare.py <criterion-dir> <baseline> <candidate> [--threshold PCT] [--promote]

Walks <criterion-dir> (normally target/criterion) for every benchmark that
has both baselines saved (`cargo bench -- --save-baseline <name>`), prints a
Markdown table of their mean times, and exits 1 if any candidate is more than
--threshold percent (default 25) slower than the baseline. A benchmark with no
baseline yet is listed as new and never fails.

--promote then copies the candidate over the baseline, so the next run
compares against this one. Stdlib only.
"""

import argparse
import json
import os
import shutil
import sys


def mean_ns(path):
    with open(os.path.join(path, "estimates.json")) as f:
        return json.load(f)["mean"]["point_estimate"]


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
    ap.add_argument("--promote", action="store_true",
                    help="copy the candidate baseline over the baseline afterwards")
    args = ap.parse_args()

    rows, regressions, compared = [], [], []
    for root, dirs, _ in os.walk(args.criterion_dir):
        if args.candidate not in dirs or "report" in root.split(os.sep):
            continue
        name = os.path.relpath(root, args.criterion_dir)
        new = mean_ns(os.path.join(root, args.candidate))
        base_dir = os.path.join(root, args.baseline)
        if not os.path.isdir(base_dir):
            rows.append((name, "new", human(new), ""))
            compared.append(root)
            continue
        base = mean_ns(base_dir)
        change = (new - base) / base * 100
        mark = ""
        if change > args.threshold:
            mark = "regression"
            regressions.append(name)
        elif change < -args.threshold:
            mark = "faster"
        rows.append((name, human(base), human(new), f"{change:+.1f}% {mark}".strip()))
        compared.append(root)

    if not rows:
        sys.exit(f"no benchmarks with a saved '{args.candidate}' baseline under {args.criterion_dir}")

    print(f"| Benchmark | {args.baseline} | {args.candidate} | Change |")
    print("|---|---:|---:|---|")
    for row in sorted(rows):
        print("| " + " | ".join(row) + " |")
    if regressions:
        print(f"\n{len(regressions)} benchmark(s) more than {args.threshold:.0f}% slower: "
              + ", ".join(sorted(regressions)))

    if args.promote:
        for root in compared:
            target = os.path.join(root, args.baseline)
            shutil.rmtree(target, ignore_errors=True)
            shutil.copytree(os.path.join(root, args.candidate), target)

    sys.exit(1 if regressions else 0)


if __name__ == "__main__":
    main()
