# ikigai-browse

Repository browsing as [ikigai](https://github.com/ikigai-rs) resources — the
foundation of the repository-browsing family. A host mounts
[`space`](src/lib.rs) over a set of named **roots** (`(name, directory)`
pairs), and each root answers three resource families:

| resource | what it is |
|----------|------------|
| `urn:repo:{repo}:tree` / `urn:repo:{repo}:tree:{path}` | a directory listing — `text/plain` (default; `name`⇥`kind`⇥`size` per line), `as=text/html` (htmx-navigable), `as=text/turtle` (the skolemized graph) |
| `urn:repo:{repo}:file:{path}` | file content — raw bytes under an extension-mapped media type; `as=text/html` for a syntax-highlighted, line-numbered view with `#L{n}` anchors; `annotations=include` (S3, store mounted) serves the text plus a compact, drift-reconciled margin-notes section — content and human annotations in one resolution |
| `urn:repo:{repo}:state` | the **freshness oracle** — HEAD sha + `clean`/`dirty:{n}` on one line; `as=application/json` for `{head, dirty: [paths]}` |
| `urn:repo:{repo}:hash[:{path}]` | the **content hash** (S1) — `sha256:{hex}` of a file's bytes, or the **merkle** construction over a directory's entries (ignore-filtered), so one edit re-keys exactly the path to the root |
| `urn:repo:{repo}:explain[:{path}]` | an **LLM-derived orientation explanation** (S1), archived by `(path, content-hash, version-tag)` — derived once per content version, reused forever; `as=application/json` adds `{content_hash, version_tag, derived}`, `as=text/html` the page face with provenance, `as=text/turtle` the archive entry's graph; `version=` addresses an older tag; `annotations=include` (S3) folds the target's annotations in — the json face gains an `annotations` array, the text face appends margin notes, and a directory rollup folds its subtree's |
| `urn:repo:{repo}:explain-versions[:{path}]` | what the archive holds for a path — one row per entry (tag, hash, model, derived-at), across content versions and tags; a pure store read (no net capability) |
| `urn:annotation[:{id}]` | a **W3C Web Annotation** (S2) on a file — Sink creates/updates (anchoring the quoted text; the bare `urn:annotation` mints a uuid id), Source reads with drift **re-anchoring**, Delete removes; faces: `text/plain` (the body), `as=application/json`, `as=text/turtle` |
| `urn:repo:{repo}:annotations[:{path}]` | every annotation on one file (or the whole repo, path omitted) in reading order, drift-reconciled on each read; faces: `application/json` (default), `as=text/html` (panel fragment), `as=text/turtle` |

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
the host's adapter maps `/k/<command>` onto its engine. Highlighting uses
[two-face](https://crates.io/crates/two-face)'s extended syntax set (~100
formats the stock syntect set misses — TOML, TypeScript, Dockerfile, …) under
the pure-Rust fancy-regex engine, plus an embedded house Turtle/TriG
definition ([assets/](assets/)) so the graph faces highlight too; unknown
formats degrade to escaped plain text, and extensionless well-known names
(`Dockerfile`) match by file name. File views wrap each
line in `<span id="L{n}">` with a self-linking gutter number, so `#L42`
deep-links a line — the anchor surface annotations target. With the
annotation store mounted, the file view marks annotated lines
(`browse-line-annotated`) and appends an annotations panel: one card per
annotation at its `#L{n}` anchor (orphans visually flagged) plus a create
form that `hx-post`s a Sink of `urn:annotation` through the host's `/k/`
adapter (form fields become sink args — htmx only, no scripts).

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

**Capabilities.** Per-verb `ActionSpec`s: Source requires
`urn:cap:browse:read:*` (checked against the annotation's root, like every
browse read); Sink and Delete require `urn:cap:annotate`; Sink also declares
the browse wildcard because anchoring sources the target through the kernel —
a capability that cannot read a file cannot annotate it.

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
and `dcterms:created`.

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
