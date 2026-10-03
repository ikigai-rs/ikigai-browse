//! gonk's HTML face: pages, htmx fragments, the form adapter, SPARQL, passkeys, assets.
//!
//! ```text
//! /                          urn:iki:gonk:page:home                 Source  the first readable ledger
//! /l/{ledger}                urn:iki:gonk:page:ledger:{ledger}      Source  a ledger, filtered
//! /l/{ledger}/items          urn:iki:gonk:fragment:items:{ledger}   Source  the same, as a fragment
//! /l/{ledger}/item/{id}      urn:iki:gonk:page:item:{ledger}:{id}   Source  one item
//! /l/{ledger}/item/{id}/card urn:iki:gonk:fragment:item:{…}:{id}    Source  the same, as a fragment
//! /act                       urn:iki:gonk:act                       Sink    a form → one ledger action
//! /sparql                    urn:iki:gonk:sparql                    Source  editor, or results by conneg
//! /sparql/results            urn:iki:gonk:fragment:sparql           Source  results as a fragment
//! /auth/{op}                 urn:iki:gonk:passkey:{op}              Sink    passkey ceremonies, sessions
//! /static/{name}             urn:iki:gonk:asset:{name}              Source  css, js, htmx
//! /k?c={command}             urn:iki:gonk:k                         Source/Sink  the browse faces' adapter
//! /browse/{iri}              urn:iki:gonk:page:browse:{iri}         Source  the page they render inside
//! ```
//!
//! The last two are [`crate::k`] — the door `ikigai-browse`'s HTML faces are authored
//! against. They are pages of this face and bound with the rest of it; what is different
//! about them, and why the command travels as an argument here, is that module's own doc.
//!
//! These are bound ONLY in the HTTP door's kernel ([`crate::doors::http_kernel`]): the socket
//! and QUIC doors serve exactly the hub's catalog, as before.
//!
//! # ★ Every page is a transform of a graph face
//!
//! A ledger page is `urn:iki:ledger:{ledger}:items as=text/turtle`; an item page is
//! `…:item:{id} as=text/turtle`. Both are issued under the CALLER's capability, re-serialized
//! as RDF/XML and rendered by one stylesheet keyed on `rdf:type` ([`crate::render`]). Nothing
//! here reads the store directly or builds a page from anything but those graphs and the
//! view triples added to them.
//!
//! # ★ Every form issues an action the manifold already offers
//!
//! htmx posts a form as `application/x-www-form-urlencoded`, and `ikigai-web` hands that body
//! to one resource as `content` — so a form cannot post straight to
//! `urn:iki:ledger:close`, whose `item` must arrive as an argument. `Act` is the adapter,
//! and it is deliberately unable to do anything the ledger does not already declare:
//!
//! - the target is always `urn:iki:ledger:{ledger}:{action}` (or `:item:{id}`), built by
//!   `ikigai-ledger`'s own naming, never a free IRI;
//! - the verb must be one the target DESCRIBES, and every argument a name that verb's
//!   `ActionSpec` DECLARES — checked against the hub's live `describe()`, so a form field the
//!   manifold does not know is refused rather than dropped;
//! - it runs under the caller's capability, so the ledger's own checks decide.
//!
//! That is the same contract a manifold-driven form renderer will need later; this adapter is
//! what it will post to.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, InputSource, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Result, UriTemplate, Verb,
};
use ikigai_ledger::Ledger;
use oxigraph::model::NamedOrBlankNode;
use serde_json::{json, Value};

use crate::identity::{self, Passkeys, Purpose};
use crate::render::{self, element, envelope, Graph};
use crate::rules::{self, Rules};

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const LEDGER_NS: &str = "https://ikigai-rs.dev/ns/ledger#";
const DCTERMS: &str = "http://purl.org/dc/terms/";
pub(crate) const HTML: &str = "text/html";

/// How many rows a ledger listing RENDERS unless the caller asks for more.
///
/// ★ **This is a latency bound and the number is measured, not chosen.** `xrust` builds the
/// result tree node by node and the cost is in what it WRITES, not what it reads: the list
/// page costs ~150 ms of chrome plus ~6 ms per row at fifty rows and ~15 ms per row at four
/// hundred — 410 rows measured 6.5 s, which is the whole of the ~6 s page (parsing the
/// Turtle, adding the view triples and serializing RDF/XML together cost 70 ms of it).
/// Handing the stylesheet a SMALLER GRAPH does not help — a 3.4× smaller input moved the
/// same render by 3% — so the only lever is fewer rows. `examples/render-cost.rs` is how to
/// take those numbers again; ledger #443 carries the first set.
const ROWS: usize = 50;

/// The ceiling on one render, whatever `limit` asks for — about eight seconds' worth.
/// A caller who wants the whole ledger wants the Turtle face, not this page.
const MAX_ROWS: usize = 500;

/// How many items the listing READS in order to count them.
///
/// The read is cheap — 411 items is 0.09 s, two orders off the render — so the page reads
/// the whole filtered set and counts it, and bounds only the rendering. That is what lets
/// the banner say "the 50 most recently updated of 411" instead of reporting a page as a
/// total, which is the mistake ledger #419 records on the text face. A set that fills this
/// cap is reported as a floor (`411+`), never as a total.
const COUNT_CAP: usize = 2000;

/// What the HTML face is built from.
pub struct Web {
    /// The hub — for `describe()` in `Act`. Every REQUEST goes through the invocation.
    pub hub: Arc<Kernel>,
    /// The ledgers listed first: `gonk.http.ledger`.
    pub ledgers: Vec<String>,
    /// The browse roots this server was configured with (`gonk.browse.root`), by NAME —
    /// the `{root}` in `urn:repo:{root}:tree`. Empty when no root is configured, in which
    /// case `main` wires no browse space at all and the header offers no way in.
    ///
    /// ⚠ The names only. The paths are the mount's business ([`crate::browse::wire`]);
    /// a page never needs one, and holding one here would put a filesystem path one
    /// rendering mistake away from a caller who may not read the root it belongs to.
    pub browse_roots: Vec<String>,
    // ★ **No `review` field, and its absence is the shape of ledger
    // [#466](http://localhost:1060/l/default/item/466)'s fix.** This face used to hold the
    // trigger so the Queue page could count its inbox directly — the only way to reach a
    // number that lived behind `urn:cap:space:read`, a token this server mints for nobody
    // ([#464](http://localhost:1060/l/default/item/464)). The depth is a RESOURCE now
    // (`urn:iki:gonk:review:depth`) with a floor of its own, so the page reads it through
    // the kernel under the caller's capability like everything else, and this face holds no
    // process state about the queue at all.
    /// The passkey relying party.
    pub passkeys: Arc<Passkeys>,
    /// The render rules in effect, as Turtle — the TEXT, not a parsed table, because the
    /// text is what `urn:iki:gonk:render-rules` serves and what the renderer resolves back.
    /// [`crate::rules::DEFAULT_RULES`] unless this deployment named its own file.
    pub rules: Arc<str>,
    /// Which severities the Queue page asks a human about (`gonk.queue.serious`), and what
    /// the header badge counts as serious. Ledger
    /// [#496](http://localhost:1060/l/default/item/496).
    pub queue: crate::config::QueuePolicy,
}

/// Bind the face.
pub fn space(web: Arc<Web>) -> EndpointSpace {
    let template = |t: &str| UriTemplate::parse(t).expect("a constant template");
    EndpointSpace::new()
        .bind(
            Exact::new("urn:iki:gonk:page:home"),
            LedgerView {
                web: Arc::clone(&web),
                shape: Shape::Home,
            },
        )
        .bind(
            template("urn:iki:gonk:page:ledger:{ledger}"),
            LedgerView {
                web: Arc::clone(&web),
                shape: Shape::Page,
            },
        )
        .bind(
            template("urn:iki:gonk:fragment:items:{ledger}"),
            LedgerView {
                web: Arc::clone(&web),
                shape: Shape::Fragment,
            },
        )
        .bind(
            template("urn:iki:gonk:page:item:{ledger}:{id}"),
            ItemView {
                web: Arc::clone(&web),
                full: true,
            },
        )
        .bind(
            template("urn:iki:gonk:fragment:item:{ledger}:{id}"),
            ItemView {
                web: Arc::clone(&web),
                full: false,
            },
        )
        .bind(
            Exact::new("urn:iki:gonk:act"),
            Act {
                web: Arc::clone(&web),
            },
        )
        .bind(
            Exact::new("urn:iki:gonk:sparql"),
            Sparql {
                web: Arc::clone(&web),
                fragment: false,
            },
        )
        .bind(
            Exact::new("urn:iki:gonk:fragment:sparql"),
            Sparql {
                web: Arc::clone(&web),
                fragment: true,
            },
        )
        .bind(
            template("urn:iki:gonk:passkey:{op}"),
            PasskeyDoor {
                passkeys: Arc::clone(&web.passkeys),
            },
        )
        .bind(
            Exact::new(rules::RULES_IRI),
            RenderRules {
                turtle: Arc::clone(&web.rules),
            },
        )
        .bind(template("urn:iki:gonk:asset:{name}"), Asset)
        // The browse family's door — see [`crate::k`]. Bound here because these are pages
        // of this face, reachable through the HTTP door and no other.
        .bind(
            Exact::new(crate::k::K_IRI),
            crate::k::KAdapter {
                web: Arc::clone(&web),
            },
        )
        .bind(
            Exact::new(crate::k::ROOTS_IRI),
            crate::k::BrowseRoots {
                web: Arc::clone(&web),
            },
        )
        .bind(
            template(crate::k::BROWSE_PAGE_TEMPLATE),
            crate::k::BrowseShell {
                web: Arc::clone(&web),
            },
        )
        // The review queue — see [`crate::queue`]. A page of this face like the others: it
        // reads `ikigai-browse`'s findings and writes its decisions under the CALLER's
        // capability, and binds nothing of its own behind the socket or QUIC.
        .bind(
            Exact::new(crate::queue::QUEUE_IRI),
            crate::queue::QueuePage {
                web: Arc::clone(&web),
                fragment: false,
            },
        )
        .bind(
            Exact::new(crate::queue::ROWS_IRI),
            crate::queue::QueuePage {
                web: Arc::clone(&web),
                fragment: true,
            },
        )
        .bind(
            Exact::new(crate::queue::DECIDE_IRI),
            crate::queue::Decide {
                web: Arc::clone(&web),
            },
        )
        // The batch decline (ledger #506) — see [`crate::batch`]. The same shape as the single
        // decision: a form adapter over the finding Sink, under the caller's capability.
        .bind(
            Exact::new(crate::batch::BATCH_IRI),
            crate::batch::Batch {
                web: Arc::clone(&web),
            },
        )
        // The header's live depth badge — see [`crate::queue::Badge`].
        .bind(
            Exact::new(crate::queue::BADGE_IRI),
            crate::queue::Badge {
                web: Arc::clone(&web),
            },
        )
}

// ------------------------------------------------------------------------------ shared

pub(crate) fn html(text: String) -> Representation {
    Representation::new(
        ReprType::new(HTML).with_param("charset", "utf-8"),
        text.into_bytes(),
    )
}

fn arg(name: &str, summary: &str) -> ArgSpec {
    ArgSpec::new(name).summary(summary).class(XSD_STRING)
}

pub(crate) fn as_html_arg() -> ArgSpec {
    arg("as", "The face: this resource serves HTML only.")
        .one_of([HTML])
        .default_value(HTML)
        .optional()
}

/// Refuse any `as` but HTML — a page is HTML, and a different answer is not a better one.
pub(crate) fn html_only(inv: &Invocation<'_>) -> Result<()> {
    match inv.inline_str("as") {
        Ok(asked) if bare(asked) != HTML => Err(Error::InvalidArgument {
            name: "as".to_string(),
            detail: format!("`{asked}` is not a face this page serves; it serves {HTML}"),
        }),
        _ => Ok(()),
    }
}

fn bare(media: &str) -> &str {
    media.split(';').next().unwrap_or(media).trim()
}

pub(crate) fn binding(inv: &Invocation<'_>, name: &str) -> Result<String> {
    inv.bindings
        .get(name)
        .map(str::to_string)
        .ok_or_else(|| Error::Endpoint(format!("no `{name}` captured by this resource's grammar")))
}

fn optional(inv: &Invocation<'_>, name: &str) -> Option<String> {
    inv.inline_str(name)
        .ok()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

pub(crate) fn render_err(e: String) -> Error {
    Error::Endpoint(format!("rendering the page failed: {e}"))
}

/// The exact read grants for one ledger, checked up front so a page refuses with a typed
/// `Denied` naming the token rather than rendering an empty ledger.
fn require_read(inv: &Invocation<'_>, ledger: &Ledger) -> Result<()> {
    for scope in [
        ledger.cap_read(),
        ikigai_store::cap_read_graph(&ledger.graph()),
    ] {
        if !inv.capability.allows(&scope) {
            return Err(Error::Denied(format!(
                "this capability does not hold `{scope}`, so this ledger's pages are not \
                 visible to it. Sign in with a passkey whose grant names the ledger."
            )));
        }
    }
    Ok(())
}

/// The ledgers to list: the configured ones, then any the caller's grant names, keeping
/// only those it may read.
pub(crate) fn readable_ledgers(web: &Web, inv: &Invocation<'_>) -> Vec<Ledger> {
    let mut names: Vec<String> = web.ledgers.clone();
    if let Some(scopes) = inv.capability.scopes() {
        for scope in scopes {
            if let Some(name) = scope.strip_prefix("urn:cap:ledger:read:") {
                if !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
        .iter()
        .filter_map(|n| Ledger::parse(n).ok())
        .filter(|l| inv.capability.allows(&l.cap_read()))
        .collect()
}

/// The header's navigation: one element per readable ledger, then the browse family's
/// entry point when this caller may read at least one root.
///
/// ★ The browse link is computed the same way the ledger links are — from what the caller
/// may READ, not from what the server has — so a caller is never offered a door that
/// answers it a 403. Ledger #442: before this, `/browse/{iri}` was reachable only by
/// typing an IRI, and a door nobody can find is most of the way to not having one.
pub(crate) fn nav(
    web: &Web,
    inv: &Invocation<'_>,
    ledgers: &[Ledger],
    current: Option<&str>,
) -> String {
    let mut out: String = ledgers
        .iter()
        .map(|l| {
            element(
                "ledger",
                &[
                    ("name", l.name()),
                    ("href", &format!("/l/{}", l.name())),
                    (
                        "current",
                        if Some(l.name()) == current {
                            "true"
                        } else {
                            "false"
                        },
                    ),
                ],
                "",
            )
        })
        .collect();
    if !crate::k::readable_roots(web, inv).is_empty() {
        out.push_str(&element("browse", &[("href", crate::k::ROOTS_PATH)], ""));
    }
    // ★ And the Queue, on the same rule one step stronger: a caller is offered it only when
    // it may read a repository AND may DECIDE. Ledger #444 settled the label and the gate —
    // "a link to a page of things you cannot decide is worse than no link" — so the token
    // this one keys on is `urn:cap:annotate`, not a browse read.
    // ★ And it carries a LIVE badge. `depth-url` + `every` are what the stylesheet turns
    // into `hx-get` + `hx-trigger="load, every Ns"` — the web-demo nav-clock shape, and the
    // only liveness signal the armed review trigger has: a queue depth that stops falling is
    // the visible symptom of a watcher thread that died while this server lived
    // ([#466](http://localhost:1060/l/default/item/466)). The badge is on the nav rather
    // than the Queue page because the page is where you go once you already suspect
    // something.
    if crate::queue::offers_queue(web, inv) {
        out.push_str(&element(
            "queue",
            &[
                ("href", crate::queue::QUEUE_PATH),
                ("depth-url", crate::queue::BADGE_PATH),
                ("every", crate::queue::BADGE_EVERY),
            ],
            "",
        ));
    }
    out
}

/// `2026-09-14T10:00:00.123Z` → `2026-09-14 10:00 UTC`.
pub(crate) fn when(iso: &str) -> String {
    if iso.len() >= 16 && iso.as_bytes()[10] == b'T' {
        format!("{} {} UTC", &iso[..10], &iso[11..16])
    } else {
        iso.to_string()
    }
}

fn flag(yes: bool) -> &'static str {
    if yes {
        "true"
    } else {
        "false"
    }
}

/// The view triples an item needs, added to `graph` in place.
fn enrich_items(graph: &mut Graph, ledger: &Ledger, inv: &Invocation<'_>, list_status: &str) {
    let can_write = inv.capability.allows(&ledger.cap_write());
    let can_delete = inv.capability.allows(&ledger.cap_delete());
    let can_purge = inv.capability.allows(&ledger.cap_purge());
    for item in graph.subjects_of_type(&format!("{LEDGER_NS}Item")) {
        let id = item
            .rsplit_once(":item:")
            .map(|(_, id)| id)
            .unwrap_or(&item)
            .to_string();
        let number = graph
            .value(&item, &format!("{LEDGER_NS}number"))
            .unwrap_or_default();
        let open = graph
            .value(&item, &format!("{LEDGER_NS}status"))
            .is_some_and(|s| s == format!("{LEDGER_NS}open"));
        let reason = graph
            .value(&item, &format!("{LEDGER_NS}closedReason"))
            .and_then(|iri| ikigai_ledger::vocabulary::close_reason_name(&iri).map(str::to_string));
        let priority = graph
            .value(&item, &format!("{LEDGER_NS}priority"))
            .map(|p| format!("p{p}"))
            .unwrap_or_else(|| "p-".to_string());
        let deferred = graph
            .value(&item, &format!("{LEDGER_NS}deferred"))
            .is_some_and(|d| d == "true");
        let kind = graph
            .values(&item, "http://www.w3.org/1999/02/22-rdf-syntax-ns#type")
            .into_iter()
            .find(|t| t != &format!("{LEDGER_NS}Item"));
        let title = graph
            .value(&item, &format!("{DCTERMS}title"))
            .unwrap_or_default();
        let body = graph
            .value(&item, &format!("{LEDGER_NS}body"))
            .unwrap_or_default();
        let created = graph
            .value(&item, &format!("{DCTERMS}created"))
            .unwrap_or_default();
        let modified = graph
            .value(&item, &format!("{DCTERMS}modified"))
            .unwrap_or_default();

        graph.view(&item, "iri", item.clone());
        graph.view(&item, "id", id.clone());
        graph.view(
            &item,
            "short",
            ledger.number(number.parse::<i64>().unwrap_or_default()),
        );
        graph.view(&item, "href", format!("/l/{}/item/{id}", ledger.name()));
        graph.view(&item, "ledger", ledger.name());
        graph.view(&item, "ledgerHref", format!("/l/{}", ledger.name()));
        graph.view(&item, "listStatus", list_status);
        graph.view(&item, "status", if open { "open" } else { "closed" });
        if let Some(reason) = reason {
            graph.view(&item, "reason", reason);
        }
        graph.view(&item, "priority", priority);
        graph.view(&item, "deferred", flag(deferred));
        if let Some(kind) = kind {
            graph.view(&item, "kind", kind);
        }
        graph.view(&item, "created", when(&created));
        graph.view(&item, "modified", when(&modified));
        graph.view(
            &item,
            "content",
            if body.is_empty() {
                title
            } else {
                format!("{title}\n\n{body}")
            },
        );
        graph.view(&item, "canWrite", flag(can_write));
        graph.view(&item, "canDelete", flag(can_delete));
        graph.view(&item, "canPurge", flag(can_purge));

        // Links and about-targets become view nodes, because a repeated sub-element that needs
        // its item's IRI cannot reach it in xrust (no parent axis worth trusting, no variable).
        let mut order = 0;
        for (rel, removable) in [
            ("blocks", true),
            ("parent", true),
            ("related", true),
            ("about", false),
        ] {
            for target in graph.values(&item, &format!("{LEDGER_NS}{rel}")) {
                order += 1;
                let node = format!("urn:iki:gonk:view:link:{id}:{order}");
                graph.typed(&node, &format!("{}Link", render::VIEW_NS));
                graph.view(&node, "order", format!("{order:04}"));
                graph.view(&node, "rel", rel);
                graph.view(&node, "ledger", ledger.name());
                graph.view(&node, "id", id.clone());
                graph.view(&node, "item", item.clone());
                graph.view(&node, "target", target.clone());
                match Ledger::item_iri(&target) {
                    Some((other, _)) if removable => {
                        let target_id = target.rsplit_once(":item:").map(|(_, t)| t).unwrap_or("");
                        graph.view(
                            &node,
                            "href",
                            format!("/l/{}/item/{target_id}", other.name()),
                        );
                        graph.view(&node, "text", format!("{} {target_id}", other.name()));
                    }
                    _ => graph.view(&node, "text", target.clone()),
                }
                graph.view(&node, "canUnlink", flag(removable && can_write));
            }
        }
    }
    for comment in graph.subjects_of_type(&format!("{LEDGER_NS}Comment")) {
        let created = graph
            .value(&comment, &format!("{DCTERMS}created"))
            .unwrap_or_default();
        graph.view(&comment, "created", when(&created));
    }
}

/// Issue `request` and return its bytes.
async fn fetch(inv: &Invocation<'_>, request: Request) -> Result<Vec<u8>> {
    Ok(inv.issue(request).await?.bytes)
}

fn request(verb: Verb, iri: &str) -> Result<Request> {
    Ok(Request::new(
        verb,
        Iri::parse(iri).map_err(|e| Error::Endpoint(format!("`{iri}`: {e}")))?,
    ))
}

fn with(request: Request, name: &str, value: &str) -> Request {
    request.with_arg(name, ArgRef::Inline(value.as_bytes().to_vec()))
}

// ------------------------------------------------------------------------ ledger pages

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Home,
    Page,
    Fragment,
}

struct LedgerView {
    web: Arc<Web>,
    shape: Shape,
}

/// Render one ledger's listing — the page, the fragment, and the view `Act` returns.
/// What a listing request asks for: which items, and how much of the answer to draw.
struct Listing<'a> {
    /// `open`, `closed` or `all`.
    status: &'a str,
    /// A case-insensitive substring of the title.
    text: Option<&'a str>,
    /// How many rows to render — see [`rows_wanted`]. The filter above is what gets
    /// COUNTED; this is only what gets drawn.
    limit: Option<&'a str>,
}

async fn ledger_listing(
    web: &Web,
    inv: &Invocation<'_>,
    ledger: &Ledger,
    listing: &Listing<'_>,
    full: bool,
    flash: Option<(&str, &str)>,
) -> Result<Representation> {
    let Listing {
        status,
        text,
        limit,
    } = *listing;
    require_read(inv, ledger)?;
    if !matches!(status, "open" | "closed" | "all") {
        return Err(Error::InvalidArgument {
            name: "status".to_string(),
            detail: format!("`{status}` is not one of open, closed, all"),
        });
    }
    let rows = rows_wanted(limit)?;
    let mut items = with(
        with(
            request(Verb::Source, &format!("{}items", ledger.prefix()))?,
            "as",
            "text/turtle",
        ),
        "status",
        status,
    );
    items = with(items, "limit", &COUNT_CAP.to_string());
    if let Some(text) = text {
        items = with(items, "text", text);
    }
    let mut graph = Graph::from_turtle(&fetch(inv, items).await?).map_err(render_err)?;
    let count = newest_rows(&mut graph, rows);
    enrich_items(&mut graph, ledger, inv, status);
    let ledgers = readable_ledgers(web, inv);
    let mut children = nav(web, inv, &ledgers, Some(ledger.name()));
    if let Some((kind, message)) = flash {
        children.push_str(&element("flash", &[("kind", kind)], message));
    }
    children.push_str(&graph.rdfxml().map_err(render_err)?);
    let title = if ledger.is_default() {
        "Ledger".to_string()
    } else {
        format!("Ledger {}", ledger.name())
    };
    let page_url = format!("/l/{}", ledger.name());
    let items_url = format!("/l/{}/items", ledger.name());
    let query = count.more_query(status, text);
    let doc = envelope(
        "page",
        &[
            ("view", "ledger"),
            ("full", flag(full)),
            ("title", &title),
            ("ledger", ledger.name()),
            ("status", status),
            ("text", text.unwrap_or("")),
            ("page-url", &page_url),
            ("items-url", &items_url),
            ("count", &count.sentence(status)),
            ("more", flag(query.is_some())),
            ("more-label", &count.more_label()),
            (
                "more-url",
                &query
                    .as_ref()
                    .map(|q| format!("{page_url}{q}"))
                    .unwrap_or_default(),
            ),
            (
                "more-items-url",
                &query
                    .as_ref()
                    .map(|q| format!("{items_url}{q}"))
                    .unwrap_or_default(),
            ),
            (
                "can-write",
                flag(inv.capability.allows(&ledger.cap_write())),
            ),
        ],
        &children,
    );
    Ok(html(render::render(&doc, full).map_err(render_err)?))
}

/// How many rows the caller asked to see: [`ROWS`] by default, `all` for as many as one
/// render is allowed, and anything above [`MAX_ROWS`] clamped down to it rather than
/// refused — a bound that refuses a number a person typed into a URL helps nobody, and the
/// page SAYS what it rendered.
fn rows_wanted(limit: Option<&str>) -> Result<usize> {
    match limit {
        None => Ok(ROWS),
        Some("all") => Ok(MAX_ROWS),
        Some(other) => match other.parse::<usize>() {
            Ok(n) if n > 0 => Ok(n.min(MAX_ROWS)),
            _ => Err(Error::InvalidArgument {
                name: "limit".to_string(),
                detail: format!("`{other}` is not a positive number of rows, or `all`"),
            }),
        },
    }
}

/// What a listing rendered, and out of what.
struct Count {
    /// Rows left in the graph.
    shown: usize,
    /// Items the ledger returned for this filter.
    total: usize,
    /// The read filled [`COUNT_CAP`], so `total` is a floor and not a total.
    capped: bool,
}

impl Count {
    /// The sentence over the list. `status` is the filter in force, so "411 open items"
    /// and "411 items" (for `all`) both read as English.
    fn sentence(&self, status: &str) -> String {
        let kind = match status {
            "all" => "items".to_string(),
            other => format!("{other} items"),
        };
        let total = if self.capped {
            format!("{}+", self.total)
        } else {
            self.total.to_string()
        };
        if self.shown >= self.total && !self.capped {
            format!("{total} {kind}")
        } else {
            format!(
                "showing the {} most recently updated of {total} {kind}",
                self.shown
            )
        }
    }

    fn more_label(&self) -> String {
        format!("Show {}", self.next_rows())
    }

    fn next_rows(&self) -> usize {
        self.total.min(MAX_ROWS)
    }

    /// The query string of the "show more" link, or `None` when there is nothing more this
    /// page can render.
    fn more_query(&self, status: &str, text: Option<&str>) -> Option<String> {
        if self.shown >= self.total || self.next_rows() <= self.shown {
            return None;
        }
        let mut query = format!("?status={status}&limit={}", self.next_rows());
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            query.push_str(&format!("&text={}", percent_encode(text)));
        }
        Some(query)
    }
}

/// Percent-encode a query-string VALUE. Nothing here builds a URL from anything but a
/// filter the caller already typed, and a search for `a & b` must not become two
/// parameters.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Cut `graph` down to the `rows` most recently updated items, and report what it held.
///
/// ★ **The order is the STYLESHEET's order**, `dcterms:modified` descending — an ISO-8601
/// instant sorts lexically, which is why `xsl:sort` and this agree without a date type.
/// A truncation that kept a different set would render a page whose rows are not the ones
/// the banner describes.
///
/// It also drops every subject that is not a kept item: a listing renders rows, and a
/// comment or a link node is input the row templates never look at.
fn newest_rows(graph: &mut Graph, rows: usize) -> Count {
    let mut items: Vec<(String, String)> = graph
        .subjects_of_type(&format!("{LEDGER_NS}Item"))
        .into_iter()
        .map(|iri| {
            let modified = graph
                .value(&iri, &format!("{DCTERMS}modified"))
                .unwrap_or_default();
            (modified, iri)
        })
        .collect();
    let total = items.len();
    items.sort_by(|a, b| b.cmp(a));
    let keep: HashSet<String> = items.into_iter().take(rows).map(|(_, iri)| iri).collect();
    let shown = keep.len();
    graph.retain(
        |t| matches!(&t.subject, NamedOrBlankNode::NamedNode(s) if keep.contains(s.as_str())),
    );
    Count {
        shown,
        total,
        capped: total >= COUNT_CAP,
    }
}

#[async_trait]
impl Endpoint for LedgerView {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        html_only(inv)?;
        let status = optional(inv, "status").unwrap_or_else(|| "open".to_string());
        let text = optional(inv, "text");
        let limit = optional(inv, "limit");
        let ledger = match self.shape {
            Shape::Home => match readable_ledgers(&self.web, inv).into_iter().next() {
                Some(ledger) => ledger,
                None => {
                    // ⚠ The nav still renders: a grant may carry browse roots and no
                    // ledger at all, and such a caller used to land here with a header
                    // that offered nothing and a page that said the server was empty.
                    let doc = envelope(
                        "page",
                        &[
                            ("view", "empty"),
                            ("full", "true"),
                            ("title", "No ledger to show"),
                            (
                                "message",
                                "This browser holds no grant to read any ledger here. Sign in \
                                 with a passkey — `ikigai-gonk passkey invite` on the server \
                                 makes one.",
                            ),
                        ],
                        &nav(&self.web, inv, &[], None),
                    );
                    return Ok(html(render::render(&doc, true).map_err(render_err)?));
                }
            },
            _ => Ledger::parse(&binding(inv, "ledger")?)?,
        };
        ledger_listing(
            &self.web,
            inv,
            &ledger,
            &Listing {
                status: &status,
                text: text.as_deref(),
                limit: limit.as_deref(),
            },
            self.shape != Shape::Fragment,
            None,
        )
        .await
    }

    fn name(&self) -> &str {
        match self.shape {
            Shape::Home => "gonk-page-home",
            Shape::Page => "gonk-page-ledger",
            Shape::Fragment => "gonk-fragment-items",
        }
    }

    fn describe(&self) -> Description {
        let (title, summary) = match self.shape {
            Shape::Home => (
                "The front page",
                "The first ledger this caller may read, as an HTML page — or a sign-in page \
                 when it may read none.",
            ),
            Shape::Page => (
                "A ledger, as a page",
                "One ledger's items as an HTML page: the ledger's Turtle face rendered through \
                 gonk's stylesheet, with the forms this caller's grant allows.",
            ),
            Shape::Fragment => (
                "A ledger, as an htmx fragment",
                "The same listing as the ledger page, without the page around it.",
            ),
        };
        let mut desc = Description::new(self.name())
            .title(title)
            .summary(summary)
            .verb(Verb::Source)
            .verb(Verb::Meta);
        if self.shape != Shape::Home {
            // The home page is public — it is where a caller with no grant is told how to get
            // one. A ledger page reads the ledger, and declares exactly what that needs.
            desc = desc
                .requires(ikigai_ledger::CAP_READ)
                .requires(ikigai_store::CAP_READ_GRAPH)
                .input(
                    arg("ledger", "Which ledger.")
                        .binding()
                        .default_value("default")
                        .optional(),
                );
        }
        desc.input(
            arg("status", "Which items: open (the default), closed, or all.")
                .one_of(["open", "closed", "all"])
                .default_value("open")
                .optional(),
        )
        .input(arg("text", "A case-insensitive substring of the title.").optional())
        .input(
            arg(
                "limit",
                "How many rows to render: a number, or `all` for as many as one render is \
                 allowed. The page counts the whole filtered set either way and says what \
                 it rendered.",
            )
            .default_value(ROWS.to_string())
            .optional(),
        )
        .input(as_html_arg())
        .output(HTML)
    }
}

// -------------------------------------------------------------------------- item pages

struct ItemView {
    web: Arc<Web>,
    full: bool,
}

async fn item_card(
    web: &Web,
    inv: &Invocation<'_>,
    ledger: &Ledger,
    id: &str,
    full: bool,
    flash: Option<(&str, &str)>,
) -> Result<Representation> {
    require_read(inv, ledger)?;
    let read = with(
        request(Verb::Source, &ledger.item(id))?,
        "as",
        "text/turtle",
    );
    let mut graph = Graph::from_turtle(&fetch(inv, read).await?).map_err(render_err)?;
    enrich_items(&mut graph, ledger, inv, "open");
    let title = graph
        .subjects_of_type(&format!("{LEDGER_NS}Item"))
        .first()
        .and_then(|item| graph.value(item, &format!("{DCTERMS}title")))
        .unwrap_or_else(|| id.to_string());
    let mut children = nav(web, inv, &readable_ledgers(web, inv), Some(ledger.name()));
    if let Some((kind, message)) = flash {
        children.push_str(&element("flash", &[("kind", kind)], message));
    }
    children.push_str(&graph.rdfxml().map_err(render_err)?);
    let doc = envelope(
        "page",
        &[
            ("view", "item"),
            ("full", flag(full)),
            ("title", &title),
            ("ledger", ledger.name()),
            ("page-url", &format!("/l/{}", ledger.name())),
        ],
        &children,
    );
    Ok(html(render::render(&doc, full).map_err(render_err)?))
}

#[async_trait]
impl Endpoint for ItemView {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        html_only(inv)?;
        let ledger = Ledger::parse(&binding(inv, "ledger")?)?;
        let id = binding(inv, "id")?;
        item_card(&self.web, inv, &ledger, &id, self.full, None).await
    }

    fn name(&self) -> &str {
        if self.full {
            "gonk-page-item"
        } else {
            "gonk-fragment-item"
        }
    }

    fn describe(&self) -> Description {
        Description::new(self.name())
            .title(if self.full {
                "An item, as a page"
            } else {
                "An item, as an htmx fragment"
            })
            .summary(
                "One ledger item with its comments and links — the item's Turtle face rendered \
                 through gonk's stylesheet — and the forms this caller's grant allows: comment, \
                 edit, close or reopen, claim, defer, label, link, and (separately) delete and \
                 purge.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_ledger::CAP_READ)
            .requires(ikigai_store::CAP_READ_GRAPH)
            .input(
                arg("ledger", "Which ledger.")
                    .binding()
                    .default_value("default")
                    .optional(),
            )
            .input(arg("id", "The item's opaque id or its number.").binding())
            .input(as_html_arg())
            .output(HTML)
    }
}

// --------------------------------------------------------------------------------- act

/// The ledger actions a form may name. Everything else is refused before the manifold is
/// even consulted — the adapter reaches the ledger and nothing else.
const ACTIONS: [&str; 10] = [
    "append", "comment", "close", "reopen", "claim", "defer", "link", "label", "purge", "item",
];

struct Act {
    web: Arc<Web>,
}

/// A form that is missing a field it needs is a malformed `content`, not a missing ARGUMENT:
/// the fields live inside the body, and the contract declares the body.
fn bad_form(detail: String) -> Error {
    Error::InvalidArgument {
        name: "content".to_string(),
        detail: format!("the form is missing {detail}"),
    }
}

/// `application/x-www-form-urlencoded` → ordered pairs. A repeated name keeps its LAST value.
pub(crate) fn form(body: &str) -> BTreeMap<String, String> {
    form_pairs(body).into_iter().collect()
}

/// The same body as EVERY pair, in order, repeats kept — for a form whose checkboxes share
/// one name (`member=a&member=b`), which [`form`]'s map would collapse to the last box ticked.
pub(crate) fn form_pairs(body: &str) -> Vec<(String, String)> {
    let decode = |s: &str| {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                // ⚠ The two digits are read from the BYTES: slicing the `&str` at `i + 1`
                // would panic when a multibyte character follows the `%`, and this body is
                // whatever any client sent.
                b'%' if i + 2 < bytes.len() => {
                    match std::str::from_utf8(&bytes[i + 1..i + 3])
                        .ok()
                        .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                    {
                        Some(b) => {
                            out.push(b);
                            i += 2;
                        }
                        None => out.push(b'%'),
                    }
                }
                c => out.push(c),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    body.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(pair), String::new()),
        })
        .collect()
}

#[async_trait]
impl Endpoint for Act {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(Error::Endpoint(
                "`urn:iki:gonk:act` answers Sink only".to_string(),
            ));
        }
        let mut fields = form(inv.inline_str("content")?);
        let mut take = |name: &str| fields.remove(name).filter(|v| !v.trim().is_empty());
        let ledger = Ledger::parse(
            &take("_ledger")
                .ok_or_else(|| bad_form("_ledger (the form names no ledger)".to_string()))?,
        )?;
        let action = take("_action")
            .ok_or_else(|| bad_form("_action (the form names no action)".to_string()))?;
        if !ACTIONS.contains(&action.as_str()) {
            return Err(Error::InvalidArgument {
                name: "_action".to_string(),
                detail: format!(
                    "`{action}` is not a ledger action a form may take; one of {}",
                    ACTIONS.join(", ")
                ),
            });
        }
        let verb = match take("_verb").as_deref() {
            None | Some("Sink") => Verb::Sink,
            Some("Delete") => Verb::Delete,
            Some(other) => {
                return Err(Error::InvalidArgument {
                    name: "_verb".to_string(),
                    detail: format!("`{other}` is not Sink or Delete"),
                })
            }
        };
        let id = take("_id");
        let then = take("_then").unwrap_or_else(|| "items".to_string());
        let list_status = take("_status").unwrap_or_else(|| "open".to_string());
        let target = if action == "item" {
            ledger.item(
                id.as_deref()
                    .ok_or_else(|| bad_form("_id (an item action names no item)".to_string()))?,
            )
        } else {
            format!("{}{action}", ledger.prefix())
        };
        let target_iri =
            Iri::parse(&target).map_err(|e| Error::Endpoint(format!("`{target}`: {e}")))?;

        // ★ The manifold decides what a form may send. An empty field is "not given", which
        // is what an optional form control left blank means.
        let description =
            self.web.hub.describe(&target_iri).ok_or_else(|| {
                Error::NotFound(format!("the ledger binds nothing at `{target}`"))
            })?;
        let spec = description
            .action_specs()
            .into_iter()
            .find(|spec| spec.verb == verb)
            .ok_or_else(|| Error::InvalidArgument {
                name: "_verb".to_string(),
                detail: format!("`{target}` does not declare {verb:?}"),
            })?;
        let mut request = Request::new(verb, target_iri);
        for (name, value) in fields {
            if name.starts_with('_') || value.trim().is_empty() {
                continue;
            }
            // A binding is part of the IRI the adapter built, never a form field.
            let declared = spec
                .inputs
                .iter()
                .any(|input| input.name == name && input.source != InputSource::Binding);
            if !declared {
                return Err(Error::InvalidArgument {
                    name,
                    detail: format!(
                        "`{target}` does not declare this input for {verb:?}; a form may send \
                         only what the resource's own contract names"
                    ),
                });
            }
            request = with(request, &name, value.replace("\r\n", "\n").as_str());
        }

        let answer = inv.issue(request).await?;
        let said = String::from_utf8_lossy(&answer.bytes).trim().to_string();
        let flash = Some(("ok", said.as_str()));
        match then.as_str() {
            "card" => {
                let id = id.ok_or_else(|| {
                    bad_form("_id (a card to render after the action)".to_string())
                })?;
                item_card(&self.web, inv, &ledger, &id, false, flash).await
            }
            "gone" => {
                let ledgers = readable_ledgers(&self.web, inv);
                let doc = envelope(
                    "page",
                    &[
                        ("view", "gone"),
                        ("full", "false"),
                        ("ledger", ledger.name()),
                        ("page-url", &format!("/l/{}", ledger.name())),
                    ],
                    &format!(
                        "{}{}",
                        nav(&self.web, inv, &ledgers, Some(ledger.name())),
                        element("flash", &[("kind", "ok")], &said)
                    ),
                );
                Ok(html(render::render(&doc, false).map_err(render_err)?))
            }
            _ => {
                let listing = Listing {
                    status: &list_status,
                    text: None,
                    limit: None,
                };
                ledger_listing(&self.web, inv, &ledger, &listing, false, flash).await
            }
        }
    }

    fn name(&self) -> &str {
        "gonk-act"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-act")
            .title("Take a ledger action from a form")
            .summary(
                "The form adapter: an urlencoded form naming `_ledger`, `_action` and `_verb` \
                 becomes ONE request to that ledger resource, carrying only the inputs its \
                 contract declares, under the caller's capability — then re-renders the view \
                 named by `_then` (items, card, gone). It holds no authority of its own: the \
                 ledger resource it reaches enforces its own grant.",
            )
            .verb(Verb::Sink)
            .verb(Verb::Meta)
            // ★ The floor EVERY reachable action shares, and nothing more: each ledger
            // mutation declares both of the store's per-graph families. The ledger family
            // (write, delete or purge) depends on which action the form names, and a
            // description has no way to declare "one of these" — so that part is enforced by
            // the target, one hop in, exactly as a mount's forward is.
            .requires(ikigai_store::CAP_READ_GRAPH)
            .requires(ikigai_store::CAP_WRITE_GRAPH)
            .input(arg(
                "content",
                "The form body, application/x-www-form-urlencoded — where a pipe's value lands.",
            ))
            .output(HTML)
    }
}

// ------------------------------------------------------------------------------ sparql

const DEFAULT_QUERY: &str = "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>

SELECT ?number ?title ?status WHERE {
  ?item a ledger:Item ;
        ledger:number ?number ;
        dcterms:title ?title ;
        ledger:status ?status .
}
ORDER BY DESC(?number)
LIMIT 50";

/// The sample queries offered above the editor: `(id, label, query)`. Public so the test
/// that runs every one of them can be the same list the page renders.
///
/// ★ **Every one is RUN, not merely written.**
/// `tests/web.rs::every_sample_query_returns_rows` seeds a corpus in the shape
/// `ikigai-ledger` writes and asserts each sample comes back with at least one row and no
/// `view:error` — a sample query that errors is worse than no sample query. They are held to
/// the ledger's real predicates: `ledger:Item`, `ledger:number`, `ledger:priority`,
/// `ledger:status` (the IRIs `ledger:open`/`ledger:closed`), `ledger:label`, `ledger:body`,
/// `ledger:closedReason`, `ledger:Comment` + `ledger:onItem`, `dcterms:title`,
/// `dcterms:created`, `dcterms:modified`.
///
/// ⚠ **There is deliberately no "oldest p1" sample**, though that is the question that was
/// asked. `dcterms:created` is when an item was FILED, and a bulk migration files hundreds
/// within a minute of each other — ordering by it sorts by migration order, not by how long
/// the work has waited. The real age of most claims lives in prose inside `ledger:body`. The
/// page says so next to these buttons — that sentence is [`CREATED_IS_FILING_TIME`].
pub const SAMPLES: [(&str, &str, &str); 9] = [
    (
        "sample-ledger",
        "One named ledger",
        // ★ The partition, taught by a query. Each named ledger is its OWN graph — gated by
        // `urn:cap:ledger:read:{name}` and `urn:cap:store:read:graph:<iri>` — and nothing
        // else on this page shows that, because every other sample leans on the graph the
        // editor already scopes to. Measured 2026-09-16: `urn:iki:ledger:graph:default`
        // holds every ledger quad, so naming the graph is how a query says which ledger it
        // is about, even while there is one.
        //
        // ⚠ browse's quads are NOT reachable from this page. They live in
        // `urn:iki:browse:graph:default` (`crate::browse::Graph`), and this editor scopes to
        // a LEDGER — the box above picks a ledger name, not a graph — so an annotation or an
        // archived explanation is invisible here whatever token the caller holds. A graph
        // selector rather than a ledger selector is what that would take. ★ The DOOR can do
        // it since `ikigai-store` 0.2.5 (a scoped read takes a SET of graphs, and a caller
        // holding both read tokens runs the ledger↔browse join through
        // `urn:iki:store:graph-select` directly); it is this PAGE that names one graph. The
        // page says so itself now, and shows that join without offering to run it —
        // [`CROSS_GRAPH`], #401.
        //
        // ⚠ `GRAPH <other>` matches NOTHING rather than erroring (ikigai-store confines the
        // query's available named graphs to the set it was issued for, and this page always
        // issues one), so editing this to another ledger's graph without also choosing that
        // ledger above returns zero rows.
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>

# Each named ledger lives in its own graph. Name it, and the query says which
# ledger it is about — change `default` here AND in the Ledger box above.
SELECT ?number ?status ?title WHERE {
  GRAPH <urn:iki:ledger:graph:default> {
    ?item a ledger:Item ;
          ledger:number ?number ;
          ledger:status ?status ;
          dcterms:title ?title .
  }
}
ORDER BY DESC(?number)
LIMIT 50",
    ),
    (
        "sample-priority",
        "Open by priority",
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>

SELECT ?number ?priority ?title WHERE {
  ?item a ledger:Item ;
        ledger:status ledger:open ;
        ledger:number ?number ;
        dcterms:title ?title .
  OPTIONAL { ?item ledger:priority ?priority }
}
ORDER BY COALESCE(?priority, 9) ?number
LIMIT 50",
    ),
    (
        "sample-p1",
        "p1 only",
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>

SELECT ?number ?title WHERE {
  ?item a ledger:Item ;
        ledger:priority 1 ;
        ledger:status ledger:open ;
        ledger:number ?number ;
        dcterms:title ?title .
}
ORDER BY ?number",
    ),
    (
        "sample-repos",
        "Count by repo",
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>

SELECT ?repo (COUNT(?item) AS ?open) WHERE {
  ?item a ledger:Item ;
        ledger:status ledger:open ;
        ledger:label ?repo .
  FILTER(STRSTARTS(?repo, \"repo:\"))
}
GROUP BY ?repo
ORDER BY DESC(?open) ?repo",
    ),
    (
        "sample-security",
        "Security",
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>

SELECT ?number ?priority ?status ?title WHERE {
  ?item a ledger:Item ;
        ledger:label \"security\" ;
        ledger:number ?number ;
        ledger:status ?status ;
        dcterms:title ?title .
  OPTIONAL { ?item ledger:priority ?priority }
}
ORDER BY ?number",
    ),
    (
        "sample-recent",
        "Recently updated",
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>

SELECT ?modified ?number ?status ?title WHERE {
  ?item a ledger:Item ;
        ledger:number ?number ;
        ledger:status ?status ;
        dcterms:title ?title ;
        dcterms:modified ?modified .
}
ORDER BY DESC(?modified)
LIMIT 25",
    ),
    (
        "sample-comments",
        "Items with comments",
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>

SELECT ?number ?title (COUNT(?comment) AS ?comments) WHERE {
  ?comment a ledger:Comment ;
           ledger:onItem ?item .
  ?item ledger:number ?number ;
        dcterms:title ?title .
}
GROUP BY ?number ?title
ORDER BY DESC(?comments) ?number",
    ),
    (
        "sample-closed",
        "Closed, with their reasons",
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>

SELECT ?number ?reason ?modified ?title WHERE {
  ?item a ledger:Item ;
        ledger:status ledger:closed ;
        ledger:number ?number ;
        dcterms:title ?title ;
        dcterms:modified ?modified .
  OPTIONAL { ?item ledger:closedReason ?reason }
}
ORDER BY DESC(?modified)
LIMIT 50",
    ),
    (
        "sample-text",
        "Search the body text",
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>

SELECT ?number ?title ?body WHERE {
  ?item a ledger:Item ;
        ledger:number ?number ;
        dcterms:title ?title ;
        ledger:body ?body .
  # Change the word in quotes — that is the whole search.
  FILTER(CONTAINS(LCASE(?body), \"passkey\"))
}
ORDER BY ?number",
    ),
];

/// The sentence that sits with the buttons, because the ledger cannot answer the question
/// everyone asks first. See [`SAMPLES`].
pub const CREATED_IS_FILING_TIME: &str =
    "A sample replaces the query in the box; nothing runs until you press Run. There is no \
     \"oldest\" sample on purpose: dcterms:created is when an item was FILED, and items \
     migrated in bulk share a filing minute, so it does not say how long the work has waited.";

/// The sample buttons and the sentence that goes with them, as view elements.
fn samples() -> String {
    let buttons: String = SAMPLES
        .iter()
        .map(|(id, label, query)| element("sample", &[("id", id), ("label", label)], query))
        .collect();
    format!("{buttons}{}", element("hint", &[], CREATED_IS_FILING_TIME))
}

/// Why the cross-graph example is SHOWN and not offered as a button. See [`CROSS_GRAPH`].
pub const CROSS_GRAPH_WHY: &str =
    "This one is shown, not loadable: the box above sends a single ledger's graph, so this \
     query pressed into it would return no rows and make a working capability look broken. \
     Run it where the graphs are named — from ikigai -c, or any caller holding a read token \
     for each graph. Within the set you were issued, GRAPH <…> picks one; outside it, GRAPH <…> \
     matches nothing rather than erroring.";

/// ★ The join, SHOWN rather than offered — the widened capability made visible without
/// pretending this page has it.
///
/// `sparql_results` sends `ledger.graph()` and nothing else, so loading this into the editor
/// would return zero rows: a real feature presented as a broken one. It therefore travels as
/// a `view:cross-graph` element with no `id` and no `label`, is rendered outside
/// `#sample-queries`, and carries no `data-query` — so `web/gonk.js`, which loads a sample by
/// looking up `data-query` as an element id, cannot reach it.
///
/// ⚠ Verified live 2026-09-17 against plasma's store (39 rows, one explanation). Three traps
/// it is shaped around, and each of the three returns **zero rows with no error** when got
/// wrong — which reads exactly like "the join does not work":
///
/// 1. ★ the `FILTER` sits at the TOP level of the `WHERE`, not inside the ledger's `GRAPH`
///    block. Inside it, `?repo` is bound only in the sibling group, so the filter sees it
///    unbound and is false everywhere. Measured both ways. Re-run the query if this is ever
///    restructured for readability;
/// 2. `ledger:status` is an IRI (`ledger:open`), not the string `"open"`;
/// 3. the title is `dcterms:title`, not `ledger:title`.
pub const CROSS_GRAPH: &str = "# Two graphs in ONE read — at the endpoint, not in the box above:
#
#   ikigai -c 'source urn:iki:store:graph-select
#     graph=\"urn:iki:ledger:graph:default urn:iki:browse:graph:default\"
#     query=\"…\" as=text/csv'
#
# `graph=` is whitespace-separated BARE IRIs, no angle brackets, and the caller
# holds urn:cap:store:read:graph:<that graph> for EACH of them.

PREFIX ik: <https://ikigai-rs.dev/ns#>
PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
PREFIX dcterms: <http://purl.org/dc/terms/>

SELECT ?number ?title ?file WHERE {
  GRAPH <urn:iki:browse:graph:default> {
    ?ex a ik:Explanation ; ik:repo ?repo ; ik:about ?file .
  }
  GRAPH <urn:iki:ledger:graph:default> {
    ?item a ledger:Item ; ledger:number ?number ; dcterms:title ?title ;
          ledger:label ?label ; ledger:status ledger:open .
  }
  # The join key is a STRING bridge — ik:repo is \"gonk\", the label is
  # \"repo:ikigai-gonk\" — because nothing yet links an item to a repo file.
  # ★ The FILTER stays HERE, at the top level of the WHERE: inside either GRAPH
  # block the other group's variable is unbound and this returns zero rows in
  # silence.
  FILTER(?label = CONCAT(\"repo:ikigai-\", ?repo))
}
ORDER BY ?number";

/// The cross-graph example and its explanation, as one view element. See [`CROSS_GRAPH`].
fn cross_graph() -> String {
    element("cross-graph", &[("why", CROSS_GRAPH_WHY)], CROSS_GRAPH)
}

struct Sparql {
    web: Arc<Web>,
    fragment: bool,
}

/// The query form — `select`, `ask`, `construct` or `describe` — read past comments, `BASE`
/// and `PREFIX`. A wrong guess costs nothing: the store refuses a query of the wrong shape at
/// the wrong IRI, and says which IRI to use.
pub fn query_form(query: &str) -> Option<&'static str> {
    let mut words = query
        .lines()
        .map(|line| match line.find('#') {
            // A `#` inside an IRI (`<…#>`) is not a comment; only strip one that is not
            // inside angle brackets on this line.
            Some(at) if line[..at].matches('<').count() == line[..at].matches('>').count() => {
                &line[..at]
            }
            _ => line,
        })
        .flat_map(str::split_whitespace);
    while let Some(word) = words.next() {
        match word.to_ascii_lowercase().as_str() {
            "prefix" => {
                words.next();
                words.next();
            }
            "base" => {
                words.next();
            }
            "select" => return Some("select"),
            "ask" => return Some("ask"),
            "construct" => return Some("construct"),
            "describe" => return Some("describe"),
            _ => return None,
        }
    }
    None
}

/// The local name of a `ledger:` IRI — `…/ledger#open` → `open` — or `None` for anything
/// else.
///
/// ★ **Display only, and only this ONE namespace.** A `ledger:status` cell printing
/// `https://ikigai-rs.dev/ns/ledger#open` is honest and unreadable at 200 rows. The BYTES the
/// store returned never change — a caller asking for `application/sparql-results+json` gets
/// the raw IRIs (`tests/web.rs`).
///
/// Nothing else is shortened, because in this data no other namespace reaches a result CELL
/// as a value: `dcterms:`, `prov:` and `sig:` name predicates whose objects are literals, and
/// an item's own IRI (`urn:iki:ledger:{name}:item:{id}`) is the identifier a person copies —
/// abbreviating that would hide the useful half, and it becomes a LINK instead
/// ([`crate::rules`]). If a future ledger grows IRI-valued properties in another namespace,
/// a second entry in [`PREFIX_LEGEND`] beats a second special case here.
fn ledger_local_name(value: &str) -> Option<&str> {
    let local = value.strip_prefix(LEDGER_NS)?;
    (!local.is_empty() && !local.contains(['/', '#'])).then_some(local)
}

/// The sentence printed ONCE above a result set that shortened an IRI.
///
/// ★ **This is #241's answer for a cell that does not become a link.** The shortening used
/// to live entirely in a `title` attribute, which is not keyboard-reachable and which most
/// screen readers do not announce — so for those readers the full IRI was simply gone from
/// the HTML face. It is in the page as TEXT now, and the cell reads as a CURIE
/// (`ledger:open`, not `open`) so the legend has something to bind. Once per result set, not
/// once per row: a 200-row answer must not read the same IRI 200 times either.
///
/// The `title` stays as well, for a pointer user who wants it without moving their eye — but
/// it no longer carries the information alone, which is the whole of the complaint.
pub const PREFIX_LEGEND: &str =
    "ledger: is short for https://ikigai-rs.dev/ns/ledger# — this table writes it that way.";

/// One result cell, ready to render.
struct Cell {
    /// `uri`, `literal`, `bnode`, as the store reported it.
    kind: &'static str,
    /// What the cell reads as.
    display: String,
    /// The full IRI, when [`display`](Cell::display) shortened it.
    full: Option<String>,
    /// The value as the store returned it — what a rule matches on, never the display.
    lexical: String,
}

fn cell(term: &Value) -> Cell {
    let kind = term
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("literal");
    let value = term
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let lexical = value.to_string();
    match kind {
        "uri" => match ledger_local_name(value) {
            Some(local) => Cell {
                kind: "uri",
                display: format!("ledger:{local}"),
                full: Some(lexical.clone()),
                lexical,
            },
            None => Cell {
                kind: "uri",
                display: lexical.clone(),
                full: None,
                lexical,
            },
        },
        "bnode" => Cell {
            kind: "bnode",
            display: format!("_:{value}"),
            full: None,
            lexical,
        },
        _ => {
            let suffix = term
                .get("xml:lang")
                .and_then(Value::as_str)
                .map(|lang| format!(" @{lang}"))
                .unwrap_or_default();
            Cell {
                kind: "literal",
                display: format!("{value}{suffix}"),
                full: None,
                lexical,
            }
        }
    }
}

/// The rule table in effect, RESOLVED — `urn:iki:gonk:render-rules`, through the kernel,
/// like every other thing this face is built from.
///
/// A failure here falls back to the shipped table rather than failing the page: `main`
/// already refused to start on a table it could not parse, so the only way to arrive here
/// with a bad one is a defect, and a results page with no links beats no results page.
async fn rules_in_effect(inv: &Invocation<'_>) -> Rules {
    let answer = match request(Verb::Source, rules::RULES_IRI) {
        Ok(r) => inv.issue(with(r, "as", "text/turtle")).await,
        Err(e) => Err(e),
    };
    answer
        .ok()
        .and_then(|repr| Rules::parse(&String::from_utf8_lossy(&repr.bytes)).ok())
        .unwrap_or_default()
}

/// Run `query` against `ledger` and build the `<view:results>` element.
///
/// A SELECT result becomes a `view:table` whose cells are already aligned to the header —
/// the XSLT cannot do that alignment itself (it would need a variable or `current()` to
/// match each binding to its column), so the result face is reshaped here, not recomputed.
/// A store refusal (a `FROM` clause, a syntax error, a query of the wrong shape) is shown as
/// the error it is, in the page, rather than as a failed request.
async fn sparql_results(inv: &Invocation<'_>, ledger: &Ledger, query: &str) -> String {
    let graph = ledger.graph();
    let wrap = |summary: String, body: String| {
        format!(
            "<view:results summary=\"{}\">{body}</view:results>",
            render::escape(&summary)
        )
    };
    let Some(form) = query_form(query) else {
        return wrap(
            String::new(),
            element(
                "error",
                &[],
                "Not a query form this page runs: SELECT, ASK, CONSTRUCT or DESCRIBE. Updates are \
                 not accepted here.",
            ),
        );
    };
    let graph_shaped = matches!(form, "construct" | "describe");
    let face = if graph_shaped {
        "text/turtle"
    } else {
        "application/sparql-results+json"
    };
    let answer = match request(Verb::Source, &format!("urn:iki:store:graph-{form}")) {
        Ok(r) => {
            inv.issue(with(
                with(with(r, "graph", &graph), "query", query),
                "as",
                face,
            ))
            .await
        }
        Err(e) => Err(e),
    };
    let repr = match answer {
        Ok(repr) => repr,
        Err(e) => return wrap(String::new(), element("error", &[], &e.to_string())),
    };
    if graph_shaped {
        return wrap(
            format!("{form} over {graph}"),
            element("graph", &[], &String::from_utf8_lossy(&repr.bytes)),
        );
    }
    let v: Value = match serde_json::from_slice(&repr.bytes) {
        Ok(v) => v,
        Err(e) => {
            return wrap(
                String::new(),
                element(
                    "error",
                    &[],
                    &format!("the store's results were not JSON: {e}"),
                ),
            )
        }
    };
    if let Some(b) = v.get("boolean").and_then(Value::as_bool) {
        return wrap(
            format!("ask over {graph}"),
            element("boolean", &[], flag(b)),
        );
    }
    let vars: Vec<&str> = v["head"]["vars"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let rows = v["results"]["bindings"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let rules = rules_in_effect(inv).await;
    let mut shortened = false;
    let mut table: String = vars.iter().map(|name| element("col", &[], name)).collect();
    for row in rows {
        table.push_str("<view:row>");
        for name in &vars {
            match row.get(*name) {
                Some(term) => {
                    let cell = cell(term);
                    let action = rules.action(name, cell.kind, &cell.lexical, ledger.name());
                    let mut attributes = vec![("kind", cell.kind)];
                    match &action {
                        // A link IS the reachable form of the full value, so the tooltip
                        // stops carrying it here — keeping both would be answering #241
                        // with the thing #241 is about.
                        Some(action) => {
                            attributes.push(("href", action.href.as_str()));
                            attributes.push(("label", action.label.as_str()));
                            attributes.push(("action", action.kind));
                        }
                        None => {
                            if let Some(full) = &cell.full {
                                shortened = true;
                                attributes.push(("title", full.as_str()));
                            }
                        }
                    }
                    table.push_str(&element("cell", &attributes, &cell.display));
                }
                None => table.push_str(&element("cell", &[("kind", "unbound")], "")),
            }
        }
        table.push_str("</view:row>");
    }
    let legend = if shortened {
        element("prefix", &[], PREFIX_LEGEND)
    } else {
        String::new()
    };
    wrap(
        format!("{} row(s) from {graph}", rows.len()),
        format!("{legend}<view:table>{table}</view:table>"),
    )
}

#[async_trait]
impl Endpoint for Sparql {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let ledgers = readable_ledgers(&self.web, inv);
        let ledger = match optional(inv, "ledger") {
            Some(name) => Ledger::parse(&name)?,
            None => ledgers.first().cloned().unwrap_or_else(Ledger::default),
        };
        // ★ The exact grant, checked before anything runs: the declared family is "some
        // graph", and this names THE graph — the one the query will be confined to.
        let scope = ikigai_store::cap_read_graph(&ledger.graph());
        if !inv.capability.allows(&scope) {
            return Err(Error::Denied(format!(
                "this capability does not hold `{scope}`, so it cannot query `{}`",
                ledger.graph()
            )));
        }
        let query = optional(inv, "query");
        let asked = inv.inline_str("as").ok().map(bare).map(str::to_string);

        // Conneg: a caller asking for anything but HTML gets the store's own answer.
        if !self.fragment {
            if let (Some(query), Some(asked)) = (&query, &asked) {
                if asked != HTML {
                    let form = query_form(query).ok_or_else(|| Error::InvalidArgument {
                        name: "query".to_string(),
                        detail: "not a SELECT, ASK, CONSTRUCT or DESCRIBE query".to_string(),
                    })?;
                    let run = with(
                        with(
                            with(
                                request(Verb::Source, &format!("urn:iki:store:graph-{form}"))?,
                                "graph",
                                &ledger.graph(),
                            ),
                            "query",
                            query,
                        ),
                        "as",
                        asked,
                    );
                    return inv.issue(run).await;
                }
            }
        }

        if self.fragment {
            html_only(inv)?;
            let query = query.ok_or_else(|| Error::MissingArgument("query".to_string()))?;
            let results = sparql_results(inv, &ledger, &query).await;
            let doc = envelope("page", &[("view", "results"), ("full", "false")], &results);
            return Ok(html(render::render(&doc, false).map_err(render_err)?));
        }
        let results = match &query {
            Some(query) => sparql_results(inv, &ledger, query).await,
            None => String::new(),
        };
        let children = format!(
            "{}{}{}{}{}",
            nav(&self.web, inv, &ledgers, Some(ledger.name())),
            samples(),
            cross_graph(),
            element("query", &[], query.as_deref().unwrap_or(DEFAULT_QUERY)),
            results
        );
        let doc = envelope(
            "page",
            &[("view", "sparql"), ("full", "true"), ("title", "SPARQL")],
            &children,
        );
        Ok(html(render::render(&doc, true).map_err(render_err)?))
    }

    fn name(&self) -> &str {
        if self.fragment {
            "gonk-fragment-sparql"
        } else {
            "gonk-sparql"
        }
    }

    fn describe(&self) -> Description {
        let desc = Description::new(self.name())
            .title(if self.fragment {
                "SPARQL results, as an htmx fragment"
            } else {
                "SPARQL over one ledger's graph (this page's limit, not the store's)"
            })
            .summary(
                "A read-only SPARQL query. THIS page sends exactly one graph — the named \
                 ledger's — as the whole dataset: it runs at \
                 `urn:iki:store:graph-{select,ask,construct,describe}` with `graph=<that \
                 ledger's graph>`, under the caller's capability, so a grant naming one ledger \
                 cannot see another's. That store endpoint itself accepts a SET of graphs in \
                 one read (`graph=` is whitespace-separated), so a caller holding a read token \
                 for each can join across them — a ledger and the browse graph, say — by \
                 calling it directly; this page has no graph selector yet. FROM / FROM NAMED \
                 are refused by the store either way, over a set as much as over one graph. \
                 HTML when the caller asks for it; otherwise the store's own result format, by \
                 Accept.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_store::CAP_READ_GRAPH)
            .input(
                arg(
                    "ledger",
                    "Which ledger's graph to query; the first readable one when omitted.",
                )
                .optional(),
            );
        if self.fragment {
            desc.input(arg("query", "A SELECT, ASK, CONSTRUCT or DESCRIBE query."))
                .input(as_html_arg())
                .output(HTML)
        } else {
            desc.input(arg("query", "A SELECT, ASK, CONSTRUCT or DESCRIBE query.").optional())
                .input(
                    arg(
                        "as",
                        "text/html for the editor page; otherwise a SPARQL results or RDF \
                         serialization the store serves.",
                    )
                    .one_of([
                        HTML,
                        "application/sparql-results+json",
                        "application/sparql-results+xml",
                        "text/csv",
                        "text/tab-separated-values",
                        "text/turtle",
                        "application/n-triples",
                    ])
                    .default_value(HTML)
                    .optional(),
                )
                .output(HTML)
                .output("application/sparql-results+json")
                .output("application/sparql-results+xml")
                .output("text/csv")
                .output("text/tab-separated-values")
                .output("text/turtle")
                .output("application/n-triples")
        }
    }
}

// ----------------------------------------------------------------------------- passkeys

const PASSKEY_OPS: [&str; 6] = [
    "register-options",
    "register",
    "login-options",
    "login",
    "logout",
    "session",
];

struct PasskeyDoor {
    passkeys: Arc<Passkeys>,
}

fn json_repr(value: Value) -> Representation {
    Representation::new(
        ReprType::new("application/json"),
        value.to_string().into_bytes(),
    )
}

#[async_trait]
impl Endpoint for PasskeyDoor {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(Error::Endpoint(
                "the passkey resources answer Sink only".to_string(),
            ));
        }
        let op = binding(inv, "op")?;
        let now = identity::now_seconds();
        let refused = |detail: String| Error::InvalidArgument {
            name: "content".to_string(),
            detail,
        };
        let content = inv.inline_str("content").unwrap_or("");
        match op.as_str() {
            "register-options" | "login-options" => {
                let purpose = if op == "login-options" {
                    Purpose::Login
                } else {
                    Purpose::Register
                };
                let challenge = self
                    .passkeys
                    .challenge(purpose, now)
                    .map_err(Error::Unavailable)?;
                Ok(json_repr(
                    json!({ "challenge": challenge, "rpId": identity::RP_ID }),
                ))
            }
            "register" => {
                let enrolled = self
                    .passkeys
                    .register(content.as_bytes(), now)
                    .map_err(refused)?;
                Ok(json_repr(
                    json!({ "label": enrolled.label, "grant": enrolled.grant }),
                ))
            }
            "login" => {
                let (token, who) = self
                    .passkeys
                    .login(content.as_bytes(), now)
                    .map_err(Error::Denied)?;
                Ok(json_repr(json!({
                    "session": token,
                    "seconds": who.expires.saturating_sub(now),
                    "label": who.enrolled.label,
                    "grant": who.enrolled.grant,
                })))
            }
            "logout" => {
                self.passkeys.logout(content.trim());
                Ok(json_repr(json!({ "signedOut": true })))
            }
            "session" => match self.passkeys.identity(content.trim(), now) {
                Some(who) => Ok(json_repr(json!({
                    "label": who.enrolled.label,
                    "grant": who.enrolled.grant,
                    "scopes": who.scopes,
                    "seconds": who.expires.saturating_sub(now),
                }))),
                None => Err(Error::NotFound(
                    "no live session for that token".to_string(),
                )),
            },
            other => Err(Error::InvalidArgument {
                name: "op".to_string(),
                detail: format!("`{other}` is not one of {}", PASSKEY_OPS.join(", ")),
            }),
        }
    }

    fn name(&self) -> &str {
        "gonk-passkey"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-passkey")
            .title("Passkey sign-in")
            .summary(
                "The WebAuthn ceremonies and the session they open. `register-options` and \
                 `login-options` mint a single-use challenge; `register` enrols a credential \
                 against a one-time invite; `login` verifies an assertion and opens a session \
                 whose capability is its grant in gonk/grants.json; `session` and `logout` take \
                 the session token as the body. Public by design — this is how a caller with no \
                 grant gets one — so it declares no capability.",
            )
            .verb(Verb::Sink)
            .verb(Verb::Meta)
            .input(
                arg("op", "Which step.")
                    .binding()
                    .one_of(PASSKEY_OPS)
                    .default_value("login-options"),
            )
            .input(
                arg(
                    "content",
                    "The step's JSON body, or the session token for `session` and `logout` — \
                     where a pipe's value lands.",
                )
                .optional(),
            )
            .output("application/json")
    }
}

// ------------------------------------------------------------------------- render rules

/// The rule table, served. See [`crate::rules`] for what it decides and why it is a
/// resource rather than a constant.
struct RenderRules {
    turtle: Arc<str>,
}

#[async_trait]
impl Endpoint for RenderRules {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if let Ok(asked) = inv.inline_str("as") {
            if !matches!(bare(asked), "text/turtle" | "text/plain") {
                return Err(Error::InvalidArgument {
                    name: "as".to_string(),
                    detail: format!("`{asked}` is not a face this table serves; it serves Turtle"),
                });
            }
        }
        Ok(Representation::new(
            ReprType::new("text/turtle").with_param("charset", "utf-8"),
            self.turtle.as_bytes().to_vec(),
        ))
    }

    fn name(&self) -> &str {
        "gonk-render-rules"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-render-rules")
            .title("Which SPARQL result cells become controls")
            .summary(
                "The render rules as Turtle: a rule matches a cell on its term KIND, its value \
                 SHAPE, a value PREFIX or (where the value alone cannot say — a bare integer) \
                 the variable NAME, and turns it into a link or into a query the editor runs. \
                 A resource rather than a compiled-in list of blessed column names, because a \
                 result set carries whatever variables its author chose: a deployment replaces \
                 the whole table with <config home>/gonk/render-rules.ttl. Read-only here; \
                 public, because it describes presentation and reads no ledger.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                arg("as", "The face: this table serves Turtle.")
                    .one_of(["text/turtle", "text/plain"])
                    .default_value("text/turtle")
                    .optional(),
            )
            .output("text/turtle")
    }
}

// ------------------------------------------------------------------------------ assets

const ASSETS: [(&str, &str, &str); 3] = [
    ("gonk.css", "text/css", include_str!("../web/gonk.css")),
    ("gonk.js", "text/javascript", include_str!("../web/gonk.js")),
    (
        "htmx.min.js",
        "text/javascript",
        include_str!("../web/htmx.min.js"),
    ),
];

struct Asset;

#[async_trait]
impl Endpoint for Asset {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let name = binding(inv, "name")?;
        let (_, media, body) = ASSETS
            .iter()
            .find(|(n, _, _)| *n == name)
            .ok_or_else(|| Error::NotFound(format!("no asset called `{name}`")))?;
        Ok(Representation::new(
            ReprType::new(*media).with_param("charset", "utf-8"),
            body.as_bytes().to_vec(),
        ))
    }

    fn name(&self) -> &str {
        "gonk-asset"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-asset")
            .title("The HTML face's static files")
            .summary(
                "gonk.css, gonk.js and htmx 2.0.4, compiled into the binary so the page needs \
                 no other origin — which is what lets the default same-origin CSP stand.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                arg("name", "Which file.")
                    .binding()
                    .one_of(ASSETS.iter().map(|(n, _, _)| *n))
                    .default_value("gonk.css"),
            )
            .output("text/css")
            .output("text/javascript")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_body_decodes_plus_percent_and_repeats() {
        let fields = form("_ledger=default&content=First+line%0A%0Abody+%26+more&x=1&x=2&flag");
        assert_eq!(fields["content"], "First line\n\nbody & more");
        assert_eq!(fields["x"], "2");
        assert_eq!(fields["flag"], "");
        assert_eq!(form("a=%zz%4")["a"], "%zz%4");
        // A multibyte character right after `%` must not panic the request.
        assert_eq!(form("a=%é1&b=%%")["a"], "%é1");
    }

    #[test]
    fn the_query_form_is_read_past_the_prologue() {
        assert_eq!(query_form(DEFAULT_QUERY), Some("select"));
        assert_eq!(
            query_form("# a comment\nBASE <urn:x#>\nPREFIX a: <urn:a#> ask { ?s ?p ?o }"),
            Some("ask")
        );
        assert_eq!(
            query_form("CONSTRUCT WHERE { ?s ?p ?o }"),
            Some("construct")
        );
        assert_eq!(query_form("INSERT DATA { <a:b> <a:b> <a:b> }"), None);
    }
}
