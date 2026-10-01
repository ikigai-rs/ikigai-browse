//! The one-shot that moves a pre-0.3.0 store's annotations from
//! `urn:annotation:` to `urn:iki:annotation:`.
//!
//! ## ★ This was a STOPGAP for a missing primitive, and the primitive LANDED
//!
//! This module was written saying "when `urn:sparql:update` exists, this is a
//! query rather than a program". **It exists.** `ikigai-sparql` binds
//! `urn:sparql:update` (a `Sink` under `urn:cap:sparql:update`, one
//! transaction, all-or-nothing) over the host's shared store, and
//! `ikigai-store` binds `urn:iki:store:update` / `urn:iki:store:graph-update`
//! over the persistent one — the dev server and gonk respectively. So the
//! in-place moves below, [`plan`](crate::migrate::plan) and
//! [`plan_into_graph`](crate::migrate::plan_into_graph), ARE now one
//! `DELETE … INSERT … WHERE` against a host that binds a writing verb over the
//! browse store, and the binary that runs them is a convenience rather than a
//! necessity. Retiring it is a real option, not a hypothetical one.
//!
//! What the writing verb does NOT give, and why
//! [`plan_transfer`](crate::migrate::plan_transfer) below is
//! still a program:
//!
//! - **One endpoint writes ONE store.** The cross-store move reads a store one
//!   host owns and writes a store another host owns; there is no single
//!   transaction spanning both, and no verb that takes a source.
//! - **The root rename is not safely expressible as SPARQL string surgery.**
//!   It renames one path segment in four IRI positions AND one literal, and it
//!   has to assign each annotation's selector children to the root named only
//!   on their parent. `IRI(CONCAT(…))` over `STR(?s)` can approximate the first
//!   half and cannot express the second.
//! - **The refusals are the point.** An undecided root, an unassignable
//!   subject, a rename collision and a dangling root reference are what stop a
//!   silent half-migration, and an UPDATE has nowhere to put them.
//!
//! Said plainly because the note this replaces was load-bearing for a whole
//! class of decision, and it went stale without anything going red.
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
//! selectors are gone. [`Scope::SubjectOnly`](crate::migrate::Scope::SubjectOnly) exists so the test suite can
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
//! See [`Counts::passed`](crate::migrate::Counts::passed).

use std::collections::{BTreeMap, BTreeSet};

use ikigai_core::{Error, Result};
use oxigraph::model::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
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

/// Every IRI prefix browse mints a **subject** under.
///
/// The graph move below selects on this and nothing else, which is complete
/// because browse never writes a quad about a subject someone else minted:
/// annotations and their two selector children are `urn:iki:annotation:…`
/// (`urn:annotation:…` before 0.3.0, which is why the legacy prefix is here
/// too — the two migrations must not strand each other's rows), and the
/// explanation archive and the review passes are `urn:ikigai:browse:…`.
/// `crate::archive` states the same fact from the writing side, and
/// `every_quad_browse_writes_has_a_browse_minted_subject` pins it against the
/// real writers rather than against either comment.
pub const BROWSE_SUBJECT_PREFIXES: [&str; 3] = [OLD_PREFIX, NEW_PREFIX, "urn:ikigai:browse:"];

/// Whether `quad` is one browse wrote — see [`BROWSE_SUBJECT_PREFIXES`].
fn browse_owned(quad: &Quad) -> bool {
    match &quad.subject {
        NamedOrBlankNode::NamedNode(n) => BROWSE_SUBJECT_PREFIXES
            .iter()
            .any(|p| n.as_str().starts_with(p)),
        _ => false,
    }
}

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
fn rewrite(quad: &Quad, scope: Scope, into: Option<&GraphName>) -> Option<Quad> {
    let mut moved_any = false;

    // The graph move, when one is asked for: a browse-owned quad that is not
    // already in the target graph. Computed first so it composes with the
    // namespace rewrite below — a pre-0.3.0 store opting into a graph moves
    // both in ONE transaction rather than in two runs whose order would then
    // matter.
    let into = into.filter(|g| browse_owned(quad) && quad.graph_name != **g);
    if into.is_some() {
        moved_any = true;
    }

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
                into.cloned().unwrap_or_else(|| quad.graph_name.clone()),
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

    // The target graph wins when there is one: it is the destination, not a
    // term to be namespace-rewritten.
    let graph_name = match (into, &quad.graph_name) {
        (Some(g), _) => g.clone(),
        (None, GraphName::NamedNode(n)) => match moved(n) {
            Some(m) => {
                moved_any = true;
                GraphName::NamedNode(m)
            }
            None => quad.graph_name.clone(),
        },
        (None, other) => other.clone(),
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
    plan_with(store, Scope::AllIriPositions, None)
}

/// [`plan`] under an explicit [`Scope`]. Callers other than the ablation test
/// want [`plan`].
pub fn plan_with_scope(store: &Store, scope: Scope) -> Result<Plan> {
    plan_with(store, scope, None)
}

/// [`plan`] that ALSO moves every browse-owned quad into `into` — the second
/// half of `Mount::graph`, for a store that already holds data.
///
/// Existing archives are entirely in the default graph (browse had no other
/// option before 0.4.0), so a host that names a graph must move its quads or
/// the mount reads an empty archive: the data is still there, and browse is
/// confined to a graph it is not in. That failure is SILENT in exactly the way
/// the namespace rename was — empty panels, empty folds, no error anywhere —
/// which is why the fix ships with the knob rather than after it.
///
/// One pass, one transaction, both moves. The namespace rewrite still runs, so
/// a pre-0.3.0 store opting into a graph is migrated once instead of twice in
/// an order nothing would enforce.
pub fn plan_into_graph(store: &Store, into: &GraphName) -> Result<Plan> {
    plan_with(store, Scope::AllIriPositions, Some(into))
}

/// The one planner: a full scan, one predicate, both moves. [`plan`],
/// [`plan_with_scope`] and [`plan_into_graph`] are the three spellings callers
/// actually want.
pub fn plan_with(store: &Store, scope: Scope, into: Option<&GraphName>) -> Result<Plan> {
    let mut out = Plan::default();
    for quad in store.iter() {
        let quad = quad.map_err(store_err)?;
        if let Some(rewritten) = rewrite(&quad, scope, into) {
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
#[non_exhaustive]
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
    /// **Every quad in the store.** A migration creates and destroys nothing,
    /// so this must be IDENTICAL before and after.
    ///
    /// The four columns above describe one particular transform; this one is
    /// true of every transform this module can express, including the ones it
    /// does not have a column for. It is what catches the failure a rewrite
    /// can produce without touching any of the others: two distinct quads
    /// whose rewritten twins are EQUAL collapse into one on insert, and the
    /// store is quietly one quad smaller with every other number unmoved.
    pub total: u64,
    /// Browse-owned quads (see [`BROWSE_SUBJECT_PREFIXES`]) that are **not**
    /// in the graph the migration targets. Must reach 0.
    ///
    /// `None` when no graph migration is in view — the namespace move alone
    /// has no target graph, and a 0 there would claim a check that was never
    /// run. [`Counts::passed`] treats `None` on both sides as "not asked".
    pub outside_target_graph: Option<u64>,
}

/// Count the four. Both prefix counts are per-QUAD (a quad whose subject and
/// object both move counts once), and they are taken over every IRI position
/// rather than the harness's subject-or-object: a superset that is equal on
/// real data, since no predicate or graph name has ever carried the
/// annotation prefix, and a genuine residue check if one ever did.
pub fn counts(store: &Store) -> Result<Counts> {
    counts_for_graph(store, None)
}

/// [`counts`] with [`Counts::outside_target_graph`] measured against `into` —
/// what a run that names a graph reports. `None` is exactly [`counts`].
pub fn counts_for_graph(store: &Store, into: Option<&GraphName>) -> Result<Counts> {
    let mut annotations: BTreeSet<String> = BTreeSet::new();
    let mut out = Counts::default();
    let mut outside = 0u64;
    for quad in store.iter() {
        let quad = quad.map_err(store_err)?;
        out.total += 1;
        if let Some(g) = into {
            if browse_owned(&quad) && quad.graph_name != *g {
                outside += 1;
            }
        }

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
    out.outside_target_graph = into.map(|_| outside);
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
    ///
    /// Two more clauses, added in 0.4.0 with the graph move: **total equal**
    /// (a migration creates and destroys nothing, and this is the clause that
    /// holds for a transform these columns do not otherwise describe), and
    /// **outside the target graph → 0** when a graph was named at all. A run
    /// with no graph in view reports `None` on both sides and the clause is
    /// vacuous — never a silent pass for a check that was not run, because
    /// `None` on ONE side is a disagreement and fails.
    pub fn passed(before: &Counts, after: &Counts) -> bool {
        let graph_ok = matches!(
            (before.outside_target_graph, after.outside_target_graph),
            (None, None) | (Some(_), Some(0))
        );
        after.annotations == before.annotations
            && after.old_prefix == 0
            && after.new_prefix == before.old_prefix + before.new_prefix
            && after.dangling_selectors == 0
            && after.total == before.total
            && graph_ok
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
    out.push_str(&row("total quads", before.total, after.total));
    // Printed only when a graph was named: an "outside target graph  0  0" row
    // on a namespace-only run would read as a check that passed.
    if let (Some(b), Some(a)) = (before.outside_target_graph, after.outside_target_graph) {
        out.push_str(&row("outside target graph", b, a));
    }
    out
}

// --- operational preconditions ----------------------------------------------
//
// Feature-free on purpose: these touch the filesystem and `ps`, never RocksDB,
// so they compile and are reachable in the default build even though only the
// gated binaries call them. Two binaries with two copies of a lock refusal is
// how one of them stops matching the other.

/// Refuse a path that is not an existing Oxigraph/RocksDB store.
///
/// ⚠ `Store::open` CREATES a store at a path that has none. A typo would
/// otherwise produce a brand-new empty store and a serene all-zero PASS — the
/// exact shape of a successful migration, reported over data that was never
/// touched. Require the RocksDB marker instead.
pub fn require_store_dir(path: &std::path::Path) -> std::result::Result<(), String> {
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    if !path.join("CURRENT").exists() {
        return Err(format!(
            "{} does not look like an Oxigraph/RocksDB store (no CURRENT file). \
             Refusing rather than creating an empty one.",
            path.display()
        ));
    }
    Ok(())
}

/// Who holds a store's RocksDB lock, if anyone.
///
/// A locked store is the *expected* failure for a migration: somebody in a hurry
/// runs one against a live deployment, and "IO error: lock hold by current
/// process" is not an answer they can act on. Ask the operating system which
/// process has the LOCK file open and name it. Best-effort by design — no
/// `lsof`, or a platform where this does not work, degrades to letting
/// `Store::open` produce its own error rather than blocking the migration.
pub fn lock_holder(path: &std::path::Path) -> Option<String> {
    let lock = path.join("LOCK");
    if !lock.exists() {
        return None;
    }
    let out = std::process::Command::new("lsof")
        .args(["-t", "--"])
        .arg(&lock)
        .output()
        .ok()?;
    let pids: Vec<&str> = std::str::from_utf8(&out.stdout)
        .ok()?
        .split_whitespace()
        .collect();
    if pids.is_empty() {
        return None;
    }
    let described: Vec<String> = pids
        .iter()
        .map(|pid| {
            match std::process::Command::new("ps")
                .args(["-o", "command=", "-p", pid])
                .output()
            {
                Ok(o) => {
                    let command = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if command.is_empty() {
                        format!("pid {pid}")
                    } else {
                        format!("pid {pid} ({command})")
                    }
                }
                Err(_) => format!("pid {pid}"),
            }
        })
        .collect();
    Some(described.join(", "))
}

// --- the cross-store root move ----------------------------------------------
//
// The second migration this module carries, and a different shape from the one
// above: the namespace and graph moves rewrite a store IN PLACE, this one reads
// a SOURCE store and writes rewritten quads into a DIFFERENT one. It exists
// because two hosts named the same repositories differently — a dev server that
// took its root names from the path basename (`ikigai-core`) and a host that
// names them explicitly (`core`) — and an archive is only worth moving if the
// names in it are the names the new host asks with.

/// `urn:repo:{root}` — the join spine. An explanation's `ik:about`, an
/// annotation's `ik:annotates` and a review pass's `prov:used` all point here.
pub const REPO_PREFIX: &str = "urn:repo:";
/// An archived explanation's subject: `urn:ikigai:browse:explain:{root}:{hash}:{tag}:{path}`.
pub const EXPLAIN_PREFIX: &str = "urn:ikigai:browse:explain:";
/// A review pass's subject: `urn:ikigai:browse:review:{root}:{hash}:{tag}:{path}`.
pub const REVIEW_PREFIX: &str = "urn:ikigai:browse:review:";
/// A review pass's region memo: `urn:ikigai:browse:review-region:{root}:{region-hash}:{tag}:{path}`
/// (0.8.0). A sibling of the pass prefix, not a segment under it — see
/// `review::REGION_PREFIX` for why — so it needs its own row here.
pub const REGION_PREFIX: &str = crate::review::REGION_PREFIX;

/// Every prefix under which the NEXT path segment is a root name.
///
/// ⚠ `urn:ikigai:browse:review:` is here because it occurs as an OBJECT as well
/// as a subject: an annotation minted by a review pass carries
/// `prov:wasGeneratedBy <urn:ikigai:browse:review:{root}:…>` — 14 of them on
/// plasma. A rewrite that walked only subjects and `urn:repo:` objects would
/// leave every one of those aimed at a pass IRI that no longer exists. It is the
/// same argument the namespace move makes for `oa:hasSelector`, one family
/// further out, and it is why this list is the authority rather than a list of
/// predicates: the position is what carries the name, not the property.
/// `urn:ikigai:browse:review-region:` is an object too: a pass carries
/// `prov:used <region memo>` for every region it carried forward, and a memo
/// carries `prov:wasGeneratedBy <pass>`.
pub const ROOT_BEARING_PREFIXES: [&str; 4] =
    [REPO_PREFIX, EXPLAIN_PREFIX, REVIEW_PREFIX, REGION_PREFIX];

/// `ik:repo` — the root name as a LITERAL, on explanations, review passes and
/// annotations alike. Not addressable, so no resolution test can catch it being
/// wrong; it is what the JSON and HTML faces print.
const IK_REPO: &str = "https://ikigai-rs.dev/ns#repo";
const IK_EXPLANATION: &str = "https://ikigai-rs.dev/ns#Explanation";
const IK_REVIEW: &str = "https://ikigai-rs.dev/ns#Review";
const RDF_TYPE_IRI: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// What a source root's quads should become.
///
/// There is deliberately no "default" behaviour. A root the operator has said
/// nothing about is [`RootDecision::Unmapped`], which is not an outcome — it is
/// a question the run has not answered, and [`Transfer::unmapped`] is what the
/// binary refuses on. Carrying an unmapped root silently would put an archive in
/// the target under a name no root of the target produces: present, countable,
/// SPARQL-visible, and unreachable by every actual read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RootDecision {
    /// Carry this root's quads, renaming to the given name. The identity rename
    /// (`--root folio=folio`) is how an operator says "carry it verbatim" —
    /// deliberately the same gesture as any other mapping, so that carrying a
    /// root the target cannot serve is always something someone TYPED.
    Rename(String),
    /// Leave this root's quads in the source. Counted and reported, never
    /// silent.
    Drop,
    /// The operator has not decided. Never carried, never dropped: refused.
    #[default]
    Unmapped,
}

/// The source-root → target-root decisions for one run.
#[derive(Clone, Debug, Default)]
pub struct RootMap(BTreeMap<String, RootDecision>);

impl RootMap {
    /// An empty map — every root [`RootDecision::Unmapped`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Carry `from`'s quads as `to`. `from == to` is the identity rename.
    #[must_use]
    pub fn rename(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.0.insert(from.into(), RootDecision::Rename(to.into()));
        self
    }

    /// Leave `from`'s quads behind.
    #[must_use]
    pub fn dropped(mut self, from: impl Into<String>) -> Self {
        self.0.insert(from.into(), RootDecision::Drop);
        self
    }

    /// What was decided for `root`.
    pub fn decide(&self, root: &str) -> RootDecision {
        self.0.get(root).cloned().unwrap_or_default()
    }

    /// Whether anything was decided at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Which of the THREE root-bearing positions the rewrite touches.
///
/// Production always uses [`RootScope::All`]. The two below it are ablations
/// kept compiled for the same reason [`Scope::SubjectOnly`] is: each one names a
/// read that breaks, and the suite produces that break on purpose rather than
/// trusting a comment that it would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootScope {
    /// Subject IRI, object IRI and the `ik:repo` literal. The correct rewrite,
    /// and the only one the binary offers.
    All,
    /// ⚠ **ABLATION ONLY.** Subject IRIs alone. `urn:repo:{root}:explain:{path}`
    /// then HITS the archive — the subject is the key — while
    /// `urn:repo:{root}:explain-versions:{path}` returns nothing, because the
    /// listing joins on `ik:about`, an object.
    SubjectOnly,
    /// ⚠ **ABLATION ONLY.** Both IRI positions, leaving `ik:repo` holding the
    /// old name. Every read succeeds and every face prints a repository that
    /// does not exist on this host: the position no resolution test can catch.
    IrisOnly,
}

/// Split a root-bearing IRI into `(prefix, root, rest)`.
///
/// `rest` keeps its leading colon, or is empty — `urn:repo:folio:tree` has root
/// `folio` and rest `:tree`, and `urn:repo:folio` has the same root and no rest.
/// Reassembly is concatenation, so it can neither lose a delimiter nor invent
/// one.
fn split_root(iri: &str) -> Option<(&'static str, &str, &str)> {
    for prefix in ROOT_BEARING_PREFIXES {
        let Some(rest) = iri.strip_prefix(prefix) else {
            continue;
        };
        let (root, tail) = match rest.find(':') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if root.is_empty() {
            return None;
        }
        return Some((prefix, root, tail));
    }
    None
}

/// The annotation id a browse-minted annotation subject belongs to — the
/// annotation node and both of its `:selector:` children answer the same id.
///
/// Annotations are the one browse family whose subject carries NO root: the IRI
/// is a uuid. Their root is discoverable only from the `ik:repo` literal on the
/// annotation node, which is why the planner needs a first pass before it can
/// decide anything about a selector quad.
fn annotation_cluster(iri: &str) -> Option<&str> {
    let rest = iri
        .strip_prefix(NEW_PREFIX)
        .or_else(|| iri.strip_prefix(OLD_PREFIX))?;
    Some(match rest.find(':') {
        Some(i) => &rest[..i],
        None => rest,
    })
}

/// `iri` with its root renamed, or `None` when nothing about it moves (not
/// root-bearing, not carried, or renamed to the same name).
///
/// `new_unchecked` is sound for the same reason [`moved`]'s is: the input parsed
/// as an IRI, and the edit replaces one path segment with another the binary has
/// already refused unless it is a bare root name.
fn renamed_iri(iri: &str, map: &RootMap) -> Option<NamedNode> {
    let (prefix, root, rest) = split_root(iri)?;
    let RootDecision::Rename(to) = map.decide(root) else {
        return None;
    };
    if to == root {
        return None;
    }
    Some(NamedNode::new_unchecked(format!("{prefix}{to}{rest}")))
}

/// Per-root arithmetic for one run — one line each in the dry run's table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RootTally {
    /// What the operator asked for this root.
    pub decision: RootDecision,
    /// Browse-owned quads in the source belonging to this root.
    pub quads: u64,
    /// How many of those would land in the target.
    pub carried: u64,
    /// How many of the carried ones actually CHANGED. A root renamed to itself
    /// carries everything and rewrites nothing, and the two columns disagreeing
    /// is how that shows on the table rather than in a comment.
    pub rewritten: u64,
    /// `ik:Explanation` subjects.
    pub explanations: u64,
    /// `ik:Review` subjects.
    pub reviews: u64,
    /// `oa:Annotation` subjects.
    pub annotations: u64,
}

/// The cross-store plan: rewritten quads, and everything the operator has to
/// look at before letting them land. Building it reads both stores and writes
/// neither.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Transfer {
    /// The rewritten quads, already carrying the target graph name.
    pub insert: Vec<Quad>,
    /// Every root the source MENTIONS — as an owner or only as a reference —
    /// with what happens to it.
    pub roots: BTreeMap<String, RootTally>,
    /// Browse-owned subjects no root could be assigned to: an annotation with no
    /// `ik:repo`, or a subject shape this module does not know. Neither carried
    /// nor dropped, because "I do not know whose this is" is not a thing to
    /// decide silently.
    pub unassigned: BTreeSet<String>,
    /// Quads that WOULD be carried but reference a root that is not — the
    /// dangling-reference check for this transform, and the direct analogue of
    /// [`Counts::dangling_selectors`]. Must be empty.
    pub dangling_root_refs: Vec<Quad>,
}

impl Transfer {
    /// Roots the operator said nothing about. The binary refuses on this.
    pub fn unmapped(&self) -> Vec<&str> {
        self.roots
            .iter()
            .filter(|(_, t)| t.decision == RootDecision::Unmapped)
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Quads that would land.
    pub fn carried(&self) -> u64 {
        self.roots.values().map(|t| t.carried).sum()
    }

    /// Quads deliberately left in the source.
    pub fn dropped(&self) -> u64 {
        self.roots
            .values()
            .filter(|t| t.decision == RootDecision::Drop)
            .map(|t| t.quads)
            .sum()
    }
}

/// Which root a browse-minted subject's quads belong to.
fn owner_root(subject: &str, annotation_roots: &BTreeMap<String, String>) -> Option<String> {
    if let Some((prefix, root, _)) = split_root(subject) {
        // `urn:repo:` is never a browse-minted SUBJECT; only the archive
        // families are. Guarding on the prefix keeps a subject shape added later
        // from being silently mis-assigned instead of reported as unassigned.
        if prefix == EXPLAIN_PREFIX || prefix == REVIEW_PREFIX || prefix == REGION_PREFIX {
            return Some(root.to_string());
        }
    }
    let id = annotation_cluster(subject)?;
    annotation_roots.get(id).cloned()
}

/// The root each annotation belongs to, read from its `ik:repo` literal.
fn annotation_roots(source: &Store) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for quad in source.iter() {
        let quad = quad.map_err(store_err)?;
        if quad.predicate.as_str() != IK_REPO {
            continue;
        }
        let NamedOrBlankNode::NamedNode(subject) = &quad.subject else {
            continue;
        };
        let Some(id) = annotation_cluster(subject.as_str()) else {
            continue;
        };
        if let Term::Literal(value) = &quad.object {
            out.insert(id.to_string(), value.value().to_string());
        }
    }
    Ok(out)
}

/// Plan the move of every browse-owned quad in `source` into `into` in some
/// OTHER store, renaming roots by `map`.
///
/// ★ **All three positions or nothing.** The subject IRI keys the archive, the
/// object IRI is what `explain-versions` joins on, and the `ik:repo` literal is
/// what the faces print. Each is read by a different thing, so leaving one behind
/// produces a store that passes whichever check you happened to write and fails
/// the one you did not. [`RootScope`]'s two ablations exist so the suite can show
/// each of those failures rather than assert this paragraph.
pub fn plan_transfer(source: &Store, map: &RootMap, into: &GraphName) -> Result<Transfer> {
    plan_transfer_with(source, map, into, RootScope::All)
}

/// [`plan_transfer`] under an explicit [`RootScope`]. Callers other than the
/// ablation tests want [`plan_transfer`].
pub fn plan_transfer_with(
    source: &Store,
    map: &RootMap,
    into: &GraphName,
    scope: RootScope,
) -> Result<Transfer> {
    let annotations = annotation_roots(source)?;
    let mut out = Transfer::default();

    for quad in source.iter() {
        let quad = quad.map_err(store_err)?;
        if !browse_owned(&quad) {
            continue;
        }
        let subject = match &quad.subject {
            NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
            other => other.to_string(),
        };
        let Some(owner) = owner_root(&subject, &annotations) else {
            out.unassigned.insert(subject);
            continue;
        };

        // Every root this quad so much as mentions gets a row, so a root that
        // owns nothing and is only POINTED AT still has to be decided.
        for root in mentioned_roots(&quad) {
            out.roots.entry(root.clone()).or_default().decision = map.decide(&root);
        }

        let decision = map.decide(&owner);
        {
            let tally = out.roots.entry(owner.clone()).or_default();
            tally.decision = decision.clone();
            tally.quads += 1;
            if quad.predicate.as_str() == RDF_TYPE_IRI {
                if let Term::NamedNode(class) = &quad.object {
                    match class.as_str() {
                        IK_EXPLANATION => tally.explanations += 1,
                        IK_REVIEW => tally.reviews += 1,
                        OA_ANNOTATION => tally.annotations += 1,
                        _ => {}
                    }
                }
            }
        }
        if !matches!(decision, RootDecision::Rename(_)) {
            continue;
        }

        // The dangling check runs on the SOURCE quad, where the names are still
        // the ones `map` is keyed on.
        if mentioned_roots(&quad)
            .iter()
            .any(|root| !matches!(map.decide(root), RootDecision::Rename(_)))
        {
            out.dangling_root_refs.push(quad.clone());
        }

        let rewritten = rewrite_roots(&quad, map, scope, into);
        let changed = rewritten.subject != quad.subject || rewritten.object != quad.object;
        {
            let tally = out.roots.entry(owner).or_default();
            tally.carried += 1;
            if changed {
                tally.rewritten += 1;
            }
        }
        out.insert.push(rewritten);
    }
    Ok(out)
}

/// Every root name a quad carries, in any of the three positions.
fn mentioned_roots(quad: &Quad) -> Vec<String> {
    let mut out: Vec<String> = iri_positions(quad)
        .into_iter()
        .filter_map(|iri| split_root(iri).map(|(_, root, _)| root.to_string()))
        .collect();
    if quad.predicate.as_str() == IK_REPO {
        if let Term::Literal(value) = &quad.object {
            out.push(value.value().to_string());
        }
    }
    out
}

/// One quad, roots renamed in every position `scope` allows, in `into`.
fn rewrite_roots(quad: &Quad, map: &RootMap, scope: RootScope, into: &GraphName) -> Quad {
    let subject = match &quad.subject {
        NamedOrBlankNode::NamedNode(n) => match renamed_iri(n.as_str(), map) {
            Some(m) => NamedOrBlankNode::NamedNode(m),
            None => quad.subject.clone(),
        },
        other => other.clone(),
    };
    if scope == RootScope::SubjectOnly {
        return Quad::new(
            subject,
            quad.predicate.clone(),
            quad.object.clone(),
            into.clone(),
        );
    }

    let object = match &quad.object {
        Term::NamedNode(n) => match renamed_iri(n.as_str(), map) {
            Some(m) => Term::NamedNode(m),
            None => quad.object.clone(),
        },
        // ★ The `ik:repo` literal — a root name that is DATA, not a reference.
        // `renamed_iri` cannot reach it (it takes IRIs), so the ONE place this
        // module edits a literal is here, gated on the predicate. Every other
        // literal is untouchable by construction, which is what keeps an
        // `oa:exact` that quotes a root name from being rewritten — the same
        // structural safety the namespace move relies on.
        Term::Literal(value) if scope == RootScope::All && quad.predicate.as_str() == IK_REPO => {
            match map.decide(value.value()) {
                RootDecision::Rename(to) => Term::Literal(Literal::new_simple_literal(to)),
                _ => quad.object.clone(),
            }
        }
        other => other.clone(),
    };

    Quad::new(subject, quad.predicate.clone(), object, into.clone())
}

/// Target-side arithmetic: what `transfer.insert` actually ADDS to a store that
/// may already hold some of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Landing {
    /// Quads the plan would insert.
    pub rewritten: u64,
    /// How many of those are DISTINCT. RDF is a set: two source quads whose
    /// rewritten twins are equal land as one.
    pub distinct: u64,
    /// `rewritten - distinct` — quads a rename collapsed together. Must be 0:
    /// two roots renamed to the same name can key two different archives to one
    /// IRI, and nothing else in this table would move.
    pub collapsed: u64,
    /// Distinct quads the target already holds. A re-run lands nothing, which is
    /// what makes this tool idempotent.
    pub already_present: u64,
    /// `distinct - already_present`: the growth the target graph must show.
    pub new: u64,
}

/// Measure a plan against the store it would land in.
pub fn landing(target: &Store, transfer: &Transfer) -> Result<Landing> {
    let mut distinct: BTreeMap<String, &Quad> = BTreeMap::new();
    for quad in &transfer.insert {
        distinct.insert(quad.to_string(), quad);
    }
    let mut already = 0u64;
    for quad in distinct.values() {
        if target.contains(quad.as_ref()).map_err(store_err)? {
            already += 1;
        }
    }
    let rewritten = transfer.insert.len() as u64;
    let distinct_n = distinct.len() as u64;
    Ok(Landing {
        rewritten,
        distinct: distinct_n,
        collapsed: rewritten - distinct_n,
        already_present: already,
        new: distinct_n - already,
    })
}

/// How many quads a store holds in ONE graph — the before/after column of a
/// cross-store run, since the target's other tenants (a ledger, a vocabulary)
/// make a whole-store total meaningless here.
pub fn quads_in_graph(store: &Store, graph: &GraphName) -> Result<u64> {
    let mut n = 0u64;
    for quad in store.quads_for_pattern(None, None, None, Some(graph.as_ref())) {
        quad.map_err(store_err)?;
        n += 1;
    }
    Ok(n)
}

/// Apply a transfer to the TARGET store in one transaction.
pub fn apply_transfer(target: &Store, transfer: &Transfer) -> Result<()> {
    if transfer.insert.is_empty() {
        return Ok(());
    }
    let mut tx = target.start_transaction().map_err(store_err)?;
    for quad in &transfer.insert {
        tx.insert(quad.as_ref());
    }
    tx.commit().map_err(store_err)
}

/// The target as it WOULD be — the same projection discipline as [`project`], so
/// a dry run's "would be" column and a commit's "after" column come out of the
/// same counting function rather than out of two implementations that can
/// disagree.
pub fn project_transfer(target: &Store, transfer: &Transfer) -> Result<Store> {
    let projected = Store::new().map_err(store_err)?;
    for quad in target.iter() {
        projected
            .insert(quad.map_err(store_err)?.as_ref())
            .map_err(store_err)?;
    }
    for quad in &transfer.insert {
        projected.insert(quad.as_ref()).map_err(store_err)?;
    }
    Ok(projected)
}

/// PASS for a cross-store root move: **nothing undecided · nothing unassigned ·
/// no dangling root reference · nothing collapsed · the target graph grew by
/// exactly what was new.**
///
/// The last clause is what makes the other four worth printing: it ties the
/// plan's arithmetic to a count taken from the store itself, so a plan that was
/// right about what it WOULD do and wrong about what it DID cannot pass.
pub fn transfer_passed(transfer: &Transfer, land: &Landing, before: u64, after: u64) -> bool {
    transfer.unmapped().is_empty()
        && transfer.unassigned.is_empty()
        && transfer.dangling_root_refs.is_empty()
        && land.collapsed == 0
        && after == before + land.new
}

/// The per-root table — what a dry run prints, and what Brian reads before
/// typing `--commit`.
pub fn transfer_report(transfer: &Transfer) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {:<16} {:<14} {:>6} {:>7} {:>8} {:>5} {:>4} {:>4}\n",
        "source root", "becomes", "quads", "carried", "rewritten", "expl", "rev", "ann"
    ));
    for (name, tally) in &transfer.roots {
        let becomes = match &tally.decision {
            RootDecision::Rename(to) => to.clone(),
            RootDecision::Drop => "(dropped)".to_string(),
            RootDecision::Unmapped => "?? UNMAPPED".to_string(),
        };
        out.push_str(&format!(
            "  {:<16} {:<14} {:>6} {:>7} {:>8} {:>5} {:>4} {:>4}\n",
            name,
            becomes,
            tally.quads,
            tally.carried,
            tally.rewritten,
            tally.explanations,
            tally.reviews,
            tally.annotations,
        ));
    }
    out
}

/// The landing table: the plan's arithmetic beside the target graph's own count.
pub fn landing_report(land: &Landing, before: u64, after: u64, after_label: &str) -> String {
    let row = |name: &str, value: u64| format!("  {name:<24} {value}\n");
    let mut out = String::new();
    out.push_str(&row("quads rewritten", land.rewritten));
    out.push_str(&row("distinct", land.distinct));
    out.push_str(&row("collapsed by rename", land.collapsed));
    out.push_str(&row("already in target", land.already_present));
    out.push_str(&row("new to target", land.new));
    out.push_str(&format!(
        "  {:<24} {:<9} {}\n",
        "target graph", "before", after_label
    ));
    out.push_str(&format!("  {:<24} {:<9} {}\n", "", before, after));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxigraph::model::{GraphNameRef, Literal, NamedNode, Quad};

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

    /// The graph move, over the same fixture the namespace move is proved on —
    /// plus a quad browse did NOT write, sitting in the default graph, which
    /// must still be there afterwards. A migration that swept the default
    /// graph rather than selecting browse's own subjects would take it too,
    /// and on a shared dataset that quad belongs to someone else.
    #[test]
    fn a_graph_move_takes_every_browse_quad_and_nothing_else() {
        let store = plasma_shaped_store();
        let stranger = quad(
            "urn:iki:ledger:item:7",
            &format!("{IK}title"),
            Term::Literal(Literal::new_simple_literal("not browse's")),
        );
        store.insert(stranger.as_ref()).unwrap();

        let graph = GraphName::NamedNode(iri("urn:iki:graph:browse"));
        let before = counts_for_graph(&store, Some(&graph)).unwrap();
        assert_eq!(
            before.outside_target_graph,
            Some(211),
            "14 annotations x 15 quads, plus the explanation-archive entry"
        );
        assert_eq!(before.total, 212, "211 browse quads and one stranger");

        let plan = plan_into_graph(&store, &graph).unwrap();
        assert_eq!(plan.len(), 211, "the stranger is not in the plan");
        apply(&store, &plan).unwrap();

        let after = counts_for_graph(&store, Some(&graph)).unwrap();
        assert!(
            Counts::passed(&before, &after),
            "{}",
            report(&before, &after, "after")
        );
        assert_eq!(after.outside_target_graph, Some(0));
        assert_eq!(after.total, before.total, "not one quad lost");

        // The stranger did not move.
        assert_eq!(
            store
                .quads_for_pattern(None, None, None, Some(GraphNameRef::DefaultGraph))
                .count(),
            1
        );
        assert!(store.contains(stranger.as_ref()).unwrap());
    }

    /// One pass, both moves: a store that never ran the namespace migration
    /// and now opts into a graph is migrated ONCE, in one transaction. The
    /// alternative — two runs — has an order, and nothing would enforce it.
    #[test]
    fn a_graph_move_carries_the_namespace_move_with_it() {
        let store = plasma_shaped_store();
        let graph = GraphName::NamedNode(iri("urn:iki:graph:browse"));
        let before = counts_for_graph(&store, Some(&graph)).unwrap();
        assert_eq!(before.old_prefix, 210, "the fixture is pre-0.3.0");

        apply(&store, &plan_into_graph(&store, &graph).unwrap()).unwrap();

        let after = counts_for_graph(&store, Some(&graph)).unwrap();
        assert!(
            Counts::passed(&before, &after),
            "{}",
            report(&before, &after, "after")
        );
        assert_eq!(after.old_prefix, 0);
        assert_eq!(after.new_prefix, 210);
        assert_eq!(after.dangling_selectors, 0);
        assert_eq!(after.outside_target_graph, Some(0));
    }

    /// A second run is a no-op, like the namespace move's: the selection is by
    /// "not already there", and after one pass nothing matches.
    #[test]
    fn a_graph_move_is_idempotent() {
        let store = plasma_shaped_store();
        let graph = GraphName::NamedNode(iri("urn:iki:graph:browse"));
        apply(&store, &plan_into_graph(&store, &graph).unwrap()).unwrap();
        let settled = counts_for_graph(&store, Some(&graph)).unwrap();

        assert!(plan_into_graph(&store, &graph).unwrap().is_empty());
        assert_eq!(counts_for_graph(&store, Some(&graph)).unwrap(), settled);
    }

    /// The dry run and the commit agree — the same property the namespace move
    /// has, and for the same reason: the "would be" column is `counts` over
    /// `project`, which is the function that produces the "after" column.
    #[test]
    fn the_projection_of_a_graph_move_equals_the_committed_store() {
        let store = plasma_shaped_store();
        let graph = GraphName::NamedNode(iri("urn:iki:graph:browse"));
        let plan = plan_into_graph(&store, &graph).unwrap();
        let projected = project(&store, &plan).unwrap();
        let would_be = counts_for_graph(&projected, Some(&graph)).unwrap();

        apply(&store, &plan).unwrap();
        assert_eq!(counts_for_graph(&store, Some(&graph)).unwrap(), would_be);
    }

    #[test]
    fn the_report_names_every_count_it_was_given() {
        let before = Counts {
            annotations: 14,
            old_prefix: 224,
            new_prefix: 0,
            dangling_selectors: 28,
            total: 500,
            outside_target_graph: None,
        };
        let after = Counts {
            annotations: 14,
            old_prefix: 0,
            new_prefix: 224,
            dangling_selectors: 0,
            total: 500,
            outside_target_graph: None,
        };
        let text = report(&before, &after, "after");
        for row in [
            "oa:Annotation subjects",
            "old-prefix quads",
            "new-prefix quads",
            "dangling hasSelector",
            "total quads",
        ] {
            assert!(text.contains(row), "{text}");
        }
        assert!(text.contains("224"), "{text}");
        // The graph row is printed only when a graph was named — otherwise a
        // `0  0` row would read as a check that ran and passed.
        assert!(!text.contains("outside target graph"), "{text}");

        let before = Counts {
            outside_target_graph: Some(224),
            ..before
        };
        let after = Counts {
            outside_target_graph: Some(0),
            ..after
        };
        let text = report(&before, &after, "after");
        assert!(text.contains("outside target graph"), "{text}");
    }

    #[test]
    fn a_lost_quad_fails_even_when_every_other_column_agrees() {
        // The failure `total` exists for: nothing about the four namespace
        // columns notices a store that came back one quad smaller.
        let before = Counts {
            annotations: 14,
            old_prefix: 224,
            new_prefix: 0,
            dangling_selectors: 28,
            total: 500,
            outside_target_graph: None,
        };
        let honest = Counts {
            old_prefix: 0,
            new_prefix: 224,
            dangling_selectors: 0,
            ..before
        };
        assert!(Counts::passed(&before, &honest));
        assert!(!Counts::passed(
            &before,
            &Counts {
                total: 499,
                ..honest
            }
        ));
    }

    #[test]
    fn a_graph_check_that_was_not_run_cannot_pass_as_zero() {
        let before = Counts {
            annotations: 1,
            old_prefix: 0,
            new_prefix: 4,
            dangling_selectors: 0,
            total: 10,
            outside_target_graph: Some(4),
        };
        // Asked for, and done.
        assert!(Counts::passed(
            &before,
            &Counts {
                outside_target_graph: Some(0),
                ..before
            }
        ));
        // Asked for, and not done.
        assert!(!Counts::passed(
            &before,
            &Counts {
                outside_target_graph: Some(1),
                ..before
            }
        ));
        // Asked for BEFORE and not measured after: a disagreement, not a pass.
        assert!(!Counts::passed(
            &before,
            &Counts {
                outside_target_graph: None,
                ..before
            }
        ));
    }
}
// --- the cross-store root move: tests ---------------------------------------

/// ★ **These are READS, not counts.**
///
/// A count proves a copy happened. Only a resolution proves the rewrite was
/// right, because the failure this migration can produce is SILENT: the quads
/// land, SPARQL finds them, and every actual read misses — a read builds its IRI
/// from the TARGET host's root name and gets no hit. So the suite mounts browse
/// over the migrated store under the target's root name and asks it the two
/// questions a user asks, with a fake LLM counting derivations: an archive hit
/// costs no ask, and a miss is visible as one.
///
/// Each of the three root-bearing positions gets a test that BREAKS it and names
/// the read that notices. That is what makes "all three or nothing" a claim the
/// compiler checks instead of a paragraph.
#[cfg(test)]
mod root_move_tests {
    use super::*;
    use crate::{repr_utf8, ExplainConfig, Mount};
    use futures::executor::block_on;
    use ikigai_core::{
        ArgRef, Capability, Description, EndpointSpace, Exact, Fallback, FnEndpoint, Invocation,
        Iri, Kernel, Representation, Request, Verb,
    };
    use oxigraph::model::Literal;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// The dev server's name for the root. The whole point of this migration is
    /// that it is not the target's.
    const DEV_ROOT: &str = "ikigai-core";
    /// gonk's name for the same directory.
    const GONK_ROOT: &str = "core";
    /// gonk's browse graph. The dev server's quads are in the DEFAULT graph
    /// (pre-0.4.0 shape); this is where they have to land.
    const TARGET_GRAPH: &str = "urn:iki:browse:graph:default";

    const FILE_PROVIDER: &str = "urn:llm:coder:ask";
    const DIR_PROVIDER: &str = "urn:llm:ask";

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-rootmove-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "fn main() {}\n").unwrap();
        dir
    }

    /// A fake LLM whose every answer is `EXPL#{n}` and whose ask count is the
    /// observable: a resolution that costs no ask was served from the archive.
    fn fake_llm(asks: &Arc<AtomicUsize>) -> EndpointSpace {
        let mut space = EndpointSpace::new();
        for provider in [FILE_PROVIDER, DIR_PROVIDER] {
            let asks = Arc::clone(asks);
            space = space.bind(
                Exact::new(provider),
                FnEndpoint::new("fake-llm", move |_: &Invocation<'_>| {
                    let n = asks.fetch_add(1, Ordering::Relaxed) + 1;
                    Ok(repr_utf8("text/plain", format!("EXPL#{n}")))
                })
                .with_description(
                    Description::new("fake-llm")
                        .verb(Verb::Source)
                        .requires("urn:cap:net:*"),
                ),
            );
        }
        space
    }

    /// A browse kernel over `root`, mounted under `name`, archiving into
    /// `store` — and into `graph` when one is named (a host that names none
    /// writes the default graph, which is the dev server's shape).
    fn kernel(
        name: &str,
        root: &Path,
        store: &Arc<Store>,
        graph: Option<&str>,
        asks: &Arc<AtomicUsize>,
    ) -> Kernel {
        let config = ExplainConfig::new(Arc::clone(store))
            .file_model_label("m1")
            .dir_model_label("d1");
        let mut mount = Mount::new([(name.to_string(), root.to_path_buf())]);
        if let Some(graph) = graph {
            mount = mount.graph(NamedNode::new(graph).unwrap());
        }
        Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(mount.explain(config).space()),
            Arc::new(fake_llm(asks)),
        ])))
    }

    fn cap(root: &str) -> Capability {
        Capability::scoped([
            format!("urn:cap:browse:read:{root}"),
            "urn:cap:net:localhost".to_string(),
        ])
    }

    fn read(kernel: &Kernel, iri: &str, root: &str, args: &[(&str, &str)]) -> Result<String> {
        let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
        for (k, v) in args {
            request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
        }
        let repr: Representation = block_on(kernel.issue(request, &cap(root)))?;
        Ok(String::from_utf8_lossy(&repr.bytes).into_owned())
    }

    fn json(kernel: &Kernel, iri: &str, root: &str) -> serde_json::Value {
        serde_json::from_str(&read(kernel, iri, root, &[("as", "application/json")]).unwrap())
            .unwrap()
    }

    /// The dev server, reproduced: a repo whose root is named `ikigai-core`, an
    /// archive in the DEFAULT graph, one derived explanation. Returns the repo
    /// directory, the store, and the ask counter (which reads `1`).
    fn dev_server_with_one_explanation() -> (PathBuf, Arc<Store>, Arc<AtomicUsize>) {
        let root = temp_dir();
        let store = Arc::new(Store::new().unwrap());
        let asks = Arc::new(AtomicUsize::new(0));
        let k = kernel(DEV_ROOT, &root, &store, None, &asks);
        let first = read(&k, "urn:repo:ikigai-core:explain:src/lib.rs", DEV_ROOT, &[]).unwrap();
        assert_eq!(first.trim(), "EXPL#1");
        assert_eq!(asks.load(Ordering::Relaxed), 1);
        (root, store, asks)
    }

    fn target_graph() -> GraphName {
        GraphName::NamedNode(NamedNode::new(TARGET_GRAPH).unwrap())
    }

    /// Migrate `source` into a fresh target store under `scope`, returning the
    /// target and the plan.
    fn migrated(source: &Store, map: &RootMap, scope: RootScope) -> (Arc<Store>, Transfer) {
        let target = Arc::new(Store::new().unwrap());
        let transfer = plan_transfer_with(source, map, &target_graph(), scope).unwrap();
        apply_transfer(&target, &transfer).unwrap();
        (target, transfer)
    }

    fn dev_to_gonk() -> RootMap {
        RootMap::new().rename(DEV_ROOT, GONK_ROOT)
    }

    // --- acceptance: the two reads --------------------------------------------

    /// ACCEPTANCE 1 and 2, in one resolution each: after the move, the target
    /// host's own IRI serves the migrated explanation **without inference**, and
    /// `explain-versions` lists it.
    #[test]
    fn the_migrated_archive_answers_the_target_hosts_own_iri_without_deriving() {
        let (root, source, asks) = dev_server_with_one_explanation();
        let (target, _) = migrated(&source, &dev_to_gonk(), RootScope::All);

        let k = kernel(GONK_ROOT, &root, &target, Some(TARGET_GRAPH), &asks);

        // (1) The explanation itself — the text that was paid for once.
        let hit = json(&k, "urn:repo:core:explain:src/lib.rs", GONK_ROOT);
        assert_eq!(hit["text"], "EXPL#1");
        assert_eq!(
            hit["derived"], false,
            "a cache MISS would derive a fresh explanation and look exactly like success"
        );
        assert_eq!(
            asks.load(Ordering::Relaxed),
            1,
            "the archive hit must cost no inference"
        );
        // The join spine, rewritten: gonk's own root produces this IRI.
        assert_eq!(hit["about"], "urn:repo:core:file:src/lib.rs");

        // (2) The version listing — a different read, joining on ik:about.
        let versions = json(&k, "urn:repo:core:explain-versions:src/lib.rs", GONK_ROOT);
        let rows = versions.as_array().unwrap();
        assert_eq!(rows.len(), 1, "the migrated version must be listed");
        assert_eq!(rows[0]["version_tag"], "code-v1@m1");
        assert_eq!(asks.load(Ordering::Relaxed), 1);

        // (3) The `ik:repo` literal, which only the graph face prints.
        let turtle = read(
            &k,
            "urn:repo:core:explain:src/lib.rs",
            GONK_ROOT,
            &[("as", "text/turtle")],
        )
        .unwrap();
        assert!(
            turtle.contains("ik:repo \"core\""),
            "the stored repo literal must be the TARGET's name: {turtle}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ The failure this tool exists to prevent, produced on purpose: copy the
    /// archive with the names UNCHANGED and every count looks right while the
    /// only read anyone performs misses and pays for inference again.
    #[test]
    fn an_unrenamed_archive_is_present_countable_and_unreachable() {
        let (root, source, asks) = dev_server_with_one_explanation();
        // The identity map: a faithful copy, no rename. Ten quads move.
        let (target, transfer) = migrated(
            &source,
            &RootMap::new().rename(DEV_ROOT, DEV_ROOT),
            RootScope::All,
        );
        // Nine quads: the archive entry's eight properties plus its type. There
        // is no `ik:derivedAt` because this kernel has no clock bound — in
        // production there is one, and the entry is ten.
        assert_eq!(transfer.carried(), 9, "the copy itself succeeded");
        assert_eq!(quads_in_graph(&target, &target_graph()).unwrap(), 9);

        let k = kernel(GONK_ROOT, &root, &target, Some(TARGET_GRAPH), &asks);
        let miss = json(&k, "urn:repo:core:explain:src/lib.rs", GONK_ROOT);
        assert_eq!(
            miss["derived"], true,
            "the archive is right there and the read cannot see it"
        );
        assert_eq!(asks.load(Ordering::Relaxed), 2, "inference paid for twice");
        std::fs::remove_dir_all(&root).ok();
    }

    /// ABLATION — subject only. The archive key is the subject, so the
    /// explanation still HITS; `explain-versions` joins on `ik:about` and
    /// returns nothing. One read right, one read wrong: the shape that makes a
    /// partial rewrite look like a working migration.
    #[test]
    fn rewriting_only_the_subject_hits_the_explanation_and_loses_the_versions() {
        let (root, source, asks) = dev_server_with_one_explanation();
        let (target, _) = migrated(&source, &dev_to_gonk(), RootScope::SubjectOnly);
        let k = kernel(GONK_ROOT, &root, &target, Some(TARGET_GRAPH), &asks);

        let hit = json(&k, "urn:repo:core:explain:src/lib.rs", GONK_ROOT);
        assert_eq!(hit["text"], "EXPL#1");
        assert_eq!(
            hit["derived"], false,
            "the subject rewrite alone still hits"
        );
        assert_eq!(
            hit["about"], "urn:repo:ikigai-core:file:src/lib.rs",
            "and points at an IRI no root of this host produces"
        );

        let versions = json(&k, "urn:repo:core:explain-versions:src/lib.rs", GONK_ROOT);
        assert!(
            versions.as_array().unwrap().is_empty(),
            "the listing joins on ik:about, so it finds nothing: {versions}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// ABLATION — both IRI positions, leaving the `ik:repo` LITERAL. Every read
    /// succeeds. The graph face prints a repository this host does not have, and
    /// no resolution test can see it: the position that needs a test looking at
    /// the data rather than at whether the data answered.
    #[test]
    fn rewriting_only_the_iris_leaves_every_read_working_and_the_repo_literal_lying() {
        let (root, source, asks) = dev_server_with_one_explanation();
        let (target, _) = migrated(&source, &dev_to_gonk(), RootScope::IrisOnly);
        let k = kernel(GONK_ROOT, &root, &target, Some(TARGET_GRAPH), &asks);

        let hit = json(&k, "urn:repo:core:explain:src/lib.rs", GONK_ROOT);
        assert_eq!(hit["derived"], false);
        let versions = json(&k, "urn:repo:core:explain-versions:src/lib.rs", GONK_ROOT);
        assert_eq!(versions.as_array().unwrap().len(), 1);
        assert_eq!(asks.load(Ordering::Relaxed), 1, "both reads hit");

        let turtle = read(
            &k,
            "urn:repo:core:explain:src/lib.rs",
            GONK_ROOT,
            &[("as", "text/turtle")],
        )
        .unwrap();
        assert!(
            turtle.contains("ik:repo \"ikigai-core\""),
            "the literal still names the dev server's root: {turtle}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The target graph is the OTHER half of the move: a dev-server archive is
    /// in the default graph, and a host confined to a named one cannot see it.
    #[test]
    fn every_carried_quad_lands_in_the_target_graph() {
        let (root, source, _) = dev_server_with_one_explanation();
        let (target, transfer) = migrated(&source, &dev_to_gonk(), RootScope::All);
        assert!(transfer
            .insert
            .iter()
            .all(|q| q.graph_name == target_graph()));
        assert_eq!(
            quads_in_graph(&target, &GraphName::DefaultGraph).unwrap(),
            0,
            "nothing may be left in the default graph of the target"
        );
        assert_eq!(
            quads_in_graph(&target, &target_graph()).unwrap(),
            transfer.carried()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A second run lands nothing: the rewritten quads are already there, and
    /// RDF is a set. What makes re-running safe after an interrupted deploy.
    #[test]
    fn a_second_transfer_lands_nothing() {
        let (root, source, _) = dev_server_with_one_explanation();
        let (target, _) = migrated(&source, &dev_to_gonk(), RootScope::All);
        let before = quads_in_graph(&target, &target_graph()).unwrap();

        let again = plan_transfer(&source, &dev_to_gonk(), &target_graph()).unwrap();
        let land = landing(&target, &again).unwrap();
        assert_eq!(land.new, 0);
        assert_eq!(land.already_present, land.distinct);
        apply_transfer(&target, &again).unwrap();
        let after = quads_in_graph(&target, &target_graph()).unwrap();
        assert_eq!(after, before);
        assert!(transfer_passed(&again, &land, before, after));
        std::fs::remove_dir_all(&root).ok();
    }

    // --- the transform, over a hand-built store --------------------------------

    fn iri(s: &str) -> NamedNode {
        NamedNode::new(s).unwrap()
    }

    fn lit(s: &str) -> Term {
        Term::Literal(Literal::new_simple_literal(s))
    }

    /// plasma's shape in miniature: one explanation under `ikigai-core`, one
    /// review pass and one annotation under `ikigai-browse` (with the
    /// `prov:wasGeneratedBy` back-reference the brief's list of three positions
    /// did not mention), and one explanation under `folio`, which has no root on
    /// the target at all.
    fn plasma_shaped_source() -> Store {
        let store = Store::new().unwrap();
        let q = |s: &str, p: &str, o: Term| {
            store
                .insert(Quad::new(iri(s), iri(p), o, GraphName::DefaultGraph).as_ref())
                .unwrap();
        };
        let ik = |t: &str| format!("https://ikigai-rs.dev/ns#{t}");
        let oa = |t: &str| format!("http://www.w3.org/ns/oa#{t}");

        let expl = "urn:ikigai:browse:explain:ikigai-core:sha256:abc:code-v1:src/lib.rs";
        q(expl, RDF_TYPE_IRI, Term::NamedNode(iri(&ik("Explanation"))));
        q(expl, &ik("repo"), lit("ikigai-core"));
        q(expl, &ik("path"), lit("src/lib.rs"));
        q(
            expl,
            &ik("about"),
            Term::NamedNode(iri("urn:repo:ikigai-core:file:src/lib.rs")),
        );
        q(expl, &ik("explanation"), lit("what it does"));

        let pass = "urn:ikigai:browse:review:ikigai-browse:sha256:def:review-v1:pr:11";
        let ann = "urn:iki:annotation:a1";
        q(pass, RDF_TYPE_IRI, Term::NamedNode(iri(&ik("Review"))));
        q(pass, &ik("repo"), lit("ikigai-browse"));
        q(
            pass,
            "http://www.w3.org/ns/prov#used",
            Term::NamedNode(iri("urn:repo:ikigai-browse:pr:11")),
        );
        q(
            pass,
            "http://www.w3.org/ns/prov#generated",
            Term::NamedNode(iri(ann)),
        );

        q(ann, RDF_TYPE_IRI, Term::NamedNode(iri(&oa("Annotation"))));
        q(ann, &ik("repo"), lit("ikigai-browse"));
        q(
            ann,
            &ik("annotates"),
            Term::NamedNode(iri("urn:repo:ikigai-browse:pr:11")),
        );
        // ⚠ The fourth root-bearing position: an object under the REVIEW
        // prefix. 14 of these on plasma.
        q(
            ann,
            "http://www.w3.org/ns/prov#wasGeneratedBy",
            Term::NamedNode(iri(pass)),
        );
        // A selector child: no root anywhere in it, and it must travel with its
        // annotation or be left with it.
        let sel = "urn:iki:annotation:a1:selector:quote";
        q(ann, &oa("hasSelector"), Term::NamedNode(iri(sel)));
        q(
            sel,
            RDF_TYPE_IRI,
            Term::NamedNode(iri(&oa("TextQuoteSelector"))),
        );
        // ★ A quoted line of source that CONTAINS a root name. It is data, and
        // the rewrite must not reach it.
        q(sel, &oa("exact"), lit("let root = \"ikigai-core\";"));

        let folio = "urn:ikigai:browse:explain:folio:sha256:ghi:code-v1:a.ts";
        q(
            folio,
            RDF_TYPE_IRI,
            Term::NamedNode(iri(&ik("Explanation"))),
        );
        q(folio, &ik("repo"), lit("folio"));
        q(
            folio,
            &ik("about"),
            Term::NamedNode(iri("urn:repo:folio:file:a.ts")),
        );

        // A tenant browse does not own: it must not move, whatever happens.
        store
            .insert(
                Quad::new(
                    iri("https://ikigai-rs.dev/ns#Explanation"),
                    iri("http://www.w3.org/2000/01/rdf-schema#label"),
                    lit("Explanation"),
                    GraphName::NamedNode(iri("urn:ikigai:vocab")),
                )
                .as_ref(),
            )
            .unwrap();
        store
    }

    fn full_map() -> RootMap {
        RootMap::new()
            .rename("ikigai-core", "core")
            .rename("ikigai-browse", "browse")
            .dropped("folio")
    }

    #[test]
    fn the_review_back_reference_moves_with_everything_else() {
        let source = plasma_shaped_source();
        let (target, _) = migrated(&source, &full_map(), RootScope::All);
        let generated_by = target
            .quads_for_pattern(
                None,
                Some(iri("http://www.w3.org/ns/prov#wasGeneratedBy").as_ref()),
                None,
                None,
            )
            .map(|q| q.unwrap().object.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            generated_by,
            ["<urn:ikigai:browse:review:browse:sha256:def:review-v1:pr:11>"],
            "an object under the review prefix is the fourth root-bearing position"
        );
    }

    #[test]
    fn a_quoted_root_name_in_a_literal_is_untouched() {
        let source = plasma_shaped_source();
        let (target, _) = migrated(&source, &full_map(), RootScope::All);
        let exact: Vec<String> = target
            .quads_for_pattern(
                None,
                Some(iri("http://www.w3.org/ns/oa#exact").as_ref()),
                None,
                None,
            )
            .map(|q| match q.unwrap().object {
                Term::Literal(l) => l.value().to_string(),
                other => other.to_string(),
            })
            .collect();
        assert_eq!(exact, ["let root = \"ikigai-core\";"]);
    }

    #[test]
    fn a_dropped_root_leaves_its_quads_and_its_references_behind() {
        let source = plasma_shaped_source();
        let (target, transfer) = migrated(&source, &full_map(), RootScope::All);
        assert_eq!(transfer.roots["folio"].decision, RootDecision::Drop);
        assert_eq!(transfer.roots["folio"].quads, 3);
        assert_eq!(transfer.roots["folio"].carried, 0);
        assert_eq!(transfer.dropped(), 3);
        for quad in target.iter() {
            let quad = quad.unwrap();
            assert!(
                !quad.to_string().contains("folio"),
                "a dropped root must leave nothing behind: {quad}"
            );
        }
        assert!(transfer.dangling_root_refs.is_empty());
    }

    #[test]
    fn an_annotations_selector_children_follow_its_repo_literal() {
        let source = plasma_shaped_source();
        // Drop the annotation's root and its selector — which names no root at
        // all — must not be carried on its own.
        let map = RootMap::new()
            .rename("ikigai-core", "core")
            .dropped("ikigai-browse")
            .dropped("folio");
        let (target, transfer) = migrated(&source, &map, RootScope::All);
        assert_eq!(transfer.roots["ikigai-browse"].carried, 0);
        for quad in target.iter() {
            let quad = quad.unwrap();
            assert!(
                !quad.to_string().contains("urn:iki:annotation:"),
                "the selector child travels with its annotation: {quad}"
            );
        }
    }

    #[test]
    fn a_root_nobody_decided_is_refused_rather_than_guessed() {
        let source = plasma_shaped_source();
        let partial = RootMap::new()
            .rename("ikigai-core", "core")
            .rename("ikigai-browse", "browse");
        let transfer = plan_transfer(&source, &partial, &target_graph()).unwrap();
        assert_eq!(transfer.unmapped(), ["folio"]);
        assert_eq!(transfer.roots["folio"].decision, RootDecision::Unmapped);
        assert_eq!(transfer.roots["folio"].carried, 0);
        let land = landing(&Store::new().unwrap(), &transfer).unwrap();
        assert!(
            !transfer_passed(&transfer, &land, 0, land.new),
            "an undecided root is a FAIL even when every other column agrees"
        );
    }

    #[test]
    fn nothing_browse_does_not_own_is_carried() {
        let source = plasma_shaped_source();
        let (target, _) = migrated(&source, &full_map(), RootScope::All);
        assert_eq!(
            quads_in_graph(&target, &GraphName::NamedNode(iri("urn:ikigai:vocab"))).unwrap(),
            0,
            "the vocabulary the dev server loaded into its store is not browse's to move"
        );
    }

    /// Two source roots renamed to the SAME target name can key two different
    /// archives to one IRI. Every per-root count still agrees; only the landing
    /// arithmetic notices.
    #[test]
    fn a_rename_collision_collapses_quads_and_fails() {
        let store = Store::new().unwrap();
        let ik = |t: &str| format!("https://ikigai-rs.dev/ns#{t}");
        for root in ["one", "two"] {
            let subject = format!("urn:ikigai:browse:explain:{root}:sha256:abc:code-v1:a.rs");
            for (p, o) in [
                (ik("path"), lit("a.rs")),
                (ik("explanation"), lit("same text")),
            ] {
                store
                    .insert(Quad::new(iri(&subject), iri(&p), o, GraphName::DefaultGraph).as_ref())
                    .unwrap();
            }
        }
        let map = RootMap::new()
            .rename("one", "merged")
            .rename("two", "merged");
        let transfer = plan_transfer(&store, &map, &target_graph()).unwrap();
        assert_eq!(transfer.carried(), 4);
        let target = Store::new().unwrap();
        let land = landing(&target, &transfer).unwrap();
        assert_eq!(land.collapsed, 2, "two pairs of quads became one pair");
        apply_transfer(&target, &transfer).unwrap();
        let after = quads_in_graph(&target, &target_graph()).unwrap();
        assert_eq!(after, 2);
        assert!(!transfer_passed(&transfer, &land, 0, after));
    }

    #[test]
    fn a_browse_subject_with_no_assignable_root_is_reported_not_moved() {
        let store = Store::new().unwrap();
        // An annotation with no ik:repo: nothing says whose it is.
        store
            .insert(
                Quad::new(
                    iri("urn:iki:annotation:orphan"),
                    iri("http://www.w3.org/ns/oa#bodyValue"),
                    lit("a finding"),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            )
            .unwrap();
        let transfer = plan_transfer(&store, &RootMap::new(), &target_graph()).unwrap();
        assert_eq!(
            transfer.unassigned.iter().collect::<Vec<_>>(),
            ["urn:iki:annotation:orphan"]
        );
        assert!(transfer.insert.is_empty());
        let land = landing(&Store::new().unwrap(), &transfer).unwrap();
        assert!(!transfer_passed(&transfer, &land, 0, 0));
    }

    /// A carried quad pointing at a root that is NOT carried is the silent
    /// failure one family out: it resolves to an IRI the target cannot serve.
    #[test]
    fn a_reference_into_a_dropped_root_is_reported_as_dangling() {
        let store = Store::new().unwrap();
        let ik = |t: &str| format!("https://ikigai-rs.dev/ns#{t}");
        let subject = "urn:ikigai:browse:explain:kept:sha256:abc:code-v1:a.rs";
        store
            .insert(
                Quad::new(
                    iri(subject),
                    iri(&ik("about")),
                    Term::NamedNode(iri("urn:repo:gone:file:a.rs")),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            )
            .unwrap();
        let map = RootMap::new().rename("kept", "kept").dropped("gone");
        let transfer = plan_transfer(&store, &map, &target_graph()).unwrap();
        assert_eq!(transfer.dangling_root_refs.len(), 1);
        let land = landing(&Store::new().unwrap(), &transfer).unwrap();
        assert!(!transfer_passed(&transfer, &land, 0, land.new));
    }

    #[test]
    fn the_per_root_table_names_every_root_and_its_decision() {
        let source = plasma_shaped_source();
        let transfer = plan_transfer(&source, &full_map(), &target_graph()).unwrap();
        let table = transfer_report(&transfer);
        assert!(table.contains("ikigai-core"));
        assert!(table.contains("ikigai-browse"));
        assert!(table.contains("folio"));
        assert!(table.contains("(dropped)"));
        // The identity case reports carried without rewritten, which is the
        // only signal that a root travelled under its old name on purpose.
        let identity = plan_transfer(
            &source,
            &RootMap::new()
                .rename("ikigai-core", "ikigai-core")
                .dropped("ikigai-browse")
                .dropped("folio"),
            &target_graph(),
        )
        .unwrap();
        assert_eq!(identity.roots["ikigai-core"].carried, 5);
        assert_eq!(identity.roots["ikigai-core"].rewritten, 0);
    }

    #[test]
    fn split_root_reads_a_bare_root_and_a_pathed_one_alike() {
        assert_eq!(
            split_root("urn:repo:folio"),
            Some((REPO_PREFIX, "folio", ""))
        );
        assert_eq!(
            split_root("urn:repo:folio:tree"),
            Some((REPO_PREFIX, "folio", ":tree"))
        );
        assert_eq!(
            split_root("urn:ikigai:browse:review:browse:sha:tag:pr:11"),
            Some((REVIEW_PREFIX, "browse", ":sha:tag:pr:11"))
        );
        // The region memo is a SIBLING prefix: `review-` is not `review:`, so
        // it must have its own row or a root move leaves every memo behind.
        assert_eq!(
            split_root("urn:ikigai:browse:review-region:browse:sha256:ab:tag:src%2Flib.rs"),
            Some((REGION_PREFIX, "browse", ":sha256:ab:tag:src%2Flib.rs"))
        );
        assert_eq!(split_root("urn:iki:annotation:abc"), None);
        assert_eq!(split_root("urn:repo:"), None);
    }
}
