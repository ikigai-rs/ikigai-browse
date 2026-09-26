#!/usr/bin/env python3
"""Score review-probe runs over the intent corpus.

    python3 tests/corpus/intent/score.py --arm general=<out-dir> --arm intent=<out-dir> [--runs 2]

For every arm and corpus entry it parses each raw answer (the probe writes
`<entry>_<path>.<i>.txt` per run), anchors every QUOTE by substring containment
against the blob exactly as the pass does (after stripping leading decoration),
and prints, per entry:

  - each run's findings as `line severity note` so a human can judge them against
    the manifest's `finding` text - the corpus hit is a HUMAN judgment, this
    script only lays the evidence out;
  - the per-arm run-to-run overlap (Jaccard) on two matchers: anchor LINE alone,
    and line plus a loose gist match on the note's content words - bytes never
    repeat at temperature 0.2, so identity is line + gist (ledger #475);
  - findings per pass, serious share (critical/major) and orphan rate, per arm;
  - and finally `CANDIDATES`: every finding anchored within a few lines of one of
    the entry's anchors, the short list to read first.

Nothing here decides the verdict. It is the instrument the disclosure corpus
README describes by hand, written down once.
"""

import argparse
import json
import os
import re
import sys

SERIOUS = {"critical", "major"}
STOP = set(
    "the this that with from into which where what when have been will would could should "
    "there their they them then than also only over under about after before because being "
    "does done each such very more most some same other these those while your it's".split()
)


def strip_decoration(quote):
    q = quote.strip()
    q = re.sub(r"^(?:[-*+]\s+|\d+[.)]\s+)", "", q)
    q = q.strip("`\"'")
    return q.strip()


def parse(answer):
    """(findings, malformed): findings are dicts with quote/severity/note."""
    regions = re.split(r"^--- region \d+ of \d+ \(bytes \d+[–-]\d+\) ---\n", answer, flags=re.M)
    findings, malformed = [], 0
    for region_index, region in enumerate(regions):
        current = None
        for line in region.splitlines():
            stripped = line.strip().strip("*").strip()
            m = re.match(r"^(QUOTE|SEVERITY|NOTE):\s*(.*)$", stripped)
            if not m:
                if current is not None and current.get("note") is not None and stripped:
                    current["note"] += " " + stripped
                continue
            key, value = m.group(1).lower(), m.group(2).strip()
            if key == "quote":
                if current is not None:
                    if current.get("note") is None:
                        malformed += 1
                    findings.append(current)
                current = {"quote": value, "severity": None, "note": None, "region": region_index}
            elif current is None:
                malformed += 1
            else:
                current[key] = value
        if current is not None:
            if current.get("note") is None:
                malformed += 1
            findings.append(current)
    return findings, malformed


def anchor(finding, blob):
    for candidate in (finding["quote"], strip_decoration(finding["quote"])):
        if candidate and candidate in blob:
            return blob[: blob.index(candidate)].count("\n") + 1
    return None


def words(note):
    return {w for w in re.findall(r"[a-z_:]{4,}", (note or "").lower()) if w not in STOP}


def gist_match(a, b):
    wa, wb = words(a["note"]), words(b["note"])
    if not wa or not wb:
        return False
    return len(wa & wb) / len(wa | wb) >= 0.15


def jaccard(runs, same):
    """Greedy one-to-one matching between run 0 and run 1 under `same`."""
    if len(runs) < 2:
        return None, 0, 0
    a, b = list(runs[0]), list(runs[1])
    matched = 0
    used = set()
    for fa in a:
        for j, fb in enumerate(b):
            if j in used:
                continue
            if same(fa, fb):
                used.add(j)
                matched += 1
                break
    union = len(a) + len(b) - matched
    return (matched / union if union else 1.0), matched, union


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", action="append", required=True, help="name=out-dir")
    ap.add_argument("--runs", type=int, default=2)
    ap.add_argument("--corpus", default=os.path.dirname(os.path.abspath(__file__)))
    ap.add_argument("--near", type=int, default=4, help="candidate window around an anchor, lines")
    args = ap.parse_args()

    manifest = json.load(open(os.path.join(args.corpus, "corpus.json")))
    arms = [a.split("=", 1) for a in args.arm]
    totals = {name: {"findings": 0, "serious": 0, "orphans": 0, "passes": 0, "malformed": 0} for name, _ in arms}
    floors = {name: {"line": [], "gist": []} for name, _ in arms}
    candidates = []

    for entry in manifest:
        name, path = entry["entry"], entry["path"]
        blob = open(os.path.join(args.corpus, name, path), encoding="utf-8").read()
        anchor_lines = []
        for a in entry["anchors"]:
            start = 0
            while True:
                i = blob.find(a, start)
                if i < 0:
                    break
                anchor_lines.append(blob[:i].count("\n") + 1)
                start = i + 1
        print("=" * 100)
        print(f"{name}  {path}  Q{entry['question']}  anchors at lines {sorted(set(anchor_lines))}")
        print(f"  truth: {entry['finding']}")
        for arm, out in arms:
            runs = []
            for i in range(args.runs):
                file = os.path.join(out, f"{name}/{path}".replace("/", "_") + f".{i}.txt")
                if not os.path.exists(file):
                    print(f"  [{arm} #{i}] MISSING {file}")
                    runs.append([])
                    continue
                text = open(file, encoding="utf-8", errors="replace").read()
                if text.startswith("ERROR:"):
                    print(f"  [{arm} #{i}] {text[:200]}")
                    runs.append([])
                    continue
                findings, malformed = parse(text)
                for f in findings:
                    f["line"] = anchor(f, blob)
                anchored = [f for f in findings if f["line"] is not None]
                runs.append(anchored)
                t = totals[arm]
                t["passes"] += 1
                t["findings"] += len(findings)
                t["malformed"] += malformed
                t["serious"] += sum(1 for f in findings if (f["severity"] or "").lower() in SERIOUS)
                t["orphans"] += len(findings) - len(anchored)
                clean = "NOTHING ABOVE THRESHOLD" in text
                print(f"  [{arm} #{i}] {len(findings)} findings, {len(findings) - len(anchored)} orphan, "
                      f"{malformed} malformed{', says NOTHING ABOVE THRESHOLD' if clean else ''}")
                for f in sorted(anchored, key=lambda f: f["line"]):
                    near = any(abs(f["line"] - al) <= args.near for al in anchor_lines)
                    flag = "*" if near else " "
                    sev = (f["severity"] or "?").lower()
                    note = f["note"] or "(no NOTE line)"
                    print(f"     {flag} L{f['line']:<5} {sev:<9} {note[:230]}")
                    if near:
                        candidates.append((name, arm, i, f["line"], sev, note))
            j_line, m1, u1 = jaccard(runs, lambda a, b: a["line"] == b["line"])
            j_gist, m2, u2 = jaccard(runs, lambda a, b: a["line"] == b["line"] and gist_match(a, b))
            if j_line is not None:
                floors[arm]["line"].append(j_line)
                floors[arm]["gist"].append(j_gist)
                print(f"  [{arm}] run 0 vs run 1: line-only Jaccard {j_line:.2f} ({m1}/{u1}); "
                      f"line+gist Jaccard {j_gist:.2f} ({m2}/{u2})")

    print("=" * 100)
    print("PER-ARM TOTALS")
    for arm, _ in arms:
        t = totals[arm]
        if not t["passes"]:
            continue
        fl = floors[arm]
        print(f"  {arm:<10} passes {t['passes']:>3}  findings/pass {t['findings'] / t['passes']:.2f}  "
              f"serious share {t['serious'] / max(t['findings'], 1):.1%}  "
              f"orphan rate {t['orphans'] / max(t['findings'], 1):.1%}  malformed {t['malformed']}")
        if fl["line"]:
            print(f"  {'':<10} run-to-run Jaccard, mean over files: line-only {sum(fl['line']) / len(fl['line']):.2f}  "
                  f"line+gist {sum(fl['gist']) / len(fl['gist']):.2f}")
    print("=" * 100)
    print("CANDIDATES (anchored within the window of a ground-truth anchor; judge by hand)")
    for name, arm, i, line, sev, note in candidates:
        print(f"  {name:<7} {arm:<8} #{i} L{line:<5} {sev:<9} {note}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
