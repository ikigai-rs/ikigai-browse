//! `urn:iki:annotation:{id}` + `urn:repo:{repo}:annotations[:{path}]` — W3C **Web
//! Annotation** (`oa:`) annotations on browse resources (S2), stored in the
//! SAME shared Oxigraph store as the explanation archive: explanations and
//! annotations are one graph, queryable together.
//!
//! ## The annotation graph (skolemized)
//!
//! Vanilla `oa:` loves blank nodes (the target and selector nodes of the W3C
//! model are conventionally anonymous). The house deviation: every node is a
//! stable IRI, and the intermediate `oa:hasTarget` node is flattened away —
//! `ik:annotates` points straight at the annotated browse resource. The `oa:`
//! *property* names are kept.
//!
//! ```turtle
//! <urn:iki:annotation:{id}> a oa:Annotation ;
//!     oa:bodyValue "the note text" ;
//!     ik:annotates <urn:repo:demo:file:src/lib.rs> ;
//!     ik:repo "demo" ; ik:path "src/lib.rs" ;
//!     ik:contentHash "sha256:…" ;              # the file version annotated
//!     oa:hasSelector <urn:iki:annotation:{id}:selector:quote> ,
//!                    <urn:iki:annotation:{id}:selector:position> ;
//!     dcterms:created "2026-08-08T17:00:00.000Z"^^xsd:dateTime .
//!
//! <urn:iki:annotation:{id}:selector:quote> a oa:TextQuoteSelector ;
//!     oa:prefix "…" ; oa:exact "the quoted text" ; oa:suffix "…" .
//!
//! <urn:iki:annotation:{id}:selector:position> a oa:TextPositionSelector ;
//!     oa:start "120"^^xsd:nonNegativeInteger ;
//!     oa:end "135"^^xsd:nonNegativeInteger .
//! ```
//!
//! `oa:start`/`oa:end` are CHARACTER offsets into the target's UTF-8 text (the
//! W3C model's counting). `ik:reanchored true` marks selectors that were
//! re-derived after drift; `ik:orphaned true` marks annotations whose quote is
//! gone from the current content. Both flags are stored only when true. v1 is
//! single-user: no authorship triples (the passkey→workspace arc adds them).
//!
//! ## Ids
//!
//! `Sink urn:iki:annotation:{id}` creates (or updates) under a CALLER-SUPPLIED
//! slug (`[A-Za-z0-9._~-]+`); `Sink urn:iki:annotation` (no id) MINTS a v4 uuid
//! and the acknowledgement names the new IRI. Source/Delete require the id.
//!
//! ## Anchoring and re-anchoring under drift
//!
//! A Sink anchors `exact` in the target's CURRENT content (sourced through
//! the kernel): occurrences are scored by how much of the caller's
//! `prefix`/`suffix` context matches, the best score wins, and on a tie the
//! FIRST (lowest-offset) occurrence wins — deterministic by construction. The
//! stored quote selector always carries context DERIVED from the anchored
//! occurrence (up to 32 characters each side), which is what future
//! re-anchoring matches against; caller-supplied `prefix`/`suffix` serve only
//! to disambiguate the initial anchor.
//!
//! ⚠ One concession, and only one: a quote that misses is retried ONCE with a
//! leading run of non-alphanumeric characters stripped (the marker a model
//! decorates a quote with — ledger #488), and the text then RECORDED is the
//! target's own characters, not the caller's. Everything after that leading
//! run must still match character-for-character; the anchor is the proof the
//! quote came off the file.
//!
//! On every Source/list, when the target's current hash differs from the
//! annotation's recorded `ik:contentHash`, the quote is re-searched in the new
//! content (same scoring). Found → BOTH selectors and the recorded hash are
//! updated in the store and the annotation is marked `ik:reanchored true`.
//! Gone (or the file itself gone/binary) → `ik:orphaned true`, and the
//! annotation still renders, flagged, against its recorded positions — NEVER
//! silently dropped. An orphan is re-searched on later reads (content may be
//! restored), but the store is only touched when something actually changes,
//! so repeat reads of an orphan do not re-flag or churn the graph.
//!
//! ## Capabilities (per-verb — the multi-verb rule)
//!
//! Source requires `urn:cap:browse:read:*` (wildcard offering; the target's
//! root is checked against the grant, like every browse read). Sink and
//! Delete require `urn:cap:annotate`. Sink ADDITIONALLY declares the browse
//! wildcard: anchoring reads the target through the kernel, and attenuation
//! makes that structural — a capability that cannot read the target cannot
//! annotate it. Declared = enforced: the kernel baseline-checks each verb's
//! `requires` before dispatch.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgSpec, Bindings, Description, Endpoint, EndpointSpace, Error, Grammar,
    Invocation, Iri, Representation, Result, UriTemplate, Verb,
};
use oxigraph::model::{Literal, NamedNode, Quad, Term};
use sha2::{Digest, Sha256};

use crate::archive::Archive;
use crate::explain::{ik, iso8601, parse_iri, IK};
use crate::{
    crumbs_html, esc, file_iri, granted, iri_decode, path_binding, repo_root, repr, repr_utf8,
    ttl_str, Roots, CAP_WILDCARD,
};

/// The capability Sink and Delete require: authority to create, update, and
/// remove annotations. A literal scope (not parameterized) in v1.
pub const CAP_ANNOTATE: &str = "urn:cap:annotate";

const OA: &str = "http://www.w3.org/ns/oa#";
const DCTERMS_CREATED: &str = "http://purl.org/dc/terms/created";
/// Machine provenance (S4, the review layer) — all STANDARD terms, no vocab
/// publish needed: `dcterms:creator` carries the model identity on
/// machine-minted annotations, `oa:motivatedBy` distinguishes the review's
/// `oa:assessing` from the human `oa:commenting`, and `prov:wasGeneratedBy`
/// links a machine annotation back to the review pass that minted it.
const DCTERMS_CREATOR: &str = "http://purl.org/dc/terms/creator";
pub(crate) const PROV: &str = "http://www.w3.org/ns/prov#";
const PROV_WAS_GENERATED_BY: &str = "http://www.w3.org/ns/prov#wasGeneratedBy";
/// `prov:wasDerivedFrom` — a PUBLISHED annotation back to the pending finding
/// a human promoted it from (entity to entity, which is what PROV's derivation
/// is for). Absent on every annotation that was never a finding.
pub(crate) const PROV_WAS_DERIVED_FROM: &str = "http://www.w3.org/ns/prov#wasDerivedFrom";
pub(crate) const PROV_USED: &str = "http://www.w3.org/ns/prov#used";
pub(crate) const PROV_GENERATED: &str = "http://www.w3.org/ns/prov#generated";

/// The finding family's body predicate.
///
/// ⚠ **NOT `oa:bodyValue`, and the reason is entailment, not style.** The W3C
/// Web Annotation vocabulary gives `oa:bodyValue` (and `oa:motivatedBy`)
/// `rdfs:domain oa:Annotation`: writing either onto a pending finding would
/// type it INTO the annotation family under any reasoner, which is precisely
/// what [`Family::Finding`] exists to prevent. No code path would have shown
/// it. `dcterms:description` is domain-free and says the same thing.
pub(crate) const DCTERMS_DESCRIPTION: &str = "http://purl.org/dc/terms/description";
/// `dcterms:type` — the decision node's outcome, one of
/// [`Outcome::iri`]'s two values.
pub(crate) const DCTERMS_TYPE: &str = "http://purl.org/dc/terms/type";

/// `sh:resultSeverity` — the severity of an assessment result.
///
/// ★ A PUBLISHED term for exactly this concept, chosen over inventing
/// `ik:severity` because the vocabulary lives in `ikigai-core` and a browse arc
/// cannot add to it (the conformance walk's `VOCABULARY` check refuses an `ik:`
/// term the published vocabulary does not define, and rightly). SHACL's own
/// three severities are instances of `sh:Severity` and the spec allows more, so
/// the `urn:iki:severity:*` values below sit legally beside `sh:Violation`.
/// The `ik:severity` / `ik:Finding` vocabulary need is reported up.
pub(crate) const SH_RESULT_SEVERITY: &str = "http://www.w3.org/ns/shacl#resultSeverity";

/// The `oa:motivatedBy` value the human Sink stamps.
const MOTIVATION_HUMAN: &str = "commenting";
/// The `oa:motivatedBy` value a PUBLISHED review finding carries.
///
/// ★ Still `assessing` after a human publishes it, deliberately: a person
/// vouched for the claim, they did not write it. Only [`MOTIVATION_HUMAN`]
/// means "a human's own words".
pub(crate) const MOTIVATION_REVIEW: &str = "assessing";

/// The two decision words, one place: the Sink's `one_of`, the form buttons'
/// values, and the match that reads them.
pub(crate) const PUBLISH: &str = "publish";
pub(crate) const DECLINE: &str = "decline";

/// How much context the stored quote selector carries on each side of the
/// exact quote (characters). Part of the re-anchoring contract.
const CONTEXT_CHARS: usize = 32;

fn oa(term: &str) -> NamedNode {
    NamedNode::new(format!("{OA}{term}")).expect("oa terms are valid IRIs")
}

// --- the two families -------------------------------------------------------

/// Which FAMILY a stored anchored note belongs to.
///
/// ★ The two families share this record, this store, this anchoring and this
/// drift pass — and **nothing else**. A machine review pass writes only
/// [`Family::Finding`]; the only way into [`Family::Annotation`] is a human
/// publishing a finding (`crate::finding`) or a human Sink of
/// `urn:iki:annotation` (ledger #444: *"nothing gets published to Gonk except
/// by the human"*).
///
/// ⚠ The separation is carried by BOTH discriminators every reader here keys
/// on — the IRI prefix and `rdf:type` — so neither `list_annotations`
/// (`a oa:Annotation`) nor `list_annotations_for_target` (the
/// `urn:iki:annotation:` prefix) can see a finding even by accident.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Family {
    /// `urn:iki:annotation:{id}`, `a oa:Annotation` — the published family
    /// every existing reader and query already sees.
    Annotation,
    /// `urn:iki:finding:{id}`, `a prov:Entity` — a machine claim AWAITING a
    /// human. Never typed `oa:Annotation`, never under the annotation prefix,
    /// and never carrying an `oa:` term whose domain is `oa:Annotation`.
    Finding,
}

impl Family {
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            Family::Annotation => "urn:iki:annotation:",
            Family::Finding => "urn:iki:finding:",
        }
    }

    /// The family an IRI belongs to, with its id — `None` for anything else.
    /// Checked longest-prefix-free: the two prefixes share no prefix.
    pub(crate) fn split(iri: &str) -> Option<(Family, &str)> {
        for family in [Family::Annotation, Family::Finding] {
            if let Some(id) = iri.strip_prefix(family.prefix()) {
                return Some((family, id));
            }
        }
        None
    }
}

// --- IRIs -------------------------------------------------------------------

pub(crate) fn record_iri(family: Family, id: &str) -> String {
    format!("{}{id}", family.prefix())
}

pub(crate) fn annotation_iri(id: &str) -> String {
    record_iri(Family::Annotation, id)
}

fn quote_iri(family: Family, id: &str) -> String {
    format!("{}:selector:quote", record_iri(family, id))
}

fn position_iri(family: Family, id: &str) -> String {
    format!("{}:selector:position", record_iri(family, id))
}

/// The human act on a pending finding — its own node, so the model's proposal
/// on the finding is never overwritten by it.
pub(crate) fn decision_iri(id: &str) -> String {
    format!("{}:decision", record_iri(Family::Finding, id))
}

/// Caller-supplied slugs must embed cleanly in the URN (and must not collide
/// with the `:selector:` sub-IRIs, which a `:` would).
fn validate_id(id: &str) -> Result<String> {
    let ok = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b));
    if !ok {
        return Err(Error::InvalidArgument {
            name: "id".to_string(),
            detail: format!("`{id}` is not a valid annotation id ([A-Za-z0-9._~-]+)"),
        });
    }
    Ok(id.to_string())
}

/// A Sink's parsed target: one file, or one pull request (its diff text is
/// the anchor surface).
enum SinkTarget {
    File { repo: String, rel: String },
    Pr { repo: String, number: u64 },
}

/// The annotated browse resource: `urn:repo:{repo}:file:{path}` or
/// `urn:repo:{repo}:pr:{n}`, where `{repo}` is a configured root. Both are
/// text surfaces — a file's content, a PR's unified diff; the line anchors
/// and quote selectors are text-content concepts either way.
fn parse_target(target: &str, roots: &BTreeMap<String, std::path::PathBuf>) -> Result<SinkTarget> {
    let bad = |detail: String| Error::InvalidArgument {
        name: "target".to_string(),
        detail,
    };
    let rest = target.strip_prefix("urn:repo:").ok_or_else(|| {
        bad(format!(
            "`{target}` is not a urn:repo:{{repo}}:file:{{path}} or urn:repo:{{repo}}:pr:{{n}} IRI"
        ))
    })?;
    let (repo, rest) = rest
        .split_once(':')
        .ok_or_else(|| bad(format!("`{target}` carries no path")))?;
    if !roots.contains_key(repo) {
        return Err(bad(format!("`{repo}` is not a configured root")));
    }
    if let Some(number) = rest.strip_prefix("pr:") {
        // The PR page itself, digits only — `pr:{n}:explain` and friends are
        // derived layers, not annotation surfaces.
        let number: u64 = number.parse().map_err(|_| {
            bad(format!(
                "`{target}` is not an annotatable pull-request page (urn:repo:{{repo}}:pr:{{n}})"
            ))
        })?;
        return Ok(SinkTarget::Pr {
            repo: repo.to_string(),
            number,
        });
    }
    let rel_encoded = rest.strip_prefix("file:").ok_or_else(|| {
        bad("only file and pull-request resources are annotatable \
             (urn:repo:{repo}:file:{path} or urn:repo:{repo}:pr:{n})"
            .to_string())
    })?;
    let rel = iri_decode(rel_encoded)?;
    if rel.is_empty() {
        return Err(bad(format!("`{target}` carries no path")));
    }
    Ok(SinkTarget::File {
        repo: repo.to_string(),
        rel,
    })
}

// --- the annotation record --------------------------------------------------

/// What a human decided about a pending finding.
///
/// ★ Its own node (`urn:iki:finding:{id}:decision`), and that is the whole
/// point: the model's proposal lives on the FINDING and the human's final
/// rating lives HERE, so the two coexist and "is this reviewer calibrated?"
/// stays a query rather than an impression (ledger #444, comment 2). One
/// field that the human overwrote would destroy the only signal that could
/// ever answer it, unrecoverably, on the first write.
#[derive(Clone, Debug)]
pub(crate) struct Decision {
    pub(crate) outcome: Outcome,
    /// The FINAL severity — the human's re-rating, or the model's proposal
    /// accepted unchanged. Always present: a decision states a rating.
    pub(crate) severity: String,
    pub(crate) at: Option<String>,
    /// The human's reason, if they gave one (the Sink's piped `content`).
    /// ★ A declined finding with a reason is the beginning of a feedback
    /// signal; a discarded one is churn.
    pub(crate) note: Option<String>,
    /// The annotation minted on publish — `None` for a decline, which is
    /// exactly what makes a decline a RECORD rather than a deletion.
    pub(crate) minted: Option<String>,
}

/// The two ways a human can answer a pending finding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Outcome {
    Published,
    Declined,
}

impl Outcome {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Outcome::Published => "published",
            Outcome::Declined => "declined",
        }
    }

    pub(crate) fn iri(self) -> String {
        format!("urn:iki:finding:outcome:{}", self.label())
    }

    pub(crate) fn from_iri(iri: &str) -> Option<Outcome> {
        match iri.strip_prefix("urn:iki:finding:outcome:")? {
            "published" => Some(Outcome::Published),
            "declined" => Some(Outcome::Declined),
            _ => None,
        }
    }
}

/// One anchored note, as stored (and as served) — an annotation or a pending
/// finding, told apart by [`Annotation::family`]. `start`/`end` are character
/// offsets into the target's text at `hash`.
#[derive(Clone, Debug)]
pub(crate) struct Annotation {
    /// Which family this record is in. ⚠ Every IRI it owns derives from this;
    /// a record loaded from one family and stored into the other would MOVE
    /// it, which is what promotion does deliberately and nothing else may.
    pub(crate) family: Family,
    pub(crate) id: String,
    pub(crate) body: String,
    pub(crate) target_iri: String,
    pub(crate) repo: String,
    pub(crate) rel: String,
    pub(crate) hash: String,
    pub(crate) prefix: String,
    pub(crate) exact: String,
    pub(crate) suffix: String,
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) created: Option<String>,
    pub(crate) reanchored: bool,
    pub(crate) orphaned: bool,
    /// `dcterms:creator` — the model identity on machine-minted annotations.
    /// `Some` IS the machine/human discriminator: human annotations never
    /// carry a creator (v1 is single-user; the passkey→workspace arc will add
    /// human authorship on a different axis).
    pub(crate) creator: Option<String>,
    /// `oa:motivatedBy` (the short term: `commenting` / `assessing`). Absent
    /// on pre-S4 stores — read compatibility, never a discriminator.
    pub(crate) motivation: Option<String>,
    /// `prov:wasGeneratedBy` — the review pass entry that minted this
    /// annotation (machine annotations only).
    pub(crate) generated_by: Option<String>,
    /// `sh:resultSeverity`, as a bare severity name (`critical` … `praise`).
    ///
    /// ★ It means a DIFFERENT thing per family, and that is the design: on a
    /// [`Family::Finding`] it is the MODEL'S PROPOSAL and is never rewritten;
    /// on a [`Family::Annotation`] it is the HUMAN'S FINAL rating, copied in
    /// at promotion. Both survive, one hop apart along
    /// `prov:wasDerivedFrom`.
    pub(crate) severity: Option<String>,
    /// `prov:wasDerivedFrom` — the pending finding a published annotation was
    /// promoted from. Annotation family only.
    pub(crate) derived_from: Option<String>,
    /// The human act. Finding family only; `None` IS the pending state.
    pub(crate) decision: Option<Decision>,
}

/// What an annotation's recorded target IS — derived from the stored
/// `ik:annotates` IRI, never a separate triple: a file's content, or a pull
/// request's diff.
pub(crate) enum TargetRef<'a> {
    File(&'a str),
    Pr(u64),
}

impl Annotation {
    pub(crate) fn iri(&self) -> String {
        record_iri(self.family, &self.id)
    }

    /// Where a finding is in the human pipeline: `pending` until someone
    /// answers it, then the decision's outcome. `None` for an annotation —
    /// an annotation IS the published state, it does not have one.
    pub(crate) fn state(&self) -> Option<&'static str> {
        match self.family {
            Family::Annotation => None,
            Family::Finding => Some(match &self.decision {
                None => "pending",
                Some(d) => d.outcome.label(),
            }),
        }
    }

    /// The rating that governs: the human's, once there is one.
    pub(crate) fn effective_severity(&self) -> Option<&str> {
        match &self.decision {
            Some(d) => Some(d.severity.as_str()),
            None => self.severity.as_deref(),
        }
    }

    fn machine(&self) -> bool {
        self.creator.is_some()
    }

    /// Parse the recorded target IRI. Anything that is not this repo's
    /// `pr:{n}` page reads as a file target at the recorded path — including
    /// legacy records, whose `ik:annotates` always named a file.
    pub(crate) fn target_ref(&self) -> TargetRef<'_> {
        if let Some(number) = self
            .target_iri
            .strip_prefix("urn:repo:")
            .and_then(|rest| rest.strip_prefix(&self.repo))
            .and_then(|rest| rest.strip_prefix(":pr:"))
            .and_then(|n| n.parse::<u64>().ok())
        {
            return TargetRef::Pr(number);
        }
        TargetRef::File(&self.rel)
    }

    /// The PR number, for targets that are PR pages.
    fn pr_number(&self) -> Option<u64> {
        match self.target_ref() {
            TargetRef::Pr(n) => Some(n),
            TargetRef::File(_) => None,
        }
    }

    /// Where this annotation lives, for display: the file path, or `pr#{n}`.
    fn place(&self) -> String {
        match self.target_ref() {
            TargetRef::Pr(n) => format!("pr#{n}"),
            TargetRef::File(rel) => rel.to_string(),
        }
    }
}

fn store_err(e: impl std::fmt::Display) -> Error {
    Error::Endpoint(format!("browse: annotation store: {e}"))
}

/// Insert the annotation's quads (the annotation node plus both selector
/// nodes). Flags are stored only when true — absence means false.
pub(crate) fn store_annotation(archive: &Archive, ann: &Annotation) -> Result<()> {
    use oxigraph::model::vocab::{rdf, xsd};
    let subject = NamedNode::new(ann.iri()).map_err(store_err)?;
    let quote = NamedNode::new(quote_iri(ann.family, &ann.id)).map_err(store_err)?;
    let position = NamedNode::new(position_iri(ann.family, &ann.id)).map_err(store_err)?;
    let target = NamedNode::new(&ann.target_iri).map_err(store_err)?;
    let g = archive.graph().clone();
    // ⚠ The class and the body predicate are the whole family boundary. A
    // finding is `prov:Entity` + `dcterms:description`; `oa:Annotation` +
    // `oa:bodyValue` would put an unapproved machine claim where every
    // existing reader and query already looks.
    let (class, body_predicate) = match ann.family {
        Family::Annotation => (oa("Annotation"), oa("bodyValue")),
        Family::Finding => (
            NamedNode::new(format!("{PROV}Entity")).map_err(store_err)?,
            NamedNode::new(DCTERMS_DESCRIPTION).map_err(store_err)?,
        ),
    };
    let mut quads: Vec<Quad> = vec![
        Quad::new(subject.clone(), rdf::TYPE, class, g.clone()),
        Quad::new(
            subject.clone(),
            body_predicate,
            Literal::new_simple_literal(&ann.body),
            g.clone(),
        ),
        Quad::new(subject.clone(), ik("annotates"), target, g.clone()),
        Quad::new(
            subject.clone(),
            ik("repo"),
            Literal::new_simple_literal(&ann.repo),
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            ik("contentHash"),
            Literal::new_simple_literal(&ann.hash),
            g.clone(),
        ),
        Quad::new(subject.clone(), oa("hasSelector"), quote.clone(), g.clone()),
        Quad::new(
            subject.clone(),
            oa("hasSelector"),
            position.clone(),
            g.clone(),
        ),
        Quad::new(quote.clone(), rdf::TYPE, oa("TextQuoteSelector"), g.clone()),
        Quad::new(
            quote.clone(),
            oa("exact"),
            Literal::new_simple_literal(&ann.exact),
            g.clone(),
        ),
        Quad::new(
            position.clone(),
            rdf::TYPE,
            oa("TextPositionSelector"),
            g.clone(),
        ),
        Quad::new(
            position.clone(),
            oa("start"),
            Literal::new_typed_literal(ann.start.to_string(), xsd::NON_NEGATIVE_INTEGER),
            g.clone(),
        ),
        Quad::new(
            position,
            oa("end"),
            Literal::new_typed_literal(ann.end.to_string(), xsd::NON_NEGATIVE_INTEGER),
            g.clone(),
        ),
    ];
    // A PR annotation carries no path (its target IRI is the record) — the
    // triple is written only when there is one.
    if !ann.rel.is_empty() {
        quads.push(Quad::new(
            subject.clone(),
            ik("path"),
            Literal::new_simple_literal(&ann.rel),
            g.clone(),
        ));
    }
    if !ann.prefix.is_empty() {
        quads.push(Quad::new(
            quote.clone(),
            oa("prefix"),
            Literal::new_simple_literal(&ann.prefix),
            g.clone(),
        ));
    }
    if !ann.suffix.is_empty() {
        quads.push(Quad::new(
            quote,
            oa("suffix"),
            Literal::new_simple_literal(&ann.suffix),
            g.clone(),
        ));
    }
    if let Some(at) = &ann.created {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(DCTERMS_CREATED).map_err(store_err)?,
            Literal::new_typed_literal(at, xsd::DATE_TIME),
            g.clone(),
        ));
    }
    if ann.reanchored {
        quads.push(Quad::new(
            subject.clone(),
            ik("reanchored"),
            Literal::new_typed_literal("true", xsd::BOOLEAN),
            g.clone(),
        ));
    }
    if ann.orphaned {
        quads.push(Quad::new(
            subject.clone(),
            ik("orphaned"),
            Literal::new_typed_literal("true", xsd::BOOLEAN),
            g.clone(),
        ));
    }
    // ⚠ ANNOTATION FAMILY ONLY. `oa:motivatedBy` carries `rdfs:domain
    // oa:Annotation` in the W3C vocabulary, so stamping it on a finding would
    // type the finding into the annotation family under entailment — no code
    // path here would ever have shown it. A finding is machine by
    // construction (it carries the model as `dcterms:creator`) and needs no
    // motivation triple to say so.
    if let (Family::Annotation, Some(motivation)) = (ann.family, &ann.motivation) {
        quads.push(Quad::new(
            subject.clone(),
            oa("motivatedBy"),
            oa(motivation),
            g.clone(),
        ));
    }
    if let Some(creator) = &ann.creator {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(DCTERMS_CREATOR).map_err(store_err)?,
            Literal::new_simple_literal(creator),
            g.clone(),
        ));
    }
    if let Some(pass) = &ann.generated_by {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(PROV_WAS_GENERATED_BY).map_err(store_err)?,
            NamedNode::new(pass).map_err(store_err)?,
            g.clone(),
        ));
    }
    if let Some(severity) = &ann.severity {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(SH_RESULT_SEVERITY).map_err(store_err)?,
            NamedNode::new(severity_iri(severity)).map_err(store_err)?,
            g.clone(),
        ));
    }
    if let Some(from) = &ann.derived_from {
        quads.push(Quad::new(
            subject.clone(),
            NamedNode::new(PROV_WAS_DERIVED_FROM).map_err(store_err)?,
            NamedNode::new(from).map_err(store_err)?,
            g.clone(),
        ));
    }
    // The human act, on its own node — never folded into the finding, whose
    // triples are the MODEL's and stay the model's.
    if let (Family::Finding, Some(decision)) = (ann.family, &ann.decision) {
        let node = NamedNode::new(decision_iri(&ann.id)).map_err(store_err)?;
        quads.push(Quad::new(
            node.clone(),
            rdf::TYPE,
            NamedNode::new(format!("{PROV}Activity")).map_err(store_err)?,
            g.clone(),
        ));
        quads.push(Quad::new(
            node.clone(),
            NamedNode::new(PROV_USED).map_err(store_err)?,
            subject,
            g.clone(),
        ));
        quads.push(Quad::new(
            node.clone(),
            NamedNode::new(DCTERMS_TYPE).map_err(store_err)?,
            NamedNode::new(decision.outcome.iri()).map_err(store_err)?,
            g.clone(),
        ));
        quads.push(Quad::new(
            node.clone(),
            NamedNode::new(SH_RESULT_SEVERITY).map_err(store_err)?,
            NamedNode::new(severity_iri(&decision.severity)).map_err(store_err)?,
            g.clone(),
        ));
        if let Some(at) = &decision.at {
            quads.push(Quad::new(
                node.clone(),
                NamedNode::new(DCTERMS_CREATED).map_err(store_err)?,
                Literal::new_typed_literal(at, xsd::DATE_TIME),
                g.clone(),
            ));
        }
        if let Some(note) = &decision.note {
            quads.push(Quad::new(
                node.clone(),
                NamedNode::new(DCTERMS_DESCRIPTION).map_err(store_err)?,
                Literal::new_simple_literal(note),
                g.clone(),
            ));
        }
        if let Some(minted) = &decision.minted {
            quads.push(Quad::new(
                node,
                NamedNode::new(PROV_GENERATED).map_err(store_err)?,
                NamedNode::new(minted).map_err(store_err)?,
                g,
            ));
        }
    }
    for quad in &quads {
        archive.insert(quad).map_err(store_err)?;
    }
    Ok(())
}

/// The IRI of a severity value. ⚠ An OBJECT, not a vocabulary term: the
/// conformance walk checks predicates and `rdf:type` objects, and a severity
/// is data. See [`SH_RESULT_SEVERITY`] for why the predicate is SHACL's.
pub(crate) fn severity_iri(name: &str) -> String {
    format!("urn:iki:severity:{name}")
}

/// Remove every quad under the record's subjects — the record, both
/// selectors, and (finding family) its decision node.
pub(crate) fn remove_annotation(archive: &Archive, family: Family, id: &str) -> Result<()> {
    let mut subjects = vec![
        record_iri(family, id),
        quote_iri(family, id),
        position_iri(family, id),
    ];
    if family == Family::Finding {
        subjects.push(decision_iri(id));
    }
    for iri in subjects {
        let subject = NamedNode::new(&iri).map_err(store_err)?;
        let quads: Vec<Quad> = archive
            .quads_for_pattern(Some(subject.as_ref().into()), None, None)
            .collect::<std::result::Result<_, _>>()
            .map_err(store_err)?;
        for quad in &quads {
            archive.remove(quad).map_err(store_err)?;
        }
    }
    Ok(())
}

/// Replace the annotation's stored state (the update path and the
/// re-anchor/orphan persistence path).
pub(crate) fn rewrite_annotation(archive: &Archive, ann: &Annotation) -> Result<()> {
    remove_annotation(archive, ann.family, &ann.id)?;
    store_annotation(archive, ann)
}

/// Load one annotation by id — `None` when the store holds no `oa:bodyValue`
/// for it.
pub(crate) fn load_annotation(archive: &Archive, id: &str) -> Result<Option<Annotation>> {
    load_record(archive, Family::Annotation, id)
}

/// Load one record of either family by id — `None` when the store holds no
/// body for it (`oa:bodyValue` / `dcterms:description`, per family).
pub(crate) fn load_record(
    archive: &Archive,
    family: Family,
    id: &str,
) -> Result<Option<Annotation>> {
    let mut ann = Annotation {
        family,
        id: id.to_string(),
        body: String::new(),
        target_iri: String::new(),
        repo: String::new(),
        rel: String::new(),
        hash: String::new(),
        prefix: String::new(),
        exact: String::new(),
        suffix: String::new(),
        start: 0,
        end: 0,
        created: None,
        reanchored: false,
        orphaned: false,
        creator: None,
        motivation: None,
        generated_by: None,
        severity: None,
        derived_from: None,
        decision: None,
    };
    let literal = |term: &Term| match term {
        Term::Literal(l) => l.value().to_string(),
        other => other.to_string(),
    };
    let mut found = false;
    let subject = match NamedNode::new(record_iri(family, id)) {
        Ok(node) => node,
        Err(_) => return Ok(None),
    };
    for quad in archive.quads_for_pattern(Some(subject.as_ref().into()), None, None) {
        let quad = quad.map_err(store_err)?;
        let predicate = quad.predicate.as_str();
        if predicate == DCTERMS_DESCRIPTION && family == Family::Finding {
            ann.body = literal(&quad.object);
            found = true;
        } else if predicate == SH_RESULT_SEVERITY {
            if let Term::NamedNode(node) = &quad.object {
                ann.severity = node
                    .as_str()
                    .strip_prefix("urn:iki:severity:")
                    .map(str::to_string);
            }
        } else if predicate == PROV_WAS_DERIVED_FROM {
            if let Term::NamedNode(node) = &quad.object {
                ann.derived_from = Some(node.as_str().to_string());
            }
        } else if let Some(term) = predicate.strip_prefix(OA) {
            match term {
                "bodyValue" if family == Family::Annotation => {
                    ann.body = literal(&quad.object);
                    found = true;
                }
                "motivatedBy" => {
                    if let Term::NamedNode(node) = &quad.object {
                        if let Some(short) = node.as_str().strip_prefix(OA) {
                            ann.motivation = Some(short.to_string());
                        }
                    }
                }
                _ => {}
            }
        } else if let Some(term) = predicate.strip_prefix(IK) {
            match term {
                "repo" => ann.repo = literal(&quad.object),
                "path" => ann.rel = literal(&quad.object),
                "contentHash" => ann.hash = literal(&quad.object),
                "reanchored" => ann.reanchored = literal(&quad.object) == "true",
                "orphaned" => ann.orphaned = literal(&quad.object) == "true",
                "annotates" => {
                    if let Term::NamedNode(node) = &quad.object {
                        ann.target_iri = node.as_str().to_string();
                    }
                }
                // Legacy term (pre-0.2.2 stores wrote ik:target; the routing
                // family owns that term now). Read-only compatibility: any
                // rewrite (update, re-anchor, orphan) re-stores the graph
                // with ik:annotates; ik:annotates wins if both are present.
                "target" if ann.target_iri.is_empty() => {
                    if let Term::NamedNode(node) = &quad.object {
                        ann.target_iri = node.as_str().to_string();
                    }
                }
                _ => {}
            }
        } else if predicate == DCTERMS_CREATED {
            ann.created = Some(literal(&quad.object));
        } else if predicate == DCTERMS_CREATOR {
            ann.creator = Some(literal(&quad.object));
        } else if predicate == PROV_WAS_GENERATED_BY {
            if let Term::NamedNode(node) = &quad.object {
                ann.generated_by = Some(node.as_str().to_string());
            }
        }
    }
    if !found {
        return Ok(None);
    }
    if family == Family::Finding {
        ann.decision = load_decision(archive, id)?;
    }
    for (iri, is_quote) in [
        (quote_iri(family, id), true),
        (position_iri(family, id), false),
    ] {
        let subject = NamedNode::new(&iri).map_err(store_err)?;
        for quad in archive.quads_for_pattern(Some(subject.as_ref().into()), None, None) {
            let quad = quad.map_err(store_err)?;
            match (is_quote, quad.predicate.as_str().strip_prefix(OA)) {
                (true, Some("prefix")) => ann.prefix = literal(&quad.object),
                (true, Some("exact")) => ann.exact = literal(&quad.object),
                (true, Some("suffix")) => ann.suffix = literal(&quad.object),
                (false, Some("start")) => ann.start = literal(&quad.object).parse().unwrap_or(0),
                (false, Some("end")) => ann.end = literal(&quad.object).parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    Ok(Some(ann))
}

/// The human act on a finding, if there has been one. `None` IS "pending" —
/// the absence of a decision node, not a flag on the finding.
fn load_decision(archive: &Archive, id: &str) -> Result<Option<Decision>> {
    let subject = match NamedNode::new(decision_iri(id)) {
        Ok(node) => node,
        Err(_) => return Ok(None),
    };
    let mut outcome = None;
    let mut severity = None;
    let mut at = None;
    let mut note = None;
    let mut minted = None;
    for quad in archive.quads_for_pattern(Some(subject.as_ref().into()), None, None) {
        let quad = quad.map_err(store_err)?;
        let value = match &quad.object {
            Term::Literal(l) => l.value().to_string(),
            other => other.to_string(),
        };
        match quad.predicate.as_str() {
            DCTERMS_TYPE => {
                if let Term::NamedNode(node) = &quad.object {
                    outcome = Outcome::from_iri(node.as_str());
                }
            }
            SH_RESULT_SEVERITY => {
                if let Term::NamedNode(node) = &quad.object {
                    severity = node
                        .as_str()
                        .strip_prefix("urn:iki:severity:")
                        .map(str::to_string);
                }
            }
            DCTERMS_CREATED => at = Some(value),
            DCTERMS_DESCRIPTION => note = Some(value),
            PROV_GENERATED => {
                if let Term::NamedNode(node) = &quad.object {
                    minted = Some(node.as_str().to_string());
                }
            }
            _ => {}
        }
    }
    // The outcome is the discriminator: a node without one is not a decision.
    let Some(outcome) = outcome else {
        return Ok(None);
    };
    Ok(Some(Decision {
        outcome,
        severity: severity.unwrap_or_else(|| "info".to_string()),
        at,
        note,
        minted,
    }))
}

/// Every annotation in the store for one repo — optionally narrowed to one
/// path. Filters by `rdf:type oa:Annotation` (the shared store also holds
/// `ik:Explanation` entries with `ik:repo`/`ik:about` triples — type is the
/// discriminator). Sorted by (path, start, id) for a stable reading order.
fn list_annotations(archive: &Archive, repo: &str, rel: Option<&str>) -> Result<Vec<Annotation>> {
    use oxigraph::model::vocab::rdf;
    let mut out = Vec::new();
    for quad in archive.quads_for_pattern(
        None,
        Some(rdf::TYPE),
        Some(oa("Annotation").as_ref().into()),
    ) {
        let quad = quad.map_err(store_err)?;
        let subject = quad.subject.to_string();
        let iri = subject.trim_start_matches('<').trim_end_matches('>');
        let Some(id) = iri.strip_prefix("urn:iki:annotation:") else {
            continue;
        };
        let Some(ann) = load_annotation(archive, id)? else {
            continue;
        };
        if ann.repo == repo && rel.is_none_or(|rel| ann.rel == rel) {
            out.push(ann);
        }
    }
    out.sort_by(|a, b| (&a.rel, a.start, &a.id).cmp(&(&b.rel, b.start, &b.id)));
    Ok(out)
}

/// Every PENDING-FINDING record for one repo, optionally narrowed to a path.
///
/// Filtered on BOTH discriminators — `rdf:type prov:Entity` and the
/// `urn:iki:finding:` prefix — for the same reason [`list_annotations`] filters
/// on type: one shared store holds explanation entries, review passes,
/// annotations and findings, and a listing that keyed on only one of them
/// would eventually pick up something else's node.
///
/// Order is TRIAGE order, not reading order: severity rank first (critical
/// before praise), then path and position. ★ Severity stopped being a gate
/// when nothing auto-publishes; what it is now is "what to look at first", and
/// a queue that did not sort by it would not be offering that.
pub(crate) fn list_findings(
    archive: &Archive,
    repo: &str,
    rel: Option<&str>,
) -> Result<Vec<Annotation>> {
    use oxigraph::model::vocab::rdf;
    let entity = NamedNode::new(format!("{PROV}Entity")).map_err(store_err)?;
    let mut out = Vec::new();
    for quad in archive.quads_for_pattern(None, Some(rdf::TYPE), Some(entity.as_ref().into())) {
        let quad = quad.map_err(store_err)?;
        let subject = quad.subject.to_string();
        let iri = subject.trim_start_matches('<').trim_end_matches('>');
        let Some(id) = iri.strip_prefix(Family::Finding.prefix()) else {
            continue;
        };
        let Some(finding) = load_record(archive, Family::Finding, id)? else {
            continue;
        };
        if finding.repo == repo && rel.is_none_or(|rel| finding.rel == rel) {
            out.push(finding);
        }
    }
    sort_findings(&mut out);
    Ok(out)
}

/// Triage order for a slice of findings (and the same comparison the row
/// listings re-apply after the drift pass has moved positions).
pub(crate) fn sort_findings(findings: &mut [Annotation]) {
    findings.sort_by(|a, b| {
        (
            severity_rank(a.effective_severity()),
            &a.rel,
            a.start,
            &a.id,
        )
            .cmp(&(
                severity_rank(b.effective_severity()),
                &b.rel,
                b.start,
                &b.id,
            ))
    });
}

/// Where a severity sorts. An unrated finding sorts with `info` rather than
/// first or last: the model declining to rate is not evidence either way, and
/// burying it would hide exactly the findings whose format the model got wrong.
pub(crate) fn severity_rank(severity: Option<&str>) -> usize {
    crate::finding::SEVERITIES
        .iter()
        .position(|s| Some(*s) == severity)
        .unwrap_or_else(|| {
            crate::finding::SEVERITIES
                .iter()
                .position(|s| *s == "info")
                .expect("`info` is a declared severity")
        })
}

// --- anchoring --------------------------------------------------------------

/// Where a quote sits in a text: byte and character coordinates plus the
/// 1-based line its first character is on (the `#L{n}` anchor).
pub(crate) struct Anchor {
    byte_start: usize,
    byte_end: usize,
    pub(crate) char_start: u64,
    pub(crate) char_end: u64,
    pub(crate) line: u64,
}

/// Find `exact` in `content`, deterministically: every occurrence is scored by
/// how much of the given context matches (`prefix` immediately before it,
/// `suffix` immediately after — empty context scores nothing), the best score
/// wins, and a tie goes to the FIRST occurrence.
fn find_anchor(content: &str, exact: &str, prefix: &str, suffix: &str) -> Option<Anchor> {
    if exact.is_empty() {
        return None;
    }
    let mut best: Option<(u8, usize)> = None;
    for (idx, _) in content.match_indices(exact) {
        let mut score = 0u8;
        if !prefix.is_empty() && content[..idx].ends_with(prefix) {
            score += 1;
        }
        if !suffix.is_empty() && content[idx + exact.len()..].starts_with(suffix) {
            score += 1;
        }
        // Strictly-greater keeps the earliest occurrence on ties.
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, idx));
        }
    }
    let (_, byte_start) = best?;
    let byte_end = byte_start + exact.len();
    let char_start = content[..byte_start].chars().count() as u64;
    Some(Anchor {
        byte_start,
        byte_end,
        char_start,
        char_end: char_start + exact.chars().count() as u64,
        line: 1 + content[..byte_start].matches('\n').count() as u64,
    })
}

/// A unified-diff line with its one-column marker (`+`/`-`/context space)
/// removed — what a model that quoted the CODE line effectively saw.
fn strip_marker(line: &str) -> &str {
    line.strip_prefix(['+', '-', ' ']).unwrap_or(line)
}

/// A diff-surface anchoring result: where the quote landed, and — when it
/// only landed after marker-stripping — the ORIGINAL diff line to store as
/// the exact quote (drift must keep comparing real diff content, never the
/// stripped fiction the match was found through).
pub(crate) struct DiffAnchor {
    pub(crate) anchor: Anchor,
    pub(crate) stored_exact: Option<String>,
}

/// Anchoring against a unified diff — the PR targets' surface. Models (and
/// humans) quote the CODE they read, not the diff's leading `+`/`-`/space
/// column — and the QUOTE parser trims, so a context line's leading space is
/// gone before anchoring ever runs. Precedence, first hit wins:
///
/// 1. the quote, exactly as given, anywhere in the raw diff (context-scored,
///    [`find_anchor`]'s rule) — a marker-faithful quote anchors precisely,
///    to its own span;
/// 2. the quote, as given, in the marker-stripped shadow of the diff (every
///    line's leading `+`/`-`/space removed) — a quote of consecutive CODE
///    lines matches across the markers that interleave them;
/// 3. the quote with ITS OWN single leading `+`/`-`/space removed, in the
///    shadow (the model carried a marker, but a stale or wrong one);
/// 4. that marker-stripped quote whitespace-trimmed, in the shadow (models
///    that pad the marker — `+ code` for `+code` — or drop indentation).
///
/// Stages 2-4 search first-occurrence (the shadow has no honest context to
/// score) and anchor the WHOLE original diff line(s) the hit spans,
/// reporting that original text as the exact to store — drift must keep
/// comparing real diff content, never the stripped fiction the match was
/// found through.
fn find_anchor_in_diff(
    content: &str,
    exact: &str,
    prefix: &str,
    suffix: &str,
) -> Option<DiffAnchor> {
    if let Some(anchor) = find_anchor(content, exact, prefix, suffix) {
        return Some(DiffAnchor {
            anchor,
            stored_exact: None,
        });
    }
    // The marker-stripped shadow, plus per shadow line: the original line's
    // byte span and its own starting offset in the shadow.
    let mut shadow = String::with_capacity(content.len());
    let mut lines: Vec<(usize, usize, usize)> = Vec::new();
    let mut pos = 0usize;
    for line in content.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        lines.push((pos, pos + body.len(), shadow.len()));
        shadow.push_str(strip_marker(body));
        if body.len() != line.len() {
            shadow.push('\n');
        }
        pos += line.len();
    }
    let marker_stripped = strip_marker(exact);
    let mut needles = vec![exact, marker_stripped, marker_stripped.trim()];
    needles.dedup();
    for needle in needles {
        if needle.is_empty() {
            continue;
        }
        let Some(hit) = shadow.find(needle) else {
            continue;
        };
        let end = hit + needle.len();
        // The whole original lines the shadow hit spans.
        let first = lines.partition_point(|&(_, _, s)| s <= hit) - 1;
        let last = lines.partition_point(|&(_, _, s)| s < end).max(1) - 1;
        let (byte_start, byte_end) = (lines[first].0, lines[last].1);
        let char_start = content[..byte_start].chars().count() as u64;
        let original = &content[byte_start..byte_end];
        return Some(DiffAnchor {
            anchor: Anchor {
                byte_start,
                byte_end,
                char_start,
                char_end: char_start + original.chars().count() as u64,
                line: 1 + content[..byte_start].matches('\n').count() as u64,
            },
            stored_exact: Some(original.to_string()),
        });
    }
    None
}

/// The quote with a leading run of non-alphanumeric characters removed —
/// `Some` only when there WAS such a run and something survives it.
///
/// This is the whole concession [`find_anchor_on_file`] makes, and its
/// narrowness is the point: the run is LEADING, the rest must still match
/// character-for-character, and the retry happens once. A general fuzzy match
/// would destroy the property the anchor exists for — that it is proof the
/// model read the file.
fn strip_leading_decoration(exact: &str) -> Option<&str> {
    let rest = exact.trim_start_matches(|c: char| !c.is_alphanumeric());
    (rest.len() < exact.len() && !rest.is_empty()).then_some(rest)
}

/// Anchoring against file content: the quote as given, and — once, only when
/// that missed — the quote with a leading run of non-alphanumeric characters
/// stripped.
///
/// ⚠ The retry exists because the model DECORATES the quote with the marker
/// the document is written in rather than the characters the line carries
/// (ledger #488). Measured on a warning-dense Markdown file, it prefixed
/// quotes with a `⚠` the line does not have —
///
///   quoted: `⚠ **A per-repo cron CANNOT be the ecosystem's clock`
///   file:   `9e. **A per-repo cron CANNOT be the ecosystem's clock`
///
/// — and every other character matched. It is the same reflex the prompt's
/// backtick clause already fights, with characters that clause does not name;
/// on that file 28-48% of findings orphaned against the 1-5% the prompt
/// advertises, and the orphans were not random lines but exactly the marked
/// rules. ★ Fixing it HERE rather than with a sixth prompt clause is
/// deliberate: five phrasings were measured (ledger #483) and one made things
/// worse, a clause competes for attention a long file can crowd out, and this
/// is testable without a model.
///
/// ★ The stored exact is read back out of `content`, never handed through
/// from the caller's string: `exact` is the anchor, the dedupe key and part of
/// the finding id (`sha256(pass ‖ char_start ‖ exact)`), so storing the
/// model's decorated form would make the same finding a different id on every
/// pass.
fn find_anchor_on_file(
    content: &str,
    exact: &str,
    prefix: &str,
    suffix: &str,
) -> Option<DiffAnchor> {
    if let Some(anchor) = find_anchor(content, exact, prefix, suffix) {
        return Some(DiffAnchor {
            anchor,
            stored_exact: None,
        });
    }
    let anchor = find_anchor(content, strip_leading_decoration(exact)?, prefix, suffix)?;
    let stored_exact = content[anchor.byte_start..anchor.byte_end].to_string();
    Some(DiffAnchor {
        anchor,
        stored_exact: Some(stored_exact),
    })
}

/// Which anchoring discipline a target's text demands: [`find_anchor_on_file`]
/// for file content, the marker-tolerant [`find_anchor_in_diff`] for a pull
/// request's unified diff.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Surface {
    File,
    Diff,
}

/// Anchor on the right surface, normalizing both disciplines to a
/// [`DiffAnchor`]. Either surface may report a stored-exact override — the
/// text to record is always the FILE's (or diff's) characters, never the
/// caller's.
pub(crate) fn find_anchor_on(
    surface: Surface,
    content: &str,
    exact: &str,
    prefix: &str,
    suffix: &str,
) -> Option<DiffAnchor> {
    match surface {
        Surface::Diff => find_anchor_in_diff(content, exact, prefix, suffix),
        Surface::File => find_anchor_on_file(content, exact, prefix, suffix),
    }
}

/// The stored context around an anchored quote: up to [`CONTEXT_CHARS`]
/// characters each side. Always derived from the anchored occurrence (never
/// the caller's raw arguments) so re-anchoring matches real neighbors.
pub(crate) fn context_around(content: &str, anchor: &Anchor) -> (String, String) {
    let before = &content[..anchor.byte_start];
    let prefix_start = before
        .char_indices()
        .rev()
        .nth(CONTEXT_CHARS - 1)
        .map_or(0, |(i, _)| i);
    let prefix = before[prefix_start..].to_string();
    let suffix: String = content[anchor.byte_end..]
        .chars()
        .take(CONTEXT_CHARS)
        .collect();
    (prefix, suffix)
}

/// The 1-based line a character offset falls on — clamped to the last line
/// when the offset outruns the text (an orphan's recorded position rendered
/// against shorter current content).
pub(crate) fn line_of(content: &str, char_offset: u64) -> u64 {
    let mut line = 1;
    for (i, ch) in content.chars().enumerate() {
        if i as u64 >= char_offset {
            break;
        }
        if ch == '\n' {
            line += 1;
        }
    }
    line
}

/// The target's current text, when it is reachable and textual.
pub(crate) enum CurrentContent {
    /// UTF-8 text plus its `sha256:{hex}` content hash.
    Text(String, String),
    /// The target is gone or binary — nothing to anchor against.
    Unavailable,
    /// The target could not be consulted at all (a PR whose data facades are
    /// not mounted, or whose fetch failed). Distinct from [`Unavailable`]:
    /// "could not look" must not orphan what "looked and it is gone" would.
    Unknown,
}

/// `sha256:{hex}` of raw bytes — the annotation layer's content key (also
/// what the review layers stamp on annotations they mint against text they
/// already hold).
pub(crate) fn content_hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// The drift pass, run on every read (Source, list, and the file face's
/// panel): reconcile one annotation against the target's current content.
/// Returns the line its anchor renders at (`None` when no content is in
/// hand). Persists to the store ONLY when something changed — repeat reads of
/// an unchanged (or already-orphaned) annotation touch nothing.
pub(crate) fn refresh(
    archive: &Archive,
    ann: &mut Annotation,
    current: &CurrentContent,
) -> Result<Option<u64>> {
    match current {
        // Served exactly as recorded, flags untouched: no content was in
        // hand, so nothing can honestly be said about drift.
        CurrentContent::Unknown => Ok(None),
        CurrentContent::Unavailable => {
            if !ann.orphaned {
                ann.orphaned = true;
                rewrite_annotation(archive, ann)?;
            }
            Ok(None)
        }
        CurrentContent::Text(text, hash) => {
            if *hash == ann.hash {
                // The recorded selectors are valid for this exact content — a
                // previously-orphaned annotation whose content came back is
                // whole again.
                if ann.orphaned {
                    ann.orphaned = false;
                    rewrite_annotation(archive, ann)?;
                }
                return Ok(Some(line_of(text, ann.start)));
            }
            // PR annotations re-anchor with the diff discipline: a new head's
            // diff may flip a line's marker (`+` settling into context), and
            // the marker-stripped retry follows the line across that.
            let surface = match ann.target_ref() {
                TargetRef::Pr(_) => Surface::Diff,
                TargetRef::File(_) => Surface::File,
            };
            match find_anchor_on(surface, text, &ann.exact, &ann.prefix, &ann.suffix) {
                Some(DiffAnchor {
                    anchor,
                    stored_exact,
                }) => {
                    let (prefix, suffix) = context_around(text, &anchor);
                    if let Some(exact) = stored_exact {
                        ann.exact = exact;
                    }
                    ann.prefix = prefix;
                    ann.suffix = suffix;
                    ann.start = anchor.char_start;
                    ann.end = anchor.char_end;
                    ann.hash = hash.clone();
                    ann.reanchored = true;
                    ann.orphaned = false;
                    rewrite_annotation(archive, ann)?;
                    Ok(Some(anchor.line))
                }
                None => {
                    if !ann.orphaned {
                        ann.orphaned = true;
                        rewrite_annotation(archive, ann)?;
                    }
                    // Flagged, but still rendered — at its RECORDED position
                    // projected onto the current text (clamped).
                    Ok(Some(line_of(text, ann.start)))
                }
            }
        }
    }
}

/// Run the drift pass over a set of loaded annotations, fetching each distinct
/// target's current content through the kernel once, and return the rows in
/// reading order (path, position, id) — the shared middle of the listing
/// endpoint and the `annotations=include` fetch.
async fn reconcile(
    inv: &Invocation<'_>,
    archive: &Archive,
    roots: &BTreeMap<String, std::path::PathBuf>,
    repo: &str,
    mut anns: Vec<Annotation>,
) -> Result<Vec<(Annotation, Option<u64>)>> {
    let mut contents: BTreeMap<String, CurrentContent> = BTreeMap::new();
    for ann in &anns {
        if !contents.contains_key(&ann.target_iri) {
            let current = current_content_for(inv, roots, repo, &ann.target_ref()).await?;
            contents.insert(ann.target_iri.clone(), current);
        }
    }
    let mut rows: Vec<(Annotation, Option<u64>)> = Vec::with_capacity(anns.len());
    for mut ann in anns.drain(..) {
        let current = contents.get(&ann.target_iri).expect("fetched above");
        let line = refresh(archive, &mut ann, current)?;
        rows.push((ann, line));
    }
    // Re-anchoring may have moved positions — restore reading order.
    rows.sort_by(|(a, _), (b, _)| (&a.rel, a.start, &a.id).cmp(&(&b.rel, b.start, &b.id)));
    Ok(rows)
}

/// The drift pass over a set of findings — the queue listing's middle, and
/// the same `reconcile` the annotation listing runs: one content fetch per
/// distinct target, then re-anchor or orphan each. ★ Findings do not get a
/// drift story of their own; they get THE drift story.
pub(crate) async fn reconcile_findings(
    inv: &Invocation<'_>,
    archive: &Archive,
    roots: &BTreeMap<String, std::path::PathBuf>,
    repo: &str,
    findings: Vec<Annotation>,
) -> Result<Vec<(Annotation, Option<u64>)>> {
    reconcile(inv, archive, roots, repo, findings).await
}

/// Restore triage order after the drift pass has moved positions.
pub(crate) fn sort_finding_rows(rows: &mut [(Annotation, Option<u64>)]) {
    rows.sort_by(|(a, _), (b, _)| {
        (
            severity_rank(a.effective_severity()),
            &a.rel,
            a.start,
            &a.id,
        )
            .cmp(&(
                severity_rank(b.effective_severity()),
                &b.rel,
                b.start,
                &b.id,
            ))
    });
}

/// The drift pass against content already in hand (the file face's path — no
/// kernel fetch), rows in reading order.
fn reconcile_against_text(
    archive: &Archive,
    repo: &str,
    rel: &str,
    text: &str,
) -> Result<Vec<(Annotation, Option<u64>)>> {
    let mut anns = list_annotations(archive, repo, Some(rel))?;
    let current = CurrentContent::Text(text.to_string(), content_hash(text.as_bytes()));
    let mut rows: Vec<(Annotation, Option<u64>)> = Vec::with_capacity(anns.len());
    for mut ann in anns.drain(..) {
        let line = refresh(archive, &mut ann, &current)?;
        rows.push((ann, line));
    }
    rows.sort_by(|(a, _), (b, _)| (a.start, &a.id).cmp(&(b.start, &b.id)));
    Ok(rows)
}

// --- annotations=include (S3) ------------------------------------------------

/// Which annotations an `annotations=include` resolution folds in: exactly one
/// file's, or (a directory explain) everything under a subtree — `""` is the
/// whole repo.
#[derive(Clone, Copy)]
pub(crate) enum TargetFilter<'a> {
    File(&'a str),
    Subtree(&'a str),
}

/// Drift-reconciled annotation rows ready to fold into another resource's
/// response — the `annotations=include` payload for the file and explain
/// faces.
pub(crate) struct Included {
    rows: Vec<(Annotation, Option<u64>)>,
    /// Rows may span multiple files (a subtree filter) — margin notes then
    /// carry the path.
    with_paths: bool,
}

impl Included {
    /// The JSON face's `annotations` array — the same row shape the listing
    /// endpoint serves (quote, body, line, drift flags, and the rest).
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.rows
                .iter()
                .map(|(ann, line)| annotation_json(ann, *line))
                .collect(),
        )
    }

    /// The compact margin-notes section a text face appends: a counted header,
    /// then one line per annotation — anchor line, drift flag, clipped quote,
    /// whitespace-collapsed body.
    pub(crate) fn margin_text(&self) -> String {
        let mut out = format!("--- annotations ({}) ---", self.rows.len());
        for (ann, line) in &self.rows {
            out.push('\n');
            if self.with_paths {
                out.push_str(&ann.place());
                out.push(' ');
            }
            if let Some(n) = line {
                out.push_str(&format!("L{n} "));
            }
            if ann.orphaned {
                out.push_str("[orphaned] ");
            } else if ann.reanchored {
                out.push_str("[re-anchored] ");
            }
            if let Some(creator) = &ann.creator {
                out.push_str(&format!("[review:{creator}] "));
            }
            out.push_str(&format!(
                "\"{}\" -- {}",
                clip(&ann.exact),
                collapse(&ann.body)
            ));
        }
        out
    }

    /// The annotation cards as an HTML fragment — what an
    /// `annotations=include` html face folds in, the same card markup the
    /// file face's panel renders. Subtree rows label each card with its path;
    /// `form_target` (a concrete file IRI) appends the create form — a
    /// rollup passes `None` (a create needs one target).
    pub(crate) fn panel_html(&self, form_target: Option<&str>) -> String {
        let mut out = String::from("<div class=\"browse-annotations\">");
        for (ann, line) in &self.rows {
            out.push_str(&annotation_card_html(ann, *line, self.with_paths));
        }
        if let Some(target) = form_target {
            out.push_str(&annotation_form_html(target));
        }
        out.push_str("</div>");
        out
    }
}

/// A quote clipped to margin width (60 chars), char-boundary safe.
fn clip(s: &str) -> String {
    clip_to(s, 60)
}

/// Clip to `max` chars with an ellipsis, char-boundary safe.
fn clip_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let clipped: String = s.chars().take(max - 1).collect();
    format!("{clipped}…")
}

/// A body collapsed to one line for the margin (all whitespace runs → one
/// space).
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `annotations=include` fetch through the kernel — same
/// drift-reconciliation as the listing endpoint. A [`TargetFilter::Subtree`]
/// keeps only annotations under the directory (all of them when it names the
/// root).
pub(crate) async fn included_for(
    inv: &Invocation<'_>,
    archive: &Archive,
    roots: &BTreeMap<String, std::path::PathBuf>,
    repo: &str,
    filter: TargetFilter<'_>,
) -> Result<Included> {
    let anns = match filter {
        TargetFilter::File(rel) => list_annotations(archive, repo, Some(rel))?,
        TargetFilter::Subtree(rel) => {
            // File annotations only: a subtree is a directory concept, and a
            // PR annotation lives under no directory (its own page folds it).
            let all = list_annotations(archive, repo, None)?;
            let files = all
                .into_iter()
                .filter(|ann| matches!(ann.target_ref(), TargetRef::File(_)));
            if rel.is_empty() {
                files.collect()
            } else {
                let prefix = format!("{rel}/");
                files.filter(|ann| ann.rel.starts_with(&prefix)).collect()
            }
        }
    };
    let rows = reconcile(inv, archive, roots, repo, anns).await?;
    Ok(Included {
        rows,
        with_paths: matches!(filter, TargetFilter::Subtree(_)),
    })
}

/// The `annotations=include` payload for a file face whose content is already
/// in hand — the drift pass runs against the very text being served.
pub(crate) fn included_for_text(
    archive: &Archive,
    repo: &str,
    rel: &str,
    text: &str,
) -> Result<Included> {
    Ok(Included {
        rows: reconcile_against_text(archive, repo, rel, text)?,
        with_paths: false,
    })
}

/// Source the target's current content through the kernel. A NotFound (the
/// file was deleted) or non-UTF-8 answer is `Unavailable` — drift, not an
/// error; anything else propagates.
async fn current_content(inv: &Invocation<'_>, repo: &str, rel: &str) -> Result<CurrentContent> {
    match inv.source(&parse_iri(&file_iri(repo, rel))?).await {
        Ok(repr) => Ok(match String::from_utf8(repr.bytes.clone()) {
            Ok(text) => {
                let hash = content_hash(&repr.bytes);
                CurrentContent::Text(text, hash)
            }
            Err(_) => CurrentContent::Unavailable,
        }),
        Err(Error::NotFound(_)) => Ok(CurrentContent::Unavailable),
        Err(e) => Err(e),
    }
}

/// [`current_content`], dispatched on the target's kind. A PR target's
/// surface is its unified diff, fetched through ikigai-repo's facade — ANY
/// failure there (facades unmounted, gh error) is `Unknown`, never an error
/// and never an orphaning: one broken PR fetch must not kill (or rewrite) a
/// listing.
pub(crate) async fn current_content_for(
    inv: &Invocation<'_>,
    roots: &BTreeMap<String, std::path::PathBuf>,
    repo: &str,
    target: &TargetRef<'_>,
) -> Result<CurrentContent> {
    match target {
        TargetRef::File(rel) => current_content(inv, repo, rel).await,
        TargetRef::Pr(n) => {
            let Some(dir) = roots.get(repo) else {
                return Ok(CurrentContent::Unknown);
            };
            match crate::pr::diff_text(inv, dir, *n).await {
                Ok(text) => {
                    let hash = content_hash(text.as_bytes());
                    Ok(CurrentContent::Text(text, hash))
                }
                Err(_) => Ok(CurrentContent::Unknown),
            }
        }
    }
}

// --- binding ----------------------------------------------------------------

pub(crate) fn bind(space: EndpointSpace, roots: &Roots, archive: &Arc<Archive>) -> EndpointSpace {
    let space = space.bind(
        AnnotationGrammar::new(),
        AnnotationEndpoint {
            roots: Arc::clone(roots),
            archive: Arc::clone(archive),
        },
    );
    let listing: Arc<dyn Endpoint> = Arc::new(AnnotationsEndpoint {
        roots: Arc::clone(roots),
        archive: Arc::clone(archive),
    });
    crate::bind_family(
        space,
        roots,
        listing,
        Some("annotations"),
        Some("annotations:{path}"),
    )
}

/// `urn:iki:annotation:{id}`, plus the bare `urn:iki:annotation` (Sink mints an id).
struct AnnotationGrammar {
    with_id: UriTemplate,
}

impl AnnotationGrammar {
    fn new() -> Self {
        AnnotationGrammar {
            with_id: UriTemplate::parse("urn:iki:annotation:{id}")
                .expect("the annotation template is valid"),
        }
    }
}

impl Grammar for AnnotationGrammar {
    fn match_iri(&self, iri: &Iri) -> Option<Bindings> {
        if iri.as_str() == "urn:iki:annotation" {
            return Some(Bindings::new());
        }
        self.with_id.match_iri(iri)
    }

    fn pattern(&self) -> String {
        // The advertised row is the template — a real pattern a probe can
        // expand and every verb can drive (Sink's `id` is optional there, and
        // the description documents the minting form). The bare
        // `urn:iki:annotation` stays resolvable but UNLISTED: as a row of its own
        // it would offer Source/Delete actions that cannot succeed without an
        // id, and `[:{id}]` display sugar is not a template any grammar
        // matches (it kept every annotation row out of every manifold).
        "urn:iki:annotation:{id}".to_string()
    }
}

// --- the annotation endpoint (multi-verb) -----------------------------------

struct AnnotationEndpoint {
    roots: Roots,
    archive: Arc<Archive>,
}

#[async_trait]
impl Endpoint for AnnotationEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match inv.request.verb {
            Verb::Source => self.read(inv).await,
            Verb::Sink => self.write(inv).await,
            Verb::Delete => self.delete(inv),
            other => Err(Error::Endpoint(format!(
                "annotation does not support the {other:?} verb"
            ))),
        }
    }

    fn name(&self) -> &str {
        "annotation"
    }

    fn describe(&self) -> Description {
        annotation_description()
    }
}

impl AnnotationEndpoint {
    fn id_binding(inv: &Invocation<'_>) -> Result<String> {
        inv.bindings
            .get("id")
            .map(str::to_string)
            .ok_or_else(|| Error::MissingArgument("id".to_string()))
    }

    fn load_required(&self, id: &str) -> Result<Annotation> {
        load_annotation(&self.archive, id)?.ok_or_else(|| {
            Error::NotFound(format!("browse: no annotation `{}`", annotation_iri(id)))
        })
    }

    /// Source: load, run the drift pass against the target's current content
    /// (re-anchor or orphan as needed), serve a face. The per-root browse
    /// check gates on the ANNOTATION'S repo — reading an annotation is
    /// reading (about) its target.
    async fn read(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let id = Self::id_binding(inv)?;
        let mut ann = self.load_required(&id)?;
        granted(inv, &ann.repo)?;
        let current = current_content_for(inv, &self.roots, &ann.repo, &ann.target_ref()).await?;
        let line = refresh(&self.archive, &mut ann, &current)?;
        match inv.inline_str("as").unwrap_or("text/plain") {
            t if t.starts_with("application/json") => Ok(repr(
                "application/json",
                annotation_json(&ann, line).to_string(),
            )),
            t if t.starts_with("text/turtle") => {
                Ok(repr("text/turtle", annotation_turtle_document(&[ann])))
            }
            _ => Ok(repr_utf8("text/plain", ann.body.clone())),
        }
    }

    /// Sink: anchor the quote in the target's current content (sourced
    /// through the kernel — a capability that cannot read the target cannot
    /// annotate it) and create or update the annotation. An update re-anchors
    /// fresh and clears any orphan/re-anchor flags; its `dcterms:created`
    /// survives from the first creation.
    async fn write(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let (id, minted) = match inv.bindings.get("id") {
            Some(id) => (validate_id(id)?, false),
            None => (uuid::Uuid::new_v4().to_string(), true),
        };
        let target = inv.inline_str("target")?.trim().to_string();
        let parsed = parse_target(&target, &self.roots)?;
        // The per-root grant, enforced uniformly: for a file target the
        // anchoring read below enforces it structurally anyway; for a PR
        // target the diff facade knows nothing of browse roots, so the check
        // here is the declared wildcard's enforcement.
        let (repo, rel, target_iri) = match &parsed {
            SinkTarget::File { repo, rel } => (repo.clone(), rel.clone(), file_iri(repo, rel)),
            SinkTarget::Pr { repo, number } => (
                repo.clone(),
                String::new(),
                crate::pr::pr_iri(repo, *number),
            ),
        };
        granted(inv, &repo)?;
        // Pipeline citizenship: the note text is the `body` arg, with the
        // piped `content` as fallback.
        let body = inv
            .inline_str("body")
            .ok()
            .or_else(|| inv.inline_str("content").ok())
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(str::to_string)
            .ok_or_else(|| Error::MissingArgument("body".to_string()))?;
        let exact = inv.inline_str("exact")?.to_string();
        if exact.is_empty() {
            return Err(Error::InvalidArgument {
                name: "exact".to_string(),
                detail: "the quoted text must be non-empty".to_string(),
            });
        }
        let hint_prefix = inv.inline_str("prefix").unwrap_or("");
        let hint_suffix = inv.inline_str("suffix").unwrap_or("");

        let tref = match &parsed {
            SinkTarget::File { rel, .. } => TargetRef::File(rel),
            SinkTarget::Pr { number, .. } => TargetRef::Pr(*number),
        };
        let CurrentContent::Text(text, hash) =
            current_content_for(inv, &self.roots, &repo, &tref).await?
        else {
            return Err(Error::InvalidArgument {
                name: "target".to_string(),
                detail: format!(
                    "`{target}` is not annotatable text (missing, binary, or — for a pull \
                     request — its urn:repo:pr:* facades are not mounted)"
                ),
            });
        };
        // A PR target's surface is its unified diff — anchor with the
        // marker-tolerant discipline (a quote of the code line still lands;
        // the stored exact becomes the original diff line).
        let surface = match &parsed {
            SinkTarget::Pr { .. } => Surface::Diff,
            SinkTarget::File { .. } => Surface::File,
        };
        let DiffAnchor {
            anchor,
            stored_exact,
        } = find_anchor_on(surface, &text, &exact, hint_prefix, hint_suffix).ok_or_else(|| {
            Error::InvalidArgument {
                name: "exact".to_string(),
                detail: format!("the quote was not found in `{target}`"),
            }
        })?;
        let exact = stored_exact.unwrap_or(exact);
        let (prefix, suffix) = context_around(&text, &anchor);

        // An update keeps its original creation instant.
        let created = match load_annotation(&self.archive, &id)? {
            Some(existing) if existing.created.is_some() => existing.created,
            _ => inv.now().map(|t| iso8601(t.as_millis())),
        };
        let ann = Annotation {
            family: Family::Annotation,
            id: id.clone(),
            body,
            target_iri,
            repo,
            rel,
            hash,
            prefix,
            exact,
            suffix,
            start: anchor.char_start,
            end: anchor.char_end,
            created,
            reanchored: false,
            orphaned: false,
            // The human Sink: oa:commenting, no creator (the machine
            // discriminator), no generating pass. An update of a
            // machine-minted id rewrites it as human commentary — the words
            // are no longer the model's.
            creator: None,
            motivation: Some(MOTIVATION_HUMAN.to_string()),
            generated_by: None,
            // A human's own note carries no severity: severity is the review
            // pipeline's triage axis, and inventing one here would put a
            // rating on a claim nobody rated.
            severity: None,
            derived_from: None,
            decision: None,
        };
        rewrite_annotation(&self.archive, &ann)?;
        match inv.inline_str("as").unwrap_or("text/plain") {
            t if t.starts_with("application/json") => {
                let mut json = annotation_json(&ann, Some(anchor.line));
                json["minted"] = serde_json::Value::Bool(minted);
                Ok(repr("application/json", json.to_string()))
            }
            // The plain acknowledgement is the annotation's IRI — sinkable
            // output that pipes straight into a Source.
            _ => Ok(repr_utf8("text/plain", ann.iri())),
        }
    }

    fn delete(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let id = Self::id_binding(inv)?;
        let ann = self.load_required(&id)?;
        remove_annotation(&self.archive, Family::Annotation, &id)?;
        Ok(repr_utf8("text/plain", format!("deleted {}", ann.iri())))
    }
}

fn annotation_description() -> Description {
    use crate::XSD_STRING;
    Description::new("annotation")
        .title("Web Annotation (oa:) on a browse resource")
        .summary(
            "A W3C Web Annotation on a repository file — urn:iki:annotation:{id}, stored as \
             skolemized RDF in the same shared store as the explanation archive. Sink \
             creates or updates (anchoring the quoted text in the target's current \
             content; sink the bare urn:iki:annotation to mint a uuid id); Source reads it \
             back, re-anchoring the selectors when the target has drifted (ik:reanchored) \
             and flagging quotes that are gone (ik:orphaned — never silently dropped); \
             Delete removes it. Selectors are stored as BOTH oa:TextQuoteSelector \
             (prefix/exact/suffix) and oa:TextPositionSelector (character start/end), \
             keyed to the annotated content version by ik:contentHash.",
        )
        .verb(Verb::Meta)
        .action(
            ActionSpec::new(Verb::Source)
                .summary("read one annotation, re-anchored against the target's current content")
                .requires(CAP_WILDCARD)
                .input(
                    ArgSpec::new("id")
                        .binding()
                        .class(XSD_STRING)
                        .summary("the annotation id (a slug or minted uuid)"),
                )
                .input(
                    ArgSpec::new("as")
                        .optional()
                        .class(XSD_STRING)
                        .summary("the face to render")
                        .one_of(["text/plain", "application/json", "text/turtle"])
                        .default_value("text/plain"),
                )
                .output("text/plain;charset=utf-8")
                .output("application/json")
                .output("text/turtle"),
        )
        .action(
            ActionSpec::new(Verb::Sink)
                .summary(
                    "create or update an annotation: anchor the quote in the target and store \
                     body + both selectors",
                )
                .requires(CAP_ANNOTATE)
                // Anchoring sources the target through the kernel — a
                // capability that cannot read the target cannot annotate it.
                .requires(CAP_WILDCARD)
                .input(
                    ArgSpec::new("id")
                        .binding()
                        .optional()
                        .class(XSD_STRING)
                        .summary(
                            "caller-supplied slug ([A-Za-z0-9._~-]+); sink the bare \
                             urn:iki:annotation to mint a uuid",
                        ),
                )
                .input(
                    ArgSpec::new("target")
                        .class("https://ikigai-rs.dev/ns#File")
                        .summary("the annotated resource — urn:repo:{repo}:file:{path}"),
                )
                .input(ArgSpec::new("body").class(XSD_STRING).optional().summary(
                    "the note text (oa:bodyValue); falls back to piped content — one of the \
                     two must be present",
                ))
                // Pipeline citizenship: the Sink has read a piped body since the
                // family shipped and never SAID so, so `… | sink
                // urn:iki:annotation:{id}` was a write path no manifold announced.
                .input(
                    ArgSpec::new("content")
                        .class(XSD_STRING)
                        .optional()
                        .summary(
                            "the piped form of body — where a pipe's value and a sink's \
                             request body arrive; body= wins when both are present",
                        ),
                )
                .input(
                    ArgSpec::new("exact")
                        .class(XSD_STRING)
                        .summary("the quoted target text to anchor (oa:exact)"),
                )
                .input(ArgSpec::new("prefix").class(XSD_STRING).optional().summary(
                    "disambiguating context immediately before the quote (the stored selector \
                     derives its own context from the anchored occurrence)",
                ))
                .input(
                    ArgSpec::new("suffix")
                        .class(XSD_STRING)
                        .optional()
                        .summary("disambiguating context immediately after the quote"),
                )
                .input(
                    ArgSpec::new("as")
                        .optional()
                        .class(XSD_STRING)
                        .summary("application/json for the structured acknowledgement")
                        .one_of(["text/plain", "application/json"])
                        .default_value("text/plain"),
                )
                .output("text/plain;charset=utf-8")
                .output("application/json"),
        )
        .action(
            ActionSpec::new(Verb::Delete)
                .summary("remove an annotation and its selectors from the store")
                .requires(CAP_ANNOTATE)
                .input(
                    ArgSpec::new("id")
                        .binding()
                        .class(XSD_STRING)
                        .summary("the annotation id"),
                )
                .output("text/plain;charset=utf-8"),
        )
}

// --- the listing endpoint ---------------------------------------------------

/// `urn:repo:{repo}:annotations[:{path}]` — every annotation on one file, or
/// (path omitted) on the whole repo. Runs the same drift pass as Source, one
/// content fetch per distinct target.
struct AnnotationsEndpoint {
    roots: Roots,
    archive: Arc<Archive>,
}

#[async_trait]
impl Endpoint for AnnotationsEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "browse-annotations does not support the {:?} verb",
                inv.request.verb
            )));
        }
        let (repo, _root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        let rel = path_binding(inv)?;
        let filter = (!rel.is_empty()).then_some(rel.as_str());
        let anns = list_annotations(&self.archive, repo, filter)?;
        // One content fetch per distinct target, then the drift pass each.
        let rows = reconcile(inv, &self.archive, &self.roots, repo, anns).await?;

        match inv.inline_str("as").unwrap_or("application/json") {
            t if t.starts_with("text/html") => Ok(repr_utf8(
                "text/html",
                annotations_listing_html(repo, &rel, &rows),
            )),
            t if t.starts_with("text/turtle") => {
                let anns: Vec<Annotation> = rows.into_iter().map(|(a, _)| a).collect();
                Ok(repr("text/turtle", annotation_turtle_document(&anns)))
            }
            _ => {
                let rows: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|(ann, line)| annotation_json(ann, *line))
                    .collect();
                Ok(repr(
                    "application/json",
                    serde_json::Value::Array(rows).to_string(),
                ))
            }
        }
    }

    fn name(&self) -> &str {
        "browse-annotations"
    }

    fn describe(&self) -> Description {
        annotations_description()
    }
}

/// `repo` is not an ArgSpec: every advertised row fixes the root in its
/// pattern (see `crate::bind_family`); the binding is grammar-injected.
fn annotations_description() -> Description {
    Description::new("browse-annotations")
        .title("Annotations on a browse target")
        .summary(
            "Every annotation on one file (urn:repo:{repo}:annotations:{path}) or on the \
             whole repo (path omitted), in reading order (path, position). Each read runs \
             the drift pass: selectors re-anchor when the target's content moved \
             (ik:reanchored), and quotes that are gone are flagged ik:orphaned — never \
             silently dropped. application/json (default) is the structured rows; \
             as=text/html an htmx-styled panel fragment with #L{n} line anchors; \
             as=text/turtle the full oa: graph.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("path")
                .binding()
                .optional()
                .class(crate::XSD_STRING)
                .summary("file path within the root, percent-encoded (omitted = the whole repo)"),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .class(crate::XSD_STRING)
                .summary("the face to render")
                .one_of(["application/json", "text/html", "text/turtle"])
                .default_value("application/json"),
        )
        .output("application/json")
        .output("text/html;charset=utf-8")
        .output("text/turtle")
}

// --- faces ------------------------------------------------------------------

pub(crate) fn annotation_json(ann: &Annotation, line: Option<u64>) -> serde_json::Value {
    serde_json::json!({
        "id": ann.id,
        "iri": ann.iri(),
        "annotates": ann.target_iri,
        "repo": ann.repo,
        "path": ann.rel,
        "pr": ann.pr_number(),
        "body": ann.body,
        "prefix": ann.prefix,
        "exact": ann.exact,
        "suffix": ann.suffix,
        "start": ann.start,
        "end": ann.end,
        "line": line,
        "content_hash": ann.hash,
        "created": ann.created,
        "reanchored": ann.reanchored,
        "orphaned": ann.orphaned,
        "machine": ann.machine(),
        "creator": ann.creator,
        "motivation": ann.motivation,
        "generated_by": ann.generated_by,
        // ★ Both ratings, always, on every row: `severity` is the MODEL'S
        // PROPOSAL on a finding (and the human's final on a published
        // annotation), `decision.severity` is what the human settled on.
        // A consumer that wants one number takes `effective_severity`.
        "severity": ann.severity,
        "effective_severity": ann.effective_severity(),
        "state": ann.state(),
        "derived_from": ann.derived_from,
        "decision": ann.decision.as_ref().map(|d| serde_json::json!({
            "outcome": d.outcome.label(),
            "severity": d.severity,
            "decided_at": d.at,
            "note": d.note,
            "minted": d.minted,
        })),
    })
}

/// One annotation's graph as Turtle (the same skolemized shape the store
/// holds).
fn annotation_turtle(ann: &Annotation) -> String {
    let (class, body_predicate) = match ann.family {
        Family::Annotation => ("oa:Annotation", "oa:bodyValue"),
        Family::Finding => ("prov:Entity", "dcterms:description"),
    };
    let mut props = vec![
        format!("a {class}"),
        format!("{body_predicate} {}", ttl_str(&ann.body)),
        format!("ik:annotates <{}>", ann.target_iri),
        format!("ik:repo {}", ttl_str(&ann.repo)),
        format!("ik:path {}", ttl_str(&ann.rel)),
        format!("ik:contentHash {}", ttl_str(&ann.hash)),
        format!(
            "oa:hasSelector <{}>, <{}>",
            quote_iri(ann.family, &ann.id),
            position_iri(ann.family, &ann.id)
        ),
    ];
    if let Some(at) = &ann.created {
        props.push(format!("dcterms:created \"{at}\"^^xsd:dateTime"));
    }
    if ann.reanchored {
        props.push("ik:reanchored true".to_string());
    }
    if ann.orphaned {
        props.push("ik:orphaned true".to_string());
    }
    // Annotation family only — `oa:motivatedBy`'s domain would type a
    // finding into the annotation family (see `store_annotation`).
    if let (Family::Annotation, Some(motivation)) = (ann.family, &ann.motivation) {
        props.push(format!("oa:motivatedBy oa:{motivation}"));
    }
    if let Some(creator) = &ann.creator {
        props.push(format!("dcterms:creator {}", ttl_str(creator)));
    }
    if let Some(pass) = &ann.generated_by {
        props.push(format!("prov:wasGeneratedBy <{pass}>"));
    }
    if let Some(severity) = &ann.severity {
        props.push(format!("sh:resultSeverity <{}>", severity_iri(severity)));
    }
    if let Some(from) = &ann.derived_from {
        props.push(format!("prov:wasDerivedFrom <{from}>"));
    }
    let mut out = format!("<{}> {} .\n", ann.iri(), props.join(" ;\n    "));
    if let (Family::Finding, Some(decision)) = (ann.family, &ann.decision) {
        let mut act = vec![
            "a prov:Activity".to_string(),
            format!("prov:used <{}>", ann.iri()),
            format!("dcterms:type <{}>", decision.outcome.iri()),
            format!("sh:resultSeverity <{}>", severity_iri(&decision.severity)),
        ];
        if let Some(at) = &decision.at {
            act.push(format!("dcterms:created \"{at}\"^^xsd:dateTime"));
        }
        if let Some(note) = &decision.note {
            act.push(format!("dcterms:description {}", ttl_str(note)));
        }
        if let Some(minted) = &decision.minted {
            act.push(format!("prov:generated <{minted}>"));
        }
        out.push_str(&format!(
            "\n<{}> {} .\n",
            decision_iri(&ann.id),
            act.join(" ;\n    ")
        ));
    }

    let mut quote = vec![
        "a oa:TextQuoteSelector".to_string(),
        format!("oa:exact {}", ttl_str(&ann.exact)),
    ];
    if !ann.prefix.is_empty() {
        quote.push(format!("oa:prefix {}", ttl_str(&ann.prefix)));
    }
    if !ann.suffix.is_empty() {
        quote.push(format!("oa:suffix {}", ttl_str(&ann.suffix)));
    }
    out.push_str(&format!(
        "\n<{}> {} .\n",
        quote_iri(ann.family, &ann.id),
        quote.join(" ;\n    ")
    ));
    out.push_str(&format!(
        "\n<{}> a oa:TextPositionSelector ;\n    oa:start \"{}\"^^xsd:nonNegativeInteger ;\n    \
         oa:end \"{}\"^^xsd:nonNegativeInteger .\n",
        position_iri(ann.family, &ann.id),
        ann.start,
        ann.end
    ));
    out
}

pub(crate) fn annotation_turtle_document(anns: &[Annotation]) -> String {
    let mut out = format!(
        "@prefix oa: <{OA}> .\n@prefix ik: <{IK}> .\n@prefix dcterms: <http://purl.org/dc/terms/> \
         .\n@prefix prov: <{PROV}> .\n@prefix sh: <http://www.w3.org/ns/shacl#> .\n@prefix xsd: \
         <http://www.w3.org/2001/XMLSchema#> .\n"
    );
    for ann in anns {
        out.push('\n');
        out.push_str(&annotation_turtle(ann));
    }
    out
}

/// One annotation as an HTML card: the line anchor, the quote, the body, and
/// any drift flags. Orphans keep their (approximate) anchor but are visually
/// flagged. `show_path` labels the card with its file (subtree folds span
/// many files).
pub(crate) fn annotation_card_html(ann: &Annotation, line: Option<u64>, show_path: bool) -> String {
    let orphan_class = if ann.orphaned {
        " browse-annotation-orphaned"
    } else {
        ""
    };
    // Machine cards are visibly machine: the class hook plus the model
    // identity, so review commentary is never mistaken for a human note.
    let machine_class = if ann.machine() {
        " browse-annotation-machine"
    } else {
        ""
    };
    let model = ann
        .creator
        .as_deref()
        .map(|creator| {
            format!(
                "<span class=\"browse-annotation-model\">review by {}</span> ",
                esc(creator)
            )
        })
        .unwrap_or_default();
    let path = if show_path {
        format!(
            "<span class=\"browse-annotation-path\">{}</span> ",
            esc(&ann.place())
        )
    } else {
        String::new()
    };
    let anchor = match line {
        Some(n) => format!("<a class=\"browse-annotation-line\" href=\"#L{n}\">L{n}</a> "),
        None => String::new(),
    };
    let mut flags = String::new();
    if ann.orphaned {
        flags.push_str(
            "<span class=\"browse-annotation-flag\">orphaned — quote no longer in the current \
             content</span>",
        );
    } else if ann.reanchored {
        flags.push_str("<span class=\"browse-annotation-flag\">re-anchored</span>");
    }
    // The finding family's extra furniture: the model's proposed severity, the
    // pipeline state, and — while it is pending — the human's decision form.
    // ⚠ A finding card NEVER renders as a plain annotation card: a reader who
    // cannot tell a published note from a machine claim awaiting approval is
    // looking at the failure this whole family exists to prevent.
    let (state_class, severity_html, decision_html) = match ann.family {
        Family::Annotation => (String::new(), String::new(), String::new()),
        Family::Finding => (
            format!(
                " browse-finding browse-finding-{}",
                ann.state().unwrap_or("pending")
            ),
            crate::finding::severity_badge_html(ann.severity.as_deref(), ann.decision.as_ref()),
            crate::finding::decision_html(&ann.id, ann.severity.as_deref(), ann.decision.as_ref()),
        ),
    };
    format!(
        "<div class=\"browse-annotation{orphan_class}{machine_class}{state_class}\" \
         id=\"annotation-{id}\">{path}{anchor}{model}{severity_html}\
         <blockquote class=\"browse-annotation-quote\">{exact}</blockquote>\
         <p class=\"browse-annotation-body\">{body}</p>{flags}{decision_html}</div>",
        id = esc(&ann.id),
        exact = esc(&ann.exact),
        body = esc(&ann.body),
    )
}

/// The create affordance: a server-rendered form the HOST's `/k/` adapter
/// turns into a Sink of `urn:iki:annotation` (form fields become sink args — the
/// same adapter assumption the S0 faces document for `hx-get`). htmx
/// attributes only; no scripts.
fn annotation_form_html(target_iri: &str) -> String {
    format!(
        "<form class=\"browse-annotate\" hx-post=\"/k/sink urn:iki:annotation\" \
         hx-target=\"#browse\" hx-swap=\"innerHTML\">\
         <input type=\"hidden\" name=\"target\" value=\"{target}\">\
         <input name=\"exact\" placeholder=\"quote to anchor\" required>\
         <textarea name=\"body\" placeholder=\"note\" required></textarea>\
         <button type=\"submit\">annotate</button></form>",
        target = esc(target_iri)
    )
}

/// The annotations panel under a file view (or the standalone listing
/// fragment): cards in reading order, then the create affordance.
fn annotations_panel_html(target_iri: &str, rows: &[(Annotation, Option<u64>)]) -> String {
    let mut out = String::from("<div class=\"browse-annotations\">");
    for (ann, line) in rows {
        out.push_str(&annotation_card_html(ann, *line, false));
    }
    out.push_str(&annotation_form_html(target_iri));
    out.push_str("</div>");
    out
}

fn annotations_listing_html(repo: &str, rel: &str, rows: &[(Annotation, Option<u64>)]) -> String {
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&crumbs_html(repo, rel));
    if rel.is_empty() {
        // The repo-wide listing spans many targets — cards only, no form (a
        // create needs a concrete target).
        out.push_str("<div class=\"browse-annotations\">");
        for (ann, line) in rows {
            out.push_str(&annotation_card_html(ann, *line, false));
        }
        out.push_str("</div>");
    } else {
        out.push_str(&annotations_panel_html(&file_iri(repo, rel), rows));
    }
    out.push_str("</div>");
    out
}

/// One anchored line's inline marker in the file view: the annotation id
/// (`#annotation-{id}` — its card's anchor in the panel below) and a clipped,
/// one-line note for the marker's native tooltip.
pub(crate) struct Marker {
    pub(crate) id: String,
    pub(crate) note: String,
    /// Machine-minted (review) markers render hollow (`○`) against the solid
    /// human dot (`●`) — the two kinds are distinguishable at the line.
    pub(crate) machine: bool,
}

/// The file face's overlay (called from the S0 HTML view when a store is
/// mounted): per 1-based line, the markers of its live (non-orphaned)
/// anchors — the view marks the line and renders them inline — plus the
/// rendered panel. Orphans keep their card in the panel but get no marker
/// (their quote is at no current line). Runs the same drift pass as Source,
/// against the content the view already read.
pub(crate) fn file_overlay(
    archive: &Archive,
    repo: &str,
    rel: &str,
    text: &str,
) -> Result<(BTreeMap<u64, Vec<Marker>>, String)> {
    let rows = reconcile_against_text(archive, repo, rel, text)?;
    let mut marked: BTreeMap<u64, Vec<Marker>> = BTreeMap::new();
    for (ann, line) in &rows {
        if ann.orphaned {
            continue;
        }
        let Some(line) = line else { continue };
        marked.entry(*line).or_default().push(Marker {
            id: ann.id.clone(),
            note: clip_to(&collapse(&ann.body), 160),
            machine: ann.machine(),
        });
    }
    let panel = annotations_panel_html(&file_iri(repo, rel), &rows);
    Ok((marked, panel))
}

// --- the review layer's mint (S4): a PENDING FINDING, never an annotation ---

/// Mint one **pending finding** — a machine review claim in
/// [`Family::Finding`], awaiting a human.
///
/// ★ This used to mint an `oa:Annotation` directly, and that was the terminal
/// step of every review pass. It is not any more (ledger #444): *"nothing gets
/// published to Gonk except by the human"*, so a pass produces findings and
/// **promotion on publish is the only path into the annotation family**. Both
/// causes of a pass — the Review button and a git-event trigger — land here,
/// which is what makes the "a clicked review and a triggered one are the same
/// thing" invariant hold by CONSTRUCTION rather than by care: publication is a
/// third act and it is always human.
///
/// The provenance is the same shape a machine annotation carried:
/// `dcterms:creator` (the model), `prov:wasGeneratedBy` (the pass entry), plus
/// `sh:resultSeverity` — the MODEL'S PROPOSAL, which a human may later
/// override without ever overwriting. (No `oa:motivatedBy`: see
/// [`store_annotation`] — its domain would type the finding into the
/// annotation family.)
///
/// ## The id is derived from the POSITION, not minted fresh
///
/// `sha256(pass ‖ char_start ‖ exact)`, truncated — so a re-derivation of the
/// same pass over the same content re-mints the SAME finding IRI and an
/// existing decision survives it. That matters because the review archive is a
/// cache, not a ledger: compaction (ledger #437) or a cleared store would
/// otherwise turn every re-run into a fresh queue of findings a human has
/// already answered. A uuid would have made re-runs cost a second triage pass
/// each time.
///
/// Anchors `exact` in `text` (already in hand — the pass sourced it) with no
/// context hints. `surface` selects the anchoring discipline — [`Surface::Diff`]
/// for a PR pass tolerates dropped/wrong leading diff markers and stores the
/// original diff line as the exact. Returns the finding's IRI, or `None` when
/// the quote does not anchor — the caller counts it and moves on (one bad
/// item must not kill the pass). `target_iri` names the reviewed surface (a
/// file, or a PR page whose diff is `text`); `rel` is its path, empty for a PR.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mint_pending_finding(
    archive: &Archive,
    target_iri: &str,
    repo: &str,
    rel: &str,
    text: &str,
    hash: &str,
    exact: &str,
    note: &str,
    severity: Option<&str>,
    model: &str,
    pass_iri: &str,
    created: Option<String>,
    surface: Surface,
) -> Result<Option<String>> {
    let Some(DiffAnchor {
        anchor,
        stored_exact,
    }) = find_anchor_on(surface, text, exact, "", "")
    else {
        return Ok(None);
    };
    let exact = stored_exact.as_deref().unwrap_or(exact);
    let (prefix, suffix) = context_around(text, &anchor);
    let id = finding_id(pass_iri, anchor.char_start, exact);
    // Re-minting an already-answered finding must not erase the answer, and
    // must not resurrect a decided one as pending.
    if let Some(existing) = load_record(archive, Family::Finding, &id)? {
        return Ok(Some(existing.iri()));
    }
    let ann = Annotation {
        family: Family::Finding,
        id,
        body: note.to_string(),
        target_iri: target_iri.to_string(),
        repo: repo.to_string(),
        rel: rel.to_string(),
        hash: hash.to_string(),
        prefix,
        exact: exact.to_string(),
        suffix,
        start: anchor.char_start,
        end: anchor.char_end,
        created,
        reanchored: false,
        orphaned: false,
        creator: Some(model.to_string()),
        // Carried for the promotion: the annotation this becomes is a machine
        // claim a human published, so it is still `oa:assessing`. Never
        // STORED on the finding itself.
        motivation: Some(MOTIVATION_REVIEW.to_string()),
        generated_by: Some(pass_iri.to_string()),
        severity: severity.map(str::to_string),
        derived_from: None,
        decision: None,
    };
    store_annotation(archive, &ann)?;
    Ok(Some(ann.iri()))
}

/// The deterministic finding id — see [`mint_pending_finding`]. 24 hex
/// characters of sha256 over the three things that fix a finding's POSITION:
/// which pass produced it, where in the content it anchored, and what it
/// quoted.
pub(crate) fn finding_id(pass_iri: &str, char_start: u64, exact: &str) -> String {
    let digest = Sha256::digest(format!("{pass_iri}\n{char_start}\n{exact}").as_bytes());
    format!("{digest:x}").chars().take(24).collect()
}

/// The named annotations (a review pass's minted set), drift-reconciled
/// against content already in hand — the review faces' rows. Ids that no
/// longer load (someone deleted the annotation) are skipped: the pass entry
/// records history, the store records the present. Same [`Included`] the
/// `annotations=include` folds serve, so the faces are shared.
pub(crate) fn included_for_ids(archive: &Archive, iris: &[String], text: &str) -> Result<Included> {
    let current = CurrentContent::Text(text.to_string(), content_hash(text.as_bytes()));
    let mut rows: Vec<(Annotation, Option<u64>)> = Vec::with_capacity(iris.len());
    for iri in iris {
        // Either family: a pass entry records finding IRIs now, and entries
        // archived before ledger #444 record annotation IRIs. Both still read.
        let Some((family, id)) = Family::split(iri) else {
            continue;
        };
        let Some(mut ann) = load_record(archive, family, id)? else {
            continue;
        };
        let line = refresh(archive, &mut ann, &current)?;
        rows.push((ann, line));
    }
    rows.sort_by(|(a, _), (b, _)| (a.start, &a.id).cmp(&(b.start, &b.id)));
    Ok(Included {
        rows,
        with_paths: false,
    })
}

// --- target-scoped reads (the PR page's surface) ------------------------------

/// Every annotation whose recorded target is `target_iri` (matching the
/// legacy `ik:target` predicate too), in (position, id) order.
fn list_annotations_for_target(archive: &Archive, target_iri: &str) -> Result<Vec<Annotation>> {
    let target = match NamedNode::new(target_iri) {
        Ok(node) => node,
        Err(_) => return Ok(Vec::new()),
    };
    let mut ids = std::collections::BTreeSet::new();
    for predicate in [ik("annotates"), ik("target")] {
        for quad in
            archive.quads_for_pattern(None, Some(predicate.as_ref()), Some(target.as_ref().into()))
        {
            let quad = quad.map_err(store_err)?;
            let subject = quad.subject.to_string();
            let iri = subject.trim_start_matches('<').trim_end_matches('>');
            if let Some(id) = iri.strip_prefix("urn:iki:annotation:") {
                ids.insert(id.to_string());
            }
        }
    }
    let mut out = Vec::new();
    for id in &ids {
        if let Some(ann) = load_annotation(archive, id)? {
            out.push(ann);
        }
    }
    out.sort_by(|a, b| (a.start, &a.id).cmp(&(b.start, &b.id)));
    Ok(out)
}

/// The drift pass for one target against content already in hand — the PR
/// page's path (its diff is the anchor surface), rows in reading order.
fn reconcile_target_against_text(
    archive: &Archive,
    target_iri: &str,
    text: &str,
) -> Result<Vec<(Annotation, Option<u64>)>> {
    let mut anns = list_annotations_for_target(archive, target_iri)?;
    let current = CurrentContent::Text(text.to_string(), content_hash(text.as_bytes()));
    let mut rows: Vec<(Annotation, Option<u64>)> = Vec::with_capacity(anns.len());
    for mut ann in anns.drain(..) {
        let line = refresh(archive, &mut ann, &current)?;
        rows.push((ann, line));
    }
    rows.sort_by(|(a, _), (b, _)| (a.start, &a.id).cmp(&(b.start, &b.id)));
    Ok(rows)
}

/// The overlay for a rendered text surface whose annotations target one IRI
/// (the PR page's diff view): markers per annotated line plus the panel with
/// its create form targeting that IRI. The [`file_overlay`] of the PR world.
pub(crate) fn target_overlay(
    archive: &Archive,
    target_iri: &str,
    text: &str,
) -> Result<(BTreeMap<u64, Vec<Marker>>, String)> {
    let rows = reconcile_target_against_text(archive, target_iri, text)?;
    let mut marked: BTreeMap<u64, Vec<Marker>> = BTreeMap::new();
    for (ann, line) in &rows {
        if ann.orphaned {
            continue;
        }
        let Some(line) = line else { continue };
        marked.entry(*line).or_default().push(Marker {
            id: ann.id.clone(),
            note: clip_to(&collapse(&ann.body), 160),
            machine: ann.machine(),
        });
    }
    let panel = annotations_panel_html(target_iri, &rows);
    Ok((marked, panel))
}

/// The `annotations=include` payload for a target whose text is already in
/// hand — the PR page's fold.
pub(crate) fn included_for_target_text(
    archive: &Archive,
    target_iri: &str,
    text: &str,
) -> Result<Included> {
    Ok(Included {
        rows: reconcile_target_against_text(archive, target_iri, text)?,
        with_paths: false,
    })
}

// --- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{ArgRef, Capability, Kernel, Request};
    use oxigraph::model::vocab::rdf;
    use oxigraph::model::GraphName;
    use oxigraph::store::Store;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-annotate-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn kernel(root: &std::path::Path, store: &Arc<Store>) -> Kernel {
        Kernel::new(Arc::new(crate::space_with_annotations(
            vec![("demo".to_string(), root.to_path_buf())],
            Arc::clone(store),
        )))
    }

    /// browse read on demo + annotate: the full-capability caller.
    fn cap() -> Capability {
        Capability::scoped(["urn:cap:browse:read:demo", CAP_ANNOTATE])
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

    fn body(repr: &Representation) -> String {
        String::from_utf8_lossy(&repr.bytes).into_owned()
    }

    fn json_of(repr: &Representation) -> serde_json::Value {
        serde_json::from_str(&body(repr)).unwrap()
    }

    /// Create an annotation of `exact` on demo's `path` under `id`.
    fn annotate(k: &Kernel, id: &str, path: &str, exact: &str, note: &str) -> serde_json::Value {
        let out = issue(
            k,
            Verb::Sink,
            &format!("urn:iki:annotation:{id}"),
            &[
                ("target", &format!("urn:repo:demo:file:{path}")),
                ("exact", exact),
                ("body", note),
                ("as", "application/json"),
            ],
            &cap(),
        )
        .unwrap();
        json_of(&out)
    }

    /// A mount that NAMES its graph: `Mount::graph`, over the same store the
    /// helper above uses without one.
    fn kernel_in_graph(root: &std::path::Path, store: &Arc<Store>, graph: &str) -> Kernel {
        Kernel::new(Arc::new(
            crate::Mount::new(vec![("demo".to_string(), root.to_path_buf())])
                .annotations(Arc::clone(store))
                .graph(NamedNode::new(graph).unwrap())
                .space(),
        ))
    }

    /// The graph names of everything in the store, so a test can say WHERE a
    /// quad landed and not only that it exists.
    fn graphs_of(store: &Store) -> std::collections::BTreeSet<String> {
        store
            .iter()
            .map(|q| q.unwrap().graph_name.to_string())
            .collect()
    }

    /// ★ The half that is easy to miss. Before 0.4.0 the reads passed `None`
    /// for the graph, and `None` in `quads_for_pattern` means EVERY graph in
    /// the store — so a knob that moved only the writes would have left browse
    /// writing into its own graph and still answering out of anyone else's.
    ///
    /// This is also the answer to "is confining the DEFAULT case to the
    /// default graph the same as matching all graphs?" It is not, and the
    /// difference is exactly this decoy: for a store browse is the only writer
    /// of they coincide, and for a shared one they do not. That is a real
    /// behaviour change in 0.4.0, and it is the change that closes the hole.
    #[test]
    fn a_default_mount_does_not_read_another_graph() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "mine", "a.rs", "fn one()", "the default graph's own");

        // A decoy shaped exactly like a real annotation — same predicates,
        // same type, same repo — sitting in SOMEONE ELSE'S graph.
        let other = NamedNode::new("urn:iki:graph:another-tenant").unwrap();
        let decoy = NamedNode::new("urn:iki:annotation:theirs").unwrap();
        for (p, o) in [
            (
                NamedNode::new("http://www.w3.org/1999/02/22-rdf-syntax-ns#type").unwrap(),
                Term::NamedNode(oa("Annotation")),
            ),
            (
                oa("bodyValue"),
                Term::Literal(Literal::new_simple_literal("not ours")),
            ),
            (
                ik("repo"),
                Term::Literal(Literal::new_simple_literal("demo")),
            ),
            (
                ik("path"),
                Term::Literal(Literal::new_simple_literal("a.rs")),
            ),
            (
                ik("annotates"),
                Term::NamedNode(NamedNode::new("urn:repo:demo:file:a.rs").unwrap()),
            ),
        ] {
            store
                .insert(Quad::new(decoy.clone(), p, o, other.clone()).as_ref())
                .unwrap();
        }

        let listed =
            body(&issue(&k, Verb::Source, "urn:repo:demo:annotations", &[], &cap()).unwrap());
        assert!(listed.contains("urn:iki:annotation:mine"), "{listed}");
        assert!(
            !listed.contains("urn:iki:annotation:theirs"),
            "a read reached into another graph: {listed}"
        );
        let err = issue(&k, Verb::Source, "urn:iki:annotation:theirs", &[], &cap()).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The other direction: a mount that names a graph writes there and reads
    /// there, and the two mounts over ONE store cannot see each other.
    #[test]
    fn a_named_graph_mount_writes_and_reads_only_its_graph() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let tenant = kernel_in_graph(&root, &store, "urn:iki:graph:tenant-a");
        annotate(&tenant, "t1", "a.rs", "fn one()", "tenant a's note");

        assert_eq!(
            graphs_of(&store),
            std::collections::BTreeSet::from(["<urn:iki:graph:tenant-a>".to_string()]),
            "every quad landed in the named graph, and none in the default one"
        );
        let read = json_of(
            &issue(
                &tenant,
                Verb::Source,
                "urn:iki:annotation:t1",
                &[("as", "application/json")],
                &cap(),
            )
            .unwrap(),
        );
        assert_eq!(read["exact"], "fn one()");

        // The default mount over the SAME store is blind to it, which is the
        // whole point of the boundary.
        let plain = kernel(&root, &store);
        let err = issue(&plain, Verb::Source, "urn:iki:annotation:t1", &[], &cap()).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        assert!(!body(
            &issue(
                &plain,
                Verb::Source,
                "urn:repo:demo:annotations",
                &[],
                &cap()
            )
            .unwrap()
        )
        .contains("t1"));

        // Delete stays inside the graph too — and takes every selector quad.
        issue(&tenant, Verb::Delete, "urn:iki:annotation:t1", &[], &cap()).unwrap();
        assert_eq!(store.len().unwrap(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    /// The knob and the migration compose: data written by a default mount is
    /// readable by a graph-naming mount AFTER `migrate::plan_into_graph`, and
    /// not before. Without this step the archive looks EMPTY — the silent
    /// failure the namespace rename had, which is why the move ships with the
    /// knob.
    #[test]
    fn a_migrated_store_reads_back_under_the_named_graph() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        annotate(
            &kernel(&root, &store),
            "legacy",
            "a.rs",
            "fn two()",
            "written before the host opted in",
        );

        let graph = GraphName::NamedNode(NamedNode::new("urn:iki:graph:browse").unwrap());
        let tenant = kernel_in_graph(&root, &store, "urn:iki:graph:browse");
        assert!(
            issue(
                &tenant,
                Verb::Source,
                "urn:iki:annotation:legacy",
                &[],
                &cap()
            )
            .is_err(),
            "unmigrated data is invisible to the mount that named the graph"
        );

        let before = crate::migrate::counts_for_graph(&store, Some(&graph)).unwrap();
        let plan = crate::migrate::plan_into_graph(&store, &graph).unwrap();
        crate::migrate::apply(&store, &plan).unwrap();
        let after = crate::migrate::counts_for_graph(&store, Some(&graph)).unwrap();
        assert!(
            crate::migrate::Counts::passed(&before, &after),
            "{}",
            crate::migrate::report(&before, &after, "after")
        );

        let read = json_of(
            &issue(
                &tenant,
                Verb::Source,
                "urn:iki:annotation:legacy",
                &[("as", "application/json")],
                &cap(),
            )
            .unwrap(),
        );
        assert_eq!(read["exact"], "fn two()");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Two knobs, one decision: a disagreement is a misconfiguration, and it
    /// fails at mount time like a bad root name rather than splitting a host's
    /// data across two graphs where only a store audit would find it.
    #[test]
    #[should_panic(expected = "name different graphs")]
    fn a_mount_and_a_config_that_name_different_graphs_fail_loud() {
        let store = Arc::new(Store::new().unwrap());
        let config = crate::ExplainConfig::new(store)
            .graph(NamedNode::new("urn:iki:graph:from-config").unwrap());
        let _ = crate::Mount::new(vec![("demo".to_string(), temp_dir())])
            .graph(NamedNode::new("urn:iki:graph:from-mount").unwrap())
            .explain(config)
            .space();
    }

    /// The same two knobs AGREEING is legal — a host wiring both from one
    /// setting must not have to pick which call site to leave out.
    #[test]
    fn a_mount_and_a_config_that_agree_are_accepted() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let g = NamedNode::new("urn:iki:graph:agreed").unwrap();
        let space = crate::Mount::new(vec![("demo".to_string(), root.clone())])
            .graph(g.clone())
            .explain(crate::ExplainConfig::new(Arc::clone(&store)).graph(g))
            .space();
        let k = Kernel::new(Arc::new(space));
        annotate(&k, "agreed", "a.rs", "fn one()", "one graph");
        assert_eq!(
            graphs_of(&store),
            std::collections::BTreeSet::from(["<urn:iki:graph:agreed>".to_string()])
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// `ExplainConfig::graph` ALONE — the only spelling a host calling
    /// `space_with_explain` has, which is what `ikigai-cli`'s embedded host
    /// does. `Mount::graph` never being called must not quietly win.
    #[test]
    fn a_config_graph_alone_governs_the_whole_mount() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let space = crate::space_with_explain(
            vec![("demo".to_string(), root.clone())],
            crate::ExplainConfig::new(Arc::clone(&store))
                .graph(NamedNode::new("urn:iki:graph:from-config").unwrap()),
        );
        let k = Kernel::new(Arc::new(space));
        annotate(&k, "c1", "a.rs", "fn one()", "config named the graph");
        assert_eq!(
            graphs_of(&store),
            std::collections::BTreeSet::from(["<urn:iki:graph:from-config>".to_string()]),
            "the annotation family follows the graph the EXPLAIN config named"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The claim `crate::archive` and `crate::migrate::BROWSE_SUBJECT_PREFIXES`
    /// both rest on: browse writes no quad about a subject it did not mint. It
    /// is what makes the graph migration's subject-selection COMPLETE — a
    /// writer that stored a triple on, say, the annotated file's own IRI would
    /// leave that quad behind in the default graph, silently.
    #[test]
    fn every_quad_browse_writes_has_a_browse_minted_subject() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "n1", "a.rs", "fn one()", "first");
        annotate(&k, "n2", "a.rs", "fn two()", "second");
        // Re-anchoring rewrites the graph on a Source — another write path.
        std::fs::write(root.join("a.rs"), "// moved\nfn one() {}\nfn two() {}\n").unwrap();
        issue(&k, Verb::Source, "urn:iki:annotation:n1", &[], &cap()).unwrap();

        assert!(store.len().unwrap() > 0);
        for quad in store.iter() {
            let subject = quad.unwrap().subject.to_string();
            let iri = subject.trim_start_matches('<').trim_end_matches('>');
            assert!(
                crate::migrate::BROWSE_SUBJECT_PREFIXES
                    .iter()
                    .any(|p| iri.starts_with(p)),
                "a stored quad hangs off a subject browse did not mint: {iri}"
            );
        }

        // The other two writers store every quad under ONE subject each, and
        // these are the functions that mint it.
        assert!(
            crate::explain::entry_iri("demo", "a.rs", "sha256:abc", "code-v1")
                .starts_with("urn:ikigai:browse:")
        );
        assert!(
            crate::review::pass_iri("demo", "a.rs", "sha256:abc", "review-v1")
                .starts_with("urn:ikigai:browse:")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn crud_round_trips_through_a_kernel() {
        let root = temp_dir();
        std::fs::write(
            root.join("a.rs"),
            "fn one() {}\nfn two() {}\nfn three() {}\n",
        )
        .unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);

        let created = annotate(&k, "note-1", "a.rs", "fn two()", "the middle function");
        assert_eq!(created["iri"], "urn:iki:annotation:note-1");
        assert_eq!(created["annotates"], "urn:repo:demo:file:a.rs");
        assert_eq!(created["line"], 2);
        assert_eq!(created["start"], 12);
        assert_eq!(created["end"], 20);
        assert!(created["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));

        // Source: text/plain is the body; json carries the whole record.
        let plain = issue(&k, Verb::Source, "urn:iki:annotation:note-1", &[], &cap()).unwrap();
        assert_eq!(body(&plain), "the middle function");
        let full = json_of(
            &issue(
                &k,
                Verb::Source,
                "urn:iki:annotation:note-1",
                &[("as", "application/json")],
                &cap(),
            )
            .unwrap(),
        );
        assert_eq!(full["exact"], "fn two()");
        assert_eq!(full["orphaned"], false);
        assert_eq!(full["reanchored"], false);

        // Update under the same id: new body, created preserved.
        let first_created = full["created"].as_str().map(str::to_string);
        let updated = annotate(&k, "note-1", "a.rs", "fn three()", "now the third");
        assert_eq!(updated["line"], 3);
        assert_eq!(
            updated["created"].as_str().map(str::to_string),
            first_created,
            "an update keeps dcterms:created"
        );

        // Delete removes it and every selector quad with it.
        let ack = issue(&k, Verb::Delete, "urn:iki:annotation:note-1", &[], &cap()).unwrap();
        assert_eq!(body(&ack), "deleted urn:iki:annotation:note-1");
        let err = issue(&k, Verb::Source, "urn:iki:annotation:note-1", &[], &cap()).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        assert_eq!(
            store.len().unwrap(),
            0,
            "delete leaves no selector quads behind"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_bare_sink_mints_a_uuid_id() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);

        let ack = issue(
            &k,
            Verb::Sink,
            "urn:iki:annotation",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "fn one()"),
                ("body", "minted"),
            ],
            &cap(),
        )
        .unwrap();
        let iri = body(&ack);
        let id = iri.strip_prefix("urn:iki:annotation:").unwrap();
        assert_eq!(id.len(), 36, "a v4 uuid: {iri}");
        // The minted IRI resolves.
        let read = issue(&k, Verb::Source, &iri, &[], &cap()).unwrap();
        assert_eq!(body(&read), "minted");

        // Source/Delete on the bare IRI have no id to work with.
        let err = issue(&k, Verb::Source, "urn:iki:annotation", &[], &cap()).unwrap_err();
        assert!(matches!(err, Error::MissingArgument(_)), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_body_falls_back_to_piped_content() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        issue(
            &k,
            Verb::Sink,
            "urn:iki:annotation:piped",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "fn one()"),
                ("content", "the piped note"),
            ],
            &cap(),
        )
        .unwrap();
        let read = issue(&k, Verb::Source, "urn:iki:annotation:piped", &[], &cap()).unwrap();
        assert_eq!(body(&read), "the piped note");

        // Neither body nor content: a typed missing-argument error.
        let err = issue(
            &k,
            Verb::Sink,
            "urn:iki:annotation:empty",
            &[("target", "urn:repo:demo:file:a.rs"), ("exact", "fn one()")],
            &cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::MissingArgument(_)), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn capabilities_gate_per_verb() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "n", "a.rs", "fn one()", "note");

        // Sink without annotate: denied by the kernel baseline (declared =
        // enforced), before the endpoint runs.
        let browse_only = Capability::scoped(["urn:cap:browse:read:demo"]);
        let err = issue(
            &k,
            Verb::Sink,
            "urn:iki:annotation:x",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "fn one()"),
                ("body", "no"),
            ],
            &browse_only,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");

        // Source without any browse read: denied at the baseline.
        let annotate_only = Capability::scoped([CAP_ANNOTATE]);
        let err = issue(
            &k,
            Verb::Source,
            "urn:iki:annotation:n",
            &[],
            &annotate_only,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");

        // Source with a browse grant on the WRONG root: past the baseline
        // wildcard, denied by the per-root check on the annotation's repo.
        let wrong_root = Capability::scoped(["urn:cap:browse:read:other"]);
        let err = issue(&k, Verb::Source, "urn:iki:annotation:n", &[], &wrong_root).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");

        // Delete without annotate: denied.
        let err = issue(&k, Verb::Delete, "urn:iki:annotation:n", &[], &browse_only).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");

        // The listing requires a browse grant like every browse read.
        let err = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:annotations",
            &[],
            &annotate_only,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn listings_filter_per_path_and_repo_wide() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        std::fs::write(root.join("b.rs"), "fn other() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        // Created out of reading order — listings must sort by position.
        annotate(&k, "n2", "a.rs", "fn two()", "second in a");
        annotate(&k, "n1", "a.rs", "fn one()", "first in a");
        annotate(&k, "nb", "b.rs", "fn other()", "in b");

        let per_file = json_of(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:annotations:a.rs",
                &[],
                &cap(),
            )
            .unwrap(),
        );
        let ids: Vec<&str> = per_file
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["n1", "n2"], "a.rs only, in position order");

        let repo_wide =
            json_of(&issue(&k, Verb::Source, "urn:repo:demo:annotations", &[], &cap()).unwrap());
        let ids: Vec<&str> = repo_wide
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["n1", "n2", "nb"], "path-major reading order");
        std::fs::remove_dir_all(&root).ok();
    }

    // --- the re-anchoring suite ---------------------------------------------

    #[test]
    fn anchored_markers_appear_at_their_lines_and_orphans_keep_their_card() {
        let root = temp_dir();
        std::fs::write(
            root.join("a.rs"),
            "fn one() {}\nfn two() {}\nfn three() {}\n",
        )
        .unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "mid", "a.rs", "fn two()", "the middle function");
        let html = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:file:a.rs",
                &[("as", "text/html")],
                &cap(),
            )
            .unwrap(),
        );
        // The marker renders INSIDE the anchored line: its card anchor sits
        // between L2's opening and L3's, and the line carries the mark class.
        let l2 = html.find("id=\"L2\"").expect("L2 anchor");
        let l3 = html.find("id=\"L3\"").expect("L3 anchor");
        let marker = html.find("href=\"#annotation-mid\"").expect("marker");
        assert!(l2 < marker && marker < l3, "{html}");
        assert!(
            html.contains("browse-line browse-line-annotated\" id=\"L2\""),
            "{html}"
        );
        assert!(
            html.contains("class=\"browse-annotation-marker\""),
            "{html}"
        );
        assert!(html.contains("title=\"the middle function\""), "{html}");
        // The bottom panel still lists the card.
        assert!(html.contains("id=\"annotation-mid\""), "{html}");

        // Edit the quote away: the card stays (orphan-flagged, the overview
        // is where orphans live), the marker goes.
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn three() {}\n").unwrap();
        let html = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:file:a.rs",
                &[("as", "text/html")],
                &cap(),
            )
            .unwrap(),
        );
        assert!(!html.contains("browse-annotation-marker"), "{html}");
        assert!(!html.contains("browse-line-annotated"), "{html}");
        assert!(html.contains("browse-annotation-orphaned"), "{html}");
        assert!(html.contains("id=\"annotation-mid\""), "{html}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_moved_quote_reanchors_and_the_reanchor_persists() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn target() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let created = annotate(&k, "n", "a.rs", "fn target()", "watch this");
        assert_eq!(created["line"], 1);
        let original_hash = created["content_hash"].as_str().unwrap().to_string();

        // Line churn ABOVE the quote: the quote itself is untouched.
        std::fs::write(root.join("a.rs"), "// new\n// lines\nfn target() {}\n").unwrap();
        let read = json_of(
            &issue(
                &k,
                Verb::Source,
                "urn:iki:annotation:n",
                &[("as", "application/json")],
                &cap(),
            )
            .unwrap(),
        );
        assert_eq!(read["line"], 3, "position selector follows the quote");
        assert_eq!(read["reanchored"], true);
        assert_eq!(read["orphaned"], false);
        assert_ne!(read["content_hash"].as_str().unwrap(), original_hash);

        // The re-anchor PERSISTED: the stored graph carries the new hash and
        // positions (checked straight in the store, not through the face).
        let archive = Archive::new(Arc::clone(&store), GraphName::DefaultGraph);
        let stored = load_annotation(&archive, "n").unwrap().unwrap();
        assert_eq!(stored.hash, read["content_hash"].as_str().unwrap());
        assert_eq!(stored.start, read["start"].as_u64().unwrap());
        assert!(stored.reanchored);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_edited_away_quote_is_orphaned_never_dropped() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn target() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let created = annotate(&k, "n", "a.rs", "fn target()", "watch this");
        let recorded_start = created["start"].as_u64().unwrap();

        std::fs::write(root.join("a.rs"), "fn renamed() {}\n").unwrap();
        let read = json_of(
            &issue(
                &k,
                Verb::Source,
                "urn:iki:annotation:n",
                &[("as", "application/json")],
                &cap(),
            )
            .unwrap(),
        );
        assert_eq!(read["orphaned"], true);
        assert_eq!(read["body"], "watch this", "orphans still render");
        assert_eq!(
            read["start"].as_u64().unwrap(),
            recorded_start,
            "recorded positions are kept"
        );
        // And it still appears in listings, flagged.
        let listing = json_of(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:annotations:a.rs",
                &[],
                &cap(),
            )
            .unwrap(),
        );
        assert_eq!(listing.as_array().unwrap().len(), 1);
        assert_eq!(listing[0]["orphaned"], true);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_orphan_is_not_reflagged_on_repeat_reads() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn target() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "n", "a.rs", "fn target()", "watch this");
        std::fs::write(root.join("a.rs"), "fn renamed() {}\n").unwrap();

        issue(&k, Verb::Source, "urn:iki:annotation:n", &[], &cap()).unwrap();
        issue(&k, Verb::Source, "urn:iki:annotation:n", &[], &cap()).unwrap();
        issue(
            &k,
            Verb::Source,
            "urn:repo:demo:annotations:a.rs",
            &[],
            &cap(),
        )
        .unwrap();

        // Exactly ONE orphaned triple, no duplicates from the repeat reads.
        let subject = NamedNode::new("urn:iki:annotation:n").unwrap();
        let orphan_quads: Vec<_> = store
            .quads_for_pattern(
                Some(subject.as_ref().into()),
                Some(ik("orphaned").as_ref()),
                None,
                None,
            )
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(orphan_quads.len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_restored_quote_clears_the_orphan_flag() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn target() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "n", "a.rs", "fn target()", "watch this");

        std::fs::write(root.join("a.rs"), "fn renamed() {}\n").unwrap();
        let read = json_of(
            &issue(
                &k,
                Verb::Source,
                "urn:iki:annotation:n",
                &[("as", "application/json")],
                &cap(),
            )
            .unwrap(),
        );
        assert_eq!(read["orphaned"], true);

        // The edit is reverted: the quote is back, the annotation is whole.
        std::fs::write(root.join("a.rs"), "fn target() {}\n").unwrap();
        let read = json_of(
            &issue(
                &k,
                Verb::Source,
                "urn:iki:annotation:n",
                &[("as", "application/json")],
                &cap(),
            )
            .unwrap(),
        );
        assert_eq!(read["orphaned"], false);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_ambiguous_quote_anchors_by_context_then_first_match() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "let x = 1;\nlet y = 1;\nlet z = 1;\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);

        // `= 1;` occurs three times — the prefix hint picks the middle one.
        let with_context = issue(
            &k,
            Verb::Sink,
            "urn:iki:annotation:ctx",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "= 1;"),
                ("prefix", "let y "),
                ("body", "the y line"),
                ("as", "application/json"),
            ],
            &cap(),
        )
        .unwrap();
        assert_eq!(json_of(&with_context)["line"], 2);

        // No context: deterministically the FIRST occurrence.
        let first = annotate(&k, "first", "a.rs", "= 1;", "first wins");
        assert_eq!(first["line"], 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn diff_anchoring_precedence_exact_then_shadow_then_stripped_quote() {
        let diff = "--- a/f\n+++ b/f\n@@ -1,2 +1,3 @@\n context()\n-removed()\n+added()\n";

        // 1. A marker-faithful quote anchors exactly — its own span, no
        //    stored-exact override.
        let hit = find_anchor_in_diff(diff, "+added()", "", "").unwrap();
        assert_eq!(hit.stored_exact, None);
        assert_eq!(hit.anchor.line, 6);

        // A markerless quote that is a plain substring of its line also
        // anchors at stage 1 (substring search never needed the marker).
        let hit = find_anchor_in_diff(diff, "removed()", "", "").unwrap();
        assert_eq!(hit.stored_exact, None);
        assert_eq!(hit.anchor.line, 5);

        // 3. A WRONG marker misses raw and the marker-stripped retry lands
        //    it — the stored exact is the ORIGINAL diff line.
        let hit = find_anchor_in_diff(diff, "-added()", "", "").unwrap();
        assert_eq!(hit.stored_exact.as_deref(), Some("+added()"));
        assert_eq!(hit.anchor.line, 6);

        // 4. A padded marker (`+ code` for `+code`) trims down and lands.
        let hit = find_anchor_in_diff(diff, "+ added()", "", "").unwrap();
        assert_eq!(hit.stored_exact.as_deref(), Some("+added()"));

        // 2. A quote spanning CODE lines crosses the interleaved markers in
        //    the shadow; the stored exact is the original diff lines.
        let hit = find_anchor_in_diff(diff, "removed()\nadded()", "", "").unwrap();
        assert_eq!(hit.stored_exact.as_deref(), Some("-removed()\n+added()"));
        assert_eq!(hit.anchor.line, 5);

        // Still a miss when the code is simply not there.
        assert!(find_anchor_in_diff(diff, "vanished()", "", "").is_none());
    }

    /// Ledger #488: the model prefixes a quote with the marker the document
    /// is written in (`⚠`, `★`, a bullet, a number) rather than the
    /// characters the line carries. One leading strip, retried once, and the
    /// recorded exact is the FILE's characters.
    #[test]
    fn a_decorated_quote_anchors_once_stripped_and_stores_the_files_characters() {
        // The measured shape, verbatim from #488: the file numbers the rule,
        // the model marks it.
        let file = "9d. **`cargo install` IGNORES `Cargo.lock`**\n\
                    9e. **A per-repo cron CANNOT be the ecosystem's clock**\n";
        let quoted = "⚠ **A per-repo cron CANNOT be the ecosystem's clock**";

        let hit = find_anchor_on(Surface::File, file, quoted, "", "").unwrap();
        assert_eq!(hit.anchor.line, 2);
        // ★ What is stored is the file's text, never the model's: the `⚠`
        // AND the `**` the strip took with it are gone.
        let stored = hit.stored_exact.as_deref().unwrap();
        assert_eq!(stored, "A per-repo cron CANNOT be the ecosystem's clock**");
        assert!(file.contains(stored), "stored exact must be file text");
        assert!(!stored.starts_with('⚠'));

        // ★ And the id is the id the undecorated quote would have minted —
        // the whole reason `exact` may not be the model's characters.
        let clean = find_anchor_on(Surface::File, file, stored, "", "").unwrap();
        assert_eq!(clean.stored_exact, None, "an exact quote needs no retry");
        assert_eq!(
            finding_id("urn:pass", hit.anchor.char_start, stored),
            finding_id("urn:pass", clean.anchor.char_start, stored),
        );

        // The side of the same rule that keeps it honest: ONE LEADING run.
        // An interior decoration still orphans...
        assert!(
            find_anchor_on(Surface::File, file, "⚠ **A per-repo ⚠ cron", "", "").is_none(),
            "the retry strips a prefix, it does not fuzzy-match"
        );
        // ...a quote that is simply not in the file still orphans, decorated
        // or not (the anchor is the proof the model read the file)...
        assert!(find_anchor_on(Surface::File, file, "⚠ a cron job may never", "", "").is_none());
        // ...and a quote that is ALL decoration anchors nothing rather than
        // matching the empty string everywhere.
        assert!(find_anchor_on(Surface::File, file, "⚠ ** ", "", "").is_none());
    }

    /// The same rule catches the other decoration a model adds for free:
    /// indentation it did not read off the line.
    #[test]
    fn a_reindented_quote_anchors_to_the_files_own_indentation() {
        let file = "fn f() {\n    let x = 1;\n}\n";
        let hit = find_anchor_on(Surface::File, file, "        let x = 1;", "", "").unwrap();
        assert_eq!(hit.stored_exact.as_deref(), Some("let x = 1;"));
        assert_eq!(hit.anchor.line, 2);
    }

    #[test]
    fn a_missing_quote_is_a_typed_argument_error() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let err = issue(
            &k,
            Verb::Sink,
            "urn:iki:annotation:x",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "nowhere to be found"),
                ("body", "no"),
            ],
            &cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");
        // Nothing was stored.
        assert_eq!(store.len().unwrap(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    // --- faces --------------------------------------------------------------

    #[test]
    fn turtle_faces_parse_and_are_skolemized() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "n1", "a.rs", "fn one()", "first");
        annotate(&k, "n2", "a.rs", "fn two()", "second");

        for iri in ["urn:iki:annotation:n1", "urn:repo:demo:annotations:a.rs"] {
            let out = issue(&k, Verb::Source, iri, &[("as", "text/turtle")], &cap()).unwrap();
            assert_eq!(out.repr_type.media_type, "text/turtle");
            let ttl = body(&out);
            let triples: Vec<_> = oxttl::TurtleParser::new()
                .for_slice(out.bytes.as_slice())
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap_or_else(|e| panic!("turtle face must parse: {e}\n{ttl}"));
            assert!(!triples.is_empty());
            for t in &triples {
                assert!(!t.subject.to_string().starts_with("_:"), "{ttl}");
                assert!(!t.object.to_string().starts_with("_:"), "{ttl}");
            }
            assert!(ttl.contains("a oa:Annotation"), "{ttl}");
            assert!(
                ttl.contains("ik:annotates <urn:repo:demo:file:a.rs>"),
                "{ttl}"
            );
            assert!(!ttl.contains("ik:target"), "the retired term: {ttl}");
            assert!(ttl.contains("oa:exact \"fn one()\""), "{ttl}");
            assert!(
                ttl.contains("<urn:iki:annotation:n1:selector:position> a oa:TextPositionSelector"),
                "{ttl}"
            );
            assert!(ttl.contains("ik:contentHash \"sha256:"), "{ttl}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// Pre-0.2.2 stores wrote `ik:target` where the code now writes
    /// `ik:annotates` (the term was ceded to the routing family). Legacy
    /// annotations still load (never-drop extends to renames), and the first
    /// rewrite — an update, re-anchor, or orphan pass — re-stores the graph
    /// under the new term.
    #[test]
    fn a_legacy_ik_target_annotation_reads_and_migrates_on_rewrite() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "old", "a.rs", "fn one()", "kept");

        // Rewrite the stored graph to the legacy shape in place.
        let minted: Vec<Quad> = store
            .quads_for_pattern(None, Some(ik("annotates").as_ref()), None, None)
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(minted.len(), 1);
        for quad in &minted {
            store.remove(quad).unwrap();
            store
                .insert(&Quad::new(
                    quad.subject.clone(),
                    ik("target"),
                    quad.object.clone(),
                    quad.graph_name.clone(),
                ))
                .unwrap();
        }

        // The legacy annotation still reads, subject IRI intact.
        let row = json_of(
            &issue(
                &k,
                Verb::Source,
                "urn:iki:annotation:old",
                &[("as", "application/json")],
                &cap(),
            )
            .unwrap(),
        );
        assert_eq!(row["annotates"], "urn:repo:demo:file:a.rs", "{row}");
        assert_eq!(row["body"], "kept", "{row}");

        // An update rewrites the whole graph — the legacy term is gone.
        annotate(&k, "old", "a.rs", "fn one()", "updated");
        let target_quads = store
            .quads_for_pattern(None, Some(ik("target").as_ref()), None, None)
            .count();
        let annotates_quads = store
            .quads_for_pattern(None, Some(ik("annotates").as_ref()), None, None)
            .count();
        assert_eq!(target_quads, 0, "the rewrite retires the legacy term");
        assert_eq!(annotates_quads, 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_file_html_face_gains_the_annotations_overlay() {
        let root = temp_dir();
        std::fs::write(
            root.join("a.rs"),
            "fn one() {}\nfn two() {}\nfn three() {}\n",
        )
        .unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "n", "a.rs", "fn two()", "the middle one");
        std::fs::write(
            root.join("a.rs"),
            "// pushed down\nfn one() {}\nfn two() {}\nfn three() {}\n",
        )
        .unwrap();

        let html = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:file:a.rs",
                &[("as", "text/html")],
                &cap(),
            )
            .unwrap(),
        );
        // The panel renders the annotation at its RE-ANCHORED line (drift ran
        // during the render), the anchored line is marked in the code view,
        // and the create affordance posts a Sink through the host adapter.
        assert!(html.contains("browse-annotation"), "{html}");
        assert!(html.contains("href=\"#L3\""), "{html}");
        assert!(html.contains("re-anchored"), "{html}");
        assert!(
            html.contains("class=\"browse-line browse-line-annotated\" id=\"L3\""),
            "{html}"
        );
        assert!(
            html.contains("hx-post=\"/k/sink urn:iki:annotation\""),
            "{html}"
        );
        assert!(html.contains("value=\"urn:repo:demo:file:a.rs\""), "{html}");

        // Without a store (the plain S0 space), the face is unchanged: no
        // panel, no form.
        let plain = Kernel::new(Arc::new(crate::space(vec![(
            "demo".to_string(),
            root.clone(),
        )])));
        let html = body(
            &issue(
                &plain,
                Verb::Source,
                "urn:repo:demo:file:a.rs",
                &[("as", "text/html")],
                &Capability::scoped(["urn:cap:browse:read:demo"]),
            )
            .unwrap(),
        );
        assert!(!html.contains("browse-annotations"), "{html}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn orphans_are_flagged_in_the_html_faces() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn target() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "n", "a.rs", "fn target()", "watch this");
        std::fs::write(root.join("a.rs"), "fn renamed() {}\n").unwrap();

        let html = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:file:a.rs",
                &[("as", "text/html")],
                &cap(),
            )
            .unwrap(),
        );
        assert!(html.contains("browse-annotation-orphaned"), "{html}");
        assert!(html.contains("orphaned"), "{html}");
        // An orphan's approximate anchor must NOT mark a code line as
        // annotated (the quote is not there).
        assert!(!html.contains("browse-line-annotated"), "{html}");
        std::fs::remove_dir_all(&root).ok();
    }

    // --- the shared-graph thesis --------------------------------------------

    #[test]
    fn explanations_and_annotations_coexist_in_one_store() {
        use ikigai_core::{Exact, Fallback, FnEndpoint};
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());

        // The explain family (with a canned LLM) AND the annotation family,
        // over the SAME store.
        let llm = EndpointSpace::new().bind(
            Exact::new("urn:llm:coder:ask"),
            FnEndpoint::new("fake-llm", |_inv: &Invocation<'_>| {
                Ok(repr_utf8("text/plain", "An explanation.".to_string()))
            })
            .with_description(
                Description::new("fake-llm")
                    .verb(Verb::Source)
                    .requires(crate::explain::CAP_NET),
            ),
        );
        let browse = crate::space_with_explain(
            vec![("demo".to_string(), root.clone())],
            crate::ExplainConfig::new(Arc::clone(&store)).file_model_label("m1"),
        );
        let k = Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(llm),
        ])));
        let full = Capability::scoped([
            "urn:cap:browse:read:demo",
            "urn:cap:net:localhost",
            CAP_ANNOTATE,
        ]);

        // Derive an explanation and create an annotation on the same file.
        issue(&k, Verb::Source, "urn:repo:demo:explain:a.rs", &[], &full).unwrap();
        issue(
            &k,
            Verb::Sink,
            "urn:iki:annotation:n",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "fn one()"),
                ("body", "note"),
            ],
            &full,
        )
        .unwrap();

        // ONE store now holds both shapes, queryable together: the
        // explanation's ik:about and the annotation's ik:annotates name the
        // same resource IRI.
        let typed = |class: NamedNode| -> Vec<String> {
            store
                .quads_for_pattern(None, Some(rdf::TYPE), Some(class.as_ref().into()), None)
                .map(|q| q.unwrap().subject.to_string())
                .collect()
        };
        let explanations = typed(ik("Explanation"));
        let annotations = typed(oa("Annotation"));
        assert_eq!(explanations.len(), 1, "the explanation is archived");
        assert_eq!(annotations.len(), 1, "the annotation is stored");
        let objects_of = |predicate: NamedNode| -> Vec<String> {
            store
                .quads_for_pattern(None, Some(predicate.as_ref()), None, None)
                .map(|q| match q.unwrap().object {
                    Term::NamedNode(n) => n.as_str().to_string(),
                    other => other.to_string(),
                })
                .collect()
        };
        let abouts = objects_of(ik("about"));
        let annotated = objects_of(ik("annotates"));
        assert_eq!(
            abouts,
            ["urn:repo:demo:file:a.rs"],
            "the explanation's subject"
        );
        assert_eq!(
            annotated,
            ["urn:repo:demo:file:a.rs"],
            "the annotation's target"
        );
        assert!(
            objects_of(ik("target")).is_empty(),
            "nothing writes the retired ik:target term"
        );

        // And both resource families still answer off that one store.
        let explained = issue(&k, Verb::Source, "urn:repo:demo:explain:a.rs", &[], &full).unwrap();
        assert_eq!(body(&explained), "An explanation.");
        let annotated = issue(&k, Verb::Source, "urn:iki:annotation:n", &[], &full).unwrap();
        assert_eq!(body(&annotated), "note");
        std::fs::remove_dir_all(&root).ok();
    }

    // --- contracts ----------------------------------------------------------

    #[test]
    fn describe_declares_per_verb_actions_with_their_capabilities() {
        let description = annotation_description();
        let specs = description.action_specs();
        assert_eq!(specs.len(), 3);
        let of = |verb: Verb| specs.iter().find(|s| s.verb == verb).unwrap();

        let source = of(Verb::Source);
        assert_eq!(source.requires, vec![CAP_WILDCARD.to_string()]);
        let sink = of(Verb::Sink);
        assert!(sink.requires.contains(&CAP_ANNOTATE.to_string()));
        assert!(
            sink.requires.contains(&CAP_WILDCARD.to_string()),
            "anchoring reads the target through the kernel"
        );
        let sink_inputs: Vec<&str> = sink.inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            sink_inputs,
            ["id", "target", "body", "content", "exact", "prefix", "suffix", "as"],
            "`content` is declared beside `body`: the Sink has read a piped body \
             since the family shipped, and a mutating action that reads its payload \
             from somewhere the manifold does not name is a contract bug"
        );
        assert!(
            sink.inputs
                .iter()
                .find(|i| i.name == "target")
                .unwrap()
                .required
        );
        let delete = of(Verb::Delete);
        assert_eq!(delete.requires, vec![CAP_ANNOTATE.to_string()]);

        // The listing is a plain single-verb read.
        let listing = annotations_description();
        assert!(listing.requires.contains(&CAP_WILDCARD.to_string()));
        assert!(!listing.requires.contains(&CAP_ANNOTATE.to_string()));
    }

    #[test]
    fn annotations_include_folds_margin_notes_into_the_file_text_face() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "note-1", "a.rs", "fn two()", "the second\nfunction");

        let out = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:file:a.rs",
            &[("annotations", "include")],
            &cap(),
        )
        .unwrap();
        // The composite face is text/plain — it is no longer just the file.
        assert_eq!(out.repr_type.media_type, "text/plain");
        let text = body(&out);
        assert!(text.starts_with("fn one() {}\nfn two() {}\n"), "{text}");
        assert!(text.contains("--- annotations (1) ---"), "{text}");
        // Compact margin line: anchor, quote, whitespace-collapsed body.
        assert!(
            text.contains("L2 \"fn two()\" -- the second function"),
            "{text}"
        );

        // annotations=true is the declared boolean spelling of the same.
        let same = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:file:a.rs",
                &[("annotations", "true")],
                &cap(),
            )
            .unwrap(),
        );
        assert_eq!(same, text);

        // The default (annotations=false) raw face is untouched.
        let raw = issue(&k, Verb::Source, "urn:repo:demo:file:a.rs", &[], &cap()).unwrap();
        assert_eq!(raw.repr_type.media_type, "text/x-rust");
        assert_eq!(body(&raw), "fn one() {}\nfn two() {}\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn included_margin_notes_carry_drift_flags() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        annotate(&k, "note-1", "a.rs", "fn two()", "watch this");

        // Content shifts: the include pass re-anchors and says so.
        std::fs::write(root.join("a.rs"), "// moved\nfn one() {}\nfn two() {}\n").unwrap();
        let text = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:file:a.rs",
                &[("annotations", "include")],
                &cap(),
            )
            .unwrap(),
        );
        assert!(
            text.contains("L3 [re-anchored] \"fn two()\" -- watch this"),
            "{text}"
        );

        // The quote disappears: flagged orphaned, never dropped.
        std::fs::write(root.join("a.rs"), "// moved\nfn one() {}\n").unwrap();
        let text = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:file:a.rs",
                &[("annotations", "include")],
                &cap(),
            )
            .unwrap(),
        );
        assert!(
            text.contains("[orphaned] \"fn two()\" -- watch this"),
            "{text}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn annotations_include_fails_loud_when_it_cannot_be_honored() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\n").unwrap();
        std::fs::write(root.join("img.png"), [0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF]).unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);

        // Binary content has no text face to fold notes into.
        let err = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:file:img.png",
            &[("annotations", "include")],
            &cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");

        // A value outside the declared enum is a typed argument error.
        let err = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:file:a.rs",
            &[("annotations", "maybe")],
            &cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");

        // A plain space() mounts no store: asking is an error, not a silent
        // no-op (and the arg is not declared there — see the describe test).
        let bare = ikigai_core::Kernel::new(Arc::new(crate::space(vec![(
            "demo".to_string(),
            root.clone(),
        )])));
        let err = issue(
            &bare,
            Verb::Source,
            "urn:repo:demo:file:a.rs",
            &[("annotations", "include")],
            &Capability::scoped(["urn:cap:browse:read:demo"]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn bad_ids_and_bad_targets_are_typed_errors() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);

        // A slug that would collide with the selector sub-IRIs is refused.
        let err = issue(
            &k,
            Verb::Sink,
            "urn:iki:annotation:has%3Acolon",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "fn one()"),
                ("body", "no"),
            ],
            &cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");

        // A tree target is not annotatable; an unconfigured root neither.
        for target in ["urn:repo:demo:tree:src", "urn:repo:nope:file:a.rs"] {
            let err = issue(
                &k,
                Verb::Sink,
                "urn:iki:annotation:x",
                &[("target", target), ("exact", "fn one()"), ("body", "no")],
                &cap(),
            )
            .unwrap_err();
            assert!(
                matches!(err, Error::InvalidArgument { .. }),
                "{target}: {err:?}"
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }

    // --- the `urn:iki:` migration (0.3.0) ------------------------------------

    /// ★ PREFIX SURGERY — the class a binding sweep misses, pinned.
    ///
    /// The namespace rename moved one *binding* (`AnnotationGrammar`'s
    /// template) and three *literal prefix scans* over stored subject IRIs:
    /// [`list_annotations`] (the repo listing), [`list_annotations_for_target`]
    /// (the PR page's overlay) and [`included_for_ids`] (a review pass's minted
    /// set). None of the three is a binding — no `UriTemplate` or `Exact` grep
    /// reaches them and the compiler cannot see inside the string — and all
    /// three fail the SAME quiet way if one is left behind: `strip_prefix`
    /// returns `None`, the loop `continue`s, and the caller gets an EMPTY
    /// result rather than an error.
    ///
    /// So every assertion below is a non-emptiness: the shape a missed rename
    /// produces is a zero, not a panic, which is why the minting/read/delete
    /// round trip on its own would have passed over all three.
    #[test]
    fn every_stored_iri_scan_reads_the_migrated_namespace() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);

        // Minted through the bare IRI, so the id comes from the endpoint and
        // the new prefix is the one the store actually receives.
        let ack = issue(
            &k,
            Verb::Sink,
            "urn:iki:annotation",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "fn one()"),
                ("body", "migrated"),
            ],
            &cap(),
        )
        .unwrap();
        let iri = body(&ack);
        assert!(iri.starts_with("urn:iki:annotation:"), "{iri}");
        assert_eq!(
            body(&issue(&k, Verb::Source, &iri, &[], &cap()).unwrap()),
            "migrated"
        );

        // Site 1 — `list_annotations`, reached through the repo listing row.
        let listed =
            body(&issue(&k, Verb::Source, "urn:repo:demo:annotations", &[], &cap()).unwrap());
        assert!(
            listed.contains("migrated"),
            "the repo listing went empty: {listed}"
        );

        // Site 2 — `list_annotations_for_target`, the PR/target overlay's scan.
        let archive = Archive::new(Arc::clone(&store), GraphName::DefaultGraph);
        let rows = list_annotations_for_target(&archive, "urn:repo:demo:file:a.rs").unwrap();
        assert_eq!(rows.len(), 1, "the target scan went empty");
        assert_eq!(rows[0].iri(), iri);

        // Site 3 — `included_for_ids`, a review pass's minted set by IRI.
        let text = std::fs::read_to_string(root.join("a.rs")).unwrap();
        let included = included_for_ids(&archive, std::slice::from_ref(&iri), &text).unwrap();
        assert_eq!(included.rows.len(), 1, "the minted-set scan went empty");

        // The selector sub-IRIs are minted under the new prefix too, so a
        // Turtle consumer sees one namespace and not two.
        let ttl = body(&issue(&k, Verb::Source, &iri, &[("as", "text/turtle")], &cap()).unwrap());
        assert!(
            !ttl.contains("urn:annotation:"),
            "an old-name IRI leaked: {ttl}"
        );
        assert!(ttl.contains(":selector:quote"), "{ttl}");

        // Delete takes it back out of every scan.
        issue(&k, Verb::Delete, &iri, &[], &cap()).unwrap();
        assert!(
            list_annotations_for_target(&archive, "urn:repo:demo:file:a.rs")
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
