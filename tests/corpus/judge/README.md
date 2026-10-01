# The judge eval set

Decided review findings, each LABELED by what a human (or a reproduction) said about it, each
carrying the exact file version its review pass was shown. It exists so a judge — a second
call that confirms or refutes a serious finding before it reaches the queue — can be measured
rather than admired.

⚠ **Nothing here compiles and nothing here runs.** `files/` holds other repos' file versions as
judge INPUTS. `tests/corpus.rs` checks the instrument is intact (every file is the reviewed
bytes, every quote sits where the pass anchored it); it never checks that a judge is right,
because that is a measurement.

⚠ **Excluded from the published crate** (`Cargo.toml` `exclude`): about 5 MB of file versions.

## Why it exists

Ledger [#655](http://localhost:1060/l/default/item/655), critical sweep 1: of the 24 CRITICAL
pending findings on code, verified one by one by reproduction, 1 was partly real and 23 were
not defects (19 `misread`, 4 `no-issue`). The misses share a cause — the reviewer judges a
16 KiB tile it cannot see around — and the plan Brian approved on 2026-10-01 is a judge that
confirms each serious finding WITH the context the reviewer lacked (ledger
[#483](http://localhost:1060/l/default/item/483)). This set is step 1 of that plan: the thing
the judge is measured on.

**It is not `../disclosure/`.** That corpus is nine hand-written REVIEW inputs with planted
contradictions, scored by whether a review pass finds them. This one is real findings a review
pass already made, scored by whether a judge keeps the true ones and refutes the false ones.
Different question, different input, so a directory of its own.

## What is in it (exported 2026-10-01)

| group | what | serious (critical/major) | all |
| --- | --- | --- | --- |
| known-false | a decline whose reason word says the claim does not hold: `misread` 23, `restates` 5, `no-issue` 4 | 32 | 32 |
| known-real / verified | the one sweep-1 finding that was partly real (`801ad4f2…`, fixed in ikigai-cli PR #375) and the intent corpus's six pre-fix defects | 7 | 7 |
| known-real / published | findings a human published into the annotation family | 65 | 97 |

By repo (serious only): known-false — cli 15, gonk 9, browse 8; published — core 22, browse
18, cli 14, gonk 11; verified — cli 2, and ledger, llm, gonk, store, core one each (the intent
entries name their own repos).

**Every finding was recovered.** 0 entries are unrecovered on this export: each pass IRI's
sha256 matched a blob of the path in its repo's git history. The exporter still writes
`unrecovered: <why>` for a finding whose version is missing (a PR-diff finding, a pass over
uncommitted bytes) rather than guessing one.

**Private repos are excluded**: 11 labeled findings on `ikigai-devtools` (3 known-false from the
sweep, 8 published) are not here, because this repo is public and a finding's quote, note and
file are the private repo's content. The manifest's `excluded_private` carries the count.

### How small the real set is, said plainly

The **verified** real set is **7**, and six of those are not model findings at all: the intent
corpus's claims were written by a human from the fix, so a judge may find them more persuasive
than a model's prose. They carry `claim_by: human`. One model-written finding in the whole
store is known by reproduction to be (partly) real.

The **published** set is larger and WEAKER. Publication is a human deciding a note is worth
keeping, not a reproduction of the defect, and the set proves it: two published findings quote
the same line as a known-false one —

- `ikigai-browse:db73e162…` (published, critical) and `ikigai-browse:03d5f843…` (misread,
  sweep 1) both claim `let (byte_start, byte_end) = (lines[first].0, lines[last].1);` panics.
  The sweep ran an exhaustive 3M-call anchor probe and found no panic.
- `ikigai-cli:44585ef8…` (published, major) and `ikigai-cli:55711939…` (misread) quote the same
  manifest line.

Those carry `contested_by` in the manifest and the scorer lists them. So "known-real lost" on the
published row is an UPPER bound on a judge's damage, not a measurement of it: some of what it
"loses" there will be claims a reproduction would also refute.

### Repeated quotes

Ten entries quote a line that occurs more than once in the file (`occurrences` in the
manifest), two of them the wrong-copy anchors the sweep reported (`00b8a9ce…`, `013070c2…`).
`char_start` is where the pass ACTUALLY anchored, recovered exactly rather than guessed: a
finding id is `sha256(pass ‖ char_start ‖ exact)`, so the one occurrence that reproduces the id
is the minted one. Every model entry reproduces its id (`anchor: "minted"`).

## The manifest

`corpus.json` → `entries[]`, one per finding:

| field | meaning |
| --- | --- |
| `entry` | `<repo>:<finding id>` — the name a run's verdicts use. Never named after the defect: a path that said the answer would leak it (the intent corpus's rule) |
| `label`, `basis` | `known-false` with `basis` = the decline word, or `known-real` with `basis` = `published` / `sweep-partly-real` / `intent-corpus` |
| `reason`, `decision_note`, `batch` | the decision's word, its free-text note (for the sweep declines, the reproduction evidence — WHY the claim is false), and its batch (`critical-sweep-1`) |
| `severity`, `decided_severity` | the model's proposal and the human's final rating |
| `quote`, `claim`, `model`, `pass`, `tag` | what the reviewer said, about which text, with which model, in which pass |
| `file`, `content_hash`, `blob`, `commit` | the recovered version (`files/<repo>/<blob prefix>/<path>`), its sha256, its git blob id, and one commit that carries it — a judge that wants the REPO at that version (to find the tests that mention a symbol) reads it from the local clone at `commit` |
| `char_start`, `line`, `anchor`, `occurrences` | where the pass anchored the quote in that version |
| `split` | `dev` or `holdout`, a content-blind hash of the id (about a third held out) |
| `contested_by` | known-false entries on the same line, when there are any |

## Scoring a judge

A judge run is a JSONL file, one `{"entry": …, "verdict": "confirmed" | "refuted" | "unsure",
"calls": n}` per entry. Anything that writes that can be scored:

```sh
python3 tests/corpus/judge/score.py RUN.jsonl [RUN2.jsonl] [--split dev|holdout] [--scope all]
```

It reports known-false refuted / let through, known-real kept / lost (verified and published
apart), unsure, missing, calls made, and given two runs, repeatability (agreement, and every
flip). `--scope serious` (default) scores what the judge is asked to look at.

**The queue as it is today** — `score.py baseline`, a judge that confirms everything:

| group | n | confirmed | refuted |
| --- | --- | --- | --- |
| known-false | 32 | 32 | 0 |
| known-real / verified | 7 | 7 | 0 |
| known-real / published | 65 | 65 | 0 |

Every false finding through, every real one kept, zero calls. A judge earns its place by
refuting known-false findings while keeping known-real ones, by more than its own run-to-run
disagreement.

## Re-exporting

```sh
python3 tests/corpus/judge/export.py
```

Reads gonk through its socket and git history; writes nothing anywhere but this directory. The
numbers move as more findings are decided, so a measurement names the export it ran on.
