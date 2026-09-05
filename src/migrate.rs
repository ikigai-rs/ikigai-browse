//! The one-shot that moves a pre-0.3.0 store's annotations from
//! `urn:annotation:` to `urn:iki:annotation:`.
//!
//! ## ★ This is a STOPGAP standing in for a missing primitive
//!
//! **When `urn:sparql:update` exists, this is a query rather than a program.**
//! The whole operation is one `DELETE { ?s ?p ?o } INSERT { … } WHERE { … }`
//! over a bound store, and the only reason it is a Rust binary is that the
//! ecosystem's SPARQL surface (`urn:sparql:ask` / `construct` / `describe` /
//! `select`) has no writing verb at all. Read this module as a placeholder:
//! the day an UPDATE mechanism lands, delete the binary and keep the counts.
//! Left unsaid, a bespoke migration binary becomes the permanent answer and
//! the next namespace move copies it.
//!
//! ## What went wrong, and why it was silent
//!
//! 0.3.0 renamed the annotation family and `strip_prefix`es the **new** prefix
//! over the **stored** subject IRI (`annotate::list_annotations`). Every
//! pre-0.3.0 row therefore returns `None`, the loop `continue`s, and the
//! annotation goes **invisible with no error** — empty panels, empty
//! `annotations=include` folds, review passes that lost their findings. There
//! is no failure to observe; only absence.
//!
//! ## The operation
//!
//! Replace-subgraph, in one transaction: every quad with an IRI under the old
//! prefix in **any** position is removed, and its rewritten twin inserted.
//!
//! ⚠ **Both positions or nothing.** The object half is not a detail. An
//! annotation's `oa:hasSelector` points at `urn:annotation:{id}:selector:…`
//! and a review pass's `prov:generated` points at `urn:annotation:{id}` — on
//! plasma, 28 and 14 references respectively. A subject-only rewrite leaves
//! every one of them aimed at an IRI that no longer exists, which is a worse
//! store than the one it started from: the annotations are visible and their
//! selectors are gone. [`Scope::SubjectOnly`] exists so the test suite can
//! produce that failure on purpose rather than trusting a comment about it.
//!
//! ## Counts, and what PASS means
//!
//! The four counts are lifted verbatim from the tested export harness
//! (`ikigai-devtools/migrate-annotation-ns.sh`), which proved this transform
//! against plasma's real data before any of it was Rust:
//!
//! | count | meaning |
//! |-------|---------|
//! | `annotations` | distinct `oa:Annotation` subjects — the population that must not change |
//! | `old_prefix` | quads touching the old prefix — must reach 0 |
//! | `new_prefix` | quads touching the new prefix — must reach the old's former count |
//! | `dangling_selectors` | `oa:hasSelector` objects not under the new prefix — must reach 0 |
//!
//! See [`Counts::passed`].

use std::collections::BTreeSet;

use ikigai_core::{Error, Result};
use oxigraph::model::{GraphName, NamedNode, NamedOrBlankNode, Quad, Term};
use oxigraph::store::Store;

/// The pre-0.3.0 annotation namespace.
///
/// The bare minting IRI `urn:annotation` (no trailing colon) is deliberately
/// NOT matched: it is a request-time name the host aliases, never a stored
/// term — every persisted annotation node is `urn:annotation:{id}` or one of
/// its `:selector:` children.
pub const OLD_PREFIX: &str = "urn:annotation:";
/// The 0.3.0 annotation namespace.
pub const NEW_PREFIX: &str = "urn:iki:annotation:";

const OA_HAS_SELECTOR: &str = "http://www.w3.org/ns/oa#hasSelector";
const OA_ANNOTATION: &str = "http://www.w3.org/ns/oa#Annotation";

fn store_err(e: impl std::fmt::Display) -> Error {
    Error::Endpoint(format!("browse: migrate: {e}"))
}

// --- the rewrite ------------------------------------------------------------

/// Which IRI positions the rewrite touches.
///
/// Production always uses [`Scope::AllIriPositions`]; the variant below it is
/// an ablation kept compiled on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Subject, predicate, object and graph name — every position an IRI can
    /// occupy. The correct rewrite, and the only one the binary offers.
    AllIriPositions,
    /// ⚠ **ABLATION ONLY — never use this to migrate anything.** The naive
    /// implementation: rewrite the subject and leave every reference to it
    /// where it was. It is public, and compiled into release builds, because
    /// the test suite migrates a store with it and asserts the 28 dangling
    /// `oa:hasSelector` references that result. A test that has never seen
    /// that failure proves nothing about the case this tool exists for, and a
    /// `#[cfg(test)]` ablation is one refactor away from being deleted as
    /// dead code by someone who does not know why it is there.
    SubjectOnly,
}

/// Rewrite one IRI, or `None` when it is not under the old prefix.
///
/// `new_unchecked` is sound here: the input parsed as an IRI, and the edit
/// only inserts the ASCII `iki:` inside an existing path segment sequence —
/// it can neither introduce a delimiter nor remove one.
fn moved(node: &NamedNode) -> Option<NamedNode> {
    node.as_str()
        .strip_prefix(OLD_PREFIX)
        .map(|rest| NamedNode::new_unchecked(format!("{NEW_PREFIX}{rest}")))
}

/// The rewritten twin of `quad`, or `None` when nothing in it moves.
///
/// ★ **LITERAL SAFETY IS STRUCTURAL, NOT TEXTUAL.** Every branch below
/// pattern-matches a [`NamedNode`]; `Term::Literal` and `Term::BlankNode`
/// have no arm at all, so a literal is not something this function declines
/// to rewrite — it is something the function cannot reach. That matters
/// because `oa:exact` / `oa:prefix` / `oa:suffix` hold **source-code quotes**,
/// and an annotation on a line that mentions `urn:annotation:` stores that
/// string as ordinary data. The shell harness had to approximate this with an
/// angle-bracket match over Turtle text (`s|<urn:annotation:|…|`), which is a
/// lexical proxy for a type distinction and is correct only for as long as
/// the serializer never writes an IRI any other way. Operating on parsed
/// terms removes the question instead of answering it.
fn rewrite(quad: &Quad, scope: Scope) -> Option<Quad> {
    let mut moved_any = false;

    let subject = match &quad.subject {
        NamedOrBlankNode::NamedNode(n) => match moved(n) {
            Some(m) => {
                moved_any = true;
                NamedOrBlankNode::NamedNode(m)
            }
            None => quad.subject.clone(),
        },
        other => other.clone(),
    };

    // The ablation stops here: subject rewritten, every reference to it left
    // pointing at an IRI that is about to stop existing.
    if scope == Scope::SubjectOnly {
        return moved_any.then(|| {
            Quad::new(
                subject,
                quad.predicate.clone(),
                quad.object.clone(),
                quad.graph_name.clone(),
            )
        });
    }

    let predicate = match moved(&quad.predicate) {
        Some(m) => {
            moved_any = true;
            m
        }
        None => quad.predicate.clone(),
    };

    let object = match &quad.object {
        Term::NamedNode(n) => match moved(n) {
            Some(m) => {
                moved_any = true;
                Term::NamedNode(m)
            }
            None => quad.object.clone(),
        },
        other => other.clone(),
    };

    let graph_name = match &quad.graph_name {
        GraphName::NamedNode(n) => match moved(n) {
            Some(m) => {
                moved_any = true;
                GraphName::NamedNode(m)
            }
            None => quad.graph_name.clone(),
        },
        other => other.clone(),
    };

    moved_any.then(|| Quad::new(subject, predicate, object, graph_name))
}

// --- the plan ---------------------------------------------------------------

/// What the migration would remove and insert. Building it reads; nothing in
/// here has touched the store.
#[derive(Debug, Default)]
pub struct Plan {
    /// The quads carrying an old-prefix IRI, exactly as stored.
    pub remove: Vec<Quad>,
    /// Their rewritten twins, index-aligned with [`Plan::remove`].
    pub insert: Vec<Quad>,
}

impl Plan {
    /// Nothing to do — the store is already migrated (or never held the old
    /// names). This is what makes a second run a no-op rather than a second
    /// rewrite: the selection is by the OLD prefix, and after one pass no
    /// quad matches it. `urn:iki:annotation:` does not start with
    /// `urn:annotation:`, so the two namespaces cannot chain.
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty()
    }

    /// How many quads move.
    pub fn len(&self) -> usize {
        self.remove.len()
    }
}

/// Build the migration plan: a full scan, because a prefix match is not an
/// index the store offers (`quads_for_pattern` wants whole terms). A browse
/// store is thousands of quads, so the scan is the simple thing rather than
/// the expensive one.
pub fn plan(store: &Store) -> Result<Plan> {
    plan_with_scope(store, Scope::AllIriPositions)
}

/// [`plan`] under an explicit [`Scope`]. Callers other than the ablation test
/// want [`plan`].
pub fn plan_with_scope(store: &Store, scope: Scope) -> Result<Plan> {
    let mut out = Plan::default();
    for quad in store.iter() {
        let quad = quad.map_err(store_err)?;
        if let Some(rewritten) = rewrite(&quad, scope) {
            out.remove.push(quad);
            out.insert.push(rewritten);
        }
    }
    Ok(out)
}

/// Apply a plan in ONE transaction: the replace-subgraph either lands whole or
/// not at all. A half-applied namespace move is the one outcome worse than
/// not running — annotations under one prefix and their selectors under the
/// other, with no way to tell by looking which half is which.
pub fn apply(store: &Store, plan: &Plan) -> Result<()> {
    if plan.is_empty() {
        return Ok(());
    }
    let mut tx = store.start_transaction().map_err(store_err)?;
    for quad in &plan.remove {
        tx.remove(quad.as_ref());
    }
    for quad in &plan.insert {
        tx.insert(quad.as_ref());
    }
    tx.commit().map_err(store_err)
}

/// The store as it WOULD be, without touching the one on disk — an in-memory
/// Oxigraph holding `(store − plan.remove) ∪ plan.insert`.
///
/// This is what makes the dry run trustworthy: the "after" column of a dry run
/// is produced by [`counts`] over this projection, which is the SAME function
/// that produces the "after" column of a real commit. There is no second
/// implementation of the arithmetic to disagree with the first.
///
/// It costs one copy of the store in memory. That bound is the reason this is
/// a migration tool and not a general facility.
pub fn project(store: &Store, plan: &Plan) -> Result<Store> {
    let removed: BTreeSet<String> = plan.remove.iter().map(|q| q.to_string()).collect();
    let projected = Store::new().map_err(store_err)?;
    for quad in store.iter() {
        let quad = quad.map_err(store_err)?;
        if !removed.contains(&quad.to_string()) {
            projected.insert(quad.as_ref()).map_err(store_err)?;
        }
    }
    for quad in &plan.insert {
        projected.insert(quad.as_ref()).map_err(store_err)?;
    }
    Ok(projected)
}

// --- the counts -------------------------------------------------------------

/// The four numbers the shell harness printed, computed over a store instead
/// of over an exported file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Distinct subjects typed `oa:Annotation`. The population under
    /// migration: it must be IDENTICAL before and after, because a namespace
    /// move creates and destroys nothing.
    pub annotations: u64,
    /// Quads with an old-prefix IRI in any position. Must reach 0.
    pub old_prefix: u64,
    /// Quads with a new-prefix IRI in any position. Must reach what
    /// `old_prefix` was.
    pub new_prefix: u64,
    /// `oa:hasSelector` objects that are not under the new prefix — the
    /// count a subject-only rewrite leaves standing. Must reach 0.
    pub dangling_selectors: u64,
}

/// Count the four. Both prefix counts are per-QUAD (a quad whose subject and
/// object both move counts once), and they are taken over every IRI position
/// rather than the harness's subject-or-object: a superset that is equal on
/// real data, since no predicate or graph name has ever carried the
/// annotation prefix, and a genuine residue check if one ever did.
pub fn counts(store: &Store) -> Result<Counts> {
    let mut annotations: BTreeSet<String> = BTreeSet::new();
    let mut out = Counts::default();
    for quad in store.iter() {
        let quad = quad.map_err(store_err)?;

        if quad.predicate.as_str() == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
            && matches!(&quad.object, Term::NamedNode(n) if n.as_str() == OA_ANNOTATION)
        {
            annotations.insert(quad.subject.to_string());
        }

        let iris = iri_positions(&quad);
        if iris.iter().any(|iri| iri.starts_with(OLD_PREFIX)) {
            out.old_prefix += 1;
        }
        if iris.iter().any(|iri| iri.starts_with(NEW_PREFIX)) {
            out.new_prefix += 1;
        }

        if quad.predicate.as_str() == OA_HAS_SELECTOR {
            let ok =
                matches!(&quad.object, Term::NamedNode(n) if n.as_str().starts_with(NEW_PREFIX));
            if !ok {
                out.dangling_selectors += 1;
            }
        }
    }
    out.annotations = annotations.len() as u64;
    Ok(out)
}

/// Every IRI a quad carries. Literals and blank nodes contribute nothing —
/// the same structural reason [`rewrite`] cannot reach them.
fn iri_positions(quad: &Quad) -> Vec<&str> {
    let mut out = vec![quad.predicate.as_str()];
    if let NamedOrBlankNode::NamedNode(n) = &quad.subject {
        out.push(n.as_str());
    }
    if let Term::NamedNode(n) = &quad.object {
        out.push(n.as_str());
    }
    if let GraphName::NamedNode(n) = &quad.graph_name {
        out.push(n.as_str());
    }
    out
}

impl Counts {
    /// PASS, in the harness's words: **annotations equal · old → 0 · new →
    /// old's former count · dangling → 0.**
    ///
    /// The third clause is written as `before.old + before.new` so that it
    /// also holds on a partially-migrated store (and on the second run of an
    /// already-migrated one, where `before.old` is 0). It would fail if a
    /// single quad ever mixed the two prefixes — no writer mints such a quad,
    /// and if one appeared, a FAIL that says so is the right outcome.
    pub fn passed(before: &Counts, after: &Counts) -> bool {
        after.annotations == before.annotations
            && after.old_prefix == 0
            && after.new_prefix == before.old_prefix + before.new_prefix
            && after.dangling_selectors == 0
    }
}

/// The before/after table, in the harness's layout — its columns are what
/// Brian will be reading in the deploy window, so they are not re-invented.
pub fn report(before: &Counts, after: &Counts, after_label: &str) -> String {
    let row = |name: &str, b: u64, a: u64| format!("  {name:<24} {b:<9} {a}\n");
    let mut out = String::new();
    out.push_str(&format!("  {:<24} {:<9} {}\n", "", "before", after_label));
    out.push_str(&row(
        "oa:Annotation subjects",
        before.annotations,
        after.annotations,
    ));
    out.push_str(&row(
        "old-prefix quads",
        before.old_prefix,
        after.old_prefix,
    ));
    out.push_str(&row(
        "new-prefix quads",
        before.new_prefix,
        after.new_prefix,
    ));
    out.push_str(&row(
        "dangling hasSelector",
        before.dangling_selectors,
        after.dangling_selectors,
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxigraph::model::{Literal, NamedNode, Quad};

    fn iri(s: &str) -> NamedNode {
        NamedNode::new(s).unwrap()
    }

    fn quad(s: &str, p: &str, o: Term) -> Quad {
        Quad::new(iri(s), iri(p), o, GraphName::DefaultGraph)
    }

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const OA: &str = "http://www.w3.org/ns/oa#";
    const IK: &str = "https://ikigai-rs.dev/ns#";
    const PROV_GENERATED: &str = "http://www.w3.org/ns/prov#generated";

    /// plasma's shape, at plasma's scale: 14 machine-review findings under the
    /// OLD prefix, each with two selectors (28 `oa:hasSelector`) and each
    /// referenced by its review pass's `prov:generated` (14 references whose
    /// SUBJECT does not move — the object half, in its pure form).
    ///
    /// One annotation's `oa:exact` quotes a line of source that itself
    /// mentions the old prefix. That is not a contrivance: these are findings
    /// on THIS codebase, and the namespace is what the code talks about.
    fn plasma_shaped_store() -> Store {
        let store = Store::new().unwrap();
        let lit = |s: &str| Term::Literal(Literal::new_simple_literal(s));

        for n in 0..14 {
            let ann = format!("urn:annotation:m{n}");
            let quote = format!("{ann}:selector:quote");
            let position = format!("{ann}:selector:position");
            let pass = "urn:ikigai:browse:review:ikigai-browse:sha256:abc:review-v1:pr:11";

            let exact = if n == 0 {
                // ⚠ a source-code quote that CONTAINS the old prefix.
                r#"let Some(id) = iri.strip_prefix("urn:annotation:") else {"#
            } else {
                "fn list_annotations(store: &Store)"
            };

            for q in [
                quad(
                    &ann,
                    RDF_TYPE,
                    Term::NamedNode(iri(&format!("{OA}Annotation"))),
                ),
                quad(&ann, &format!("{OA}bodyValue"), lit("a finding")),
                quad(
                    &ann,
                    &format!("{IK}annotates"),
                    Term::NamedNode(iri("urn:repo:ikigai-browse:pr:11")),
                ),
                quad(&ann, &format!("{IK}repo"), lit("ikigai-browse")),
                quad(&ann, &format!("{IK}contentHash"), lit("sha256:abc")),
                quad(
                    &ann,
                    &format!("{OA}hasSelector"),
                    Term::NamedNode(iri(&quote)),
                ),
                quad(
                    &ann,
                    &format!("{OA}hasSelector"),
                    Term::NamedNode(iri(&position)),
                ),
                quad(
                    &quote,
                    RDF_TYPE,
                    Term::NamedNode(iri(&format!("{OA}TextQuoteSelector"))),
                ),
                quad(&quote, &format!("{OA}exact"), lit(exact)),
                quad(&quote, &format!("{OA}prefix"), lit("        ")),
                quad(
                    &quote,
                    &format!("{OA}suffix"),
                    lit("\n            continue;"),
                ),
                quad(
                    &position,
                    RDF_TYPE,
                    Term::NamedNode(iri(&format!("{OA}TextPositionSelector"))),
                ),
                quad(&position, &format!("{OA}start"), lit("120")),
                quad(&position, &format!("{OA}end"), lit("135")),
                // The object half in its pure form: the pass IRI stays put,
                // only the reference moves.
                quad(pass, PROV_GENERATED, Term::NamedNode(iri(&ann))),
            ] {
                store.insert(q.as_ref()).unwrap();
            }
        }

        // An explanation-archive entry sharing the store, mentioning neither
        // prefix. It must come through untouched.
        store
            .insert(
                quad(
                    "urn:ikigai:browse:explain:ikigai-browse:sha256:abc:v1:src/lib.rs",
                    &format!("{IK}model"),
                    lit("qwen3-coder:30b"),
                )
                .as_ref(),
            )
            .unwrap();
        store
    }

    fn migrate(store: &Store, scope: Scope) -> Plan {
        let plan = plan_with_scope(store, scope).unwrap();
        apply(store, &plan).unwrap();
        plan
    }

    #[test]
    fn the_four_counts_reach_pass() {
        let store = plasma_shaped_store();
        let before = counts(&store).unwrap();
        assert_eq!(before.annotations, 14);
        assert_eq!(before.dangling_selectors, 28);
        assert_eq!(before.new_prefix, 0);
        assert!(before.old_prefix > 0);

        migrate(&store, Scope::AllIriPositions);
        let after = counts(&store).unwrap();

        assert_eq!(after.annotations, 14);
        assert_eq!(after.old_prefix, 0);
        assert_eq!(after.new_prefix, before.old_prefix);
        assert_eq!(after.dangling_selectors, 0);
        assert!(Counts::passed(&before, &after), "{before:?} -> {after:?}");
    }

    /// ★ THE ABLATION. Rewrite only the subject — the implementation a reading
    /// of "move the annotations" produces — and the dangling count does not
    /// move off 28. This is the failure the tool exists to avoid, and the
    /// reason the object half is not an optimization.
    #[test]
    fn ablating_the_object_rewrite_leaves_28_dangling_selectors() {
        let store = plasma_shaped_store();
        let before = counts(&store).unwrap();
        assert_eq!(before.dangling_selectors, 28);

        migrate(&store, Scope::SubjectOnly);
        let after = counts(&store).unwrap();

        // The annotations moved…
        assert_eq!(after.annotations, 14);
        // …and every reference to them stayed behind.
        assert_eq!(
            after.dangling_selectors, 28,
            "subject-only rewrite must leave all 28 oa:hasSelector references dangling"
        );
        assert_ne!(after.old_prefix, 0, "the selector nodes are still old");
        assert!(
            !Counts::passed(&before, &after),
            "a subject-only migration must NOT report PASS"
        );

        // And the 14 prov:generated references now point at nothing: their
        // objects are old-prefix IRIs with no quads under them.
        let orphaned = store
            .quads_for_pattern(None, Some(iri(PROV_GENERATED).as_ref()), None, None)
            .map(|q| q.unwrap())
            .filter(
                |q| matches!(&q.object, Term::NamedNode(n) if n.as_str().starts_with(OLD_PREFIX)),
            )
            .count();
        assert_eq!(orphaned, 14, "prov:generated references left dangling");
    }

    /// The full rewrite leaves no reference behind: every `oa:hasSelector` and
    /// every `prov:generated` object resolves to a subject that exists.
    #[test]
    fn no_reference_points_at_a_subject_that_does_not_exist() {
        let store = plasma_shaped_store();
        migrate(&store, Scope::AllIriPositions);

        for predicate in [OA_HAS_SELECTOR, PROV_GENERATED] {
            for quad in store.quads_for_pattern(None, Some(iri(predicate).as_ref()), None, None) {
                let object = quad.unwrap().object;
                let Term::NamedNode(node) = object else {
                    panic!("{predicate} object must be an IRI");
                };
                assert!(node.as_str().starts_with(NEW_PREFIX), "{node}");
                let subject: NamedOrBlankNode = node.clone().into();
                assert!(
                    store
                        .quads_for_pattern(Some(subject.as_ref()), None, None, None)
                        .next()
                        .is_some(),
                    "{node} has no quads — the reference dangles"
                );
            }
        }
    }

    /// ⚠ The literal that mentions the old prefix is DATA. It is a quote of
    /// source code, and rewriting it would silently corrupt the annotation's
    /// anchor — the quote would stop matching the file it came from and the
    /// annotation would orphan on the next read.
    #[test]
    fn literals_holding_the_old_prefix_are_untouched() {
        let store = plasma_shaped_store();
        let quoted = r#"let Some(id) = iri.strip_prefix("urn:annotation:") else {"#;

        let exact_literals = |store: &Store| -> Vec<String> {
            store
                .quads_for_pattern(None, Some(iri(&format!("{OA}exact")).as_ref()), None, None)
                .map(|q| match q.unwrap().object {
                    Term::Literal(l) => l.value().to_string(),
                    other => panic!("oa:exact must be a literal, got {other}"),
                })
                .collect()
        };

        assert!(exact_literals(&store).iter().any(|l| l == quoted));
        migrate(&store, Scope::AllIriPositions);
        assert!(
            exact_literals(&store).iter().any(|l| l == quoted),
            "the source-code quote must survive the migration character for character"
        );
        // And nothing anywhere gained the new prefix inside a literal.
        for quad in store.iter() {
            if let Term::Literal(l) = quad.unwrap().object {
                assert!(!l.value().contains(NEW_PREFIX), "{l} was rewritten");
            }
        }
    }

    /// Idempotent: the second run has nothing to plan, and the counts do not
    /// budge. `urn:iki:annotation:` does not start with `urn:annotation:`, so
    /// there is no second rewrite to accidentally perform.
    #[test]
    fn a_second_run_is_a_no_op() {
        let store = plasma_shaped_store();
        migrate(&store, Scope::AllIriPositions);
        let once = counts(&store).unwrap();

        let second = plan(&store).unwrap();
        assert!(second.is_empty(), "{} quads replanned", second.len());
        apply(&store, &second).unwrap();

        assert_eq!(counts(&store).unwrap(), once);
        assert!(
            Counts::passed(&once, &once),
            "an already-migrated store passes"
        );
    }

    /// The rewrite is one-for-one: nothing is dropped, nothing is duplicated,
    /// and the unrelated explanation entry sharing the store is still there.
    #[test]
    fn the_store_keeps_its_size_and_its_other_tenants() {
        let store = plasma_shaped_store();
        let size_before = store.len().unwrap();

        migrate(&store, Scope::AllIriPositions);

        assert_eq!(store.len().unwrap(), size_before);
        assert_eq!(
            store
                .quads_for_pattern(
                    Some(
                        iri("urn:ikigai:browse:explain:ikigai-browse:sha256:abc:v1:src/lib.rs")
                            .as_ref()
                            .into()
                    ),
                    None,
                    None,
                    None,
                )
                .count(),
            1,
            "the explanation archive shares this store and must be untouched"
        );
    }

    /// The dry run is not a separate code path: projecting the plan and
    /// counting the projection gives exactly the numbers a commit produces.
    #[test]
    fn the_dry_run_projection_equals_the_commit() {
        let dry = plasma_shaped_store();
        let wet = plasma_shaped_store();

        let plan = plan(&dry).unwrap();
        let projected = counts(&project(&dry, &plan).unwrap()).unwrap();
        // …and the dry run left the store alone.
        assert_eq!(
            counts(&dry).unwrap(),
            counts(&plasma_shaped_store()).unwrap()
        );

        migrate(&wet, Scope::AllIriPositions);
        assert_eq!(projected, counts(&wet).unwrap());
    }

    /// The same transform against a REAL on-disk RocksDB store, which is the
    /// only kind the tool will ever be pointed at. Gated on the `migrate`
    /// feature because that is what brings `Store::open` in at all; everything
    /// above runs in the default, C++-toolchain-free build.
    #[cfg(feature = "migrate")]
    #[test]
    fn the_transform_survives_a_round_trip_through_rocksdb() {
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-migrate-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        {
            let disk = Store::open(&dir).unwrap();
            for quad in plasma_shaped_store().iter() {
                disk.insert(quad.unwrap().as_ref()).unwrap();
            }
            let before = counts(&disk).unwrap();
            assert_eq!(before.dangling_selectors, 28);
            let plan = plan(&disk).unwrap();
            apply(&disk, &plan).unwrap();
            disk.flush().unwrap();
            assert!(Counts::passed(&before, &counts(&disk).unwrap()));
        }
        // Reopened — the rewrite is on disk, and a second run finds nothing.
        {
            let disk = Store::open(&dir).unwrap();
            assert_eq!(counts(&disk).unwrap().annotations, 14);
            assert_eq!(counts(&disk).unwrap().old_prefix, 0);
            assert!(plan(&disk).unwrap().is_empty());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_report_names_all_four_counts() {
        let before = Counts {
            annotations: 14,
            old_prefix: 224,
            new_prefix: 0,
            dangling_selectors: 28,
        };
        let after = Counts {
            annotations: 14,
            old_prefix: 0,
            new_prefix: 224,
            dangling_selectors: 0,
        };
        let text = report(&before, &after, "after");
        for row in [
            "oa:Annotation subjects",
            "old-prefix quads",
            "new-prefix quads",
            "dangling hasSelector",
        ] {
            assert!(text.contains(row), "{text}");
        }
        assert!(text.contains("224"), "{text}");
    }
}
