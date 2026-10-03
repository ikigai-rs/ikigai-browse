//! The representation cache: what the kernel keeps, how long it stays valid, and
//! what it is willing to forget.
//!
//! Three concerns live here, and keeping them apart is the whole design:
//!
//! 1. **Validity** — golden-thread edges ([`Thread`] generations) and the time
//!    deadline on [`Expiry::At`]. This decides whether an entry is *correct* to
//!    serve, and nothing else in this module may override it.
//! 2. **Admission and eviction** — [`CachePolicy`]. This decides whether an entry is
//!    *worth keeping*. A policy can never make the kernel serve something stale: it
//!    is consulted only on the way in and on the way out, never on the way back.
//! 3. **Bounds** — [`CacheBound`]. Before this module the cache was two bare
//!    `HashMap`s with no eviction anywhere: entries left only when a lookup happened
//!    to find them invalid, so a long-lived process driven by ungated cacheable
//!    operations grew without limit. Both maps are bounded now.
//!
//! # The lost-cut race
//!
//! A cache entry pins each of its golden threads to the generation that thread held
//! *when the entry was stored*. Reading those generations after the invocation
//! returns is wrong, and silently so:
//!
//! - t0 — a read begins; the endpoint observes state S.
//! - t1 — a `Sink` cuts thread T, bumping its generation 5 → 6.
//! - t2 — the read returns a representation built from S, which is now stale.
//! - t3 — the store pins T at **6**, and the stale representation is filed as valid
//!   against the very cut that should have invalidated it.
//!
//! The entry then stays "fresh" until some future, unrelated cut. [`ReprCache`]
//! closes this with a **cut sequence**: every cut takes the next number from one
//! monotonic counter and is recorded in a bounded log. A request takes a
//! [`CutSnapshot`] before it invokes anything, and [`ReprCache::store`] declines the
//! entry if any thread it depends on was cut after that snapshot. Declining is
//! always safe — not caching is never wrong — so this never tries to reconcile.
//!
//! Two properties of that choice are worth stating, because both were deliberate:
//!
//! - It is **per-thread, not global**. A global "did anything get cut?" check would
//!   be simpler and far more destructive: a one-second graph build on a busy host
//!   would decline to cache because some unrelated resource was written during it,
//!   turning a correctness fix into the kind of ~2000× read regression that
//!   propagation bugs cause. Only a cut to a thread *this result depends on*
//!   declines the store.
//! - It is **conservative when it cannot tell**. The cut log is bounded
//!   ([`CUT_LOG`]); if a snapshot is older than the oldest cut still retained, cuts
//!   in between may have been forgotten, and the store is declined rather than
//!   guessed at.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::repr::{Expiry, Representation, Thread, Time};
use crate::request::RequestId;
use crate::uncached::Declined;

/// How many recent cuts the race check remembers. A store whose snapshot predates
/// the oldest retained cut is declined (see the module docs): the bound costs
/// missed caching under an implausible burst, never correctness.
pub const CUT_LOG: usize = 4096;

/// A cached representation's key: the content-addressed request, the
/// fingerprint of the authority that computed it, **and** the fingerprint of the
/// resolution chain it was computed in.
///
/// The capability half is not decoration. A cache hit is served *before* the
/// endpoint runs, so an entry keyed only by request id would let one authority read
/// another's cached result, skipping the capability check entirely.
///
/// The scope half is the same argument for the chain: a request resolved inside a
/// confined chain, or with a corridor injected ahead of the root, can resolve one
/// name to a *different endpoint* than the same request in the plain chain. A cached
/// representation may be shared across that boundary only when the corridors
/// consulted are the same on both sides; with the whole chain in the key that holds
/// by construction, over-partitioned rather than unsound. The empty chain's
/// fingerprint is `0`, so [`new`](Self::new) — which does not take one — is the empty
/// chain's key, and every key built before scopes existed is unchanged.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct CacheKey {
    /// The content-addressed identity of the request that produced the entry.
    pub request: RequestId,
    /// A stable fingerprint of the capability the request was issued under.
    pub capability: u64,
    /// A stable fingerprint of the resolution chain the request was resolved in
    /// ([`Scope::fingerprint`](crate::Scope::fingerprint)); `0` for the empty chain.
    #[serde(default)]
    pub scope: u64,
}

impl CacheKey {
    /// A key over a request id and a capability fingerprint, in the empty
    /// resolution chain (scope fingerprint `0`).
    pub fn new(request: RequestId, capability: u64) -> Self {
        CacheKey {
            request,
            capability,
            scope: 0,
        }
    }

    /// The same key in the resolution chain `scope` fingerprints (builder).
    pub fn in_scope(mut self, scope: u64) -> Self {
        self.scope = scope;
        self
    }
}

/// A cached representation plus the golden-thread edges that keep it valid: each
/// `(thread, generation)` records the generation that thread held when the entry was
/// stored. The entry is valid only while every thread is still at that generation —
/// cut any of them and it is stale.
///
/// `Serialize`/`Deserialize` are derived because everything inside already is, and
/// keeping the door open for an ejected cache costs nothing here. **Serializing one
/// of these is not the hard part of ejection, and this derive is not permission to
/// import one** — `generation` is a per-process counter and means nothing in another
/// instance. See `docs/design/cache-ejection.md`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheEntry {
    /// The IRI of the request that produced this entry — kept so `urn:kernel:cache`
    /// can name what is cached (the cache is otherwise keyed by a content hash).
    pub target: String,
    /// The cached representation itself.
    pub representation: Representation,
    /// The golden threads this entry depends on, each pinned to a generation.
    pub edges: Vec<(Thread, u64)>,
    /// What it cost to produce, in milliseconds, when the kernel had a clock to
    /// measure with. `None` on a clock-free kernel — a policy that reads it must
    /// degrade rather than assume.
    pub cost_millis: Option<u64>,
    /// How many times this entry has been served since it was stored.
    pub hits: u64,
    /// The cache tick at which it was stored (insertion order — FIFO reads this).
    pub stored_tick: u64,
    /// The cache tick at which it was last served (recency — LRU reads this).
    pub last_used_tick: u64,
}

impl CacheEntry {
    /// The entry's size in bytes: the representation's payload. Metadata overhead is
    /// deliberately not counted — the bound is about what the cache is *holding*,
    /// and a policy comparing entries wants the number it can also reason about.
    pub fn bytes(&self) -> usize {
        self.representation.bytes.len()
    }

    fn facts(&self) -> EntryFacts<'_> {
        EntryFacts {
            target: &self.target,
            media_type: &self.representation.repr_type.media_type,
            bytes: self.bytes(),
            cost_millis: self.cost_millis,
            hits: self.hits,
            threads: self.edges.len(),
            stored_tick: self.stored_tick,
            last_used_tick: self.last_used_tick,
        }
    }
}

/// What a [`CachePolicy`] may see about one entry — resident or candidate.
///
/// This is the smallest set that supports FIFO ([`Fifo`], on `stored_tick`), LRU
/// ([`Lru`], on `last_used_tick`) and a cost-versus-size heuristic ([`CostAware`], on
/// `cost_millis` and `bytes`), which is the bar the design set. It is a struct rather
/// than a widening argument list so adding a fact later is not a signature change for
/// every implementor.
///
/// Deliberately absent: the representation's bytes, the capability fingerprint, and
/// the request id. A policy decides what is worth keeping; it has no business reading
/// the payload, and nothing it returns may depend on *whose* entry it is.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct EntryFacts<'a> {
    /// The IRI the entry was resolved from.
    pub target: &'a str,
    /// The representation's media type.
    pub media_type: &'a str,
    /// The entry's payload size in bytes.
    pub bytes: usize,
    /// What it cost to produce, in milliseconds — `None` on a clock-free kernel.
    pub cost_millis: Option<u64>,
    /// How many times it has been served since being stored (0 for a candidate).
    pub hits: u64,
    /// How many golden threads it depends on.
    pub threads: usize,
    /// The cache tick at which it was stored: a monotonic insertion order.
    pub stored_tick: u64,
    /// The cache tick at which it was last served, or stored if never served.
    pub last_used_tick: u64,
}

/// One resident entry as `urn:kernel:cache` reports it — a human face
/// (ledger #59), not a wire format; `#[non_exhaustive]` so a later fact is not a
/// break for a reader.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CacheRow {
    /// The IRI the entry was resolved from.
    pub target: String,
    /// The representation's media type.
    pub media_type: String,
    /// The entry's payload size in bytes.
    pub bytes: usize,
    /// How many golden threads it depends on.
    pub threads: usize,
    /// The fingerprint of the resolution chain it was computed in
    /// ([`Scope::fingerprint`](crate::Scope::fingerprint)); `0` for the empty chain.
    pub scope: u64,
    /// Whether the entry would be served right now — the same predicate serving and
    /// [`ReprCache::probe`] use (ledger #612).
    pub state: EntryState,
}

/// Whether a resident entry would be served, and if not, why not.
///
/// A cut is LAZY: it bumps a generation and leaves every entry pinned to the old
/// one resident until a read finds it stale and evicts it. So "what the cache holds"
/// and "what the cache would serve" differ right after every edit, and a readout that
/// listed rows without saying which was which read like the edit had invalidated
/// nothing (ledger #612). Stale rows are still worth listing — they occupy the bound
/// until something reads them — so the readout marks them rather than hiding them.
///
/// ```
/// use ikigai_core::EntryState;
///
/// assert_eq!(EntryState::Live.word(), "live");
/// assert_eq!(EntryState::Cut.word(), "cut");
/// assert_eq!(EntryState::Expired.word(), "expired");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EntryState {
    /// Every golden thread still stands at its pinned generation and any deadline is
    /// still ahead: a read is served from this entry.
    Live,
    /// A thread it depends on was cut since it was stored: the next read evicts it
    /// and recomputes.
    Cut,
    /// Its threads stand but its deadline has passed (or the kernel has no clock to
    /// say it has not): the next read evicts it and recomputes.
    Expired,
}

impl EntryState {
    /// The word `urn:kernel:cache` prints for the state: `live`, `cut` or `expired`.
    pub fn word(self) -> &'static str {
        match self {
            EntryState::Live => "live",
            EntryState::Cut => "cut",
            EntryState::Expired => "expired",
        }
    }

    /// Whether a read would be served from the entry.
    pub fn is_live(self) -> bool {
        self == EntryState::Live
    }
}

/// The ceiling a [`CachePolicy`] enforces: a count and a byte budget, both hard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheBound {
    /// The most entries the cache may hold.
    pub max_entries: usize,
    /// The most bytes of cached representations it may hold in total.
    pub max_bytes: usize,
}

impl CacheBound {
    /// A bound of `max_entries` entries and `max_bytes` bytes.
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        CacheBound {
            max_entries,
            max_bytes,
        }
    }
}

impl Default for CacheBound {
    /// The default bound: **4096 entries and 64 MiB**.
    ///
    /// It is a real number rather than "unbounded, configure it yourself" because
    /// the process that most needs a bound is the edge — the one facing strangers —
    /// and it is exactly the process nobody remembers to configure. The two halves
    /// do different jobs: the byte budget is the memory ceiling, and the entry count
    /// guards against a flood of tiny entries whose per-entry overhead (key, target
    /// IRI, thread names) the byte budget does not see.
    ///
    /// 64 MiB is chosen to sit *above* the working set of the live services
    /// (derived graphs measured in single-digit MiB) and well below anything that
    /// threatens a small VM or a browser tab. A host that knows better should say
    /// so: `Kernel::with_cache_policy(Arc::new(Lru::with_bound(CacheBound::new(256,
    /// 8 << 20))))`.
    fn default() -> Self {
        CacheBound::new(4096, 64 << 20)
    }
}

/// Admission and eviction — never validity.
///
/// The kernel consults a policy in exactly two places: when it is about to store an
/// entry ([`admit`](CachePolicy::admit)), and when the cache is over its
/// [`capacity`](CachePolicy::capacity) and something must go
/// ([`victim`](CachePolicy::victim)). It is never consulted when serving, so no
/// policy — including a hostile one — can make the kernel hand back a representation
/// whose golden thread was cut or whose deadline has passed.
///
/// Implement it to trade differently: keep what was expensive, evict what is large,
/// bias toward what an operator knows is hot. [`Lru`] is the default, [`Fifo`] and
/// [`CostAware`] ship beside it.
pub trait CachePolicy: Send + Sync {
    /// The ceiling this policy enforces. Read on every store, so keep it cheap.
    fn capacity(&self) -> CacheBound {
        CacheBound::default()
    }

    /// Whether to store a candidate at all. The default refuses a single entry
    /// larger than the whole byte budget — admitting it would evict everything else
    /// and still leave the cache over its bound.
    fn admit(&self, candidate: &EntryFacts<'_>) -> bool {
        candidate.bytes <= self.capacity().max_bytes
    }

    /// Pick the resident entry to evict, as an index into `resident`. `None` leaves
    /// the cache over its bound, which the kernel accepts as the policy's decision —
    /// so a policy that wants an unbounded cache says so by returning `None` here,
    /// visibly, rather than by omission.
    fn victim(&self, resident: &[EntryFacts<'_>]) -> Option<usize>;
}

/// Evict the least recently served entry. **The default policy.**
///
/// Recency is the right default for a resolution cache because resolution is bursty
/// and composite: a page resolves the same sub-resources repeatedly within a request
/// and then moves on.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lru(CacheBound);

impl Lru {
    /// An LRU policy with a non-default bound.
    pub fn with_bound(bound: CacheBound) -> Self {
        Lru(bound)
    }
}

impl CachePolicy for Lru {
    fn capacity(&self) -> CacheBound {
        self.0
    }

    fn victim(&self, resident: &[EntryFacts<'_>]) -> Option<usize> {
        argmin(resident, |facts| facts.last_used_tick)
    }
}

/// Evict the oldest entry, however recently it was served.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fifo(CacheBound);

impl Fifo {
    /// A FIFO policy with a non-default bound.
    pub fn with_bound(bound: CacheBound) -> Self {
        Fifo(bound)
    }
}

impl CachePolicy for Fifo {
    fn capacity(&self) -> CacheBound {
        self.0
    }

    fn victim(&self, resident: &[EntryFacts<'_>]) -> Option<usize> {
        argmin(resident, |facts| facts.stored_tick)
    }
}

/// Evict the entry with the least value per byte: what it cost to recompute, times
/// how often it has been wanted, divided by what it occupies.
///
/// ⚠ Cost is measured with the kernel's [`Clock`](crate::Clock). A kernel with no
/// clock measures nothing, and this policy degrades to [`Lru`] rather than treating
/// every entry as free — an unmeasured cost is not a zero cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct CostAware(CacheBound);

impl CostAware {
    /// A cost-versus-size policy with a non-default bound.
    pub fn with_bound(bound: CacheBound) -> Self {
        CostAware(bound)
    }
}

impl CachePolicy for CostAware {
    fn capacity(&self) -> CacheBound {
        self.0
    }

    fn victim(&self, resident: &[EntryFacts<'_>]) -> Option<usize> {
        // No entry carries a measured cost ⇒ nothing to weigh; fall back to recency.
        if resident.iter().all(|facts| facts.cost_millis.is_none()) {
            return argmin(resident, |facts| facts.last_used_tick);
        }
        // Integer arithmetic, scaled: a millisecond of compute per KiB held, times
        // one plus the hit count. Ties (including every unmeasured entry, which
        // scores zero) fall back to recency via the tick in the tuple.
        argmin(resident, |facts| {
            let cost = facts.cost_millis.unwrap_or(0);
            let density = cost
                .saturating_mul(1024)
                .saturating_mul(facts.hits.saturating_add(1))
                / (facts.bytes.max(1) as u64);
            (density, facts.last_used_tick)
        })
    }
}

/// The index of the smallest score, or `None` for an empty slice. Ties take the
/// first, which is stable given the unique ticks every score carries.
fn argmin<T: Ord>(
    resident: &[EntryFacts<'_>],
    score: impl Fn(&EntryFacts<'_>) -> T,
) -> Option<usize> {
    resident
        .iter()
        .enumerate()
        .min_by_key(|(_, facts)| score(facts))
        .map(|(index, _)| index)
}

/// A point in the kernel's cut history, taken **before** a request invokes anything.
/// [`ReprCache::store`] compares it against the cuts that have happened since; see
/// the module docs for why the comparison has to run in that direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CutSnapshot(u64);

/// The kernel's representation cache: valid entries, thread generations, and the
/// bounded cut log that makes the race check possible.
pub struct ReprCache {
    state: Mutex<CacheState>,
    policy: Arc<dyn CachePolicy>,
    /// The cut counter, mirrored out of the lock so [`snapshot`](Self::snapshot) —
    /// taken on every request — costs one atomic load.
    ///
    /// Ordering: the counter is only ever advanced while the state lock is held, and
    /// published with `Release` *after* the cut is recorded. A snapshot loading with
    /// `Acquire` therefore either sees the new value (and the recorded cut with it)
    /// or the old one — in which case the cut's sequence is greater than the
    /// snapshot and the race check catches it anyway. Both readings are safe.
    cut_seq: AtomicU64,
}

struct CacheState {
    entries: HashMap<CacheKey, CacheEntry>,
    /// Current generation of each golden thread (absent ⇒ generation 0).
    generations: HashMap<Thread, u64>,
    /// The most recent cuts, oldest first, as `(thread, sequence)`.
    cuts: VecDeque<(Thread, u64)>,
    /// The highest cut sequence no longer retained in `cuts`. A snapshot older than
    /// this cannot be checked, so a store against it is declined.
    horizon: u64,
    /// The cut counter (the authoritative copy; `ReprCache::cut_seq` mirrors it).
    seq: u64,
    /// A monotonic tick advanced on every store and every hit, giving the policy an
    /// insertion order and a recency order without a clock.
    tick: u64,
    /// Running total of `entries`' payload bytes.
    bytes: usize,
}

impl Default for ReprCache {
    fn default() -> Self {
        ReprCache::new(Arc::new(Lru::default()))
    }
}

impl ReprCache {
    /// An empty cache under `policy`.
    pub fn new(policy: Arc<dyn CachePolicy>) -> Self {
        ReprCache {
            state: Mutex::new(CacheState {
                entries: HashMap::new(),
                generations: HashMap::new(),
                cuts: VecDeque::new(),
                horizon: 0,
                seq: 0,
                tick: 0,
                bytes: 0,
            }),
            policy,
            cut_seq: AtomicU64::new(0),
        }
    }

    /// The cut history as of now. Take this **before** invoking, and hand it back to
    /// [`store`](Self::store).
    pub fn snapshot(&self) -> CutSnapshot {
        CutSnapshot(self.cut_seq.load(Ordering::Acquire))
    }

    /// Serve `key` if a valid entry exists, evicting it if it has gone stale.
    /// `now` is the kernel's clock reading, or `None` on a clock-free kernel (which
    /// conservatively treats any deadline as passed).
    pub fn get(&self, key: &CacheKey, now: Option<Time>) -> Option<Representation> {
        let mut state = self.state.lock().expect("cache lock");
        match state.entries.get(key) {
            None => None,
            Some(entry) if !state.is_valid(entry, now) => {
                if let Some(evicted) = state.entries.remove(key) {
                    state.bytes = state.bytes.saturating_sub(evicted.bytes());
                }
                None
            }
            Some(_) => {
                state.tick += 1;
                let tick = state.tick;
                let entry = state.entries.get_mut(key).expect("entry present");
                entry.hits += 1;
                entry.last_used_tick = tick;
                Some(entry.representation.clone())
            }
        }
    }

    /// Whether `key` would be served right now — read-only: it neither evicts a
    /// stale entry nor counts as a hit, so a probe cannot perturb what a policy
    /// sees.
    pub fn probe(&self, key: &CacheKey, now: Option<Time>) -> bool {
        let state = self.state.lock().expect("cache lock");
        state
            .entries
            .get(key)
            .is_some_and(|entry| state.is_valid(entry, now))
    }

    /// Store a computed representation, unless a thread it depends on was cut since
    /// `taken` (the lost-cut race) or the policy declines to admit it. Returns
    /// whether it was stored — `false` is always a safe outcome.
    pub fn store(
        &self,
        key: CacheKey,
        target: String,
        representation: Representation,
        taken: CutSnapshot,
        cost_millis: Option<u64>,
    ) -> bool {
        self.store_outcome(key, target, representation, taken, cost_millis)
            .is_ok()
    }

    /// [`store`](Self::store), saying WHICH of the two declines it was — the
    /// kernel's answer to "why was this read not cached" needs the difference
    /// (`crate::uncached`).
    pub(crate) fn store_outcome(
        &self,
        key: CacheKey,
        target: String,
        representation: Representation,
        taken: CutSnapshot,
        cost_millis: Option<u64>,
    ) -> Result<(), Declined> {
        let mut state = self.state.lock().expect("cache lock");

        // ★ THE RACE CHECK. The representation was built from state observed before
        // any of this; if a thread it depends on moved in the meantime, what came
        // back is already stale and must not be filed against the post-cut
        // generation. Declining is free — the next read recomputes.
        if state.cut_since(representation.threads(), taken) {
            return Err(Declined::CutInFlight);
        }

        let edges: Vec<(Thread, u64)> = representation
            .threads()
            .iter()
            .map(|thread| (thread.clone(), state.generation_of(thread)))
            .collect();
        state.tick += 1;
        let entry = CacheEntry {
            target,
            representation,
            edges,
            cost_millis,
            hits: 0,
            stored_tick: state.tick,
            last_used_tick: state.tick,
        };
        if !self.policy.admit(&entry.facts()) {
            return Err(Declined::Policy);
        }
        let bytes = entry.bytes();
        if let Some(replaced) = state.entries.insert(key, entry) {
            state.bytes = state.bytes.saturating_sub(replaced.bytes());
        }
        state.bytes += bytes;
        self.enforce_bound(&mut state);
        state.sweep_generations(self.policy.capacity().max_entries);
        Ok(())
    }

    /// Cut a golden thread: bump its generation (invalidating every entry pinned to
    /// an earlier one) and record the cut so an in-flight request cannot file a
    /// result that predates it.
    pub fn cut(&self, thread: Thread) {
        let mut state = self.state.lock().expect("cache lock");
        *state.generations.entry(thread.clone()).or_insert(0) += 1;
        state.seq += 1;
        let seq = state.seq;
        if state.cuts.len() >= CUT_LOG {
            if let Some((_, forgotten)) = state.cuts.pop_front() {
                state.horizon = forgotten;
            }
        }
        state.cuts.push_back((thread, seq));
        // Published last: a snapshot that sees this value has seen the record above.
        self.cut_seq.store(seq, Ordering::Release);
    }

    /// The number of entries currently held.
    pub fn len(&self) -> usize {
        self.state.lock().expect("cache lock").entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total payload bytes currently held.
    pub fn bytes(&self) -> usize {
        self.state.lock().expect("cache lock").bytes
    }

    /// The bound in force, for the operator readout.
    pub fn bound(&self) -> CacheBound {
        self.policy.capacity()
    }

    /// One row per entry for `urn:kernel:cache`, with the resolution chain each
    /// was computed in — as the key's scope fingerprint, which is all the cache
    /// holds; the kernel maps it back to a rendered chain for the readout.
    ///
    /// With no clock reading, so every entry with a deadline reads
    /// [`Expired`](EntryState::Expired) — the conservative answer serving gives too.
    /// [`rows_at`](Self::rows_at) takes the reading.
    pub fn rows_with_scope(&self) -> Vec<CacheRow> {
        self.rows_at(None)
    }

    /// [`rows_with_scope`](Self::rows_with_scope), each row marked with whether it
    /// would be served as of `now` ([`EntryState`]). Read-only, like
    /// [`probe`](Self::probe): a stale row is reported, not evicted, so reading the
    /// readout cannot change what it reads (ledger #612).
    pub fn rows_at(&self, now: Option<Time>) -> Vec<CacheRow> {
        let state = self.state.lock().expect("cache lock");
        state
            .entries
            .iter()
            .map(|(key, entry)| CacheRow {
                target: entry.target.clone(),
                media_type: entry.representation.repr_type.media_type.clone(),
                bytes: entry.bytes(),
                threads: entry.edges.len(),
                scope: key.scope,
                state: state.state_of(entry, now),
            })
            .collect()
    }

    /// One row per entry for `urn:kernel:cache`: the IRI it was resolved from, its
    /// media type, its size in bytes, and how many golden threads it depends on.
    /// [`rows_with_scope`](Self::rows_with_scope) adds the chain.
    pub fn rows(&self) -> Vec<(String, String, usize, usize)> {
        let state = self.state.lock().expect("cache lock");
        state
            .entries
            .values()
            .map(|entry| {
                (
                    entry.target.clone(),
                    entry.representation.repr_type.media_type.clone(),
                    entry.bytes(),
                    entry.edges.len(),
                )
            })
            .collect()
    }

    /// One row per tracked thread for `urn:kernel:threads`: its name and how many
    /// times it has been cut. A thread no live entry depends on may have been swept,
    /// so this is the *tracked* history, not the total one.
    pub fn generation_rows(&self) -> Vec<(String, u64)> {
        let state = self.state.lock().expect("cache lock");
        state
            .generations
            .iter()
            .map(|(thread, generation)| (thread.as_str().to_string(), *generation))
            .collect()
    }

    /// Evict until the policy's bound is met, or until the policy declines to name a
    /// victim.
    fn enforce_bound(&self, state: &mut CacheState) {
        let bound = self.policy.capacity();
        while state.entries.len() > bound.max_entries || state.bytes > bound.max_bytes {
            let keys: Vec<CacheKey> = state.entries.keys().copied().collect();
            let facts: Vec<EntryFacts<'_>> =
                keys.iter().map(|key| state.entries[key].facts()).collect();
            let Some(index) = self.policy.victim(&facts) else {
                return;
            };
            let Some(key) = keys.get(index).copied() else {
                return;
            };
            match state.entries.remove(&key) {
                Some(evicted) => state.bytes = state.bytes.saturating_sub(evicted.bytes()),
                None => return,
            }
        }
    }
}

impl CacheState {
    fn generation_of(&self, thread: &Thread) -> u64 {
        self.generations.get(thread).copied().unwrap_or(0)
    }

    /// Whether an entry is valid *right now*: every golden-thread edge is still at
    /// its pinned generation, and any deadline is still in the future. The single
    /// source of truth behind both [`ReprCache::get`] and [`ReprCache::probe`], so
    /// the serving path and the read-only probe can never disagree.
    fn is_valid(&self, entry: &CacheEntry, now: Option<Time>) -> bool {
        self.state_of(entry, now).is_live()
    }

    /// [`is_valid`](Self::is_valid), saying which half failed — a cut edge is
    /// reported ahead of a passed deadline, since the cut is the one an edit caused.
    fn state_of(&self, entry: &CacheEntry, now: Option<Time>) -> EntryState {
        let edges_current = entry
            .edges
            .iter()
            .all(|(thread, generation)| self.generation_of(thread) == *generation);
        if !edges_current {
            return EntryState::Cut;
        }
        match entry.representation.expiry {
            Expiry::At(deadline) if !now.is_some_and(|now| now < deadline) => EntryState::Expired,
            _ => EntryState::Live,
        }
    }

    /// Whether any of `threads` was cut after `taken` — or whether the log can no
    /// longer answer, in which case the answer is yes.
    fn cut_since(&self, threads: &BTreeSet<Thread>, taken: CutSnapshot) -> bool {
        if threads.is_empty() {
            // Nothing to invalidate: a thread-free entry (pure computation) cannot
            // be raced by a cut at all.
            return false;
        }
        if taken.0 < self.horizon {
            return true;
        }
        self.cuts
            .iter()
            .rev()
            .take_while(|(_, seq)| *seq > taken.0)
            .any(|(thread, _)| threads.contains(thread))
    }

    /// Forget the generations of threads no resident entry depends on.
    ///
    /// The generation map is keyed by thread NAME, and a name can derive from caller
    /// input (a mutating verb auto-cuts a thread named after its target), so it is
    /// the same exhaustion vector as the entry map with a different key. Forgetting
    /// is safe exactly when nothing pins the thread: a generation number is only ever
    /// compared against an entry's pin, so a thread nobody pins can start again from
    /// zero without making any entry look fresher than it is. Pins are taken under
    /// this same lock, so no in-flight request can pin across a sweep — and the race
    /// check reads the cut log, not this map.
    fn sweep_generations(&mut self, max_entries: usize) {
        let watermark = max_entries.saturating_mul(2).max(1024);
        if self.generations.len() <= watermark {
            return;
        }
        let live: HashSet<Thread> = self
            .entries
            .values()
            .flat_map(|entry| entry.edges.iter().map(|(thread, _)| thread.clone()))
            .collect();
        self.generations.retain(|thread, _| live.contains(thread));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iri::Iri;
    use crate::repr::ReprType;
    use crate::request::Request;
    use crate::verb::Verb;

    fn key(n: u8) -> CacheKey {
        let request = Request::new(
            Verb::Source,
            Iri::parse(format!("urn:test:{n}")).expect("iri"),
        );
        CacheKey::new(request.id(), 0)
    }

    fn repr(bytes: usize) -> Representation {
        Representation::new(ReprType::new("text/plain"), vec![b'x'; bytes]).cacheable()
    }

    fn threaded(bytes: usize, thread: &str) -> Representation {
        repr(bytes).depends_on(thread)
    }

    #[test]
    fn a_cut_after_the_snapshot_declines_the_store() {
        let cache = ReprCache::default();
        let taken = cache.snapshot();
        cache.cut(Thread::new("urn:data:state"));
        assert!(
            !cache.store(
                key(1),
                "urn:test:a".into(),
                threaded(8, "urn:data:state"),
                taken,
                None
            ),
            "a result predating the cut must not be stored"
        );
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn a_cut_to_an_unrelated_thread_still_stores() {
        // The check is per-thread on purpose: a busy host cutting other resources
        // must not stop an expensive result from ever being cached.
        let cache = ReprCache::default();
        let taken = cache.snapshot();
        cache.cut(Thread::new("urn:data:elsewhere"));
        assert!(cache.store(
            key(1),
            "urn:test:a".into(),
            threaded(8, "urn:data:state"),
            taken,
            None
        ));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn a_snapshot_older_than_the_cut_log_declines() {
        let cache = ReprCache::default();
        let taken = cache.snapshot();
        for n in 0..(CUT_LOG + 1) {
            cache.cut(Thread::new(format!("urn:data:{n}")));
        }
        assert!(
            !cache.store(
                key(1),
                "urn:test:a".into(),
                threaded(8, "urn:data:state"),
                taken,
                None
            ),
            "cuts it can no longer see must be assumed hostile"
        );
    }

    #[test]
    fn a_thread_free_entry_is_never_raced() {
        let cache = ReprCache::default();
        let taken = cache.snapshot();
        cache.cut(Thread::new("urn:data:state"));
        assert!(
            cache.store(key(1), "urn:test:a".into(), repr(8), taken, None),
            "no threads ⇒ no cut can invalidate it"
        );
    }

    #[test]
    fn the_entry_bound_evicts() {
        let cache = ReprCache::new(Arc::new(Lru::with_bound(CacheBound::new(2, 1 << 20))));
        for n in 0..4u8 {
            assert!(cache.store(
                key(n),
                format!("urn:test:{n}"),
                repr(8),
                cache.snapshot(),
                None
            ));
        }
        assert_eq!(cache.len(), 2, "the bound is enforced on every store");
    }

    #[test]
    fn the_byte_bound_evicts_and_refuses_the_oversized() {
        let cache = ReprCache::new(Arc::new(Lru::with_bound(CacheBound::new(16, 64))));
        assert!(cache.store(
            key(1),
            "urn:test:a".into(),
            repr(40),
            cache.snapshot(),
            None
        ));
        assert!(cache.store(
            key(2),
            "urn:test:b".into(),
            repr(40),
            cache.snapshot(),
            None
        ));
        assert_eq!(cache.len(), 1, "80 bytes does not fit in 64");
        assert!(
            !cache.store(
                key(3),
                "urn:test:c".into(),
                repr(65),
                cache.snapshot(),
                None
            ),
            "an entry larger than the whole budget is refused, not admitted-then-evicted"
        );
    }

    #[test]
    fn lru_evicts_the_least_recently_served() {
        let cache = ReprCache::new(Arc::new(Lru::with_bound(CacheBound::new(2, 1 << 20))));
        cache.store(key(1), "a".into(), repr(8), cache.snapshot(), None);
        cache.store(key(2), "b".into(), repr(8), cache.snapshot(), None);
        // Serving 1 makes 2 the least recently used.
        assert!(cache.get(&key(1), None).is_some());
        cache.store(key(3), "c".into(), repr(8), cache.snapshot(), None);
        assert!(cache.probe(&key(1), None), "recently served survives");
        assert!(
            !cache.probe(&key(2), None),
            "least recently served is evicted"
        );
    }

    #[test]
    fn fifo_evicts_the_oldest_however_recently_served() {
        let cache = ReprCache::new(Arc::new(Fifo::with_bound(CacheBound::new(2, 1 << 20))));
        cache.store(key(1), "a".into(), repr(8), cache.snapshot(), None);
        cache.store(key(2), "b".into(), repr(8), cache.snapshot(), None);
        assert!(cache.get(&key(1), None).is_some());
        cache.store(key(3), "c".into(), repr(8), cache.snapshot(), None);
        assert!(
            !cache.probe(&key(1), None),
            "oldest goes, hits notwithstanding"
        );
        assert!(cache.probe(&key(2), None));
    }

    #[test]
    fn cost_aware_keeps_the_expensive_and_compact() {
        let cache = ReprCache::new(Arc::new(CostAware::with_bound(CacheBound::new(2, 1 << 20))));
        // Cheap and large versus expensive and small.
        cache.store(key(1), "cheap".into(), repr(64), cache.snapshot(), Some(1));
        cache.store(key(2), "dear".into(), repr(8), cache.snapshot(), Some(900));
        cache.store(key(3), "new".into(), repr(8), cache.snapshot(), Some(500));
        assert!(
            !cache.probe(&key(1), None),
            "cheap-per-byte is evicted first"
        );
        assert!(cache.probe(&key(2), None));
    }

    #[test]
    fn cost_aware_without_a_clock_falls_back_to_recency() {
        let cache = ReprCache::new(Arc::new(CostAware::with_bound(CacheBound::new(2, 1 << 20))));
        cache.store(key(1), "a".into(), repr(8), cache.snapshot(), None);
        cache.store(key(2), "b".into(), repr(8), cache.snapshot(), None);
        assert!(cache.get(&key(1), None).is_some());
        cache.store(key(3), "c".into(), repr(8), cache.snapshot(), None);
        assert!(
            cache.probe(&key(1), None),
            "unmeasured cost is not zero cost"
        );
        assert!(!cache.probe(&key(2), None));
    }

    #[test]
    fn a_cut_invalidates_and_the_lookup_evicts() {
        let cache = ReprCache::default();
        assert!(cache.store(
            key(1),
            "a".into(),
            threaded(8, "urn:data:state"),
            cache.snapshot(),
            None
        ));
        cache.cut(Thread::new("urn:data:state"));
        assert!(!cache.probe(&key(1), None), "a cut entry is not served");
        assert_eq!(cache.len(), 1, "the probe does not evict");
        assert!(cache.get(&key(1), None).is_none());
        assert_eq!(
            cache.len(),
            0,
            "the serving path evicts what it found stale"
        );
    }

    #[test]
    fn the_rows_say_which_entries_a_cut_has_made_stale_without_evicting_them() {
        let cache = ReprCache::default();
        assert!(cache.store(
            key(1),
            "urn:test:a".into(),
            threaded(8, "urn:data:state"),
            cache.snapshot(),
            None
        ));
        assert!(cache.store(
            key(2),
            "urn:test:b".into(),
            threaded(8, "urn:data:other"),
            cache.snapshot(),
            None
        ));
        let deadline = Representation::new(ReprType::new("text/plain"), b"x".to_vec())
            .cacheable_until(Time::from_millis(100));
        assert!(cache.store(
            key(3),
            "urn:test:c".into(),
            deadline,
            cache.snapshot(),
            None
        ));
        cache.cut(Thread::new("urn:data:state"));
        let mut rows = cache.rows_at(Some(Time::from_millis(100)));
        rows.sort_by(|a, b| a.target.cmp(&b.target));
        let states: Vec<(&str, EntryState)> =
            rows.iter().map(|r| (r.target.as_str(), r.state)).collect();
        assert_eq!(
            states,
            [
                ("urn:test:a", EntryState::Cut),
                ("urn:test:b", EntryState::Live),
                ("urn:test:c", EntryState::Expired),
            ]
        );
        assert_eq!(cache.len(), 3, "reading the rows evicts nothing");
    }

    #[test]
    fn generations_are_swept_once_nothing_pins_them() {
        let cache = ReprCache::new(Arc::new(Lru::with_bound(CacheBound::new(4, 1 << 20))));
        // One live entry pinning one thread, at a generation worth remembering.
        // (A thread that has never been cut is absent from the map entirely — the
        // map holds cut history, so there is nothing to sweep for one.)
        cache.cut(Thread::new("urn:data:pinned"));
        assert!(cache.store(
            key(1),
            "a".into(),
            threaded(8, "urn:data:pinned"),
            cache.snapshot(),
            None
        ));
        // …and a flood of caller-named threads nothing depends on.
        for n in 0..2100 {
            cache.cut(Thread::new(format!("urn:data:flood:{n}")));
        }
        assert!(
            cache.generation_rows().len() > 2000,
            "the sweep is driven by a store, not by the cut itself"
        );
        assert!(cache.store(key(2), "b".into(), repr(8), cache.snapshot(), None));
        let rows = cache.generation_rows();
        assert_eq!(rows.len(), 1, "only pinned threads survive the sweep");
        assert_eq!(rows[0], ("urn:data:pinned".to_string(), 1));
        // …and the entry that pins it is still valid, which is what makes it safe.
        assert!(cache.probe(&key(1), None));
    }

    #[test]
    fn a_policy_declining_to_name_a_victim_leaves_the_cache_over_its_bound() {
        // The escape hatch is explicit, never accidental: a policy that wants an
        // unbounded cache has to say so here.
        struct KeepEverything;
        impl CachePolicy for KeepEverything {
            fn capacity(&self) -> CacheBound {
                CacheBound::new(1, 1)
            }
            fn admit(&self, _candidate: &EntryFacts<'_>) -> bool {
                true
            }
            fn victim(&self, _resident: &[EntryFacts<'_>]) -> Option<usize> {
                None
            }
        }
        let cache = ReprCache::new(Arc::new(KeepEverything));
        for n in 0..5u8 {
            assert!(cache.store(key(n), "a".into(), repr(8), cache.snapshot(), None));
        }
        assert_eq!(cache.len(), 5);
    }

    #[test]
    fn a_deadline_is_honored_and_a_clockless_kernel_assumes_the_worst() {
        let cache = ReprCache::default();
        let repr = Representation::new(ReprType::new("text/plain"), b"x".to_vec())
            .cacheable_until(Time::from_millis(100));
        assert!(cache.store(key(1), "a".into(), repr, cache.snapshot(), None));
        assert!(cache.probe(&key(1), Some(Time::from_millis(99))));
        assert!(!cache.probe(&key(1), Some(Time::from_millis(100))));
        assert!(
            !cache.probe(&key(1), None),
            "no clock ⇒ cannot claim unexpired"
        );
    }
}
