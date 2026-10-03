//! Selection — match a need against the self-descriptions in a [`Space`], the same way at
//! two scales:
//!
//! - [`select_transreptor`] — find a chain of transreptors converting a representation from
//!   one **media type** to another, using the `from`/`to` each transreptor declares
//!   ([`EndpointKind::Transreptor`](crate::EndpointKind)). This is what metadata rendering,
//!   content-negotiation, and sniff-and-dispatch build on: "give me a way from media type A
//!   to B." v1 finds a **direct single hop**, else a **two-hop pivot via the canonical RDF
//!   type (`text/turtle`)** — the hub our transreptors share. Through **lossless edges
//!   only** by default: a transreptor that declares itself a projection
//!   ([`Description::lossy`](crate::Description::lossy)) is planned through only under an
//!   explicit [`TransreptionPolicy`] ([`select_transreptor_with`]), and every plan reports
//!   which of its steps are lossy.
//! - [`select_action`] — find endpoints whose required inputs are satisfiable by the **RDF
//!   classes** present in a context: "given these typed entities, what can I do with them?"
//!   (the seed of layer action-inference). Matches on [`ArgSpec::class`](crate::ArgSpec).
//!
//! Both are RDF-free Rust walks over the same `entries → Meta → describe` path the catalog
//! uses (no SPARQL, no oxigraph in core). General N-hop path-finding, a cached selection
//! index, and capability-scoped filtering are later refinements; today each enumerates per
//! call.
//!
//! Only **auto-invocable** transreptors are selected — ones drivable with just a piped
//! `content` and a target `as` (see [`is_auto_invocable`]). A *parameterized* transreptor
//! like `urn:xslt:transform` (which requires a `stylesheet`) is still a transreptor for
//! discovery, but can't be invoked automatically, so it's excluded here.

use std::sync::Arc;

use crate::describe::{Description, InputSource};
use crate::grammar::{Bindings, UriTemplate};
use crate::iri::Iri;
use crate::request::Request;
use crate::space::{Resolution, Resolved, Scope, Space, SpaceEntry};
use crate::verb::Verb;

/// The canonical RDF media type the transreptor graph hubs on — the pivot for two-hop
/// conversions when no direct transreptor exists.
pub const CANONICAL: &str = "text/turtle";

/// One step of a transreption plan: invoke the transreptor at `endpoint` with the input
/// piped as `content` and `as` set to `to`. `#[non_exhaustive]`: a step is read, never
/// authored, outside core.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TransreptionStep {
    /// The transreptor endpoint's IRI.
    pub endpoint: String,
    /// The media type to request from it (its `as`).
    pub to: String,
    /// Whether the transreptor DECLARES this edge lossless
    /// ([`Transreption::lossless`](crate::Transreption::lossless)). A plan reports it per
    /// step so a caller that consented to a lossy hop can still see which hop it was;
    /// under the default policy every step is `true` by construction.
    pub lossless: bool,
}

impl TransreptionStep {
    fn new(endpoint: &str, to: &str, lossless: bool) -> Self {
        TransreptionStep {
            endpoint: endpoint.to_string(),
            to: to.to_string(),
            lossless,
        }
    }
}

/// Whether every step of a plan is declared lossless — the plan is a transreption in
/// the formal sense (a composition of injective functions is injective) iff each edge
/// is. A lossy first hop is never rescued by a lossless second; a lossy second hop is
/// lossy for the plan. `true` for the empty plan.
pub fn is_lossless_plan(plan: &[TransreptionStep]) -> bool {
    plan.iter().all(|step| step.lossless)
}

/// How selection may plan: whether a **lossy** edge (a declared projection) may appear
/// in a plan at all. The default is lossless-only — a caller that asks for a
/// representation in another form receives the same resource in that form, or no
/// plan. Consent ([`allow_lossy`](Self::allow_lossy)) WIDENS the search: a lossless plan
/// is still preferred wherever one exists, and a lossy plan is the residual, with its
/// lossy steps reported ([`TransreptionStep::lossless`]). `#[non_exhaustive]`: build
/// it with the constructors — a later axis (cost, capability) is then not a flag day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TransreptionPolicy {
    /// Plan through edges declared lossless only (the default).
    pub lossless_only: bool,
}

impl Default for TransreptionPolicy {
    fn default() -> Self {
        TransreptionPolicy {
            lossless_only: true,
        }
    }
}

impl TransreptionPolicy {
    /// Lossless edges only — the default, and what [`select_transreptor`] applies.
    pub const fn lossless() -> Self {
        TransreptionPolicy {
            lossless_only: true,
        }
    }

    /// Consent to a lossy edge where no lossless plan exists. The plan reports each
    /// lossy step; the consent is the caller's to give per request (`lossy=allow` on a
    /// `Meta` request), never a kernel-wide setting.
    pub const fn allow_lossy() -> Self {
        TransreptionPolicy {
            lossless_only: false,
        }
    }

    /// Whether a lossy edge may appear in a plan under this policy.
    pub const fn allows_lossy(&self) -> bool {
        !self.lossless_only
    }
}

/// A transreptor's declared conversions, paired with its IRI.
struct Candidate {
    iri: String,
    from: Vec<String>,
    to: Vec<String>,
    lossless: bool,
}

impl Candidate {
    fn handles(&self, from: &str, to: &str) -> bool {
        self.from.iter().any(|f| f == from) && self.to.iter().any(|t| t == to)
    }

    fn step(&self, to: &str) -> TransreptionStep {
        TransreptionStep::new(&self.iri, to, self.lossless)
    }
}

/// Whether a transreptor can be invoked automatically — driven with only a piped `content`
/// and a target `as`. True iff every *required* input is `content` or `as`. A transreptor
/// with another required input (e.g. `urn:xslt:transform`'s `stylesheet`) is parameterized
/// and must be invoked explicitly, so it is not auto-selected.
pub fn is_auto_invocable(description: &Description) -> bool {
    description
        .inputs
        .iter()
        .filter(|i| i.required)
        .all(|i| i.name == "content" || i.name == "as")
}

/// Find a chain of auto-invocable transreptors in `root` converting `from` → `to`
/// through **lossless edges only** (the default [`TransreptionPolicy`]): a direct single
/// hop if one exists, else a two-hop pivot via [`CANONICAL`]. `None` if no such chain is
/// available (or if `from == to`, which needs no transreption). A transreptor declared
/// lossy ([`Description::lossy`](crate::Description::lossy)) is invisible here; to
/// consent to one, plan with [`select_transreptor_with`].
pub fn select_transreptor(root: &dyn Space, from: &str, to: &str) -> Option<Vec<TransreptionStep>> {
    select_transreptor_with(root, from, to, &TransreptionPolicy::lossless())
}

/// [`select_transreptor`] under an explicit [`TransreptionPolicy`]. The search order is
/// fixed and consent only WIDENS it: (1) a lossless direct hop, (2) a lossless pivot via
/// [`CANONICAL`], then — only if the policy allows lossy edges — (3) any direct hop,
/// (4) any pivot. So a lossless plan is chosen wherever one exists whatever the policy
/// (a two-hop transreption conserves information; a one-hop projection does not), and a
/// plan that crosses a lossy edge says so on the step.
///
/// The two-hop rule is per edge: the pivot is lossless iff BOTH edges are. A lossy first
/// hop under a lossless second is a lossy plan (refused by default, reported when
/// allowed), and so is the converse.
pub fn select_transreptor_with(
    root: &dyn Space,
    from: &str,
    to: &str,
    policy: &TransreptionPolicy,
) -> Option<Vec<TransreptionStep>> {
    if from == to {
        return None;
    }
    let candidates = collect(root);
    if let Some(plan) = plan_over(&candidates, from, to, true) {
        return Some(plan);
    }
    if policy.allows_lossy() {
        return plan_over(&candidates, from, to, false);
    }
    None
}

/// One pass of the star walk over `candidates`: a direct hop, else the pivot through
/// [`CANONICAL`]. With `lossless_only`, only candidates declaring the edge lossless are
/// eligible; otherwise every candidate is, and the steps carry each one's declaration.
fn plan_over(
    candidates: &[Candidate],
    from: &str,
    to: &str,
    lossless_only: bool,
) -> Option<Vec<TransreptionStep>> {
    let eligible = |c: &&Candidate| !lossless_only || c.lossless;

    // Direct: a single transreptor that reads `from` and produces `to`.
    if let Some(c) = candidates
        .iter()
        .filter(eligible)
        .find(|c| c.handles(from, to))
    {
        return Some(vec![c.step(to)]);
    }

    // Pivot: `from → text/turtle` then `text/turtle → to` (two distinct hops).
    if from != CANONICAL && to != CANONICAL {
        let first = candidates
            .iter()
            .filter(eligible)
            .find(|c| c.handles(from, CANONICAL))?;
        let second = candidates
            .iter()
            .filter(eligible)
            .find(|c| c.handles(CANONICAL, to))?;
        return Some(vec![first.step(CANONICAL), second.step(to)]);
    }

    None
}

/// Enumerate `root`'s auto-invocable transreptors (the same `entries → Meta → describe`
/// walk the catalog uses). Template-bound entries are deliberately excluded: a
/// transreption step invokes its endpoint with only `content` + `as`, so a pattern
/// needing binding arguments to form its IRI can never be auto-invoked.
fn collect(root: &dyn Space) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for entry in root.entries().unwrap_or_default() {
        let Ok(iri) = Iri::parse(&entry.pattern) else {
            continue;
        };
        if let Some(resolved) = probe(root, iri) {
            let description = resolved.endpoint.describe();
            if let Some(t) = description.transreption() {
                if is_auto_invocable(&description) {
                    candidates.push(Candidate {
                        iri: entry.pattern.clone(),
                        from: t.from.clone(),
                        to: t.to.clone(),
                        lossless: t.lossless,
                    });
                }
            }
        }
    }
    candidates
}

/// Convenience: select over an `Arc<dyn Space>` root (as a kernel holds) — lossless
/// edges only, as [`select_transreptor`].
pub fn select_transreptor_in(
    root: &Arc<dyn Space>,
    from: &str,
    to: &str,
) -> Option<Vec<TransreptionStep>> {
    select_transreptor(root.as_ref(), from, to)
}

/// Convenience: [`select_transreptor_with`] over an `Arc<dyn Space>` root.
pub fn select_transreptor_in_with(
    root: &Arc<dyn Space>,
    from: &str,
    to: &str,
    policy: &TransreptionPolicy,
) -> Option<Vec<TransreptionStep>> {
    select_transreptor_with(root.as_ref(), from, to, policy)
}

/// The placeholder each `{var}` takes when a template pattern is probe-expanded into a
/// concrete IRI for `Meta` resolution (see [`describe_entry`]). The value is arbitrary:
/// `describe()` does not depend on bindings, and [`describe_entry`]'s identity guard
/// catches the case where the probe IRI resolves elsewhere.
const PROBE: &str = "probe";

/// **The one resolver every `entries → Meta → describe` walk uses**, and where the
/// reachability algebra's subtraction lands: `Meta`-resolve `iri` in the empty chain
/// and hand back the resolution — unless it is a hit on ⊥
/// ([`Endpoint::is_limiter`](crate::Endpoint::is_limiter)), which is `None` exactly
/// as a miss is. A [`Limit`](crate::Limit) ahead of a member carves a family out of
/// what resolution reaches; enumeration cannot see that (a pattern list cannot decide
/// membership of a template in a grammar's family), so the walks decide it here, per
/// name, by the same resolution the kernel would perform. Without this the catalog
/// would describe a hole and the manifold would offer a name the kernel refuses —
/// the over-offer R2.3 forbids.
pub(crate) fn probe(space: &dyn Space, iri: Iri) -> Option<Resolved> {
    match space.resolve(&Request::new(Verb::Meta, iri), &Scope::empty()) {
        Resolution::Hit(resolved) if !resolved.endpoint.is_limiter() => Some(resolved),
        _ => None,
    }
}

/// A space entry's self-description, with how its pattern names the endpoint: `None`
/// for an exact, directly resolvable IRI; `Some(vars)` for a URI-template pattern
/// whose variables must be supplied (as `Binding`-source arguments) to form one.
pub(crate) struct EntryDescription {
    /// The bound endpoint's `describe()`.
    pub description: Description,
    /// The template's variable names, when the pattern is a template grammar.
    pub template_vars: Option<Vec<String>>,
}

/// Describe one space entry — the shared step of every `entries → Meta → describe`
/// walk (catalog, selection, validate's id lookup). An exact pattern IS the IRI to
/// `Meta`-resolve. A template pattern (`urn:file:{path}`) is not an IRI at all, so it is
/// **probe-expanded**: each `{var}` takes a placeholder, and the concrete probe IRI is
/// resolved the normal way — reaching the same endpoint the template binds, since
/// `describe()` does not depend on bindings. Resolution is first-match-wins, so a probe
/// IRI *could* land on a different binding; such a hit is discarded (better invisible
/// than misdescribed). `None` for patterns that are neither parseable IRIs nor parseable
/// templates, and for misses.
///
/// The guard takes **either witness**: the resolved endpoint's `name()`, or the id of the
/// description it hands back — the very artifact about to be attached to this pattern.
/// One witness is not enough, because over a **mounted** space they are not equally
/// authoritative. A mount resolves nothing locally: it always hits with a forwarder whose
/// `name()` is the client's *guess*, replayed from the remote's pattern STRINGS
/// first-match-wins and blind to the remote's real grammar semantics (ikigai-browse's PR
/// row rejects an `n` spanning a `:`, which no pattern string can express). The forwarder's
/// `describe()` is a Meta round-trip to the remote — its own answer for that very IRI —
/// so the description is right where the label is wrong. On a name-only guard the two
/// PR-grain rows (`…:pr:{n}:explain`, `…:pr:{n}:review`) were swallowed by the shorter
/// `…:pr:{n}` sibling's guess and vanished from every mounted manifold and MCP tool list,
/// while resolving correctly through the REPL.
pub(crate) fn describe_entry(root: &dyn Space, entry: &SpaceEntry) -> Option<EntryDescription> {
    if let Ok(iri) = Iri::parse(&entry.pattern) {
        let resolved = probe(root, iri)?;
        return Some(EntryDescription {
            description: resolved.endpoint.describe(),
            template_vars: None,
        });
    }
    let template = UriTemplate::parse(&entry.pattern).ok()?;
    let vars: Vec<String> = template.variables().map(str::to_string).collect();
    if vars.is_empty() {
        return None; // no variables ⇒ just a malformed IRI, not a template
    }
    let mut bindings = Bindings::new();
    for var in &vars {
        bindings.insert(var.clone(), PROBE);
    }
    let expanded = Iri::parse(template.expand(&bindings)?).ok()?;
    // A probe expansion that lands in a limited family hits ⊥ and is dropped here,
    // ahead of the identity guard below — the row is not listed, not misattributed.
    let resolved = probe(root, expanded)?;
    let description = resolved.endpoint.describe();
    if resolved.endpoint.name() != entry.endpoint && description.id != entry.endpoint {
        return None;
    }
    Some(EntryDescription {
        description,
        template_vars: Some(vars),
    })
}

/// Whether a template action is drivable from its declared contract: every template
/// variable must be a declared `Binding`-source input ([`ArgSpec::binding`]
/// (crate::ArgSpec::binding)), so a caller — engine, MCP projection, agent — knows to
/// substitute it into the pattern to form the concrete IRI. A template variable that is
/// undeclared (or declared as a by-value argument) leaves the IRI unconstructible from
/// the contract alone, so the action stays out of the manifold — the same principle that
/// keeps untyped required inputs out of typed selection.
fn template_drivable(action: &crate::describe::ActionSpec, vars: &[String]) -> bool {
    vars.iter().all(|var| {
        action
            .inputs
            .iter()
            .any(|i| i.name == *var && i.source == InputSource::Binding)
    })
}

/// One selected action — an (endpoint, verb) pair whose contract the query satisfied: its
/// required capability scopes are allowed, its verb/output fit the asked-for shape, and its
/// required typed inputs are satisfiable by the present RDF classes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionMatch {
    /// The bound pattern — what you invoke. For an exact grammar this is the endpoint's
    /// resolvable IRI; for a template grammar it is the URI-template pattern itself
    /// (`urn:file:{path}`), whose `Binding`-source arguments substitute in to form the
    /// concrete IRI.
    pub endpoint: String,
    /// The endpoint's description id (the catalog subject is `urn:ikigai:endpoint:{id}`).
    pub id: String,
    /// The matched verb.
    pub verb: Verb,
    /// The action node's catalog IRI (`urn:ikigai:endpoint:{id}:action:{verb}`) — joins
    /// this match to the full contract in the catalog graph.
    pub action: String,
    /// The capability scopes the action requires (all satisfied by the query's capability).
    pub requires: Vec<String>,
    /// How many *optional typed* inputs the present classes could not fill — the v1
    /// ranking: fewer = a better-fitted match.
    pub missing_optional: usize,
}

/// A selection query — every axis optional, so the degenerate query lists the caller's
/// whole capability-scoped action manifold ("what can I do at all?").
#[derive(Clone, Copy, Debug, Default)]
pub struct ActionQuery<'a> {
    /// RDF classes present in the context (entities you hold). Empty = no type filter.
    pub present: &'a [&'a str],
    /// Only actions with this verb.
    pub verb: Option<Verb>,
    /// Only actions that can produce this media type.
    pub want: Option<&'a str>,
    /// The caller's capability: actions whose `requires` it does not allow are NOT
    /// offered. This is the same [`Capability::allows`](crate::Capability::allows) check
    /// enforcement uses at invoke time — selection is a pre-flight of enforcement, never a
    /// substitute — so an attenuated agent's manifold simply lacks what it may not do.
    /// `None` = no capability filter (equivalent to root).
    pub capability: Option<&'a crate::Capability>,
}

/// Find endpoints in `root` whose required inputs are *fully typed and satisfiable* by
/// `present` — the RDF classes available in a context. An endpoint matches iff it has at
/// least one required input and **every** required input declares an
/// [`ArgSpec::class`](crate::ArgSpec) that appears in `present`. This is
/// [`select_transreptor`]'s sibling at the RDF-class level — "given these typed entities,
/// what can I do with them?" — the seed of layer action-inference. Optional inputs are
/// ignored; required inputs without a declared class make an endpoint un-inferable (it can't
/// be driven from the present types alone), so it's excluded. Capability-scoping — offering
/// only what the caller may invoke — composes on top and is **not** applied here.
pub fn select_action(root: &dyn Space, present: &[&str]) -> Vec<ActionMatch> {
    let query = ActionQuery {
        present,
        ..Default::default()
    };
    // The historical per-endpoint view: dedup the per-action matches by endpoint.
    let mut matches = select_actions(root, &query);
    let mut seen = std::collections::BTreeSet::new();
    matches.retain(|m| seen.insert(m.endpoint.clone()));
    matches
}

/// The action-level selection funnel: walk every bound endpoint's normalized per-verb
/// contracts ([`Description::action_specs`]) and keep the actions the query satisfies —
/// capability first (the caller never sees what it may not invoke), then verb, then the
/// wanted output type, then type-satisfiability of required inputs. Ordered
/// best-fitted-first ([`ActionMatch::missing_optional`], then endpoint/verb for
/// determinism).
pub fn select_actions(root: &dyn Space, query: &ActionQuery) -> Vec<ActionMatch> {
    let mut matches = Vec::new();
    for entry in root.entries().unwrap_or_default() {
        let Some(described) = describe_entry(root, &entry) else {
            continue;
        };
        let description = described.description;
        for action in description.action_specs() {
            if let Some(vars) = &described.template_vars {
                if !template_drivable(&action, vars) {
                    continue;
                }
            }
            if let Some(verb) = query.verb {
                if action.verb != verb {
                    continue;
                }
            }
            if let Some(want) = query.want {
                if !action.outputs.iter().any(|o| o == want) {
                    continue;
                }
            }
            if let Some(capability) = query.capability {
                if !action
                    .requires
                    .iter()
                    .all(|scope| cap_satisfies(capability, scope))
                {
                    continue;
                }
            }
            if !query.present.is_empty() && !spec_satisfiable(&action, query.present) {
                continue;
            }
            let missing_optional = action
                .inputs
                .iter()
                .filter(|i| !i.required)
                .filter(|i| {
                    i.class
                        .as_deref()
                        .is_some_and(|c| !query.present.contains(&c))
                })
                .count();
            let verb_name = format!("{:?}", action.verb).to_lowercase();
            matches.push(ActionMatch {
                endpoint: entry.pattern.clone(),
                id: description.id.clone(),
                verb: action.verb,
                action: format!("urn:ikigai:endpoint:{}:action:{verb_name}", description.id),
                requires: action.requires.clone(),
                missing_optional,
            });
        }
    }
    matches.sort_by(|a, b| {
        (a.missing_optional, &a.endpoint, a.verb as u8).cmp(&(
            b.missing_optional,
            &b.endpoint,
            b.verb as u8,
        ))
    });
    matches
}

/// Whether `capability` satisfies one required scope — [`Capability::allows`] for a plain
/// scope, or, for a `…:*` wildcard, "holds ANY grant under this prefix". The wildcard is
/// how parameterized capability grammars (`urn:cap:net:<host-rule>`,
/// `urn:cap:fs:<action>:<path>`) annotate their actions: no single static IRI names what
/// they require, because authorization depends on the argument (which selection doesn't
/// have yet). Offering-level semantics only — enforcement at invoke time still checks the
/// exact target against the ACL.
pub(crate) fn cap_satisfies(capability: &crate::Capability, scope: &str) -> bool {
    match scope.strip_suffix('*') {
        Some(prefix) => match capability.scopes() {
            None => true, // root
            Some(held) => held.iter().any(|s| s.starts_with(prefix)),
        },
        None => capability.allows(scope),
    }
}

/// Whether every required input of `action` declares a class present in `present`, with
/// at least one required input (see [`select_action`]).
fn spec_satisfiable(action: &crate::describe::ActionSpec, present: &[&str]) -> bool {
    let mut has_required = false;
    for input in action.inputs.iter().filter(|i| i.required) {
        has_required = true;
        match &input.class {
            Some(class) if present.contains(&class.as_str()) => {}
            _ => return false,
        }
    }
    has_required
}

/// Convenience: select actions over an `Arc<dyn Space>` root (as a kernel holds).
pub fn select_action_in(root: &Arc<dyn Space>, present: &[&str]) -> Vec<ActionMatch> {
    select_action(root.as_ref(), present)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::describe::Description;
    use crate::endpoint::{Endpoint, FnEndpoint};
    use crate::grammar::Exact;
    use crate::repr::{ReprType, Representation};
    use crate::space::EndpointSpace;

    /// A stub transreptor: declares from/to (auto-invocable: content + as), does nothing.
    fn transreptor(id: &'static str, from: &[&str], to: &[&str]) -> FnEndpoint {
        FnEndpoint::new(id, |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new(id)
                .verb(Verb::Source)
                .input(crate::describe::ArgSpec::new("content"))
                .input(crate::describe::ArgSpec::new("as"))
                .transreptor(from.iter().copied(), to.iter().copied()),
        )
    }

    fn space() -> EndpointSpace {
        EndpointSpace::new()
            // turtle <-> rdf/xml, n-triples, html (an rdf-transrept-like hub)
            .bind(
                Exact::new("urn:rdf:transrept"),
                transreptor(
                    "rdf",
                    &[
                        "text/turtle",
                        "application/rdf+xml",
                        "application/n-triples",
                    ],
                    &[
                        "text/turtle",
                        "application/rdf+xml",
                        "application/n-triples",
                        "text/html",
                    ],
                ),
            )
            // a plain (non-transreptor) endpoint — must be ignored
            .bind(
                Exact::new("urn:test:to-upper"),
                FnEndpoint::new("toUpper", |_inv| {
                    Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
                }),
            )
    }

    #[test]
    fn finds_a_direct_hop() {
        let plan = select_transreptor(&space(), "application/rdf+xml", "text/turtle").unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].endpoint, "urn:rdf:transrept");
        assert_eq!(plan[0].to, "text/turtle");
    }

    #[test]
    fn pivots_via_turtle_when_no_direct_hop() {
        // rdf/xml → html: no single transreptor declares that pair directly here? It does
        // (rdf handles rdf+xml→html). Use a case needing the pivot: add a turtle→csv-only
        // transreptor and ask rdf+xml → text/csv.
        let space = space().bind(
            Exact::new("urn:demo:csv"),
            transreptor("csv", &["text/turtle"], &["text/csv"]),
        );
        let plan = select_transreptor(&space, "application/rdf+xml", "text/csv").unwrap();
        assert_eq!(plan.len(), 2, "{plan:?}");
        assert_eq!(plan[0].to, "text/turtle"); // pivot
        assert_eq!(plan[1].endpoint, "urn:demo:csv");
        assert_eq!(plan[1].to, "text/csv");
    }

    #[test]
    fn none_when_unreachable_or_identity() {
        assert!(select_transreptor(&space(), "text/turtle", "text/turtle").is_none());
        assert!(select_transreptor(&space(), "application/pdf", "image/png").is_none());
    }

    // --- lossless by default; a lossy edge only with consent ---

    /// A stub PROJECTION: the same shape as `transreptor`, declared lossy.
    fn projection(id: &'static str, from: &[&str], to: &[&str]) -> FnEndpoint {
        FnEndpoint::new(id, |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new(id)
                .verb(Verb::Source)
                .input(crate::describe::ArgSpec::new("content"))
                .input(crate::describe::ArgSpec::new("as"))
                .transreptor(from.iter().copied(), to.iter().copied())
                .lossy(),
        )
    }

    const ALLOW: TransreptionPolicy = TransreptionPolicy::allow_lossy();

    #[test]
    fn every_default_plan_is_lossless_and_says_so() {
        // Nothing here declares `.lossy()`: the plans of 0.1.76 are unchanged, and each
        // step now carries the declaration it planned on.
        let direct = select_transreptor(&space(), "application/rdf+xml", "text/turtle").unwrap();
        assert!(is_lossless_plan(&direct));
        let space = space().bind(
            Exact::new("urn:demo:csv"),
            transreptor("csv", &["text/turtle"], &["text/csv"]),
        );
        let pivot = select_transreptor(&space, "application/rdf+xml", "text/csv").unwrap();
        assert_eq!(pivot.len(), 2);
        assert!(is_lossless_plan(&pivot));
        assert!(is_lossless_plan(&[]));
    }

    #[test]
    fn a_lossy_direct_hop_is_not_chosen_by_default_but_is_with_consent_and_reported() {
        let space = space().bind(
            Exact::new("urn:demo:summarize"),
            projection("summarize", &["text/turtle"], &["text/x-summary"]),
        );
        // Default: the only route is a projection, so there is no plan.
        assert!(select_transreptor(&space, "text/turtle", "text/x-summary").is_none());
        // Consent: the same route is planned, and the step says it is lossy.
        let plan = select_transreptor_with(&space, "text/turtle", "text/x-summary", &ALLOW)
            .expect("consented");
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].endpoint, "urn:demo:summarize");
        assert!(!plan[0].lossless);
        assert!(!is_lossless_plan(&plan));
    }

    #[test]
    fn consent_widens_the_search_but_a_lossless_plan_still_wins() {
        // Two direct routes to the same type, the projection bound FIRST (first-match
        // would pick it): under consent the lossless one is still chosen — consent
        // admits a lossy edge, it does not prefer one.
        let space = EndpointSpace::new()
            .bind(
                Exact::new("urn:demo:lossy-html"),
                projection("lossy-html", &["text/turtle"], &["text/html"]),
            )
            .bind(
                Exact::new("urn:demo:html"),
                transreptor("html", &["text/turtle"], &["text/html"]),
            );
        for policy in [TransreptionPolicy::lossless(), ALLOW] {
            let plan = select_transreptor_with(&space, "text/turtle", "text/html", &policy)
                .expect("a lossless route exists");
            assert_eq!(plan[0].endpoint, "urn:demo:html", "{policy:?}");
            assert!(is_lossless_plan(&plan));
        }
        // And a lossless TWO-hop beats a lossy ONE-hop: information conservation over
        // hop count (a composition of injections is an injection; a projection is not).
        let space = EndpointSpace::new()
            .bind(
                Exact::new("urn:demo:lossy-direct"),
                projection("lossy-direct", &["application/rdf+xml"], &["text/csv"]),
            )
            .bind(
                Exact::new("urn:rdf:transrept"),
                transreptor("rdf", &["application/rdf+xml"], &["text/turtle"]),
            )
            .bind(
                Exact::new("urn:demo:csv"),
                transreptor("csv", &["text/turtle"], &["text/csv"]),
            );
        let plan =
            select_transreptor_with(&space, "application/rdf+xml", "text/csv", &ALLOW).unwrap();
        assert_eq!(plan.len(), 2, "{plan:?}");
        assert!(is_lossless_plan(&plan));
    }

    #[test]
    fn a_pivot_is_lossless_iff_both_edges_are() {
        // Three pivots to text/csv from rdf+xml, each with one edge lossy — or neither.
        let cases: [(&str, bool, bool); 3] = [
            ("lossless+lossy", true, false),
            ("lossy+lossless", false, true),
            ("lossy+lossy", false, false),
        ];
        for (label, first_lossless, second_lossless) in cases {
            let first = if first_lossless {
                transreptor("rdf", &["application/rdf+xml"], &["text/turtle"])
            } else {
                projection("rdf", &["application/rdf+xml"], &["text/turtle"])
            };
            let second = if second_lossless {
                transreptor("csv", &["text/turtle"], &["text/csv"])
            } else {
                projection("csv", &["text/turtle"], &["text/csv"])
            };
            let space = EndpointSpace::new()
                .bind(Exact::new("urn:rdf:transrept"), first)
                .bind(Exact::new("urn:demo:csv"), second);
            // Refused by default: a lossy edge anywhere in the pivot is a lossy plan —
            // a lossy first hop is never rescued by a lossless second, and a lossless
            // first hop does not make a lossy second one a transreption.
            assert!(
                select_transreptor(&space, "application/rdf+xml", "text/csv").is_none(),
                "{label}: planned through a lossy edge without consent"
            );
            // Allowed with consent, and each step reports its own declaration.
            let plan = select_transreptor_with(&space, "application/rdf+xml", "text/csv", &ALLOW)
                .unwrap_or_else(|| panic!("{label}: consented and still no plan"));
            assert_eq!(plan.len(), 2, "{label}: {plan:?}");
            assert_eq!(plan[0].lossless, first_lossless, "{label}");
            assert_eq!(plan[1].lossless, second_lossless, "{label}");
            assert!(!is_lossless_plan(&plan), "{label}");
        }
    }

    #[test]
    fn the_policy_defaults_to_lossless_only() {
        assert_eq!(
            TransreptionPolicy::default(),
            TransreptionPolicy::lossless()
        );
        assert!(!TransreptionPolicy::default().allows_lossy());
        assert!(ALLOW.allows_lossy());
    }

    #[test]
    fn parameterized_transreptors_are_not_auto_invocable() {
        // An xslt-like transreptor with a required `stylesheet` is excluded from selection.
        let xslt = FnEndpoint::new("xslt", |_inv| {
            Ok(Representation::new(ReprType::new("text/html"), Vec::new()))
        })
        .with_description(
            Description::new("xslt")
                .verb(Verb::Source)
                .input(crate::describe::ArgSpec::new("content"))
                .input(crate::describe::ArgSpec::new("stylesheet"))
                .input(crate::describe::ArgSpec::new("as"))
                .transreptor(["application/xml"], ["text/html"]),
        );
        assert!(!is_auto_invocable(&xslt.describe()));
        let space = EndpointSpace::new().bind(Exact::new("urn:xslt:transform"), xslt);
        assert!(select_transreptor(&space, "application/xml", "text/html").is_none());
    }

    // --- select_action ---

    const PERSON: &str = "https://schema.org/Person";
    const PLACE: &str = "https://schema.org/Place";
    const DATE: &str = "https://schema.org/Date";

    /// An endpoint with the given required, typed inputs (each input named `inN`, classed).
    fn typed_action(id: &'static str, classes: &[&str]) -> FnEndpoint {
        let mut d = Description::new(id).verb(Verb::Source);
        for (n, class) in classes.iter().enumerate() {
            d = d.input(crate::describe::ArgSpec::new(format!("in{n}")).class(*class));
        }
        FnEndpoint::new(id, |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(d)
    }

    fn action_space() -> EndpointSpace {
        EndpointSpace::new()
            // schedule(Person, Place, Date) — the "invite to dinner" action.
            .bind(
                Exact::new("urn:demo:schedule"),
                typed_action("schedule", &[PERSON, PLACE, DATE]),
            )
            // greet(Person) — satisfiable from just a Person.
            .bind(
                Exact::new("urn:demo:greet"),
                typed_action("greet", &[PERSON]),
            )
            // a plain untyped endpoint (content/as) — never an inferred action.
            .bind(
                Exact::new("urn:rdf:transrept"),
                transreptor("rdf", &["text/turtle"], &["text/html"]),
            )
    }

    fn endpoints(matches: &[ActionMatch]) -> Vec<&str> {
        let mut v: Vec<&str> = matches.iter().map(|m| m.endpoint.as_str()).collect();
        v.sort();
        v
    }

    #[test]
    fn action_matches_when_all_required_typed_inputs_are_present() {
        // Canvas with a Person, a Place, and Date(s) → both schedule and greet are offerable.
        let m = select_action(&action_space(), &[PERSON, PLACE, DATE]);
        assert_eq!(endpoints(&m), vec!["urn:demo:greet", "urn:demo:schedule"]);
    }

    #[test]
    fn action_excluded_when_a_required_type_is_missing() {
        // Only a Person present → greet matches, schedule (needs Place + Date) does not.
        let m = select_action(&action_space(), &[PERSON]);
        assert_eq!(endpoints(&m), vec!["urn:demo:greet"]);
    }

    // --- template grammars in the manifold ---

    use crate::describe::ActionSpec;
    use crate::grammar::UriTemplate;

    /// A file-like endpoint bound by template: one Source action, capability-gated,
    /// with its template variable declared as a Binding-source input.
    fn template_file_endpoint() -> FnEndpoint {
        FnEndpoint::new("file", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new("file").action(
                ActionSpec::new(Verb::Source)
                    .requires("urn:cap:fs:read:*")
                    .input(
                        crate::describe::ArgSpec::new("path")
                            .summary("captured from the IRI")
                            .binding(),
                    ),
            ),
        )
    }

    #[test]
    fn a_template_action_with_declared_binding_args_joins_the_manifold() {
        let space = EndpointSpace::new().bind(
            UriTemplate::parse("urn:file:{path}").unwrap(),
            template_file_endpoint(),
        );

        // Under a capability holding a grant beneath the wildcard, the action is offered
        // — and the match carries the PATTERN string round-trip, not a probe IRI.
        let reader = crate::Capability::scoped(["urn:cap:fs:read:/notes"]);
        let query = ActionQuery {
            capability: Some(&reader),
            ..Default::default()
        };
        let m = select_actions(&space, &query);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].endpoint, "urn:file:{path}");
        assert_eq!(m[0].id, "file");
        assert_eq!(m[0].verb, Verb::Source);
        assert_eq!(m[0].action, "urn:ikigai:endpoint:file:action:source");

        // Under a capability with no fs grant, the manifold simply lacks it.
        let denied = crate::Capability::scoped(["urn:cap:unrelated"]);
        let query = ActionQuery {
            capability: Some(&denied),
            ..Default::default()
        };
        assert!(select_actions(&space, &query).is_empty());
    }

    #[test]
    fn a_template_whose_variables_lack_binding_argspecs_stays_out() {
        // `path` declared as a by-value ARGUMENT, not a Binding: the contract gives a
        // caller no way to construct the concrete IRI, so the action is not offered —
        // the same principle that keeps untyped required inputs out of typed selection.
        let undeclared = FnEndpoint::new("file", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new("file")
                .verb(Verb::Source)
                .input(crate::describe::ArgSpec::new("path")),
        );
        let space =
            EndpointSpace::new().bind(UriTemplate::parse("urn:file:{path}").unwrap(), undeclared);
        assert!(select_actions(&space, &ActionQuery::default()).is_empty());
    }

    #[test]
    fn a_shadowed_probe_is_discarded_not_misattributed() {
        // The probe IRI for `urn:t:{v}:x` is `urn:t:probe:x` — bound here, FIRST, to a
        // different endpoint. Resolution hands back the shadow; the name guard rejects
        // it rather than attaching the shadow's description to the template pattern.
        let shadow = FnEndpoint::new("shadow", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(Description::new("shadow").verb(Verb::Source).input(
            crate::describe::ArgSpec::new("v").binding(), // even "drivable" on paper
        ));
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:t:probe:x"), shadow)
            .bind(
                UriTemplate::parse("urn:t:{v}:x").unwrap(),
                template_file_endpoint(),
            );
        let matches = select_actions(&space, &ActionQuery::default());
        assert!(
            !matches.iter().any(|m| m.endpoint == "urn:t:{v}:x"),
            "shadowed template must stay invisible, not lie: {matches:?}"
        );
        // The exact binding itself is still offered normally.
        assert!(matches.iter().any(|m| m.endpoint == "urn:t:probe:x"));
    }

    #[test]
    fn a_probe_that_lands_in_a_limited_family_is_subtracted_not_listed() {
        // Reach by enumeration must not OVER-approximate under a limiter: the raw
        // pattern list still carries every later member's rows, and it is the probe
        // — the same resolution the kernel performs — that subtracts the family.
        // Two shapes: an exact row inside the family, and a template row whose
        // probe expansion (`urn:t:probe:secret`) lands inside it.
        use crate::space::{Fallback, Limit};
        let public = FnEndpoint::new("public", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(Description::new("public").verb(Verb::Source));
        let s = Arc::new(
            EndpointSpace::new()
                .bind(Exact::new("urn:t:public"), public)
                .bind(Exact::new("urn:t:probe:secret"), template_file_endpoint())
                .bind(
                    UriTemplate::parse("urn:t:{v}:secret").unwrap(),
                    template_file_endpoint(),
                ),
        );
        let limited: Arc<dyn Space> = Arc::new(Fallback::new(vec![
            Arc::new(Limit::matching(
                UriTemplate::parse("urn:t:{v}:secret").unwrap(),
            )),
            s,
        ]));

        // Enumeration lists all three: it is a list of what is bound.
        let patterns: Vec<String> = limited
            .entries()
            .expect("enumerable")
            .into_iter()
            .map(|e| e.pattern)
            .collect();
        assert_eq!(
            patterns,
            ["urn:t:public", "urn:t:probe:secret", "urn:t:{v}:secret"]
        );
        // The walk subtracts the family — both rows — and keeps the rest.
        let offered: Vec<String> = select_actions(limited.as_ref(), &ActionQuery::default())
            .into_iter()
            .map(|m| m.endpoint)
            .collect();
        assert_eq!(
            offered,
            ["urn:t:public"],
            "a limited name reached the manifold"
        );
        // And the shared step says so for each row directly.
        for pattern in ["urn:t:probe:secret", "urn:t:{v}:secret"] {
            assert!(
                describe_entry(limited.as_ref(), &SpaceEntry::new(pattern, "file")).is_none(),
                "`{pattern}` described a hole"
            );
        }
    }

    // --- template rows behind a MOUNT ---

    /// The two rows a mounted browse family binds for one root, in the remote's own
    /// order: the shorter one ends in a variable and is a prefix of the longer.
    const MOUNTED_ROWS: [(&str, &str); 2] = [
        ("urn:t:pr:{n}", "pr-page"),
        ("urn:t:pr:{n}:explain", "pr-explain"),
    ];

    /// The REMOTE's honest resolution of a concrete IRI: its `pr:{n}` grammar rejects an
    /// `n` spanning a `:` (exactly what ikigai-browse's PR row does), so the probe IRI
    /// `urn:t:pr:probe:explain` genuinely reaches the `:explain` endpoint over there.
    fn remote_endpoint_of(target: &str) -> Option<&'static str> {
        let rest = target.strip_prefix("urn:t:pr:")?;
        match rest.strip_suffix(":explain") {
            Some(n) if !n.is_empty() && !n.contains(':') => Some("pr-explain"),
            _ => (!rest.is_empty() && !rest.contains(':')).then_some("pr-page"),
        }
    }

    /// The CLIENT's guess at the remote endpoint's name: the remote's pattern strings
    /// replayed first-match-wins, which is all a mount has locally (`RemoteNames` in
    /// ikigai-resolve). It cannot see the `:`-rejection above, so the shorter row's
    /// trailing variable swallows the longer row's probe IRI.
    fn guessed_endpoint_of(target: &Iri) -> Option<&'static str> {
        use crate::grammar::Grammar;
        MOUNTED_ROWS.iter().find_map(|(pattern, endpoint)| {
            UriTemplate::parse(*pattern)
                .ok()?
                .match_iri(target)
                .map(|_| *endpoint)
        })
    }

    /// A stand-in for a mounted remote space (ikigai-cli's `MountedRemote`): it resolves
    /// nothing itself — every request is forwarded — so it always hands back a forwarder
    /// whose `name()` is the local guess and whose `describe()` is the remote's own
    /// answer for that very IRI.
    struct MountFace;

    struct Forwarder {
        guessed: &'static str,
        described: Description,
    }

    #[async_trait::async_trait]
    impl crate::Endpoint for Forwarder {
        async fn invoke(&self, _inv: &crate::Invocation<'_>) -> crate::Result<Representation> {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        }

        fn name(&self) -> &str {
            self.guessed
        }

        fn describe(&self) -> Description {
            self.described.clone()
        }
    }

    fn mounted_description(id: &str) -> Description {
        Description::new(id).action(
            ActionSpec::new(Verb::Source).input(crate::describe::ArgSpec::new("n").binding()),
        )
    }

    impl Space for MountFace {
        fn resolve(&self, request: &Request, _scope: &Scope) -> Resolution {
            // A mount never misses: routing is the prefix's job.
            Resolution::Hit(crate::space::Resolved::new(
                Arc::new(Forwarder {
                    guessed: guessed_endpoint_of(&request.target).unwrap_or("remote"),
                    described: mounted_description(
                        remote_endpoint_of(request.target.as_str()).unwrap_or("remote"),
                    ),
                }),
                Bindings::new(),
            ))
        }

        fn entries(&self) -> Option<Vec<SpaceEntry>> {
            Some(
                MOUNTED_ROWS
                    .iter()
                    .map(|(pattern, endpoint)| SpaceEntry::new(*pattern, *endpoint))
                    .collect(),
            )
        }
    }

    #[test]
    fn a_mounted_template_row_survives_a_sibling_that_shadows_only_the_local_guess() {
        let matches = select_actions(&MountFace, &ActionQuery::default());
        assert_eq!(
            endpoints(&matches),
            vec!["urn:t:pr:{n}", "urn:t:pr:{n}:explain"],
            "both mounted rows join the manifold: the longer row's probe IRI is what the \
             REMOTE resolves, and its description says so, even though the local \
             first-match-wins guess labels it with the shorter sibling's endpoint"
        );
        // The right description is attached — not the shadowing sibling's.
        let explain = matches
            .iter()
            .find(|m| m.endpoint == "urn:t:pr:{n}:explain")
            .expect("the explain row is offered");
        assert_eq!(explain.id, "pr-explain");
        assert_eq!(
            explain.action,
            "urn:ikigai:endpoint:pr-explain:action:source"
        );
    }

    #[test]
    fn a_row_whose_description_also_disowns_it_stays_invisible() {
        // Neither witness names the entry's endpoint — the guard still discards it, so a
        // genuinely misresolved probe never attaches a foreign contract to a pattern.
        struct Disowned;
        impl Space for Disowned {
            fn resolve(&self, _request: &Request, _scope: &Scope) -> Resolution {
                Resolution::Hit(crate::space::Resolved::new(
                    Arc::new(Forwarder {
                        guessed: "someone-else",
                        described: mounted_description("someone-else"),
                    }),
                    Bindings::new(),
                ))
            }

            fn entries(&self) -> Option<Vec<SpaceEntry>> {
                Some(vec![SpaceEntry::new("urn:t:pr:{n}:explain", "pr-explain")])
            }
        }
        assert!(select_actions(&Disowned, &ActionQuery::default()).is_empty());
    }

    #[test]
    fn untyped_and_no_required_endpoints_are_never_inferred_actions() {
        // The transreptor's required inputs (content/as) carry no class → not an action,
        // even when every present type is offered.
        let m = select_action(&action_space(), &[PERSON, PLACE, DATE]);
        assert!(!endpoints(&m).contains(&"urn:rdf:transrept"));

        // An endpoint with no required inputs is not an inferred action either.
        let space = EndpointSpace::new().bind(
            Exact::new("urn:demo:ping"),
            FnEndpoint::new("ping", |_inv| {
                Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
            }),
        );
        assert!(select_action(&space, &[PERSON]).is_empty());
    }
}
