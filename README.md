# ikigai-browse

Repository browsing as [ikigai](https://github.com/ikigai-rs) resources — the
foundation of the repository-browsing family. A host mounts
[`space`](src/lib.rs) over a set of named **roots** (`(name, directory)`
pairs), and each root answers three resource families:

| resource | what it is |
|----------|------------|
| `urn:repo:{repo}:tree` / `urn:repo:{repo}:tree:{path}` | a directory listing — `text/plain` (default; `name`⇥`kind`⇥`size` per line), `as=text/html` (htmx-navigable; with the explanation family mounted, an explain link for the directory and one per entry), `as=text/turtle` (the skolemized graph) |
| `urn:repo:{repo}:file:{path}` | file content — raw bytes under an extension-mapped media type; `as=text/html` for a syntax-highlighted, line-numbered view with `#L{n}` anchors, inline markers at annotated lines, and (explanations mounted) an explain link; `annotations=include` (S3, store mounted) serves the text plus a compact, drift-reconciled margin-notes section — content and human annotations in one resolution |
| `urn:repo:{repo}:state` | the **freshness oracle** — HEAD sha + `clean`/`dirty:{n}` on one line; `as=application/json` for `{head, dirty: [paths]}` |
| `urn:repo:{repo}:hash[:{path}]` | the **content hash** (S1) — `sha256:{hex}` of a file's bytes, or the **merkle** construction over a directory's entries (ignore-filtered), so one edit re-keys exactly the path to the root |
| `urn:repo:{repo}:explain[:{path}]` | an **LLM-derived orientation explanation** (S1), archived by `(path, content-hash, version-tag)` — derived once per content version, reused forever; `as=application/json` adds `{content_hash, version_tag, derived}`, `as=text/html` the page face with provenance and a backlink to the explained resource, `as=text/turtle` the archive entry's graph; `version=` addresses an older tag; `annotations=include` (S3) folds the target's annotations in — the json face gains an `annotations` array, the text face appends margin notes, the html face renders the annotation cards, and a directory rollup folds its subtree's |
| `urn:repo:{repo}:explain-versions[:{path}]` | what the archive holds for a path — one row per entry (tag, hash, model, derived-at), across content versions and tags; a pure store read (no net capability) |
| `urn:annotation[:{id}]` | a **W3C Web Annotation** (S2) on a file — Sink creates/updates (anchoring the quoted text; the bare `urn:annotation` mints a uuid id), Source reads with drift **re-anchoring**, Delete removes; faces: `text/plain` (the body), `as=application/json`, `as=text/turtle` |
| `urn:repo:{repo}:annotations[:{path}]` | every annotation on one file (or the whole repo, path omitted) in reading order, drift-reconciled on each read; faces: `application/json` (default), `as=text/html` (panel fragment), `as=text/turtle` |
| `urn:repo:{repo}:review:{path}` | the **machine review pass** (S4) — region-grain LLM commentary minted as real annotations (provenance-distinguished), the pass archived by `(path, content-hash, review-tag)` so re-sourcing unchanged content mints nothing; faces: `text/plain` (the margin digest), `as=application/json` (`{minted, orphaned_items, reviewed_bytes, total_bytes, annotations, …}`), `as=text/html` (the card page), `as=text/turtle` (the pass's provenance graph); `debug=raw` derives and returns the model's **unparsed answer** (nothing minted or archived) — the parse-failure diagnosis face |
| `urn:repo:{repo}:prs` | the root's **pull requests** — ikigai-repo's `urn:repo:pr:list` facade resolved through the kernel with `dir=` the root's directory; `state=` (`open`/`closed`/`merged`/`all`) and `limit=` forward to the facade (ikigai-repo ≥ 0.1.4 — omitted, the facade's defaults apply); `text/plain` (default) is `number`⇥`title`⇥`branch`⇥`updated`⇥`state` per line (empty = no matching PRs), `as=application/json` the facade's structured rows, `as=text/html` the listing with each PR linking its page (`chrome=embed` for the rows-only fragment other faces fold in) |
| `urn:repo:{repo}:pr:{n}` | the **PR page** — metadata (`urn:repo:pr:view` json: author object, `headRefOid`) + the unified diff (`urn:repo:pr:diff`); the DIFF TEXT is an annotation surface (annotations target the PR IRI and quote diff lines, drifting like file annotations); `as=text/html` renders the highlighted, line-anchored diff with markers and the annotations panel; `annotations=include` folds the margin into the plain/json faces |
| `urn:repo:{repo}:pr:{n}:explain` | a **review-shaped PR explanation** — what the change does and what a reviewer would look at — archived by `(repo, pr, headRefOid, version-tag)`: new commits derive fresh, prior entries stay addressable (`version=`) |
| `urn:repo:{repo}:pr:{n}:review` | the **machine review pass over the diff** — findings minted as machine annotations targeting the PR IRI, the pass archived by `(repo, pr, headRefOid, review-tag)` so an unchanged head mints nothing; `reviewed_bytes`/`total_bytes` on the json face say how much of a big diff the model actually saw, and `debug=raw` returns the unparsed answer |

**Resolution is the access model.** A `{repo}` that is not a configured root is
a clean resolution *miss* (the grammar refuses to match; other mounted spaces
may still answer), never an error from here. Paths are **jailed** to their
root: `..` and absolute segments are rejected lexically, and the canonicalized
target must stay inside the canonicalized root, so a symlink cannot escape.
Paths in IRIs are percent-encoded (`hello%20world.txt`); bindings are decoded
before they touch the filesystem.

**Manifold citizenship.** Roots are known at bind time, so the space
enumerates **per-configured-root rows**: for each root, the concrete resources
(`urn:repo:{name}:tree`, `:state`, `:hash`, `:explain`, …) and the
`{path}`-templated ones (`urn:repo:{name}:file:{path}`, …) are separate
entries. The catalog and the capability-scoped action manifold
(`urn:kernel:actions`) therefore advertise exactly the repos an agent can
actually browse, and every templated row survives the kernel's
probe-expansion. `urn:annotation:{id}` is the annotation family's row; the
bare `urn:annotation` (Sink mints a uuid id) stays resolvable but unlisted.

**Capabilities.** Every action declares `urn:cap:browse:read:*` — the wildcard
*offering* form ("holds some grant under this prefix"). A **grant** names
roots: `urn:cap:browse:read:{repo}` grants one root; the literal
`urn:cap:browse:read:*` scope grants them all. Declared = enforced: the kernel
baseline-checks the wildcard before dispatch, and the endpoint checks the
target's root against the grant.

**The explanation archive (S1).** `space_with_explain` takes an
`ExplainConfig` around a host-injected Oxigraph store handle (`Arc<Store>`) —
ONE shared store; later stages (annotations) join it. Explanations are derived
THROUGH the kernel (`urn:repo:…:hash`, `:file`/`:tree`, the children's own
`:explain`, and `urn:llm:{provider}:ask` are all sub-requests) and persist as
skolemized RDF (`ik:Explanation` entries keyed by content hash + version tag).
Directory explanations synthesize their children's — the merkle hash cascade
means one edit re-derives exactly the path to the root, and everything else is
an archive hit. Model tiers are config: file grain defaults to
`urn:llm:coder:ask` (400-token ceiling), rollups to `urn:llm:ask` (600),
`temperature=0.2` — per-call `max_tokens` ceilings are mandatory, and an empty
model answer is an error, never archived. Prompts are type-aware (code /
note / skill-or-agent definition / plain text, by extension + path heuristics)
and individually versioned: a prompt edit bumps its version constant, which
lazily re-derives while old tags stay addressable via `version=`.

**Live, uncacheable reads.** The browsing families are live reads — cheap by
design; the hash is the probe the archive keys on. `ExplainConfig` model
labels (`file_model_label` / `dir_model_label`) should name the real model ids
so a model swap re-keys the archive.

**Non-git roots work.** `state` answers `not a git repository` (JSON:
`{"head": null, "dirty": []}`) while `tree` and `file` are unaffected — future
roots (memory dirs, skills dirs) need not be repositories.

**Native-only by nature** — it reads the roots' filesystem directly and spawns
`git` for the state oracle (an argument vector, never a shell string). It is
to source trees what `ikigai-repo` is to dev tooling. No wasm face.

**The PR family needs the pr facades mounted.** Browse does NOT depend on the
`ikigai-repo` crate — the PR resources resolve `urn:repo:pr:list` / `:view` /
`:diff` THROUGH THE KERNEL at runtime, passing `dir=` so they run in the
root's directory. A composition without those facades answers a typed
`NotFound` naming the gap (the rest of browse is untouched), and the facades
enforce their own capability (`urn:cap:exec:gh`) on dispatch — attenuation
means a caller of the PR rows must hold it too.

```rust
use ikigai_core::Kernel;
use std::sync::Arc;

let kernel = Kernel::new(Arc::new(ikigai_browse::space([
    ("core".to_string(), "/path/to/ikigai-core".into()),
    ("cli".to_string(), "/path/to/ikigai-cli".into()),
])));
// source urn:repo:core:tree            (under a urn:cap:browse:read:core grant)
// source urn:repo:core:file:src/lib.rs as=text/html
// source urn:repo:core:state as=application/json
```

Run `cargo run --example browse-demo` to watch it browse its own repository,
and `cargo run --example explain-demo` (needs a local Ollama with
`qwen3-coder:30b` and `llama3.3` pulled) to watch it explain itself with real
models — the second pass serves every explanation from the archive in
milliseconds.

## The HTML face (house style)

The `text/html` faces are htmx **fragments**, not pages — ikigai-runbook's
server-driven house style. Entries and breadcrumbs `hx-get`
`/k/source <iri> as=text/html` into a `#browse` container the host provides;
the host's adapter maps `/k/<command>` onto its engine.

**The host contract, in full.** Beyond `#browse` and the `/k/` adapter, every
crumb strip opens with a **home affordance**: `<a class="browse-home-link"
href="/">⌂</a>`. It is a plain anchor to the host's index — in ikigai-web `/`
is the index so it works untouched; any other host either styles/rebinds
`.browse-home-link` (its page, its rules — e.g. `hx-boost`, or rewriting the
`href`) or ships it harmlessly unstyled. The PR pages carry a real ancestor
trail (repo → `prs` → `#n` → `explain`/`review`), every ancestor a live crumb.
The **root tree** additionally renders a lazy *recent pull requests* block
(`browse-recent-prs`): a `div` with `hx-get="/k/source urn:repo:{repo}:prs
state=all limit=10 chrome=embed as=text/html"` and `hx-trigger="load"`,
swapping into itself. The tree face itself never consults the pr facades — it
renders instantly, and when the facades are not mounted the lazy fetch answers
the typed 404-with-guidance, which the host renders per its own error
handling (a host that shows kernel errors inline needs nothing extra). Highlighting uses
[two-face](https://crates.io/crates/two-face)'s extended syntax set (~100
formats the stock syntect set misses — TOML, TypeScript, Dockerfile, …) under
the pure-Rust fancy-regex engine, plus an embedded house Turtle/TriG
definition ([assets/](assets/)) so the graph faces highlight too; unknown
formats degrade to escaped plain text, and extensionless well-known names
(`Dockerfile`) match by file name. File views wrap each
line in `<span id="L{n}">` with a self-linking gutter number, so `#L42`
deep-links a line — the anchor surface annotations target. With the
annotation store mounted, the file view marks annotated lines
(`browse-line-annotated`), renders an inline marker per anchored annotation
between the gutter number and the code (`browse-annotation-marker` — an
anchor down to the annotation's card whose native `title` tooltip reveals
the note; hosts may style it as a margin dot), and appends an annotations
panel: one card per annotation at its `#L{n}` anchor (orphans visually
flagged, listed without a marker) plus a create form that `hx-post`s a Sink
of `urn:annotation` through the host's `/k/` adapter (form fields become
sink args — htmx only, no scripts). With the explanation family mounted,
the tree and file faces carry explain links (`browse-explain-link` — the
tree face one per entry plus the directory's own under a
`browse-actions` nav), and the explain face backlinks its target
(`browse-view-link`) and folds the annotation cards in under
`annotations=include`.

## Annotations (S2)

`space_with_annotations(roots, store)` mounts W3C Web Annotations over the
same host-injected Oxigraph store the explanation archive uses
(`space_with_explain` includes both families — ONE shared graph, queryable
together). The shape is skolemized `oa:` — stable IRIs, no blank nodes, and
the W3C target node flattened to `ik:annotates` — with BOTH selector kinds
stored per annotation: `oa:TextQuoteSelector` (`oa:prefix`/`oa:exact`/
`oa:suffix`, context derived from the anchored occurrence) and
`oa:TextPositionSelector` (`oa:start`/`oa:end`, character offsets), keyed to
the annotated content version by `ik:contentHash`.

**Re-anchoring under drift.** Every read reconciles each annotation against
the target's current content: hash unchanged → served as stored; content
moved → the quote is re-searched (context-scored, first match wins ties)
and BOTH selectors plus the recorded hash update in place (`ik:reanchored
true`); quote gone → `ik:orphaned true`, still rendered and flagged, never
silently dropped — and a later read that finds the quote again (an edit
reverted) heals it. The store is only written when something changed.

**Diff targets anchor marker-tolerantly.** A PR target's surface is its
unified diff, and quotes — model or human — name the CODE, not the diff's
leading `+`/`-`/space column. Anchoring against a diff therefore tries, in
order, first hit wins: **(1)** the quote exactly as given, anywhere in the
raw diff (context-scored — a marker-faithful quote anchors to its precise
span); **(2)** the quote in the **marker-stripped shadow** of the diff (every
line's leading marker removed — a quote of consecutive code lines matches
across the interleaved markers); **(3)** the quote with its own single
leading marker removed (a wrong or stale marker); **(4)** that stripped quote
whitespace-trimmed (padded markers, dropped indentation). A stage-2/3/4 hit
anchors the whole original diff line(s) and stores THAT original text as
`oa:exact` — drift keeps comparing real diff content, never the stripped
fiction the match was found through. The same discipline runs at Sink time,
at review-mint time, and on every drift pass (so a `+` line that settles into
context on a later head is followed, its stored exact rewritten to the new
line).

**Capabilities.** Per-verb `ActionSpec`s: Source requires
`urn:cap:browse:read:*` (checked against the annotation's root, like every
browse read); Sink and Delete require `urn:cap:annotate`; Sink also declares
the browse wildcard because anchoring sources the target through the kernel —
a capability that cannot read a file cannot annotate it.

## The machine review pass (S4)

`urn:repo:{repo}:review:{path}` (mounted by `space_with_explain`) is the
review layer: Source asks the review model for findings — each an **exact
quote** from the file plus a reviewer's note — anchors every quote, and mints
each anchored finding as a real `urn:annotation:` through the same machinery
human notes use. Machine and human annotations live on ONE queryable axis,
distinguished only by provenance (all standard terms — no vocab publish):

- `dcterms:creator` — the model identity (its presence IS the machine
  discriminator; the JSON rows also carry a `machine` boolean).
- `oa:motivatedBy` — `oa:assessing` on review findings; the human Sink stamps
  `oa:commenting` (absent on pre-S4 stores — read compatibility).
- `prov:wasGeneratedBy` — the pass entry that minted the finding; the pass
  records the inverse as `prov:generated` and the reviewed file as
  `prov:used`.

The pass is archived like an explanation — keyed
`(path, content-hash, review-v{N}@model)`, the minted IRIs recorded in the
entry — so **re-sourcing unchanged content is an archive hit that mints
nothing**. Changed content is a fresh pass, and the earlier pass's annotations
re-anchor or orphan exactly like human ones: the drift is the review-history
story, kept visible. A finding whose quote does not anchor (the model
misquoted) mints nothing and is counted (`orphaned_items`), never fatal; a
pass in which nothing parses or nothing anchors is an error and is NOT
archived (an empty pass must not poison a key that would never re-derive) —
the parse-failure error carries the raw answer's opening, and `debug=raw`
re-sources the resource into the model's full **unparsed** answer (nothing
minted, nothing archived) so a collapsed answer is inspectable live.
The PR pass (`pr:{n}:review`, prompt `pr-review-v3`) tells the model to quote
diff lines *including* their leading `+`/`-`/space marker AND anchors with
the marker-tolerant diff discipline (see S2) — belt and suspenders: a model
that ignores the instruction still anchors, and a marker-faithful quote
anchors precisely. Both review prompts (v2 file / v3 PR) restate the format
contract *after* the content: on a big input, a contract stated only up top
loses to the content and the model answers label-free (measured — the last
words the model reads must be the format).
Inputs larger than `max_prompt_bytes` (default 16 KiB) are truncated on the
prompt side only — quotes anchor against the whole surface — and the pass
says so honestly: `reviewed_bytes`/`total_bytes` on the json face and the
archive entry, a `(input truncated)` notice on the text and html faces.
Faces render the kinds distinguishably: hollow line markers (`○`,
`browse-annotation-marker-machine`) against the solid human dot, a
`review by {model}` identity line on machine cards
(`browse-annotation-machine`), and a `[review:{model}]` label in margin text.

The review action `requires` all three of `urn:cap:browse:read:*`,
`urn:cap:net:*`, and `urn:cap:annotate` — it reads, asks a model, and writes.
Knobs on `ExplainConfig`: `review_provider` (default `urn:llm:coder:ask`),
`review_max_tokens` (default 800), `review_model_label` (the tag override,
same precedence as the explain labels).

Because findings are ordinary annotations in the shared graph, one SPARQL axis
answers review questions directly, e.g. every machine finding still anchored
in the current content:

```sparql
PREFIX oa: <http://www.w3.org/ns/oa#>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX ik: <https://ikigai-rs.dev/ns#>
SELECT ?file ?quote ?note ?model WHERE {
  ?a a oa:Annotation ; dcterms:creator ?model ;
     ik:annotates ?file ; oa:bodyValue ?note ;
     oa:hasSelector [ oa:exact ?quote ] .
  FILTER NOT EXISTS { ?a ik:orphaned true }
}
```

— or only the human notes: `FILTER NOT EXISTS { ?a dcterms:creator ?m }`.

## Vocabulary (Turtle face)

The graph face skolemizes everything under the same `urn:repo:…` IRIs that
resolve — directory children as `tree:` IRIs, files as `file:` IRIs — so the
graph is diffable, SPARQL-able, *and navigable*. It uses these `ik:`
(`https://ikigai-rs.dev/ns#`) terms: `ik:Directory`, `ik:File`, `ik:Symlink`,
`ik:Explanation` (classes), `ik:entry`, `ik:fileName`, `ik:path`, `ik:repo`,
`ik:byteSize`, `ik:about` (an explanation's subject), `ik:annotates` (an
annotation's target), `ik:contentHash`, `ik:versionTag`, `ik:model`,
`ik:promptKind`, `ik:explanation`, `ik:derivedAt`, and (S2) `ik:reanchored`,
`ik:orphaned` (properties). These are pending addition to the published
vocabulary (`ik:model` already exists). Before 0.2.2 both families wrote
`ik:target` for the subject/target link; that term belongs to the inbound-HTTP
routing family, so browse retired it. Stores written by older versions read
fine — both loaders and the versions listing accept the legacy predicate, all
new writes (and any annotation rewrite) use the new terms, and lingering
`ik:target` triples in an old archive are harmless. The annotation graphs additionally
use the external `oa:` (`http://www.w3.org/ns/oa#`) terms `oa:Annotation`,
`oa:TextQuoteSelector`, `oa:TextPositionSelector`, `oa:bodyValue`,
`oa:hasSelector`, `oa:prefix`, `oa:exact`, `oa:suffix`, `oa:start`, `oa:end`,
and `dcterms:created`. The S4 review layer adds the standard provenance terms
`dcterms:creator`, `oa:motivatedBy` (`oa:assessing` / `oa:commenting`), and
`prov:` (`http://www.w3.org/ns/prov#`) `prov:wasGeneratedBy` /
`prov:generated` / `prov:used`, plus two `ik:` terms pending addition to the
published vocabulary: `ik:Review` (the pass-entry class) and
`ik:orphanedItems` (the count of findings whose quotes did not anchor).

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
