//! The kernel's own operations, as self-describing resources.
//!
//! `urn:kernel:*` is resolved *intrinsically* — [`Kernel::issue`](crate::Kernel::issue)
//! intercepts the prefix before the root space, which therefore never binds those
//! names. That is correct for dispatch and was wrong for **description**: the root
//! space is what every `entries → Meta → describe` walk enumerates, so the catalog,
//! the action manifold, describe-by-id and `urn:kernel:validate` all listed every
//! endpoint in the system except the kernel's own. The resources that make everything
//! else legible were the ones the system could not describe — `ikigai -c list`
//! returned 266 bindings and not one `urn:kernel:`.
//!
//! This module closes that by giving each operation a real [`Description`] and
//! wrapping them in a [`Space`] the kernel composes IN FRONT OF its root **for
//! description only** ([`Kernel::described`](crate::Kernel)). Dispatch is untouched:
//! `urn:kernel:*` still never reaches a space on the issue path, and [`KernelOp`]
//! refuses invocation precisely so that a future refactor which *did* route through
//! here would fail loudly instead of silently answering nothing.
//!
//! ## Declared = enforced, mechanically
//!
//! Each description's `requires` is the same scope its arm in
//! [`Kernel::issue_kernel`](crate::Kernel) passes to `require_cap`, and the kernel
//! gates on the declaration itself before dispatching, so a declaration cannot
//! over-offer. The converse — an arm enforcing a scope nobody declared — is what
//! `kernel_ops_declare_exactly_what_they_enforce` in `kernel.rs` pins.
//!
//! [`Verb::Meta`] is deliberately absent from every `requires`: `action_specs()`
//! filters Meta out (it is universal, never a selectable action), so describing a
//! kernel operation is ungated exactly as `describe urn:kernel:actions` has always
//! been. Reading a description discloses nothing the crate documentation does not.

use std::sync::Arc;

use async_trait::async_trait;

use crate::describe::{ArgSpec, Description};
use crate::endpoint::{Endpoint, Invocation};
use crate::error::{Error, Result};
use crate::grammar::Bindings;
use crate::kernel::KERNEL_NS;
use crate::repr::Representation;
use crate::request::Request;
use crate::space::{Resolution, Resolved, Scope, Space, SpaceEntry};
use crate::verb::Verb;

const TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";
const TEXT_TURTLE: &str = "text/turtle";

/// The capability gating the kernel's introspection operations.
const CAP_INSPECT: &str = "urn:cap:kernel:inspect";
/// The capability gating thread-cutting.
const CAP_CUT: &str = "urn:cap:kernel:cut";

/// Every operation the kernel serves under `urn:kernel:`, in listing order. The
/// single source of truth for what the kernel can be asked — enumerating the
/// namespace, not a hand-kept parallel list.
pub(crate) const OPS: &[&str] = &[
    "actions",
    "aliases",
    "cache",
    "catalog",
    "constraint",
    "cut",
    "scheduler",
    "threads",
    "topology",
    "uncached",
    "validate",
];

/// Whether `op`'s answer is a function of the resolution chain it is asked in.
/// Every kernel operation is resolved ahead of the chain and answers the same in
/// any chain — keyed with scope `0` — except the topology, whose subject IS the
/// chain (corridors innermost first, then the root unless severed), so its entry
/// is keyed by the chain's fingerprint like any scoped resolution.
pub(crate) fn depends_on_chain(op: &str) -> bool {
    op == "topology"
}

/// The self-description of one kernel operation, or `None` for a name the kernel
/// does not serve.
pub(crate) fn description(op: &str) -> Option<Description> {
    Some(match op {
        "actions" => actions(),
        "aliases" => aliases(),
        "cache" => cache(),
        "catalog" => catalog(),
        "constraint" => constraint(),
        "cut" => cut(),
        "scheduler" => scheduler(),
        "threads" => threads(),
        "topology" => topology(),
        "uncached" => uncached(),
        "validate" => validate(),
        _ => return None,
    })
}

/// The capability scopes invoking `op` with `verb` requires, per its declaration.
/// Empty for an undeclared (op, verb) pair — including every `Meta`, which
/// `action_specs()` excludes by design.
pub(crate) fn required_scopes(op: &str, verb: Verb) -> Vec<String> {
    description(op)
        .map(|d| {
            d.action_specs()
                .into_iter()
                .filter(|spec| spec.verb == verb)
                .flat_map(|spec| spec.requires)
                .collect()
        })
        .unwrap_or_default()
}

/// The description of `urn:kernel:actions` — the capability-scoped action manifold.
/// Declares `types` so the engine routes `types=` (it only names *declared* inputs)
/// and `describe urn:kernel:actions` works, surfacing typed action-selection like any
/// bound endpoint.
fn actions() -> Description {
    Description::new("kernel-actions")
        .title("Action selection")
        .summary(
            "Given the RDF classes of the entities you have, list the endpoints whose required \
             typed inputs are all satisfied — \"what can I do with these?\". One endpoint IRI \
             per line; pipe into a `..` map to act on each.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .input(
            ArgSpec::new("types")
                .summary("present RDF class IRIs, comma- or space-separated")
                .optional(),
        )
        .input(
            ArgSpec::new("verb")
                .summary("only actions answering this verb")
                .one_of(["source", "sink", "exists", "delete"])
                .optional(),
        )
        .input(
            ArgSpec::new("want")
                .summary("only actions that can produce this media type")
                .optional(),
        )
        .input(
            ArgSpec::new("as")
                .summary("response face")
                .one_of(["text/plain", TEXT_TURTLE])
                .default_value("text/plain"),
        )
        .output(TEXT_PLAIN_UTF8)
        .output(TEXT_TURTLE)
}

/// The description of `urn:kernel:validate` — stage four of the selection funnel.
fn validate() -> Description {
    Description::new("kernel-validate")
        .title("Validate a proposed invocation")
        .summary(
            "Stage four of the selection funnel: check a proposed invocation against the \
             action's declared contract BEFORE firing it — required inputs present, one_of \
             respected, XSD scalars plausible, no unknown arguments, and the ambient \
             capability satisfying the action's requires. The answer is a SHACL validation \
             report: violations are data, and sh:resultPath joins each one back to the \
             catalog's input node.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .input(
            ArgSpec::new("action")
                .summary("the catalog action IRI (urn:ikigai:endpoint:<id>:action:<verb>)")
                .optional(),
        )
        .input(
            ArgSpec::new("endpoint")
                .summary("alternative: the bound endpoint IRI, with verb=")
                .optional(),
        )
        .input(
            ArgSpec::new("verb")
                .summary("the verb, when addressing by endpoint=")
                .one_of(["source", "sink", "exists", "delete"])
                .optional(),
        )
        .input(
            ArgSpec::new("args")
                .summary("the proposed arguments: key=value pairs joined with & (or newlines)")
                .optional(),
        )
        .output(TEXT_TURTLE)
}

/// The description of `urn:kernel:catalog` — every bound endpoint's `describe()` as
/// one RDF graph.
fn catalog() -> Description {
    Description::new("kernel-catalog")
        .title("Endpoint catalog")
        .summary(
            "Every bound endpoint's self-description as one Turtle graph — the kernel made \
             queryable about itself with SPARQL, and renderable to HTML by transreption. \
             Says what EXISTS, for a caller with inspect authority; `urn:kernel:actions` is \
             the capability-scoped answer to what you MAY DO.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_INSPECT)
        .output(TEXT_TURTLE)
}

/// The description of `urn:kernel:topology` — the arrangement as a resource.
fn topology() -> Description {
    Description::new("kernel-topology")
        .title("Resolution topology")
        .summary(
            "The arrangement resolution walks, as one Turtle graph: the chain this request \
             is resolved in (its corridors innermost first, then the root unless severed), \
             each space as an ik:Fallback (ordered ik:layers), ik:Mount (ik:prefix, ik:space), \
             ik:Limit (ik:family), ik:EndpointSpace (ik:pattern), ik:Alias (ik:rewrites), \
             ik:Rewrite or ik:OpaqueSpace, named by its own IRI or skolemized. What the \
             catalog lists flat, this states as structure — so whether a family is \
             reachable without passing a limiter is a path query, not a request.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_INSPECT)
        .output(TEXT_TURTLE)
}

/// The description of `urn:kernel:cut` — the thread-cutting Sink.
fn cut() -> Description {
    Description::new("kernel-cut")
        .title("Cut a golden thread")
        .summary(
            "Invalidate every cached representation depending on a golden thread, by \
             resolving rather than through a special method — so an endpoint, a filesystem \
             watcher or a remote peer all invalidate the same way. Name the thread in \
             `thread`, or sink it as content (`sink urn:kernel:cut <thread>`); exactly one \
             of the two is required, which a contract of independent arguments cannot say.",
        )
        .verb(Verb::Sink)
        .verb(Verb::Meta)
        .requires(CAP_CUT)
        .input(
            ArgSpec::new("thread")
                .summary(
                    "the golden thread to cut; `urn:kernel:bindings` is the well-known one \
                     meaning the binding set changed, and recomputes every self-description face",
                )
                .optional(),
        )
        .input(
            ArgSpec::new("content")
                .summary("the thread as sunk content — the `sink urn:kernel:cut <thread>` form")
                .optional(),
        )
        .output(TEXT_PLAIN_UTF8)
}

/// The description of `urn:kernel:cache`.
fn cache() -> Description {
    Description::new("kernel-cache")
        .title("Cache readout")
        .summary(
            "What the representation cache holds: a count, then one line per entry — the IRI \
             it was resolved from, its representation type and size, and how many golden \
             threads it depends on (cut any of them and the entry recomputes).",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_INSPECT)
        .output(TEXT_PLAIN_UTF8)
}

/// The description of `urn:kernel:uncached`.
fn uncached() -> Description {
    Description::new("kernel-uncached")
        .title("Why reads were not cached")
        .summary(
            "The last few resources the kernel computed and did NOT cache, most recent first: \
             the IRI, how many times it has been computed uncached, the reason (the endpoint \
             declared no caching; a named dependency, refusal or failure made it volatile; a \
             volatile piped input; a deadline with no clock or already past; a cut while it \
             computed; the cache policy), and the chain it was computed in. Needs no tracer \
             installed.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_INSPECT)
        .output(TEXT_PLAIN_UTF8)
}

/// The description of `urn:kernel:threads`.
fn threads() -> Description {
    Description::new("kernel-threads")
        .title("Golden threads")
        .summary(
            "The golden threads that have been cut and their current generations — a cached \
             representation is valid only while every thread it depends on still stands at \
             the generation it was stored under.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_INSPECT)
        .output(TEXT_PLAIN_UTF8)
}

/// The description of `urn:kernel:aliases`.
fn aliases() -> Description {
    Description::new("kernel-aliases")
        .title("Logical rewrite table")
        .summary(
            "The installed logical-rewrite table as a resource: which logical names are \
             rewritten, to what, and — the part that matters when something is mysteriously \
             not resolving — how often each rule has fired, and how often it fired onto a \
             name nothing was bound to. Needs no tracer installed.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_INSPECT)
        .output(TEXT_PLAIN_UTF8)
}

/// The description of `urn:kernel:scheduler`.
fn scheduler() -> Description {
    Description::new("kernel-scheduler")
        .title("Scheduler readout")
        .summary(
            "The host scheduler as the kernel sees it — backend, threads, live task counts. \
             A kernel with no scheduler injected reports the runtime-free single-threaded \
             default.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_INSPECT)
        .output(TEXT_PLAIN_UTF8)
}

/// The description of `urn:kernel:constraint`.
fn constraint() -> Description {
    Description::new("kernel-constraint")
        .title("Throughput constraint")
        .summary(
            "Where the constraint is right now: the resources that consumed the most \
             *uncached* compute over the recent window, heaviest first. Cache hits are \
             excluded — they cost nothing — so the leader is the bottleneck (Goldratt step \
             one).",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_INSPECT)
        .output(TEXT_PLAIN_UTF8)
}

/// One kernel operation as an [`Endpoint`], for description only.
///
/// It answers `describe()` and nothing else. `invoke` is unreachable on every path
/// that exists — [`KernelOps`] is composed in front of the root space only for the
/// description walks, never installed as a kernel's root — and it refuses loudly
/// rather than returning an empty representation, so a refactor that accidentally
/// routed dispatch through here fails instead of silently answering nothing.
struct KernelOp {
    description: Description,
}

#[async_trait]
impl Endpoint for KernelOp {
    async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
        Err(Error::Endpoint(format!(
            "`{}` is served intrinsically by the kernel, not through a space — this endpoint \
             describes it and cannot invoke it",
            self.description.id
        )))
    }

    fn name(&self) -> &str {
        &self.description.id
    }

    fn describe(&self) -> Description {
        self.description.clone()
    }
}

/// A [`Space`] over the kernel's own `urn:kernel:*` operations.
///
/// Enumerable and `Meta`-resolvable, which is all any description walk needs. The
/// kernel composes it in FRONT of the root space (mirroring the intercept order on
/// the issue path, where the root space cannot shadow `urn:kernel:*` either), but
/// only for those walks — resolution proper never consults it.
pub(crate) struct KernelOps;

impl Space for KernelOps {
    fn resolve(&self, request: &Request, _scope: &Scope) -> Resolution {
        let Some(op) = request.target.as_str().strip_prefix(KERNEL_NS) else {
            return Resolution::Miss;
        };
        match description(op) {
            Some(description) => Resolution::Hit(Resolved::new(
                Arc::new(KernelOp { description }),
                Bindings::new(),
            )),
            None => Resolution::Miss,
        }
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        Some(
            OPS.iter()
                .map(|op| {
                    let id = description(op)
                        .expect("every listed op describes itself")
                        .id;
                    SpaceEntry::new(format!("{KERNEL_NS}{op}"), id)
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_op_describes_itself_with_a_safe_id() {
        for op in OPS {
            let description = description(op).expect("listed op has a description");
            assert!(
                description.validate().is_ok(),
                "`{op}` description is not IRI-safe: {:?}",
                description.validate()
            );
            assert!(
                description.id.starts_with("kernel-"),
                "`{op}` id `{}` is not namespaced to the kernel",
                description.id
            );
            assert!(
                description.verbs.contains(&Verb::Meta),
                "`{op}` must answer Meta — it is how the catalog describes it"
            );
            assert!(
                !description.title.is_empty() && !description.summary.is_empty(),
                "`{op}` needs a title and a summary — it is a published tool contract"
            );
        }
    }

    #[test]
    fn the_space_enumerates_and_resolves_exactly_the_listed_ops() {
        let space = KernelOps;
        let entries = space.entries().expect("enumerable");
        assert_eq!(entries.len(), OPS.len());
        for entry in &entries {
            let iri = crate::iri::Iri::parse(&entry.pattern).expect("op patterns are exact IRIs");
            let Resolution::Hit(resolved) =
                space.resolve(&Request::new(Verb::Meta, iri), &Scope::empty())
            else {
                panic!("`{}` enumerated but did not resolve", entry.pattern);
            };
            assert_eq!(resolved.endpoint.name(), entry.endpoint);
        }
        // A name outside the namespace, and an unserved name inside it, both miss.
        for miss in ["urn:example:toUpper", "urn:kernel:nonesuch"] {
            let iri = crate::iri::Iri::parse(miss).expect("valid IRI");
            assert!(
                matches!(
                    space.resolve(&Request::new(Verb::Meta, iri), &Scope::empty()),
                    Resolution::Miss
                ),
                "`{miss}` must not resolve in the kernel-op space"
            );
        }
    }

    #[test]
    fn meta_is_never_a_gated_action() {
        // `action_specs()` drops Meta, so no `requires` can attach to it — which is
        // why describing a kernel operation is ungated. Pin it: the day Meta became
        // selectable, every one of these descriptions would start claiming authority
        // the Meta arm does not enforce.
        for op in OPS {
            assert!(
                required_scopes(op, Verb::Meta).is_empty(),
                "`{op}` declares a capability for Meta, which is never enforced"
            );
        }
    }
}
