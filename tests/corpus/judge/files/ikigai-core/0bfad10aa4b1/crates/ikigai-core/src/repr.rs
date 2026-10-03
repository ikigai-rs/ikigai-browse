use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::content::ContentId;
use crate::hashing::{feed_bytes, feed_str};

/// A **golden thread**: a named validity token a cached representation can depend
/// on.
///
/// A thread has identity but no representation — it names a piece of state whose
/// change should invalidate caches. Cutting a thread ([`Kernel::cut`](crate::Kernel::cut))
/// invalidates every cached representation that depended on it, directly or
/// transitively through composition. By convention a thread's name is the IRI of
/// the state it tracks (e.g. `urn:file:notes.txt`, `urn:person:alice`), so a
/// `Sink` that mutates a resource — or an external watcher — cuts the thread named
/// after it.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Thread(String);

impl Thread {
    /// A thread with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Thread(name.into())
    }

    /// The thread's name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Thread {
    fn from(s: String) -> Self {
        Thread(s)
    }
}

impl From<&str> for Thread {
    fn from(s: &str) -> Self {
        Thread(s.to_string())
    }
}

impl fmt::Display for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A representation type: a media type plus canonicalized parameters.
///
/// Parameters (e.g. `charset`, parse format) are part of the type so the cache
/// never conflates, say, a UTF-8 decode with a Latin-1 decode of the same bytes.
/// Parameters are stored sorted, giving a single canonical form.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ReprType {
    /// The media type, e.g. `text/turtle`.
    pub media_type: String,
    /// Canonicalized parameters (sorted by key).
    //
    // No `skip_serializing_if`: it would omit an empty map, which a
    // non-self-describing binary codec (postcard, used by the IPC wire) can't
    // round-trip. `default` still fills a missing field in self-describing formats.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

impl ReprType {
    /// A representation type with no parameters.
    pub fn new(media_type: impl Into<String>) -> Self {
        ReprType {
            media_type: media_type.into(),
            params: BTreeMap::new(),
        }
    }

    /// Add or replace a parameter (builder style).
    pub fn with_param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.insert(key.into(), value.into());
        self
    }

    /// The canonical string form: `media/type;k=v;...` with sorted params.
    pub fn canonical(&self) -> String {
        let mut s = self.media_type.clone();
        for (k, v) in &self.params {
            s.push(';');
            s.push_str(k);
            s.push('=');
            s.push_str(v);
        }
        s
    }
}

impl fmt::Display for ReprType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical())
    }
}

/// An absolute instant, milliseconds since the Unix epoch.
///
/// The kernel's notion of "now" is supplied by an injected [`Clock`](crate::Clock)
/// rather than read from the system directly, so pure resolution stays
/// deterministic for replay; a [`Time`] is just the value such a clock returns and
/// the deadline an [`Expiry::At`] is measured against. Plain `u64` millis: cheap,
/// `Ord`, serializable, and host-agnostic (system time natively, `Date.now()` in a
/// browser).
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct Time(pub u64);

impl Time {
    /// A time from milliseconds since the Unix epoch.
    pub fn from_millis(millis: u64) -> Self {
        Time(millis)
    }

    /// Milliseconds since the Unix epoch.
    pub fn as_millis(self) -> u64 {
        self.0
    }

    /// This time advanced by `millis` (saturating), e.g. a `max-age` deadline
    /// computed as `now.plus_millis(max_age * 1000)`.
    pub fn plus_millis(self, millis: u64) -> Self {
        Time(self.0.saturating_add(millis))
    }
}

/// How long a representation stays valid in the cache.
///
/// A lattice from most- to least-volatile: [`Always`](Expiry::Always) (never
/// cached) › [`At`](Expiry::At) (cached until a deadline) › [`Never`](Expiry::Never)
/// (permanently cacheable). Time-based validity rides alongside golden threads —
/// an `At` entry is valid only while *both* its deadline is in the future and its
/// threads are uncut.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Expiry {
    /// Always expired — never cached. The safe default: an endpoint must opt in
    /// to caching (mirroring NetKernel, where a response with no expiry is volatile).
    #[default]
    Always,
    /// Cached until an absolute deadline (e.g. an HTTP `Cache-Control: max-age`
    /// turned into `now + max-age`). Evaluated against the kernel's injected
    /// [`Clock`](crate::Clock); a kernel with no clock cannot honor a deadline, so
    /// it declines to cache such a result rather than risk serving it forever.
    At(Time),
    /// Never expires — permanently cacheable. Correct for a pure function of
    /// content-addressed inputs, where the request identity fully determines the result.
    Never,
}

impl Expiry {
    /// The more restrictive (sooner-expiring) of two expiries — the cache *meet*.
    /// [`Always`](Expiry::Always) dominates (any volatile part makes the whole
    /// volatile); an [`At`](Expiry::At) deadline beats [`Never`](Expiry::Never);
    /// two deadlines take the earlier. Used to combine a result's own expiry with
    /// its dependencies', so a composite is never fresher than its most volatile part.
    pub fn most_restrictive(self, other: Expiry) -> Expiry {
        use Expiry::*;
        match (self, other) {
            (Always, _) | (_, Always) => Always,
            (At(a), At(b)) => At(a.min(b)),
            (At(t), Never) | (Never, At(t)) => At(t),
            (Never, Never) => Never,
        }
    }
}

/// The cache **provenance** an upstream pipe stage hands to the next: the expiry
/// and golden threads of whatever produced this request's *input*.
///
/// The kernel folds it into the result's effective cacheability via
/// [`issue_with_incoming`](crate::Kernel::issue_with_incoming), so cacheability
/// flows down a pipeline — `source <X> | transform` is no more cacheable than `X`,
/// and cutting `X`'s thread invalidates the transformed result too. A transform
/// that is itself a pure function of its input (e.g. RDF transreption) thus
/// *inherits* its source's cacheability rather than asserting its own.
#[derive(Clone, Debug)]
pub struct Provenance {
    /// The upstream representation's expiry.
    pub expiry: Expiry,
    /// The upstream representation's golden threads.
    pub threads: BTreeSet<Thread>,
}

impl Provenance {
    /// The provenance of an upstream representation: its expiry and threads.
    pub fn new(expiry: Expiry, threads: BTreeSet<Thread>) -> Self {
        Provenance { expiry, threads }
    }
}

/// A typed value produced by an endpoint.
///
/// M0 carries the universal byte form; richer in-memory forms (RDF graphs,
/// solution sets) arrive with the store.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Representation {
    /// The representation type.
    pub repr_type: ReprType,
    /// The representation's bytes.
    pub bytes: Vec<u8>,
    /// Cache validity; defaults to [`Expiry::Always`] (uncacheable).
    #[serde(default)]
    pub expiry: Expiry,
    /// Golden threads this representation depends on (its cache *provenance*).
    ///
    /// Set by [`depends_on`](Representation::depends_on) and grown by the kernel,
    /// which unions in the threads of every sub-resource resolved while producing
    /// it — so a composite inherits its parts' threads and cutting any of them
    /// invalidates it. Kernel-local: **not serialized** (cache validity is a
    /// per-kernel concern, not part of a representation crossing a wire) and **not
    /// part of representation identity** (see the manual `PartialEq`).
    ///
    /// Shared, not owned: a cache hit clones the representation, and since 0.1.73
    /// every cacheable `Source`/`Exists` answer carries at least one thread (its own
    /// canonical target), so an owned set would cost a tree-node and a string
    /// allocation on every hit. `None` is the empty set, which is what every
    /// freshly built representation starts as — no allocation until a thread is
    /// declared or inherited. Measured: the second cut of hole A took the ~7 %
    /// hit-path cost of the first cut back down (formalism §10).
    #[serde(skip)]
    threads: Option<Arc<BTreeSet<Thread>>>,
}

/// The thread set of a representation that depends on nothing — what
/// [`Representation::threads`] hands back for `None`, so callers see one type.
static NO_THREADS: BTreeSet<Thread> = BTreeSet::new();

// Threads are cache provenance, not content: two representations with the same
// type and bytes are equal regardless of how their validity was tracked. (Also
// keeps wire round-trips equal, since `threads` is `serde(skip)`.)
impl PartialEq for Representation {
    fn eq(&self, other: &Self) -> bool {
        self.repr_type == other.repr_type
            && self.bytes == other.bytes
            && self.expiry == other.expiry
    }
}

impl Eq for Representation {}

impl Representation {
    /// Build a representation from a type and bytes (uncacheable by default).
    pub fn new(repr_type: ReprType, bytes: impl Into<Vec<u8>>) -> Self {
        Representation {
            repr_type,
            bytes: bytes.into(),
            expiry: Expiry::Always,
            threads: None,
        }
    }

    /// Mark this representation permanently cacheable ([`Expiry::Never`]).
    pub fn cacheable(mut self) -> Self {
        self.expiry = Expiry::Never;
        self
    }

    /// Mark this representation cacheable until an absolute deadline
    /// ([`Expiry::At`]). The kernel serves it from cache only while its injected
    /// [`Clock`](crate::Clock) reads before `deadline` (and its golden threads are
    /// uncut); a clockless kernel declines to cache it.
    pub fn cacheable_until(mut self, deadline: Time) -> Self {
        self.expiry = Expiry::At(deadline);
        self
    }

    /// Set the expiry explicitly (builder).
    pub fn with_expiry(mut self, expiry: Expiry) -> Self {
        self.expiry = expiry;
        self
    }

    /// Declare that this representation depends on a golden [`Thread`] (builder).
    ///
    /// When it is cached, cutting that thread invalidates it. A handler reading
    /// mutable state names the thread for that state (conventionally its IRI) and
    /// cuts it on write; an external watcher cuts it on change. Threads also
    /// propagate up through composition automatically, so a composer need only
    /// resolve its parts.
    pub fn depends_on(mut self, thread: impl Into<Thread>) -> Self {
        // Copy-on-write: a set shared with a cache entry is cloned before it is
        // grown, a set this representation owns alone is grown in place.
        Arc::make_mut(self.threads.get_or_insert_with(Default::default)).insert(thread.into());
        self
    }

    /// The golden threads this representation depends on.
    pub fn threads(&self) -> &BTreeSet<Thread> {
        self.threads.as_deref().unwrap_or(&NO_THREADS)
    }

    /// Replace the thread set (kernel use: install the effective set after
    /// unioning in dependency threads). An empty set is stored as nothing.
    pub(crate) fn with_threads(mut self, threads: BTreeSet<Thread>) -> Self {
        self.threads = (!threads.is_empty()).then(|| Arc::new(threads));
        self
    }

    /// The content address of this representation (its type and bytes together).
    pub fn content_id(&self) -> ContentId {
        let mut h = blake3::Hasher::new();
        feed_str(&mut h, "ikigai.repr.v0");
        feed_str(&mut h, &self.repr_type.canonical());
        feed_bytes(&mut h, &self.bytes);
        ContentId::from_hasher(h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_sorts_params() {
        let t = ReprType::new("text/plain")
            .with_param("charset", "utf-8")
            .with_param("boundary", "x");
        assert_eq!(t.canonical(), "text/plain;boundary=x;charset=utf-8");
    }

    #[test]
    fn type_is_part_of_identity() {
        let utf8 = Representation::new(
            ReprType::new("text/plain").with_param("charset", "utf-8"),
            b"hi".to_vec(),
        );
        let latin1 = Representation::new(
            ReprType::new("text/plain").with_param("charset", "latin-1"),
            b"hi".to_vec(),
        );
        assert_ne!(utf8.content_id(), latin1.content_id());
        assert_eq!(utf8.content_id(), utf8.clone().content_id());
    }
}
