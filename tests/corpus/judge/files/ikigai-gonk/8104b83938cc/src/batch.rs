//! The Queue's **batch view** — a machine-proposed group of pending findings, decided once by
//! the human (ledger [#506](http://localhost:1060/l/default/item/506)).
//!
//! ```text
//! /queue?group=<kind>        urn:iki:gonk:page:queue       Source  the page, one kind at a time
//! /queue/rows?group=<kind>   urn:iki:gonk:fragment:queue   Source  the same section, for htmx
//! /queue/batch               urn:iki:gonk:queue:batch      Sink    one batch decline, fanned out
//! ```
//!
//! `ikigai-browse` 0.12.0 PROPOSES the groups: `urn:repo:{repo}:findings group=<kind>` answers
//! the pending set as groups, each with a suggested reason word, and decides nothing. This is
//! the face that lets a person decide one. There is **no batch Sink** anywhere: a batch is one
//! call per ticked member to the existing finding Sink, so every member gets an ordinary
//! decision node, and the recurrence mark on a later like claim reads a batch decline exactly
//! like a single one — because it IS the human's decision, made once for many.
//!
//! # ★ The rules, each one a decision already made
//!
//! - **Members are SHOWN before deciding, and each can be ticked or unticked.** Nothing is
//!   pre-decided: a box ticked on the page is an offer, and nothing reaches the store until
//!   the button.
//! - ★ **A member starts TICKED only when its group carries INDIVIDUAL evidence** (Brian,
//!   2026-09-25, ledger [#508](http://localhost:1060/l/default/item/508)): a `twin` — this
//!   claim was already declined on this line — or a `kept` row — this is a reworded repeat of
//!   that one. A group proposed by target alone (`by_target_only`) starts with every member
//!   UNTICKED, its suggested word SHOWN beside the group and NOT selected, and the person
//!   ticks what they have read. A 2026-09-24 sample of such a group was MIXED — restated
//!   comments beside a plausibly real claim — and a mixed group must not be one click from a
//!   whole-file decline. ⚠ The rule is over the data (`twin`, `kept`), never a kind's name,
//!   which is why it needs no field from browse and holds for any kind that carries no
//!   evidence per member.
//! - ★★ **And evidence must be CONFIRMED** (Brian, 2026-10-01, amending the rule above —
//!   ledger [#653](http://localhost:1060/l/default/item/653)). browse 0.14.0 computes
//!   `confirmed` on every decision: a decline with no word made in a batch or inside a burst of
//!   declines reads `false`. A twin whose current decision is unconfirmed, or a member whose own
//!   `prior_decision` is, is NOT evidence: the member starts unticked, and its row says why in
//!   words and links the walk ([`crate::walk`]) where that decline can be revisited. Measured
//!   the day the rule changed: 39 of the 43 declines recurrences pointed at were burst-made and
//!   wordless, and each was pre-ticking its repeats.
//! - ★ **The door stamps how each decline was made** (ledger #653): `made=batch` and
//!   `batch=<the group key it was proposed in>`, per member — the twin-carrying form holds many
//!   groups, so each member's box carries its own key (`key:<id>`). A form naming `made` or
//!   `batch` itself is refused: the browser never chooses its own provenance, the rule that
//!   already governs the ledger's `author`.
//! - **A batch DECLINE requires a reason word** — the finding Sink's own `reason` `one_of`.
//!   Where the group carries evidence its suggested word is PRE-SELECTED in the picker,
//!   never applied without the press. A batch with no word is refused whole, before any
//!   member is touched: a bulk judgment without content is exactly what ledger #506 exists
//!   to prevent.
//! - ★★ **A refusal never loses what the person entered** (ledger
//!   [#657](http://localhost:1060/l/default/item/657), P0). The refused form comes back
//!   exactly as it was sent — every tick and untick, every word — with EVERY offending member
//!   marked in words and the first one focused; only an applied batch re-renders from the
//!   source. And the page stops the round trip where it can: the form itself carries the
//!   htmx post, so the browser's own constraint check runs before anything is sent, and
//!   `web/gonk.js` makes a ticked member's picker required exactly while it has no word and
//!   no batch word stands behind it.
//! - ★ **The twin-carrying kind is the one exception** (Brian, 2026-09-25). Its groups are
//!   almost all singletons — one re-raise per declined twin — so "decide once for many" only
//!   works as ALL of its groups in one form, each member declined with ITS OWN twin's word.
//!   Every member still gets a reason, and it is the human's own earlier word for the same
//!   claim on the same line. ⚠ The mode is chosen by the DATA — a view whose groups carry a
//!   `twin` — never by the kind's name, which this crate does not spell.
//! - ★ **A twin with no word takes the BATCH word** (Brian, 2026-09-25, ledger #508). Every
//!   decline made before reason words existed has `decision.reason == null`, so on the live
//!   store every such twin was wordless and the form asked for one word per member — which
//!   defeats the batch. The twin-carrying form carries one OPTIONAL batch-wide picker, "a
//!   word for every ticked finding whose twin had none", and says how many members will take
//!   it. A member's own picker OVERRIDES it. The word is recorded on each new decision as the
//!   human's stated reason for THAT claim: a recurrence decline says "the same claim as one I
//!   already rejected", and the word describes the claim, not the twin's history. A batch is
//!   still refused when any ticked member ends up with no word from either source.
//! - **Severity** (Brian, 2026-09-25): under the default `severity=serious` scope the members
//!   are filtered to `gonk.queue.serious` exactly as the rows are (`crate::queue::rated`,
//!   [`crate::config::QueuePolicy::queues`]), and the counts shown are GONK's — browse's
//!   `kinds` counts span every severity and would not match what the page lists. What the
//!   filter left out is said, with the link that lists it (`severity=all`).
//! - **One proposal is shown once, under the kind that carries a word.** For a doc file
//!   browse's comment-shaped group and its whole-file group hold the same members, because
//!   every line of a doc is "a comment" — and so do the two groups on a CODE file whose
//!   pending findings all quote comments. A twin-less, keep-less group whose target and
//!   members equal another kind's group is shown once (`fold_repeats`): under the one whose
//!   `reason` is a word when only one of them carries one, else under the later kind. ⚠ The
//!   first version folded toward the later kind unconditionally, which folded a worded group
//!   into a wordless one and lost the suggestion (ledger #508). Two groups carrying two
//!   DIFFERENT words are two proposals and both are shown. The rule is stated over the data,
//!   so no kind word and no extension list is written here.
//! - **Publish is not batchable.** A publish mints an annotation on each line, and a bulk
//!   publish is the one act here that writes to what other readers see. The view offers
//!   Decline only, and [`Batch`] refuses any other decision by name.
//! - The group that proposes a row to KEEP shows it above the members, unticked and locked.
//!
//! # ⚠ A decline needs `urn:cap:annotate` — the brief said otherwise
//!
//! The dispatch brief assumed a decline needs no `urn:cap:annotate`. The finding Sink's
//! contract says it does: its one Sink action requires that token and the browse read
//! wildcard, for either decision ("declining writes the record that a holder of that
//! authority made"). So [`Batch`] declares both — declared = enforced — and the kernel refuses
//! a caller without them before a single member is tried.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Description, Endpoint, Error, Invocation, Iri, Representation,
    Request, Result, Verb,
};
use serde_json::Value;

use crate::queue::{
    self, browse_url, can_decide, finding_iri, findings_iri, flag, one_of, one_of_with_meanings,
    page_url, rows_url, QueuePage, REASONED_DECISION, REASON_ARG, SCOPE_ALL, SCOPE_ARG,
    SCOPE_SERIOUS,
};
use crate::render::{self, element, envelope, wrap};
use crate::web::{self, Web};

/// The findings face's grouping argument (browse 0.12.0). Its WORDS are the contract's
/// `one_of`; only the argument's name is ours.
pub const GROUP_ARG: &str = "group";
/// `urn:iki:gonk:queue:batch` — one batch decline, fanned out to the finding Sink.
pub const BATCH_IRI: &str = "urn:iki:gonk:queue:batch";
/// Where the batch form posts.
pub const BATCH_PATH: &str = "/queue/batch";

/// The checkbox name every member shares: `member=<id>`, once per ticked box.
const MEMBER_FIELD: &str = "member";
/// A member's OWN reason word, in the twin-carrying view: `reason:<id>=<word>`.
const MEMBER_REASON_PREFIX: &str = "reason:";
/// The group a member was proposed in: `key:<id>=<group key>` — what its decline's `batch=` is
/// stamped with (ledger #653). The twin-carrying form holds many groups, so the form's own
/// `_key` cannot say it; a group form's `_key` IS the group's, and is the fallback.
const MEMBER_KEY_PREFIX: &str = "key:";
/// GONK's own field on a group, set while gating: how many members the serious scope left
/// out of it. Browse's JSON never carries it.
const LEFT_OUT: &str = "gonk_left_out";

/// The picker's empty option when a word must still be chosen — selected, not submittable.
const CHOOSE_REASON_LABEL: &str = "choose a reason";
/// The batch-wide fallback picker's empty option: choosable, and submits nothing.
const NO_BATCH_WORD_LABEL: &str = "no batch word";
/// A member's own picker's empty option in the twin-carrying view: the batch word applies.
const TAKE_BATCH_WORD_LABEL: &str = "the batch word";

/// The slot the twin-carrying form leaves for its groups — see [`render::chunk`].
const GROUPS_SLOT: &str = "groups";
/// The slot the section leaves for the per-group forms, each a chunk of its own.
const BATCHES_SLOT: &str = "batches";

/// The `group=` a request asked for, checked against the contract's own set — `None` for the
/// ordinary rows view.
///
/// ⚠ **Refused, never ignored**, in the three ways browse refuses it too: a kind the contract
/// does not declare, a `state` other than pending (a group proposes PENDING findings only),
/// and a browse that declares no groups at all (0.11.x). An argument accepted and then
/// ignored is the failure invisible from the caller's side.
pub(crate) fn kind_wanted(
    asked: Option<&str>,
    kinds: Option<&[String]>,
    state: &str,
) -> Result<Option<String>> {
    let Some(asked) = asked.map(str::trim).filter(|a| !a.is_empty()) else {
        return Ok(None);
    };
    let Some(kinds) = kinds else {
        return Err(Error::InvalidArgument {
            name: GROUP_ARG.to_string(),
            detail: format!(
                "`{asked}`: the findings resource declares no `{GROUP_ARG}` set, so there is no \
                 batch view to show — it needs ikigai-browse 0.12.0 or later"
            ),
        });
    };
    if !kinds.iter().any(|k| k == asked) {
        return Err(Error::InvalidArgument {
            name: GROUP_ARG.to_string(),
            detail: format!("`{asked}` is not one of {}", kinds.join(", ")),
        });
    }
    if state != "pending" {
        return Err(Error::InvalidArgument {
            name: "state".to_string(),
            detail: format!(
                "`{GROUP_ARG}={asked}` proposes groups of PENDING findings only — drop \
                 `state={state}`, or drop `{GROUP_ARG}`"
            ),
        });
    }
    Ok(Some(asked.to_string()))
}

/// `group=<kind>`, `…&repo=x`, `…&severity=all` — the batch view's query.
pub(crate) fn group_query(kind: &str, repo: Option<&str>, scope: &str) -> String {
    let mut out = format!("{GROUP_ARG}={kind}");
    if let Some(repo) = repo.filter(|r| !r.is_empty()) {
        out.push_str(&format!("&repo={repo}"));
    }
    if scope != SCOPE_SERIOUS {
        out.push_str(&format!("&{SCOPE_ARG}={scope}"));
    }
    out
}

/// The kind nav, one entry per word of the contract's `group` set, in contract order.
///
/// ⚠ No count on the entries. Browse's `kinds` counts span every severity and would not match
/// what a serious-scoped view lists, and gonk's own counts would cost a grouped read per kind
/// per root on every page; the count is on the view itself, where it is gonk's.
pub(crate) fn kind_nav(
    kinds: &[String],
    current: Option<&str>,
    only: Option<&str>,
    scope: &str,
) -> String {
    kinds
        .iter()
        .map(|kind| {
            let query = group_query(kind, only, scope);
            element(
                "kind",
                &[
                    ("name", kind),
                    ("href", &page_url(&query)),
                    ("rows-url", &rows_url(&query)),
                    ("current", flag(current == Some(kind.as_str()))),
                ],
                "",
            )
        })
        .collect()
}

/// What the batch view is rendered from — everything [`QueuePage::body`] already settled.
pub(crate) struct Frame<'a> {
    pub(crate) kind: &'a str,
    pub(crate) kinds: &'a [String],
    pub(crate) states: Option<&'a [String]>,
    pub(crate) only: Option<&'a str>,
    pub(crate) scope: &'static str,
    pub(crate) chosen: &'a [String],
    pub(crate) no_roots: bool,
    /// Whether the findings contract offers the walk over unconfirmed declines
    /// ([`crate::walk`]), so the nav carries its link.
    pub(crate) walk: bool,
    pub(crate) flash: Option<(&'a str, &'a str)>,
    /// What a REFUSED batch form carried (ledger #657): rendered back into that form, every
    /// tick and word as sent and every offending row marked, so a refusal loses nothing.
    /// `None` on a read, and on every batch that was applied — those re-render from the
    /// source, so the page never shows a decision the store did not record.
    pub(crate) submitted: Option<&'a Submitted>,
}

/// One root's groups of one kind, as the findings face's JSON answers them — `groups` only;
/// the `kinds` counts are browse's and are not shown (see [`kind_nav`]).
async fn read_groups(
    inv: &Invocation<'_>,
    root: &str,
    kind: &str,
) -> std::result::Result<Vec<Value>, String> {
    let iri = findings_iri(root);
    let target = Iri::parse(&iri).map_err(|_| format!("`{iri}` is not an IRI"))?;
    let request = Request::new(Verb::Source, target)
        .with_arg("as", ArgRef::Inline(queue::JSON.as_bytes().to_vec()))
        .with_arg(GROUP_ARG, ArgRef::Inline(kind.as_bytes().to_vec()));
    let answer = inv.issue(request).await.map_err(|e| format!("{e}"))?;
    match serde_json::from_slice::<Value>(&answer.bytes) {
        Ok(Value::Object(mut body)) => match body.remove("groups") {
            Some(Value::Array(groups)) => Ok(groups),
            _ => Err(format!(
                "`{iri}` {GROUP_ARG}={kind} answered no `groups` array"
            )),
        },
        _ => Err(format!(
            "`{iri}` {GROUP_ARG}={kind} answered something that is not a JSON object"
        )),
    }
}

fn is_null_or_absent(group: &Value, key: &str) -> bool {
    group.get(key).is_none_or(Value::is_null)
}

/// A group browse proposes by TARGET alone: no declined twin rides along and no row is
/// proposed to keep. Only such a group can repeat another kind's proposal.
fn by_target_only(group: &Value) -> bool {
    is_null_or_absent(group, "twin") && is_null_or_absent(group, "kept")
}

/// The identity of a proposal: its target and the ids of its members.
type Proposal = (String, BTreeSet<String>);

/// Every target-only proposal of the given kinds on one root, each with its suggested word —
/// one grouped read per kind; a kind that cannot be read proposes nothing here.
async fn target_only_proposals(
    inv: &Invocation<'_>,
    root: &str,
    kinds: &[String],
) -> Vec<(Proposal, Option<String>)> {
    let mut found = Vec::new();
    for kind in kinds {
        if let Ok(theirs) = read_groups(inv, root, kind).await {
            found.extend(
                theirs
                    .iter()
                    .filter(|g| by_target_only(g))
                    .map(|g| (proposal(g), suggested(g).map(str::to_string))),
            );
        }
    }
    found
}

fn proposal(group: &Value) -> Proposal {
    let target = group
        .get("annotates")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let members = members(group)
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    (target, members)
}

fn members(group: &Value) -> &[Value] {
    group
        .get("members")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// The group's suggested word, when the JSON carries one.
fn suggested(group: &Value) -> Option<&str> {
    group.get("reason").and_then(Value::as_str)
}

/// ★ **One proposal, shown once — under the kind that carries a word.** For a doc file every
/// quote is "a comment or doc line", so browse's comment-shaped group on it holds exactly the
/// members of its whole-file group, and showing both asks the same question twice. The rule,
/// over the data rather than a name: a group proposed by target alone ([`by_target_only`])
/// whose target and members equal those of another kind's target-only group is dropped here
/// and shown there when
///
/// - that other group carries a suggested `reason` and this one does not (a suggestion is
///   information, and folding toward the wordless group lost it — ledger #508), or
/// - the two carry the same `reason` (or none) and the other kind is LATER in the contract's
///   order.
///
/// Two groups carrying two different words are two proposals, and both are shown. Returns
/// how many were folded, so the page can say so.
///
/// ⚠ The reads are paid only where they can matter: the later kinds only when some group
/// here is target-only at all, the earlier kinds only when some target-only group here has
/// no word (the one case an earlier group can win). A view whose groups all carry a twin or
/// a kept row pays nothing.
async fn fold_repeats(
    inv: &Invocation<'_>,
    root: &str,
    earlier: &[String],
    later: &[String],
    groups: &mut Vec<Value>,
) -> usize {
    if !groups.iter().any(by_target_only) {
        return 0;
    }
    let elsewhere_later = target_only_proposals(inv, root, later).await;
    let elsewhere_earlier = if groups
        .iter()
        .any(|g| by_target_only(g) && suggested(g).is_none())
    {
        target_only_proposals(inv, root, earlier).await
    } else {
        Vec::new()
    };
    let before = groups.len();
    groups.retain(|g| {
        if !by_target_only(g) {
            return true;
        }
        let mine = proposal(g);
        let word = suggested(g);
        let later_wins = elsewhere_later
            .iter()
            .any(|(theirs, w)| *theirs == mine && (w.as_deref() == word || word.is_none()));
        let earlier_wins = word.is_none()
            && elsewhere_earlier
                .iter()
                .any(|(theirs, w)| *theirs == mine && w.is_some());
        !(later_wins || earlier_wins)
    });
    before - groups.len()
}

/// `n thing` / `n things`.
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The batch view: the queue section, with one kind's groups in place of the rows.
pub(crate) async fn section(
    page: &QueuePage,
    inv: &Invocation<'_>,
    frame: Frame<'_>,
) -> Result<Representation> {
    let web = &page.web;
    let policy = &web.queue;
    let decide = can_decide(inv);
    let Frame {
        kind,
        kinds,
        states,
        only,
        scope,
        chosen,
        no_roots,
        walk,
        flash,
        submitted,
    } = frame;
    let earlier: Vec<String> = kinds
        .iter()
        .take_while(|k| k.as_str() != kind)
        .cloned()
        .collect();
    let later: Vec<String> = kinds
        .iter()
        .skip_while(|k| k.as_str() != kind)
        .skip(1)
        .cloned()
        .collect();

    // The reads, one grouped read per chosen root (and the other kinds' only to fold).
    let mut read: Vec<(String, std::result::Result<Vec<Value>, String>)> = Vec::new();
    let mut folded = 0usize;
    for root in chosen {
        let mut answer = read_groups(inv, root, kind).await;
        if let Ok(groups) = &mut answer {
            folded += fold_repeats(inv, root, &earlier, &later, groups).await;
        }
        read.push((root.clone(), answer));
    }

    // ★★ THE GATE, as on the rows (ledger #496): under the serious scope a member is shown
    // only when the Queue would ask about it. A group left with no member is not a proposal.
    let mut hidden = 0usize;
    // ★ And ORDERED inside each group by the judge's verdict (ledger #696): confirmed first,
    // refuted folded last, every member still shown and still tickable ([`crate::verdict`]).
    // Stable, so within one standing the group keeps browse's member order.
    let unjudgeable = crate::backfill::unjudgeable(inv).await;
    let cannot = |id: &str| unjudgeable.get(id).cloned();
    for (_, answer) in &mut read {
        if let Ok(groups) = answer {
            for group in groups.iter_mut() {
                if let Some(Value::Array(members)) = group.get_mut("members") {
                    crate::verdict::order(members, |row| row, &cannot);
                }
                if scope != SCOPE_SERIOUS {
                    continue;
                }
                let mut left_out = 0usize;
                if let Some(Value::Array(members)) = group.get_mut("members") {
                    let before = members.len();
                    members.retain(|row| policy.queues(queue::rated(row)));
                    left_out = before - members.len();
                }
                hidden += left_out;
                // Browse's label counts every severity ("22 findings on …"); the group
                // says on itself how many the gate left out, so the two numbers agree.
                if let (true, Some(object)) = (left_out > 0, group.as_object_mut()) {
                    object.insert(LEFT_OUT.to_string(), Value::from(left_out));
                }
            }
            groups.retain(|group| !members(group).is_empty());
        }
    }
    let groups: Vec<&Value> = read
        .iter()
        .filter_map(|(_, answer)| answer.as_ref().ok())
        .flatten()
        .collect();
    let shown: usize = groups.iter().map(|g| members(g).len()).sum();
    // ★ The mode is the DATA's: a view whose groups carry a declined twin decides each
    // member with its own twin's word, all groups in one form.
    let per_twin = groups.iter().any(|g| !is_null_or_absent(g, "twin"));

    // The contract's reason words — the picker's options and the only words a batch may
    // carry. `None` when the contract cannot be read: no form, never an invented menu.
    let reasons = one_of_with_meanings(
        &web.hub,
        &finding_iri(queue::PROBE_ID),
        Verb::Sink,
        REASON_ARG,
    );

    // ---- the header: the same navs as the rows view, with this kind current
    let mut children = web::nav(web, inv, &web::readable_ledgers(web, inv), None);
    if let Some((flash_kind, text)) = flash {
        children.push_str(&element("flash", &[("kind", flash_kind)], text));
    }
    children.push_str(&page.intray_element(inv).await);
    if let Some(known) = states {
        for name in known {
            let q = queue::query(name, only, scope);
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
    }
    let serious_label = format!("serious: {}", policy.serious.join(", "));
    for (name, label) in [
        (SCOPE_SERIOUS, serious_label.as_str()),
        (SCOPE_ALL, "all severities"),
    ] {
        let q = group_query(kind, only, name);
        children.push_str(&element(
            "scope",
            &[
                ("name", name),
                ("label", label),
                ("href", &page_url(&q)),
                ("rows-url", &rows_url(&q)),
                ("current", flag(name == scope)),
            ],
            "",
        ));
    }
    children.push_str(&kind_nav(kinds, Some(kind), only, scope));
    if walk {
        children.push_str(&crate::walk::nav(only, false));
    }
    for (root, answer) in &read {
        if let Err(why) = answer {
            children.push_str(&element("denied", &[("repo", root)], why));
        }
    }
    let where_ = match chosen {
        [one] => one.clone(),
        many => format!("{} repositories", many.len()),
    };
    if hidden > 0 {
        let all = group_query(kind, only, SCOPE_ALL);
        let others = queue::proposal_words(&web.hub, policy);
        let rated = match &others {
            Some(words) if !words.is_empty() => format!(" rated {}", queue::join_or(words)),
            _ => String::new(),
        };
        children.push_str(&element(
            "hidden",
            &[
                ("count", &hidden.to_string()),
                ("href", &page_url(&all)),
                ("rows-url", &rows_url(&all)),
                ("label", "list all severities"),
            ],
            &format!(
                "{} in these {kind} groups{rated} {} left out: the Queue does not ask about \
                 {}.",
                plural(hidden, "other finding", "other findings"),
                if hidden == 1 { "is" } else { "are" },
                if hidden == 1 { "it" } else { "them" },
            ),
        ));
    }
    if folded > 0 {
        children.push_str(&element(
            "folded",
            &[],
            &format!(
                "{} held exactly the members of another kind's group on the same file, so {} \
                 shown there, once — under the kind whose group carries a suggested word, or \
                 else the later one.",
                plural(folded, &format!("{kind} group"), &format!("{kind} groups")),
                if folded == 1 { "it is" } else { "they are" },
            ),
        ));
    }

    // ---- the groups
    let form_attributes = |key: &str| -> Vec<(&'static str, String)> {
        vec![
            ("action", BATCH_PATH.to_string()),
            ("key", key.to_string()),
            ("group", kind.to_string()),
            ("repo", only.unwrap_or("").to_string()),
            ("scope", scope.to_string()),
            ("decide", flag(decide && reasons.is_some()).to_string()),
            ("per-twin", flag(per_twin).to_string()),
            // The single-row Decline button's own class, so the batch button reads as the
            // same act — the decision word is the wording table's, not spelled in the sheet.
            ("button-class", format!("decide-button {REASONED_DECISION}")),
        ]
    };
    // ★ A refusal gives back what was typed (ledger #657): the refused submission is drawn
    // into the ONE form it came from, every other form from the source as usual. The form
    // carrying it says so (`refused`), which is what keeps the news refresh from wiping it.
    let echo_for = |key: &str, holds: &dyn Fn(&str) -> bool| -> Option<&Submitted> {
        submitted.filter(|s| s.belongs_to(key, holds))
    };
    // ★ Each group is rendered as its own chunk document, apart from the shell (ledger
    // #519), and `render::splice` joins the pieces into exactly the HTML one transform
    // produced (`render::chunk` has the numbers). The chunk is the whole FORM where there is
    // one form per group — the shell then holds one slot, not forty-nine forms of pickers
    // it would transform on every poll — and one group where the twin-carrying view puts
    // every group in one form, whose slot sits ahead of the batch-wide picker.
    let mut chunks: Vec<String> = Vec::new();
    let mut slot: Option<&str> = None;
    if per_twin {
        let holds = |id: &str| {
            groups.iter().any(|g| {
                members(g)
                    .iter()
                    .any(|m| m.get("id").and_then(Value::as_str) == Some(id))
            })
        };
        let echo = echo_for(kind, &holds);
        // The first offending row's own picker takes the focus; `true` until one has.
        let mut focus = echo.is_some_and(|e| !e.offending.is_empty());
        let mut inner = String::new();
        let mut ticked = 0usize;
        let mut wordless = 0usize;
        // Wordless members the person could still tick — an unconfirmed twin's repeats start
        // unticked (ledger #653), and ticking one must still find a batch word to fall back on.
        let mut wordless_tickable = 0usize;
        for group in &groups {
            chunks.push(render::chunk(&group_element(
                group,
                decide,
                reasons.as_deref(),
                true,
                echo,
                &mut focus,
                &cannot,
            )));
            let word = twin_word(group);
            for member in members(group) {
                if !(decide && rated(member)) {
                    continue;
                }
                let id = member.get("id").and_then(Value::as_str).unwrap_or("");
                // As drawn: ticked and worded as SENT when this form was refused, else
                // ticked where the twin is CONFIRMED evidence, with its twin's word.
                let (is_ticked, own) = match echo {
                    Some(e) => (e.members.contains(id), e.words.get(id).map(String::as_str)),
                    None => (unconfirmed_evidence(group, member).is_none(), word),
                };
                if own.is_none() {
                    wordless_tickable += 1;
                }
                if is_ticked {
                    ticked += 1;
                    if own.is_none() {
                        wordless += 1;
                    }
                }
            }
        }
        let mut attributes = form_attributes(kind);
        attributes.push((
            "button-label",
            "Decline the ticked findings, each with its twin's word".to_string(),
        ));
        if echo.is_some() {
            attributes.push(("refused", "true".to_string()));
        }
        // ★ The batch-wide FALLBACK word (ledger #508): offered only when some ticked member
        // has no word of its own — or when the refused form carried one — optional, and
        // overridden by a member's own picker.
        let batch_word = echo.and_then(|e| e.word.as_deref());
        if let (Some(reasons), true) = (&reasons, wordless_tickable > 0 || batch_word.is_some()) {
            attributes.push(("word-required", "false".to_string()));
            attributes.push((
                "word-label",
                format!(
                    "A word for every ticked finding whose twin had none — {wordless} of \
                     {ticked} will take it; a finding's own word wins"
                ),
            ));
            // No offending row could take the focus (each was decided meanwhile): the batch
            // word is then the control that would have fixed it.
            if focus {
                attributes.push(("word-autofocus", "true".to_string()));
            }
            inner.push_str(&reason_options(
                reasons,
                batch_word,
                Placeholder::Optional(NO_BATCH_WORD_LABEL),
            ));
        }
        if !groups.is_empty() {
            inner.insert_str(0, &render::slot(GROUPS_SLOT));
            slot = Some(GROUPS_SLOT);
            children.push_str(&wrap("batch", &borrowed(&attributes), &inner));
        }
    } else {
        let mut focus_taken = false;
        for group in &groups {
            let key = group.get("key").and_then(Value::as_str).unwrap_or("");
            let holds = |id: &str| {
                members(group)
                    .iter()
                    .any(|m| m.get("id").and_then(Value::as_str) == Some(id))
            };
            let echo = echo_for(key, &holds);
            // A group form's members carry no word of their own, so no ROW takes the focus:
            // the batch word is every member's, and it is the control that fixes them all.
            let mut rows_focus = false;
            let mut inner = group_element(
                group,
                decide,
                reasons.as_deref(),
                false,
                echo,
                &mut rows_focus,
                &cannot,
            );
            // ★ Pre-selected only beside evidence: a target-only group's word is shown on the
            // group (`suggested`, in `group_element`) and the picker waits for the person. A
            // refused form shows the word it SENT, or none.
            let preselected = match echo {
                Some(e) => e.word.as_deref(),
                None if by_target_only(group) => None,
                None => suggested(group),
            };
            let mut attributes = form_attributes(key);
            attributes.push(("button-label", "Decline the ticked findings".to_string()));
            attributes.push(("word-required", "true".to_string()));
            attributes.push(("word-label", "Reason, for every ticked finding".to_string()));
            if let Some(echo) = echo {
                attributes.push(("refused", "true".to_string()));
                if !echo.offending.is_empty() && !focus_taken {
                    attributes.push(("word-autofocus", "true".to_string()));
                    focus_taken = true;
                }
            }
            if let Some(reasons) = &reasons {
                inner.push_str(&reason_options(reasons, preselected, Placeholder::Required));
            }
            chunks.push(render::chunk(&wrap(
                "batch",
                &borrowed(&attributes),
                &inner,
            )));
        }
        if !groups.is_empty() {
            children.push_str(&render::slot(BATCHES_SLOT));
            slot = Some(BATCHES_SLOT);
        }
    }

    // ---- the section's own attributes
    let q = group_query(kind, only, scope);
    let mut attributes: Vec<(&str, String)> = vec![
        ("view", "queue".to_string()),
        ("mode", "batch".to_string()),
        ("full", flag(!page.fragment).to_string()),
        ("title", "Queue".to_string()),
        ("state", "pending".to_string()),
        ("scope", scope.to_string()),
        ("group", kind.to_string()),
        ("page-url", page_url(&q)),
        ("rows-url", rows_url(&q)),
        ("refresh-url", rows_url(&q)),
        ("news", queue::NEWS_EVENT.to_string()),
        (
            "stale-text",
            "New findings arrived while you were deciding. They will appear when this batch is \
             submitted or the selection is left as it was."
                .to_string(),
        ),
        (
            "message",
            "Decide in batches. The machine proposed these groups; nothing here is decided until \
             you press Decline, a batch declines only the findings still ticked, and each one \
             gets its own ordinary decision. Publishing is one finding at a time."
                .to_string(),
        ),
    ];
    if let Some(repo) = only {
        attributes.push(("repo", repo.to_string()));
    }
    let serious = if scope == SCOPE_SERIOUS {
        "serious "
    } else {
        ""
    };
    if no_roots {
        attributes.push(("empty", "true".to_string()));
        attributes.push((
            "empty-text",
            "No repository here is readable under this grant, so there is nothing to group."
                .to_string(),
        ));
    } else if groups.is_empty() && read.iter().all(|(_, a)| a.is_ok()) {
        attributes.push(("empty", "true".to_string()));
        attributes.push((
            "empty-text",
            format!(
                "No {kind} groups among the {serious}pending findings in {where_}. Nothing here \
                 is waiting for a batch decision."
            ),
        ));
    } else if !groups.is_empty() {
        attributes.push((
            "count-text",
            format!(
                "{} holding {} in {where_}",
                plural(
                    groups.len(),
                    &format!("{kind} group"),
                    &format!("{kind} groups")
                ),
                plural(
                    shown,
                    &format!("{serious}pending finding"),
                    &format!("{serious}pending findings")
                ),
            ),
        ));
    }
    if !decide && !no_roots {
        attributes.push(("posture", "read-only".to_string()));
        attributes.push((
            "posture-text",
            format!(
                "This grant may read findings and not decide them: that needs `{}`.",
                ikigai_browse::CAP_ANNOTATE
            ),
        ));
    } else if decide && reasons.is_none() && !groups.is_empty() {
        children.push_str(&element(
            "no-form",
            &[],
            "The finding contract does not describe its decline reasons, so this page will not \
             invent them, and a batch decline needs one. Nothing can be decided here until it \
             does.",
        ));
    }
    let doc = envelope("page", &borrowed(&attributes), &children);
    let shell = render::render(&doc, !page.fragment).map_err(web::render_err)?;
    let slots = match slot {
        Some(name) => vec![(
            name.to_string(),
            render::rendered_chunks(inv, &chunks).await?,
        )],
        None => Vec::new(),
    };
    Ok(web::html(
        render::splice(shell, &slots).map_err(web::render_err)?,
    ))
}

fn borrowed<'a>(attributes: &'a [(&'a str, String)]) -> Vec<(&'a str, &'a str)> {
    attributes.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// What a picker's empty first option means, when nothing is pre-selected.
#[derive(Clone, Copy)]
enum Placeholder {
    /// A word MUST be chosen: the empty option is locked, so the browser cannot submit it.
    Required,
    /// The word may be left out — a fallback covers it, or it IS the fallback: the empty
    /// option stays choosable under this label, and submitting it sends nothing.
    Optional(&'static str),
}

/// The contract's reason words as picker options, `preselected` selected when given. With
/// none — or a word the contract no longer declares — an empty first option is selected: a
/// [`Placeholder::Required`] one is locked and the batch is refused until a word is picked;
/// a [`Placeholder::Optional`] one can be chosen back and submits nothing.
fn reason_options(
    reasons: &[(String, Option<String>)],
    preselected: Option<&str>,
    placeholder: Placeholder,
) -> String {
    let suggested = preselected.filter(|s| reasons.iter().any(|(w, _)| w == s));
    let mut out = String::new();
    if suggested.is_none() {
        let (label, locked) = match placeholder {
            Placeholder::Required => (CHOOSE_REASON_LABEL, "true"),
            Placeholder::Optional(label) => (label, "false"),
        };
        out.push_str(&element(
            "reason-option",
            &[
                ("value", ""),
                ("label", label),
                ("title", ""),
                ("selected", "true"),
                ("placeholder", "true"),
                ("locked", locked),
            ],
            "",
        ));
    }
    for (word, meaning) in reasons {
        out.push_str(&element(
            "reason-option",
            &[
                ("value", word),
                ("label", word),
                ("title", meaning.as_deref().unwrap_or("")),
                ("selected", flag(suggested == Some(word.as_str()))),
                ("placeholder", "false"),
                ("locked", "false"),
            ],
            "",
        ));
    }
    out
}

/// Whether a member row carries the model's rating — without one the finding Sink refuses a
/// decline, and a batch states no rating of its own.
fn rated(member: &Value) -> bool {
    member.get("severity").and_then(Value::as_str).is_some()
}

/// The word the group's declined twin was declined with, when it has one.
fn twin_word(group: &Value) -> Option<&str> {
    group
        .get("twin")
        .filter(|t| !t.is_null())
        .and_then(|t| t.get("decision"))
        .and_then(|d| d.get(REASON_ARG))
        .and_then(Value::as_str)
}

/// ★ **Why a member's evidence does not count, when it does not** (Brian, 2026-10-01, amending
/// the 2026-09-25 rule — ledger #653): a group's `twin` is evidence only when the twin's
/// CURRENT decision is `confirmed` (browse 0.14.0), and so is a member's own `prior_decision`
/// when it carries one. A wordless decline made in a batch or a burst says nothing a person
/// evidently meant, and pre-ticking its repeats is how one mis-click propagated. The member is
/// still shown and still tickable; it only starts unticked, and the row says why, in words.
///
/// `None` = the evidence stands; `Some(sentence)` = the unconfirmed decision's description.
fn unconfirmed_evidence(group: &Value, member: &Value) -> Option<String> {
    let twin = group
        .get("twin")
        .filter(|t| !t.is_null())
        .and_then(|t| t.get("decision"))
        .filter(|d| !d.is_null());
    let prior = member.get("prior_decision").filter(|p| !p.is_null());
    for decision in [twin, prior].into_iter().flatten() {
        if !queue::confirmed(decision) {
            return Some(
                queue::unconfirmed_sentence(decision)
                    .unwrap_or_else(|| "not marked confirmed".to_string()),
            );
        }
    }
    None
}

/// One proposed group: its label, the declined twin or the kept row, and its members.
///
/// ★ A member starts TICKED only where the group carries evidence about it — a `twin` or a
/// `kept` row (the module docs). A target-only group's members start unticked, and its
/// suggested word is carried on the group (`suggested`) for the page to show beside it.
///
/// ★ With `echo` — this form's own refused submission (ledger #657) — every member is drawn
/// as it was SENT: ticked exactly when its box was, its own picker on the word it carried,
/// and a member the refusal named marked with why, in words. `focus` hands the browser's
/// focus to the first marked member's picker, once.
fn group_element(
    group: &Value,
    decide: bool,
    reasons: Option<&[(String, Option<String>)]>,
    per_twin: bool,
    echo: Option<&Submitted>,
    focus: &mut bool,
    cannot: &dyn Fn(&str) -> Option<String>,
) -> String {
    let text = |key: &str| group.get(key).and_then(Value::as_str).unwrap_or("");
    let mut children = String::new();
    let twin = group.get("twin").filter(|t| !t.is_null());
    if let Some(twin) = twin {
        children.push_str(&twin_element(twin));
    }
    if let Some(kept) = group.get("kept").filter(|k| !k.is_null()) {
        children.push_str(&row_element("kept", kept, &[], cannot));
    }
    let evidence = !by_target_only(group);
    // A member's own word, in the twin-carrying view: the twin's, when it has one.
    let twin_word = twin_word(group);
    for member in members(group) {
        let id = member.get("id").and_then(Value::as_str).unwrap_or("");
        // ⚠ An UNRATED finding cannot be declined without a rating the human states, and a
        // batch states none — so it is shown, and not offered, rather than offered and then
        // refused by the Sink. The row says so, and links the finding's own page, where a
        // single decision can carry the rating.
        let rated = rated(member);
        let tickable = decide && rated;
        // ★ Evidence must be CONFIRMED to pre-tick (ledger #653): see [`unconfirmed_evidence`].
        let unconfirmed = evidence
            .then(|| unconfirmed_evidence(group, member))
            .flatten();
        let ticked = match echo {
            Some(echo) => tickable && echo.members.contains(id),
            None => tickable && evidence && unconfirmed.is_none(),
        };
        let mut extra: Vec<(&str, String)> = vec![
            ("tickable", flag(tickable).to_string()),
            ("ticked", flag(ticked).to_string()),
            ("finding-href", browse_url(&finding_iri(id))),
            // The group this member was proposed in — the `batch=` its decline is stamped
            // with when this form fans out (ledger #653).
            (
                "group-key",
                group
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
        ];
        if let (true, Some(why)) = (tickable && echo.is_none(), &unconfirmed) {
            extra.push((
                "unticked-why",
                format!(
                    "Not pre-ticked: the decline it repeats is {why}. Read it, then tick it —                      or revisit that decline first."
                ),
            ));
            extra.push(("walk-href", crate::walk::href(None)));
            extra.push(("walk-label", crate::walk::REVISIT_LABEL.to_string()));
        }
        if decide && !rated {
            extra.push((
                "untickable",
                "unrated: a batch states no rating and a decline needs one, so decide this \
                 one singly —"
                    .to_string(),
            ));
            extra.push(("untickable-link", "open the finding".to_string()));
        }
        let mut inner = String::new();
        let mut picker = false;
        if per_twin && tickable {
            if let Some(reasons) = reasons {
                let where_ = where_of(member);
                extra.push(("reason-name", format!("{MEMBER_REASON_PREFIX}{id}")));
                extra.push(("reason-label", format!("reason for {where_}")));
                // What the page says, before any round trip, when this box is ticked with no
                // word of its own and no batch word is chosen — `web/gonk.js` makes the
                // picker required exactly then, and the browser shows this sentence.
                extra.push((
                    "reason-missing",
                    format!(
                        "{where_} is ticked with no word: pick one for it, choose a batch \
                         word, or untick it."
                    ),
                ));
                let own = match echo {
                    Some(echo) => echo.words.get(id).map(String::as_str),
                    None => twin_word,
                };
                inner = reason_options(reasons, own, Placeholder::Optional(TAKE_BATCH_WORD_LABEL));
                picker = true;
            }
        }
        if let Some(problem) = echo.and_then(|echo| echo.offending.get(id)) {
            extra.push(("problem", problem.sentence(per_twin && picker)));
            if picker && *focus {
                extra.push(("autofocus", "true".to_string()));
                *focus = false;
            }
        }
        children.push_str(&row_element_with("member", member, &extra, &inner, cannot));
    }
    let mut attributes: Vec<(&str, String)> = vec![
        ("key", text("key").to_string()),
        ("label", text("label").to_string()),
        ("repo", text("repo").to_string()),
    ];
    if !text("annotates").is_empty() {
        attributes.push(("browse-href", browse_url(text("annotates"))));
    }
    if let Some(left_out) = group.get(LEFT_OUT).and_then(Value::as_u64) {
        attributes.push((
            "left-out",
            format!(
                "{} in this group {} rated below the serious set and left out",
                plural(left_out as usize, "other finding", "other findings"),
                if left_out == 1 { "is" } else { "are" },
            ),
        ));
    }
    if let (false, Some(word)) = (evidence, suggested(group)) {
        attributes.push(("suggested", word.to_string()));
        attributes.push((
            "suggested-label",
            format!("suggested word: {word} — not selected; tick what you have read"),
        ));
    }
    wrap("group", &borrowed(&attributes), &children)
}

/// The declined twin's decision line: what a human already said about this claim.
fn twin_element(twin: &Value) -> String {
    let decision = twin.get("decision").filter(|d| !d.is_null());
    let text = |key: &str| {
        decision
            .and_then(|d| d.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    let id = twin.get("id").and_then(Value::as_str).unwrap_or("");
    let mut attributes: Vec<(&str, String)> = vec![
        ("twin-id", id.to_string()),
        ("outcome", text("outcome").to_string()),
        ("severity", text("severity").to_string()),
        ("at", web::when(text("decided_at"))),
    ];
    if !text(REASON_ARG).is_empty() {
        attributes.push((REASON_ARG, text(REASON_ARG).to_string()));
    }
    // ★ An unconfirmed twin is SAID, in words (ledger #653), with the way to the walk.
    if let Some(why) = decision.and_then(queue::unconfirmed_sentence) {
        attributes.push(("unconfirmed", why));
        attributes.push(("walk-href", crate::walk::href(None)));
        attributes.push(("walk-label", crate::walk::REVISIT_LABEL.to_string()));
    }
    let children = match text("note") {
        "" => String::new(),
        note => element("note", &[], note),
    };
    wrap("twin", &borrowed(&attributes), &children)
}

fn row_element(
    name: &str,
    row: &Value,
    extra: &[(&str, String)],
    cannot: &dyn Fn(&str) -> Option<String>,
) -> String {
    row_element_with(name, row, extra, "", cannot)
}

/// `path:line`, or the path alone — how a row is named to a person.
fn where_of(row: &Value) -> String {
    let path = row.get("path").and_then(Value::as_str).unwrap_or("");
    match row.get("line").and_then(Value::as_u64) {
        Some(line) => format!("{path}:{line}"),
        None => path.to_string(),
    }
}

/// One finding in a group — the fields a person needs to judge it before unticking it.
fn row_element_with(
    name: &str,
    row: &Value,
    extra: &[(&str, String)],
    inner: &str,
    cannot: &dyn Fn(&str) -> Option<String>,
) -> String {
    let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("");
    let proposal = row.get("severity").and_then(Value::as_str);
    // The judge's standing in words, on every row of a group (ledger #696); a refuted MEMBER
    // is folded, its box and picker outside the fold so it is still decided like the rest.
    let standing = crate::verdict::Standing::of(row, cannot);
    let mut attributes: Vec<(&str, String)> = vec![
        ("id", text("id").to_string()),
        ("where", where_of(row)),
        ("severity", proposal.unwrap_or("unrated").to_string()),
        (
            "severity-label",
            format!("model: {}", proposal.unwrap_or("unrated")),
        ),
        (
            "orphaned",
            flag(row.get("orphaned").and_then(Value::as_bool) == Some(true)).to_string(),
        ),
        ("provenance", queue::provenance(row)),
        ("verdict-label", standing.label()),
        ("verdict", standing.class().to_string()),
        (
            "fold",
            flag(name == "member" && standing.folds(queue::decided(row))).to_string(),
        ),
    ];
    if !text("annotates").is_empty() {
        attributes.push(("browse-href", browse_url(text("annotates"))));
    }
    attributes.extend(extra.iter().cloned());
    let mut children = element("body", &[], text("body"));
    if !text("exact").is_empty() {
        children.push_str(&element("quote", &[], text("exact")));
    }
    children.push_str(&crate::verdict::answers_element(
        row,
        name == "member" && standing.folds(queue::decided(row)),
    ));
    if let Some(prior) = row.get("prior_decision").filter(|p| !p.is_null()) {
        children.push_str(&queue::prior_element(prior));
    }
    children.push_str(inner);
    wrap(name, &borrowed(&attributes), &children)
}

// -------------------------------------------------------------------- the fan-out

/// `urn:iki:gonk:queue:batch` — one batch DECLINE, fanned out to the finding Sink.
///
/// ★ One `Sink urn:iki:finding:{id} decision=decline reason=<word>` per ticked member, under
/// the CALLER's capability, reported back as "declined N of M". A member that fails — decided
/// elsewhere meanwhile, a refusal — is named with its error and does not stop the others. The
/// section is then re-rendered from the SOURCE, so it never shows a decision the store did not
/// record.
///
/// ⚠ **The whole batch is checked before any member is tried**: nothing ticked, a member with
/// no word, a word the contract does not declare, or a decision other than decline refuses
/// the batch outright. A refusal that arrived half-way through would leave some members
/// decided and the person unsure which. ★ That refusal re-renders the form AS SENT, not from
/// the source (`Submitted`, ledger #657): all-or-nothing is about the store, never about
/// what the person typed.
pub struct Batch {
    pub web: Arc<Web>,
}

/// What a batch form carried.
struct Form {
    /// `_key`: which form on the page this was — the group's key, or the kind for the
    /// twin-carrying view's one form. Only a refusal's re-render reads it.
    key: Option<String>,
    members: Vec<String>,
    word: Option<String>,
    words: BTreeMap<String, String>,
    /// `key:<id>` — each member's group key, for the provenance stamp.
    keys: BTreeMap<String, String>,
    kind: Option<String>,
    repo: Option<String>,
    scope: Option<String>,
    decision: Option<String>,
}

fn read_form(body: &str) -> Result<Form> {
    let mut form = Form {
        key: None,
        members: Vec::new(),
        word: None,
        words: BTreeMap::new(),
        keys: BTreeMap::new(),
        kind: None,
        repo: None,
        scope: None,
        decision: None,
    };
    let kept = |v: String| Some(v.trim().to_string()).filter(|v| !v.is_empty());
    for (name, value) in web::form_pairs(body) {
        match name.as_str() {
            MEMBER_FIELD => {
                if let Some(id) = kept(value) {
                    if !form.members.contains(&id) {
                        form.members.push(id);
                    }
                }
            }
            REASON_ARG => form.word = kept(value),
            "decision" => form.decision = kept(value),
            "_group" => form.kind = kept(value),
            "_key" => form.key = kept(value),
            "_repo" => form.repo = kept(value),
            "_severity" => form.scope = kept(value),
            // ★ The door says how a decision was made (ledger #653): a form naming the
            // provenance itself is refused by name, as the single decide refuses it.
            queue::MADE_ARG | queue::BATCH_ARG => return Err(queue::provenance_refused(&name)),
            other => match other.strip_prefix(MEMBER_REASON_PREFIX) {
                Some(id) => {
                    if let Some(word) = kept(value) {
                        form.words.insert(id.to_string(), word);
                    }
                }
                None if other.starts_with(MEMBER_KEY_PREFIX) => {
                    if let Some(key) = kept(value) {
                        form.keys
                            .insert(other[MEMBER_KEY_PREFIX.len()..].to_string(), key);
                    }
                }
                None if other.starts_with('_') => {}
                None => {
                    return Err(Error::InvalidArgument {
                        name: other.to_string(),
                        detail: format!(
                            "a batch form carries `{MEMBER_FIELD}`, `{REASON_ARG}` and \
                             `{MEMBER_REASON_PREFIX}<id>`; this field is none of them"
                        ),
                    })
                }
            },
        }
    }
    Ok(form)
}

/// Why the refusal named one member.
pub(crate) enum Problem {
    /// Ticked, with no word of its own and no batch word to fall back on.
    NoWord,
    /// Its word is not one the finding contract declares.
    Undeclared(String),
}

impl Problem {
    /// The mark on the member's row, in words — never color alone. `own_picker` is whether
    /// the row carries a picker of its own (the twin-carrying view) or takes the form's word.
    fn sentence(&self, own_picker: bool) -> String {
        match (self, own_picker) {
            (Problem::NoWord, true) => "Not declined — this finding has no word: pick one \
                                        here, choose a batch word, or untick it."
                .to_string(),
            (Problem::NoWord, false) => "Not declined — this finding has no word: choose \
                                         the reason below, or untick it."
                .to_string(),
            (Problem::Undeclared(word), _) => format!(
                "Not declined — `{word}` is not a reason word the finding contract declares: \
                 pick another."
            ),
        }
    }
}

/// ★ **What a REFUSED batch form carried, handed back to the page** (ledger #657).
///
/// The refusal itself stays whole — nothing is written when any member fails the checks,
/// which is what keeps a batch from ever landing half-applied. What used to be lost was the
/// FORM: the section was re-rendered from the source, so every tick, untick and word the
/// person had set was replaced by the page's defaults, and nothing about them was stored.
/// Brian lost a whole recurrence batch to one missing word that way (2026-10-01). So a
/// refusal now re-renders that one form exactly as it was sent, every offending member
/// marked and the first one focused. A batch that WAS applied still re-renders from the
/// source: the page never shows a decision the store did not record.
pub(crate) struct Submitted {
    key: Option<String>,
    members: BTreeSet<String>,
    words: BTreeMap<String, String>,
    word: Option<String>,
    offending: BTreeMap<String, Problem>,
}

impl Submitted {
    /// Whether this submission came from the form keyed `key`. A page drawn before `_key`
    /// was sent (an open tab across a deploy) is matched by its members instead.
    fn belongs_to(&self, key: &str, holds: &dyn Fn(&str) -> bool) -> bool {
        match &self.key {
            Some(sent) => sent == key,
            None => self.members.iter().any(|id| holds(id)),
        }
    }
}

/// A batch the checks refused: the sentence, and the members it names.
struct Refusal {
    text: String,
    offending: BTreeMap<String, Problem>,
}

impl From<String> for Refusal {
    fn from(text: String) -> Refusal {
        Refusal {
            text,
            offending: BTreeMap::new(),
        }
    }
}

impl Batch {
    /// The whole batch's checks, and the word each member will carry — or the refusal, before
    /// anything is written. ⚠ Every offending member is named, not the first: a person fixing
    /// one row at a time, one round trip each, is the failure this answer exists to prevent.
    fn plan(&self, form: &Form) -> std::result::Result<Vec<(String, String)>, Refusal> {
        if let Some(other) = form.decision.as_deref().filter(|d| *d != REASONED_DECISION) {
            return Err(format!(
                "Nothing was decided: a batch only declines, and this form asked for `{other}`. \
                 Publishing mints an annotation on each line, so it is one finding at a time."
            )
            .into());
        }
        if form.members.is_empty() {
            return Err("Nothing was declined: no finding was ticked."
                .to_string()
                .into());
        }
        let declared = one_of(
            &self.web.hub,
            &finding_iri(queue::PROBE_ID),
            Verb::Sink,
            REASON_ARG,
        )
        .ok_or_else(|| {
            Refusal::from(
                "Nothing was declined: the finding contract does not declare its reason \
                 words, and a batch decline needs one."
                    .to_string(),
            )
        })?;
        let mut plan = Vec::new();
        let mut offending = BTreeMap::new();
        let mut wordless: Vec<&str> = Vec::new();
        let mut undeclared: Vec<&str> = Vec::new();
        for id in &form.members {
            match form.words.get(id).or(form.word.as_ref()) {
                Some(word) if declared.contains(word) => plan.push((id.clone(), word.clone())),
                Some(word) => {
                    if !undeclared.contains(&word.as_str()) {
                        undeclared.push(word);
                    }
                    offending.insert(id.clone(), Problem::Undeclared(word.clone()));
                }
                None => {
                    wordless.push(id);
                    offending.insert(id.clone(), Problem::NoWord);
                }
            }
        }
        if offending.is_empty() {
            return Ok(plan);
        }
        let mut why = Vec::new();
        if !undeclared.is_empty() {
            let quoted: Vec<String> = undeclared.iter().map(|w| format!("`{w}`")).collect();
            why.push(format!(
                "{} {} the finding contract declares ({}).",
                quoted.join(", "),
                if undeclared.len() == 1 {
                    "is not a reason word"
                } else {
                    "are not reason words"
                },
                declared.join(", ")
            ));
        }
        if !wordless.is_empty() {
            why.push(format!(
                "a batch decline needs a reason word for every finding, and {} {} none — {}. \
                 Pick a word, or untick {}.",
                plural(wordless.len(), "ticked finding", "ticked findings"),
                if wordless.len() == 1 { "has" } else { "have" },
                wordless.join(", "),
                if wordless.len() == 1 { "it" } else { "them" },
            ));
        }
        Err(Refusal {
            text: format!(
                "Nothing was declined: {} Every tick and word you set is kept below, and {} \
                 marked.",
                why.join(" "),
                if offending.len() == 1 {
                    "the finding that needs one is"
                } else {
                    "each finding that needs one is"
                }
            ),
            offending,
        })
    }
}

#[async_trait]
impl Endpoint for Batch {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(Error::Endpoint(format!("`{BATCH_IRI}` answers Sink only")));
        }
        let form = read_form(inv.inline_str("content")?)?;
        // Like the single decision: with no finding family bound there is nothing to decide,
        // and that is a typed NotFound rather than a rendered sentence.
        let probe = finding_iri(queue::PROBE_ID);
        let probe_iri = Iri::parse(&probe).map_err(|e| Error::Endpoint(format!("{probe}: {e}")))?;
        if self.web.hub.describe(&probe_iri).is_none() {
            return Err(Error::NotFound(format!(
                "nothing is bound at `{}{{id}}`",
                queue::FINDING_PREFIX
            )));
        }

        // ⚠ A refusal is RENDERED, as on the single decision: htmx does not swap a non-2xx
        // response, and the refusal is the most informative answer the page can give. ★ And
        // it is rendered WITH WHAT WAS SENT (ledger #657), so it costs the person nothing.
        let mut submitted = None;
        let flash = match self.plan(&form) {
            Err(refusal) => {
                submitted = Some(Submitted {
                    key: form.key.clone(),
                    members: form.members.iter().cloned().collect(),
                    words: form.words.clone(),
                    word: form.word.clone(),
                    offending: refusal.offending,
                });
                ("error", refusal.text)
            }
            Ok(plan) => {
                let total = plan.len();
                let mut failed: Vec<String> = Vec::new();
                // ★ Stamped at the door (ledger #653): every decline this adapter forwards was
                // made in a batch, under the group it was proposed in — when the contract
                // declares the word ([`queue::made_word`]) and the form says which group.
                let made = queue::made_word(&self.web.hub, queue::MADE_BATCH);
                for (id, word) in &plan {
                    let target = finding_iri(id);
                    let key = form
                        .keys
                        .get(id)
                        .or(form.key.as_ref())
                        .or(form.kind.as_ref());
                    let outcome = match Iri::parse(&target) {
                        Err(e) => Err(format!("`{target}`: {e}")),
                        Ok(target) => {
                            let mut request = Request::new(Verb::Sink, target)
                                .with_arg(
                                    "decision",
                                    ArgRef::Inline(REASONED_DECISION.as_bytes().to_vec()),
                                )
                                .with_arg(REASON_ARG, ArgRef::Inline(word.as_bytes().to_vec()));
                            if let (Some(made), Some(key)) = (made, key) {
                                request = request
                                    .with_arg(
                                        queue::MADE_ARG,
                                        ArgRef::Inline(made.as_bytes().to_vec()),
                                    )
                                    .with_arg(
                                        queue::BATCH_ARG,
                                        ArgRef::Inline(key.as_bytes().to_vec()),
                                    );
                            }
                            inv.issue(request)
                                .await
                                .map(|_| ())
                                .map_err(|e| format!("{e}"))
                        }
                    };
                    if let Err(why) = outcome {
                        failed.push(format!("{id} — {why}"));
                    }
                }
                let declined = total - failed.len();
                let mut sentence = format!("Declined {declined} of {total}.");
                if !failed.is_empty() {
                    sentence.push_str(&format!(" Not declined: {}.", failed.join("; ")));
                }
                (if failed.is_empty() { "ok" } else { "error" }, sentence)
            }
        };
        let rows = QueuePage {
            web: Arc::clone(&self.web),
            fragment: true,
        };
        let params = queue::Params {
            state: None,
            repo: form.repo,
            limit: None,
            scope: form.scope,
            group: form.kind,
            summary: None,
        };
        let echo = match &submitted {
            Some(submitted) => queue::Echo::Batch(submitted),
            None => queue::Echo::Nothing,
        };
        rows.body(inv, &params, Some((flash.0, flash.1.as_str())), echo)
            .await
    }

    fn name(&self) -> &str {
        "gonk-queue-batch"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-queue-batch")
            .title("Decline a proposed group of findings, once")
            .summary(
                "The Queue's batch form adapter (ledger #506): an urlencoded body naming every \
                 ticked `member=<id>`, one `reason` word for the batch, and/or `reason:<id>` \
                 per member — a member's own word wins, and `reason` is the fallback for a \
                 member without one (ledger #508: a declined twin from before reason words \
                 existed has none). The whole \
                 batch is checked first — nothing ticked, a member with no word, or a word the \
                 finding contract does not declare refuses it before anything is written. Then \
                 ONE `Sink urn:iki:finding:{id} decision=decline reason=<word>` per member, \
                 under the caller's own capability, so each gets an ordinary decision node, \
                 stamped by the door `made=batch batch=<the member's group key>` (ledger #653; \
                 a form naming `made` or `batch` is refused); a \
                 member that fails is named with its error and does not stop the others. \
                 Answered with the queue section re-rendered from the source — or, when the \
                 checks refused the batch, with the submitted form drawn back as it was sent \
                 and every offending member marked (ledger #657). ⚠ Decline only: \
                 publishing mints an annotation on each line and is one finding at a time.",
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary("decline every ticked member, one decision each, and re-render")
                    // Every call it can make is the finding Sink, which requires both.
                    .requires(ikigai_browse::CAP_ANNOTATE)
                    .requires(ikigai_browse::CAP_WILDCARD)
                    .input(ArgSpec::new("content").class(XSD_STRING).summary(
                        "the form: `member` (repeated), `reason:<id>` per member and/or \
                         `reason` as the fallback for members without one, `key:<id>` per \
                         member (its group key, for the provenance stamp), plus `_group`, \
                         `_key`, `_repo` and `_severity` for the re-render",
                    ))
                    .output("text/html"),
            )
            .verb(Verb::Meta)
    }
}

/// `xsd:string`, spelled once here.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
