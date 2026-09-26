# The intent-defect corpus

Six real defects this ecosystem shipped and fixed, each checked in as the **pre-fix blob** so the
defect is present in the reviewed text, and each tagged with the intent question it belongs to.
`corpus.json` is the manifest: repo, path, the commit BEFORE the fix, the fix, the question,
verbatim anchors a correct finding would sit on, and what a correct finding says — one or two
sentences a human matches a finding against. `tests/corpus.rs` checks the manifest is complete and
the anchors still occur; it never checks that a model finds anything, because that is a
measurement, not a gate.

⚠ **Nothing here compiles and nothing here runs.** These are review INPUTS handed to the model by
`examples/review-probe.rs`. The directory names are the REPO names and the paths are the real
paths (`time/crates/ikigai-time/src/lib.rs`) because the pass shows the model `Path: …`, and an
entry named after its defect would put the answer in the prompt.

⚠ **Edit a blob and the numbers below stop applying.** A new defect wants a new entry beside these.

## Why it exists

Ledger [#456](http://localhost:1060/l/default/item/456): does a LENS — a prompt carrying a
class of question the general pass has no reason to ask — find defects the general pass missed
on the same bytes, by more than the general pass's own run-to-run variance? The first candidate
is the INTENT lens ([#518](http://localhost:1060/l/default/item/518) idea 3): the four questions
the constitution already states as rules — declared = enforced; the confused deputy; the tenancy
boundary; a verb that lies. [#452](http://localhost:1060/l/default/item/452) fixes the metric:
RECALL on known serious defects; findings-per-file is never the metric.

## The entries

| entry | repo @ pre-fix sha | Q | the defect | fixed by |
| --- | --- | --- | --- | --- |
| `ledger` | ikigai-ledger @ `21e25b6` | 1 | `item:{id}`'s per-verb spec demands the broad `urn:cap:store:read` beside a flat sibling declaring the per-graph family; a narrow-grant caller is denied one item | 0.2.1, PR #5 |
| `llm` | ikigai-llm @ `6b1930a` | 1 | `CAP_NET` declared exact (`urn:cap:net`) where `require_net` enforces the per-host family | 0.9.1, PR #3 |
| `time` | ikigai-cli @ `e4211a8` (ikigai-time) | 4 | a scheduled job fires under the registry's `Capability::root()`, never the scheduler's | ikigai-time 0.4.0, PR #356, ledger #79 |
| `gonk` | ikigai-gonk @ `22e34cd` | 3 | any loopback peer — a browser page from any origin — gets the ledgers' write tokens; no Origin/Host/Sec-Fetch-Site check | PR #2 |
| `store` | ikigai-store @ `fe676d0` | 3 | writes are per-graph, every read is dataset-wide; the tenancy boundary is half a boundary | 0.2.2, PR #4 |
| `vocab` | ikigai-core @ `f005754` (ikigai-vocab) | 2 | `description.id` interpolated into an IRI position on the strength of a comment | PR #99 |

**Carving notes, reported rather than forced.** `vocab` is question 2's MECHANISM (a value
reaching an emitted document as syntax) without its authority shape — the id is author-supplied,
not caller-supplied; it is the closest one-file Q2 blob the ecosystem has. `time` is Q4 with a Q1
edge (no `urn:time:*` action declares `requires`). Two of the brief's candidates have no pre-fix
blob at all: `graph-update` refused the broad write token from its birth in store 0.2.1, and
core #78's compat survey found no module that declared a capability and forgot the check — the
defect was in the kernel's dispatch, not on the module side. The ledger's 0.2.0 confused-deputy
shape (its own SPARQL escaper over the broad store token) is disclosed in that file's own header,
so a finding there could not be told from a restatement
([#483](http://localhost:1060/l/default/item/483)); dropped.

⚠ **The evidence is split across TILES in two entries.** At the pass's 16 KiB regions, `ledger`'s
two declarations (lines ~496 and ~981 of 112 KB) and `llm`'s constant (line 39) and its
enforcement (line ~1490 of 88 KB) are never in one prompt. That is the projection variable
([#455](http://localhost:1060/l/default/item/455)) and it is what the numbers below show.

## The measurement (2026-09-25)

`qwen3-coder:30b-a3b-q8_0` (the live `coder`), temperature 0.2, `debug=raw` (the archive bypassed,
so every pass is an independent sample), whole files, two runs per file per arm, through
`examples/review-probe.rs`. The lens arm added `INTENT_LENS` (the four questions) between the
instruction and the file; the format contract, the reminder and the system prompt were
byte-identical between arms. Scored by `score.py` (anchoring by substring containment, as the pass
does) and then by hand against the manifest's `finding` text. A HIT means the note names the
mechanism well enough that a reader would fix the right thing.

**The noise floor first** — the general pass against itself, same bytes, same prompt:

| | general | intent lens |
| --- | --- | --- |
| passes | 12 | 12 |
| findings / pass | 31.5 | 32.3 |
| serious share (critical + major) | 42.3% | **69.3%** |
| orphan rate | 5.0% | 4.4% |
| run-to-run Jaccard, mean over files, anchor line only | **0.43** | 0.38 |
| run-to-run Jaccard, anchor line + note gist | **0.24** | 0.18 |
| wall clock, 12 passes | 10:48 (54 s/pass) | 13:14 (66 s/pass, 1.23×) |

Two runs of `review-v5@coder` on the same file agree on about 43% of anchor lines and 24% of
(line, gist) pairs — consistent with the 0.44–0.53 measured on #449's four files, now on six
larger ones. On corpus defects specifically the floor is **0 of 6**: no defect was found by one
general run and missed by the other, because none was found at all.

**Recall on the corpus:**

| entry | Q | general #0 | general #1 | lens #0 | lens #1 | what the lens said |
| --- | --- | --- | --- | --- | --- | --- |
| `ledger` | 1 | – | – | – | – | Q1 asked of the wrong lines; and false "declared but never enforced" claims on `requires` the kernel enforces |
| `llm` | 1 | – | – | near | near | anchors `.requires(CAP_NET)` both runs and argues declared ≠ enforced — the WRONG mismatch (other backends might skip `require_net`); the constant's value is in another region |
| `time` | 4 | – | – | near | **HIT** | #1: schedule never checks the caller's authority over the target, so Delete/Sink can be scheduled unauthorized; #0 muddles it into "the resolver is not validated". Both runs also flag `time-cancel`/`time-jobs` enforcing nothing — a true defect #79's fix also closed, outside the written truth |
| `gonk` | 3 | near | near | near | near | every run anchors `http_cap`; general argues doc-vs-design, lens argues peer-address spoofing; nobody says a browser page can write |
| `store` | 3 | – | – | **HIT** | **HIT** | reads are not graph-scoped where writes are; #1 names the missing `CAP_READ_GRAPH` outright |
| `vocab` | 2 | – | – | – | – | both arms fixate on `ik:verb`/`ik:requires` literals (which ARE escaped) and never look at the subject IRI |
| **either run** | | **0 / 6** | | **2 / 6** | | |
| **both runs** | | 0 / 6 | | 1 / 6 | | |

- **Unique yield of the lens**: 2 of 6 either-run, 1 of 6 in both runs, against a corpus noise
  floor of 0.
- **What the lens lost**: nothing the general pass found, because it found nothing on this corpus.
- **Near misses**: `llm` and `gonk` in both lens runs — right question, right line, wrong claim.
  `llm`'s is a tiling artifact; `gonk`'s is not.

## The verdict, by the rule fixed before the run

> The lens earns its place only if its unique yield on corpus defects exceeds the noise floor, and
> it loses nothing the general pass reliably found.

**Keep** — 2 > 0 and nothing lost. Stated with its weaknesses, because the rule was fixed so
that taste could not decide either way:

1. The yield rests on one STABLE hit (`store`, 2/2) and one coin flip (`time`, 1/2) —
   [#492](http://localhost:1060/l/default/item/492)'s bimodality applies to the lens as much as
   to the general pass.
2. The lens did NOT narrow: 32.3 findings/pass against 31.5, despite "and nothing outside them".
   It RE-LABELLED — serious share 42% → 69% — which is exactly what
   [#449](http://localhost:1060/l/default/item/449) measured when calibration prose was added.
   A lensed pass costs a reader as much as a general one and rates higher.
3. Its Q1 produces confident false positives: "declares `requires(X)` but the code never checks
   X" on endpoints where the KERNEL checks X (core ≥ 0.1.49) — the lens text says "or by the
   kernel's floor" and the model does not believe it. Those would arrive in a queue rated `major`.
4. It is 1.23× the general pass's wall clock, serially, for one extra question set.

So: a lens to SELECT for a file that carries authority — a door, a registry, a store's endpoint
table — and never one to arm on the trigger. That is [#455](http://localhost:1060/l/default/item/455)'s
design ("lenses must be selected, not run exhaustively"), and it is what the product change
does: a `lens` argument with `one_of` over the lenses that exist, the tag `review-v5+intent@…`
keeping `review-v5@…` byte-identical for the lens-less pass.

## What the projection variant would need

Both near-misses and both Q1 misses have the same shape: the model asks the right question of
the line it can see and cannot complete the argument because the other half — the constant's
value, the sibling declaration, the enforcement site — is in another 16 KiB region. #455's
projection for this lens is the endpoint's `describe()`/`ActionSpec` surface beside its
`invoke()`, plus the capability constants they name, as ONE input. For `llm` that is ~60 lines
out of 88 KB; for `ledger` it is `read_scopes` + `read_action` + `ItemEndpoint::describe()` +
the sub-requests it makes. That projection needs a structural pass over the file
([#451](http://localhost:1060/l/default/item/451)'s outline) or the manifold itself: the
declared half is already a Turtle graph at `urn:kernel:actions`; the enforced half is the one
that needs the code.

## Running it again

    cargo build --example review-probe
    M=qwen3-coder:30b-a3b-q8_0
    F="gonk/src/doors.rs vocab/crates/ikigai-vocab/src/lib.rs store/src/endpoints.rs \
       time/crates/ikigai-time/src/lib.rs llm/src/lib.rs ledger/src/endpoints.rs"
    ./target/debug/examples/review-probe --model $M tests/corpus/intent <out>/general 2 $F
    ./target/debug/examples/review-probe --model $M --lens intent tests/corpus/intent <out>/intent 2 $F
    python3 tests/corpus/intent/score.py --arm general=<out>/general --arm intent=<out>/intent

Then read the per-entry listings against `corpus.json`'s `finding` text. The `CANDIDATES` list at
the end is only the findings anchored near a ground-truth anchor; a correct finding can anchor on
a doc comment thirty lines away, so read the whole listing for each entry before calling a miss.
