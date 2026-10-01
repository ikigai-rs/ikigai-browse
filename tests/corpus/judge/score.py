#!/usr/bin/env python3
"""Score a judge's verdicts over the judge eval set.

    python3 tests/corpus/judge/score.py [--scope serious|all] [--split dev|holdout|all] \
        RUN.jsonl [RUN2.jsonl]
    python3 tests/corpus/judge/score.py baseline > confirm-all.jsonl

A RUN is one judge's verdicts, one JSON object per line:

    {"entry": "<corpus entry>", "verdict": "confirmed" | "refuted" | "unsure", "calls": 1}

`entry` is the manifest's `entry` field; `calls` is how many model calls that verdict
cost (default 1). Any other fields (answers, seconds, the raw answer) ride along and are
ignored here. That contract is the whole interface: ANY judge that writes it can be
scored, which is the point of the set. `examples/judge-probe.rs` is one such judge; the
`baseline` subcommand is another, the trivial one that confirms everything, so the first
table this prints is what the queue does today.

Reported per run:

  known-false   refuted (the judge caught it), let through (confirmed), unsure, missing
  known-real    kept (confirmed), lost (refuted), unsure, missing — split by BASIS,
                because the bases are not equally strong (README: `verified` is a defect
                somebody reproduced or fixed, `published` is a human keeping the note)
  calls         total, and per verdict

and, given two runs, REPEATABILITY: the share of entries both runs judged on which they
gave the same verdict, overall and per label, with every flip listed. At temperature 0 a
local backend should agree with itself completely; anything less is worth reading.

`--scope serious` (the default) scores only entries whose PROPOSED severity is critical
or major, because that is what the judge is asked to look at. `--split` scores the dev or
the held-out part (see the manifest's `split`, a content-blind hash of the id).
"""

import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SERIOUS = {"critical", "major"}
VERDICTS = ("confirmed", "refuted", "unsure")


def basis_group(entry):
    if entry["label"] == "known-false":
        return "known-false"
    return "known-real/published" if entry["basis"] == "published" else "known-real/verified"


def load_manifest(scope, split):
    with open(os.path.join(HERE, "corpus.json")) as f:
        manifest = json.load(f)
    out = {}
    for e in manifest["entries"]:
        if scope == "serious" and e.get("severity") not in SERIOUS:
            continue
        if split != "all" and e.get("split") != split:
            continue
        if not e.get("file"):
            continue
        out[e["entry"]] = e
    return out


def load_run(path):
    run = {}
    with open(path) as f:
        for n, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            verdict = row.get("verdict")
            if verdict not in VERDICTS:
                sys.exit(f"{path}:{n}: verdict {verdict!r} is not one of {VERDICTS}")
            run[row["entry"]] = row
    return run


def table(name, entries, run):
    groups = {}
    for key, e in entries.items():
        groups.setdefault(basis_group(e), []).append(key)
    print(f"\n== {name}")
    print(f"   {'group':22} {'n':>4} {'confirmed':>10} {'refuted':>8} {'unsure':>7} {'missing':>8}")
    for group in ("known-false", "known-real/verified", "known-real/published"):
        keys = groups.get(group, [])
        if not keys:
            continue
        counts = {v: 0 for v in VERDICTS}
        missing = 0
        for k in keys:
            row = run.get(k)
            if row is None:
                missing += 1
            else:
                counts[row["verdict"]] += 1
        print(
            f"   {group:22} {len(keys):>4} {counts['confirmed']:>10} {counts['refuted']:>8} "
            f"{counts['unsure']:>7} {missing:>8}"
        )
    contested = [k for k in entries if entries[k].get("contested_by") and k in run]
    for k in contested:
        print(f"   contested {k} ({run[k]['verdict']}): same line as {', '.join(entries[k]['contested_by'])}")
    calls = sum(int(run[k].get("calls", 1)) for k in entries if k in run)
    print(f"   calls made: {calls} over {sum(1 for k in entries if k in run)} verdicts")
    false_keys = [k for k in entries if basis_group(entries[k]) == "known-false" and k in run]
    real_keys = [k for k in entries if entries[k]["label"] == "known-real" and k in run]
    if false_keys:
        caught = sum(1 for k in false_keys if run[k]["verdict"] == "refuted")
        through = sum(1 for k in false_keys if run[k]["verdict"] == "confirmed")
        print(f"   known-false refuted {caught}/{len(false_keys)}, let through {through}/{len(false_keys)}")
    if real_keys:
        kept = sum(1 for k in real_keys if run[k]["verdict"] == "confirmed")
        lost = sum(1 for k in real_keys if run[k]["verdict"] == "refuted")
        print(f"   known-real kept {kept}/{len(real_keys)}, lost {lost}/{len(real_keys)}")


def repeatability(entries, a, b):
    both = [k for k in entries if k in a and k in b]
    print(f"\n== repeatability over {len(both)} entries judged by both runs")
    if not both:
        return
    by = {}
    flips = []
    for k in both:
        g = basis_group(entries[k])
        same = a[k]["verdict"] == b[k]["verdict"]
        n, s = by.get(g, (0, 0))
        by[g] = (n + 1, s + int(same))
        if not same:
            flips.append((k, a[k]["verdict"], b[k]["verdict"]))
    agree = len(both) - len(flips)
    print(f"   agreement {agree}/{len(both)} = {agree / len(both):.2f}")
    for g, (n, s) in sorted(by.items()):
        print(f"   {g:22} {s}/{n}")
    for k, va, vb in flips:
        print(f"   flip  {k}: {va} -> {vb}")


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "baseline":
        manifest = load_manifest("all", "all")
        for key in manifest:
            print(json.dumps({"entry": key, "verdict": "confirmed", "calls": 0}))
        return
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--scope", choices=("serious", "all"), default="serious")
    ap.add_argument("--split", choices=("dev", "holdout", "all"), default="all")
    ap.add_argument("runs", nargs="+")
    args = ap.parse_args()
    if len(args.runs) > 2:
        sys.exit("one or two runs")
    entries = load_manifest(args.scope, args.split)
    print(f"scope {args.scope}, split {args.split}: {len(entries)} recovered entries")
    runs = [load_run(p) for p in args.runs]
    for path, run in zip(args.runs, runs):
        table(path, entries, run)
    if len(runs) == 2:
        repeatability(entries, runs[0], runs[1])


if __name__ == "__main__":
    main()
