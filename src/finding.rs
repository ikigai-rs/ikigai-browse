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
//! ## A decision is final, and a decline is a record
//!
//! Declining KEEPS the finding, with the human's reason if they gave one: a
//! re-run then knows a person looked and said no, which is the beginning of a
//! feedback signal rather than churn. The reason has two halves, both
//! optional and kept apart: `reason=` is ONE WORD from [`DECLINE_REASONS`],
//! stored on the decision node as a term (`dcterms:subject
//! <urn:iki:decline-reason:restates>` — an interim predicate, see
//! [`crate::annotate::DCTERMS_SUBJECT`]) so it can be counted; the piped
//! `content` is the free-text note (`dcterms:description`). A word is refused
//! beside `decision=publish` — a publish reason has no consumer. And a decision that would CHANGE a
//! recorded one is REFUSED, naming what is on file — silently overwriting the
//! record is the same failure as overwriting the proposal, one level up. (An
//! identical repeat is a no-op, so a double-clicked button is not an error.) Undoing a publication is `Delete urn:iki:annotation:{id}`, which
//! the annotation family already offers under the same capability: a separate,
//! visible act.
//!
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
    self, Annotation, Decision, Family, Outcome, CAP_ANNOTATE, DECLINE, PUBLISH,
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
            s = esc(&decision.severity),
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
        return out;
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
    // The reason picker: every word of [`DECLINE_REASONS`], its meaning as the
    // option's title, behind an EMPTY default — which the Sink reads as
    // "omitted", so the publish button sharing this form is never refused
    // for it and a decline without a word stays one click.
    let mut reasons = String::from("<option value=\"\" selected>—</option>");
    for (word, meaning) in DECLINE_REASONS.iter().zip(DECLINE_REASON_MEANINGS) {
        reasons.push_str(&format!(
            "<option value=\"{word}\" title=\"{}\">{word}</option>",
            esc(meaning)
        ));
    }
    format!(
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
    )
}

/// The standing note every queue face carries. ⚠ It is not decoration: "queued
/// for review" reads like a gate, and this pipeline gates nothing.
const NOT_A_GATE: &str = "Findings wait for a person. Nothing here blocks a commit, a push or a \
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
pub(crate) const SUMMARIES: [&str; 2] = ["declined", "states"];

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
        let line = annotate::refresh(&self.archive, &mut finding, &current)?;
        face_one(inv, &finding, line)
    }

    /// Sink: the human's answer. `decision=publish` promotes into the
    /// annotation family (this is the ONLY promotion path); `decision=decline`
    /// records that a person looked and said no.
    async fn decide(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let id = Self::id_binding(inv)?;
        let mut finding = self.load_required(&id)?;
        granted(inv, &finding.repo)?;
        let word = inv.inline_str("decision")?.trim().to_string();
        let outcome = match word.as_str() {
            PUBLISH => Outcome::Published,
            DECLINE => Outcome::Declined,
            other => {
                return Err(Error::InvalidArgument {
                    name: "decision".to_string(),
                    detail: format!("`{other}` is not a decision — one of: {PUBLISH}, {DECLINE}"),
                })
            }
        };
        // The final rating: the human's choice, or the model's proposal
        // accepted unchanged. ⚠ The proposal on the finding is never touched
        // by either path.
        let severity = match inv.inline_str("severity").ok().map(str::trim) {
            Some(chosen) if !chosen.is_empty() => {
                if !is_severity(chosen) {
                    return Err(Error::InvalidArgument {
                        name: "severity".to_string(),
                        detail: format!(
                            "`{chosen}` is not a severity — one of: {}",
                            SEVERITIES.join(", ")
                        ),
                    });
                }
                chosen.to_string()
            }
            _ => finding
                .severity
                .clone()
                .ok_or_else(|| Error::InvalidArgument {
                    name: "severity".to_string(),
                    detail: format!(
                        "the model proposed no severity for `{}`, so a decision must state one — \
                     one of: {}",
                        finding_iri(&id),
                        SEVERITIES.join(", ")
                    ),
                })?,
        };
        // Why a decline, in one contract word. An EMPTY value is "omitted",
        // exactly as for `severity`: a form's unselected picker submits one,
        // and a publish button sharing that form must not be refused for it.
        // ⚠ A real word beside a publish IS refused, by name — a publish
        // reason has no consumer, and an argument accepted and then ignored
        // is the failure that is invisible from the caller's side.
        let reason = match inv.inline_str("reason").ok().map(str::trim) {
            Some(word) if !word.is_empty() => {
                if outcome != Outcome::Declined {
                    return Err(Error::InvalidArgument {
                        name: "reason".to_string(),
                        detail: format!(
                            "`reason={word}` says why a finding was declined, and this \
                             decision is {PUBLISH} — drop `reason`, or use decision={DECLINE}"
                        ),
                    });
                }
                if !is_decline_reason(word) {
                    return Err(Error::InvalidArgument {
                        name: "reason".to_string(),
                        detail: format!(
                            "`{word}` is not a decline reason — one of: {}",
                            DECLINE_REASONS.join(", ")
                        ),
                    });
                }
                Some(word.to_string())
            }
            _ => None,
        };
        // ★ A decision is the RECORD, and a record is not overwritten. A
        // repeat of the SAME answer is accepted as a no-op (a double-clicked
        // button must not be an error, and promotion is idempotent anyway);
        // anything that would CHANGE the recorded outcome, rating or reason is
        // refused, naming what is on file. A repeat that states no reason does
        // not contradict one on file; a repeat that states a DIFFERENT one
        // (including a reason where none was recorded) does. ⚠ The identical
        // repeat keeps the FIRST decision entirely, note included.
        if let Some(existing) = &finding.decision {
            let same_reason = reason.is_none() || reason == existing.reason;
            let same_answer = existing.outcome == outcome && existing.severity == severity;
            if same_answer && same_reason {
                return ack(inv, &finding);
            }
            // Name the argument that differs: a repeat that changes only the
            // reason is refused for its `reason`, not for its `decision`.
            let name = match same_answer {
                true => "reason",
                false => "decision",
            };
            return Err(Error::InvalidArgument {
                name: name.to_string(),
                detail: format!(
                    "`{}` was already {} as `{}`{} by a human{} — a decision is the record and \
                     is not overwritten. Undo a publication by deleting the annotation it \
                     minted{}.",
                    finding_iri(&id),
                    existing.outcome.label(),
                    existing.severity,
                    match &existing.reason {
                        Some(word) => format!(" ({word})"),
                        None if existing.outcome == Outcome::Declined =>
                            " (no reason stated)".to_string(),
                        None => String::new(),
                    },
                    existing
                        .at
                        .as_deref()
                        .map(|at| format!(" at {at}"))
                        .unwrap_or_default(),
                    existing
                        .minted
                        .as_deref()
                        .map(|iri| format!(" (`{iri}`)"))
                        .unwrap_or_default(),
                ),
            });
        }

        // Pipeline citizenship: a piped value is the human's reason.
        let note = inv
            .inline_str("content")
            .ok()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string);
        let at = inv.now().map(|t| iso8601(t.as_millis()));

        let minted = match outcome {
            Outcome::Declined => None,
            Outcome::Published => Some(promote(&self.archive, &finding, &severity)?),
        };
        finding.decision = Some(Decision {
            outcome,
            severity,
            at,
            note,
            reason,
            minted,
        });
        annotate::rewrite_annotation(&self.archive, &finding)?;
        ack(inv, &finding)
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
            || summary.as_deref() == Some("states");
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
        let findings: Vec<Annotation> = all
            .into_iter()
            .filter(|f| state == "all" || f.state() == Some(state.as_str()))
            .collect();
        let mut rows =
            annotate::reconcile_findings(inv, &self.archive, &self.roots, repo, findings, contents)
                .await?;
        annotate::sort_finding_rows(&mut rows);
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
                listing_html(repo, &rel, &state, &rows, &declined, hidden.as_deref()),
            )),
            t if t.starts_with("text/turtle") => {
                let findings: Vec<Annotation> = rows.into_iter().map(|(f, _)| f).collect();
                Ok(repr(
                    "text/turtle",
                    annotate::annotation_turtle_document(&findings),
                ))
            }
            t if t.starts_with("text/plain") => Ok(repr_utf8(
                "text/plain",
                plain(&rows, &declined, hidden.as_deref()),
            )),
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

fn plain_row(finding: &Annotation, line: Option<u64>) -> String {
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
        out.push_str(&format!(" -> {}", decision.severity));
        if let Some(reason) = &decision.reason {
            out.push_str(&format!(" ({reason})"));
        }
    }
    if finding.orphaned {
        out.push_str(" [orphaned]");
    } else if finding.reanchored {
        out.push_str(" [re-anchored]");
    }
    if let Some(prior) = &finding.prior {
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
             optional) — it is never discarded, and a decision is not overwritten by a \
             second one. The \
             finding carries the MODEL'S proposed sh:resultSeverity; the decision node \
             carries the human's final one, so both survive and calibration stays a query. \
             Reads run the annotation layer's drift pass (ik:reanchored / ik:orphaned). \
             text/plain (default) is one triage line; as=application/json the full row \
             including both ratings; as=text/html the card with its decision form; \
             as=text/turtle the finding and its decision graph.",
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
                    "the human's answer: publish (mints the annotation) or decline (keeps the \
                     finding as a record). An identical repeat is a no-op; anything that \
                     would CHANGE a recorded decision is refused.",
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
                             re-run knows.",
                        )
                        .one_of([PUBLISH, DECLINE]),
                )
                .input(
                    ArgSpec::new("severity")
                        .class(XSD_STRING)
                        .optional()
                        .summary(
                            "the human's FINAL rating. Omitted = accept the model's proposal \
                             unchanged (required when the model proposed none). ⚠ It never \
                             overwrites the proposal: both are stored, on different nodes.",
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
                    ArgSpec::new("content")
                        .class(XSD_STRING)
                        .optional()
                        .summary(
                            "the human's reason, by pipe or request body — kept on a publish \
                             and on a decline alike",
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
             reason word and its note), so the second decision is one click; \
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
             findings they do not list. \
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
                     superseded them). Without it the json \
                     face is the bare rows array it always was.",
                )
                .one_of(SUMMARIES),
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
        let reply = FINDINGS.to_string();
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
        let cfg = ExplainConfig::new(Arc::clone(store)).review_model_label("r1");
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
        assert_eq!(decision.one_of, [PUBLISH, DECLINE]);
        // Pipeline citizenship: the piped reason is declared.
        assert!(sink.inputs.iter().any(|i| i.name == "content"));

        // The menu renders every declared value and nothing else.
        let menu = decision_html("x", Some("major"), None);
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

        let form = decision_html("x", Some("major"), None);
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
}
