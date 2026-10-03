//! The Queue's **walk** — the unconfirmed declines that still steer, revisited one by one
//! (ledger [#653](http://localhost:1060/l/default/item/653)).
//!
//! ```text
//! /queue?summary=unconfirmed        urn:iki:gonk:page:queue       Source  the page, as the walk
//! /queue/rows?summary=unconfirmed   urn:iki:gonk:fragment:queue   Source  the same section, for htmx
//! /queue/decide                     urn:iki:gonk:queue:decide     Sink    one revision (`revises=`)
//! ```
//!
//! # Why it exists
//!
//! Brian, 2026-10-01: *"Some of the previous declines may have not been intentional."* Measured
//! the same day: the 43 declines that pending recurrences pointed at carried no reason word, and
//! 39 of them were made in same-second bursts, before the batch view existed. Each still marked
//! its recurrences and, by the 2026-09-25 rule, pre-ticked them in a recurrence batch — one
//! mis-click, propagated. `ikigai-browse` 0.14.0 made a decision REVISABLE (a later decision
//! names the current one with `revises=`, and both are kept) and computes whether a decline was
//! evidently meant (`confirmed`). `urn:repo:{repo}:findings summary=unconfirmed` lists the ones
//! that were not and still steer a pending finding, grouped by the burst or batch they were
//! made in, oldest first. This is the face that lets a person walk that list.
//!
//! # ★ The rules
//!
//! - **The list is browse's.** Which declines are unconfirmed, how they group and in what
//!   order is the findings resource's answer, read under the CALLER's capability per readable
//!   root; nothing is re-derived here. A burst is counted graph-wide, so one burst can surface
//!   in two roots' answers: those are joined by `(by, key)` into one group.
//! - **Every revision is an ordinary single decision.** The forms post to the Queue's own
//!   [`crate::queue::Decide`] with `revises=<the decline's IRI>`, so the door stamps
//!   `made=single` on it exactly as on a first answer, and the walk re-renders from the source:
//!   a confirmed, withdrawn or reversed decline leaves the list.
//! - **The menus are the contract's.** The severity and reason words are the finding Sink's
//!   `one_of`; the decision words are split by `crate::queue::decision_words` into the
//!   answers (confirm, reverse — one form, with a rating) and the revision-only words (one form
//!   of their own, with no rating, because the Sink refuses a rating beside a withdrawal).
//!   None of those words is spelled in this file.
//! - **A refused revision keeps its input** (ledger #657): the form comes back as it was sent.

use serde_json::Value;

use ikigai_core::{ArgRef, Error, Invocation, Iri, Kernel, Representation, Request, Result, Verb};

use crate::queue::{
    self, browse_url, can_decide, decision_words, finding_iri, findings_iri, flag, one_of,
    one_of_with_meanings, page_url, rows_url, Kept, QueuePage, DECIDE_PATH, REASONED_DECISION,
    REASON_ARG, REVISES_ARG,
};
use crate::render::{self, element, envelope, wrap};
use crate::web;

/// The Queue page's argument that asks for the walk — the findings face's own argument name.
pub const SUMMARY_ARG: &str = "summary";
/// The one `summary` word the Queue answers: browse's walk over unconfirmed declines.
/// ⚠ Offered only when the findings contract's own `summary` set declares it (`offered`).
pub const WALK: &str = "unconfirmed";
/// The link a recurrence mark carries to the walk.
pub const REVISIT_LABEL: &str = "revisit it";

/// The nav entry's label.
const NAV_LABEL: &str = "unconfirmed declines";

/// The slot the section leaves for the groups — see [`render::chunk`].
const WALK_SLOT: &str = "walk";

/// Whether the findings resource at `findings` declares the walk — browse 0.14.0 or later.
pub(crate) fn offered(hub: &Kernel, findings: &str) -> bool {
    one_of(hub, findings, Verb::Source, SUMMARY_ARG)
        .is_some_and(|words| words.iter().any(|w| w == WALK))
}

/// Whether a request asked for the walk — refused, never ignored, when it asked for something
/// the Queue cannot show: another `summary` word, a walk the contract does not declare, or a
/// walk AND a batch view at once.
pub(crate) fn wanted(asked: Option<&str>, offered: bool, group: Option<&str>) -> Result<bool> {
    let Some(asked) = asked.map(str::trim).filter(|a| !a.is_empty()) else {
        return Ok(false);
    };
    if asked != WALK {
        return Err(Error::InvalidArgument {
            name: SUMMARY_ARG.to_string(),
            detail: format!("`{asked}`: the Queue's one summary view is `{WALK}`"),
        });
    }
    if !offered {
        return Err(Error::InvalidArgument {
            name: SUMMARY_ARG.to_string(),
            detail: format!(
                "`{WALK}`: the findings resource declares no such summary, so there is no walk \
                 to show — it needs ikigai-browse 0.14.0 or later"
            ),
        });
    }
    if let Some(group) = group.map(str::trim).filter(|g| !g.is_empty()) {
        return Err(Error::InvalidArgument {
            name: SUMMARY_ARG.to_string(),
            detail: format!(
                "`{SUMMARY_ARG}={WALK}` and `{}={group}` are two different views — drop one",
                crate::batch::GROUP_ARG
            ),
        });
    }
    Ok(true)
}

/// `summary=unconfirmed`, `…&repo=x` — the walk's query.
fn query(repo: Option<&str>) -> String {
    let mut out = format!("{SUMMARY_ARG}={WALK}");
    if let Some(repo) = repo.filter(|r| !r.is_empty()) {
        out.push_str(&format!("&repo={repo}"));
    }
    out
}

/// The walk's page URL — what a recurrence mark links to.
pub(crate) fn href(repo: Option<&str>) -> String {
    page_url(&query(repo))
}

/// The nav entry beside the batch kinds.
pub(crate) fn nav(only: Option<&str>, current: bool) -> String {
    let q = query(only);
    element(
        "walk",
        &[
            ("label", NAV_LABEL),
            ("href", &page_url(&q)),
            ("rows-url", &rows_url(&q)),
            ("current", flag(current)),
        ],
        "",
    )
}

/// What the walk is rendered from — everything [`QueuePage::body`] already settled.
pub(crate) struct Frame<'a> {
    pub(crate) kinds: Option<&'a [String]>,
    pub(crate) states: Option<&'a [String]>,
    pub(crate) only: Option<&'a str>,
    pub(crate) scope: &'static str,
    pub(crate) chosen: &'a [String],
    pub(crate) no_roots: bool,
    pub(crate) flash: Option<(&'a str, &'a str)>,
    /// A REFUSED revision (ledger #657): drawn back into the one form it came from.
    pub(crate) kept: Option<&'a Kept>,
}

/// One root's walk, as the findings face's JSON answers it: the `unconfirmed` object.
async fn read_walk(inv: &Invocation<'_>, root: &str) -> std::result::Result<Value, String> {
    let iri = findings_iri(root);
    let target = Iri::parse(&iri).map_err(|_| format!("`{iri}` is not an IRI"))?;
    let request = Request::new(Verb::Source, target)
        .with_arg("as", ArgRef::Inline(queue::JSON.as_bytes().to_vec()))
        .with_arg(SUMMARY_ARG, ArgRef::Inline(WALK.as_bytes().to_vec()));
    let answer = inv.issue(request).await.map_err(|e| format!("{e}"))?;
    match serde_json::from_slice::<Value>(&answer.bytes) {
        Ok(Value::Object(mut body)) => match body.remove(WALK) {
            Some(walk @ Value::Object(_)) => Ok(walk),
            _ => Err(format!(
                "`{iri}` {SUMMARY_ARG}={WALK} answered no `{WALK}` object"
            )),
        },
        _ => Err(format!(
            "`{iri}` {SUMMARY_ARG}={WALK} answered something that is not a JSON object"
        )),
    }
}

/// One burst or batch, joined across roots.
struct Group {
    by: String,
    key: String,
    first: String,
    size: Option<u64>,
    declines: Vec<(String, Value)>,
}

/// `n thing` / `n things`.
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The walk: the queue section, with the unconfirmed declines in place of the rows.
pub(crate) async fn section(
    page: &QueuePage,
    inv: &Invocation<'_>,
    frame: Frame<'_>,
) -> Result<Representation> {
    let web = &page.web;
    let decide = can_decide(inv);
    let Frame {
        kinds,
        states,
        only,
        scope,
        chosen,
        no_roots,
        flash,
        kept,
    } = frame;

    // The reads, one per chosen root; the groups joined by `(by, key)` in the order first seen.
    let mut groups: Vec<Group> = Vec::new();
    let mut refused: Vec<(String, String)> = Vec::new();
    let (mut count, mut steered) = (0u64, 0u64);
    for root in chosen {
        match read_walk(inv, root).await {
            Err(why) => refused.push((root.clone(), why)),
            Ok(walk) => {
                count += walk.get("count").and_then(Value::as_u64).unwrap_or(0);
                steered += walk.get("steered").and_then(Value::as_u64).unwrap_or(0);
                for group in walk
                    .get("groups")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let text = |key: &str| group.get(key).and_then(Value::as_str).unwrap_or("");
                    let (by, key) = (text("by").to_string(), text("key").to_string());
                    let at = match groups.iter().position(|g| g.by == by && g.key == key) {
                        Some(at) => at,
                        None => {
                            groups.push(Group {
                                by,
                                key,
                                first: text("first_decided_at").to_string(),
                                size: group.get("size").and_then(Value::as_u64),
                                declines: Vec::new(),
                            });
                            groups.len() - 1
                        }
                    };
                    let joined = &mut groups[at];
                    let first = text("first_decided_at");
                    if !first.is_empty()
                        && (joined.first.is_empty() || first < joined.first.as_str())
                    {
                        joined.first = first.to_string();
                    }
                    for decline in group
                        .get("declines")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        joined.declines.push((root.clone(), decline.clone()));
                    }
                }
            }
        }
    }
    // Oldest first, as browse orders each root's groups. ⚠ By the SECOND, not the text: the
    // store serves an xsd:dateTime in canonical form, whose text order within one second is
    // not its time order — and inside a second the roots' own orders already hold.
    let second = |at: &str| at.get(..19).unwrap_or(at).to_string();
    groups.sort_by_key(|g| (g.first.is_empty(), second(&g.first)));

    // The contract's menus, once: every decline is a finding, and the description is the
    // template's, identical for every id.
    let probe = finding_iri(queue::PROBE_ID);
    let severities = one_of(&web.hub, &probe, Verb::Sink, "severity");
    let words = decision_words(&web.hub, &probe);
    let reasons = one_of_with_meanings(&web.hub, &probe, Verb::Sink, REASON_ARG);
    let menus = match (&severities, &words) {
        (Some(severities), Some(words)) => Some(Menus {
            severities,
            answers: &words.answers,
            revision_only: &words.revision_only,
            reasons: reasons.as_deref(),
        }),
        _ => None,
    };

    // ---- the header: the same navs as the rows view, with the walk current
    let mut children = web::nav(web, inv, &web::readable_ledgers(web, inv), None);
    if let Some((kind, text)) = flash {
        children.push_str(&element("flash", &[("kind", kind)], text));
    }
    children.push_str(&page.intray_element(inv).await);
    for name in states.unwrap_or_default() {
        let q = queue::query(name, only, scope, false);
        children.push_str(&element(
            "state",
            &[
                ("name", name),
                ("href", &page_url(&q)),
                ("rows-url", &rows_url(&q)),
                ("current", "false"),
            ],
            "",
        ));
    }
    if let Some(kinds) = kinds {
        children.push_str(&crate::batch::kind_nav(kinds, None, only, scope, false));
    }
    children.push_str(&nav(only, true));
    for (root, why) in &refused {
        children.push_str(&element("denied", &[("repo", root)], why));
    }

    // ---- the groups, each its own chunk document (ledger #519)
    let mut chunks: Vec<String> = Vec::new();
    for group in &groups {
        let mut inner = String::new();
        for (root, decline) in &group.declines {
            let echo =
                kept.filter(|k| decline.get("id").and_then(Value::as_str) == Some(k.id.as_str()));
            inner.push_str(&decline_element(
                decline,
                root,
                decide,
                menus.as_ref(),
                only,
                echo,
            ));
        }
        let label = match group.size {
            Some(size) => format!(
                "{} at {} — {} inside one second, across every repository",
                group.by,
                web::when(&group.key),
                plural(size as usize, "decline", "declines")
            ),
            None => format!("{} {}", group.by, group.key),
        };
        chunks.push(render::chunk(&wrap(
            "walk-group",
            &[
                ("label", &label),
                (
                    "count-text",
                    &format!(
                        "{} here still steer{}",
                        plural(
                            group.declines.len(),
                            "unconfirmed decline",
                            "unconfirmed declines"
                        ),
                        if group.declines.len() == 1 { "s" } else { "" }
                    ),
                ),
            ],
            &inner,
        )));
    }
    if !chunks.is_empty() {
        children.push_str(&render::slot(WALK_SLOT));
    }
    if decide && menus.is_none() && !groups.is_empty() {
        children.push_str(&element(
            "no-form",
            &[],
            "The finding contract does not describe its `severity` and `decision` menus, so \
             this page will not invent them. Nothing can be revised here until it does.",
        ));
    }

    // ---- the section's own attributes
    let where_ = match chosen {
        [one] => one.clone(),
        many => format!("{} repositories", many.len()),
    };
    let q = query(only);
    let mut attributes: Vec<(&str, String)> = vec![
        ("view", "queue".to_string()),
        ("mode", "walk".to_string()),
        ("full", flag(!page.fragment).to_string()),
        ("title", "Queue".to_string()),
        ("state", "pending".to_string()),
        ("scope", scope.to_string()),
        ("page-url", page_url(&q)),
        ("rows-url", rows_url(&q)),
        ("refresh-url", rows_url(&q)),
        ("news", queue::NEWS_EVENT.to_string()),
        (
            "stale-text",
            "New findings arrived while you were deciding. They will appear when this revision \
             is submitted or the selection is left as it was."
                .to_string(),
        ),
        (
            "message",
            "Revisit declines nobody evidently meant: each one here has no reason word and was \
             made in a batch or inside a burst of declines, and it still marks the pending \
             findings that repeat its claim. Confirm it with a word, withdraw it, or reverse it. \
             Every revision is a new decision; the old one is kept."
                .to_string(),
        ),
    ];
    if let Some(repo) = only {
        attributes.push(("repo", repo.to_string()));
    }
    if no_roots {
        attributes.push(("empty", "true".to_string()));
        attributes.push((
            "empty-text",
            "No repository here is readable under this grant, so there is nothing to walk."
                .to_string(),
        ));
    } else if groups.is_empty() && refused.is_empty() {
        attributes.push(("empty", "true".to_string()));
        attributes.push((
            "empty-text",
            format!(
                "No unconfirmed decline steers a pending finding in {where_}. Nothing here is \
                 waiting to be revisited."
            ),
        ));
    } else if !groups.is_empty() {
        attributes.push((
            "count-text",
            format!(
                "{} still steer{} {} in {where_} — oldest first",
                plural(
                    count as usize,
                    "unconfirmed decline",
                    "unconfirmed declines"
                ),
                if count == 1 { "s" } else { "" },
                plural(steered as usize, "pending finding", "pending findings"),
            ),
        ));
    }
    if !decide && !no_roots {
        attributes.push(("posture", "read-only".to_string()));
        attributes.push((
            "posture-text",
            format!(
                "This grant may read findings and not revise their decisions: that needs `{}`.",
                ikigai_browse::CAP_ANNOTATE
            ),
        ));
    }
    let attributes: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let doc = envelope("page", &attributes, &children);
    let shell = render::render(&doc, !page.fragment).map_err(web::render_err)?;
    let slots = if chunks.is_empty() {
        Vec::new()
    } else {
        vec![(
            WALK_SLOT.to_string(),
            render::rendered_chunks(inv, &chunks).await?,
        )]
    };
    Ok(web::html(
        render::splice(shell, &slots).map_err(web::render_err)?,
    ))
}

/// The contract's menus, read once per render.
struct Menus<'a> {
    severities: &'a [String],
    answers: &'a [String],
    revision_only: &'a [String],
    reasons: Option<&'a [(String, Option<String>)]>,
}

/// The walk's wording for an ANSWER word on a decision already on record. ⚠ The set is the
/// contract's; only the two words the wording table knows get a sentence of their own here.
fn answer_label(word: &str) -> String {
    match word {
        REASONED_DECISION => "Confirm the decline".to_string(),
        other => format!("{} instead", queue::decision_label(other)),
    }
}

/// One unconfirmed decline: the finding, its decision, what it steers, and — for a caller who
/// may decide — the two revise forms.
fn decline_element(
    row: &Value,
    root: &str,
    decide: bool,
    menus: Option<&Menus<'_>>,
    only: Option<&str>,
    kept: Option<&Kept>,
) -> String {
    let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("");
    let id = text("id");
    let path = text("path");
    let where_ = match row.get("line").and_then(Value::as_u64) {
        Some(line) => format!("{path}:{line}"),
        None => path.to_string(),
    };
    let proposal = row.get("severity").and_then(Value::as_str);
    let mut attributes: Vec<(&str, String)> = vec![
        ("id", id.to_string()),
        (
            "repo",
            match text("repo") {
                "" => root.to_string(),
                named => named.to_string(),
            },
        ),
        ("where", where_),
        ("severity", proposal.unwrap_or("unrated").to_string()),
        (
            "severity-label",
            format!("model: {}", proposal.unwrap_or("unrated")),
        ),
        ("provenance", queue::provenance(row)),
        ("finding-href", browse_url(&finding_iri(id))),
    ];
    if !text("annotates").is_empty() {
        attributes.push(("browse-href", browse_url(text("annotates"))));
    }
    let mut children = element("body", &[], text("body"));
    if !text("exact").is_empty() {
        children.push_str(&element("quote", &[], text("exact")));
    }
    let decision = row.get("decision").filter(|d| !d.is_null());
    if let Some(decision) = decision {
        children.push_str(&queue::decision_element_linking(decision, false));
    }
    // What this decline still steers: the pending findings that repeat its claim.
    let pending: Vec<&str> = row
        .get("pending")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut steers = String::new();
    for iri in &pending {
        steers.push_str(&element(
            "pending",
            &[("href", &browse_url(iri))],
            iri.rsplit(':').next().unwrap_or(iri),
        ));
    }
    children.push_str(&wrap(
        "steers",
        &[(
            "label",
            &format!(
                "marks {} that repeat{} its claim:",
                plural(pending.len(), "pending finding", "pending findings"),
                if pending.len() == 1 { "s" } else { "" }
            ),
        )],
        &steers,
    ));
    if let (true, Some(menus), Some(decision)) = (decide, menus, decision) {
        let revises = decision.get("iri").and_then(Value::as_str).unwrap_or("");
        children.push_str(&revise_element(id, revises, decision, menus, only, kept));
    }
    let attributes: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
    wrap("decline", &attributes, &children)
}

/// The two revise forms for one decline, as one element the stylesheet draws as two forms:
/// the answers (with a rating, a word and a note) and the revision-only words (a note only —
/// the Sink refuses a rating beside a withdrawal). Each names the decline it revises.
fn revise_element(
    id: &str,
    revises: &str,
    decision: &Value,
    menus: &Menus<'_>,
    only: Option<&str>,
    kept: Option<&Kept>,
) -> String {
    let on_file = decision.get("severity").and_then(Value::as_str);
    let chosen = kept
        .and_then(|k| k.severity.as_deref())
        .filter(|word| menus.severities.iter().any(|s| s == word))
        .or(on_file);
    let mut options = String::new();
    if chosen.is_none() {
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
    for value in menus.severities {
        options.push_str(&element(
            "severity-option",
            &[
                ("value", value),
                ("label", value),
                ("selected", flag(chosen == Some(value.as_str()))),
                ("placeholder", "false"),
            ],
            "",
        ));
    }
    if let Some(reasons) = menus.reasons {
        let picked = kept.and_then(|k| k.reason.as_deref());
        options.push_str(&element(
            "reason-option",
            &[
                ("value", ""),
                ("label", "why? (a word, optional)"),
                ("title", ""),
            ],
            "",
        ));
        for (word, meaning) in reasons {
            options.push_str(&element(
                "reason-option",
                &[
                    ("value", word),
                    ("label", word),
                    ("title", meaning.as_deref().unwrap_or("")),
                    ("selected", flag(picked == Some(word.as_str()))),
                ],
                "",
            ));
        }
    }
    for word in menus.answers {
        options.push_str(&element(
            "answer-option",
            &[("value", word), ("label", &answer_label(word))],
            "",
        ));
    }
    for word in menus.revision_only {
        options.push_str(&element(
            "withdraw-option",
            &[("value", word), ("label", word)],
            "",
        ));
    }
    if let Some(note) = kept.and_then(|k| k.note.as_deref()) {
        options.push_str(&element("note", &[], note));
    }
    let mut attributes: Vec<(&str, &str)> = vec![
        ("action", DECIDE_PATH),
        ("id", id),
        (REVISES_ARG, revises),
        ("repo", only.unwrap_or("")),
        ("summary", WALK),
        ("required", flag(chosen.is_none())),
        (
            "withdraw-text",
            "Withdrawing leaves the finding undecided again, and the findings that repeat its \
             claim lose the mark.",
        ),
    ];
    if let Some(kept) = kept {
        attributes.push(("refused", "true"));
        attributes.push(("problem", &kept.problem));
    }
    wrap("revise", &attributes, &options)
}
