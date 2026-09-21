# The disclosure-vs-defect corpus

Nine fixtures, each carrying **one planted contradiction between what a comment says and
what the code does**, and each also carrying two or three HONEST disclosures — `⚠` passages
that warn about a real hazard the code does not have. That mixture is the point: the
discrimination has to be measurable *inside one file*, or a variant that simply says less
will look like a variant that says the right things.

⚠ **Nothing here compiles and nothing here runs.** These are review INPUTS: text handed to
the model by `examples/review-probe.rs`. `cargo` does not discover files under `tests/`
subdirectories, so they are not a test target, and a `.rs` extension is kept only because
the path is part of what the pass shows the model.

⚠ **Edit them and the numbers below stop applying.** They are a fixed instrument; a new
question wants a new fixture beside these, not a rewrite of one of them.

## Why it exists

Ledger [#483](http://localhost:1060/l/default/item/483): the reviewer cannot tell a
DISCLOSED hazard from a PRESENT one, and rates the disclosure `critical`. The obvious fix
is a prompt line telling it that text which states a problem is not a finding — and the
thing that makes that fix dangerous is that **a comment can state a problem the code still
has**, which is the most valuable class this reviewer catches. A variant measured only on a
document full of warnings would look perfect and be untested on the case that matters.
So: the guide measures the noise, this corpus measures the cost.

## The plants, and what three prompts did with them

Six `debug=raw` passes per arm (the archive-bypass arm — the ordinary face would answer an
archive hit), `qwen3-coder:30b`, temperature 0.2, 2026-09-21. "Detected" means a finding
whose NOTE names the contradiction, not one that merely anchors on the right line.

| fixture | the comment claims | the code does | v5 | one-line rule | contrast rule |
| --- | --- | --- | --- | --- | --- |
| `paths.rs` | `rel` is rejected before it is joined | `resolve` joins whatever it is given | 6/6 | 6/6 | 5/6 |
| `cache.rs` | a pure function of its inputs, `.cacheable()` | `rows()` reads `SystemTime::now()` | 6/6 | 6/6 | 6/6 |
| `queue.rs` | every caller handles the missing row | `severity_of` unwraps it | 6/6 | 6/6 | 6/6 |
| `writer.rs` | the segment hash is chained for tamper evidence | the digest is the file's LENGTH | 6/6 | 5/6 | 5/6 |
| `pins.toml` | the pin is a FLOOR, later minors ride free | `^0.12` is `<0.13` — a ceiling too | 0/6 | 0/6 | **4/6** |
| `writer.rs` | every write path calls `check_cap` first | `create` does not | 0/6 | 0/6 | 0/6 |
| `grant.rs` | "the five scopes a review pass actually needs" | six are listed | 0/6 | 0/6 | 0/6 |
| `anchor.rs` | `line_of` is 1-based, the gutter number | it returns the newline COUNT | 0/6 | 0/6 | 0/6 |
| `gates.rs` | it never pipes the command it is judging | the clippy gate is `… \| tee \| tail` | 0/6 | 0/6 | 0/6 |

★ **The split down that table is the finding, and it is not about prompt wording.** Every
plant in the top four is a contradiction whose offending behaviour is EXECUTABLE CODE THE
MODEL CAN QUOTE — a join, a clock read, an unwrap, a length passed off as a digest. Every
plant in the bottom five is a contradiction whose offending behaviour is an ABSENCE (a call
that is not made), a COUNT (six things under a doc that says five), a BASE (0 where the doc
says 1), or a SHAPE the model would have to know a third system's rules to see (Cargo's 0.x
caret). No arm reported any of those — and `anchor.rs` is the sharp case: every arm flagged
`line_of` in nearly every pass, and none of them ever said the thing its own doc comment
gets wrong. **The reviewer anchors in the right place for the wrong reason**, which reads
like recall and is not.

⚠ `pins.toml` is the one cell that moved, and it moved for a half-right reason: the arm that
found it argued that `"0.12"` is an exact pin rather than a caret range. The conclusion was
right, the mechanism it gave was wrong, and a triage reader could not tell from the note.

## Running it again

    cargo run --example review-probe -- tests/corpus/disclosure <out-dir> 6 \
        src/paths.rs src/cache.rs src/queue.rs src/writer.rs pins.toml \
        src/grant.rs src/anchor.rs src/gates.rs

⚠ The `src/` layout is part of the instrument: the pass puts `Path: src/paths.rs` in the
prompt, and the numbers above were taken with those paths. Flattening the directory changes
what the model is told about the file.

Then anchor each `QUOTE:` by substring containment against the fixture (that is exactly what
the pass does before it mints) and count both populations — raw and anchored. The disclosure
half of the measurement is a second run against a document that is mostly warnings;
`ikigai-devtools`'s `claude/CLAUDE.md` is the one the baseline was taken on.

⚠ A difference smaller than the noise floor (0.44–0.53 finding-set agreement between two
identical-prompt runs, ledger [#449](http://localhost:1060/l/default/item/449)) is not a
difference. Per-pass counts on the guide ranged 7–17 within one arm.
