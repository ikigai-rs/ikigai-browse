//! `urn:repo:{repo}:prs` + `urn:repo:{repo}:pr:{n}` (+ `:explain`, `:review`)
//! — the **pull-request family**: browse's archive + annotation machinery
//! extended to PRs.
//!
//! ## Data through the kernel, never the network
//!
//! Browse does NOT depend on the `ikigai-repo` crate. It resolves that
//! module's data facades THROUGH THE KERNEL at runtime — `urn:repo:pr:list`,
//! `urn:repo:pr:view` (`as=application/json` carries `headRefOid`), and
//! `urn:repo:pr:diff` — passing `dir=` (the configured root's directory) so
//! the facades run in the right repo. The composition decides whether those
//! facades exist; when they are not mounted, PR resources answer a typed
//! `NotFound` naming the gap (never a panic), and the rest of browse is
//! untouched.
//!
//! ## The PR page is an annotation surface
//!
//! `urn:repo:{repo}:pr:{n}` renders metadata + the unified diff, and the DIFF
//! TEXT is the anchor surface: annotations target the PR IRI itself and quote
//! diff lines, re-anchoring or orphaning as the diff drifts (a force-push
//! changes the text) exactly like file annotations. Human notes and the
//! machine review pass land in the same `urn:annotation:` family.
//!
//! ## Archives key on the head commit
//!
//! `urn:repo:{repo}:pr:{n}:explain` archives by `(repo, pr, headRefOid,
//! version-tag)` — a PR explanation is *of a commit*, not of a mutable branch
//! tip: new commits = fresh derivation, prior entries stay addressable
//! (`version=` for older tags). `urn:repo:{repo}:pr:{n}:review` mints its
//! findings as machine annotations anchored in the diff and archives the pass
//! under the same `(oid, tag)` construction — re-sourcing an unchanged head
//! mints nothing.
//!
//! ## Capabilities
//!
//! The family declares the browse read wildcard (per-root grant enforced
//! here), plus net (explain/review call a model) and annotate (review writes
//! annotations). The pr facades additionally enforce their own capability
//! (`urn:cap:exec:gh` in ikigai-repo) when the sub-request dispatches — the
//! composition's concern, surfaced honestly by attenuation.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Bindings, Description, Endpoint, EndpointSpace, Error, Grammar, Invocation,
    Iri, Representation, Request, Result, UriTemplate, Verb,
};
use oxigraph::store::Store;

use crate::annotate::{self, Included, CAP_ANNOTATE};
use crate::explain::{
    entry_iri, explain_turtle, iso8601, load_entry, parse_iri, provider_label, resolve_model,
    store_entry, truncate, ArchiveEntry, CAP_NET, SYSTEM_PROMPT,
};
use crate::review::{load_pass, parse_findings, pass_iri, pass_turtle, store_pass, PassEntry};
use crate::{
    bind_family, esc, granted, highlight_html, repo_root, repr, repr_utf8, ExplainConfig, Roots,
    CAP_WILDCARD,
};

// --- the facade contract (ikigai-repo ≥ 0.1.3) --------------------------------

/// `number⇥title⇥branch⇥updated` per line; empty body + success = no open PRs.
const FACADE_LIST: &str = "urn:repo:pr:list";
/// `as=application/json` carries `headRefOid` (the archive key) and `author`
/// as an OBJECT (`login`/`name`/`is_bot`).
const FACADE_VIEW: &str = "urn:repo:pr:view";
/// The byte-pure unified diff.
const FACADE_DIFF: &str = "urn:repo:pr:diff";

// --- prompts (each constant is versioned; edit ⇒ bump its version) ------------

/// Version of [`PR_PROMPT`], folded into the explain archive key.
const PR_PROMPT_VERSION: &str = "pr-v1";
/// The PR-explain prompt — review-shaped by design (the classroom
/// centerpiece: "what does this change do, what would a reviewer look at").
const PR_PROMPT: &str = "Explain this pull request from its diff: what does this change do, and \
     what would a careful reviewer look at? Name the intent of the change, \
     the areas it touches, any behavior changes or risks the diff implies, \
     and what deserves the closest review attention.";

/// Version of the PR review prompt pair, folded into the pass archive key.
/// v2: the QUOTE must include the line's leading diff marker (+/-/space) —
/// v1 quotes routinely dropped it and nothing anchored (the diff-aware
/// anchoring in [`crate::annotate`] is the other half of that fix).
const PR_REVIEW_PROMPT_VERSION: &str = "pr-review-v2";
/// The PR reviewer persona (the file pass's, retargeted at a diff).
const PR_REVIEW_SYSTEM_PROMPT: &str =
    "You are an experienced engineer reviewing a colleague's pull request. \
     You write the kind of margin notes a thoughtful human reviewer leaves on \
     a diff: you name the change's intent and its tradeoffs, point at subtle \
     risks and edge cases the diff introduces, question misleading names or \
     comments, and call out one genuine strength when you see it. You never \
     restate what the diff plainly does, never nitpick formatting, and never \
     invent problems to fill space.";

/// The per-PR instruction: the finding format is the machine contract (each
/// finding anchors by its verbatim quote from the DIFF).
const PR_REVIEW_PROMPT: &str = "Review this pull request's diff and give your 3 to 6 most useful \
     findings. Format each finding as exactly two lines and nothing else:\n\
     QUOTE: <one line copied character-for-character from the diff, INCLUDING \
     its leading '+', '-', or space column - under 80 characters, distinctive \
     enough to occur only once>\n\
     NOTE: <one or two sentences of review commentary on that region>\n\
     Do not number the findings. Do not add headings, preamble, or closing \
     remarks. The QUOTE must appear verbatim in the diff, leading diff marker \
     and all, or the finding is discarded.";

// --- IRIs ---------------------------------------------------------------------

pub(crate) fn prs_iri(repo: &str) -> String {
    format!("urn:repo:{repo}:prs")
}

pub(crate) fn pr_iri(repo: &str, n: u64) -> String {
    format!("urn:repo:{repo}:pr:{n}")
}

fn pr_explain_iri(repo: &str, n: u64) -> String {
    format!("urn:repo:{repo}:pr:{n}:explain")
}

/// The archive `ik:path`-style discriminator for PR-scoped entries
/// (`pr:{n}`): stable, URN-clean, and never a real filesystem path (the
/// entry's `ik:about` names the PR IRI either way).
fn pr_rel(n: u64) -> String {
    format!("pr:{n}")
}

/// The PR family's breadcrumb trail — repo → prs → #n [→ layer] — built
/// explicitly because a PR is not a directory path: every ancestor is a live
/// crumb (the repo to its root tree, `prs` to the listing, `#n` to the PR
/// page when a deeper layer is current), and the current segment is inert.
fn pr_crumbs(repo: &str, n: Option<u64>, layer: Option<&str>) -> String {
    let mut items = vec![
        (repo.to_string(), Some(crate::tree_iri(repo, ""))),
        ("prs".to_string(), Some(prs_iri(repo))),
    ];
    if let Some(n) = n {
        items.push((format!("#{n}"), layer.is_some().then(|| pr_iri(repo, n))));
    }
    if let Some(layer) = layer {
        items.push((layer.to_string(), None));
    }
    // The trail's last entry must be inert: with no PR number, `prs` itself
    // is current.
    if n.is_none() {
        items.last_mut().expect("non-empty").1 = None;
    }
    crate::crumb_trail(&items)
}

// --- through the kernel -------------------------------------------------------

/// Issue one facade Source. An `Unresolved` miss becomes a typed `NotFound`
/// naming the composition gap: OUR row resolved — the data facade behind it
/// is simply not mounted.
async fn facade(
    inv: &Invocation<'_>,
    iri: &str,
    args: &[(&str, String)],
) -> Result<Representation> {
    let mut request = Request::new(Verb::Source, parse_iri(iri)?);
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.clone().into_bytes()));
    }
    match inv.issue(request).await {
        Err(Error::Unresolved(_)) => Err(Error::NotFound(format!(
            "browse: `{iri}` did not resolve — PR resources need ikigai-repo's pr facades \
             mounted in the composition"
        ))),
        other => other,
    }
}

fn dir_arg(root: &Path) -> (&'static str, String) {
    ("dir", root.to_string_lossy().into_owned())
}

/// The PR's unified diff, through the kernel — also the annotation layer's
/// drift fetch ([`crate::annotate`] calls it to reconcile PR annotations).
pub(crate) async fn diff_text(inv: &Invocation<'_>, root: &Path, n: u64) -> Result<String> {
    let repr = facade(inv, FACADE_DIFF, &[("pr", n.to_string()), dir_arg(root)]).await?;
    String::from_utf8(repr.bytes).map_err(|_| {
        Error::Endpoint(format!(
            "browse: `{FACADE_DIFF}` returned a non-UTF-8 diff for pr {n}"
        ))
    })
}

/// The metadata the structured view face carries — the fields the pages and
/// archives read. `head_oid` is required (it is the archive key); everything
/// else degrades to empty.
struct PrView {
    number: u64,
    title: String,
    state: String,
    draft: bool,
    head_ref: String,
    base_ref: String,
    head_oid: String,
    /// The author OBJECT, passed through verbatim (`login`/`name`/`is_bot`).
    author: serde_json::Value,
    updated_at: String,
    url: String,
    body: String,
}

impl PrView {
    /// The author's display label: `login`, else `name`, else `unknown`.
    fn author_label(&self) -> String {
        for key in ["login", "name"] {
            if let Some(label) = self.author.get(key).and_then(|v| v.as_str()) {
                if !label.is_empty() {
                    return label.to_string();
                }
            }
        }
        "unknown".to_string()
    }

    /// The one-line metadata summary the plain and html faces share.
    fn summary_line(&self) -> String {
        let state = if self.draft {
            format!("{} (draft)", self.state.to_lowercase())
        } else {
            self.state.to_lowercase()
        };
        format!(
            "{state} · {} · {} → {} · updated {}",
            self.author_label(),
            self.head_ref,
            self.base_ref,
            self.updated_at
        )
    }
}

async fn view(inv: &Invocation<'_>, root: &Path, n: u64) -> Result<PrView> {
    let repr = facade(
        inv,
        FACADE_VIEW,
        &[
            ("pr", n.to_string()),
            ("as", "application/json".to_string()),
            dir_arg(root),
        ],
    )
    .await?;
    let parsed: serde_json::Value = serde_json::from_slice(&repr.bytes).map_err(|e| {
        Error::Endpoint(format!(
            "browse: `{FACADE_VIEW}` did not return JSON for pr {n}: {e}"
        ))
    })?;
    let text = |key: &str| {
        parsed
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let head_oid = text("headRefOid");
    if head_oid.is_empty() {
        return Err(Error::Endpoint(format!(
            "browse: `{FACADE_VIEW}` carried no headRefOid for pr {n} — the archive key is \
             missing (ikigai-repo ≥ 0.1.3 exposes it on the json face)"
        )));
    }
    Ok(PrView {
        number: parsed.get("number").and_then(|v| v.as_u64()).unwrap_or(n),
        title: text("title"),
        state: text("state"),
        draft: parsed
            .get("isDraft")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        head_ref: text("headRefName"),
        base_ref: text("baseRefName"),
        head_oid,
        author: parsed
            .get("author")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        updated_at: text("updatedAt"),
        url: text("url"),
        body: text("body"),
    })
}

/// One ask through the kernel — system + prompt + temperature + the mandatory
/// `max_tokens` ceiling; an empty answer errors and archives nothing.
async fn ask(
    inv: &Invocation<'_>,
    config: &ExplainConfig,
    provider: &str,
    system: &str,
    prompt: &str,
    max_tokens: u32,
) -> Result<String> {
    let request = Request::new(Verb::Source, parse_iri(provider)?)
        .with_arg("prompt", ArgRef::Inline(prompt.as_bytes().to_vec()))
        .with_arg("system", ArgRef::Inline(system.as_bytes().to_vec()))
        .with_arg(
            "temperature",
            ArgRef::Inline(config.temperature.clone().into_bytes()),
        )
        .with_arg(
            "max_tokens",
            ArgRef::Inline(max_tokens.to_string().into_bytes()),
        );
    let answer = inv.issue(request).await?;
    let text = String::from_utf8_lossy(&answer.bytes).trim().to_string();
    if text.is_empty() {
        return Err(Error::Endpoint(format!(
            "browse: `{provider}` returned an empty answer (max_tokens {max_tokens} — thinking \
             models may need a higher ceiling); nothing archived"
        )));
    }
    Ok(text)
}

/// The model identity for a PR-family tag: the explicit config label (the
/// operator's override) → the provider's resolved `:model` identity → the
/// provider-IRI heuristic. Infallible by design, like the explain tiers.
async fn model_label(inv: &Invocation<'_>, provider: &str, explicit: &Option<String>) -> String {
    if let Some(label) = explicit {
        return label.clone();
    }
    match resolve_model(inv, provider).await {
        Some(model) => model,
        None => provider_label(provider),
    }
}

// --- binding ------------------------------------------------------------------

/// One PR-page row for one configured root: `urn:repo:{name}:pr:{n}` where
/// `{n}` must not span a `:` — otherwise the trailing-variable template would
/// swallow the `…:pr:{n}:explain` / `…:pr:{n}:review` rows (a trailing `{var}`
/// captures to the end of the IRI, and resolution is first-match-wins). The
/// probe placeholder (`probe`) carries no `:`, so the row still survives the
/// manifold's probe-expansion.
struct PrPageRow {
    repo: String,
    template: UriTemplate,
}

impl PrPageRow {
    fn new(repo: &str) -> Self {
        PrPageRow {
            repo: repo.to_string(),
            template: UriTemplate::parse(format!("urn:repo:{repo}:pr:{{n}}"))
                .expect("the pr template is valid"),
        }
    }
}

impl Grammar for PrPageRow {
    fn match_iri(&self, iri: &Iri) -> Option<Bindings> {
        let mut bindings = self.template.match_iri(iri)?;
        if bindings.get("n")?.contains(':') {
            return None;
        }
        bindings.insert("repo", self.repo.as_str());
        Some(bindings)
    }

    fn pattern(&self) -> String {
        self.template.source().to_string()
    }
}

/// Bind the data pages (`prs` + `pr:{n}`) for every root — every space
/// variant carries them: they need no store and no LLM, only the runtime
/// facades. `store` (when mounted) gives the PR page its annotations overlay;
/// `explain` gates the explain affordance exactly like the file faces.
pub(crate) fn bind_pages(
    space: EndpointSpace,
    roots: &Roots,
    store: Option<&Arc<Store>>,
    explain: bool,
) -> EndpointSpace {
    let prs: Arc<dyn Endpoint> = Arc::new(PrsEndpoint {
        roots: Arc::clone(roots),
    });
    let mut space = bind_family(space, roots, prs, Some("prs"), None);
    let page: Arc<dyn Endpoint> = Arc::new(PrEndpoint {
        roots: Arc::clone(roots),
        store: store.map(Arc::clone),
        explain,
    });
    for name in roots.keys() {
        space = space.bind_arc(PrPageRow::new(name), Arc::clone(&page));
    }
    space
}

/// Bind the derived layers (`pr:{n}:explain` + `pr:{n}:review`) — they ride
/// with the explanation family: same LLM seam, same store.
pub(crate) fn bind_explain(
    space: EndpointSpace,
    roots: &Roots,
    config: &Arc<ExplainConfig>,
) -> EndpointSpace {
    let explain: Arc<dyn Endpoint> = Arc::new(PrExplainEndpoint {
        roots: Arc::clone(roots),
        config: Arc::clone(config),
    });
    let space = bind_family(space, roots, explain, None, Some("pr:{n}:explain"));
    let review: Arc<dyn Endpoint> = Arc::new(PrReviewEndpoint {
        roots: Arc::clone(roots),
        config: Arc::clone(config),
    });
    bind_family(space, roots, review, None, Some("pr:{n}:review"))
}

/// The `n` binding as a PR number.
fn pr_binding(inv: &Invocation<'_>) -> Result<u64> {
    let n = inv
        .bindings
        .get("n")
        .ok_or_else(|| Error::MissingArgument("n (the pull-request number)".to_string()))?;
    n.parse().map_err(|_| Error::InvalidArgument {
        name: "n".to_string(),
        detail: format!("`{n}` is not a pull-request number"),
    })
}

// --- the listing endpoint -----------------------------------------------------

struct PrsEndpoint {
    roots: Roots,
}

#[async_trait]
impl Endpoint for PrsEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "browse-prs does not support the {:?} verb",
                inv.request.verb
            )));
        }
        let (repo, root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        // The 0.1.4 facade args, forwarded ONLY when the caller supplied them
        // — an older mounted facade never sees an argument it cannot answer,
        // and the defaults stay the facade's own (state=open, no limit).
        let mut args = vec![dir_arg(root)];
        for name in ["state", "limit"] {
            if let Ok(value) = inv.inline_str(name) {
                args.push((name, value.to_string()));
            }
        }
        match inv.inline_str("as").unwrap_or("text/plain") {
            // The structured face is the facade's own `--json` export, passed
            // through verbatim (number/title/headRefName/updatedAt/state rows).
            t if t.starts_with("application/json") => {
                let mut args = args;
                args.push(("as", "application/json".to_string()));
                facade(inv, FACADE_LIST, &args).await
            }
            t if t.starts_with("text/html") => {
                let listing = facade(inv, FACADE_LIST, &args).await?;
                let text = String::from_utf8_lossy(&listing.bytes).to_string();
                // chrome=embed: the rows-only fragment another face folds in
                // (the root tree's lazy recent-PRs block) — no crumbs, no
                // wrapper, the embedding page already carries both.
                let embed = inv.inline_str("chrome").is_ok_and(|c| c == "embed");
                Ok(repr_utf8("text/html", prs_html(repo, &text, embed)))
            }
            // The default face is the facade's text contract, untouched:
            // number⇥title⇥branch⇥updated[⇥state] per line, empty = no PRs.
            _ => facade(inv, FACADE_LIST, &args).await,
        }
    }

    fn name(&self) -> &str {
        "browse-prs"
    }

    fn describe(&self) -> Description {
        prs_description()
    }
}

/// `repo` is not an ArgSpec: every advertised row fixes the root in its
/// pattern (see `crate::bind_family`); the binding is grammar-injected.
fn prs_description() -> Description {
    Description::new("browse-prs")
        .title("Pull requests of a browse root")
        .summary(
            "The pull requests of a configured browse root — urn:repo:{repo}:prs, \
             resolved through the kernel from ikigai-repo's urn:repo:pr:list facade run \
             in the root's directory (the facades must be mounted in the composition and \
             enforce their own exec capability; unmounted, this answers a typed NotFound \
             naming the gap). state= and limit= forward to the facade (ikigai-repo >= \
             0.1.4: state filtering plus recency sort; omitted, the facade's own \
             defaults apply — open PRs, unlimited). text/plain (default) is one \
             number<TAB>title<TAB>branch<TAB>updated<TAB>state line per PR — an empty \
             body means no matching PRs, not an error; as=application/json passes the \
             facade's structured rows through; as=text/html renders the listing with \
             each PR linking its page (urn:repo:{repo}:pr:{n}) — chrome=embed serves \
             the rows-only fragment for embedding (the root tree's lazy recent-PRs \
             block). Live and uncacheable.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("state")
                .optional()
                .summary("filter by state, forwarded to the facade")
                .one_of(["open", "closed", "merged", "all"])
                .default_value("open"),
        )
        .input(
            ArgSpec::new("limit")
                .optional()
                .class("http://www.w3.org/2001/XMLSchema#integer")
                .summary("at most this many PRs, most recently updated first"),
        )
        .input(
            ArgSpec::new("chrome")
                .optional()
                .summary("embed renders the html rows only (no crumbs/wrapper)")
                .one_of(["full", "embed"])
                .default_value("full"),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("the face to render")
                .one_of(["text/plain", "application/json", "text/html"])
                .default_value("text/plain"),
        )
        .output("text/plain;charset=utf-8")
        .output("application/json")
        .output("text/html;charset=utf-8")
}

fn prs_html(repo: &str, listing: &str, embed: bool) -> String {
    let mut rows = String::new();
    for line in listing.lines() {
        let mut cols = line.split('\t');
        let (Some(number), Some(title)) = (cols.next(), cols.next()) else {
            continue;
        };
        let Ok(n) = number.parse::<u64>() else {
            continue;
        };
        let branch = cols.next().unwrap_or("");
        let updated = cols.next().unwrap_or("");
        // The 0.1.4 text contract's fifth column; absent on an older facade,
        // and an empty state simply renders no badge.
        let state = cols.next().unwrap_or("");
        let state_span = if state.is_empty() {
            String::new()
        } else {
            format!(" <span class=\"browse-pr-state\">{}</span>", esc(state))
        };
        rows.push_str(&format!(
            "<li><button class=\"browse-pr\" hx-get=\"/k/source {iri} as=text/html\" \
             hx-target=\"#browse\" hx-swap=\"innerHTML\">#{n} {title}</button>{state_span} \
             <span class=\"browse-pr-branch\">{branch}</span> \
             <span class=\"browse-pr-updated\">{updated}</span></li>",
            iri = pr_iri(repo, n),
            title = esc(title),
            branch = esc(branch),
            updated = esc(updated),
        ));
    }
    let body = if rows.is_empty() {
        "<p class=\"browse-prs-empty\">no pull requests</p>".to_string()
    } else {
        format!("<ul class=\"browse-prs\">{rows}</ul>")
    };
    if embed {
        return body;
    }
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&pr_crumbs(repo, None, None));
    out.push_str(&body);
    out.push_str("</div>");
    out
}

// --- the PR page --------------------------------------------------------------

struct PrEndpoint {
    roots: Roots,
    store: Option<Arc<Store>>,
    explain: bool,
}

#[async_trait]
impl Endpoint for PrEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "browse-pr does not support the {:?} verb",
                inv.request.verb
            )));
        }
        let (repo, root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        let n = pr_binding(inv)?;
        let view = view(inv, root, n).await?;
        let diff = diff_text(inv, root, n).await?;
        let include = crate::include_annotations(inv)?;
        let target = pr_iri(repo, n);
        match inv.inline_str("as").unwrap_or("text/plain") {
            t if t.starts_with("text/html") => {
                // The overlay when the annotation store is mounted: markers on
                // annotated diff lines plus the panel with its create form
                // targeting THIS PR — the diff text is the anchor surface.
                let overlay = self
                    .store
                    .as_deref()
                    .map(|store| annotate::target_overlay(store, &target, &diff))
                    .transpose()?;
                let (marked, panel) = overlay.unwrap_or_default();
                Ok(repr_utf8(
                    "text/html",
                    pr_html(repo, n, &view, &diff, &marked, &panel, self.explain),
                ))
            }
            t if t.starts_with("application/json") => {
                let mut json = serde_json::json!({
                    "number": view.number,
                    "title": view.title,
                    "state": view.state,
                    "draft": view.draft,
                    "author": view.author,
                    "head_ref": view.head_ref,
                    "base_ref": view.base_ref,
                    "head_ref_oid": view.head_oid,
                    "updated_at": view.updated_at,
                    "url": view.url,
                    "body": view.body,
                    "diff": diff,
                });
                if include {
                    json["annotations"] = self.included(&target, &diff)?.json();
                }
                Ok(repr("application/json", json.to_string()))
            }
            _ => {
                let mut out = format!("#{} {}\n{}\n", view.number, view.title, view.summary_line());
                out.push_str(&format!("head {}\n", view.head_oid));
                if !view.url.is_empty() {
                    out.push_str(&view.url);
                    out.push('\n');
                }
                if !view.body.trim().is_empty() {
                    out.push('\n');
                    out.push_str(view.body.trim());
                    out.push('\n');
                }
                out.push('\n');
                out.push_str(&diff);
                if include {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push('\n');
                    out.push_str(&self.included(&target, &diff)?.margin_text());
                }
                Ok(repr_utf8("text/plain", out))
            }
        }
    }

    fn name(&self) -> &str {
        "browse-pr"
    }

    fn describe(&self) -> Description {
        pr_description(self.store.is_some(), self.explain)
    }
}

impl PrEndpoint {
    /// The `annotations=include` payload: the PR's annotations reconciled
    /// against the very diff being served. Fails loud when no store is
    /// mounted — the manifold only offers the arg when one is.
    fn included(&self, target: &str, diff: &str) -> Result<Included> {
        let Some(store) = self.store.as_deref() else {
            return Err(Error::InvalidArgument {
                name: "annotations".to_string(),
                detail: "no annotation store is mounted (space_with_annotations / \
                         space_with_explain)"
                    .to_string(),
            });
        };
        annotate::included_for_target_text(store, target, diff)
    }
}

/// `repo` is not an ArgSpec — see [`prs_description`]'s note.
fn pr_description(has_store: bool, explain: bool) -> Description {
    let mut summary = String::from(
        "One pull request of a configured browse root — urn:repo:{repo}:pr:{n}: \
         metadata (from ikigai-repo's urn:repo:pr:view json face, run in the root's \
         directory) plus the unified diff (urn:repo:pr:diff) — the facades must be \
         mounted in the composition and enforce their own exec capability; unmounted, \
         this answers a typed NotFound naming the gap. The DIFF TEXT is the annotation \
         surface: annotations target this PR IRI and quote diff lines, drifting exactly \
         like file annotations. text/plain (default) is the metadata header + diff; \
         as=application/json the structured record (author is an object, head_ref_oid \
         the head commit); as=text/html the page — highlighted diff with line anchors \
         and, when the annotation store is mounted, annotated-line markers and the \
         annotations panel with its create form targeting this PR. Live and uncacheable.",
    );
    if explain {
        summary.push_str(
            " The html face links the PR's explain resource (urn:repo:{repo}:pr:{n}:explain).",
        );
    }
    let mut description = Description::new("browse-pr")
        .title("Pull request page (metadata + diff)")
        .summary(summary)
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("n")
                .binding()
                .class("http://www.w3.org/2001/XMLSchema#integer")
                .summary("the pull-request number"),
        );
    if has_store {
        description = description.input(
            ArgSpec::new("annotations")
                .optional()
                .summary(
                    "include folds the PR's annotations in, drift-reconciled against the \
                     very diff served (the html face already renders them)",
                )
                .one_of(["include", "true", "false"])
                .default_value("false"),
        );
    }
    description
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("the face to render")
                .one_of(["text/plain", "application/json", "text/html"])
                .default_value("text/plain"),
        )
        .output("text/plain;charset=utf-8")
        .output("application/json")
        .output("text/html;charset=utf-8")
}

#[allow(clippy::too_many_arguments)]
fn pr_html(
    repo: &str,
    n: u64,
    view: &PrView,
    diff: &str,
    marked: &BTreeMap<u64, Vec<annotate::Marker>>,
    panel: &str,
    explain: bool,
) -> String {
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&pr_crumbs(repo, Some(n), None));
    let mut actions = format!(
        "<button class=\"browse-view-link\" hx-get=\"/k/source {} as=text/html\" \
         hx-target=\"#browse\" hx-swap=\"innerHTML\">pull requests</button>",
        prs_iri(repo),
    );
    if explain {
        actions.push_str(&format!(
            "<button class=\"browse-explain-link\" hx-get=\"/k/source {} as=text/html\" \
             hx-target=\"#browse\" hx-swap=\"innerHTML\">explain</button>",
            pr_explain_iri(repo, n),
        ));
    }
    out.push_str(&format!("<nav class=\"browse-actions\">{actions}</nav>"));
    out.push_str(&format!(
        "<div class=\"browse-pr-meta\"><h3>#{} {}</h3>\
         <p class=\"browse-pr-line\">{}</p>\
         <p class=\"browse-pr-head\">head {}</p></div>",
        view.number,
        esc(&view.title),
        esc(&view.summary_line()),
        esc(&view.head_oid),
    ));
    // The diff through the same highlighted, line-anchored view files get —
    // the `.diff` name selects the diff syntax; markers land like a file's.
    out.push_str(&highlight_html(&format!("pr-{n}.diff"), diff, marked));
    out.push_str(panel);
    out.push_str("</div>");
    out
}

// --- the explain layer --------------------------------------------------------

struct PrExplainEndpoint {
    roots: Roots,
    config: Arc<ExplainConfig>,
}

#[async_trait]
impl Endpoint for PrExplainEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "browse-pr-explain does not support the {:?} verb",
                inv.request.verb
            )));
        }
        let (repo, root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        let n = pr_binding(inv)?;
        let config = &self.config;

        // The archive key's backbone: the HEAD COMMIT, not the branch tip —
        // new commits re-key, force-pushes re-key, and the old entries stay.
        let view = view(inv, root, n).await?;
        let model = model_label(inv, &config.pr_provider, &config.pr_model_label).await;
        let current_tag = format!("{PR_PROMPT_VERSION}@{model}");
        let requested = inv.inline_str("version").ok().map(str::to_string);
        let tag = requested.clone().unwrap_or_else(|| current_tag.clone());

        let iri = entry_iri(repo, &pr_rel(n), &view.head_oid, &tag);
        if let Some(entry) = load_entry(&config.store, &iri)? {
            return pr_explain_face(inv, repo, n, &entry, false);
        }
        if let Some(tag) = requested {
            return Err(Error::NotFound(format!(
                "browse: no archived explanation of pr {n} at version `{tag}` for head {}",
                view.head_oid
            )));
        }

        // Miss under the current tag: derive from the diff, archive, serve.
        let diff = diff_text(inv, root, n).await?;
        let prompt = format!(
            "{PR_PROMPT}\n\nRepository: {repo}\nPull request: #{} {}\nBranch: {} -> {}\n\n\
             ```diff\n{}\n```",
            view.number,
            view.title,
            view.head_ref,
            view.base_ref,
            truncate(&diff, config.max_prompt_bytes),
        );
        let text = ask(
            inv,
            config,
            &config.pr_provider,
            SYSTEM_PROMPT,
            &prompt,
            config.pr_max_tokens,
        )
        .await?;
        let entry = ArchiveEntry {
            iri,
            repo: repo.to_string(),
            rel: pr_rel(n),
            target_iri: pr_iri(repo, n),
            hash: view.head_oid.clone(),
            tag: current_tag,
            model,
            kind: "pr".to_string(),
            text,
            derived_at: inv.now().map(|t| iso8601(t.as_millis())),
        };
        store_entry(&config.store, &entry)?;
        pr_explain_face(inv, repo, n, &entry, true)
    }

    fn name(&self) -> &str {
        "browse-pr-explain"
    }

    fn describe(&self) -> Description {
        pr_explain_description()
    }
}

fn pr_explain_face(
    inv: &Invocation<'_>,
    repo: &str,
    n: u64,
    entry: &ArchiveEntry,
    derived: bool,
) -> Result<Representation> {
    match inv.inline_str("as").unwrap_or("text/plain") {
        t if t.starts_with("application/json") => Ok(repr(
            "application/json",
            serde_json::json!({
                "text": entry.text,
                "content_hash": entry.hash,
                "version_tag": entry.tag,
                "derived": derived,
                "model": entry.model,
                "prompt_kind": entry.kind,
                "about": entry.target_iri,
            })
            .to_string(),
        )),
        t if t.starts_with("text/html") => {
            let mut out = String::from("<div class=\"browse\">");
            out.push_str(&pr_crumbs(repo, Some(n), Some("explain")));
            out.push_str(&format!(
                "<nav class=\"browse-actions\"><button class=\"browse-view-link\" \
                 hx-get=\"/k/source {} as=text/html\" hx-target=\"#browse\" \
                 hx-swap=\"innerHTML\">view pull request</button></nav>",
                entry.target_iri,
            ));
            out.push_str("<div class=\"browse-explain\">");
            for paragraph in entry.text.split("\n\n").filter(|p| !p.trim().is_empty()) {
                out.push_str(&format!("<p>{}</p>", esc(paragraph.trim())));
            }
            out.push_str("</div>");
            let oid_short: String = entry.hash.chars().take(12).collect();
            out.push_str(&format!(
                "<p class=\"browse-provenance\">explained by {} · {} · head {}… · {}</p></div>",
                esc(&entry.model),
                esc(&entry.tag),
                esc(&oid_short),
                if derived {
                    "derived now"
                } else {
                    "from the archive"
                },
            ));
            Ok(repr_utf8("text/html", out))
        }
        t if t.starts_with("text/turtle") => Ok(repr("text/turtle", explain_turtle(entry))),
        _ => Ok(repr_utf8("text/plain", entry.text.clone())),
    }
}

/// `repo` is not an ArgSpec — see [`prs_description`]'s note.
fn pr_explain_description() -> Description {
    Description::new("browse-pr-explain")
        .title("Pull-request explanation (archived derivation)")
        .summary(
            "An LLM-derived, review-shaped explanation of a pull request's diff — \
             urn:repo:{repo}:pr:{n}:explain — what the change does and what a reviewer \
             would look at, ARCHIVED by (repo, pr, headRefOid, version-tag): a PR \
             explanation is of a COMMIT, so new commits derive fresh and prior entries \
             stay addressable (version= for an older tag at the same head). Data flows \
             through ikigai-repo's pr facades (urn:repo:pr:view json for the head oid, \
             urn:repo:pr:diff for the material), run in the root's directory — the \
             facades must be mounted and enforce their own exec capability. text/plain \
             (default) is the text; as=application/json adds {content_hash (the head \
             oid), version_tag, derived}; as=text/html the page face with provenance; \
             as=text/turtle the archive entry's graph.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .requires(CAP_NET)
        .input(
            ArgSpec::new("n")
                .binding()
                .class("http://www.w3.org/2001/XMLSchema#integer")
                .summary("the pull-request number"),
        )
        .input(ArgSpec::new("version").optional().summary(
            "an archived version tag (e.g. pr-v1@qwen3-coder:30b) instead of the current one",
        ))
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("the face to render")
                .one_of(["text/plain", "application/json", "text/html", "text/turtle"])
                .default_value("text/plain"),
        )
        .output("text/plain;charset=utf-8")
        .output("application/json")
        .output("text/html;charset=utf-8")
        .output("text/turtle")
}

// --- the review layer ---------------------------------------------------------

struct PrReviewEndpoint {
    roots: Roots,
    config: Arc<ExplainConfig>,
}

#[async_trait]
impl Endpoint for PrReviewEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "browse-pr-review does not support the {:?} verb",
                inv.request.verb
            )));
        }
        let (repo, root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        let n = pr_binding(inv)?;
        let config = &self.config;

        let view = view(inv, root, n).await?;
        // The diff is needed on BOTH paths: the anchor surface on a miss, the
        // drift-reconciliation surface on a hit.
        let diff = diff_text(inv, root, n).await?;
        let model = model_label(inv, &config.review_provider, &config.review_model_label).await;
        let tag = format!("{PR_REVIEW_PROMPT_VERSION}@{model}");

        let iri = pass_iri(repo, &pr_rel(n), &view.head_oid, &tag);
        if let Some(entry) = load_pass(&config.store, &iri)? {
            // The hit path mints NOTHING; the recorded set is reconciled
            // against the diff in hand.
            let included = annotate::included_for_ids(&config.store, &entry.minted, &diff)?;
            return pr_review_face(inv, repo, n, &entry, false, &included);
        }

        // Miss: derive one pass. Ask, parse, anchor in the diff, mint, archive.
        let prompt = format!(
            "{PR_REVIEW_PROMPT}\n\nRepository: {repo}\nPull request: #{} {}\n\n```diff\n{}\n```",
            view.number,
            view.title,
            truncate(&diff, config.max_prompt_bytes),
        );
        let answer = ask(
            inv,
            config,
            &config.review_provider,
            PR_REVIEW_SYSTEM_PROMPT,
            &prompt,
            config.review_max_tokens,
        )
        .await?;
        let (findings, malformed) = parse_findings(&answer);
        if findings.is_empty() {
            return Err(Error::Endpoint(format!(
                "browse: `{}` returned no parseable QUOTE:/NOTE: findings for pr {n} \
                 (max_tokens {} — thinking models may need a higher ceiling); nothing archived",
                config.review_provider, config.review_max_tokens
            )));
        }

        let target = pr_iri(repo, n);
        // The minted annotations key their drift on the DIFF TEXT's hash (the
        // anchor surface); the pass keys on the head oid (the commit).
        let diff_hash = annotate::content_hash(diff.as_bytes());
        let created = inv.now().map(|t| iso8601(t.as_millis()));
        let mut minted = Vec::new();
        let mut orphaned_items = malformed;
        for finding in &findings {
            match annotate::mint_review_annotation(
                &config.store,
                &target,
                repo,
                "",
                &diff,
                &diff_hash,
                &finding.quote,
                &finding.note,
                &model,
                &iri,
                created.clone(),
                annotate::Surface::Diff,
            )? {
                Some(annotation_iri) => minted.push(annotation_iri),
                None => orphaned_items += 1,
            }
        }
        minted.sort();
        if minted.is_empty() {
            return Err(Error::Endpoint(format!(
                "browse: none of the {} finding(s) for pr {n} anchored in the diff (every \
                 quote was misquoted); nothing archived",
                findings.len()
            )));
        }
        let entry = PassEntry {
            iri,
            repo: repo.to_string(),
            rel: pr_rel(n),
            target_iri: target,
            hash: view.head_oid.clone(),
            tag,
            model,
            minted,
            orphaned_items,
            derived_at: created,
        };
        store_pass(&config.store, &entry)?;
        let included = annotate::included_for_ids(&config.store, &entry.minted, &diff)?;
        pr_review_face(inv, repo, n, &entry, true, &included)
    }

    fn name(&self) -> &str {
        "browse-pr-review"
    }

    fn describe(&self) -> Description {
        pr_review_description()
    }
}

fn pr_review_face(
    inv: &Invocation<'_>,
    repo: &str,
    n: u64,
    entry: &PassEntry,
    derived: bool,
    included: &Included,
) -> Result<Representation> {
    match inv.inline_str("as").unwrap_or("text/plain") {
        t if t.starts_with("application/json") => Ok(repr(
            "application/json",
            serde_json::json!({
                "about": entry.target_iri,
                "content_hash": entry.hash,
                "version_tag": entry.tag,
                "model": entry.model,
                "derived": derived,
                "minted": entry.minted,
                "orphaned_items": entry.orphaned_items,
                "derived_at": entry.derived_at,
                "annotations": included.json(),
            })
            .to_string(),
        )),
        t if t.starts_with("text/html") => {
            let mut out = String::from("<div class=\"browse\">");
            out.push_str(&pr_crumbs(repo, Some(n), Some("review")));
            out.push_str(&format!(
                "<nav class=\"browse-actions\"><button class=\"browse-view-link\" \
                 hx-get=\"/k/source {} as=text/html\" hx-target=\"#browse\" \
                 hx-swap=\"innerHTML\">view pull request</button></nav>",
                entry.target_iri,
            ));
            out.push_str(&included.panel_html(None));
            let oid_short: String = entry.hash.chars().take(12).collect();
            let mut provenance = format!(
                "reviewed by {} · {} · head {}… · {}",
                esc(&entry.model),
                esc(&entry.tag),
                esc(&oid_short),
                if derived {
                    "derived now"
                } else {
                    "from the archive"
                },
            );
            if entry.orphaned_items > 0 {
                provenance.push_str(&format!(
                    " · {} item(s) did not anchor",
                    entry.orphaned_items
                ));
            }
            out.push_str(&format!(
                "<p class=\"browse-provenance\">{provenance}</p></div>"
            ));
            Ok(repr_utf8("text/html", out))
        }
        t if t.starts_with("text/turtle") => Ok(repr("text/turtle", pass_turtle(entry))),
        _ => {
            let mut out = format!(
                "review by {} · {} · {} finding(s)",
                entry.model,
                entry.tag,
                entry.minted.len()
            );
            if entry.orphaned_items > 0 {
                out.push_str(&format!(
                    " · {} item(s) did not anchor",
                    entry.orphaned_items
                ));
            }
            out.push('\n');
            out.push_str(&included.margin_text());
            Ok(repr_utf8("text/plain", out))
        }
    }
}

/// `repo` is not an ArgSpec — see [`prs_description`]'s note.
fn pr_review_description() -> Description {
    Description::new("browse-pr-review")
        .title("Machine review pass over a pull request's diff")
        .summary(
            "A region-grain machine review of one pull request — \
             urn:repo:{repo}:pr:{n}:review. Source asks the review model for findings on \
             the DIFF (each an exact quote plus a reviewer's note), mints every anchored \
             finding as a real urn:annotation: targeting the PR IRI (provenance: \
             dcterms:creator = the model, oa:motivatedBy oa:assessing, \
             prov:wasGeneratedBy = this pass) and ARCHIVES the pass by (repo, pr, \
             headRefOid, review-tag) — re-sourcing an unchanged head is an archive hit \
             that mints nothing; new commits are a fresh pass and the earlier pass's \
             annotations re-anchor or orphan as the diff drifts. Quotes that do not \
             anchor are counted (orphaned_items), never fatal. Data flows through \
             ikigai-repo's pr facades run in the root's directory — they must be mounted \
             and enforce their own exec capability. text/plain (default) is the \
             margin-notes digest; as=application/json adds {minted, orphaned_items, \
             annotations}; as=text/html the card page; as=text/turtle the pass's \
             provenance graph.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .requires(CAP_NET)
        .requires(CAP_ANNOTATE)
        .input(
            ArgSpec::new("n")
                .binding()
                .class("http://www.w3.org/2001/XMLSchema#integer")
                .summary("the pull-request number"),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("the face to render")
                .one_of(["text/plain", "application/json", "text/html", "text/turtle"])
                .default_value("text/plain"),
        )
        .output("text/plain;charset=utf-8")
        .output("application/json")
        .output("text/html;charset=utf-8")
        .output("text/turtle")
}

// --- tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{Capability, Exact, Fallback, FnEndpoint, Kernel};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-pr-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";
    const DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs\n\
         --- a/src/lib.rs\n\
         +++ b/src/lib.rs\n\
         @@ -1,3 +1,4 @@\n \
         fn alpha() {}\n\
         +fn beta() {}\n \
         fn gamma() {}\n";
    const LIST: &str =
        "3\tAdd beta\tfeature/beta\t2026-08-09T12:00:00Z\n7\tFix <gamma>\tfix/g\t2026-08-08T09:30:00Z\n";

    /// The mutable state behind the fake ikigai-repo facades, plus a log of
    /// every call's args — the kernel-resolution seam the tests stub, exactly
    /// as the explain tests stub the LLM.
    struct FakeRepo {
        list: Mutex<String>,
        view: Mutex<serde_json::Value>,
        diff: Mutex<String>,
        calls: Mutex<Vec<(String, BTreeMap<String, String>)>>,
    }

    impl FakeRepo {
        fn new() -> Arc<Self> {
            Arc::new(FakeRepo {
                list: Mutex::new(LIST.to_string()),
                view: Mutex::new(serde_json::json!({
                    "number": 3,
                    "title": "Add beta",
                    "state": "OPEN",
                    "isDraft": false,
                    "headRefName": "feature/beta",
                    "headRefOid": OID,
                    "baseRefName": "main",
                    "author": {"login": "alice", "name": "Alice", "is_bot": false},
                    "updatedAt": "2026-08-09T12:00:00Z",
                    "url": "https://github.com/demo/demo/pull/3",
                    "body": "Adds beta between alpha and gamma.",
                })),
                diff: Mutex::new(DIFF.to_string()),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn set_diff(&self, diff: &str) {
            *self.diff.lock().unwrap() = diff.to_string();
        }

        fn set_oid(&self, oid: &str) {
            self.view.lock().unwrap()["headRefOid"] = serde_json::Value::String(oid.to_string());
        }

        fn calls_to(&self, iri: &str) -> Vec<BTreeMap<String, String>> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(i, _)| i == iri)
                .map(|(_, args)| args.clone())
                .collect()
        }
    }

    /// Bind the three pr facades as deterministic fakes. Each declares
    /// `urn:cap:exec:gh` like the real ikigai-repo module, so attenuation is
    /// exercised for real.
    fn fake_repo_space(state: &Arc<FakeRepo>) -> EndpointSpace {
        let mut space = EndpointSpace::new();
        for iri in [FACADE_LIST, FACADE_VIEW, FACADE_DIFF] {
            let state = Arc::clone(state);
            space = space.bind(
                Exact::new(iri),
                FnEndpoint::new("fake-repo", move |inv: &Invocation<'_>| {
                    let mut args = BTreeMap::new();
                    for name in ["pr", "dir", "as", "repo", "state", "limit"] {
                        if let Ok(value) = inv.inline_str(name) {
                            args.insert(name.to_string(), value.to_string());
                        }
                    }
                    let want_json = args
                        .get("as")
                        .is_some_and(|a| a.starts_with("application/json"));
                    state.calls.lock().unwrap().push((iri.to_string(), args));
                    match iri {
                        FACADE_LIST if want_json => Ok(repr(
                            "application/json",
                            "[{\"number\":3,\"title\":\"Add beta\"}]".to_string(),
                        )),
                        FACADE_LIST => {
                            Ok(repr_utf8("text/plain", state.list.lock().unwrap().clone()))
                        }
                        FACADE_VIEW => Ok(repr(
                            "application/json",
                            state.view.lock().unwrap().to_string(),
                        )),
                        _ => Ok(repr_utf8("text/plain", state.diff.lock().unwrap().clone())),
                    }
                })
                .with_description(
                    Description::new("fake-repo")
                        .verb(Verb::Source)
                        .requires("urn:cap:exec:gh"),
                ),
            );
        }
        space
    }

    #[derive(Default)]
    struct Log {
        asks: Mutex<Vec<(String, String, String)>>, // (prompt, system, max_tokens)
    }

    impl Log {
        fn count(&self) -> usize {
            self.asks.lock().unwrap().len()
        }
        fn last(&self) -> (String, String, String) {
            self.asks.lock().unwrap().last().unwrap().clone()
        }
    }

    /// The coder-tier fake (both the pr explain and the review pass default
    /// to `urn:llm:coder:ask`).
    fn fake_llm_space(log: &Arc<Log>, reply: &str) -> EndpointSpace {
        let log = Arc::clone(log);
        let reply = reply.to_string();
        EndpointSpace::new().bind(
            Exact::new("urn:llm:coder:ask"),
            FnEndpoint::new("fake-llm", move |inv: &Invocation<'_>| {
                log.asks.lock().unwrap().push((
                    inv.inline_str("prompt").unwrap_or("").to_string(),
                    inv.inline_str("system").unwrap_or("").to_string(),
                    inv.inline_str("max_tokens").unwrap_or("").to_string(),
                ));
                Ok(repr_utf8("text/plain", reply.clone()))
            })
            .with_description(
                Description::new("fake-llm")
                    .verb(Verb::Source)
                    .requires(CAP_NET),
            ),
        )
    }

    /// Pages only (no LLM): space_with_annotations over the shared store,
    /// composed with the fake facades.
    fn pages_kernel(root: &Path, store: &Arc<Store>, state: &Arc<FakeRepo>) -> Kernel {
        let browse = crate::space_with_annotations(
            vec![("demo".to_string(), root.to_path_buf())],
            Arc::clone(store),
        );
        Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(fake_repo_space(state)),
        ])))
    }

    /// The full stack: space_with_explain + fake facades + fake coder LLM.
    fn explain_kernel(
        root: &Path,
        store: &Arc<Store>,
        state: &Arc<FakeRepo>,
        log: &Arc<Log>,
        reply: &str,
    ) -> Kernel {
        let cfg = ExplainConfig::new(Arc::clone(store))
            .pr_model_label("p1")
            .review_model_label("r1");
        let browse = crate::space_with_explain(vec![("demo".to_string(), root.to_path_buf())], cfg);
        Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(fake_repo_space(state)),
            Arc::new(fake_llm_space(log, reply)),
        ])))
    }

    fn cap() -> Capability {
        Capability::scoped([
            "urn:cap:browse:read:demo",
            "urn:cap:exec:gh",
            "urn:cap:net:localhost",
            CAP_ANNOTATE,
        ])
    }

    fn issue(
        kernel: &Kernel,
        verb: Verb,
        iri: &str,
        args: &[(&str, &str)],
        cap: &Capability,
    ) -> Result<Representation> {
        let mut request = Request::new(verb, Iri::parse(iri).unwrap());
        for (k, v) in args {
            request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
        }
        block_on(kernel.issue(request, cap))
    }

    fn source(kernel: &Kernel, iri: &str, args: &[(&str, &str)]) -> Result<Representation> {
        issue(kernel, Verb::Source, iri, args, &cap())
    }

    fn body(repr: &Representation) -> String {
        String::from_utf8_lossy(&repr.bytes).into_owned()
    }

    fn json(kernel: &Kernel, iri: &str, extra: &[(&str, &str)]) -> serde_json::Value {
        let mut args = vec![("as", "application/json")];
        args.extend_from_slice(extra);
        serde_json::from_str(&body(&source(kernel, iri, &args).unwrap())).unwrap()
    }

    #[test]
    fn the_listing_serves_the_facade_faces_and_runs_in_the_roots_directory() {
        let root = temp_dir();
        let store = Arc::new(Store::new().unwrap());
        let state = FakeRepo::new();
        let k = pages_kernel(&root, &store, &state);

        // text/plain is the facade's text contract, untouched.
        let text = source(&k, "urn:repo:demo:prs", &[]).unwrap();
        assert_eq!(body(&text), LIST);
        // The facade ran in the CONFIGURED ROOT's directory — dir= was passed.
        let calls = state.calls_to(FACADE_LIST);
        assert_eq!(
            calls[0].get("dir"),
            Some(&root.to_string_lossy().into_owned())
        );

        // json passes the facade's structured export through.
        let rows = json(&k, "urn:repo:demo:prs", &[]);
        assert_eq!(rows[0]["number"], 3);

        // The html face links each PR's page; markup is escaped.
        let html = body(&source(&k, "urn:repo:demo:prs", &[("as", "text/html")]).unwrap());
        assert!(
            html.contains("hx-get=\"/k/source urn:repo:demo:pr:3 as=text/html\""),
            "{html}"
        );
        assert!(html.contains("#7 Fix &lt;gamma&gt;"), "{html}");
        assert!(html.contains("feature/beta"), "{html}");

        // The 0.1.4 facade args are forwarded ONLY when supplied: the calls
        // above carried neither, this one carries both — and the fifth
        // (state) column renders as a badge.
        assert!(!calls[0].contains_key("state"), "{:?}", calls[0]);
        assert!(!calls[0].contains_key("limit"), "{:?}", calls[0]);
        *state.list.lock().unwrap() =
            "3\tAdd beta\tfeature/beta\t2026-08-09T12:00:00Z\tmerged\n".to_string();
        let html = body(
            &source(
                &k,
                "urn:repo:demo:prs",
                &[("as", "text/html"), ("state", "all"), ("limit", "10")],
            )
            .unwrap(),
        );
        assert!(
            html.contains("<span class=\"browse-pr-state\">merged</span>"),
            "{html}"
        );
        let forwarded = state.calls_to(FACADE_LIST).pop().unwrap();
        assert_eq!(forwarded.get("state"), Some(&"all".to_string()));
        assert_eq!(forwarded.get("limit"), Some(&"10".to_string()));

        // chrome=embed is the rows-only fragment — no crumbs, no wrapper.
        let embedded = body(
            &source(
                &k,
                "urn:repo:demo:prs",
                &[("as", "text/html"), ("chrome", "embed")],
            )
            .unwrap(),
        );
        assert!(
            embedded.starts_with("<ul class=\"browse-prs\">"),
            "{embedded}"
        );
        assert!(!embedded.contains("browse-crumbs"), "{embedded}");

        // No PRs: empty text passthrough, a friendly html empty state.
        state.list.lock().unwrap().clear();
        assert_eq!(body(&source(&k, "urn:repo:demo:prs", &[]).unwrap()), "");
        let html = body(&source(&k, "urn:repo:demo:prs", &[("as", "text/html")]).unwrap());
        assert!(html.contains("no pull requests"), "{html}");

        // The root tree's html face links the listing AND lazy-loads the
        // recent-PRs block (hx-trigger=load, embed chrome, the 0.1.4 args) —
        // the tree itself renders without ever consulting the facades.
        let html = body(&source(&k, "urn:repo:demo:tree", &[("as", "text/html")]).unwrap());
        assert!(
            html.contains("hx-get=\"/k/source urn:repo:demo:prs as=text/html\""),
            "{html}"
        );
        assert!(
            html.contains(
                "hx-get=\"/k/source urn:repo:demo:prs state=all limit=10 chrome=embed \
                 as=text/html\" hx-trigger=\"load\""
            ),
            "{html}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn absent_facades_are_a_typed_not_found_naming_the_gap() {
        let root = temp_dir();
        let store = Arc::new(Store::new().unwrap());
        // No fake facade space in the composition at all.
        let k = Kernel::new(Arc::new(crate::space_with_annotations(
            vec![("demo".to_string(), root.to_path_buf())],
            Arc::clone(&store),
        )));
        for iri in ["urn:repo:demo:prs", "urn:repo:demo:pr:3"] {
            let err = source(&k, iri, &[]).unwrap_err();
            assert!(matches!(err, Error::NotFound(_)), "{iri}: {err:?}");
            assert!(format!("{err:?}").contains("ikigai-repo"), "{err:?}");
        }
        // An unconfigured repo stays a clean resolution MISS, as everywhere.
        let err = source(&k, "urn:repo:nope:prs", &[]).unwrap_err();
        assert!(matches!(err, Error::Unresolved(_)), "{err:?}");
        // A non-numeric PR segment is a typed argument error, not a panic.
        let err = source(&k, "urn:repo:demo:pr:abc", &[]).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_pr_page_renders_metadata_and_diff_across_faces() {
        let root = temp_dir();
        let store = Arc::new(Store::new().unwrap());
        let state = FakeRepo::new();
        let k = pages_kernel(&root, &store, &state);

        // Plain: the metadata header, then the diff verbatim.
        let text = body(&source(&k, "urn:repo:demo:pr:3", &[]).unwrap());
        assert!(text.starts_with("#3 Add beta\n"), "{text}");
        assert!(
            text.contains("open · alice · feature/beta → main · updated 2026-08-09T12:00:00Z"),
            "{text}"
        );
        assert!(text.contains(&format!("head {OID}")), "{text}");
        assert!(text.ends_with(DIFF), "{text}");

        // json: the structured record — author is an OBJECT, the head oid is
        // the archive key downstream layers use.
        let record = json(&k, "urn:repo:demo:pr:3", &[]);
        assert_eq!(record["number"], 3);
        assert_eq!(record["head_ref_oid"], OID);
        assert_eq!(record["author"]["login"], "alice");
        assert_eq!(record["author"]["is_bot"], false);
        assert_eq!(record["diff"], DIFF);

        // html: highlighted, line-anchored diff + the annotate form targeting
        // THIS PR + the way back to the listing.
        let html = body(&source(&k, "urn:repo:demo:pr:3", &[("as", "text/html")]).unwrap());
        assert!(html.contains("#3 Add beta"), "{html}");
        assert!(html.contains("browse-code"), "{html}");
        // The crumb trail: home affordance, repo and prs as live crumbs,
        // the PR number inert.
        assert!(
            html.contains("<a class=\"browse-home-link\" href=\"/\""),
            "{html}"
        );
        assert!(
            html.contains("<span class=\"browse-here\">#3</span>"),
            "{html}"
        );
        assert!(html.contains("id=\"L6\""), "{html}");
        assert!(
            html.contains("name=\"target\" value=\"urn:repo:demo:pr:3\""),
            "{html}"
        );
        assert!(
            html.contains("hx-get=\"/k/source urn:repo:demo:prs as=text/html\""),
            "{html}"
        );
        // View passed pr= and dir=; both facades were asked.
        assert_eq!(
            state.calls_to(FACADE_VIEW)[0].get("pr"),
            Some(&"3".to_string())
        );
        assert_eq!(
            state.calls_to(FACADE_DIFF)[0].get("dir"),
            Some(&root.to_string_lossy().into_owned())
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn pr_annotations_anchor_in_the_diff_and_drift_with_it() {
        let root = temp_dir();
        let store = Arc::new(Store::new().unwrap());
        let state = FakeRepo::new();
        let k = pages_kernel(&root, &store, &state);

        // A human note on the added line, targeting the PR IRI itself.
        let ack = issue(
            &k,
            Verb::Sink,
            "urn:annotation:pr-note",
            &[
                ("target", "urn:repo:demo:pr:3"),
                ("exact", "+fn beta() {}"),
                ("body", "the new entry point"),
                ("as", "application/json"),
            ],
            &cap(),
        )
        .unwrap();
        let created: serde_json::Value = serde_json::from_str(&body(&ack)).unwrap();
        assert_eq!(created["annotates"], "urn:repo:demo:pr:3");
        assert_eq!(created["pr"], 3);
        assert_eq!(created["path"], "");
        assert_eq!(created["line"], 6, "the diff line the quote sits on");

        // The page html marks the annotated diff line and renders the card.
        let html = body(&source(&k, "urn:repo:demo:pr:3", &[("as", "text/html")]).unwrap());
        assert!(html.contains("browse-line-annotated"), "{html}");
        assert!(html.contains("the new entry point"), "{html}");

        // annotations=include folds the margin into the plain and json faces.
        let text = body(&source(&k, "urn:repo:demo:pr:3", &[("annotations", "include")]).unwrap());
        assert!(text.contains("--- annotations (1) ---"), "{text}");
        assert!(
            text.contains("\"+fn beta() {}\" -- the new entry point"),
            "{text}"
        );
        let record = json(&k, "urn:repo:demo:pr:3", &[("annotations", "include")]);
        assert_eq!(record["annotations"][0]["exact"], "+fn beta() {}");

        // The repo-wide listing carries it; a path-scoped listing of a file
        // does not (a PR annotation lives under no path).
        let all = json(&k, "urn:repo:demo:annotations", &[]);
        assert_eq!(all.as_array().unwrap().len(), 1);
        assert_eq!(all[0]["pr"], 3);
        std::fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
        let scoped = json(&k, "urn:repo:demo:annotations:a.rs", &[]);
        assert_eq!(scoped.as_array().unwrap().len(), 0);

        // The diff drifts (a line lands above): the annotation re-anchors.
        let drifted = format!("--- preamble line\n{DIFF}");
        state.set_diff(&drifted);
        let rows = json(&k, "urn:repo:demo:annotations", &[]);
        assert_eq!(rows[0]["reanchored"], true);
        assert_eq!(rows[0]["line"], 7);

        // A later head flips the line's marker (the addition settles into
        // context): the marker-stripped retry follows it, and the stored
        // exact becomes the NEW original diff line — drift stays honest.
        state.set_diff(&DIFF.replace("+fn beta() {}", " fn beta() {}"));
        let rows = json(&k, "urn:repo:demo:annotations", &[]);
        assert_eq!(rows[0]["exact"], " fn beta() {}");
        assert_eq!(rows[0]["reanchored"], true);
        assert_eq!(rows[0]["orphaned"], false);

        // The quote vanishes entirely: orphaned, never dropped.
        state.set_diff("diff --git a/x b/x\n+++ b/x\n+nothing left\n");
        let rows = json(&k, "urn:repo:demo:annotations", &[]);
        assert_eq!(rows[0]["orphaned"], true);

        // Facades gone (a composition without ikigai-repo): the annotation
        // still serves, AS RECORDED — "could not look" must not re-flag.
        let bare = Kernel::new(Arc::new(crate::space_with_annotations(
            vec![("demo".to_string(), root.to_path_buf())],
            Arc::clone(&store),
        )));
        let row: serde_json::Value = serde_json::from_str(&body(
            &issue(
                &bare,
                Verb::Source,
                "urn:annotation:pr-note",
                &[("as", "application/json")],
                &cap(),
            )
            .unwrap(),
        ))
        .unwrap();
        assert_eq!(row["body"], "the new entry point");
        assert_eq!(row["orphaned"], true, "the recorded flag, untouched");
        assert_eq!(row["line"], serde_json::Value::Null);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_pr_explanation_archives_on_the_head_commit() {
        let root = temp_dir();
        let store = Arc::new(Store::new().unwrap());
        let state = FakeRepo::new();
        let log = Arc::new(Log::default());
        let k = explain_kernel(&root, &store, &state, &log, "This PR adds beta.");

        let first = json(&k, "urn:repo:demo:pr:3:explain", &[]);
        assert_eq!(first["text"], "This PR adds beta.");
        assert_eq!(first["derived"], true);
        assert_eq!(first["version_tag"], "pr-v1@p1");
        assert_eq!(first["content_hash"], OID, "keyed on the head commit");
        assert_eq!(first["about"], "urn:repo:demo:pr:3");
        assert_eq!(first["prompt_kind"], "pr");
        assert_eq!(log.count(), 1);

        // The prompt is review-shaped and carries the diff and the metadata.
        let (prompt, system, max_tokens) = log.last();
        assert!(
            prompt.contains("what would a careful reviewer look at"),
            "{prompt}"
        );
        assert!(prompt.contains("+fn beta() {}"), "{prompt}");
        assert!(prompt.contains("Pull request: #3 Add beta"), "{prompt}");
        assert!(system.contains("explanation layer"), "{system}");
        assert_eq!(max_tokens, "600");

        // Same head: an archive hit, no new ask.
        let hit = json(&k, "urn:repo:demo:pr:3:explain", &[]);
        assert_eq!(hit["derived"], false);
        assert_eq!(log.count(), 1, "the hit must not re-derive");

        // A new commit re-keys: fresh derivation.
        state.set_oid("fedcba9876543210fedcba9876543210fedcba98");
        state.set_diff(&format!("{DIFF}+fn delta() {{}}\n"));
        let fresh = json(&k, "urn:repo:demo:pr:3:explain", &[]);
        assert_eq!(fresh["derived"], true);
        assert_eq!(log.count(), 2);

        // An unknown version tag is a typed miss.
        let err = source(&k, "urn:repo:demo:pr:3:explain", &[("version", "pr-v9@x")]).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");

        // The turtle face is the archive entry's graph.
        let ttl =
            body(&source(&k, "urn:repo:demo:pr:3:explain", &[("as", "text/turtle")]).unwrap());
        assert!(ttl.contains("a ik:Explanation"), "{ttl}");
        assert!(ttl.contains("ik:about <urn:repo:demo:pr:3>"), "{ttl}");
        std::fs::remove_dir_all(&root).ok();
    }

    const TWO_FINDINGS: &str = "QUOTE: +fn beta() {}\nNOTE: The new function lands without a \
         caller - is it wired anywhere?\nQUOTE: @@ -1,3 +1,4 @@\nNOTE: A tight, single-hunk \
         change - easy to review.\n";

    #[test]
    fn the_review_pass_mints_machine_annotations_anchored_in_the_diff() {
        let root = temp_dir();
        let store = Arc::new(Store::new().unwrap());
        let state = FakeRepo::new();
        let log = Arc::new(Log::default());
        let k = explain_kernel(&root, &store, &state, &log, TWO_FINDINGS);

        let pass = json(&k, "urn:repo:demo:pr:3:review", &[]);
        assert_eq!(pass["derived"], true);
        assert_eq!(pass["version_tag"], "pr-review-v2@r1");
        assert_eq!(
            pass["content_hash"], OID,
            "the pass keys on the head commit"
        );
        assert_eq!(pass["about"], "urn:repo:demo:pr:3");
        assert_eq!(pass["minted"].as_array().unwrap().len(), 2);
        assert_eq!(pass["orphaned_items"], 0);
        assert_eq!(log.count(), 1);
        let (prompt, system, _) = log.last();
        assert!(prompt.contains("pull request's diff"), "{prompt}");
        assert!(system.contains("colleague's pull request"), "{system}");

        // The findings ARE annotations targeting the PR IRI — machine-marked,
        // provenance-linked, in the one shared family.
        let rows = json(&k, "urn:repo:demo:annotations", &[]);
        assert_eq!(rows.as_array().unwrap().len(), 2);
        assert_eq!(rows[0]["annotates"], "urn:repo:demo:pr:3");
        assert_eq!(rows[0]["machine"], true);
        assert_eq!(rows[0]["creator"], "r1");
        assert_eq!(rows[0]["motivation"], "assessing");
        assert!(rows[0]["generated_by"]
            .as_str()
            .unwrap()
            .starts_with("urn:ikigai:browse:review:demo:"));

        // A hit mints nothing and re-asks nothing.
        let hit = json(&k, "urn:repo:demo:pr:3:review", &[]);
        assert_eq!(hit["derived"], false);
        assert_eq!(hit["minted"], pass["minted"]);
        assert_eq!(log.count(), 1);
        assert_eq!(
            json(&k, "urn:repo:demo:annotations", &[])
                .as_array()
                .unwrap()
                .len(),
            2
        );

        // The page html shows the machine markers on the diff lines.
        let html = body(&source(&k, "urn:repo:demo:pr:3", &[("as", "text/html")]).unwrap());
        assert!(html.contains("browse-annotation-marker-machine"), "{html}");
        assert!(html.contains("review by r1"), "{html}");

        // The pass's turtle records the provenance chain.
        let ttl = body(&source(&k, "urn:repo:demo:pr:3:review", &[("as", "text/turtle")]).unwrap());
        assert!(ttl.contains("a ik:Review"), "{ttl}");
        assert!(ttl.contains("prov:used <urn:repo:demo:pr:3>"), "{ttl}");
        assert!(ttl.contains("prov:generated <urn:annotation:"), "{ttl}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A realistic diff for the anchoring fix: change lines carry `+`/`-`
    /// markers over indented code, context lines a leading space — the
    /// surface every one of the live pass's six quotes misquoted against.
    // Leading whitespace is load-bearing (context lines start with a space,
    // code carries indentation) — embedded newlines, no `\` continuations.
    const REAL_DIFF: &str = "diff --git a/src/config.rs b/src/config.rs
index 3f9c2d1..8a41b77 100644
--- a/src/config.rs
+++ b/src/config.rs
@@ -10,7 +10,9 @@ impl Config {
     pub fn load(path: &Path) -> Result<Config> {
-        let text = std::fs::read_to_string(path)?;
+        let text = std::fs::read_to_string(path)
+            .with_context(|| format!(\"reading {}\", path.display()))?;
         toml::from_str(&text).map_err(Into::into)
     }
 }
";

    #[test]
    fn model_style_quotes_anchor_in_the_marker_stripped_diff() {
        let root = temp_dir();
        let store = Arc::new(Store::new().unwrap());
        let state = FakeRepo::new();
        let log = Arc::new(Log::default());
        // Three model-style findings against REAL_DIFF:
        //   1. marker-faithful (the v2 prompt obeyed) — anchors exactly;
        //   2. a WRONG marker (`+` on what is a context line) — anchors via
        //      the marker-stripped retry, storing the original context line;
        //   3. no marker at all (the v1 live failure's shape) — a plain
        //      substring of its diff line, anchoring exactly.
        let findings = "QUOTE: -        let text = std::fs::read_to_string(path)?;\n\
             NOTE: The bare ? lost the path - good riddance.\n\
             QUOTE: +toml::from_str(&text).map_err(Into::into)\n\
             NOTE: Parse errors still lack the path context the read now carries.\n\
             QUOTE: pub fn load(path: &Path) -> Result<Config> {\n\
             NOTE: Loading stays sync - fine at config scale.\n";
        let k = explain_kernel(&root, &store, &state, &log, findings);
        state.set_diff(REAL_DIFF);

        let pass = json(&k, "urn:repo:demo:pr:3:review", &[]);
        assert_eq!(pass["derived"], true);
        assert_eq!(
            pass["minted"].as_array().unwrap().len(),
            3,
            "every model-style quote anchors: {pass}"
        );
        assert_eq!(pass["orphaned_items"], 0);

        // The stored exacts stay honest: a stripped match records the
        // ORIGINAL diff line (marker and indentation intact); exact matches
        // keep the model's own quote.
        let rows = json(&k, "urn:repo:demo:annotations", &[]);
        let exacts: Vec<&str> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["exact"].as_str().unwrap())
            .collect();
        assert!(
            exacts.contains(&"pub fn load(path: &Path) -> Result<Config> {"),
            "{exacts:?}"
        );
        assert!(
            exacts.contains(&"-        let text = std::fs::read_to_string(path)?;"),
            "{exacts:?}"
        );
        assert!(
            exacts.contains(&"         toml::from_str(&text).map_err(Into::into)"),
            "{exacts:?}"
        );

        // A human Sink quoting consecutive CODE lines (no markers, the way
        // anyone reads a diff): the marker-stripped shadow carries the match
        // across the interleaved `+` columns, and the stored exact is the
        // original two diff lines.
        let ack = issue(
            &k,
            Verb::Sink,
            "urn:annotation:pr-multiline",
            &[
                ("target", "urn:repo:demo:pr:3"),
                (
                    "exact",
                    "        let text = std::fs::read_to_string(path)\n            \
                     .with_context(|| format!(\"reading {}\", path.display()))?;",
                ),
                ("body", "the two-line read"),
                ("as", "application/json"),
            ],
            &cap(),
        )
        .unwrap();
        let created: serde_json::Value = serde_json::from_str(&body(&ack)).unwrap();
        assert_eq!(created["line"], 8, "anchored at the first spanned line");
        assert_eq!(
            created["exact"],
            "+        let text = std::fs::read_to_string(path)\n\
             +            .with_context(|| format!(\"reading {}\", path.display()))?;"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn pr_capabilities_are_declared_and_enforced() {
        let root = temp_dir();
        let store = Arc::new(Store::new().unwrap());
        let state = FakeRepo::new();
        let log = Arc::new(Log::default());
        let k = explain_kernel(&root, &store, &state, &log, TWO_FINDINGS);

        // No browse grant on this root: denied by the per-root check.
        let wrong_root = Capability::scoped([
            "urn:cap:browse:read:other",
            "urn:cap:exec:gh",
            "urn:cap:net:localhost",
            CAP_ANNOTATE,
        ]);
        for iri in ["urn:repo:demo:prs", "urn:repo:demo:pr:3"] {
            let err = issue(&k, Verb::Source, iri, &[], &wrong_root).unwrap_err();
            assert!(matches!(err, Error::Denied(_)), "{iri}: {err:?}");
        }

        // No exec grant: the FACADE's own enforcement surfaces through
        // attenuation when the sub-request dispatches.
        let no_exec = Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:net:localhost"]);
        let err = issue(&k, Verb::Source, "urn:repo:demo:prs", &[], &no_exec).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");

        // No net grant: the explain layer is denied at the baseline before
        // any ask leaves.
        let no_net = Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:exec:gh"]);
        let err = issue(&k, Verb::Source, "urn:repo:demo:pr:3:explain", &[], &no_net).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert_eq!(log.count(), 0, "no ask ever left");

        // The declared contracts: pages need the browse wildcard; explain
        // adds net; review adds net + annotate. `n` is the only binding (the
        // root is fixed per row, never an ArgSpec).
        let page = PrEndpoint {
            roots: Arc::new(std::collections::BTreeMap::from([(
                "demo".to_string(),
                root.clone(),
            )])),
            store: None,
            explain: false,
        };
        let description = page.describe();
        assert_eq!(description.requires, vec![CAP_WILDCARD.to_string()]);
        let names: Vec<&str> = description.inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["n", "as"]);
        let explain_desc = pr_explain_description();
        for cap in [CAP_WILDCARD, CAP_NET] {
            assert!(explain_desc.requires.contains(&cap.to_string()), "{cap}");
        }
        let review_desc = pr_review_description();
        for cap in [CAP_WILDCARD, CAP_NET, CAP_ANNOTATE] {
            assert!(review_desc.requires.contains(&cap.to_string()), "{cap}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn pr_rows_are_manifold_citizens_and_derived_rows_are_not_shadowed() {
        use ikigai_core::ActionQuery;
        let root = temp_dir();
        let store = Arc::new(Store::new().unwrap());
        let state = FakeRepo::new();
        let log = Arc::new(Log::default());
        let k = explain_kernel(&root, &store, &state, &log, "text");

        // The rows survive probe-expansion: all four appear in the manifold.
        let query = ActionQuery {
            capability: Some(&cap()),
            ..Default::default()
        };
        let offered: std::collections::BTreeSet<String> = k
            .select_actions(&query)
            .into_iter()
            .map(|m| m.endpoint)
            .collect();
        for row in [
            "urn:repo:demo:prs",
            "urn:repo:demo:pr:{n}",
            "urn:repo:demo:pr:{n}:explain",
            "urn:repo:demo:pr:{n}:review",
        ] {
            assert!(offered.contains(row), "missing {row}: {offered:#?}");
        }
        // And the page's trailing-{n} template does NOT swallow the derived
        // rows at resolution time (the colon guard).
        let out = json(&k, "urn:repo:demo:pr:3:explain", &[]);
        assert_eq!(out["prompt_kind"], "pr");
        std::fs::remove_dir_all(&root).ok();
    }
}
