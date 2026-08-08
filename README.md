# ikigai-browse

Repository browsing as [ikigai](https://github.com/ikigai-rs) resources — the
foundation of the repository-browsing family. A host mounts
[`space`](src/lib.rs) over a set of named **roots** (`(name, directory)`
pairs), and each root answers three resource families:

| resource | what it is |
|----------|------------|
| `urn:repo:{repo}:tree` / `urn:repo:{repo}:tree:{path}` | a directory listing — `text/plain` (default; `name`⇥`kind`⇥`size` per line), `as=text/html` (htmx-navigable), `as=text/turtle` (the skolemized graph) |
| `urn:repo:{repo}:file:{path}` | file content — raw bytes under an extension-mapped media type; `as=text/html` for a syntax-highlighted, line-numbered view with `#L{n}` anchors |
| `urn:repo:{repo}:state` | the **freshness oracle** — HEAD sha + `clean`/`dirty:{n}` on one line; `as=application/json` for `{head, dirty: [paths]}` |

**Resolution is the access model.** A `{repo}` that is not a configured root is
a clean resolution *miss* (the grammar refuses to match; other mounted spaces
may still answer), never an error from here. Paths are **jailed** to their
root: `..` and absolute segments are rejected lexically, and the canonicalized
target must stay inside the canonicalized root, so a symlink cannot escape.
Paths in IRIs are percent-encoded (`hello%20world.txt`); bindings are decoded
before they touch the filesystem.

**Capabilities.** Every action declares `urn:cap:browse:read:*` — the wildcard
*offering* form ("holds some grant under this prefix"). A **grant** names
roots: `urn:cap:browse:read:{repo}` grants one root; the literal
`urn:cap:browse:read:*` scope grants them all. Declared = enforced: the kernel
baseline-checks the wildcard before dispatch, and the endpoint checks the
target's root against the grant.

**Live, uncacheable.** All three families are live reads — cheap by design.
The caching economics arrive in S1 (explanations archived by content hash,
keyed on `state`); S0 does not fake freshness.

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

Run `cargo run --example browse-demo` to watch it browse its own repository.

## The HTML face (house style)

The `text/html` faces are htmx **fragments**, not pages — ikigai-runbook's
server-driven house style. Entries and breadcrumbs `hx-get`
`/k/source <iri> as=text/html` into a `#browse` container the host provides;
the host's adapter maps `/k/<command>` onto its engine. File views wrap each
line in `<span id="L{n}">` with a self-linking gutter number, so `#L42`
deep-links a line — the anchor surface later annotation stages target.

## Vocabulary (Turtle face)

The graph face skolemizes everything under the same `urn:repo:…` IRIs that
resolve — directory children as `tree:` IRIs, files as `file:` IRIs — so the
graph is diffable, SPARQL-able, *and navigable*. It uses these `ik:`
(`https://ikigai-rs.dev/ns#`) terms: `ik:Directory`, `ik:File`, `ik:Symlink`
(classes), `ik:entry`, `ik:fileName`, `ik:path`, `ik:repo`, `ik:byteSize`
(properties). These are pending addition to the published vocabulary.

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
