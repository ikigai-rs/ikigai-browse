//! `ikigai-browse` — repository browsing as ikigai resources.
//!
//! A standalone **ikigai module crate** (like `ikigai-fs` / `ikigai-repo`): a
//! host links it in and mounts [`space`] (or [`space_with_explain`]) over a
//! set of named **roots** — `(name, directory)` pairs. Each root then answers
//! these resource families:
//!
//! - `urn:repo:{repo}:tree` / `urn:repo:{repo}:tree:{path}` — a directory
//!   listing. Faces: `text/plain` (default; one `name<TAB>kind<TAB>size` entry
//!   per line), `as=text/html` (an htmx-navigable fragment — entries `hx-get`
//!   the child tree/file resources into `#browse`, ikigai-runbook's
//!   server-driven house style), `as=text/turtle` (the skolemized graph:
//!   `ik:Directory` / `ik:File` nodes under stable `urn:repo:…` IRIs, no blank
//!   nodes).
//! - `urn:repo:{repo}:file:{path}` — file content. Raw bytes by default under
//!   an extension-mapped media type (`application/octet-stream` fallback, with
//!   a UTF-8 sniff to `text/plain`); `as=text/html` renders a
//!   syntax-highlighted, line-numbered view whose lines carry `id="L{n}"`
//!   anchors (the surface S2's annotations target), inline markers at
//!   annotated lines, and — explanations mounted — an explain link.
//! - `urn:repo:{repo}:state` — the **freshness oracle**: the git HEAD sha plus
//!   a short-status digest, one line; `as=application/json` yields
//!   `{head, dirty: [paths]}`. Uncacheable by design — it exists to be the
//!   cheap "has anything changed?" probe that later stages key caches on.
//! - `urn:repo:{repo}:hash[:{path}]` — the **content hash** (S1): sha-256 of a
//!   file's bytes, or the merkle construction over a directory's entries, so
//!   one edit re-keys exactly the path to the root.
//! - `urn:repo:{repo}:explain[:{path}]` + `:explain-versions[:{path}]` — the
//!   S1 **explanation archive** ([`space_with_explain`]): LLM-derived
//!   orientation, derived once per `(path, content-hash, version-tag)` and
//!   persisted in a host-injected Oxigraph store.
//! - `urn:annotation:{id}` + `urn:repo:{repo}:annotations[:{path}]` — S2 **Web
//!   Annotations** (`oa:`) on files ([`space_with_annotations`], and included
//!   by [`space_with_explain`] — one shared store): quote + position selectors
//!   that RE-ANCHOR when the target drifts and are orphan-flagged (never
//!   dropped) when the quote is gone. The file HTML face gains an annotations
//!   panel and marks annotated lines.
//! - `urn:repo:{repo}:review:{path}` — the S4 **machine review pass**
//!   ([`space_with_explain`]): region-grain LLM commentary minted as real
//!   annotations (provenance-distinguished — `dcterms:creator`,
//!   `oa:motivatedBy oa:assessing`, `prov:wasGeneratedBy`), the pass archived
//!   by `(path, content-hash, review-tag)` so re-sourcing unchanged content
//!   mints nothing.
//! - `urn:repo:{repo}:prs` + `urn:repo:{repo}:pr:{n}` (and, explanations
//!   mounted, `…:pr:{n}:explain` / `…:pr:{n}:review`) — the **pull-request
//!   family**: ikigai-repo's pr facades resolved THROUGH THE KERNEL at
//!   runtime (`dir=` the root's directory; no crate dependency, and a typed
//!   NotFound when they are not mounted). The PR page's DIFF is an annotation
//!   surface — annotations target the PR IRI and drift like file ones — and
//!   the derived layers archive by the HEAD COMMIT (`headRefOid`), so new
//!   commits re-derive and prior entries stay addressable.
//! - `urn:repo:style` — the **theme stylesheet** the classed highlight faces
//!   bind to (`text/css`, root-independent, cacheable): each configured theme
//!   inside its own `@media (prefers-color-scheme: …)` block, all targeting the
//!   `hl-` classes the HTML faces emit. The themes and the contrast floor come
//!   from the layered `a11y.toml` via `ikigai-a11y` (see [`Mount::app`]), and
//!   every colour below that floor is lifted to its own theme's default
//!   foreground — repaired in the theme's palette, nothing invented.
//! - `urn:repo:{repo}:prs:{path}` — the **contextual** listing: the PRs that
//!   touched anything at or under a path, newest first — open PRs by
//!   intersecting their changed files (`urn:repo:pr:files`), merged PRs mined
//!   from the path-scoped commit log (`urn:repo:log path=`) by the
//!   squash-merge `(#N)` subject convention. Subdirectory tree pages lazy-load
//!   THEIR listing; the root keeps the repo-wide block.
//!
//! ## Resolution is the access model
//!
//! A `{repo}` that is not a configured root is a **clean miss** — no bound
//! grammar mentions it, so resolution falls through to whatever else is
//! mounted rather than erroring here. Paths are **jailed** to their root:
//! `..` and absolute segments are rejected lexically, and the canonicalized
//! target must stay inside the canonicalized root, so a symlink cannot escape.
//!
//! ## Manifold citizenship
//!
//! Roots are known at bind time, so the space enumerates **per-configured-root
//! rows**: for each root, the concrete resources (`urn:repo:{name}:tree`,
//! `:state`, `:hash`, …) and the `{path}`-templated ones
//! (`urn:repo:{name}:file:{path}`, …) are separate entries — the catalog and
//! the capability-scoped action manifold (`urn:kernel:actions`) advertise
//! exactly the repos an agent can actually browse, and every templated row
//! survives the kernel's probe-expansion (a `{repo}` template cannot: no
//! placeholder names a configured root). The bare `urn:annotation` (Sink mints
//! an id) stays resolvable but unlisted; `urn:annotation:{id}` is its row.
//!
//! ## Capabilities
//!
//! Every action requires `urn:cap:browse:read:*` (the wildcard **offering**
//! form: "holds some grant under this prefix"). A **grant** names roots —
//! `urn:cap:browse:read:{repo}` — and enforcement checks the target's root is
//! granted; the literal `urn:cap:browse:read:*` scope is an all-roots grant.
//! Declared = enforced: the kernel baseline-checks the wildcard before
//! dispatch, and the per-root check here does the rest.
//!
//! ## Platforms
//!
//! Native-only by design: it reads the filesystem directly (the roots are the
//! platform data it fronts, like `ikigai-repo`) and shells out to `git` for
//! the state oracle (argument vector, never a shell string). No wasm face.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use ikigai_core::{
    ArgSpec, Bindings, Description, Endpoint, EndpointSpace, Error, FnEndpoint, Grammar,
    Invocation, Iri, ReprType, Representation, Result, UriTemplate, Verb,
};
use oxigraph::store::Store;
use syntect::html::{css_for_theme_with_class_style, line_tokens_to_classed_spans, ClassStyle};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxDefinition, SyntaxSet};
use syntect::util::LinesWithEndings;

mod annotate;
mod explain;
mod hash;
mod pr;
mod review;

pub use annotate::CAP_ANNOTATE;
pub use explain::ExplainConfig;

/// The wildcard capability every browse action declares: an agent is offered
/// these resources iff it holds *some* grant under this prefix. Held literally,
/// it is an all-roots grant.
pub const CAP_WILDCARD: &str = "urn:cap:browse:read:*";

/// The grant prefix: `urn:cap:browse:read:{repo}` grants one configured root.
pub const CAP_PREFIX: &str = "urn:cap:browse:read:";

pub(crate) type Roots = Arc<BTreeMap<String, PathBuf>>;

/// Mount the browse module over `roots` — `(name, directory)` pairs.
///
/// Binds `urn:repo:{repo}:tree[:{path}]`, `urn:repo:{repo}:file:{path}`, and
/// `urn:repo:{repo}:state`, where `{repo}` only ever matches a configured root
/// name (anything else is a resolution miss, not an error). Roots need not be
/// git repositories: `state` reports "not a git repository" while `tree` and
/// `file` work unchanged.
///
/// # Panics
///
/// Fails loud at mount time on a misconfiguration: an empty root name, a name
/// containing `:` or `/` (it must embed cleanly in the URN grammar), or a
/// duplicate name.
pub fn space(roots: impl IntoIterator<Item = (String, PathBuf)>) -> EndpointSpace {
    Mount::new(roots).space()
}

/// [`space`] plus the S2 **annotation** family over a host-injected Oxigraph
/// store — no LLM anywhere: `urn:annotation:{id}` (Sink creates/updates,
/// Source reads with drift re-anchoring, Delete removes) and
/// `urn:repo:{repo}:annotations[:{path}]` (the per-target listing), and the
/// file HTML face gains its annotations panel. [`space_with_explain`] includes
/// this family too, over the SAME store as the explanation archive.
pub fn space_with_annotations(
    roots: impl IntoIterator<Item = (String, PathBuf)>,
    store: Arc<Store>,
) -> EndpointSpace {
    Mount::new(roots).annotations(store).space()
}

/// [`space`] plus the S1 **explanation** family: `urn:repo:{repo}:explain[:{path}]`
/// (LLM-derived orientation, archived per content version in the injected
/// Oxigraph store) and `urn:repo:{repo}:explain-versions[:{path}]` (what the
/// archive holds for a path). See [`ExplainConfig`] for the knobs: providers,
/// token ceilings, model labels, and the ignore policy (which also governs the
/// directory hashes the archive keys on). The S2 **annotation** family rides
/// along over the same shared store ([`space_with_annotations`] gets it
/// without the LLM machinery).
pub fn space_with_explain(
    roots: impl IntoIterator<Item = (String, PathBuf)>,
    config: ExplainConfig,
) -> EndpointSpace {
    Mount::new(roots).explain(config).space()
}

/// A browse mount under construction — the seam that carries what the module
/// needs from the HOST rather than from a request.
///
/// The three `space*` functions are this builder with one knob set, kept because
/// they are what every existing host calls. Reach for the builder when a mount
/// needs a combination they do not spell — today that means [`Mount::app`],
/// which no positional constructor could take without changing three
/// signatures at once.
///
/// ```no_run
/// # use std::path::PathBuf;
/// let space = ikigai_browse::Mount::new([("core".to_string(), PathBuf::from("/src/core"))])
///     .app("dev-server")
///     .space();
/// ```
pub struct Mount {
    roots: Roots,
    app: Option<String>,
    store: Option<Arc<Store>>,
    explain: Option<ExplainConfig>,
}

impl Mount {
    /// A mount over `roots` — `(name, directory)` pairs, validated as [`space`]
    /// validates them.
    pub fn new(roots: impl IntoIterator<Item = (String, PathBuf)>) -> Self {
        Mount {
            roots: build_roots(roots),
            app: None,
            store: None,
            explain: None,
        }
    }

    /// The **process's** name — `dev-server`, `web`, `cms-web` — which selects
    /// the `{app}.a11y.toml` layer `urn:repo:style` reads its themes and its
    /// contrast floor from.
    ///
    /// It is the host's name, not this crate's: `ikigai-browse` is a library,
    /// and the operator writing `dev-server.a11y.toml` is configuring the front
    /// end they are looking at, not a module linked into it. A host that says
    /// nothing gets the shared `a11y.toml` only, which is why this is optional
    /// rather than required — a mount that names no app is under-configured,
    /// never wrong.
    pub fn app(mut self, app: impl Into<String>) -> Self {
        self.app = Some(app.into());
        self
    }

    /// Mount the S2 **annotation** family over a host-injected Oxigraph store.
    /// See [`space_with_annotations`].
    pub fn annotations(mut self, store: Arc<Store>) -> Self {
        self.store = Some(store);
        self
    }

    /// Mount the S1 **explanation** family (and the S4 review pass, and the
    /// derived pull-request layers) — the store rides in the config. See
    /// [`space_with_explain`].
    pub fn explain(mut self, config: ExplainConfig) -> Self {
        self.store = Some(Arc::clone(&config.store));
        self.explain = Some(config);
        self
    }

    /// Build the space.
    pub fn space(self) -> EndpointSpace {
        let Mount {
            roots,
            app,
            store,
            explain,
        } = self;
        let app = app.as_deref();
        let Some(config) = explain else {
            let ignore = Arc::new(hash::default_ignore());
            let space = base_space(&roots, &ignore, store.as_ref(), false, app);
            return match store {
                Some(store) => annotate::bind(space, &roots, &store),
                None => space,
            };
        };
        let ignore = Arc::new(config.ignore.clone());
        let store = Arc::clone(&config.store);
        let shared = Arc::new(config.clone());
        let space = base_space(&roots, &ignore, Some(&store), true, app);
        let space = explain::bind(space, &roots, config);
        // The S4 review pass (machine-minted annotations) rides with the
        // explanation family: it needs the same LLM seam and the same store.
        let space = review::bind(space, &roots, &shared);
        // So do the pull-request derived layers (pr:{n}:explain / pr:{n}:review).
        let space = pr::bind_explain(space, &roots, &shared);
        annotate::bind(space, &roots, &store)
    }
}

fn build_roots(roots: impl IntoIterator<Item = (String, PathBuf)>) -> Roots {
    let mut map = BTreeMap::new();
    for (name, dir) in roots {
        // No `:`/`/` (the name must embed cleanly in the URN grammar) and no
        // `{`/`}` (it is spliced literally into per-root URI templates).
        assert!(
            !name.is_empty() && !name.contains([':', '/', '{', '}']),
            "browse: root name `{name}` must be non-empty and contain no `:`, `/`, `{{`, or `}}`"
        );
        assert!(
            map.insert(name.clone(), dir).is_none(),
            "browse: duplicate root name `{name}`"
        );
    }
    Arc::new(map)
}

/// The families S0 shipped plus the S1 content-hash oracle, over shared roots.
/// `store` (present when the host mounted the annotation/explanation store)
/// lets the file HTML face render its annotations overlay; `explain` (the
/// explanation family is mounted) lets the tree and file HTML faces render
/// their explain affordances — on a plain mount the link would dangle (no
/// bound grammar answers it), so it is simply not rendered.
fn base_space(
    roots: &Roots,
    ignore: &Arc<BTreeSet<String>>,
    store: Option<&Arc<Store>>,
    explain: bool,
    app: Option<&str>,
) -> EndpointSpace {
    let tree: Arc<dyn Endpoint> = Arc::new(tree_endpoint(roots, explain));
    let file: Arc<dyn Endpoint> = Arc::new(file_endpoint(roots, store, explain));
    let state: Arc<dyn Endpoint> = Arc::new(state_endpoint(roots));
    let hash: Arc<dyn Endpoint> = Arc::new(hash::hash_endpoint(roots, ignore));
    let space = EndpointSpace::new();
    let space = bind_family(space, roots, tree, Some("tree"), Some("tree:{path}"));
    let space = bind_family(space, roots, file, None, Some("file:{path}"));
    let space = bind_family(space, roots, state, Some("state"), None);
    let space = bind_family(space, roots, hash, Some("hash"), Some("hash:{path}"));
    // The theme stylesheet: one concrete row shared by all roots.
    let space = space.bind(StyleRow, style_endpoint(app));
    // The pull-request pages ride with every variant: they need no store and
    // no LLM — only ikigai-repo's pr facades resolved through the kernel at
    // runtime (unmounted facades answer a typed NotFound, not a panic).
    pr::bind_pages(space, roots, store, explain)
}

// --- grammar ----------------------------------------------------------------

/// One manifold row for one configured root: a concrete IRI
/// (`urn:repo:{name}:tree`) or a URI template whose `{repo}` is already fixed
/// (`urn:repo:{name}:tree:{path}`). `pattern()` IS the row the manifold
/// advertises, so a concrete row resolves directly and a templated row
/// survives the catalog's **probe-expansion** (core expands each `{var}` with
/// a placeholder and the expansion must match the grammar that owns the row —
/// see `ikigai-core`'s `describe_entry`). The earlier `{repo}`-templated
/// grammar could not: no placeholder names a configured root, so every browse
/// row was invisible to every manifold. The root name is injected as the
/// `repo` binding either way, so endpoints keep reading `bindings["repo"]`,
/// and an unconfigured repo stays a clean resolution MISS — no bound grammar
/// mentions it at all.
pub(crate) struct RootRow {
    repo: String,
    matcher: RowMatcher,
}

enum RowMatcher {
    Exact(String),
    Template(UriTemplate),
}

impl RootRow {
    fn exact(repo: &str, iri: String) -> Self {
        RootRow {
            repo: repo.to_string(),
            matcher: RowMatcher::Exact(iri),
        }
    }

    fn template(repo: &str, template: &str) -> Self {
        RootRow {
            repo: repo.to_string(),
            matcher: RowMatcher::Template(
                UriTemplate::parse(template).expect("browse templates are valid"),
            ),
        }
    }
}

impl Grammar for RootRow {
    fn match_iri(&self, iri: &Iri) -> Option<Bindings> {
        let mut bindings = match &self.matcher {
            RowMatcher::Exact(exact) => (iri.as_str() == exact).then(Bindings::new)?,
            RowMatcher::Template(template) => template.match_iri(iri)?,
        };
        bindings.insert("repo", self.repo.as_str());
        Some(bindings)
    }

    fn pattern(&self) -> String {
        match &self.matcher {
            RowMatcher::Exact(exact) => exact.clone(),
            RowMatcher::Template(template) => template.source().to_string(),
        }
    }
}

/// Bind one resource family for every configured root, all rows sharing one
/// endpoint: per root, the concrete row (`urn:repo:{name}:{concrete}`) and/or
/// the templated row (`urn:repo:{name}:{templated}`). Roots are known at bind
/// time, so the space's `entries()` enumerates exactly the repos an agent can
/// browse — the manifold advertises per-root rows instead of a `{repo}`
/// template no probe can satisfy, and the old `[:...]` display sugar becomes
/// its real concrete + templated pair.
pub(crate) fn bind_family(
    mut space: EndpointSpace,
    roots: &Roots,
    endpoint: Arc<dyn Endpoint>,
    concrete: Option<&str>,
    templated: Option<&str>,
) -> EndpointSpace {
    for name in roots.keys() {
        if let Some(suffix) = concrete {
            space = space.bind_arc(
                RootRow::exact(name, format!("urn:repo:{name}:{suffix}")),
                Arc::clone(&endpoint),
            );
        }
        if let Some(suffix) = templated {
            space = space.bind_arc(
                RootRow::template(name, &format!("urn:repo:{name}:{suffix}")),
                Arc::clone(&endpoint),
            );
        }
    }
    space
}

// --- shared helpers ---------------------------------------------------------

/// The repo binding and its configured root directory. The grammar only
/// matches configured roots, so the lookup failing means a caller reached the
/// endpoint outside resolution — still answered honestly.
pub(crate) fn repo_root<'a>(
    inv: &'a Invocation<'_>,
    roots: &'a BTreeMap<String, PathBuf>,
) -> Result<(&'a str, &'a Path)> {
    let repo = inv
        .bindings
        .get("repo")
        .ok_or_else(|| Error::MissingArgument("repo".to_string()))?;
    let root = roots
        .get(repo)
        .ok_or_else(|| Error::NotFound(format!("browse: `{repo}` is not a configured root")))?;
    Ok((repo, root))
}

/// The per-root capability check (the declared wildcard's enforcement): the
/// capability must grant this root (`urn:cap:browse:read:{repo}`) or all roots
/// (the literal wildcard); root capability passes via [`Capability::allows`].
pub(crate) fn granted(inv: &Invocation<'_>, repo: &str) -> Result<()> {
    let scope = format!("{CAP_PREFIX}{repo}");
    if inv.capability.allows(&scope) || inv.capability.allows(CAP_WILDCARD) {
        return Ok(());
    }
    // Typed `Denied` — a permanent authority failure the trace, manifold, and
    // wire recognize as a 403-equivalent without sniffing message text.
    Err(Error::Denied(format!(
        "browse: capability does not grant `{scope}`"
    )))
}

/// The decoded `path` binding, or `""` (the root) when the grammar captured
/// none. IRIs carry percent-encoded paths (a space is not IRI-legal), so the
/// binding is decoded before it touches the filesystem.
pub(crate) fn path_binding(inv: &Invocation<'_>) -> Result<String> {
    match inv.bindings.get("path") {
        Some(path) => iri_decode(path),
        None => Ok(String::new()),
    }
}

/// The lexical half of the jail, shared by [`resolve`] and the path-scoped PR
/// listing (which scopes HISTORY — a deleted directory's past is still real,
/// so the path is never canonicalized there): `..` and absolute segments are
/// rejected outright.
pub(crate) fn lexical_jail(rel: &str) -> Result<()> {
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(bad_path("parent-directory (`..`) segments are not allowed"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(bad_path("absolute paths are not allowed"));
            }
        }
    }
    Ok(())
}

/// Resolve a root-relative path to a canonical path within the root — **the
/// jail**. `..` and absolute segments are rejected lexically; the target is
/// then canonicalized (it must exist — browsing is a read) and required to sit
/// within the canonical root, so a symlink component cannot escape.
pub(crate) fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
    lexical_jail(rel)?;
    let canonical_root = root
        .canonicalize()
        .map_err(|e| Error::Endpoint(format!("browse: root `{}`: {e}", root.display())))?;
    let target = canonical_root.join(rel);
    let canonical = target
        .canonicalize()
        .map_err(|_| Error::NotFound(format!("browse: no such path `{rel}`")))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(Error::Endpoint(format!(
            "browse: `{rel}` resolves outside its root"
        )));
    }
    Ok(canonical)
}

fn bad_path(detail: &str) -> Error {
    Error::InvalidArgument {
        name: "path".to_string(),
        detail: detail.to_string(),
    }
}

pub(crate) fn repr(media: &str, body: String) -> Representation {
    Representation::new(ReprType::new(media), body.into_bytes())
}

pub(crate) fn repr_utf8(media: &str, body: String) -> Representation {
    Representation::new(
        ReprType::new(media).with_param("charset", "utf-8"),
        body.into_bytes(),
    )
}

/// The `annotations` arg shared by the file and explain faces: `include` (the
/// S2-recommended spelling) or `true` folds the target's annotations into the
/// response; `false` (the default) leaves it untouched. Anything else is a
/// typed argument error — the manifold declares exactly these values.
pub(crate) fn include_annotations(inv: &Invocation<'_>) -> Result<bool> {
    match inv.inline_str("annotations").unwrap_or("false") {
        "include" | "true" => Ok(true),
        "" | "false" => Ok(false),
        other => Err(Error::InvalidArgument {
            name: "annotations".to_string(),
            detail: format!("`{other}` is not a recognized value (include, true, or false)"),
        }),
    }
}

/// Minimal HTML escaping for names and paths embedded in attributes and text.
pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Percent-encode a root-relative path for embedding in a `urn:repo:…` IRI.
/// `/` and the URN-safe punctuation stay literal; everything else (spaces,
/// `%`, non-ASCII) is encoded so the emitted IRI is RFC 3987-valid and the
/// encode/decode pair round-trips any UTF-8 filename.
pub(crate) fn iri_encode(path: &str) -> String {
    const SAFE: &[u8] = b"-._~/!$&'()*+,;=:@";
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || SAFE.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Percent-decode a `{path}` binding. Malformed escapes pass through
/// literally; the decoded bytes must be UTF-8.
pub(crate) fn iri_decode(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1), bytes.get(i + 2)) {
            (b'%', Some(hi), Some(lo)) => {
                let hex = |b: u8| (b as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hex(*hi), hex(*lo)) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            (byte, _, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| Error::InvalidArgument {
        name: "path".to_string(),
        detail: "not valid UTF-8 after percent-decoding".to_string(),
    })
}

/// A Turtle string literal (quote-and-escape).
pub(crate) fn ttl_str(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "")
            .replace('\n', "\\n")
    )
}

/// The tree IRI for a repo + root-relative directory path (`""` = the root).
pub(crate) fn tree_iri(repo: &str, rel: &str) -> String {
    if rel.is_empty() {
        format!("urn:repo:{repo}:tree")
    } else {
        format!("urn:repo:{repo}:tree:{}", iri_encode(rel))
    }
}

pub(crate) fn file_iri(repo: &str, rel: &str) -> String {
    format!("urn:repo:{repo}:file:{}", iri_encode(rel))
}

// --- directory listing ------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Dir,
    File,
    Link,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Dir => "dir",
            Kind::File => "file",
            Kind::Link => "link",
        }
    }
}

pub(crate) struct Entry {
    name: String,
    kind: Kind,
    /// Byte size — files only (a directory's disk size is not its meaning).
    size: Option<u64>,
}

/// List a directory: directories first, then files and links, each
/// alphabetical. Non-UTF-8 names are skipped rather than mangled.
pub(crate) fn list_entries(dir: &Path) -> Result<Vec<Entry>> {
    let read = std::fs::read_dir(dir)
        .map_err(|e| Error::Endpoint(format!("browse: read {}: {e}", dir.display())))?;
    let mut entries = Vec::new();
    for entry in read.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        // `file_type`/`metadata` on a DirEntry do NOT follow symlinks — a link
        // is reported as a link, and an escaping one is only ever denied later,
        // at resolution.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let kind = if file_type.is_dir() {
            Kind::Dir
        } else if file_type.is_symlink() {
            Kind::Link
        } else {
            Kind::File
        };
        let size = (kind == Kind::File)
            .then(|| entry.metadata().ok().map(|m| m.len()))
            .flatten();
        entries.push(Entry { name, kind, size });
    }
    entries.sort_by(|a, b| {
        let rank = |e: &Entry| (e.kind != Kind::Dir) as u8;
        (rank(a), a.name.as_str()).cmp(&(rank(b), b.name.as_str()))
    });
    Ok(entries)
}

// --- tree endpoint ----------------------------------------------------------

fn tree_endpoint(roots: &Roots, explain: bool) -> FnEndpoint {
    let held = Arc::clone(roots);
    FnEndpoint::new("browse-tree", move |inv: &Invocation<'_>| {
        let (repo, root) = repo_root(inv, &held)?;
        granted(inv, repo)?;
        let rel = path_binding(inv)?;
        let dir = resolve(root, &rel)?;
        if !dir.is_dir() {
            return Err(Error::NotFound(format!(
                "browse: `{rel}` is not a directory ({} serves its content)",
                file_iri(repo, &rel)
            )));
        }
        let entries = list_entries(&dir)?;
        match inv.inline_str("as").unwrap_or("text/plain") {
            t if t.starts_with("text/turtle") => {
                Ok(repr("text/turtle", tree_turtle(repo, &rel, &entries)))
            }
            t if t.starts_with("text/html") => Ok(repr_utf8(
                "text/html",
                tree_html(repo, &rel, &entries, explain),
            )),
            _ => Ok(repr_utf8("text/plain", tree_text(&entries))),
        }
    })
    .with_description(tree_description(explain))
}

/// NOTE (the manifold contract): `repo` is deliberately NOT an ArgSpec. Every
/// advertised row fixes the root in its pattern (`urn:repo:{name}:tree…`), so
/// there is no `{repo}` variable left to declare — declaring one would make
/// MCP/validate demand an argument no row can substitute. The `repo` binding
/// the endpoint reads is injected by [`RootRow`], not supplied by callers.
fn tree_description(explain: bool) -> Description {
    let mut summary = String::from(
        "A directory listing of a configured browse root — urn:repo:{repo}:tree is the \
         top, urn:repo:{repo}:tree:{path} a subdirectory. text/plain (default) is one \
         name<TAB>kind<TAB>size entry per line; as=text/html is an htmx-navigable \
         fragment (entries hx-get child tree/file resources into #browse); \
         as=text/turtle is the skolemized graph (ik:Directory/ik:File nodes under \
         stable urn:repo: IRIs, no blank nodes). Live and uncacheable.",
    );
    if explain {
        summary.push_str(
            " The html face links the directory's explain resource and each entry's \
             (urn:repo:{repo}:explain[:{path}]).",
        );
    }
    Description::new("browse-tree")
        .title("Repository tree")
        .summary(summary)
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("path")
                .binding()
                .optional()
                .summary("directory path within the root, percent-encoded (omitted = the top)"),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("the face to render")
                .one_of(["text/plain", "text/html", "text/turtle"])
                .default_value("text/plain"),
        )
        .output("text/plain;charset=utf-8")
        .output("text/html;charset=utf-8")
        .output("text/turtle")
}

fn tree_text(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|e| {
            let size = e.size.map_or("-".to_string(), |s| s.to_string());
            format!("{}\t{}\t{}", e.name, e.kind.label(), size)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The home affordance every crumb strip starts with: a plain anchor to the
/// HOST's index. `href="/"` is deliberate — in ikigai-web `/` is the host
/// index, so it works untouched; any other host either styles/rebinds
/// `.browse-home-link` (its adapter owns the page) or ships it harmlessly
/// unstyled. Documented in the README's host-contract section.
fn home_link_html() -> &'static str {
    "<a class=\"browse-home-link\" href=\"/\" aria-label=\"home\">&#8962;</a>\
     <span class=\"browse-sep\">/</span>"
}

/// The generalized breadcrumb strip: the home affordance, then one crumb per
/// item. `Some(iri)` crumbs `hx-get` that resource's html face into
/// `#browse`; `None` is the inert current segment. Labels are escaped here.
pub(crate) fn crumb_trail(items: &[(String, Option<String>)]) -> String {
    let mut out = String::from("<nav class=\"browse-crumbs\">");
    out.push_str(home_link_html());
    for (label, target) in items {
        match target {
            Some(iri) => out.push_str(&format!(
                "<button class=\"browse-crumb\" hx-get=\"/k/source {iri} as=text/html\" \
                 hx-target=\"#browse\" hx-swap=\"innerHTML\">{}</button>\
                 <span class=\"browse-sep\">/</span>",
                esc(label)
            )),
            None => out.push_str(&format!(
                "<span class=\"browse-here\">{}</span>",
                esc(label)
            )),
        }
    }
    out.push_str("</nav>");
    out
}

/// The path-shaped breadcrumb strip: the repo name and every ancestor
/// directory `hx-get` their tree face; the final segment (the current dir or
/// file) is inert text.
pub(crate) fn crumbs_html(repo: &str, rel: &str) -> String {
    let segments: Vec<&str> = if rel.is_empty() {
        Vec::new()
    } else {
        rel.split('/').collect()
    };
    let mut items = Vec::with_capacity(segments.len() + 1);
    items.push((
        repo.to_string(),
        (!segments.is_empty()).then(|| tree_iri(repo, "")),
    ));
    for (i, segment) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        let prefix = segments[..=i].join("/");
        items.push((
            segment.to_string(),
            (!last).then(|| tree_iri(repo, &prefix)),
        ));
    }
    crumb_trail(&items)
}

/// The explain affordance (rendered only when the explanation family is
/// mounted): a button that hx-gets the target's explain face into #browse,
/// like every other navigation here. `title` names the target for the
/// compact per-row `?` form; the header form is self-evident and passes
/// none.
fn explain_button(repo: &str, rel: &str, label: &str, title: Option<&str>) -> String {
    let title = title
        .map(|t| format!(" title=\"{}\"", esc(t)))
        .unwrap_or_default();
    format!(
        "<button class=\"browse-explain-link\"{title} hx-get=\"/k/source {iri} \
         as=text/html\" hx-target=\"#browse\" hx-swap=\"innerHTML\">{label}</button>",
        iri = explain::explain_iri(repo, rel),
    )
}

/// The header strip under the crumbs: face-level actions (today just the
/// explain link, when that family is mounted; empty otherwise), followed by
/// the explain option menu.
///
/// The menu is a SIBLING of the action row, not a member of it: the row is
/// `display:flex` in every host that styles it, and a disclosure opened inside
/// a flex row is sized to its content — on a phone that is a column of
/// wrapped fragments. Block-level below the row, it lays out at any width with
/// no CSS at all.
fn actions_html(repo: &str, rel: &str, explain: bool) -> String {
    if !explain {
        return String::new();
    }
    format!(
        "<nav class=\"browse-actions\">{}</nav>{}",
        explain_button(repo, rel, "explain", None),
        explain::menu_html(repo, rel),
    )
}

fn tree_html(repo: &str, rel: &str, entries: &[Entry], explain: bool) -> String {
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&crumbs_html(repo, rel));
    // The header actions: the explain affordance (family mounted), and — at
    // the top only — the repo's pull requests (a repo-grain resource, always
    // bound; its data facades answer at resolution time).
    let mut actions = String::new();
    if explain {
        actions.push_str(&explain_button(repo, rel, "explain", None));
    }
    if rel.is_empty() {
        actions.push_str(&format!(
            "<button class=\"browse-prs-link\" hx-get=\"/k/source {} as=text/html\" \
             hx-target=\"#browse\" hx-swap=\"innerHTML\">pull requests</button>",
            pr::prs_iri(repo),
        ));
    }
    if !actions.is_empty() {
        out.push_str(&format!("<nav class=\"browse-actions\">{actions}</nav>"));
    }
    // The directory's own option menu, once — NOT one per row. The rows keep
    // their compact `?`; a listing of a thousand entries must not carry a
    // thousand disclosures, each of which would read the archive and the model
    // inventory the moment it opened.
    if explain {
        out.push_str(&explain::menu_html(repo, rel));
    }
    out.push_str("<ul class=\"browse-entries\">");
    for e in entries {
        let child = if rel.is_empty() {
            e.name.clone()
        } else {
            format!("{rel}/{}", e.name)
        };
        let (iri, label) = match e.kind {
            Kind::Dir => (tree_iri(repo, &child), format!("{}/", esc(&e.name))),
            _ => (file_iri(repo, &child), esc(&e.name)),
        };
        let size = e
            .size
            .map(|s| format!(" <span class=\"browse-size\">{}</span>", human_size(s)))
            .unwrap_or_default();
        // The per-row explain affordance: one compact `?` per entry (its
        // tooltip names the target) — the archive answers instantly once
        // derived, so the row stays honest about cost only on first click.
        let explain_link = if explain {
            format!(
                " {}",
                explain_button(repo, &child, "?", Some(&format!("explain {}", e.name)))
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<li><button class=\"browse-{kind}\" hx-get=\"/k/source {iri} as=text/html\" \
             hx-target=\"#browse\" hx-swap=\"innerHTML\">{label}</button>{size}\
             {explain_link}</li>",
            kind = e.kind.label(),
        ));
    }
    out.push_str("</ul>");
    // Every tree page's recent-PRs section, LAZY by design: the tree renders
    // instantly and htmx fetches the listing after insertion
    // (hx-trigger="load", swapping into the block itself). When the pr
    // facades are absent the fetch answers the typed 404-with-guidance and
    // the host renders it per its error handling — the tree never blocks on
    // it. `chrome=embed` keeps the loaded fragment crumb-free (this page
    // already has the trail). The ROOT loads the repo-wide listing
    // (`state=all limit=10`, ikigai-repo >= 0.1.4 facade args forwarded by
    // the prs endpoint); a SUBDIRECTORY loads its own path-scoped listing
    // (`urn:repo:{repo}:prs:{path}`, default state=all), so every directory
    // page shows the PRs that touched IT.
    let (iri_with_args, label) = if rel.is_empty() {
        (
            format!("{} state=all limit=10", pr::prs_iri(repo)),
            "recent pull requests".to_string(),
        )
    } else {
        (
            format!("{} limit=10", pr::prs_scoped_iri(repo, rel)),
            "pull requests touching this directory".to_string(),
        )
    };
    out.push_str(&format!(
        "<section class=\"browse-recent-prs\"><h4>{label}</h4>\
         <div hx-get=\"/k/source {iri_with_args} chrome=embed as=text/html\" \
         hx-trigger=\"load\" hx-swap=\"innerHTML\">\
         <p class=\"browse-recent-prs-loading\">loading&#8230;</p></div></section>",
    ));
    out.push_str("</div>");
    out
}

pub(crate) fn human_size(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1048575 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1048576.0),
    }
}

/// The Turtle face: the listed directory and its entries, **skolemized** —
/// every node is the stable `urn:repo:…` IRI that also *resolves*, so the
/// graph is diffable, SPARQL-able, and navigable. Directory children are
/// `tree:` IRIs, file children `file:` IRIs. No blank nodes.
fn tree_turtle(repo: &str, rel: &str, entries: &[Entry]) -> String {
    let mut ttl = String::from("@prefix ik: <https://ikigai-rs.dev/ns#> .\n");
    let child_iri = |e: &Entry, child: &str| match e.kind {
        Kind::Dir => tree_iri(repo, child),
        _ => file_iri(repo, child),
    };
    let mut props = vec![
        "a ik:Directory".to_string(),
        format!("ik:repo {}", ttl_str(repo)),
    ];
    if !rel.is_empty() {
        props.push(format!("ik:path {}", ttl_str(rel)));
    }
    let child_rel = |e: &Entry| {
        if rel.is_empty() {
            e.name.clone()
        } else {
            format!("{rel}/{}", e.name)
        }
    };
    if !entries.is_empty() {
        let refs: Vec<String> = entries
            .iter()
            .map(|e| format!("<{}>", child_iri(e, &child_rel(e))))
            .collect();
        props.push(format!("ik:entry {}", refs.join(", ")));
    }
    ttl.push_str(&format!(
        "\n<{}> {} .\n",
        tree_iri(repo, rel),
        props.join(" ;\n    ")
    ));
    for e in entries {
        let child = child_rel(e);
        let class = match e.kind {
            Kind::Dir => "ik:Directory",
            Kind::File => "ik:File",
            Kind::Link => "ik:Symlink",
        };
        let mut props = vec![
            format!("a {class}"),
            format!("ik:fileName {}", ttl_str(&e.name)),
            format!("ik:path {}", ttl_str(&child)),
        ];
        if let Some(size) = e.size {
            props.push(format!("ik:byteSize {size}"));
        }
        ttl.push_str(&format!(
            "\n<{}> {} .\n",
            child_iri(e, &child),
            props.join(" ;\n    ")
        ));
    }
    ttl
}

// --- file endpoint ----------------------------------------------------------

fn file_endpoint(roots: &Roots, store: Option<&Arc<Store>>, explain: bool) -> FnEndpoint {
    let held = Arc::clone(roots);
    let store = store.map(Arc::clone);
    let has_store = store.is_some();
    FnEndpoint::new("browse-file", move |inv: &Invocation<'_>| {
        let (repo, root) = repo_root(inv, &held)?;
        granted(inv, repo)?;
        let rel = path_binding(inv)?;
        if rel.is_empty() {
            return Err(Error::MissingArgument("path".to_string()));
        }
        let target = resolve(root, &rel)?;
        if target.is_dir() {
            return Err(Error::NotFound(format!(
                "browse: `{rel}` is a directory ({} lists it)",
                tree_iri(repo, &rel)
            )));
        }
        let bytes = std::fs::read(&target)
            .map_err(|e| Error::Endpoint(format!("browse: read `{rel}`: {e}")))?;
        let include = include_annotations(inv)?;
        match inv.inline_str("as").unwrap_or("") {
            // The HTML face already renders the annotations panel when the
            // store is mounted — `annotations=include` changes nothing there.
            t if t.starts_with("text/html") => Ok(repr_utf8(
                "text/html",
                file_html(repo, &rel, &bytes, store.as_deref(), explain)?,
            )),
            _ if include => {
                // One resolution = content + human margin notes (the
                // agent-grounding face). Only a mounted store and textual
                // content can honor it — anything else fails loud.
                let Some(store) = store.as_deref() else {
                    return Err(Error::InvalidArgument {
                        name: "annotations".to_string(),
                        detail: "no annotation store is mounted (space_with_annotations / \
                                 space_with_explain)"
                            .to_string(),
                    });
                };
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    return Err(Error::InvalidArgument {
                        name: "annotations".to_string(),
                        detail: format!(
                            "`{rel}` is binary — there is no text face to fold annotations into"
                        ),
                    });
                };
                let included = annotate::included_for_text(store, repo, &rel, text)?;
                let mut out = text.to_string();
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
                out.push_str(&included.margin_text());
                Ok(repr_utf8("text/plain", out))
            }
            _ => Ok(Representation::new(media_type_for(&target, &bytes), bytes)),
        }
    })
    .with_description(file_description(has_store, explain))
}

/// `repo` is not an ArgSpec — see [`tree_description`]'s note.
fn file_description(has_store: bool, explain: bool) -> Description {
    let mut summary = String::from(
        "The content of one file within a configured browse root — \
         urn:repo:{repo}:file:{path}. Raw bytes by default, under an extension-mapped \
         media type (application/octet-stream fallback, UTF-8 sniffed to text/plain); \
         as=text/html renders a syntax-highlighted, line-numbered view whose lines \
         carry id=\"L{n}\" anchors — and, when the annotation store is mounted, marks \
         annotated lines with inline markers anchored to their cards and appends the \
         annotations panel with its create form. annotations=include (store mounted, \
         textual content) serves the text plus a compact margin-notes section — \
         content and human annotations in one resolution. Live and uncacheable.",
    );
    if explain {
        summary.push_str(
            " The html face links the file's explain resource \
             (urn:repo:{repo}:explain:{path}).",
        );
    }
    let mut description = Description::new("browse-file")
        .title("Repository file")
        .summary(summary)
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("path")
                .binding()
                .summary("file path within the root, percent-encoded"),
        );
    // Declared only when a store is mounted — offering the arg on a plain
    // `space()` mount would make the manifold over-offer.
    if has_store {
        description = description.input(
            ArgSpec::new("annotations")
                .optional()
                .summary(
                    "include folds the file's annotations in: the text face appends a \
                     margin-notes section, drift-reconciled against the very content served \
                     (the html face already renders them)",
                )
                .one_of(["include", "true", "false"])
                .default_value("false"),
        );
    }
    description
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("text/html for the highlighted, line-anchored view")
                .one_of(["text/html"]),
        )
        .output("application/octet-stream")
        .output("text/html;charset=utf-8")
        .output("text/plain;charset=utf-8")
}

/// The extension→media-type map for the raw face; unknown extensions fall back
/// to a UTF-8 sniff (`text/plain`) and finally `application/octet-stream`.
pub(crate) fn media_type_for(path: &Path, bytes: &[u8]) -> ReprType {
    let media = match path.extension().and_then(|e| e.to_str()) {
        Some("txt") => "text/plain",
        Some("md") => "text/markdown",
        Some("ttl") => "text/turtle",
        Some("nt") => "application/n-triples",
        Some("json") => "application/json",
        Some("jsonld") => "application/ld+json",
        Some("html") | Some("htm") => "text/html",
        Some("css") => "text/css",
        Some("js") | Some("mjs") => "text/javascript",
        Some("xml") => "application/xml",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("pdf") => "application/pdf",
        Some("wasm") => "application/wasm",
        Some("toml") => "application/toml",
        Some("yaml") | Some("yml") => "application/yaml",
        Some("csv") => "text/csv",
        Some("rs") => "text/x-rust",
        Some("py") => "text/x-python",
        Some("sh") | Some("fish") => "text/x-shellscript",
        _ if std::str::from_utf8(bytes).is_ok() => "text/plain",
        _ => "application/octet-stream",
    };
    if media.starts_with("text/") {
        ReprType::new(media).with_param("charset", "utf-8")
    } else {
        ReprType::new(media)
    }
}

/// The house Turtle/TriG definition (assets/): two-face's extended set (bat's
/// assets) still carries no RDF syntaxes, and the graph faces of this very
/// module deserve highlighting.
const TURTLE_SYNTAX: &str = include_str!("../assets/Turtle.sublime-syntax");

/// two-face's extended syntax set (~100 formats the stock syntect set misses —
/// TOML, TypeScript, Dockerfile, …) plus the embedded Turtle definition.
/// Unknown formats still degrade to escaped plain text.
fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(|| {
        let mut builder = two_face::syntax::extra_newlines().into_builder();
        builder.add(
            SyntaxDefinition::load_from_str(TURTLE_SYNTAX, true, None)
                .expect("the embedded Turtle syntax is valid"),
        );
        builder.build()
    })
}

/// The class prefix every highlight span and the theme stylesheet share.
/// Under [`CLASS_STYLE`] a scope like `comment.line.rust` renders as
/// `class="hl-comment hl-line hl-rust"`, and [`STYLE_IRI`]'s rules target
/// exactly those classes — markup and stylesheet cannot drift apart.
const CLASS_PREFIX: &str = "hl-";

/// syntect's class-based output mode: classes in the markup, colors in the
/// stylesheet — the host's color scheme decides, not the representation.
/// (S0-S5 baked InspiredGitHub's colors inline, forcing light islands on
/// dark hosts.)
const CLASS_STYLE: ClassStyle = ClassStyle::SpacedPrefixed {
    prefix: CLASS_PREFIX,
};

/// The theme stylesheet resource — one concrete, root-independent row (the
/// classed markup it styles renders on every mount). It cannot collide with
/// a root named `style`: root rows always carry a family segment
/// (`urn:repo:style:tree`), never the bare IRI.
pub const STYLE_IRI: &str = "urn:repo:style";

/// The generated theme stylesheet: the configured light theme, the configured
/// dark theme, each generated from two-face's embedded theme set via
/// `css_for_theme_with_class_style` and each run through `ikigai-a11y`'s
/// contrast-floor pass at the configured floor. The `.hl-code` base rule carries
/// each theme's foreground/background, which the `<pre>` opts into.
///
/// **Which themes, and which floor, are configuration** — the layered
/// `a11y.toml` ⊕ `{app}.a11y.toml`, read through `ikigai-a11y`. With no config
/// files present the defaults are `InspiredGithub` / `Base16OceanDark` / 4.5,
/// which are the constants this function used to hard-code, so a machine that
/// has written no `a11y.toml` sees no change but the floor pass.
///
/// **The floor pass replaces hand-patching.** Until 0.2.12 one dark rule was
/// corrected by a typed constant (`.hl-variable.hl-parameter { color: #c0c5ce }`,
/// written after parameters were noticed rendering at 3.23:1). That was one
/// instance of a general fact — some of any theme's scope colours miss the floor
/// against that theme's own ground — so it is now derived for every rule of both
/// themes: a sub-floor colour is lifted to **the theme's own default
/// foreground**, the one colour a theme guarantees is legible on its own ground.
/// Nothing is invented, and a rule whose own repainted background defeats even
/// that is left alone rather than churned (see `ikigai_a11y::css`).
///
/// **Each theme lives inside its OWN `prefers-color-scheme` block**, and that is
/// load-bearing rather than tidy. The light rules used to sit at top level with
/// only the dark ones in a media query — but **a media query contributes no
/// specificity**, so the two themes shared one cascade and the more specific
/// selector won regardless of scheme. syntect emits deeply nested scope
/// selectors (up to 33 classes in one selector here), so no prefix or `:root`
/// repetition could have made the dark block reliably outrank the light one;
/// the only fix is to stop the light rules matching in dark mode at all.
///
/// What that cost, concretely: a Rust parameter is `hl-variable hl-parameter`,
/// which light styles at `.hl-variable.hl-parameter` (0,2,0, `#323232`) while
/// dark's nearest applicable rule is the bare `.hl-variable` (0,1,0). Light won
/// in dark mode — `#323232` on `#2b303b`, a contrast ratio of about **1.03:1**,
/// which is to say invisible. Dark's own `.hl-variable.hl-parameter.hl-function`
/// never applied, because the markup carries no `hl-function` on a parameter.
/// Parameters were the visible case, not the only one: every scope where
/// InspiredGitHub is more specific than base16-ocean.dark leaked the same way.
///
/// The `.hl-code` floor stays OUTSIDE both blocks so a user agent matching
/// neither still gets a legible ground rather than dark-on-dark. Modern browsers
/// always report `light` or `dark` (the `no-preference` value was dropped from
/// the spec), so in practice one block always matches and the floor is belt and
/// braces — a monochrome-but-readable fallback, never a wrong-colour one.
fn style_css(config: &ikigai_a11y::A11y) -> Result<String> {
    let themes = theme_set();
    let light = scheme_css(themes, &config.theme.light, config.contrast.min)?;
    let dark = scheme_css(themes, &config.theme.dark, config.contrast.min)?;
    // The floor IS the light theme's own `.hl-code` rule, read off the theme
    // rather than copied out of it — until 0.2.12 it was a constant a test had
    // to keep honest, and a configurable light theme has no constant to copy.
    let floor = format!(
        ".hl-code {{\n color: {};\n background-color: {};\n}}\n",
        light.foreground.to_css(),
        light.ground.to_css()
    );
    Ok(format!(
        "{floor}\n\
         @media (prefers-color-scheme: light) {{\n{}}}\n\
         @media (prefers-color-scheme: dark) {{\n{}}}\n",
        light.css, dark.css
    ))
}

/// two-face's embedded theme set, loaded once: it is ~2MB of data and a pure
/// function of the build, so memoizing it in the process is safe in the way
/// memoizing the CONFIG would not be (a config file can change under us; the
/// embedded themes cannot).
fn theme_set() -> &'static two_face::theme::EmbeddedLazyThemeSet {
    static THEMES: OnceLock<two_face::theme::EmbeddedLazyThemeSet> = OnceLock::new();
    THEMES.get_or_init(two_face::theme::extra)
}

/// One scheme's generated CSS, floor-repaired, with the theme's own ground and
/// default foreground — the two colours the floor rule is written from.
struct SchemeCss {
    css: String,
    ground: ikigai_a11y::Rgba,
    foreground: ikigai_a11y::Rgba,
}

/// Generate one configured theme's CSS and lift every sub-floor colour in it to
/// that theme's own foreground.
///
/// This is `ikigai_a11y::themes::theme_css` spelled out, for two reasons: it
/// shares ONE embedded theme set across both schemes rather than loading two,
/// and it needs the ground/foreground pair for the `.hl-code` floor rule, which
/// the turnkey call computes internally and does not return.
fn scheme_css(
    themes: &two_face::theme::EmbeddedLazyThemeSet,
    name: &str,
    min: f64,
) -> Result<SchemeCss> {
    // Unreachable via the config, which rejects an unknown theme name at parse
    // — but this function is also the seam a future caller could hand a raw
    // name, and a silent fallback to the default theme would leave an operator
    // believing they had changed something.
    let embedded = ikigai_a11y::themes::embedded(name).ok_or_else(|| {
        Error::Endpoint(format!(
            "browse: `{name}` is not an embedded theme (see ikigai_a11y::configurable_themes)"
        ))
    })?;
    let theme = &themes[embedded];
    let (ground, foreground) = ikigai_a11y::themes::ground_and_foreground(theme);
    let css = css_for_theme_with_class_style(theme, CLASS_STYLE)
        .map_err(|e| Error::Endpoint(format!("browse: `{name}` generates no CSS: {e}")))?;
    Ok(SchemeCss {
        css: ikigai_a11y::apply_floor(&css, ground, foreground, min).css,
        ground,
        foreground,
    })
}

/// The effective accessibility config for this mount's application.
///
/// A machine with **no config home at all** (no `HOME`, no `XDG_CONFIG_HOME`)
/// gets the built-in defaults rather than an error: it has not misconfigured
/// anything, it simply has nowhere to configure, and a stylesheet is the wrong
/// place to discover that. Every other failure — an unreadable file, a
/// misspelled theme, an out-of-range floor — is returned loud, because those are
/// an operator having changed something and being owed the news.
fn a11y_config(app: Option<&str>) -> Result<ikigai_a11y::A11y> {
    match ikigai_a11y::load::load(app) {
        Ok(config) => Ok(config),
        Err(ikigai_a11y::ConfigError::NoConfigHome) => Ok(ikigai_a11y::A11y::default()),
        Err(e) => Err(Error::Endpoint(format!("browse: {e}"))),
    }
}

fn file_html(
    repo: &str,
    rel: &str,
    bytes: &[u8],
    store: Option<&Store>,
    explain: bool,
) -> Result<String> {
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&crumbs_html(repo, rel));
    out.push_str(&actions_html(repo, rel, explain));
    match std::str::from_utf8(bytes) {
        Err(_) => out.push_str(&format!(
            "<p class=\"browse-binary\">binary file — {} ({})</p>",
            human_size(bytes.len() as u64),
            esc(&media_type_for(Path::new(rel), bytes).media_type),
        )),
        Ok(text) => {
            // The S2 overlay, when the annotation store is mounted: which
            // lines carry a live anchor (marked in the view) plus the
            // annotations panel with its create affordance. The drift pass
            // runs against the very content being rendered.
            let overlay = store
                .map(|store| annotate::file_overlay(store, repo, rel, text))
                .transpose()?;
            let (marked, panel) = overlay.unwrap_or_default();
            out.push_str(&highlight_html(rel, text, &marked));
            out.push_str(&panel);
        }
    }
    out.push_str("</div>");
    Ok(out)
}

/// The highlighted, line-numbered code view. Every line is a span with
/// `id="L{n}"` and a self-link gutter number, so `#L42` deep-links a line —
/// the anchor surface S2's annotations target. Lines in `annotated` carry
/// the `browse-line-annotated` mark plus one inline marker per anchored
/// annotation, between the gutter number and the code (hosts may style it as
/// a margin dot): an anchor down to the annotation's card whose native
/// `title` tooltip reveals the note — no scripts, and inline flow keeps the
/// pre's line layout intact.
fn highlight_html(
    rel: &str,
    text: &str,
    annotated: &BTreeMap<u64, Vec<annotate::Marker>>,
) -> String {
    let syntaxes = syntaxes();
    let path = Path::new(rel);
    let syntax = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| syntaxes.find_syntax_by_extension(e))
        // Extensionless well-known names (Dockerfile, Makefile, …): sublime
        // syntaxes list full file names among their "extensions".
        .or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| syntaxes.find_syntax_by_extension(n))
        })
        .or_else(|| {
            text.lines()
                .next()
                .and_then(|l| syntaxes.find_syntax_by_first_line(l))
        })
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    let mut parse = ParseState::new(syntax);
    let mut scopes = ScopeStack::new();
    let mut out = format!("<pre class=\"browse-code {CLASS_PREFIX}code\"><code>");
    for (i, line) in LinesWithEndings::from(text).enumerate() {
        let n = i + 1;
        // Classed spans with PER-LINE balance: spans syntect leaves open
        // across lines (a block comment, a multi-line string) are closed at
        // the line's end and re-opened with the same classes at the next
        // line's start, so every line wrapper stays a well-formed unit and
        // the `#L{n}` anchor contract survives. A parser hiccup on one line
        // degrades that line to escaped plain text rather than failing the
        // whole face.
        let html = parse
            .parse_line(line, syntaxes)
            .ok()
            .and_then(|ops| {
                let reopened = reopen_spans(scopes.as_slice());
                line_tokens_to_classed_spans(line, &ops, CLASS_STYLE, &mut scopes)
                    .ok()
                    .map(|(spans, _)| {
                        let mut html = reopened;
                        html.push_str(&spans);
                        // The stack now reflects the line's END: everything
                        // still on it is a span this line opened (or
                        // re-opened) and must close.
                        html.push_str(&"</span>".repeat(scopes.len()));
                        html
                    })
            })
            .unwrap_or_else(|| esc(line));
        let markers = annotated.get(&(n as u64));
        let class = if markers.is_some() {
            "browse-line browse-line-annotated"
        } else {
            "browse-line"
        };
        let marks = markers.map_or_else(String::new, |markers| {
            markers
                .iter()
                .map(|m| {
                    // Hollow for machine (review) annotations, solid for
                    // human — the two kinds are distinguishable at the line.
                    let (class, dot) = if m.machine {
                        (
                            "browse-annotation-marker browse-annotation-marker-machine",
                            "○",
                        )
                    } else {
                        ("browse-annotation-marker", "●")
                    };
                    format!(
                        "<a class=\"{class}\" href=\"#annotation-{}\" title=\"{}\">{dot}</a>",
                        esc(&m.id),
                        esc(&m.note)
                    )
                })
                .collect()
        });
        out.push_str(&format!(
            "<span class=\"{class}\" id=\"L{n}\"><a class=\"browse-ln\" \
             href=\"#L{n}\">{n}</a>{marks}{html}</span>"
        ));
    }
    out.push_str("</code></pre>");
    out
}

/// Re-open the spans a previous line left dangling: one `<span>` per scope
/// still on the stack, classed exactly as `line_tokens_to_classed_spans`
/// classes its own (each scope atom, prefixed — syntect keeps that mapping
/// private, but it is tiny, and the multi-line test pins ours against real
/// output: the re-opened comment span must carry the same `hl-comment` its
/// opener did).
fn reopen_spans(scopes: &[Scope]) -> String {
    let mut out = String::new();
    for scope in scopes {
        out.push_str("<span class=\"");
        for (i, atom) in scope.build_string().split('.').enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(CLASS_PREFIX);
            out.push_str(atom);
        }
        out.push_str("\">");
    }
    out
}

// --- style endpoint ---------------------------------------------------------

/// The stylesheet's one concrete grammar row: the bare [`STYLE_IRI`], no
/// bindings — it is the same stylesheet whatever the roots are.
struct StyleRow;

impl Grammar for StyleRow {
    fn match_iri(&self, iri: &Iri) -> Option<Bindings> {
        (iri.as_str() == STYLE_IRI).then(Bindings::new)
    }

    fn pattern(&self) -> String {
        STYLE_IRI.to_string()
    }
}

/// `urn:repo:style` — the theme stylesheet every classed highlight face binds
/// to. There is no per-root residual to enforce, so the declared wildcard IS the
/// whole check (the kernel baseline runs it before dispatch).
///
/// **Still cacheable, and that is the load-bearing part.** The sheet is now a
/// function of the build AND of the layered `a11y.toml`, and effective expiry
/// propagates from dependencies — so had the config been treated as
/// uncacheable-because-it-reads-a-file, every read of this stylesheet would
/// have become a cold generation (measured at ~1.4ms against ~2µs cached:
/// `cargo run --release --example style_cache`). Instead the representation
/// declares a **golden thread per candidate config file**, including the ones
/// that do not exist yet, so an operator creating `dev-server.a11y.toml`
/// invalidates this too — on a host that watches the config home and cuts those
/// threads. No host does yet; until one does, the threads are declared and
/// never cut, which costs a restart to pick up an edit and costs nothing else.
fn style_endpoint(app: Option<&str>) -> FnEndpoint {
    let app = app.map(str::to_string);
    FnEndpoint::new("browse-style", move |_inv: &Invocation<'_>| {
        let app = app.as_deref();
        let config = a11y_config(app)?;
        let mut repr = repr_utf8("text/css", style_css(&config)?).cacheable();
        // Threads even when there is no config home: `threads` errors there, and
        // an absent config home is exactly the case `a11y_config` already
        // decided is not an error. Nothing to watch, nothing to declare.
        for thread in ikigai_a11y::load::threads(app).unwrap_or_default() {
            repr = repr.depends_on(thread);
        }
        Ok(repr)
    })
    .with_description(style_description())
}

fn style_description() -> Description {
    Description::new("browse-style")
        .title("Highlight stylesheet")
        .summary(
            "The theme stylesheet the classed highlight faces bind to — urn:repo:style, \
             text/css. The configured light and dark themes (a11y.toml ⊕ {app}.a11y.toml, \
             defaulting to InspiredGithub and Base16OceanDark), each inside its OWN \
             @media (prefers-color-scheme: …) block: one stylesheet, both schemes, neither \
             able to outrank the other by specificity. Every colour below the configured \
             contrast floor (default 4.5:1, WCAG AA) is lifted to its own theme's default \
             foreground — repaired in-palette, nothing invented. Every rule targets the \
             hl- classes the html faces emit, plus an unconditional .hl-code floor \
             (foreground/background) their <pre> carries, so a client matching neither \
             scheme still reads. Cacheable, with a golden thread per candidate config file.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .output("text/css;charset=utf-8")
}

// --- state endpoint ---------------------------------------------------------

struct GitState {
    /// `None` when the root is not a git repository, or on an unborn branch.
    head: Option<String>,
    /// Repo-relative paths with uncommitted changes (`git status --porcelain`).
    dirty: Vec<String>,
    /// Whether the root is inside a git work tree at all.
    git: bool,
}

/// Run `git -C <root> <args…>` — an argument vector, never a shell string.
fn git(root: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git").arg("-C").arg(root).args(args).output()
}

fn git_state(root: &Path) -> Result<GitState> {
    let status = git(root, &["status", "--porcelain"])
        .map_err(|e| Error::Endpoint(format!("browse: could not run git: {e}")))?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        if stderr.contains("not a git repository") {
            return Ok(GitState {
                head: None,
                dirty: Vec::new(),
                git: false,
            });
        }
        return Err(Error::Endpoint(format!(
            "browse: git status exited {}: {}",
            status.status.code().unwrap_or(-1),
            stderr.trim()
        )));
    }
    let dirty: Vec<String> = String::from_utf8_lossy(&status.stdout)
        .lines()
        // Porcelain v1: two status columns + a space, then the path (renames
        // keep their full `old -> new` form).
        .filter_map(|line| line.get(3..).map(str::to_string))
        .filter(|path| !path.is_empty())
        .collect();
    // An unborn branch (init, no commits) has no HEAD — that is `head: None`
    // with `git: true`, distinct from a non-repo.
    let head = git(root, &["rev-parse", "HEAD"])
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    Ok(GitState {
        head,
        dirty,
        git: true,
    })
}

fn state_endpoint(roots: &Roots) -> FnEndpoint {
    let held = Arc::clone(roots);
    FnEndpoint::new("browse-state", move |inv: &Invocation<'_>| {
        let (repo, root) = repo_root(inv, &held)?;
        granted(inv, repo)?;
        let state = git_state(root)?;
        match inv.inline_str("as").unwrap_or("text/plain") {
            t if t.starts_with("application/json") => {
                let json = serde_json::json!({ "head": state.head, "dirty": state.dirty });
                Ok(repr("application/json", json.to_string()))
            }
            _ => {
                let line = if !state.git {
                    "not a git repository".to_string()
                } else {
                    let head = state.head.as_deref().unwrap_or("unborn");
                    if state.dirty.is_empty() {
                        format!("{head} clean")
                    } else {
                        format!("{head} dirty:{}", state.dirty.len())
                    }
                };
                Ok(repr_utf8("text/plain", line))
            }
        }
    })
    .with_description(state_description())
}

/// `repo` is not an ArgSpec — see [`tree_description`]'s note.
fn state_description() -> Description {
    Description::new("browse-state")
        .title("Repository state (freshness oracle)")
        .summary(
            "The git state of a configured browse root — urn:repo:{repo}:state. One line: \
             the HEAD sha plus `clean` or `dirty:{n}` (or `not a git repository`); \
             as=application/json yields {head, dirty: [paths]} (head null off-git or on an \
             unborn branch). Deliberately uncacheable: this is the cheap probe other \
             resources key their freshness on.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("application/json for the structured form")
                .one_of(["application/json"])
                .default_value("text/plain"),
        )
        .output("text/plain;charset=utf-8")
        .output("application/json")
}

// --- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{ArgRef, Capability, Kernel, Request};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A demo root: `src/lib.rs`, `README.md`, `img.png` (binary), and a
    /// file with a space in its name.
    fn demo_root() -> PathBuf {
        let root = temp_dir();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn main() {\n    ()\n}\n").unwrap();
        std::fs::write(root.join("README.md"), "# demo\n").unwrap();
        std::fs::write(root.join("img.png"), [0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF]).unwrap();
        std::fs::write(root.join("hello world.txt"), "hi\n").unwrap();
        root
    }

    fn kernel(roots: Vec<(String, PathBuf)>) -> Kernel {
        Kernel::new(Arc::new(space(roots)))
    }

    fn source(
        kernel: &Kernel,
        iri: &str,
        args: &[(&str, &str)],
        cap: &Capability,
    ) -> Result<Representation> {
        let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
        for (k, v) in args {
            request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
        }
        block_on(kernel.issue(request, cap))
    }

    fn body(repr: &Representation) -> String {
        String::from_utf8_lossy(&repr.bytes).into_owned()
    }

    fn demo_cap() -> Capability {
        Capability::scoped(["urn:cap:browse:read:demo"])
    }

    #[test]
    fn tree_plain_lists_name_kind_size_dirs_first() {
        let root = demo_root();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        let out = source(&k, "urn:repo:demo:tree", &[], &demo_cap()).unwrap();
        assert_eq!(out.repr_type.media_type, "text/plain");
        let text = body(&out);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "src\tdir\t-", "{text}");
        assert!(lines.contains(&"README.md\tfile\t7"), "{text}");
        assert!(lines.contains(&"img.png\tfile\t6"), "{text}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tree_resolves_subdirectories_and_unknown_repo_is_a_clean_miss() {
        let root = demo_root();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        let out = source(&k, "urn:repo:demo:tree:src", &[], &demo_cap()).unwrap();
        assert!(body(&out).contains("lib.rs\tfile"), "{}", body(&out));

        // An unconfigured {repo} never matches the grammar: the kernel reports
        // Unresolved (a miss that other mounted spaces could still answer),
        // not an error from this module.
        let err = source(&k, "urn:repo:nope:tree", &[], &Capability::root()).unwrap_err();
        assert!(matches!(err, Error::Unresolved(_)), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_jail_rejects_traversal_and_absolute_paths() {
        let root = demo_root();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        // `..` (encoded so the IRI parses) — rejected lexically.
        let err = source(
            &k,
            "urn:repo:demo:tree:%2E%2E/escape",
            &[],
            &Capability::root(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");
        // An absolute path — rejected lexically.
        let err = source(
            &k,
            "urn:repo:demo:file:%2Fetc/passwd",
            &[],
            &Capability::root(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cannot_escape_the_root_even_at_full_authority() {
        let root = demo_root();
        let outside = temp_dir();
        std::fs::write(outside.join("secret.txt"), "hidden").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        // Listing through the link escapes the canonical root — denied.
        let err = source(&k, "urn:repo:demo:tree:link", &[], &Capability::root()).unwrap_err();
        assert!(matches!(err, Error::Endpoint(_)), "{err:?}");
        // Reading a file through it, likewise.
        let err = source(
            &k,
            "urn:repo:demo:file:link/secret.txt",
            &[],
            &Capability::root(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Endpoint(_)), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn tree_html_face_navigates_to_children() {
        let root = demo_root();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        let out = source(
            &k,
            "urn:repo:demo:tree",
            &[("as", "text/html")],
            &demo_cap(),
        )
        .unwrap();
        assert_eq!(out.repr_type.media_type, "text/html");
        let html = body(&out);
        // Directory entries hx-get the child tree; files the file face — both
        // into the #browse container (the runbook house style).
        assert!(
            html.contains("hx-get=\"/k/source urn:repo:demo:tree:src as=text/html\""),
            "{html}"
        );
        assert!(
            html.contains("hx-get=\"/k/source urn:repo:demo:file:README.md as=text/html\""),
            "{html}"
        );
        assert!(html.contains("hx-target=\"#browse\""), "{html}");
        // A space in a filename is percent-encoded in the IRI it links.
        assert!(
            html.contains("urn:repo:demo:file:hello%20world.txt"),
            "{html}"
        );
        // Every crumb strip opens with the host-wireable home affordance.
        assert!(
            html.contains("<a class=\"browse-home-link\" href=\"/\""),
            "{html}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tree_turtle_face_is_valid_and_skolemized() {
        let root = demo_root();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        let out = source(
            &k,
            "urn:repo:demo:tree",
            &[("as", "text/turtle")],
            &demo_cap(),
        )
        .unwrap();
        assert_eq!(out.repr_type.media_type, "text/turtle");
        let ttl = body(&out);
        // The graph parses as Turtle, and every node is a stable IRI — no
        // blank nodes anywhere.
        let triples: Vec<_> = oxttl::TurtleParser::new()
            .for_slice(out.bytes.as_slice())
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap_or_else(|e| panic!("turtle face must parse: {e}\n{ttl}"));
        assert!(!triples.is_empty());
        for t in &triples {
            assert!(!t.subject.to_string().starts_with("_:"), "{ttl}");
            assert!(!t.object.to_string().starts_with("_:"), "{ttl}");
        }
        assert!(
            ttl.contains("<urn:repo:demo:file:README.md> a ik:File"),
            "{ttl}"
        );
        assert!(
            ttl.contains("<urn:repo:demo:tree:src> a ik:Directory"),
            "{ttl}"
        );
        assert!(ttl.contains("ik:byteSize 7"), "{ttl}");
        // The listed directory links its entries.
        assert!(ttl.contains("ik:entry"), "{ttl}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn explain_affordances_render_only_when_the_family_is_mounted() {
        let root = demo_root();
        // A plain space(): no explain family, so no affordance anywhere — a
        // rendered link would dangle.
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        for iri in ["urn:repo:demo:tree", "urn:repo:demo:file:src/lib.rs"] {
            let html = body(&source(&k, iri, &[("as", "text/html")], &demo_cap()).unwrap());
            assert!(!html.contains("browse-explain-link"), "{iri}: {html}");
            // The option menu is part of the same family: an unmounted
            // explain-versions would answer nothing, so the disclosure that
            // fetches it must not render either.
            assert!(!html.contains("browse-explain-menu"), "{iri}: {html}");
        }

        // space_with_explain: the tree face links the directory's explain and
        // each entry's; the file face links its own.
        let store = Arc::new(oxigraph::store::Store::new().unwrap());
        let k = Kernel::new(Arc::new(space_with_explain(
            vec![("demo".to_string(), root.clone())],
            ExplainConfig::new(store),
        )));
        let html = body(
            &source(
                &k,
                "urn:repo:demo:tree",
                &[("as", "text/html")],
                &demo_cap(),
            )
            .unwrap(),
        );
        for target in [
            "hx-get=\"/k/source urn:repo:demo:explain as=text/html\"",
            "hx-get=\"/k/source urn:repo:demo:explain:src as=text/html\"",
            "hx-get=\"/k/source urn:repo:demo:explain:README.md as=text/html\"",
            // …and one option menu for the directory itself, pointing at its
            // versions face. One, not one per row.
            "hx-get=\"/k/source urn:repo:demo:explain-versions as=text/html\"",
        ] {
            assert!(html.contains(target), "missing {target}: {html}");
        }
        assert_eq!(html.matches("browse-explain-menu\"").count(), 1, "{html}");
        let html = body(
            &source(
                &k,
                "urn:repo:demo:file:src/lib.rs",
                &[("as", "text/html")],
                &demo_cap(),
            )
            .unwrap(),
        );
        assert!(
            html.contains("hx-get=\"/k/source urn:repo:demo:explain:src/lib.rs as=text/html\""),
            "{html}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn file_raw_face_passes_binary_through_with_a_mapped_type() {
        let root = demo_root();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        let out = source(&k, "urn:repo:demo:file:img.png", &[], &demo_cap()).unwrap();
        assert_eq!(out.repr_type.media_type, "image/png");
        assert_eq!(out.bytes, [0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF]);
        // A known text extension maps and carries a charset.
        let out = source(&k, "urn:repo:demo:file:src/lib.rs", &[], &demo_cap()).unwrap();
        assert_eq!(out.repr_type.media_type, "text/x-rust");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn file_html_face_is_line_anchored() {
        let root = demo_root();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        let out = source(
            &k,
            "urn:repo:demo:file:src/lib.rs",
            &[("as", "text/html")],
            &demo_cap(),
        )
        .unwrap();
        let html = body(&out);
        // Three lines, each anchored: id="L{n}" spans with self-link numbers —
        // the surface S2 annotations will target.
        assert!(html.contains("id=\"L1\""), "{html}");
        assert!(html.contains("id=\"L3\""), "{html}");
        assert!(html.contains("href=\"#L2\""), "{html}");
        // Highlighted via CLASSES (the rules live in urn:repo:style), never
        // inline styles — the host's color scheme decides, not the markup.
        assert!(
            html.contains("<pre class=\"browse-code hl-code\">"),
            "{html}"
        );
        assert!(html.contains("class=\"hl-"), "{html}");
        assert!(!html.contains("style=\""), "{html}");
        assert!(!html.contains("color:#"), "{html}");
        // The binary sibling renders a stub, not garbage.
        let out = source(
            &k,
            "urn:repo:demo:file:img.png",
            &[("as", "text/html")],
            &demo_cap(),
        )
        .unwrap();
        assert!(body(&out).contains("binary file"), "{}", body(&out));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_extended_syntax_set_covers_wide_formats_and_turtle() {
        let syntaxes = syntaxes();
        // Formats the stock syntect set misses, now covered by two-face…
        for ext in ["toml", "ts", "tsx", "dockerfile"] {
            assert!(
                syntaxes.find_syntax_by_extension(ext).is_some(),
                "two-face should cover `{ext}`"
            );
        }
        // …the extensionless well-known name…
        assert!(syntaxes.find_syntax_by_extension("Dockerfile").is_some());
        // …and the embedded house Turtle/TriG definition.
        for ext in ["ttl", "trig"] {
            assert!(
                syntaxes.find_syntax_by_extension(ext).is_some(),
                "the embedded Turtle syntax should cover `{ext}`"
            );
        }
    }

    #[test]
    fn wide_formats_highlight_and_line_anchors_survive() {
        let root = temp_dir();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        std::fs::write(root.join("Dockerfile"), "FROM rust:1\nRUN cargo build\n").unwrap();
        std::fs::write(
            root.join("graph.ttl"),
            "@prefix ik: <https://ikigai-rs.dev/ns#> .\n<urn:x> a ik:File .\n",
        )
        .unwrap();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        for path in ["Cargo.toml", "Dockerfile", "graph.ttl"] {
            let out = source(
                &k,
                &format!("urn:repo:demo:file:{path}"),
                &[("as", "text/html")],
                &demo_cap(),
            )
            .unwrap();
            let html = body(&out);
            // A real syntax matched: the view carries more than one distinct
            // token class (plain-text degradation carries at most one).
            let classes: BTreeSet<&str> = html
                .split("<span class=\"hl-")
                .skip(1)
                .filter_map(|s| s.split('"').next())
                .collect();
            assert!(classes.len() >= 2, "{path} fell back to plain text: {html}");
            // The per-line anchor contract survives the two-face swap.
            assert!(html.contains("id=\"L1\""), "{path}: {html}");
            assert!(html.contains("id=\"L2\""), "{path}: {html}");
            assert!(html.contains("href=\"#L2\""), "{path}: {html}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// The per-line rebalance: a construct syntect keeps a span open across
    /// lines (here a Rust block comment) must (a) leave every line's span
    /// structure balanced — the line wrappers ARE the `#L{n}` anchor surface,
    /// an unclosed highlight span would swallow them — and (b) carry its
    /// classes onto the continuation lines via the re-opened spans, pinning
    /// our reimplementation of syntect's scope→class mapping against real
    /// output.
    #[test]
    fn multi_line_constructs_stay_balanced_and_keep_their_classes() {
        let root = temp_dir();
        std::fs::write(root.join("lib.rs"), "/* one\n   two\n*/\nfn x() {}\n").unwrap();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        let html = body(
            &source(
                &k,
                "urn:repo:demo:file:lib.rs",
                &[("as", "text/html")],
                &demo_cap(),
            )
            .unwrap(),
        );
        // Balanced overall…
        assert_eq!(
            html.matches("<span").count(),
            html.matches("</span>").count(),
            "{html}"
        );
        // …and per line: each browse-line chunk closes what it opens.
        for chunk in html.split("<span class=\"browse-line\"").skip(1) {
            let line = chunk.split("<span class=\"browse-line\"").next().unwrap();
            // +1 for the browse-line wrapper itself, closed within the chunk
            // (the next line's wrapper starts a new chunk).
            assert_eq!(
                line.matches("<span").count() + 1,
                line.matches("</span>").count(),
                "unbalanced line: {line}"
            );
        }
        // The comment's classes reach line 2 (only via the re-opened span —
        // no token starts there).
        let line2 = html
            .split("id=\"L2\"")
            .nth(1)
            .and_then(|s| s.split("id=\"L3\"").next())
            .unwrap();
        assert!(line2.contains("hl-comment"), "{line2}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The stylesheet face: both scheme blocks in one text/css representation,
    /// classed to match the markup, and cacheable (a pure function of the
    /// build).
    #[test]
    fn the_style_face_serves_both_scheme_blocks_and_is_cacheable() {
        let root = demo_root();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        let cap = demo_cap();
        let out = source(&k, STYLE_IRI, &[], &cap).unwrap();
        assert_eq!(out.repr_type.media_type, "text/css");
        let css = body(&out);
        // The unconditional floor the <pre class="… hl-code"> opts into, before
        // either scheme block — a client matching neither still reads.
        let floor = css
            .split("@media")
            .next()
            .expect("there is text before the first media block");
        assert!(floor.contains(".hl-code {"), "{css}");
        assert!(floor.contains("background-color: #ffffff"), "{css}");
        // Each theme inside its OWN block: light (InspiredGitHub's white page)…
        let light = css
            .split("@media (prefers-color-scheme: light) {")
            .nth(1)
            .expect("the light block exists");
        assert!(light.contains("background-color: #ffffff"), "{css}");
        // …and dark (base16-ocean.dark's slate).
        let dark = css
            .split("@media (prefers-color-scheme: dark) {")
            .nth(1)
            .expect("the dark block exists");
        assert!(dark.contains(".hl-code {"), "{css}");
        assert!(dark.contains("background-color: #2b303b"), "{css}");
        // Scope rules target the same prefix the markup emits.
        assert!(css.contains(".hl-comment"), "{css}");
        // Cacheable: the second resolution is a cache hit.
        let request = Request::new(Verb::Source, Iri::parse(STYLE_IRI).unwrap());
        assert!(k.is_cached(&request, &cap), "the stylesheet should cache");
        // No browse grant at all ⇒ denied by the kernel baseline, like every
        // other browse action.
        let unrelated = Capability::scoped(["urn:cap:unrelated"]);
        let err = source(&k, STYLE_IRI, &[], &unrelated).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The stylesheet under the BUILT-IN defaults — never this machine's
    /// `a11y.toml`. A developer who has configured a different theme must not
    /// change what these tests measure, and CI (which has no config home
    /// contents) must measure the same thing they do.
    fn default_css() -> String {
        style_css(&ikigai_a11y::A11y::default()).expect("the default themes generate CSS")
    }

    /// The two scheme blocks' inner text, light first.
    fn scheme_blocks(css: &str) -> (String, String) {
        let (light, dark) = css
            .split_once("@media (prefers-color-scheme: dark) {")
            .expect("the dark block exists");
        let light = light
            .split_once("@media (prefers-color-scheme: light) {")
            .expect("the light block exists")
            .1;
        (light.to_string(), dark.to_string())
    }

    /// The colour a browser paints on an element carrying exactly `classes`,
    /// given one scheme block: among the selectors that APPLY (every class in
    /// the selector is on the element), the one with the most classes wins and
    /// source order breaks ties. That is the CSS cascade minus the parts syntect
    /// never emits — and it is what the old substring assertions only stood in
    /// for. Asserting on the winning rule catches a regression that adds a more
    /// specific, worse rule, which a `contains` cannot.
    fn cascade_colour(block: &str, classes: &[&str]) -> Option<String> {
        // syntect writes a banner comment; a brace inside it is not structure.
        let mut stripped = String::new();
        let mut rest = block;
        while let Some((before, after)) = rest.split_once("/*") {
            stripped.push_str(before);
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
        }
        stripped.push_str(rest);

        let mut best: Option<(usize, String)> = None;
        for rule in stripped.split('}') {
            let Some((selectors, body)) = rule.split_once('{') else {
                continue;
            };
            let Some(colour) = body
                .lines()
                .find_map(|line| line.trim().strip_prefix("color:"))
            else {
                continue;
            };
            let colour = colour.trim().trim_end_matches(';').to_string();
            for selector in selectors.split(',') {
                let selector = selector.trim();
                if !selector.starts_with('.') || selector.contains(char::is_whitespace) {
                    continue;
                }
                let parts: Vec<&str> = selector.trim_start_matches('.').split('.').collect();
                if !parts.iter().all(|part| classes.contains(part)) {
                    continue;
                }
                let wins = match &best {
                    Some((count, _)) => parts.len() >= *count,
                    None => true,
                };
                if wins {
                    best = Some((parts.len(), colour.clone()));
                }
            }
        }
        best.map(|(_, colour)| colour)
    }

    /// The invariant that makes a cross-scheme leak structurally impossible: the
    /// ONLY rule outside a scheme block is the floor. A media query adds no
    /// specificity, so any theme rule left at top level competes with the other
    /// theme's rules in the other scheme — and syntect's selectors run to 33
    /// classes, so a leaked rule can be unbeatable. Confinement is the guard;
    /// nothing about specificity needs reasoning about once this holds.
    #[test]
    fn no_theme_rule_sits_outside_a_scheme_block() {
        let css = default_css();
        let floor = css
            .split("@media")
            .next()
            .expect("there is text before the first media block");
        assert_eq!(
            floor.trim(),
            ".hl-code {\n color: #323232;\n background-color: #ffffff;\n}",
            "only the .hl-code floor may sit outside a scheme block — anything \
             else can outrank the other theme in its own scheme"
        );
    }

    /// The exact collision that made parameters unreadable, pinned so it cannot
    /// return: a Rust parameter is `hl-variable hl-parameter`, and the light rule
    /// for it (`#323232`) must live inside the LIGHT block only. Against dark's
    /// `#2b303b` ground that colour is ~1.03:1 contrast — invisible.
    #[test]
    fn the_light_parameter_colour_cannot_reach_the_dark_ground() {
        let css = default_css();
        let (light, dark) = scheme_blocks(&css);
        assert!(
            light.contains(".hl-variable.hl-parameter"),
            "the light theme still styles parameters: {light}"
        );
        assert!(
            !dark.contains("#323232"),
            "the light foreground must not appear anywhere in the dark block: {dark}"
        );
    }

    /// A parameter must read at the dark theme's own foreground, not fall through
    /// to the palette's red — 7.63:1 rather than 3.23:1.
    ///
    /// Until 0.2.12 a hand-written constant supplied that colour for this one
    /// scope. It is now the floor pass's output: the bare `.hl-variable` rule the
    /// parameter falls through to was BELOW the floor, so the pass lifted it to
    /// the theme's own `#c0c5ce`. Same colour, from the theme, without the
    /// constant — and every other sub-floor scope repaired with it.
    #[test]
    fn a_parameter_reads_at_the_theme_s_own_foreground() {
        let css = default_css();
        let (_, dark) = scheme_blocks(&css);
        let colour = cascade_colour(&dark, &["hl-variable", "hl-parameter"])
            .expect("some rule styles a parameter");
        assert_eq!(colour, "#c0c5ce", "the dark theme's own foreground");
        let ratio = ikigai_a11y::ratio(
            ikigai_a11y::Rgba::parse(&colour).expect("a colour"),
            ikigai_a11y::Rgba::parse("#2b303b").expect("a colour"),
        );
        assert!(
            (ratio - 7.63).abs() < 0.01,
            "parameters read at 7.63:1, not {ratio:.2}:1"
        );
    }

    /// The floor rule IS the light theme's own `.hl-code`, derived from the theme
    /// rather than copied out of it — so the drift the old constant needed a test
    /// to catch cannot happen. What is still worth pinning is that
    /// `ground_and_foreground` and syntect's generated `.hl-code` agree about
    /// what the theme's ground and foreground ARE.
    #[test]
    fn the_floor_is_the_light_theme_s_own_base_rule() {
        let css = default_css();
        let floor = css
            .split("@media")
            .next()
            .expect("there is text before the first media block");
        let (light, _) = scheme_blocks(&css);
        let base = light
            .split_once(".hl-code {")
            .and_then(|(_, rest)| rest.split_once('}'))
            .map(|(decls, _)| decls.to_string())
            .expect("the light theme declares .hl-code");
        for decl in base.lines().map(str::trim).filter(|l| !l.is_empty()) {
            assert!(
                floor.contains(decl),
                "the floor has drifted from the light theme's .hl-code: missing {decl:?}"
            );
        }
    }

    /// Adopting the config changes nothing for a machine that has not written
    /// one: `ikigai-a11y`'s defaults ARE the constants this crate hard-coded.
    #[test]
    fn the_defaults_are_the_themes_this_crate_used_to_hard_code() {
        let default = ikigai_a11y::A11y::default();
        assert_eq!(default.theme.light, "InspiredGithub");
        assert_eq!(default.theme.dark, "Base16OceanDark");
        assert_eq!(default.contrast.min, 4.5, "WCAG AA body text");
    }

    /// The pass ran, and ran to completion: a second pass over either block lifts
    /// nothing more. What it leaves in `unrepaired` is not a bug — those are
    /// rules that repaint their own background, where the theme's foreground
    /// would not clear the floor either and inventing a colour is not the pass's
    /// business. On base16-ocean.dark the leftovers are the two git-gutter
    /// markers, which are chrome rather than prose.
    #[test]
    fn the_floor_pass_is_complete_on_both_blocks() {
        let css = default_css();
        let (light, dark) = scheme_blocks(&css);
        for (block, ground, foreground) in [
            (&light, "#ffffff", "#323232"),
            (&dark, "#2b303b", "#c0c5ce"),
        ] {
            let again = ikigai_a11y::apply_floor(
                block,
                ikigai_a11y::Rgba::parse(ground).expect("a colour"),
                ikigai_a11y::Rgba::parse(foreground).expect("a colour"),
                4.5,
            );
            assert!(
                again.lifted.is_empty(),
                "a second pass found more to lift: {:?}",
                again.lifted
            );
            assert!(again.repairable(4.5), "the theme can repair itself at all");
        }
    }

    /// The themes are configuration now, not constants — asserted on a config
    /// value rather than on a file, so the test says what it means without
    /// depending on this machine's config home.
    #[test]
    fn a_configured_theme_reaches_the_stylesheet() {
        let mut config = ikigai_a11y::A11y::default();
        config.theme.dark = "Nord".to_string();
        let css = style_css(&config).expect("Nord generates CSS");
        let (_, dark) = scheme_blocks(&css);
        assert!(
            dark.contains("background-color: #2e3440"),
            "the dark block is Nord's, not base16-ocean.dark's: {dark}"
        );
        // …and the floor still comes from the LIGHT theme, unchanged.
        assert!(css.starts_with(".hl-code {\n color: #323232;"), "{css}");
    }

    /// A theme name no theme answers to is refused rather than silently
    /// defaulted: an operator who misspells a theme is owed the news.
    #[test]
    fn an_unknown_theme_is_an_error_not_a_fallback() {
        let mut config = ikigai_a11y::A11y::default();
        config.theme.light = "Base16OceanDrak".to_string();
        assert!(matches!(style_css(&config), Err(Error::Endpoint(_))));
    }

    /// The stylesheet stays CACHEABLE while depending on the config, and names
    /// every CANDIDATE config file as a golden thread — including one that does
    /// not exist, so an operator creating an override invalidates the sheet on a
    /// host that watches the config home. An uncacheable config here would have
    /// made every read a cold generation (~1.4ms against ~2µs).
    #[test]
    fn the_stylesheet_is_cacheable_and_declares_the_config_files() {
        let root = demo_root();
        let space = Mount::new(vec![("demo".to_string(), root.clone())])
            .app("browse-test")
            .space();
        let k = Kernel::new(Arc::new(space));
        let rep = block_on(k.issue(
            Request::new(Verb::Source, Iri::parse(STYLE_IRI).unwrap()),
            &demo_cap(),
        ))
        .expect("the stylesheet resolves");
        assert_eq!(rep.expiry, ikigai_core::Expiry::Never, "cacheable");
        let threads: Vec<String> = rep.threads().iter().map(|t| t.to_string()).collect();
        match ikigai_core::config::config_home() {
            // The shared layer and the app one, in that order.
            Some(_) => {
                assert_eq!(threads.len(), 2, "{threads:?}");
                assert!(threads[0].ends_with("/a11y.toml"), "{threads:?}");
                assert!(
                    threads[1].ends_with("/browse-test.a11y.toml"),
                    "{threads:?}"
                );
            }
            // No config home on this machine: nothing to watch, nothing declared,
            // and the sheet still renders from the built-in defaults.
            None => assert!(threads.is_empty(), "{threads:?}"),
        }
        assert!(String::from_utf8_lossy(&rep.bytes).starts_with(".hl-code {"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_percent_encoded_path_resolves() {
        let root = demo_root();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        let out = source(&k, "urn:repo:demo:file:hello%20world.txt", &[], &demo_cap()).unwrap();
        assert_eq!(body(&out), "hi\n");
        std::fs::remove_dir_all(&root).ok();
    }

    /// `git -C dir` with a throwaway identity, asserting success.
    fn run_git(dir: &Path, args: &[&str]) {
        let mut all = vec![
            "-C",
            dir.to_str().unwrap(),
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
        ];
        all.extend(args);
        let out = Command::new("git").args(&all).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn state_tracks_commits_and_dirt() {
        let root = temp_dir();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        run_git(&root, &["init"]);
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "-m", "first"]);
        let k = kernel(vec![("demo".to_string(), root.clone())]);

        let clean = body(&source(&k, "urn:repo:demo:state", &[], &demo_cap()).unwrap());
        assert!(clean.ends_with(" clean"), "{clean}");
        let head1 = clean.split_whitespace().next().unwrap().to_string();
        assert_eq!(head1.len(), 40, "{clean}");

        // Dirty the tree: the digest counts it and the JSON face names it.
        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        let dirty = body(&source(&k, "urn:repo:demo:state", &[], &demo_cap()).unwrap());
        assert!(dirty.ends_with(" dirty:1"), "{dirty}");
        let json = body(
            &source(
                &k,
                "urn:repo:demo:state",
                &[("as", "application/json")],
                &demo_cap(),
            )
            .unwrap(),
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["head"].as_str().unwrap(), head1, "{json}");
        assert_eq!(parsed["dirty"][0].as_str().unwrap(), "a.txt", "{json}");

        // A new commit moves HEAD and cleans the digest.
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "-m", "second"]);
        let after = body(&source(&k, "urn:repo:demo:state", &[], &demo_cap()).unwrap());
        assert!(after.ends_with(" clean"), "{after}");
        assert_ne!(after.split_whitespace().next().unwrap(), head1, "{after}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_non_git_root_still_serves_tree_and_file() {
        let root = demo_root(); // no .git
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        let state = body(&source(&k, "urn:repo:demo:state", &[], &demo_cap()).unwrap());
        assert_eq!(state, "not a git repository");
        let json = body(
            &source(
                &k,
                "urn:repo:demo:state",
                &[("as", "application/json")],
                &demo_cap(),
            )
            .unwrap(),
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["head"].is_null(), "{json}");
        assert_eq!(parsed["dirty"].as_array().unwrap().len(), 0, "{json}");
        // tree and file are unaffected — future roots (memory, skills dirs)
        // need not be repositories.
        assert!(source(&k, "urn:repo:demo:tree", &[], &demo_cap()).is_ok());
        assert!(source(&k, "urn:repo:demo:file:README.md", &[], &demo_cap()).is_ok());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn capability_gates_per_root() {
        let a = demo_root();
        let b = demo_root();
        let k = kernel(vec![
            ("alpha".to_string(), a.clone()),
            ("beta".to_string(), b.clone()),
        ]);
        // A grant naming alpha reads alpha…
        let alpha_only = Capability::scoped(["urn:cap:browse:read:alpha"]);
        assert!(source(&k, "urn:repo:alpha:tree", &[], &alpha_only).is_ok());
        // …and is denied on beta — typed, permanent.
        let err = source(&k, "urn:repo:beta:tree", &[], &alpha_only).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert!(!err.is_transient(), "{err:?}");
        // A capability with no browse grant at all is denied by the kernel's
        // baseline (declared = enforced) before the endpoint runs.
        let unrelated = Capability::scoped(["urn:cap:unrelated"]);
        let err = source(&k, "urn:repo:alpha:tree", &[], &unrelated).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        // The literal wildcard is an all-roots grant.
        let all = Capability::scoped([CAP_WILDCARD]);
        assert!(source(&k, "urn:repo:alpha:tree", &[], &all).is_ok());
        assert!(source(&k, "urn:repo:beta:state", &[], &all).is_ok());
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }

    #[test]
    fn describe_declares_argspecs_and_the_wildcard_capability() {
        let root = demo_root();
        let roots: Roots = Arc::new(BTreeMap::from([("demo".to_string(), root.clone())]));
        for (endpoint, wants_path) in [
            (tree_endpoint(&roots, false), true),
            (file_endpoint(&roots, None, false), true),
            (state_endpoint(&roots), false),
        ] {
            use ikigai_core::Endpoint;
            let description = endpoint.describe();
            assert!(
                description.requires.contains(&CAP_WILDCARD.to_string()),
                "{}: requires {:?}",
                description.id,
                description.requires
            );
            let names: Vec<&str> = description.inputs.iter().map(|i| i.name.as_str()).collect();
            assert!(names.contains(&"as"), "{}: {names:?}", description.id);
            assert_eq!(names.contains(&"path"), wants_path, "{}", description.id);
            // `repo` is NOT an ArgSpec: every advertised row fixes the root in
            // its pattern, so declaring a repo argument would demand something
            // no row can substitute. The binding is grammar-injected.
            assert!(!names.contains(&"repo"), "{}: {names:?}", description.id);
        }
        // The annotations arg is offered only when a store is mounted —
        // declaring it on a plain space() would make the manifold over-offer.
        {
            use ikigai_core::Endpoint;
            let bare = file_endpoint(&roots, None, false).describe();
            assert!(!bare.inputs.iter().any(|i| i.name == "annotations"));
            let store = Arc::new(oxigraph::store::Store::new().unwrap());
            let with_store = file_endpoint(&roots, Some(&store), false).describe();
            let ann = with_store
                .inputs
                .iter()
                .find(|i| i.name == "annotations")
                .expect("declared when the store is mounted");
            assert_eq!(ann.one_of, vec!["include", "true", "false"]);
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// The acceptance IS the manifold: every browse family must reach
    /// `select_actions` under an authorizing capability, as per-configured-root
    /// rows, and every templated row must survive the catalog's
    /// probe-expansion (core expands each `{var}` with a placeholder and the
    /// expansion must match the very grammar that owns the row). This is what
    /// 0.2.2 failed: `{repo}`-templated grammars probe-expanded to
    /// `urn:repo:probe:…`, which no configured-root grammar matches, so browse
    /// rows never reached ANY manifold.
    #[test]
    fn the_manifold_offers_per_root_rows_that_survive_probe_expansion() {
        use ikigai_core::ActionQuery;
        let root = demo_root();
        let store = Arc::new(oxigraph::store::Store::new().unwrap());
        let k = Kernel::new(Arc::new(crate::space_with_explain(
            vec![("demo".to_string(), root.clone())],
            crate::ExplainConfig::new(store),
        )));
        // browse grant + a net grant (explain declares urn:cap:net:* too).
        let cap = Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:net:localhost"]);
        let query = ActionQuery {
            capability: Some(&cap),
            ..Default::default()
        };
        let offered: BTreeSet<String> = k
            .select_actions(&query)
            .into_iter()
            .map(|m| m.endpoint)
            .collect();
        for row in [
            "urn:repo:demo:tree",
            "urn:repo:demo:tree:{path}",
            "urn:repo:demo:file:{path}",
            "urn:repo:demo:state",
            "urn:repo:demo:hash",
            "urn:repo:demo:hash:{path}",
            "urn:repo:demo:explain",
            "urn:repo:demo:explain:{path}",
            "urn:repo:demo:explain-versions",
            "urn:repo:demo:explain-versions:{path}",
            "urn:repo:demo:annotations",
            "urn:repo:demo:annotations:{path}",
            "urn:annotation:{id}",
            "urn:repo:style",
        ] {
            assert!(
                offered.contains(row),
                "manifold is missing `{row}`; offered: {offered:#?}"
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// What the capability grammar supports at SELECTION time: the browse
    /// actions declare the wildcard offering (`urn:cap:browse:read:*` = "holds
    /// some grant under this prefix"), and the description is per-endpoint —
    /// shared by every root's rows — so a single-root grant is offered ALL
    /// roots' rows. Root scoping is enforced at invoke time (`granted`), not
    /// narrowed per row at selection; a capability with no browse grant at all
    /// sees nothing. Pinned here so the residual over-offer is a documented
    /// fact, not an accident.
    #[test]
    fn selection_offers_wildcard_wide_and_enforcement_scopes_per_root() {
        use ikigai_core::ActionQuery;
        let a = demo_root();
        let b = demo_root();
        let k = Kernel::new(Arc::new(space(vec![
            ("alpha".to_string(), a.clone()),
            ("beta".to_string(), b.clone()),
        ])));
        let offered = |cap: &Capability| -> BTreeSet<String> {
            let query = ActionQuery {
                capability: Some(cap),
                ..Default::default()
            };
            k.select_actions(&query)
                .into_iter()
                .map(|m| m.endpoint)
                .collect()
        };
        // A single-root grant satisfies the wildcard offering, so BOTH roots'
        // rows are offered (invoking beta is still Denied — enforcement is the
        // authority, selection its pre-flight).
        let alpha_only = Capability::scoped(["urn:cap:browse:read:alpha"]);
        let rows = offered(&alpha_only);
        assert!(rows.contains("urn:repo:alpha:tree"), "{rows:#?}");
        assert!(rows.contains("urn:repo:beta:tree"), "{rows:#?}");
        // No browse grant ⇒ no browse rows at all.
        let unrelated = Capability::scoped(["urn:cap:unrelated"]);
        assert!(
            offered(&unrelated)
                .iter()
                .all(|r| !r.starts_with("urn:repo:")),
            "{:#?}",
            offered(&unrelated)
        );
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }

    #[test]
    fn reads_are_uncacheable_live_facts() {
        let root = demo_root();
        let k = kernel(vec![("demo".to_string(), root.clone())]);
        let cap = demo_cap();
        let request = Request::new(Verb::Source, Iri::parse("urn:repo:demo:tree").unwrap());
        block_on(k.issue(request.clone(), &cap)).unwrap();
        assert!(
            !k.is_cached(&request, &cap),
            "S0 reads are live — caching arrives with S1's content-hash keys"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
