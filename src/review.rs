//! `urn:repo:{repo}:review:{path}` — the **machine review layer** (S4):
//! region-grain LLM commentary on a file, minted as REAL annotations. The
//! explain family answers "what is this file?"; the review pass answers "what
//! would a careful reviewer say about these lines?" — and its findings live in
//! the same `urn:iki:annotation:` family as human notes, distinguished by
//! provenance, queryable on one axis.
//!
//! ## Derive-once, mint-once
//!
//! Source asks the review model for findings — each an EXACT quote from the
//! file plus a note — anchors every quote, mints each anchored finding as an
//! annotation via the S2 machinery, and ARCHIVES the pass keyed
//! `(path, content-hash, review-v{N}@model)` with the minted IRIs recorded in
//! the pass entry. Re-sourcing unchanged content is an archive hit that mints
//! NOTHING: idempotency comes from the key, and the recorded IRIs are what the
//! hit serves. Changed content is a fresh pass; the previous pass's
//! annotations re-anchor or orphan exactly like human ones — that drift IS the
//! review-history story.
//!
//! ## Choosing the backend per request
//!
//! `provider={iri}` derives THIS pass against a backend the caller names
//! rather than the configured review tier, on the explain family's terms (see
//! that module's header): the selectable set is the operator's — every
//! configured tier plus [`crate::ExplainConfig::allow_provider`] — and
//! anything else is `Denied` before any work, never a silent fall back.
//!
//! ★ IT IS A SECOND PASS, NOT A REPLACEMENT. The archive key folds the model
//! identity, so a second backend serving a different model derives and mints
//! its OWN pass over the same content, alongside the first: two reviewers'
//! margins on one file, both queryable on the annotation axis. Two backends
//! serving the SAME model share the key, so the second is an archive hit that
//! asks nothing and mints nothing — the same rule the explain menu is built
//! on, and the reason the operator's `review_model_label` applies only while
//! `provider` IS the configured one (a label written for one model must never
//! key another model's pass).
//!
//! ## The affordance: a CALLER, never a second implementation
//!
//! The file HTML face carries a `review` button and a "review with…" menu
//! ([`review_button_html`], [`menu_html`]), whose rows come from
//! `urn:repo:{repo}:review-options:{path}` — this host's own `provider=`
//! allowlist, grouped by model, never a hard-coded list.
//!
//! ★ BOTH EMIT EXACTLY `urn:repo:{repo}:review:{path}` (the menu adding
//! `provider=`), which is precisely the call a git-event trigger makes: same
//! resource, same arguments, same capability check, same archive key, same
//! minted annotations. The markup chooses the FACE and nothing else. Nothing
//! here assembles a prompt, post-processes a finding, or writes an annotation
//! by another path — a UI that did any of those would produce results a
//! trigger could never reproduce, and the two would then diverge silently.
//! The only legitimate difference between a clicked review and a triggered one
//! is what caused it.
//!
//! ⚠ The reverse reading is the useful one: whatever the button needs, a
//! headless trigger needs too, WITHOUT a human present — the net grant, the
//! annotate grant, the browse read, and an answer to what bounds the spend.
//!
//! ## Manual review is the human annotation affordance, unchanged
//!
//! There is no second path for a human note. The file face's annotations panel
//! renders machine findings and human notes in one reading order, machine ones
//! prefixed by their model, and ends with the create form that Sinks
//! `urn:iki:annotation` — so a reviewer answers a finding beside it rather
//! than in another view, over the resource that already existed.
//!
//! ## Provenance — standard terms only, no vocab publish
//!
//! A machine annotation carries `dcterms:creator` (the model identity),
//! `oa:motivatedBy oa:assessing` (the human Sink stamps `oa:commenting`), and
//! `prov:wasGeneratedBy` pointing at the pass entry; the pass entry records
//! its minted set as `prov:generated` and the reviewed file as `prov:used`.
//! Faces render the two kinds distinguishably: hollow line markers and a
//! model-identity line for machine cards, `machine`/`creator`/`motivation` in
//! the JSON rows.
//!
//! ## Failure containment
//!
//! A finding whose quote does not anchor (the model misquoted) mints nothing
//! and is COUNTED (`orphaned_items` in the entry and the json face) — one bad
//! item must not kill the pass. But a pass in which NOTHING parses or NOTHING
//! anchors is an error and is not archived: silently serving an empty review
//! under a key that will never re-derive would poison the archive.
//!
//! ## Capabilities
//!
//! The pass reads (browse), calls a model (net), and WRITES annotations —
//! `requires` all three (`urn:cap:browse:read:*`, `urn:cap:net:*`,
//! `urn:cap:annotate`); declared = enforced by the kernel baseline, and the
//! per-root grant check covers the target.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Invocation, Representation,
    Request, Result, Verb,
};
use oxigraph::model::{Literal, NamedNode, Quad, Term};

use crate::annotate::{self, Included, PROV};
use crate::archive::Archive;
use crate::explain::{
    command_safe, ik, iso8601, menu_options, parse_iri, provider_label, resolve_model, truncate,
    truncated_len, MenuTier, ModelOption, CAP_NET, IK,
};
use crate::hash::hash_iri;
use crate::{
    crumbs_html, esc, file_iri, granted, iri_encode, path_binding, repo_root, repr, repr_utf8,
    resolve, ttl_str, ExplainConfig, Roots, CAP_WILDCARD,
};

// --- the prompt (versioned; edit ⇒ bump) -------------------------------------

/// Version of the review prompt pair, folded into the archive key. A prompt
/// edit bumps this; earlier passes stay recorded under their old tag.
/// v2: the format contract is RESTATED after the content — with a large
/// input, a contract stated only up top loses to the content and the model
/// answers label-free (the pr-review-v2 live failure: quote-and-commentary
/// prose, zero `QUOTE:`/`NOTE:` lines, nothing parseable).
/// v3: every finding now carries a `SEVERITY:` line, constrained to
/// [`crate::finding::SEVERITIES`] — and the findings are PENDING, so a pass's
/// output is no longer what a v2 pass's output was. The tag change is what
/// keeps a v2 archive entry (whose `prov:generated` names annotations) readable
/// beside a v3 one (whose `prov:generated` names findings) instead of
/// colliding on one key.
const REVIEW_PROMPT_VERSION: &str = "review-v3";

/// The reviewer persona. The centerpiece constraint: commentary a thoughtful
/// colleague would leave — intent, tradeoffs, risks, and earned praise — not
/// mechanical lint.
const REVIEW_SYSTEM_PROMPT: &str =
    "You are an experienced engineer reviewing a colleague's file. You write \
     the kind of margin notes a thoughtful human reviewer leaves: you name the \
     design's intent and its tradeoffs, point at subtle risks and edge cases, \
     question misleading names or comments, and call out one genuine strength \
     when you see it. You never restate what the code plainly does, never \
     nitpick formatting, and never invent problems to fill space.";

/// The per-file instruction: the finding format is the machine contract (each
/// finding anchors by its verbatim quote), so it is spelled out rigidly.
const REVIEW_PROMPT: &str =
    "Review this file and give your 3 to 6 most useful findings. Format each \
     finding as exactly three lines and nothing else:\n\
     QUOTE: <a short snippet copied character-for-character from one line of \
     the file - under 80 characters, distinctive enough to occur only once>\n\
     SEVERITY: <exactly one of: critical, major, minor, info, praise>\n\
     NOTE: <one or two sentences of review commentary on that region>\n\
     Use critical for something that will bite in production (data loss, a \
     security hole, corruption), major for a real defect or design risk that \
     should be fixed, minor for a small improvement where correctness is not \
     at stake, info for an observation or a question, and praise for a genuine \
     strength. Use no other word for SEVERITY.\n\
     Do not number the findings. Do not add headings, preamble, or closing \
     remarks. The QUOTE must appear verbatim in the file or the finding is \
     discarded.";

/// The format contract again, appended AFTER the content: the last words the
/// model reads must be the format, or a long file crowds the contract out of
/// its answer (measured on qwen3-coder:30b — a 16 KiB prompt with the
/// contract only up top yielded label-free findings; restated, 6/6 labeled).
const REVIEW_REMINDER: &str =
    "Now give the findings. Remember the format contract: each finding is \
     exactly three lines - the first starts `QUOTE: ` followed by a short \
     snippet copied character-for-character from one line of the file above, \
     the second starts `SEVERITY: ` followed by exactly one of critical, \
     major, minor, info or praise, the third starts `NOTE: ` with your \
     commentary. No headings, no numbering, nothing else.";

// --- the archive entry (RDF in the shared store) ------------------------------

/// One archived review pass, skolemized like the explanation archive:
///
/// ```turtle
/// <urn:ikigai:browse:review:{repo}:{hash}:{tag}:{path}> a ik:Review ;
///     ik:repo "demo" ; ik:path "src/lib.rs" ;
///     prov:used <urn:repo:demo:file:src/lib.rs> ;
///     ik:contentHash "sha256:…" ; ik:versionTag "review-v1@qwen3-coder:30b" ;
///     ik:model "qwen3-coder:30b" ;
///     prov:generated <urn:iki:annotation:{id}> , … ;
///     ik:orphanedItems "1"^^xsd:nonNegativeInteger ;
///     ik:derivedAt "2026-08-09T17:00:00.000Z"^^xsd:dateTime .
/// ```
///
/// Every `ik:` term here is published: `ikigai-vocab` 0.1.69 added `ik:Review`,
/// `ik:orphanedItems`, `ik:reviewedBytes` and `ik:totalBytes`, which is what put
/// this face under the conformance walk's `VOCABULARY` check. Every provenance
/// link is standard PROV / DC / OA.
///
/// ⚠ `ik:versionTag` and `ik:derivedAt` are shared with the EXPLANATION archive
/// and deliberately carry NO `rdfs:domain`. It was `ik:Explanation` until 0.1.69,
/// which under entailment typed every entry here as an explanation as well —
/// giving either term a domain again re-breaks this graph.
pub(crate) struct PassEntry {
    pub(crate) iri: String,
    pub(crate) repo: String,
    pub(crate) rel: String,
    pub(crate) target_iri: String,
    pub(crate) hash: String,
    pub(crate) tag: String,
    pub(crate) model: String,
    pub(crate) minted: Vec<String>,
    pub(crate) orphaned_items: u64,
    /// How much of the input the model actually saw vs. its full size —
    /// honest reporting for big inputs (`max_prompt_bytes` truncates what is
    /// fed; quotes still anchor against everything). `None` on entries
    /// archived before these fields existed.
    pub(crate) reviewed_bytes: Option<u64>,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) derived_at: Option<String>,
}

impl PassEntry {
    /// The truncation notice the plain and html faces append when the model
    /// saw less than the whole input — silence would misrepresent the pass.
    pub(crate) fn truncation_note(&self) -> Option<String> {
        match (self.reviewed_bytes, self.total_bytes) {
            (Some(reviewed), Some(total)) if reviewed < total => Some(format!(
                " · reviewed {reviewed} of {total} bytes (input truncated)"
            )),
            _ => None,
        }
    }
}

/// A one-line, length-capped excerpt of a raw model answer — carried by the
/// parse-failure errors so a collapsed answer is diagnosable from the error
/// itself (the full answer is one `debug=raw` re-source away).
pub(crate) fn answer_excerpt(answer: &str) -> String {
    const MAX_BYTES: usize = 240;
    let flat = answer.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.len() <= MAX_BYTES {
        return flat;
    }
    let mut end = MAX_BYTES;
    while end > 0 && !flat.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &flat[..end])
}

pub(crate) fn pass_iri(repo: &str, rel: &str, hash: &str, tag: &str) -> String {
    format!(
        "urn:ikigai:browse:review:{repo}:{hash}:{}:{}",
        iri_encode(tag),
        iri_encode(rel)
    )
}

fn store_err(e: impl std::fmt::Display) -> Error {
    Error::Endpoint(format!("browse: review archive: {e}"))
}

const PROV_USED: &str = "http://www.w3.org/ns/prov#used";
const PROV_GENERATED: &str = "http://www.w3.org/ns/prov#generated";

fn prov(term: &str) -> NamedNode {
    NamedNode::new(format!("{PROV}{term}")).expect("prov terms are valid IRIs")
}

pub(crate) fn store_pass(archive: &Archive, entry: &PassEntry) -> Result<()> {
    use oxigraph::model::vocab::{rdf, xsd};
    let subject = NamedNode::new(&entry.iri).map_err(store_err)?;
    let target = NamedNode::new(&entry.target_iri).map_err(store_err)?;
    let g = archive.graph().clone();
    let mut quads: Vec<Quad> = vec![
        Quad::new(subject.clone(), rdf::TYPE, ik("Review"), g.clone()),
        Quad::new(
            subject.clone(),
            ik("repo"),
            Literal::new_simple_literal(&entry.repo),
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            ik("path"),
            Literal::new_simple_literal(&entry.rel),
            g.clone(),
        ),
        Quad::new(subject.clone(), prov("used"), target, g.clone()),
        Quad::new(
            subject.clone(),
            ik("contentHash"),
            Literal::new_simple_literal(&entry.hash),
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            ik("versionTag"),
            Literal::new_simple_literal(&entry.tag),
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            ik("model"),
            Literal::new_simple_literal(&entry.model),
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            ik("orphanedItems"),
            Literal::new_typed_literal(entry.orphaned_items.to_string(), xsd::NON_NEGATIVE_INTEGER),
            g.clone(),
        ),
    ];
    for (term, value) in [
        ("reviewedBytes", entry.reviewed_bytes),
        ("totalBytes", entry.total_bytes),
    ] {
        if let Some(value) = value {
            quads.push(Quad::new(
                subject.clone(),
                ik(term),
                Literal::new_typed_literal(value.to_string(), xsd::NON_NEGATIVE_INTEGER),
                g.clone(),
            ));
        }
    }
    for iri in &entry.minted {
        quads.push(Quad::new(
            subject.clone(),
            prov("generated"),
            NamedNode::new(iri).map_err(store_err)?,
            g.clone(),
        ));
    }
    if let Some(at) = &entry.derived_at {
        quads.push(Quad::new(
            subject,
            ik("derivedAt"),
            Literal::new_typed_literal(at, xsd::DATE_TIME),
            g,
        ));
    }
    for quad in &quads {
        archive.insert(quad).map_err(store_err)?;
    }
    Ok(())
}

/// Load one archived pass by its key IRI — `None` on a miss (no
/// `ik:versionTag` under that subject).
pub(crate) fn load_pass(archive: &Archive, iri: &str) -> Result<Option<PassEntry>> {
    let subject = match NamedNode::new(iri) {
        Ok(node) => node,
        Err(_) => return Ok(None),
    };
    let mut entry = PassEntry {
        iri: iri.to_string(),
        repo: String::new(),
        rel: String::new(),
        target_iri: String::new(),
        hash: String::new(),
        tag: String::new(),
        model: String::new(),
        minted: Vec::new(),
        orphaned_items: 0,
        reviewed_bytes: None,
        total_bytes: None,
        derived_at: None,
    };
    let mut found = false;
    for quad in archive.quads_for_pattern(Some(subject.as_ref().into()), None, None) {
        let quad = quad.map_err(store_err)?;
        let literal = |term: &Term| match term {
            Term::Literal(l) => l.value().to_string(),
            other => other.to_string(),
        };
        let predicate = quad.predicate.as_str();
        match predicate.strip_prefix(IK) {
            Some("versionTag") => {
                entry.tag = literal(&quad.object);
                found = true;
            }
            Some("repo") => entry.repo = literal(&quad.object),
            Some("path") => entry.rel = literal(&quad.object),
            Some("contentHash") => entry.hash = literal(&quad.object),
            Some("model") => entry.model = literal(&quad.object),
            Some("orphanedItems") => {
                entry.orphaned_items = literal(&quad.object).parse().unwrap_or(0);
            }
            Some("reviewedBytes") => {
                entry.reviewed_bytes = literal(&quad.object).parse().ok();
            }
            Some("totalBytes") => {
                entry.total_bytes = literal(&quad.object).parse().ok();
            }
            Some("derivedAt") => entry.derived_at = Some(literal(&quad.object)),
            _ => match predicate {
                PROV_USED => {
                    if let Term::NamedNode(node) = &quad.object {
                        entry.target_iri = node.as_str().to_string();
                    }
                }
                PROV_GENERATED => {
                    if let Term::NamedNode(node) = &quad.object {
                        entry.minted.push(node.as_str().to_string());
                    }
                }
                _ => {}
            },
        }
    }
    // A stable reading of the minted set (insertion order from the store is
    // arbitrary; the face rows re-sort by position anyway).
    entry.minted.sort();
    Ok(found.then_some(entry))
}

// --- parsing the model's findings --------------------------------------------

pub(crate) struct Finding {
    pub(crate) quote: String,
    pub(crate) note: String,
    /// The model's proposed severity, constrained to
    /// [`crate::finding::SEVERITIES`].
    ///
    /// ⚠ `None` means the model gave no `SEVERITY:` line or invented a word
    /// outside the set — and the finding is KEPT unrated rather than dropped
    /// or silently defaulted. The failure-containment rule applies to the
    /// rating exactly as it applies to the quote: one bad item must not kill
    /// the pass, and a fabricated `info` would be indistinguishable from a
    /// rating the model actually made.
    pub(crate) severity: Option<String>,
}

/// Parse `QUOTE:`/`NOTE:` pairs out of the model's answer. Returns the
/// well-formed findings plus the count of malformed items (a `QUOTE:` that
/// never got a note, or a stray `NOTE:`) — counted alongside unanchorable
/// quotes rather than killing the pass. Bare lines after a `NOTE:` continue
/// the note (models wrap); anything before the first `QUOTE:` is preamble and
/// is ignored.
pub(crate) fn parse_findings(answer: &str) -> (Vec<Finding>, u64) {
    let mut findings = Vec::new();
    let mut malformed = 0u64;
    let mut quote: Option<String> = None;
    let mut severity: Option<String> = None;
    let mut note = String::new();
    let mut flush = |quote: &mut Option<String>,
                     severity: &mut Option<String>,
                     note: &mut String,
                     malformed: &mut u64| {
        let severity = severity.take();
        match quote.take() {
            Some(q) if !q.is_empty() && !note.trim().is_empty() => findings.push(Finding {
                quote: q,
                note: note.trim().to_string(),
                severity,
            }),
            Some(_) => *malformed += 1,
            None => {}
        }
        note.clear();
    };
    for line in answer.lines() {
        let trimmed = line.trim();
        if let Some(q) = trimmed.strip_prefix("QUOTE:") {
            flush(&mut quote, &mut severity, &mut note, &mut malformed);
            quote = Some(q.trim().to_string());
        } else if let Some(s) = trimmed.strip_prefix("SEVERITY:") {
            // Lower-cased and trimmed, then checked against the ONE set. A
            // word outside it is dropped, not mapped: the model and the menu
            // must offer the same five words or the two disagree the first
            // time it invents a sixth.
            let word = s.trim().trim_end_matches('.').to_ascii_lowercase();
            severity = crate::finding::is_severity(&word).then_some(word);
        } else if let Some(n) = trimmed.strip_prefix("NOTE:") {
            if quote.is_none() {
                // A stray NOTE with no quote to anchor it.
                malformed += 1;
                continue;
            }
            if !note.is_empty() {
                note.push(' ');
            }
            note.push_str(n.trim());
        } else if quote.is_some() && !note.is_empty() && !trimmed.is_empty() {
            note.push(' ');
            note.push_str(trimmed);
        }
    }
    flush(&mut quote, &mut severity, &mut note, &mut malformed);
    (findings, malformed)
}

// --- binding -----------------------------------------------------------------

pub(crate) fn bind(
    space: EndpointSpace,
    roots: &Roots,
    config: &Arc<ExplainConfig>,
) -> EndpointSpace {
    let review: Arc<dyn Endpoint> = Arc::new(ReviewEndpoint {
        roots: Arc::clone(roots),
        config: Arc::clone(config),
    });
    let space = crate::bind_family(space, roots, review, None, Some("review:{path}"));
    let options: Arc<dyn Endpoint> = Arc::new(OptionsEndpoint {
        roots: Arc::clone(roots),
        config: Arc::clone(config),
    });
    crate::bind_family(space, roots, options, None, Some("review-options:{path}"))
}

/// `urn:repo:{repo}:review:{path}` — the pass a Review button asks for, and
/// the same IRI #261's git-event trigger will ask for.
pub(crate) fn review_iri(repo: &str, rel: &str) -> String {
    format!("urn:repo:{repo}:review:{}", iri_encode(rel))
}

/// `urn:repo:{repo}:review-options:{path}` — the menu's own resource.
fn options_iri(repo: &str, rel: &str) -> String {
    format!("urn:repo:{repo}:review-options:{}", iri_encode(rel))
}

/// The review affordance on a file face: a button that asks for the pass with
/// the host's CONFIGURED backend — `urn:repo:{repo}:review:{path}` with no
/// arguments at all beyond the face, which is exactly the call a headless
/// trigger makes.
///
/// ★ IT ADDS NOTHING OF ITS OWN. No prompt, no post-processing, no second
/// annotation path: the button is a caller of one resource, so a trigger can
/// reuse this IRI verbatim with a different cause and get the same archive key
/// and the same minted annotations. The only thing the markup decides is the
/// FACE (`as=text/html`, because a browser is asking).
pub(crate) fn review_button_html(repo: &str, rel: &str) -> String {
    format!(
        "<button class=\"browse-review-link\" title=\"review this file — one model call, \
         findings minted as annotations\" hx-get=\"/k/source {iri} as=text/html\" \
         hx-target=\"#browse\" hx-swap=\"innerHTML\">review</button>",
        iri = review_iri(repo, rel),
    )
}

/// The closed "review with…" disclosure: markup only, no resolution. Its body
/// is a SEPARATE resolution of this path's `review-options … as=text/html`,
/// fetched on the `toggle` event — so a file view costs nothing for the menu
/// until a human opens it, and opening it costs ONE sub-request (the model
/// inventory) however many backends the host has.
///
/// `<details>`/`<summary>` for the same reasons explain's menu gives: keyboard
/// operable with no CSS and no JavaScript of ours, announced as a disclosure,
/// never hover-only, and block-level so it lays out at any width.
pub(crate) fn menu_html(repo: &str, rel: &str) -> String {
    format!(
        "<details class=\"browse-review-menu\"><summary>review with…</summary>\
         <div class=\"browse-review-menu-body\" hx-get=\"/k/source {iri} as=text/html\" \
         hx-trigger=\"toggle once from:closest details\" hx-target=\"this\" \
         hx-swap=\"innerHTML\"><p>loading options…</p></div></details>",
        iri = options_iri(repo, rel),
    )
}

struct ReviewEndpoint {
    roots: Roots,
    config: Arc<ExplainConfig>,
}

#[async_trait]
impl Endpoint for ReviewEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "browse-review does not support the {:?} verb",
                inv.request.verb
            )));
        }
        let (repo, root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        let rel = path_binding(inv)?;
        if rel.is_empty() {
            return Err(Error::MissingArgument("path".to_string()));
        }
        let target = resolve(root, &rel)?;
        if target.is_dir() {
            return Err(Error::NotFound(format!(
                "browse: `{rel}` is a directory — the review pass is file-grain (annotations \
                 anchor in text)"
            )));
        }
        let config = &self.config;

        // The caller's backend choice, validated BEFORE any work — explain's
        // rule, verbatim: an unknown or non-allowed provider is a refusal that
        // names what was asked for and what is on offer, never a silent fall
        // back to the configured one. A caller who asked for one model, got
        // another, and had the pass archived (and annotations MINTED) under
        // that other model's identity has been lied to durably — the entry and
        // its findings are thereafter indistinguishable from legitimate ones.
        //
        // Review has no `version=`: nothing here addresses an archived pass
        // without deriving one, so unlike explain's there is no argument for
        // this one to be exclusive with.
        let provider = match inv.inline_str("provider") {
            Ok(requested) => {
                let selectable = config.selectable();
                if !selectable.contains(requested) {
                    return Err(Error::Denied(format!(
                        "browse: `{requested}` is not a provider this host offers to review \
                         with; selectable here: {}",
                        selectable.into_iter().collect::<Vec<_>>().join(", ")
                    )));
                }
                requested.to_string()
            }
            Err(_) => config.review_provider.clone(),
        };

        // The archive key's backbone, THROUGH the kernel (dependency-recorded)
        // — same construction as explain.
        let hash_repr = inv.source(&parse_iri(&hash_iri(repo, &rel))?).await?;
        let hash = String::from_utf8_lossy(&hash_repr.bytes).trim().to_string();

        // The content, also through the kernel: the anchor surface (full text
        // — the model sees at most max_prompt_bytes, but quotes anchor
        // against everything).
        let content = inv.source(&parse_iri(&file_iri(repo, &rel))?).await?;
        let Ok(text) = String::from_utf8(content.bytes.clone()) else {
            return Err(Error::InvalidArgument {
                name: "path".to_string(),
                detail: format!("`{rel}` is binary — there is nothing to review"),
            });
        };

        // The model identity for the tag: explicit config label → the
        // provider's resolved `:model` identity → the provider-IRI heuristic.
        //
        // ★ THE LABEL DESCRIBES THE OPERATOR'S BACKEND, so it is keyed to the
        // provider and not to the pass: a request that selects a different
        // backend resolves THAT backend's own identity instead of inheriting a
        // label written for another model. Stamping `review_model_label` onto
        // a model it does not describe would write a wrong identity into the
        // archive KEY and onto every annotation's `dcterms:creator`.
        let explicit = match provider == config.review_provider {
            true => config.review_model_label.as_ref(),
            false => None,
        };
        let model = match explicit {
            Some(label) => label.clone(),
            None => resolve_model(inv, &provider)
                .await
                .unwrap_or_else(|| provider_label(&provider)),
        };
        let tag = format!("{REVIEW_PROMPT_VERSION}@{model}");

        // `debug=raw` is the diagnosis face: derive one fresh answer and
        // return it UNPARSED — nothing minted, nothing archived, the archive
        // neither consulted nor written (a probe must never poison a key).
        let debug_raw = match inv.inline_str("debug") {
            Ok("raw") => true,
            Ok(other) => {
                return Err(Error::InvalidArgument {
                    name: "debug".to_string(),
                    detail: format!("unknown debug face `{other}` (the one face is `raw`)"),
                })
            }
            Err(_) => false,
        };

        let iri = pass_iri(repo, &rel, &hash, &tag);
        if !debug_raw {
            if let Some(entry) = load_pass(&config.archive, &iri)? {
                // The hit path: mints NOTHING. The recorded annotations are
                // drift-reconciled against the very content in hand.
                let included = annotate::included_for_ids(&config.archive, &entry.minted, &text)?;
                return face(inv, repo, &rel, &entry, false, &included);
            }
        }

        // Miss: derive one pass. Ask, parse, anchor, mint, archive.
        let prompt = format!(
            "{REVIEW_PROMPT}\n\nRepository: {repo}\nPath: {rel}\n\n```\n{}\n```\n\n\
             {REVIEW_REMINDER}",
            truncate(&text, config.max_prompt_bytes),
        );
        let request = Request::new(Verb::Source, parse_iri(&provider)?)
            .with_arg("prompt", ArgRef::Inline(prompt.into_bytes()))
            .with_arg(
                "system",
                ArgRef::Inline(REVIEW_SYSTEM_PROMPT.as_bytes().to_vec()),
            )
            .with_arg(
                "temperature",
                ArgRef::Inline(config.temperature.clone().into_bytes()),
            )
            .with_arg(
                "max_tokens",
                ArgRef::Inline(config.review_max_tokens.to_string().into_bytes()),
            );
        let answer = inv.issue(request).await?;
        let answer = String::from_utf8_lossy(&answer.bytes).to_string();
        if debug_raw {
            return Ok(repr_utf8("text/plain", answer));
        }
        let (findings, malformed) = parse_findings(&answer);
        if findings.is_empty() {
            // Nothing parseable at all IS a failure — erroring (and archiving
            // nothing) keeps the key re-derivable instead of poisoning it
            // with an empty pass. The error carries the answer's opening so
            // the collapse is diagnosable (a label-free format, a refusal, an
            // empty ceiling-starved reply all read differently).
            return Err(Error::Endpoint(format!(
                "browse: `{}` returned no parseable QUOTE:/NOTE: findings for `{rel}` \
                 (max_tokens {}); nothing archived. The answer began: \"{}\" — re-source \
                 with debug=raw for the full unparsed answer",
                provider,
                config.review_max_tokens,
                answer_excerpt(&answer)
            )));
        }

        let created = inv.now().map(|t| iso8601(t.as_millis()));
        let mut minted = Vec::new();
        let mut orphaned_items = malformed;
        for finding in &findings {
            match annotate::mint_pending_finding(
                &config.archive,
                &file_iri(repo, &rel),
                repo,
                &rel,
                &text,
                &hash,
                &finding.quote,
                &finding.note,
                finding.severity.as_deref(),
                &model,
                &iri,
                created.clone(),
                annotate::Surface::File,
            )? {
                Some(finding_iri) => minted.push(finding_iri),
                // The model misquoted: mint nothing for this item, count it.
                None => orphaned_items += 1,
            }
        }
        // The same stable order a later load reconstructs (the store keeps no
        // insertion order); the face rows re-sort by anchor position anyway.
        minted.sort();
        if minted.is_empty() {
            return Err(Error::Endpoint(format!(
                "browse: none of the {} finding(s) for `{rel}` anchored (every quote was \
                 misquoted); nothing archived",
                findings.len()
            )));
        }
        let entry = PassEntry {
            iri,
            repo: repo.to_string(),
            rel: rel.clone(),
            target_iri: file_iri(repo, &rel),
            hash,
            tag,
            model,
            minted,
            orphaned_items,
            reviewed_bytes: Some(truncated_len(&text, config.max_prompt_bytes) as u64),
            total_bytes: Some(text.len() as u64),
            derived_at: created,
        };
        store_pass(&config.archive, &entry)?;
        let included = annotate::included_for_ids(&config.archive, &entry.minted, &text)?;
        face(inv, repo, &rel, &entry, true, &included)
    }

    fn name(&self) -> &str {
        "browse-review"
    }

    fn describe(&self) -> Description {
        review_description(&self.config)
    }
}

// --- faces -------------------------------------------------------------------

fn face(
    inv: &Invocation<'_>,
    repo: &str,
    rel: &str,
    entry: &PassEntry,
    derived: bool,
    included: &Included,
) -> Result<Representation> {
    match inv.inline_str("as").unwrap_or("text/plain") {
        t if t.starts_with("application/json") => {
            let json = serde_json::json!({
                "about": entry.target_iri,
                "content_hash": entry.hash,
                "version_tag": entry.tag,
                "model": entry.model,
                "derived": derived,
                "minted": entry.minted,
                "orphaned_items": entry.orphaned_items,
                "reviewed_bytes": entry.reviewed_bytes,
                "total_bytes": entry.total_bytes,
                "derived_at": entry.derived_at,
                "annotations": included.json(),
            });
            Ok(repr("application/json", json.to_string()))
        }
        t if t.starts_with("text/html") => Ok(repr_utf8(
            "text/html",
            review_html(repo, rel, entry, derived, included),
        )),
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
            if let Some(note) = entry.truncation_note() {
                out.push_str(&note);
            }
            out.push('\n');
            out.push_str(&included.margin_text());
            Ok(repr_utf8("text/plain", out))
        }
    }
}

/// The S0 page style: crumbs, a backlink to the reviewed file, the pass's
/// annotation cards (the same machine-marked markup every annotation face
/// renders — no create form, this is the model's margin, not an authoring
/// surface), and the provenance line.
fn review_html(
    repo: &str,
    rel: &str,
    entry: &PassEntry,
    derived: bool,
    included: &Included,
) -> String {
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&crumbs_html(repo, rel));
    out.push_str(&format!(
        "<nav class=\"browse-actions\"><button class=\"browse-view-link\" \
         hx-get=\"/k/source {} as=text/html\" hx-target=\"#browse\" \
         hx-swap=\"innerHTML\">view file</button></nav>",
        entry.target_iri,
    ));
    out.push_str(&included.panel_html(None));
    let hash_short: String = entry.hash.chars().take(19).collect(); // "sha256:" + 12 hex
    let mut provenance = format!(
        "reviewed by {} · {} · {}… · {}",
        esc(&entry.model),
        esc(&entry.tag),
        esc(&hash_short),
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
    if let Some(note) = entry.truncation_note() {
        provenance.push_str(&esc(&note));
    }
    out.push_str(&format!(
        "<p class=\"browse-provenance\">{provenance}</p></div>"
    ));
    out
}

/// The pass entry as Turtle — the same skolemized shape the store holds. The
/// minted annotations are addressable at their own IRIs (and the listing's
/// turtle face serves their full graphs); this face is the pass's record.
pub(crate) fn pass_turtle(entry: &PassEntry) -> String {
    let mut props = vec![
        "a ik:Review".to_string(),
        format!("ik:repo {}", ttl_str(&entry.repo)),
        format!("ik:path {}", ttl_str(&entry.rel)),
        format!("prov:used <{}>", entry.target_iri),
        format!("ik:contentHash {}", ttl_str(&entry.hash)),
        format!("ik:versionTag {}", ttl_str(&entry.tag)),
        format!("ik:model {}", ttl_str(&entry.model)),
        format!(
            "ik:orphanedItems \"{}\"^^xsd:nonNegativeInteger",
            entry.orphaned_items
        ),
    ];
    for (term, value) in [
        ("reviewedBytes", entry.reviewed_bytes),
        ("totalBytes", entry.total_bytes),
    ] {
        if let Some(value) = value {
            props.push(format!("ik:{term} \"{value}\"^^xsd:nonNegativeInteger"));
        }
    }
    if !entry.minted.is_empty() {
        let refs: Vec<String> = entry.minted.iter().map(|iri| format!("<{iri}>")).collect();
        props.push(format!("prov:generated {}", refs.join(", ")));
    }
    if let Some(at) = &entry.derived_at {
        props.push(format!("ik:derivedAt \"{at}\"^^xsd:dateTime"));
    }
    format!(
        "@prefix ik: <{IK}> .\n@prefix prov: <{PROV}> .\n@prefix xsd: \
         <http://www.w3.org/2001/XMLSchema#> .\n\n<{}> {} .\n",
        entry.iri,
        props.join(" ;\n    ")
    )
}

/// `repo` is not an ArgSpec: every advertised row fixes the root in its
/// pattern (see `crate::bind_family`); the binding is grammar-injected.
/// Takes the config because the `provider` argument's `one_of` IS this host's
/// allowlist ([`ExplainConfig::selectable`]) — the manifold must state which
/// backends a caller may actually name, not a hard-coded guess, so that
/// `urn:kernel:validate` can reject a bad one before dispatch and a UI can
/// build its "review with" menu from the description alone.
fn review_description(config: &ExplainConfig) -> Description {
    Description::new("browse-review")
        .title("Machine review pass (annotations minted by a model)")
        .summary(
            "A region-grain machine review of one file — urn:repo:{repo}:review:{path}. \
             Source asks the review model for findings (each an exact quote, a proposed \
             severity and a reviewer's note) and mints every anchored finding as a PENDING \
             urn:iki:finding: — NOT an annotation. ⚠ Nothing a pass produces reaches the \
             urn:iki:annotation: family on its own: Sink urn:iki:finding:{id} \
             decision=publish is the only path in, and it needs urn:cap:annotate, which \
             this pass deliberately does not. Provenance on a finding: dcterms:creator = \
             the model, sh:resultSeverity = its PROPOSED severity, prov:wasGeneratedBy = \
             this pass. The pass is ARCHIVED by (path, content-hash, review-tag) — \
             re-sourcing unchanged content is an archive hit that mints nothing, and a \
             re-derivation re-mints the SAME finding IRIs, so a human decision survives \
             it. Changed content is a fresh pass; earlier findings re-anchor or orphan \
             like annotations. Quotes that do not anchor are counted (orphaned_items), \
             never fatal; a missing or invented SEVERITY leaves the finding unrated \
             rather than dropping it. provider= derives this pass \
             against a different (host-allowed) backend, keyed by that backend's own \
             model identity, so a second model is a second coexisting pass rather than a \
             replacement. text/plain (default) is the \
             margin-notes digest; as=application/json adds {minted, orphaned_items, \
             findings}; as=text/html the card page, each finding with its publish/decline \
             affordance; as=text/turtle the pass's \
             provenance graph.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        // ⚠ NO `urn:cap:annotate` — and its absence is the point, not an
        // oversight. A pass writes PENDING FINDINGS and cannot reach the
        // annotation family at all, so the authority it used to demand is
        // required only by `Sink urn:iki:finding:{id} decision=publish`.
        // ★ That is the safety interlock: a git-event trigger can run
        // headlessly with browse+net and still publish nothing. Declared =
        // enforced in both directions — declaring the annotate cap here would
        // be an over-offer the pass no longer honours.
        .requires(CAP_NET)
        .input(
            ArgSpec::new("path")
                .binding()
                .class(crate::XSD_STRING)
                .summary("file path within the root, percent-encoded"),
        )
        .input(
            ArgSpec::new("provider")
                // The value is an endpoint IRI, not free text — the class says
                // so, so type-based selection can offer it a resource rather
                // than a string. Spelled exactly as explain's, deliberately:
                // one argument name for one concept across the module.
                .class("http://www.w3.org/2001/XMLSchema#anyURI")
                .optional()
                .summary(
                    "the LLM provider IRI that derives THIS review pass, instead of the \
                     configured review tier; one_of is what this host allows. That backend's \
                     own model identity keys the archive entry, so a second model yields a \
                     second pass COEXISTING with the first — its own findings, minted as its \
                     own annotations, alongside rather than instead. A backend serving the \
                     same model as an existing pass keys that same entry: an archive hit \
                     that asks nothing and mints nothing. Unlike explain there is no \
                     version= here — no argument addresses an archived pass — so this one \
                     is exclusive with nothing.",
                )
                .one_of(config.selectable()),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .class(crate::XSD_STRING)
                .summary("the face to render")
                .one_of(["text/plain", "application/json", "text/html", "text/turtle"])
                .default_value("text/plain"),
        )
        .input(
            ArgSpec::new("debug")
                .optional()
                .class(crate::XSD_STRING)
                .summary(
                    "raw: derive and return the model's unparsed answer (text/plain) — \
                     nothing parsed, minted, or archived; the parse-failure diagnosis face",
                )
                .one_of(["raw"]),
        )
        .output("text/plain;charset=utf-8")
        .output("application/json")
        .output("text/html;charset=utf-8")
        .output("text/turtle")
}

// --- the "review with…" menu -------------------------------------------------

/// `urn:repo:{repo}:review-options:{path}` — **which backends this host will
/// review with**, grouped by the model each serves, and nothing else.
///
/// ## What it is not
///
/// ⚠ It is NOT a listing of archived passes, and there deliberately is none.
/// `urn:repo:{repo}:annotations:{path} as=application/json` already carries
/// `creator` (the model) and `generated_by` (the pass) on every row, so "which
/// models have reviewed this file" is a group-by over a listing the file face
/// already renders. A parallel listing would duplicate a join the data answers.
/// This resource answers the other question — what COULD review it — which
/// nothing else does in a form a menu can render.
///
/// ## Why it is its own resource rather than part of the file face
///
/// Two reasons, both about cost and authority:
///
/// * **Cost.** Grouping by model needs `urn:llm:models`. Folding that read
///   into the file face would spend it on every file view, whether or not
///   anyone wants a menu — and on a host with a DISCOVERING backend that read
///   probes, so the file face would inherit a network round trip on the hot
///   path. A separate resource fetched on the disclosure's `toggle` costs
///   nothing at all until a human opens the menu.
/// * **Authority.** It could not live on `browse-review`: that endpoint
///   DECLARES `urn:cap:net:*` and `urn:cap:annotate` because it spends and
///   mints, so a browse-only session would be refused its own menu. This one
///   requires the browse grant alone — reading what is on offer is not
///   spending — which is the same split `explain-versions` makes.
struct OptionsEndpoint {
    roots: Roots,
    config: Arc<ExplainConfig>,
}

#[async_trait]
impl Endpoint for OptionsEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "browse-review-options does not support the {:?} verb",
                inv.request.verb
            )));
        }
        let (repo, _root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        let rel = path_binding(inv)?;
        if rel.is_empty() {
            return Err(Error::MissingArgument("path".to_string()));
        }
        // No filesystem touch and no archive read: the choices are a property
        // of the HOST, not of the path. The path only names what a chosen row
        // would review, so this answers for a path that has since been
        // deleted exactly as it answers for one that has not — and a menu that
        // 404s the moment a file moves would be worse than one that does not.
        //
        // ★ THE ONE SUB-REQUEST. `menu_options` resolves `urn:llm:models`,
        // once, best-effort: no llm module bound (or an unreachable one)
        // degrades the menu to provider IRIs rather than failing it. It never
        // asks a model anything — opening a menu must not cost what the menu
        // exists to let you decide about.
        let options = menu_options(inv, &self.config, &review_tiers(&self.config)).await;
        match inv.inline_str("as").unwrap_or("text/plain") {
            t if t.starts_with("application/json") => {
                let rows: Vec<serde_json::Value> = options
                    .iter()
                    .map(|o| {
                        serde_json::json!({
                            "label": o.label(),
                            "model": o.model(),
                            "provider": o.provider(),
                            "providers": o.providers(),
                            "default_for": o.default_for(),
                        })
                    })
                    .collect();
                Ok(repr(
                    "application/json",
                    serde_json::Value::Array(rows).to_string(),
                ))
            }
            t if t.starts_with("text/html") => Ok(repr_utf8(
                "text/html",
                options_panel_html(repo, &rel, &options),
            )),
            _ => {
                let lines: Vec<String> = options
                    .iter()
                    .map(|o| {
                        format!(
                            "{}\t{}\t{}",
                            o.label(),
                            o.providers().join(","),
                            o.default_for().unwrap_or("-")
                        )
                    })
                    .collect();
                Ok(repr_utf8("text/plain", lines.join("\n")))
            }
        }
    }

    fn name(&self) -> &str {
        "browse-review-options"
    }

    fn describe(&self) -> Description {
        options_description()
    }
}

/// The tier a REVIEW menu marks against: the one configured `review_provider`.
/// Explain has two grains and therefore two tiers; review is file-grain only,
/// so exactly one row can ever be "what a plain review click already does".
fn review_tiers(config: &ExplainConfig) -> [MenuTier<'_>; 1] {
    [MenuTier {
        provider: config.review_provider.as_str(),
        label: "review",
    }]
}

/// The menu panel: one row per MODEL, each button sending
/// `provider={iri}` to the very resource the plain button asks for.
///
/// ★ ONE ROW PER MODEL, NOT PER BACKEND — the same rule the explain menu is
/// built on, for the same reason: the archive tag folds model identity, so two
/// backends serving one model key ONE pass. A menu of backends would offer a
/// second review it cannot produce, and its no-op would read as a bug.
fn options_panel_html(repo: &str, rel: &str, options: &[ModelOption]) -> String {
    let review = review_iri(repo, rel);
    let mut out = String::from("<div class=\"browse-review-menu-panel\">");
    out.push_str(
        "<p class=\"browse-review-menu-heading\">review with \
         <span class=\"browse-size\">derives — one model call, findings minted as \
         annotations</span></p>",
    );
    if options.is_empty() {
        // Unreachable while `selectable()` always holds the configured tiers,
        // but a menu that renders an empty list with no word for it is the
        // kind of blank a reader blames on the fetch.
        out.push_str(
            "<p class=\"browse-review-menu-empty\">this host offers no review backend.</p>",
        );
    }
    out.push_str("<ul class=\"browse-entries browse-review-choices\">");
    for option in options {
        let mut notes: Vec<String> = Vec::new();
        if let Some(tier) = option.default_for() {
            notes.push(format!("default for {tier}"));
        }
        if option.providers().len() > 1 {
            notes.push(format!(
                "served by {}",
                option
                    .providers()
                    .iter()
                    .map(|p| provider_label(p))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if option.model().is_none() {
            notes.push("backend reports no model id".to_string());
        }
        let detail = if notes.is_empty() {
            String::new()
        } else {
            format!(
                " <span class=\"browse-size\">{}</span>",
                esc(&notes.join(" · "))
            )
        };
        let label = option.label();
        if command_safe(option.provider()) {
            out.push_str(&format!(
                "<li><button class=\"browse-review-link\" hx-get=\"/k/source {review} \
                 as=text/html provider={provider}\" hx-target=\"#browse\" \
                 hx-swap=\"innerHTML\">{label}</button>{detail}</li>",
                provider = esc(option.provider()),
                label = esc(&label),
            ));
        } else {
            out.push_str(&format!(
                "<li><span class=\"browse-review-inert\">{}</span>{detail}</li>",
                esc(&label),
            ));
        }
    }
    out.push_str("</ul>");
    out.push_str(
        "<p class=\"browse-review-menu-note\"><span class=\"browse-size\">One row per model, \
         not per backend: a pass is archived by the model and the prompt that made it, so two \
         backends serving one model give one review — the second request is an archive hit \
         that mints nothing. A second MODEL is a second pass alongside the first, not a \
         replacement: both sets of findings stay on the file.</span></p>",
    );
    out.push_str("</div>");
    out
}

/// `repo` is not an ArgSpec — see [`review_description`]'s note.
fn options_description() -> Description {
    Description::new("browse-review-options")
        .title("Backends this host will review with")
        .summary(
            "Which providers a review of this path may name — \
             urn:repo:{repo}:review-options:{path}: this host's `provider=` allowlist \
             grouped by the MODEL each backend serves, because the review archive keys on \
             the model and two backends serving one model key ONE pass. Derives nothing, \
             asks no model, needs no network grant, and reads neither the working tree nor \
             the archive: the rows are a property of the host, so a deleted path still \
             answers. It is NOT a listing of archived passes — \
             urn:repo:{repo}:annotations:{path} as=application/json already carries creator \
             and generated_by per finding, which is the same question answered by data that \
             already exists. text/plain (default) is label<TAB>providers<TAB>defaultFor \
             lines; as=application/json the structured rows; as=text/html the option menu \
             the file face opens beside its review button, each row sending provider= to \
             urn:repo:{repo}:review:{path}. The html and json faces read urn:llm:models \
             once, best-effort: without it the menu degrades to provider IRIs rather than \
             failing.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("path")
                .binding()
                .class(crate::XSD_STRING)
                .summary("file path within the root, percent-encoded"),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .class(crate::XSD_STRING)
                .summary("application/json for the structured rows, text/html for the option menu")
                .one_of(["text/plain", "application/json", "text/html"])
                .default_value("text/plain"),
        )
        .output("text/plain;charset=utf-8")
        .output("application/json")
        .output("text/html;charset=utf-8")
}

// --- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate::CAP_ANNOTATE;
    use futures::executor::block_on;
    use ikigai_core::{Capability, Exact, Fallback, FnEndpoint, Iri, Kernel};
    use oxigraph::store::Store;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-review-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The review provider the default config asks.
    const PROVIDER: &str = "urn:llm:coder:ask";

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

    /// A deterministic fake review model: every ask is recorded and answered
    /// with the canned `reply`. Declares the net wildcard like the real module.
    fn llm_space(log: &Arc<Log>, reply: &str) -> EndpointSpace {
        let log = Arc::clone(log);
        let reply = reply.to_string();
        EndpointSpace::new().bind(
            Exact::new(PROVIDER),
            FnEndpoint::new("fake-review-llm", move |inv: &Invocation<'_>| {
                log.asks.lock().unwrap().push((
                    inv.inline_str("prompt").unwrap_or("").to_string(),
                    inv.inline_str("system").unwrap_or("").to_string(),
                    inv.inline_str("max_tokens").unwrap_or("").to_string(),
                ));
                Ok(repr_utf8("text/plain", reply.clone()))
            })
            .with_description(
                Description::new("fake-review-llm")
                    .verb(Verb::Source)
                    .requires(CAP_NET),
            ),
        )
    }

    fn kernel_with(
        root: &std::path::Path,
        store: &Arc<Store>,
        log: &Arc<Log>,
        reply: &str,
    ) -> Kernel {
        let cfg = ExplainConfig::new(Arc::clone(store)).review_model_label("r1");
        let browse = crate::space_with_explain(vec![("demo".to_string(), root.to_path_buf())], cfg);
        Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(llm_space(log, reply)),
        ])))
    }

    /// A second bound backend, for the `provider=` tests. No `:model`
    /// resource answers for it, so its tag label is the provider heuristic
    /// (`alt`) — which is also the point: it is NOT `r1`, the label the
    /// operator wrote for the configured backend.
    const ALT_PROVIDER: &str = "urn:llm:alt:ask";
    const ALT_FINDINGS: &str =
        "QUOTE: fn gamma() {}\nNOTE: The third entry point has no caller in this file.\n";

    fn alt_llm_space(log: &Arc<Log>, reply: &str) -> EndpointSpace {
        let log = Arc::clone(log);
        let reply = reply.to_string();
        EndpointSpace::new().bind(
            Exact::new(ALT_PROVIDER),
            FnEndpoint::new("fake-alt-llm", move |inv: &Invocation<'_>| {
                log.asks.lock().unwrap().push((
                    inv.inline_str("prompt").unwrap_or("").to_string(),
                    inv.inline_str("system").unwrap_or("").to_string(),
                    inv.inline_str("max_tokens").unwrap_or("").to_string(),
                ));
                Ok(repr_utf8("text/plain", reply.clone()))
            })
            .with_description(
                Description::new("fake-alt-llm")
                    .verb(Verb::Source)
                    .requires(CAP_NET),
            ),
        )
    }

    /// Both backends bound, with the host config in the test's hands — the
    /// shape every `provider=` case needs.
    fn kernel_with_alt(
        root: &std::path::Path,
        store: &Arc<Store>,
        log: &Arc<Log>,
        alt_log: &Arc<Log>,
        config: impl FnOnce(ExplainConfig) -> ExplainConfig,
    ) -> Kernel {
        let cfg = config(ExplainConfig::new(Arc::clone(store)).review_model_label("r1"));
        let browse = crate::space_with_explain(vec![("demo".to_string(), root.to_path_buf())], cfg);
        Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(llm_space(log, TWO_FINDINGS)),
            Arc::new(alt_llm_space(alt_log, ALT_FINDINGS)),
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

    fn json(kernel: &Kernel, iri: &str, extra: &[(&str, &str)]) -> serde_json::Value {
        let mut args = vec![("as", "application/json")];
        args.extend_from_slice(extra);
        serde_json::from_str(&body(
            &issue(kernel, Verb::Source, iri, &args, &cap()).unwrap(),
        ))
        .unwrap()
    }

    const CONTENT: &str = "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n";
    /// Two well-formed v3 findings: one rated `praise`, one rated `minor` —
    /// so every count below is also a check that the `SEVERITY:` line parsed
    /// and reached the stored proposal.
    const TWO_FINDINGS: &str = "QUOTE: fn alpha() {}\nSEVERITY: praise\nNOTE: A clear entry \
         point; the naming makes the call order obvious.\nQUOTE: fn beta() {}\nSEVERITY: \
         minor\nNOTE: Consider a doc comment - the role of this helper is not evident.\n";

    fn demo_root() -> PathBuf {
        let root = temp_dir();
        std::fs::write(root.join("a.rs"), CONTENT).unwrap();
        root
    }

    #[test]
    fn a_review_derives_once_and_mints_once() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, TWO_FINDINGS);

        let first = json(&k, "urn:repo:demo:review:a.rs", &[]);
        assert_eq!(first["derived"], true);
        assert_eq!(first["version_tag"], "review-v3@r1");
        assert_eq!(first["model"], "r1");
        assert_eq!(first["orphaned_items"], 0);
        // Nothing was truncated, and the face says exactly what was seen.
        assert_eq!(first["reviewed_bytes"], CONTENT.len());
        assert_eq!(first["total_bytes"], CONTENT.len());
        assert_eq!(first["minted"].as_array().unwrap().len(), 2);
        assert_eq!(first["annotations"].as_array().unwrap().len(), 2);
        assert_eq!(log.count(), 1);

        // ★★ THE PASS MINTS NOTHING INTO THE ANNOTATION FAMILY. This is the
        // whole of ledger #444 in one assertion: the model produced two
        // findings, they are addressable and rated, and the family every
        // existing reader and query looks at is EMPTY until a human acts.
        let annotations = json(&k, "urn:repo:demo:annotations:a.rs", &[]);
        assert_eq!(
            annotations.as_array().unwrap().len(),
            0,
            "a review pass must not publish anything: {annotations}"
        );
        for iri in first["minted"].as_array().unwrap() {
            let iri = iri.as_str().unwrap();
            assert!(iri.starts_with("urn:iki:finding:"), "{iri}");
        }

        // They are in the PENDING QUEUE instead, rated by the model, in
        // triage order (minor before praise).
        let queue = json(&k, "urn:repo:demo:findings:a.rs", &[]);
        let rows = queue.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["exact"], "fn beta() {}");
        assert_eq!(rows[0]["severity"], "minor");
        assert_eq!(rows[0]["state"], "pending");
        assert_eq!(rows[0]["machine"], true);
        assert_eq!(rows[0]["creator"], "r1");
        assert_eq!(rows[0]["decision"], serde_json::Value::Null);
        assert_eq!(rows[1]["exact"], "fn alpha() {}");
        assert_eq!(rows[1]["severity"], "praise");
        assert_eq!(rows[1]["line"], 1);

        // Re-source on unchanged content: an archive hit that MINTS NOTHING —
        // no new ask, no new findings, the same recorded set.
        let second = json(&k, "urn:repo:demo:review:a.rs", &[]);
        assert_eq!(second["derived"], false);
        assert_eq!(second["minted"], first["minted"]);
        assert_eq!(log.count(), 1, "the hit must not re-ask");
        let queue = json(&k, "urn:repo:demo:findings:a.rs", &[]);
        assert_eq!(queue.as_array().unwrap().len(), 2, "mint-once");

        // The prompt fed the model the file and the format contract — and the
        // contract is RESTATED after the content (long inputs crowd a
        // top-only contract out of the answer).
        let (prompt, system, max_tokens) = log.last();
        assert!(prompt.contains("QUOTE:"), "{prompt}");
        assert!(prompt.contains("fn beta()"), "{prompt}");
        assert!(
            prompt.rfind("format contract").unwrap() > prompt.rfind("fn gamma()").unwrap(),
            "the reminder must follow the content: {prompt}"
        );
        assert!(system.contains("reviewing a colleague's file"), "{system}");
        assert_eq!(max_tokens, "800");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn machine_and_human_annotations_are_distinguishable_across_faces() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, TWO_FINDINGS);

        // A human note next to the machine pass.
        issue(
            &k,
            Verb::Sink,
            "urn:iki:annotation:h1",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "fn gamma() {}"),
                ("body", "a human margin note"),
            ],
            &cap(),
        )
        .unwrap();
        let pass = json(&k, "urn:repo:demo:review:a.rs", &[]);

        // Only the human note is in the annotation family: the machine's two
        // are pending. A human PUBLISHES one of them — the only way in.
        let pending: Vec<String> = pass["minted"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i.as_str().unwrap().to_string())
            .collect();
        let published = issue(
            &k,
            Verb::Sink,
            &pending[0],
            &[("decision", "publish")],
            &cap(),
        )
        .unwrap();
        let published = body(&published);
        assert!(
            published.starts_with("urn:iki:annotation:"),
            "publishing answers with the annotation it minted: {published}"
        );

        // JSON: one axis, two kinds, provenance on every row — and the
        // machine row is there because a person put it there.
        let listing = json(&k, "urn:repo:demo:annotations:a.rs", &[]);
        let rows = listing.as_array().unwrap();
        assert_eq!(rows.len(), 2, "one published finding, one human note");
        let machine: Vec<bool> = rows.iter().map(|r| r["machine"] == true).collect();
        assert_eq!(machine, [true, false], "reading order: the quote, gamma");
        assert_eq!(rows[1]["motivation"], "commenting");
        assert_eq!(rows[1]["creator"], serde_json::Value::Null);
        assert_eq!(rows[0]["motivation"], "assessing");
        assert!(rows[0]["generated_by"]
            .as_str()
            .unwrap()
            .starts_with("urn:ikigai:browse:review:demo:sha256:"));
        // ★ The published annotation points back at the finding it was rated
        // against, so the model's proposal is one hop from the published note.
        assert_eq!(
            rows[0]["derived_from"],
            serde_json::Value::String(pending[0].clone())
        );

        // The file HTML face: hollow machine markers, solid human dot, the
        // model identity on the machine cards.
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
        assert!(html.contains("browse-annotation-marker-machine"), "{html}");
        assert!(html.contains("○"), "{html}");
        assert!(html.contains("●"), "{html}");
        assert!(html.contains("browse-annotation-machine"), "{html}");
        assert!(html.contains("review by r1"), "{html}");

        // The text margin labels the machine rows.
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
        assert!(text.contains("[review:r1]"), "{text}");
        assert!(text.contains("a human margin note"), "{text}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_misquoted_finding_is_counted_not_fatal() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let with_bad = format!(
            "{TWO_FINDINGS}QUOTE: fn missing() {{}}\nNOTE: this quote is not in the file.\n"
        );
        let k = kernel_with(&root, &store, &log, &with_bad);

        let pass = json(&k, "urn:repo:demo:review:a.rs", &[]);
        assert_eq!(pass["minted"].as_array().unwrap().len(), 2);
        assert_eq!(pass["orphaned_items"], 1);
        // The count survives into the archived entry (the hit serves it too).
        let hit = json(&k, "urn:repo:demo:review:a.rs", &[]);
        assert_eq!(hit["orphaned_items"], 1);
        assert_eq!(log.count(), 1);
        // And the plain face names it.
        let text =
            body(&issue(&k, Verb::Source, "urn:repo:demo:review:a.rs", &[], &cap()).unwrap());
        assert!(text.contains("1 item(s) did not anchor"), "{text}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_unusable_review_is_an_error_and_never_archived() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());

        // Nothing parseable at all: error, nothing archived, retry re-asks —
        // and the error CARRIES the answer's opening plus the debug=raw
        // affordance (the collapse must be diagnosable from the error).
        let k = kernel_with(&root, &store, &log, "I think this file is nice overall.");
        let err = issue(&k, Verb::Source, "urn:repo:demo:review:a.rs", &[], &cap()).unwrap_err();
        assert!(format!("{err:?}").contains("no parseable"), "{err:?}");
        assert!(
            format!("{err:?}").contains("I think this file is nice overall."),
            "{err:?}"
        );
        assert!(format!("{err:?}").contains("debug=raw"), "{err:?}");
        let err = issue(&k, Verb::Source, "urn:repo:demo:review:a.rs", &[], &cap()).unwrap_err();
        assert!(format!("{err:?}").contains("no parseable"), "{err:?}");
        assert_eq!(log.count(), 2, "an unarchived pass re-derives");
        let listing = json(&k, "urn:repo:demo:findings:a.rs", &[]);
        assert_eq!(listing.as_array().unwrap().len(), 0, "nothing minted");

        // Every quote misquoted: likewise fatal, nothing minted or archived.
        let store2 = Arc::new(Store::new().unwrap());
        let k2 = kernel_with(
            &root,
            &store2,
            &log,
            "QUOTE: fn nowhere() {}\nNOTE: a ghost finding.\n",
        );
        let err = issue(&k2, Verb::Source, "urn:repo:demo:review:a.rs", &[], &cap()).unwrap_err();
        assert!(format!("{err:?}").contains("anchored"), "{err:?}");
        let listing = json(&k2, "urn:repo:demo:findings:a.rs", &[]);
        assert_eq!(listing.as_array().unwrap().len(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_changed_file_gets_a_fresh_pass_and_the_old_notes_drift() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, TWO_FINDINGS);
        issue(&k, Verb::Source, "urn:repo:demo:review:a.rs", &[], &cap()).unwrap();

        // beta is edited away, a line lands above alpha: the next review is a
        // fresh pass (new hash, new mints)…
        std::fs::write(root.join("a.rs"), "// new\nfn alpha() {}\nfn gamma() {}\n").unwrap();
        let fresh = json(&k, "urn:repo:demo:review:a.rs", &[]);
        assert_eq!(fresh["derived"], true);
        assert_eq!(log.count(), 2);

        // …while the FIRST pass's findings re-anchor or orphan exactly like
        // annotations do — the drift is the review history, kept visible.
        // ★ A pending finding gets THE drift story, not a second one: a
        // finding whose file has since changed is stale by construction, and
        // the answer to that already existed.
        let listing = json(&k, "urn:repo:demo:findings:a.rs", &[]);
        let rows = listing.as_array().unwrap();
        assert_eq!(rows.len(), 3, "2 from pass one + 1 anchoring from pass two");
        let alpha_old: Vec<&serde_json::Value> = rows
            .iter()
            .filter(|r| r["exact"] == "fn alpha() {}" && r["reanchored"] == true)
            .collect();
        assert_eq!(
            alpha_old.len(),
            1,
            "pass one's alpha re-anchored: {listing}"
        );
        let beta: Vec<&serde_json::Value> = rows
            .iter()
            .filter(|r| r["exact"] == "fn beta() {}")
            .collect();
        assert_eq!(beta.len(), 1);
        assert_eq!(beta[0]["orphaned"], true, "pass one's beta orphaned");
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★★ **The safety interlock, in one test.** A review pass needs browse
    /// and net and nothing else — so a headless git-event trigger can run
    /// without the authority to publish anything — and publishing needs
    /// `urn:cap:annotate`, which is the authority nobody has by accident.
    #[test]
    fn a_review_runs_unarmed_and_only_publishing_needs_annotate() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, TWO_FINDINGS);

        for missing in [
            // No net: the pass asks a model — denied at baseline.
            Capability::scoped(["urn:cap:browse:read:demo", CAP_ANNOTATE]),
            // No browse grant at all.
            Capability::scoped(["urn:cap:net:localhost", CAP_ANNOTATE]),
        ] {
            let err =
                issue(&k, Verb::Source, "urn:repo:demo:review:a.rs", &[], &missing).unwrap_err();
            assert!(matches!(err, Error::Denied(_)), "{err:?}");
        }
        // A browse grant on the WRONG root: past the baseline wildcard,
        // denied by the per-root check.
        let wrong = Capability::scoped([
            "urn:cap:browse:read:other",
            "urn:cap:net:localhost",
            CAP_ANNOTATE,
        ]);
        let err = issue(&k, Verb::Source, "urn:repo:demo:review:a.rs", &[], &wrong).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert_eq!(log.count(), 0, "no ask ever left");

        // ★ THE TRIGGER'S GRANT: browse + net, no annotate. The pass runs,
        // the findings land, and nothing is published.
        let unarmed = Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:net:localhost"]);
        let pass = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:review:a.rs",
            &[("as", "application/json")],
            &unarmed,
        )
        .unwrap();
        let pass: serde_json::Value = serde_json::from_str(&body(&pass)).unwrap();
        let pending: Vec<String> = pass["minted"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i.as_str().unwrap().to_string())
            .collect();
        assert_eq!(pending.len(), 2);
        assert_eq!(log.count(), 1, "the unarmed pass really derived");

        // …and that same grant cannot publish one of them.
        let denied = issue(
            &k,
            Verb::Sink,
            &pending[0],
            &[("decision", "publish")],
            &unarmed,
        )
        .unwrap_err();
        assert!(matches!(denied, Error::Denied(_)), "{denied:?}");
        let annotations = json(&k, "urn:repo:demo:annotations:a.rs", &[]);
        assert_eq!(annotations.as_array().unwrap().len(), 0, "{annotations}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_turtle_faces_record_the_provenance() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, TWO_FINDINGS);

        let out = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:review:a.rs",
            &[("as", "text/turtle")],
            &cap(),
        )
        .unwrap();
        assert_eq!(out.repr_type.media_type, "text/turtle");
        let triples: Vec<_> = oxttl::TurtleParser::new()
            .for_slice(out.bytes.as_slice())
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap_or_else(|e| panic!("turtle face must parse: {e}\n{}", body(&out)));
        assert!(!triples.is_empty());
        for t in &triples {
            assert!(!t.subject.to_string().starts_with("_:"), "no blank nodes");
        }
        let ttl = body(&out);
        assert!(ttl.contains("a ik:Review"), "{ttl}");
        assert!(ttl.contains("prov:used <urn:repo:demo:file:a.rs>"), "{ttl}");
        // ⚠ The pass generated FINDINGS, not annotations: the entry's
        // `prov:generated` is the pending set a human has yet to answer.
        assert!(ttl.contains("prov:generated <urn:iki:finding:"), "{ttl}");
        assert!(!ttl.contains("urn:iki:annotation:"), "{ttl}");
        assert!(ttl.contains("ik:versionTag \"review-v3@r1\""), "{ttl}");
        assert!(
            ttl.contains("ik:orphanedItems \"0\"^^xsd:nonNegativeInteger"),
            "{ttl}"
        );
        assert!(
            ttl.contains(&format!(
                "ik:totalBytes \"{}\"^^xsd:nonNegativeInteger",
                CONTENT.len()
            )),
            "{ttl}"
        );

        // The minted findings' own turtle carries the standard provenance —
        // and NOT `oa:Annotation`, nor any `oa:` term whose domain would
        // entail it.
        let ttl = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:findings:a.rs",
                &[("as", "text/turtle")],
                &cap(),
            )
            .unwrap(),
        );
        assert!(ttl.contains("dcterms:creator \"r1\""), "{ttl}");
        assert!(ttl.contains("a prov:Entity"), "{ttl}");
        assert!(
            ttl.contains("sh:resultSeverity <urn:iki:severity:"),
            "{ttl}"
        );
        assert!(!ttl.contains("a oa:Annotation"), "{ttl}");
        assert!(!ttl.contains("oa:bodyValue"), "{ttl}");
        assert!(!ttl.contains("oa:motivatedBy"), "{ttl}");
        assert!(
            ttl.contains("prov:wasGeneratedBy <urn:ikigai:browse:review:"),
            "{ttl}"
        );
        let triples: Vec<_> = oxttl::TurtleParser::new()
            .for_slice(ttl.as_bytes())
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap_or_else(|e| panic!("annotation turtle must parse: {e}\n{ttl}"));
        assert!(!triples.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn directories_and_binaries_are_not_reviewable() {
        let root = demo_root();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("img.png"), [0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF]).unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, TWO_FINDINGS);

        let err = issue(&k, Verb::Source, "urn:repo:demo:review:sub", &[], &cap()).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        let err = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:review:img.png",
            &[],
            &cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");
        assert_eq!(log.count(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn describe_declares_the_capability_contract() {
        let roots: Roots = Arc::new(std::collections::BTreeMap::from([(
            "demo".to_string(),
            PathBuf::from("/tmp"),
        )]));
        let config = Arc::new(ExplainConfig::new(Arc::new(Store::new().unwrap())));
        let endpoint = ReviewEndpoint { roots, config };
        let description = endpoint.describe();
        for cap in [CAP_WILDCARD, CAP_NET] {
            assert!(
                description.requires.contains(&cap.to_string()),
                "missing {cap}"
            );
        }
        // ★ And NOT annotate. Declared = enforced in both directions: a pass
        // that cannot reach the annotation family must not demand the
        // authority to, or every trigger has to be armed to run at all.
        assert!(
            !description.requires.contains(&CAP_ANNOTATE.to_string()),
            "the pass mints pending findings; publishing is the only annotate act"
        );
        // No `repo` ArgSpec: rows fix the root; the binding is
        // grammar-injected.
        let names: Vec<&str> = description.inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["path", "provider", "as", "debug"]);

        // The manifold publishes the allowlist: `provider`'s one_of IS what
        // this host permits, so validate can reject before dispatch and a UI
        // can build its "review with" menu from the description alone. On a
        // default config every tier points at one of two backends, so the set
        // is the same two explain offers — no new reach.
        let provider_spec = description
            .inputs
            .iter()
            .find(|i| i.name == "provider")
            .unwrap();
        assert_eq!(provider_spec.one_of, ["urn:llm:ask", "urn:llm:coder:ask"]);
    }

    #[test]
    fn debug_raw_returns_the_unparsed_answer_and_touches_nothing() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, TWO_FINDINGS);

        // An archived pass first — the probe must bypass it, not serve it.
        issue(&k, Verb::Source, "urn:repo:demo:review:a.rs", &[], &cap()).unwrap();
        assert_eq!(log.count(), 1);

        let raw = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:review:a.rs",
            &[("debug", "raw")],
            &cap(),
        )
        .unwrap();
        assert_eq!(raw.repr_type.media_type, "text/plain");
        assert_eq!(body(&raw), TWO_FINDINGS, "the answer verbatim, unparsed");
        assert_eq!(log.count(), 2, "a fresh ask, not the archive hit");
        // Nothing new minted by the probe.
        let listing = json(&k, "urn:repo:demo:findings:a.rs", &[]);
        assert_eq!(listing.as_array().unwrap().len(), 2);

        // The probe works where the normal pass FAILS — the whole point.
        let store2 = Arc::new(Store::new().unwrap());
        let log2 = Arc::new(Log::default());
        let k2 = kernel_with(&root, &store2, &log2, "label-free musings about the file");
        let raw = issue(
            &k2,
            Verb::Source,
            "urn:repo:demo:review:a.rs",
            &[("debug", "raw")],
            &cap(),
        )
        .unwrap();
        assert_eq!(body(&raw), "label-free musings about the file");
        let listing = json(&k2, "urn:repo:demo:findings:a.rs", &[]);
        assert_eq!(listing.as_array().unwrap().len(), 0, "nothing minted");

        // An unknown debug face is a typed argument error, no ask spent.
        let err = issue(
            &k2,
            Verb::Source,
            "urn:repo:demo:review:a.rs",
            &[("debug", "verbose")],
            &cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");
        assert_eq!(log2.count(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_truncated_input_is_reported_honestly() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        // A ceiling below the file size: the model sees a prefix, the quotes
        // still anchor against the WHOLE file, and every face says how much
        // was actually reviewed.
        let cfg = ExplainConfig::new(Arc::clone(&store))
            .review_model_label("r1")
            .max_prompt_bytes(20);
        let browse = crate::space_with_explain(vec![("demo".to_string(), root.clone())], cfg);
        let k = Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(llm_space(&log, TWO_FINDINGS)),
        ])));

        let pass = json(&k, "urn:repo:demo:review:a.rs", &[]);
        assert_eq!(pass["reviewed_bytes"], 20);
        assert_eq!(pass["total_bytes"], CONTENT.len());
        // `fn beta() {}` lies past the 20-byte window yet anchors: the anchor
        // surface is the full text, only the prompt is truncated.
        assert_eq!(pass["minted"].as_array().unwrap().len(), 2);
        let (prompt, _, _) = log.last();
        assert!(prompt.contains("… (content truncated)"), "{prompt}");

        // The archive hit serves the same honest numbers, and the plain face
        // names the truncation.
        let hit = json(&k, "urn:repo:demo:review:a.rs", &[]);
        assert_eq!(hit["derived"], false);
        assert_eq!(hit["reviewed_bytes"], 20);
        let text =
            body(&issue(&k, Verb::Source, "urn:repo:demo:review:a.rs", &[], &cap()).unwrap());
        assert!(
            text.contains(&format!(
                "reviewed 20 of {} bytes (input truncated)",
                CONTENT.len()
            )),
            "{text}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parse_findings_is_tolerant_of_model_wrapping() {
        // Preamble ignored, wrapped notes joined, stray NOTE counted, a
        // QUOTE without a note counted.
        let answer = "Here are my findings:\n\
             NOTE: stray with no quote\n\
             QUOTE: fn alpha() {}\n\
             NOTE: first line\n\
             wrapped second line\n\
             QUOTE: fn beta() {}\n\
             NOTE: fine\n\
             QUOTE: fn gamma() {}\n";
        let (findings, malformed) = parse_findings(answer);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].quote, "fn alpha() {}");
        assert_eq!(findings[0].note, "first line wrapped second line");
        assert_eq!(findings[1].note, "fine");
        assert_eq!(malformed, 2, "the stray NOTE and the noteless QUOTE");
    }

    #[test]
    fn a_second_provider_derives_a_second_coexisting_pass() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let alt_log = Arc::new(Log::default());
        let k = kernel_with_alt(&root, &store, &log, &alt_log, |c| {
            c.allow_provider(ALT_PROVIDER)
        });

        // The configured backend first.
        let first = json(&k, "urn:repo:demo:review:a.rs", &[]);
        assert_eq!(first["derived"], true);
        assert_eq!(first["version_tag"], "review-v3@r1");
        assert_eq!(first["minted"].as_array().unwrap().len(), 2);
        assert_eq!((log.count(), alt_log.count()), (1, 0));

        // The second backend over the SAME content: a different model
        // identity is a different key, so this derives rather than hitting —
        // and the operator's `review_model_label` does not follow it.
        let second = json(
            &k,
            "urn:repo:demo:review:a.rs",
            &[("provider", ALT_PROVIDER)],
        );
        assert_eq!(second["derived"], true);
        assert_eq!(second["version_tag"], "review-v3@alt");
        assert_eq!(second["model"], "alt");
        assert_eq!(second["minted"].as_array().unwrap().len(), 1);
        assert_eq!((log.count(), alt_log.count()), (1, 1));
        assert_ne!(first["minted"], second["minted"]);

        // ★ COEXISTING, not replacing: the first pass is still there, still a
        // hit, still its own findings — and the file's one QUEUE now carries
        // both reviewers' margins, each awaiting the same human.
        let again = json(&k, "urn:repo:demo:review:a.rs", &[]);
        assert_eq!(again["derived"], false, "the first pass was overwritten");
        assert_eq!(again["version_tag"], "review-v3@r1");
        assert_eq!(again["minted"], first["minted"]);
        let alt_again = json(
            &k,
            "urn:repo:demo:review:a.rs",
            &[("provider", ALT_PROVIDER)],
        );
        assert_eq!(alt_again["derived"], false);
        assert_eq!(alt_again["minted"], second["minted"]);
        assert_eq!(
            (log.count(), alt_log.count()),
            (1, 1),
            "neither hit may re-ask"
        );

        let listing = json(&k, "urn:repo:demo:findings:a.rs", &[]);
        let rows = listing.as_array().unwrap();
        assert_eq!(rows.len(), 3, "two passes' findings on one axis: {rows:?}");
        let creators: Vec<&str> = rows
            .iter()
            .map(|r| r["creator"].as_str().unwrap())
            .collect();
        // ⚠ TRIAGE order, not reading order — the queue's whole job. r1's
        // beta is `minor`, alt gave no SEVERITY at all (so it sorts with
        // `info`), r1's alpha is `praise`.
        assert_eq!(creators, ["r1", "alt", "r1"], "{rows:?}");
        let severities: Vec<&serde_json::Value> = rows.iter().map(|r| &r["severity"]).collect();
        assert_eq!(
            severities,
            [
                &serde_json::Value::String("minor".into()),
                &serde_json::Value::Null,
                &serde_json::Value::String("praise".into())
            ],
            "an unrated finding is kept unrated, never defaulted: {rows:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_provider_this_host_does_not_offer_is_refused_before_any_ask() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let alt_log = Arc::new(Log::default());
        // Bound, but NOT in the operator's selectable set.
        let k = kernel_with_alt(&root, &store, &log, &alt_log, |c| c);

        let err = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:review:a.rs",
            &[("provider", ALT_PROVIDER)],
            &cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        let message = err.to_string();
        assert!(message.contains(ALT_PROVIDER), "{message}");
        assert!(
            message.contains(PROVIDER),
            "names what IS on offer: {message}"
        );
        assert_eq!(
            (log.count(), alt_log.count()),
            (0, 0),
            "refused before any work"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_distinct_review_tier_is_selectable_as_itself() {
        // ★ THE TRAP THIS ARC CLOSED. `selectable()` held the two explain
        // tiers only, so on a host whose review tier is a DIFFERENT backend,
        // `provider=<the review default>` — the manifold naming exactly what
        // the server does when asked nothing — came back Denied. It never
        // showed on our host because there the review tier equals the file
        // tier.
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let alt_log = Arc::new(Log::default());
        let k = kernel_with_alt(&root, &store, &log, &alt_log, |c| {
            c.review_provider(ALT_PROVIDER)
        });

        let pass = json(
            &k,
            "urn:repo:demo:review:a.rs",
            &[("provider", ALT_PROVIDER)],
        );
        assert_eq!(pass["derived"], true);
        assert_eq!(
            pass["version_tag"], "review-v3@r1",
            "the configured tier keeps its label"
        );
        assert_eq!((log.count(), alt_log.count()), (0, 1));

        // And the manifold says so: the one_of a UI builds its menu from.
        let description = review_description(
            &ExplainConfig::new(Arc::clone(&store)).review_provider(ALT_PROVIDER),
        );
        let provider = description
            .inputs
            .iter()
            .find(|a| a.name == "provider")
            .expect("review declares provider");
        assert!(
            provider.one_of.iter().any(|v| v == ALT_PROVIDER),
            "{:?}",
            provider.one_of
        );
        std::fs::remove_dir_all(&root).ok();
    }

    // --- the review affordance and its "review with…" menu ------------------

    /// Two backends on ONE model (`coder` — the configured review tier — and
    /// `alt` both serve `same:1b`), a third on its own that the bare
    /// `urn:llm:ask` facade routes to.
    const INVENTORY: &str = r#"{
        "default": "gp",
        "models": {
            "coder": {"backend": "urn:llm:coder:ask", "model": "same:1b"},
            "alt":   {"backend": "urn:llm:alt:ask",   "model": "same:1b"},
            "gp":    {"backend": "urn:llm:gp:ask",    "model": "big:70b"}
        }
    }"#;

    /// A fake `urn:llm:models` over a literal inventory, COUNTING resolves —
    /// the counter is how these tests observe that rendering a file costs no
    /// inventory read and opening its menu costs exactly one.
    fn fake_models_space(body: &str, calls: &Arc<AtomicU32>) -> EndpointSpace {
        let text = body.to_string();
        let counter = Arc::clone(calls);
        EndpointSpace::new().bind(
            Exact::new("urn:llm:models"),
            FnEndpoint::new("fake-llm-models", move |_inv: &Invocation<'_>| {
                counter.fetch_add(1, Ordering::Relaxed);
                Ok(repr("application/json", text.clone()))
            })
            .with_description(Description::new("fake-llm-models").verb(Verb::Source)),
        )
    }

    /// A counted backend at an arbitrary provider IRI — the menu may offer any
    /// selectable one, and the "everything offered is accepted" test actually
    /// clicks them.
    fn fake_llm_at(iri: &str, log: &Arc<Log>, reply: &str) -> EndpointSpace {
        let log = Arc::clone(log);
        let reply = reply.to_string();
        EndpointSpace::new().bind(
            Exact::new(iri),
            FnEndpoint::new("fake-any-llm", move |inv: &Invocation<'_>| {
                log.asks.lock().unwrap().push((
                    inv.inline_str("prompt").unwrap_or("").to_string(),
                    inv.inline_str("system").unwrap_or("").to_string(),
                    inv.inline_str("max_tokens").unwrap_or("").to_string(),
                ));
                Ok(repr_utf8("text/plain", reply.clone()))
            })
            .with_description(
                Description::new("fake-any-llm")
                    .verb(Verb::Source)
                    .requires(CAP_NET),
            ),
        )
    }

    /// Browse, the inventory, and every backend the inventory names.
    fn kernel_with_menu(
        root: &std::path::Path,
        store: &Arc<Store>,
        log: &Arc<Log>,
        calls: &Arc<AtomicU32>,
        config: impl FnOnce(ExplainConfig) -> ExplainConfig,
    ) -> Kernel {
        let cfg = config(ExplainConfig::new(Arc::clone(store)));
        let browse = crate::space_with_explain(vec![("demo".to_string(), root.to_path_buf())], cfg);
        Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(fake_models_space(INVENTORY, calls)),
            Arc::new(fake_llm_at(PROVIDER, log, TWO_FINDINGS)),
            Arc::new(fake_llm_at(ALT_PROVIDER, log, ALT_FINDINGS)),
            Arc::new(fake_llm_at("urn:llm:ask", log, TWO_FINDINGS)),
        ])))
    }

    fn html_face(kernel: &Kernel, iri: &str) -> String {
        body(&issue(kernel, Verb::Source, iri, &[("as", "text/html")], &cap()).unwrap())
    }

    /// Every `provider=` a menu emits, in order.
    fn offered_providers(html: &str) -> Vec<String> {
        html.match_indices("provider=")
            .map(|(i, _)| {
                html[i + "provider=".len()..]
                    .split(['"', ' '])
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    /// ★ THE COST DISCIPLINE, which is the good part of the explain menu and
    /// is preserved here: a file view pays NOTHING for the menu, and opening
    /// it pays one inventory read — never a probe per backend, and never an
    /// inference call. Opening a menu must not cost what the menu exists to
    /// let you decide about.
    #[test]
    fn opening_the_review_menu_costs_one_inventory_read_and_no_model_call() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_menu(&root, &store, &log, &calls, |c| {
            c.allow_provider(ALT_PROVIDER)
        });

        // Rendering the file: the button and the closed disclosure, and not a
        // single sub-request for either.
        let page = html_face(&k, "urn:repo:demo:file:a.rs");
        assert_eq!(page.matches("browse-review-link").count(), 1, "{page}");
        assert_eq!(page.matches("browse-review-menu\"").count(), 1, "{page}");
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "a file view must not fan out"
        );
        assert_eq!(log.count(), 0);

        // Opening it: one inventory read, no ask.
        let menu = html_face(&k, "urn:repo:demo:review-options:a.rs");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(log.count(), 0, "a menu never derives");
        assert!(menu.contains("same:1b"), "{menu}");

        // And a second open is a second read, not a fan-out: the count tracks
        // opens, so the assertion above is about the menu and not about a
        // cache that happens to be warm.
        html_face(&k, "urn:repo:demo:review-options:a.rs");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(log.count(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ ONE ROW PER MODEL, because the review archive keys on the model: two
    /// backends serving one model key ONE pass, so a second row would offer a
    /// review it cannot produce and its no-op would read as a bug.
    #[test]
    fn two_backends_serving_one_model_are_one_review_row() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_menu(&root, &store, &log, &calls, |c| {
            c.allow_provider(ALT_PROVIDER)
        });

        let menu = html_face(&k, "urn:repo:demo:review-options:a.rs");
        // {coder, alt} serve same:1b; the bare facade serves big:70b. Three
        // selectable providers, TWO rows.
        assert_eq!(menu.matches("same:1b").count(), 1, "{menu}");
        let offered = offered_providers(&menu);
        assert_eq!(offered.len(), 2, "{menu}");
        // The backends are named as a fact beside the row, never offered as a
        // second button.
        assert!(menu.contains("served by coder, alt"), "{menu}");
        // The row's button names the CONFIGURED review tier, not the
        // alphabetically first of the pair: which backend answers cannot
        // change the archive key, but it does decide which machine spends the
        // time on a miss.
        assert!(offered.contains(&PROVIDER.to_string()), "{menu}");
        assert!(!offered.contains(&ALT_PROVIDER.to_string()), "{menu}");
        // The row a plain `review` click already takes is marked as such.
        assert!(menu.contains("default for review"), "{menu}");
        // And the panel says why there is one row, in the markup itself.
        assert!(
            menu.contains("One row per model, not per backend"),
            "{menu}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The menu is the host's allowlist, never a hard-coded list — so it can
    /// never render a click that comes back `Denied`, and never omits one the
    /// operator allowed.
    #[test]
    fn the_menu_offers_exactly_what_review_accepts_and_never_more() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_menu(&root, &store, &log, &calls, |c| {
            c.allow_provider(ALT_PROVIDER)
        });

        let menu = html_face(&k, "urn:repo:demo:review-options:a.rs");
        for provider in offered_providers(&menu) {
            assert!(
                issue(
                    &k,
                    Verb::Source,
                    "urn:repo:demo:review:a.rs",
                    &[("provider", &provider)],
                    &cap(),
                )
                .is_ok(),
                "the menu offered `{provider}`, which review refused"
            );
        }
        // A backend the inventory names and the operator did NOT allow is
        // absent — the manifold's one_of and the menu are the same set.
        assert!(!menu.contains("urn:llm:gp:ask"), "{menu}");
        assert!(!menu.contains("big:70b\n"), "{menu}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Reading what is on offer is not spending, so the menu asks for the
    /// browse grant alone. It could not have lived on `browse-review`, which
    /// DECLARES net and annotate because it spends and mints — a browse-only
    /// session would have been refused its own menu.
    #[test]
    fn the_menu_needs_neither_a_net_nor_an_annotate_grant() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_menu(&root, &store, &log, &calls, |c| c);
        let browse_only = Capability::scoped(["urn:cap:browse:read:demo"]);

        let menu = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:review-options:a.rs",
                &[("as", "text/html")],
                &browse_only,
            )
            .expect("a browse grant reads what is on offer"),
        );
        assert!(menu.contains("browse-review-menu-panel"), "{menu}");
        // The pass itself stays refused under the same capability — the menu
        // shows the door, it does not open it.
        let denied = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:review:a.rs",
            &[],
            &browse_only,
        );
        assert!(matches!(denied, Err(Error::Denied(_))), "{denied:?}");
        assert_eq!(log.count(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ THE INVARIANT: the button is a CALLER, not a second implementation.
    ///
    /// Everything the affordance emits names `urn:repo:{repo}:review:{path}`
    /// and adds nothing but a FACE (and, from a menu row, the `provider=` the
    /// manifold already declares). So a git-event trigger firing the same IRI
    /// with a different cause lands on the same archive key and serves the
    /// same minted annotations — which is what the second half asserts: the
    /// button's exact call derives, and the trigger's exact call is a HIT on
    /// it, same tag, same minted set.
    #[test]
    fn the_button_and_a_trigger_are_one_call_with_two_causes() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_menu(&root, &store, &log, &calls, |c| {
            c.allow_provider(ALT_PROVIDER)
        });

        // What the markup asks for, read off the page rather than from the
        // helper that wrote it.
        let page = html_face(&k, "urn:repo:demo:file:a.rs");
        assert!(
            page.contains(
                "hx-get=\"/k/source urn:repo:demo:review:a.rs as=text/html\" \
                 hx-target=\"#browse\""
            ),
            "the plain button must add nothing but the face: {page}"
        );
        let menu = html_face(&k, "urn:repo:demo:review-options:a.rs");
        assert!(
            menu.contains(&format!(
                "hx-get=\"/k/source urn:repo:demo:review:a.rs as=text/html provider={PROVIDER}\""
            )),
            "a menu row must add nothing but the face and provider=: {menu}"
        );
        // Nothing else is reachable from either: no second annotation path, no
        // prompt of the UI's own.
        assert!(!page.contains("urn:iki:annotation:mint"), "{page}");

        // The button's call, verbatim.
        let clicked = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:review:a.rs",
            &[("as", "text/html")],
            &cap(),
        )
        .unwrap();
        assert!(body(&clicked).contains("review by"), "{}", body(&clicked));
        assert_eq!(log.count(), 1, "the click derived");

        // A trigger's call, verbatim — same IRI, no provider, a machine's
        // face. It is an archive HIT on what the click derived: same tag, and
        // the same minted annotations, not a second pass.
        let triggered = json(&k, "urn:repo:demo:review:a.rs", &[]);
        assert_eq!(triggered["derived"], false, "a trigger must not re-derive");
        // The tag folds `urn:llm:{p}:model`, which this fixture does not bind,
        // so it falls back to the provider heuristic — the documented
        // asymmetry between what a MENU can learn (the cheap inventory) and
        // what a TAG resolves (the per-provider identity). What matters here
        // is that both causes land on the ONE tag, whichever it is.
        assert_eq!(triggered["version_tag"], "review-v3@coder");
        assert_eq!(log.count(), 1, "the trigger paid nothing");
        assert_eq!(
            triggered["minted"].as_array().unwrap().len(),
            2,
            "the trigger serves the click's findings"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The menu renders on a host with no llm module bound at all: the
    /// configured tier is still offered, labelled by the same provider
    /// heuristic its version tag falls back to.
    #[test]
    fn the_review_menu_renders_with_no_inventory_bound() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let browse = crate::space_with_explain(
            vec![("demo".to_string(), root.clone())],
            ExplainConfig::new(Arc::clone(&store)),
        );
        let k = Kernel::new(Arc::new(browse));

        let menu = html_face(&k, "urn:repo:demo:review-options:a.rs");
        let offered = offered_providers(&menu);
        assert!(offered.contains(&PROVIDER.to_string()), "{menu}");
        assert!(menu.contains(">coder</button>"), "{menu}");
        assert!(menu.contains("backend reports no model id"), "{menu}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A directory has no review affordance, because it has no review: the
    /// pass is file-grain (findings anchor in text), so a tree-level button
    /// would be an affordance whose only possible answer is a refusal.
    #[test]
    fn a_directory_offers_no_review_and_the_pass_refuses_one() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let calls = Arc::new(AtomicU32::new(0));
        let k = kernel_with_menu(&root, &store, &log, &calls, |c| c);

        let tree = html_face(&k, "urn:repo:demo:tree");
        assert!(!tree.contains("browse-review-link"), "{tree}");
        assert!(!tree.contains("browse-review-menu"), "{tree}");
        // And the resource agrees, for the reason the markup encodes.
        let refused = issue(&k, Verb::Source, "urn:repo:demo:review:", &[], &cap());
        assert!(refused.is_err(), "{refused:?}");
        std::fs::remove_dir_all(&root).ok();
    }
}
