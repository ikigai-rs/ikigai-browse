//! `urn:annotation:{id}` + `urn:repo:{repo}:annotations[:{path}]` — W3C **Web
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
//! <urn:annotation:{id}> a oa:Annotation ;
//!     oa:bodyValue "the note text" ;
//!     ik:annotates <urn:repo:demo:file:src/lib.rs> ;
//!     ik:repo "demo" ; ik:path "src/lib.rs" ;
//!     ik:contentHash "sha256:…" ;              # the file version annotated
//!     oa:hasSelector <urn:annotation:{id}:selector:quote> ,
//!                    <urn:annotation:{id}:selector:position> ;
//!     dcterms:created "2026-08-08T17:00:00.000Z"^^xsd:dateTime .
//!
//! <urn:annotation:{id}:selector:quote> a oa:TextQuoteSelector ;
//!     oa:prefix "…" ; oa:exact "the quoted text" ; oa:suffix "…" .
//!
//! <urn:annotation:{id}:selector:position> a oa:TextPositionSelector ;
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
//! `Sink urn:annotation:{id}` creates (or updates) under a CALLER-SUPPLIED
//! slug (`[A-Za-z0-9._~-]+`); `Sink urn:annotation` (no id) MINTS a v4 uuid
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
use oxigraph::model::{GraphName, Literal, NamedNode, Quad, Term};
use oxigraph::store::Store;
use sha2::{Digest, Sha256};

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

/// The `oa:motivatedBy` value the human Sink stamps.
const MOTIVATION_HUMAN: &str = "commenting";
/// The `oa:motivatedBy` value the review pass stamps.
const MOTIVATION_REVIEW: &str = "assessing";

/// How much context the stored quote selector carries on each side of the
/// exact quote (characters). Part of the re-anchoring contract.
const CONTEXT_CHARS: usize = 32;

fn oa(term: &str) -> NamedNode {
    NamedNode::new(format!("{OA}{term}")).expect("oa terms are valid IRIs")
}

// --- IRIs -------------------------------------------------------------------

pub(crate) fn annotation_iri(id: &str) -> String {
    format!("urn:annotation:{id}")
}

fn quote_iri(id: &str) -> String {
    format!("urn:annotation:{id}:selector:quote")
}

fn position_iri(id: &str) -> String {
    format!("urn:annotation:{id}:selector:position")
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

/// The annotated browse resource: `urn:repo:{repo}:file:{path}` where `{repo}`
/// is a configured root. Only file targets are annotatable in v1 (the line
/// anchors and quote selectors are text-content concepts).
fn parse_target(
    target: &str,
    roots: &BTreeMap<String, std::path::PathBuf>,
) -> Result<(String, String)> {
    let bad = |detail: String| Error::InvalidArgument {
        name: "target".to_string(),
        detail,
    };
    let rest = target.strip_prefix("urn:repo:").ok_or_else(|| {
        bad(format!(
            "`{target}` is not a urn:repo:{{repo}}:file:{{path}} IRI"
        ))
    })?;
    let (repo, rest) = rest
        .split_once(':')
        .ok_or_else(|| bad(format!("`{target}` carries no path")))?;
    let rel_encoded = rest.strip_prefix("file:").ok_or_else(|| {
        bad("only file resources are annotatable (urn:repo:{repo}:file:{path})".to_string())
    })?;
    if !roots.contains_key(repo) {
        return Err(bad(format!("`{repo}` is not a configured root")));
    }
    let rel = iri_decode(rel_encoded)?;
    if rel.is_empty() {
        return Err(bad(format!("`{target}` carries no path")));
    }
    Ok((repo.to_string(), rel))
}

// --- the annotation record --------------------------------------------------

/// One annotation, as stored (and as served). `start`/`end` are character
/// offsets into the target's text at `hash`.
#[derive(Clone, Debug)]
struct Annotation {
    id: String,
    body: String,
    target_iri: String,
    repo: String,
    rel: String,
    hash: String,
    prefix: String,
    exact: String,
    suffix: String,
    start: u64,
    end: u64,
    created: Option<String>,
    reanchored: bool,
    orphaned: bool,
    /// `dcterms:creator` — the model identity on machine-minted annotations.
    /// `Some` IS the machine/human discriminator: human annotations never
    /// carry a creator (v1 is single-user; the passkey→workspace arc will add
    /// human authorship on a different axis).
    creator: Option<String>,
    /// `oa:motivatedBy` (the short term: `commenting` / `assessing`). Absent
    /// on pre-S4 stores — read compatibility, never a discriminator.
    motivation: Option<String>,
    /// `prov:wasGeneratedBy` — the review pass entry that minted this
    /// annotation (machine annotations only).
    generated_by: Option<String>,
}

impl Annotation {
    fn iri(&self) -> String {
        annotation_iri(&self.id)
    }

    fn machine(&self) -> bool {
        self.creator.is_some()
    }
}

fn store_err(e: impl std::fmt::Display) -> Error {
    Error::Endpoint(format!("browse: annotation store: {e}"))
}

/// Insert the annotation's quads (the annotation node plus both selector
/// nodes). Flags are stored only when true — absence means false.
fn store_annotation(store: &Store, ann: &Annotation) -> Result<()> {
    use oxigraph::model::vocab::{rdf, xsd};
    let subject = NamedNode::new(ann.iri()).map_err(store_err)?;
    let quote = NamedNode::new(quote_iri(&ann.id)).map_err(store_err)?;
    let position = NamedNode::new(position_iri(&ann.id)).map_err(store_err)?;
    let target = NamedNode::new(&ann.target_iri).map_err(store_err)?;
    let g = GraphName::DefaultGraph;
    let mut quads: Vec<Quad> = vec![
        Quad::new(subject.clone(), rdf::TYPE, oa("Annotation"), g.clone()),
        Quad::new(
            subject.clone(),
            oa("bodyValue"),
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
            ik("path"),
            Literal::new_simple_literal(&ann.rel),
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
    if let Some(motivation) = &ann.motivation {
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
            subject,
            NamedNode::new(PROV_WAS_GENERATED_BY).map_err(store_err)?,
            NamedNode::new(pass).map_err(store_err)?,
            g,
        ));
    }
    for quad in &quads {
        store.insert(quad).map_err(store_err)?;
    }
    Ok(())
}

/// Remove every quad under the annotation's three subjects.
fn remove_annotation(store: &Store, id: &str) -> Result<()> {
    for iri in [annotation_iri(id), quote_iri(id), position_iri(id)] {
        let subject = NamedNode::new(&iri).map_err(store_err)?;
        let quads: Vec<Quad> = store
            .quads_for_pattern(Some(subject.as_ref().into()), None, None, None)
            .collect::<std::result::Result<_, _>>()
            .map_err(store_err)?;
        for quad in &quads {
            store.remove(quad).map_err(store_err)?;
        }
    }
    Ok(())
}

/// Replace the annotation's stored state (the update path and the
/// re-anchor/orphan persistence path).
fn rewrite_annotation(store: &Store, ann: &Annotation) -> Result<()> {
    remove_annotation(store, &ann.id)?;
    store_annotation(store, ann)
}

/// Load one annotation by id — `None` when the store holds no `oa:bodyValue`
/// for it.
fn load_annotation(store: &Store, id: &str) -> Result<Option<Annotation>> {
    let mut ann = Annotation {
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
    };
    let literal = |term: &Term| match term {
        Term::Literal(l) => l.value().to_string(),
        other => other.to_string(),
    };
    let mut found = false;
    let subject = match NamedNode::new(annotation_iri(id)) {
        Ok(node) => node,
        Err(_) => return Ok(None),
    };
    for quad in store.quads_for_pattern(Some(subject.as_ref().into()), None, None, None) {
        let quad = quad.map_err(store_err)?;
        let predicate = quad.predicate.as_str();
        if let Some(term) = predicate.strip_prefix(OA) {
            match term {
                "bodyValue" => {
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
    for (iri, is_quote) in [(quote_iri(id), true), (position_iri(id), false)] {
        let subject = NamedNode::new(&iri).map_err(store_err)?;
        for quad in store.quads_for_pattern(Some(subject.as_ref().into()), None, None, None) {
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

/// Every annotation in the store for one repo — optionally narrowed to one
/// path. Filters by `rdf:type oa:Annotation` (the shared store also holds
/// `ik:Explanation` entries with `ik:repo`/`ik:about` triples — type is the
/// discriminator). Sorted by (path, start, id) for a stable reading order.
fn list_annotations(store: &Store, repo: &str, rel: Option<&str>) -> Result<Vec<Annotation>> {
    use oxigraph::model::vocab::rdf;
    let mut out = Vec::new();
    for quad in store.quads_for_pattern(
        None,
        Some(rdf::TYPE),
        Some(oa("Annotation").as_ref().into()),
        None,
    ) {
        let quad = quad.map_err(store_err)?;
        let subject = quad.subject.to_string();
        let iri = subject.trim_start_matches('<').trim_end_matches('>');
        let Some(id) = iri.strip_prefix("urn:annotation:") else {
            continue;
        };
        let Some(ann) = load_annotation(store, id)? else {
            continue;
        };
        if ann.repo == repo && rel.is_none_or(|rel| ann.rel == rel) {
            out.push(ann);
        }
    }
    out.sort_by(|a, b| (&a.rel, a.start, &a.id).cmp(&(&b.rel, b.start, &b.id)));
    Ok(out)
}

// --- anchoring --------------------------------------------------------------

/// Where a quote sits in a text: byte and character coordinates plus the
/// 1-based line its first character is on (the `#L{n}` anchor).
struct Anchor {
    byte_start: usize,
    byte_end: usize,
    char_start: u64,
    char_end: u64,
    line: u64,
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

/// The stored context around an anchored quote: up to [`CONTEXT_CHARS`]
/// characters each side. Always derived from the anchored occurrence (never
/// the caller's raw arguments) so re-anchoring matches real neighbors.
fn context_around(content: &str, anchor: &Anchor) -> (String, String) {
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
fn line_of(content: &str, char_offset: u64) -> u64 {
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
enum CurrentContent {
    /// UTF-8 text plus its `sha256:{hex}` content hash.
    Text(String, String),
    /// The file is gone or binary — nothing to anchor against.
    Unavailable,
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// The drift pass, run on every read (Source, list, and the file face's
/// panel): reconcile one annotation against the target's current content.
/// Returns the line its anchor renders at (`None` when no content is in
/// hand). Persists to the store ONLY when something changed — repeat reads of
/// an unchanged (or already-orphaned) annotation touch nothing.
fn refresh(store: &Store, ann: &mut Annotation, current: &CurrentContent) -> Result<Option<u64>> {
    match current {
        CurrentContent::Unavailable => {
            if !ann.orphaned {
                ann.orphaned = true;
                rewrite_annotation(store, ann)?;
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
                    rewrite_annotation(store, ann)?;
                }
                return Ok(Some(line_of(text, ann.start)));
            }
            match find_anchor(text, &ann.exact, &ann.prefix, &ann.suffix) {
                Some(anchor) => {
                    let (prefix, suffix) = context_around(text, &anchor);
                    ann.prefix = prefix;
                    ann.suffix = suffix;
                    ann.start = anchor.char_start;
                    ann.end = anchor.char_end;
                    ann.hash = hash.clone();
                    ann.reanchored = true;
                    ann.orphaned = false;
                    rewrite_annotation(store, ann)?;
                    Ok(Some(anchor.line))
                }
                None => {
                    if !ann.orphaned {
                        ann.orphaned = true;
                        rewrite_annotation(store, ann)?;
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
    store: &Store,
    repo: &str,
    mut anns: Vec<Annotation>,
) -> Result<Vec<(Annotation, Option<u64>)>> {
    let mut contents: BTreeMap<String, CurrentContent> = BTreeMap::new();
    for ann in &anns {
        if !contents.contains_key(&ann.rel) {
            let current = current_content(inv, repo, &ann.rel).await?;
            contents.insert(ann.rel.clone(), current);
        }
    }
    let mut rows: Vec<(Annotation, Option<u64>)> = Vec::with_capacity(anns.len());
    for mut ann in anns.drain(..) {
        let current = contents.get(&ann.rel).expect("fetched above");
        let line = refresh(store, &mut ann, current)?;
        rows.push((ann, line));
    }
    // Re-anchoring may have moved positions — restore reading order.
    rows.sort_by(|(a, _), (b, _)| (&a.rel, a.start, &a.id).cmp(&(&b.rel, b.start, &b.id)));
    Ok(rows)
}

/// The drift pass against content already in hand (the file face's path — no
/// kernel fetch), rows in reading order.
fn reconcile_against_text(
    store: &Store,
    repo: &str,
    rel: &str,
    text: &str,
) -> Result<Vec<(Annotation, Option<u64>)>> {
    let mut anns = list_annotations(store, repo, Some(rel))?;
    let current = CurrentContent::Text(text.to_string(), hash_bytes(text.as_bytes()));
    let mut rows: Vec<(Annotation, Option<u64>)> = Vec::with_capacity(anns.len());
    for mut ann in anns.drain(..) {
        let line = refresh(store, &mut ann, &current)?;
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
                out.push_str(&ann.rel);
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
    store: &Store,
    repo: &str,
    filter: TargetFilter<'_>,
) -> Result<Included> {
    let anns = match filter {
        TargetFilter::File(rel) => list_annotations(store, repo, Some(rel))?,
        TargetFilter::Subtree(rel) => {
            let all = list_annotations(store, repo, None)?;
            if rel.is_empty() {
                all
            } else {
                let prefix = format!("{rel}/");
                all.into_iter()
                    .filter(|ann| ann.rel.starts_with(&prefix))
                    .collect()
            }
        }
    };
    let rows = reconcile(inv, store, repo, anns).await?;
    Ok(Included {
        rows,
        with_paths: matches!(filter, TargetFilter::Subtree(_)),
    })
}

/// The `annotations=include` payload for a file face whose content is already
/// in hand — the drift pass runs against the very text being served.
pub(crate) fn included_for_text(
    store: &Store,
    repo: &str,
    rel: &str,
    text: &str,
) -> Result<Included> {
    Ok(Included {
        rows: reconcile_against_text(store, repo, rel, text)?,
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
                let hash = hash_bytes(&repr.bytes);
                CurrentContent::Text(text, hash)
            }
            Err(_) => CurrentContent::Unavailable,
        }),
        Err(Error::NotFound(_)) => Ok(CurrentContent::Unavailable),
        Err(e) => Err(e),
    }
}

// --- binding ----------------------------------------------------------------

pub(crate) fn bind(space: EndpointSpace, roots: &Roots, store: &Arc<Store>) -> EndpointSpace {
    let space = space.bind(
        AnnotationGrammar::new(),
        AnnotationEndpoint {
            roots: Arc::clone(roots),
            store: Arc::clone(store),
        },
    );
    let listing: Arc<dyn Endpoint> = Arc::new(AnnotationsEndpoint {
        roots: Arc::clone(roots),
        store: Arc::clone(store),
    });
    crate::bind_family(
        space,
        roots,
        listing,
        Some("annotations"),
        Some("annotations:{path}"),
    )
}

/// `urn:annotation:{id}`, plus the bare `urn:annotation` (Sink mints an id).
struct AnnotationGrammar {
    with_id: UriTemplate,
}

impl AnnotationGrammar {
    fn new() -> Self {
        AnnotationGrammar {
            with_id: UriTemplate::parse("urn:annotation:{id}")
                .expect("the annotation template is valid"),
        }
    }
}

impl Grammar for AnnotationGrammar {
    fn match_iri(&self, iri: &Iri) -> Option<Bindings> {
        if iri.as_str() == "urn:annotation" {
            return Some(Bindings::new());
        }
        self.with_id.match_iri(iri)
    }

    fn pattern(&self) -> String {
        // The advertised row is the template — a real pattern a probe can
        // expand and every verb can drive (Sink's `id` is optional there, and
        // the description documents the minting form). The bare
        // `urn:annotation` stays resolvable but UNLISTED: as a row of its own
        // it would offer Source/Delete actions that cannot succeed without an
        // id, and `[:{id}]` display sugar is not a template any grammar
        // matches (it kept every annotation row out of every manifold).
        "urn:annotation:{id}".to_string()
    }
}

// --- the annotation endpoint (multi-verb) -----------------------------------

struct AnnotationEndpoint {
    roots: Roots,
    store: Arc<Store>,
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
        load_annotation(&self.store, id)?.ok_or_else(|| {
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
        let current = current_content(inv, &ann.repo, &ann.rel).await?;
        let line = refresh(&self.store, &mut ann, &current)?;
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
        let (repo, rel) = parse_target(&target, &self.roots)?;
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

        let CurrentContent::Text(text, hash) = current_content(inv, &repo, &rel).await? else {
            return Err(Error::InvalidArgument {
                name: "target".to_string(),
                detail: format!("`{target}` is not annotatable text (missing or binary)"),
            });
        };
        let anchor = find_anchor(&text, &exact, hint_prefix, hint_suffix).ok_or_else(|| {
            Error::InvalidArgument {
                name: "exact".to_string(),
                detail: format!("the quote was not found in `{target}`"),
            }
        })?;
        let (prefix, suffix) = context_around(&text, &anchor);

        // An update keeps its original creation instant.
        let created = match load_annotation(&self.store, &id)? {
            Some(existing) if existing.created.is_some() => existing.created,
            _ => inv.now().map(|t| iso8601(t.as_millis())),
        };
        let ann = Annotation {
            id: id.clone(),
            body,
            target_iri: file_iri(&repo, &rel),
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
        };
        rewrite_annotation(&self.store, &ann)?;
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
        remove_annotation(&self.store, &id)?;
        Ok(repr_utf8("text/plain", format!("deleted {}", ann.iri())))
    }
}

fn annotation_description() -> Description {
    const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
    Description::new("annotation")
        .title("Web Annotation (oa:) on a browse resource")
        .summary(
            "A W3C Web Annotation on a repository file — urn:annotation:{id}, stored as \
             skolemized RDF in the same shared store as the explanation archive. Sink \
             creates or updates (anchoring the quoted text in the target's current \
             content; sink the bare urn:annotation to mint a uuid id); Source reads it \
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
                        .summary("the annotation id (a slug or minted uuid)"),
                )
                .input(
                    ArgSpec::new("as")
                        .optional()
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
                .input(ArgSpec::new("id").binding().optional().summary(
                    "caller-supplied slug ([A-Za-z0-9._~-]+); sink the bare urn:annotation to \
                     mint a uuid",
                ))
                .input(
                    ArgSpec::new("target")
                        .class("https://ikigai-rs.dev/ns#File")
                        .summary("the annotated resource — urn:repo:{repo}:file:{path}"),
                )
                .input(ArgSpec::new("body").class(XSD_STRING).optional().summary(
                    "the note text (oa:bodyValue); falls back to piped content — one of the \
                     two must be present",
                ))
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
                        .summary("application/json for the structured acknowledgement")
                        .one_of(["application/json"])
                        .default_value("text/plain"),
                )
                .output("text/plain;charset=utf-8")
                .output("application/json"),
        )
        .action(
            ActionSpec::new(Verb::Delete)
                .summary("remove an annotation and its selectors from the store")
                .requires(CAP_ANNOTATE)
                .input(ArgSpec::new("id").binding().summary("the annotation id"))
                .output("text/plain;charset=utf-8"),
        )
}

// --- the listing endpoint ---------------------------------------------------

/// `urn:repo:{repo}:annotations[:{path}]` — every annotation on one file, or
/// (path omitted) on the whole repo. Runs the same drift pass as Source, one
/// content fetch per distinct target.
struct AnnotationsEndpoint {
    roots: Roots,
    store: Arc<Store>,
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
        let anns = list_annotations(&self.store, repo, filter)?;
        // One content fetch per distinct target, then the drift pass each.
        let rows = reconcile(inv, &self.store, repo, anns).await?;

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
                .summary("file path within the root, percent-encoded (omitted = the whole repo)"),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("the face to render")
                .one_of(["application/json", "text/html", "text/turtle"])
                .default_value("application/json"),
        )
        .output("application/json")
        .output("text/html;charset=utf-8")
        .output("text/turtle")
}

// --- faces ------------------------------------------------------------------

fn annotation_json(ann: &Annotation, line: Option<u64>) -> serde_json::Value {
    serde_json::json!({
        "id": ann.id,
        "iri": ann.iri(),
        "annotates": ann.target_iri,
        "repo": ann.repo,
        "path": ann.rel,
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
    })
}

/// One annotation's graph as Turtle (the same skolemized shape the store
/// holds).
fn annotation_turtle(ann: &Annotation) -> String {
    let mut props = vec![
        "a oa:Annotation".to_string(),
        format!("oa:bodyValue {}", ttl_str(&ann.body)),
        format!("ik:annotates <{}>", ann.target_iri),
        format!("ik:repo {}", ttl_str(&ann.repo)),
        format!("ik:path {}", ttl_str(&ann.rel)),
        format!("ik:contentHash {}", ttl_str(&ann.hash)),
        format!(
            "oa:hasSelector <{}>, <{}>",
            quote_iri(&ann.id),
            position_iri(&ann.id)
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
    if let Some(motivation) = &ann.motivation {
        props.push(format!("oa:motivatedBy oa:{motivation}"));
    }
    if let Some(creator) = &ann.creator {
        props.push(format!("dcterms:creator {}", ttl_str(creator)));
    }
    if let Some(pass) = &ann.generated_by {
        props.push(format!("prov:wasGeneratedBy <{pass}>"));
    }
    let mut out = format!("<{}> {} .\n", ann.iri(), props.join(" ;\n    "));

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
        quote_iri(&ann.id),
        quote.join(" ;\n    ")
    ));
    out.push_str(&format!(
        "\n<{}> a oa:TextPositionSelector ;\n    oa:start \"{}\"^^xsd:nonNegativeInteger ;\n    \
         oa:end \"{}\"^^xsd:nonNegativeInteger .\n",
        position_iri(&ann.id),
        ann.start,
        ann.end
    ));
    out
}

fn annotation_turtle_document(anns: &[Annotation]) -> String {
    let mut out = format!(
        "@prefix oa: <{OA}> .\n@prefix ik: <{IK}> .\n@prefix dcterms: <http://purl.org/dc/terms/> \
         .\n@prefix prov: <{PROV}> .\n@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n"
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
fn annotation_card_html(ann: &Annotation, line: Option<u64>, show_path: bool) -> String {
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
            esc(&ann.rel)
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
    format!(
        "<div class=\"browse-annotation{orphan_class}{machine_class}\" \
         id=\"annotation-{id}\">{path}{anchor}{model}\
         <blockquote class=\"browse-annotation-quote\">{exact}</blockquote>\
         <p class=\"browse-annotation-body\">{body}</p>{flags}</div>",
        id = esc(&ann.id),
        exact = esc(&ann.exact),
        body = esc(&ann.body),
    )
}

/// The create affordance: a server-rendered form the HOST's `/k/` adapter
/// turns into a Sink of `urn:annotation` (form fields become sink args — the
/// same adapter assumption the S0 faces document for `hx-get`). htmx
/// attributes only; no scripts.
fn annotation_form_html(target_iri: &str) -> String {
    format!(
        "<form class=\"browse-annotate\" hx-post=\"/k/sink urn:annotation\" \
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
    store: &Store,
    repo: &str,
    rel: &str,
    text: &str,
) -> Result<(BTreeMap<u64, Vec<Marker>>, String)> {
    let rows = reconcile_against_text(store, repo, rel, text)?;
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

// --- the review layer's mint (S4) -------------------------------------------

/// Mint one MACHINE annotation — the review pass's finding, stored through the
/// same machinery as a human note and distinguished only by provenance:
/// `dcterms:creator` (the model identity), `oa:motivatedBy oa:assessing`, and
/// `prov:wasGeneratedBy` (the pass entry). Anchors `exact` in `text` (already
/// in hand — the pass sourced it) with no context hints: the model was told to
/// pick distinctive quotes, and the deterministic first-occurrence rule covers
/// the rest. Returns the minted IRI, or `None` when the quote does not anchor
/// — the caller counts it and moves on (one bad item must not kill the pass).
#[allow(clippy::too_many_arguments)]
pub(crate) fn mint_review_annotation(
    store: &Store,
    repo: &str,
    rel: &str,
    text: &str,
    hash: &str,
    exact: &str,
    note: &str,
    model: &str,
    pass_iri: &str,
    created: Option<String>,
) -> Result<Option<String>> {
    let Some(anchor) = find_anchor(text, exact, "", "") else {
        return Ok(None);
    };
    let (prefix, suffix) = context_around(text, &anchor);
    let ann = Annotation {
        id: uuid::Uuid::new_v4().to_string(),
        body: note.to_string(),
        target_iri: file_iri(repo, rel),
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
        motivation: Some(MOTIVATION_REVIEW.to_string()),
        generated_by: Some(pass_iri.to_string()),
    };
    store_annotation(store, &ann)?;
    Ok(Some(ann.iri()))
}

/// The named annotations (a review pass's minted set), drift-reconciled
/// against content already in hand — the review faces' rows. Ids that no
/// longer load (someone deleted the annotation) are skipped: the pass entry
/// records history, the store records the present. Same [`Included`] the
/// `annotations=include` folds serve, so the faces are shared.
pub(crate) fn included_for_ids(store: &Store, iris: &[String], text: &str) -> Result<Included> {
    let current = CurrentContent::Text(text.to_string(), hash_bytes(text.as_bytes()));
    let mut rows: Vec<(Annotation, Option<u64>)> = Vec::with_capacity(iris.len());
    for iri in iris {
        let Some(id) = iri.strip_prefix("urn:annotation:") else {
            continue;
        };
        let Some(mut ann) = load_annotation(store, id)? else {
            continue;
        };
        let line = refresh(store, &mut ann, &current)?;
        rows.push((ann, line));
    }
    rows.sort_by(|(a, _), (b, _)| (a.start, &a.id).cmp(&(b.start, &b.id)));
    Ok(Included {
        rows,
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
            &format!("urn:annotation:{id}"),
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
        assert_eq!(created["iri"], "urn:annotation:note-1");
        assert_eq!(created["annotates"], "urn:repo:demo:file:a.rs");
        assert_eq!(created["line"], 2);
        assert_eq!(created["start"], 12);
        assert_eq!(created["end"], 20);
        assert!(created["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));

        // Source: text/plain is the body; json carries the whole record.
        let plain = issue(&k, Verb::Source, "urn:annotation:note-1", &[], &cap()).unwrap();
        assert_eq!(body(&plain), "the middle function");
        let full = json_of(
            &issue(
                &k,
                Verb::Source,
                "urn:annotation:note-1",
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
        let ack = issue(&k, Verb::Delete, "urn:annotation:note-1", &[], &cap()).unwrap();
        assert_eq!(body(&ack), "deleted urn:annotation:note-1");
        let err = issue(&k, Verb::Source, "urn:annotation:note-1", &[], &cap()).unwrap_err();
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
            "urn:annotation",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "fn one()"),
                ("body", "minted"),
            ],
            &cap(),
        )
        .unwrap();
        let iri = body(&ack);
        let id = iri.strip_prefix("urn:annotation:").unwrap();
        assert_eq!(id.len(), 36, "a v4 uuid: {iri}");
        // The minted IRI resolves.
        let read = issue(&k, Verb::Source, &iri, &[], &cap()).unwrap();
        assert_eq!(body(&read), "minted");

        // Source/Delete on the bare IRI have no id to work with.
        let err = issue(&k, Verb::Source, "urn:annotation", &[], &cap()).unwrap_err();
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
            "urn:annotation:piped",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "fn one()"),
                ("content", "the piped note"),
            ],
            &cap(),
        )
        .unwrap();
        let read = issue(&k, Verb::Source, "urn:annotation:piped", &[], &cap()).unwrap();
        assert_eq!(body(&read), "the piped note");

        // Neither body nor content: a typed missing-argument error.
        let err = issue(
            &k,
            Verb::Sink,
            "urn:annotation:empty",
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
            "urn:annotation:x",
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
        let err = issue(&k, Verb::Source, "urn:annotation:n", &[], &annotate_only).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");

        // Source with a browse grant on the WRONG root: past the baseline
        // wildcard, denied by the per-root check on the annotation's repo.
        let wrong_root = Capability::scoped(["urn:cap:browse:read:other"]);
        let err = issue(&k, Verb::Source, "urn:annotation:n", &[], &wrong_root).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");

        // Delete without annotate: denied.
        let err = issue(&k, Verb::Delete, "urn:annotation:n", &[], &browse_only).unwrap_err();
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
                "urn:annotation:n",
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
        let stored = load_annotation(&store, "n").unwrap().unwrap();
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
                "urn:annotation:n",
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

        issue(&k, Verb::Source, "urn:annotation:n", &[], &cap()).unwrap();
        issue(&k, Verb::Source, "urn:annotation:n", &[], &cap()).unwrap();
        issue(
            &k,
            Verb::Source,
            "urn:repo:demo:annotations:a.rs",
            &[],
            &cap(),
        )
        .unwrap();

        // Exactly ONE orphaned triple, no duplicates from the repeat reads.
        let subject = NamedNode::new("urn:annotation:n").unwrap();
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
                "urn:annotation:n",
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
                "urn:annotation:n",
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
            "urn:annotation:ctx",
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
    fn a_missing_quote_is_a_typed_argument_error() {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), "fn one() {}\n").unwrap();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let err = issue(
            &k,
            Verb::Sink,
            "urn:annotation:x",
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

        for iri in ["urn:annotation:n1", "urn:repo:demo:annotations:a.rs"] {
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
                ttl.contains("<urn:annotation:n1:selector:position> a oa:TextPositionSelector"),
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
                "urn:annotation:old",
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
            html.contains("hx-post=\"/k/sink urn:annotation\""),
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
            "urn:annotation:n",
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
        let annotated = issue(&k, Verb::Source, "urn:annotation:n", &[], &full).unwrap();
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
            ["id", "target", "body", "exact", "prefix", "suffix", "as"]
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
            "urn:annotation:has%3Acolon",
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
                "urn:annotation:x",
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
}
