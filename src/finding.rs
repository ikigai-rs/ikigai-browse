//! `urn:iki:finding:{id}` + `urn:repo:{repo}:findings[:{path}]` — the **pending
//! review queue**: what a machine review pass produces now, and the only door
//! into the annotation family.
//!
//! ## The rule this module implements
//!
//! > *"Nothing gets published to Gonk except by the human."*
//!
//! Not just critical findings — **everything**. A review pass used to mint its
//! findings as live `urn:iki:annotation:` records, immediately, as its terminal
//! step. It does not any more: a pass mints [`crate::annotate::Family::Finding`]
//! records, and `Sink urn:iki:finding:{id} decision=publish` — gated by
//! `urn:cap:annotate` — is the only thing that ever promotes one into the
//! annotation family.
//!
//! ★ **That makes the button/trigger invariant hold by construction rather than
//! by care.** The standing rule is that a clicked review and a git-event
//! trigger must be the same call with two causes. The old worry was that
//! "automated findings queue, clicked ones publish" would break it. Under this
//! rule there is nothing to break: **both causes produce pending findings, and
//! publication is a third act that is always human.** The gate keys on the
//! FINDING, never on the cause — so severity is triage ("what to look at
//! first"), not permission.
//!
//! ⚠ **And a queue is not a gate.** A finding awaiting publication blocks
//! nothing — not the commit, not a push, not a merge. The word "queued" reads
//! like a gate to anyone who has used one, so the faces say so in as many
//! words.
//!
//! ## Two ratings, and neither overwrites the other
//!
//! The model proposes a severity; the human accepts it or re-rates it. **Both
//! survive**, because they live on different nodes:
//!
//! ```turtle
//! <urn:iki:finding:{id}> a prov:Entity ;                 # the machine's claim
//!     dcterms:description "the reviewer's note" ;
//!     dcterms:creator "qwen3-coder:30b" ;
//!     prov:wasGeneratedBy <urn:ikigai:browse:review:…> ;
//!     sh:resultSeverity <urn:iki:severity:major> ;       # THE PROPOSAL
//!     ik:annotates <urn:repo:demo:file:src/lib.rs> ;
//!     ik:repo "demo" ; ik:path "src/lib.rs" ; ik:contentHash "sha256:…" ;
//!     oa:hasSelector <urn:iki:finding:{id}:selector:quote> ,
//!                    <urn:iki:finding:{id}:selector:position> .
//!
//! <urn:iki:finding:{id}:decision> a prov:Activity ;      # the human's act
//!     prov:used <urn:iki:finding:{id}> ;
//!     dcterms:type <urn:iki:finding:outcome:published> ;
//!     sh:resultSeverity <urn:iki:severity:minor> ;       # THE FINAL RATING
//!     dcterms:created "…"^^xsd:dateTime ;
//!     prov:generated <urn:iki:annotation:{id}> .
//! ```
//!
//! ★ *"How often does the model over-rate?"*, *"which backends are calibrated
//! on this codebase?"*, *"did re-rating shift after we changed the prompt?"*
//! are then queries over the graph rather than impressions — and they are
//! unrecoverable the moment one `severity` field lets a human overwrite the
//! proposal.
//!
//! ## The severity set is a closed `one_of`, in the contract
//!
//! [`SEVERITIES`] is the one list: it is the `one_of` on the Sink's ArgSpec,
//! the words the review prompt constrains the model to, and what the option
//! menu renders from. ⚠ The live counter-example this avoids is the ledger's
//! close `reason`, whose five legal values are named in its ERROR MESSAGE and
//! not in its contract — so a caller learns the options only by getting it
//! wrong. A UI (this module's, or gonk's) builds its menu from the description,
//! never from a hard-coded list.
//!
//! ## A decision is a record, and a later one may revise it
//!
//! Declining KEEPS the finding, with the human's reason if they gave one: a
//! re-run then knows a person looked and said no, which is the beginning of a
//! feedback signal rather than churn. The reason has two halves, both
//! optional and kept apart: `reason=` is ONE WORD from [`DECLINE_REASONS`],
//! stored on the decision node as a term (`dcterms:subject
//! <urn:iki:decline-reason:restates>` — an interim predicate, see
//! [`crate::annotate::DCTERMS_SUBJECT`]) so it can be counted; the piped
//! `content` is the free-text note (`dcterms:description`). A word is refused
//! beside anything but `decision=decline` — it would have no consumer.
//!
//! ★ **The record is append-only (ledger #653).** Every decision is its own
//! node, written once and never re-stored or removed; a finding's answers are
//! a CHAIN of them, oldest first. Nothing is overwritten — what can change is
//! which node is CURRENT:
//!
//! ```turtle
//! <urn:iki:finding:{id}:decision> a prov:Activity ;       # the first answer
//!     prov:used <urn:iki:finding:{id}> ;
//!     dcterms:type <urn:iki:finding:outcome:declined> ;
//!     sh:resultSeverity <urn:iki:severity:major> ;
//!     dcterms:created "2026-09-23T02:42:38.120Z"^^xsd:dateTime .
//!
//! <urn:iki:finding:{id}:decision:2> a prov:Activity ;     # its revision
//!     prov:used <urn:iki:finding:{id}> ;
//!     dcterms:type <urn:iki:finding:outcome:declined> ;
//!     sh:resultSeverity <urn:iki:severity:major> ;
//!     dcterms:subject <urn:iki:decline-reason:restates> ;
//!     dcterms:replaces <urn:iki:finding:{id}:decision> ;
//!     dcterms:provenance <urn:iki:decision-made:single> ;
//!     dcterms:created "2026-10-01T16:05:11.004Z"^^xsd:dateTime .
//! ```
//!
//! (`dcterms:replaces`, not `prov:wasRevisionOf`: PROV-O types that term's
//! subject and object as `prov:Entity`, which a `prov:Activity` cannot be —
//! see [`crate::annotate::DCTERMS_REPLACES`].)
//!
//! The Sink's rules, each one a test:
//!
//! * **A second decision is refused by default**, naming what is on file and
//!   the IRI to revise — a stray double-submit must not rewrite history. An
//!   IDENTICAL repeat is a no-op (a double-clicked button is not an error), and
//!   so is the second submit of a revision that already landed.
//! * **`revises=<decision IRI>` makes it a revision**, accepted only when it
//!   names the finding's CURRENT decision (the head of the chain). Naming an
//!   earlier one, or one that is not the finding's, is refused with the
//!   current one's IRI. The revision keeps its own author, time, rating,
//!   reason, note and provenance, and becomes current. Three revisions matter:
//!   *confirm* (a wordless decline gains a word: decline → decline),
//!   *retract*, and *reverse* (decline → publish, which promotes exactly as a
//!   first publish does — orphaned if the quote is gone).
//! * **`decision=retract` withdraws the current answer.** It is an OUTCOME
//!   word with its own node (`dcterms:type <urn:iki:finding:outcome:retracted>`),
//!   not a revision with no outcome: the outcome is how the loader tells a
//!   decision node from anything else, and a retraction is a human act with an
//!   author, a time, a note and a provenance like any other. It rates nothing
//!   (`severity=` is refused beside it). After it the finding is UNDECIDED —
//!   pending or superseded as its file says — and takes a decision like any
//!   undecided finding, without `revises=` (the new node still links the
//!   retraction, so the chain stays one chain). A retracted decline is not a
//!   `prior_decision` for any recurrence, and neither is a decline reversed to
//!   a publish: the mark follows the twin's CURRENT decision.
//! * **A publication stands while its annotation does.** Publish → anything
//!   is refused while `urn:iki:annotation:{id}` exists, with the instruction:
//!   delete it (`Delete urn:iki:annotation:{id}`, the annotation family's own
//!   visible act, under the same capability), then revise. ★ Chosen over a
//!   revision that deletes the annotation as a side effect, because a decision
//!   Sink that silently removes a published note from every reader is the
//!   invisible write this family exists to prevent; once the annotation is
//!   gone, the revision is an ordinary one, and the publish stays in the chain.
//! * Capability: exactly what the first decision needs, `urn:cap:annotate`.
//!
//! ## How a decision was made, and whether it was meant
//!
//! `made=single`, or `made=batch batch=<group key>` (a `group=` proposal's
//! `key`), is recorded on the node (`dcterms:provenance`) when the caller says
//! — gonk stamps it at its door. Older decisions have none. Every decision and
//! every `prior_decision` then carries a COMPUTED `confirmed` flag: false for a
//! decline with no reason word that was made in a batch or, with no provenance
//! on record, in a BURST (three or more declines inside one second —
//! [`crate::revision`] says why those numbers). A confirming revision is a new
//! decision and reads true by the same rule. Nothing is hidden or dropped by
//! it: it is information for a host's pre-tick rule and display. And
//! `summary=unconfirmed` lists the unconfirmed declines that still steer a
//! pending finding, by burst, oldest first — the list a human walks.
//!
//! ### The JSON a host reads (the contract)
//!
//! On a finding row: `decision` (the CURRENT answer, null while undecided),
//! `decisions` (every node, oldest first) and `prior_decision`, each decision
//! in one shape — `iri`, `outcome` (`published` | `declined` | `retracted`),
//! `severity`, `decided_at`, `note`, `reason`, `minted`, `revises` (the node it
//! revises, or null), `made` (`single` | `batch` | null), `batch` (the group
//! key, or null), `confirmed` (bool) and `burst` (the burst's first timestamp,
//! or null); `prior_decision` adds `finding`, the declined twin.
//! ## Staleness borrows the annotation layer's answer; it does not invent one
//!
//! A pending finding whose file has since changed is stale by construction.
//! Every read runs the SAME drift pass annotations run — re-anchor when the
//! quote moved (`ik:reanchored`), orphan when it is gone (`ik:orphaned`), never
//! silently drop. Promotion copies the finding's selectors as they stand, so a
//! published-while-orphaned finding becomes an orphaned annotation and the
//! annotation layer keeps reconciling it. There is exactly one drift story in
//! this crate and findings are inside it.
//!
//! ## Superseded: a state nobody decides and nothing stores
//!
//! An undecided finding whose file's CURRENT review does not stand behind it
//! — the code it was about changed, and no pass over the file's current
//! content minted or carried it — is `superseded`, not `pending` (ledger
//! #504). Without it, a changed region's old findings stayed pending forever
//! and the queue grew with commit count. It is computed on every read from the
//! store and the file's current hash ([`crate::supersede`]), never written, so
//! a revert un-supersedes with no code for it. It asserts no judgment: a human
//! may still publish or decline a superseded finding, and it never feeds the
//! declined-twin mark.
//!
//! ## Capabilities
//!
//! Source requires the browse read wildcard (reading a finding is reading about
//! its target). Sink requires **`urn:cap:annotate`** — publishing MINTS into the
//! annotation family, and declining writes the record that a holder of that
//! authority made — plus the browse wildcard, since a decision reads the
//! target to re-anchor.
//!
//! ⚠ The review pass itself no longer requires `urn:cap:annotate`, because it
//! no longer writes annotations. ★ That is the safety interlock: a git-event
//! trigger can run headlessly with browse+net and still cannot publish
//! anything. It is also the one thing to notice about the pending queue's own
//! cost — anyone who may run a review may fill the queue, and nothing
//! compacts it yet (ledger #437).

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgSpec, Bindings, Description, Endpoint, EndpointSpace, Error, Grammar,
    Invocation, Iri, Representation, Result, UriTemplate, Verb,
};

use crate::annotate::{
    self, Annotation, Decision, Family, Made, Outcome, CAP_ANNOTATE, DECLINE, MADE_WORDS, PUBLISH,
    RETRACT,
};
use crate::archive::Archive;
use crate::explain::iso8601;
use crate::{
    crumbs_html, esc, granted, path_binding, repo_root, repr, repr_utf8, Roots, CAP_WILDCARD,
    XSD_STRING,
};

/// **The severity set** — the closed list, in triage order (most urgent
/// first). It is the `one_of` on the decision Sink, the words the review
/// prompt hands the model, and what every menu renders from.
///
/// ⚠ Choosing these was a decision, not an inheritance of whatever the prompt
/// happened to emit, and the model is constrained to exactly this list — two
/// lists disagree the first time a model invents a word.
///
/// What each word MEANS is [`SEVERITY_MEANINGS`], not a list here: a prose
/// gloss in this comment is a second copy that no test can check, and it would
/// drift from the definitions the model is actually rating against.
///
/// ★ `praise` is the one that needs a note beyond its definition. It is not a
/// severity in the usual sense and is deliberately in the set anyway: review-v4
/// stopped ASKING for it, but a set with no bucket for a compliment forces the
/// model to file one as `info`, after which triage cannot tell them apart.
pub(crate) const SEVERITIES: [&str; 5] = ["critical", "major", "minor", "info", "praise"];

/// What each of [`SEVERITIES`] MEANS, in the same order — the definitions the
/// review prompt spells out for the model, reading as `"{word} for {meaning}"`.
///
/// ★ They live beside the words rather than inside the prompt string because a
/// severity whose meaning is stated in one place and enforced from another is
/// the same defect as a closed set named only in an error message. One source:
/// the model rates against this text, a human triaging reads it here, and the
/// prompt is built from it.
pub(crate) const SEVERITY_MEANINGS: [&str; 5] = [
    "something that will bite in production (data loss, a security hole, corruption)",
    "a real defect or design risk that should be fixed",
    "a small improvement where correctness is not at stake",
    "an observation or a question",
    "a genuine strength",
];

/// How many of [`SEVERITIES`], counted from the front, are SERIOUS — the class
/// the review prompt reports without limit.
///
/// ★ The list is ordered worst-first, so the reporting threshold is a PREFIX
/// LENGTH over it and not a fourth copy of the words. The Sink's `one_of`, the
/// menu, the prompt and this split therefore move together: adding a severity
/// or reordering the list changes what the prompt asks for in the same edit,
/// and [`the_reporting_threshold_partitions_the_severity_list`] fails if the
/// order stops matching the tiers.
pub(crate) const SERIOUS_SEVERITIES: usize = 2;

/// Where the compliment bucket begins — everything between
/// [`SERIOUS_SEVERITIES`] and here is a SUGGESTION: welcome, optional, and
/// bounded, because a rejected suggestion costs one click while a missed
/// serious problem is silent.
pub(crate) const PRAISE_SEVERITY: usize = 4;

/// Whether `word` is one of [`SEVERITIES`] — the one place the set is checked.
pub(crate) fn is_severity(word: &str) -> bool {
    SEVERITIES.contains(&word)
}

/// **Why a human declined** — the closed list, one word each, in the order a
/// picker shows them. It is the `one_of` on the decision Sink's `reason`, and
/// the ONLY place the words are spelled: gonk's decide form reads them from
/// the Meta face, as it reads [`SEVERITIES`], because a word set written down
/// in the host is the one place the words could drift.
///
/// ★ The set came from the 2026-09-21/22 hand triage, not from a taxonomy:
/// each word names a shape that recurred in the 96 declines read that night
/// (`restates` alone was 60 of them). What each MEANS is
/// [`DECLINE_REASON_MEANINGS`], the text the ArgSpec summary carries.
///
/// ⚠ Stored as a TERM, `urn:iki:decline-reason:{word}`, never as text — so
/// "how many of this file's declines were `restates`?" is a count over IRIs,
/// and a word outside the set reads back as no reason rather than as an
/// invented one.
pub(crate) const DECLINE_REASONS: [&str; 5] =
    ["misread", "restates", "no-issue", "wont-fix", "duplicate"];

/// What each of [`DECLINE_REASONS`] MEANS, in the same order. The Sink's
/// `reason` summary is built from this, so a manifold reader sees the
/// definitions a human chose against.
pub(crate) const DECLINE_REASON_MEANINGS: [&str; 5] = [
    "the claim is false — the model misunderstood the code or the domain",
    "the text already says it — a comment or doc that DISCLOSES a hazard, restated as a finding",
    "true, but not a defect — style, \"could be clearer\", or an absence asserted as a finding",
    "real, and deliberately left as it is",
    "already raised — a twin exists",
];

/// Whether `word` is one of [`DECLINE_REASONS`] — the one place the set is
/// checked, by the Sink and by the loader alike.
pub(crate) fn is_decline_reason(word: &str) -> bool {
    DECLINE_REASONS.contains(&word)
}

/// The Sink's `reason` summary: when it applies, then every word with its
/// meaning, in contract order.
fn decline_reason_summary() -> String {
    let words: Vec<String> = DECLINE_REASONS
        .iter()
        .zip(DECLINE_REASON_MEANINGS)
        .map(|(word, meaning)| format!("{word}: {meaning}"))
        .collect();
    format!(
        "why a human declined, in one word — only with decision=decline (refused with \
         publish). Omitted = no reason stated. Stored as the term \
         urn:iki:decline-reason:{{word}}, separate from the free-text content. {}.",
        words.join("; ")
    )
}

/// Severity words as prose for the prompt — `join_words(&["minor", "info"],
/// "or")` reads `minor or info`. The prompt hands the model the SAME words the
/// contract declares rather than a retyped list.
pub(crate) fn join_words(words: &[&str], conjunction: &str) -> String {
    match words {
        [] => String::new(),
        [one] => one.to_string(),
        [head @ .., last] => format!("{} {conjunction} {last}", head.join(", ")),
    }
}

/// `urn:iki:finding:{id}` — one pending finding.
pub(crate) fn finding_iri(id: &str) -> String {
    annotate::record_iri(Family::Finding, id)
}

/// `urn:repo:{repo}:findings[:{path}]` — the queue face.
pub(crate) fn findings_iri(repo: &str, rel: &str) -> String {
    match rel.is_empty() {
        true => format!("urn:repo:{repo}:findings"),
        false => format!("urn:repo:{repo}:findings:{}", crate::iri_encode(rel)),
    }
}

/// The queue link a file face carries beside its review button: the findings
/// for THIS path, whatever state they are in.
///
/// ★ It is unconditional and carries no count on purpose. A count would mean
/// scanning the store on every file render to decide whether to draw a link,
/// and an absent link is indistinguishable from an absent feature — which is
/// exactly the black hole this arc had to avoid between here and gonk's Queue
/// page.
pub(crate) fn findings_link_html(repo: &str, rel: &str) -> String {
    format!(
        "<button class=\"browse-findings-link\" title=\"review findings awaiting a human — \
         publishing is never automatic\" hx-get=\"/k/source {iri} as=text/html state=all\" \
         hx-target=\"#browse\" hx-swap=\"innerHTML\">findings</button>",
        iri = findings_iri(repo, rel),
    )
}

// --- faces shared with the annotation card ----------------------------------

/// The severity line on a finding card: the model's proposal, and — once a
/// human has answered — their final rating beside it.
///
/// ★ **Both, always, even when they agree.** "The model said major and the
/// human agreed" and "the human rated major" are different facts, and a card
/// that collapsed them would quietly delete the calibration signal from the
/// only surface a person actually reads.
pub(crate) fn severity_badge_html(proposed: Option<&str>, decision: Option<&Decision>) -> String {
    let proposed = proposed.unwrap_or("unrated");
    let mut out = format!(
        "<span class=\"browse-finding-severity browse-finding-severity-{p}\">{p}</span>\
         <span class=\"browse-finding-rater\">proposed by the model</span>",
        p = esc(proposed),
    );
    if let Some(decision) = decision {
        out.push_str(&format!(
            "<span class=\"browse-finding-severity browse-finding-severity-{s}\">{s}</span>\
             <span class=\"browse-finding-rater\">{outcome} by a human</span>",
            s = esc(decision.severity.as_deref().unwrap_or("unrated")),
            outcome = decision.outcome.label(),
        ));
    }
    out
}

/// The decision affordance under a PENDING finding's card, or the record of
/// the decision once there is one.
///
/// The form is two submit buttons sharing one severity menu: publishing and
/// declining differ in one field, and splitting them into two forms would mean
/// two menus that can disagree. The menu's options come from [`SEVERITIES`] —
/// the same constant the Sink's `one_of` is built from — with the model's
/// proposal pre-selected, so accepting is the default path and re-rating is one
/// selection away.
///
/// Markup only: the host's `/k/` adapter turns the form into a Sink, the same
/// assumption every other S0 face documents. The activating button's
/// `name`/`value` is what carries `decision=`.
pub(crate) fn decision_html(
    id: &str,
    proposed: Option<&str>,
    decision: Option<&Decision>,
    head: Option<&Decision>,
) -> String {
    if let Some(decision) = decision {
        let mut out = format!(
            "<p class=\"browse-finding-decision\">{} by a human",
            esc(decision.outcome.label())
        );
        if let Some(reason) = &decision.reason {
            out.push_str(&format!(
                " <span class=\"browse-finding-decline-reason\">({})</span>",
                esc(reason)
            ));
        }
        if !decision.confirmed {
            out.push_str(&format!(
                " <span class=\"browse-finding-unconfirmed\">unconfirmed — no word, made in {}\
                 </span>",
                match &decision.made {
                    Some(Made::Batch(key)) => format!("batch {}", esc(key)),
                    _ => format!(
                        "a burst at {}",
                        esc(decision.burst.as_deref().unwrap_or("an unknown time"))
                    ),
                }
            ));
        }
        if let Some(at) = &decision.at {
            out.push_str(&format!(" · {}", esc(at)));
        }
        if let Some(minted) = &decision.minted {
            out.push_str(&format!(
                " · <a class=\"browse-finding-minted\" href=\"#\" \
                 hx-get=\"/k/source {iri} as=application/json\" hx-target=\"#browse\" \
                 hx-swap=\"innerHTML\">{iri}</a>",
                iri = esc(minted)
            ));
        }
        out.push_str("</p>");
        if let Some(note) = &decision.note {
            out.push_str(&format!(
                "<p class=\"browse-finding-reason\">{}</p>",
                esc(note)
            ));
        }
        if decision.outcome == Outcome::Declined {
            out.push_str(&revise_html(id, decision));
        }
        return out;
    }
    let mut out = String::new();
    // Undecided AGAIN: say so, so the form below reads as a second answer.
    if let Some(retraction) = head.filter(|d| d.outcome == Outcome::Retracted) {
        out.push_str(&format!(
            "<p class=\"browse-finding-decision browse-finding-retracted\">an earlier answer \
             was retracted by a human{}</p>",
            retraction
                .at
                .as_deref()
                .map(|at| format!(" · {}", esc(at)))
                .unwrap_or_default()
        ));
    }
    let mut options = String::new();
    for severity in SEVERITIES {
        let selected = match proposed == Some(severity) {
            true => " selected",
            false => "",
        };
        options.push_str(&format!(
            "<option value=\"{severity}\"{selected}>{severity}</option>"
        ));
    }
    out.push_str(&format!(
        "<form class=\"browse-finding-decide\" hx-post=\"/k/sink {iri}\" hx-target=\"#browse\" \
         hx-swap=\"innerHTML\">\
         <label class=\"browse-finding-label\">severity \
         <select name=\"severity\">{options}</select></label>\
         <label class=\"browse-finding-label\">reason (decline only) \
         <select name=\"reason\">{reasons}</select></label>\
         <textarea name=\"content\" placeholder=\"why (optional; kept either way)\"></textarea>\
         <button type=\"submit\" name=\"decision\" value=\"{PUBLISH}\">publish</button>\
         <button type=\"submit\" name=\"decision\" value=\"{DECLINE}\">decline</button>\
         </form>",
        iri = esc(&finding_iri(id)),
        reasons = reason_options(None),
    ));
    out
}

/// The reason picker: every word of [`DECLINE_REASONS`], its meaning as the
/// option's title, behind an EMPTY default — which the Sink reads as
/// "omitted", so the publish button sharing a form is never refused for it
/// and a decline without a word stays one click. `current` pre-selects a word
/// on file instead.
fn reason_options(current: Option<&str>) -> String {
    let mut reasons = format!(
        "<option value=\"\"{}>—</option>",
        if current.is_none() { " selected" } else { "" }
    );
    for (word, meaning) in DECLINE_REASONS.iter().zip(DECLINE_REASON_MEANINGS) {
        reasons.push_str(&format!(
            "<option value=\"{word}\" title=\"{}\"{}>{word}</option>",
            esc(meaning),
            if current == Some(*word) {
                " selected"
            } else {
                ""
            }
        ));
    }
    reasons
}

/// The REVISION affordance under a declined finding's record (ledger #653):
/// confirm (decline again, with a word), reverse (publish), or retract — each
/// naming the decision it revises, so the Sink accepts it and keeps both.
///
/// ★ Two forms, not one: a retraction states no rating and is refused one,
/// so it cannot share the form whose severity menu always submits a value.
/// Behind `<details>`, because a decline is usually final and the record is
/// what a reader came for. Markup only, like every other S0 face — and it
/// stamps no `made=`: how a decision was made is the host's to say at its
/// door, and a form cannot know whether it is being fanned out.
fn revise_html(id: &str, decision: &Decision) -> String {
    let iri = esc(&finding_iri(id));
    let revises = esc(&decision.iri);
    let mut options = String::new();
    for severity in SEVERITIES {
        let selected = match decision.severity.as_deref() == Some(severity) {
            true => " selected",
            false => "",
        };
        options.push_str(&format!(
            "<option value=\"{severity}\"{selected}>{severity}</option>"
        ));
    }
    format!(
        "<details class=\"browse-finding-revise\"><summary>revise this decision</summary>\
         <form class=\"browse-finding-decide\" hx-post=\"/k/sink {iri}\" hx-target=\"#browse\" \
         hx-swap=\"innerHTML\">\
         <input type=\"hidden\" name=\"revises\" value=\"{revises}\">\
         <label class=\"browse-finding-label\">severity \
         <select name=\"severity\">{options}</select></label>\
         <label class=\"browse-finding-label\">reason (decline only) \
         <select name=\"reason\">{reasons}</select></label>\
         <textarea name=\"content\" placeholder=\"why (optional; kept either way)\"></textarea>\
         <button type=\"submit\" name=\"decision\" value=\"{DECLINE}\">decline (confirm)</button>\
         <button type=\"submit\" name=\"decision\" value=\"{PUBLISH}\">publish instead</button>\
         </form>\
         <form class=\"browse-finding-retract\" hx-post=\"/k/sink {iri}\" hx-target=\"#browse\" \
         hx-swap=\"innerHTML\">\
         <input type=\"hidden\" name=\"revises\" value=\"{revises}\">\
         <textarea name=\"content\" placeholder=\"why withdraw it (optional)\"></textarea>\
         <button type=\"submit\" name=\"decision\" value=\"{RETRACT}\">retract</button>\
         </form></details>",
        reasons = reason_options(decision.reason.as_deref()),
    )
}

/// The standing note every queue face carries. ⚠ It is not decoration: "queued
/// for review" reads like a gate, and this pipeline gates nothing.
pub(crate) const NOT_A_GATE: &str =
    "Findings wait for a person. Nothing here blocks a commit, a push or a \
                          merge, and nothing reaches the annotation family until someone \
                          publishes it.";

/// **The queue states** a listing can show — the `one_of` on the findings
/// face's `state`, the state nav's buttons, and the check the listing makes,
/// all from this one list. gonk builds its own state nav from the contract, so
/// a state added here reaches it with no host change.
///
/// `superseded` (ledger #504) is an UNDECIDED finding whose file's current
/// review does not stand behind it — computed on read, see
/// [`crate::supersede`]. `pending` no longer includes it; `all` does.
pub(crate) const STATES: [&str; 5] = ["pending", "superseded", "published", "declined", "all"];

/// What `summary=` may ask for — each widens the json face to an object that
/// carries the rows plus its own key, and names a different grain.
pub(crate) const SUMMARIES: [&str; 3] = ["declined", "states", "unconfirmed"];

// --- binding ----------------------------------------------------------------

pub(crate) fn bind(space: EndpointSpace, roots: &Roots, archive: &Arc<Archive>) -> EndpointSpace {
    let space = space.bind(
        FindingGrammar::new(),
        FindingEndpoint {
            roots: Arc::clone(roots),
            archive: Arc::clone(archive),
        },
    );
    let listing: Arc<dyn Endpoint> = Arc::new(FindingsEndpoint {
        roots: Arc::clone(roots),
        archive: Arc::clone(archive),
    });
    crate::bind_family(
        space,
        roots,
        listing,
        Some("findings"),
        Some("findings:{path}"),
    )
}

/// `urn:iki:finding:{id}`. Unlike the annotation family there is no bare form:
/// a finding is never created by a caller — only a review pass mints one.
struct FindingGrammar {
    template: UriTemplate,
}

impl FindingGrammar {
    fn new() -> Self {
        FindingGrammar {
            template: UriTemplate::parse("urn:iki:finding:{id}")
                .expect("the finding template is valid"),
        }
    }
}

impl Grammar for FindingGrammar {
    fn match_iri(&self, iri: &Iri) -> Option<Bindings> {
        // ⚠ The decision node and the two selectors are sub-IRIs of a finding
        // and are DATA, not resources: matching them would advertise a Sink on
        // `urn:iki:finding:{id}:decision` that would then mint
        // `urn:iki:finding:{id}:decision:decision`.
        let bindings = self.template.match_iri(iri)?;
        let id = bindings.get("id")?;
        (!id.contains(':')).then_some(bindings)
    }

    fn pattern(&self) -> String {
        "urn:iki:finding:{id}".to_string()
    }
}

// --- the finding endpoint (Source + the decision Sink) -----------------------

struct FindingEndpoint {
    roots: Roots,
    archive: Arc<Archive>,
}

#[async_trait]
impl Endpoint for FindingEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match inv.request.verb {
            Verb::Source => self.read(inv).await,
            Verb::Sink => self.decide(inv).await,
            other => Err(Error::Endpoint(format!(
                "finding does not support the {other:?} verb — a finding is never deleted: \
                 declining is how a human removes one, and the decline is the record"
            ))),
        }
    }

    fn name(&self) -> &str {
        "finding"
    }

    fn describe(&self) -> Description {
        finding_description()
    }
}

impl FindingEndpoint {
    fn id_binding(inv: &Invocation<'_>) -> Result<String> {
        inv.bindings
            .get("id")
            .map(str::to_string)
            .ok_or_else(|| Error::MissingArgument("id".to_string()))
    }

    fn load_required(&self, id: &str) -> Result<Annotation> {
        annotate::load_record(&self.archive, Family::Finding, id)?
            .ok_or_else(|| Error::NotFound(format!("browse: no finding `{}`", finding_iri(id))))
    }

    async fn read(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let id = Self::id_binding(inv)?;
        let mut finding = self.load_required(&id)?;
        granted(inv, &finding.repo)?;
        let current =
            annotate::current_content_for(inv, &self.roots, &finding.repo, &finding.target_ref())
                .await?;
        // The same state the queue listing shows, from the content already in
        // hand — one finding's face must not say `pending` where its file's
        // listing says `superseded`.
        let hash = current.hash().map(str::to_string);
        crate::supersede::mark(&self.archive, std::slice::from_mut(&mut finding), |_| {
            hash.clone()
        })?;
        crate::revision::mark(&self.archive, std::slice::from_mut(&mut finding))?;
        let line = annotate::refresh(&self.archive, &mut finding, &current)?;
        face_one(inv, &finding, line)
    }

    /// Sink: the human's answer. `decision=publish` promotes into the
    /// annotation family (this is the ONLY promotion path); `decision=decline`
    /// records that a person looked and said no; `decision=retract`
    /// withdraws the current answer. A second answer on a decided finding is
    /// a REVISION and must name the decision it revises (`revises=`).
    async fn decide(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let id = Self::id_binding(inv)?;
        let mut finding = self.load_required(&id)?;
        granted(inv, &finding.repo)?;
        let word = inv.inline_str("decision")?.trim().to_string();
        let outcome = match word.as_str() {
            PUBLISH => Outcome::Published,
            DECLINE => Outcome::Declined,
            RETRACT => Outcome::Retracted,
            other => {
                return Err(Error::InvalidArgument {
                    name: "decision".to_string(),
                    detail: format!(
                        "`{other}` is not a decision — one of: {PUBLISH}, {DECLINE}, {RETRACT}"
                    ),
                })
            }
        };
        let stated = |name: &str| {
            inv.inline_str(name)
                .ok()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        // The final rating: the human's choice, or the model's proposal
        // accepted unchanged. ⚠ The proposal on the finding is never touched
        // by either path. A retraction states none, and is refused one by
        // name rather than accepting a rating it would not record.
        let severity = match (outcome, stated("severity")) {
            (Outcome::Retracted, Some(chosen)) => {
                return Err(Error::InvalidArgument {
                    name: "severity".to_string(),
                    detail: format!(
                        "`severity={chosen}` rates a finding, and decision={RETRACT} withdraws                          an answer without giving one — drop `severity`"
                    ),
                })
            }
            (Outcome::Retracted, None) => None,
            (_, Some(chosen)) => {
                if !is_severity(&chosen) {
                    return Err(Error::InvalidArgument {
                        name: "severity".to_string(),
                        detail: format!(
                            "`{chosen}` is not a severity — one of: {}",
                            SEVERITIES.join(", ")
                        ),
                    });
                }
                Some(chosen)
            }
            (_, None) => Some(finding.severity.clone().ok_or_else(|| {
                Error::InvalidArgument {
                    name: "severity".to_string(),
                    detail: format!(
                        "the model proposed no severity for `{}`, so a decision must state one — \
                         one of: {}",
                        finding_iri(&id),
                        SEVERITIES.join(", ")
                    ),
                }
            })?),
        };
        // Why a decline, in one contract word. An EMPTY value is "omitted",
        // exactly as for `severity`: a form's unselected picker submits one,
        // and a publish button sharing that form must not be refused for it.
        // ⚠ A real word beside anything but a decline IS refused, by name —
        // it has no consumer, and an argument accepted and then ignored is
        // the failure that is invisible from the caller's side.
        let reason = match stated("reason") {
            Some(word) => {
                if outcome != Outcome::Declined {
                    return Err(Error::InvalidArgument {
                        name: "reason".to_string(),
                        detail: format!(
                            "`reason={word}` says why a finding was declined, and this \
                             decision is {} — drop `reason`, or use decision={DECLINE}",
                            word_of(outcome)
                        ),
                    });
                }
                if !is_decline_reason(&word) {
                    return Err(Error::InvalidArgument {
                        name: "reason".to_string(),
                        detail: format!(
                            "`{word}` is not a decline reason — one of: {}",
                            DECLINE_REASONS.join(", ")
                        ),
                    });
                }
                Some(word)
            }
            None => None,
        };
        let made = made_arg(inv)?;
        let revises_arg = stated("revises");
        let head = finding.history.last().cloned();
        let same_answer = |d: &Decision| {
            d.outcome == outcome
                && d.severity == severity
                && (reason.is_none() || reason == d.reason)
        };

        // ★ A decision is the RECORD, and a record is not overwritten. What
        // may follow one is a REVISION — a new node that names the one it
        // revises — and nothing else; see the module doc for every case.
        let revises = match (&revises_arg, &head) {
            (Some(named), None) => {
                return Err(Error::InvalidArgument {
                    name: "revises".to_string(),
                    detail: format!(
                        "`{}` has no decision to revise — `{named}` is not on file; drop \
                         `revises` to make the first decision",
                        finding_iri(&id)
                    ),
                })
            }
            (Some(named), Some(head)) if *named != head.iri => {
                // The second submit of a revision that already landed is the
                // double click of THIS path: a no-op, not an error.
                if head.revises.as_deref() == Some(named.as_str()) && same_answer(head) {
                    return self.answer(inv, finding);
                }
                return Err(Error::InvalidArgument {
                    name: "revises".to_string(),
                    detail: format!(
                        "`{named}` is not the current decision on `{}`{} — a revision names the \
                         decision it revises, and the current one is `{}` ({}). Revise that one.",
                        finding_iri(&id),
                        match finding.history.iter().any(|d| d.iri == *named) {
                            true => " (a later decision already revised it)",
                            false => " (it is not one of its decisions)",
                        },
                        head.iri,
                        on_file(head),
                    ),
                });
            }
            (Some(_), Some(head)) => {
                self.revisable(&id, head, outcome)?;
                Some(head.iri.clone())
            }
            (None, Some(head)) if head.outcome != Outcome::Retracted => {
                // A repeat of the SAME answer is accepted as a no-op (a
                // double-clicked button must not be an error, and promotion
                // is idempotent anyway); anything that would CHANGE the
                // answer on file is refused, naming it and the way to
                // revise. A repeat that states no reason does not
                // contradict one on file; a repeat that states a DIFFERENT
                // one (including a reason where none was recorded) does.
                if same_answer(head) {
                    return self.answer(inv, finding);
                }
                let name = match head.outcome == outcome && head.severity == severity {
                    true => "reason",
                    false => "decision",
                };
                return Err(Error::InvalidArgument {
                    name: name.to_string(),
                    detail: format!(
                        "`{}` was already {} — a decision is the record and is not \
                         overwritten. To REVISE it, name it: revises={} (both answers are \
                         kept).{}",
                        finding_iri(&id),
                        on_file(head),
                        head.iri,
                        match &head.minted {
                            Some(iri) => format!(
                                " A publication is revised only once the annotation it minted \
                                 (`{iri}`) is deleted."
                            ),
                            None => String::new(),
                        },
                    ),
                });
            }
            (None, Some(head)) => {
                // The current answer was withdrawn, so the finding is
                // undecided again and takes a decision like any pending one;
                // the chain stays one chain by linking the retraction. A
                // second retraction is the double click of the first.
                if outcome == Outcome::Retracted {
                    return self.answer(inv, finding);
                }
                Some(head.iri.clone())
            }
            (None, None) => {
                if outcome == Outcome::Retracted {
                    return Err(Error::InvalidArgument {
                        name: "decision".to_string(),
                        detail: format!(
                            "`{}` has no decision to retract — it is undecided",
                            finding_iri(&id)
                        ),
                    });
                }
                None
            }
        };

        // Pipeline citizenship: a piped value is the human's reason.
        let note = stated("content");
        let at = inv.now().map(|t| iso8601(t.as_millis()));
        let minted = match (outcome, &severity) {
            (Outcome::Published, Some(severity)) => {
                Some(promote(&self.archive, &finding, severity)?)
            }
            _ => None,
        };
        let n = finding.history.len() as u32 + 1;
        let decision = Decision {
            iri: annotate::revision_iri(&id, n),
            outcome,
            severity,
            at,
            revises,
            made,
            confirmed: false,
            burst: None,
            note,
            reason,
            minted,
        };
        annotate::append_decision(&self.archive, &finding.iri(), &decision)?;
        finding.history.push(decision);
        finding.settle();
        self.answer(inv, finding)
    }

    /// Whether the CURRENT decision may be revised to `outcome` — refused,
    /// naming the way, when not.
    ///
    /// ★ A PUBLICATION stands while the annotation it minted does. Revising
    /// it is "delete the annotation (`Delete urn:iki:annotation:{id}`, the
    /// annotation family's own visible act), then revise" — never a decision
    /// Sink that reaches into the annotation family and removes a published
    /// note as a side effect. Once the annotation is gone, a publish may be
    /// revised to anything, including a fresh publish that re-mints it.
    fn revisable(&self, id: &str, head: &Decision, outcome: Outcome) -> Result<()> {
        if head.outcome == Outcome::Retracted && outcome == Outcome::Retracted {
            return Err(Error::InvalidArgument {
                name: "decision".to_string(),
                detail: format!(
                    "`{}` is already retracted — there is no answer to withdraw",
                    finding_iri(id)
                ),
            });
        }
        if let Some(minted) = &head.minted {
            let still = match Family::split(minted) {
                Some((Family::Annotation, aid)) => annotate::load_annotation(&self.archive, aid)?,
                _ => None,
            };
            if still.is_some() {
                return Err(Error::InvalidArgument {
                    name: "revises".to_string(),
                    detail: format!(
                        "`{}` was published as `{minted}`, and a publication stands while its \
                         annotation does — Delete `{minted}` first (the annotation family's own \
                         act), then revise",
                        finding_iri(id)
                    ),
                });
            }
        }
        Ok(())
    }

    /// The finding, settled for the faces (confirmation), as an ack.
    fn answer(&self, inv: &Invocation<'_>, mut finding: Annotation) -> Result<Representation> {
        crate::revision::mark(&self.archive, std::slice::from_mut(&mut finding))?;
        ack(inv, &finding)
    }
}

/// The decision word for an outcome, as the Sink spells it.
fn word_of(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Published => PUBLISH,
        Outcome::Declined => DECLINE,
        Outcome::Retracted => RETRACT,
    }
}

/// What is on file, in the words a refusal quotes: `declined as `major` (no
/// reason stated) by a human at …` and its kin.
fn on_file(d: &Decision) -> String {
    let mut out = d.outcome.label().to_string();
    if let Some(severity) = &d.severity {
        out.push_str(&format!(" as `{severity}`"));
    }
    match &d.reason {
        Some(word) => out.push_str(&format!(" ({word})")),
        None if d.outcome == Outcome::Declined => out.push_str(" (no reason stated)"),
        None => {}
    }
    out.push_str(" by a human");
    if let Some(at) = &d.at {
        out.push_str(&format!(" at {at}"));
    }
    if let Some(iri) = &d.minted {
        out.push_str(&format!(" (`{iri}`)"));
    }
    out
}

/// `made=` and `batch=`: how the decision was made, when the caller says.
/// Omitted = not recorded (and so open to the burst rule). `batch=` is the
/// batch's group key, required beside `made=batch` and refused beside
/// `made=single` — an argument accepted and ignored is invisible.
fn made_arg(inv: &Invocation<'_>) -> Result<Option<Made>> {
    let stated = |name: &str| {
        inv.inline_str(name)
            .ok()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let refuse = |name: &str, detail: String| Error::InvalidArgument {
        name: name.to_string(),
        detail,
    };
    match (stated("made").as_deref(), stated("batch")) {
        (None, None) => Ok(None),
        (None, Some(key)) => Err(refuse(
            "batch",
            format!("`batch={key}` names a batch's group, and only beside made=batch"),
        )),
        (Some("single"), None) => Ok(Some(Made::Single)),
        (Some("single"), Some(key)) => Err(refuse(
            "batch",
            format!("`batch={key}` names a batch's group, and this decision was made=single"),
        )),
        (Some("batch"), Some(key)) => Ok(Some(Made::Batch(key))),
        (Some("batch"), None) => Err(refuse(
            "batch",
            "made=batch states that a decision was one of a batch; batch= names the batch's \
             group key (a group= proposal's `key`)"
                .to_string(),
        )),
        (Some(other), _) => Err(refuse(
            "made",
            format!(
                "`{other}` is not how a decision is made — one of: {}",
                MADE_WORDS.join(", ")
            ),
        )),
    }
}

/// The decision's acknowledgement, in the caller's face. The plain form is the
/// IRI the decision produced — the minted annotation on a publish, the finding
/// itself on a decline — so either pipes straight into a Source.
fn ack(inv: &Invocation<'_>, finding: &Annotation) -> Result<Representation> {
    match inv.inline_str("as").unwrap_or("text/plain") {
        t if t.starts_with("application/json") => Ok(repr(
            "application/json",
            annotate::annotation_json(finding, None).to_string(),
        )),
        t if t.starts_with("text/html") => Ok(repr_utf8("text/html", one_html(finding, None))),
        _ => Ok(repr_utf8(
            "text/plain",
            match &finding.decision {
                Some(Decision {
                    minted: Some(iri), ..
                }) => iri.clone(),
                _ => finding.iri(),
            },
        )),
    }
}

/// Promote one finding into the annotation family — the ONE path in, and the
/// one place `urn:cap:annotate` buys anything.
///
/// The minted annotation KEEPS the finding's id, so `urn:iki:finding:{id}` and
/// `urn:iki:annotation:{id}` are the same claim before and after a human said
/// yes: publishing twice is idempotent rather than duplicating, and the pair is
/// legible without a join. `prov:wasDerivedFrom` says it in the graph as well,
/// which is what a query needs to walk from a published note back to the
/// proposal it was rated against.
///
/// The annotation carries the HUMAN'S FINAL severity and keeps
/// `dcterms:creator` (the model) and `oa:motivatedBy oa:assessing`: a published
/// finding is still a machine claim — a human vouched for it, they did not
/// write it.
fn promote(archive: &Archive, finding: &Annotation, severity: &str) -> Result<String> {
    let mut annotation = finding.clone();
    annotation.family = Family::Annotation;
    // The finding carried no `oa:motivatedBy` (its domain would have typed it
    // into this family); the annotation it becomes carries the machine one.
    annotation.motivation = Some(annotate::MOTIVATION_REVIEW.to_string());
    annotation.severity = Some(severity.to_string());
    annotation.derived_from = Some(finding.iri());
    annotation.decision = None;
    annotation.history = Vec::new();
    annotate::store_annotation(archive, &annotation)?;
    Ok(annotation.iri())
}

// --- the queue listing ------------------------------------------------------

struct FindingsEndpoint {
    roots: Roots,
    archive: Arc<Archive>,
}

#[async_trait]
impl Endpoint for FindingsEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "browse-findings does not support the {:?} verb",
                inv.request.verb
            )));
        }
        let (repo, _root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        let rel = path_binding(inv)?;
        let filter = (!rel.is_empty()).then_some(rel.as_str());
        let state = inv.inline_str("state").unwrap_or("pending").to_string();
        if !STATES.contains(&state.as_str()) {
            return Err(Error::InvalidArgument {
                name: "state".to_string(),
                detail: format!(
                    "`{state}` is not a queue state — one of: {}",
                    STATES.join(", ")
                ),
            });
        }
        // `summary=` widens the json face from the rows to an object that
        // also carries a count — a NEW shape under a NEW argument, so the
        // array every existing consumer reads is byte-identical without it.
        let summary = match inv.inline_str("summary") {
            Ok(word) if SUMMARIES.contains(&word) => Some(word.to_string()),
            Ok(other) => {
                return Err(Error::InvalidArgument {
                    name: "summary".to_string(),
                    detail: format!(
                        "`{other}` is not a summary — one of: {}",
                        SUMMARIES.join(", ")
                    ),
                })
            }
            Err(_) => None,
        };
        // `group=` (ledger #506) is a different read of the SAME pending set:
        // it narrows nothing new and decides nothing, so it rides this face
        // (one binding, one capability, one supersession pass) rather than a
        // sibling IRI. It is refused beside anything that would ask it to
        // group something other than pending, or to be a second shape at once
        // — an argument accepted and then ignored is the failure invisible
        // from the caller's side.
        let group = match inv.inline_str("group") {
            Ok(word) => Some(crate::group::kind_arg(word)?),
            Err(_) => None,
        };
        if let Some(kind) = group {
            let conflict = |name: &str, detail: String| Error::InvalidArgument {
                name: name.to_string(),
                detail,
            };
            if state != "pending" {
                return Err(conflict(
                    "state",
                    format!(
                        "`group={kind}` proposes groups of PENDING findings only — drop \
                         `state={state}`, or drop `group`"
                    ),
                ));
            }
            if let Some(word) = &summary {
                return Err(conflict(
                    "summary",
                    format!(
                        "`group={kind}` is its own shape and already counts every kind — drop \
                         `summary={word}`"
                    ),
                ));
            }
            if inv
                .inline_str("as")
                .is_ok_and(|t| t.starts_with("text/turtle"))
            {
                return Err(conflict(
                    "as",
                    format!(
                        "`group={kind}` has json, html and plain faces — a group is a proposal, \
                         not a graph; the members' graphs are the pending listing's turtle"
                    ),
                ));
            }
        }
        let mut all = annotate::list_findings(&self.archive, repo, filter)?;
        // The file grain is read BEFORE the state filter: a pending-only
        // listing still says how many declines the file carries, which is
        // the reading that makes a recurring claim legible.
        let declined = DeclinedSummary::of(&all);
        // ★ Supersession (ledger #504) is decided BEFORE the state filter,
        // because it is what `pending` now means. It needs each undecided
        // file finding's CURRENT content hash — which the drift pass below
        // fetches anyway, so it is fetched once here and the same map is
        // handed on: a pending listing reads exactly the files it read
        // before. A listing of decided findings needs none of it (a decision
        // never changes state), unless the caller asked for the counts.
        let supersession = !matches!(state.as_str(), "published" | "declined")
            || matches!(summary.as_deref(), Some("states" | "unconfirmed"));
        let mut contents = std::collections::BTreeMap::new();
        if supersession {
            let undecided_files: Vec<&Annotation> = all
                .iter()
                .filter(|f| {
                    f.decision.is_none() && matches!(f.target_ref(), annotate::TargetRef::File(_))
                })
                .collect();
            annotate::fetch_contents(inv, &self.roots, repo, undecided_files, &mut contents)
                .await?;
            crate::supersede::mark(&self.archive, &mut all, |f| {
                contents
                    .get(&f.target_iri)
                    .and_then(|c| c.hash())
                    .map(str::to_string)
            })?;
        }
        let states = supersession.then(|| StateCounts::of(&all));
        // Confirmation (ledger #653), once over everything loaded — the rows,
        // the declined twins a group names, and every recurrence mark.
        let bursts = crate::revision::Bursts::of(&self.archive)?;
        bursts.apply(&mut all);
        // The walk (`summary=unconfirmed`) reads every state, so it is read
        // here, before the state filter, and rendered in the face asked for.
        let face = inv
            .inline_str("as")
            .unwrap_or("application/json")
            .to_string();
        let walk = (summary.as_deref() == Some("unconfirmed")).then(|| {
            let view = crate::revision::Unconfirmed::of(&all, &bursts);
            match face.as_str() {
                t if t.starts_with("text/html") => Walk::Text(view.html()),
                t if t.starts_with("text/plain") => Walk::Text(view.plain()),
                _ => Walk::Json(view.json()),
            }
        });
        // The declined findings `recurrence` looks its twins up in — already
        // loaded, before the state filter drops them.
        let declined_records: Vec<Annotation> = match group {
            Some(_) => all
                .iter()
                .filter(|f| f.state() == Some("declined"))
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        let findings: Vec<Annotation> = all
            .into_iter()
            .filter(|f| state == "all" || f.state() == Some(state.as_str()))
            .collect();
        let mut rows =
            annotate::reconcile_findings(inv, &self.archive, &self.roots, repo, findings, contents)
                .await?;
        annotate::sort_finding_rows(&mut rows);
        if let Some(kind) = group {
            let groups = crate::group::groups(kind, &rows, &declined_records);
            let counts = crate::group::counts(&rows, &declined_records);
            return Ok(match inv.inline_str("as").unwrap_or("application/json") {
                t if t.starts_with("text/html") => repr_utf8(
                    "text/html",
                    crate::group::html(repo, &rel, kind, &groups, &counts),
                ),
                t if t.starts_with("text/plain") => {
                    repr_utf8("text/plain", crate::group::plain(kind, &groups, &counts))
                }
                _ => repr(
                    "application/json",
                    crate::group::json(repo, filter, kind, &groups, &counts).to_string(),
                ),
            });
        }
        // The one line a PENDING listing owes its reader: how many findings
        // are not in it because they were superseded. A suppression nobody
        // can measure is how an orphan rate hid (ledger #488).
        let hidden = match (state.as_str(), &states) {
            ("pending", Some(counts)) => counts.hidden_words(),
            _ => None,
        };

        match inv.inline_str("as").unwrap_or("application/json") {
            t if t.starts_with("text/html") => Ok(repr_utf8(
                "text/html",
                listing_html(
                    repo,
                    &rel,
                    &state,
                    &rows,
                    &declined,
                    hidden.as_deref(),
                    walk.as_ref().and_then(Walk::text),
                ),
            )),
            t if t.starts_with("text/turtle") => {
                let findings: Vec<Annotation> = rows.into_iter().map(|(f, _)| f).collect();
                Ok(repr(
                    "text/turtle",
                    annotate::annotation_turtle_document(&findings),
                ))
            }
            t if t.starts_with("text/plain") => {
                let mut out = plain(&rows, &declined, hidden.as_deref());
                if let Some(text) = walk.as_ref().and_then(Walk::text) {
                    out.push('\n');
                    out.push_str(text);
                }
                Ok(repr_utf8("text/plain", out))
            }
            _ => {
                let rows: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|(f, line)| annotate::annotation_json(f, *line))
                    .collect();
                let json = match (summary.as_deref(), &states) {
                    (Some("declined"), _) => serde_json::json!({
                        "repo": repo,
                        "path": filter,
                        "state": state,
                        "rows": rows,
                        "declined": declined.json(),
                    }),
                    (Some("states"), Some(counts)) => serde_json::json!({
                        "repo": repo,
                        "path": filter,
                        "state": state,
                        "rows": rows,
                        "states": counts.json(),
                    }),
                    (Some("unconfirmed"), _) => serde_json::json!({
                        "repo": repo,
                        "path": filter,
                        "state": state,
                        "rows": rows,
                        "unconfirmed": match walk {
                            Some(Walk::Json(v)) => v,
                            _ => serde_json::Value::Null,
                        },
                    }),
                    _ => serde_json::Value::Array(rows),
                };
                Ok(repr("application/json", json.to_string()))
            }
        }
    }

    fn name(&self) -> &str {
        "browse-findings"
    }

    fn describe(&self) -> Description {
        findings_description()
    }
}

// --- faces ------------------------------------------------------------------

/// The `summary=unconfirmed` walk, rendered in the face the listing serves.
enum Walk {
    Json(serde_json::Value),
    Text(String),
}

impl Walk {
    fn text(&self) -> Option<&str> {
        match self {
            Walk::Text(text) => Some(text),
            Walk::Json(_) => None,
        }
    }
}

fn face_one(
    inv: &Invocation<'_>,
    finding: &Annotation,
    line: Option<u64>,
) -> Result<Representation> {
    match inv.inline_str("as").unwrap_or("text/plain") {
        t if t.starts_with("application/json") => Ok(repr(
            "application/json",
            annotate::annotation_json(finding, line).to_string(),
        )),
        t if t.starts_with("text/turtle") => Ok(repr(
            "text/turtle",
            annotate::annotation_turtle_document(std::slice::from_ref(finding)),
        )),
        t if t.starts_with("text/html") => Ok(repr_utf8("text/html", one_html(finding, line))),
        _ => Ok(repr_utf8("text/plain", plain_row(finding, line))),
    }
}

pub(crate) fn plain_row(finding: &Annotation, line: Option<u64>) -> String {
    let mut out = String::new();
    if !finding.rel.is_empty() {
        out.push_str(&finding.rel);
        out.push(' ');
    }
    if let Some(n) = line {
        out.push_str(&format!("L{n} "));
    }
    out.push_str(&format!(
        "[{}] {}",
        finding.state().unwrap_or("pending"),
        finding.severity.as_deref().unwrap_or("unrated"),
    ));
    if let Some(decision) = &finding.decision {
        out.push_str(&format!(
            " -> {}",
            decision.severity.as_deref().unwrap_or("unrated")
        ));
        if let Some(reason) = &decision.reason {
            out.push_str(&format!(" ({reason})"));
        }
        if !decision.confirmed {
            out.push_str(" [unconfirmed]");
        }
    }
    if finding.orphaned {
        out.push_str(" [orphaned]");
    } else if finding.reanchored {
        out.push_str(" [re-anchored]");
    }
    if let Some(prior) = finding.prior.as_ref().filter(|p| p.active().is_some()) {
        out.push_str(&format!(" [{}]", prior.words()));
    }
    out.push_str(&format!(" \"{}\" -- {}", finding.exact, finding.body));
    out
}

fn plain(
    rows: &[(Annotation, Option<u64>)],
    declined: &DeclinedSummary,
    hidden: Option<&str>,
) -> String {
    let mut out = format!("--- findings ({}) ---\n{NOT_A_GATE}", rows.len());
    if let Some(line) = hidden {
        out.push('\n');
        out.push_str(line);
    }
    if let Some(line) = declined.words() {
        out.push('\n');
        out.push_str(&line);
    }
    for (finding, line) in rows {
        out.push('\n');
        out.push_str(&plain_row(finding, *line));
    }
    out
}

/// The FILE-GRAIN view of what a human has already said no to: how many
/// declined findings one file carries, by the quote they declined (ledger
/// #475).
///
/// ★ The mark on a row (`Annotation::prior`) is row-shaped; the recurrence it
/// answers is file-shaped. A manifest with reasoning comments or a vocabulary
/// draws the same misreading at every new content hash, on new lines as well
/// as old, and no line-keyed mark can show "this file has 14 declined findings
/// of this shape". This is what a reader — and the host's queue page, later —
/// says it with. Computed from the findings already loaded for the listing,
/// before the `state=` filter narrows them, so it costs nothing extra.
struct DeclinedSummary {
    /// Declined findings on the file, in total.
    count: usize,
    /// How many of them carry each of [`DECLINE_REASONS`], in contract order,
    /// and last how many state none. ★ This is the number ledger #483 wants:
    /// "this file's declines are mostly `restates`" is the disclosure shape,
    /// measured rather than remembered.
    by_reason: [usize; DECLINE_REASONS.len()],
    unstated: usize,
    /// Per distinct quote, in triage order of first appearance: the quote,
    /// how many declines it carries, the latest decision date among them, and
    /// the declined findings' IRIs.
    quotes: Vec<DeclinedQuote>,
}

struct DeclinedQuote {
    exact: String,
    count: usize,
    latest: Option<String>,
    findings: Vec<String>,
}

impl DeclinedSummary {
    fn of(findings: &[Annotation]) -> Self {
        let mut quotes: Vec<DeclinedQuote> = Vec::new();
        let mut count = 0;
        let mut by_reason = [0; DECLINE_REASONS.len()];
        let mut unstated = 0;
        for finding in findings {
            let Some(decision) = &finding.decision else {
                continue;
            };
            if decision.outcome != Outcome::Declined {
                continue;
            }
            count += 1;
            match decision
                .reason
                .as_deref()
                .and_then(|word| DECLINE_REASONS.iter().position(|w| *w == word))
            {
                Some(i) => by_reason[i] += 1,
                None => unstated += 1,
            }
            match quotes.iter_mut().find(|q| q.exact == finding.exact) {
                Some(quote) => {
                    quote.count += 1;
                    if decision.at > quote.latest {
                        quote.latest = decision.at.clone();
                    }
                    quote.findings.push(finding.iri());
                }
                None => quotes.push(DeclinedQuote {
                    exact: finding.exact.clone(),
                    count: 1,
                    latest: decision.at.clone(),
                    findings: vec![finding.iri()],
                }),
            }
        }
        // Most-declined quote first; ties keep triage order.
        quotes.sort_by_key(|quote| std::cmp::Reverse(quote.count));
        for quote in &mut quotes {
            quote.findings.sort();
        }
        DeclinedSummary {
            count,
            by_reason,
            unstated,
            quotes,
        }
    }

    /// The per-reason counts as a fixed-shape list: every word of
    /// [`DECLINE_REASONS`] in contract order, zeros included, then `null` for
    /// the declines that state none — so a consumer renders it without
    /// knowing the words, and no word can collide with "unstated".
    fn reasons_json(&self) -> serde_json::Value {
        let mut out: Vec<serde_json::Value> = DECLINE_REASONS
            .iter()
            .zip(self.by_reason)
            .map(|(word, count)| serde_json::json!({"reason": word, "count": count}))
            .collect();
        out.push(serde_json::json!({"reason": null, "count": self.unstated}));
        serde_json::Value::Array(out)
    }

    /// The non-zero reasons in words — `restates 3, misread 1, no reason 2`.
    fn reasons_words(&self) -> String {
        let mut parts: Vec<String> = DECLINE_REASONS
            .iter()
            .zip(self.by_reason)
            .filter(|(_, n)| *n > 0)
            .map(|(word, n)| format!("{word} {n}"))
            .collect();
        if self.unstated > 0 {
            parts.push(format!("no reason {}", self.unstated));
        }
        parts.join(", ")
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "count": self.count,
            "reasons": self.reasons_json(),
            "quotes": self.quotes.iter().map(|q| serde_json::json!({
                "exact": q.exact,
                "count": q.count,
                "latest_decided_at": q.latest,
                "findings": q.findings,
            })).collect::<Vec<_>>(),
        })
    }

    /// One line for the plain face — `None` when the file carries no decline,
    /// so a listing with nothing to say adds no line.
    fn words(&self) -> Option<String> {
        match self.count {
            0 => None,
            n => Some(format!(
                "{n} declined finding{} on this file, on {} distinct quote{} ({})",
                if n == 1 { "" } else { "s" },
                self.quotes.len(),
                if self.quotes.len() == 1 { "" } else { "s" },
                self.reasons_words(),
            )),
        }
    }

    fn html(&self) -> String {
        let Some(words) = self.words() else {
            return String::new();
        };
        let mut out = format!(
            "<div class=\"browse-findings-declined\"><p>{}</p><ul>",
            esc(&words)
        );
        for quote in &self.quotes {
            out.push_str(&format!(
                "<li>{}× <code>{}</code>{}</li>",
                quote.count,
                esc(&quote.exact),
                quote
                    .latest
                    .as_deref()
                    .map(|at| format!(" · last {}", esc(at.get(..10).unwrap_or(at))))
                    .unwrap_or_default(),
            ));
        }
        out.push_str("</ul></div>");
        out
    }
}

/// How many findings are in each state, over the whole listing (repo or
/// file) BEFORE the `state=` filter — and, per file, how many are pending
/// against how many superseded (ledger #504). The observability half of
/// supersession: `summary=states` on the json face, one line on a pending
/// listing's plain and html faces.
struct StateCounts {
    pending: usize,
    superseded: usize,
    published: usize,
    declined: usize,
    /// Per path with anything undecided, in path order: pending, superseded,
    /// and the pass that superseded them (the newest current pass — one per
    /// file, since the reading is per file).
    files: std::collections::BTreeMap<String, FileStates>,
}

#[derive(Default)]
struct FileStates {
    pending: usize,
    superseded: usize,
    superseded_by: Option<String>,
}

impl StateCounts {
    fn of(findings: &[Annotation]) -> Self {
        let mut counts = StateCounts {
            pending: 0,
            superseded: 0,
            published: 0,
            declined: 0,
            files: std::collections::BTreeMap::new(),
        };
        for finding in findings {
            match finding.state() {
                Some("pending") => {
                    counts.pending += 1;
                    counts.files.entry(finding.rel.clone()).or_default().pending += 1;
                }
                Some("superseded") => {
                    counts.superseded += 1;
                    let file = counts.files.entry(finding.rel.clone()).or_default();
                    file.superseded += 1;
                    file.superseded_by.clone_from(&finding.superseded_by);
                }
                Some("published") => counts.published += 1,
                Some("declined") => counts.declined += 1,
                _ => {}
            }
        }
        counts
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "pending": self.pending,
            "superseded": self.superseded,
            "published": self.published,
            "declined": self.declined,
            "files": self.files.iter().map(|(path, file)| serde_json::json!({
                "path": path,
                "pending": file.pending,
                "superseded": file.superseded,
                "superseded_by": file.superseded_by,
            })).collect::<Vec<_>>(),
        })
    }

    /// The line a pending listing carries when it is not showing everything
    /// undecided — `None` when nothing was superseded.
    fn hidden_words(&self) -> Option<String> {
        match self.superseded {
            0 => None,
            n => Some(format!(
                "{n} superseded finding{s} not listed: the code {they} about changed, and the \
                 file's current review does not carry {them} (state=superseded lists {them})",
                s = if n == 1 { "" } else { "s" },
                they = if n == 1 { "it was" } else { "they were" },
                them = if n == 1 { "it" } else { "them" },
            )),
        }
    }
}

fn one_html(finding: &Annotation, line: Option<u64>) -> String {
    format!(
        "<div class=\"browse\">{}<p class=\"browse-findings-note\">{NOT_A_GATE}</p>\
         <div class=\"browse-annotations\">{}</div></div>",
        crumbs_html(&finding.repo, &finding.rel),
        annotate::annotation_card_html(finding, line, false),
    )
}

fn listing_html(
    repo: &str,
    rel: &str,
    state: &str,
    rows: &[(Annotation, Option<u64>)],
    declined: &DeclinedSummary,
    hidden: Option<&str>,
    walk: Option<&str>,
) -> String {
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&crumbs_html(repo, rel));
    out.push_str(&format!(
        "<p class=\"browse-findings-note\">{NOT_A_GATE}</p>\
         <nav class=\"browse-findings-states\">"
    ));
    for option in STATES {
        let current = match option == state {
            true => " browse-findings-state-current",
            false => "",
        };
        out.push_str(&format!(
            "<button class=\"browse-findings-state{current}\" hx-get=\"/k/source {iri} \
             as=text/html state={option}\" hx-target=\"#browse\" \
             hx-swap=\"innerHTML\">{option}</button>",
            iri = findings_iri(repo, rel),
        ));
    }
    out.push_str("</nav>");
    if let Some(line) = hidden {
        out.push_str(&format!(
            "<p class=\"browse-findings-superseded\">{}</p>",
            esc(line)
        ));
    }
    // The file grain, on a FILE's listing only: a repo-wide page would be
    // summing declines across files, which is the join this count exists
    // not to hide behind.
    if !rel.is_empty() {
        out.push_str(&declined.html());
    }
    if let Some(walk) = walk {
        out.push_str(walk);
    }
    if rows.is_empty() {
        out.push_str(&format!(
            "<p class=\"browse-findings-empty\">No {} findings{}.</p>",
            esc(state),
            match rel.is_empty() {
                true => String::new(),
                false => format!(" for {}", esc(rel)),
            }
        ));
    }
    out.push_str("<div class=\"browse-annotations\">");
    for (finding, line) in rows {
        out.push_str(&annotate::annotation_card_html(
            finding,
            *line,
            rel.is_empty(),
        ));
    }
    out.push_str("</div></div>");
    out
}

// --- descriptions -----------------------------------------------------------

fn finding_description() -> Description {
    Description::new("finding")
        .title("A machine review finding awaiting a human (the publish gate)")
        .summary(
            "One pending review finding — urn:iki:finding:{id}, a prov:Entity in the shared \
             store, NOT an oa:Annotation. A review pass mints findings and nothing else; \
             Sink with decision=publish is the ONLY path into the urn:iki:annotation: \
             family, and it needs urn:cap:annotate. decision=decline keeps the finding as \
             a record that a human looked and said no (with reason= — one contract word: \
             misread, restates, no-issue, wont-fix or duplicate — and the piped note, both \
             optional) — it is never discarded. A decision is never overwritten: a second \
             one is refused unless it names the CURRENT decision with revises=, and then it \
             is a REVISION — a new decision node linked to the old (dcterms:replaces), both \
             kept. decision=retract (only as a revision) withdraws the current answer, so \
             the finding is undecided again; a publication is revised only after the \
             annotation it minted is deleted. made=single|batch (with batch=<group key>) \
             records how a decision was made. The \
             finding carries the MODEL'S proposed sh:resultSeverity; the decision node \
             carries the human's final one, so both survive and calibration stays a query. \
             Reads run the annotation layer's drift pass (ik:reanchored / ik:orphaned). \
             text/plain (default) is one triage line; as=application/json the full row \
             including both ratings, the current decision, every decision (decisions) and \
             each one's computed confirmed flag; as=text/html the card with its decision \
             form; as=text/turtle the finding and its decision graph.",
        )
        .verb(Verb::Meta)
        .action(
            ActionSpec::new(Verb::Source)
                .summary("read one finding, re-anchored against the target's current content")
                .requires(CAP_WILDCARD)
                .input(
                    ArgSpec::new("id").binding().class(XSD_STRING).summary(
                        "the finding id (derived from the pass, the anchor and the quote)",
                    ),
                )
                .input(
                    ArgSpec::new("as")
                        .optional()
                        .class(XSD_STRING)
                        .summary("the face to render")
                        .one_of(["text/plain", "application/json", "text/html", "text/turtle"])
                        .default_value("text/plain"),
                )
                .output("text/plain;charset=utf-8")
                .output("application/json")
                .output("text/html;charset=utf-8")
                .output("text/turtle"),
        )
        .action(
            ActionSpec::new(Verb::Sink)
                .summary(
                    "the human's answer: publish (mints the annotation), decline (keeps the \
                     finding as a record), or retract (withdraws the current answer). An \
                     identical repeat is a no-op; anything that would CHANGE a recorded \
                     decision is refused unless it names that decision with revises=, which \
                     makes it a revision and keeps both.",
                )
                // Publishing MINTS into the annotation family; declining writes
                // the record of a holder's decision. Both are this authority.
                .requires(CAP_ANNOTATE)
                // The decision re-anchors against the target, which is a read
                // through the kernel — a capability that cannot read the
                // target cannot publish a finding about it.
                .requires(CAP_WILDCARD)
                .input(
                    ArgSpec::new("id")
                        .binding()
                        .class(XSD_STRING)
                        .summary("the finding id"),
                )
                .input(
                    ArgSpec::new("decision")
                        .class(XSD_STRING)
                        .summary(
                            "publish: mint the annotation (urn:cap:annotate). decline: record \
                             that a human looked and said no — the finding is kept, so a \
                             re-run knows. retract: withdraw the current decision (with \
                             revises=, or a repeat of a retraction) — the finding is \
                             undecided again and a retracted decline marks nothing.",
                        )
                        .one_of([PUBLISH, DECLINE, RETRACT]),
                )
                .input(
                    ArgSpec::new("severity")
                        .class(XSD_STRING)
                        .optional()
                        .summary(
                            "the human's FINAL rating. Omitted = accept the model's proposal \
                             unchanged (required when the model proposed none). ⚠ It never \
                             overwrites the proposal: both are stored, on different nodes. \
                             Refused beside decision=retract, which rates nothing.",
                        )
                        .one_of(SEVERITIES),
                )
                .input(
                    ArgSpec::new("reason")
                        .class(XSD_STRING)
                        .optional()
                        .summary(decline_reason_summary())
                        .one_of(DECLINE_REASONS),
                )
                .input(
                    ArgSpec::new("revises")
                        .class(XSD_STRING)
                        .optional()
                        .summary(
                            "the IRI of the decision this one REVISES — required to change a \
                             decided finding's answer, and it must be the finding's CURRENT \
                             decision (decision.iri on its json row). The new decision links \
                             it and both are kept. Refused on an undecided finding, and on a \
                             publication whose minted annotation still exists (delete that \
                             first).",
                        ),
                )
                .input(
                    ArgSpec::new("made")
                        .class(XSD_STRING)
                        .optional()
                        .summary(
                            "how the decision was made, recorded on it: single (one finding, \
                             one decision) or batch (one of many decided together; name the \
                             batch's group key with batch=). Omitted = not recorded — and a \
                             wordless decline without it is read as unconfirmed when it falls \
                             in a burst of declines inside one second.",
                        )
                        .one_of(MADE_WORDS),
                )
                .input(ArgSpec::new("batch").class(XSD_STRING).optional().summary(
                    "the batch's group key (a group= proposal's key) — required with \
                             made=batch, refused otherwise",
                ))
                .input(
                    ArgSpec::new("content")
                        .class(XSD_STRING)
                        .optional()
                        .summary(
                            "the human's reason, by pipe or request body — kept on a publish, \
                             a decline and a retraction alike",
                        ),
                )
                .input(
                    ArgSpec::new("as")
                        .optional()
                        .class(XSD_STRING)
                        .summary("the acknowledgement face")
                        .one_of(["text/plain", "application/json", "text/html"])
                        .default_value("text/plain"),
                )
                .output("text/plain;charset=utf-8")
                .output("application/json")
                .output("text/html;charset=utf-8"),
        )
}

/// `repo` is not an ArgSpec: every advertised row fixes the root in its
/// pattern (see `crate::bind_family`); the binding is grammar-injected.
pub(crate) fn findings_description() -> Description {
    Description::new("browse-findings")
        .title("The review queue: findings awaiting a human")
        .summary(
            "Every machine review finding on one file (urn:repo:{repo}:findings:{path}) or \
             on the whole repo (path omitted), in TRIAGE order — severity first, then path \
             and position. ⚠ A queue, not a gate: nothing here blocks a commit, a push or a \
             merge, and nothing reaches the annotation family until a human publishes it \
             (Sink urn:iki:finding:{id}). state= narrows to pending (the default), \
             superseded, published, declined, or all. ★ superseded is an UNDECIDED \
             finding its file's current review does not stand behind — the code it was \
             about changed, and no pass over the file's current content (else the most \
             recently derived pass) minted or carried it. It is computed on every read, \
             never stored, so a revert makes the older pass current again and its \
             findings pending again; it asserts no human judgment, never feeds the \
             declined-twin mark, and a human may still publish or decline it. Decided \
             findings and PR-page findings are never superseded. Each row carries \
             superseded_by (the pass, else null). Each read runs the annotation layer's drift pass, \
             so a finding whose file moved is re-anchored (ik:reanchored) and one whose \
             quote is gone is flagged (ik:orphaned) rather than dropped. ★ A finding \
             minted where a like claim was already DECLINED on the same file carries that \
             decision on its row (prior_decision: the declined finding, its date, its \
             reason word and its note — the twin's CURRENT decision, absent once it is \
             retracted or reversed), so the second decision is one click. ★ Every decision \
             and prior_decision carries a computed confirmed flag: false for a decline \
             with no reason word made in a batch (made=batch) or, with no recorded \
             provenance, in a burst of three or more declines inside one second; \
             summary=unconfirmed widens the json face to {repo, path, state, rows, \
             unconfirmed: {count, steered, groups: [{by, key, first_decided_at, size, \
             count, declines: [row + pending]}]}} — the unconfirmed declines that still \
             steer a pending finding, by burst or batch, oldest first, for a human to \
             confirm, retract or reverse; the html and plain faces say the same. \
             summary=declined widens the json face to {repo, path, state, rows, declined: \
             {count, reasons: [{reason, count}], quotes: [{exact, count, latest_decided_at, \
             findings}]}} — how many declines the file carries, by reason word (every word \
             in contract order, then reason null for none stated) and by the quote they \
             declined — and the html and plain faces of a file's \
             listing say the same in words. summary=states widens it instead to {repo, \
             path, state, rows, states: {pending, superseded, published, declined, files: \
             [{path, pending, superseded, superseded_by}]}} — the count in each state \
             whatever state= shows, and per file what is pending against what was \
             superseded; a pending listing's html and plain faces say how many superseded \
             findings they do not list. ★ group=<kind> PROPOSES batches of pending \
             findings for one human decision each — recurrence, near-duplicate, \
             comment-shape or file (see the group input) — each with a suggested reason \
             word from the finding Sink's reason one_of, never applied: nothing here \
             decides anything, and a host fans a batch out to the finding Sink one call \
             per member. \
             application/json (default) is the structured rows carrying BOTH ratings; \
             as=text/html the queue page with each card's decision form; as=text/plain a \
             triage digest; as=text/turtle the findings and their decision graphs.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("path")
                .binding()
                .optional()
                .class(XSD_STRING)
                .summary("file path within the root, percent-encoded (omitted = the whole repo)"),
        )
        .input(
            ArgSpec::new("state")
                .optional()
                .class(XSD_STRING)
                .summary(
                    "which part of the pipeline to show. pending: undecided and stood behind \
                     by the file's current review. superseded: undecided, and the file's \
                     current review does not carry it. published / declined: a human \
                     decided. all: every finding.",
                )
                .one_of(STATES)
                .default_value("pending"),
        )
        .input(
            ArgSpec::new("summary")
                .optional()
                .class(XSD_STRING)
                .summary(
                    "declined: the json face becomes {repo, path, state, rows, declined} — \
                     the rows as before plus the file-grain count of DECLINED findings by \
                     reason word and by the quote they declined, whatever state= shows. \
                     states: {repo, path, state, rows, states} — the count in each queue \
                     state and, per file, pending against superseded (with the pass that \
                     superseded them). unconfirmed: {repo, path, state, rows, \
                     unconfirmed} — the unconfirmed declines that still steer a pending \
                     finding, grouped by the burst or batch they were made in, oldest first. \
                     Without it the json face is the bare rows array it always was.",
                )
                .one_of(SUMMARIES),
        )
        .input(
            ArgSpec::new("group")
                .optional()
                .class(XSD_STRING)
                .summary(crate::group::group_summary())
                .one_of(crate::group::GROUP_KINDS),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .class(XSD_STRING)
                .summary("the face to render")
                .one_of(["application/json", "text/html", "text/plain", "text/turtle"])
                .default_value("application/json"),
        )
        .output("application/json")
        .output("text/html;charset=utf-8")
        .output("text/plain;charset=utf-8")
        .output("text/turtle")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explain::CAP_NET;
    use crate::{repr_utf8, ExplainConfig};
    use futures::executor::block_on;
    use ikigai_core::{Capability, Exact, Fallback, FnEndpoint, Iri, Kernel, Request, Verb};
    use oxigraph::store::Store;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    const PROVIDER: &str = "urn:llm:coder:ask";
    const CONTENT: &str = "fn alpha() {}\nfn beta() {}\n";
    /// One `major`, one unrated — the second is how "the model invented a word
    /// or said nothing" reaches the decision path.
    const FINDINGS: &str = "QUOTE: fn alpha() {}\nSEVERITY: major\nNOTE: no caller.\n\
                            QUOTE: fn beta() {}\nSEVERITY: urgent!!\nNOTE: unclear role.\n";

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-finding-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn demo_root() -> PathBuf {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), CONTENT).unwrap();
        root
    }

    fn kernel(root: &std::path::Path, store: &Arc<Store>) -> Kernel {
        kernel_replying(root, store, FINDINGS)
    }

    /// A kernel whose stub model answers `reply` — a second pass with
    /// different notes re-raises the first pass's claims without being an
    /// exact repeat (which the mint would withhold).
    fn kernel_replying(root: &std::path::Path, store: &Arc<Store>, reply: &str) -> Kernel {
        let reply = reply.to_string();
        let llm = EndpointSpace::new().bind(
            Exact::new(PROVIDER),
            FnEndpoint::new("fake-llm", move |_inv: &Invocation<'_>| {
                Ok(repr_utf8("text/plain", reply.clone()))
            })
            .with_description(
                Description::new("fake-llm")
                    .verb(Verb::Source)
                    .requires(CAP_NET),
            ),
        );
        // The judge is off: these tests count decision nodes and model calls,
        // and a verdict is neither (its own tests are in `review` and `judge`).
        let cfg = ExplainConfig::new(Arc::clone(store))
            .review_model_label("r1")
            .no_judge();
        let browse = crate::space_with_explain(vec![("demo".to_string(), root.to_path_buf())], cfg);
        Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(llm),
        ])))
    }

    fn cap() -> Capability {
        Capability::scoped([
            "urn:cap:browse:read:demo",
            "urn:cap:net:localhost",
            CAP_ANNOTATE,
        ])
    }

    fn issue(
        k: &Kernel,
        verb: Verb,
        iri: &str,
        args: &[(&str, &str)],
    ) -> Result<ikigai_core::Representation> {
        let mut request = Request::new(verb, Iri::parse(iri.to_string()).unwrap());
        for (name, value) in args {
            request = request.with_arg(
                *name,
                ikigai_core::ArgRef::Inline(value.as_bytes().to_vec()),
            );
        }
        block_on(k.issue(request, &cap()))
    }

    fn body(r: &ikigai_core::Representation) -> String {
        String::from_utf8_lossy(&r.bytes).to_string()
    }

    fn json(k: &Kernel, iri: &str, args: &[(&str, &str)]) -> serde_json::Value {
        let mut args = args.to_vec();
        args.push(("as", "application/json"));
        serde_json::from_str(&body(&issue(k, Verb::Source, iri, &args).unwrap())).unwrap()
    }

    /// Run the pass and return its findings' IRIs, in the order the pass
    /// recorded them.
    fn pass(k: &Kernel) -> Vec<String> {
        let pass = json(k, "urn:repo:demo:review:a.rs", &[]);
        pass["minted"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i.as_str().unwrap().to_string())
            .collect()
    }

    fn of(k: &Kernel, iri: &str) -> serde_json::Value {
        json(k, iri, &[])
    }

    /// ★★ **Both ratings survive a re-rate — the item's headline decision.**
    ///
    /// The model proposed `major`; the human publishes it as `minor`. After
    /// that, "what did the model think?" and "what did the human decide?" are
    /// both still answerable, which is the whole of the calibration argument.
    #[test]
    fn a_re_rating_keeps_the_model_s_proposal() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = findings
            .iter()
            .find(|iri| of(&k, iri)["exact"] == "fn alpha() {}")
            .unwrap()
            .clone();
        assert_eq!(of(&k, &alpha)["severity"], "major");

        let minted = body(
            &issue(
                &k,
                Verb::Sink,
                &alpha,
                &[("decision", "publish"), ("severity", "minor")],
            )
            .unwrap(),
        );

        let row = of(&k, &alpha);
        assert_eq!(row["severity"], "major", "THE PROPOSAL IS NOT OVERWRITTEN");
        assert_eq!(row["decision"]["severity"], "minor", "the human's final");
        assert_eq!(row["decision"]["outcome"], "published");
        assert_eq!(row["decision"]["minted"], minted);
        assert_eq!(row["effective_severity"], "minor");
        assert_eq!(row["state"], "published");

        // And the published annotation carries the HUMAN's rating with a link
        // back to the finding, so one hop recovers the proposal.
        let published = json(&k, &minted, &[]);
        assert_eq!(published["severity"], "minor");
        assert_eq!(published["derived_from"], alpha);
        assert_eq!(published["creator"], "r1", "still the model's words");
        assert_eq!(published["motivation"], "assessing");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A decline KEEPS the finding, with the human's reason, and mints
    /// nothing — so a re-run knows a person looked and said no.
    #[test]
    fn a_decline_is_a_record_not_a_deletion() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);

        let answer = body(
            &issue(
                &k,
                Verb::Sink,
                &findings[0],
                &[
                    ("decision", "decline"),
                    ("severity", "info"),
                    ("content", "already tracked elsewhere"),
                ],
            )
            .unwrap(),
        );
        assert_eq!(answer, findings[0], "a decline answers with the finding");

        let row = of(&k, &findings[0]);
        assert_eq!(row["state"], "declined");
        assert_eq!(row["decision"]["note"], "already tracked elsewhere");
        assert_eq!(row["decision"]["minted"], serde_json::Value::Null);
        assert_eq!(
            json(&k, "urn:repo:demo:annotations:a.rs", &[])
                .as_array()
                .unwrap()
                .len(),
            0,
            "a decline publishes nothing"
        );
        // It is still in the queue, under its own state, and out of `pending`.
        assert_eq!(
            json(&k, "urn:repo:demo:findings:a.rs", &[("state", "declined")])
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let pending = json(&k, "urn:repo:demo:findings:a.rs", &[]);
        assert_eq!(pending.as_array().unwrap().len(), 1, "{pending}");
        assert_eq!(
            json(&k, "urn:repo:demo:findings:a.rs", &[("state", "all")])
                .as_array()
                .unwrap()
                .len(),
            2
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A recorded decision is not overwritten — but an identical repeat is a
    /// no-op, so a double-clicked button is not an error.
    #[test]
    fn a_decision_cannot_be_changed_but_can_be_repeated() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let first = body(
            &issue(
                &k,
                Verb::Sink,
                &findings[0],
                &[("decision", "publish"), ("severity", "major")],
            )
            .unwrap(),
        );
        let again = body(
            &issue(
                &k,
                Verb::Sink,
                &findings[0],
                &[("decision", "publish"), ("severity", "major")],
            )
            .unwrap(),
        );
        assert_eq!(first, again, "an identical repeat is a no-op");
        assert_eq!(
            json(&k, "urn:repo:demo:annotations:a.rs", &[])
                .as_array()
                .unwrap()
                .len(),
            1,
            "and mints one annotation, not two"
        );

        for change in [
            &[("decision", "decline"), ("severity", "major")][..],
            &[("decision", "publish"), ("severity", "minor")][..],
        ] {
            let err = issue(&k, Verb::Sink, &findings[0], change).unwrap_err();
            assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");
            assert!(err.to_string().contains("already published"), "{err}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ The review threshold is a PREFIX of [`SEVERITIES`], so the prompt's
    /// three classes are slices of the one declared list. The list's ORDER is
    /// therefore load-bearing in a way a reader of `["critical", …]` would not
    /// guess — reorder it, or insert a word, and the prompt silently starts
    /// asking for a different thing. This is the test that refuses to let that
    /// happen quietly.
    #[test]
    fn the_reporting_threshold_partitions_the_severity_list() {
        assert_eq!(&SEVERITIES[..SERIOUS_SEVERITIES], ["critical", "major"]);
        assert_eq!(
            &SEVERITIES[SERIOUS_SEVERITIES..PRAISE_SEVERITY],
            ["minor", "info"]
        );
        assert_eq!(&SEVERITIES[PRAISE_SEVERITY..], ["praise"]);
        assert_eq!(SEVERITY_MEANINGS.len(), SEVERITIES.len());
        assert_eq!(
            join_words(&SEVERITIES[..SERIOUS_SEVERITIES], "or"),
            "critical or major"
        );
        assert_eq!(
            join_words(&SEVERITIES[..PRAISE_SEVERITY], "and"),
            "critical, major, minor and info"
        );
        assert_eq!(join_words(&SEVERITIES[PRAISE_SEVERITY..], "or"), "praise");
        assert_eq!(join_words(&[], "or"), "");
    }

    /// The severity set is CLOSED, it is in the CONTRACT, and the menu is
    /// rendered from the same constant — the ledger-close-`reason` failure
    /// (legal values only in an error message) does not repeat here.
    #[test]
    fn the_severity_set_is_declared_and_enforced() {
        let description = finding_description();
        let sink = description
            .action_specs()
            .into_iter()
            .find(|s| s.verb == Verb::Sink)
            .expect("the Sink action");
        let severity = sink
            .inputs
            .iter()
            .find(|i| i.name == "severity")
            .expect("severity is declared");
        assert_eq!(severity.one_of, SEVERITIES);
        let decision = sink
            .inputs
            .iter()
            .find(|i| i.name == "decision")
            .expect("decision is declared");
        assert_eq!(decision.one_of, [PUBLISH, DECLINE, RETRACT]);
        // Pipeline citizenship: the piped reason is declared.
        assert!(sink.inputs.iter().any(|i| i.name == "content"));

        // The menu renders every declared value and nothing else.
        let menu = decision_html("x", Some("major"), None, None);
        for value in SEVERITIES {
            assert!(
                menu.contains(&format!("<option value=\"{value}\"")),
                "{menu}"
            );
        }
        assert!(menu.contains("value=\"major\" selected"), "{menu}");

        // And a word outside the set is refused, naming the set.
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let err = issue(
            &k,
            Verb::Sink,
            &findings[0],
            &[("decision", "publish"), ("severity", "urgent")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("critical, major"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// ⚠ An unrated finding stays unrated — the model's invented word is
    /// dropped, never mapped onto a real rating — and a decision on it must
    /// then state one.
    #[test]
    fn an_invented_severity_leaves_the_finding_unrated() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let beta = findings
            .iter()
            .find(|iri| of(&k, iri)["exact"] == "fn beta() {}")
            .unwrap()
            .clone();
        assert_eq!(of(&k, &beta)["severity"], serde_json::Value::Null);

        let err = issue(&k, Verb::Sink, &beta, &[("decision", "publish")]).unwrap_err();
        assert!(err.to_string().contains("proposed no severity"), "{err}");

        issue(
            &k,
            Verb::Sink,
            &beta,
            &[("decision", "publish"), ("severity", "info")],
        )
        .unwrap();
        let row = of(&k, &beta);
        assert_eq!(row["severity"], serde_json::Value::Null, "still unrated");
        assert_eq!(row["decision"]["severity"], "info");
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★★ **The family boundary, in the graph rather than in the code.** A
    /// finding is not an `oa:Annotation` and carries no `oa:` term whose
    /// domain would make it one — so no reader, query or reasoner can mistake
    /// an unapproved machine claim for a published note.
    #[test]
    fn a_finding_is_not_an_annotation_under_any_reading() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);

        let ttl = body(&issue(&k, Verb::Source, &findings[0], &[("as", "text/turtle")]).unwrap());
        assert!(ttl.contains("a prov:Entity"), "{ttl}");
        assert!(!ttl.contains("oa:Annotation"), "{ttl}");
        // ⚠ Both of these carry `rdfs:domain oa:Annotation` in the W3C
        // vocabulary: using either would type the finding into the family
        // under entailment, with no code path showing it.
        assert!(!ttl.contains("oa:bodyValue"), "{ttl}");
        assert!(!ttl.contains("oa:motivatedBy"), "{ttl}");
        assert!(ttl.contains("dcterms:description"), "{ttl}");

        // And the annotation family's own readers see nothing: the listing,
        // the file face's overlay, and the file's margin notes.
        assert_eq!(
            json(&k, "urn:repo:demo:annotations:a.rs", &[])
                .as_array()
                .unwrap()
                .len(),
            0
        );
        let file = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:file:a.rs",
                &[("as", "text/html")],
            )
            .unwrap(),
        );
        assert!(!file.contains("browse-annotation-marker"), "{file}");
        assert!(!file.contains("no caller."), "{file}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Staleness is the ANNOTATION LAYER'S answer, not a second one: a
    /// finding whose quote is gone orphans, still renders, and still
    /// publishes — as an orphaned annotation that keeps reconciling.
    #[test]
    fn a_stale_finding_orphans_and_still_publishes() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = findings
            .iter()
            .find(|iri| of(&k, iri)["exact"] == "fn alpha() {}")
            .unwrap()
            .clone();

        std::fs::write(root.join("a.rs"), "fn beta() {}\n").unwrap();
        let row = of(&k, &alpha);
        assert_eq!(row["orphaned"], true, "{row}");
        assert_eq!(row["state"], "pending", "an orphan is still answerable");

        let minted = body(
            &issue(
                &k,
                Verb::Sink,
                &alpha,
                &[("decision", "publish"), ("severity", "minor")],
            )
            .unwrap(),
        );
        assert_eq!(json(&k, &minted, &[])["orphaned"], true);
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ The id is derived from the POSITION, so a re-derivation of the same
    /// pass re-mints the SAME finding and a human's decision survives it.
    /// That is what keeps a re-run (or a compacted archive, ledger #437) from
    /// costing a second triage pass over findings someone already answered.
    #[test]
    fn a_re_derived_pass_re_mints_the_same_finding_and_keeps_its_decision() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        issue(
            &k,
            Verb::Sink,
            &findings[0],
            &[("decision", "decline"), ("severity", "minor")],
        )
        .unwrap();

        // The id really is a pure function of (pass, anchor, quote) — which
        // is what makes a re-mint land on the SAME node rather than beside it.
        let row = of(&k, &findings[0]);
        let recomputed = crate::annotate::finding_id(
            row["generated_by"].as_str().unwrap(),
            row["start"].as_u64().unwrap(),
            row["exact"].as_str().unwrap(),
        );
        assert_eq!(
            finding_iri(&recomputed),
            findings[0],
            "the id is the position"
        );

        // And re-sourcing the pass (an archive hit, and the shape a trigger
        // repeats) serves the same set without resurrecting the answered one.
        assert_eq!(pass(&k), findings, "the same findings, not a second queue");
        assert_eq!(of(&k, &findings[0])["state"], "declined");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The FILE grain of a decline (ledger #475): `summary=declined` widens
    /// the json face to an object carrying how many declines the file holds,
    /// by the quote they declined — whatever `state=` shows — and the html
    /// and plain faces of a file's listing say it in words. Without the
    /// argument the json face is the bare array it always was, and a
    /// repo-wide page does not sum declines across files.
    #[test]
    fn the_file_listing_counts_its_declines_by_quote() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = findings
            .iter()
            .find(|iri| of(&k, iri)["exact"] == "fn alpha() {}")
            .unwrap()
            .clone();
        issue(
            &k,
            Verb::Sink,
            &alpha,
            &[
                ("decision", "decline"),
                ("content", "the severity was a misread"),
            ],
        )
        .unwrap();

        let bare = json(&k, "urn:repo:demo:findings:a.rs", &[]);
        assert_eq!(bare.as_array().unwrap().len(), 1, "unchanged shape: {bare}");
        let wide = json(
            &k,
            "urn:repo:demo:findings:a.rs",
            &[("summary", "declined")],
        );
        assert_eq!(wide["repo"], "demo");
        assert_eq!(wide["path"], "a.rs");
        assert_eq!(wide["state"], "pending");
        assert_eq!(wide["rows"], bare, "the rows are the array, as they were");
        assert_eq!(wide["declined"]["count"], 1, "{wide}");
        let quotes = wide["declined"]["quotes"].as_array().unwrap();
        assert_eq!(quotes.len(), 1, "{wide}");
        assert_eq!(quotes[0]["exact"], "fn alpha() {}");
        assert_eq!(quotes[0]["count"], 1);
        assert_eq!(quotes[0]["findings"], serde_json::json!([alpha]));
        // The date is the decision's own (null here: this kernel has no
        // clock, so the decision recorded none — and the summary invents
        // none either).
        assert_eq!(
            quotes[0]["latest_decided_at"],
            of(&k, &alpha)["decision"]["decided_at"],
            "{wide}"
        );

        // The count is the file's, whatever state the rows show.
        let declined_view = json(
            &k,
            "urn:repo:demo:findings:a.rs",
            &[("summary", "declined"), ("state", "published")],
        );
        assert_eq!(declined_view["rows"].as_array().unwrap().len(), 0);
        assert_eq!(declined_view["declined"]["count"], 1);

        let html = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:findings:a.rs",
                &[("as", "text/html")],
            )
            .unwrap(),
        );
        assert!(html.contains("browse-findings-declined"), "{html}");
        assert!(
            html.contains("1 declined finding on this file, on 1 distinct quote"),
            "{html}"
        );
        assert!(html.contains("1× <code>fn alpha() {}</code>"), "{html}");
        let plain = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:findings:a.rs",
                &[("as", "text/plain")],
            )
            .unwrap(),
        );
        assert!(
            plain.contains("1 declined finding on this file, on 1 distinct quote"),
            "{plain}"
        );
        let repo_wide = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:findings",
                &[("as", "text/html")],
            )
            .unwrap(),
        );
        assert!(
            !repo_wide.contains("browse-findings-declined"),
            "{repo_wide}"
        );

        let err = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:findings:a.rs",
            &[("summary", "everything")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a summary"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The finding named by its quote — the fixture's two lines are distinct.
    fn finding_on(k: &Kernel, findings: &[String], exact: &str) -> String {
        findings
            .iter()
            .find(|iri| of(k, iri)["exact"] == exact)
            .unwrap_or_else(|| panic!("no finding on {exact}"))
            .clone()
    }

    /// ★ The reason words are the CONTRACT's, read exactly the way gonk reads
    /// its menus: `Kernel::describe` on a finding IRI, the Sink action's
    /// `reason` input, its `one_of` — the five, in the picker's order. The
    /// meanings travel in the summary, and browse's own form renders from the
    /// same constant behind an empty "omitted" default.
    #[test]
    fn the_decline_reasons_are_declared_in_order() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let description = k
            .describe(&Iri::parse("urn:iki:finding:x".to_string()).unwrap())
            .expect("a finding describes itself");
        let sink = description
            .action_specs()
            .into_iter()
            .find(|s| s.verb == Verb::Sink)
            .expect("the Sink action");
        let reason = sink
            .inputs
            .iter()
            .find(|i| i.name == "reason")
            .expect("reason is declared");
        assert_eq!(
            reason.one_of,
            ["misread", "restates", "no-issue", "wont-fix", "duplicate"]
        );
        assert!(!reason.required, "omitted stays valid");
        let summary = &reason.summary;
        for (word, meaning) in DECLINE_REASONS.iter().zip(DECLINE_REASON_MEANINGS) {
            assert!(summary.contains(&format!("{word}: {meaning}")), "{summary}");
        }
        // The Source action takes no reason: it is an answer, not a filter.
        let source = description
            .action_specs()
            .into_iter()
            .find(|s| s.verb == Verb::Source)
            .unwrap();
        assert!(source.inputs.iter().all(|i| i.name != "reason"));

        let form = decision_html("x", Some("major"), None, None);
        assert!(
            form.contains("<select name=\"reason\"><option value=\"\" selected>"),
            "{form}"
        );
        let mut at = 0;
        for word in DECLINE_REASONS {
            let here = form
                .find(&format!("<option value=\"{word}\""))
                .unwrap_or_else(|| panic!("{word} missing: {form}"));
            assert!(here > at, "{word} out of order: {form}");
            at = here;
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★★ Every word round-trips — stored as a TERM on the decision node, read
    /// back on every face: the json `decision.reason`, the turtle
    /// `dcterms:subject <urn:iki:decline-reason:{word}>`, the plain row and the
    /// html card, in words.
    #[test]
    fn every_decline_reason_round_trips_on_every_face() {
        for word in DECLINE_REASONS {
            let root = demo_root();
            let store = Arc::new(Store::new().unwrap());
            let k = kernel(&root, &store);
            let findings = pass(&k);
            let alpha = finding_on(&k, &findings, "fn alpha() {}");
            issue(
                &k,
                Verb::Sink,
                &alpha,
                &[
                    ("decision", "decline"),
                    ("reason", word),
                    ("content", "a free-text note, kept apart"),
                ],
            )
            .unwrap();

            let row = of(&k, &alpha);
            assert_eq!(row["decision"]["reason"], word, "{row}");
            assert_eq!(row["decision"]["note"], "a free-text note, kept apart");
            assert_eq!(row["decision"]["outcome"], "declined");

            // In the store as an IRI object, not a literal.
            let subject = oxigraph::model::NamedNode::new(format!("{alpha}:decision")).unwrap();
            let predicate =
                oxigraph::model::NamedNode::new(crate::annotate::DCTERMS_SUBJECT).unwrap();
            let objects: Vec<String> = store
                .quads_for_pattern(
                    Some(subject.as_ref().into()),
                    Some(predicate.as_ref()),
                    None,
                    None,
                )
                .map(|q| q.unwrap().object.to_string())
                .collect();
            assert_eq!(objects, [format!("<urn:iki:decline-reason:{word}>")]);

            let ttl = body(&issue(&k, Verb::Source, &alpha, &[("as", "text/turtle")]).unwrap());
            assert!(
                ttl.contains(&format!("dcterms:subject <urn:iki:decline-reason:{word}>")),
                "{ttl}"
            );
            assert!(ttl.contains("dcterms:type <urn:iki:finding:outcome:declined>"));
            let plain = body(&issue(&k, Verb::Source, &alpha, &[("as", "text/plain")]).unwrap());
            assert!(plain.contains(&format!("-> major ({word})")), "{plain}");
            let html = body(&issue(&k, Verb::Source, &alpha, &[("as", "text/html")]).unwrap());
            assert!(
                html.contains(&format!(
                    "declined by a human <span class=\"browse-finding-decline-reason\">\
                     ({word})</span>"
                )),
                "{html}"
            );
            std::fs::remove_dir_all(&root).ok();
        }
    }

    /// Fail loud: a word beside a publish is refused BY NAME (a publish reason
    /// has no consumer, and an accepted-then-ignored argument is invisible), a
    /// word outside the set is refused naming the set — and neither writes
    /// anything. An EMPTY value is omitted, so the publish button sharing a
    /// form with an unselected picker still publishes.
    #[test]
    fn a_reason_is_refused_beside_a_publish_and_outside_the_set() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");

        let err = issue(
            &k,
            Verb::Sink,
            &alpha,
            &[("decision", "publish"), ("reason", "restates")],
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidArgument { name, .. } if name == "reason"),
            "{err:?}"
        );
        assert!(err.to_string().contains("decision=decline"), "{err}");

        let err = issue(
            &k,
            Verb::Sink,
            &alpha,
            &[("decision", "decline"), ("reason", "bogus")],
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidArgument { name, .. } if name == "reason"),
            "{err:?}"
        );
        assert!(
            err.to_string()
                .contains("one of: misread, restates, no-issue, wont-fix, duplicate"),
            "{err}"
        );
        assert_eq!(of(&k, &alpha)["state"], "pending", "nothing was recorded");

        issue(
            &k,
            Verb::Sink,
            &alpha,
            &[("decision", "publish"), ("reason", " ")],
        )
        .expect("an empty reason is omitted, not refused");
        let row = of(&k, &alpha);
        assert_eq!(row["state"], "published");
        assert_eq!(row["decision"]["reason"], serde_json::Value::Null);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Omitted is today's behaviour and stays valid: the decision reads back
    /// `reason: null` and writes no reason triple. And the record rule covers
    /// the reason: a repeat stating none, or the same word, is the no-op a
    /// double click needs; a repeat stating a DIFFERENT word — including one
    /// where none was recorded — is refused, by `reason`, naming what is on
    /// file.
    #[test]
    fn a_decline_without_a_reason_reads_back_null_and_a_reason_is_not_rewritten() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        let beta = finding_on(&k, &findings, "fn beta() {}");

        issue(&k, Verb::Sink, &alpha, &[("decision", "decline")]).unwrap();
        let row = of(&k, &alpha);
        assert_eq!(row["decision"]["reason"], serde_json::Value::Null, "{row}");
        assert!(
            row["decision"].get("reason").is_some(),
            "the key is present"
        );
        let ttl = body(&issue(&k, Verb::Source, &alpha, &[("as", "text/turtle")]).unwrap());
        assert!(!ttl.contains("dcterms:subject"), "{ttl}");
        let err = issue(
            &k,
            Verb::Sink,
            &alpha,
            &[("decision", "decline"), ("reason", "misread")],
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidArgument { name, .. } if name == "reason"),
            "{err:?}"
        );
        assert!(err.to_string().contains("(no reason stated)"), "{err}");

        issue(
            &k,
            Verb::Sink,
            &beta,
            &[
                ("decision", "decline"),
                ("severity", "info"),
                ("reason", "no-issue"),
            ],
        )
        .unwrap();
        for repeat in [
            &[("decision", "decline"), ("severity", "info")][..],
            &[
                ("decision", "decline"),
                ("severity", "info"),
                ("reason", "no-issue"),
            ][..],
        ] {
            issue(&k, Verb::Sink, &beta, repeat).expect("an identical repeat is a no-op");
        }
        let err = issue(
            &k,
            Verb::Sink,
            &beta,
            &[
                ("decision", "decline"),
                ("severity", "info"),
                ("reason", "duplicate"),
            ],
        )
        .unwrap_err();
        assert!(err.to_string().contains("as `info` (no-issue)"), "{err}");
        assert_eq!(of(&k, &beta)["decision"]["reason"], "no-issue");
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ Load-back is EXACT, the way the severity loader is: a decision node
    /// whose reason term is not a word of the set — a word a later release
    /// adds, a typo in a hand-written triple, an IRI from somewhere else —
    /// reads back as `reason: null`, never mapped onto a real word, and the
    /// rest of the decision is untouched.
    #[test]
    fn an_unknown_reason_term_reads_back_as_no_reason() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        let beta = finding_on(&k, &findings, "fn beta() {}");
        issue(
            &k,
            Verb::Sink,
            &alpha,
            &[("decision", "decline"), ("content", "hand-edited later")],
        )
        .unwrap();
        issue(
            &k,
            Verb::Sink,
            &beta,
            &[("decision", "decline"), ("severity", "minor")],
        )
        .unwrap();
        let predicate = oxigraph::model::NamedNode::new(crate::annotate::DCTERMS_SUBJECT).unwrap();
        for (finding, object) in [
            (&alpha, "urn:iki:decline-reason:restatez"),
            (&beta, "urn:example:restates"),
        ] {
            store
                .insert(&oxigraph::model::Quad::new(
                    oxigraph::model::NamedNode::new(format!("{finding}:decision")).unwrap(),
                    predicate.clone(),
                    oxigraph::model::NamedNode::new(object).unwrap(),
                    oxigraph::model::GraphName::DefaultGraph,
                ))
                .unwrap();
            let row = of(&k, finding);
            assert_eq!(row["decision"]["reason"], serde_json::Value::Null, "{row}");
            assert_eq!(row["decision"]["outcome"], "declined", "{row}");
        }
        assert_eq!(of(&k, &alpha)["decision"]["note"], "hand-edited later");
        let wide = json(
            &k,
            "urn:repo:demo:findings:a.rs",
            &[("summary", "declined")],
        );
        assert_eq!(
            wide["declined"]["reasons"][5],
            serde_json::json!({"reason": null, "count": 2}),
            "{wide}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The file grain counts declines BY REASON (the number ledger #483
    /// wants): every word in contract order with zeros, then `null` for the
    /// declines that state none — and the plain and html faces say the
    /// non-zero ones in words.
    #[test]
    fn the_file_listing_counts_its_declines_by_reason() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        let beta = finding_on(&k, &findings, "fn beta() {}");
        issue(
            &k,
            Verb::Sink,
            &alpha,
            &[("decision", "decline"), ("reason", "restates")],
        )
        .unwrap();
        issue(
            &k,
            Verb::Sink,
            &beta,
            &[("decision", "decline"), ("severity", "info")],
        )
        .unwrap();

        let wide = json(
            &k,
            "urn:repo:demo:findings:a.rs",
            &[("summary", "declined")],
        );
        assert_eq!(wide["declined"]["count"], 2);
        assert_eq!(
            wide["declined"]["reasons"],
            serde_json::json!([
                {"reason": "misread", "count": 0},
                {"reason": "restates", "count": 1},
                {"reason": "no-issue", "count": 0},
                {"reason": "wont-fix", "count": 0},
                {"reason": "duplicate", "count": 0},
                {"reason": null, "count": 1},
            ]),
            "{wide}"
        );
        let words =
            "2 declined findings on this file, on 2 distinct quotes (restates 1, no reason 1)";
        for face in ["text/plain", "text/html"] {
            let out = body(
                &issue(
                    &k,
                    Verb::Source,
                    "urn:repo:demo:findings:a.rs",
                    &[("as", face)],
                )
                .unwrap(),
            );
            assert!(out.contains(words), "{out}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// The queue's own faces: the state nav, the not-a-gate note, and the
    /// decision form on a pending card.
    #[test]
    fn the_queue_page_says_it_is_not_a_gate_and_offers_the_decision() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        pass(&k);
        let html = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:findings:a.rs",
                &[("as", "text/html")],
            )
            .unwrap(),
        );
        assert!(html.contains("blocks a commit"), "{html}");
        assert!(html.contains("browse-finding-pending"), "{html}");
        assert!(
            html.contains("hx-post=\"/k/sink urn:iki:finding:"),
            "{html}"
        );
        assert!(html.contains("proposed by the model"), "{html}");
        assert!(html.contains("browse-findings-state-current"), "{html}");

        // The file face offers the queue, unconditionally.
        let file = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:file:a.rs",
                &[("as", "text/html")],
            )
            .unwrap(),
        );
        assert!(file.contains("browse-findings-link"), "{file}");
        std::fs::remove_dir_all(&root).ok();
    }

    // --- revisions (ledger #653) ---------------------------------------------

    /// The second pass's notes: the same two quotes, reworded, so each is a
    /// RECURRENCE of the first pass's claim on its line (marked with the
    /// declined twin), never an exact repeat the mint would withhold.
    const REWORDED: &str = "QUOTE: fn alpha() {}\nSEVERITY: major\nNOTE: still no caller.\n\
                            QUOTE: fn beta() {}\nSEVERITY: minor\nNOTE: role unclear.\n";

    /// Edit the file and run the reworded pass: the findings it mints on
    /// `fn alpha() {}` and `fn beta() {}`, in that order.
    fn recur(root: &std::path::Path, store: &Arc<Store>) -> (Kernel, String, String) {
        std::fs::write(root.join("a.rs"), format!("{CONTENT}// edited\n")).unwrap();
        let k = kernel_replying(root, store, REWORDED);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        let beta = finding_on(&k, &findings, "fn beta() {}");
        (k, alpha, beta)
    }

    fn sink(k: &Kernel, iri: &str, args: &[(&str, &str)]) -> Result<String> {
        issue(k, Verb::Sink, iri, args).map(|r| body(&r))
    }

    fn refused(result: Result<String>, name: &str, says: &str) {
        match result {
            Err(Error::InvalidArgument { name: got, detail }) => {
                assert_eq!(got, name, "{detail}");
                assert!(detail.contains(says), "{detail}");
            }
            other => panic!("expected `{name}` refused saying `{says}`, got {other:?}"),
        }
    }

    /// Every decision node on a finding, from the STORE, with its outcome —
    /// the record a revision must leave whole.
    fn nodes(store: &Store, finding: &str) -> Vec<(String, String)> {
        let used = oxigraph::model::NamedNode::new(crate::annotate::PROV_USED).unwrap();
        let object = oxigraph::model::NamedNode::new(finding).unwrap();
        let ty = oxigraph::model::NamedNode::new(crate::annotate::DCTERMS_TYPE).unwrap();
        let mut out: Vec<(String, String)> = store
            .quads_for_pattern(
                None,
                Some(used.as_ref()),
                Some(object.as_ref().into()),
                None,
            )
            .map(|q| q.unwrap().subject.to_string())
            .map(|s| s.trim_matches(|c| c == '<' || c == '>').to_string())
            .map(|node| {
                let subject = oxigraph::model::NamedNode::new(&node).unwrap();
                let outcome = store
                    .quads_for_pattern(Some(subject.as_ref().into()), Some(ty.as_ref()), None, None)
                    .map(|q| q.unwrap().object.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                (node, outcome)
            })
            .collect();
        out.sort();
        out
    }

    /// ★★ **Confirm**: a wordless decline gains a word — but only by NAMING the
    /// decision it revises. Without `revises=` the change is refused, naming
    /// the IRI to revise; with it, a second node lands, links the first, and
    /// becomes current — and the first is still in the store exactly as it
    /// was written.
    #[test]
    fn a_wordless_decline_is_confirmed_by_a_revision_and_both_are_kept() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        sink(&k, &alpha, &[("decision", "decline")]).unwrap();
        let first = format!("{alpha}:decision");
        assert_eq!(of(&k, &alpha)["decision"]["iri"], first.as_str());

        refused(
            sink(
                &k,
                &alpha,
                &[("decision", "decline"), ("reason", "restates")],
            ),
            "reason",
            &format!("revises={first}"),
        );

        sink(
            &k,
            &alpha,
            &[
                ("decision", "decline"),
                ("reason", "restates"),
                ("revises", &first),
                ("content", "the comment above says so"),
            ],
        )
        .unwrap();
        let row = of(&k, &alpha);
        let second = format!("{alpha}:decision:2");
        assert_eq!(row["state"], "declined");
        assert_eq!(row["decision"]["iri"], second.as_str(), "{row}");
        assert_eq!(row["decision"]["reason"], "restates");
        assert_eq!(row["decision"]["revises"], first.as_str());
        assert_eq!(row["decision"]["note"], "the comment above says so");
        let chain = row["decisions"].as_array().unwrap();
        assert_eq!(chain.len(), 2, "{row}");
        assert_eq!(chain[0]["iri"], first.as_str());
        assert_eq!(
            chain[0]["reason"],
            serde_json::Value::Null,
            "the first is untouched"
        );
        assert_eq!(chain[1], row["decision"]);

        // In the store: both nodes, both declines, the link on the second.
        let declined = "<urn:iki:finding:outcome:declined>".to_string();
        assert_eq!(
            nodes(&store, &alpha),
            [
                (first.clone(), declined.clone()),
                (second.clone(), declined)
            ]
        );
        let ttl = body(&issue(&k, Verb::Source, &alpha, &[("as", "text/turtle")]).unwrap());
        assert!(
            ttl.contains(&format!("<{second}> a prov:Activity")),
            "{ttl}"
        );
        assert!(
            ttl.contains(&format!("dcterms:replaces <{first}>")),
            "{ttl}"
        );
        assert!(!ttl.contains("wasRevisionOf"), "{ttl}");

        // A drift rewrite of the record leaves the chain alone.
        std::fs::write(root.join("a.rs"), format!("// moved\n{CONTENT}")).unwrap();
        assert_eq!(of(&k, &alpha)["decisions"].as_array().unwrap().len(), 2);
        assert_eq!(nodes(&store, &alpha).len(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★★ **Retract**: the decline is withdrawn, the finding is undecided
    /// again, and the recurrence mark FOLLOWS the current decision — a
    /// retracted decline is no `prior_decision`, forms no recurrence group,
    /// and lists in no decline count. A fresh decline after it needs no
    /// `revises=`, links the retraction, and makes the mark steer again.
    #[test]
    fn a_retracted_decline_is_undecided_and_marks_nothing() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        sink(&k, &alpha, &[("decision", "decline")]).unwrap();
        let (k, again, _) = recur(&root, &store);
        let mark = &of(&k, &again)["prior_decision"];
        assert_eq!(mark["finding"], alpha.as_str(), "the recurrence is marked");
        assert_eq!(mark["outcome"], "declined");

        let first = format!("{alpha}:decision");
        sink(
            &k,
            &alpha,
            &[
                ("decision", "retract"),
                ("revises", &first),
                ("content", "mis-click"),
            ],
        )
        .unwrap();
        let row = of(&k, &alpha);
        assert_eq!(row["decision"], serde_json::Value::Null, "{row}");
        assert_ne!(row["state"], "declined");
        let chain = row["decisions"].as_array().unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[1]["outcome"], "retracted");
        assert_eq!(chain[1]["severity"], serde_json::Value::Null);
        assert_eq!(chain[1]["note"], "mis-click");
        assert_eq!(chain[1]["revises"], first.as_str());

        assert_eq!(
            of(&k, &again)["prior_decision"],
            serde_json::Value::Null,
            "a retracted decline steers nothing"
        );
        let groups = json(&k, "urn:repo:demo:findings", &[("group", "recurrence")]);
        assert!(
            groups["groups"]
                .as_array()
                .unwrap()
                .iter()
                .all(|g| g["twin"]["iri"] != alpha.as_str()),
            "{groups}"
        );
        let wide = json(
            &k,
            "urn:repo:demo:findings:a.rs",
            &[("summary", "declined")],
        );
        assert_eq!(wide["declined"]["count"], 0, "{wide}");
        // A second retraction is the double click of the first.
        sink(&k, &alpha, &[("decision", "retract")]).unwrap();
        assert_eq!(of(&k, &alpha)["decisions"].as_array().unwrap().len(), 2);

        // Undecided again: a plain decision is accepted and links the
        // retraction — and the mark steers again.
        sink(
            &k,
            &alpha,
            &[("decision", "decline"), ("reason", "misread")],
        )
        .unwrap();
        let row = of(&k, &alpha);
        assert_eq!(row["decisions"].as_array().unwrap().len(), 3);
        assert_eq!(
            row["decision"]["revises"],
            format!("{alpha}:decision:2").as_str()
        );
        let mark = &of(&k, &again)["prior_decision"];
        assert_eq!(mark["reason"], "misread", "{mark}");
        assert_eq!(mark["iri"], format!("{alpha}:decision:3").as_str());
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★★ **Reverse**: decline → publish promotes exactly as a first publish
    /// does, both answers stay on record, and the recurrence mark stops — a
    /// like claim was not declined any more.
    #[test]
    fn a_decline_reversed_to_a_publish_promotes_and_stops_marking() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        sink(&k, &alpha, &[("decision", "decline")]).unwrap();
        let (k, again, _) = recur(&root, &store);

        let minted = sink(
            &k,
            &alpha,
            &[
                ("decision", "publish"),
                ("severity", "minor"),
                ("revises", &format!("{alpha}:decision")),
            ],
        )
        .unwrap();
        let row = of(&k, &alpha);
        assert_eq!(row["state"], "published", "{row}");
        assert_eq!(row["decision"]["minted"], minted.as_str());
        assert_eq!(row["decisions"][0]["outcome"], "declined");
        let published = json(&k, &minted, &[]);
        assert_eq!(published["severity"], "minor");
        assert_eq!(published["derived_from"], alpha.as_str());
        assert_eq!(of(&k, &again)["prior_decision"], serde_json::Value::Null);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A PUBLICATION stands while its annotation does: revising it is refused,
    /// naming the Delete — and once the annotation is deleted, the revision is
    /// an ordinary one and the publish stays in the chain.
    #[test]
    fn a_publication_is_revised_only_after_its_annotation_is_deleted() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        let minted = sink(&k, &alpha, &[("decision", "publish")]).unwrap();
        let first = format!("{alpha}:decision");
        for decision in ["decline", "retract"] {
            refused(
                sink(&k, &alpha, &[("decision", decision), ("revises", &first)]),
                "revises",
                &format!("Delete `{minted}` first"),
            );
        }
        // Without `revises`, the refusal says both halves of the way.
        refused(
            sink(&k, &alpha, &[("decision", "decline")]),
            "decision",
            "revised only once the annotation it minted",
        );

        issue(&k, Verb::Delete, &minted, &[]).unwrap();
        sink(
            &k,
            &alpha,
            &[
                ("decision", "decline"),
                ("reason", "wont-fix"),
                ("revises", &first),
            ],
        )
        .unwrap();
        let row = of(&k, &alpha);
        assert_eq!(row["state"], "declined");
        assert_eq!(row["decisions"][0]["outcome"], "published");
        assert_eq!(
            row["decisions"][0]["minted"],
            minted.as_str(),
            "the record of it"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The refusals: `revises` on an undecided finding, naming a decision that
    /// is not current (with the current one named back), naming something that
    /// is not the finding's; a retraction with nothing to retract or with a
    /// rating; a word beside a revision to publish or retract; and `made`
    /// arguments that disagree. None of them writes anything — and the second
    /// submit of a revision that already landed is a no-op, not a refusal.
    #[test]
    fn revisions_refuse_what_they_cannot_revise() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        let first = format!("{alpha}:decision");

        refused(
            sink(&k, &alpha, &[("decision", "decline"), ("revises", &first)]),
            "revises",
            "has no decision to revise",
        );
        refused(
            sink(&k, &alpha, &[("decision", "retract")]),
            "decision",
            "no decision to retract",
        );
        sink(&k, &alpha, &[("decision", "decline")]).unwrap();
        refused(
            sink(
                &k,
                &alpha,
                &[
                    ("decision", "retract"),
                    ("severity", "minor"),
                    ("revises", &first),
                ],
            ),
            "severity",
            "drop `severity`",
        );
        refused(
            sink(
                &k,
                &alpha,
                &[
                    ("decision", "retract"),
                    ("reason", "misread"),
                    ("revises", &first),
                ],
            ),
            "reason",
            "this decision is retract",
        );
        refused(
            sink(
                &k,
                &alpha,
                &[
                    ("decision", "publish"),
                    ("reason", "misread"),
                    ("revises", &first),
                ],
            ),
            "reason",
            "this decision is publish",
        );
        refused(
            sink(
                &k,
                &alpha,
                &[
                    ("decision", "decline"),
                    ("revises", "urn:iki:finding:x:decision"),
                ],
            ),
            "revises",
            "it is not one of its decisions",
        );
        let revision = [
            ("decision", "decline"),
            ("reason", "restates"),
            ("revises", first.as_str()),
        ];
        sink(&k, &alpha, &revision).unwrap();
        sink(&k, &alpha, &revision).expect("the second submit of a landed revision is a no-op");
        refused(
            sink(
                &k,
                &alpha,
                &[
                    ("decision", "decline"),
                    ("reason", "misread"),
                    ("revises", &first),
                ],
            ),
            "revises",
            &format!(
                "(a later decision already revised it) — a revision names the decision it \
                      revises, and the current one is `{alpha}:decision:2`"
            ),
        );
        for (args, name) in [
            (&[("decision", "decline"), ("made", "batch")][..], "batch"),
            (
                &[("decision", "decline"), ("batch", "file:a.rs")][..],
                "batch",
            ),
            (
                &[("decision", "decline"), ("made", "single"), ("batch", "k")][..],
                "batch",
            ),
            (&[("decision", "decline"), ("made", "twice")][..], "made"),
        ] {
            let beta = finding_on(&k, &findings, "fn beta() {}");
            let mut args = args.to_vec();
            args.push(("severity", "info"));
            let err = sink(&k, &beta, &args).unwrap_err();
            assert!(
                matches!(&err, Error::InvalidArgument { name: n, .. } if n == name),
                "{err:?}"
            );
            assert_eq!(of(&k, &beta)["state"], "pending", "nothing was recorded");
        }
        assert_eq!(
            nodes(&store, &alpha).len(),
            2,
            "only the one revision landed"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ **How a decision was made** is recorded when the caller says and
    /// returned on every face, and it decides `confirmed` with the reason word:
    /// a wordless decline made in a batch is UNCONFIRMED — on its own row and
    /// on the `prior_decision` of every recurrence it marks — while a worded
    /// one, or one made singly, is confirmed. A revision made singly confirms.
    #[test]
    fn made_is_recorded_and_a_wordless_batch_decline_is_unconfirmed() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        let beta = finding_on(&k, &findings, "fn beta() {}");
        let key = "recurrence:abc def";
        sink(
            &k,
            &alpha,
            &[("decision", "decline"), ("made", "batch"), ("batch", key)],
        )
        .unwrap();
        sink(
            &k,
            &beta,
            &[
                ("decision", "decline"),
                ("severity", "info"),
                ("made", "single"),
            ],
        )
        .unwrap();
        let row = of(&k, &alpha);
        assert_eq!(row["decision"]["made"], "batch");
        assert_eq!(row["decision"]["batch"], key);
        assert_eq!(row["decision"]["confirmed"], false, "{row}");
        assert_eq!(row["decision"]["burst"], serde_json::Value::Null);
        let row = of(&k, &beta);
        assert_eq!(row["decision"]["made"], "single");
        assert_eq!(row["decision"]["batch"], serde_json::Value::Null);
        assert_eq!(row["decision"]["confirmed"], true);
        let ttl = body(&issue(&k, Verb::Source, &alpha, &[("as", "text/turtle")]).unwrap());
        assert!(
            ttl.contains("dcterms:provenance <urn:iki:decision-made:batch:recurrence:abc%20def>"),
            "{ttl}"
        );
        let plain = body(&issue(&k, Verb::Source, &alpha, &[("as", "text/plain")]).unwrap());
        assert!(plain.contains("[unconfirmed]"), "{plain}");

        let (k, again, _) = recur(&root, &store);
        let mark = &of(&k, &again)["prior_decision"];
        assert_eq!(mark["confirmed"], false, "{mark}");
        assert_eq!(mark["made"], "batch");
        let plain = body(&issue(&k, Verb::Source, &again, &[("as", "text/plain")]).unwrap());
        assert!(plain.contains("(unconfirmed)"), "{plain}");

        // Confirmed by a single revision with a word: the old node keeps its
        // own reading, the new one is current and confirmed.
        sink(
            &k,
            &alpha,
            &[
                ("decision", "decline"),
                ("reason", "no-issue"),
                ("made", "single"),
                ("revises", &format!("{alpha}:decision")),
            ],
        )
        .unwrap();
        let row = of(&k, &alpha);
        assert_eq!(row["decisions"][0]["confirmed"], false);
        assert_eq!(row["decision"]["confirmed"], true);
        assert_eq!(of(&k, &again)["prior_decision"]["confirmed"], true);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Write a decision node exactly as 0.13 did — no provenance, a
    /// timestamp — the shape every decline before ledger #653 has.
    fn legacy_decline(store: &Store, node: &str, finding: &str, at: &str) {
        use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
        let n = NamedNode::new(node).unwrap();
        let p = |iri: &str| NamedNode::new(iri).unwrap();
        for (predicate, object) in [
            (
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                oxigraph::model::Term::from(p("http://www.w3.org/ns/prov#Activity")),
            ),
            (crate::annotate::PROV_USED, p(finding).into()),
            (
                crate::annotate::DCTERMS_TYPE,
                p("urn:iki:finding:outcome:declined").into(),
            ),
            (
                crate::annotate::SH_RESULT_SEVERITY,
                p("urn:iki:severity:major").into(),
            ),
            (
                crate::annotate::DCTERMS_CREATED,
                Literal::new_typed_literal(at, oxigraph::model::vocab::xsd::DATE_TIME).into(),
            ),
        ] {
            store
                .insert(&Quad::new(
                    n.clone(),
                    p(predicate),
                    object,
                    GraphName::DefaultGraph,
                ))
                .unwrap();
        }
    }

    /// ★★ **Legacy bursts, and the walk.** Three declines written the 0.13
    /// way inside one second (two on this repo's findings, one elsewhere in
    /// the graph) read UNCONFIRMED, named by their burst; a fourth, alone, is
    /// confirmed. `summary=unconfirmed` lists the two that still steer a
    /// pending recurrence, under their burst (its graph-wide size), oldest
    /// first, with the findings they steer — and the walk shrinks as a human
    /// confirms one and retracts the other.
    #[test]
    fn legacy_burst_declines_read_unconfirmed_and_the_walk_lists_them() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        let beta = finding_on(&k, &findings, "fn beta() {}");
        // ⚠ Three significant fraction digits: the store keeps an
        // xsd:dateTime as a value and serves its CANONICAL form, so `.120Z`
        // would read back as `.12Z`.
        let burst = "2026-09-23T02:42:38.125Z";
        legacy_decline(&store, &format!("{alpha}:decision"), &alpha, burst);
        legacy_decline(
            &store,
            &format!("{beta}:decision"),
            &beta,
            "2026-09-23T02:42:38.400Z",
        );
        legacy_decline(
            &store,
            "urn:iki:finding:elsewhere:decision",
            "urn:iki:finding:elsewhere",
            "2026-09-23T02:42:39.050Z",
        );
        legacy_decline(
            &store,
            "urn:iki:finding:alone:decision",
            "urn:iki:finding:alone",
            "2026-09-23T02:43:10.000Z",
        );
        let row = of(&k, &alpha);
        assert_eq!(row["decision"]["confirmed"], false, "{row}");
        assert_eq!(row["decision"]["burst"], burst);
        assert_eq!(row["decision"]["made"], serde_json::Value::Null);

        let (k, alpha_again, beta_again) = recur(&root, &store);
        let walk = json(&k, "urn:repo:demo:findings", &[("summary", "unconfirmed")]);
        let unconfirmed = &walk["unconfirmed"];
        assert_eq!(unconfirmed["count"], 2, "{walk}");
        assert_eq!(unconfirmed["steered"], 2);
        let groups = unconfirmed["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{walk}");
        assert_eq!(groups[0]["by"], "burst");
        assert_eq!(groups[0]["key"], burst);
        assert_eq!(groups[0]["size"], 3, "the burst is counted graph-wide");
        assert_eq!(groups[0]["first_decided_at"], burst);
        let declines = groups[0]["declines"].as_array().unwrap();
        assert_eq!(declines[0]["iri"], alpha.as_str(), "oldest first");
        assert_eq!(declines[0]["pending"], serde_json::json!([alpha_again]));
        assert_eq!(declines[1]["iri"], beta.as_str());
        assert_eq!(declines[1]["pending"], serde_json::json!([beta_again]));
        // The rows are the listing's, unchanged in shape.
        assert_eq!(
            walk["rows"],
            json(&k, "urn:repo:demo:findings", &[]),
            "{walk}"
        );
        for face in ["text/plain", "text/html"] {
            let out = body(
                &issue(
                    &k,
                    Verb::Source,
                    "urn:repo:demo:findings",
                    &[("summary", "unconfirmed"), ("as", face)],
                )
                .unwrap(),
            );
            assert!(
                out.contains("2 unconfirmed declines still steer 2 pending findings"),
                "{out}"
            );
            assert!(
                out.contains(&format!("burst at {burst} (3 declines")),
                "{out}"
            );
        }
        let html = body(&issue(&k, Verb::Source, &alpha, &[("as", "text/html")]).unwrap());
        assert!(html.contains("browse-finding-unconfirmed"), "{html}");
        assert!(
            html.contains(&format!("name=\"revises\" value=\"{alpha}:decision\"")),
            "{html}"
        );

        // Walk it: confirm alpha with a word, retract beta.
        sink(
            &k,
            &alpha,
            &[
                ("decision", "decline"),
                ("reason", "restates"),
                ("revises", &format!("{alpha}:decision")),
            ],
        )
        .unwrap();
        let walk = json(&k, "urn:repo:demo:findings", &[("summary", "unconfirmed")]);
        assert_eq!(walk["unconfirmed"]["count"], 1, "{walk}");
        sink(
            &k,
            &beta,
            &[
                ("decision", "retract"),
                ("revises", &format!("{beta}:decision")),
            ],
        )
        .unwrap();
        let walk = json(&k, "urn:repo:demo:findings", &[("summary", "unconfirmed")]);
        assert_eq!(walk["unconfirmed"]["count"], 0, "{walk}");
        let plain = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:findings",
                &[("summary", "unconfirmed"), ("as", "text/plain")],
            )
            .unwrap(),
        );
        assert!(plain.contains("no unconfirmed decline steers"), "{plain}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ The loader is PESSIMISTIC and every face runs the burst pass: a
    /// wordless decline with no provenance that is in NO burst reads
    /// `confirmed: true` on the single read, the Sink's json ack, the listing,
    /// a recurrence group's twin and the `prior_decision` of the row it marks
    /// — any face that skipped the pass would say `false` here.
    #[test]
    fn a_lone_decline_reads_confirmed_on_every_face() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        let ack: serde_json::Value = serde_json::from_str(
            &sink(
                &k,
                &alpha,
                &[("decision", "decline"), ("as", "application/json")],
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(ack["decision"]["confirmed"], true, "{ack}");
        assert_eq!(of(&k, &alpha)["decision"]["confirmed"], true);
        let declined = json(&k, "urn:repo:demo:findings", &[("state", "declined")]);
        assert_eq!(declined[0]["decision"]["confirmed"], true, "{declined}");
        let (k, again, _) = recur(&root, &store);
        let pending = json(&k, "urn:repo:demo:findings", &[]);
        let row = pending
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["iri"] == again.as_str())
            .unwrap();
        assert_eq!(row["prior_decision"]["confirmed"], true, "{row}");
        let groups = json(&k, "urn:repo:demo:findings", &[("group", "recurrence")]);
        let twin = groups["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["twin"]["iri"] == alpha.as_str())
            .unwrap_or_else(|| panic!("{groups}"));
        assert_eq!(twin["twin"]["decision"]["confirmed"], true, "{groups}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ The JSON a host reads, pinned by NAME: every decision object —
    /// `decision`, each of `decisions`, `prior_decision` (plus `finding`) —
    /// carries exactly these keys. A rename here is a host's silent break.
    #[test]
    fn the_decision_json_carries_the_fields_a_host_reads() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let findings = pass(&k);
        let alpha = finding_on(&k, &findings, "fn alpha() {}");
        let pending = of(&k, &alpha);
        assert_eq!(pending["decision"], serde_json::Value::Null);
        assert_eq!(pending["decisions"], serde_json::json!([]));
        sink(&k, &alpha, &[("decision", "decline")]).unwrap();
        let keys = |v: &serde_json::Value| -> Vec<String> {
            let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        };
        let expected = [
            "batch",
            "burst",
            "confirmed",
            "decided_at",
            "iri",
            "made",
            "minted",
            "note",
            "outcome",
            "reason",
            "revises",
            "severity",
        ];
        let row = of(&k, &alpha);
        assert_eq!(keys(&row["decision"]), expected);
        assert_eq!(keys(&row["decisions"][0]), expected);
        let (k, again, _) = recur(&root, &store);
        let mut with_finding: Vec<&str> = expected.to_vec();
        with_finding.push("finding");
        with_finding.sort();
        assert_eq!(keys(&of(&k, &again)["prior_decision"]), with_finding);
        std::fs::remove_dir_all(&root).ok();
    }

    /// The decision node and the selectors are DATA, not resources: the
    /// grammar must not match them, or a Sink on a sub-IRI would mint a
    /// decision's decision.
    #[test]
    fn sub_iris_of_a_finding_are_not_resources() {
        let grammar = FindingGrammar::new();
        assert!(grammar
            .match_iri(&Iri::parse("urn:iki:finding:abc".to_string()).unwrap())
            .is_some());
        for sub in [
            "urn:iki:finding:abc:decision",
            "urn:iki:finding:abc:selector:quote",
        ] {
            assert!(
                grammar
                    .match_iri(&Iri::parse(sub.to_string()).unwrap())
                    .is_none(),
                "{sub}"
            );
        }
    }

    // --- only a CONFIRMED decline withholds a repeat (ledger #659) ----------

    /// Edit the file and run the FIRST pass's reply again, verbatim: each item
    /// is an EXACT repeat of the first pass's claim on its line (same quote,
    /// same proposed severity, byte-identical note) — the one shape the mint
    /// may withhold. The pass's json, and the pending findings on alpha's line.
    fn repeat(
        root: &std::path::Path,
        store: &Arc<Store>,
    ) -> (Kernel, serde_json::Value, Vec<serde_json::Value>) {
        std::fs::write(root.join("a.rs"), format!("{CONTENT}// edited\n")).unwrap();
        let k = kernel(root, store);
        let pass = json(&k, "urn:repo:demo:review:a.rs", &[]);
        let pending = json(&k, "urn:repo:demo:findings:a.rs", &[]);
        let on_alpha = pending
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["exact"] == "fn alpha() {}")
            .cloned()
            .collect();
        (k, pass, on_alpha)
    }

    /// ★ A CONFIRMED decline withholds its exact repeat: a decline with a
    /// word, one recorded as made singly, and a wordless legacy decline in no
    /// burst. Each withholds alpha's repeat, counted, and nothing on alpha's
    /// line is pending.
    #[test]
    fn a_confirmed_decline_withholds_its_exact_repeat() {
        type Decide = fn(&Kernel, &Store, &str);
        let cases: [(&str, Decide); 3] = [
            ("with a word", |k, _, alpha| {
                sink(k, alpha, &[("decision", "decline"), ("reason", "misread")]).unwrap();
            }),
            ("made singly", |k, _, alpha| {
                sink(k, alpha, &[("decision", "decline"), ("made", "single")]).unwrap();
            }),
            ("legacy, in no burst", |_, store, alpha| {
                legacy_decline(
                    store,
                    &format!("{alpha}:decision"),
                    alpha,
                    "2026-09-23T02:42:38.125Z",
                );
            }),
        ];
        for (case, decide) in cases {
            let root = demo_root();
            let store = Arc::new(Store::new().unwrap());
            let k = kernel(&root, &store);
            let alpha = finding_on(&k, &pass(&k), "fn alpha() {}");
            decide(&k, &store, &alpha);
            assert_eq!(of(&k, &alpha)["decision"]["confirmed"], true, "{case}");
            let (_, pass, on_alpha) = repeat(&root, &store);
            assert_eq!(pass["suppressed_items"], 1, "{case}: {pass}");
            assert!(
                on_alpha.is_empty(),
                "{case}: withheld, not pending: {on_alpha:?}"
            );
            std::fs::remove_dir_all(&root).ok();
        }
    }

    /// ★★ An UNCONFIRMED decline withholds nothing (Brian, 2026-10-01: "Only
    /// confirmed declines should withhold repeats"). A wordless decline made in
    /// a batch, or — with no provenance on record — in a burst, lets the exact
    /// repeat mint PENDING, marked with that decline as its `prior_decision`
    /// (`confirmed: false`), so it is in the queue and in the
    /// `summary=unconfirmed` walk, where a human confirms or retracts the
    /// decline that would otherwise have hidden it.
    #[test]
    fn an_unconfirmed_decline_lets_its_exact_repeat_mint_pending_and_marked() {
        type Decide = fn(&Kernel, &Store, &str);
        let cases: [(&str, Decide); 2] = [
            ("a wordless batch", |k, _, alpha| {
                sink(
                    k,
                    alpha,
                    &[
                        ("decision", "decline"),
                        ("made", "batch"),
                        ("batch", "recurrence-1"),
                    ],
                )
                .unwrap();
            }),
            ("a legacy burst", |_, store, alpha| {
                legacy_decline(
                    store,
                    &format!("{alpha}:decision"),
                    alpha,
                    "2026-09-23T02:42:38.125Z",
                );
                for (n, at) in [
                    ("one", "2026-09-23T02:42:38.400Z"),
                    ("two", "2026-09-23T02:42:39.050Z"),
                ] {
                    legacy_decline(
                        store,
                        &format!("urn:iki:finding:{n}:decision"),
                        &format!("urn:iki:finding:{n}"),
                        at,
                    );
                }
            }),
        ];
        for (case, decide) in cases {
            let root = demo_root();
            let store = Arc::new(Store::new().unwrap());
            let k = kernel(&root, &store);
            let alpha = finding_on(&k, &pass(&k), "fn alpha() {}");
            decide(&k, &store, &alpha);
            assert_eq!(of(&k, &alpha)["decision"]["confirmed"], false, "{case}");
            let (k, pass, on_alpha) = repeat(&root, &store);
            assert_eq!(pass["suppressed_items"], 0, "{case}: {pass}");
            assert_eq!(on_alpha.len(), 1, "{case}: {on_alpha:?}");
            let again = &on_alpha[0];
            assert_eq!(again["state"], "pending", "{case}");
            assert_eq!(again["body"], "no caller.", "{case}: the exact repeat");
            assert_ne!(again["iri"], alpha.as_str(), "{case}: a new id");
            let prior = &again["prior_decision"];
            assert_eq!(prior["finding"], alpha.as_str(), "{case}: {again}");
            assert_eq!(prior["confirmed"], false, "{case}: {again}");
            let walk = json(&k, "urn:repo:demo:findings", &[("summary", "unconfirmed")]);
            let steered: Vec<&serde_json::Value> = walk["unconfirmed"]["groups"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|g| g["declines"].as_array().unwrap())
                .filter(|d| d["iri"] == alpha.as_str())
                .flat_map(|d| d["pending"].as_array().unwrap())
                .collect();
            assert_eq!(steered, [&again["iri"]], "{case}: {walk}");

            // Confirming the decline is what would have withheld it: the
            // NEXT exact repeat is withheld again.
            sink(
                &k,
                &alpha,
                &[
                    ("decision", "decline"),
                    ("reason", "restates"),
                    ("revises", &format!("{alpha}:decision")),
                ],
            )
            .unwrap();
            std::fs::write(root.join("a.rs"), format!("{CONTENT}// edited twice\n")).unwrap();
            let pass = json(&kernel(&root, &store), "urn:repo:demo:review:a.rs", &[]);
            assert_eq!(pass["suppressed_items"], 1, "{case}: {pass}");
            std::fs::remove_dir_all(&root).ok();
        }
    }

    /// A RETRACTED or REVERSED decline withholds nothing and marks nothing:
    /// the twin's current decision is not a decline, so the repeat mints
    /// pending with no `prior_decision` at all.
    #[test]
    fn a_retracted_or_reversed_decline_withholds_nothing() {
        type Revise = fn(&Kernel, &str);
        let cases: [(&str, Revise); 2] = [
            ("retracted", |k, alpha| {
                sink(
                    k,
                    alpha,
                    &[
                        ("decision", "retract"),
                        ("revises", &format!("{alpha}:decision")),
                    ],
                )
                .unwrap();
            }),
            ("reversed", |k, alpha| {
                sink(
                    k,
                    alpha,
                    &[
                        ("decision", "publish"),
                        ("severity", "minor"),
                        ("revises", &format!("{alpha}:decision")),
                    ],
                )
                .unwrap();
            }),
        ];
        for (case, revise) in cases {
            let root = demo_root();
            let store = Arc::new(Store::new().unwrap());
            let k = kernel(&root, &store);
            let alpha = finding_on(&k, &pass(&k), "fn alpha() {}");
            sink(
                &k,
                &alpha,
                &[
                    ("decision", "decline"),
                    ("reason", "misread"),
                    ("made", "single"),
                ],
            )
            .unwrap();
            revise(&k, &alpha);
            let (_, pass, on_alpha) = repeat(&root, &store);
            assert_eq!(pass["suppressed_items"], 0, "{case}: {pass}");
            let again: Vec<&serde_json::Value> = on_alpha
                .iter()
                .filter(|r| r["iri"] != alpha.as_str())
                .collect();
            assert_eq!(again.len(), 1, "{case}: {on_alpha:?}");
            assert_eq!(again[0]["state"], "pending", "{case}");
            assert_eq!(
                again[0]["prior_decision"],
                serde_json::Value::Null,
                "{case}: {}",
                again[0]
            );
            std::fs::remove_dir_all(&root).ok();
        }
    }
}
