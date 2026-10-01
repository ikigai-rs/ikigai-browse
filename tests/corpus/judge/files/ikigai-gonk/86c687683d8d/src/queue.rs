//! The **Queue** — the review pipeline's state, and the one place a human publishes.
//!
//! ```text
//! /queue          urn:iki:gonk:page:queue      Source  the page
//! /queue/rows     urn:iki:gonk:fragment:queue  Source  the same section, for htmx
//! /queue/decide   urn:iki:gonk:queue:decide    Sink    one human decision
//! ```
//!
//! Ledger [#444](http://localhost:1060/l/default/item/444). `ikigai-browse` 0.5.0 stopped a
//! review pass from minting annotations: a pass produces **pending findings**, and
//! `Sink urn:iki:finding:{id} decision=publish` is the only path into the
//! `urn:iki:annotation:` family. This is the face for that act.
//!
//! # ★ The rule, and the thing that enforces it
//!
//! Brian, 2026-09-19: *"Nothing gets published to Gonk except by the human."* The enforcement
//! is the capability layer, not this page: publishing requires `urn:cap:annotate`, and a
//! review pass no longer declares it, so a trigger can run complete and be unable to publish
//! anything. **This page's job is to not lie about that** — it renders a decision form only
//! to a caller who could actually submit it, because an offer you refuse is a worse UI than
//! no offer and teaches people to ignore refusals.
//!
//! ⚠ **What an anonymous loopback caller can do to a finding: nothing, in either direction.**
//! Measured rather than assumed — `tests/queue.rs::an_anonymous_loopback_caller_can_neither
//! _read_nor_decide_a_finding`. The anonymous grant is `grants_for_all(gonk.http.ledger,
//! Write)` ([`crate::doors::HttpDoor`]), which is four ledger tokens per ledger and nothing
//! else. Reading `urn:repo:{repo}:findings` needs `urn:cap:browse:read:*` and deciding needs
//! that plus `urn:cap:annotate`; the anonymous capability holds neither, so both are typed
//! `Denied`. The `can-write = true` an earlier arc measured for an anonymous caller is the
//! LEDGER's write, which is a different authority over a different graph.
//!
//! # ★★ The menu is rendered FROM THE CONTRACT
//!
//! The severity words, the decision words and the state words are never written down here.
//! They are read from the resources' own descriptions ([`one_of`]) — the same `one_of`
//! `ikigai-browse` serves to the Sink's ArgSpec, to the review prompt, and to its own menu,
//! from one constant. A fourth hard-coded copy in this crate would re-introduce exactly the
//! drift that design prevents, and it would drift silently: the page would go on offering
//! a word the resource had stopped accepting, and the refusal would arrive at the click.
//! ⚠ `tests/queue.rs::no_severity_word_is_written_down_in_this_crate` holds this file and the
//! stylesheet to it, with the CONTRACT as the oracle — so the assertion cannot go stale
//! either.
//!
//! ⚠ **Only the WORDING is ours.** `publish` gets the label "Publish to Gonk" and `decline`
//! gets "Decline"; a value this table does not know renders under its own word rather than
//! being dropped, so a new outcome is offered the day it exists.
//!
//! ⚠ **And when the contract cannot be read, no form is rendered at all** — not a fallback
//! list. A page that invents a menu when the manifold is silent is the failure this whole
//! approach exists to prevent, one layer up.
//!
//! # The asymmetry the page has to make legible
//!
//! A decision is **final**: an identical repeat is a no-op, and anything that would change
//! the record is refused naming what is on file. There is **no Delete on a finding** —
//! declining is how a human removes one, and the decline is the record. Undoing a
//! publication is `delete urn:iki:annotation:{id}`, a separate visible act under the same
//! capability, reached through the annotation itself in the browse face. The cards say so.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Description, Endpoint, Error, InputSource, Invocation, Iri,
    Kernel, Representation, Request, Result, Verb,
};
use serde_json::Value;

use crate::render::{self, element, envelope, wrap};
use crate::web::{self, Web};

/// `urn:iki:gonk:page:queue` — the whole page.
pub const QUEUE_IRI: &str = "urn:iki:gonk:page:queue";
/// Where that page is served, for the header link.
pub const QUEUE_PATH: &str = "/queue";
/// `urn:iki:gonk:fragment:queue` — the queue section alone, for an htmx swap.
pub const ROWS_IRI: &str = "urn:iki:gonk:fragment:queue";
/// Where the fragment is served.
pub const ROWS_PATH: &str = "/queue/rows";
/// `urn:iki:gonk:queue:decide` — one human decision, from a form.
pub const DECIDE_IRI: &str = "urn:iki:gonk:queue:decide";
/// Where the form posts.
pub const DECIDE_PATH: &str = "/queue/decide";
/// `urn:iki:gonk:fragment:queue-depth` — the nav badge, one number and one state word.
pub const BADGE_IRI: &str = "urn:iki:gonk:fragment:queue-depth";
/// Where the badge polls.
pub const BADGE_PATH: &str = "/queue/depth";

/// How often the badge asks, as htmx spells an interval. See [`Badge`] for why it is not
/// one second.
pub const BADGE_EVERY: &str = "10s";

/// The same cadence as arithmetic — the number [`BADGE_RECENT_MS`] is derived from.
///
/// ⚠ Two spellings of one number, because htmx wants `"10s"` and a window wants
/// milliseconds. `tests/queue.rs::the_badge_cadence_has_one_number_in_two_spellings` is what
/// keeps them the same number; nothing else can.
pub const BADGE_EVERY_MS: u64 = 10_000;

/// How long after a pass ENDS the badge still reports that something happened.
///
/// ★ **The window is the poll interval, and that is the whole of the fix for
/// [#469](http://localhost:1060/l/default/item/469).** A depth gauge can only report what is
/// true at the instant it is asked, so a pass that starts and finishes between two polls is
/// invisible to it — which is exactly what happened on the first live run: three seconds of
/// work between two ten-second polls, and the badge read 0 then 0. A window at least as long
/// as the interval makes *"anything since the last poll"* the unit, and then nothing can
/// happen entirely unseen: whatever the phase, at least one poll lands inside the window.
///
/// Twice the interval rather than exactly one, so the afterglow lasts one or two polls and a
/// person who looks up a moment late still sees it.
pub const BADGE_RECENT_MS: u64 = 2 * BADGE_EVERY_MS;

/// ⚠ The guarantee the fix rests on, checked by the COMPILER rather than believed: a window
/// shorter than the interval would let a pass begin and end between two polls and be seen by
/// neither, which is the bug itself.
const _: () = assert!(BADGE_RECENT_MS >= BADGE_EVERY_MS);

/// The event the header's badge asks the Queue page's list to refresh on, when its
/// `depth_rev` has changed — `web/gonk.js` relays it, `web/gonk.xsl` listens for it.
///
/// ★ **One cadence, not two.** The badge already polls; a second `every Ns` on the list
/// would double the server's clock and let the two drift. So the list has no clock of its
/// own: it refreshes when the poll that is already happening brings news, and never
/// otherwise.
pub const NEWS_EVENT: &str = "gonk:news";

/// The finding family's own name — the Sink a decision reaches, and the description the
/// menu is rendered from. Taken one id at a time (`{prefix}{id}`), never guessed.
const FINDING_PREFIX: &str = "urn:iki:finding:";

/// The machine face this module asks every resource it reads for.
const JSON: &str = "application/json";

/// How many rows a page draws before it stops and says so.
///
/// The ledger listing's bound and its reason (`README`, ledger #419): the server-side XSLT
/// costs milliseconds per row, and a page slow enough to read as a hung server is worse than
/// a page that admits what it left out. `?limit=` asks for more.
const ROWS: usize = 50;

/// The ceiling `?limit=all` means. Past this the bound REFUSES rather than truncating.
const MAX_ROWS: usize = 500;

// ------------------------------------------------------------------ the contract

/// The closed set a resource's own contract declares for one argument of one verb — the
/// ONLY source of a menu's options in this module.
///
/// `None` for five reasons, in the order the body rules them out. Every caller treats
/// `None` the same way — "do not render a menu", never "use the usual list" — so the code
/// cannot act on the difference, and the list is for the person holding a card that has no
/// form and wanting to know which thing is missing.
///
/// 1. `iri` is not a well-formed IRI. A bug on this side; it says nothing about the
///    resource, which was never asked.
/// 2. **It does not resolve.** Nothing is bound there, or what is bound does not answer
///    `Meta` — either way the kernel has no description to hand back at all. The resource
///    is ABSENT.
/// 3. **It resolves and does not answer that verb.** The endpoint is present and describes
///    itself; `verb` is simply not among the verbs it lists. The resource is PRESENT and
///    answers a different question. ⚠ This is not "declares no [`ActionSpec`]" — an
///    endpoint that answers the verb but authors flat gets a spec synthesized from its own
///    `inputs`, so a missing per-verb spec never reaches here. ⚠ And `Verb::Meta` is
///    filtered out of `action_specs()` upstream, so asking for it is `None` whatever the
///    contract says.
/// 4. It answers the verb and names no such input.
/// 5. It names the input and leaves it open-valued. A contract saying "any string" — a
///    decision, not an omission, and the reason this collapses to `None` with the rest is
///    that an open-valued input has no menu to draw either.
pub fn one_of(hub: &Kernel, iri: &str, verb: Verb, argument: &str) -> Option<Vec<String>> {
    let target = Iri::parse(iri).ok()?;
    let values = hub
        .describe(&target)?
        .action_specs()
        .into_iter()
        .find(|spec| spec.verb == verb)?
        .inputs
        .iter()
        .find(|input| input.name == argument)?
        .one_of
        .clone();
    (!values.is_empty()).then_some(values)
}

/// The findings resource for one root.
fn findings_iri(root: &str) -> String {
    format!("urn:repo:{root}:findings")
}

/// One finding's own resource.
fn finding_iri(id: &str) -> String {
    format!("{FINDING_PREFIX}{id}")
}

/// Whether this capability may decide — the token `urn:iki:finding:{id}`'s Sink declares.
///
/// Presentation only: the Sink enforces it whatever this page shows, and
/// [`Decide`] declares it too, so the kernel refuses before dispatch. What this decides is
/// whether a form is drawn at all.
fn can_decide(inv: &Invocation<'_>) -> bool {
    inv.capability.allows(ikigai_browse::CAP_ANNOTATE)
}

/// Whether the header should carry a Queue link for this caller.
///
/// ★ Both halves, and the second is easy to forget: a caller must be able to DECIDE
/// (ledger #444 settled that — "a link to a page of things you cannot decide is worse than no
/// link") **and** there must be a repository it may read, or the page behind the link is a
/// refusal with a nav entry pointing at it.
pub(crate) fn offers_queue(web: &Web, inv: &Invocation<'_>) -> bool {
    can_decide(inv) && !crate::k::readable_roots(web, inv).is_empty()
}

// ---------------------------------------------------------------------- the page

/// `urn:iki:gonk:page:queue` and `urn:iki:gonk:fragment:queue`.
pub struct QueuePage {
    pub web: Arc<Web>,
    /// `false` renders the whole page; `true` renders the queue section alone.
    pub fragment: bool,
}

/// What one root's read answered.
enum Rows {
    /// The rows, in the order the resource returned them (already triage order).
    Got(Vec<Value>),
    /// The read was refused or failed, with the reason as the resource stated it.
    Failed(String),
}

/// What narrows a rendering: the three values both entrances agree on.
#[derive(Default)]
struct Params {
    /// Which part of the pipeline — validated against the findings contract, not here.
    state: Option<String>,
    /// One configured root, or every readable one.
    repo: Option<String>,
    /// How many rows to draw.
    limit: Option<String>,
}

impl Params {
    /// From a Source's query arguments.
    fn from(inv: &Invocation<'_>) -> Params {
        let arg = |name: &str| inv.inline_str(name).ok().map(str::to_string);
        Params {
            state: arg("state"),
            repo: arg("repo"),
            limit: arg("limit"),
        }
    }
}

/// How many rows to draw: `?limit=<n>` or `?limit=all`.
fn rows_wanted(limit: Option<&str>) -> Result<usize> {
    match limit {
        None => Ok(ROWS),
        Some("all") => Ok(MAX_ROWS),
        Some(other) => match other.parse::<usize>() {
            Ok(n) if n > 0 && n <= MAX_ROWS => Ok(n),
            _ => Err(Error::InvalidArgument {
                name: "limit".to_string(),
                detail: format!(
                    "`{other}`: a row count between 1 and {MAX_ROWS}, or `all` for {MAX_ROWS}"
                ),
            }),
        },
    }
}

impl QueuePage {
    /// Read one root's findings at `state`, under the CALLER's capability.
    async fn read(&self, inv: &Invocation<'_>, root: &str, state: &str) -> Rows {
        let iri = findings_iri(root);
        let Ok(target) = Iri::parse(&iri) else {
            return Rows::Failed(format!("`{iri}` is not an IRI"));
        };
        let request = Request::new(Verb::Source, target)
            .with_arg("as", ArgRef::Inline(b"application/json".to_vec()))
            .with_arg("state", ArgRef::Inline(state.as_bytes().to_vec()));
        match inv.issue(request).await {
            Err(e) => Rows::Failed(format!("{e}")),
            Ok(answer) => match serde_json::from_slice::<Value>(&answer.bytes) {
                Ok(Value::Array(rows)) => Rows::Got(rows),
                Ok(_) | Err(_) => Rows::Failed(format!(
                    "`{iri}` answered something that is not a JSON array of findings"
                )),
            },
        }
    }

    /// The whole body: the state nav, the intray line, the rows, and the refusals.
    ///
    /// ⚠ `params` is passed rather than read from `inv`, because [`Decide`] renders this same
    /// section after a write and its invocation carries the FORM, not a query string — and
    /// `Invocation` has no reborrow that swaps the request (only `with_bindings`). Threading
    /// the three values is what keeps one renderer serving both entrances.
    async fn body(
        &self,
        inv: &Invocation<'_>,
        params: &Params,
        flash: Option<(&str, &str)>,
    ) -> Result<Representation> {
        let roots = crate::k::readable_roots(&self.web, inv);
        let wanted = rows_wanted(params.limit.as_deref())?;
        let only = params
            .repo
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string);
        if let Some(repo) = &only {
            if !roots.iter().any(|r| r == repo) {
                return Err(Error::Denied(format!(
                    "`{repo}` is not a repository this grant may read. A grant names one root \
                     as `{}{repo}`, or every root as `{}`.",
                    ikigai_browse::CAP_PREFIX,
                    ikigai_browse::CAP_WILDCARD
                )));
            }
        }
        let chosen: Vec<String> = match &only {
            Some(repo) => vec![repo.clone()],
            None => roots.clone(),
        };

        // ★ The states come from the findings resource's own contract, like every other menu
        // here. With no readable root there is no contract to read and no rows to filter, so
        // the nav is absent rather than invented.
        let states = roots
            .first()
            .and_then(|root| one_of(&self.web.hub, &findings_iri(root), Verb::Source, "state"));
        let state = match params.state.as_deref().map(str::trim) {
            None | Some("") => "pending".to_string(),
            Some(asked) => match &states {
                Some(known) if !known.iter().any(|s| s == asked) => {
                    return Err(Error::InvalidArgument {
                        name: "state".to_string(),
                        detail: format!("`{asked}` is not one of {}", known.join(", ")),
                    })
                }
                _ => asked.to_string(),
            },
        };

        let mut read: Vec<(String, Rows)> = Vec::new();
        for root in &chosen {
            let rows = self.read(inv, root, &state).await;
            read.push((root.clone(), rows));
        }

        let matched: usize = read
            .iter()
            .map(|(_, r)| match r {
                Rows::Got(rows) => rows.len(),
                Rows::Failed(_) => 0,
            })
            .sum();
        let refused: Vec<&(String, Rows)> = read
            .iter()
            .filter(|(_, r)| matches!(r, Rows::Failed(_)))
            .collect();

        let decide = can_decide(inv);
        let mut children = web::nav(&self.web, inv, &web::readable_ledgers(&self.web, inv), None);
        if let Some((kind, text)) = flash {
            children.push_str(&element("flash", &[("kind", kind)], text));
        }
        children.push_str(&self.intray_element(inv).await);
        if let Some(known) = &states {
            for name in known {
                children.push_str(&element(
                    "state",
                    &[
                        ("name", name),
                        ("href", &page_url(&state_query(name, only.as_deref()))),
                        ("rows-url", &rows_url(&state_query(name, only.as_deref()))),
                        ("current", flag(name == &state)),
                    ],
                    "",
                ));
            }
        }
        for (root, rows) in &read {
            if let Rows::Failed(why) = rows {
                children.push_str(&element("denied", &[("repo", root)], why));
            }
        }

        // The rows, each repository's in the order its resource returned them — which is
        // already triage order (severity rank, then path, then position). ⚠ They are NOT
        // re-sorted across repositories: that ordering is `ikigai-browse`'s, computed from a
        // severity rank this crate does not have and must not re-derive, so the page groups
        // rather than claiming a global order it did not compute.
        let mut drawn = 0usize;
        for (root, rows) in &read {
            let Rows::Got(rows) = rows else { continue };
            for row in rows {
                if drawn == wanted {
                    break;
                }
                children.push_str(&self.finding_element(
                    row,
                    root,
                    decide,
                    &state,
                    only.as_deref(),
                ));
                drawn += 1;
            }
        }

        let mut attributes: Vec<(&str, String)> = vec![
            ("view", "queue".to_string()),
            ("full", flag(!self.fragment).to_string()),
            ("title", "Queue".to_string()),
            ("state", state.clone()),
            ("page-url", page_url(&state_query(&state, only.as_deref()))),
            ("rows-url", rows_url(&state_query(&state, only.as_deref()))),
            // ★ How this section re-fetches ITSELF when the header's poll brings news
            // ([#469](http://localhost:1060/l/default/item/469)). It is the rows URL with
            // this request's own `limit` kept, because a human who asked to see all 300
            // findings must not be quietly cut back to the first 50 by a refresh nobody
            // asked for.
            (
                "refresh-url",
                rows_url(&refresh_query(
                    &state,
                    only.as_deref(),
                    params.limit.as_deref(),
                )),
            ),
            ("news", NEWS_EVENT.to_string()),
            // ⚠ Shown only when a refresh was HELD BACK — `web/gonk.js` unhides it. The
            // wording lives here with every other sentence this face speaks, so the
            // stylesheet stays a stylesheet and the script stays a relay.
            (
                "stale-text",
                "New findings arrived while you were deciding. They will appear when this \
                 decision is submitted or the selection is left as it was."
                    .to_string(),
            ),
            (
                "message",
                "The review pipeline. ⚠ A queue, not a gate: nothing here blocks a commit, a \
                 push or a merge, and nothing reaches Gonk until a person publishes it."
                    .to_string(),
            ),
        ];
        if let Some(repo) = &only {
            attributes.push(("repo", repo.clone()));
        }
        if roots.is_empty() {
            attributes.push(("empty", "true".to_string()));
            attributes.push((
                "empty-text",
                if self.web.browse_roots.is_empty() {
                    "This server has no browse root configured (`gonk.browse.root`), so no \
                     review pass can run and there is no queue to show."
                        .to_string()
                } else {
                    format!(
                        "This browser holds no grant naming a repository here, so no finding is \
                         readable. A grant names one root as `{}<root>`, or every root as `{}`.",
                        ikigai_browse::CAP_PREFIX,
                        ikigai_browse::CAP_WILDCARD
                    )
                },
            ));
        } else if matched == 0 && refused.is_empty() {
            // ⚠ The affirmative sentence, and it is the point of writing it out: "no pending
            // findings" is a DIFFERENT statement from a page that failed to load, and the two
            // must not look alike (ledger #446).
            attributes.push(("empty", "true".to_string()));
            attributes.push((
                "empty-text",
                format!(
                    "No {state} findings in {}. Nothing is waiting for a decision.",
                    match &only {
                        Some(repo) => repo.clone(),
                        None => format!("{} repositories", chosen.len()),
                    }
                ),
            ));
        } else if matched > 0 {
            attributes.push((
                "count-text",
                count_sentence(matched, drawn, &state, &chosen),
            ));
            if drawn < matched {
                attributes.push(("more", "true".to_string()));
                attributes.push((
                    "more-url",
                    page_url(&format!(
                        "{}&limit=all",
                        state_query(&state, only.as_deref())
                    )),
                ));
                attributes.push((
                    "more-rows-url",
                    rows_url(&format!(
                        "{}&limit=all",
                        state_query(&state, only.as_deref())
                    )),
                ));
                attributes.push(("more-label", format!("show all {matched}")));
            }
        }
        if !decide && !roots.is_empty() {
            attributes.push(("posture", "read-only".to_string()));
            attributes.push((
                "posture-text",
                format!(
                    "This grant may read findings and not publish them: that needs `{}`. \
                     Nothing gets published to Gonk except by a human who holds it.",
                    ikigai_browse::CAP_ANNOTATE
                ),
            ));
        }
        let attributes: Vec<(&str, &str)> =
            attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let doc = envelope("page", &attributes, &children);
        Ok(web::html(
            render::render(&doc, !self.fragment).map_err(web::render_err)?,
        ))
    }

    /// The intray's depth, as one affirmative sentence — see [`crate::trigger::depth`].
    ///
    /// ⚠ Since the trigger can be ARMED ([#466](http://localhost:1060/l/default/item/466))
    /// the count alone is no longer the statement: a queue with twelve waiting and a pass in
    /// flight is working, and a queue with twelve waiting and nothing in flight is a dead
    /// watcher. Both render as "12 waiting" if only the number is printed, and exactly one
    /// of them needs a person. So the trigger's own [`crate::trigger::Status`] writes the
    /// sentence and this only chooses the word the stylesheet colours it by.
    async fn intray_element(&self, inv: &Invocation<'_>) -> String {
        match read_depth(inv).await {
            Ok(status) => element(
                "intray",
                &[("kind", depth_kind(&status))],
                depth_sentence(&status),
            ),
            // ★ **An UNRESOLVED depth is "no queue configured", and it is the manifold
            // saying so rather than this page guessing.** `gonk.review.space` is the whole
            // switch: with no line, `trigger::space` binds nothing, so the absence of the
            // resource IS the absence of the queue. Reading it that way keeps
            // declared = enforced — an unconfigured gonk must not advertise a queue — and
            // leaves the page with one fewer fact of its own to get wrong.
            Err(Error::Unresolved(_) | Error::NotFound(_)) => element(
                "intray",
                &[("kind", "absent")],
                "No review queue is configured (`gonk.review.space`), so nothing is dropping \
                 review requests here.",
            ),
            // ⚠ Anything else is NOT "no queue". This page renders only for callers holding
            // the browse read the depth asks for, so a failure here is a real one rather
            // than a posture — and a depth that cannot be read is the one thing this line
            // must not render as a zero (ledger #446).
            Err(e) => element(
                "intray",
                &[("kind", "error")],
                &format!("The review queue's depth could not be read: {e}"),
            ),
        }
    }

    /// One row.
    fn finding_element(
        &self,
        row: &Value,
        root: &str,
        decide: bool,
        state: &str,
        only: Option<&str>,
    ) -> String {
        let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("");
        let yes = |key: &str| row.get(key).and_then(Value::as_bool).unwrap_or(false);
        let id = text("id");
        let annotates = text("annotates");
        let path = text("path");
        let where_ = match row.get("line").and_then(Value::as_u64) {
            Some(line) => format!("{path}:{line}"),
            None => path.to_string(),
        };
        let proposal = row.get("severity").and_then(Value::as_str);
        let row_state = text("state");

        let mut attributes: Vec<(&str, String)> = vec![
            ("id", id.to_string()),
            ("iri", text("iri").to_string()),
            // The row's own `repo`, falling back to the root this read was ISSUED for — a
            // fact the page knows independently, so a row that omits the field is still
            // attributed rather than rendered under an empty name.
            (
                "repo",
                match text("repo") {
                    "" => root.to_string(),
                    named => named.to_string(),
                },
            ),
            ("where", where_),
            ("state", row_state.to_string()),
            ("severity", proposal.unwrap_or("unrated").to_string()),
            (
                "severity-label",
                match proposal {
                    Some(word) => format!("model: {word}"),
                    None => "model: unrated".to_string(),
                },
            ),
            ("orphaned", flag(yes("orphaned")).to_string()),
            ("reanchored", flag(yes("reanchored")).to_string()),
            ("provenance", provenance(row)),
        ];
        if !annotates.is_empty() {
            attributes.push(("browse-href", browse_url(annotates)));
        }
        // ⚠ **Only when a human actually rated it.** `effective_severity` FALLS BACK to the
        // model's proposal when there is no decision — which is right for a consumer that
        // wants one number, and wrong for a badge labelled "human": rendering it on a pending
        // row would show a rating nobody made, on the one page built to keep the two apart.
        if let (Some(effective), true) = (
            row.get("effective_severity").and_then(Value::as_str),
            row.get("decision").is_some_and(|d| !d.is_null()),
        ) {
            attributes.push(("effective", effective.to_string()));
        }

        let mut children = element("body", &[], text("body"));
        let quote = text("exact");
        if !quote.is_empty() {
            children.push_str(&element("quote", &[], quote));
        }
        if let Some(decision) = row.get("decision").filter(|d| !d.is_null()) {
            children.push_str(&decision_element(decision));
        } else if decide {
            children.push_str(&self.decide_element(id, proposal, state, only));
        }
        let attributes: Vec<(&str, &str)> =
            attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        wrap("finding", &attributes, &children)
    }

    /// The decision form for one undecided finding — **built from the contract**.
    ///
    /// ⚠ When either menu is missing from the description, no form is rendered and the card
    /// says why. A hard-coded fallback here would be the fourth copy of the severity set and
    /// would go on offering words the resource had stopped accepting.
    fn decide_element(
        &self,
        id: &str,
        proposal: Option<&str>,
        state: &str,
        only: Option<&str>,
    ) -> String {
        let iri = finding_iri(id);
        let (Some(severities), Some(decisions)) = (
            one_of(&self.web.hub, &iri, Verb::Sink, "severity"),
            one_of(&self.web.hub, &iri, Verb::Sink, "decision"),
        ) else {
            return element(
                "no-form",
                &[],
                &format!(
                    "`{iri}` does not describe its `severity` and `decision` menus, so this \
                     page will not invent them. Nothing can be decided here until it does."
                ),
            );
        };
        let mut options = String::new();
        // ⚠ The model may have proposed NOTHING, and the contract says so: `severity` omitted
        // means "accept the proposal", which is an error when there is none. So an unrated
        // finding gets a placeholder that is selected and not submittable — the human must
        // choose, which is what the resource will insist on anyway.
        if proposal.is_none() {
            options.push_str(&element(
                "severity-option",
                &[
                    ("value", ""),
                    ("label", "choose a rating"),
                    ("selected", "true"),
                    ("placeholder", "true"),
                ],
                "",
            ));
        }
        for value in &severities {
            options.push_str(&element(
                "severity-option",
                &[
                    ("value", value),
                    ("label", value),
                    ("selected", flag(proposal == Some(value.as_str()))),
                    ("placeholder", "false"),
                ],
                "",
            ));
        }
        for value in &decisions {
            options.push_str(&element(
                "decision-option",
                &[
                    ("value", value),
                    ("label", decision_label(value)),
                    ("action", DECIDE_PATH),
                    // ⚠ The htmx payload is built HERE because a curly brace cannot appear in
                    // a literal attribute value in the stylesheet's engine at all (they are
                    // attribute-value-template delimiters), and JSON is nothing but curly
                    // braces. The button's own `name`/`value` carries the same word for the
                    // scripting-off path, so both entrances send one decision.
                    ("vals", &format!(r#"{{"decision":"{value}"}}"#)),
                ],
                "",
            ));
        }
        wrap(
            "decide",
            &[
                ("action", DECIDE_PATH),
                ("id", id),
                ("state", state),
                ("repo", only.unwrap_or("")),
                ("rows-url", &rows_url(&state_query(state, only))),
                ("required", flag(proposal.is_none())),
            ],
            &options,
        )
    }
}

/// The record of a decision already taken, and the asymmetry that follows it.
fn decision_element(decision: &Value) -> String {
    let text = |key: &str| decision.get(key).and_then(Value::as_str).unwrap_or("");
    let outcome = text("outcome");
    let minted = text("minted");
    let mut attributes: Vec<(&str, String)> = vec![
        ("outcome", outcome.to_string()),
        ("severity", text("severity").to_string()),
        ("at", web::when(text("decided_at"))),
        (
            "undo",
            // ★ The asymmetry, said rather than hidden: a publication is undone by deleting
            // the annotation it minted (a separate, visible act under the same capability);
            // a decline is not undone at all, because the decline IS the record.
            match outcome {
                "published" => format!(
                    "Published. Undoing this is a separate act: `delete {minted}` — the \
                     annotation, not the finding."
                ),
                _ => "Declined. The decline is the record: a finding is never deleted, and a \
                      re-run knows a person looked and said no."
                    .to_string(),
            },
        ),
    ];
    if !minted.is_empty() {
        attributes.push(("minted", minted.to_string()));
        attributes.push(("minted-href", browse_url(minted)));
    }
    let note = text("note");
    let attributes: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
    wrap("decision", &attributes, &element("note", &[], note))
}

/// The wording for one decision value. ⚠ The SET is the contract's; only these words are
/// ours, and a value this table does not know keeps its own word rather than disappearing.
fn decision_label(value: &str) -> &str {
    match value {
        "publish" => "Publish to Gonk",
        "decline" => "Decline",
        other => other,
    }
}

/// Where a finding came from, in one line.
fn provenance(row: &Value) -> String {
    let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("");
    let mut parts: Vec<String> = Vec::new();
    match text("creator") {
        "" => parts.push("an unnamed reviewer".to_string()),
        who => parts.push(format!("found by {who}")),
    }
    if !text("generated_by").is_empty() {
        parts.push(format!("in pass {}", text("generated_by")));
    }
    if !text("created").is_empty() {
        parts.push(web::when(text("created")));
    }
    if !text("content_hash").is_empty() {
        parts.push(format!("against {}", text("content_hash")));
    }
    parts.join(" · ")
}

// ------------------------------------------------------------------ the live badge

/// Read [`crate::trigger::DEPTH`]'s JSON face through the kernel, under the CALLER's
/// capability.
///
/// ★ **Through the kernel, although this very process holds the counters.** The depth is a
/// resource with a capability floor now
/// ([#464](http://localhost:1060/l/default/item/464) chose that over minting the space's read
/// token), and a page reaching past the floor into `crate::trigger`'s own functions would
/// make the floor decorative: the rendering caller would be shown a number the same caller is
/// refused at the door. It also means the badge and the socket answer from one place.
async fn read_depth(inv: &Invocation<'_>) -> Result<Value> {
    let answer = inv
        .issue(
            Request::new(
                Verb::Source,
                Iri::parse(crate::trigger::DEPTH)
                    .map_err(|e| Error::Endpoint(format!("{}: {e}", crate::trigger::DEPTH)))?,
            )
            .with_arg("as", ArgRef::Inline(JSON.as_bytes().to_vec())),
        )
        .await?;
    serde_json::from_slice(&answer.bytes).map_err(|e| {
        Error::Endpoint(format!(
            "{} did not answer JSON: {e}",
            crate::trigger::DEPTH
        ))
    })
}

/// The sentence that resource wrote — never re-derived here, so the badge, the page and the
/// socket cannot disagree about what a queue is doing.
fn depth_sentence(status: &Value) -> &str {
    status
        .get("sentence")
        .and_then(Value::as_str)
        .unwrap_or("The review queue's depth could not be read.")
}

/// The word the stylesheet colours by. The only thing this face adds to the resource's own
/// answer, because a colour is presentation and a count is not.
///
/// ⚠ **`in_flight` is asked BEFORE the count, and that ordering is a bug fix.** It used to be
/// a guard on the `Some(_)` arm, so a pass running against an EMPTY inbox — which is the
/// ordinary case, because the reactor moves the tuple out of the inbox before it starts the
/// work — rendered as `empty`. The badge was showing "nothing is happening" during the one
/// interval when something was ([#469](http://localhost:1060/l/default/item/469)).
fn depth_kind(status: &Value) -> &'static str {
    let yes = |key: &str| status.get(key).and_then(Value::as_bool).unwrap_or(false);
    if !yes("configured") {
        return "absent";
    }
    if status.get("unreadable").is_some_and(|v| !v.is_null()) {
        return "error";
    }
    if yes("stuck") {
        return "stuck";
    }
    if yes("in_flight") {
        return "working";
    }
    match status.get("waiting").and_then(Value::as_u64) {
        // An empty queue that JUST finished something is a different statement from an empty
        // queue that has been idle for an hour, and only the first one answers "is anything
        // happening". See [`BADGE_RECENT_MS`].
        Some(0) | None => match depth_activity(status) {
            Some(_) => "recent",
            None => "empty",
        },
        Some(_) => "count",
    }
}

/// What the badge draws a spark for: a pass running now, or one that ended inside
/// [`BADGE_RECENT_MS`]. `None` = nothing to say.
///
/// ★ This is the ACTIVITY question, and it is not the depth question. "Is there a backlog"
/// and "is anything happening" have different answers on a fast queue, and the badge was
/// only ever answering the first ([#469](http://localhost:1060/l/default/item/469)). Both
/// answers come out of the same poll of the same resource — the numbers were already in
/// `urn:iki:gonk:review:depth` and were being discarded here.
fn depth_activity(status: &Value) -> Option<&'static str> {
    if status.get("in_flight").and_then(Value::as_bool) == Some(true) {
        return Some("running");
    }
    match status.get("since_last_pass_ms").and_then(Value::as_u64) {
        Some(ms) if ms <= BADGE_RECENT_MS => Some("recent"),
        _ => None,
    }
}

/// The queue's whole observable state as one opaque token: what a poll compares against the
/// poll before it to decide whether there is anything new to show.
///
/// ★ **It is a REVISION, not a timestamp**, so it is stable while nothing moves — which is
/// what makes "refresh the list only on news" possible at all. Every number here is one the
/// depth resource already publishes, and each one changing means a finding may have appeared,
/// been handed to a pass, or been given up on.
///
/// ⚠ **What it cannot see: a finding minted by ANOTHER process.** `passes_*` are this
/// server's own counters ([`crate::trigger::Passes`] — "what this process has spent"), and
/// `waiting`/`handled` are this server's queue. A review run from a second gonk, or by hand
/// through the CLI, writes findings into the same browse graph and moves none of these, so
/// this page will not learn of it until its own queue moves. A revision on the findings
/// resource itself is `ikigai-browse`'s to offer and is reported up rather than guessed at
/// here.
fn depth_rev(status: &Value) -> String {
    let n = |key: &str| match status.get(key).and_then(Value::as_u64) {
        Some(value) => value.to_string(),
        None => "-".to_string(),
    };
    format!(
        "{}.{}.{}.{}.{}",
        n("waiting"),
        n("handled"),
        n("dead_lettered"),
        n("passes_succeeded"),
        n("passes_failed")
    )
}

/// `urn:iki:gonk:fragment:queue-depth` — the nav badge, polled by htmx.
///
/// # ★ Why this exists at all: it is the liveness signal, not decoration
///
/// Brian asked for *"notifications so someone knows there are things in the queue (kind of
/// like the Live indicator in the Web demo)"*. ⚠ That indicator is a **static dot**; what is
/// actually live in web-demo is `hx-trigger="load, every 1s"` on the nav clock, and this is
/// that shape. But the reason is stronger than a nicety: `SpaceReactor::watch` catches up at
/// startup and then lives on a thread, and **a watcher thread that dies while gonk lives
/// drains nothing and says nothing until a restart**. gonk runs no log at all
/// ([#383](http://localhost:1060/l/default/item/383)), so a depth that stops falling is the
/// only symptom there is, and it has to be somewhere a person is already looking.
///
/// A stuck queue looks like this: the badge shows a number, it does not go down, and the
/// Queue page's own line says `NONE IN FLIGHT`. A slow one shows a number that falls and a
/// pass in flight. They are told apart by the second fact, never by the first.
///
/// ⚠ **[`BADGE_EVERY`] is ten seconds, not one.** The clock in web-demo is a clock and has to
/// tick; this is a queue whose entries take a model call each, and every poll here is an
/// XSLT render plus a directory listing. A second would cost this server more than the thing
/// it is watching.
///
/// # ★★ What one poll carries, after [#469](http://localhost:1060/l/default/item/469)
///
/// The first live end-to-end run showed the cadence was not the problem and the RENDERING
/// was: a pass ran in three seconds between two polls, and a badge that draws only a depth
/// had nothing to say about it either time. One poll now answers three questions from the
/// one read it was already doing:
///
/// - **how deep** — `count` and `kind`, as before;
/// - **is anything happening** — `depth_activity`, which survives a pass shorter than the
///   interval because its window IS the interval ([`BADGE_RECENT_MS`]);
/// - **is there anything new to show** — `depth_rev`, which `web/gonk.js` compares against
///   the previous poll's and turns into one [`NEWS_EVENT`] when it changes.
///
/// ★ So the Queue page's findings list refreshes off THIS poll and has no clock of its own:
/// one cadence, owned in Rust, and a list that is re-fetched when there is news rather than
/// every ten seconds regardless.
pub struct Badge {
    pub web: Arc<Web>,
}

#[async_trait]
impl Endpoint for Badge {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "the queue badge answers Source, not {:?}",
                inv.request.verb
            )));
        }
        let (kind, text, count, activity, rev) = match read_depth(inv).await {
            Ok(status) => (
                depth_kind(&status),
                depth_sentence(&status).to_string(),
                status
                    .get("waiting")
                    .and_then(Value::as_u64)
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                depth_activity(&status),
                depth_rev(&status),
            ),
            // No `gonk.review.space`: nothing is bound, so there is nothing to show — and
            // nothing is the right answer rather than a zero, which would say "empty queue".
            // The host span collapses on `.queue-badge:empty`.
            Err(Error::Unresolved(_) | Error::NotFound(_)) => return Ok(web::html(String::new())),
            // ⚠ A badge that renders nothing when it cannot read is a badge that looks like
            // an empty queue. It says so instead, in the one character it has room for.
            // ⚠ And NO revision: a depth that cannot be read says nothing about whether the
            // findings list has changed, so the list is left alone. A stale list beside a red
            // badge is honest; a list that re-fetches every ten seconds because the badge is
            // broken is a second failure on top of the first.
            Err(e) => (
                "error",
                format!("{e}"),
                "!".to_string(),
                None,
                String::new(),
            ),
        };
        let mut attributes = vec![
            ("view", "queue-badge"),
            ("kind", kind),
            ("count", count.as_str()),
            ("title", text.as_str()),
        ];
        if let Some(activity) = activity {
            attributes.push(("activity", activity));
        }
        if !rev.is_empty() {
            attributes.push(("rev", rev.as_str()));
        }
        let doc = envelope("page", &attributes, "");
        Ok(web::html(
            render::render(&doc, false).map_err(web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-queue-badge"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-queue-badge")
            .title("The review queue's depth, for the header")
            .summary(
                "One number, one state word and one revision, polled by the Queue link in \
                 the header. It renders `urn:iki:gonk:review:depth` and adds nothing to it \
                 but a colour — and it is a LIVENESS signal rather than a count: a queue \
                 that is armed and not empty with nothing in flight is a dead watcher, and a \
                 pass shorter than the poll interval still reports itself, because the \
                 window the activity is measured over is the interval itself. The revision \
                 is what the Queue page's findings list refreshes on, so there is one \
                 cadence on this server and not two.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            .input(web::as_html_arg())
            .output(web::HTML)
    }
}

/// What the filter MATCHED and what the page DREW, which are different numbers.
fn count_sentence(matched: usize, drawn: usize, state: &str, chosen: &[String]) -> String {
    let scope = match chosen {
        [one] => one.clone(),
        many => format!("{} repositories", many.len()),
    };
    if drawn < matched {
        format!("showing the first {drawn} of {matched} {state} findings in {scope}")
    } else {
        format!(
            "{matched} {state} finding{} in {scope}",
            if matched == 1 { "" } else { "s" }
        )
    }
}

fn flag(yes: bool) -> &'static str {
    if yes {
        "true"
    } else {
        "false"
    }
}

/// `state=pending` or `state=pending&repo=x` — the query both URLs carry.
fn state_query(state: &str, repo: Option<&str>) -> String {
    match repo {
        Some(repo) if !repo.is_empty() => format!("state={state}&repo={repo}"),
        _ => format!("state={state}"),
    }
}

fn page_url(query: &str) -> String {
    format!("{QUEUE_PATH}?{query}")
}

fn rows_url(query: &str) -> String {
    format!("{ROWS_PATH}?{query}")
}

/// The query a self-refresh repeats: the filter AND the row bound this request was made
/// with, so a refresh shows what the human is already looking at rather than the default.
fn refresh_query(state: &str, repo: Option<&str>, limit: Option<&str>) -> String {
    match limit.map(str::trim).filter(|l| !l.is_empty()) {
        Some(limit) => format!("{}&limit={limit}", state_query(state, repo)),
        None => state_query(state, repo),
    }
}

/// A `urn:*` resource's page in gonk's own browse shell.
fn browse_url(iri: &str) -> String {
    format!("{}/{iri}", crate::k::ROOTS_PATH)
}

#[async_trait]
impl Endpoint for QueuePage {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(
                "the queue page answers Source only".to_string(),
            ));
        }
        web::html_only(inv)?;
        self.body(inv, &Params::from(inv), None).await
    }

    fn name(&self) -> &str {
        if self.fragment {
            "gonk-fragment-queue"
        } else {
            "gonk-page-queue"
        }
    }

    fn describe(&self) -> Description {
        let id = self.name();
        Description::new(id)
            .title(if self.fragment {
                "The review queue, as a fragment"
            } else {
                "The review queue"
            })
            .summary(
                "Every machine review finding this caller's grant may read, in the triage \
                 order `urn:repo:{repo}:findings` returns them, with the model's proposed \
                 severity, the human's rating once there is one, and — for a caller holding \
                 `urn:cap:annotate` — a publish/decline form whose menus are rendered from \
                 the finding resource's OWN contract rather than from any list held here. \
                 It also shows the git-event trigger's intray depth, which no other face \
                 has, so \"nothing has happened yet\" and \"39 still queued\" are different \
                 sentences. ⚠ A queue, not a gate: nothing in it blocks a commit, a push or \
                 a merge.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("state")
                    .optional()
                    .class(XSD_STRING)
                    .summary(
                        "which part of the pipeline to show — the values \
                         `urn:repo:{repo}:findings` declares (pending, published, declined, \
                         all). Validated against that contract, never against a list here.",
                    )
                    .default_value("pending"),
            )
            .input(
                ArgSpec::new("repo")
                    .optional()
                    .class(XSD_STRING)
                    .summary("one configured browse root; omitted = every root this grant reads"),
            )
            .input(
                ArgSpec::new("limit")
                    .optional()
                    .class(XSD_STRING)
                    .summary(format!(
                        "how many rows to draw: 1–{MAX_ROWS}, or `all` for {MAX_ROWS} \
                         (default {ROWS})"
                    )),
            )
            .input(web::as_html_arg())
            .output("text/html")
    }
}

/// `xsd:string`, spelled once.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

// -------------------------------------------------------------------- the decision

/// `urn:iki:gonk:queue:decide` — one human decision, from the queue page's form.
///
/// ★ **It declares `urn:cap:annotate`, and that is not decoration.** Every call it can make
/// is `Sink urn:iki:finding:{id}`, which requires exactly that token — so declaring it here
/// is true, the kernel refuses before dispatch, and `urn:kernel:actions` does not offer the
/// action to a caller who could not complete it. (`crate::k`'s adapter cannot declare a floor
/// because its target is not known until its command is read; this one has a single family.)
pub struct Decide {
    pub web: Arc<Web>,
}

#[async_trait]
impl Endpoint for Decide {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(Error::Endpoint(format!("`{DECIDE_IRI}` answers Sink only")));
        }
        let mut fields = web::form(inv.inline_str("content")?);
        let mut take = |name: &str| fields.remove(name).filter(|v| !v.trim().is_empty());
        let id = take("id").ok_or_else(|| Error::InvalidArgument {
            name: "id".to_string(),
            detail: "the form names no finding".to_string(),
        })?;
        let state = take("_state").unwrap_or_else(|| "pending".to_string());
        let repo = take("_repo");
        let target = finding_iri(&id);
        let target_iri = Iri::parse(&target).map_err(|e| Error::InvalidArgument {
            name: "id".to_string(),
            detail: format!("`{target}`: {e}"),
        })?;

        // ★ The manifold decides what the form may send — the rule `crate::web::Act` states,
        // for the same reason: a dropped `severity=` here would not fail, it would silently
        // record the model's proposal as a human's considered rating.
        let description = self
            .web
            .hub
            .describe(&target_iri)
            .ok_or_else(|| Error::NotFound(format!("nothing is bound at `{target}`")))?;
        let spec = description
            .action_specs()
            .into_iter()
            .find(|spec| spec.verb == Verb::Sink)
            .ok_or_else(|| Error::InvalidArgument {
                name: "id".to_string(),
                detail: format!("`{target}` declares no Sink, so nothing can be decided"),
            })?;
        let mut request = Request::new(Verb::Sink, target_iri);
        for (name, value) in fields {
            if name.starts_with('_') || value.trim().is_empty() {
                continue;
            }
            let declared = spec
                .inputs
                .iter()
                .any(|input| input.name == name && input.source != InputSource::Binding);
            if !declared {
                return Err(Error::InvalidArgument {
                    name,
                    detail: format!(
                        "`{target}` does not declare this input for Sink; a form may send only \
                         what the resource's own contract names"
                    ),
                });
            }
            request = request.with_arg(
                name,
                ArgRef::Inline(value.replace("\r\n", "\n").into_bytes()),
            );
        }

        // ⚠ A refusal is RENDERED, not returned as a status. The two refusals this resource
        // makes are the interesting ones — "a decision is not overwritten, here is what is on
        // file" and the capability denial — and htmx does not swap a non-2xx response, so
        // returning one would make the page silently ignore the most informative answer the
        // system can give. The capability layer has already refused; this decides how the
        // refusal is read.
        let flash = match inv.issue(request).await {
            Ok(answer) => (
                "ok",
                String::from_utf8_lossy(&answer.bytes).trim().to_string(),
            ),
            Err(e) => ("error", format!("{e}")),
        };
        let rows = QueuePage {
            web: Arc::clone(&self.web),
            fragment: true,
        };
        let params = Params {
            state: Some(state),
            repo,
            limit: None,
        };
        rows.body(inv, &params, Some((flash.0, flash.1.as_str())))
            .await
    }

    fn name(&self) -> &str {
        "gonk-queue-decide"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-queue-decide")
            .title("Publish or decline one finding")
            .summary(
                "The Queue page's form adapter: an urlencoded body naming `id`, `decision` \
                 and optionally `severity` and `content` (the human's reason), forwarded to \
                 `urn:iki:finding:{id}`'s Sink under the caller's own capability and answered \
                 with the queue section re-rendered. ⚠ A refusal — a capability denial, or a \
                 second decision that would CHANGE a recorded one — is rendered into that \
                 section rather than returned as a status, because the refusal is the most \
                 informative answer here and a page that drops it teaches people to ignore \
                 refusals. It declares `urn:cap:annotate` because every call it can make \
                 requires it: nothing gets published to Gonk except by a human who holds it.",
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("forward one decision and re-render the queue")
                    .requires(ikigai_browse::CAP_ANNOTATE)
                    .input(ArgSpec::new("content").class(XSD_STRING).summary(
                        "the form: `id`, `decision`, optional `severity` and `content`, \
                                 plus `_state` and `_repo` for the re-render",
                    ))
                    .output("text/html"),
            )
            .verb(Verb::Meta)
    }
}
