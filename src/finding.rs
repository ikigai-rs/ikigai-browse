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
//! feedback signal rather than churn. And a decision that would CHANGE a
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
/// * `critical` — will bite in production: data loss, a security hole, corruption.
/// * `major` — a real defect or design risk that should be fixed.
/// * `minor` — a small improvement; correctness is not at stake.
/// * `info` — an observation, a question, context worth recording.
/// * `praise` — an earned strength. ★ Not a severity in the usual sense, and
///   deliberately here anyway: the reviewer prompt explicitly asks for one
///   genuine strength, and a set with no bucket for it forces the model to
///   file praise as `info`, after which triage cannot tell a compliment from a
///   note.
pub(crate) const SEVERITIES: [&str; 5] = ["critical", "major", "minor", "info", "praise"];

/// Whether `word` is one of [`SEVERITIES`] — the one place the set is checked.
pub(crate) fn is_severity(word: &str) -> bool {
    SEVERITIES.contains(&word)
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
    format!(
        "<form class=\"browse-finding-decide\" hx-post=\"/k/sink {iri}\" hx-target=\"#browse\" \
         hx-swap=\"innerHTML\">\
         <label class=\"browse-finding-label\">severity \
         <select name=\"severity\">{options}</select></label>\
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
        // ★ A decision is the RECORD, and a record is not overwritten. A
        // repeat of the SAME answer is accepted as a no-op (a double-clicked
        // button must not be an error, and promotion is idempotent anyway);
        // anything that would CHANGE the recorded outcome or rating is
        // refused, naming what is on file. ⚠ The identical repeat keeps the
        // FIRST decision entirely, reason included.
        if let Some(existing) = &finding.decision {
            if existing.outcome == outcome && existing.severity == severity {
                return ack(inv, &finding);
            }
            return Err(Error::InvalidArgument {
                name: "decision".to_string(),
                detail: format!(
                    "`{}` was already {} as `{}` by a human{} — a decision is the record and \
                     is not overwritten. Undo a publication by deleting the annotation it \
                     minted{}.",
                    finding_iri(&id),
                    existing.outcome.label(),
                    existing.severity,
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
        if !["pending", "published", "declined", "all"].contains(&state.as_str()) {
            return Err(Error::InvalidArgument {
                name: "state".to_string(),
                detail: format!(
                    "`{state}` is not a queue state — one of: pending, published, declined, all"
                ),
            });
        }
        let findings: Vec<Annotation> = annotate::list_findings(&self.archive, repo, filter)?
            .into_iter()
            .filter(|f| state == "all" || f.state() == Some(state.as_str()))
            .collect();
        let mut rows =
            annotate::reconcile_findings(inv, &self.archive, &self.roots, repo, findings).await?;
        annotate::sort_finding_rows(&mut rows);

        match inv.inline_str("as").unwrap_or("application/json") {
            t if t.starts_with("text/html") => Ok(repr_utf8(
                "text/html",
                listing_html(repo, &rel, &state, &rows),
            )),
            t if t.starts_with("text/turtle") => {
                let findings: Vec<Annotation> = rows.into_iter().map(|(f, _)| f).collect();
                Ok(repr(
                    "text/turtle",
                    annotate::annotation_turtle_document(&findings),
                ))
            }
            t if t.starts_with("text/plain") => Ok(repr_utf8("text/plain", plain(&rows))),
            _ => {
                let rows: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|(f, line)| annotate::annotation_json(f, *line))
                    .collect();
                Ok(repr(
                    "application/json",
                    serde_json::Value::Array(rows).to_string(),
                ))
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
    }
    if finding.orphaned {
        out.push_str(" [orphaned]");
    } else if finding.reanchored {
        out.push_str(" [re-anchored]");
    }
    out.push_str(&format!(" \"{}\" -- {}", finding.exact, finding.body));
    out
}

fn plain(rows: &[(Annotation, Option<u64>)]) -> String {
    let mut out = format!("--- findings ({}) ---\n{NOT_A_GATE}", rows.len());
    for (finding, line) in rows {
        out.push('\n');
        out.push_str(&plain_row(finding, *line));
    }
    out
}

fn one_html(finding: &Annotation, line: Option<u64>) -> String {
    format!(
        "<div class=\"browse\">{}<p class=\"browse-findings-note\">{NOT_A_GATE}</p>\
         <div class=\"browse-annotations\">{}</div></div>",
        crumbs_html(&finding.repo, &finding.rel),
        annotate::annotation_card_html(finding, line, false),
    )
}

fn listing_html(repo: &str, rel: &str, state: &str, rows: &[(Annotation, Option<u64>)]) -> String {
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&crumbs_html(repo, rel));
    out.push_str(&format!(
        "<p class=\"browse-findings-note\">{NOT_A_GATE}</p>\
         <nav class=\"browse-findings-states\">"
    ));
    for option in ["pending", "published", "declined", "all"] {
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
             a record that a human looked and said no (with the piped reason, if given) — \
             it is never discarded, and a decision is not overwritten by a second one. The \
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
fn findings_description() -> Description {
    Description::new("browse-findings")
        .title("The review queue: findings awaiting a human")
        .summary(
            "Every machine review finding on one file (urn:repo:{repo}:findings:{path}) or \
             on the whole repo (path omitted), in TRIAGE order — severity first, then path \
             and position. ⚠ A queue, not a gate: nothing here blocks a commit, a push or a \
             merge, and nothing reaches the annotation family until a human publishes it \
             (Sink urn:iki:finding:{id}). state= narrows to pending (the default), \
             published, declined, or all. Each read runs the annotation layer's drift pass, \
             so a finding whose file moved is re-anchored (ik:reanchored) and one whose \
             quote is gone is flagged (ik:orphaned) rather than dropped. \
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
                .summary("which part of the pipeline to show")
                .one_of(["pending", "published", "declined", "all"])
                .default_value("pending"),
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
