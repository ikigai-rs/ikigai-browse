//! `ikigai-browse` — repository browsing as ikigai resources.
//!
//! A standalone **ikigai module crate** (like `ikigai-fs` / `ikigai-repo`): a
//! host links it in and mounts [`space`] over a set of named **roots** —
//! `(name, directory)` pairs. Each root then answers three resource families:
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
//!   anchors (S2's annotations will target them).
//! - `urn:repo:{repo}:state` — the **freshness oracle**: the git HEAD sha plus
//!   a short-status digest, one line; `as=application/json` yields
//!   `{head, dirty: [paths]}`. Uncacheable by design — it exists to be the
//!   cheap "has anything changed?" probe that later stages key caches on.
//!
//! ## Resolution is the access model
//!
//! A `{repo}` that is not a configured root is a **clean miss** — the grammar
//! itself refuses to match, so resolution falls through to whatever else is
//! mounted rather than erroring here. Paths are **jailed** to their root:
//! `..` and absolute segments are rejected lexically, and the canonicalized
//! target must stay inside the canonicalized root, so a symlink cannot escape.
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

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use ikigai_core::{
    ArgSpec, Bindings, Description, EndpointSpace, Error, FnEndpoint, Grammar, Invocation, Iri,
    ReprType, Representation, Result, UriTemplate, Verb,
};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::html::{styled_line_to_highlighted_html, IncludeBackground};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// The wildcard capability every browse action declares: an agent is offered
/// these resources iff it holds *some* grant under this prefix. Held literally,
/// it is an all-roots grant.
pub const CAP_WILDCARD: &str = "urn:cap:browse:read:*";

/// The grant prefix: `urn:cap:browse:read:{repo}` grants one configured root.
pub const CAP_PREFIX: &str = "urn:cap:browse:read:";

type Roots = Arc<BTreeMap<String, PathBuf>>;

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
    let mut map = BTreeMap::new();
    for (name, dir) in roots {
        assert!(
            !name.is_empty() && !name.contains([':', '/']),
            "browse: root name `{name}` must be non-empty and contain no `:` or `/`"
        );
        assert!(
            map.insert(name.clone(), dir).is_none(),
            "browse: duplicate root name `{name}`"
        );
    }
    let roots: Roots = Arc::new(map);
    EndpointSpace::new()
        .bind(
            KnownRepo::new(
                &["urn:repo:{repo}:tree:{path}", "urn:repo:{repo}:tree"],
                "urn:repo:{repo}:tree[:{path}]",
                &roots,
            ),
            tree_endpoint(&roots),
        )
        .bind(
            KnownRepo::new(
                &["urn:repo:{repo}:file:{path}"],
                "urn:repo:{repo}:file:{path}",
                &roots,
            ),
            file_endpoint(&roots),
        )
        .bind(
            KnownRepo::new(&["urn:repo:{repo}:state"], "urn:repo:{repo}:state", &roots),
            state_endpoint(&roots),
        )
}

// --- grammar ----------------------------------------------------------------

/// A URI-template grammar that additionally requires the captured `{repo}` to
/// name a configured root — so an unconfigured repo is a clean resolution
/// MISS (falling through to other mounted spaces), never an error from here.
struct KnownRepo {
    templates: Vec<UriTemplate>,
    pattern: String,
    roots: Roots,
}

impl KnownRepo {
    fn new(templates: &[&str], pattern: &str, roots: &Roots) -> Self {
        KnownRepo {
            templates: templates
                .iter()
                .map(|t| UriTemplate::parse(*t).expect("browse templates are valid"))
                .collect(),
            pattern: pattern.to_string(),
            roots: Arc::clone(roots),
        }
    }
}

impl Grammar for KnownRepo {
    fn match_iri(&self, iri: &Iri) -> Option<Bindings> {
        for template in &self.templates {
            if let Some(bindings) = template.match_iri(iri) {
                if bindings
                    .get("repo")
                    .is_some_and(|r| self.roots.contains_key(r))
                {
                    return Some(bindings);
                }
            }
        }
        None
    }

    fn pattern(&self) -> String {
        self.pattern.clone()
    }
}

// --- shared helpers ---------------------------------------------------------

/// The repo binding and its configured root directory. The grammar only
/// matches configured roots, so the lookup failing means a caller reached the
/// endpoint outside resolution — still answered honestly.
fn repo_root<'a>(
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
fn granted(inv: &Invocation<'_>, repo: &str) -> Result<()> {
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
fn path_binding(inv: &Invocation<'_>) -> Result<String> {
    match inv.bindings.get("path") {
        Some(path) => iri_decode(path),
        None => Ok(String::new()),
    }
}

/// Resolve a root-relative path to a canonical path within the root — **the
/// jail**. `..` and absolute segments are rejected lexically; the target is
/// then canonicalized (it must exist — browsing is a read) and required to sit
/// within the canonical root, so a symlink component cannot escape.
fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
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

fn repr(media: &str, body: String) -> Representation {
    Representation::new(ReprType::new(media), body.into_bytes())
}

fn repr_utf8(media: &str, body: String) -> Representation {
    Representation::new(
        ReprType::new(media).with_param("charset", "utf-8"),
        body.into_bytes(),
    )
}

/// Minimal HTML escaping for names and paths embedded in attributes and text.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Percent-encode a root-relative path for embedding in a `urn:repo:…` IRI.
/// `/` and the URN-safe punctuation stay literal; everything else (spaces,
/// `%`, non-ASCII) is encoded so the emitted IRI is RFC 3987-valid and the
/// encode/decode pair round-trips any UTF-8 filename.
fn iri_encode(path: &str) -> String {
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
fn iri_decode(s: &str) -> Result<String> {
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
fn ttl_str(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "")
            .replace('\n', "\\n")
    )
}

/// The tree IRI for a repo + root-relative directory path (`""` = the root).
fn tree_iri(repo: &str, rel: &str) -> String {
    if rel.is_empty() {
        format!("urn:repo:{repo}:tree")
    } else {
        format!("urn:repo:{repo}:tree:{}", iri_encode(rel))
    }
}

fn file_iri(repo: &str, rel: &str) -> String {
    format!("urn:repo:{repo}:file:{}", iri_encode(rel))
}

// --- directory listing ------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
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

struct Entry {
    name: String,
    kind: Kind,
    /// Byte size — files only (a directory's disk size is not its meaning).
    size: Option<u64>,
}

/// List a directory: directories first, then files and links, each
/// alphabetical. Non-UTF-8 names are skipped rather than mangled.
fn list_entries(dir: &Path) -> Result<Vec<Entry>> {
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

fn tree_endpoint(roots: &Roots) -> FnEndpoint {
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
            t if t.starts_with("text/html") => {
                Ok(repr_utf8("text/html", tree_html(repo, &rel, &entries)))
            }
            _ => Ok(repr_utf8("text/plain", tree_text(&entries))),
        }
    })
    .with_description(tree_description(roots))
}

fn tree_description(roots: &Roots) -> Description {
    Description::new("browse-tree")
        .title("Repository tree")
        .summary(
            "A directory listing of a configured browse root — urn:repo:{repo}:tree is the \
             top, urn:repo:{repo}:tree:{path} a subdirectory. text/plain (default) is one \
             name<TAB>kind<TAB>size entry per line; as=text/html is an htmx-navigable \
             fragment (entries hx-get child tree/file resources into #browse); \
             as=text/turtle is the skolemized graph (ik:Directory/ik:File nodes under \
             stable urn:repo: IRIs, no blank nodes). Live and uncacheable.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("repo")
                .binding()
                .summary("a configured root name")
                .one_of(roots.keys().cloned()),
        )
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

/// The breadcrumb strip: the repo name and every ancestor directory `hx-get`
/// their tree face; the final segment (the current dir or file) is inert text.
fn crumbs_html(repo: &str, rel: &str) -> String {
    let mut out = String::from("<nav class=\"browse-crumbs\">");
    let segments: Vec<&str> = if rel.is_empty() {
        Vec::new()
    } else {
        rel.split('/').collect()
    };
    let mut push_crumb = |target: &str, label: &str, last: bool| {
        if last {
            out.push_str(&format!(
                "<span class=\"browse-here\">{}</span>",
                esc(label)
            ));
        } else {
            out.push_str(&format!(
                "<button class=\"browse-crumb\" hx-get=\"/k/source {target} as=text/html\" \
                 hx-target=\"#browse\" hx-swap=\"innerHTML\">{}</button>\
                 <span class=\"browse-sep\">/</span>",
                esc(label)
            ));
        }
    };
    push_crumb(&tree_iri(repo, ""), repo, segments.is_empty());
    for (i, segment) in segments.iter().enumerate() {
        let prefix = segments[..=i].join("/");
        push_crumb(&tree_iri(repo, &prefix), segment, i + 1 == segments.len());
    }
    out.push_str("</nav>");
    out
}

fn tree_html(repo: &str, rel: &str, entries: &[Entry]) -> String {
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&crumbs_html(repo, rel));
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
        out.push_str(&format!(
            "<li><button class=\"browse-{kind}\" hx-get=\"/k/source {iri} as=text/html\" \
             hx-target=\"#browse\" hx-swap=\"innerHTML\">{label}</button>{size}</li>",
            kind = e.kind.label(),
        ));
    }
    out.push_str("</ul></div>");
    out
}

fn human_size(bytes: u64) -> String {
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

fn file_endpoint(roots: &Roots) -> FnEndpoint {
    let held = Arc::clone(roots);
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
        match inv.inline_str("as").unwrap_or("") {
            t if t.starts_with("text/html") => {
                Ok(repr_utf8("text/html", file_html(repo, &rel, &bytes)))
            }
            _ => Ok(Representation::new(media_type_for(&target, &bytes), bytes)),
        }
    })
    .with_description(file_description(roots))
}

fn file_description(roots: &Roots) -> Description {
    Description::new("browse-file")
        .title("Repository file")
        .summary(
            "The content of one file within a configured browse root — \
             urn:repo:{repo}:file:{path}. Raw bytes by default, under an extension-mapped \
             media type (application/octet-stream fallback, UTF-8 sniffed to text/plain); \
             as=text/html renders a syntax-highlighted, line-numbered view whose lines \
             carry id=\"L{n}\" anchors. Live and uncacheable.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("repo")
                .binding()
                .summary("a configured root name")
                .one_of(roots.keys().cloned()),
        )
        .input(
            ArgSpec::new("path")
                .binding()
                .summary("file path within the root, percent-encoded"),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("text/html for the highlighted, line-anchored view")
                .one_of(["text/html"]),
        )
        .output("application/octet-stream")
        .output("text/html;charset=utf-8")
}

/// The extension→media-type map for the raw face; unknown extensions fall back
/// to a UTF-8 sniff (`text/plain`) and finally `application/octet-stream`.
fn media_type_for(path: &Path, bytes: &[u8]) -> ReprType {
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

fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        ThemeSet::load_defaults()
            .themes
            .remove("InspiredGitHub")
            .expect("syntect ships InspiredGitHub")
    })
}

fn file_html(repo: &str, rel: &str, bytes: &[u8]) -> String {
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&crumbs_html(repo, rel));
    match std::str::from_utf8(bytes) {
        Err(_) => out.push_str(&format!(
            "<p class=\"browse-binary\">binary file — {} ({})</p>",
            human_size(bytes.len() as u64),
            esc(&media_type_for(Path::new(rel), bytes).media_type),
        )),
        Ok(text) => out.push_str(&highlight_html(rel, text)),
    }
    out.push_str("</div>");
    out
}

/// The highlighted, line-numbered code view. Every line is a span with
/// `id="L{n}"` and a self-link gutter number, so `#L42` deep-links a line —
/// the anchor surface S2's annotations will target.
fn highlight_html(rel: &str, text: &str) -> String {
    let syntaxes = syntaxes();
    let syntax = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| syntaxes.find_syntax_by_extension(e))
        .or_else(|| {
            text.lines()
                .next()
                .and_then(|l| syntaxes.find_syntax_by_first_line(l))
        })
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, theme());
    let mut out = String::from("<pre class=\"browse-code\"><code>");
    for (i, line) in LinesWithEndings::from(text).enumerate() {
        let n = i + 1;
        // A highlighter hiccup on one line degrades that line to escaped
        // plain text rather than failing the whole face.
        let html = highlighter
            .highlight_line(line, syntaxes)
            .ok()
            .and_then(|regions| {
                styled_line_to_highlighted_html(&regions, IncludeBackground::No).ok()
            })
            .unwrap_or_else(|| esc(line));
        out.push_str(&format!(
            "<span class=\"browse-line\" id=\"L{n}\"><a class=\"browse-ln\" \
             href=\"#L{n}\">{n}</a>{html}</span>"
        ));
    }
    out.push_str("</code></pre>");
    out
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
    .with_description(state_description(roots))
}

fn state_description(roots: &Roots) -> Description {
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
            ArgSpec::new("repo")
                .binding()
                .summary("a configured root name")
                .one_of(roots.keys().cloned()),
        )
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
        // Highlighted (spans with inline styles), inside a <pre>.
        assert!(html.contains("<pre class=\"browse-code\">"), "{html}");
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
            (tree_endpoint(&roots), true),
            (file_endpoint(&roots), true),
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
            assert!(names.contains(&"repo"), "{}: {names:?}", description.id);
            assert!(names.contains(&"as"), "{}: {names:?}", description.id);
            assert_eq!(names.contains(&"path"), wants_path, "{}", description.id);
            // The repo arg enumerates the configured roots for selection.
            let repo = description
                .inputs
                .iter()
                .find(|i| i.name == "repo")
                .unwrap();
            assert_eq!(repo.one_of, vec!["demo".to_string()], "{}", description.id);
        }
        std::fs::remove_dir_all(&root).ok();
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
