//! The resolution kernel: it ties spaces, endpoints, and the representation
//! cache together.
//!
//! [`Kernel::issue`] is `async` so the kernel can be driven by any executor — a
//! multi-threaded thread pool on native (NetKernel-style scheduling), a
//! single-threaded executor in the browser — without `ikigai-core` depending on
//! a runtime. Resolution itself is synchronous (pure routing); the asynchronous
//! work lives in endpoint invocation.
//!
//! A result is cached only when the verb is idempotent *and* the endpoint opted
//! in — permanently ([`Expiry::Never`]) or until a deadline ([`Expiry::At`]).
//! Validity is then tracked two ways, both of which must hold: **golden threads**
//! ([`Thread`]) — a cached representation depends on the threads it declared plus
//! those of every sub-resource it resolved (they propagate up through
//! composition), and [`Kernel::cut`] invalidates everything carrying a thread, so
//! results which read mutable state can be cached and invalidated on change (a
//! `Sink`, or an external watcher, cuts the thread named after the state) — and a
//! **time deadline**, evaluated against an injected [`Clock`], after which an
//! `At` entry is recomputed. A kernel with no clock simply never caches `At`
//! results, staying fully time-independent (and so deterministic for replay).
//!
//! What the cache *keeps* is a separate question from what is *valid*, and it lives
//! in [`cache`](crate::cache): a bounded store under a pluggable
//! [`CachePolicy`](crate::CachePolicy) (LRU at 4096 entries / 64 MiB by default,
//! replaceable via [`with_cache_policy`](Kernel::with_cache_policy)). A policy
//! decides admission and eviction only — it is never consulted on the serving path,
//! so nothing it does can make the kernel hand back a cut or expired representation.
//! That module also carries the **cut sequence** that closes the lost-cut race: a
//! result computed before a cut it should have seen is declined rather than filed
//! against the post-cut generation.
//!
//! The kernel also reserves the **`urn:kernel:*`** namespace for its own
//! operations as capability-gated resources, resolved intrinsically before the
//! root space: `sink urn:kernel:cut <thread>` cuts a thread (so an endpoint or a
//! remote peer can invalidate by *resolving*, not via a special method — and
//! `urn:kernel:bindings`, [`BINDINGS_THREAD`], is the well-known thread meaning
//! "the binding set changed", which every face derived from the bindings hangs
//! from), and
//! `source urn:kernel:cache` / `urn:kernel:threads` introspect cache and threads,
//! and `source urn:kernel:aliases` reads out the installed logical-rewrite table
//! (see [`alias`](crate::alias)) with its per-rule hop and miss counters.
//! `source urn:kernel:catalog` is the self-describing endpoint graph, and
//! `source urn:kernel:actions types=<classes>` lists the endpoints those typed entities
//! could drive (selection as a resource).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use async_trait::async_trait;

use crate::alias::{Alias, AliasHop, AliasTable, Canonical};
use crate::arg::ArgRef;
use crate::cache::{CacheKey, CachePolicy, ReprCache};
use crate::capability::Capability;
use crate::describe::Description;
use crate::endpoint::{Endpoint, Invocation, Issuer, Spawner};
use crate::error::{Error, Result};
use crate::iri::Iri;
use crate::meta::MetaRenderer;
use crate::repr::{Expiry, Provenance, ReprType, Representation, Thread, Time};
use crate::request::Request;
use crate::select::{ActionMatch, TransreptionPolicy, TransreptionStep};
use crate::space::{Fallback, Resolution, Scope, Space, SpaceEntry};
use crate::verb::Verb;

/// The kernel's source of "now". Injected (rather than read from the system
/// directly) so pure resolution stays deterministic and replayable: a test or a
/// replay harness supplies a fixed or recorded clock. Only entries with a
/// time-based [`Expiry::At`] deadline consult it, so a kernel with no clock is
/// fully time-independent.
pub trait Clock: Send + Sync {
    /// The current time.
    fn now(&self) -> Time;
}

/// A [`Clock`] reading the system wall clock — the default real clock. Replay and
/// test harnesses inject a fixed clock instead. (A browser host injects a
/// `Date.now()`-backed clock; `ikigai-core` itself stays runtime-agnostic.)
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Time {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Time::from_millis(millis)
    }
}

/// A [`Clock`] stopped at one instant — the only kind a test should reason against,
/// and the reason this ships from core rather than from each crate that needs one.
///
/// **The duplication is not the argument.** Two crates had written this exact
/// three-line body by 2026-08-23 (`ikigai-lisp::FixedClock`, `ikigai-log::Fixed`),
/// which is friction and no more. The argument is what the friction produced where
/// nobody wrote one: `ikigai-browse` has five endpoints that stamp their output from
/// [`Invocation::now`](crate::Invocation::now) and twenty-three test kernels, none of
/// which installs a clock — so every one of those five has only ever taken its `None`
/// branch under test, and no test would notice if the other branch were wrong. A
/// branch no test has ever entered is behaviour nobody has verified. Making the fixed
/// clock free is how it stops being the expensive option.
///
/// `Copy`, so a kernel and the test that asserts against it can hold the same instant
/// without ceremony:
///
/// ```
/// # use std::sync::Arc;
/// # use ikigai_core::{Clock, FixedClock, Kernel, Space};
/// # fn example(space: Arc<dyn Space>) {
/// let clock = FixedClock::at(1_700_000_000_000);
/// let kernel = Kernel::new(space).with_clock(Arc::new(clock));
/// assert_eq!(clock.now().as_millis(), 1_700_000_000_000);
/// # }
/// ```
///
/// It does not move, deliberately — and there is deliberately no settable twin beside
/// it, though a settable clock is the MORE duplicated of the two shapes:
/// `ikigai-core`'s own tests, `ikigai-http` and `ikigai-cms-web` each carry an
/// `AtomicU64`-backed `TestClock` with a `set`, because testing that an
/// [`Expiry::At`](crate::Expiry) entry goes stale means moving time past a deadline,
/// which this type cannot do. Three copies is a larger count than the two this
/// replaces. It is not a larger case: all three are correct, none has drifted from
/// another, and no behaviour goes untested because of them. The bar for pushing a
/// shape down here is a count *plus* observed drift or unverified behaviour — the
/// same bar `docs/design/ambient-app-name.md` sets — and the settable clock clears
/// only the count. A crate that needs to advance time keeps writing its own until it
/// does; `docs/design/hermetic-endpoint-tests.md` records what would change that.
///
/// So a test that needs two instants builds two of these, and a test that needs time
/// to *move* wants the atomic clock it already has.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FixedClock(Time);

impl FixedClock {
    /// A clock stopped at `millis` since the Unix epoch.
    pub fn at(millis: u64) -> FixedClock {
        FixedClock(Time::from_millis(millis))
    }

    /// A clock stopped at an existing [`Time`] — for advancing a scenario by handing
    /// the next kernel a clock derived from the last one's instant.
    pub fn from_time(time: Time) -> FixedClock {
        FixedClock(time)
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Time {
        self.0
    }
}

/// One resolved invocation, as the kernel reports it to an installed [`Tracer`].
/// The `trace` command turns a stream of these (from one real resolution) into the
/// execution tree — which **worker thread** each node ran on, how long it took, and
/// whether the cache served it.
///
/// Serializable so a traced resolution can cross the wire: a remote kernel records
/// its own events and ships them back to be stitched into the caller's tree.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TraceEvent {
    /// The resolved request's target IRI.
    pub target: String,
    /// The worker thread the invocation ran on (the scheduler's thread name, or the
    /// thread id) — so a fan-out's branches show up on different workers.
    pub thread: String,
    /// When the invocation started / finished, per the injected [`Clock`] (`None`
    /// if the kernel has no clock).
    pub started: Option<Time>,
    pub ended: Option<Time>,
    /// Whether the representation cache served it (vs computed now).
    pub cache_hit: bool,
    /// This invocation's span id (unique within one traced resolution).
    pub span: u64,
    /// The span of the invocation that issued this one — `None` for the root. The
    /// `(span, parent)` edges reconstruct the real execution tree, including the
    /// concurrent branches a `fan_out` spawns onto different workers.
    pub parent: Option<u64>,
    /// The authority this invocation ran under: `None` for root (full authority),
    /// else the sorted scope set. So an attenuation down the call chain — or across
    /// a grant boundary — is visible in the tree, node by node: "under which
    /// capability" alongside "against which cache state".
    pub capability: Option<Vec<String>>,
    /// Facts the endpoint attached to its own span via
    /// [`Invocation::trace_note`](crate::Invocation::trace_note) — e.g. the LLM
    /// facade noting which `model`/`provider` it resolved to, or the HTTP client
    /// noting the redirect hops it followed. Empty unless the endpoint annotated.
    /// (Wire note: adding this field changes the postcard layout — a host shipping
    /// `TraceEvent`s over the wire must bump its protocol version when adopting.)
    pub notes: Vec<(String, String)>,
}

/// The [`TraceEvent::notes`] key under which the kernel reports a CAPABILITY
/// DENIAL, paired with the scope the caller lacked — e.g. `("denied",
/// "urn:cap:fs:read")`. Public so an observer can match the key without
/// re-spelling the literal (a log deciding an entry's class, a dashboard
/// coloring a refused hop).
///
/// A denial event is the one kind that names something which never RAN: the
/// capability floor is enforced before dispatch, so `started`/`ended` are `None`
/// and `cache_hit` is `false`. That is the point of recording it — the endpoint
/// is never entered, so nothing inside it can report the refusal, and without
/// this the refusal reaches no in-kernel observer at all.
pub const DENIED_NOTE: &str = "denied";

/// The [`TraceEvent::notes`] key under which the kernel reports that a sub-request
/// was refused for **nesting past the budget**
/// ([`Kernel::with_max_depth`]), paired with the depth it would have run at — e.g.
/// `("depth", "65")` on a kernel whose bound is 64. Public so an observer can match
/// the key without re-spelling the literal.
///
/// Like a denial, a depth refusal names something which never RAN: the budget is
/// checked before resolution, so `started`/`ended` are `None` and `cache_hit` is
/// `false`, and the event's `parent` is the span of the invocation that asked for
/// one level too many — which is where a self-issuing endpoint or a transclusion
/// cycle shows in the tree. The refusal reaches the caller as
/// [`Error::DepthExceeded`]; the event is how it reaches an observer.
pub const DEPTH_NOTE: &str = "depth";

/// The nesting budget a [`Kernel`] starts with: a sub-request more than this many
/// levels below the host's own request is refused (see
/// [`Kernel::with_max_depth`]). NetKernel ships 40 and defaults to 32 for a counter
/// that also pays for resolution hops; ikigai's counts sub-request nesting only.
pub const DEFAULT_MAX_DEPTH: u32 = 64;

/// The [`TraceEvent::notes`] key under which the kernel reports that a request's
/// target arrived through a **logical rewrite**, paired with the hop — e.g.
/// `("alias", "urn:fn:toUpper -> urn:iki:fn:toUpper")`. Public so an observer can
/// match the key without re-spelling the literal.
///
/// Every event for an aliased request carries it: the computed invocation, the
/// cache hit, the capability denial, and the miss. That last pair is the point.
/// "It resolves under the old name but not the new one" with no indication an
/// alias was involved is a bad afternoon, and a *denial* on a rewritten target is
/// the exact shape that has cost this ecosystem months twice — a capability that
/// fails by simply not holding, with nothing in any log. The note, the per-rule
/// counters at `urn:kernel:aliases`, and the hop named in the denial message are
/// three independent ways to see it, because the tracer is opt-in and the other
/// two are not.
pub const ALIAS_NOTE: &str = "alias";

/// The [`TraceEvent::notes`] key marking that a rewritten target resolved to
/// **nothing** — paired with the backing name that had no binding. It appears only
/// alongside [`ALIAS_NOTE`], on an event that records a resolution which never
/// happened. Ordinary (un-aliased) misses record no event, unchanged: this one
/// exists because the caller asked for a name that *does* look bound and the
/// rewrite is the part they cannot see.
pub const ALIAS_MISS_NOTE: &str = "alias-unresolved";

/// The [`TraceEvent::notes`] key under which the kernel reports the **resolution
/// chain** a request was resolved in, when it is not the empty one — paired with
/// the chain as [`Scope`] renders it, innermost first, ending in `root` or
/// `severed`: e.g. `("scope", "urn:ctx:doc:7 severed")`. Public so an observer
/// can match the key without re-spelling the literal.
///
/// Every event of such a resolution carries it: the computed invocation, the
/// cache hit, the denial, and the miss. A request resolved in the empty chain
/// carries no note, so its events are byte-identical to before scopes existed.
/// This note is a reserved key rather than a field because adding a field to
/// [`TraceEvent`] changes its postcard layout, and a host shipping events over
/// the wire would have to bump its protocol version.
pub const SCOPE_NOTE: &str = "scope";

/// The [`TraceEvent::notes`] key marking that a request resolved to **nothing
/// inside a non-empty chain** — paired with the target that had no binding there.
/// Appears only beside [`SCOPE_NOTE`]. It exists because a miss inside a
/// confinement is deliberately indistinguishable from "no such resource" to the
/// caller (an [`Unresolved`](crate::Error::Unresolved), never a
/// [`Denied`](crate::Error::Denied)) — so the trace is the only place the fact
/// "this name IS bound, just not in here" can show. Ordinary misses in the empty
/// chain record no event, unchanged.
pub const SCOPE_MISS_NOTE: &str = "scope-unresolved";

/// The [`TraceEvent::notes`] key marking that a request resolved onto a
/// **limiter** ([`Limit`](crate::Limit)) — paired with the (canonical) target the
/// limiter admitted, e.g. `("limited", "urn:personal:calendar")`. Public so an
/// observer can match the key without re-spelling the literal.
///
/// The caller's answer is [`Unresolved`](crate::Error::Unresolved), byte-identical
/// to a name bound nowhere — that indistinguishability is the construct's whole
/// point (a boundary that reveals nothing). The trace is the one place the fact can
/// show, and it is the operator's face: whoever reads the trace is whoever placed
/// the limiter, so the note reveals nothing to anyone it was hidden from. Like a
/// denial, the event names something that never RAN: `started`/`ended` are `None`,
/// `cache_hit` is `false`; the alias hop and the chain ride along.
///
/// **Why a limited miss is traced when a plain miss is not.** The rule this crate
/// already follows is not "trace every miss" but *trace a miss whose cause the
/// response deliberately hides*: an aliased miss records ([`ALIAS_MISS_NOTE`] —
/// the rewrite is the part the caller cannot see), a miss inside a non-empty chain
/// records ([`SCOPE_MISS_NOTE`] — the name may well be bound in the root), and a
/// plain miss in the empty chain records nothing, because there is nothing hidden:
/// the caller asked for a name nobody binds and the error says so. A limited miss
/// has a hidden half — the name may be bound just behind the limiter — so it falls
/// on the traced side of that line. Tracing plain misses "for symmetry" would
/// instead change what every existing tracer sees for every typo, a change in what
/// a consumer observes with no code of its own changing; the line stays where it
/// was. Inside a confinement a limited name carries this note, NOT
/// [`SCOPE_MISS_NOTE`]: the chain *has* a door for it and the door is ⊥, which is a
/// different fact from the chain having no door at all, and the operator debugging
/// "why is this unresolved" wants to know which.
pub const LIMITED_NOTE: &str = "limited";

/// The [`TraceEvent::notes`] key under which the kernel reports the **chain's
/// clock** — the instant a temporal corridor's derived clock
/// ([`Scope::with_named_at`]) read as the event was recorded, in milliseconds since
/// the epoch, e.g. `("scope-clock", "1000000")`. Appears only beside
/// [`SCOPE_NOTE`], and only when the chain carries a clock. A corridor's name is
/// the injector's *claim* about time; this is what the kernel actually observed,
/// so a reader can tell a chain that pinned `inv.now()` from one merely named as
/// if it had. The kernel's own clock is on the event as `started` / `ended`,
/// unchanged: validity is judged there, never here.
pub const SCOPE_CLOCK_NOTE: &str = "scope-clock";

/// The well-known golden thread meaning **"the set of bindings this kernel resolves
/// against has changed"**: `urn:kernel:bindings`.
///
/// Every representation the kernel derives from its bindings hangs from it — the
/// catalog (`urn:kernel:catalog`), the action manifold (`urn:kernel:actions`, both
/// faces), a validation report (`urn:kernel:validate`), and every `Meta` answer, the
/// canonical Turtle and any transrepted face alike — and so does the kernel's memo of
/// each resolved endpoint's declared floor (the `requires` the capability floor
/// checks before dispatch — `FloorMemo` in `kernel.rs`). Cutting it recomputes all of them on
/// their next read. Nothing else the kernel caches depends on it — a stored read of
/// a resource hangs from the thread named after its target, not from this one; see
/// [`Kernel::bindings_changed`] for why that is a decision.
///
/// **Who cuts it.** The kernel cannot observe a binding change: [`Kernel`] holds its
/// root as an `Arc<dyn Space>` fixed at construction, and a `Space` is a resolver,
/// not a registry — whether it can change at all is the space's own business (a
/// wrapper with interior mutability, a mount table discovery appends to, a module
/// host that reloads code under a name). So the party that changed it cuts the
/// thread: [`Kernel::bindings_changed`] from Rust, or the resource form
/// `sink urn:kernel:cut urn:kernel:bindings` under `urn:cap:kernel:cut` from an
/// endpoint or a peer. An ordinary `Sink` cuts the thread named after its target and
/// never this one — a write changes state, not the binding set.
pub const BINDINGS_THREAD: &str = "urn:kernel:bindings";

/// Receives a [`TraceEvent`] per invocation while installed. The kernel records
/// only when one is set ([`Kernel::set_tracer`]) — off the hot path otherwise — so
/// the `trace` command can capture one real resolution and render it. The host
/// supplies the collector (e.g. a `Mutex<Vec<TraceEvent>>`).
pub trait Tracer: Send + Sync {
    /// Record one resolved invocation.
    fn record(&self, event: TraceEvent);
}

/// One traced resolution's recording context: the collector its events go to and
/// the span-id source its nodes draw from. Created per traced call
/// ([`Kernel::issue_traced`]) — or from the kernel's globally-installed tracer
/// ([`Kernel::set_tracer`]) at the root of an ordinary issue — and threaded down
/// the invocation chain (including across `fan_out` spawns, via the
/// [`Issuer::issue_scoped`](crate::Issuer::issue_scoped) seam), so **concurrent**
/// traced resolutions on one shared kernel each record into their own collector,
/// never a neighbor's. Opaque: an [`Issuer`](crate::Issuer) implementor just
/// passes it through.
#[derive(Clone)]
pub struct TraceScope {
    tracer: Arc<dyn Tracer>,
    spans: Arc<AtomicU64>,
}

impl TraceScope {
    /// A fresh scope over `tracer`; span ids start at 0, private to this trace.
    fn new(tracer: Arc<dyn Tracer>) -> Self {
        TraceScope {
            tracer,
            spans: Arc::new(AtomicU64::new(0)),
        }
    }

    /// A scope over the kernel's global tracer slot, sharing the kernel-lifetime
    /// span counter (successive one-shot `trace` runs keep unique ids).
    fn global(tracer: Arc<dyn Tracer>, spans: Arc<AtomicU64>) -> Self {
        TraceScope { tracer, spans }
    }

    fn next_span(&self) -> u64 {
        self.spans.fetch_add(1, Ordering::Relaxed)
    }

    fn record(&self, event: TraceEvent) {
        self.tracer.record(event);
    }
}

/// A read-only view of the host scheduler's live state, injected into the kernel
/// like a [`Clock`] so `urn:kernel:scheduler` can report it **intrinsically** — over
/// the wire and without the host binding an endpoint, uniform with `urn:kernel:cache`
/// and `urn:kernel:threads`. The kernel never *drives* the scheduler (which lives
/// above it, runtime-free); it only reads these rows. With none injected,
/// `urn:kernel:scheduler` reports the single-threaded default.
pub trait SchedulerReporter: Send + Sync {
    /// Label/value rows describing the scheduler (backend, thread count, task
    /// counters, …). The kernel renders them verbatim, so a host can add rows
    /// (queue depth, per-resource executors) without a core change.
    fn rows(&self) -> Vec<(String, String)>;
}

/// The current worker thread's label: its name (the scheduler names workers
/// `ikigai-sched-N`) or, unnamed, its id.
fn thread_label() -> String {
    let current = std::thread::current();
    current
        .name()
        .map(str::to_string)
        .unwrap_or_else(|| format!("{:?}", current.id()))
}

/// Resolves requests against a root space, invokes the resolved endpoint, and
/// caches cacheable representations by their content-addressed request id.
pub struct Kernel {
    root: Arc<dyn Space>,
    /// Cacheable representations and the golden-thread generations that keep them
    /// valid — see [`cache`](crate::cache) for the bound, the pluggable policy, and
    /// the cut-sequence check that closes the lost-cut race.
    ///
    /// Entries are keyed by `(request id, capability fingerprint, scope
    /// fingerprint)`. The capability is part of the key so a cached entry is only
    /// ever served back to the same authority that computed it — a different (e.g.
    /// narrower) capability misses and must pass the endpoint's own check. Without
    /// this, a cache hit is served *before* the endpoint runs (see
    /// [`issue_inner`](Self::issue_inner)), which would skip the capability check
    /// and let one authority read another's cached result. The scope is part of it
    /// so an answer computed in one resolution chain (a confinement, an injected
    /// corridor) is never served to a request in another — the same name can
    /// resolve to a different endpoint there. The empty chain fingerprints to `0`,
    /// so plain [`issue`](Self::issue) keys exactly as it always has.
    cache: ReprCache,
    meta: Option<Arc<dyn MetaRenderer>>,
    /// Source of "now" for time-based [`Expiry::At`] deadlines. Absent ⇒ the
    /// kernel cannot evaluate a deadline, so it declines to cache `At` results
    /// (golden-thread and `Never` caching are unaffected).
    clock: Option<Arc<dyn Clock>>,
    /// Host executor for concurrent fan-out, and a weak self-handle the kernel hands
    /// to invocations as an owned [`Issuer`] so a spawned sub-request can re-enter the
    /// kernel without borrowing the invocation. Both set by [`into_scheduled`](Self::into_scheduled);
    /// absent ⇒ [`Invocation::fan_out`] resolves sequentially (the default).
    spawner: Option<Arc<dyn Spawner>>,
    self_ref: Option<Weak<Kernel>>,
    /// Execution tracer, installed only while the `trace` command captures one
    /// resolution. `tracing` is a fast-path gate so an untraced issue pays just an
    /// atomic load, never the lock. This global slot is the single-tenant
    /// compatibility path; concurrent tracing uses the per-call
    /// [`issue_traced`](Self::issue_traced), which never consults it.
    tracer: Mutex<Option<Arc<dyn Tracer>>>,
    tracing: AtomicBool,
    /// Monotonic span-id source for GLOBAL-slot traces (shared into their
    /// [`TraceScope`]s so successive one-shot traces keep unique ids). Per-call
    /// traces carry their own counter.
    span_counter: Arc<AtomicU64>,
    /// Read-only handle to the host scheduler, for `urn:kernel:scheduler`. Injected
    /// by a scheduled host (the [`Clock`] pattern); absent ⇒ single-threaded default.
    scheduler: Option<Arc<dyn SchedulerReporter>>,
    /// The rendered chain behind each non-empty scope fingerprint an entry was
    /// stored under, so `urn:kernel:cache` can NAME the chain a row was computed in
    /// rather than print a hash. The cache itself keeps only the fingerprint (the
    /// key); this is the readout's memory of what it meant. Written only when a
    /// scoped result is stored — never on the empty-chain path — and pruned to the
    /// fingerprints still resident whenever the readout renders.
    scope_names: Mutex<BTreeMap<u64, String>>,
    /// Rolling window of the most recent resolutions (target, time, cache outcome),
    /// for `urn:kernel:constraint` — the kernel's throughput readout. Always-on like
    /// the cache; bounded to [`CONSTRAINT_WINDOW`]. `urn:kernel:*` introspection
    /// requests are not recorded (they short-circuit before the resolution body).
    constraint: Mutex<VecDeque<ResolutionSample>>,
    /// The installed logical-rewrite table, if any. The kernel holds it (rather than
    /// only wrapping the root space in an [`Alias`]) because canonicalization must
    /// happen BEFORE the cache key and the auto-cut are computed from the target —
    /// that is what makes a logical name and its backing name share one cache entry
    /// and one golden thread. See `alias.rs` for the full decision record.
    aliases: Option<Arc<AliasTable>>,
    /// `rdfs:subClassOf*` closure for type-aware [`select_action`](Self::select_action):
    /// each class mapped to itself plus all its (transitive) superclasses. Set from a
    /// vocabulary/alignment graph via [`with_subclass_axioms`](Self::with_subclass_axioms);
    /// empty ⇒ exact-class matching (the default, no inference).
    subclass_closure: BTreeMap<String, BTreeSet<String>>,
    /// The nesting budget: a sub-request whose depth would exceed this is refused
    /// with [`Error::DepthExceeded`] before it resolves anything. See
    /// [`with_max_depth`](Self::with_max_depth) for what it does and does not
    /// bound.
    max_depth: u32,
    /// The declared floor of every endpoint resolved so far, memoized per endpoint
    /// identity — see [`FloorMemo`]. Consulted on every request (the floor runs
    /// before the cache lookup); cleared by a cut of [`BINDINGS_THREAD`].
    floors: FloorMemo,
}

/// How many endpoints' floors the memo holds before it sweeps — dead entries first,
/// then everything. The cache's default entry bound, chosen for the same reason: a
/// ceiling on memory for a root that hands out a fresh endpoint per resolution, not
/// a tuning knob (see [`FloorMemo`]).
const FLOOR_MEMO_BOUND: usize = 4096;

/// What the capability floor checks: the `requires` an endpoint declares per verb,
/// in [`Description::action_specs`] order — the only part of a description the floor
/// reads. Extracted once per endpoint and memoized (see [`FloorMemo`]).
struct Floor {
    requires: Vec<(Verb, Vec<String>)>,
}

impl Floor {
    fn of(description: &Description) -> Self {
        Floor {
            requires: description
                .action_specs()
                .into_iter()
                .map(|spec| (spec.verb, spec.requires))
                .collect(),
        }
    }

    /// The first scope declared for `request.verb` that `capability` does not satisfy —
    /// on the same `cap_satisfies` predicate selection and `urn:kernel:validate` use, so
    /// what the manifold offers is exactly what the kernel admits. `None` ⇒ admitted.
    fn unsatisfied(&self, request: &Request, capability: &Capability) -> Option<String> {
        self.requires
            .iter()
            .filter(|(verb, _)| *verb == request.verb)
            .flat_map(|(_, requires)| requires.iter())
            .find(|scope| !crate::select::cap_satisfies(capability, scope))
            .cloned()
    }
}

/// The per-endpoint memo of declared floors, keyed by the endpoint's identity, so
/// `describe()` runs once per bound endpoint instead of once per request (ledger #22:
/// ~130 ns of a ~600 ns cached read, a `Description` built by a builder each time).
///
/// **Why it is sound.** A description is a STATIC contract. Two places already
/// assume so: the catalog renders every description once and caches it `Never`,
/// and selection matches on it (MCP projects what selection sees). An endpoint
/// whose `describe()` varied at runtime would already be lying to both; this memo
/// makes the same assumption a third time and states it here so that a future
/// endpoint wanting a live contract knows what it is breaking. So one endpoint has
/// one floor for as long as it is bound, and "the same endpoint" is exactly "the
/// same `Arc` allocation": the key is the address of [`Resolved::endpoint`], and
/// every entry holds a [`Weak`] to it. The weak is not there to be upgraded — it is
/// there to RESERVE the address. An `Arc` allocation outlives its last strong
/// reference for as long as any weak one exists, so no other endpoint can be
/// allocated at a key this memo holds, and a lookup hit is the same endpoint by
/// construction rather than by comparison. Without the reservation a rebind that
/// dropped one endpoint and allocated another at the same address would be served
/// the old contract — a floor leak, the class of bug ikigai-throttle #14 was. A
/// strong reference would reserve it too, but would keep a replaced endpoint's
/// VALUE (a wasm instance, a socket) alive until the next sweep; the weak keeps only
/// the allocation's shell.
///
/// **An overlay that swaps the endpoint gets its own entry for free.** Throttle's
/// over-budget stand-in is a different allocation with a different contract, so it
/// is a different key, and the floor evaluates the stand-in's own declaration —
/// exactly as it did without the memo. Pinned by
/// `a_swapped_endpoint_is_floored_on_its_own_contract`.
///
/// **What it costs where it cannot hit.** An overlay that wraps the resolved
/// endpoint in a fresh `Arc` on every resolution (every ikigai-throttle governor
/// does today) never presents the same address twice: each request misses,
/// describes, and inserts an entry whose weak dies at once. The bound keeps that
/// from being a leak — at [`FLOOR_MEMO_BOUND`] entries the dead ones are swept, and
/// if none were dead everything is — so such a resource pays the old per-request
/// `describe()` plus one insert, while every stably bound resource pays one read
/// lock and a hash. Reusing the wrapper per inner endpoint is the overlay's fix,
/// not the kernel's. Pinned by `a_per_resolution_wrapper_is_bounded_and_floored_correctly`.
///
/// Invalidated as a whole by a cut of [`BINDINGS_THREAD`]: the thread is this memo's
/// validity predicate, the same one the catalog hangs from.
#[derive(Default)]
struct FloorMemo {
    entries: RwLock<HashMap<usize, FloorEntry>>,
}

struct FloorEntry {
    /// Reserves the key's address for as long as the entry lives; see [`FloorMemo`].
    endpoint: Weak<dyn Endpoint>,
    floor: Floor,
}

impl FloorMemo {
    /// The thin address of the endpoint's allocation — the identity a binding keeps.
    fn key(endpoint: &Arc<dyn Endpoint>) -> usize {
        Arc::as_ptr(endpoint) as *const () as usize
    }

    /// The first scope `request.verb` on `endpoint` requires that `capability` lacks:
    /// from the memo when it has seen this endpoint, else from `describe()`, memoized.
    fn unsatisfied(
        &self,
        endpoint: &Arc<dyn Endpoint>,
        request: &Request,
        capability: &Capability,
    ) -> Option<String> {
        let key = Self::key(endpoint);
        {
            let entries = self.entries.read().expect("floor memo lock");
            if let Some(entry) = entries.get(&key) {
                // The reservation makes this a tautology; it is asserted so a change
                // to the entry's shape that loses the weak fails here, not in the field.
                debug_assert_eq!(
                    Weak::as_ptr(&entry.endpoint) as *const () as usize,
                    key,
                    "a memo hit is the same allocation by construction"
                );
                return entry.floor.unsatisfied(request, capability);
            }
        }
        let floor = Floor::of(&endpoint.describe());
        let lacking = floor.unsatisfied(request, capability);
        let mut entries = self.entries.write().expect("floor memo lock");
        if entries.len() >= FLOOR_MEMO_BOUND {
            entries.retain(|_, entry| entry.endpoint.strong_count() > 0);
            if entries.len() >= FLOOR_MEMO_BOUND {
                entries.clear();
            }
        }
        // Two threads that missed on one key both insert one floor; last wins, same value.
        entries.insert(
            key,
            FloorEntry {
                endpoint: Arc::downgrade(endpoint),
                floor,
            },
        );
        lacking
    }

    fn clear(&self) {
        self.entries.write().expect("floor memo lock").clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.read().expect("floor memo lock").len()
    }
}

/// How many recent resolutions `urn:kernel:constraint` aggregates over.
const CONSTRAINT_WINDOW: usize = 512;

/// One sampled resolution behind `urn:kernel:constraint`: which resource, how long
/// it took to compute (per the injected [`Clock`]; `None` if the kernel has none),
/// and whether the cache served it (a hit consumes no constraint capacity).
struct ResolutionSample {
    target: String,
    elapsed_ms: Option<u64>,
    cache_hit: bool,
}

impl Kernel {
    /// A kernel over the given root space.
    pub fn new(root: Arc<dyn Space>) -> Self {
        Kernel {
            root,
            cache: ReprCache::default(),
            meta: None,
            clock: None,
            spawner: None,
            self_ref: None,
            tracer: Mutex::new(None),
            tracing: AtomicBool::new(false),
            span_counter: Arc::new(AtomicU64::new(0)),
            scheduler: None,
            constraint: Mutex::new(VecDeque::new()),
            aliases: None,
            subclass_closure: BTreeMap::new(),
            max_depth: DEFAULT_MAX_DEPTH,
            floors: FloorMemo::default(),
            scope_names: Mutex::new(BTreeMap::new()),
        }
    }

    /// A kernel that answers `Meta` requests by rendering through `renderer`.
    pub fn with_meta_renderer(root: Arc<dyn Space>, renderer: Arc<dyn MetaRenderer>) -> Self {
        Kernel {
            root,
            cache: ReprCache::default(),
            meta: Some(renderer),
            clock: None,
            spawner: None,
            self_ref: None,
            tracer: Mutex::new(None),
            tracing: AtomicBool::new(false),
            span_counter: Arc::new(AtomicU64::new(0)),
            scheduler: None,
            constraint: Mutex::new(VecDeque::new()),
            aliases: None,
            subclass_closure: BTreeMap::new(),
            max_depth: DEFAULT_MAX_DEPTH,
            floors: FloorMemo::default(),
            scope_names: Mutex::new(BTreeMap::new()),
        }
    }

    /// Find a transreptor chain converting `from` → `to` among this kernel's mounted
    /// endpoints — a direct single hop, else a two-hop pivot via `text/turtle`, through
    /// **lossless edges only**. `None` if no auto-invocable lossless chain exists. The
    /// basis for selection-driven metadata rendering, content negotiation, and
    /// sniff-and-dispatch. See [`crate::select_transreptor`]; to consent to a declared
    /// projection, [`select_transreptor_with`](Self::select_transreptor_with).
    pub fn select_transreptor(&self, from: &str, to: &str) -> Option<Vec<TransreptionStep>> {
        crate::select::select_transreptor(self.root.as_ref(), from, to)
    }

    /// [`select_transreptor`](Self::select_transreptor) under an explicit
    /// [`TransreptionPolicy`]. See [`crate::select_transreptor_with`] for the search
    /// order: consent widens it, a lossless plan still wins, and every step reports its
    /// own declaration.
    pub fn select_transreptor_with(
        &self,
        from: &str,
        to: &str,
        policy: &TransreptionPolicy,
    ) -> Option<Vec<TransreptionStep>> {
        crate::select::select_transreptor_with(self.root.as_ref(), from, to, policy)
    }

    /// [`select_transreptor`](Self::select_transreptor) **in a resolution chain**:
    /// the plan is found among the transreptors the chain can resolve — each
    /// injected corridor innermost first, then the root unless the chain is
    /// severed — so a corridor that shadows a transreptor's IRI is the one the plan
    /// names, and a severed chain without one plans nothing though the root has one.
    /// The empty chain is [`select_transreptor`](Self::select_transreptor) exactly.
    /// This is what [`Invocation::select_transreptor`] reaches, with the
    /// invocation's own chain, and what the `Meta` path plans over inside a scoped
    /// resolution.
    pub fn select_transreptor_in(
        &self,
        from: &str,
        to: &str,
        scope: &Scope,
    ) -> Option<Vec<TransreptionStep>> {
        self.select_transreptor_in_with(from, to, scope, &TransreptionPolicy::lossless())
    }

    /// [`select_transreptor_in`](Self::select_transreptor_in) under an explicit
    /// [`TransreptionPolicy`] — what a `Meta` request carrying `lossy=allow` plans
    /// over, and what [`Invocation::select_transreptor_with`] reaches.
    pub fn select_transreptor_in_with(
        &self,
        from: &str,
        to: &str,
        scope: &Scope,
        policy: &TransreptionPolicy,
    ) -> Option<Vec<TransreptionStep>> {
        if scope.is_empty() {
            return self.select_transreptor_with(from, to, policy);
        }
        crate::select::select_transreptor_with(&scope.view(None, &self.root), from, to, policy)
    }

    /// Install an `rdfs:subClassOf` axiom set — `(subclass, superclass)` IRI pairs, e.g.
    /// from a vocabulary or alignment graph — so [`select_action`](Self::select_action)
    /// reasons over the type hierarchy: an entity typed `foaf:Person` then satisfies an
    /// action requiring `schema:Person` when `foaf:Person rdfs:subClassOf schema:Person`.
    /// Builder; the transitive closure (each class → itself + all superclasses) is computed
    /// once here. Without it, matching is exact-class (no inference).
    pub fn with_subclass_axioms<I, A, B>(mut self, axioms: I) -> Self
    where
        I: IntoIterator<Item = (A, B)>,
        A: Into<String>,
        B: Into<String>,
    {
        // Direct sub → {super} adjacency, and the set of every class named.
        let mut direct: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut classes: BTreeSet<String> = BTreeSet::new();
        for (sub, sup) in axioms {
            let (sub, sup) = (sub.into(), sup.into());
            classes.insert(sub.clone());
            classes.insert(sup.clone());
            direct.entry(sub).or_default().insert(sup);
        }
        // Transitive closure incl. self: DFS up the hierarchy from each class (the visited
        // set also guards against cycles).
        let mut closure: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for class in &classes {
            let mut supers = BTreeSet::new();
            let mut stack = vec![class.clone()];
            while let Some(c) = stack.pop() {
                if supers.insert(c.clone()) {
                    if let Some(parents) = direct.get(&c) {
                        stack.extend(parents.iter().cloned());
                    }
                }
            }
            closure.insert(class.clone(), supers);
        }
        self.subclass_closure = closure;
        self
    }

    /// Find endpoints among this kernel's mounted endpoints whose required inputs are
    /// satisfiable by the RDF classes in `present` — the actions available given a set of
    /// typed entities. With an `rdfs:subClassOf` closure installed (see
    /// [`with_subclass_axioms`](Self::with_subclass_axioms)) the present types are first
    /// expanded through it, so a subclass satisfies an action requiring any of its
    /// superclasses. See [`crate::select_action`].
    pub fn select_action(&self, present: &[&str]) -> Vec<ActionMatch> {
        let expanded = self.expand_present(present);
        let refs: Vec<&str> = expanded.iter().map(String::as_str).collect();
        crate::select::select_action(&self.described(), &refs)
    }

    /// [`select_action`](Self::select_action) **in a resolution chain**: the
    /// manifold of what the chain can resolve — the kernel's own operations (ahead
    /// of every chain, as on the issue path), each injected corridor innermost
    /// first, then the root unless severed. A confined endpoint's manifold
    /// therefore never lists a root-only action: an offer the chain could not then
    /// resolve is the over-offer "declared = enforced" exists to prevent, one layer
    /// up. The empty chain is [`select_action`](Self::select_action) exactly.
    pub fn select_action_in(&self, present: &[&str], scope: &Scope) -> Vec<ActionMatch> {
        if scope.is_empty() {
            return self.select_action(present);
        }
        let expanded = self.expand_present(present);
        let refs: Vec<&str> = expanded.iter().map(String::as_str).collect();
        crate::select::select_action(&self.described_in(scope), &refs)
    }

    /// The action-level selection funnel over this kernel's bindings (see
    /// `select::select_actions`, crate-private): capability-scoped, verb- and output-aware,
    /// with the present types expanded through the `rdfs:subClassOf` closure first.
    pub fn select_actions(&self, query: &crate::select::ActionQuery<'_>) -> Vec<ActionMatch> {
        let expanded = self.expand_present(query.present);
        let refs: Vec<&str> = expanded.iter().map(String::as_str).collect();
        let expanded_query = crate::select::ActionQuery {
            present: &refs,
            ..*query
        };
        crate::select::select_actions(&self.described(), &expanded_query)
    }

    /// [`select_actions`](Self::select_actions) **in a resolution chain** — see
    /// [`select_action_in`](Self::select_action_in) for what the chain's manifold
    /// is. The empty chain is [`select_actions`](Self::select_actions) exactly.
    pub fn select_actions_in(
        &self,
        query: &crate::select::ActionQuery<'_>,
        scope: &Scope,
    ) -> Vec<ActionMatch> {
        if scope.is_empty() {
            return self.select_actions(query);
        }
        let expanded = self.expand_present(query.present);
        let refs: Vec<&str> = expanded.iter().map(String::as_str).collect();
        let expanded_query = crate::select::ActionQuery {
            present: &refs,
            ..*query
        };
        crate::select::select_actions(&self.described_in(scope), &expanded_query)
    }

    /// The typed self-description of the endpoint bound at `iri`, or `None` if the
    /// IRI resolves to nothing. Returns the [`Description`] struct — not the
    /// rendered Meta representation — so a caller projecting the catalog or the
    /// action manifold (e.g. an MCP tool list) gets typed [`ArgSpec`](crate::ArgSpec)
    /// contracts without re-parsing Turtle. The companion to [`select_actions`](Self::select_actions):
    /// list the allowed actions, then `describe` each endpoint for its schema.
    pub fn describe(&self, iri: &Iri) -> Option<Description> {
        // Through the shared probe, so a limited name is `None` here as it is
        // unresolved on the issue path: describing a hole would reveal it.
        crate::select::probe(&self.described(), iri.clone())
            .map(|resolved| resolved.endpoint.describe())
    }

    /// Find a bound endpoint's description by its `Description::id` (the catalog
    /// subject identity) — the reverse of the entries → describe walk. Covers
    /// template-bound entries too (via [`crate::select::describe_entry`]'s probe), so
    /// `urn:kernel:validate` can pre-flight a template action's catalog IRI.
    fn description_for_id(&self, id: &str) -> Result<Option<Description>> {
        let space = self.described();
        for entry in space.entries().unwrap_or_default() {
            if let Some(described) = crate::select::describe_entry(&space, &entry) {
                if described.description.id == id {
                    return Ok(Some(described.description));
                }
            }
        }
        Ok(None)
    }

    /// The typed self-description of the endpoint a catalog or manifold row names.
    /// `pattern` is what [`ActionMatch::endpoint`] carries: an exact IRI (delegates to
    /// [`describe`](Self::describe)) or a URI-template pattern (`urn:file:{path}`), which
    /// no `Iri` can hold — so a projector (e.g. an MCP tool list) holding a template
    /// match gets its contract without parsing the pattern itself.
    pub fn describe_pattern(&self, pattern: &str) -> Option<Description> {
        if let Ok(iri) = Iri::parse(pattern) {
            return self.describe(&iri);
        }
        let space = self.described();
        let entry = space
            .entries()
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.pattern == pattern)?;
        crate::select::describe_entry(&space, &entry).map(|d| d.description)
    }

    /// Expand each present type to itself + its superclasses (`rdfs:subClassOf*`); an
    /// unknown type maps to just itself. No closure installed ⇒ identity.
    fn expand_present(&self, present: &[&str]) -> Vec<String> {
        if self.subclass_closure.is_empty() {
            return present.iter().map(|p| p.to_string()).collect();
        }
        let mut expanded: BTreeSet<String> = BTreeSet::new();
        for &p in present {
            match self.subclass_closure.get(p) {
                Some(supers) => expanded.extend(supers.iter().cloned()),
                None => {
                    expanded.insert(p.to_string());
                }
            }
        }
        expanded.into_iter().collect()
    }

    /// Render `description` to `target` by transrepting its canonical Turtle: render the
    /// Turtle, [`select`](Self::select_transreptor_in_with) a transreptor chain
    /// `text/turtle → target` **in the resolution chain the Meta request runs in** and
    /// under the request's [`TransreptionPolicy`], and run it (piping `content`, setting
    /// `as`) through the kernel in that same chain, as sub-requests of the Meta
    /// resolution. Used by the `Meta` path for any type the renderer doesn't emit
    /// directly.
    ///
    /// **When no plan reaches `target`, the answer is the canonical Turtle — a
    /// substitution, and not a lie.** The representation's `repr_type` SAYS
    /// `text/turtle`: a caller that reads the type it was handed learns exactly what it
    /// got, and the canonical face is the honest self-description when no renderer
    /// exists for the one asked for — Meta prefers a description in a space the caller
    /// did not ask for to no description (the formal document, §8, states this as a
    /// deviation in strictness from Definition 8; ledger #21 records the JSON case,
    /// where a client that does not read the type parses Turtle as JSON). A root
    /// transreptor a severed chain cannot resolve is not offered, so it is not a plan
    /// that fails on a branch that looked selectable.
    ///
    /// **A lossy route is a different matter, and is refused.** If the only chain to
    /// `target` crosses a transreptor declared a projection
    /// ([`Description::lossy`]), Meta must not take it without consent: that would be a
    /// description that LOST information while claiming the very type asked for — the
    /// type would then be true and the content would not, which no reader of
    /// `repr_type` can detect. And it is not silently substituted either: a substitution
    /// would hide that a route exists at all, so the refusal names the lossy step and
    /// the consent (`lossy=allow` on the `Meta` request) that admits it. The error is
    /// [`Error::Endpoint`]: the face is not absent (`NotFound` would say the type is
    /// unreachable, and it is reachable, with consent), no capability is at issue
    /// (`Denied` is a principal's authority, and consent here is per request, not per
    /// holder), and the request's arguments are each well-formed (`InvalidArgument`
    /// would blame `as`). The engine's `describe <iri> <type>` shows the message.
    #[allow(clippy::too_many_arguments)] // the Meta arm's sub-request: every fact of its resolution, named
    async fn transrept_meta(
        &self,
        description: &Description,
        target: &ReprType,
        policy: &TransreptionPolicy,
        capability: &Capability,
        parent: Option<u64>,
        trace: &Option<TraceScope>,
        scope: &Scope,
        depth: u32,
    ) -> Result<Representation> {
        let renderer = self
            .meta
            .as_ref()
            .ok_or_else(|| Error::Endpoint("no Meta renderer configured".to_string()))?;
        let canonical = renderer.render(description, &ReprType::new(crate::select::CANONICAL))?;
        let Some(plan) = self.select_transreptor_in_with(
            crate::select::CANONICAL,
            &target.media_type,
            scope,
            policy,
        ) else {
            // No permitted plan. If a LOSSY one exists, the caller must consent to it,
            // and is told so — the second walk runs only on this branch, which used to
            // substitute; the default path is the one walk it always was.
            if !policy.allows_lossy() {
                if let Some(lossy) = self.select_transreptor_in_with(
                    crate::select::CANONICAL,
                    &target.media_type,
                    scope,
                    &TransreptionPolicy::allow_lossy(),
                ) {
                    let steps = lossy
                        .iter()
                        .filter(|s| !s.lossless)
                        .map(|s| format!("`{}`", s.endpoint))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(Error::Endpoint(format!(
                        "meta face `{}` is reachable only through a lossy transreptor \
                         ({steps}): a projection loses information and would not describe \
                         the endpoint; pass `lossy=allow` to consent",
                        target.media_type
                    )));
                }
            }
            // Nothing converts Turtle to the requested type — hand back the canonical Turtle.
            return Ok(canonical.cacheable());
        };
        let mut current = canonical;
        for step in plan {
            let iri = Iri::parse(&step.endpoint).map_err(|e| {
                Error::Endpoint(format!("bad transreptor IRI `{}`: {e}", step.endpoint))
            })?;
            let request = Request::new(Verb::Source, iri)
                .with_arg("content", ArgRef::Inline(current.bytes))
                .with_arg("as", ArgRef::Inline(step.to.into_bytes()));
            // Box the re-entrant issue: the async call graph (issue → transrept_meta →
            // issue) is a cycle the compiler must size via indirection, even though at
            // runtime these are plain `Source` transreptions, never `Meta`. A step runs
            // one level below the Meta resolution, in its chain and its trace.
            current = Box::pin(self.issue_inner(
                request,
                capability,
                parent,
                None,
                trace.clone(),
                scope.clone(),
                depth + 1,
            ))
            .await?;
        }
        Ok(current.cacheable())
    }

    /// Make this kernel **schedulable**: wrap it in an `Arc` and inject `spawner`, so
    /// re-entrant fan-out ([`Invocation::fan_out`]) runs concurrently on the host's
    /// executor instead of sequentially. Uses [`Arc::new_cyclic`] to store a weak
    /// self-handle the kernel hands to invocations as an owned [`Issuer`], so a
    /// spawned sub-request can re-enter the kernel without borrowing the invocation.
    /// (Chain after the other builders, e.g.
    /// `Kernel::with_meta_renderer(..).with_clock(..).into_scheduled(spawner)`.)
    pub fn into_scheduled(self, spawner: Arc<dyn Spawner>) -> Arc<Kernel> {
        Arc::new_cyclic(|weak: &Weak<Kernel>| {
            let mut kernel = self;
            kernel.spawner = Some(spawner);
            kernel.self_ref = Some(weak.clone());
            kernel
        })
    }

    /// Inject the [`Clock`] the kernel reads for time-based [`Expiry::At`]
    /// deadlines (builder). Without one, `At` results are not cached — golden-thread
    /// and `Never` caching are unaffected — so a kernel only consults a clock when a
    /// host opts into time-bounded freshness (e.g. mounting the HTTP client module).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Set the **nesting budget** (builder): the deepest sub-request this kernel
    /// will run. A request the host issues is at depth 0; a sub-request an endpoint
    /// issues — through `issue`, `source`, the attenuated forms, `fan_out`, or a
    /// `scope_sync` bridge — is one deeper than the invocation issuing it; a
    /// request whose depth would exceed `max_depth` is refused with
    /// [`Error::DepthExceeded`] before it resolves anything, and the refusal is
    /// recorded on the trace under [`DEPTH_NOTE`]. Default
    /// [`DEFAULT_MAX_DEPTH`] (64).
    ///
    /// ## What it bounds, and what it does not
    ///
    /// It bounds **computation** — an endpoint that issues its own IRI, a
    /// transclusion cycle (A composes B composes A), a governor stack that
    /// forwards to itself — which used to recurse until the stack died: on an
    /// edge process facing strangers, a one-request denial of service. It does
    /// not need to bound *resolution*, which terminates by construction:
    /// `Rewrite` resolves once in its inner space and returns `Miss` rather than
    /// falling back outward with the rewritten name, and `Alias` caps its chain,
    /// so the paper's single-mapper universality cannot arise
    /// (`docs/formalism/README.md` §5). The counter is therefore the paper's
    /// nesting budget alone, never its hop counter, and a legitimate chain
    /// shorter than the bound is unaffected.
    ///
    /// It bounds nesting **within one kernel**. The depth is carried on the
    /// invocation and through [`Issuer::issue_at_depth`]; an issuer that
    /// implements only the plain `issue` seam (a module host bridge) restarts the
    /// count on its side, and the wire has no field for it, so two peers that
    /// mount each other stay unbounded by this budget. Carrying it across a peer
    /// is a wire-protocol bump, decided separately.
    ///
    /// A refusal is never cached — errors never reach the store — and a composite
    /// that swallows one and returns a cacheable fallback is forced uncacheable,
    /// so a refusal at the bound is never served to a shallower request.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use futures::executor::block_on;
    /// use ikigai_core::{
    ///     AsyncFnEndpoint, Capability, EndpointSpace, Error, Exact, Iri, Kernel, Request, Verb,
    /// };
    ///
    /// // An endpoint that sources ITSELF: unbounded recursion, until the budget.
    /// let ouroboros = AsyncFnEndpoint::new("ouroboros", |inv| {
    ///     Box::pin(async move { inv.source(&Iri::parse("urn:self").unwrap()).await })
    /// });
    /// let kernel = Kernel::new(Arc::new(
    ///     EndpointSpace::new().bind(Exact::new("urn:self"), ouroboros),
    /// ))
    /// .with_max_depth(3);
    ///
    /// let err = block_on(kernel.issue(
    ///     Request::new(Verb::Source, Iri::parse("urn:self").unwrap()),
    ///     &Capability::root(),
    /// ))
    /// .unwrap_err();
    /// // Depths 0, 1, 2 and 3 ran; the request for depth 4 was refused, typed.
    /// assert!(matches!(err, Error::DepthExceeded { depth: 4, .. }), "{err}");
    /// assert_eq!(kernel.cache_len(), 0, "a refusal is never cached");
    /// ```
    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Install the cache's admission and eviction [`CachePolicy`] (builder) —
    /// configured at execution time, beside the clock and the meta renderer, because
    /// the right trade is the host's to make: an edge process facing strangers, a
    /// batch job that wants everything it computed, and a browser tab want different
    /// ceilings.
    ///
    /// A policy decides what is *worth keeping*, never what is *correct to serve*:
    /// golden threads and expiry are evaluated on the serving path, where no policy
    /// is consulted. Absent ⇒ [`Lru`](crate::Lru) at its
    /// [default bound](crate::CacheBound::default) (4096 entries, 64 MiB), which is a
    /// change from the unbounded behaviour before 0.1.70 — a long-lived process now
    /// evicts instead of growing.
    ///
    /// Call it before the kernel is used: it replaces the cache, so anything already
    /// stored is dropped.
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use ikigai_core::{CacheBound, Kernel, Lru, Space};
    /// # fn example(root: Arc<dyn Space>) {
    /// let kernel = Kernel::new(root)
    ///     .with_cache_policy(Arc::new(Lru::with_bound(CacheBound::new(256, 8 << 20))));
    /// # let _ = kernel; }
    /// ```
    pub fn with_cache_policy(mut self, policy: Arc<dyn CachePolicy>) -> Self {
        self.cache = ReprCache::new(policy);
        self
    }

    /// Install a logical-rewrite table (builder): stable logical names resolve to
    /// different backing resources, invisibly to the caller. `urn:fn:toUpper` →
    /// `urn:iki:fn:toUpper` for a namespace migration; `urn:log:config` →
    /// `file:/logConfig.yaml` for an indirection. See [`alias`](crate::alias) for
    /// the design and its five forced decisions.
    ///
    /// This does two things:
    ///
    /// 1. It wraps the root space in an [`Alias`], so anything resolving against
    ///    the kernel's space directly (a host walking `Kernel::entries`, a
    ///    `describe`) sees the same rewrite.
    /// 2. It keeps the table in the kernel, which buys the *operator's* half: the
    ///    live readout at `urn:kernel:aliases`, refusal of a cyclic or over-long
    ///    chain **before** dispatch instead of at invoke, per-rule attribution of a
    ///    rewrite that landed on nothing, and the un-migrated-grant hint on a
    ///    denial.
    ///
    /// It is **not** what makes the two names one resource. That is
    /// [`Resolved::canonical`](crate::Resolved::canonical): the [`Alias`] reports
    /// each rewrite it performs and the kernel adopts the reported name before the
    /// request id, the capability floor, the auto-cut and the [`Invocation`]. So an
    /// `Alias` composed by hand — under a `RateLimit`, say — gets the same single
    /// cache entry and single golden thread, which before 0.1.64 only this builder
    /// could give it.
    ///
    /// Installing it twice stacks the wrap and keeps only the newest table; call it
    /// once, with the whole table.
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use ikigai_core::{AliasTable, Kernel, Space};
    /// # fn example(root: Arc<dyn Space>) {
    /// let kernel = Kernel::new(root)
    ///     .with_aliases(Arc::new(AliasTable::new().prefix("urn:fn:", "urn:iki:fn:")));
    /// # let _ = kernel; }
    /// ```
    pub fn with_aliases(mut self, table: Arc<AliasTable>) -> Self {
        self.root = Arc::new(Alias::new(Arc::clone(&table), self.root));
        self.aliases = Some(table);
        self
    }

    /// The installed rewrite table, if any.
    pub fn aliases(&self) -> Option<&Arc<AliasTable>> {
        self.aliases.as_ref()
    }

    /// Canonicalize a target through the installed table. `Ok(None)` means the name
    /// stands as written (no table, or no rule matched); `Ok(Some(hop))` carries the
    /// rewrite; `Err` is a deliberate refusal (a cycle, an over-long chain, or a
    /// substitution that is not a valid IRI) — never a half-applied rewrite, which
    /// would resolve to whichever name the walk happened to stop on.
    ///
    /// The refusal is an `Endpoint` error rather than a new variant: it is
    /// permanent (`is_transient() == false`, correctly — re-issuing cannot fix a
    /// table), and adding a variant to the error enum would silently re-route every
    /// consumer's catch-all arm for a condition only an operator can fix.
    fn canonicalize(&self, target: &Iri) -> Result<Option<AliasHop>> {
        match self
            .aliases
            .as_ref()
            .map(|table| table.canonicalize(target))
        {
            None | Some(Canonical::Direct) => Ok(None),
            Some(Canonical::Aliased(hop)) => Ok(Some(hop)),
            Some(Canonical::Refused(refusal)) => Err(Error::Endpoint(format!(
                "alias table refused `{}`: {refusal}",
                refusal.logical
            ))),
        }
    }

    /// Inject a [`SchedulerReporter`] so `urn:kernel:scheduler` reports the host
    /// scheduler's live state (builder). Without one it reports the single-threaded
    /// default. A scheduled host injects this alongside
    /// [`into_scheduled`](Self::into_scheduled).
    pub fn with_scheduler_reporter(mut self, reporter: Arc<dyn SchedulerReporter>) -> Self {
        self.scheduler = Some(reporter);
        self
    }

    /// Install a [`Tracer`] to record each invocation of the *next* resolution. The
    /// `trace` command sets one around a single real `source`, then calls
    /// [`clear_tracer`](Self::clear_tracer). Takes `&self` (interior-mutable), so it
    /// works on the shared `Arc<Kernel>` the engine drives. Off the hot path when no
    /// tracer is set — an untraced issue pays only an atomic load.
    pub fn set_tracer(&self, tracer: Arc<dyn Tracer>) {
        *self.tracer.lock().expect("tracer lock") = Some(tracer);
        self.tracing.store(true, Ordering::SeqCst);
    }

    /// Remove the installed tracer.
    pub fn clear_tracer(&self) {
        self.tracing.store(false, Ordering::SeqCst);
        *self.tracer.lock().expect("tracer lock") = None;
    }

    /// The clock's "now", or `None` if the kernel has no clock — the start stamp
    /// shared by a [`TraceEvent`] and the always-on `urn:kernel:constraint` window.
    fn now_stamp(&self) -> Option<Time> {
        self.clock.as_ref().map(|clock| clock.now())
    }

    /// Milliseconds elapsed since `started`, per the injected clock — the honest
    /// proxy for what a result cost to produce, and the only cost signal a
    /// [`CachePolicy`] gets. `None` without a clock (or without a start stamp): an
    /// unmeasured cost must not read as a zero cost.
    fn elapsed_since(&self, started: Option<Time>) -> Option<u64> {
        match (started, self.clock.as_ref()) {
            (Some(start), Some(clock)) => {
                Some(clock.now().as_millis().saturating_sub(start.as_millis()))
            }
            _ => None,
        }
    }

    /// Record one resolution into the rolling constraint window (always-on, bounded
    /// to [`CONSTRAINT_WINDOW`]). `started` is the pre-resolution stamp; the elapsed
    /// compute time is `now − started` (only when the kernel has a clock). A cache
    /// hit is logged but its elapsed is left out of the constraint total.
    fn record_resolution(&self, request: &Request, started: Option<Time>, cache_hit: bool) {
        let elapsed_ms = self.elapsed_since(started);
        let mut window = self.constraint.lock().expect("constraint lock");
        if window.len() >= CONSTRAINT_WINDOW {
            window.pop_front();
        }
        window.push_back(ResolutionSample {
            target: request.target.as_str().to_string(),
            elapsed_ms,
            cache_hit,
        });
    }

    /// The recording scope for the kernel's globally-installed tracer, if one is
    /// set — the single-tenant compatibility path behind
    /// [`set_tracer`](Self::set_tracer), consulted only at resolution roots.
    /// Per-call tracing ([`issue_traced`](Self::issue_traced)) never reads it.
    fn global_scope(&self) -> Option<TraceScope> {
        if !self.tracing.load(Ordering::Relaxed) {
            return None;
        }
        self.tracer
            .lock()
            .expect("tracer lock")
            .clone()
            .map(|tracer| TraceScope::global(tracer, Arc::clone(&self.span_counter)))
    }

    /// Report one resolved invocation into its resolution's [`TraceScope`], if the
    /// resolution is being traced — tagged with its own `span` and its issuer's
    /// `parent` span, so the events form a tree.
    #[allow(clippy::too_many_arguments)]
    fn trace_record(
        &self,
        trace: &Option<TraceScope>,
        request: &Request,
        capability: &Capability,
        span: Option<u64>,
        parent: Option<u64>,
        started: Option<Time>,
        cache_hit: bool,
        mut notes: Vec<(String, String)>,
        scope: &Scope,
    ) {
        let Some(recording) = trace else {
            return;
        };
        // The chain is disclosed on every event of a non-empty-chain resolution;
        // rendered here, inside the tracing gate, so an untraced issue never pays
        // for the string.
        notes.extend(scope_notes(scope));
        recording.record(TraceEvent {
            target: request.target.as_str().to_string(),
            thread: thread_label(),
            started,
            ended: self.clock.as_ref().map(|clock| clock.now()),
            cache_hit,
            span: span.unwrap_or(0),
            parent,
            // The authority this hop ran under, mirrored from `Capability::scopes`
            // (None = root). Recorded only while tracing, so the clone is off the
            // hot path.
            capability: capability.scopes().map(|s| s.iter().cloned().collect()),
            notes,
        });
    }

    /// Report a CAPABILITY DENIAL into the resolution's [`TraceScope`] — the
    /// refusal itself, as an event, tagged [`DENIED_NOTE`] with the scope the
    /// caller lacked.
    ///
    /// A sibling of [`trace_record`](Self::trace_record) rather than a call into
    /// it, because the two disagree about time on purpose: `trace_record` stamps
    /// `ended` from the clock, and nothing ran here. `started`/`ended` stay
    /// `None` and `cache_hit` is `false`, which is the honest reading of a
    /// request that was refused before dispatch.
    ///
    /// Why the kernel is the only place this can be reported: the capability
    /// floor is enforced BEFORE the endpoint is entered (that is the whole point
    /// of "declared = enforced"), so no in-endpoint observer — and no
    /// `trace_note` — can ever see it. A refused authority is a security fact,
    /// and this is the sole point that holds both the target and the authority
    /// it was refused under.
    #[allow(clippy::too_many_arguments)]
    fn trace_denial(
        &self,
        trace: &Option<TraceScope>,
        request: &Request,
        capability: &Capability,
        span: Option<u64>,
        parent: Option<u64>,
        lacking: &str,
        alias: Option<&AliasHop>,
        scope: &Scope,
    ) {
        let Some(trace_scope) = trace else {
            return;
        };
        // A denial on a REWRITTEN target is the failure shape this whole primitive
        // has to defend against: the capability check runs against the backing name
        // (decision 1), so a grant that still names the pre-alias scope fails by
        // simply not holding. Carrying the hop on the denial event is what stops
        // that from being invisible.
        let mut notes = vec![(DENIED_NOTE.to_string(), lacking.to_string())];
        notes.extend(alias_notes(alias));
        notes.extend(scope_notes(scope));
        trace_scope.record(TraceEvent {
            target: request.target.as_str().to_string(),
            thread: thread_label(),
            // Nothing ran: no start, no end.
            started: None,
            ended: None,
            cache_hit: false,
            span: span.unwrap_or(0),
            parent,
            // The authority that was NOT enough — the other half of the fact,
            // beside the scope named in the note.
            capability: capability.scopes().map(|s| s.iter().cloned().collect()),
            notes,
        });
    }

    /// Report a NESTING-BUDGET refusal into the resolution's [`TraceScope`] — the
    /// refusal itself, as an event, tagged [`DEPTH_NOTE`] with the depth the
    /// request would have run at. The shape [`trace_denial`](Self::trace_denial)
    /// established: nothing ran, so `started`/`ended` stay `None` and `cache_hit`
    /// is `false`; the alias hop and the chain ride along so an observer can see
    /// which name, under which rewrite, in which chain, asked for one level too
    /// many. The `parent` is the invocation that asked — the place a cycle shows.
    #[allow(clippy::too_many_arguments)]
    fn trace_depth(
        &self,
        trace: &Option<TraceScope>,
        request: &Request,
        capability: &Capability,
        span: Option<u64>,
        parent: Option<u64>,
        depth: u32,
        alias: Option<&AliasHop>,
        scope: &Scope,
    ) {
        let Some(trace_scope) = trace else {
            return;
        };
        let mut notes = vec![(DEPTH_NOTE.to_string(), depth.to_string())];
        notes.extend(alias_notes(alias));
        notes.extend(scope_notes(scope));
        trace_scope.record(TraceEvent {
            target: request.target.as_str().to_string(),
            thread: thread_label(),
            // Nothing ran: no start, no end.
            started: None,
            ended: None,
            cache_hit: false,
            span: span.unwrap_or(0),
            parent,
            capability: capability.scopes().map(|s| s.iter().cloned().collect()),
            notes,
        });
    }

    /// Record a resolution that MISSED where the miss is not the whole story: after
    /// a rewrite (the alias fired, and nothing was bound at the name it produced),
    /// or inside a non-empty chain (the name may well be bound in the root, and the
    /// chain is why it was not reached). Nothing ran, so — like a denial — no
    /// observer inside an endpoint can ever report it. A plain miss in the empty
    /// chain records nothing, unchanged.
    #[allow(clippy::too_many_arguments)]
    fn trace_miss(
        &self,
        trace: &Option<TraceScope>,
        request: &Request,
        capability: &Capability,
        span: Option<u64>,
        parent: Option<u64>,
        alias: Option<&AliasHop>,
        scope: &Scope,
    ) {
        let Some(trace_scope) = trace else {
            return;
        };
        if alias.is_none() && scope.is_empty() {
            return;
        }
        let mut notes = alias_notes(alias);
        if alias.is_some() {
            notes.push((
                ALIAS_MISS_NOTE.to_string(),
                request.target.as_str().to_string(),
            ));
        }
        notes.extend(scope_notes(scope));
        if !scope.is_empty() {
            notes.push((
                SCOPE_MISS_NOTE.to_string(),
                request.target.as_str().to_string(),
            ));
        }
        trace_scope.record(TraceEvent {
            target: request.target.as_str().to_string(),
            thread: thread_label(),
            started: None,
            ended: None,
            cache_hit: false,
            span: span.unwrap_or(0),
            parent,
            capability: capability.scopes().map(|s| s.iter().cloned().collect()),
            notes,
        });
    }

    /// Record a resolution that landed on a LIMITER: the request is about to be
    /// answered `Unresolved` exactly as an unbound name would be, so — like a
    /// confined miss — the trace is the only place the fact can show. The
    /// [`trace_denial`](Self::trace_denial) shape: nothing ran, `started`/`ended`
    /// stay `None`, `cache_hit` is `false`; the alias hop and the chain ride along.
    /// See [`LIMITED_NOTE`] for why this records when a plain miss does not.
    #[allow(clippy::too_many_arguments)]
    fn trace_limited(
        &self,
        trace: &Option<TraceScope>,
        request: &Request,
        capability: &Capability,
        span: Option<u64>,
        parent: Option<u64>,
        alias: Option<&AliasHop>,
        scope: &Scope,
    ) {
        let Some(trace_scope) = trace else {
            return;
        };
        let mut notes = vec![(
            LIMITED_NOTE.to_string(),
            request.target.as_str().to_string(),
        )];
        notes.extend(alias_notes(alias));
        notes.extend(scope_notes(scope));
        trace_scope.record(TraceEvent {
            target: request.target.as_str().to_string(),
            thread: thread_label(),
            started: None,
            ended: None,
            cache_hit: false,
            span: span.unwrap_or(0),
            parent,
            capability: capability.scopes().map(|s| s.iter().cloned().collect()),
            notes,
        });
    }

    /// The denial message for an unmet capability floor. When the target arrived
    /// through a rewrite it names the hop, and — if the caller's own grants mention
    /// a namespace this table rewrites — says plainly that the grant looks
    /// un-migrated. Authority is never rewritten to make the check pass (that is how
    /// a silent *grant* is manufactured); the mismatch is reported instead.
    fn denial_message(
        &self,
        scope: &str,
        request: &Request,
        capability: &Capability,
        alias: Option<&AliasHop>,
    ) -> String {
        let mut message = format!(
            "capability does not grant `{scope}` (declared by `{}`)",
            request.target.as_str()
        );
        let Some(hop) = alias else {
            return message;
        };
        message.push_str(&format!(" — target arrived via alias `{hop}`"));
        let (Some(table), Some(held)) = (self.aliases.as_ref(), capability.scopes()) else {
            return message;
        };
        let stale = table.scopes_naming_stale_namespaces(held.iter());
        if !stale.is_empty() {
            message.push_str(&format!(
                "; the capability holds {} — scope(s) naming a namespace this table \
                 rewrites, so the grant may not have been migrated with the resource",
                stale
                    .iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        message
    }

    /// Issue a request: return a valid cached representation if one exists,
    /// otherwise resolve, invoke the endpoint, and cache the result if cacheable.
    pub async fn issue(&self, request: Request, capability: &Capability) -> Result<Representation> {
        // Top-level entry: no parent span (this is a trace root if one is recording
        // via the globally-installed tracer).
        let trace = self.global_scope();
        self.issue_inner(request, capability, None, None, trace, Scope::empty(), 0)
            .await
    }

    /// Issue a request in a **resolution chain** other than the plain root: the
    /// corridors `scope` injects are consulted ahead of the root — shadowing it
    /// for this request and for every sub-request it gives rise to — and, if the
    /// scope is [severed](Scope::sever), the root is not consulted at all. With
    /// [`Scope::empty`] this is [`issue`](Self::issue).
    ///
    /// # ★ This is authority, and it is the host's
    ///
    /// A corridor placed ahead of the root can stand in for any door — bind
    /// `urn:personal:contacts` in one and every sub-request of this resolution
    /// that asks for contacts gets the corridor's answer. So the widening face of
    /// the chain lives here, on the kernel, reachable only by whoever holds it: the
    /// same trust line as [`Capability::root`], and the same reason
    /// `Invocation::issue_under` is private (`docs/design/sub-request-authority.md`
    /// — an authority-carrying sub-request takes its authority from the kernel,
    /// never from its caller). From inside an endpoint the only chain-changing
    /// operation is [`Invocation::confine`], which is strictly narrowing.
    ///
    /// Two things the chain does not touch: `urn:kernel:*` is intercepted ahead of
    /// it, so no corridor can shadow a kernel operation and a severed chain still
    /// reaches them, capability-gated as ever; and the declared-capability floor
    /// runs unchanged against whichever endpoint the chain resolved to.
    ///
    /// The cache is partitioned by the chain's [fingerprint](Scope::fingerprint),
    /// which is computed over the corridors' **names** — see [`Scope`] on what a
    /// name claims, and why an unnamed corridor never shares.
    ///
    /// One name, two contexts, two resources — and two cache entries:
    ///
    /// ```
    /// use std::sync::Arc;
    /// use futures::executor::block_on;
    /// use ikigai_core::{
    ///     Capability, EndpointSpace, Exact, FnEndpoint, Iri, Kernel, ReprType, Representation,
    ///     Request, Scope, Verb,
    /// };
    ///
    /// // The root binds `urn:doc:title`…
    /// let kernel = Kernel::new(Arc::new(EndpointSpace::new().bind(
    ///     Exact::new("urn:doc:title"),
    ///     FnEndpoint::new("title", |_| {
    ///         Ok(Representation::new(ReprType::new("text/plain"), b"resource oriented computing".to_vec())
    ///             .cacheable())
    ///     }),
    /// )));
    /// // …and so does a corridor the host injects for one request: the SAME name.
    /// let at_six = Arc::new(EndpointSpace::new().bind(
    ///     Exact::new("urn:doc:title"),
    ///     FnEndpoint::new("title-at-six", |_| {
    ///         Ok(Representation::new(ReprType::new("text/plain"), b"as it stood at 18:00".to_vec())
    ///             .cacheable())
    ///     }),
    /// ));
    /// let root = Capability::root();
    /// let title = || Request::new(Verb::Source, Iri::parse("urn:doc:title").unwrap());
    /// let six_pm = Scope::empty().with_named(Iri::parse("urn:ctx:six-pm").unwrap(), at_six);
    ///
    /// // The corridor is consulted ahead of the root, so it shadows the root's door.
    /// let here = block_on(kernel.issue(title(), &root)).unwrap();
    /// let there = block_on(kernel.issue_in(title(), &root, six_pm)).unwrap();
    /// assert_eq!(here.bytes, b"resource oriented computing");
    /// assert_eq!(there.bytes, b"as it stood at 18:00");
    ///
    /// // Two entries, because the chain is part of the key: neither answer can be
    /// // served to the other context.
    /// assert_eq!(kernel.cache_len(), 2);
    /// ```
    pub async fn issue_in(
        &self,
        request: Request,
        capability: &Capability,
        scope: Scope,
    ) -> Result<Representation> {
        let trace = self.global_scope();
        self.issue_inner(request, capability, None, None, trace, scope, 0)
            .await
    }

    /// Issue a request, recording every invocation of **this resolution** into
    /// `tracer` — isolated from any other traced resolution running concurrently on
    /// the same kernel. Each call gets its own span-id space (starting at 0), so
    /// returned spans are self-contained and re-basable. This is the
    /// concurrency-safe form of [`set_tracer`](Self::set_tracer) + issue: a wire
    /// server tracing one connection's call must use this, or concurrent tenants'
    /// events interleave into whichever collector was installed last.
    ///
    /// ★ **It is EXCLUSIVE of the globally-installed tracer**, not additive: this
    /// resolution records into `tracer` and into nothing else, so a host that calls
    /// [`set_tracer`](Self::set_tracer) *and* routes some resolutions through here
    /// will not see those resolutions in the global collector. That is deliberate —
    /// the alternative is a double-write, and a per-connection trace wants exactly one
    /// destination — but it is a behavioural contract you would otherwise only find by
    /// reading `issue_inner`, which is not where a host author looks.
    pub async fn issue_traced(
        &self,
        request: Request,
        capability: &Capability,
        tracer: Arc<dyn Tracer>,
    ) -> Result<Representation> {
        self.issue_inner(
            request,
            capability,
            None,
            None,
            Some(TraceScope::new(tracer)),
            Scope::empty(),
            0,
        )
        .await
    }

    /// Issue a request whose input was produced by an upstream pipe stage, folding
    /// that upstream's [`Provenance`] (expiry + golden threads) into the result's
    /// effective cacheability. So `source <X> | transform` is no more cacheable than
    /// `X` — cacheability flows down the pipe — and cutting `X`'s thread invalidates
    /// the transformed result too. The engine passes this for each pipe stage; plain
    /// [`issue`](Self::issue) is the no-upstream case.
    pub async fn issue_with_incoming(
        &self,
        request: Request,
        capability: &Capability,
        incoming: Provenance,
    ) -> Result<Representation> {
        self.issue_with_incoming_in(request, capability, incoming, Scope::empty())
            .await
    }

    /// [`issue_with_incoming`](Self::issue_with_incoming) **in a resolution chain**
    /// — the pipe stage resolved in `scope` ([`issue_in`](Self::issue_in)'s chain)
    /// with the upstream's provenance folded in exactly as in the empty chain. This
    /// is what lets an engine run a whole pipeline inside one chain, stage by stage,
    /// instead of resolving every stage in the plain root; the empty chain is
    /// [`issue_with_incoming`](Self::issue_with_incoming) exactly.
    pub async fn issue_with_incoming_in(
        &self,
        request: Request,
        capability: &Capability,
        incoming: Provenance,
        scope: Scope,
    ) -> Result<Representation> {
        let trace = self.global_scope();
        self.issue_inner(request, capability, None, Some(incoming), trace, scope, 0)
            .await
    }

    /// The resolution path, carrying the `parent` span of the invocation that issued
    /// this request (`None` at the top level), any upstream pipe [`Provenance`], and
    /// the resolution chain (`scope`) to resolve in.
    /// [`Issuer::issue_in_scope`] threads the span and the chain through re-entrant
    /// sub-requests so a recorded run links each node to its parent and a
    /// confinement holds below the endpoint that entered it; the public
    /// [`issue`](Self::issue) is the parentless, no-upstream, empty-chain entry
    /// point.
    #[allow(clippy::too_many_arguments)] // the one resolution path; every caller names each fact
    async fn issue_inner(
        &self,
        request: Request,
        capability: &Capability,
        parent: Option<u64>,
        incoming: Option<Provenance>,
        trace: Option<TraceScope>,
        scope: Scope,
        depth: u32,
    ) -> Result<Representation> {
        // ★ THE CUT SNAPSHOT, BEFORE ANYTHING ELSE. Everything this request is
        // about to observe is state as of *now*; a cut that lands after this point
        // invalidates whatever comes back, and `ReprCache::store` declines the entry
        // rather than pinning it to a generation the result never saw. Taken here
        // rather than beside the invocation because resolution reads state too, and
        // an over-early snapshot only ever costs a decline (never caching is safe;
        // caching a stale answer is not). See `cache.rs` for the race it closes.
        let taken = self.cache.snapshot();

        // ★ LOGICAL REWRITE, FIRST. A rewrite the kernel holds the table for is
        // applied before ANYTHING else reads the target: before the `urn:kernel:*`
        // dispatch, before the request id that keys the representation cache, before
        // the declared-capability floor, before the auto-cut that a mutating verb
        // fires, and before the `Invocation` the endpoint receives. Four of the five
        // decisions in `alias.rs` bear on this placement:
        //
        //   1. Capability BEFORE/AFTER — after. Authority is evaluated against the
        //      backing resource, the thing that will actually be touched. Checking
        //      the logical name first would make an alias an escalation device.
        //   2. `Meta` reports the BACKING name — the description is the endpoint's
        //      own, and the endpoint is reached under its real name.
        //   3. ONE cache entry, ONE golden thread for both names — the id and the
        //      cut are computed from the canonical target below.
        //   5. A refusal (cycle / over-long chain / malformed rewrite) stops HERE,
        //      pre-dispatch, rather than resolving a half-applied name. This is the
        //      only place that can: a `Space` cannot return an error, so an `Alias`
        //      composed by hand can only refuse at invoke.
        //
        // (Decision 4, what the catalog lists, is `Alias::entries`.)
        //
        // ★ But this is not the only rewrite that can happen. A `Space` may rewrite
        // on its own — an `Alias` or a `Rewrite` nested under some other overlay —
        // and the kernel cannot know about that in advance. Such a rewrite is
        // REPORTED back on `Resolved::canonical` and adopted just after the resolve
        // below, which is the earliest moment it is knowable and still ahead of
        // every consumer of the target. Decision 3 holds for both.
        let mut alias = self.canonicalize(&request.target)?;
        let mut request = match &alias {
            None => request,
            Some(hop) => {
                let mut rewritten = request;
                rewritten.target = hop.canonical().clone();
                rewritten
            }
        };

        // ★ THE NESTING BUDGET, before anything resolves. A sub-request deeper than
        // the bound is refused here — typed, traced, never cached — rather than
        // recursing until the stack dies. After the kernel's own rewrite so the
        // refusal names the canonical target; before the `urn:kernel:*` arm so
        // the bound is uniform (a kernel operation at excess depth is refused like
        // any other request, though none can recurse). Cheap: an integer compare
        // on every request, an event only on the refusal.
        if depth > self.max_depth {
            let span = trace.as_ref().map(TraceScope::next_span);
            self.trace_depth(
                &trace,
                &request,
                capability,
                span,
                parent,
                depth,
                alias.as_ref(),
                &scope,
            );
            return Err(Error::DepthExceeded {
                depth,
                target: request.target.clone(),
            });
        }

        // The kernel-behavior namespace (`urn:kernel:*`) is resolved by the kernel
        // itself — before the root space, which cannot shadow it — exposing the
        // kernel's own operations (cut a thread, inspect cache and threads) as
        // capability-gated resources. A *cacheable* builtin (the catalog) still
        // participates in the cache, so a re-resolution serves `[cached]`; the live
        // introspection builtins return `Always` and are simply never stored.
        //
        // ★ And before the resolution CHAIN, for the same reason: an injected
        // corridor cannot shadow a kernel operation either, and a severed chain
        // still reaches them. So the answer does not depend on the scope and the
        // key carries none (scope fingerprint 0): a confined endpoint reading the
        // catalog sees the same catalog as everyone else — a list of NAMES it may
        // not be able to resolve, which is names and not content.
        if let Some(op) = request.target.as_str().strip_prefix(KERNEL_NS) {
            let key = CacheKey::new(request.id(), capability_key(capability));
            let cacheable_verb = request.verb.is_cacheable();
            let started = self.now_stamp();
            if cacheable_verb {
                if let Some(cached) = self.cache.get(&key, started) {
                    return Ok(cached);
                }
            }
            let representation = self.issue_kernel(op, &request, capability, &trace, parent)?;
            let storable = cacheable_verb
                && match representation.expiry {
                    Expiry::Always => false,
                    Expiry::Never => true,
                    Expiry::At(_) => self.clock.is_some(),
                };
            if storable {
                self.cache.store(
                    key,
                    request.target.as_str().to_string(),
                    representation.clone(),
                    taken,
                    self.elapsed_since(started),
                );
            }
            return Ok(representation);
        }

        let cacheable_verb = request.verb.is_cacheable();
        // Start stamp, shared by the trace event and the always-on constraint window
        // (one clock read); the span only advances while this resolution is traced.
        let started = self.now_stamp();
        let span = trace.as_ref().map(TraceScope::next_span);

        // Resolution is synchronous, pure routing — and it runs BEFORE the cache
        // lookup, so the declared-capability floor below gates cached and computed
        // answers alike (a cached representation must never be served to a caller
        // the endpoint's own declaration would refuse). It runs in the request's
        // CHAIN: injected corridors innermost first, then the root — unless the
        // chain is severed, in which case a name the corridors do not bind is a
        // miss, exactly as if nothing were bound. That "exactly" is the point of
        // confinement: the caller cannot tell a name outside the chain from a name
        // that does not exist, so there is no decision here to misconfigure.
        let mut resolved = match scope.resolve_in(&request, &self.root) {
            Resolution::Hit(resolved) => resolved,
            Resolution::Miss => {
                // A rewrite that lands on nothing is reported as a rewrite. The error
                // names the CANONICAL target — the caller already knows what they
                // typed, and the name they have never seen is the informative half
                // ("why is it saying `urn:iki:`?" *is* the discovery). Beside it: a
                // trace event marking the miss, and an always-on per-rule counter at
                // `urn:kernel:aliases` for the operator with no tracer installed.
                // A miss inside a non-empty chain is traced for the same reason —
                // the error is indistinguishable from "unbound" by design, so the
                // trace is the only place it can show.
                if let Some(hop) = alias.as_ref() {
                    if let Some(table) = self.aliases.as_ref() {
                        table.record_unresolved(hop);
                    }
                }
                self.trace_miss(
                    &trace,
                    &request,
                    capability,
                    span,
                    parent,
                    alias.as_ref(),
                    &scope,
                );
                return Err(Error::Unresolved(request.target.clone()));
            }
        };

        // ★ ADOPT WHAT THE RESOLUTION REPORTED IT REWROTE — before the request id
        // below, before the capability floor, before the cache lookup, before the
        // auto-cut. The kernel's own table (canonicalized at the top) only ever
        // catches a rewrite the kernel was told about; an `Alias` composed by hand
        // under another overlay does the rewriting itself, and this is the only
        // point at which the kernel can learn of it. Without it the logical and
        // backing names get an id each — two cache entries and two golden threads
        // over one resource, so a `Sink` through one does not cut the other.
        //
        // Ordering, restated because it is the security-relevant half: this sits
        // AFTER `self.root.resolve` (routing is the only thing that knows) and
        // BEFORE the declared-capability floor, so authority is still evaluated
        // against the BACKING name — decision 1. Nothing has been invoked yet.
        if let Some(canonical) = resolved.canonical.take() {
            if canonical != request.target {
                let previous = std::mem::replace(&mut request.target, canonical.clone());
                // Chain the note onto any hop the kernel's own table already took,
                // so a two-stage rewrite reads `logical -> final`, not `mid -> final`.
                let logical = alias
                    .as_ref()
                    .map(|hop| hop.logical().clone())
                    .unwrap_or(previous);
                alias = Some(AliasHop::reported(logical, canonical));
            }
        }
        let alias = alias.as_ref();

        // ★ THE LIMITER. A hit on ⊥ (`Endpoint::is_limiter`) is the paper's
        // Definition 7: the identifier is admitted by a door whose endpoint is the
        // distinguished bottom, so it is UNRESOLVED — resolution neither serves it
        // nor continues outward — and the caller gets the SAME error, the same
        // text, as a name bound nowhere (§9.6's remark: a limiter leaves the
        // requester unable to tell "limited" from "no door anywhere"). Placed
        // after canonical adoption so an aliased name that lands on a limiter is
        // limited under its canonical, and before everything that would reveal or
        // remember a door: no request id is needed, the capability floor never
        // sees ⊥ (it declares nothing — and a `Denied` would reveal a binding),
        // nothing is looked up or stored, and `Meta` never reaches its arm
        // (describing a hole would reveal it). Only the trace may know.
        if resolved.endpoint.is_limiter() {
            self.trace_limited(&trace, &request, capability, span, parent, alias, &scope);
            return Err(Error::Unresolved(request.target.clone()));
        }

        // Identity is settled: the id keys the representation cache, and it is
        // derived from the target AFTER every rewrite has been accounted for.
        let id = request.id();

        // The baseline capability floor: **declared = enforced**. Every `requires`
        // the endpoint declares for this verb must be satisfied — on the SAME
        // `cap_satisfies` predicate selection and `urn:kernel:validate` use, so
        // what the manifold offers is exactly what the kernel admits. Declarations
        // are the coarse floor (parameterized families use the wildcard form,
        // `urn:cap:net:*`); a module's finer runtime gate (path/host ACL) remains
        // its ceiling on top. An endpoint declaring nothing is public, unchanged.
        // (`Meta` is exempt by construction: `action_specs()` carries no Meta
        // spec — self-description stays readable wherever the catalog offers it.)
        // A denial is REPORTED before it is returned: enforcement happens before
        // dispatch, so the endpoint is never entered and no observer inside it can
        // ever see the refusal. This is the only point that holds it.
        // The floor runs against the endpoint the CHAIN resolved to, whichever
        // corridor answered — a corridor that shadows a root door is checked on its
        // own declaration, not the root's. The declaration comes from the
        // per-endpoint memo: `describe()` runs once per bound endpoint, not once per
        // request, and a cut of `urn:kernel:bindings` forgets it (ledger #22, #510).
        if let Some(lacking) = self
            .floors
            .unsatisfied(&resolved.endpoint, &request, capability)
        {
            self.trace_denial(
                &trace, &request, capability, span, parent, &lacking, alias, &scope,
            );
            return Err(Error::Denied(
                self.denial_message(&lacking, &request, capability, alias),
            ));
        }

        // Representation-cache lookup (idempotent verbs only): serve a cached entry
        // whose golden-thread edges are all still current. A cut entry is evicted
        // here and recomputed below. The guard is dropped before any await.
        //
        // Keyed by the chain too: the same request in another chain may have
        // resolved to another endpoint, and a representation is shared across that
        // boundary only when the corridors consulted are the same on both sides.
        // The whole chain's fingerprint guarantees that (over-partitioned, sound);
        // the empty chain's is 0, so this key is unchanged for plain `issue`.
        let key = CacheKey::new(id, capability_key(capability)).in_scope(scope.fingerprint());
        if cacheable_verb {
            // Freshness is evaluated as of `started` — the instant this resolution
            // began, one clock reading shared with the trace event and the
            // constraint window rather than a fresh read per lookup.
            if let Some(cached) = self.cache.get(&key, started) {
                self.trace_record(
                    &trace,
                    &request,
                    capability,
                    span,
                    parent,
                    started,
                    true,
                    alias_notes(alias),
                    &scope,
                );
                self.record_resolution(&request, started, true);
                return Ok(cached);
            }
        }

        // Facts the endpoint attaches to its own span via `Invocation::trace_note`
        // (hoisted out of the invocation arm — the Meta arm has no invocation).
        let mut trace_notes = Vec::new();
        let representation = if request.verb == Verb::Meta {
            // Selection-driven Meta: the renderer emits the endpoint's description in its
            // *canonical* serializations (Turtle, and JSON/text where the renderer supports
            // them). Any other requested type is produced by transrepting the canonical
            // Turtle through a *selected* transreptor chain — so metadata rendering rides
            // the same transreptor model as everything else, with no per-format logic and no
            // hardcoded transreptor IRIs here.
            let renderer = self
                .meta
                .as_ref()
                .ok_or_else(|| Error::Endpoint("no Meta renderer configured".to_string()))?;
            let description = resolved.endpoint.describe();
            let target = meta_target(&request);
            let policy = meta_policy(&request)?;
            let rendered = match renderer.render(&description, &target) {
                Ok(repr) => repr.cacheable(),
                // The renderer doesn't emit this type directly — transrept the canonical
                // Turtle to it.
                Err(_) => {
                    self.transrept_meta(
                        &description,
                        &target,
                        &policy,
                        capability,
                        span,
                        &trace,
                        &scope,
                        depth,
                    )
                    .await?
                }
            };
            // A description is a function of the BINDING, not of any state, so the
            // one thread it hangs from is the binding-change thread — every face,
            // the canonical rendering and a transrepted one alike (ledger #26).
            rendered.depends_on(BINDINGS_THREAD)
        } else {
            // Invocation is asynchronous. On a scheduled kernel, hand it the spawner
            // and an owned self-handle so re-entrant fan-out runs concurrently.
            let issuer_arc: Option<Arc<dyn Issuer>> = self
                .self_ref
                .as_ref()
                .and_then(Weak::upgrade)
                .map(|kernel| {
                    let issuer: Arc<dyn Issuer> = kernel;
                    issuer
                });
            // …and the chain this request resolved in, so every sub-request the
            // endpoint issues resolves in the same one. This is what makes a
            // corridor hold "for the request and everything below it", and what
            // keeps a confinement closed under the endpoint it was applied to.
            let invocation =
                Invocation::with_issuer(&request, &resolved.bindings, capability, self)
                    .with_concurrency(self.spawner.clone(), issuer_arc)
                    .with_span(span)
                    .with_trace(trace.clone())
                    .with_scope(scope.clone())
                    .with_depth(depth);
            let representation = resolved.endpoint.invoke(&invocation).await?;
            // Effective expiry propagates from the dependencies: the result is no
            // fresher than its most volatile part. The endpoint's own expiry is met
            // with the combined dependency expiry — `Always` if either is volatile,
            // the earlier deadline if both are time-bounded, `Never` only if all are.
            let mut effective = representation
                .expiry
                .most_restrictive(invocation.dependency_expiry());
            // Golden threads propagate too: the result depends on its own declared
            // threads plus those of every sub-resource it resolved, so cutting any
            // of them invalidates this composite.
            let mut threads = representation.threads().clone();
            threads.extend(invocation.dependency_threads());
            // ★ THE DUAL OF THE AUTO-CUT. A cacheable answer to `Source` or `Exists`
            // hangs from the thread named after its own CANONICAL target — the very
            // name the mutating-verb cut below fires on — whether or not the
            // endpoint declared it. Before 0.1.73 the kernel stored only the
            // declared threads plus the sub-requests', so a write through the same
            // name invalidated a cached read only when its endpoint had remembered
            // `.depends_on(target)` by hand: every other cacheable endpoint fronting
            // mutable state was one forgotten line from serving stale bytes (hole A
            // of ledger #512; `docs/formalism/README.md` R4.4, now unconditional).
            //
            // Attached to the REPRESENTATION, not just the stored entry, so a
            // composite that sources this resource inherits the thread through the
            // dependency record above and is cut by the same write — an entry-only
            // edge would have closed the hole one level and reopened it at the
            // next. Canonical (post-alias, post-`Resolved::canonical`), so the
            // logical and backing names stay one thread. Only when the answer can
            // be stored: an `Always` result is never looked up, so the edge would
            // cost an allocation and buy nothing. Meta is served from `describe()`
            // in its own arm and gets no target thread — a binding change, not a
            // write, is what invalidates a description, so it hangs from
            // `urn:kernel:bindings` instead (ledger #510) — and the `urn:kernel:*`
            // intrinsics returned above are live state or faces derived from the
            // bindings, which hang from the same thread. A pure function gains a
            // thread nobody will ever cut: one generation lookup per validity
            // check. Measured 2026-09-26
            // (formalism §10): the first cut cost +20–28 ns (~7 %) on a ~325 ns
            // cache-hit read — not the lookup but the hit's CLONE of a now
            // non-empty thread set — and sharing the set behind an `Arc`
            // (`repr.rs`) took all of it back, to 0–10 ns under main.
            if effective != Expiry::Always {
                threads.insert(Thread::from(request.target.as_str()));
            }
            // …and from the pipe: an upstream stage's provenance folds in the same
            // way, so a transform over a piped input is no more cacheable than that
            // input, and inherits its threads.
            if let Some(incoming) = incoming {
                effective = effective.most_restrictive(incoming.expiry);
                threads.extend(incoming.threads);
            }
            trace_notes = invocation.take_trace_notes();
            // The rewrite is provenance of the invocation, so it leads the endpoint's
            // own notes rather than being appended after them.
            let mut notes = alias_notes(alias);
            notes.append(&mut trace_notes);
            trace_notes = notes;
            representation.with_expiry(effective).with_threads(threads)
        };
        if request.verb == Verb::Meta {
            // The Meta arm takes no invocation, so it never reached the note-merging
            // above; the rewrite still has to show on the event.
            trace_notes = alias_notes(alias);
        }
        // Computed (not served from cache) — record after the invocation completes.
        self.trace_record(
            &trace,
            &request,
            capability,
            span,
            parent,
            started,
            false,
            trace_notes,
            &scope,
        );
        self.record_resolution(&request, started, false);

        // A successful mutating verb invalidates its target: cut the thread named
        // after it, so cached `Source`s of that resource — and composites over
        // them — recompute. This is the internal half of the golden thread (the
        // kernel owns invalidation on writes); an external watcher cuts the same
        // thread on an out-of-band change.
        if request.verb.is_mutating() {
            self.cut(request.target.as_str());
        }

        // Store an idempotent result unless it's volatile (`Always`). A time-based
        // `At` deadline is only storable when a clock is present to later evaluate
        // it — otherwise the kernel could never tell when it expired, so it declines.
        let storable = cacheable_verb
            && match representation.expiry {
                Expiry::Always => false,
                Expiry::Never => true,
                Expiry::At(_) => self.clock.is_some(),
            };
        if storable {
            // The store may still decline: `taken` predates the invocation, so a cut
            // that landed while it ran means this representation is already stale.
            let stored = self.cache.store(
                key,
                request.target.as_str().to_string(),
                representation.clone(),
                taken,
                self.elapsed_since(started),
            );
            // Remember what the fingerprint meant, for the readout. Off the
            // empty-chain path entirely: a plain `issue` never takes this lock.
            if stored && !scope.is_empty() {
                self.scope_names
                    .lock()
                    .expect("scope names lock")
                    .entry(key.scope)
                    .or_insert_with(|| scope.to_string());
            }
        }
        Ok(representation)
    }

    /// Cut a golden thread: invalidate every cached representation that depends on
    /// it, directly or transitively through composition. Cheap — it bumps the
    /// thread's generation; dependent entries are evicted lazily on next lookup.
    /// A `Sink` that mutates a resource cuts the thread named after it; an external
    /// watcher cuts it on change.
    ///
    /// The cut is also *sequenced*, so a request already in flight cannot file a
    /// result that predates it (see [`cache`](crate::cache)).
    pub fn cut(&self, thread: impl Into<Thread>) {
        let thread = thread.into();
        // The binding-change thread also forgets the floor memo, which is not a cache
        // entry but hangs from the same predicate — so whoever cuts it, by this
        // method or by resolving `urn:kernel:cut`, invalidates both.
        if thread.as_str() == BINDINGS_THREAD {
            self.floors.clear();
        }
        self.cache.cut(thread);
    }

    /// Tell the kernel **the set of bindings it resolves against has changed**: cut
    /// [`BINDINGS_THREAD`], so the catalog, the action manifold, validation reports,
    /// every `Meta` answer and the per-endpoint floor memo are recomputed on their
    /// next read instead of describing a root that no longer exists.
    ///
    /// **When to call it.** After any change to what a name resolves to: a space with
    /// interior mutability took a new binding (a discovery mount table gained a peer,
    /// a module host reloaded code under a name), or the host swapped a space inside
    /// the tree. The kernel cannot see any of these — its root is an `Arc<dyn Space>`
    /// fixed at construction and a `Space` reports resolutions, not changes — so the
    /// party that made the change makes this call. Cheap: one generation bump and one
    /// map clear; dependents recompute lazily on their next read. Equivalent to
    /// `sink urn:kernel:cut urn:kernel:bindings`, the form for an endpoint or a remote
    /// peer (gated by `urn:cap:kernel:cut`).
    ///
    /// **What it does not do — a decision, not an omission.** A stored READ of a
    /// resource whose binding moved is not invalidated: a cached representation hangs
    /// from the thread named after its target and from the threads it declared, never
    /// from this one. Two reasons. A rebind is rare and usually leaves the resource's
    /// content as it was — the same module reloaded, a peer that came back — so
    /// throwing away every cached read in the kernel on each one would make the cache
    /// only as warm as the binding set is stable, and a discovery-driven root could
    /// keep it cold forever. And the host that rebinds knows which names it moved,
    /// which the kernel does not: it cuts those ([`Kernel::cut`] by name, or reads
    /// them off `urn:kernel:cache` and cuts what lies under the moved prefix) when the
    /// new binding answers differently. "Safe" would have been one more thread on
    /// every entry; "cheap" is the line that is not there (`docs/formalism/README.md`,
    /// R3.2).
    ///
    /// **Not a rebind API.** This reports a change; it does not make one. A
    /// `Kernel::set_root` — who may replace a running kernel's root — sits on the same
    /// trust line as [`Capability::root`] and is its own design question, deliberately
    /// not answered here.
    ///
    /// ```
    /// use ikigai_core::{builtins, Capability, EndpointSpace, Exact, Iri, Kernel, Request, Verb};
    /// use std::sync::Arc;
    ///
    /// let space = EndpointSpace::new().bind(Exact::new("urn:demo:up"), builtins::to_upper());
    /// let kernel = Kernel::new(Arc::new(space));
    /// let root = Capability::root();
    /// let manifold = || Request::new(Verb::Source, Iri::parse("urn:kernel:actions").unwrap());
    ///
    /// futures::executor::block_on(kernel.issue(manifold(), &root)).unwrap();
    /// assert!(kernel.is_cached(&manifold(), &root));
    ///
    /// // …the host swapped a space, discovery added a mount, a module reloaded:
    /// kernel.bindings_changed();
    /// assert!(!kernel.is_cached(&manifold(), &root), "recomputed on its next read");
    /// ```
    pub fn bindings_changed(&self) {
        self.cut(BINDINGS_THREAD);
    }

    /// Resolve a `urn:kernel:*` request — a kernel operation exposed as a
    /// capability-gated resource. `op` is the suffix after `urn:kernel:`.
    ///
    /// These are deliberately resources, not just Rust methods: an endpoint can
    /// invalidate another resource by *resolving* `urn:kernel:cut` (no special
    /// `Issuer` method); a remote peer can do the same over the wire, gated by its
    /// capability; and `describe`/the dashboard can see them. Results are live
    /// kernel state, so they are uncacheable — except the faces derived from the
    /// bindings (catalog, actions, validate), which cache under [`BINDINGS_THREAD`].
    fn issue_kernel(
        &self,
        op: &str,
        request: &Request,
        capability: &Capability,
        trace: &Option<TraceScope>,
        parent: Option<u64>,
    ) -> Result<Representation> {
        // The capability gate for these operations. A refusal is REPORTED, not just
        // returned — a denied `urn:kernel:cut` is the same kind of security fact as
        // a denied endpoint, and the same reasoning applies: the operation never
        // runs, so nothing downstream can report it.
        //
        // ★ Asymmetry, deliberate: a SUCCESSFUL kernel builtin records no event at
        // all (this whole arm returns before the trace path, and is likewise absent
        // from the constraint window — these are introspection, not throughput). So
        // an observer sees denials from `urn:kernel:*` and never successes. That is
        // the security-correct trade, not an oversight: the refusal is the fact
        // worth keeping.
        let require_cap = |scope: &str| -> Result<()> {
            if capability.allows(scope) {
                return Ok(());
            }
            // Spans are allocated lazily here: this arm short-circuits before the
            // resolution path's span, so a denial is the only thing that needs one.
            let span = trace.as_ref().map(TraceScope::next_span);
            // A kernel operation is resolved ahead of the chain, so its denial is
            // reported in the empty one — whatever chain the caller ran in.
            self.trace_denial(
                trace,
                request,
                capability,
                span,
                parent,
                scope,
                None,
                &Scope::empty(),
            );
            Err(Error::Denied(format!(
                "capability does not grant `{scope}`"
            )))
        };
        // ★ DECLARED = ENFORCED, made mechanical rather than intentional. Each kernel
        // operation's [`Description`](crate::Description) states the scopes its verb
        // requires (`crate::kernel_ops`), and those declarations are what the catalog
        // publishes and what `urn:kernel:actions` filters the manifold by. Gating on
        // the declaration HERE, before dispatch, makes over-offering impossible: a
        // scope this namespace advertises is a scope this namespace demands, and the
        // manifold cannot drift into a lie by a description being edited alone.
        //
        // It does not replace the per-arm `require_cap` calls below — those stay as
        // each operation's local, readable statement of its own authority, and as the
        // fail-safe if the table and the arm ever disagree. A second capability check
        // is cheap; the arms are what a reviewer reads. What it removes is the
        // possibility of the DECLARATION being the looser of the two. The converse
        // (an arm enforcing a scope nobody declared, so the manifold over-offers) is
        // not statically preventable and is pinned by
        // `kernel_ops_declare_exactly_what_they_enforce`.
        //
        // `Meta` declares nothing by construction (`action_specs` drops it), so
        // describing an operation stays ungated exactly as it always has been.
        for scope in crate::kernel_ops::required_scopes(op, request.verb) {
            require_cap(&scope)?;
        }
        match (op, request.verb) {
            // Cut a golden thread. The thread is the sunk content — so
            // `sink urn:kernel:cut <thread>` works — or an explicit `thread` arg.
            ("cut", Verb::Sink) => {
                require_cap("urn:cap:kernel:cut")?;
                let thread = kernel_arg(request, "thread")
                    .or_else(|| kernel_arg(request, "content"))
                    .ok_or_else(|| Error::MissingArgument("thread".to_string()))?;
                self.cut(thread);
                Ok(kernel_text(format!("cut {thread}\n")))
            }
            // Inspect the cache: a count, then one line per entry — the IRI it was
            // resolved from, its representation type and size, how many golden
            // threads it depends on (cut any of them and this entry recomputes), and
            // the resolution chain it was computed in (`root` for the empty chain,
            // else the chain as `Scope` renders it, or its fingerprint if the name
            // is no longer known).
            ("cache", Verb::Source) => {
                require_cap("urn:cap:kernel:inspect")?;
                let mut rows = self.cache.rows_with_scope();
                rows.sort_by(|a, b| (&a.target, a.scope).cmp(&(&b.target, b.scope)));
                let names = {
                    let mut names = self.scope_names.lock().expect("scope names lock");
                    names.retain(|fingerprint, _| rows.iter().any(|r| r.scope == *fingerprint));
                    names.clone()
                };
                let bound = self.cache.bound();
                let mut body = format!(
                    "cache\n  entries  {} / {}\n  size     {} / {}\n",
                    rows.len(),
                    bound.max_entries,
                    human_size(self.cache.bytes()),
                    human_size(bound.max_bytes),
                );
                let width = rows
                    .iter()
                    .map(|r| r.target.chars().count())
                    .max()
                    .unwrap_or(0)
                    .min(48);
                for row in rows {
                    let deps = if row.threads == 1 {
                        "1 thread".to_string()
                    } else {
                        format!("{} threads", row.threads)
                    };
                    let size = human_size(row.bytes);
                    let scope = match row.scope {
                        0 => "root".to_string(),
                        fingerprint => names
                            .get(&fingerprint)
                            .cloned()
                            .unwrap_or_else(|| format!("{fingerprint:016x}")),
                    };
                    let (target, media) = (row.target, row.media_type);
                    body.push_str(&format!(
                        "  {target:<width$}  {media:<24}  {size:>9}  {deps:<10}  {scope}\n"
                    ));
                }
                Ok(kernel_text(body))
            }
            // Inspect the golden threads that have been cut, and how many times.
            ("threads", Verb::Source) => {
                require_cap("urn:cap:kernel:inspect")?;
                let mut rows = self.cache.generation_rows();
                let mut body = String::from("threads (cut generations)\n");
                if rows.is_empty() {
                    body.push_str("  (none cut)\n");
                } else {
                    rows.sort();
                    for (thread, generation) in rows {
                        body.push_str(&format!("  {thread}  gen {generation}\n"));
                    }
                }
                Ok(kernel_text(body))
            }
            // ★ The rewrite table AS A RESOURCE — not a hardcode. An operator can read
            // the live table out of the kernel: which logical names are being
            // rewritten, to what, and — the part that matters when something is
            // mysteriously not resolving — how often each rule has FIRED and how
            // often it fired onto a name nothing was bound to. That readout needs no
            // tracer installed, which is the whole point: the tracer is opt-in and
            // this question arrives unannounced.
            ("aliases", Verb::Source) => {
                require_cap("urn:cap:kernel:inspect")?;
                let mut body = String::from("aliases\n");
                let Some(table) = self.aliases.as_ref() else {
                    body.push_str("  (no rewrite table installed)\n");
                    return Ok(kernel_text(body));
                };
                body.push_str(&format!("  rules     {}\n", table.rules().len()));
                body.push_str(&format!("  max hops  {}\n", table.max_hops()));
                let width = table
                    .rules()
                    .iter()
                    .map(|rule| rule.from().chars().count())
                    .max()
                    .unwrap_or(0)
                    .min(40);
                for rule in table.rules() {
                    let from = rule.from();
                    let kind = rule.kind().keyword();
                    body.push_str(&format!("  {kind:<6}  {from:<width$}  ->  {}", rule.to()));
                    body.push_str(&format!("   {} hops", rule.hops()));
                    if rule.unresolved() > 0 {
                        body.push_str(&format!(", {} unresolved", rule.unresolved()));
                    }
                    if rule.refused() > 0 {
                        body.push_str(&format!(", {} refused", rule.refused()));
                    }
                    body.push('\n');
                }
                Ok(kernel_text(body))
            }
            // Inspect the host scheduler (backend, threads, live task counts).
            ("scheduler", Verb::Source) => {
                require_cap("urn:cap:kernel:inspect")?;
                let mut body = String::from("scheduler\n");
                match &self.scheduler {
                    Some(reporter) => {
                        for (label, value) in reporter.rows() {
                            body.push_str(&format!("  {label:<10} {value}\n"));
                        }
                    }
                    // No scheduler injected ⇒ the runtime-free single-threaded default.
                    None => {
                        body.push_str("  backend    single\n");
                        body.push_str("  threads    1\n");
                    }
                }
                Ok(kernel_text(body))
            }
            // The throughput readout: where the constraint is right now — the
            // resources that consumed the most *uncached* compute over the recent
            // window. Cache hits are excluded from the total (they cost nothing); the
            // heaviest target is the bottleneck (Goldratt step 1).
            ("constraint", Verb::Source) => {
                require_cap("urn:cap:kernel:inspect")?;
                Ok(kernel_text(self.render_constraint()))
            }
            // The kernel's own catalog: every bound endpoint's `describe()` as one RDF
            // (Turtle) graph — so the kernel is queryable *about itself* with SPARQL and
            // renderable to HTML via transreption. Cacheable under `urn:kernel:bindings`:
            // valid until the host reports that the binding set changed
            // ([`Kernel::bindings_changed`]). Each Exact-bound endpoint is resolved and
            // rendered via the meta renderer; template patterns (not concrete IRIs) render after them via
            // the probe walk, once per description id — an endpoint bound both exactly
            // and by template (the calendar shape) is not described twice, and an
            // endpoint bound ONLY by template (urn:file:{path}) is no longer invisible.
            ("catalog", Verb::Source) => {
                require_cap("urn:cap:kernel:inspect")?;
                let renderer = self
                    .meta
                    .as_ref()
                    .ok_or_else(|| Error::Endpoint("no Meta renderer configured".to_string()))?;
                let turtle = ReprType::new("text/turtle");
                let mut body = String::new();
                let mut rendered_ids = BTreeSet::new();
                let mut templates = Vec::new();
                // The kernel's own operations come first (see [`Self::described`]),
                // so the catalog opens with the resources that make the rest of it
                // legible. Note the division the design already draws: the catalog
                // says what EXISTS — every kernel operation, for a caller with the
                // inspect authority this arm demanded — while `urn:kernel:actions`
                // says what you MAY DO and filters by capability.
                let space = self.described();
                for entry in space.entries().unwrap_or_default() {
                    let Ok(iri) = Iri::parse(&entry.pattern) else {
                        templates.push(entry);
                        continue;
                    };
                    // Through the shared probe: a hit on ⊥ is dropped, so a name a
                    // limiter carved out is listed by no catalog (the subtraction
                    // of the reachability algebra, at the face that computes reach).
                    if let Some(resolved) = crate::select::probe(&space, iri) {
                        let description = resolved.endpoint.describe();
                        rendered_ids.insert(description.id.clone());
                        if let Ok(repr) = renderer.render(&description, &turtle) {
                            if let Ok(text) = String::from_utf8(repr.bytes) {
                                body.push_str(text.trim_end());
                                body.push('\n');
                            }
                        }
                    }
                }
                for entry in templates {
                    let Some(described) = crate::select::describe_entry(&space, &entry) else {
                        continue;
                    };
                    if !rendered_ids.insert(described.description.id.clone()) {
                        continue;
                    }
                    if let Ok(repr) = renderer.render(&described.description, &turtle) {
                        if let Ok(text) = String::from_utf8(repr.bytes) {
                            body.push_str(text.trim_end());
                            body.push('\n');
                        }
                    }
                }
                Ok(Representation::new(
                    ReprType::new("text/turtle").with_param("charset", "utf-8"),
                    body.into_bytes(),
                )
                .cacheable()
                .depends_on(BINDINGS_THREAD))
            }
            // Selection as a resource: the endpoints whose required inputs are satisfiable by
            // the RDF classes in `types` (comma/space-separated IRIs) — "given these typed
            // entities, what can I do with them?" (see [`crate::select_action`]). One endpoint
            // IRI per line, so it pipes into a `..` map. Cacheable like the catalog (a pure
            // function of the binding set + `types`).
            ("actions", Verb::Source) => {
                // NO inspect gate, deliberately: unlike the catalog (which discloses the
                // whole kernel), the manifold is SELF-LIMITING — it only ever contains
                // actions the asking capability can invoke. "What can I do?" must be
                // answerable by any capability about itself; denying it just forces an
                // attenuated agent into trial-and-error probing.
                // Every axis optional: bare `urn:kernel:actions` is the caller's whole
                // capability-scoped action manifold — "what can I do at all?".
                let types_arg = kernel_arg(request, "types").unwrap_or_default();
                let types: Vec<&str> = types_arg
                    .split([',', ' ', '\n', '\t'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                let verb = match kernel_arg(request, "verb") {
                    None => None,
                    Some(v) => Some(parse_verb(v)?),
                };
                let want = kernel_arg(request, "want");
                let query = crate::select::ActionQuery {
                    present: &types,
                    verb,
                    want,
                    // The AMBIENT capability: selection is cap-scoped by construction,
                    // with the same `allows` check enforcement runs at invoke time. The
                    // representation cache keys on the capability fingerprint, so a
                    // cached manifold is only ever served back to the authority it was
                    // computed for.
                    capability: Some(capability),
                };
                let matches = self.select_actions(&query);
                let turtle = kernel_arg(request, "as") == Some("text/turtle");
                if turtle {
                    // The auditable face: each match an ik:ActionMatch node joining the
                    // selection to the full contract in the catalog graph. An exact match
                    // carries its resolvable IRI as ik:endpoint; a template match carries
                    // its pattern as the ik:template LITERAL — `<urn:file:{path}>` would
                    // not be a legal IRI in Turtle, and downstream RDF parsers must keep
                    // parsing the whole manifold.
                    let mut body = String::from("@prefix ik: <https://ikigai-rs.dev/ns#> .\n");
                    for m in &matches {
                        let named = if Iri::parse(&m.endpoint).is_ok() {
                            format!("ik:endpoint <{}>", m.endpoint)
                        } else {
                            format!(
                                "ik:template \"{}\"",
                                m.endpoint.replace('\\', "\\\\").replace('"', "\\\"")
                            )
                        };
                        // `m.action` embeds the endpoint id, which `Description` never
                        // validates — percent-encode rather than trust it, or a `>` in an
                        // id closes the IRI and the rest of the manifold parses as Turtle.
                        body.push_str(&format!(
                            "\n<{}> a ik:ActionMatch ;\n    {named} ;\n    ik:verb \"{:?}\"",
                            crate::escape_iri_fragment(&m.action),
                            m.verb
                        ));
                        for scope in &m.requires {
                            let term = if scope.starts_with("urn:")
                                || scope.starts_with("http://")
                                || scope.starts_with("https://")
                            {
                                format!("<{}>", crate::escape_iri_fragment(scope))
                            } else {
                                format!("\"{}\"", scope.replace('\\', "\\\\").replace('"', "\\\""))
                            };
                            body.push_str(&format!(" ;\n    ik:requires {term}"));
                        }
                        if m.missing_optional > 0 {
                            body.push_str(&format!(
                                " ;\n    ik:missingOptional {}",
                                m.missing_optional
                            ));
                        }
                        body.push_str(" .\n");
                    }
                    return Ok(Representation::new(
                        ReprType::new("text/turtle").with_param("charset", "utf-8"),
                        body.into_bytes(),
                    )
                    .cacheable()
                    .depends_on(BINDINGS_THREAD));
                }
                // The plain face stays one endpoint identifier per line (deduped across
                // its matched actions): a resolvable IRI for an exact binding, the
                // URI-template pattern for a template one. Exact lines still pipe into a
                // `..` map unchanged; a template line names the pattern to fill in.
                let mut body = String::new();
                let mut seen = BTreeSet::new();
                for m in &matches {
                    if seen.insert(&m.endpoint) {
                        body.push_str(&m.endpoint);
                        body.push('\n');
                    }
                }
                Ok(Representation::new(
                    ReprType::new("text/plain").with_param("charset", "utf-8"),
                    body.into_bytes(),
                )
                .cacheable()
                .depends_on(BINDINGS_THREAD))
            }
            // Validated invoke, stage four of the selection funnel: check a PROPOSED
            // invocation against the action's declared contract BEFORE firing it —
            // required inputs present, one_of respected, XSD scalars plausible, no
            // unknown argument names, and the ambient capability satisfies the
            // action's requires (the same checks selection and enforcement run).
            // The report is genuine SHACL vocabulary: violations are data, and
            // sh:resultPath joins each one back to the catalog's input node. Pure
            // function of (action, args, capability) — cacheable; the cache key
            // carries the capability fingerprint.
            ("validate", Verb::Source) => {
                let description = match (
                    kernel_arg(request, "action"),
                    kernel_arg(request, "endpoint"),
                ) {
                    (Some(action), _) => {
                        let rest = action.strip_prefix("urn:ikigai:endpoint:").ok_or_else(|| {
                            Error::Endpoint(format!(
                                "validate: action must be a catalog action IRI (urn:ikigai:endpoint:<id>:action:<verb>), not `{action}`"
                            ))
                        })?;
                        let (id, verb_name) = rest.split_once(":action:").ok_or_else(|| {
                            Error::Endpoint(format!(
                                "validate: `{action}` names no :action: segment"
                            ))
                        })?;
                        self.description_for_id(id)?
                            .map(|d| (d, parse_verb(verb_name)))
                            .ok_or_else(|| {
                                Error::Endpoint(format!("validate: no endpoint with id `{id}`"))
                            })?
                    }
                    (None, Some(endpoint)) => {
                        let iri = Iri::parse(endpoint).map_err(|e| {
                            Error::Endpoint(format!("validate: bad endpoint IRI: {e}"))
                        })?;
                        // Through the shared probe: a limited name pre-flights as
                        // unresolved, as it would resolve.
                        let Some(resolved) = crate::select::probe(&self.root, iri) else {
                            return Err(Error::Unresolved(Iri::parse(endpoint).expect("parsed")));
                        };
                        let verb_name = kernel_arg(request, "verb")
                            .ok_or_else(|| Error::MissingArgument("verb".to_string()))?;
                        (resolved.endpoint.describe(), parse_verb(verb_name))
                    }
                    (None, None) => {
                        return Err(Error::MissingArgument(
                            "action (or endpoint + verb)".to_string(),
                        ))
                    }
                };
                let (description, verb) = description;
                let verb = verb?;
                let Some(spec) = description
                    .action_specs()
                    .into_iter()
                    .find(|a| a.verb == verb)
                else {
                    return Err(Error::Endpoint(format!(
                        "validate: `{}` declares no {verb:?} action",
                        description.id
                    )));
                };
                let proposed = kernel_arg(request, "args").unwrap_or_default();
                let verb_lower = format!("{verb:?}").to_lowercase();
                let action_iri =
                    format!("urn:ikigai:endpoint:{}:action:{verb_lower}", description.id);
                // resultPath must join to the catalog: explicitly authored actions own
                // action-scoped input nodes; synthesized ones reference endpoint-level.
                let explicit = description.actions.iter().any(|a| a.verb == verb);
                let input_ns = if explicit {
                    format!("{action_iri}:input:")
                } else {
                    format!("urn:ikigai:endpoint:{}:input:", description.id)
                };
                let report =
                    validate_against_spec(&action_iri, &input_ns, &spec, proposed, capability);
                Ok(Representation::new(
                    ReprType::new("text/turtle").with_param("charset", "utf-8"),
                    report.into_bytes(),
                )
                .cacheable()
                .depends_on(BINDINGS_THREAD))
            }
            // `describe urn:kernel:<op>` — the operation's own self-description, so every
            // kernel operation is introspectable exactly like a bound endpoint (and the
            // engine routes its named arguments, which it does only for *declared* inputs).
            // Rendered on this sync path in its canonical forms (JSON for the engine,
            // Turtle for `describe`); the async transrept-to-other-types route the normal
            // Meta path uses isn't available here, so any other requested type falls back
            // to canonical Turtle.
            //
            // Ungated, and not by omission: `Description::action_specs` excludes `Meta`
            // (it is universal, never a selectable action), so no kernel operation can
            // declare a scope for it — which is what the pre-gate above enforces against.
            // Reading a contract discloses nothing the crate documentation does not; the
            // authority to *invoke* is checked on the verb arms below.
            (op, Verb::Meta) => {
                let description = crate::kernel_ops::description(op)
                    .ok_or_else(|| Error::Unresolved(request.target.clone()))?;
                let renderer = self
                    .meta
                    .as_ref()
                    .ok_or_else(|| Error::Endpoint("no Meta renderer configured".to_string()))?;
                let target = meta_target(request);
                let repr = renderer
                    .render(&description, &target)
                    .or_else(|_| renderer.render(&description, &ReprType::new("text/turtle")))?;
                Ok(repr.cacheable())
            }
            _ => Err(Error::Unresolved(request.target.clone())),
        }
    }

    /// Aggregate the constraint window by target and render the heaviest first.
    /// Ranks by uncached compute time when the kernel has a clock, else by uncached
    /// call count — so the readout is meaningful even on a clockless kernel.
    fn render_constraint(&self) -> String {
        let window = self.constraint.lock().expect("constraint lock");
        let total = window.len();
        // Per target: (uncached_ms, calls, cached, any_timed).
        let mut by_target: HashMap<&str, (u64, usize, usize, bool)> = HashMap::new();
        for sample in window.iter() {
            let row = by_target.entry(&sample.target).or_default();
            row.1 += 1;
            if sample.cache_hit {
                row.2 += 1;
            }
            if let Some(ms) = sample.elapsed_ms {
                row.3 = true;
                if !sample.cache_hit {
                    row.0 += ms;
                }
            }
        }
        let mut rows: Vec<_> = by_target.into_iter().collect();
        // Heaviest uncached time first; break ties by uncached call count.
        rows.sort_by(|a, b| {
            let a_uncached = a.1 .1 - a.1 .2;
            let b_uncached = b.1 .1 - b.1 .2;
            b.1 .0.cmp(&a.1 .0).then(b_uncached.cmp(&a_uncached))
        });

        let mut body = format!("constraint  (last {total} resolutions)\n");
        if rows.is_empty() {
            body.push_str("  (no resolutions yet)\n");
            return body;
        }
        for (rank, (target, (uncached_ms, calls, cached, timed))) in rows.iter().take(5).enumerate()
        {
            let uncached = calls - cached;
            let cached_pct = if *calls > 0 { cached * 100 / calls } else { 0 };
            // The leader is the constraint only if it actually consumed capacity.
            let marker = if rank == 0 && (*uncached_ms > 0 || uncached > 0) {
                "   ← constraint"
            } else {
                ""
            };
            let cost = if *timed {
                format!("{uncached_ms}ms uncached")
            } else {
                format!("{uncached} uncached")
            };
            body.push_str(&format!(
                "  {target}{marker}\n    {cost} · {calls} calls · {cached_pct}% cached\n"
            ));
        }
        body
    }

    /// The number of representations currently cached (diagnostics/tests).
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Whether issuing `request` right now would be served from the cache — a
    /// read-only probe that neither resolves nor mutates. `false` for verbs that
    /// aren't cacheable, and for any request whose result isn't already cached
    /// (including one that would be a miss). Lets a caller report cache state
    /// without the observer effect of actually issuing the request.
    pub fn is_cached(&self, request: &Request, capability: &Capability) -> bool {
        self.is_cached_in(request, capability, &Scope::empty())
    }

    /// [`is_cached`](Self::is_cached) **in a resolution chain**: whether issuing
    /// `request` through [`issue_in`](Self::issue_in) with `scope` right now would
    /// be served from the cache. The chain is a dimension of the key, so the
    /// empty chain's entry and a corridor's are different entries and the probe
    /// answers for the one asked about; the empty chain is
    /// [`is_cached`](Self::is_cached) exactly. Freshness is judged on the
    /// **kernel's** clock, as serving is — a chain's own clock
    /// ([`Scope::with_named_at`]) never enters it.
    pub fn is_cached_in(&self, request: &Request, capability: &Capability, scope: &Scope) -> bool {
        if !request.verb.is_cacheable() {
            return false;
        }
        // The probe canonicalizes exactly as the serving path does, or it would
        // answer "not cached" for a logical name whose backing name IS cached —
        // the two disagreeing about one entry, which is the whole failure mode
        // decision 3 exists to close. A refused rewrite resolves to nothing, so it
        // is not cached either.
        //
        // Limit, stated because it is invisible otherwise: this sees only rewrites
        // the kernel's OWN table performs. A canonical reported by a `Space`
        // (`Resolved::canonical`) is knowable only by resolving, and resolution is
        // not side-effect-free for the overlay family — a `RateLimit` spends budget
        // to answer. A read-only probe must not spend it, so a nested rewrite makes
        // this answer "not cached" under the logical name while the serving path
        // correctly serves one entry for both. Conservative in the safe direction:
        // it under-reports a hit, never invents one.
        let id = match self.canonicalize(&request.target) {
            Err(_) => return false,
            Ok(None) => request.id(),
            Ok(Some(hop)) => {
                let mut canonical = request.clone();
                canonical.target = hop.canonical().clone();
                canonical.id()
            }
        };
        // Read-only probe: an entry that is cut (a thread bumped) or expired (its
        // deadline passed) is not "cached", matching what `valid_cached` would
        // serve. Keyed by capability too, so the probe answers "cached *for this
        // authority*" — consistent with what issuing under it would actually serve.
        // Does not evict — eviction happens on the serving path.
        self.cache.probe(
            &CacheKey::new(id, capability_key(capability)).in_scope(scope.fingerprint()),
            self.now_stamp(),
        )
    }

    /// Enumerate this kernel's bindings, if the root space supports enumeration:
    /// the kernel's own `urn:kernel:*` operations first, then the root space's.
    /// `None` when the root space is not enumerable — the kernel operations alone
    /// are not an answer to "what is bound here?", and reporting `Some` for a
    /// non-enumerable root would turn "cannot say" into "nothing else".
    ///
    /// The kernel operations are listed here (and described through the same
    /// composed space the catalog walks) because they are resolved *intrinsically* —
    /// intercepted ahead of the root space, which therefore never binds them. Before
    /// this, `list` returned every endpoint in the system except the ones that make
    /// the rest of it legible.
    pub fn entries(&self) -> Option<Vec<SpaceEntry>> {
        self.root.entries().map(|bound| {
            let mut entries = crate::kernel_ops::KernelOps.entries().unwrap_or_default();
            entries.extend(bound);
            entries
        })
    }

    /// The space every `entries → Meta → describe` walk runs over: the kernel's own
    /// operations composed in FRONT of the root space, mirroring the intercept order
    /// on the issue path (where the root cannot shadow `urn:kernel:*` either).
    ///
    /// **Description only.** Resolution proper still goes straight to the root space
    /// after the `urn:kernel:` intercept, so nothing here can change what a request
    /// resolves to; the endpoints this space hands back refuse invocation. What it
    /// changes is what the kernel can SAY about itself — the catalog, the action
    /// manifold, describe-by-id and `urn:kernel:validate` all consume this walk, so
    /// they inherit the kernel from one place instead of nine.
    fn described(&self) -> Fallback {
        Fallback::new(vec![
            Arc::new(crate::kernel_ops::KernelOps),
            Arc::clone(&self.root),
        ])
    }

    /// [`described`](Self::described) for a non-empty chain: the kernel's own
    /// operations ahead, then the chain's corridors innermost first, then the root
    /// unless severed — the same order [`Scope::resolve_in`] consults, from the
    /// same walk, so a chain's manifold is what resolution in that chain reaches.
    fn described_in(&self, scope: &Scope) -> crate::space::ChainView {
        scope.view(Some(Arc::new(crate::kernel_ops::KernelOps)), &self.root)
    }
}

/// A stable fingerprint of a capability's authority, used to namespace cache entries.
/// Root (full authority) gets its own namespace; a scoped capability's namespace is
/// derived from its sorted scope set (so two equal capabilities share a namespace, and
/// a narrower one cannot collide with a broader one). BLAKE3-based (truncated to the
/// cache key's `u64`): `DefaultHasher` is not collision-resistant, and this key
/// partitions cached representations BY AUTHORITY — an engineered collision would
/// serve one authority's cached result to another. Recomputed per request; the cache
/// holds the `u64`, never the capability.
fn capability_key(capability: &Capability) -> u64 {
    let mut hasher = blake3::Hasher::new();
    match capability.scopes() {
        // Root authority — a fixed namespace distinct from any scoped set.
        None => crate::hashing::feed_u8(&mut hasher, 0),
        Some(scopes) => {
            crate::hashing::feed_u8(&mut hasher, 1);
            // `scopes` is a `BTreeSet`, so iteration is sorted ⇒ a stable
            // fingerprint; `feed_str` length-prefixes, so scope boundaries can't
            // be forged by concatenation.
            for scope in scopes {
                crate::hashing::feed_str(&mut hasher, scope);
            }
        }
    }
    let digest = hasher.finalize();
    u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("8-byte prefix"))
}

/// The kernel's baseline capability floor for a resolved endpoint: every scope the
/// endpoint's description `requires` for the request's verb must be satisfied by
/// the caller's capability (all of them — an action needing net AND a secret needs
/// both). Predicate = [`cap_satisfies`](crate::select) — identical to selection
/// and validation, so offer, preflight, and enforcement agree. A verb the
/// description doesn't declare carries no spec and passes (dispatch behavior for
/// undeclared verbs is unchanged).
///
/// Yields the first scope the caller lacks rather than the finished error, so the
/// call site can REPORT the denial ([`Kernel::trace_denial`]) before returning it.
/// `None` means the floor is met. Short-circuits on the first failure, as the
/// error form did.
/// The trace notes disclosing a logical rewrite: empty when the target was not
/// aliased, so an un-aliased request's events are byte-identical to before.
fn alias_notes(alias: Option<&AliasHop>) -> Vec<(String, String)> {
    alias
        .map(|hop| vec![(ALIAS_NOTE.to_string(), hop.to_string())])
        .unwrap_or_default()
}

/// The trace notes disclosing the resolution chain — and, when the chain carries a
/// clock, the instant it read: empty for the empty chain, so a plain `issue`'s
/// events are byte-identical to before scopes existed.
fn scope_notes(scope: &Scope) -> Vec<(String, String)> {
    if scope.is_empty() {
        return Vec::new();
    }
    let mut notes = vec![(SCOPE_NOTE.to_string(), scope.to_string())];
    if let Some(now) = scope.now() {
        notes.push((SCOPE_CLOCK_NOTE.to_string(), now.as_millis().to_string()));
    }
    notes
}

/// The reserved kernel-behavior namespace prefix.
pub(crate) const KERNEL_NS: &str = "urn:kernel:";

/// Parse a `verb=` argument (case-insensitive verb name).
fn parse_verb(name: &str) -> Result<Verb> {
    match name.to_ascii_lowercase().as_str() {
        "source" => Ok(Verb::Source),
        "sink" => Ok(Verb::Sink),
        "exists" => Ok(Verb::Exists),
        "delete" => Ok(Verb::Delete),
        "meta" => Ok(Verb::Meta),
        other => Err(Error::Endpoint(format!(
            "unknown verb \"{other}\" (source, sink, exists, delete, meta)"
        ))),
    }
}

/// Check `proposed` (key=value pairs joined by `&`/newlines) against `spec`,
/// including the capability pre-flight, and render the SHACL report.
/// `input_ns` is the IRI prefix of the spec's input nodes in the catalog
/// (action-scoped for explicitly authored actions, endpoint-scoped for
/// synthesized ones), so `sh:resultPath` joins violations to the contract.
fn validate_against_spec(
    action_iri: &str,
    input_ns: &str,
    spec: &crate::describe::ActionSpec,
    proposed: &str,
    capability: &Capability,
) -> String {
    use crate::describe::InputSource;
    let escape = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut provided: Vec<(&str, &str)> = Vec::new();
    for pair in proposed.split(['&', '\n']) {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        match pair.split_once('=') {
            Some((k, v)) => provided.push((k.trim(), v.trim())),
            None => provided.push((pair, "")),
        }
    }

    // (message, Some(input node IRI)) per violation.
    let mut violations: Vec<(String, Option<String>)> = Vec::new();

    for scope in &spec.requires {
        if !crate::select::cap_satisfies(capability, scope) {
            violations.push((format!("the capability does not grant `{scope}`"), None));
        }
    }
    for input in &spec.inputs {
        let node = format!("{input_ns}{}", input.name);
        let value = provided
            .iter()
            .find(|(k, _)| *k == input.name)
            .map(|(_, v)| *v);
        if input.required && input.source != InputSource::Binding && value.is_none() {
            violations.push((
                format!("required input `{}` is missing", input.name),
                Some(node),
            ));
            continue;
        }
        let Some(value) = value else { continue };
        if !input.one_of.is_empty() && !input.one_of.iter().any(|v| v == value) {
            violations.push((
                format!(
                    "`{value}` is not an accepted value of `{}` (one of: {})",
                    input.name,
                    input.one_of.join(", ")
                ),
                Some(node),
            ));
            continue;
        }
        if let Some(class) = &input.class {
            let plausible = match class.as_str() {
                "http://www.w3.org/2001/XMLSchema#boolean" => value == "true" || value == "false",
                "http://www.w3.org/2001/XMLSchema#integer" => value.parse::<i64>().is_ok(),
                "http://www.w3.org/2001/XMLSchema#dateTime" => {
                    let b = value.as_bytes();
                    b.len() >= 10
                        && b[..4].iter().all(u8::is_ascii_digit)
                        && b[4] == b'-'
                        && b[5..7].iter().all(u8::is_ascii_digit)
                        && b[7] == b'-'
                        && b[8..10].iter().all(u8::is_ascii_digit)
                }
                _ => true, // entity classes are not statically checkable
            };
            if !plausible {
                violations.push((
                    format!(
                        "`{value}` does not look like a {} for `{}`",
                        class, input.name
                    ),
                    Some(node),
                ));
            }
        }
    }
    for (k, _) in &provided {
        if !spec.inputs.iter().any(|i| i.name == *k) {
            violations.push((
                format!("unknown argument `{k}` — not in this action's contract"),
                None,
            ));
        }
    }

    let mut ttl = String::from("@prefix sh: <http://www.w3.org/ns/shacl#> .\n\n");
    if violations.is_empty() {
        ttl.push_str(
            "<urn:ikigai:validation:report> a sh:ValidationReport ;\n    sh:conforms true .\n",
        );
        return ttl;
    }
    ttl.push_str("<urn:ikigai:validation:report> a sh:ValidationReport ;\n    sh:conforms false");
    for (message, path) in &violations {
        // Both the focus node and the result path carry an endpoint id (and the path an
        // argument name) that nothing validated — encode, never interpolate raw.
        ttl.push_str(" ;\n    sh:result [ a sh:ValidationResult ;\n        sh:resultSeverity sh:Violation ;\n        sh:focusNode <");
        ttl.push_str(&crate::escape_iri_fragment(action_iri));
        ttl.push('>');
        if let Some(path) = path {
            ttl.push_str(&format!(
                " ;\n        sh:resultPath <{}>",
                crate::escape_iri_fragment(path)
            ));
        }
        ttl.push_str(&format!(
            " ;\n        sh:resultMessage \"{}\" ]",
            escape(message)
        ));
    }
    ttl.push_str(" .\n");
    ttl
}

/// An inline argument of a kernel request, decoded as UTF-8.
fn kernel_arg<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    match request.args.get(name) {
        Some(ArgRef::Inline(bytes)) => std::str::from_utf8(bytes).ok(),
        _ => None,
    }
}

/// A `text/plain` representation of live kernel state (uncacheable by default).
fn kernel_text(body: String) -> Representation {
    Representation::new(
        ReprType::new("text/plain").with_param("charset", "utf-8"),
        body.into_bytes(),
    )
}

/// A short human-readable byte size (`512 B`, `1.5 KB`, `2.0 MB`) for the cache readout.
fn human_size(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// The representation type a `Meta` request asks for: the `as` inline argument
/// if present, else `text/turtle`.
fn meta_target(request: &Request) -> ReprType {
    if let Some(ArgRef::Inline(bytes)) = request.args.get("as") {
        if let Ok(media) = std::str::from_utf8(bytes) {
            return ReprType::new(media);
        }
    }
    ReprType::new("text/turtle")
}

/// The name of the `Meta` argument that consents to a lossy transreption step:
/// `lossy=allow`. `lossy=deny` is the default, spelled; any other value is refused
/// rather than read as the default, so a mistyped consent cannot silently become a
/// refusal that then blames the face.
pub const META_LOSSY_ARG: &str = "lossy";

/// The [`TransreptionPolicy`] a `Meta` request plans under: lossless-only unless it
/// carries [`META_LOSSY_ARG`]`=allow`.
fn meta_policy(request: &Request) -> Result<TransreptionPolicy> {
    let Some(ArgRef::Inline(bytes)) = request.args.get(META_LOSSY_ARG) else {
        return Ok(TransreptionPolicy::lossless());
    };
    match std::str::from_utf8(bytes) {
        Ok("allow") => Ok(TransreptionPolicy::allow_lossy()),
        Ok("deny") => Ok(TransreptionPolicy::lossless()),
        other => Err(Error::InvalidArgument {
            name: META_LOSSY_ARG.to_string(),
            detail: format!(
                "expected `allow` or `deny`, got `{}`",
                other.unwrap_or("<non-utf8>")
            ),
        }),
    }
}

#[async_trait]
impl Issuer for Kernel {
    async fn issue(&self, request: Request, capability: &Capability) -> Result<Representation> {
        // Delegate to the inherent method (which the context calls back into).
        Kernel::issue(self, request, capability).await
    }

    async fn issue_with_parent(
        &self,
        request: Request,
        capability: &Capability,
        parent: Option<u64>,
    ) -> Result<Representation> {
        // Compatibility path (no scope threaded — an external Issuer wrapper that
        // predates issue_scoped): record into the global tracer if one is set,
        // preserving pre-scope behavior. A sub-resource resolves on its own
        // merits — no pipe upstream here. No depth threaded either: an external
        // wrapper on this seam restarts the count (see `Issuer::issue_at_depth`).
        let trace = self.global_scope();
        self.issue_inner(request, capability, parent, None, trace, Scope::empty(), 0)
            .await
    }

    async fn issue_scoped(
        &self,
        request: Request,
        capability: &Capability,
        parent: Option<u64>,
        trace: Option<TraceScope>,
    ) -> Result<Representation> {
        // Re-entrant sub-request: thread the issuing node's span AND its
        // resolution's trace scope through, so the recorded events link
        // parent → child (across the fan-out spawn) inside the RIGHT trace —
        // concurrent traced resolutions never bleed into each other's collectors.
        // (Compatibility path — no chain and no depth threaded; `issue_at_depth`
        // is the full seam and the one `Invocation` calls.)
        self.issue_inner(request, capability, parent, None, trace, Scope::empty(), 0)
            .await
    }

    async fn issue_in_scope(
        &self,
        request: Request,
        capability: &Capability,
        parent: Option<u64>,
        trace: Option<TraceScope>,
        scope: Scope,
    ) -> Result<Representation> {
        // Span, trace scope AND resolution chain, so a sub-request resolves in the
        // chain its issuer ran in — a confinement holds below the endpoint that
        // entered it, an injected corridor is visible all the way down. Depth 0:
        // a caller on this seam did not say how deep it is, and the count restarts
        // here rather than guessing (see `Issuer::issue_at_depth`).
        self.issue_inner(request, capability, parent, None, trace, scope, 0)
            .await
    }

    async fn issue_at_depth(
        &self,
        request: Request,
        capability: &Capability,
        parent: Option<u64>,
        trace: Option<TraceScope>,
        scope: Scope,
        depth: u32,
    ) -> Result<Representation> {
        // The full re-entrant seam, and the one `Invocation` calls: everything
        // `issue_in_scope` carries plus the nesting depth the budget is checked
        // against in `issue_inner`.
        self.issue_inner(request, capability, parent, None, trace, scope, depth)
            .await
    }

    fn now(&self) -> Option<Time> {
        self.clock.as_ref().map(|clock| clock.now())
    }

    fn select_transreptor(&self, from: &str, to: &str) -> Option<Vec<TransreptionStep>> {
        // Delegate to the inherent method (selection over the kernel's root space).
        Kernel::select_transreptor(self, from, to)
    }

    fn select_action(&self, present: &[&str]) -> Vec<ActionMatch> {
        Kernel::select_action(self, present)
    }

    fn select_transreptor_in(
        &self,
        from: &str,
        to: &str,
        scope: &Scope,
    ) -> Option<Vec<TransreptionStep>> {
        // Selection in the chain the invocation runs in: what its sub-requests could
        // actually resolve.
        Kernel::select_transreptor_in(self, from, to, scope)
    }

    fn select_transreptor_in_with(
        &self,
        from: &str,
        to: &str,
        scope: &Scope,
        policy: &TransreptionPolicy,
    ) -> Option<Vec<TransreptionStep>> {
        Kernel::select_transreptor_in_with(self, from, to, scope, policy)
    }

    fn select_action_in(&self, present: &[&str], scope: &Scope) -> Vec<ActionMatch> {
        Kernel::select_action_in(self, present, scope)
    }

    fn record_subtree(&self, parent: Option<u64>, spans: Vec<TraceEvent>) {
        if !self.tracing.load(Ordering::Relaxed) || spans.is_empty() {
            return;
        }
        let Some(tracer) = self.tracer.lock().expect("tracer lock").clone() else {
            return;
        };
        // Re-base: give each incoming span a fresh local id (a remote kernel's ids
        // must not collide with this kernel's), remap parent links within the
        // subtree, and parent the subtree's roots (parent `None` on the remote)
        // under `parent` — the local mount node that forwarded the request.
        let mut remap = std::collections::HashMap::new();
        for event in &spans {
            remap.insert(
                event.span,
                self.span_counter.fetch_add(1, Ordering::Relaxed),
            );
        }
        for event in spans {
            let span = remap[&event.span];
            let parent = match event.parent {
                Some(p) => remap.get(&p).copied().or(parent),
                None => parent,
            };
            tracer.record(TraceEvent {
                span,
                parent,
                ..event
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arg::ArgRef;
    use crate::builtins;
    use crate::describe::Description;
    use crate::endpoint::{BoxFuture, Endpoint, FnEndpoint, Invocation, Issuer, Spawner};
    use crate::grammar::Exact;
    use crate::iri::Iri;
    use crate::repr::ReprType;
    use crate::space::EndpointSpace;
    use crate::verb::Verb;
    use futures::executor::block_on;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    /// A [`Clock`] whose "now" the test can set, to drive deadline expiry
    /// deterministically (no real time involved).
    #[derive(Clone)]
    struct TestClock(Arc<AtomicU64>);
    impl TestClock {
        fn at(millis: u64) -> Self {
            TestClock(Arc::new(AtomicU64::new(millis)))
        }
        fn set(&self, millis: u64) {
            self.0.store(millis, Ordering::SeqCst);
        }
    }
    impl Clock for TestClock {
        fn now(&self) -> Time {
            Time::from_millis(self.0.load(Ordering::SeqCst))
        }
    }

    fn iri(s: &str) -> Iri {
        Iri::parse(s).unwrap()
    }

    struct EchoIdRenderer;
    impl MetaRenderer for EchoIdRenderer {
        fn render(&self, description: &Description, _target: &ReprType) -> Result<Representation> {
            Ok(Representation::new(
                ReprType::new("text/plain"),
                description.id.clone().into_bytes(),
            )
            .cacheable())
        }
    }

    #[test]
    fn meta_is_routed_through_the_renderer() {
        let space =
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper());
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(EchoIdRenderer));
        let cap = Capability::root();
        let rep = block_on(kernel.issue(Request::new(Verb::Meta, iri("urn:test:to-upper")), &cap))
            .unwrap();
        assert_eq!(rep.bytes, b"toUpper");
    }

    /// A Turtle-only meta renderer (like `ikigai-vocab`'s): emits `text/turtle` for the
    /// canonical request, errors for anything else — which is what makes the kernel fall
    /// through to selection-driven transreption.
    struct TurtleishRenderer;
    impl MetaRenderer for TurtleishRenderer {
        fn render(&self, description: &Description, target: &ReprType) -> Result<Representation> {
            match target.media_type.as_str() {
                "text/turtle" | "*/*" | "" => Ok(Representation::new(
                    ReprType::new("text/turtle"),
                    format!("<urn:x> a ik:Endpoint ; ik:id \"{}\" .", description.id).into_bytes(),
                )
                .cacheable()),
                other => Err(Error::Endpoint(format!(
                    "unsupported meta target `{other}`"
                ))),
            }
        }
    }

    /// A stub auto-invocable transreptor (`text/turtle → application/rdf+xml`): wraps the
    /// piped `content` so a test can see it ran.
    fn stub_rdf_transrept() -> FnEndpoint {
        FnEndpoint::new("rdf-transrept", |inv: &Invocation<'_>| {
            let content = inv.inline_str("content").unwrap_or("");
            Ok(Representation::new(
                ReprType::new("application/rdf+xml"),
                format!("RDFXML({content})").into_bytes(),
            )
            .cacheable())
        })
        .with_description(
            Description::new("rdf-transrept")
                .verb(Verb::Source)
                .input(crate::describe::ArgSpec::new("content"))
                .input(crate::describe::ArgSpec::new("as"))
                .transreptor(["text/turtle"], ["application/rdf+xml"]),
        )
    }

    fn meta_kernel() -> Kernel {
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:test:to-upper"), builtins::to_upper())
            .bind(Exact::new("urn:rdf:transrept"), stub_rdf_transrept());
        Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleishRenderer))
    }

    fn meta_as(kernel: &Kernel, target: &str, as_type: &str) -> Representation {
        let request = Request::new(Verb::Meta, iri(target))
            .with_arg("as", ArgRef::Inline(as_type.as_bytes().to_vec()));
        block_on(kernel.issue(request, &Capability::root())).unwrap()
    }

    #[test]
    fn an_endpoint_can_select_a_transreptor_through_its_invocation() {
        // An endpoint reaches the kernel's transreptor selection via inv.select_transreptor
        // (delegated through the Issuer) — the seam urn:transrept:auto / content-negotiation
        // build on.
        let probe = FnEndpoint::new("probe", |inv: &Invocation<'_>| {
            let body = match inv.select_transreptor("text/turtle", "application/rdf+xml") {
                Some(steps) => steps
                    .iter()
                    .map(|s| s.endpoint.clone())
                    .collect::<Vec<_>>()
                    .join(","),
                None => "none".to_string(),
            };
            Ok(Representation::new(
                ReprType::new("text/plain"),
                body.into_bytes(),
            ))
        });
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:probe"), probe)
            .bind(Exact::new("urn:rdf:transrept"), stub_rdf_transrept());
        let kernel = Kernel::new(Arc::new(space));
        let rep = block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:probe")),
            &Capability::root(),
        ))
        .unwrap();
        assert_eq!(String::from_utf8(rep.bytes).unwrap(), "urn:rdf:transrept");
    }

    #[test]
    fn a_detached_invocation_selects_nothing() {
        let request = Request::new(Verb::Source, iri("urn:x"));
        let bindings = crate::grammar::Bindings::default();
        let cap = Capability::root();
        let inv = Invocation::detached(&request, &bindings, &cap);
        assert!(inv
            .select_transreptor("text/turtle", "application/rdf+xml")
            .is_none());
        assert!(inv.select_action(&["https://schema.org/Person"]).is_empty());
    }

    #[test]
    fn an_endpoint_can_select_actions_through_its_invocation() {
        // An endpoint surfaces the actions available for a set of present types via
        // inv.select_action (delegated through the Issuer) — the seed of layer action-inference.
        let probe = FnEndpoint::new("probe", |inv: &Invocation<'_>| {
            let body = inv
                .select_action(&["https://schema.org/Person"])
                .iter()
                .map(|m| m.endpoint.clone())
                .collect::<Vec<_>>()
                .join(",");
            Ok(Representation::new(
                ReprType::new("text/plain"),
                body.into_bytes(),
            ))
        });
        let greet = FnEndpoint::new("greet", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new("greet")
                .verb(Verb::Source)
                .input(crate::describe::ArgSpec::new("who").class("https://schema.org/Person")),
        );
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:probe"), probe)
            .bind(Exact::new("urn:demo:greet"), greet);
        let kernel = Kernel::new(Arc::new(space));
        let rep = block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:probe")),
            &Capability::root(),
        ))
        .unwrap();
        assert_eq!(String::from_utf8(rep.bytes).unwrap(), "urn:demo:greet");
    }

    #[test]
    fn meta_serves_canonical_turtle_directly() {
        let rep = meta_as(&meta_kernel(), "urn:test:to-upper", "text/turtle");
        assert_eq!(rep.repr_type.media_type, "text/turtle");
        assert!(String::from_utf8(rep.bytes)
            .unwrap()
            .contains("ik:id \"toUpper\""));
    }

    #[test]
    fn meta_transrepts_to_a_non_canonical_type_via_selection() {
        // as=application/rdf+xml: the renderer can't emit it, so the kernel renders canonical
        // Turtle and runs the selected turtle→rdf+xml transreptor over it.
        let rep = meta_as(&meta_kernel(), "urn:test:to-upper", "application/rdf+xml");
        assert_eq!(rep.repr_type.media_type, "application/rdf+xml");
        let body = String::from_utf8(rep.bytes).unwrap();
        assert!(body.starts_with("RDFXML("), "transreptor ran: {body}");
        assert!(
            body.contains("ik:id \"toUpper\""),
            "over the canonical turtle: {body}"
        );
    }

    #[test]
    fn meta_falls_back_to_turtle_when_no_transreptor_reaches_the_type() {
        // as=application/pdf: nothing converts turtle→pdf, so fall back to canonical Turtle.
        let rep = meta_as(&meta_kernel(), "urn:test:to-upper", "application/pdf");
        assert_eq!(rep.repr_type.media_type, "text/turtle");
        assert!(String::from_utf8(rep.bytes)
            .unwrap()
            .contains("ik:id \"toUpper\""));
    }

    /// A stub PROJECTION (`text/turtle → text/x-summary`, declared `.lossy()`): a
    /// summariser registered as a transreptor — the exposure ledger #514 names.
    fn stub_summarize() -> FnEndpoint {
        FnEndpoint::new("summarize", |inv: &Invocation<'_>| {
            let content = inv.inline_str("content").unwrap_or("");
            Ok(Representation::new(
                ReprType::new("text/x-summary"),
                format!("SUMMARY({})", content.len()).into_bytes(),
            )
            .cacheable())
        })
        .with_description(
            Description::new("summarize")
                .verb(Verb::Source)
                .input(crate::describe::ArgSpec::new("content"))
                .input(crate::describe::ArgSpec::new("as"))
                .transreptor(["text/turtle"], ["text/x-summary"])
                .lossy(),
        )
    }

    fn meta_kernel_with_a_projection() -> Kernel {
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:test:to-upper"), builtins::to_upper())
            .bind(Exact::new("urn:rdf:transrept"), stub_rdf_transrept())
            .bind(Exact::new("urn:demo:summarize"), stub_summarize());
        Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleishRenderer))
    }

    #[test]
    fn meta_refuses_a_lossy_route_by_default_and_names_the_consent() {
        // as=text/x-summary: the only route is a projection. Not substituted (a
        // substitution would hide that a route exists), not taken (it would lose
        // information under the type asked for): refused, naming the step and the
        // argument that consents.
        let kernel = meta_kernel_with_a_projection();
        let request = Request::new(Verb::Meta, iri("urn:test:to-upper"))
            .with_arg("as", ArgRef::Inline(b"text/x-summary".to_vec()));
        let err = block_on(kernel.issue(request, &Capability::root())).unwrap_err();
        match &err {
            Error::Endpoint(msg) => {
                assert!(msg.contains("lossy"), "{msg}");
                assert!(msg.contains("`urn:demo:summarize`"), "{msg}");
                assert!(msg.contains("lossy=allow"), "{msg}");
            }
            other => panic!("expected Error::Endpoint, got {other:?}"),
        }
        // Spelling the default is the default.
        let request = Request::new(Verb::Meta, iri("urn:test:to-upper"))
            .with_arg("as", ArgRef::Inline(b"text/x-summary".to_vec()))
            .with_arg(META_LOSSY_ARG, ArgRef::Inline(b"deny".to_vec()));
        assert!(matches!(
            block_on(kernel.issue(request, &Capability::root())),
            Err(Error::Endpoint(_))
        ));
    }

    #[test]
    fn meta_crosses_a_lossy_route_with_consent() {
        let kernel = meta_kernel_with_a_projection();
        let request = Request::new(Verb::Meta, iri("urn:test:to-upper"))
            .with_arg("as", ArgRef::Inline(b"text/x-summary".to_vec()))
            .with_arg(META_LOSSY_ARG, ArgRef::Inline(b"allow".to_vec()));
        let rep = block_on(kernel.issue(request, &Capability::root())).unwrap();
        assert_eq!(rep.repr_type.media_type, "text/x-summary");
        assert!(String::from_utf8(rep.bytes)
            .unwrap()
            .starts_with("SUMMARY("));
    }

    #[test]
    fn meta_through_a_lossless_route_is_unchanged_beside_a_projection() {
        // The projection's presence changes nothing for a type a lossless route reaches,
        // and nothing for a type no route reaches (canonical Turtle, as before).
        let kernel = meta_kernel_with_a_projection();
        let rep = meta_as(&kernel, "urn:test:to-upper", "application/rdf+xml");
        assert_eq!(rep.repr_type.media_type, "application/rdf+xml");
        let rep = meta_as(&kernel, "urn:test:to-upper", "application/pdf");
        assert_eq!(rep.repr_type.media_type, "text/turtle");
    }

    #[test]
    fn a_mistyped_consent_is_refused_not_read_as_the_default() {
        let kernel = meta_kernel_with_a_projection();
        let request = Request::new(Verb::Meta, iri("urn:test:to-upper"))
            .with_arg("as", ArgRef::Inline(b"text/x-summary".to_vec()))
            .with_arg(META_LOSSY_ARG, ArgRef::Inline(b"alow".to_vec()));
        assert!(matches!(
            block_on(kernel.issue(request, &Capability::root())),
            Err(Error::InvalidArgument { name, .. }) if name == META_LOSSY_ARG
        ));
    }

    #[test]
    fn an_invocation_plans_through_a_projection_only_with_its_own_consent() {
        // The seam an endpoint consents through: `select_transreptor` (the default
        // policy) sees no route; `select_transreptor_with(allow_lossy)` sees it and the
        // step reports the loss.
        let probe = FnEndpoint::new("probe", |inv: &Invocation<'_>| {
            let by_default = inv.select_transreptor("text/turtle", "text/x-summary");
            let consented = inv.select_transreptor_with(
                "text/turtle",
                "text/x-summary",
                &TransreptionPolicy::allow_lossy(),
            );
            let body = format!(
                "{} {}",
                by_default.map_or("none".to_string(), |p| p.len().to_string()),
                consented.map_or("none".to_string(), |p| format!(
                    "{}:{}",
                    p[0].endpoint,
                    if crate::select::is_lossless_plan(&p) {
                        "lossless"
                    } else {
                        "lossy"
                    }
                )),
            );
            Ok(Representation::new(
                ReprType::new("text/plain"),
                body.into_bytes(),
            ))
        });
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:probe"), probe)
            .bind(Exact::new("urn:demo:summarize"), stub_summarize());
        let kernel = Kernel::new(Arc::new(space));
        let rep = block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:probe")),
            &Capability::root(),
        ))
        .unwrap();
        assert_eq!(
            String::from_utf8(rep.bytes).unwrap(),
            "none urn:demo:summarize:lossy"
        );
    }

    #[test]
    fn meta_without_a_renderer_errors() {
        let space =
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper());
        let kernel = Kernel::new(Arc::new(space));
        let cap = Capability::root();
        assert!(
            block_on(kernel.issue(Request::new(Verb::Meta, iri("urn:test:to-upper")), &cap))
                .is_err()
        );
    }

    #[test]
    fn catalog_enumerates_every_bound_endpoint_through_the_renderer() {
        // The catalog is the kernel's self-describing graph: it walks the root
        // space and renders each entry's `describe()` via the Meta renderer, so a
        // SPARQL query / transreption can run over "the endpoints" as a resource.
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:test:to-upper"), builtins::to_upper())
            .bind(Exact::new("urn:test:echo"), builtins::echo());
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(EchoIdRenderer));
        let cap = Capability::root();
        let rep =
            block_on(kernel.issue(Request::new(Verb::Source, iri("urn:kernel:catalog")), &cap))
                .unwrap();
        let body = String::from_utf8(rep.bytes).unwrap();
        assert!(
            body.contains("toUpper"),
            "catalog should describe toUpper: {body}"
        );
        assert!(
            body.contains("echo"),
            "catalog should describe echo: {body}"
        );
        assert_eq!(rep.repr_type.media_type, "text/turtle");
        // Cacheable: a downstream SPARQL query inherits this so it can hit cache.
        assert_eq!(rep.expiry, Expiry::Never);
        // And it genuinely participates in the cache despite living in the
        // `urn:kernel:*` namespace — re-resolution is served from cache.
        let catalog = || Request::new(Verb::Source, iri("urn:kernel:catalog"));
        assert!(
            kernel.is_cached(&catalog(), &cap),
            "catalog should be cached after first issue"
        );
        assert_eq!(kernel.cache_len(), 1, "exactly the catalog is cached");
        block_on(kernel.issue(catalog(), &cap)).unwrap();
        assert_eq!(
            kernel.cache_len(),
            1,
            "re-issue is a cache hit, not a second entry"
        );
    }

    /// A file-like endpoint for the template-grammar tests: one capability-gated Source
    /// action whose template variable is a declared Binding-source input.
    fn template_file_endpoint() -> FnEndpoint {
        use crate::describe::ActionSpec;
        FnEndpoint::new("file", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new("file").action(
                ActionSpec::new(Verb::Source)
                    .requires("urn:cap:fs:read:*")
                    .input(crate::describe::ArgSpec::new("path").binding()),
            ),
        )
    }

    #[test]
    fn the_catalog_describes_template_bound_endpoints_once() {
        use crate::grammar::UriTemplate;
        // A calendar-shaped endpoint bound BOTH exactly (bare = week) and by template,
        // plus an endpoint bound ONLY by template — the previously-invisible case.
        let cal = || {
            FnEndpoint::new("cal", |_inv| {
                Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
            })
            .with_description(Description::new("cal").verb(Verb::Source))
        };
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:personal:calendar"), cal())
            .bind(
                UriTemplate::parse("urn:personal:calendar:{period}").unwrap(),
                cal(),
            )
            .bind(
                UriTemplate::parse("urn:file:{path}").unwrap(),
                template_file_endpoint(),
            );
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(EchoIdRenderer));
        let rep = block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:kernel:catalog")),
            &Capability::root(),
        ))
        .unwrap();
        let body = String::from_utf8(rep.bytes).unwrap();
        assert_eq!(
            body.matches("cal").count(),
            1,
            "a dual-bound endpoint is described once, not per binding: {body}"
        );
        assert_eq!(
            body.matches("file").count(),
            1,
            "a template-only endpoint joins the catalog: {body}"
        );
    }

    #[test]
    fn the_manifold_offers_template_actions_on_both_faces() {
        use crate::grammar::UriTemplate;
        let space = EndpointSpace::new().bind(
            UriTemplate::parse("urn:file:{path}").unwrap(),
            template_file_endpoint(),
        );
        let kernel = Kernel::new(Arc::new(space));
        let manifold = |cap: &Capability, as_turtle: bool| {
            let mut req = Request::new(Verb::Source, iri("urn:kernel:actions"));
            if as_turtle {
                req = req.with_arg("as", ArgRef::Inline(b"text/turtle".to_vec()));
            }
            let rep = block_on(kernel.issue(req, cap)).unwrap();
            String::from_utf8(rep.bytes).unwrap()
        };

        // The plain face lists the pattern; the turtle face carries it as the
        // ik:template LITERAL — never an illegal `<…{…}>` IRI a downstream RDF
        // parser would choke on mid-manifold.
        let reader = Capability::scoped(["urn:cap:fs:read:/notes"]);
        assert!(manifold(&reader, false).contains("urn:file:{path}"));
        let turtle = manifold(&reader, true);
        assert!(
            turtle.contains("<urn:ikigai:endpoint:file:action:source> a ik:ActionMatch"),
            "{turtle}"
        );
        assert!(
            turtle.contains("ik:template \"urn:file:{path}\""),
            "{turtle}"
        );
        assert!(!turtle.contains("<urn:file:{"), "{turtle}");

        // Under a capability with no fs grant, both faces simply lack it.
        let denied = Capability::scoped(["urn:cap:unrelated"]);
        assert!(!manifold(&denied, false).contains("urn:file"));
        assert!(!manifold(&denied, true).contains("urn:file"));
    }

    #[test]
    fn a_hostile_endpoint_id_cannot_inject_triples_into_the_kernel_faces() {
        // Both kernel-authored Turtle faces embed an endpoint id in an IRI position, and
        // `Description::new` validates nothing — so an id carrying `>` would close the
        // IRI and the rest of the document would parse as attacker-authored Turtle.
        // Same hole, two more emitters than the vocabulary projection.
        let evil = "ev>il";
        let endpoint = FnEndpoint::new(evil, |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new(evil)
                .verb(Verb::Source)
                .requires("urn:cap:x> . <urn:everything")
                .input(crate::describe::ArgSpec::new("a>b")),
        );
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:evil"), endpoint),
        ));
        let cap = Capability::scoped(["urn:cap:x> . <urn:everything"]);

        // The action manifold …
        let manifold = String::from_utf8(
            block_on(
                kernel.issue(
                    Request::new(Verb::Source, iri("urn:kernel:actions"))
                        .with_arg("as", ArgRef::Inline(b"text/turtle".to_vec())),
                    &cap,
                ),
            )
            .unwrap()
            .bytes,
        )
        .unwrap();
        assert!(
            manifold.contains("<urn:ikigai:endpoint:ev%3Eil:action:source> a ik:ActionMatch"),
            "{manifold}"
        );
        assert!(
            manifold.contains("ik:requires <urn:cap:x%3E%20.%20%3Curn:everything>"),
            "{manifold}"
        );
        assert!(!manifold.contains("<urn:everything>"), "{manifold}");

        // … and the SHACL pre-flight report, whose focus node and result path carry the
        // id and the argument name.
        let report = String::from_utf8(
            block_on(
                kernel.issue(
                    Request::new(Verb::Source, iri("urn:kernel:validate"))
                        .with_arg(
                            "action",
                            ArgRef::Inline(b"urn:ikigai:endpoint:ev>il:action:source".to_vec()),
                        )
                        .with_arg("args", ArgRef::Inline(b"nope=1".to_vec())),
                    &cap,
                ),
            )
            .unwrap()
            .bytes,
        )
        .unwrap();
        assert!(
            report.contains("sh:focusNode <urn:ikigai:endpoint:ev%3Eil:action:source>"),
            "{report}"
        );
        assert!(!report.contains("<urn:ikigai:endpoint:ev>il"), "{report}");
    }

    #[test]
    fn validate_pre_flights_a_template_action_by_catalog_iri() {
        use crate::grammar::UriTemplate;
        // Stage four of the funnel reaches template-bound endpoints: the catalog action
        // IRI a template match carries resolves to its contract via the id lookup.
        let space = EndpointSpace::new().bind(
            UriTemplate::parse("urn:file:{path}").unwrap(),
            template_file_endpoint(),
        );
        let kernel = Kernel::new(Arc::new(space));
        let validate = |args: &str| {
            let req = Request::new(Verb::Source, iri("urn:kernel:validate"))
                .with_arg(
                    "action",
                    ArgRef::Inline(b"urn:ikigai:endpoint:file:action:source".to_vec()),
                )
                .with_arg("args", ArgRef::Inline(args.as_bytes().to_vec()));
            let rep = block_on(kernel.issue(req, &Capability::root())).unwrap();
            String::from_utf8(rep.bytes).unwrap()
        };
        assert!(validate("path=notes.md").contains("sh:conforms true"));
        // A Binding-source input may live in the IRI rather than the args, so its
        // absence from `args` is NOT a violation (existing validate semantics) — but
        // the contract is genuinely live: an undeclared argument is still caught.
        assert!(validate("").contains("sh:conforms true"));
        let unknown = validate("bogus=1");
        assert!(
            unknown.contains("unknown argument `bogus`"),
            "the template action's contract is enforced: {unknown}"
        );
    }

    #[test]
    fn describe_pattern_covers_templates_and_exact_iris() {
        use crate::grammar::UriTemplate;
        let greet = FnEndpoint::new("greet", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(Description::new("greet").verb(Verb::Source));
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:demo:greet"), greet)
            .bind(
                UriTemplate::parse("urn:file:{path}").unwrap(),
                template_file_endpoint(),
            );
        let kernel = Kernel::new(Arc::new(space));
        assert_eq!(
            kernel.describe_pattern("urn:demo:greet").unwrap().id,
            "greet"
        );
        assert_eq!(
            kernel.describe_pattern("urn:file:{path}").unwrap().id,
            "file",
            "a template pattern — unparseable as an Iri — still yields its contract"
        );
        assert!(kernel.describe_pattern("urn:absent:{x}").is_none());
    }

    #[test]
    fn actions_resource_lists_type_satisfiable_endpoints() {
        // urn:kernel:actions exposes select_action as a resource: given the RDF classes in
        // `types`, list the endpoints those entities could drive — one IRI per line.
        let greet = FnEndpoint::new("greet", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new("greet")
                .verb(Verb::Source)
                .input(crate::describe::ArgSpec::new("who").class("https://schema.org/Person")),
        );
        let space = EndpointSpace::new()
            // untyped required inputs → never an inferred action
            .bind(Exact::new("urn:test:to-upper"), builtins::to_upper())
            .bind(Exact::new("urn:demo:greet"), greet);
        let kernel = Kernel::new(Arc::new(space));
        let cap = Capability::root();
        let req = Request::new(Verb::Source, iri("urn:kernel:actions")).with_arg(
            "types",
            ArgRef::Inline(b"https://schema.org/Person".to_vec()),
        );
        let rep = block_on(kernel.issue(req, &cap)).unwrap();
        let body = String::from_utf8(rep.bytes).unwrap();
        assert!(body.contains("urn:demo:greet"), "{body}");
        assert!(
            !body.contains("toUpper"),
            "untyped endpoint isn't an action: {body}"
        );
        assert_eq!(rep.repr_type.media_type, "text/plain");

        // Bare `urn:kernel:actions` is the whole (capability-scoped) action manifold —
        // every axis of the query is optional.
        let rep =
            block_on(kernel.issue(Request::new(Verb::Source, iri("urn:kernel:actions")), &cap))
                .unwrap();
        let body = String::from_utf8(rep.bytes).unwrap();
        assert!(body.contains("urn:demo:greet"), "{body}");
        assert!(
            body.contains("urn:test:to-upper"),
            "no type filter applied: {body}"
        );
    }

    #[test]
    fn the_action_manifold_is_capability_scoped() {
        // An endpoint whose Source and Sink are different actions under different
        // capabilities — the calendar shape. An attenuated agent's manifold simply
        // lacks the write action; root sees both. Selection uses the same `allows`
        // check enforcement runs, so the two can never drift.
        use crate::describe::ActionSpec;
        let cal = FnEndpoint::new("cal", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new("cal")
                .action(
                    ActionSpec::new(Verb::Source)
                        .requires("urn:cap:cal:read")
                        .output("text/turtle"),
                )
                .action(ActionSpec::new(Verb::Sink).requires("urn:cap:cal:write")),
        );
        let space = EndpointSpace::new().bind(Exact::new("urn:demo:cal"), cal);
        let kernel = Kernel::new(Arc::new(space));

        let turtle = |cap: &Capability| {
            let req = Request::new(Verb::Source, iri("urn:kernel:actions"))
                .with_arg("as", ArgRef::Inline(b"text/turtle".to_vec()));
            let rep = block_on(kernel.issue(req, cap)).unwrap();
            String::from_utf8(rep.bytes).unwrap()
        };

        let all = turtle(&Capability::root());
        assert!(all.contains("cal:action:source> a ik:ActionMatch"), "{all}");
        assert!(all.contains("cal:action:sink> a ik:ActionMatch"), "{all}");
        assert!(all.contains("ik:requires <urn:cap:cal:write>"), "{all}");

        // No kernel:inspect in the scope — an agent may always read its OWN manifold.
        let reader = Capability::scoped(["urn:cap:cal:read"]);
        let scoped = turtle(&reader);
        assert!(scoped.contains("cal:action:source>"), "{scoped}");
        assert!(
            !scoped.contains("action:sink"),
            "the write action must not be OFFERED to a read capability: {scoped}"
        );

        // A wildcard requires ("urn:cap:net:*") offers to any capability holding
        // SOME grant under the prefix — the parameterized-ACL annotation form.
        let net = FnEndpoint::new("net", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new("net")
                .verb(Verb::Source)
                .requires("urn:cap:net:*"),
        );
        let space2 = EndpointSpace::new().bind(Exact::new("urn:demo:net"), net);
        let kernel2 = Kernel::new(Arc::new(space2));
        let query = |cap: &Capability| {
            let req = Request::new(Verb::Source, iri("urn:kernel:actions"));
            let rep = block_on(kernel2.issue(req, cap)).unwrap();
            String::from_utf8(rep.bytes).unwrap()
        };
        let host_scoped = Capability::scoped(["urn:cap:net:api.example.com"]);
        assert!(query(&host_scoped).contains("urn:demo:net"));
        let no_net = Capability::scoped(["urn:cap:cal:read"]);
        assert!(!query(&no_net).contains("urn:demo:net"));

        // verb= and want= narrow the funnel deterministically.
        let req = Request::new(Verb::Source, iri("urn:kernel:actions"))
            .with_arg("verb", ArgRef::Inline(b"sink".to_vec()));
        let rep = block_on(kernel.issue(req, &Capability::root())).unwrap();
        let body = String::from_utf8(rep.bytes).unwrap();
        assert!(body.contains("urn:demo:cal"), "{body}");
        let req = Request::new(Verb::Source, iri("urn:kernel:actions"))
            .with_arg("want", ArgRef::Inline(b"text/html".to_vec()));
        let rep = block_on(kernel.issue(req, &Capability::root())).unwrap();
        assert!(rep.bytes.is_empty(), "nothing produces text/html");
    }

    #[test]
    fn describe_returns_the_typed_struct() {
        let greet = FnEndpoint::new("greet", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new("greet")
                .verb(Verb::Source)
                .input(crate::describe::ArgSpec::new("who").class("https://schema.org/Person")),
        );
        let space = EndpointSpace::new().bind(Exact::new("urn:demo:greet"), greet);
        let kernel = Kernel::new(Arc::new(space));

        let d = kernel.describe(&iri("urn:demo:greet")).expect("resolves");
        assert_eq!(d.id, "greet");
        assert_eq!(d.inputs[0].name, "who");
        assert_eq!(
            d.inputs[0].class.as_deref(),
            Some("https://schema.org/Person")
        );
        assert!(kernel.describe(&iri("urn:demo:absent")).is_none());
    }

    #[test]
    fn validate_reports_violations_as_shacl() {
        use crate::describe::ActionSpec;
        let cal = FnEndpoint::new("cal", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
        })
        .with_description(
            Description::new("cal").action(
                ActionSpec::new(Verb::Sink)
                    .requires("urn:cap:cal:write")
                    .input(
                        crate::describe::ArgSpec::new("start")
                            .class("http://www.w3.org/2001/XMLSchema#dateTime"),
                    )
                    .input(
                        crate::describe::ArgSpec::new("mode")
                            .one_of(["add", "move"])
                            .optional(),
                    ),
            ),
        );
        let space = EndpointSpace::new().bind(Exact::new("urn:demo:cal"), cal);
        let kernel = Kernel::new(Arc::new(space));
        let validate = |args: &str, cap: &Capability| {
            let req = Request::new(Verb::Source, iri("urn:kernel:validate"))
                .with_arg(
                    "action",
                    ArgRef::Inline(b"urn:ikigai:endpoint:cal:action:sink".to_vec()),
                )
                .with_arg("args", ArgRef::Inline(args.as_bytes().to_vec()));
            let rep = block_on(kernel.issue(req, cap)).unwrap();
            String::from_utf8(rep.bytes).unwrap()
        };

        // Clean: everything declared, capability satisfied.
        let ok = validate("start=2026-07-10T10:00:00&mode=add", &Capability::root());
        assert!(ok.contains("sh:conforms true"), "{ok}");

        // The violation matrix: missing required, bad enum value, implausible
        // datetime, unknown argument — each a sh:ValidationResult with a message,
        // and the input-node paths join back to the catalog contract.
        let bad = validate("mode=destroy&when=now", &Capability::root());
        assert!(bad.contains("sh:conforms false"), "{bad}");
        assert!(bad.contains("required input `start` is missing"), "{bad}");
        assert!(bad.contains("not an accepted value of `mode`"), "{bad}");
        assert!(bad.contains("unknown argument `when`"), "{bad}");
        assert!(
            bad.contains("sh:resultPath <urn:ikigai:endpoint:cal:action:sink:input:start>"),
            "explicit action inputs are action-scoped: {bad}"
        );
        let odd = validate("start=whenever", &Capability::root());
        assert!(odd.contains("does not look like a"), "{odd}");

        // Capability pre-flight: a read-only capability fails validation BEFORE
        // any invocation is attempted — the same allows check enforcement runs.
        let scoped = Capability::scoped(["urn:cap:cal:read"]);
        let denied = validate("start=2026-07-10T10:00:00", &scoped);
        assert!(denied.contains("sh:conforms false"), "{denied}");
        assert!(
            denied.contains("does not grant `urn:cap:cal:write`"),
            "{denied}"
        );
    }

    #[test]
    fn select_action_reasons_over_subclass_axioms() {
        // greet requires a schema:Person. With foaf:Person rdfs:subClassOf schema:Person
        // installed, an entity typed foaf:Person satisfies it — the RDFS inference bridge.
        const FOAF_PERSON: &str = "http://xmlns.com/foaf/0.1/Person";
        const SCHEMA_PERSON: &str = "https://schema.org/Person";
        // EndpointSpace isn't Clone, so build a fresh one per kernel.
        let space = || {
            let greet = FnEndpoint::new("greet", |_inv| {
                Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
            })
            .with_description(
                Description::new("greet")
                    .verb(Verb::Source)
                    .input(crate::describe::ArgSpec::new("who").class(SCHEMA_PERSON)),
            );
            Arc::new(EndpointSpace::new().bind(Exact::new("urn:demo:greet"), greet))
                as Arc<dyn Space>
        };

        // Without the axiom: foaf:Person matches nothing (exact-class only).
        assert!(
            Kernel::new(space())
                .select_action(&[FOAF_PERSON])
                .is_empty(),
            "no axioms ⇒ exact-class only"
        );

        // With the axiom: foaf:Person ⊑ schema:Person ⇒ greet is satisfiable.
        let reasoning = Kernel::new(space()).with_subclass_axioms([(FOAF_PERSON, SCHEMA_PERSON)]);
        assert!(
            reasoning
                .select_action(&[FOAF_PERSON])
                .iter()
                .any(|m| m.endpoint == "urn:demo:greet"),
            "foaf:Person should satisfy a schema:Person action via subClassOf"
        );
        // The direct type still matches too.
        assert!(reasoning
            .select_action(&[SCHEMA_PERSON])
            .iter()
            .any(|m| m.endpoint == "urn:demo:greet"));
    }

    #[test]
    fn subclass_closure_is_transitive_over_many_axioms() {
        // Multiple axioms incl. a chain foaf:Person ⊑ schema:Person ⊑ schema:Thing: an
        // action requiring schema:Thing is reachable from a foaf:Person entity two hops up.
        const FOAF_PERSON: &str = "http://xmlns.com/foaf/0.1/Person";
        const SCHEMA_PERSON: &str = "https://schema.org/Person";
        const SCHEMA_THING: &str = "https://schema.org/Thing";
        let space = || {
            let anything = FnEndpoint::new("anything", |_inv| {
                Ok(Representation::new(ReprType::new("text/plain"), Vec::new()))
            })
            .with_description(
                Description::new("anything")
                    .verb(Verb::Source)
                    .input(crate::describe::ArgSpec::new("it").class(SCHEMA_THING)),
            );
            Arc::new(EndpointSpace::new().bind(Exact::new("urn:demo:anything"), anything))
                as Arc<dyn Space>
        };
        let kernel = Kernel::new(space())
            .with_subclass_axioms([(FOAF_PERSON, SCHEMA_PERSON), (SCHEMA_PERSON, SCHEMA_THING)]);
        // Two hops: foaf:Person ⊑ schema:Person ⊑ schema:Thing.
        assert!(
            kernel
                .select_action(&[FOAF_PERSON])
                .iter()
                .any(|m| m.endpoint == "urn:demo:anything"),
            "transitive closure should reach schema:Thing from foaf:Person"
        );
    }

    #[test]
    fn actions_resource_describes_itself() {
        // `describe urn:kernel:actions` resolves to the selector's self-description (id
        // "kernel-actions") instead of erroring unresolved — so it's introspectable and the
        // engine can route `types=` (it only names declared inputs).
        let kernel = meta_kernel();
        let turtle =
            String::from_utf8(meta_as(&kernel, "urn:kernel:actions", "text/turtle").bytes).unwrap();
        assert!(turtle.contains("kernel-actions"), "described: {turtle}");
        // A type the renderer can't emit directly falls back to canonical Turtle — the async
        // transrept-to-other-types route isn't available on the sync intrinsic path.
        let fallback =
            String::from_utf8(meta_as(&kernel, "urn:kernel:actions", "application/json").bytes)
                .unwrap();
        assert!(fallback.contains("kernel-actions"), "fell back: {fallback}");
    }

    /// Issue `urn:kernel:actions` under `capability` and return the `urn:kernel:*`
    /// lines of the plain manifold face — the caller's kernel-scoped tool list.
    fn kernel_manifold(kernel: &Kernel, capability: &Capability) -> Vec<String> {
        let request = Request::new(Verb::Source, iri("urn:kernel:actions"));
        let rep = block_on(kernel.issue(request, capability)).unwrap();
        String::from_utf8(rep.bytes)
            .unwrap()
            .lines()
            .filter(|l| l.starts_with(KERNEL_NS))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_manifold_offers_exactly_the_kernel_ops_a_capability_admits() {
        // ★ THE PROPERTY, and the one a future refactor breaks silently: an agent's
        // tool list IS the manifold under its capability, so the kernel's own
        // operations must appear there — and appear *filtered*. `list` mentioning
        // the kernel is the easy half; this is the half with teeth.
        let kernel = meta_kernel();
        const INSPECT_ONLY: [&str; 6] = [
            "urn:kernel:aliases",
            "urn:kernel:cache",
            "urn:kernel:catalog",
            "urn:kernel:constraint",
            "urn:kernel:scheduler",
            "urn:kernel:threads",
        ];
        // Ungated by design: "what can I do?" must be answerable by any capability
        // about itself, and the pre-flight that checks a proposal is not itself
        // authority (see the `actions` arm).
        const UNGATED: [&str; 2] = ["urn:kernel:actions", "urn:kernel:validate"];

        let root = kernel_manifold(&kernel, &Capability::root());
        for op in INSPECT_ONLY.iter().chain(UNGATED.iter()) {
            assert!(root.contains(&op.to_string()), "root manifold lacks {op}");
        }
        assert!(
            root.contains(&"urn:kernel:cut".to_string()),
            "root manifold lacks urn:kernel:cut"
        );

        let inspect = kernel_manifold(&kernel, &Capability::scoped(["urn:cap:kernel:inspect"]));
        for op in INSPECT_ONLY.iter().chain(UNGATED.iter()) {
            assert!(
                inspect.contains(&op.to_string()),
                "inspect manifold lacks {op}"
            );
        }
        assert!(
            !inspect.contains(&"urn:kernel:cut".to_string()),
            "inspect authority must not offer thread-cutting: {inspect:?}"
        );

        let cut = kernel_manifold(&kernel, &Capability::scoped(["urn:cap:kernel:cut"]));
        assert!(cut.contains(&"urn:kernel:cut".to_string()), "{cut:?}");
        for op in INSPECT_ONLY {
            assert!(
                !cut.contains(&op.to_string()),
                "cut authority must not offer {op}: {cut:?}"
            );
        }

        // No kernel authority at all: only the two self-limiting resources.
        let none = kernel_manifold(&kernel, &Capability::scoped(Vec::<String>::new()));
        assert_eq!(none, UNGATED.map(str::to_string).to_vec(), "{none:?}");
    }

    #[test]
    fn kernel_ops_declare_exactly_what_they_enforce() {
        // Declared = enforced, in both directions, over every operation and verb.
        // The pre-gate in `issue_kernel` makes declared ⊆ enforced structurally; what
        // no type can catch is the converse — an arm demanding a scope its
        // description never mentions, which makes the manifold over-offer and an
        // agent's tool call fail at invoke after selection said it would not.
        let kernel = meta_kernel();
        for op in crate::kernel_ops::OPS {
            let description = crate::kernel_ops::description(op).expect("listed op");
            for spec in description.action_specs() {
                let target = format!("{KERNEL_NS}{op}");
                let fire = |scopes: Vec<String>| {
                    let request = Request::new(spec.verb, iri(&target));
                    block_on(kernel.issue(request, &Capability::scoped(scopes)))
                };
                // Withholding any ONE declared scope must deny.
                for withheld in &spec.requires {
                    let held: Vec<String> = spec
                        .requires
                        .iter()
                        .filter(|s| *s != withheld)
                        .cloned()
                        .collect();
                    let error = fire(held).expect_err("must deny without the declared scope");
                    assert!(
                        matches!(&error, Error::Denied(m) if m.contains(withheld)),
                        "{target} ({:?}) declares `{withheld}` but did not refuse for it: {error:?}",
                        spec.verb
                    );
                }
                // Holding EXACTLY the declared scopes must not deny. A missing
                // argument or an absent renderer is fine — a denial is not.
                let outcome = fire(spec.requires.clone());
                if let Err(Error::Denied(message)) = &outcome {
                    panic!(
                        "{target} ({:?}) enforces authority beyond its declaration \
                         (declared {:?}): {message}",
                        spec.verb, spec.requires
                    );
                }
            }
        }
    }

    #[test]
    fn every_kernel_operation_describes_itself() {
        // The generic Meta arm: `describe urn:kernel:<op>` renders that operation's own
        // contract, for every operation — not just the two that used to have one. And
        // it stays ungated: `Meta` is universal, so no operation can declare a scope
        // for it, and reading a contract discloses nothing the crate docs do not.
        let kernel = meta_kernel();
        for op in crate::kernel_ops::OPS {
            let id = crate::kernel_ops::description(op).expect("listed op").id;
            let target = format!("{KERNEL_NS}{op}");
            let request = Request::new(Verb::Meta, iri(&target));
            let rep = block_on(kernel.issue(request, &Capability::scoped(Vec::<String>::new())))
                .unwrap_or_else(|e| panic!("describe {target}: {e:?}"));
            let turtle = String::from_utf8(rep.bytes).unwrap();
            assert!(turtle.contains(&id), "describe {target} rendered: {turtle}");
        }
        // A name inside the namespace that names no operation is still unresolved —
        // the generic arm describes what exists, it does not invent.
        let request = Request::new(Verb::Meta, iri("urn:kernel:nonesuch"));
        assert!(matches!(
            block_on(kernel.issue(request, &Capability::root())),
            Err(Error::Unresolved(_))
        ));
    }

    #[test]
    fn the_catalog_and_the_listing_both_carry_the_kernel() {
        // The catalog says what EXISTS — every kernel operation, for a caller who has
        // already proven inspect authority (capability filtering belongs in the
        // manifold, not here).
        let kernel = meta_kernel();
        let rep = block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:kernel:catalog")),
            &Capability::root(),
        ))
        .unwrap();
        let turtle = String::from_utf8(rep.bytes).unwrap();
        for op in crate::kernel_ops::OPS {
            let id = crate::kernel_ops::description(op).expect("listed op").id;
            assert!(
                turtle.contains(&format!("\"{id}\"")),
                "catalog omits `{id}`: {turtle}"
            );
        }
        assert!(turtle.contains("\"toUpper\""), "catalog lost the space");

        // …and so does enumeration, which is what `list` reads.
        let entries = kernel.entries().expect("enumerable root");
        for op in crate::kernel_ops::OPS {
            let pattern = format!("{KERNEL_NS}{op}");
            assert!(
                entries.iter().any(|e| e.pattern == pattern),
                "entries omit `{pattern}`"
            );
        }
        assert!(entries.iter().any(|e| e.pattern == "urn:test:to-upper"));
    }

    #[test]
    fn a_non_enumerable_root_still_reports_that_it_cannot_say() {
        // The kernel operations alone are not an answer to "what is bound here?".
        // Reporting `Some` over a root that cannot enumerate would turn "cannot say"
        // into "nothing else is bound", which is a different and false claim.
        struct Opaque;
        impl Space for Opaque {
            fn resolve(&self, _request: &Request, _scope: &Scope) -> Resolution {
                Resolution::Miss
            }
        }
        assert!(Kernel::new(Arc::new(Opaque)).entries().is_none());
    }

    #[test]
    fn validate_can_preflight_a_kernel_action() {
        // `description_for_id` reaches the kernel now, so stage four of the selection
        // funnel covers the kernel's own actions: an agent that found `urn:kernel:cut`
        // in its manifold can pre-flight the call it is about to make.
        let kernel = meta_kernel();
        let report = |args: &str, capability: &Capability| {
            let request = Request::new(Verb::Source, iri("urn:kernel:validate"))
                .with_arg(
                    "action",
                    ArgRef::Inline(b"urn:ikigai:endpoint:kernel-cut:action:sink".to_vec()),
                )
                .with_arg("args", ArgRef::Inline(args.as_bytes().to_vec()));
            String::from_utf8(block_on(kernel.issue(request, capability)).unwrap().bytes).unwrap()
        };
        let cutter = Capability::scoped(["urn:cap:kernel:cut"]);
        assert!(
            report("thread=urn:x", &cutter).contains("sh:conforms true"),
            "a well-formed cut under cut authority should conform"
        );
        // The contract is real: an argument outside it is a violation…
        assert!(report("thred=urn:x", &cutter).contains("unknown argument"));
        // …and so is proposing it without the authority the description declares.
        let report = report(
            "thread=urn:x",
            &Capability::scoped(["urn:cap:kernel:inspect"]),
        );
        assert!(
            report.contains("urn:cap:kernel:cut"),
            "pre-flight must report the missing capability: {report}"
        );
    }

    #[test]
    fn incoming_provenance_flows_cacheability_down_the_pipe() {
        // A cacheable endpoint (toUpper opts into caching via builtins) issued with
        // an *uncacheable* upstream provenance must itself become uncacheable —
        // cacheability is no greater than the piped input's.
        let space =
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper());
        let kernel = Kernel::new(Arc::new(space));
        let cap = Capability::root();
        let req = || {
            Request::new(Verb::Source, iri("urn:test:to-upper"))
                .with_arg("in", ArgRef::Inline(b"hi".to_vec()))
        };

        // Cacheable upstream (Never) → result stays cacheable → it caches.
        let cacheable_up = Provenance::new(Expiry::Never, BTreeSet::new());
        block_on(kernel.issue_with_incoming(req(), &cap, cacheable_up)).unwrap();
        assert_eq!(
            kernel.cache_len(),
            1,
            "cacheable upstream keeps the result cacheable"
        );

        // Uncacheable upstream (Always) → result becomes uncacheable → not cached.
        let other = Request::new(Verb::Source, iri("urn:test:to-upper"))
            .with_arg("in", ArgRef::Inline(b"yo".to_vec()));
        let volatile_up = Provenance::new(Expiry::Always, BTreeSet::new());
        let rep = block_on(kernel.issue_with_incoming(other, &cap, volatile_up)).unwrap();
        assert_eq!(
            rep.expiry,
            Expiry::Always,
            "volatile upstream makes the result volatile"
        );
        assert_eq!(
            kernel.cache_len(),
            1,
            "the volatile-upstream result was not cached"
        );
    }

    #[test]
    fn incoming_threads_are_inherited_so_cutting_the_source_invalidates() {
        // The upstream's golden threads propagate, so cutting one invalidates the
        // transformed result — the pipe is a dependency edge.
        let space =
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper());
        let kernel = Kernel::new(Arc::new(space));
        let cap = Capability::root();
        let req = || {
            Request::new(Verb::Source, iri("urn:test:to-upper"))
                .with_arg("in", ArgRef::Inline(b"hi".to_vec()))
        };
        let mut threads = BTreeSet::new();
        threads.insert(Thread::new("urn:data:source"));
        let up = Provenance::new(Expiry::Never, threads);
        block_on(kernel.issue_with_incoming(req(), &cap, up)).unwrap();
        assert!(kernel.is_cached(&req(), &cap), "cached after first issue");
        kernel.cut("urn:data:source");
        assert!(
            !kernel.is_cached(&req(), &cap),
            "cutting the inherited thread invalidates it"
        );
    }

    /// ★ THE LOST-CUT RACE. A cut that lands *while an invocation is in flight* must
    /// not be consumed by the very entry it should have invalidated. The endpoint
    /// observed state before the cut, so what it returns is already stale; pinning
    /// the entry's edges to the generation read AFTER the invocation returns files
    /// that stale representation as fresh, and it stays fresh until some later cut.
    ///
    /// This has to be a race, not a sequence — a cut between two synchronous calls
    /// reproduces nothing. The endpoint blocks on a channel the test controls, the
    /// cut lands inside that window, and only then is the invocation released.
    #[test]
    fn a_cut_during_an_in_flight_invocation_is_not_consumed_by_the_entry_it_invalidates() {
        use std::sync::mpsc;

        struct Blocking {
            entered: mpsc::SyncSender<()>,
            release: Mutex<mpsc::Receiver<()>>,
            calls: Arc<AtomicU32>,
        }

        #[async_trait]
        impl Endpoint for Blocking {
            async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                // Announce that the endpoint is inside the invocation, then hold it
                // open. Everything this result is built from is state as of THIS
                // moment — before the cut the test is about to make.
                self.entered.send(()).expect("test receiver alive");
                self.release
                    .lock()
                    .expect("release lock")
                    .recv()
                    .expect("released");
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    b"observed-before-the-cut".to_vec(),
                )
                .cacheable()
                .depends_on("urn:data:state"))
            }
            fn name(&self) -> &str {
                "blocking"
            }
        }

        let (entered_tx, entered_rx) = mpsc::sync_channel(4);
        let (release_tx, release_rx) = mpsc::channel();
        let calls = Arc::new(AtomicU32::new(0));
        let space = EndpointSpace::new().bind(
            Exact::new("urn:test:slow"),
            Blocking {
                entered: entered_tx,
                release: Mutex::new(release_rx),
                calls: Arc::clone(&calls),
            },
        );
        let kernel = Kernel::new(Arc::new(space));
        let req = || Request::new(Verb::Source, iri("urn:test:slow"));

        std::thread::scope(|scope| {
            let kernel = &kernel;
            let reader = scope.spawn(move || {
                block_on(kernel.issue(req(), &Capability::root())).expect("read succeeds")
            });
            // The endpoint is demonstrably inside the invocation…
            entered_rx.recv().expect("endpoint entered");
            // …the thread its result will depend on is cut, mid-flight…
            kernel.cut("urn:data:state");
            // …and only now does it return the representation built from pre-cut state.
            release_tx.send(()).expect("release the invocation");
            reader.join().expect("invocation completed");
        });

        assert_eq!(calls.load(Ordering::SeqCst), 1, "one invocation so far");
        assert_eq!(
            kernel.cache_len(),
            0,
            "a representation built before a cut must not be stored as valid after it"
        );
        assert!(
            !kernel.is_cached(&req(), &Capability::root()),
            "the stale representation must not be served back"
        );

        // …and the next read recomputes rather than serving the stale answer.
        release_tx.send(()).expect("release the second invocation");
        block_on(kernel.issue(req(), &Capability::root())).expect("read succeeds");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "the next read recomputes");
    }

    /// The policy is the host's to choose, at execution time — the builder sits
    /// beside `with_clock` and `with_meta_renderer` because it is the same kind of
    /// decision: one the kernel cannot make for every deployment.
    #[test]
    fn an_installed_cache_policy_bounds_what_the_kernel_keeps() {
        use crate::cache::{CacheBound, Fifo};

        let space =
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper());
        let kernel = Kernel::new(Arc::new(space))
            .with_cache_policy(Arc::new(Fifo::with_bound(CacheBound::new(2, 1 << 20))));
        let cap = Capability::root();
        let read = |word: &str| {
            Request::new(Verb::Source, iri("urn:test:to-upper"))
                .with_arg("in", ArgRef::Inline(word.as_bytes().to_vec()))
        };
        for word in ["a", "b", "c", "d"] {
            block_on(kernel.issue(read(word), &cap)).unwrap();
        }
        assert_eq!(kernel.cache_len(), 2, "the installed bound is enforced");
        assert!(
            !kernel.is_cached(&read("a"), &cap),
            "FIFO evicted the oldest"
        );
        assert!(kernel.is_cached(&read("d"), &cap), "the newest survives");
    }

    /// A policy decides admission and eviction, never validity: an entry whose
    /// golden thread has been cut is not served, whatever a policy would like.
    #[test]
    fn no_policy_can_make_a_cut_entry_serve() {
        use crate::cache::{CacheBound, CachePolicy, EntryFacts};

        struct KeepEverythingForever;
        impl CachePolicy for KeepEverythingForever {
            fn capacity(&self) -> CacheBound {
                CacheBound::new(usize::MAX, usize::MAX)
            }
            fn admit(&self, _candidate: &EntryFacts<'_>) -> bool {
                true
            }
            fn victim(&self, _resident: &[EntryFacts<'_>]) -> Option<usize> {
                None
            }
        }

        let space =
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper());
        let kernel =
            Kernel::new(Arc::new(space)).with_cache_policy(Arc::new(KeepEverythingForever));
        let cap = Capability::root();
        let req = || {
            Request::new(Verb::Source, iri("urn:test:to-upper"))
                .with_arg("in", ArgRef::Inline(b"hi".to_vec()))
        };
        let mut threads = BTreeSet::new();
        threads.insert(Thread::new("urn:data:source"));
        block_on(kernel.issue_with_incoming(req(), &cap, Provenance::new(Expiry::Never, threads)))
            .unwrap();
        assert!(kernel.is_cached(&req(), &cap));
        kernel.cut("urn:data:source");
        assert!(
            !kernel.is_cached(&req(), &cap),
            "validity is not the policy's to decide"
        );
    }

    /// The bound is part of the operator readout — the default is a number someone
    /// can see and argue with, not an implicit "unbounded".
    #[test]
    fn the_cache_readout_names_the_bound_in_force() {
        let space =
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper());
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(EchoIdRenderer));
        let request = Request::new(Verb::Source, iri("urn:kernel:cache"));
        let body = block_on(kernel.issue(request, &Capability::root())).unwrap();
        let body = String::from_utf8(body.bytes).unwrap();
        assert!(
            body.contains("entries  0 / 4096"),
            "the default entry bound is reported: {body}"
        );
        assert!(
            body.contains("64.0 MB"),
            "the default byte budget is reported: {body}"
        );
    }

    #[test]
    fn live_kernel_introspection_is_never_cached() {
        // The introspection builtins return `Always` (live) — they must not be
        // cached, even though the catalog (also `urn:kernel:*`) is.
        let space =
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper());
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(EchoIdRenderer));
        let cap = Capability::root();
        for op in [
            "urn:kernel:cache",
            "urn:kernel:threads",
            "urn:kernel:constraint",
            "urn:kernel:aliases",
        ] {
            block_on(kernel.issue(Request::new(Verb::Source, iri(op)), &cap)).unwrap();
        }
        assert_eq!(
            kernel.cache_len(),
            0,
            "live introspection must never be cached"
        );
    }

    #[test]
    fn catalog_requires_the_inspect_capability() {
        let space =
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper());
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(EchoIdRenderer));
        let unprivileged = Capability::scoped(["urn:cap:nothing".to_string()]);
        assert!(block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:kernel:catalog")),
            &unprivileged
        ))
        .is_err());
    }

    #[test]
    fn resolves_invokes_and_caches() {
        let space =
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper());
        let kernel = Kernel::new(Arc::new(space));
        let cap = Capability::root();
        let req = || {
            Request::new(Verb::Source, iri("urn:test:to-upper"))
                .with_arg("in", ArgRef::Inline(b"hi".to_vec()))
        };
        let a = block_on(kernel.issue(req(), &cap)).unwrap();
        let b = block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(a.bytes, b"HI");
        assert_eq!(a.bytes, b.bytes);
        assert_eq!(kernel.cache_len(), 1);
    }

    #[test]
    fn cacheable_endpoint_runs_only_once() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let counter = FnEndpoint::new("count", |_inv: &Invocation<'_>| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(Representation::new(ReprType::new("text/plain"), b"x".to_vec()).cacheable())
        });
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:test:count"), counter),
        ));
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:test:count"));
        block_on(kernel.issue(req(), &cap)).unwrap();
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            1,
            "second issue should be a cache hit"
        );
    }

    #[test]
    fn is_cached_probes_without_resolving() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let counter = FnEndpoint::new("count", |_inv: &Invocation<'_>| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(Representation::new(ReprType::new("text/plain"), b"x".to_vec()).cacheable())
        });
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:test:count"), counter),
        ));
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:test:count"));

        // Not cached before the first issue; probing does not resolve it.
        assert!(!kernel.is_cached(&req(), &Capability::root()));
        assert!(!kernel.is_cached(&req(), &Capability::root()));
        assert_eq!(CALLS.load(Ordering::SeqCst), 0, "is_cached must not invoke");
        assert_eq!(kernel.cache_len(), 0, "is_cached must not cache");

        // Cached after issuing once.
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert!(kernel.is_cached(&req(), &Capability::root()));

        // A different request (different argument identity) is still a miss.
        let other = Request::new(Verb::Source, iri("urn:test:count"))
            .with_arg("in", ArgRef::Inline(b"z".to_vec()));
        assert!(!kernel.is_cached(&other, &Capability::root()));
    }

    #[test]
    fn uncacheable_endpoint_runs_every_time() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        // No `.cacheable()` -> default Expiry::Always -> never cached.
        let volatile = FnEndpoint::new("vol", |_inv: &Invocation<'_>| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(Representation::new(
                ReprType::new("text/plain"),
                b"x".to_vec(),
            ))
        });
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:test:vol"), volatile),
        ));
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:test:vol"));
        block_on(kernel.issue(req(), &cap)).unwrap();
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            2,
            "uncacheable result recomputes"
        );
    }

    #[test]
    fn unresolved_target_errors() {
        let kernel = Kernel::new(Arc::new(EndpointSpace::new()));
        let cap = Capability::root();
        let err = block_on(kernel.issue(Request::new(Verb::Source, iri("urn:test:nope")), &cap))
            .unwrap_err();
        assert!(matches!(err, Error::Unresolved(_)));
    }

    /// A composing endpoint: dereference the `src` by-reference argument and
    /// upper-case its content.
    struct UpcaseOf;

    #[async_trait::async_trait]
    impl Endpoint for UpcaseOf {
        async fn invoke(&self, cx: &Invocation<'_>) -> Result<Representation> {
            let src = match cx.request.args.get("src") {
                Some(ArgRef::Reference(iri)) => iri.clone(),
                _ => return Err(Error::MissingArgument("src".to_string())),
            };
            let upstream = cx.source(&src).await?;
            let upper = String::from_utf8_lossy(&upstream.bytes).to_uppercase();
            Ok(Representation::new(ReprType::new("text/plain"), upper.into_bytes()).cacheable())
        }
    }

    #[test]
    fn composes_over_a_referenced_resource_and_caches() {
        static GREETING_CALLS: AtomicU32 = AtomicU32::new(0);
        let greeting = FnEndpoint::new("greeting", |_cx: &Invocation<'_>| {
            GREETING_CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(Representation::new(ReprType::new("text/plain"), b"hello".to_vec()).cacheable())
        });
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:data:greeting"), greeting)
            .bind(Exact::new("urn:test:upcase-of"), UpcaseOf);
        let kernel = Kernel::new(Arc::new(space));
        let cap = Capability::root();
        let req = || {
            Request::new(Verb::Source, iri("urn:test:upcase-of"))
                .with_arg("src", ArgRef::Reference(iri("urn:data:greeting")))
        };
        let a = block_on(kernel.issue(req(), &cap)).unwrap();
        let b = block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(a.bytes, b"HELLO");
        assert_eq!(a.bytes, b.bytes);
        // Both deps cacheable -> composed result cached -> greeting sourced once.
        assert_eq!(GREETING_CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn volatile_dependency_forbids_caching_the_composer() {
        static CLOCK_CALLS: AtomicU32 = AtomicU32::new(0);
        // No `.cacheable()` -> volatile dependency.
        let clock = FnEndpoint::new("clock", |_cx: &Invocation<'_>| {
            CLOCK_CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(Representation::new(
                ReprType::new("text/plain"),
                b"tick".to_vec(),
            ))
        });
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:data:clock"), clock)
            .bind(Exact::new("urn:test:upcase-of"), UpcaseOf);
        let kernel = Kernel::new(Arc::new(space));
        let cap = Capability::root();
        let req = || {
            Request::new(Verb::Source, iri("urn:test:upcase-of"))
                .with_arg("src", ArgRef::Reference(iri("urn:data:clock")))
        };
        block_on(kernel.issue(req(), &cap)).unwrap();
        block_on(kernel.issue(req(), &cap)).unwrap();
        // Expiry propagates: volatile dep -> composer not cached -> clock sourced twice.
        assert_eq!(CLOCK_CALLS.load(Ordering::SeqCst), 2);
    }

    // --- golden threads --------------------------------------------------------

    #[test]
    fn cutting_a_thread_invalidates_the_entry_that_declared_it() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        // A cacheable read of mutable state: names the thread for that state.
        let file = FnEndpoint::new("file", |_cx: &Invocation<'_>| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(
                Representation::new(ReprType::new("text/plain"), b"v".to_vec())
                    .cacheable()
                    .depends_on("urn:file:notes.txt"),
            )
        });
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:file:notes.txt"), file),
        ));
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:file:notes.txt"));

        block_on(kernel.issue(req(), &cap)).unwrap();
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            1,
            "cached on the second issue"
        );
        assert!(kernel.is_cached(&req(), &Capability::root()));

        // An external change (or a Sink) cuts the thread.
        kernel.cut("urn:file:notes.txt");
        assert!(
            !kernel.is_cached(&req(), &Capability::root()),
            "cut entry is no longer cached"
        );
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst), 2, "recomputed after the cut");
        // Re-cached at the new generation.
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            2,
            "cached again after recompute"
        );

        // Cutting an unrelated thread leaves it cached.
        kernel.cut("urn:file:other.txt");
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            2,
            "unrelated cut does not invalidate"
        );
    }

    #[test]
    fn a_thread_propagates_up_through_composition() {
        static LEAF: AtomicU32 = AtomicU32::new(0);
        let leaf = FnEndpoint::new("leaf", |_cx: &Invocation<'_>| {
            LEAF.fetch_add(1, Ordering::SeqCst);
            Ok(
                Representation::new(ReprType::new("text/plain"), b"hi".to_vec())
                    .cacheable()
                    .depends_on("urn:leaf"),
            )
        });
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:data:leaf"), leaf)
            .bind(Exact::new("urn:test:upcase-of"), UpcaseOf);
        let kernel = Kernel::new(Arc::new(space));
        let cap = Capability::root();
        let req = || {
            Request::new(Verb::Source, iri("urn:test:upcase-of"))
                .with_arg("src", ArgRef::Reference(iri("urn:data:leaf")))
        };
        block_on(kernel.issue(req(), &cap)).unwrap();
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(LEAF.load(Ordering::SeqCst), 1, "composite + leaf cached");

        // The composite never declared `urn:leaf`; it inherited it by resolving the
        // leaf. Cutting it invalidates the composite anyway.
        kernel.cut("urn:leaf");
        let out = block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(out.bytes, b"HI");
        assert_eq!(
            LEAF.load(Ordering::SeqCst),
            2,
            "composite recomputed and re-sourced the leaf"
        );
    }

    /// A team resource: concatenates three member resources it resolves, and
    /// declares its own team-level thread.
    struct Team;

    #[async_trait::async_trait]
    impl Endpoint for Team {
        async fn invoke(&self, cx: &Invocation<'_>) -> Result<Representation> {
            let mut body = Vec::new();
            for member in ["urn:person:alice", "urn:person:bob", "urn:person:carol"] {
                body.extend_from_slice(&cx.source(&iri(member)).await?.bytes);
            }
            Ok(Representation::new(ReprType::new("text/plain"), body)
                .cacheable()
                .depends_on("urn:team:engineering"))
        }
    }

    #[test]
    fn cutting_one_member_invalidates_the_team_but_not_its_siblings() {
        static ALICE: AtomicU32 = AtomicU32::new(0);
        static BOB: AtomicU32 = AtomicU32::new(0);
        static CAROL: AtomicU32 = AtomicU32::new(0);
        fn person(thread: &'static str, body: &'static str, n: &'static AtomicU32) -> FnEndpoint {
            FnEndpoint::new("person", move |_cx: &Invocation<'_>| {
                n.fetch_add(1, Ordering::SeqCst);
                Ok(
                    Representation::new(ReprType::new("text/plain"), body.as_bytes().to_vec())
                        .cacheable()
                        .depends_on(thread),
                )
            })
        }
        let space = EndpointSpace::new()
            .bind(
                Exact::new("urn:person:alice"),
                person("urn:person:alice", "A", &ALICE),
            )
            .bind(
                Exact::new("urn:person:bob"),
                person("urn:person:bob", "B", &BOB),
            )
            .bind(
                Exact::new("urn:person:carol"),
                person("urn:person:carol", "C", &CAROL),
            )
            .bind(Exact::new("urn:team:engineering"), Team);
        let kernel = Kernel::new(Arc::new(space));
        let cap = Capability::root();
        let team = || Request::new(Verb::Source, iri("urn:team:engineering"));

        let first = block_on(kernel.issue(team(), &cap)).unwrap();
        block_on(kernel.issue(team(), &cap)).unwrap();
        assert_eq!(first.bytes, b"ABC");
        let counts = || {
            (
                ALICE.load(Ordering::SeqCst),
                BOB.load(Ordering::SeqCst),
                CAROL.load(Ordering::SeqCst),
            )
        };
        assert_eq!(
            counts(),
            (1, 1, 1),
            "members resolved once; team then cached"
        );

        // Bob changes upstream. The team depends on every member individually.
        kernel.cut("urn:person:bob");
        let again = block_on(kernel.issue(team(), &cap)).unwrap();
        assert_eq!(again.bytes, b"ABC");
        // Team recomputes (it carried bob's thread) and re-resolves bob; alice and
        // carol are still valid in cache, so they are NOT recomputed.
        assert_eq!(counts(), (1, 2, 1), "only bob and the team recompute");

        // And a team-level cut hits only the team, not the members.
        kernel.cut("urn:team:engineering");
        block_on(kernel.issue(team(), &cap)).unwrap();
        assert_eq!(
            counts(),
            (1, 2, 1),
            "roster change re-runs the team, reuses members"
        );
    }

    /// A stateful resource: `Source` reads the current value (cacheable, declaring
    /// the thread named after itself); `Sink` replaces it.
    struct Cell {
        value: Mutex<Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl Endpoint for Cell {
        async fn invoke(&self, cx: &Invocation<'_>) -> Result<Representation> {
            match cx.request.verb {
                Verb::Source => {
                    let value = self.value.lock().expect("cell").clone();
                    Ok(Representation::new(ReprType::new("text/plain"), value)
                        .cacheable()
                        .depends_on(cx.request.target.as_str()))
                }
                Verb::Sink => {
                    *self.value.lock().expect("cell") = cx.inline_arg("in")?.to_vec();
                    Ok(Representation::new(
                        ReprType::new("text/plain"),
                        b"ok".to_vec(),
                    ))
                }
                other => Err(Error::Endpoint(format!("cell: unsupported {other:?}"))),
            }
        }
    }

    #[test]
    fn a_sink_invalidates_the_cached_source_of_its_target() {
        let cell = Cell {
            value: Mutex::new(b"v1".to_vec()),
        };
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:data:cell"), cell),
        ));
        let cap = Capability::root();
        let source = || Request::new(Verb::Source, iri("urn:data:cell"));

        // Read v1; it caches, declaring the `urn:data:cell` thread.
        assert_eq!(block_on(kernel.issue(source(), &cap)).unwrap().bytes, b"v1");
        assert!(
            kernel.is_cached(&source(), &Capability::root()),
            "source is cached"
        );

        // Write v2 through the kernel: the mutating verb auto-cuts `urn:data:cell`.
        let sink = Request::new(Verb::Sink, iri("urn:data:cell"))
            .with_arg("in", ArgRef::Inline(b"v2".to_vec()));
        block_on(kernel.issue(sink, &cap)).unwrap();
        assert!(
            !kernel.is_cached(&source(), &Capability::root()),
            "the write invalidated the cached read"
        );

        // Read again: the cache recomputes and sees v2.
        assert_eq!(block_on(kernel.issue(source(), &cap)).unwrap().bytes, b"v2");
    }

    #[test]
    fn cache_is_keyed_by_capability() {
        // A cacheable endpoint whose output depends on the capability. If the cache
        // ignored the capability, the first (privileged) read would be served back to a
        // restricted capability — a capability bypass, since the hit skips the endpoint's
        // own check. The cache must instead be namespaced by authority.
        let who = FnEndpoint::new("who", |inv: &Invocation<'_>| {
            let body = if inv.capability.allows("urn:cap:secret") {
                "SECRET"
            } else {
                "public"
            };
            Ok(
                Representation::new(ReprType::new("text/plain"), body.as_bytes().to_vec())
                    .cacheable(),
            )
        });
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:demo:who"), who),
        ));
        let req = || Request::new(Verb::Source, iri("urn:demo:who"));
        let privileged = Capability::root(); // allows everything, including urn:cap:secret
        let restricted = Capability::root().attenuate(["urn:cap:other".to_string()]); // no secret

        // The privileged read caches SECRET under the root namespace.
        assert_eq!(
            block_on(kernel.issue(req(), &privileged)).unwrap().bytes,
            b"SECRET"
        );
        assert!(kernel.is_cached(&req(), &privileged));
        // The restricted capability is a different namespace — it must NOT probe as
        // cached, and issuing under it recomputes rather than serving the SECRET entry.
        assert!(
            !kernel.is_cached(&req(), &restricted),
            "the cache is namespaced by capability"
        );
        assert_eq!(
            block_on(kernel.issue(req(), &restricted)).unwrap().bytes,
            b"public",
            "the restricted capability recomputes, not served the cached SECRET"
        );
    }

    // --- the kernel-behavior namespace (urn:kernel:*) --------------------------

    fn cut_request(thread: &str) -> Request {
        Request::new(Verb::Sink, iri("urn:kernel:cut"))
            .with_arg("content", ArgRef::Inline(thread.as_bytes().to_vec()))
    }

    #[test]
    fn cut_as_a_resource_invalidates_a_cached_read() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let ep = FnEndpoint::new("x", |_cx: &Invocation<'_>| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(
                Representation::new(ReprType::new("text/plain"), b"v".to_vec())
                    .cacheable()
                    .depends_on("urn:data:x"),
            )
        });
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:data:x"), ep),
        ));
        let cap = Capability::root();
        let read = || Request::new(Verb::Source, iri("urn:data:x"));

        block_on(kernel.issue(read(), &cap)).unwrap();
        block_on(kernel.issue(read(), &cap)).unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst), 1, "cached");

        // Cut the thread by RESOLVING urn:kernel:cut — not calling Kernel::cut.
        let ack = block_on(kernel.issue(cut_request("urn:data:x"), &cap)).unwrap();
        assert!(String::from_utf8_lossy(&ack.bytes).contains("cut urn:data:x"));
        assert!(
            !kernel.is_cached(&read(), &Capability::root()),
            "the resource cut invalidated it"
        );

        block_on(kernel.issue(read(), &cap)).unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst), 2, "recomputed after the cut");
    }

    #[test]
    fn cache_readout_lists_each_entry_with_iri_type_and_threads() {
        let ep = FnEndpoint::new("x", |_cx: &Invocation<'_>| {
            Ok(
                Representation::new(ReprType::new("text/plain"), b"hello".to_vec())
                    .cacheable()
                    .depends_on("urn:data:x"),
            )
        });
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:data:x"), ep),
        ));
        let cap = Capability::root();
        block_on(kernel.issue(Request::new(Verb::Source, iri("urn:data:x")), &cap)).unwrap();
        let out = block_on(kernel.issue(Request::new(Verb::Source, iri("urn:kernel:cache")), &cap))
            .unwrap();
        let text = String::from_utf8_lossy(&out.bytes);
        assert!(text.contains("entries  1"), "count: {text}");
        assert!(text.contains("urn:data:x"), "names the IRI: {text}");
        assert!(text.contains("text/plain"), "names the type: {text}");
        assert!(text.contains("5 B"), "names the size: {text}");
        assert!(
            text.contains("1 thread"),
            "names the dependency count: {text}"
        );
    }

    #[test]
    fn the_kernel_namespace_is_capability_gated() {
        let kernel = Kernel::new(Arc::new(EndpointSpace::new()));
        // A scoped capability lacking `urn:cap:kernel:cut` is refused.
        let scoped = Capability::scoped(["urn:cap:something:else"]);
        assert!(block_on(kernel.issue(cut_request("urn:data:x"), &scoped)).is_err());
        // Root holds it.
        assert!(block_on(kernel.issue(cut_request("urn:data:x"), &Capability::root())).is_ok());
    }

    #[test]
    fn the_kernel_namespace_reports_cut_threads_and_cache() {
        let kernel = Kernel::new(Arc::new(EndpointSpace::new()));
        let cap = Capability::root();
        kernel.cut("urn:data:a");
        kernel.cut("urn:data:a");
        kernel.cut("urn:data:b");

        let threads =
            block_on(kernel.issue(Request::new(Verb::Source, iri("urn:kernel:threads")), &cap))
                .unwrap();
        let text = String::from_utf8_lossy(&threads.bytes);
        assert!(text.contains("urn:data:a  gen 2"), "{text}");
        assert!(text.contains("urn:data:b  gen 1"), "{text}");

        let cache =
            block_on(kernel.issue(Request::new(Verb::Source, iri("urn:kernel:cache")), &cap))
                .unwrap();
        assert!(String::from_utf8_lossy(&cache.bytes).contains("entries"));
    }

    #[test]
    fn kernel_scheduler_resource_reports_the_injected_reporter_or_the_single_default() {
        struct FakeReporter;
        impl SchedulerReporter for FakeReporter {
            fn rows(&self) -> Vec<(String, String)> {
                vec![
                    ("backend".to_string(), "pool:4".to_string()),
                    ("threads".to_string(), "4".to_string()),
                ]
            }
        }
        let scheduler_request = || Request::new(Verb::Source, iri("urn:kernel:scheduler"));

        // With a reporter injected, the resource renders its rows.
        let kernel = Kernel::new(Arc::new(EndpointSpace::new()))
            .with_scheduler_reporter(Arc::new(FakeReporter));
        let out = block_on(kernel.issue(scheduler_request(), &Capability::root())).unwrap();
        let text = String::from_utf8_lossy(&out.bytes);
        assert!(
            text.contains("backend") && text.contains("pool:4") && text.contains("threads  "),
            "{text}"
        );

        // Without one, it reports the runtime-free single-threaded default.
        let plain = Kernel::new(Arc::new(EndpointSpace::new()));
        let out = block_on(plain.issue(scheduler_request(), &Capability::root())).unwrap();
        assert!(String::from_utf8_lossy(&out.bytes).contains("backend    single"));

        // Capability-gated like the other kernel resources: a scope lacking
        // `urn:cap:kernel:inspect` is refused.
        let scoped = Capability::scoped(["urn:cap:something:else"]);
        assert!(block_on(plain.issue(scheduler_request(), &scoped)).is_err());
    }

    #[test]
    fn kernel_constraint_resource_ranks_the_heaviest_uncached_target_first() {
        // Two uncacheable endpoints (every resolution recomputes — pure uncached work).
        let heavy = FnEndpoint::new("heavy", |_inv: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new("text/plain"),
                b"H".to_vec(),
            ))
        });
        let light = FnEndpoint::new("light", |_inv: &Invocation<'_>| {
            Ok(Representation::new(
                ReprType::new("text/plain"),
                b"L".to_vec(),
            ))
        });
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:test:heavy"), heavy)
            .bind(Exact::new("urn:test:light"), light);
        let kernel = Kernel::new(Arc::new(space));
        let cap = Capability::root();

        for _ in 0..3 {
            block_on(kernel.issue(Request::new(Verb::Source, iri("urn:test:heavy")), &cap))
                .unwrap();
        }
        block_on(kernel.issue(Request::new(Verb::Source, iri("urn:test:light")), &cap)).unwrap();

        // The introspection request itself short-circuits before recording, so the
        // window holds exactly the four resolutions above.
        let out = block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:kernel:constraint")),
            &cap,
        ))
        .unwrap();
        let text = String::from_utf8_lossy(&out.bytes);
        assert!(text.contains("last 4 resolutions"), "{text}");

        // No clock ⇒ ranked by uncached call count: heavy (3) before light (1).
        let heavy_pos = text.find("urn:test:heavy").expect("heavy listed");
        let light_pos = text.find("urn:test:light").expect("light listed");
        assert!(
            heavy_pos < light_pos,
            "heaviest target ranks first:\n{text}"
        );

        // The leader is flagged as the constraint.
        let heavy_line_end = text[heavy_pos..]
            .find('\n')
            .map_or(text.len(), |i| heavy_pos + i);
        assert!(
            text[heavy_pos..heavy_line_end].contains("← constraint"),
            "leader flagged:\n{text}"
        );
    }

    // --- Time-based expiry (Expiry::At + an injected Clock) ------------------

    #[test]
    fn most_restrictive_is_the_expiry_meet() {
        use Expiry::*;
        let t1 = Time::from_millis(100);
        let t2 = Time::from_millis(200);
        // Always dominates everything.
        assert_eq!(Always.most_restrictive(Never), Always);
        assert_eq!(At(t1).most_restrictive(Always), Always);
        // An At deadline beats Never (the composite inherits the deadline).
        assert_eq!(Never.most_restrictive(At(t1)), At(t1));
        // Two deadlines take the earlier; Never is the identity.
        assert_eq!(At(t1).most_restrictive(At(t2)), At(t1));
        assert_eq!(Never.most_restrictive(Never), Never);
    }

    /// An endpoint that stamps a deadline `window` ms ahead of the kernel's clock.
    fn timed_endpoint(name: &'static str, window: u64, calls: &'static AtomicU32) -> FnEndpoint {
        FnEndpoint::new(name, move |inv: &Invocation<'_>| {
            calls.fetch_add(1, Ordering::SeqCst);
            let repr = Representation::new(ReprType::new("text/plain"), b"v".to_vec());
            // Turn the relative freshness window into an absolute deadline via the
            // injected clock — exactly how the HTTP module will map `max-age`.
            Ok(match inv.now() {
                Some(now) => repr.cacheable_until(now.plus_millis(window)),
                None => repr.cacheable_until(Time::from_millis(window)),
            })
        })
    }

    /// The shipped fixed clock drives a real `Expiry::At` entry, so a crate reaching
    /// for it gets the same caching behaviour its hand-rolled copy gave it — and gets
    /// it without the copy. Stopped, so the entry stays fresh no matter how many reads
    /// consult the deadline; that immobility is the property, not a limitation.
    #[test]
    fn the_shipped_fixed_clock_drives_a_deadline_and_does_not_move() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let clock = FixedClock::at(1_000);
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:t:fixed"), timed_endpoint("f", 500, &CALLS)),
        ))
        .with_clock(Arc::new(clock));

        let read = || {
            block_on(kernel.issue(
                Request::new(Verb::Source, iri("urn:t:fixed")),
                &Capability::root(),
            ))
            .unwrap()
        };
        let first = read();
        assert_eq!(first.expiry, Expiry::At(Time::from_millis(1_500)));
        read();
        read();
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            1,
            "time never passed the deadline, so the cache served every later read"
        );
        assert_eq!(clock.now(), Time::from_millis(1_000), "still stopped");
    }

    #[test]
    fn time_based_entry_serves_until_its_deadline_then_recomputes() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let clock = TestClock::at(1_000);
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:t:x"), timed_endpoint("x", 500, &CALLS)),
        ))
        .with_clock(Arc::new(clock.clone()));
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:t:x"));

        // t=1000: computed, deadline = 1500.
        block_on(kernel.issue(req(), &cap)).unwrap();
        // t=1400: still fresh -> cache hit, not recomputed.
        clock.set(1_400);
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            1,
            "served from cache before deadline"
        );
        // t=1600: past the deadline -> stale, recomputes (new deadline = 2100).
        clock.set(1_600);
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst), 2, "recomputed after deadline");
    }

    #[test]
    fn a_clockless_kernel_declines_to_cache_a_deadline() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        // No clock injected: the kernel can't evaluate a deadline, so an `At`
        // result is never stored and recomputes every time (rather than risk
        // serving it forever).
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:t:x"), timed_endpoint("x", 500, &CALLS)),
        ));
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:t:x"));
        block_on(kernel.issue(req(), &cap)).unwrap();
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            2,
            "no clock -> At result not cached"
        );
        assert_eq!(kernel.cache_len(), 0);
    }

    #[test]
    fn a_deadline_propagates_through_composition() {
        static LEAF_CALLS: AtomicU32 = AtomicU32::new(0);
        let clock = TestClock::at(0);
        // `UpcaseOf` is a *permanently*-cacheable composer; sourcing a time-bounded
        // leaf must still cap the composite at the leaf's deadline (no fresher than
        // its most volatile part). LEAF_CALLS recomputing tracks the composite too,
        // since the composite only re-sources the leaf when it itself recomputes.
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new()
                .bind(
                    Exact::new("urn:t:leaf"),
                    timed_endpoint("leaf", 100, &LEAF_CALLS),
                )
                .bind(Exact::new("urn:test:upcase-of"), UpcaseOf),
        ))
        .with_clock(Arc::new(clock.clone()));
        let cap = Capability::root();
        let req = || {
            Request::new(Verb::Source, iri("urn:test:upcase-of"))
                .with_arg("src", ArgRef::Reference(iri("urn:t:leaf")))
        };

        // t=0: computed, inherited deadline = 100.
        let a = block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(a.bytes, b"V");
        // t=50: fresh -> composite served from cache, leaf not re-sourced.
        clock.set(50);
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(
            LEAF_CALLS.load(Ordering::SeqCst),
            1,
            "composite cached before the leaf's deadline"
        );
        // t=150: leaf deadline passed -> composite expired -> recomputes, re-sources leaf.
        clock.set(150);
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(
            LEAF_CALLS.load(Ordering::SeqCst),
            2,
            "composite expired with its leaf"
        );
    }

    #[test]
    fn is_cached_honours_the_deadline() {
        // The read-only probe must agree with the serving path: an expired entry
        // is not "cached", so a cache-status label (and the `cache` REPL command)
        // doesn't claim a Hit that the next issue would actually recompute.
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let clock = TestClock::at(0);
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:t:x"), timed_endpoint("x", 100, &CALLS)),
        ))
        .with_clock(Arc::new(clock.clone()));
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:t:x"));

        block_on(kernel.issue(req(), &cap)).unwrap(); // cached, deadline = 100
        clock.set(50);
        assert!(
            kernel.is_cached(&req(), &Capability::root()),
            "fresh entry probes as cached"
        );
        clock.set(150);
        assert!(
            !kernel.is_cached(&req(), &Capability::root()),
            "expired entry probes as not cached"
        );
    }

    // --- Concurrent fan-out (Invocation::fan_out + an injected Spawner) -------

    /// A cooperative spawner for tests: returns each task as its own completion
    /// future so `join_all` drives them on the current task. Proves the fan-out
    /// plumbing without real threads — the threadpool deadlock-freedom test lives in
    /// `ikigai-scheduler`, which owns the executor.
    struct InlineSpawner;
    impl Spawner for InlineSpawner {
        fn spawn(&self, task: BoxFuture<()>) -> BoxFuture<()> {
            task
        }
    }

    fn leaf(byte: &'static str) -> FnEndpoint {
        FnEndpoint::new("leaf", move |_inv: &Invocation<'_>| {
            Ok(
                Representation::new(ReprType::new("text/plain"), byte.as_bytes().to_vec())
                    .cacheable(),
            )
        })
    }

    /// An endpoint that fans out to a fixed list of leaves and concatenates them.
    struct FanParent {
        leaves: Vec<Iri>,
    }
    #[async_trait::async_trait]
    impl Endpoint for FanParent {
        async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
            let requests = self
                .leaves
                .iter()
                .map(|iri| Request::new(Verb::Source, iri.clone()))
                .collect();
            let mut body = Vec::new();
            for result in inv.fan_out(requests).await {
                body.extend_from_slice(&result?.bytes);
            }
            Ok(Representation::new(ReprType::new("text/plain"), body).cacheable())
        }
    }

    fn fan_space() -> EndpointSpace {
        EndpointSpace::new()
            .bind(Exact::new("urn:leaf:1"), leaf("1"))
            .bind(Exact::new("urn:leaf:2"), leaf("2"))
            .bind(Exact::new("urn:leaf:3"), leaf("3"))
            .bind(
                Exact::new("urn:fan:parent"),
                FanParent {
                    leaves: vec![iri("urn:leaf:1"), iri("urn:leaf:2"), iri("urn:leaf:3")],
                },
            )
    }

    #[test]
    fn fan_out_resolves_all_in_order_on_a_scheduled_kernel() {
        let kernel = Kernel::new(Arc::new(fan_space())).into_scheduled(Arc::new(InlineSpawner));
        let out = block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:fan:parent")),
            &Capability::root(),
        ))
        .unwrap();
        assert_eq!(out.bytes, b"123", "fan-out preserves request order");
    }

    #[test]
    fn fan_out_falls_back_to_sequential_without_a_spawner() {
        // A plain (non-scheduled) kernel has no spawner — fan_out resolves
        // sequentially, with an identical result.
        let kernel = Kernel::new(Arc::new(fan_space()));
        let out = block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:fan:parent")),
            &Capability::root(),
        ))
        .unwrap();
        assert_eq!(out.bytes, b"123");
    }

    #[test]
    fn declared_requires_is_kernel_enforced_before_dispatch() {
        // `Gated` DECLARES a capability and counts its own entries, so the counter
        // proves a denial happens before dispatch — declared = enforced is the
        // kernel's guarantee now, not module discipline.
        let invokes = Arc::new(AtomicU32::new(0));
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:gated"), Gated(invokes.clone())),
        ));
        let req = || Request::new(Verb::Source, iri("urn:gated"));

        // Without the declared cap: typed Denied, endpoint never entered.
        let narrow = Capability::root().attenuate(["urn:cap:other".to_string()]);
        let err = block_on(kernel.issue(req(), &narrow)).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "got {err:?}");
        assert_eq!(invokes.load(Ordering::SeqCst), 0, "denied before dispatch");

        // With it (exact grant), and as root: both pass.
        let holder = Capability::root().attenuate(["urn:cap:demo:read".to_string()]);
        block_on(kernel.issue(req(), &holder)).unwrap();
        block_on(kernel.issue(req(), &Capability::root())).unwrap();
        assert_eq!(invokes.load(Ordering::SeqCst), 2);

        // The floor also fences the CACHE: root's issue above cached the result;
        // a later caller without the declared cap is denied, not served.
        let err = block_on(kernel.issue(req(), &narrow)).unwrap_err();
        assert!(matches!(err, Error::Denied(_)));
    }

    // ---- attenuation on a sub-request ------------------------------------

    /// An endpoint that dereferences a caller-named IRI, either verbatim or under a
    /// narrowed authority — the two halves of the shape `issue_attenuated` exists for.
    struct Deref {
        /// Scopes to narrow to, or `None` to pass the caller's capability through.
        narrow_to: Option<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl Endpoint for Deref {
        async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
            // The target the CALLER named — exactly the value a module must not
            // dereference with every scope its caller happens to hold.
            let target = iri(inv.inline_str("src")?);
            let inner = match &self.narrow_to {
                Some(scopes) => inv.source_attenuated(&target, scopes.clone()).await?,
                None => inv.source(&target).await?,
            };
            Ok(Representation::new(
                ReprType::new("text/plain"),
                inner.bytes,
            ))
        }
        fn name(&self) -> &str {
            "deref"
        }
    }

    /// A space with a secret behind `urn:cap:secret:read`, bound under two derefs:
    /// one that forwards the caller's authority, one that narrows to fs-read only.
    fn deref_kernel() -> Kernel {
        let secret = FnEndpoint::new("secret", |_inv| {
            Ok(
                Representation::new(ReprType::new("text/plain"), b"s3cr3t".to_vec())
                    .cacheable()
                    .depends_on("urn:data:secret"),
            )
        })
        .with_description(
            Description::new("secret")
                .verb(Verb::Source)
                .requires("urn:cap:secret:read"),
        );
        Kernel::new(Arc::new(
            EndpointSpace::new()
                .bind(Exact::new("urn:data:secret"), secret)
                .bind(Exact::new("urn:demo:deref"), Deref { narrow_to: None })
                .bind(
                    Exact::new("urn:demo:deref-narrowed"),
                    Deref {
                        narrow_to: Some(vec!["urn:cap:fs:read".to_string()]),
                    },
                ),
        ))
    }

    fn deref_request(via: &str) -> Request {
        Request::new(Verb::Source, iri(via))
            .with_arg("src", ArgRef::Inline(b"urn:data:secret".to_vec()))
    }

    #[test]
    fn an_attenuated_sub_request_drops_authority_the_caller_still_holds() {
        let kernel = deref_kernel();
        // The caller holds secret-read. Through the forwarding deref it reaches the
        // secret — today's only behaviour, and the hazard: the module dereferenced a
        // target it did not choose with every scope its caller had.
        let caller = Capability::scoped(["urn:cap:secret:read", "urn:cap:fs:read"]);
        let reached = block_on(kernel.issue(deref_request("urn:demo:deref"), &caller)).unwrap();
        assert_eq!(reached.bytes, b"s3cr3t");

        // The SAME caller, through the deref that narrows first: denied.
        let err =
            block_on(kernel.issue(deref_request("urn:demo:deref-narrowed"), &caller)).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "got {err:?}");
    }

    #[test]
    fn attenuating_a_sub_request_cannot_widen_past_the_caller() {
        // The module asks for a scope its caller does not hold. Attenuation is an
        // intersection, so it gets nothing — a narrowing call can never mint authority.
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new()
                .bind(
                    Exact::new("urn:data:secret"),
                    FnEndpoint::new("secret", |_inv| {
                        Ok(Representation::new(
                            ReprType::new("text/plain"),
                            b"s3cr3t".to_vec(),
                        ))
                    })
                    .with_description(
                        Description::new("secret")
                            .verb(Verb::Source)
                            .requires("urn:cap:secret:read"),
                    ),
                )
                .bind(
                    Exact::new("urn:demo:grabby"),
                    Deref {
                        // "Give me secret-read" — from a caller that has none.
                        narrow_to: Some(vec!["urn:cap:secret:read".to_string()]),
                    },
                ),
        ));
        let caller = Capability::scoped(["urn:cap:fs:read"]);
        let err = block_on(kernel.issue(deref_request("urn:demo:grabby"), &caller)).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "got {err:?}");

        // And with a caller that DOES hold it, the same narrowing call succeeds —
        // proving the refusal above was the intersection, not a broken code path.
        let holder = Capability::scoped(["urn:cap:secret:read"]);
        let reached = block_on(kernel.issue(deref_request("urn:demo:grabby"), &holder)).unwrap();
        assert_eq!(reached.bytes, b"s3cr3t");
    }

    #[test]
    fn an_attenuated_sub_request_is_still_recorded_as_a_dependency() {
        // The narrowed path must record expiry and golden threads exactly like
        // `issue` — it is the same sub-request, under less authority. A silent loss
        // here would show up only as a composite that never invalidates.
        let caller = Capability::scoped(["urn:cap:secret:read", "urn:cap:fs:read"]);
        let narrowing = Deref {
            narrow_to: Some(vec![
                "urn:cap:secret:read".to_string(),
                "urn:cap:fs:read".to_string(),
            ]),
        };
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new()
                .bind(
                    Exact::new("urn:data:secret"),
                    FnEndpoint::new("secret", |_inv| {
                        Ok(
                            Representation::new(ReprType::new("text/plain"), b"s3cr3t".to_vec())
                                .cacheable()
                                .depends_on("urn:data:secret"),
                        )
                    })
                    .with_description(
                        Description::new("secret")
                            .verb(Verb::Source)
                            .requires("urn:cap:secret:read"),
                    ),
                )
                .bind(Exact::new("urn:demo:deref-narrowed"), narrowing),
        ));
        let composite =
            block_on(kernel.issue(deref_request("urn:demo:deref-narrowed"), &caller)).unwrap();
        assert!(
            composite
                .threads()
                .iter()
                .any(|t| t.as_str() == "urn:data:secret"),
            "the sub-resource's golden thread must propagate through the narrowed \
             path too; got {:?}",
            composite.threads()
        );
    }

    #[test]
    fn wildcard_requires_passes_any_grant_under_the_family() {
        struct NetLike;
        #[async_trait::async_trait]
        impl Endpoint for NetLike {
            async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    b"ok".to_vec(),
                ))
            }
            fn name(&self) -> &str {
                "netlike"
            }
            fn describe(&self) -> Description {
                Description::new("netlike")
                    .verb(Verb::Source)
                    .requires("urn:cap:net:*")
            }
        }
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:net"), NetLike),
        ));
        let req = || Request::new(Verb::Source, iri("urn:net"));
        // A host-scoped grant under the family passes the wildcard floor (the
        // module's own per-host ACL stays its ceiling).
        let host_scoped = Capability::root().attenuate(["urn:cap:net:localhost:11434".to_string()]);
        block_on(kernel.issue(req(), &host_scoped)).unwrap();
        // No grant under the family: denied at the floor.
        let unrelated = Capability::root().attenuate(["urn:cap:fs:read:/tmp".to_string()]);
        let err = block_on(kernel.issue(req(), &unrelated)).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "got {err:?}");
    }

    #[test]
    fn per_verb_action_specs_enforce_independently() {
        struct Calendarish;
        #[async_trait::async_trait]
        impl Endpoint for Calendarish {
            async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    b"ok".to_vec(),
                ))
            }
            fn name(&self) -> &str {
                "calendarish"
            }
            fn describe(&self) -> Description {
                use crate::describe::ActionSpec;
                Description::new("calendarish")
                    .verb(Verb::Source)
                    .verb(Verb::Sink)
                    .action(ActionSpec::new(Verb::Source).requires("urn:cap:cal:read"))
                    .action(ActionSpec::new(Verb::Sink).requires("urn:cap:cal:write"))
            }
        }
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:cal"), Calendarish),
        ));
        let reader = Capability::root().attenuate(["urn:cap:cal:read".to_string()]);
        block_on(kernel.issue(Request::new(Verb::Source, iri("urn:cal")), &reader)).unwrap();
        let err =
            block_on(kernel.issue(Request::new(Verb::Sink, iri("urn:cal")), &reader)).unwrap_err();
        assert!(
            matches!(err, Error::Denied(_)),
            "a reader must not pass the write action's floor: {err:?}"
        );
    }

    /// A tracer that keeps every event, for the denial tests below.
    struct Rec(std::sync::Mutex<Vec<TraceEvent>>);
    impl Tracer for Rec {
        fn record(&self, event: TraceEvent) {
            self.0.lock().unwrap().push(event);
        }
    }
    impl Rec {
        fn new() -> Arc<Rec> {
            Arc::new(Rec(std::sync::Mutex::new(Vec::new())))
        }
        fn events(&self) -> Vec<TraceEvent> {
            self.0.lock().unwrap().clone()
        }
    }

    /// An endpoint that declares a capability and counts its own entries.
    struct Gated(Arc<AtomicU32>);
    #[async_trait::async_trait]
    impl Endpoint for Gated {
        async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Representation::new(ReprType::new("text/plain"), b"in".to_vec()).cacheable())
        }
        fn name(&self) -> &str {
            "gated"
        }
        fn describe(&self) -> Description {
            Description::new("gated")
                .verb(Verb::Source)
                .requires("urn:cap:demo:read")
        }
    }

    #[test]
    fn a_denied_bound_endpoint_reports_the_refusal() {
        // The point of the whole change: enforcement happens BEFORE dispatch, so
        // the endpoint is never entered and cannot report its own refusal. Without
        // the kernel reporting it, a denial reaches no observer at all.
        let invokes = Arc::new(AtomicU32::new(0));
        // WITH a clock installed — otherwise `started`/`ended` would be `None` for
        // every event and the assertion below would prove nothing.
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:gated"), Gated(invokes.clone())),
        ))
        .with_clock(Arc::new(TestClock::at(1_000)));

        let narrow = Capability::root().attenuate(["urn:cap:other".to_string()]);
        let rec = Rec::new();
        let err = block_on(kernel.issue_traced(
            Request::new(Verb::Source, iri("urn:gated")),
            &narrow,
            rec.clone(),
        ))
        .unwrap_err();

        assert!(matches!(err, Error::Denied(_)), "got {err:?}");
        assert_eq!(invokes.load(Ordering::SeqCst), 0, "denied before dispatch");

        let events = rec.events();
        assert_eq!(events.len(), 1, "the refusal IS the event: {events:?}");
        let event = &events[0];
        assert_eq!(event.target, "urn:gated");
        // The two facts a denial entry needs, already carried by `TraceEvent`:
        // which scope was missing, and the authority it was missing from.
        assert_eq!(
            event.notes,
            vec![(DENIED_NOTE.to_string(), "urn:cap:demo:read".to_string())],
        );
        assert_eq!(
            event.capability,
            Some(vec!["urn:cap:other".to_string()]),
            "the authority that was not enough"
        );
        // Nothing ran, and the clock is installed — so these are `None` by
        // decision, not by absence.
        assert!(
            event.started.is_none() && event.ended.is_none(),
            "a refused request never ran: {event:?}"
        );
        assert!(!event.cache_hit);
    }

    #[test]
    fn a_denied_kernel_builtin_reports_the_refusal_too() {
        // A denied `urn:kernel:cut` is the same kind of security fact as a denied
        // endpoint. Note the deliberate asymmetry documented at `issue_kernel`: a
        // SUCCESSFUL builtin records nothing, so the denial is the only event this
        // namespace ever produces.
        let kernel = Kernel::new(Arc::new(EndpointSpace::new()));
        let narrow = Capability::root().attenuate(["urn:cap:other".to_string()]);
        let rec = Rec::new();
        let err = block_on(kernel.issue_traced(
            Request::new(Verb::Sink, iri("urn:kernel:cut")),
            &narrow,
            rec.clone(),
        ))
        .unwrap_err();

        assert!(matches!(err, Error::Denied(_)), "got {err:?}");
        let events = rec.events();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].target, "urn:kernel:cut");
        assert_eq!(
            events[0].notes,
            vec![(DENIED_NOTE.to_string(), "urn:cap:kernel:cut".to_string())],
        );
        assert!(events[0].started.is_none() && events[0].ended.is_none());
    }

    #[test]
    fn the_globally_installed_tracer_sees_denials_and_permitted_hops_differ() {
        // The other tracer path (`set_tracer`, the global slot) reports denials
        // too — that is the one ikigai-log installs, so it is the one that decides
        // whether "capability denials always land" is true.
        //
        // And the negative half, which is what makes the note meaningful: a
        // PERMITTED resolution carries no denial note. An observer keying on
        // `DENIED_NOTE` must not see false positives.
        let invokes = Arc::new(AtomicU32::new(0));
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:gated"), Gated(invokes.clone())),
        ));
        let rec = Rec::new();
        kernel.set_tracer(rec.clone());

        let holder = Capability::root().attenuate(["urn:cap:demo:read".to_string()]);
        block_on(kernel.issue(Request::new(Verb::Source, iri("urn:gated")), &holder)).unwrap();
        let narrow = Capability::root().attenuate(["urn:cap:other".to_string()]);
        block_on(kernel.issue(Request::new(Verb::Source, iri("urn:gated")), &narrow)).unwrap_err();

        let events = rec.events();
        assert_eq!(events.len(), 2, "{events:?}");
        let denials: Vec<_> = events
            .iter()
            .filter(|e| e.notes.iter().any(|(k, _)| k == DENIED_NOTE))
            .collect();
        assert_eq!(denials.len(), 1, "exactly the refused hop: {events:?}");
        assert_eq!(
            denials[0].notes,
            vec![(DENIED_NOTE.to_string(), "urn:cap:demo:read".to_string())],
        );
        // The permitted hop ran, so it is the one with a span the tree can hang
        // off and no denial note.
        assert_eq!(invokes.load(Ordering::SeqCst), 1);
        assert!(events[0].notes.is_empty(), "{:?}", events[0]);
    }

    #[test]
    fn issue_traced_records_a_self_contained_tree() {
        struct Rec(std::sync::Mutex<Vec<TraceEvent>>);
        impl Tracer for Rec {
            fn record(&self, event: TraceEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let kernel = Kernel::new(Arc::new(fan_space())).into_scheduled(Arc::new(InlineSpawner));
        let rec = Arc::new(Rec(std::sync::Mutex::new(Vec::new())));
        block_on(kernel.issue_traced(
            Request::new(Verb::Source, iri("urn:fan:parent")),
            &Capability::root(),
            rec.clone(),
        ))
        .unwrap();
        let events = rec.0.lock().unwrap().clone();
        assert_eq!(events.len(), 4, "parent + three leaves");
        // Span ids are private to this trace: 0-based, self-contained.
        let mut spans: Vec<u64> = events.iter().map(|e| e.span).collect();
        spans.sort_unstable();
        assert_eq!(spans, vec![0, 1, 2, 3]);
        let root = events
            .iter()
            .find(|e| e.target == "urn:fan:parent")
            .unwrap();
        assert_eq!(root.parent, None);
        for leaf in events.iter().filter(|e| e.target.starts_with("urn:leaf:")) {
            assert_eq!(
                leaf.parent,
                Some(root.span),
                "each leaf links to the fan parent"
            );
        }
    }

    #[test]
    fn trace_note_lands_on_the_endpoints_own_span() {
        struct Rec(std::sync::Mutex<Vec<TraceEvent>>);
        impl Tracer for Rec {
            fn record(&self, event: TraceEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let space = EndpointSpace::new().bind(
            Exact::new("urn:noted"),
            FnEndpoint::new("noted", |inv: &Invocation<'_>| {
                inv.trace_note("model", "llama3.2:3b");
                inv.trace_note("provider", "ollama");
                Ok(Representation::new(ReprType::new("text/plain"), vec![]))
            }),
        );
        let kernel = Kernel::new(Arc::new(space));
        let rec = Arc::new(Rec(std::sync::Mutex::new(Vec::new())));
        block_on(kernel.issue_traced(
            Request::new(Verb::Source, iri("urn:noted")),
            &Capability::root(),
            rec.clone(),
        ))
        .unwrap();
        let events = rec.0.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].notes,
            vec![
                ("model".to_string(), "llama3.2:3b".to_string()),
                ("provider".to_string(), "ollama".to_string()),
            ]
        );

        // Untraced: the same endpoint invokes fine and the note is a free no-op.
        block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:noted")),
            &Capability::root(),
        ))
        .unwrap();
    }

    #[test]
    fn concurrent_traced_resolutions_stay_isolated() {
        // Two resolutions traced CONCURRENTLY on one shared kernel: each collector
        // must receive only its own resolution's events. Under the global-slot
        // scheme this interleaved — the second set_tracer displaced the first and
        // one tenant's spans landed in the other's collector (the cross-tenant
        // trace leak from the 2026-07-21 security review).
        struct Rec(std::sync::Mutex<Vec<TraceEvent>>);
        impl Tracer for Rec {
            fn record(&self, event: TraceEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let kernel = Kernel::new(Arc::new(fan_space())).into_scheduled(Arc::new(InlineSpawner));
        let rec_a = Arc::new(Rec(std::sync::Mutex::new(Vec::new())));
        let rec_b = Arc::new(Rec(std::sync::Mutex::new(Vec::new())));
        let (a, b) = block_on(futures_util::future::join(
            kernel.issue_traced(
                Request::new(Verb::Source, iri("urn:fan:parent")),
                &Capability::root(),
                rec_a.clone(),
            ),
            kernel.issue_traced(
                Request::new(Verb::Source, iri("urn:leaf:1")),
                &Capability::root(),
                rec_b.clone(),
            ),
        ));
        a.unwrap();
        b.unwrap();
        let a_events = rec_a.0.lock().unwrap().clone();
        let b_events = rec_b.0.lock().unwrap().clone();
        assert_eq!(a_events.len(), 4, "A sees its whole tree");
        assert_eq!(b_events.len(), 1, "B sees exactly its one resolution");
        assert!(
            b_events.iter().all(|e| e.target == "urn:leaf:1"),
            "none of A's spans leaked into B's collector: {b_events:?}"
        );
        assert!(
            a_events.iter().any(|e| e.target == "urn:fan:parent"),
            "A's root is in A's collector"
        );
        // Each trace's span ids are independent and 0-based.
        assert_eq!(b_events[0].span, 0);
        assert_eq!(b_events[0].parent, None);
    }

    #[test]
    fn an_installed_tracer_records_each_invocation_then_stops_when_cleared() {
        struct Recorder(std::sync::Mutex<Vec<TraceEvent>>);
        impl Tracer for Recorder {
            fn record(&self, event: TraceEvent) {
                self.0.lock().expect("recorder").push(event);
            }
        }
        let recorder = Arc::new(Recorder(std::sync::Mutex::new(Vec::new())));
        let kernel = Kernel::new(Arc::new(fan_space()));
        let cap = Capability::root();
        let parent = || Request::new(Verb::Source, iri("urn:fan:parent"));

        kernel.set_tracer(recorder.clone());
        block_on(kernel.issue(parent(), &cap)).unwrap();
        kernel.clear_tracer();

        let events = recorder.0.lock().expect("recorder").clone();
        let targets: Vec<&str> = events.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(events.len(), 4, "parent + 3 leaves, one event each");
        assert!(targets.contains(&"urn:fan:parent"));
        assert!(["urn:leaf:1", "urn:leaf:2", "urn:leaf:3"]
            .iter()
            .all(|leaf| targets.contains(leaf)));
        assert!(
            events.iter().all(|e| !e.cache_hit),
            "first resolution computes everything"
        );

        // Span linkage: the parent is a root (no parent span); every leaf names the
        // parent's span as its own parent — the edges that rebuild the execution tree.
        let root = events
            .iter()
            .find(|e| e.target == "urn:fan:parent")
            .expect("parent recorded");
        assert_eq!(root.parent, None, "the traced root has no parent span");
        assert!(
            events
                .iter()
                .filter(|e| e.target.starts_with("urn:leaf:"))
                .all(|leaf| leaf.parent == Some(root.span)),
            "each fanned-out leaf links to the parent's span"
        );
        assert!(
            events.iter().all(|e| e.capability.is_none()),
            "under root authority every hop records capability = None"
        );

        // A cleared tracer records nothing for subsequent resolutions.
        block_on(kernel.issue(parent(), &cap)).unwrap();
        assert_eq!(recorder.0.lock().expect("recorder").len(), 4);
    }

    #[test]
    fn record_subtree_rebases_a_remote_subtree_under_the_mount() {
        struct Recorder(std::sync::Mutex<Vec<TraceEvent>>);
        impl Tracer for Recorder {
            fn record(&self, event: TraceEvent) {
                self.0.lock().expect("recorder").push(event);
            }
        }
        let recorder = Arc::new(Recorder(std::sync::Mutex::new(Vec::new())));
        let kernel = Kernel::new(Arc::new(fan_space()));
        kernel.set_tracer(recorder.clone());

        // A remote kernel's subtree: a root (parent None) with one child, in the
        // remote's own span namespace.
        let ev = |target: &str, span: u64, parent: Option<u64>| TraceEvent {
            target: target.to_string(),
            thread: "remote".to_string(),
            started: None,
            ended: None,
            cache_hit: false,
            span,
            parent,
            capability: None,
            notes: Vec::new(),
        };
        let remote = vec![
            ev("urn:remote:root", 0, None),
            ev("urn:remote:child", 1, Some(0)),
        ];
        // Forwarded from a local mount node whose span is 42.
        Issuer::record_subtree(&kernel, Some(42), remote);
        kernel.clear_tracer();

        let events = recorder.0.lock().expect("recorder").clone();
        assert_eq!(events.len(), 2);
        let root = events
            .iter()
            .find(|e| e.target == "urn:remote:root")
            .unwrap();
        let child = events
            .iter()
            .find(|e| e.target == "urn:remote:child")
            .unwrap();
        // The remote root is re-parented under the mount node; the child follows the
        // root's *re-based* span, not its original remote id.
        assert_eq!(
            root.parent,
            Some(42),
            "remote root re-parented under the mount"
        );
        assert_eq!(
            child.parent,
            Some(root.span),
            "child follows the re-based root"
        );
        assert_ne!(root.span, child.span, "distinct re-based span ids");
    }

    #[test]
    fn each_event_records_the_capability_it_ran_under() {
        struct Recorder(std::sync::Mutex<Vec<TraceEvent>>);
        impl Tracer for Recorder {
            fn record(&self, event: TraceEvent) {
                self.0.lock().expect("recorder").push(event);
            }
        }
        let recorder = Arc::new(Recorder(std::sync::Mutex::new(Vec::new())));
        let kernel = Kernel::new(Arc::new(fan_space()));
        // A scoped (non-root) authority — passed out of input order to prove the
        // recorded set is sorted (the fingerprint's stability property).
        let cap = Capability::scoped(["urn:cap:b", "urn:cap:a"]);

        kernel.set_tracer(recorder.clone());
        block_on(kernel.issue(Request::new(Verb::Source, iri("urn:fan:parent")), &cap)).unwrap();
        kernel.clear_tracer();

        let events = recorder.0.lock().expect("recorder").clone();
        assert!(!events.is_empty(), "the scoped resolution recorded events");
        assert!(
            events
                .iter()
                .all(|e| e.capability
                    == Some(vec!["urn:cap:a".to_string(), "urn:cap:b".to_string()])),
            "each hop names the sorted scope set it ran under — attenuation is visible"
        );
    }

    // ---- scope_sync: the sync-issue bridge -------------------------------

    /// A composite endpoint whose body is a SYNC closure on its own thread,
    /// issuing sub-requests through the blocking [`SyncIssuer`] — the shape
    /// every embedded evaluator (Steel, Python, JS) needs.
    struct SyncComposite;
    #[async_trait::async_trait]
    impl Endpoint for SyncComposite {
        async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
            let out = inv
                .scope_sync(|issuer| {
                    let a = issuer.source(&Iri::parse("urn:data:a").unwrap())?;
                    let upper = issuer.issue(
                        Request::new(Verb::Source, Iri::parse("urn:test:to-upper").unwrap())
                            .with_arg("in", ArgRef::Inline(a.bytes.clone())),
                    )?;
                    Ok(format!("{}!", String::from_utf8_lossy(&upper.bytes)))
                })
                .await??;
            Ok(Representation::new(ReprType::new("text/plain"), out.into_bytes()).cacheable())
        }
        fn name(&self) -> &str {
            "sync-composite"
        }
    }

    fn sync_scope_kernel(volatile_counter: Arc<AtomicU32>) -> Kernel {
        let space = EndpointSpace::new()
            .bind(
                Exact::new("urn:data:a"),
                FnEndpoint::new("a", move |_inv| {
                    // VOLATILE on purpose (no .cacheable()): each read is a new
                    // value, so anything derived from it must not be cached.
                    let n = volatile_counter.fetch_add(1, Ordering::SeqCst);
                    Ok(Representation::new(
                        ReprType::new("text/plain"),
                        format!("a{n}").into_bytes(),
                    ))
                }),
            )
            .bind(Exact::new("urn:test:to-upper"), builtins::to_upper())
            .bind(Exact::new("urn:demo:composite"), SyncComposite);
        Kernel::new(Arc::new(space))
    }

    #[test]
    fn a_sync_scope_issues_through_the_kernel() {
        let kernel = sync_scope_kernel(Arc::new(AtomicU32::new(0)));
        let cap = Capability::root();
        let rep =
            block_on(kernel.issue(Request::new(Verb::Source, iri("urn:demo:composite")), &cap))
                .unwrap();
        assert_eq!(rep.bytes, b"A0!");
    }

    /// The bridge preserves cache CORRECTNESS: the closure's sub-requests are
    /// dependencies of the composite, so a volatile dependency keeps the
    /// (nominally cacheable) composite OUT of the cache — same rule as
    /// `inv.issue` from async code.
    #[test]
    fn a_sync_scopes_volatile_dependency_defeats_the_composite_cache() {
        let counter = Arc::new(AtomicU32::new(0));
        let kernel = sync_scope_kernel(Arc::clone(&counter));
        let cap = Capability::root();
        let request = || Request::new(Verb::Source, iri("urn:demo:composite"));
        let first = block_on(kernel.issue(request(), &cap)).unwrap();
        let second = block_on(kernel.issue(request(), &cap)).unwrap();
        assert_eq!(first.bytes, b"A0!");
        assert_eq!(
            second.bytes, b"A1!",
            "recomputed — the volatile dependency crossed the bridge"
        );
    }

    /// Authority does not widen across the bridge: the closure's calls run
    /// under the MINTING invocation's capability, so a gated sub-resource
    /// stays gated.
    #[test]
    fn a_sync_scope_cannot_widen_authority() {
        struct GatedProbe;
        #[async_trait::async_trait]
        impl Endpoint for GatedProbe {
            async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
                let result = inv
                    .scope_sync(|issuer| issuer.source(&Iri::parse("urn:data:secret").unwrap()))
                    .await?;
                match result {
                    Ok(_) => Ok(Representation::new(
                        ReprType::new("text/plain"),
                        b"reached".to_vec(),
                    )),
                    Err(e) => Err(e),
                }
            }
            fn name(&self) -> &str {
                "gated-probe"
            }
        }
        let space = EndpointSpace::new()
            .bind(
                Exact::new("urn:data:secret"),
                FnEndpoint::new("secret", |_inv| {
                    Ok(Representation::new(
                        ReprType::new("text/plain"),
                        b"s3cr3t".to_vec(),
                    ))
                })
                .with_description(
                    Description::new("secret")
                        .verb(Verb::Source)
                        .requires("urn:cap:secret:read"),
                ),
            )
            .bind(Exact::new("urn:demo:probe"), GatedProbe);
        let kernel = Kernel::new(Arc::new(space));

        // Insufficient capability: the sub-request is denied INSIDE the scope.
        let narrow = Capability::scoped(["urn:cap:unrelated"]);
        let denied =
            block_on(kernel.issue(Request::new(Verb::Source, iri("urn:demo:probe")), &narrow));
        assert!(denied.is_err(), "the bridge must not widen authority");

        // Sufficient capability: the same probe reaches it.
        let held = Capability::scoped(["urn:cap:secret:read"]);
        let reached =
            block_on(kernel.issue(Request::new(Verb::Source, iri("urn:demo:probe")), &held))
                .unwrap();
        assert_eq!(reached.bytes, b"reached");
    }

    /// The handle is cloneable and Send: the closure may fan work across its
    /// OWN threads, each holding a clone; the drain serves them all.
    #[test]
    fn sync_issuer_clones_serve_multiple_threads() {
        struct Fanning;
        #[async_trait::async_trait]
        impl Endpoint for Fanning {
            async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
                let out = inv
                    .scope_sync(|issuer| {
                        let handles: Vec<_> = (0..2)
                            .map(|_| {
                                let issuer = issuer.clone();
                                std::thread::spawn(move || {
                                    issuer
                                        .issue(
                                            Request::new(
                                                Verb::Source,
                                                Iri::parse("urn:test:to-upper").unwrap(),
                                            )
                                            .with_arg("in", ArgRef::Inline(b"hi".to_vec())),
                                        )
                                        .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
                                })
                            })
                            .collect();
                        let mut parts: Vec<String> = Vec::new();
                        for h in handles {
                            parts.push(h.join().expect("worker thread")?);
                        }
                        Ok(parts.join("+"))
                    })
                    .await??;
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    out.into_bytes(),
                ))
            }
            fn name(&self) -> &str {
                "fanning"
            }
        }
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:test:to-upper"), builtins::to_upper())
            .bind(Exact::new("urn:demo:fanning"), Fanning);
        let kernel = Kernel::new(Arc::new(space));
        let rep = block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:demo:fanning")),
            &Capability::root(),
        ))
        .unwrap();
        assert_eq!(rep.bytes, b"HI+HI");
    }

    /// A panic in the closure surfaces as an ERROR from the scope — the
    /// kernel (and its executor) survive to serve the next request.
    #[test]
    fn a_panicking_sync_scope_is_an_error_not_a_poisoned_kernel() {
        struct Panicking;
        #[async_trait::async_trait]
        impl Endpoint for Panicking {
            async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
                let _: String = inv
                    .scope_sync(|_issuer| panic!("evaluator blew up"))
                    .await?;
                unreachable!("the scope must error")
            }
            fn name(&self) -> &str {
                "panicking"
            }
        }
        let space = EndpointSpace::new()
            .bind(Exact::new("urn:test:to-upper"), builtins::to_upper())
            .bind(Exact::new("urn:demo:panic"), Panicking);
        let kernel = Kernel::new(Arc::new(space));
        let err = block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:demo:panic")),
            &Capability::root(),
        ))
        .unwrap_err();
        assert!(format!("{err}").contains("panicked"), "{err}");
        // The kernel still answers.
        let ok = block_on(
            kernel.issue(
                Request::new(Verb::Source, iri("urn:test:to-upper"))
                    .with_arg("in", ArgRef::Inline(b"alive".to_vec())),
                &Capability::root(),
            ),
        )
        .unwrap();
        assert_eq!(ok.bytes, b"ALIVE");
    }

    /// Detached invocations (no kernel) refuse the scope with a clean error.
    #[test]
    fn a_detached_invocation_refuses_a_sync_scope() {
        let request = Request::new(Verb::Source, iri("urn:x:y"));
        let bindings = crate::grammar::Bindings::default();
        let cap = Capability::root();
        let inv = Invocation::detached(&request, &bindings, &cap);
        let result: Result<()> = block_on(inv.scope_sync(|_issuer| ()));
        assert!(result.is_err());
    }

    // --- cache soundness (ledger #512) and the nesting budget (#513) ----------

    /// A stateful resource like `Cell`, minus the one line: its cacheable `Source`
    /// declares NO thread. Before 0.1.73 a write through the kernel left its cached
    /// read stale; the kernel now hangs the read from its own canonical target.
    struct UndeclaredCell {
        value: Mutex<Vec<u8>>,
        /// Shared with the test, so it can count reads after the cell is bound.
        reads: Arc<AtomicU32>,
    }

    impl UndeclaredCell {
        fn holding(value: &[u8]) -> (Self, Arc<AtomicU32>) {
            let reads = Arc::new(AtomicU32::new(0));
            let cell = UndeclaredCell {
                value: Mutex::new(value.to_vec()),
                reads: Arc::clone(&reads),
            };
            (cell, reads)
        }
    }

    #[async_trait::async_trait]
    impl Endpoint for UndeclaredCell {
        async fn invoke(&self, cx: &Invocation<'_>) -> Result<Representation> {
            match cx.request.verb {
                Verb::Source => {
                    self.reads.fetch_add(1, Ordering::SeqCst);
                    let value = self.value.lock().expect("cell").clone();
                    Ok(Representation::new(ReprType::new("text/plain"), value).cacheable())
                }
                Verb::Exists => {
                    self.reads.fetch_add(1, Ordering::SeqCst);
                    let present = !self.value.lock().expect("cell").is_empty();
                    Ok(Representation::new(
                        ReprType::new("text/plain"),
                        present.to_string().into_bytes(),
                    )
                    .cacheable())
                }
                Verb::Sink => {
                    *self.value.lock().expect("cell") = cx.inline_arg("content")?.to_vec();
                    Ok(Representation::new(
                        ReprType::new("text/plain"),
                        b"ok".to_vec(),
                    ))
                }
                other => Err(Error::Endpoint(format!("cell: unsupported {other:?}"))),
            }
        }
    }

    fn sink(target: &str, content: &[u8]) -> Request {
        Request::new(Verb::Sink, iri(target)).with_arg("content", ArgRef::Inline(content.to_vec()))
    }

    #[test]
    fn a_cacheable_read_hangs_from_its_own_canonical_target_without_declaring_it() {
        // `cutting_a_thread_invalidates_the_entry_that_declared_it`, with the manual
        // `.depends_on` REMOVED and the cut fired by a `Sink` through the kernel.
        // The proof of hole A's fix: the endpoint declares nothing, and the write
        // still invalidates the read.
        let (cell, reads) = UndeclaredCell::holding(b"v1");
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:data:undeclared"), cell),
        ));
        let cap = Capability::root();
        let read = || Request::new(Verb::Source, iri("urn:data:undeclared"));

        assert_eq!(block_on(kernel.issue(read(), &cap)).unwrap().bytes, b"v1");
        assert_eq!(block_on(kernel.issue(read(), &cap)).unwrap().bytes, b"v1");
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "cached on the second issue"
        );
        assert!(kernel.is_cached(&read(), &cap));

        // The write cuts `urn:data:undeclared`; the read hangs from it, undeclared.
        block_on(kernel.issue(sink("urn:data:undeclared", b"v2"), &cap)).unwrap();
        assert!(
            !kernel.is_cached(&read(), &cap),
            "the write invalidated a read that declared no thread"
        );
        assert_eq!(block_on(kernel.issue(read(), &cap)).unwrap().bytes, b"v2");
        assert_eq!(reads.load(Ordering::SeqCst), 2);

        // The external half is the same thread: a watcher cutting the name.
        kernel.cut("urn:data:undeclared");
        assert!(!kernel.is_cached(&read(), &cap));
    }

    #[test]
    fn an_exists_answer_hangs_from_its_target_too() {
        let (cell, _reads) = UndeclaredCell::holding(b"");
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:data:maybe"), cell),
        ));
        let cap = Capability::root();
        let exists = || Request::new(Verb::Exists, iri("urn:data:maybe"));

        assert_eq!(
            block_on(kernel.issue(exists(), &cap)).unwrap().bytes,
            b"false"
        );
        assert!(kernel.is_cached(&exists(), &cap));
        block_on(kernel.issue(sink("urn:data:maybe", b"now"), &cap)).unwrap();
        assert!(
            !kernel.is_cached(&exists(), &cap),
            "the write invalidated the Exists answer"
        );
        assert_eq!(
            block_on(kernel.issue(exists(), &cap)).unwrap().bytes,
            b"true"
        );
    }

    #[test]
    fn a_composite_over_an_undeclared_read_is_cut_by_a_sink_to_the_read() {
        // Why the thread rides the REPRESENTATION and not only the stored entry:
        // the composite inherits it through the dependency record, so a write to
        // the leaf cuts the composite too. An entry-only edge would have closed
        // the hole at the leaf and reopened it one level up.
        let (cell, _reads) = UndeclaredCell::holding(b"hello");
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new()
                .bind(Exact::new("urn:data:leaf"), cell)
                .bind(Exact::new("urn:test:upcase-of"), UpcaseOf),
        ));
        let cap = Capability::root();
        let composite = || {
            Request::new(Verb::Source, iri("urn:test:upcase-of"))
                .with_arg("src", ArgRef::Reference(iri("urn:data:leaf")))
        };
        assert_eq!(
            block_on(kernel.issue(composite(), &cap)).unwrap().bytes,
            b"HELLO"
        );
        assert!(kernel.is_cached(&composite(), &cap));

        block_on(kernel.issue(sink("urn:data:leaf", b"bye"), &cap)).unwrap();
        assert!(
            !kernel.is_cached(&composite(), &cap),
            "the composite hangs from the leaf"
        );
        assert_eq!(
            block_on(kernel.issue(composite(), &cap)).unwrap().bytes,
            b"BYE"
        );
    }

    #[test]
    fn a_pure_function_gains_a_thread_it_never_needs_and_it_is_harmless() {
        // The stated cost of hole A's fix: a pure function of inline arguments
        // (`to_upper`, bound at a fixture name) now hangs from a thread nobody
        // will ever cut in practice. Cutting it is merely a recompute of the same
        // bytes.
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper()),
        ));
        let cap = Capability::root();
        let req = || {
            Request::new(Verb::Source, iri("urn:test:to-upper"))
                .with_arg("in", ArgRef::Inline(b"hi".to_vec()))
        };
        assert_eq!(block_on(kernel.issue(req(), &cap)).unwrap().bytes, b"HI");
        assert!(kernel.is_cached(&req(), &cap));
        kernel.cut("urn:test:to-upper");
        assert!(!kernel.is_cached(&req(), &cap));
        assert_eq!(block_on(kernel.issue(req(), &cap)).unwrap().bytes, b"HI");
    }

    #[test]
    fn meta_gains_no_automatic_thread() {
        // Meta is served from `describe()`, not from the resource's state: a write
        // to the resource does not change its description, so the write-cut must
        // not evict it. What would is a binding change (ledger #510), which has no
        // thread yet — stated, not closed here.
        let kernel = meta_kernel();
        let cap = Capability::root();
        let meta = || Request::new(Verb::Meta, iri("urn:test:to-upper"));
        block_on(kernel.issue(meta(), &cap)).unwrap();
        assert!(kernel.is_cached(&meta(), &cap));
        kernel.cut("urn:test:to-upper");
        assert!(
            kernel.is_cached(&meta(), &cap),
            "a description does not hang from the resource's own thread"
        );
    }

    // --- hole B: a failed sub-request is a dependency ---------------------------

    /// A resource that does not exist until something is sunk into it: `Source`
    /// is `NotFound` while empty, a cacheable read once written.
    struct MaybeCell {
        value: Mutex<Option<Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl Endpoint for MaybeCell {
        async fn invoke(&self, cx: &Invocation<'_>) -> Result<Representation> {
            match cx.request.verb {
                Verb::Source => match self.value.lock().expect("cell").clone() {
                    Some(value) => {
                        Ok(Representation::new(ReprType::new("text/plain"), value).cacheable())
                    }
                    None => Err(Error::NotFound("nothing here yet".to_string())),
                },
                Verb::Sink => {
                    *self.value.lock().expect("cell") = Some(cx.inline_arg("content")?.to_vec());
                    Ok(Representation::new(
                        ReprType::new("text/plain"),
                        b"ok".to_vec(),
                    ))
                }
                other => Err(Error::Endpoint(format!("cell: unsupported {other:?}"))),
            }
        }
    }

    /// A composite that sources `urn:data:optional` and, if that fails with an error
    /// the closure admits, returns a CACHEABLE fallback — the shape hole B is about.
    /// Counts its invocations so a test can tell "served from cache" from "ran".
    fn fallback_over(
        target: &'static str,
        admits: fn(&Error) -> bool,
        runs: Arc<AtomicU32>,
    ) -> crate::endpoint::AsyncFnEndpoint {
        crate::endpoint::AsyncFnEndpoint::new("fallback", move |inv: &Invocation<'_>| {
            let runs = Arc::clone(&runs);
            Box::pin(async move {
                runs.fetch_add(1, Ordering::SeqCst);
                match inv.source(&iri(target)).await {
                    Ok(repr) => Ok(repr),
                    Err(e) if admits(&e) => Ok(Representation::new(
                        ReprType::new("text/plain"),
                        b"fallback".to_vec(),
                    )
                    .cacheable()),
                    Err(e) => Err(e),
                }
            })
        })
    }

    #[test]
    fn a_composite_over_an_unresolved_name_hangs_from_that_name() {
        // Nothing is bound at `urn:data:absent`. The composite swallows the
        // `Unresolved` and caches a fallback — which now hangs from the absent
        // name, so whoever makes it appear (a watcher, a dynamic space) cuts it.
        let runs = Arc::new(AtomicU32::new(0));
        let kernel = Kernel::new(Arc::new(EndpointSpace::new().bind(
            Exact::new("urn:test:fallback"),
            fallback_over(
                "urn:data:absent",
                |e| matches!(e, Error::Unresolved(_)),
                Arc::clone(&runs),
            ),
        )));
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:test:fallback"));
        assert_eq!(
            block_on(kernel.issue(req(), &cap)).unwrap().bytes,
            b"fallback"
        );
        assert_eq!(
            block_on(kernel.issue(req(), &cap)).unwrap().bytes,
            b"fallback"
        );
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the fallback is cached");

        kernel.cut("urn:data:absent");
        assert!(
            !kernel.is_cached(&req(), &cap),
            "the fallback hung from the missing name"
        );
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_composite_over_a_not_found_name_recomputes_when_a_sink_creates_it() {
        // The bound-but-absent case, end to end through the kernel: `NotFound` from
        // the endpoint, a cached fallback, a `Sink` that creates the resource — and
        // the composite recomputes and sees it.
        let runs = Arc::new(AtomicU32::new(0));
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new()
                .bind(
                    Exact::new("urn:data:optional"),
                    MaybeCell {
                        value: Mutex::new(None),
                    },
                )
                .bind(
                    Exact::new("urn:test:fallback"),
                    fallback_over(
                        "urn:data:optional",
                        |e| matches!(e, Error::NotFound(_)),
                        Arc::clone(&runs),
                    ),
                ),
        ));
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:test:fallback"));
        assert_eq!(
            block_on(kernel.issue(req(), &cap)).unwrap().bytes,
            b"fallback"
        );
        assert_eq!(
            block_on(kernel.issue(req(), &cap)).unwrap().bytes,
            b"fallback"
        );
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the fallback is cached");

        block_on(kernel.issue(sink("urn:data:optional", b"created"), &cap)).unwrap();
        assert!(
            !kernel.is_cached(&req(), &cap),
            "the sink cut the fallback's thread"
        );
        assert_eq!(
            block_on(kernel.issue(req(), &cap)).unwrap().bytes,
            b"created"
        );
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_composite_built_on_a_denial_is_not_cached() {
        // A grant change has no thread, so a result built on a refusal must not be
        // cached at all — and it is not, whatever the composite declared. This is
        // the performance change of hole B's fix: every read of such a composite
        // now runs it (measured in the formalism's §10).
        let runs = Arc::new(AtomicU32::new(0));
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new()
                .bind(Exact::new("urn:gated"), Gated(Arc::new(AtomicU32::new(0))))
                .bind(
                    Exact::new("urn:test:fallback"),
                    fallback_over(
                        "urn:gated",
                        |e| matches!(e, Error::Denied(_)),
                        Arc::clone(&runs),
                    ),
                ),
        ));
        let narrow = Capability::root().attenuate(["urn:cap:other".to_string()]);
        let req = || Request::new(Verb::Source, iri("urn:test:fallback"));
        assert_eq!(
            block_on(kernel.issue(req(), &narrow)).unwrap().bytes,
            b"fallback"
        );
        assert_eq!(
            block_on(kernel.issue(req(), &narrow)).unwrap().bytes,
            b"fallback"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            2,
            "a result built on a denial is recomputed"
        );
        assert!(!kernel.is_cached(&req(), &narrow));
        assert_eq!(kernel.cache_len(), 0);
    }

    #[test]
    fn a_composite_built_on_any_other_failure_is_not_cached() {
        // Conservative by design: an `Endpoint` error (a timeout, an outage) names
        // nothing the kernel could hang a thread from, and not caching is never wrong.
        let runs = Arc::new(AtomicU32::new(0));
        let failing = FnEndpoint::new("failing", |_cx: &Invocation<'_>| {
            Err(Error::Endpoint("boom".to_string()))
        });
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new()
                .bind(Exact::new("urn:data:failing"), failing)
                .bind(
                    Exact::new("urn:test:fallback"),
                    fallback_over(
                        "urn:data:failing",
                        |e| matches!(e, Error::Endpoint(_)),
                        Arc::clone(&runs),
                    ),
                ),
        ));
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:test:fallback"));
        block_on(kernel.issue(req(), &cap)).unwrap();
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert!(!kernel.is_cached(&req(), &cap));
    }

    #[test]
    fn fan_out_records_failed_branches_by_the_same_rules() {
        // Two composites over a concurrent fan-out (a real spawner, so the spawned
        // path is the one exercised): one keeps a NotFound branch and hangs from
        // its name; the other keeps a Denied branch and is not cached at all.
        let runs = Arc::new(AtomicU32::new(0));
        let gatherer = |targets: &'static [&'static str], runs: Arc<AtomicU32>| {
            crate::endpoint::AsyncFnEndpoint::new("gather", move |inv: &Invocation<'_>| {
                let runs = Arc::clone(&runs);
                Box::pin(async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    let requests = targets
                        .iter()
                        .map(|t| Request::new(Verb::Source, iri(t)))
                        .collect();
                    let kept: Vec<u8> = inv
                        .fan_out(requests)
                        .await
                        .into_iter()
                        .filter_map(|r| r.ok())
                        .flat_map(|r| r.bytes)
                        .collect();
                    Ok(Representation::new(ReprType::new("text/plain"), kept).cacheable())
                })
            })
        };
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new()
                .bind(Exact::new("urn:data:a"), leaf("a"))
                .bind(
                    Exact::new("urn:data:optional"),
                    MaybeCell {
                        value: Mutex::new(None),
                    },
                )
                .bind(Exact::new("urn:gated"), Gated(Arc::new(AtomicU32::new(0))))
                .bind(
                    Exact::new("urn:test:over-missing"),
                    gatherer(&["urn:data:a", "urn:data:optional"], Arc::clone(&runs)),
                )
                .bind(
                    Exact::new("urn:test:over-denied"),
                    gatherer(&["urn:data:a", "urn:gated"], Arc::clone(&runs)),
                ),
        ))
        .into_scheduled(Arc::new(InlineSpawner));
        let narrow = Capability::root().attenuate(["urn:cap:other".to_string()]);

        let over_missing = || Request::new(Verb::Source, iri("urn:test:over-missing"));
        assert_eq!(
            block_on(kernel.issue(over_missing(), &narrow))
                .unwrap()
                .bytes,
            b"a"
        );
        assert!(
            kernel.is_cached(&over_missing(), &narrow),
            "a NotFound branch still caches"
        );
        block_on(kernel.issue(sink("urn:data:optional", b"b"), &narrow)).unwrap();
        assert!(
            !kernel.is_cached(&over_missing(), &narrow),
            "the fan-out hung from the missing branch's name"
        );
        assert_eq!(
            block_on(kernel.issue(over_missing(), &narrow))
                .unwrap()
                .bytes,
            b"ab"
        );

        let runs_before = runs.load(Ordering::SeqCst);
        let over_denied = || Request::new(Verb::Source, iri("urn:test:over-denied"));
        assert_eq!(
            block_on(kernel.issue(over_denied(), &narrow))
                .unwrap()
                .bytes,
            b"a"
        );
        assert_eq!(
            block_on(kernel.issue(over_denied(), &narrow))
                .unwrap()
                .bytes,
            b"a"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            runs_before + 2,
            "a Denied branch forbids caching"
        );
        assert!(!kernel.is_cached(&over_denied(), &narrow));
    }

    // --- the nesting budget (#513) ---------------------------------------------

    /// An endpoint that sources ITSELF, counting how deep it got.
    fn ouroboros(name: &'static str, runs: Arc<AtomicU32>) -> crate::endpoint::AsyncFnEndpoint {
        crate::endpoint::AsyncFnEndpoint::new("ouroboros", move |inv: &Invocation<'_>| {
            let runs = Arc::clone(&runs);
            Box::pin(async move {
                runs.fetch_add(1, Ordering::SeqCst);
                inv.source(&iri(name)).await
            })
        })
    }

    #[test]
    fn a_self_issuing_endpoint_is_refused_at_the_bound_and_the_trace_says_where() {
        let runs = Arc::new(AtomicU32::new(0));
        let kernel = Kernel::new(Arc::new(EndpointSpace::new().bind(
            Exact::new("urn:self"),
            ouroboros("urn:self", Arc::clone(&runs)),
        )))
        .with_max_depth(3);
        let rec = Rec::new();
        let err = block_on(kernel.issue_traced(
            Request::new(Verb::Source, iri("urn:self")),
            &Capability::root(),
            rec.clone(),
        ))
        .unwrap_err();

        // Depths 0..=3 ran; the request for depth 4 was refused, typed and permanent.
        assert!(
            matches!(&err, Error::DepthExceeded { depth: 4, target } if target.as_str() == "urn:self"),
            "{err:?}"
        );
        assert!(!err.is_transient());
        assert_eq!(runs.load(Ordering::SeqCst), 4);
        assert_eq!(kernel.cache_len(), 0, "a refusal never reaches the store");

        // The trace holds the refusal — the one event, since every invocation above
        // it failed (an endpoint's own failure records nothing: ledger #20) — with
        // the depth note, nothing run, and its parent the invocation that asked for
        // one level too many: spans 0..=3 were the four that ran.
        let events = rec.events();
        assert_eq!(events.len(), 1, "{events:?}");
        let refusal = &events[0];
        assert_eq!(refusal.target, "urn:self");
        assert_eq!(
            refusal.notes,
            vec![(DEPTH_NOTE.to_string(), "4".to_string())]
        );
        assert!(refusal.started.is_none() && refusal.ended.is_none());
        assert!(!refusal.cache_hit);
        assert_eq!(
            refusal.parent,
            Some(3),
            "the deepest invocation that ran asked for it"
        );
    }

    #[test]
    fn a_transclusion_cycle_is_refused_at_the_bound() {
        // Page A transcludes B, B transcludes A: the shape a compose endpoint
        // meets, and until now a stack overflow.
        let runs = Arc::new(AtomicU32::new(0));
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new()
                .bind(
                    Exact::new("urn:page:a"),
                    ouroboros("urn:page:b", Arc::clone(&runs)),
                )
                .bind(
                    Exact::new("urn:page:b"),
                    ouroboros("urn:page:a", Arc::clone(&runs)),
                ),
        ))
        .with_max_depth(5);
        let err = block_on(kernel.issue(
            Request::new(Verb::Source, iri("urn:page:a")),
            &Capability::root(),
        ))
        .unwrap_err();
        // a(0) b(1) a(2) b(3) a(4) b(5) — then a at depth 6 is refused.
        assert!(
            matches!(&err, Error::DepthExceeded { depth: 6, target } if target.as_str() == "urn:page:a"),
            "{err:?}"
        );
        assert_eq!(runs.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn a_chain_shorter_than_the_bound_is_unaffected_and_the_default_is_sixty_four() {
        // c1 sources c2 sources c3: three invocations at depths 0, 1, 2.
        let chain = |from: &'static str, to: Option<&'static str>| {
            crate::endpoint::AsyncFnEndpoint::new(from, move |inv: &Invocation<'_>| {
                Box::pin(async move {
                    let below = match to {
                        Some(next) => inv.source(&iri(next)).await?.bytes,
                        None => Vec::new(),
                    };
                    let mut bytes = inv.depth().to_string().into_bytes();
                    bytes.extend(below);
                    Ok(Representation::new(ReprType::new("text/plain"), bytes).cacheable())
                })
            })
        };
        let space = || {
            Arc::new(
                EndpointSpace::new()
                    .bind(Exact::new("urn:c1"), chain("c1", Some("urn:c2")))
                    .bind(Exact::new("urn:c2"), chain("c2", Some("urn:c3")))
                    .bind(Exact::new("urn:c3"), chain("c3", None)),
            )
        };
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:c1"));

        // A bound the chain fits under: unaffected, and the depths are what they say.
        let fits = Kernel::new(space()).with_max_depth(2);
        assert_eq!(block_on(fits.issue(req(), &cap)).unwrap().bytes, b"012");
        // One short: the deepest hop is refused, and the error names it.
        let tight = Kernel::new(space()).with_max_depth(1);
        let err = block_on(tight.issue(req(), &cap)).unwrap_err();
        assert!(
            matches!(&err, Error::DepthExceeded { depth: 2, target } if target.as_str() == "urn:c3"),
            "{err:?}"
        );
        // The default budget is DEFAULT_MAX_DEPTH: a self-sourcer runs 65 times.
        let runs = Arc::new(AtomicU32::new(0));
        let default = Kernel::new(Arc::new(EndpointSpace::new().bind(
            Exact::new("urn:self"),
            ouroboros("urn:self", Arc::clone(&runs)),
        )));
        let err =
            block_on(default.issue(Request::new(Verb::Source, iri("urn:self")), &cap)).unwrap_err();
        assert!(
            matches!(err, Error::DepthExceeded { depth, .. } if depth == DEFAULT_MAX_DEPTH + 1),
            "{err:?}"
        );
        assert_eq!(runs.load(Ordering::SeqCst), DEFAULT_MAX_DEPTH + 1);
        assert_eq!(DEFAULT_MAX_DEPTH, 64);
    }

    #[test]
    fn fan_out_branches_inherit_the_depth() {
        // A spawned branch is one deeper than the invocation that fanned out —
        // exactly as a sequential `issue` would be — so the budget holds across
        // the spawn: at bound 1 the branches run at depth 1; at bound 0 they are
        // refused, and the parent sees the typed error per branch.
        let depth_reporter = || {
            FnEndpoint::new("depth", |inv: &Invocation<'_>| {
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    inv.depth().to_string().into_bytes(),
                ))
            })
        };
        let spread = || {
            crate::endpoint::AsyncFnEndpoint::new("spread", |inv: &Invocation<'_>| {
                Box::pin(async move {
                    let results = inv
                        .fan_out(vec![
                            Request::new(Verb::Source, iri("urn:depth")),
                            Request::new(Verb::Source, iri("urn:depth")),
                        ])
                        .await;
                    let mut bytes = Vec::new();
                    for result in results {
                        match result {
                            Ok(repr) => bytes.extend(repr.bytes),
                            Err(Error::DepthExceeded { depth, .. }) => {
                                bytes.extend(format!("refused@{depth}").into_bytes())
                            }
                            Err(e) => return Err(e),
                        }
                        bytes.push(b',');
                    }
                    Ok(Representation::new(ReprType::new("text/plain"), bytes))
                })
            })
        };
        let build = |max_depth: u32| {
            Kernel::new(Arc::new(
                EndpointSpace::new()
                    .bind(Exact::new("urn:depth"), depth_reporter())
                    .bind(Exact::new("urn:spread"), spread()),
            ))
            .with_max_depth(max_depth)
            .into_scheduled(Arc::new(InlineSpawner))
        };
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:spread"));
        assert_eq!(
            block_on(build(1).issue(req(), &cap)).unwrap().bytes,
            b"1,1,"
        );
        assert_eq!(
            block_on(build(0).issue(req(), &cap)).unwrap().bytes,
            b"refused@1,refused@1,"
        );
    }

    #[test]
    fn a_depth_refusal_swallowed_into_a_fallback_is_not_cached() {
        // B.7's caching obligation, met through hole B rather than trivially: the
        // refusal itself never reaches the store, and a composite that catches it
        // and returns `.cacheable()` is forced uncacheable — so a shallower request
        // is never served a result that was computed at the bound.
        let runs = Arc::new(AtomicU32::new(0));
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new()
                .bind(Exact::new("urn:data:leaf"), leaf("x"))
                .bind(
                    Exact::new("urn:test:fallback"),
                    fallback_over(
                        "urn:data:leaf",
                        |e| matches!(e, Error::DepthExceeded { .. }),
                        Arc::clone(&runs),
                    ),
                ),
        ))
        .with_max_depth(0);
        let cap = Capability::root();
        let req = || Request::new(Verb::Source, iri("urn:test:fallback"));
        assert_eq!(
            block_on(kernel.issue(req(), &cap)).unwrap().bytes,
            b"fallback"
        );
        assert_eq!(
            block_on(kernel.issue(req(), &cap)).unwrap().bytes,
            b"fallback"
        );
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert_eq!(kernel.cache_len(), 0);
    }

    // ---- urn:kernel:bindings: the binding-change thread, and the floor memo ----
    use crate::grammar::Bindings;
    use crate::space::Resolved;

    /// A root whose bindings can change under a running kernel — the shape a
    /// discovery mount table or a module host has, which none of core's own spaces
    /// do. The kernel cannot see the swap; only the caller of `rebind` can say so.
    struct Swappable(RwLock<EndpointSpace>);
    impl Swappable {
        fn over(space: EndpointSpace) -> Arc<Self> {
            Arc::new(Swappable(RwLock::new(space)))
        }
        fn rebind(&self, space: EndpointSpace) {
            *self.0.write().expect("swappable lock") = space;
        }
    }
    impl Space for Swappable {
        fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
            self.0
                .read()
                .expect("swappable lock")
                .resolve(request, scope)
        }
        fn entries(&self) -> Option<Vec<SpaceEntry>> {
            self.0.read().expect("swappable lock").entries()
        }
    }

    /// A second endpoint to bind under `urn:test:to-upper`, distinguishable from
    /// `toUpper` by id in every face.
    fn shout() -> FnEndpoint {
        FnEndpoint::new("shout", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), b"!".to_vec()).cacheable())
        })
        .with_description(Description::new("shout").verb(Verb::Source))
    }

    #[test]
    fn a_binding_change_recomputes_every_self_description_face() {
        let root = Swappable::over(
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper()),
        );
        let kernel = Kernel::with_meta_renderer(root.clone(), Arc::new(EchoIdRenderer));
        let cap = Capability::root();
        let text = |req: Request| -> Result<String> {
            block_on(kernel.issue(req, &cap)).map(|r| String::from_utf8(r.bytes).unwrap())
        };
        let catalog = || Request::new(Verb::Source, iri("urn:kernel:catalog"));
        let actions = || Request::new(Verb::Source, iri("urn:kernel:actions"));
        let meta = || Request::new(Verb::Meta, iri("urn:test:to-upper"));
        let validate = || {
            Request::new(Verb::Source, iri("urn:kernel:validate"))
                .with_arg(
                    "action",
                    ArgRef::Inline(b"urn:ikigai:endpoint:toUpper:action:source".to_vec()),
                )
                .with_arg("args", ArgRef::Inline(b"in=x".to_vec()))
        };

        // All four faces describe the first root, and all four are cached.
        assert!(text(catalog()).unwrap().contains("toUpper"));
        assert!(text(actions()).unwrap().contains("urn:test:to-upper"));
        assert_eq!(text(meta()).unwrap(), "toUpper");
        let report = text(validate()).unwrap();
        assert!(report.contains("sh:conforms true"), "{report}");
        for req in [catalog(), actions(), meta(), validate()] {
            assert!(
                kernel.is_cached(&req, &cap),
                "{} should be cached",
                req.target
            );
        }

        // The root changes under the kernel: the same name binds a different
        // endpoint, a new name appears, the old id is gone. The kernel cannot see
        // it — every face is still served from cache — which is exactly why the
        // party that changed the root has to say so.
        root.rebind(
            EndpointSpace::new()
                .bind(Exact::new("urn:test:to-upper"), shout())
                .bind(Exact::new("urn:test:echo"), builtins::echo()),
        );
        assert_eq!(
            text(meta()).unwrap(),
            "toUpper",
            "stale until the thread is cut"
        );
        assert!(kernel.is_cached(&catalog(), &cap));

        kernel.bindings_changed();

        // Every face is invalidated, recomputes, and SHOWS the new binding.
        for req in [catalog(), actions(), meta(), validate()] {
            assert!(
                !kernel.is_cached(&req, &cap),
                "{} should be invalidated",
                req.target
            );
        }
        let body = text(catalog()).unwrap();
        assert!(body.contains("shout") && body.contains("echo"), "{body}");
        assert!(!body.contains("toUpper"), "{body}");
        assert!(text(actions()).unwrap().contains("urn:test:echo"));
        assert_eq!(text(meta()).unwrap(), "shout");
        let err = text(validate()).unwrap_err();
        assert!(
            err.to_string().contains("no endpoint with id `toUpper`"),
            "{err}"
        );
    }

    #[test]
    fn a_transrepted_meta_face_hangs_from_the_bindings_thread_too() {
        // The canonical rendering is one face; a transrepted one is produced by a
        // sub-issue through a selected transreptor and re-marked cacheable. Both
        // are descriptions of a binding, so both go when the binding set changes.
        let kernel = meta_kernel();
        let cap = Capability::root();
        let rep = meta_as(&kernel, "urn:test:to-upper", "application/rdf+xml");
        assert!(rep.threads().contains(&Thread::from(BINDINGS_THREAD)));
        assert!(String::from_utf8(rep.bytes).unwrap().starts_with("RDFXML("));
        let req = || {
            Request::new(Verb::Meta, iri("urn:test:to-upper"))
                .with_arg("as", ArgRef::Inline(b"application/rdf+xml".to_vec()))
        };
        assert!(kernel.is_cached(&req(), &cap));
        kernel.bindings_changed();
        assert!(!kernel.is_cached(&req(), &cap));
    }

    #[test]
    fn the_resource_form_cuts_the_bindings_thread_and_the_floor_memo() {
        // `sink urn:kernel:cut urn:kernel:bindings` — the form for an endpoint or a
        // peer — is the same cut as `bindings_changed()`: the faces AND the memo go.
        let root = Swappable::over(
            EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), builtins::to_upper()),
        );
        let kernel = Kernel::with_meta_renderer(root.clone(), Arc::new(EchoIdRenderer));
        let cap = Capability::root();
        let catalog = || Request::new(Verb::Source, iri("urn:kernel:catalog"));
        block_on(kernel.issue(catalog(), &cap)).unwrap();
        let upper = Request::new(Verb::Source, iri("urn:test:to-upper"))
            .with_arg("in", ArgRef::Inline(b"x".to_vec()));
        block_on(kernel.issue(upper, &cap)).unwrap();
        assert_eq!(kernel.floors.len(), 1, "to-upper's floor is memoized");

        root.rebind(EndpointSpace::new().bind(Exact::new("urn:test:to-upper"), shout()));
        block_on(kernel.issue(cut_request(BINDINGS_THREAD), &cap)).unwrap();

        assert!(!kernel.is_cached(&catalog(), &cap));
        assert_eq!(kernel.floors.len(), 0, "the memo goes with the thread");
        let body =
            String::from_utf8(block_on(kernel.issue(catalog(), &cap)).unwrap().bytes).unwrap();
        assert!(body.contains("shout"), "{body}");
    }

    #[test]
    fn an_ordinary_sink_does_not_cut_the_bindings_thread() {
        // A write changes STATE, not the binding set: the auto-cut fires on the
        // target's own thread, and the catalog and the memo stand.
        let cell = FnEndpoint::new("cell", |_inv| {
            Ok(Representation::new(ReprType::new("text/plain"), b"ok".to_vec()).cacheable())
        })
        .with_description(Description::new("cell").verb(Verb::Source).verb(Verb::Sink));
        let space = EndpointSpace::new().bind(Exact::new("urn:test:cell"), cell);
        let kernel = Kernel::with_meta_renderer(Arc::new(space), Arc::new(EchoIdRenderer));
        let cap = Capability::root();
        let catalog = || Request::new(Verb::Source, iri("urn:kernel:catalog"));
        let read = || Request::new(Verb::Source, iri("urn:test:cell"));
        block_on(kernel.issue(catalog(), &cap)).unwrap();
        block_on(kernel.issue(read(), &cap)).unwrap();
        assert!(kernel.is_cached(&read(), &cap));
        assert_eq!(kernel.floors.len(), 1);

        let write = Request::new(Verb::Sink, iri("urn:test:cell"))
            .with_arg("content", ArgRef::Inline(b"v".to_vec()));
        block_on(kernel.issue(write, &cap)).unwrap();

        assert!(!kernel.is_cached(&read(), &cap), "the auto-cut still fires");
        assert!(
            kernel.is_cached(&catalog(), &cap),
            "a write changes state, not the binding set"
        );
        assert_eq!(kernel.floors.len(), 1, "the memo is untouched by a write");
    }

    /// Counts its own `describe()` calls — the per-request cost ledger #22 is
    /// about — and declares a floor, or not, as the test needs.
    struct Described {
        describes: Arc<AtomicU32>,
        requires: Option<&'static str>,
    }
    #[async_trait::async_trait]
    impl Endpoint for Described {
        async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
            Ok(Representation::new(ReprType::new("text/plain"), b"d".to_vec()).cacheable())
        }
        fn name(&self) -> &str {
            "described"
        }
        fn describe(&self) -> Description {
            self.describes.fetch_add(1, Ordering::SeqCst);
            let mut spec = crate::describe::ActionSpec::new(Verb::Source);
            if let Some(scope) = self.requires {
                spec = spec.requires(scope);
            }
            Description::new("described").action(spec)
        }
    }
    fn described(requires: Option<&'static str>) -> (Described, Arc<AtomicU32>) {
        let describes = Arc::new(AtomicU32::new(0));
        (
            Described {
                describes: describes.clone(),
                requires,
            },
            describes,
        )
    }

    #[test]
    fn the_floor_describes_an_endpoint_once_until_the_bindings_change() {
        let (endpoint, describes) = described(Some("urn:cap:demo:read"));
        let space = EndpointSpace::new().bind(Exact::new("urn:test:d"), endpoint);
        let kernel = Kernel::new(Arc::new(space));
        let holder = Capability::scoped(["urn:cap:demo:read"]);
        let other = Capability::scoped(["urn:cap:other"]);
        let req = || Request::new(Verb::Source, iri("urn:test:d"));
        for _ in 0..5 {
            block_on(kernel.issue(req(), &holder)).unwrap();
        }
        // A different capability is a different cache key but the same endpoint:
        // the memoized floor refuses it without describing again — the memo is
        // keyed by the endpoint, the cache by the authority, and neither leaks.
        let err = block_on(kernel.issue(req(), &other)).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert_eq!(
            describes.load(Ordering::SeqCst),
            1,
            "described once, floored six times"
        );
        kernel.bindings_changed();
        block_on(kernel.issue(req(), &holder)).unwrap();
        assert_eq!(
            describes.load(Ordering::SeqCst),
            2,
            "a binding change re-describes"
        );
    }

    /// An overlay that answers with a STAND-IN endpoint while a flag is set — the
    /// shape of throttle's over-budget substitute, whose contract is not the real
    /// endpoint's (ikigai-throttle #14 was the stand-in's description leaking the
    /// real one's absence of a floor).
    struct Switch {
        real: Arc<dyn Endpoint>,
        stand_in: Arc<dyn Endpoint>,
        over: AtomicBool,
    }
    impl Space for Switch {
        fn resolve(&self, request: &Request, _scope: &Scope) -> Resolution {
            if request.target.as_str() != "urn:test:limited" {
                return Resolution::Miss;
            }
            let endpoint = if self.over.load(Ordering::SeqCst) {
                &self.stand_in
            } else {
                &self.real
            };
            Resolution::Hit(Resolved::new(Arc::clone(endpoint), Bindings::default()))
        }
    }

    #[test]
    fn a_swapped_endpoint_is_floored_on_its_own_contract() {
        let (real, _) = described(None);
        let (stand_in, _) = described(Some("urn:cap:limited:admin"));
        let switch = Arc::new(Switch {
            real: Arc::new(real),
            stand_in: Arc::new(stand_in),
            over: AtomicBool::new(false),
        });
        let kernel = Kernel::new(switch.clone());
        let cap = Capability::scoped(["urn:cap:something"]);
        let req = || Request::new(Verb::Source, iri("urn:test:limited"));

        // The real endpoint is public: admitted, cached, and its floor memoized.
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert!(kernel.is_cached(&req(), &cap));
        assert_eq!(kernel.floors.len(), 1);

        // Over budget: the SAME request resolves to the stand-in, which declares a
        // scope this caller lacks. Identity is the endpoint, not the name, so the
        // stand-in gets its own entry and the memoized public floor is not applied
        // to it — and the cached representation, which the floor guards, is not
        // served around it either.
        switch.over.store(true, Ordering::SeqCst);
        let err = block_on(kernel.issue(req(), &cap)).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert_eq!(
            kernel.floors.len(),
            2,
            "one entry per endpoint, not per name"
        );

        // Back under budget: the real endpoint's entry still stands, still public.
        switch.over.store(false, Ordering::SeqCst);
        block_on(kernel.issue(req(), &cap)).unwrap();
        assert_eq!(kernel.floors.len(), 2);
    }

    /// A wrapper endpoint that delegates its contract, over a space that wraps
    /// the resolved endpoint in a FRESH `Arc` on every resolution — the shape of
    /// every ikigai-throttle governor today.
    struct Wrapped(Arc<dyn Endpoint>);
    #[async_trait::async_trait]
    impl Endpoint for Wrapped {
        async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
            self.0.invoke(inv).await
        }
        fn describe(&self) -> Description {
            self.0.describe()
        }
    }
    struct WrapEveryTime(EndpointSpace);
    impl Space for WrapEveryTime {
        fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
            self.0
                .resolve(request, scope)
                .map_endpoint(|inner| Arc::new(Wrapped(inner)) as Arc<dyn Endpoint>)
        }
    }

    #[test]
    fn a_per_resolution_wrapper_is_bounded_and_floored_correctly() {
        let (open, _) = described(None);
        let (gated, _) = described(Some("urn:cap:demo:read"));
        let inner = EndpointSpace::new()
            .bind(Exact::new("urn:test:open"), open)
            .bind(Exact::new("urn:test:gated"), gated);
        let kernel = Kernel::new(Arc::new(WrapEveryTime(inner)));
        let cap = Capability::scoped(["urn:cap:other"]);
        // Alternate two contracts through freshly allocated wrappers, each dropped
        // before the next is allocated — the pattern under which an allocator hands
        // the same address back. The memo's weak reservation is what keeps a gated
        // wrapper from ever being served an open one's floor; past the bound the
        // dead entries are swept and the addresses become reusable, legitimately.
        for i in 0..(FLOOR_MEMO_BOUND + 64) {
            let open =
                block_on(kernel.issue(Request::new(Verb::Source, iri("urn:test:open")), &cap));
            assert!(open.is_ok(), "iteration {i}: open is public");
            let gated =
                block_on(kernel.issue(Request::new(Verb::Source, iri("urn:test:gated")), &cap));
            assert!(
                matches!(gated, Err(Error::Denied(_))),
                "iteration {i}: gated stays gated: {gated:?}"
            );
        }
        assert!(
            kernel.floors.len() <= FLOOR_MEMO_BOUND,
            "the memo is bounded: {}",
            kernel.floors.len()
        );
    }
}
