//! `urn:repo:{repo}:review:{path}` — the **machine review layer** (S4):
//! region-grain LLM commentary on a file, minted as REAL annotations. The
//! explain family answers "what is this file?"; the review pass answers "what
//! would a careful reviewer say about these lines?" — and its findings live in
//! the same `urn:annotation:` family as human notes, distinguished by
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
use oxigraph::model::{GraphName, Literal, NamedNode, Quad, Term};
use oxigraph::store::Store;

use crate::annotate::{self, Included, CAP_ANNOTATE, PROV};
use crate::explain::{
    ik, iso8601, parse_iri, provider_label, resolve_model, truncate, CAP_NET, IK,
};
use crate::hash::hash_iri;
use crate::{
    crumbs_html, esc, file_iri, granted, iri_encode, path_binding, repo_root, repr, repr_utf8,
    resolve, ttl_str, ExplainConfig, Roots, CAP_WILDCARD,
};

// --- the prompt (versioned; edit ⇒ bump) -------------------------------------

/// Version of the review prompt pair, folded into the archive key. A prompt
/// edit bumps this; earlier passes stay recorded under their old tag.
const REVIEW_PROMPT_VERSION: &str = "review-v1";

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
     finding as exactly two lines and nothing else:\n\
     QUOTE: <a short snippet copied character-for-character from one line of \
     the file - under 80 characters, distinctive enough to occur only once>\n\
     NOTE: <one or two sentences of review commentary on that region>\n\
     Do not number the findings. Do not add headings, preamble, or closing \
     remarks. The QUOTE must appear verbatim in the file or the finding is \
     discarded.";

// --- the archive entry (RDF in the shared store) ------------------------------

/// One archived review pass, skolemized like the explanation archive:
///
/// ```turtle
/// <urn:ikigai:browse:review:{repo}:{hash}:{tag}:{path}> a ik:Review ;
///     ik:repo "demo" ; ik:path "src/lib.rs" ;
///     prov:used <urn:repo:demo:file:src/lib.rs> ;
///     ik:contentHash "sha256:…" ; ik:versionTag "review-v1@qwen3-coder:30b" ;
///     ik:model "qwen3-coder:30b" ;
///     prov:generated <urn:annotation:{id}> , … ;
///     ik:orphanedItems "1"^^xsd:nonNegativeInteger ;
///     ik:derivedAt "2026-08-09T17:00:00.000Z"^^xsd:dateTime .
/// ```
///
/// `ik:Review` and `ik:orphanedItems` are the two terms the vocab does not
/// hold yet (reported up, not added here); every provenance link is standard
/// PROV / DC / OA.
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
    pub(crate) derived_at: Option<String>,
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

pub(crate) fn store_pass(store: &Store, entry: &PassEntry) -> Result<()> {
    use oxigraph::model::vocab::{rdf, xsd};
    let subject = NamedNode::new(&entry.iri).map_err(store_err)?;
    let target = NamedNode::new(&entry.target_iri).map_err(store_err)?;
    let g = GraphName::DefaultGraph;
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
        store.insert(quad).map_err(store_err)?;
    }
    Ok(())
}

/// Load one archived pass by its key IRI — `None` on a miss (no
/// `ik:versionTag` under that subject).
pub(crate) fn load_pass(store: &Store, iri: &str) -> Result<Option<PassEntry>> {
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
        derived_at: None,
    };
    let mut found = false;
    for quad in store.quads_for_pattern(Some(subject.as_ref().into()), None, None, None) {
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
    let mut note = String::new();
    let mut flush = |quote: &mut Option<String>, note: &mut String, malformed: &mut u64| {
        match quote.take() {
            Some(q) if !q.is_empty() && !note.trim().is_empty() => findings.push(Finding {
                quote: q,
                note: note.trim().to_string(),
            }),
            Some(_) => *malformed += 1,
            None => {}
        }
        note.clear();
    };
    for line in answer.lines() {
        let trimmed = line.trim();
        if let Some(q) = trimmed.strip_prefix("QUOTE:") {
            flush(&mut quote, &mut note, &mut malformed);
            quote = Some(q.trim().to_string());
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
    flush(&mut quote, &mut note, &mut malformed);
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
    crate::bind_family(space, roots, review, None, Some("review:{path}"))
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
        let model = match &config.review_model_label {
            Some(label) => label.clone(),
            None => resolve_model(inv, &config.review_provider)
                .await
                .unwrap_or_else(|| provider_label(&config.review_provider)),
        };
        let tag = format!("{REVIEW_PROMPT_VERSION}@{model}");

        let iri = pass_iri(repo, &rel, &hash, &tag);
        if let Some(entry) = load_pass(&config.store, &iri)? {
            // The hit path: mints NOTHING. The recorded annotations are
            // drift-reconciled against the very content in hand.
            let included = annotate::included_for_ids(&config.store, &entry.minted, &text)?;
            return face(inv, repo, &rel, &entry, false, &included);
        }

        // Miss: derive one pass. Ask, parse, anchor, mint, archive.
        let prompt = format!(
            "{REVIEW_PROMPT}\n\nRepository: {repo}\nPath: {rel}\n\n```\n{}\n```",
            truncate(&text, config.max_prompt_bytes),
        );
        let request = Request::new(Verb::Source, parse_iri(&config.review_provider)?)
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
        let (findings, malformed) = parse_findings(&answer);
        if findings.is_empty() {
            // Nothing parseable at all IS a failure — erroring (and archiving
            // nothing) keeps the key re-derivable instead of poisoning it
            // with an empty pass.
            return Err(Error::Endpoint(format!(
                "browse: `{}` returned no parseable QUOTE:/NOTE: findings for `{rel}` \
                 (max_tokens {} — thinking models may need a higher ceiling); nothing archived",
                config.review_provider, config.review_max_tokens
            )));
        }

        let created = inv.now().map(|t| iso8601(t.as_millis()));
        let mut minted = Vec::new();
        let mut orphaned_items = malformed;
        for finding in &findings {
            match annotate::mint_review_annotation(
                &config.store,
                &file_iri(repo, &rel),
                repo,
                &rel,
                &text,
                &hash,
                &finding.quote,
                &finding.note,
                &model,
                &iri,
                created.clone(),
                annotate::Surface::File,
            )? {
                Some(annotation_iri) => minted.push(annotation_iri),
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
            derived_at: created,
        };
        store_pass(&config.store, &entry)?;
        let included = annotate::included_for_ids(&config.store, &entry.minted, &text)?;
        face(inv, repo, &rel, &entry, true, &included)
    }

    fn name(&self) -> &str {
        "browse-review"
    }

    fn describe(&self) -> Description {
        review_description()
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
fn review_description() -> Description {
    Description::new("browse-review")
        .title("Machine review pass (annotations minted by a model)")
        .summary(
            "A region-grain machine review of one file — urn:repo:{repo}:review:{path}. \
             Source asks the review model for findings (each an exact quote plus a \
             reviewer's note), mints every anchored finding as a real urn:annotation: \
             (provenance: dcterms:creator = the model, oa:motivatedBy oa:assessing, \
             prov:wasGeneratedBy = this pass) and ARCHIVES the pass by (path, \
             content-hash, review-tag) — re-sourcing unchanged content is an archive hit \
             that mints nothing. Changed content is a fresh pass; earlier passes' \
             annotations re-anchor or orphan like human ones. Quotes that do not anchor \
             are counted (orphaned_items), never fatal. text/plain (default) is the \
             margin-notes digest; as=application/json adds {minted, orphaned_items, \
             annotations}; as=text/html the card page; as=text/turtle the pass's \
             provenance graph.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .requires(CAP_NET)
        .requires(CAP_ANNOTATE)
        .input(
            ArgSpec::new("path")
                .binding()
                .summary("file path within the root, percent-encoded"),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("the face to render")
                .one_of(["text/plain", "application/json", "text/html", "text/turtle"])
                .default_value("text/plain"),
        )
        .output("text/plain;charset=utf-8")
        .output("application/json")
        .output("text/html;charset=utf-8")
        .output("text/turtle")
}

// --- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{Capability, Exact, Fallback, FnEndpoint, Iri, Kernel};
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
    const TWO_FINDINGS: &str = "QUOTE: fn alpha() {}\nNOTE: A clear entry point; the naming makes \
         the call order obvious.\nQUOTE: fn beta() {}\nNOTE: Consider a doc comment - the role of \
         this helper is not evident.\n";

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
        assert_eq!(first["version_tag"], "review-v1@r1");
        assert_eq!(first["model"], "r1");
        assert_eq!(first["orphaned_items"], 0);
        assert_eq!(first["minted"].as_array().unwrap().len(), 2);
        assert_eq!(first["annotations"].as_array().unwrap().len(), 2);
        assert_eq!(log.count(), 1);

        // The findings ARE annotations — the one shared listing shows them.
        let listing = json(&k, "urn:repo:demo:annotations:a.rs", &[]);
        assert_eq!(listing.as_array().unwrap().len(), 2);
        assert_eq!(listing[0]["machine"], true);
        assert_eq!(listing[0]["creator"], "r1");
        assert_eq!(listing[0]["motivation"], "assessing");
        assert_eq!(listing[0]["exact"], "fn alpha() {}");
        assert_eq!(listing[0]["line"], 1);

        // Re-source on unchanged content: an archive hit that MINTS NOTHING —
        // no new ask, no new annotations, the same recorded set.
        let second = json(&k, "urn:repo:demo:review:a.rs", &[]);
        assert_eq!(second["derived"], false);
        assert_eq!(second["minted"], first["minted"]);
        assert_eq!(log.count(), 1, "the hit must not re-ask");
        let listing = json(&k, "urn:repo:demo:annotations:a.rs", &[]);
        assert_eq!(listing.as_array().unwrap().len(), 2, "mint-once");

        // The prompt fed the model the file and the format contract.
        let (prompt, system, max_tokens) = log.last();
        assert!(prompt.contains("QUOTE:"), "{prompt}");
        assert!(prompt.contains("fn beta()"), "{prompt}");
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
            "urn:annotation:h1",
            &[
                ("target", "urn:repo:demo:file:a.rs"),
                ("exact", "fn gamma() {}"),
                ("body", "a human margin note"),
            ],
            &cap(),
        )
        .unwrap();
        issue(&k, Verb::Source, "urn:repo:demo:review:a.rs", &[], &cap()).unwrap();

        // JSON: one axis, two kinds, provenance on every row.
        let listing = json(&k, "urn:repo:demo:annotations:a.rs", &[]);
        let rows = listing.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        let machine: Vec<bool> = rows.iter().map(|r| r["machine"] == true).collect();
        assert_eq!(
            machine,
            [true, true, false],
            "reading order: alpha, beta, gamma"
        );
        assert_eq!(rows[2]["motivation"], "commenting");
        assert_eq!(rows[2]["creator"], serde_json::Value::Null);
        assert!(rows[0]["generated_by"]
            .as_str()
            .unwrap()
            .starts_with("urn:ikigai:browse:review:demo:sha256:"));

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

        // Nothing parseable at all: error, nothing archived, retry re-asks.
        let k = kernel_with(&root, &store, &log, "I think this file is nice overall.");
        let err = issue(&k, Verb::Source, "urn:repo:demo:review:a.rs", &[], &cap()).unwrap_err();
        assert!(format!("{err:?}").contains("no parseable"), "{err:?}");
        let err = issue(&k, Verb::Source, "urn:repo:demo:review:a.rs", &[], &cap()).unwrap_err();
        assert!(format!("{err:?}").contains("no parseable"), "{err:?}");
        assert_eq!(log.count(), 2, "an unarchived pass re-derives");
        let listing = json(&k, "urn:repo:demo:annotations:a.rs", &[]);
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
        let listing = json(&k2, "urn:repo:demo:annotations:a.rs", &[]);
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

        // …while the FIRST pass's annotations re-anchor or orphan exactly
        // like human ones — the drift is the review history, kept visible.
        let listing = json(&k, "urn:repo:demo:annotations:a.rs", &[]);
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

    #[test]
    fn review_requires_browse_net_and_annotate() {
        let root = demo_root();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let k = kernel_with(&root, &store, &log, TWO_FINDINGS);

        for missing in [
            // No annotate: the pass writes annotations — denied at baseline.
            Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:net:localhost"]),
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
        assert!(ttl.contains("prov:generated <urn:annotation:"), "{ttl}");
        assert!(ttl.contains("ik:versionTag \"review-v1@r1\""), "{ttl}");
        assert!(
            ttl.contains("ik:orphanedItems \"0\"^^xsd:nonNegativeInteger"),
            "{ttl}"
        );

        // The minted annotations' own turtle carries the standard provenance.
        let ttl = body(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:annotations:a.rs",
                &[("as", "text/turtle")],
                &cap(),
            )
            .unwrap(),
        );
        assert!(ttl.contains("dcterms:creator \"r1\""), "{ttl}");
        assert!(ttl.contains("oa:motivatedBy oa:assessing"), "{ttl}");
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
        for cap in [CAP_WILDCARD, CAP_NET, CAP_ANNOTATE] {
            assert!(
                description.requires.contains(&cap.to_string()),
                "missing {cap}"
            );
        }
        // No `repo` ArgSpec: rows fix the root; the binding is
        // grammar-injected.
        let names: Vec<&str> = description.inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["path", "as"]);
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
}
