use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::arg::ArgRef;
use crate::capability::Capability;
use crate::describe::Description;
use crate::error::{Error, Result};
use crate::grammar::Bindings;
use crate::iri::Iri;
use crate::repr::{Expiry, Representation, Thread, Time};
use crate::request::Request;
use crate::select::{ActionMatch, TransreptionPolicy, TransreptionStep};
use crate::space::{Scope, Space};
use crate::uncached::{DepKind, VolatileDeps};
use crate::verb::Verb;

/// Lets an endpoint issue sub-requests back through the kernel. Implemented by
/// the [`Kernel`](crate::Kernel); a detached [`Invocation`] has no issuer, so
/// `source`/`issue` are unavailable when testing an endpoint in isolation.
// clippy 1.99's `double_must_use` fires inside the code `#[async_trait]` GENERATES for an async trait
// method: the macro marks the boxed-future return `#[must_use]`, and a pinned boxed `Future` is
// already must-use. The attribute is the macro's, not ours, so the lint has nothing here to fix;
// scoped to this trait (its generated methods) rather than the crate, so it covers nothing we write.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Issuer: Send + Sync {
    /// Resolve and evaluate a sub-request.
    async fn issue(&self, request: Request, capability: &Capability) -> Result<Representation>;

    /// Like [`issue`](Issuer::issue), but carrying the issuing invocation's trace
    /// `parent` span — so a recorded execution links each sub-request to the node
    /// that issued it (the tree the `trace` command renders). The default ignores it
    /// and delegates to `issue`; the kernel overrides it to thread the span. `parent`
    /// is `None` outside tracing, so this is free off the trace path.
    async fn issue_with_parent(
        &self,
        request: Request,
        capability: &Capability,
        parent: Option<u64>,
    ) -> Result<Representation> {
        let _ = parent;
        self.issue(request, capability).await
    }

    /// Like [`issue_with_parent`](Issuer::issue_with_parent), additionally carrying
    /// the [`TraceScope`](crate::TraceScope) of the resolution this sub-request
    /// belongs to — so concurrent traced resolutions on one shared kernel each
    /// record into their *own* collector, never a neighbor's. The default drops the
    /// scope and delegates (a detached or remote issuer has no trace to record
    /// into); the kernel overrides it. `trace` is `None` off the trace path.
    async fn issue_scoped(
        &self,
        request: Request,
        capability: &Capability,
        parent: Option<u64>,
        trace: Option<crate::TraceScope>,
    ) -> Result<Representation> {
        let _ = trace;
        self.issue_with_parent(request, capability, parent).await
    }

    /// Like [`issue_scoped`](Issuer::issue_scoped), additionally carrying the
    /// **resolution chain** ([`Scope`]) the sub-request must resolve in — the
    /// corridors injected ahead of the root, and whether the root is in the chain
    /// at all. The kernel overrides it to resolve in exactly that chain; that is
    /// what makes a confined endpoint's sub-requests confined and an injected
    /// corridor visible to every sub-request of the request it was injected for.
    ///
    /// **The default honors the empty chain and refuses every other.** An empty
    /// scope delegates to `issue_scoped`, so every issuer written before this
    /// method existed behaves exactly as it did. A non-empty scope is refused with
    /// [`Error::Endpoint`] rather than dropped: an issuer that cannot carry the
    /// chain (a detached one, a module host bridge, anything that forwards over a
    /// wire that has no field for it) would otherwise resolve the sub-request in
    /// the plain root chain — inside a confinement that looked like it held, under
    /// a corridor the host believed was in force — on the branch that reads like
    /// success. Refusing names the hole. Override it to carry the chain.
    async fn issue_in_scope(
        &self,
        request: Request,
        capability: &Capability,
        parent: Option<u64>,
        trace: Option<crate::TraceScope>,
        scope: Scope,
    ) -> Result<Representation> {
        if !scope.is_empty() {
            return Err(Error::Endpoint(format!(
                "sub-request for {} carries a resolution scope ({scope}) this issuer \
                 cannot honor — it implements only the plain `issue` seam — so it was \
                 refused rather than resolved outside the scope",
                request.target
            )));
        }
        self.issue_scoped(request, capability, parent, trace).await
    }

    /// Like [`issue_in_scope`](Issuer::issue_in_scope), additionally carrying the
    /// **nesting depth** the sub-request runs at — one deeper than the invocation
    /// that issued it — so the kernel can refuse it past its budget
    /// ([`Kernel::with_max_depth`](crate::Kernel::with_max_depth)) with
    /// [`Error::DepthExceeded`] instead of recursing until the stack dies. This is
    /// the seam [`Invocation`] calls; the kernel overrides it.
    ///
    /// **The default drops the depth and delegates** to `issue_in_scope`, so every
    /// issuer written before this method existed keeps compiling and behaving as it
    /// did. That is a deliberate asymmetry with `issue_in_scope`'s refusal of a
    /// chain it cannot carry: a dropped chain resolves a request *somewhere it should
    /// not*, silently; a dropped depth only stops counting at this issuer, and the
    /// count resumes at zero on the far side. The budget therefore bounds nesting
    /// **within one kernel**: it does not cross a module host bridge that implements
    /// only `issue`, and it does not cross the wire (`ikigai-wire`'s `Call` has no
    /// field for it), so two peers that mount each other remain unbounded by it.
    /// Carrying it further is a protocol decision, not a default.
    async fn issue_at_depth(
        &self,
        request: Request,
        capability: &Capability,
        parent: Option<u64>,
        trace: Option<crate::TraceScope>,
        scope: Scope,
        depth: u32,
    ) -> Result<Representation> {
        let _ = depth;
        self.issue_in_scope(request, capability, parent, trace, scope)
            .await
    }

    /// Like [`issue_at_depth`](Issuer::issue_at_depth), additionally returning the
    /// [`Dependencies`] the sub-request's own resolution recorded **when it failed**.
    /// This is the seam [`Invocation`] calls.
    ///
    /// A success needs no second channel: its expiry and golden threads travel on the
    /// [`Representation`]. A failure has nowhere to put them, and before this seam
    /// they were lost at the `?` that returned the error. That mattered for exactly
    /// one shape: a composite whose `NotFound` came from something IT read. A caller
    /// that caught that `NotFound` and cached a fallback hung it from the composite's
    /// name alone, a thread nobody cuts, so a later write to the atom underneath left
    /// the fallback cached and wrong (ledger #611). With the failure's own
    /// dependencies carried back, the fallback hangs from the atom too.
    ///
    /// **The default carries nothing** and delegates to `issue_at_depth`, so every
    /// issuer written before this method existed behaves exactly as it did: the
    /// failure is recorded under the requested name only. The kernel overrides it. An
    /// issuer that forwards over a wire (a module host bridge, a remote peer) keeps
    /// the default until its protocol has a field for the set; the residue is the old
    /// rule, which is the conservative-enough one it always was.
    async fn issue_recording(
        &self,
        request: Request,
        capability: &Capability,
        parent: Option<u64>,
        trace: Option<crate::TraceScope>,
        scope: Scope,
        depth: u32,
    ) -> (Result<Representation>, Dependencies) {
        let result = self
            .issue_at_depth(request, capability, parent, trace, scope, depth)
            .await;
        (result, Dependencies::none())
    }

    /// Merge a subtree of [`TraceEvent`](crate::TraceEvent)s produced by *another*
    /// kernel — a remote one reached through a mounted `RemoteSpace` — into this
    /// issuer's trace, re-based under `parent` (the span of the invocation that
    /// forwarded the request). The default ignores them (a detached or remote issuer
    /// has no trace to merge into); the kernel overrides it to re-map the span ids
    /// and record. Reached by an endpoint through [`Invocation::record_subtree`].
    fn record_subtree(&self, parent: Option<u64>, spans: Vec<crate::TraceEvent>) {
        let _ = (parent, spans);
    }

    /// The current time per the issuer's injected [`Clock`](crate::Clock), or
    /// `None` if it has none. An endpoint computing a time-based deadline (e.g.
    /// `now + max-age`) reads it through [`Invocation::now`]. Default `None`.
    fn now(&self) -> Option<Time> {
        None
    }

    /// Plan a chain of transreptors converting media type `from` → `to` over the
    /// issuer's mounted spaces (see [`select_transreptor`](crate::select_transreptor)).
    /// The default offers none — a detached or remote issuer can't enumerate spaces; the
    /// kernel overrides it to select over its root. An endpoint reads it through
    /// [`Invocation::select_transreptor`].
    fn select_transreptor(&self, from: &str, to: &str) -> Option<Vec<TransreptionStep>> {
        let _ = (from, to);
        None
    }

    /// Find endpoints whose required inputs are satisfiable by the RDF classes in `present`
    /// (see [`select_action`](crate::select_action)) — "given these typed entities, what can
    /// I do with them?" The default offers none (a detached or remote issuer can't enumerate
    /// spaces); the kernel overrides it. An endpoint reads it through
    /// [`Invocation::select_action`].
    fn select_action(&self, present: &[&str]) -> Vec<ActionMatch> {
        let _ = present;
        Vec::new()
    }

    /// [`select_transreptor`](Issuer::select_transreptor) **in a resolution chain**
    /// — the plan among what `scope` can resolve, which is what an endpoint's
    /// sub-requests in that chain could actually run. The kernel overrides it
    /// ([`Kernel::select_transreptor_in`](crate::Kernel::select_transreptor_in));
    /// [`Invocation::select_transreptor`] reads it with the invocation's own chain.
    ///
    /// **The default honors the empty chain and offers nothing in every other.**
    /// An empty scope delegates to `select_transreptor`, so an issuer written before
    /// this method existed behaves exactly as it did; a non-empty scope gets `None`
    /// rather than the root's plan — an issuer that cannot select in the chain must
    /// not offer a step the chain cannot then resolve (the same fail-closed shape as
    /// [`issue_in_scope`](Issuer::issue_in_scope), which would refuse the step
    /// anyway). Override it to select in the chain.
    fn select_transreptor_in(
        &self,
        from: &str,
        to: &str,
        scope: &Scope,
    ) -> Option<Vec<TransreptionStep>> {
        if !scope.is_empty() {
            return None;
        }
        self.select_transreptor(from, to)
    }

    /// [`select_action`](Issuer::select_action) **in a resolution chain** — the
    /// manifold of what `scope` can resolve. The kernel overrides it
    /// ([`Kernel::select_action_in`](crate::Kernel::select_action_in));
    /// [`Invocation::select_action`] reads it with the invocation's own chain.
    ///
    /// **The default honors the empty chain and offers nothing in every other**,
    /// for the reason [`select_transreptor_in`](Issuer::select_transreptor_in)
    /// gives: an offer the chain cannot resolve is the over-offer the manifold
    /// exists to prevent. Override it to select in the chain.
    fn select_action_in(&self, present: &[&str], scope: &Scope) -> Vec<ActionMatch> {
        if !scope.is_empty() {
            return Vec::new();
        }
        self.select_action(present)
    }

    /// [`select_transreptor_in`](Issuer::select_transreptor_in) under an explicit
    /// [`TransreptionPolicy`] — the seam a caller consents to a lossy edge through.
    /// The kernel overrides it
    /// ([`Kernel::select_transreptor_in_with`](crate::Kernel::select_transreptor_in_with));
    /// [`Invocation::select_transreptor_with`] reads it with the invocation's own chain.
    ///
    /// **The default honors the lossless policy and offers nothing under any other.**
    /// A lossless-only policy is exactly `select_transreptor_in`, so an issuer written
    /// before this method existed answers as it did; a policy that admits lossy edges
    /// gets `None`, because an issuer that cannot see the declarations cannot vouch
    /// that a plan it offers is the one the caller consented to — fail closed, as
    /// `select_transreptor_in` does for a chain it cannot select in.
    fn select_transreptor_in_with(
        &self,
        from: &str,
        to: &str,
        scope: &Scope,
        policy: &TransreptionPolicy,
    ) -> Option<Vec<TransreptionStep>> {
        if policy.allows_lossy() {
            return None;
        }
        self.select_transreptor_in(from, to, scope)
    }
}

/// A pinned, boxed, `Send` future — the unit of work a [`Spawner`] runs.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Runs futures concurrently on the host's executor. Injected into the kernel like
/// [`Clock`](crate::Clock) — via [`Kernel::into_scheduled`](crate::Kernel::into_scheduled)
/// — and object-safe so it can be a trait object; `ikigai-scheduler` implements it.
/// With no spawner, [`Invocation::fan_out`] falls back to sequential resolution, so
/// the kernel stays runtime-free and single-threaded by default.
pub trait Spawner: Send + Sync {
    /// Spawn `task` to run concurrently; the returned future resolves when it
    /// completes, so a caller can join several spawned tasks — **parking**, not
    /// blocking, until they finish (which is what keeps re-entrant fan-out from
    /// pinning a thread while its children run).
    fn spawn(&self, task: BoxFuture<()>) -> BoxFuture<()>;

    /// How many spawned tasks can make progress **simultaneously** — this executor's
    /// achievable concurrency, and only that. It is **not** a queue depth, not a count
    /// of tasks outstanding or completed, and not how many branches a caller intends to
    /// dispatch: it is how many of them would actually be running at one instant if the
    /// caller handed over more work than the executor can carry at once.
    ///
    /// A caller reads it to *size* a fan-out — to know how much concurrency a shared
    /// downstream (an inference backend, a rate-limited API) will really see — so an
    /// honest small answer is worth more than a flattering large one.
    ///
    /// **A single-threaded executor answers `Some(1)`, never `None`.** An executor that
    /// runs one task to completion before the next has width 1, and saying so is the
    /// entire point of this accessor. That covers the inline case as well as the
    /// threaded one: a spawner that returns the task's own future to be polled
    /// cooperatively on the calling thread interleaves nothing when the work inside
    /// blocks, so its width is 1, not the number of tasks handed to it.
    ///
    /// `None` means **unknown** — reserved for a spawner that genuinely cannot answer
    /// (an elastic or remote pool whose size is not observable). It is not a shorthand
    /// for "small". Returning it forces the caller to guess, and the damaging guess is
    /// the likely one: read as "wide", a serialized workload gets routed to a batching
    /// backend and runs slower than sequencing it would have.
    ///
    /// Defaulted to `None` so every existing implementor keeps compiling untouched;
    /// override it wherever the number is known. This is a **read**, deliberately: the
    /// kernel never drives the scheduler, which lives above it and stays runtime-free
    /// (see [`SchedulerReporter`](crate::SchedulerReporter)), so there is no setter and
    /// no resize — a host that changes its executor's width reports the new number here.
    fn width(&self) -> Option<usize> {
        None
    }
}

/// The context handed to an endpoint when it is invoked.
///
/// All input arrives here — the request, the grammar-captured bindings, and the
/// authorizing capability — and, when the kernel is driving, the ability to
/// issue sub-requests via [`Invocation::source`] / [`Invocation::issue`].
/// Endpoints take no ambient authority.
///
/// ## No public path takes a capability
///
/// [`Capability::root`](crate::Capability::root) and
/// [`Capability::scoped`](crate::Capability::scoped) are public, so any code in the
/// process can *construct* a capability of any strength. What makes in-process
/// non-escalation structural is that an endpoint has no way to hand one to an
/// issuer: every sub-request form on this type — [`issue`](Self::issue) and
/// [`source`](Self::source) (this invocation's capability, verbatim),
/// [`issue_attenuated`](Self::issue_attenuated) and
/// [`source_attenuated`](Self::source_attenuated) (a meet, strictly weaker),
/// [`fan_out`](Self::fan_out) (a clone per spawned request) — routes through a
/// **private** `issue_under(request, capability)`, and nothing public takes a
/// capability. That privacy is the visibility half of `docs/formalism/README.md`
/// R2.2, and this block pins it: it fails to compile with E0624, *method
/// `issue_under` is private*, and for no other reason.
///
/// ```compile_fail,E0624
/// use ikigai_core::{Bindings, Capability, Invocation, Iri, Request, Verb};
///
/// let request = Request::new(Verb::Source, Iri::parse("urn:x").unwrap());
/// let bindings = Bindings::new();
/// let weak = Capability::scoped(["urn:cap:read"]);
/// let inv = Invocation::detached(&request, &bindings, &weak);
///
/// // An endpoint holding a weak capability can construct a strong one…
/// let strong = Capability::root();
/// // …and has no way to issue under it. `issue_under` is private.
/// let _ = inv.issue_under(Request::new(Verb::Source, Iri::parse("urn:y").unwrap()), &strong);
/// ```
pub struct Invocation<'a> {
    /// The request being served.
    pub request: &'a Request,
    /// Variables captured by the grammar that resolved this request.
    pub bindings: &'a Bindings,
    /// The capability authorizing invocation.
    pub capability: &'a Capability,
    issuer: Option<&'a dyn Issuer>,
    /// Concurrency context for [`fan_out`](Invocation::fan_out): the host's spawner
    /// and an *owned* issuer handle (so a spawned sub-request can re-enter the kernel
    /// without borrowing this invocation). Both present only on a
    /// [`scheduled`](crate::Kernel::into_scheduled) kernel; otherwise fan-out is
    /// sequential.
    spawner: Option<Arc<dyn Spawner>>,
    issuer_arc: Option<Arc<dyn Issuer>>,
    /// An explicitly attached source of "now", overriding the issuer's. Set only by
    /// [`with_clock`](Invocation::with_clock) — the kernel never sets it, because a
    /// kernel-driven invocation already reads the kernel's clock through its issuer.
    clock: Option<Arc<dyn crate::Clock>>,
    /// This invocation's trace span, when the kernel is recording — so a sub-request
    /// it issues is linked to this node as its parent. `None` off the trace path.
    span: Option<u64>,
    /// The trace scope this invocation records into, when the kernel is recording —
    /// threaded into every sub-request so concurrent traced resolutions on one
    /// shared kernel stay isolated. `None` off the trace path.
    trace: Option<crate::TraceScope>,
    /// The resolution chain this invocation's request was resolved in, inherited
    /// by every sub-request it issues. Set by the kernel; the only change an
    /// endpoint can make to it is [`confine`](Invocation::confine), which narrows.
    scope: Scope,
    /// How deeply this invocation is nested: 0 for a request the host issued, one
    /// more for each sub-request between it and this one. Set by the kernel, read
    /// by every sub-request form to hand the kernel `depth + 1`, and the number the
    /// kernel's budget ([`Kernel::with_max_depth`](crate::Kernel::with_max_depth))
    /// is checked against. Private: an endpoint cannot reset it, for the reason it
    /// cannot set its own chain or its own authority.
    depth: u32,
    /// Everything the endpoint records while it runs, shared by every reborrow of
    /// this invocation. See [`Recorded`].
    recorded: Arc<Recorded>,
}

/// The recording side of an [`Invocation`]: what the endpoint accumulated while it
/// ran, for the kernel to drain once it returns.
///
/// Held behind an `Arc` so that a reborrow ([`Invocation::with_bindings`]) shares
/// it rather than starting a fresh set. That is the whole difference between a
/// reborrow and a copy: a sub-request issued through the reborrowed handle is a
/// dependency of the **same** invocation, so its expiry and its golden threads
/// reach the kernel whichever handle the endpoint happened to use. A per-reborrow
/// set would drop them on the floor when the reborrow died — silently, and on the
/// branch that looks like success, which is the shape this type exists to avoid.
#[derive(Default)]
struct Recorded {
    /// Facts the endpoint attached to its own span via
    /// [`Invocation::trace_note`]; drained by the kernel into the
    /// [`TraceEvent`](crate::TraceEvent) once the invocation completes. Only
    /// collected while tracing (no cost, no growth off the trace path).
    trace_notes: Mutex<Vec<(String, String)>>,
    deps: Mutex<Deps>,
    /// Union of the golden threads of every sub-resource resolved during this
    /// invocation — so the kernel can propagate them onto the result.
    dep_threads: Mutex<BTreeSet<Thread>>,
}

/// What a resolution depended on before it FAILED: the meet of its sub-requests'
/// expiries and the union of their golden threads, plus the thread named after its
/// own canonical target — the set a success carries on its [`Representation`], for
/// the case that has no representation to carry it.
///
/// Returned beside the error by [`Issuer::issue_recording`], and folded by the
/// issuing [`Invocation`] into its own dependency record, so a composite that
/// catches a failed sub-request's `NotFound` and returns a fallback hangs from what
/// the failure depended on as well as from the name it asked for (ledger #611).
///
/// ```
/// use ikigai_core::{Dependencies, Expiry};
///
/// // What an issuer that cannot say reports: no edges, no limit.
/// let none = Dependencies::none();
/// assert!(none.threads.is_empty());
/// assert_eq!(none.expiry, Expiry::Never);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Dependencies {
    /// The meet of the failed resolution's dependency expiries — `Never` when it
    /// read nothing, `Always` when something it read was volatile or refused.
    pub expiry: Expiry,
    /// The golden threads it hung from before it failed.
    pub threads: BTreeSet<Thread>,
}

impl Dependencies {
    /// No dependencies: no edges and no limit ([`Expiry::Never`], the meet's
    /// identity). What an issuer that cannot say reports.
    pub fn none() -> Self {
        Dependencies {
            expiry: Expiry::Never,
            threads: BTreeSet::new(),
        }
    }
}

impl Default for Dependencies {
    /// [`Dependencies::none`] — not `Expiry::default()`, which is `Always`.
    fn default() -> Self {
        Dependencies::none()
    }
}

/// What the sub-requests of one invocation did to its cacheability: the meet of
/// their expiries, and — for the ones that made it `Always` — their names, so the
/// kernel can say WHY a composite was not cached ([`crate::UNCACHED_NOTE`]). One
/// lock for both, taken once per sub-request as the expiry alone always was.
struct Deps {
    expiry: Expiry,
    volatile: VolatileDeps,
}

impl Default for Deps {
    /// No dependencies impose no limit: `Never`, the meet's identity — not
    /// `Expiry::default()`, which is `Always`.
    fn default() -> Self {
        Deps {
            expiry: Expiry::Never,
            volatile: VolatileDeps::default(),
        }
    }
}

impl Deps {
    fn meet(&mut self, expiry: Expiry) {
        self.expiry = self.expiry.most_restrictive(expiry);
    }
}

impl Recorded {
    /// Record the outcome of one sub-request for `requested` as a dependency of the
    /// invocation — **whether or not it succeeded**.
    ///
    /// A success contributes its expiry and its golden threads, as ever. A failure
    /// contributes too, by error class, because an endpoint that catches the failure
    /// and returns a cacheable fallback is otherwise cached with nothing to invalidate
    /// it — hole B of [#512](http://localhost:1060/l/default/item/512):
    ///
    /// - [`Error::Unresolved`] / [`Error::NotFound`]: a thread named after the missing
    ///   resource, so a `Sink` that later creates it (or a watcher that sees it appear)
    ///   cuts every composite built on its absence. The name is the one the kernel
    ///   reports for `Unresolved` — canonical, after every rewrite the kernel applied
    ///   — and the name that was *requested* for `NotFound`, which carries no IRI of
    ///   its own; under an alias those can differ, and a `Sink` through the alias
    ///   cuts the canonical one. The kernel closes that gap from the other side: a
    ///   `NotFound` its own resolution returned carries the canonical name in
    ///   `carried` (below).
    ///
    ///   **And the failure's own dependencies** (`carried`, from
    ///   [`Issuer::issue_recording`]): the threads the failed resolution hung from
    ///   before it failed, and the meet of their expiries. When the `NotFound` came
    ///   from a COMPOSITE — `formula:{ref}` is `NotFound` because `input:{ref}` is, or
    ///   because what `input:{ref}` holds is not a formula — the thread on the
    ///   requested name alone is one no write cuts, and a fallback over it went
    ///   silently stale when the atom underneath was written (ledger #611). An
    ///   `Unresolved` the kernel returned itself never reached an endpoint, so it
    ///   carries nothing; one an endpoint propagated carries what that endpoint read.
    /// - [`Error::Denied`]: [`Expiry::Always`]. A grant change has no thread, so a
    ///   result built on a refusal must not be cached at all.
    /// - Every other error (`Endpoint`, `Timeout`, `Unavailable`, `DepthExceeded`,
    ///   `Conflict`, a bad argument): `Always` as well, conservatively — the kernel
    ///   cannot name what would make the failure go away, and not caching is never
    ///   wrong. For `DepthExceeded` this is the paper's B.7 obligation: a refusal at
    ///   the bound is never served to a shallower request, even wrapped in a fallback.
    ///   For [`Error::Conflict`] a thread would look tempting — a change of state is
    ///   what clears it — but the state that refused is whatever the endpoint
    ///   consulted, not the name that was requested (a move conflicts on the board's
    ///   cells, not on the move's own IRI), so a thread on `requested` is one no write
    ///   would ever cut.
    fn record(&self, requested: &Iri, result: &Result<Representation>, carried: Dependencies) {
        match result {
            Ok(representation) => {
                let mut deps = self.deps.lock().expect("deps lock");
                deps.meet(representation.expiry);
                if representation.expiry == Expiry::Always {
                    deps.volatile.note(DepKind::Volatile, requested);
                }
                drop(deps);
                // Inherit the sub-resource's golden threads so cutting any of them
                // invalidates this (composite) result too.
                self.dep_threads
                    .lock()
                    .expect("dep threads lock")
                    .extend(representation.threads().iter().cloned());
            }
            Err(Error::Unresolved(canonical)) => {
                let name = Thread::from(canonical.as_str());
                self.record_miss(requested, name, carried);
            }
            Err(Error::NotFound(_)) => {
                let name = Thread::from(requested.as_str());
                self.record_miss(requested, name, carried);
            }
            Err(error) => {
                let kind = match error {
                    Error::Denied(_) => DepKind::Denied,
                    _ => DepKind::Failed,
                };
                let mut deps = self.deps.lock().expect("deps lock");
                deps.meet(Expiry::Always);
                deps.volatile.note(kind, requested);
            }
        }
    }

    /// A miss (`Unresolved` / `NotFound`): an edge on the missing `name`, and
    /// whatever the failed resolution itself depended on (`carried`) — its threads,
    /// and its expiry met into this invocation's, as a success's would be.
    fn record_miss(&self, requested: &Iri, name: Thread, carried: Dependencies) {
        if carried.expiry != Expiry::Never {
            let mut deps = self.deps.lock().expect("deps lock");
            deps.meet(carried.expiry);
            if carried.expiry == Expiry::Always {
                deps.volatile.note(DepKind::Volatile, requested);
            }
        }
        let mut threads = self.dep_threads.lock().expect("dep threads lock");
        threads.insert(name);
        threads.extend(carried.threads);
    }
}

impl<'a> Invocation<'a> {
    /// A context with no kernel attached: `source`/`issue` are unavailable.
    /// Useful for invoking an endpoint directly in tests.
    ///
    /// ## ★ Nothing fills in the bindings for you
    ///
    /// `bindings` is whatever the caller passes, and the caller is a test, so the
    /// usual value is `Bindings::default()` — **empty**. Template capture happens
    /// during resolution: [`Kernel::issue`](crate::Kernel::issue) matches the target
    /// against the grammars in scope and hands the endpoint whatever the matching
    /// [`Grammar`](crate::Grammar) captured. A detached invocation skips that step
    /// entirely, so an endpoint bound to `urn:thing:{app}` invoked detached sees no
    /// `app`, and behaves exactly as it would for the bare `urn:thing` — silently,
    /// on the branch that reads like success.
    ///
    /// That has already cost a session a failing test that looked like a golden-thread
    /// bug: the endpoint was reading the right layers, the test was resolving an IRI
    /// with an `{app}` segment in it, and the binding was never there to be read.
    /// If the behavior under test depends on a captured variable, state it:
    ///
    /// ```
    /// # use ikigai_core::{Bindings, Capability, Invocation, Iri, Request, Verb};
    /// # let request = Request::new(Verb::Source, Iri::parse("urn:thing:cms-web").unwrap());
    /// # let capability = Capability::root();
    /// let mut bindings = Bindings::new();
    /// bindings.insert("app", "cms-web"); // the grammar would have captured this
    /// let inv = Invocation::detached(&request, &bindings, &capability);
    /// assert_eq!(inv.bindings.get("app"), Some("cms-web"));
    /// ```
    ///
    /// The IRI is not the source of truth here and writing it out in full does not
    /// help: the endpoint reads `inv.bindings`, not the target's text.
    pub fn detached(
        request: &'a Request,
        bindings: &'a Bindings,
        capability: &'a Capability,
    ) -> Self {
        Invocation {
            request,
            bindings,
            capability,
            issuer: None,
            spawner: None,
            issuer_arc: None,
            clock: None,
            span: None,
            trace: None,
            scope: Scope::empty(),
            depth: 0,
            recorded: Arc::new(Recorded::default()),
        }
    }

    /// A context backed by an issuer, enabling sub-requests (`source`/`issue`).
    ///
    /// The kernel builds these with itself as the issuer. It's also the seam a
    /// dynamically-loaded **module** uses: a module shim runs its endpoint with a
    /// *host-backed* issuer, so the endpoint's `inv.source`/`inv.issue` resolve
    /// against the host kernel (its cache, its other spaces) across the module
    /// boundary — the endpoint code is unchanged, only the issuer is remote.
    pub fn with_issuer(
        request: &'a Request,
        bindings: &'a Bindings,
        capability: &'a Capability,
        issuer: &'a dyn Issuer,
    ) -> Self {
        Invocation {
            request,
            bindings,
            capability,
            issuer: Some(issuer),
            spawner: None,
            issuer_arc: None,
            clock: None,
            span: None,
            trace: None,
            scope: Scope::empty(),
            depth: 0,
            recorded: Arc::new(Recorded::default()),
        }
    }

    /// **The same invocation, with different [`bindings`](Self::bindings).** A
    /// reborrow — every handle to the kernel comes across, and what the sub-context
    /// records is recorded against this invocation.
    ///
    /// ## Why it has to exist in core
    ///
    /// `bindings` is a shared reference, so an endpoint that dispatches to more than
    /// one *target* had no way to say "same invocation, different captures". The only
    /// public constructor that takes bindings is [`detached`](Self::detached), and
    /// detached is not a substitute: it drops the issuer, the spawner, the owned
    /// issuer handle, the clock, the span and the trace scope, severing the endpoint
    /// from the kernel. So the workaround for a missing reborrow was to keep the
    /// bindings you already had.
    ///
    /// That is exactly what `ikigai-throttle`'s `Failover` did: candidate 2 was
    /// invoked with **candidate 1's** grammar captures. When the candidates match
    /// different grammars (`urn:x/{id}` against `urn:x/{name}`), candidate 2 reads
    /// `None` for its own variable and behaves as it would for the bare target —
    /// silently, on the branch that looks like success. Benign only because failover
    /// targets happen to be mirrors in practice.
    ///
    /// ## What comes across
    ///
    /// Everything: the request, the capability, the issuer, the spawner and owned
    /// issuer handle used by [`fan_out`](Self::fan_out), an attached clock, the trace
    /// span and scope. And the *recording* side is **shared, not copied** — a
    /// sub-request issued through the reborrow is a dependency of this invocation, so
    /// its expiry and golden threads reach the kernel from either handle. A copy
    /// would have lost them when the reborrow dropped, which is the same silent shape
    /// this method exists to close.
    ///
    /// The returned invocation borrows `self`, so it cannot outlive the endpoint's
    /// own context — which is the point: it is a view, not a second life.
    ///
    /// ```
    /// # use ikigai_core::{Bindings, Capability, Invocation, Iri, Request, Verb};
    /// # let request = Request::new(Verb::Source, Iri::parse("urn:x/7").unwrap());
    /// # let capability = Capability::root();
    /// let mut first = Bindings::new();
    /// first.insert("id", "7");
    /// let inv = Invocation::detached(&request, &first, &capability);
    ///
    /// let mut second = Bindings::new();
    /// second.insert("name", "seven"); // the NEXT candidate's grammar captured this
    /// let candidate = inv.with_bindings(&second);
    ///
    /// assert_eq!(candidate.bindings.get("name"), Some("seven"));
    /// assert_eq!(candidate.bindings.get("id"), None); // not candidate 1's captures
    /// assert_eq!(inv.bindings.get("id"), Some("7")); // and the original is untouched
    /// ```
    pub fn with_bindings<'b>(&'b self, bindings: &'b Bindings) -> Invocation<'b> {
        Invocation {
            request: self.request,
            bindings,
            capability: self.capability,
            issuer: self.issuer,
            spawner: self.spawner.clone(),
            issuer_arc: self.issuer_arc.clone(),
            clock: self.clock.clone(),
            span: self.span,
            trace: self.trace.clone(),
            scope: self.scope.clone(),
            depth: self.depth,
            // Shared, deliberately: see the doc above. The reborrow records INTO
            // this invocation, not beside it.
            recorded: Arc::clone(&self.recorded),
        }
    }

    /// **The same invocation, confined to `space`.** A reborrow like
    /// [`with_bindings`](Self::with_bindings) — every handle to the kernel comes
    /// across and the recording side is shared — whose sub-requests resolve in the
    /// chain [`Scope::confined`] describes: everything the host injected, then
    /// `space` where the root used to be, and **no root**. A sub-request for
    /// anything the chain does not bind is [`Error::Unresolved`], never
    /// [`Error::Denied`], and the root endpoint it would have reached is never
    /// entered. This is the trapdoor: the paper's point is that a denial is a
    /// decision that can be misconfigured, while an unresolvable identifier has
    /// nowhere to go.
    ///
    /// `name` is the corridor's identity for the cache — see [`Scope`] on what
    /// naming claims — so two confinements naming the same `name` share cached
    /// answers and must therefore bind the same doors.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::sync::atomic::{AtomicU32, Ordering};
    /// use futures::executor::block_on;
    /// use ikigai_core::{
    ///     AsyncFnEndpoint, Capability, EndpointSpace, Error, Exact, FnEndpoint, Iri, Kernel,
    ///     ReprType, Representation, Request, Verb,
    /// };
    ///
    /// // A root with a secret in it, counting how often it is entered.
    /// let reached = Arc::new(AtomicU32::new(0));
    /// let secret = {
    ///     let reached = Arc::clone(&reached);
    ///     FnEndpoint::new("secret", move |_| {
    ///         reached.fetch_add(1, Ordering::SeqCst);
    ///         Ok(Representation::new(ReprType::new("text/plain"), b"s3cr3t".to_vec()))
    ///     })
    /// };
    /// // An endpoint that confines ITSELF to a corridor holding one document, then
    /// // reads through the confined handle.
    /// let extractor = AsyncFnEndpoint::new("extract", |inv| {
    ///     Box::pin(async move {
    ///         let document = Arc::new(EndpointSpace::new().bind(
    ///             Exact::new("urn:doc:1"),
    ///             FnEndpoint::new("doc", |_| {
    ///                 Ok(Representation::new(ReprType::new("text/plain"), b"the document".to_vec()))
    ///             }),
    ///         ));
    ///         assert!(inv.scope().is_empty()); // the plain root, before
    ///         let confined = inv.confine(Iri::parse("urn:ctx:doc:1").unwrap(), document);
    ///         assert!(confined.scope().is_severed()); // no root, after…
    ///         assert!(inv.scope().is_empty()); // …and the original handle is untouched
    ///
    ///         // What the corridor binds resolves…
    ///         let doc = confined.source(&Iri::parse("urn:doc:1").unwrap()).await?;
    ///         assert_eq!(doc.bytes, b"the document");
    ///         // …and a root-bound name is unresolvable: not denied, nowhere to go.
    ///         let err = confined.source(&Iri::parse("urn:data:secret").unwrap()).await.unwrap_err();
    ///         assert!(matches!(err, Error::Unresolved(ref t) if t.as_str() == "urn:data:secret"));
    ///         Ok(doc)
    ///     })
    /// });
    /// let kernel = Kernel::new(Arc::new(
    ///     EndpointSpace::new()
    ///         .bind(Exact::new("urn:data:secret"), secret)
    ///         .bind(Exact::new("urn:extract"), extractor),
    /// ));
    ///
    /// let out = block_on(kernel.issue(
    ///     Request::new(Verb::Source, Iri::parse("urn:extract").unwrap()),
    ///     &Capability::root(),
    /// ))
    /// .unwrap();
    /// assert_eq!(out.bytes, b"the document");
    /// // Even under ROOT authority, the secret endpoint was never entered.
    /// assert_eq!(reached.load(Ordering::SeqCst), 0);
    /// ```
    ///
    /// ## Why this is the only chain-changing operation an endpoint gets
    ///
    /// It narrows, structurally. Relative to the chain this invocation runs in,
    /// the confined chain resolves nothing differently except what the **root**
    /// would have answered: `space` sits behind every injected corridor, so it
    /// cannot shadow one, and the root's doors are exactly what confinement
    /// exists to remove. The worst outcome of calling it is an `Unresolved`. The
    /// widening counterpart — injecting a corridor ahead of the root, with the
    /// root still present — is [`Kernel::issue_in`](crate::Kernel::issue_in),
    /// reachable only by whoever holds the kernel, for the same reason a
    /// sub-request's *authority* is taken from the kernel and never from its
    /// caller (see `issue_under` in this file): a corridor placed innermost can
    /// stand in for any door, for every sub-request below it.
    ///
    /// [`Confine`](crate::Confine) is this, as an endpoint decorator.
    pub fn confine<'b>(&'b self, name: Iri, space: Arc<dyn Space>) -> Invocation<'b> {
        Invocation {
            request: self.request,
            bindings: self.bindings,
            capability: self.capability,
            issuer: self.issuer,
            spawner: self.spawner.clone(),
            issuer_arc: self.issuer_arc.clone(),
            clock: self.clock.clone(),
            span: self.span,
            trace: self.trace.clone(),
            scope: self.scope.clone().confined(name, space),
            depth: self.depth,
            recorded: Arc::clone(&self.recorded),
        }
    }

    /// The resolution chain this invocation runs in — what its sub-requests
    /// resolve against. Empty (nothing injected, root present) for every plain
    /// [`Kernel::issue`](crate::Kernel::issue); [`severed`](Scope::is_severed)
    /// inside a [`confine`](Self::confine).
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Attach the resolution chain the request was resolved in (set by the
    /// kernel), so sub-requests resolve in the same chain. Crate-private on
    /// purpose: a public setter would let an endpoint hand a widening chain to its
    /// own sub-requests, which is the one thing [`confine`](Self::confine) is
    /// shaped to make impossible.
    ///
    /// That is the visibility half of `docs/formalism/README.md` R7.5 ("no endpoint
    /// can set its own chain"), and this block pins it: from outside the crate it
    /// fails to compile with E0624, *method `with_scope` is private*, and for no
    /// other reason.
    ///
    /// ```compile_fail,E0624
    /// use ikigai_core::{Bindings, Capability, Invocation, Iri, Request, Scope, Verb};
    ///
    /// let request = Request::new(Verb::Source, Iri::parse("urn:x").unwrap());
    /// let bindings = Bindings::new();
    /// let cap = Capability::root();
    /// let inv = Invocation::detached(&request, &bindings, &cap);
    ///
    /// // An endpoint cannot hand its sub-requests a chain of its own choosing.
    /// let _ = inv.with_scope(Scope::empty());
    /// ```
    pub(crate) fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    /// How deeply this invocation is nested: 0 for a request the host issued, and
    /// one more for each sub-request between that and this one. Every sub-request
    /// this invocation issues — through [`issue`](Self::issue), [`source`](Self::source),
    /// the attenuated forms, [`fan_out`](Self::fan_out) across a spawn, or a
    /// [`scope_sync`](Self::scope_sync) bridge — runs at `depth() + 1`, and the
    /// kernel refuses one past its budget
    /// ([`Kernel::with_max_depth`](crate::Kernel::with_max_depth)) with
    /// [`Error::DepthExceeded`]. A detached invocation is at 0.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// Attach the nesting depth (set by the kernel, from the depth it was asked to
    /// run the request at). Crate-private for the reason [`with_scope`](Self::with_scope)
    /// is: an endpoint that could reset its depth could reset the budget.
    pub(crate) fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    /// Attach an explicit source of "now", so [`now`](Self::now) answers without an
    /// issuer to read it from.
    ///
    /// **This exists for the detached case.** A kernel-driven invocation already has a
    /// clock — the kernel's, reached through its issuer — and the kernel does not call
    /// this. What had no answer at all was
    /// [`detached`](Self::detached): `now()` is the issuer's clock, a detached
    /// invocation has no issuer, so an endpoint that stamps its output from the kernel
    /// clock returned `None` in every detached test. Silently, and on the branch that
    /// reads like success — which is the part that cost something. By 2026-08-23
    /// `ikigai-browse` had five endpoints stamping `derived_at` from `now()` and not one
    /// test that had ever seen them produce a timestamp.
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use ikigai_core::{Bindings, Capability, FixedClock, Invocation, Iri, Request, Verb};
    /// # let request = Request::new(Verb::Source, Iri::parse("urn:example").unwrap());
    /// # let bindings = Bindings::default();
    /// # let capability = Capability::root();
    /// let inv = Invocation::detached(&request, &bindings, &capability)
    ///     .with_clock(Arc::new(FixedClock::at(1_700_000_000_000)));
    /// assert_eq!(inv.now().map(|t| t.as_millis()), Some(1_700_000_000_000));
    /// ```
    ///
    /// An explicitly attached clock **wins over the issuer's**, on the general rule that
    /// what a caller stated beats what it inherited. Nothing in the kernel path reaches
    /// this, so the two cannot disagree in production.
    ///
    /// It does not make the detached test the *better* one. A detached invocation skips
    /// grammar-driven argument routing and kernel-side capability enforcement, so an
    /// endpoint exercised only that way is exercised only in part; the fuller test is a
    /// real [`Kernel`](crate::Kernel) with [`with_clock`](crate::Kernel::with_clock),
    /// and [`FixedClock`](crate::FixedClock) is there to make that one cheap too. This
    /// is here so that reaching for the kernel clock never costs an endpoint author its
    /// unit tests — the outcome that pushes an author toward `SystemTime::now()`, which
    /// is both unmockable and not wasm-clean.
    pub fn with_clock(mut self, clock: Arc<dyn crate::Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Attach this invocation's trace span (set by the kernel when recording), so
    /// sub-requests issued from it link to this node as their parent.
    pub(crate) fn with_span(mut self, span: Option<u64>) -> Self {
        self.span = span;
        self
    }

    /// Attach the trace scope this invocation belongs to (set by the kernel when
    /// recording), threaded into sub-requests so they record into the same trace.
    pub(crate) fn with_trace(mut self, trace: Option<crate::TraceScope>) -> Self {
        self.trace = trace;
        self
    }

    /// Attach the concurrency context — the injected [`Spawner`] and an owned
    /// [`Issuer`] handle — so [`fan_out`](Self::fan_out) can spawn sub-requests
    /// concurrently. Set by the kernel when it has been made schedulable via
    /// [`Kernel::into_scheduled`](crate::Kernel::into_scheduled).
    pub(crate) fn with_concurrency(
        mut self,
        spawner: Option<Arc<dyn Spawner>>,
        issuer_arc: Option<Arc<dyn Issuer>>,
    ) -> Self {
        self.spawner = spawner;
        self.issuer_arc = issuer_arc;
        self
    }

    /// This invocation's trace span, or `None` when the kernel isn't recording. An
    /// endpoint that forwards to another kernel checks this to decide whether to
    /// trace the forward, then passes it — via [`record_subtree`](Self::record_subtree)
    /// — as the parent to re-base the returned spans under.
    pub fn trace_span(&self) -> Option<u64> {
        self.span
    }

    /// Attach a `key = value` fact to this invocation's own trace span — e.g. the
    /// LLM facade noting `model` / `provider` it resolved to, or the HTTP client
    /// noting the redirect hops it followed. A no-op unless this resolution is
    /// being traced, so it is free on the hot path; notes land on the
    /// [`TraceEvent`](crate::TraceEvent) the kernel records for this node.
    pub fn trace_note(&self, key: impl Into<String>, value: impl Into<String>) {
        if self.trace.is_none() {
            return;
        }
        self.recorded
            .trace_notes
            .lock()
            .expect("trace notes lock")
            .push((key.into(), value.into()));
    }

    /// Drain the notes recorded during this invocation (kernel-side, at
    /// trace-record time).
    pub(crate) fn take_trace_notes(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.recorded.trace_notes.lock().expect("trace notes lock"))
    }

    /// Merge a subtree of [`TraceEvent`](crate::TraceEvent)s from another kernel into
    /// this invocation's trace, re-based under this node — so a resolution forwarded
    /// to a remote kernel (through a mounted `RemoteSpace`) shows the remote's
    /// execution stitched under the mount, not collapsed into one node. A no-op off
    /// the trace path or when detached.
    pub fn record_subtree(&self, spans: Vec<crate::TraceEvent>) {
        if let Some(issuer) = self.issuer {
            issuer.record_subtree(self.span, spans);
        }
    }

    /// The bytes of an inline argument, or an error if absent / not inline.
    pub fn inline_arg(&self, name: &str) -> Result<&[u8]> {
        match self.request.args.get(name) {
            Some(ArgRef::Inline(bytes)) => Ok(bytes),
            Some(_) => Err(Error::InvalidArgument {
                name: name.to_string(),
                detail: "expected an inline value".to_string(),
            }),
            None => Err(Error::MissingArgument(name.to_string())),
        }
    }

    /// An inline argument decoded as UTF-8.
    pub fn inline_str(&self, name: &str) -> Result<&str> {
        std::str::from_utf8(self.inline_arg(name)?).map_err(|_| Error::InvalidArgument {
            name: name.to_string(),
            detail: "not valid UTF-8".to_string(),
        })
    }

    /// Issue a sub-request through the kernel, recording it as a dependency of
    /// this invocation's result so expiry propagates. Errors if detached.
    ///
    /// **A failed sub-request is recorded too.** `Unresolved` and `NotFound` add a
    /// golden thread named after the missing resource, so a fallback built on its
    /// absence is cut when it appears; `Denied` and every other error make this
    /// invocation's result uncacheable. An endpoint that catches an error and
    /// returns `.cacheable()` therefore gets exactly the cacheability its inputs
    /// warrant — which, for a swallowed denial, is none. The rules and the reason
    /// for each are on `Recorded::record` in this file.
    ///
    /// The sub-request runs one nesting level deeper than this invocation
    /// ([`depth`](Self::depth)); past the kernel's budget it is refused with
    /// [`Error::DepthExceeded`].
    ///
    /// The sub-request runs under **this invocation's own capability**, unchanged.
    /// To narrow it first — the shape a module wants when it is about to resolve a
    /// caller-supplied IRI — use
    /// [`issue_attenuated`](Self::issue_attenuated).
    pub async fn issue(&self, request: Request) -> Result<Representation> {
        self.issue_under(request, self.capability).await
    }

    /// Issue a sub-request under a **strictly weaker** authority: this invocation's
    /// capability [`attenuate`](crate::Capability::attenuate)d to `scopes`.
    ///
    /// `Root` narrows to exactly `scopes`; a scoped capability narrows to the
    /// intersection. There is no widening counterpart and there must never be one —
    /// see the note on `issue_under` in this file for why that is load-bearing — so
    /// the worst outcome of calling this is a refusal further down.
    ///
    /// ## What it is for
    ///
    /// A module that dereferences a **caller-supplied** IRI is resolving a target it
    /// did not choose, with every scope its caller happens to hold. Dropping the ones
    /// the module does not need turns "the caller named `urn:secret:prod-db` and my
    /// caller can read secrets" from an exfiltration into a `Denied`. The narrowing is
    /// voluntary: the module still holds `self.capability` and can still call
    /// [`issue`](Self::issue), so this defends the module's *downstream*, not the
    /// module itself. That is the only thing an in-process, self-applied restriction
    /// can honestly claim.
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use futures::executor::block_on;
    /// # use ikigai_core::{
    /// #     builtins, ArgRef, Capability, EndpointSpace, Exact, Iri, Kernel, Request, Verb,
    /// # };
    /// # let space = EndpointSpace::new().bind(Exact::new("urn:fn:toUpper"), builtins::to_upper());
    /// # let kernel = Kernel::new(Arc::new(space));
    /// // Held by the caller, but not something this module's sub-requests need.
    /// let caller = Capability::root().attenuate(["urn:cap:secret:read", "urn:cap:fs:read"]);
    /// let narrowed = caller.attenuate(["urn:cap:fs:read"]);
    /// assert!(!narrowed.allows("urn:cap:secret:read"));
    /// // …and asking for it back does not bring it back.
    /// assert!(!narrowed
    ///     .attenuate(["urn:cap:secret:read"])
    ///     .allows("urn:cap:secret:read"));
    /// ```
    pub async fn issue_attenuated<I, S>(
        &self,
        request: Request,
        scopes: I,
    ) -> Result<Representation>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let attenuated = self.capability.attenuate(scopes);
        self.issue_under(request, &attenuated).await
    }

    /// The one place a sub-request's authority is chosen.
    ///
    /// ★ **This is private, and that is a security property, not an oversight.**
    /// [`Capability::root`](crate::Capability::root) and
    /// [`Capability::scoped`](crate::Capability::scoped) are both public — any code in
    /// the process can *construct* a capability value of any strength. What makes
    /// in-process non-escalation structural is that an endpoint has no way to hand one
    /// to an issuer: `Invocation` owns the only reachable path to the kernel and never
    /// offers to take a capability. A public `issue_under` would hand every endpoint
    /// root authority in one line. Any future authority-carrying form must therefore
    /// take the authority **from the kernel**, never from its caller.
    async fn issue_under(
        &self,
        request: Request,
        capability: &Capability,
    ) -> Result<Representation> {
        let issuer = self
            .issuer
            .ok_or_else(|| Error::Endpoint("sub-requests require a kernel context".to_string()))?;
        // Kept for the failure record: `NotFound` carries no IRI, and the request
        // is moved into the issuer. One clone per sub-request, on the path whose
        // floor is a cache hit two orders of magnitude dearer.
        let requested = request.target.clone();
        let (result, carried) = issuer
            .issue_recording(
                request,
                capability,
                self.span,
                self.trace.clone(),
                self.scope.clone(),
                self.depth + 1,
            )
            .await;
        // Recorded on BOTH branches — a failure is a dependency too (see
        // `Recorded::record`): the endpoint may catch it and return a cacheable
        // fallback, and that fallback must hang from something — from the name it
        // asked for AND from what the failure itself read (`carried`).
        self.recorded.record(&requested, &result, carried);
        result
    }

    /// `SOURCE` another resource — dereference a by-reference argument — recording
    /// it as a dependency.
    pub async fn source(&self, target: &Iri) -> Result<Representation> {
        self.issue(Request::new(Verb::Source, target.clone())).await
    }

    /// `SOURCE` another resource under a **strictly weaker** authority — this
    /// invocation's capability narrowed to `scopes` — recording it as a dependency.
    ///
    /// The [`issue_attenuated`](Self::issue_attenuated) form of
    /// [`source`](Self::source), and the shape that case usually wants: a module
    /// dereferencing an IRI its caller named should be reaching for read authority
    /// over one family and nothing else.
    pub async fn source_attenuated<I, S>(&self, target: &Iri, scopes: I) -> Result<Representation>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.issue_attenuated(Request::new(Verb::Source, target.clone()), scopes)
            .await
    }

    /// Plan a transreptor chain converting media type `from` → `to` over what
    /// **this invocation's resolution chain** can resolve — the same chain its
    /// sub-requests run in ([`scope`](Self::scope)) — or `None` if there's no kernel
    /// context (detached) or no chain exists. The endpoint then issues each
    /// [`TransreptionStep`] — piping the bytes in as `content` and setting `as` to
    /// the step's target — to run the conversion. This is the seam
    /// content-negotiation and octet-stream sniff-and-dispatch build on: "find me a
    /// way from type A to type B," then drive it through the kernel like any
    /// sub-request.
    ///
    /// An endpoint does not have to know it is confined to get an honest plan:
    /// inside a [`confine`](Self::confine) the plan names only transreptors the
    /// severed chain reaches, and under a host-injected corridor a transreptor the
    /// corridor shadows is the one named — exactly what the step would resolve to.
    pub fn select_transreptor(&self, from: &str, to: &str) -> Option<Vec<TransreptionStep>> {
        self.issuer?.select_transreptor_in(from, to, &self.scope)
    }

    /// [`select_transreptor`](Self::select_transreptor) under an explicit
    /// [`TransreptionPolicy`] — how an endpoint that has its caller's consent (an
    /// `lossy=allow` argument of its own, say) plans through a declared projection.
    /// The plan reports each lossy step; the endpoint decides what to tell its caller.
    pub fn select_transreptor_with(
        &self,
        from: &str,
        to: &str,
        policy: &TransreptionPolicy,
    ) -> Option<Vec<TransreptionStep>> {
        self.issuer?
            .select_transreptor_in_with(from, to, &self.scope, policy)
    }

    /// Find endpoints whose required inputs are satisfiable by the RDF classes in `present`
    /// — the actions available given a set of typed entities (see
    /// [`select_action`](crate::select_action)) — among what **this invocation's
    /// resolution chain** can resolve ([`scope`](Self::scope)). Empty if there's no
    /// kernel context (detached). The seed of layer action-inference: a layer endpoint
    /// can surface "what you can do with what's on the canvas," then issue the chosen
    /// one — and inside a [`confine`](Self::confine) the list never names a root-only
    /// action the confined sub-request could not then resolve.
    pub fn select_action(&self, present: &[&str]) -> Vec<ActionMatch> {
        match self.issuer {
            Some(issuer) => issuer.select_action_in(present, &self.scope),
            None => Vec::new(),
        }
    }

    /// Resolve `requests` **concurrently**, returning their results in request order.
    ///
    /// On a [`scheduled`](crate::Kernel::into_scheduled) kernel each sub-request is
    /// spawned as its own task and the join *parks* — so a re-entrant fan-out (e.g.
    /// `compose` expanding several `$a{}` markers) never holds a thread while its
    /// children run, and a child can run on the thread the parent released. Without a
    /// spawner it falls back to sequential [`issue`](Self::issue) — the kernel's
    /// default single-threaded behavior. Either way each result's expiry and golden
    /// threads are recorded as dependencies of this invocation, exactly like `issue`
    /// — the failed branches included, by the same rules — and every branch runs
    /// one nesting level deeper than this invocation.
    pub async fn fan_out(&self, requests: Vec<Request>) -> Vec<Result<Representation>> {
        let (Some(spawner), Some(issuer)) = (&self.spawner, &self.issuer_arc) else {
            // Sequential fallback: same order, same dependency recording as `issue`.
            let mut results = Vec::with_capacity(requests.len());
            for request in requests {
                results.push(self.issue(request).await);
            }
            return results;
        };

        // Spawn each sub-request into its own slot, then join (parking) on all.
        // The slot keeps the requested target beside the result, for the failure
        // record (`NotFound` carries no IRI of its own).
        type Slot = Arc<Mutex<Option<(Result<Representation>, Dependencies)>>>;
        let slots: Vec<Slot> = requests
            .iter()
            .map(|_| Arc::new(Mutex::new(None)))
            .collect();
        let targets: Vec<Iri> = requests.iter().map(|r| r.target.clone()).collect();
        let joins: Vec<BoxFuture<()>> = requests
            .into_iter()
            .zip(&slots)
            .map(|(request, slot)| {
                let issuer = Arc::clone(issuer);
                let capability = self.capability.clone();
                let slot = Arc::clone(slot);
                // Carry this invocation's span AND trace scope across the spawn, so
                // each spawned sub-request links to this node as its parent and
                // records into this resolution's own trace — that's what lets the
                // recorded events reconstruct the real (concurrent) execution tree
                // without bleeding into a concurrently-traced neighbor.
                let parent = self.span;
                let trace = self.trace.clone();
                // …and the resolution chain: a spawned sub-request is still a
                // sub-request of this invocation, so a confinement it runs in
                // holds across the spawn.
                let scope = self.scope.clone();
                // …and the nesting depth: a spawned branch is one deeper than
                // this invocation, exactly as a sequential `issue` would be.
                let depth = self.depth + 1;
                spawner.spawn(Box::pin(async move {
                    let outcome = issuer
                        .issue_recording(request, &capability, parent, trace, scope, depth)
                        .await;
                    *slot.lock().expect("fan-out slot") = Some(outcome);
                }))
            })
            .collect();
        futures_util::future::join_all(joins).await;

        // Collect in order; record each branch as a dependency — the failed ones
        // too, by the same rules as `issue` (see `Recorded::record`).
        let mut results = Vec::with_capacity(slots.len());
        for (slot, requested) in slots.into_iter().zip(&targets) {
            let (result, carried) = slot
                .lock()
                .expect("fan-out slot")
                .take()
                .expect("spawned fan-out task completed");
            self.recorded.record(requested, &result, carried);
            results.push(result);
        }
        results
    }

    /// Run a **synchronous** closure on its own thread, giving it a cloneable,
    /// `'static`, blocking [`SyncIssuer`] whose calls are served by THIS
    /// invocation — the bridge every embedded evaluator needs.
    ///
    /// The problem this solves: [`issue`](Self::issue) is async and borrows the
    /// invocation, but a sync embedded runtime (a Steel `register_fn` builtin,
    /// a Python callable under the GIL, a JS threadsafe function) needs a
    /// `Send + Sync + 'static` handle it can call BLOCKING, and a naive
    /// `block_on` inside would nest executors and deadlock. ikigai-lisp proved
    /// the working shape — a dedicated thread plus a channel the async side
    /// drains — and this is that bridge, in core, for every consumer.
    ///
    /// Mechanics: `f` runs on a fresh thread holding a [`SyncIssuer`]; each
    /// `issuer.issue(req)` crosses a channel and is served here via
    /// [`issue`](Self::issue) — so capability attenuation and enforcement,
    /// cache dependency/golden-thread recording, and trace parentage are all
    /// EXACTLY as if the endpoint had issued the sub-request itself. The scope
    /// returns when `f` does (all issuer clones dropped ⇒ the drain ends).
    /// Sub-requests are served one at a time, in arrival order.
    ///
    /// A panic in `f` surfaces as an error, not a poisoned kernel. Requires a
    /// kernel context (errors when detached) and real threads (unavailable
    /// under wasm — module endpoints there use the host-call seam instead).
    #[cfg(not(target_family = "wasm"))]
    pub async fn scope_sync<R, F>(&self, f: F) -> Result<R>
    where
        R: Send + 'static,
        F: FnOnce(SyncIssuer) -> R + Send + 'static,
    {
        use futures_util::StreamExt;
        if self.issuer.is_none() {
            return Err(Error::Endpoint(
                "sub-requests require a kernel context".to_string(),
            ));
        }
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<SyncCall>();
        let handle = std::thread::Builder::new()
            .name("ikigai-sync-scope".to_string())
            .spawn(move || f(SyncIssuer { tx }))
            .map_err(|e| Error::Endpoint(format!("sync scope thread failed to start: {e}")))?;
        // Serve the closure's sub-requests until every issuer clone is gone —
        // which is when `f` has returned (or unwound). Each one goes through
        // `self.issue`, so this invocation records it as a dependency.
        while let Some(call) = rx.next().await {
            let result = self.issue(call.request).await;
            // A dropped receiver just means the closure gave up waiting; the
            // dependency accounting above already happened, so nothing to undo.
            let _ = call.reply.send(result);
        }
        // The channel closed, so `f` is done; this join is immediate.
        handle
            .join()
            .map_err(|_| Error::Endpoint("sync scope panicked".to_string()))
    }

    /// The current time **as this invocation should see it**, or `None` if nothing
    /// supplies one. In order: a clock attached with [`with_clock`](Self::with_clock)
    /// (what a caller stated beats what it inherited); then the **resolution
    /// chain's** clock — the one a temporal corridor derived at injection
    /// ([`Scope::with_named_at`]), so an endpoint resolved as-of a pinned instant
    /// reads that instant here as well as through `urn:time:now`, without knowing
    /// which seam it used; then the issuer's — the kernel's injected
    /// [`Clock`](crate::Clock). An endpoint turns a relative freshness window into
    /// an absolute deadline with it — e.g.
    /// `inv.now().map(|t| repr.cacheable_until(t.plus_millis(max_age)))`.
    ///
    /// ★ **Under a pinned chain that deadline is in the corridor's time and is
    /// judged in the kernel's.** The kernel never reads the chain's clock for
    /// validity — a pinned past must not un-expire a live entry — so a window
    /// computed from a pinned past is already expired (never cached) and one from
    /// a pinned future outlives its window. Data that is as-of a pinned instant is
    /// a pure function of its context: declare it [`cacheable`](Representation::cacheable),
    /// not `cacheable_until`.
    ///
    /// A [`detached`](Self::detached) invocation with none of the three has no time
    /// — which is the honest answer, not a fallback to the system clock: reading
    /// the wall clock behind the caller's back is what makes resolution
    /// non-replayable, and core does it in exactly one place, inside
    /// [`SystemClock`](crate::SystemClock), where a host opts into it by name.
    pub fn now(&self) -> Option<Time> {
        self.clock
            .as_ref()
            .map(|clock| clock.now())
            .or_else(|| self.scope.now())
            .or_else(|| self.issuer.and_then(|issuer| issuer.now()))
    }

    /// Combined expiry of the dependencies issued during this invocation: the
    /// [meet](Expiry::most_restrictive) of them all, so the result is no fresher
    /// than its most volatile dependency. `Always` if any is volatile, the earliest
    /// `At` deadline among any time-bounded ones, else `Never` (no deps ⇒ `Never`,
    /// imposing no limit).
    pub(crate) fn dependency_expiry(&self) -> Expiry {
        self.recorded.deps.lock().expect("deps lock").expiry
    }

    /// The sub-requests that made this invocation's dependencies `Always`, by the
    /// name the endpoint requested — drained, like the trace notes, by the kernel
    /// once the invocation completes and only when the result was not cached.
    /// Empty exactly when [`dependency_expiry`](Self::dependency_expiry) is not
    /// `Always`.
    pub(crate) fn take_volatile_dependencies(&self) -> VolatileDeps {
        std::mem::take(&mut self.recorded.deps.lock().expect("deps lock").volatile)
    }

    /// The union of golden threads of every dependency resolved during this
    /// invocation — the kernel unions these onto the result's own threads.
    pub(crate) fn dependency_threads(&self) -> BTreeSet<Thread> {
        self.recorded
            .dep_threads
            .lock()
            .expect("dep threads lock")
            .clone()
    }

    /// What this invocation depended on when its endpoint FAILED: everything its
    /// sub-requests recorded, plus the thread named after `canonical` — the name the
    /// kernel's write-cut fires on, which a caller holding only the requested (maybe
    /// logical) name could not supply. Drained rather than cloned: the invocation is
    /// over, and this is the last read of its record.
    pub(crate) fn failure_dependencies(&self, canonical: &Iri) -> Dependencies {
        let mut threads =
            std::mem::take(&mut *self.recorded.dep_threads.lock().expect("dep threads lock"));
        threads.insert(Thread::from(canonical.as_str()));
        Dependencies {
            expiry: self.dependency_expiry(),
            threads,
        }
    }
}

/// One bridged sub-request: the request and the channel its answer returns on.
#[cfg(not(target_family = "wasm"))]
struct SyncCall {
    request: Request,
    reply: std::sync::mpsc::SyncSender<Result<Representation>>,
}

/// A cloneable, `Send + Sync + 'static`, **blocking** handle for issuing
/// sub-requests from synchronous code — minted by
/// [`Invocation::scope_sync`], served by the invocation that minted it.
///
/// This is what a sync embedded runtime's callbacks capture: a Steel builtin,
/// a Python callable, a JS function. Authority is NOT carried here — every
/// call is resolved under the minting invocation's capability, so a handle
/// cannot widen what its endpoint could reach, and everything it resolves is
/// recorded as a dependency (cache expiry, golden threads, trace parentage)
/// of that invocation's result.
///
/// Blocking [`issue`](Self::issue) parks the CALLING thread (the closure's own
/// dedicated thread), never the kernel's executor. Once the scope that minted
/// this handle has ended, calls fail with a clean error.
#[cfg(not(target_family = "wasm"))]
#[derive(Clone)]
pub struct SyncIssuer {
    tx: futures_channel::mpsc::UnboundedSender<SyncCall>,
}

#[cfg(not(target_family = "wasm"))]
impl SyncIssuer {
    /// Issue a sub-request and BLOCK until its representation (or error)
    /// comes back. Fails cleanly when the minting scope has ended.
    pub fn issue(&self, request: Request) -> Result<Representation> {
        let (reply, rx) = std::sync::mpsc::sync_channel(1);
        self.tx
            .unbounded_send(SyncCall { request, reply })
            .map_err(|_| Error::Endpoint("the sync scope has ended".to_string()))?;
        rx.recv()
            .map_err(|_| Error::Endpoint("the sync scope ended mid-request".to_string()))?
    }

    /// `SOURCE` a resource by IRI — the common case, as sugar.
    pub fn source(&self, target: &Iri) -> Result<Representation> {
        self.issue(Request::new(Verb::Source, target.clone()))
    }
}

/// An endpoint produces a [`Representation`] in response to a request.
///
/// Endpoints are synchronous and free of ambient authority in M1: everything
/// they may use arrives through the [`Invocation`]. (Async execution and
/// sub-request issuing are introduced with the kernel.)
// clippy 1.99's `double_must_use` fires inside the code `#[async_trait]` GENERATES for an async trait
// method: the macro marks the boxed-future return `#[must_use]`, and a pinned boxed `Future` is
// already must-use. The attribute is the macro's, not ours, so the lint has nothing here to fix;
// scoped to this trait (its generated methods) rather than the crate, so it covers nothing we write.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Endpoint: Send + Sync {
    /// Produce a representation for the invocation.
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation>;

    /// A short label for diagnostics.
    fn name(&self) -> &str {
        "endpoint"
    }

    /// A structured self-description, which `ikigai-vocab` can project to RDF.
    /// The default reports just the endpoint's name.
    fn describe(&self) -> Description {
        Description::new(self.name())
    }

    /// Whether this endpoint is the distinguished ⊥ a [`Limit`](crate::Limit)
    /// resolves to. `false` for every endpoint that does work — the default, and
    /// the only answer an implementor outside core should ever give.
    ///
    /// The kernel asks this of every resolved endpoint, right after resolution
    /// and before the capability floor, the cache and dispatch: a `true` makes
    /// the request [`Unresolved`](crate::Error::Unresolved), byte-identical to a
    /// name bound nowhere, and nothing further happens. Every `entries → Meta →
    /// describe` walk drops a hit on ⊥, so a limited name is offered by no
    /// manifold. Defaulted so that adding it cost no implementor a line: a
    /// limiter is a hit on a known endpoint, not a third
    /// [`Resolution`](crate::Resolution) outcome, precisely so that nobody's
    /// `match` had to change.
    fn is_limiter(&self) -> bool {
        false
    }

    /// The corridor this endpoint confines its sub-requests to, when it is a
    /// [`Confine`](crate::Confine): the confined space's structure, named by the
    /// confinement's name. `None` — the default — for every endpoint that runs in
    /// the chain it was called in.
    ///
    /// A door reports it as `ik:confinedTo` in `urn:kernel:topology`, so the
    /// corridor a door severs into is part of the arrangement's graph rather than
    /// something only the endpoint knows. An overlay that wraps one endpoint (a
    /// governor, a retry) should forward it, as it forwards
    /// [`describe`](Self::describe); one that does not reports its door as
    /// unconfined, which is the answer it gives about everything else too.
    fn confinement(&self) -> Option<crate::topology::Topology> {
        None
    }
}

/// The boxed invocation function behind a [`FnEndpoint`].
type InvokeFn = Box<dyn Fn(&Invocation<'_>) -> Result<Representation> + Send + Sync>;

/// An endpoint backed by a Rust closure — the simplest, idempotent kind.
pub struct FnEndpoint {
    name: String,
    invoke: InvokeFn,
    description: Option<Description>,
}

impl FnEndpoint {
    /// Build an endpoint from a name and an invocation function.
    pub fn new(
        name: impl Into<String>,
        invoke: impl Fn(&Invocation<'_>) -> Result<Representation> + Send + Sync + 'static,
    ) -> Self {
        FnEndpoint {
            name: name.into(),
            invoke: Box::new(invoke),
            description: None,
        }
    }

    /// Attach a self-description declaring this endpoint's parameter contract,
    /// verbs, and outputs (builder). Without one, [`Endpoint::describe`] reports
    /// just the name.
    pub fn with_description(mut self, description: Description) -> Self {
        self.description = Some(description);
        self
    }
}

#[async_trait]
impl Endpoint for FnEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        (self.invoke)(inv)
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn describe(&self) -> Description {
        self.description
            .clone()
            .unwrap_or_else(|| Description::new(&self.name))
    }
}

/// A pinned, boxed, `Send` future that may borrow the invocation it serves —
/// what an [`AsyncFnEndpoint`] closure returns. Unlike [`BoxFuture`] (which is
/// `'static`, for [`Spawner`] tasks) this is bounded by the invocation borrow,
/// which is what lets the future call [`Invocation::issue`] /
/// [`Invocation::source`] on its way to a result.
pub type InvokeFuture<'a> = Pin<Box<dyn Future<Output = Result<Representation>> + Send + 'a>>;

/// The boxed invocation function behind an [`AsyncFnEndpoint`].
type AsyncInvokeFn = Box<dyn for<'a, 'b> Fn(&'a Invocation<'b>) -> InvokeFuture<'a> + Send + Sync>;

/// An endpoint backed by an **async** Rust closure — [`FnEndpoint`]'s twin for
/// the composite case.
///
/// [`FnEndpoint`] takes a sync closure, so an endpoint that issues
/// sub-requests ([`Invocation::issue`] / [`Invocation::source`] are async) has
/// had to hand-implement [`Endpoint`] with `#[async_trait]` plus the
/// name/describe plumbing. This type is that boilerplate, once: the same flat
/// single-verb authoring as `FnEndpoint`, with an async body. Pure async
/// plumbing — no threads, no spawning — so it is wasm-clean.
///
/// The closure returns a boxed future over the invocation borrow (the shape
/// `#[async_trait]` expands to); author it as `|inv| Box::pin(async move
/// { … })`:
///
/// ```
/// use ikigai_core::{ArgRef, AsyncFnEndpoint, Error, Representation, ReprType};
///
/// let upcase_of = AsyncFnEndpoint::new("upcaseOf", |inv| {
///     Box::pin(async move {
///         let src = match inv.request.args.get("src") {
///             Some(ArgRef::Reference(iri)) => iri.clone(),
///             _ => return Err(Error::MissingArgument("src".to_string())),
///         };
///         let body = inv.source(&src).await?; // async sub-request through the kernel
///         Ok(Representation::new(
///             ReprType::new("text/plain"),
///             body.bytes.to_ascii_uppercase(),
///         ))
///     })
/// });
/// ```
pub struct AsyncFnEndpoint {
    name: String,
    invoke: AsyncInvokeFn,
    description: Option<Description>,
}

impl AsyncFnEndpoint {
    /// Build an endpoint from a name and an async invocation function (a
    /// closure returning a boxed [`InvokeFuture`], typically
    /// `|inv| Box::pin(async move { … })`).
    pub fn new<F>(name: impl Into<String>, invoke: F) -> Self
    where
        F: for<'a, 'b> Fn(&'a Invocation<'b>) -> InvokeFuture<'a> + Send + Sync + 'static,
    {
        AsyncFnEndpoint {
            name: name.into(),
            invoke: Box::new(invoke),
            description: None,
        }
    }

    /// Attach a self-description declaring this endpoint's parameter contract,
    /// verbs, and outputs (builder). Without one, [`Endpoint::describe`] reports
    /// just the name.
    pub fn with_description(mut self, description: Description) -> Self {
        self.description = Some(description);
        self
    }
}

#[async_trait]
impl Endpoint for AsyncFnEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        (self.invoke)(inv).await
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn describe(&self) -> Description {
        self.description
            .clone()
            .unwrap_or_else(|| Description::new(&self.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capability;
    use crate::describe::ArgSpec;
    use crate::grammar::{Bindings, Exact};
    use crate::kernel::Kernel;
    use crate::repr::ReprType;
    use crate::space::EndpointSpace;
    use futures::executor::block_on;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn iri(s: &str) -> Iri {
        Iri::parse(s).unwrap()
    }

    #[test]
    fn an_async_closure_endpoint_invokes_detached() {
        // The async twin of FnEndpoint's basic contract: name + closure, invoked
        // directly against a detached invocation (no kernel).
        let greet = AsyncFnEndpoint::new("greet", |inv| {
            Box::pin(async move {
                let who = inv.inline_str("who")?;
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    format!("hi {who}").into_bytes(),
                ))
            })
        });
        assert_eq!(greet.name(), "greet");

        let request = Request::new(Verb::Source, iri("urn:demo:greet"))
            .with_arg("who", ArgRef::Inline(b"ada".to_vec()));
        let bindings = Bindings::default();
        let cap = Capability::root();
        let inv = Invocation::detached(&request, &bindings, &cap);
        let rep = block_on(greet.invoke(&inv)).unwrap();
        assert_eq!(rep.bytes, b"hi ada");
    }

    #[test]
    fn describe_defaults_to_the_name_and_honors_with_description() {
        // Catalog/manifold parity with FnEndpoint: no description reports just
        // the name; with_description reports exactly what was declared.
        let bare = AsyncFnEndpoint::new("bare", |_inv| {
            Box::pin(async { Ok(Representation::new(ReprType::new("text/plain"), Vec::new())) })
        });
        assert_eq!(bare.describe().id, "bare");

        let described = AsyncFnEndpoint::new("described", |_inv| {
            Box::pin(async { Ok(Representation::new(ReprType::new("text/plain"), Vec::new())) })
        })
        .with_description(
            Description::new("described")
                .verb(Verb::Source)
                .requires("urn:cap:demo")
                .input(ArgSpec::new("who")),
        );
        let description = described.describe();
        assert_eq!(description.id, "described");
        assert_eq!(description.verbs, vec![Verb::Source]);
        assert_eq!(description.requires, vec!["urn:cap:demo".to_string()]);
        let inputs: Vec<&str> = description.inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(inputs, ["who"]);
    }

    #[test]
    fn an_async_endpoint_issues_sub_requests_through_the_kernel() {
        // The whole reason the type exists: the closure's future issues a
        // sub-request (async, borrowing the invocation) and the result flows
        // through — no hand-rolled Endpoint impl in sight.
        static LEAF: AtomicU32 = AtomicU32::new(0);
        let leaf = FnEndpoint::new("leaf", |_inv: &Invocation<'_>| {
            LEAF.fetch_add(1, Ordering::SeqCst);
            Ok(
                Representation::new(ReprType::new("text/plain"), b"hello".to_vec())
                    .cacheable()
                    .depends_on("urn:leaf"),
            )
        });
        let upcase_of = AsyncFnEndpoint::new("upcaseOf", |inv| {
            Box::pin(async move {
                let src = match inv.request.args.get("src") {
                    Some(ArgRef::Reference(iri)) => iri.clone(),
                    _ => return Err(Error::MissingArgument("src".to_string())),
                };
                let upstream = inv.source(&src).await?;
                let upper = String::from_utf8_lossy(&upstream.bytes).to_uppercase();
                Ok(
                    Representation::new(ReprType::new("text/plain"), upper.into_bytes())
                        .cacheable(),
                )
            })
        });
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:data:leaf"), leaf)
            .bind(Exact::new("urn:test:upcase-of"), upcase_of);
        let kernel = Kernel::new(Arc::new(space));
        let cap = Capability::root();
        let req = || {
            Request::new(Verb::Source, iri("urn:test:upcase-of"))
                .with_arg("src", ArgRef::Reference(iri("urn:data:leaf")))
        };

        let rep = block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(rep.bytes, b"HELLO");
        assert_eq!(LEAF.load(Ordering::SeqCst), 1);

        // Dependency plumbing flows exactly as through a hand-rolled endpoint:
        // the composite is cached, and cutting the LEAF's golden thread (which
        // the composite never declared itself) invalidates the composite too.
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(LEAF.load(Ordering::SeqCst), 1, "composite + leaf cached");
        kernel.cut("urn:leaf");
        let rep = block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(rep.bytes, b"HELLO");
        assert_eq!(
            LEAF.load(Ordering::SeqCst),
            2,
            "cutting the inherited thread recomputed the composite"
        );
    }

    #[test]
    fn a_detached_async_endpoint_cannot_issue() {
        // Mirror of the detached FnEndpoint behavior: no kernel context means
        // sub-requests fail cleanly, not silently.
        let needs_kernel = AsyncFnEndpoint::new("needsKernel", |inv| {
            Box::pin(async move { inv.source(&iri("urn:data:leaf")).await })
        });
        let request = Request::new(Verb::Source, iri("urn:demo:needsKernel"));
        let bindings = Bindings::default();
        let cap = Capability::root();
        let inv = Invocation::detached(&request, &bindings, &cap);
        let err = block_on(needs_kernel.invoke(&inv)).unwrap_err();
        assert!(
            format!("{err:?}").contains("kernel context"),
            "detached issue fails cleanly: {err:?}"
        );
    }

    // --- Spawner::width (achievable concurrency, read-only) -------------------

    /// The single-threaded shape: the task's own future, polled cooperatively on the
    /// calling thread. Nothing interleaves, so the honest answer is `Some(1)` — never
    /// `None`, which would make a caller guess "wide" about a serialized executor.
    struct SingleThreaded;
    impl Spawner for SingleThreaded {
        fn spawn(&self, task: BoxFuture<()>) -> BoxFuture<()> {
            task
        }
        fn width(&self) -> Option<usize> {
            Some(1)
        }
    }

    /// A pool-shaped spawner reporting the number of tasks it can carry at once.
    struct Pool(usize);
    impl Spawner for Pool {
        fn spawn(&self, task: BoxFuture<()>) -> BoxFuture<()> {
            task
        }
        fn width(&self) -> Option<usize> {
            Some(self.0)
        }
    }

    /// An implementor written before `width` existed, left exactly as it was: it
    /// compiles untouched and answers `None` (unknown).
    struct Unhinted;
    impl Spawner for Unhinted {
        fn spawn(&self, task: BoxFuture<()>) -> BoxFuture<()> {
            task
        }
    }

    #[test]
    fn a_spawner_reports_its_width_through_the_trait_object() {
        // The kernel and the host both hold `Arc<dyn Spawner>`, so the number has to
        // survive dynamic dispatch — that is the whole path a caller reads it over.
        let single: Arc<dyn Spawner> = Arc::new(SingleThreaded);
        let pool: Arc<dyn Spawner> = Arc::new(Pool(8));
        let unhinted: Arc<dyn Spawner> = Arc::new(Unhinted);

        assert_eq!(
            single.width(),
            Some(1),
            "a single-threaded executor says 1, not unknown"
        );
        assert_eq!(pool.width(), Some(8), "a pool reports its achievable width");
        assert_eq!(
            unhinted.width(),
            None,
            "no override means unknown, and the default supplies it"
        );
    }

    #[test]
    fn the_width_default_changes_no_spawn_behavior() {
        // Additive by construction: `width` is a read. Whatever it answers — 1, 8, or
        // unknown — the task still runs exactly as it did before the accessor existed.
        static RAN: AtomicU32 = AtomicU32::new(0);
        let spawners: Vec<Arc<dyn Spawner>> = vec![
            Arc::new(SingleThreaded),
            Arc::new(Pool(8)),
            Arc::new(Unhinted),
        ];
        for spawner in &spawners {
            block_on(spawner.spawn(Box::pin(async {
                RAN.fetch_add(1, Ordering::SeqCst);
            })));
        }
        assert_eq!(
            RAN.load(Ordering::SeqCst),
            3,
            "every spawner ran its task regardless of the width it reports"
        );
    }

    /// The gap this closes. A detached invocation has no issuer, `now()` is the
    /// issuer's clock, so an endpoint that stamps its output from the kernel clock
    /// produced `None` in every detached test — and `None` is a plausible-looking
    /// answer, so the test that asserted around it passed forever.
    #[test]
    fn a_detached_invocation_has_no_time_until_it_is_given_one() {
        let request = Request::new(Verb::Source, iri("urn:demo:stamp"));
        let bindings = Bindings::default();
        let cap = Capability::root();

        let bare = Invocation::detached(&request, &bindings, &cap);
        assert_eq!(bare.now(), None, "no issuer and no clock is no time");

        let stamped = Invocation::detached(&request, &bindings, &cap)
            .with_clock(Arc::new(crate::FixedClock::at(1_700_000_000_000)));
        assert_eq!(stamped.now(), Some(Time::from_millis(1_700_000_000_000)));
    }

    /// An endpoint reading the kernel clock is now testable detached — which is the
    /// whole point, since `detached` is the idiom eight repos test their endpoints
    /// with. Asserted through a real endpoint rather than on `now()` directly,
    /// because what regressed silently was an endpoint's OUTPUT.
    #[test]
    fn an_endpoint_that_stamps_from_the_clock_is_testable_detached() {
        let stamp = FnEndpoint::new("stamp", |inv: &Invocation| {
            let at = inv
                .now()
                .map(|t| t.as_millis().to_string())
                .unwrap_or_else(|| "no clock".to_string());
            Ok(Representation::new(
                ReprType::new("text/plain"),
                at.into_bytes(),
            ))
        });
        let request = Request::new(Verb::Source, iri("urn:demo:stamp"));
        let bindings = Bindings::default();
        let cap = Capability::root();

        let unclocked = Invocation::detached(&request, &bindings, &cap);
        assert_eq!(
            block_on(stamp.invoke(&unclocked)).unwrap().bytes,
            b"no clock",
            "the branch every detached test used to take"
        );

        let clocked = Invocation::detached(&request, &bindings, &cap)
            .with_clock(Arc::new(crate::FixedClock::at(42)));
        assert_eq!(block_on(stamp.invoke(&clocked)).unwrap().bytes, b"42");
    }

    /// An explicitly attached clock beats the issuer's, on the rule that what a
    /// caller stated beats what it inherited. Nothing in the kernel path attaches
    /// one, so the two never disagree in production — but the precedence is a
    /// promise, so it is pinned here rather than left to whichever branch of `now()`
    /// happens to be written first.
    #[test]
    fn an_attached_clock_outranks_the_issuers() {
        let space = EndpointSpace::new().bind(
            Exact::new("urn:demo:stamp"),
            FnEndpoint::new("stamp", |_: &Invocation| {
                Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
            }),
        );
        let kernel = Kernel::new(Arc::new(space)).with_clock(Arc::new(crate::FixedClock::at(1)));

        let request = Request::new(Verb::Source, iri("urn:demo:stamp"));
        let bindings = Bindings::default();
        let cap = Capability::root();

        let inherited = Invocation::with_issuer(&request, &bindings, &cap, &kernel);
        assert_eq!(inherited.now(), Some(Time::from_millis(1)));

        let stated = Invocation::with_issuer(&request, &bindings, &cap, &kernel)
            .with_clock(Arc::new(crate::FixedClock::at(2)));
        assert_eq!(stated.now(), Some(Time::from_millis(2)));
    }

    /// `now()` is answered by the first of three sources that has one: a clock
    /// attached to the invocation, the chain's clock, the issuer's. Crate-private
    /// because only the kernel can put a chain on an invocation, so the middle
    /// rung is reachable from here alone; the two ends are public and pinned by
    /// the integration tests (`temporal_corridor.rs`).
    #[test]
    fn now_prefers_an_attached_clock_then_the_chain_then_the_issuer() {
        use crate::kernel::FixedClock;
        use crate::space::Scope;
        let kernel =
            Kernel::new(Arc::new(EndpointSpace::new())).with_clock(Arc::new(FixedClock::at(3)));
        let request = Request::new(Verb::Source, iri("urn:x"));
        let bindings = Bindings::new();
        let cap = Capability::root();
        let doors: Arc<dyn Space> = Arc::new(EndpointSpace::new());
        let chain =
            Scope::empty().with_named_at(iri("urn:ctx:t"), doors, Arc::new(FixedClock::at(2)));

        let issuer_only = Invocation::with_issuer(&request, &bindings, &cap, &kernel);
        assert_eq!(issuer_only.now(), Some(Time::from_millis(3)));
        let in_chain =
            Invocation::with_issuer(&request, &bindings, &cap, &kernel).with_scope(chain.clone());
        assert_eq!(
            in_chain.now(),
            Some(Time::from_millis(2)),
            "the chain's, over the issuer's"
        );
        let attached = Invocation::with_issuer(&request, &bindings, &cap, &kernel)
            .with_scope(chain)
            .with_clock(Arc::new(FixedClock::at(1)));
        assert_eq!(
            attached.now(),
            Some(Time::from_millis(1)),
            "what was stated, over both"
        );
        assert_eq!(
            Invocation::detached(&request, &bindings, &cap).now(),
            None,
            "nothing supplies one"
        );
    }
}
