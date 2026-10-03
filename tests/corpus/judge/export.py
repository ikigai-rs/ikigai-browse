#!/usr/bin/env python3
"""Export the judge eval set: decided review findings, labeled, with the exact
file version the reviewer saw.

    python3 tests/corpus/judge/export.py [--socket ~/.ikigai/gonk.sock] \
        [--config ~/.config/ikigai/config.toml] [--repo NAME ...] [--include-private] \
        [--labels LABELS.tsv ...] [--out DIR]
    python3 tests/corpus/judge/export.py --self-test

READS ONLY. Every finding comes out of gonk through its socket (`ikigai --mount
"urn:gk:=<socket>" -c 'source urn:gk:repo:<repo>:findings state=<s>'`), and every file
version comes out of the repo's own git history. Nothing is written to gonk, its store,
or any repo but this directory.

What it writes, beside itself:

  corpus.json   the manifest (one entry per finding, plus what was excluded and why)
  files/<repo>/<blob>/<path>
                the file version each finding was minted against, recovered by
                matching the sha256 in the finding's pass IRI against the path's
                blobs in git history. `<blob>` is the first 12 hex digits of the git
                blob id: neutral, never named after the defect.

Labels (see README.md for the reasons):

  known-false   a decline that carries a reason word saying the claim is not a defect:
                misread, restates, no-issue. The decision's note is kept: it is WHY.
  known-real    basis `verified-real`: a finding a human PUBLISHED and marked REPRODUCED
                (`decision.reproduced`, ledger #696) — a defect shown to happen, the
                decision's note saying how. The real half's strong evidence, and the one
                that grows from ordinary reviewing.
                basis `published`: a finding a human published WITHOUT the mark (it became
                an annotation; a human kept the note, nothing more is known).
                And the one sweep-1 finding that was partly real, and the intent corpus's
                pre-fix defects, whose claims are human-written (marked `claim_by: human`).

REPRODUCED labels (findings sweep 2, ledger #706) come from a LABELS FILE, not from a
decision: a satellite verified each finding by reproduction or a traced code path before
Brian recorded anything, so the finding is still pending in gonk. One row per finding,
tab-separated `iri  repo  outcome  word`, `#` comments:

  not-a-defect  -> known-false, basis `reproduced` (`reason` = the suggested decline word)
  partly-real   -> known-real,  basis `reproduced-partly-real` (`word` = the fixing PR)
  real          -> known-real,  basis `reproduced-real`

A reproduced label OUTRANKS a decision on the same finding: a reproduction is stronger
evidence than a decline word. `reproduced.tsv` beside this file holds the PUBLIC rows and
is always read; `--labels FILE` adds more (the hub's file), and when the output is this
directory its public rows are merged into `reproduced.tsv`, so a later re-export without
the flag rebuilds the same set. ⚠ A private repo's rows are never written there.
⚠ SELECTION: sweep 2 verified only findings `judge-v1@qwen3-coder-next` CONFIRMED, so the
reproduced group measures a judge's precision on what v1 let through, not a random sample.

Declines with any other word (duplicate, wont-fix) and declines with no word are not
labeled: a duplicate may be true, a wont-fix is true by definition, and a wordless decline
says nothing about why.

⚠ PRIVATE REPOS ARE EXCLUDED by default. This repo is public; a finding's quote, note and
the file it was minted against are the private repo's content. Visibility is asked of
GitHub (`gh api repos/ikigai-rs/<repo>`); a repo whose visibility cannot be established
is treated as private. `--include-private` exists for a local, uncommitted run: give it
`--out` outside this directory (a scratch dir), and score that run with `score.py --corpus`.

Re-running it rewrites corpus.json and files/ from the live store, so the counts move as
Brian decides more findings. The README's numbers name the run they came from.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
KNOWN_FALSE_WORDS = ("misread", "restates", "no-issue")
PASS_PREFIX = "urn:ikigai:browse:review:"
# The sweep-1 finding verified PARTLY real (ledger #655): a quoted named arg on a sink
# line dropped the backslash of an unknown escape; fixed in ikigai-cli PR #375. It is
# still undecided in gonk, so it is named here rather than found by a query.
EXTRA_REAL = {
    "801ad4f26abdaea286af38ae": "critical sweep 1 (ledger #655): partly real, fixed in "
    "ikigai-cli PR #375 (76eacc5)",
}
# The reproduced labels this corpus was built from (public rows only), and what each
# outcome word means for the eval set.
REPRODUCED_TSV = os.path.join(HERE, "reproduced.tsv")
REPRODUCED = {
    "not-a-defect": ("known-false", "reproduced"),
    "partly-real": ("known-real", "reproduced-partly-real"),
    "real": ("known-real", "reproduced-real"),
}
# The intent corpus's repos, by the directory name its entries use.
INTENT_DIR = os.path.join(os.path.dirname(HERE), "intent")


def run(cmd, cwd=None, stdin=None, check=True):
    p = subprocess.run(cmd, cwd=cwd, input=stdin, capture_output=True)
    if check and p.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd)}: exit {p.returncode}: {p.stderr.decode()[:400]}")
    return p.stdout


def gonk(args, iri_and_args):
    out = run([args.ikigai, "--mount", f"urn:gk:={args.socket}", "-c", f"source {iri_and_args}"])
    return json.loads(out)


def roots(config_path):
    """repo name -> checkout path, from the config home's `gonk.browse.root` lines: the
    same table gonk serves from, so a name here is a name gonk answers for."""
    table = {}
    with open(config_path) as f:
        for line in f:
            m = re.match(r'^\s*gonk\.browse\.root\s*=\s*"([^=]+)=([^"]+)"', line)
            if m:
                table[m.group(1)] = os.path.expanduser(m.group(2))
    return table


def visibility(repo):
    try:
        out = run(["gh", "api", f"repos/ikigai-rs/{repo}", "--jq", ".visibility"])
        return out.decode().strip()
    except Exception:
        return "unknown"


def iri_encode(path):
    """browse's `iri_encode`: the path as it appears in a pass IRI."""
    safe = b"-._~/!$&'()*+,;=:@"
    return "".join(
        chr(b) if chr(b).isascii() and (chr(b).isalnum() or b in safe) else f"%{b:02X}"
        for b in path.encode()
    )


def parse_pass(pass_iri, repo, path):
    """(hash, tag) out of `urn:ikigai:browse:review:{repo}:{hash}:{tag}:{path}`. The tag
    itself contains colons (`review-v5@qwen3-coder:30b`), so it is what lies between the
    hash and the known path, never a split on `:`."""
    if not pass_iri or not pass_iri.startswith(PASS_PREFIX + repo + ":"):
        return None
    rest = pass_iri[len(PASS_PREFIX) + len(repo) + 1:]
    m = re.match(r"^(sha256:[0-9a-f]{64}):(.*)$", rest)
    if not m:
        return None
    tail = ":" + iri_encode(path)
    tag = m.group(2)[: -len(tail)] if m.group(2).endswith(tail) else None
    return m.group(1), tag


def finding_id(pass_iri, char_start, exact):
    """annotate::finding_id: 24 hex of sha256(pass \\n char_start \\n exact)."""
    return hashlib.sha256(f"{pass_iri}\n{char_start}\n{exact}".encode()).hexdigest()[:24]


class History:
    """Every blob a path has had in one repo's git history, by sha256 of its bytes."""

    def __init__(self, root):
        self.root = root
        self.by_path = {}

    def blobs(self, path):
        if path in self.by_path:
            return self.by_path[path]
        found = {}
        try:
            commits = run(["git", "rev-list", "--all", "--", path], cwd=self.root).decode().split()
        except RuntimeError:
            commits = []
        if commits:
            specs = "".join(f"{c}:{path}\n" for c in commits).encode()
            checks = run(["git", "cat-file", "--batch-check"], cwd=self.root, stdin=specs)
            seen = {}
            for commit, line in zip(commits, checks.decode().splitlines()):
                parts = line.split()
                if len(parts) == 3 and parts[1] == "blob" and parts[0] not in seen:
                    seen[parts[0]] = commit
            for oid, commit in seen.items():
                data = run(["git", "cat-file", "blob", oid], cwd=self.root)
                digest = "sha256:" + hashlib.sha256(data).hexdigest()
                found.setdefault(digest, (oid, commit, data))
        self.by_path[path] = found
        return found


def recover(histories, root_table, repo, path, content_hash):
    if repo not in root_table:
        return None, f"no checkout for {repo} in the config home"
    root = root_table[repo]
    if not os.path.isdir(os.path.join(root, ".git")):
        return None, f"{root} is not a git checkout"
    history = histories.setdefault(repo, History(root))
    hit = history.blobs(path).get(content_hash)
    if hit:
        return hit, None
    # The working tree, in case the pass reviewed bytes that were never committed.
    try:
        with open(os.path.join(root, path), "rb") as f:
            data = f.read()
        if "sha256:" + hashlib.sha256(data).hexdigest() == content_hash:
            return None, "matches only the uncommitted working tree"
    except OSError:
        pass
    return None, "no blob of this path in git history has the reviewed hash"


def char_offsets(text, exact):
    """Every character offset `exact` occurs at in `text`."""
    out, start = [], 0
    while True:
        idx = text.find(exact, start)
        if idx < 0:
            return out
        out.append(len(text[:idx]))
        start = idx + 1


def split_of(entry_id):
    """A deterministic, content-blind split: about a third held out."""
    return "holdout" if int(hashlib.sha256(entry_id.encode()).hexdigest(), 16) % 3 == 0 else "dev"


def read_labels(path):
    """finding id -> {iri, repo, outcome, word} from one labels file. A row whose
    outcome is not one of REPRODUCED is an error, not a skip: a label the exporter
    cannot place would silently shrink the set."""
    out = {}
    with open(path) as f:
        for n, line in enumerate(f, 1):
            line = line.rstrip("\n")
            if not line.strip() or line.startswith("#"):
                continue
            cols = line.split("\t")
            if cols[0] == "iri":
                continue
            if len(cols) < 3 or not cols[0].startswith("urn:iki:finding:"):
                sys.exit(f"{path}:{n}: not `iri<TAB>repo<TAB>outcome<TAB>word`: {line!r}")
            iri, repo, outcome = cols[0], cols[1], cols[2]
            word = cols[3] if len(cols) > 3 else ""
            if outcome not in REPRODUCED:
                sys.exit(f"{path}:{n}: outcome {outcome!r} is not one of {sorted(REPRODUCED)}")
            out[iri[len("urn:iki:finding:"):]] = {
                "iri": iri, "repo": repo, "outcome": outcome, "word": word,
            }
    return out


def write_labels(path, labels):
    with open(path, "w") as f:
        f.write(
            "# Reproduced labels for the judge eval set (export.py reads this file; README.md says\n"
            "# why). One row per finding, verified by reproduction or a traced code path before any\n"
            "# decision was recorded. Public repos only. outcome: not-a-defect | partly-real | real.\n"
            "# word: the suggested decline word, or the PR that fixed it.\n"
            "iri\trepo\toutcome\tword\n"
        )
        for fid in sorted(labels, key=lambda k: (labels[k]["repo"], k)):
            r = labels[fid]
            f.write(f"{r['iri']}\t{r['repo']}\t{r['outcome']}\t{r['word']}\n")


def label_of(row, reproduced=None):
    """(label, basis) for one gonk finding row, or (None, why it is not labeled).

    A REPRODUCED label (`reproduced`, the row's entry in a labels file) wins over any
    decision. Otherwise a reproduced publish is `verified-real`; a plain one stays
    `published`. The mark is read from the CURRENT decision only, and only as JSON
    `true`: a row from a browse that predates the mark has no `reproduced` key and
    reads as a plain publish.
    """
    if reproduced is not None:
        return REPRODUCED[reproduced["outcome"]]
    d = row.get("decision") or {}
    if row.get("state") == "published":
        if d.get("reproduced") is True:
            return "known-real", "verified-real"
        return "known-real", "published"
    if row.get("state") == "declined":
        word = d.get("reason")
        if word in KNOWN_FALSE_WORDS:
            return "known-false", word
        return None, f"declined with {'reason `' + word + '`' if word else 'no reason word'}"
    if row["id"] in EXTRA_REAL:
        return "known-real", "sweep-partly-real"
    return None, f"state {row.get('state')}"


def self_test():
    """The labels, pinned: run by `tests/corpus.rs`, so a change to what a decision row
    means for the eval set fails a cargo test rather than a re-export nobody reads."""
    def row(state, **decision):
        return {"id": "f00", "state": state, "decision": decision or None}

    cases = [
        (row("published", outcome="published", reproduced=True), ("known-real", "verified-real")),
        (row("published", outcome="published", reproduced=False), ("known-real", "published")),
        # A browse that predates the mark: no key at all reads as a plain publish.
        (row("published", outcome="published"), ("known-real", "published")),
        # Only a JSON true is the mark: a string is not.
        (row("published", outcome="published", reproduced="yes"), ("known-real", "published")),
        (row("declined", outcome="declined", reason="misread"), ("known-false", "misread")),
        (row("declined", outcome="declined", reason="restates"), ("known-false", "restates")),
        (row("declined", outcome="declined", reason="no-issue"), ("known-false", "no-issue")),
        (row("declined", outcome="declined", reason="wont-fix"),
         (None, "declined with reason `wont-fix`")),
        (row("declined", outcome="declined"), (None, "declined with no reason word")),
        (row("pending"), (None, "state pending")),
    ]
    lab = lambda outcome: {"iri": "urn:iki:finding:f00", "repo": "r", "outcome": outcome, "word": ""}
    reproduced_cases = [
        # A reproduction outranks a decision, either way.
        (row("pending"), lab("not-a-defect"), ("known-false", "reproduced")),
        (row("published", outcome="published"), lab("not-a-defect"), ("known-false", "reproduced")),
        (row("declined", outcome="declined", reason="misread"), lab("real"),
         ("known-real", "reproduced-real")),
        (row("pending"), lab("partly-real"), ("known-real", "reproduced-partly-real")),
    ]
    failed = 0
    for given, label, want in reproduced_cases:
        got = label_of(given, label)
        if got != want:
            failed += 1
            print(f"label_of({given}, {label['outcome']}) = {got}, want {want}", file=sys.stderr)
    for given, want in cases:
        got = label_of(given)
        if got != want:
            failed += 1
            print(f"label_of({given}) = {got}, want {want}", file=sys.stderr)
    # The scorer files a verified-real entry under the verified group, never published.
    # No __pycache__ beside the corpus: the check must leave the tree as it found it.
    sys.dont_write_bytecode = True
    sys.path.insert(0, HERE)
    import score  # noqa: E402  (beside this file; imported only for the check)

    groups = [("known-real", "verified-real", "known-real/verified"),
              ("known-real", "published", "known-real/published"),
              ("known-real", "sweep-partly-real", "known-real/verified"),
              ("known-real", "intent-corpus", "known-real/verified"),
              ("known-real", "reproduced-real", "known-real/reproduced"),
              ("known-real", "reproduced-partly-real", "known-real/reproduced"),
              ("known-false", "misread", "known-false"),
              ("known-false", "reproduced", "known-false/reproduced")]
    for label, basis, group in groups:
        got = score.basis_group({"label": label, "basis": basis})
        if got != group:
            failed += 1
            print(f"basis_group({label}, {basis}) = {got}, want {group}", file=sys.stderr)
    # Every outcome a labels file may carry has a group the scorer reports.
    for outcome, (label, basis) in REPRODUCED.items():
        if score.basis_group({"label": label, "basis": basis}) not in score.GROUPS:
            failed += 1
            print(f"outcome {outcome}: no scorer group", file=sys.stderr)
    if failed:
        sys.exit(f"{failed} label case(s) wrong")
    print(f"ok: {len(cases) + len(reproduced_cases) + len(groups) + len(REPRODUCED)} label cases")


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--socket", default=os.path.expanduser("~/.ikigai/gonk.sock"))
    ap.add_argument("--config", default=os.path.expanduser("~/.config/ikigai/config.toml"))
    ap.add_argument("--ikigai", default="ikigai")
    ap.add_argument("--repo", action="append", help="limit to these repos (default: every root)")
    ap.add_argument("--include-private", action="store_true")
    ap.add_argument("--labels", action="append", default=[],
                    help="a reproduced-labels TSV to add (repeatable); reproduced.tsv is always read")
    ap.add_argument("--out", default=HERE)
    args = ap.parse_args()

    table = roots(args.config)
    repos = args.repo or sorted(table)
    labels = read_labels(REPRODUCED_TSV) if os.path.exists(REPRODUCED_TSV) else {}
    committed = dict(labels)
    for path in args.labels:
        labels.update(read_labels(os.path.expanduser(path)))
    vis = {}
    rows, excluded, private = [], [], {}
    for repo in repos:
        vis[repo] = visibility(repo)
        public = args.include_private or vis[repo] == "public"
        repo_rows = []
        for state in ("declined", "published"):
            repo_rows += gonk(args, f"urn:gk:repo:{repo}:findings state={state}")
        wanted = list(EXTRA_REAL) + [fid for fid, r in labels.items() if r["repo"] == repo]
        for fid in wanted:
            if any(r["id"] == fid for r in repo_rows):
                continue
            try:
                one = gonk(args, f"urn:gk:iki:finding:{fid} as=application/json")
            except Exception:
                if fid in labels:
                    print(f"{repo}: labeled finding {fid} not found in gonk", file=sys.stderr)
                continue
            if one.get("repo") == repo:
                repo_rows.append(one)
        for row in repo_rows:
            reproduced = labels.get(row["id"])
            label, basis = label_of(row, reproduced)
            if label is None:
                continue
            row["_reproduced"] = reproduced
            if not public:
                private[repo] = private.get(repo, 0) + 1
                continue
            row["_label"], row["_basis"] = label, basis
            rows.append(row)
        print(f"{repo}: {len(repo_rows)} decided rows read{'' if public else ' (private: none kept)'}",
              file=sys.stderr)

    files_dir = os.path.join(args.out, "files")
    if os.path.isdir(files_dir):
        shutil.rmtree(files_dir)
    histories, entries = {}, []
    for row in sorted(rows, key=lambda r: (r["repo"], r["id"])):
        repo, fid = row["repo"], row["id"]
        d = row.get("decision") or {}
        entry = {
            "entry": f"{repo}:{fid}",
            "id": fid,
            "iri": row["iri"],
            "repo": repo,
            "path": row["path"],
            "label": row["_label"],
            "basis": row["_basis"],
            "claim_by": "model",
            "severity": row.get("severity"),
            "decided_severity": d.get("severity"),
            "reason": d.get("reason"),
            "decision_note": d.get("note"),
            "batch": d.get("batch"),
            "quote": row["exact"],
            "claim": row["body"],
            "pass": row.get("generated_by"),
            "model": row.get("creator"),
            "split": split_of(fid),
        }
        if fid in EXTRA_REAL:
            entry["decision_note"] = EXTRA_REAL[fid]
        rep = row.get("_reproduced")
        if rep is not None:
            # The suggested word is the reason a known-false entry carries; the
            # decision fields stay what gonk holds (normally none: still pending).
            entry["reason"] = rep["word"] if entry["label"] == "known-false" else None
            entry["decision_note"] = (
                f"findings sweep 2 (ledger #706): {rep['outcome']} by reproduction or a traced "
                f"code path; {rep['word'] or 'no word'}. Evidence: the ledger #706 comments."
            )
            entry["batch"] = "major-sweep-2"
            entry["selected_by"] = "judge-v1@qwen3-coder-next:latest confirmed"
        if row.get("pr") is not None:
            entry.update(file=None, unrecovered="a pull-request diff, not a file version")
            entries.append(entry)
            continue
        parsed = parse_pass(row.get("generated_by"), repo, row["path"])
        if not parsed:
            entry.update(file=None, unrecovered="no review-pass IRI to read the reviewed hash from")
            entries.append(entry)
            continue
        content_hash, tag = parsed
        entry["tag"] = tag
        entry["content_hash"] = content_hash
        hit, why = recover(histories, table, repo, row["path"], content_hash)
        if not hit:
            entry.update(file=None, unrecovered=why)
            entries.append(entry)
            continue
        oid, commit, data = hit
        rel = os.path.join("files", repo, oid[:12], row["path"])
        dest = os.path.join(args.out, rel)
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        if not os.path.exists(dest):
            with open(dest, "wb") as f:
                f.write(data)
        text = data.decode("utf-8", errors="replace")
        offsets = char_offsets(text, row["exact"])
        # The minted position, exactly: the id is sha256(pass, char_start, exact), so
        # the one occurrence that reproduces it IS where the pass anchored.
        minted = [o for o in offsets if finding_id(row["generated_by"], o, row["exact"]) == fid]
        start = minted[0] if minted else (offsets[0] if offsets else None)
        entry.update(
            file=rel,
            commit=commit,
            blob=oid,
            char_start=start,
            anchor="minted" if minted else ("first-occurrence" if offsets else "quote-absent"),
            occurrences=len(offsets),
            line=None if start is None else text[:start].count("\n") + 1,
        )
        entries.append(entry)

    # The intent corpus's pre-fix defects: real by construction (each was shipped and
    # fixed), with a HUMAN-written claim — a judge may find that more persuasive than a
    # model's, so they are kept apart by `claim_by`.
    with open(os.path.join(INTENT_DIR, "corpus.json")) as f:
        intent = json.load(f)
    for item in intent:
        name = item["entry"]
        text = open(os.path.join(INTENT_DIR, name, item["path"]), encoding="utf-8").read()
        quote = item["anchors"][0]
        offsets = char_offsets(text, quote)
        start = offsets[0] if offsets else None
        entries.append({
            "entry": f"intent:{name}",
            "id": f"intent-{name}",
            "repo": item["repo"].split("/")[-1],
            "path": item["path"],
            "label": "known-real",
            "basis": "intent-corpus",
            "claim_by": "human",
            "severity": "critical",
            "quote": quote,
            "claim": item["finding"],
            "file": os.path.relpath(os.path.join(INTENT_DIR, name, item["path"]), args.out),
            "commit": item["sha"],
            "char_start": start,
            "anchor": "first-occurrence",
            "occurrences": len(offsets),
            "line": None if start is None else text[:start].count("\n") + 1,
            "split": split_of(f"intent-{name}"),
        })

    # A label CONTRADICTION, made visible rather than resolved: a published finding
    # quoting the same line of the same file as a known-false one. Publication is a
    # human keeping a note; a known-false word is a human (or a sweep, by reproduction)
    # saying the claim does not hold. When both sit on one line the scorer reports them.
    false_lines = {}
    for e in entries:
        if e["label"] == "known-false":
            false_lines.setdefault((e["repo"], e["path"], e["quote"]), []).append(e["entry"])
    for e in entries:
        if e["label"] == "known-real":
            hits = false_lines.get((e["repo"], e["path"], e["quote"]))
            if hits:
                e["contested_by"] = hits

    manifest = {
        "exported_from": "gonk (urn:gk:repo:<repo>:findings state=declined|published)",
        "known_false_words": list(KNOWN_FALSE_WORDS),
        "excluded_private": private,
        "entries": entries,
    }
    with open(os.path.join(args.out, "corpus.json"), "w") as f:
        json.dump(manifest, f, indent=1, ensure_ascii=False)
        f.write("\n")

    # The public labels this export used, kept beside it so a re-export without
    # `--labels` rebuilds the same set. Only into THIS directory, only public repos.
    if os.path.realpath(args.out) == os.path.realpath(HERE):
        keep = dict(committed)
        for fid, r in labels.items():
            if r["repo"] not in vis:
                vis[r["repo"]] = visibility(r["repo"])
            if vis[r["repo"]] == "public":
                keep[fid] = r
        if keep != committed:
            write_labels(REPRODUCED_TSV, keep)

    by = {}
    for e in entries:
        key = (e["label"], e["basis"], "recovered" if e.get("file") else "unrecovered")
        by[key] = by.get(key, 0) + 1
    for key in sorted(by):
        print(f"{key[0]:12} {key[1]:20} {key[2]:12} {by[key]}")
    print(f"excluded (private): {private}")


if __name__ == "__main__":
    main()
