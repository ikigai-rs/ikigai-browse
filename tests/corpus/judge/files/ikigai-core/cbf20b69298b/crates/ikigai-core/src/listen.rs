//! **Cut listeners**: a host is told when a golden thread it cares about is cut, and
//! what that cut invalidated — NetKernel's golden-thread listeners and the
//! recalculating golden thread (News 7.5, 7.6), for a kernel with no runtime.
//!
//! A cut is lazy: it bumps a generation and nothing else happens until a reader
//! finds its entry stale. That is the right default and the wrong shape for two
//! things a host wants to do: **push** a change to a page that is showing the
//! invalidated value, and **recompute before the first reader** so the reader who
//! comes next is served from the cache instead of paying for the recomputation. Both
//! need to know WHEN a cut happened and WHAT it invalidated, and both are the host's
//! decision — the kernel only reports.
//!
//! # The delivery shape: a bounded queue the host drains
//!
//! [`Kernel::listen`](crate::Kernel::listen) returns a [`CutListener`]. Every cut of a
//! thread the listener's [`ListenSpec`] matches (by exact name or by prefix) appends a
//! [`CutEvent`] to that listener's own queue; the host takes them with
//! [`CutListener::drain`], or awaits them with [`CutListener::wait`], which parks on a
//! [`Waker`] and needs no executor of the kernel's choosing.
//!
//! **Not a callback, deliberately.** A cut runs on the WRITER's stack — inside the
//! `Sink` that auto-cuts its target, inside `urn:kernel:cut`, inside a watcher — and
//! with the cache lock held. A callback would run there too: a host that recomputed
//! from it would issue a request re-entrantly from inside a write (the writer's
//! latency would include the reader's recomputation, and a callback that blocked on
//! a runtime would deadlock the one thread a browser or WASI kernel has). The queue
//! decouples them: the cut appends and returns, the host decides when to act, and
//! whatever it issues runs on its own stack with the lock long released. No task is
//! spawned or awaited by the kernel, and no request is issued from inside `cut`.
//!
//! # Authority: a listener learns only what its own reads rest on
//!
//! Registering is a capability, [`CAP_LISTEN`]. And a listener hears a cut only when
//! its authority has **already been told the thread's name**: the root authority hears
//! every cut its spec matches, and any other authority hears a cut of thread `T` only
//! when some entry in the cache **computed under that same authority** (its capability
//! fingerprint, the cache key's own partition) depends on `T`. A reader that read such
//! an entry received `T` on its representation's threads, so the event discloses no
//! name it did not have — and its `invalidated` list names only entries computed under
//! that authority, never what anyone else has read. A thread nothing of the listener's
//! own rests on is simply not heard, which is conservative in the only safe direction:
//! a listener that wants to hear about a name reads it first.
//!
//! # Bounded, and a drop is reported
//!
//! Each listener's queue holds at most [`ListenSpec::capacity`] events (default
//! [`LISTEN_CAPACITY`]). A cut that finds it full is **not** queued, and is counted in
//! [`CutBatch::dropped`]: the host learns it missed cuts and must resynchronize (for a
//! page: re-read what it shows), rather than silently acting on a partial history. An
//! event names at most [`INVALIDATED_NAMED`] invalidated targets and counts the rest
//! in [`CutEvent::invalidated_more`], for the same reason. Nothing is silently
//! truncated anywhere.
//!
//! ```
//! use std::sync::Arc;
//! use futures::executor::block_on;
//! use ikigai_core::{
//!     ArgRef, Capability, EndpointSpace, Error, Exact, FnEndpoint, Iri, Kernel, ListenSpec,
//!     ReprType, Representation, Request, Verb,
//! };
//!
//! // A cell: a cacheable read, and a Sink that the kernel follows with a cut.
//! let cell = FnEndpoint::new("cell", |inv| match inv.request.verb {
//!     Verb::Sink => Ok(Representation::new(ReprType::new("text/plain"), b"ok".to_vec())),
//!     _ => Ok(Representation::new(ReprType::new("text/plain"), b"5".to_vec()).cacheable()),
//! });
//! let kernel = Kernel::new(Arc::new(
//!     EndpointSpace::new().bind(Exact::new("urn:example:cell:A1"), cell),
//! ));
//! let cap = Capability::root();
//! let listener = kernel.listen(ListenSpec::new().prefix("urn:example:cell:"), &cap)?;
//!
//! let a1 = Iri::parse("urn:example:cell:A1").unwrap();
//! block_on(kernel.issue(Request::new(Verb::Source, a1.clone()), &cap))?;
//! let write = Request::new(Verb::Sink, a1).with_arg("content", ArgRef::Inline(b"6".to_vec()));
//! block_on(kernel.issue(write, &cap))?;
//!
//! let batch = listener.drain();
//! assert_eq!(batch.dropped, 0);
//! assert_eq!(batch.events.len(), 1);
//! assert_eq!(batch.events[0].thread.as_str(), "urn:example:cell:A1");
//! assert_eq!(batch.events[0].invalidated, ["urn:example:cell:A1"]);
//! # Ok::<(), Error>(())
//! ```

use std::collections::{BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::task::{Context, Poll, Waker};

use crate::cache::Dependent;
use crate::repr::Thread;

/// The capability that registering a [`CutListener`] requires.
pub const CAP_LISTEN: &str = "urn:cap:kernel:listen";

/// How many events a listener's queue holds when its [`ListenSpec`] does not say.
/// A cut arriving at a full queue is counted in [`CutBatch::dropped`], not queued.
pub const LISTEN_CAPACITY: usize = 256;

/// How many invalidated targets one [`CutEvent`] names; the rest are counted in
/// [`CutEvent::invalidated_more`].
pub const INVALIDATED_NAMED: usize = 64;

/// Which cuts a listener wants: threads named exactly, threads under a prefix, and
/// how many undrained events it will hold before it starts counting drops.
///
/// An empty spec matches nothing.
///
/// ```
/// use ikigai_core::{ListenSpec, LISTEN_CAPACITY};
///
/// let spec = ListenSpec::new()
///     .exact("urn:kernel:bindings")
///     .prefix("urn:example:cell:");
/// assert!(spec.matches("urn:kernel:bindings"));
/// assert!(spec.matches("urn:example:cell:A1"));
/// assert!(!spec.matches("urn:example:other"));
/// assert_eq!(spec.bound(), LISTEN_CAPACITY);
/// assert_eq!(ListenSpec::new().capacity(0).bound(), 1, "a queue holds at least one");
/// ```
#[derive(Clone, Debug)]
pub struct ListenSpec {
    exact: BTreeSet<String>,
    prefixes: Vec<String>,
    capacity: usize,
}

impl Default for ListenSpec {
    fn default() -> Self {
        ListenSpec {
            exact: BTreeSet::new(),
            prefixes: Vec::new(),
            capacity: LISTEN_CAPACITY,
        }
    }
}

impl ListenSpec {
    /// A spec that matches nothing yet, with the default capacity.
    pub fn new() -> Self {
        ListenSpec::default()
    }

    /// Also hear cuts of the thread named exactly `thread` (builder).
    pub fn exact(mut self, thread: impl Into<String>) -> Self {
        self.exact.insert(thread.into());
        self
    }

    /// Also hear cuts of every thread whose name starts with `prefix` (builder).
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefixes.push(prefix.into());
        self
    }

    /// Hold at most `events` undrained events (builder); at least one.
    pub fn capacity(mut self, events: usize) -> Self {
        self.capacity = events.max(1);
        self
    }

    /// The queue bound in force.
    pub fn bound(&self) -> usize {
        self.capacity
    }

    /// Whether a cut of `thread` is one this spec asks about.
    pub fn matches(&self, thread: &str) -> bool {
        self.exact.contains(thread) || self.prefixes.iter().any(|p| thread.starts_with(p))
    }
}

/// One cut, as a listener hears it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CutEvent {
    /// The thread that was cut.
    pub thread: Thread,
    /// The cut's place in the kernel's single cut order. Two cuts landing
    /// concurrently are delivered to each listener in this order.
    pub sequence: u64,
    /// The targets of the cached entries this cut invalidated — resident entries
    /// hanging from the thread at the generation this cut moved, i.e. stored since
    /// the thread's previous cut — that the listener's authority may know about (all
    /// of them for root; only those computed under the listener's own capability
    /// otherwise). Sorted, each once, at most [`INVALIDATED_NAMED`]. Only what was
    /// cached: a result the kernel never stored is not listed, because the kernel does
    /// not know it was derived from the thread. Judged on this thread's edge alone,
    /// so an entry some other thread had already made stale can appear: an
    /// over-report of something that needs recomputing anyway, never an omission.
    pub invalidated: Vec<String>,
    /// How many further invalidated entries were not named. It counts ENTRIES: a
    /// target cached under several keys (other arguments, another chain) that did not
    /// make the named list may count more than once.
    pub invalidated_more: usize,
}

/// What [`CutListener::drain`] hands back: the events queued since the last drain, in
/// cut order, and how many cuts arrived while the queue was full.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CutBatch {
    /// The queued events, oldest first.
    pub events: Vec<CutEvent>,
    /// Cuts this listener would have heard but did not queue, because its queue was
    /// full. Non-zero means the history in `events` is a PREFIX: everything after the
    /// last event is unknown, and the host must resynchronize.
    pub dropped: u64,
}

impl CutBatch {
    /// Nothing queued and nothing dropped.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.dropped == 0
    }
}

/// Whose cuts a listener may hear (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hearing {
    /// Root authority: every cut its spec matches, every invalidated target.
    Everything,
    /// Only threads some entry computed under this capability fingerprint depends
    /// on, and only those entries' targets.
    OwnReads(u64),
}

struct Shared {
    spec: ListenSpec,
    hearing: Hearing,
    queue: Mutex<Queue>,
}

#[derive(Default)]
struct Queue {
    events: VecDeque<CutEvent>,
    dropped: u64,
    waker: Option<Waker>,
}

/// A registration returned by [`Kernel::listen`](crate::Kernel::listen): the host's
/// end of one listener's bounded queue. Dropping it unregisters the listener.
pub struct CutListener {
    shared: Arc<Shared>,
}

impl CutListener {
    /// Take every queued event and the drop count, leaving the queue empty.
    pub fn drain(&self) -> CutBatch {
        let mut queue = self.shared.queue.lock().expect("listener queue");
        CutBatch {
            events: queue.events.drain(..).collect(),
            dropped: std::mem::take(&mut queue.dropped),
        }
    }

    /// How many events are queued right now.
    pub fn pending(&self) -> usize {
        self.shared
            .queue
            .lock()
            .expect("listener queue")
            .events
            .len()
    }

    /// The spec this listener was registered with.
    pub fn spec(&self) -> &ListenSpec {
        &self.shared.spec
    }

    /// A future that resolves to the next non-empty [`CutBatch`] — at once if one is
    /// queued, else when a cut (or a drop) arrives. Runtime-free: it parks on the
    /// [`Waker`] it is polled with, and the cut wakes it after releasing every kernel
    /// lock. One waiter at a time: a second concurrent `wait` replaces the first's
    /// waker.
    pub fn wait(&self) -> Wait<'_> {
        Wait { listener: self }
    }
}

/// The future [`CutListener::wait`] returns.
pub struct Wait<'a> {
    listener: &'a CutListener,
}

impl Future for Wait<'_> {
    type Output = CutBatch;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<CutBatch> {
        let mut queue = self.listener.shared.queue.lock().expect("listener queue");
        if queue.events.is_empty() && queue.dropped == 0 {
            queue.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        queue.waker = None;
        Poll::Ready(CutBatch {
            events: queue.events.drain(..).collect(),
            dropped: std::mem::take(&mut queue.dropped),
        })
    }
}

/// The kernel's registry of listeners. Held weakly, so dropping a [`CutListener`] is
/// the whole of unregistering; dead entries are swept on the next registration or the
/// next cut that notices one.
#[derive(Default)]
pub(crate) struct Listeners {
    /// Registered entries, dead or alive — the fast path's only read: zero means a
    /// cut costs one atomic load for listening and nothing more.
    count: AtomicUsize,
    list: RwLock<Vec<Weak<Shared>>>,
}

impl Listeners {
    pub(crate) fn register(&self, spec: ListenSpec, hearing: Hearing) -> CutListener {
        let shared = Arc::new(Shared {
            spec,
            hearing,
            queue: Mutex::new(Queue::default()),
        });
        let mut list = self.list.write().expect("listeners lock");
        list.retain(|weak| weak.strong_count() > 0);
        list.push(Arc::downgrade(&shared));
        self.count.store(list.len(), Ordering::Release);
        CutListener { shared }
    }

    /// Whether any live listener's spec matches `thread` — decided BEFORE the cut
    /// takes the cache lock, so a cut nobody listens for never scans the cache. A
    /// listener registered after this check does not hear this cut: a listener hears
    /// the cuts that begin after [`Kernel::listen`](crate::Kernel::listen) returns.
    pub(crate) fn interested(&self, thread: &str) -> bool {
        if self.count.load(Ordering::Acquire) == 0 {
            return false;
        }
        let mut dead = false;
        let mut matched = false;
        {
            let list = self.list.read().expect("listeners lock");
            for weak in list.iter() {
                match weak.upgrade() {
                    Some(shared) => matched |= shared.spec.matches(thread),
                    None => dead = true,
                }
            }
        }
        if dead {
            let mut list = self.list.write().expect("listeners lock");
            list.retain(|weak| weak.strong_count() > 0);
            self.count.store(list.len(), Ordering::Release);
        }
        matched
    }

    /// Queue the cut for every listener that matches and may hear it, and hand back
    /// the wakers to wake — to be woken by the caller only AFTER it has released the
    /// cache lock this runs under, so a waker that does anything at all cannot
    /// deadlock the kernel.
    pub(crate) fn deliver(
        &self,
        thread: &Thread,
        sequence: u64,
        dependents: &[Dependent<'_>],
    ) -> Vec<Waker> {
        let list = self.list.read().expect("listeners lock");
        let mut wakers = Vec::new();
        for shared in list.iter().filter_map(Weak::upgrade) {
            if !shared.spec.matches(thread.as_str()) {
                continue;
            }
            let (heard, invalidated): (bool, Vec<&str>) = match shared.hearing {
                Hearing::Everything => (
                    true,
                    dependents
                        .iter()
                        .filter(|d| d.live)
                        .map(|d| d.target)
                        .collect(),
                ),
                Hearing::OwnReads(fingerprint) => {
                    let mine = || dependents.iter().filter(|d| d.capability == fingerprint);
                    (
                        mine().next().is_some(),
                        mine().filter(|d| d.live).map(|d| d.target).collect(),
                    )
                }
            };
            if !heard {
                continue;
            }
            let mut queue = shared.queue.lock().expect("listener queue");
            if queue.events.len() >= shared.spec.capacity {
                // Full: counted, and none of the naming work below is done for it.
                queue.dropped += 1;
            } else {
                let (named, invalidated_more) = name_the_first(invalidated);
                queue.events.push_back(CutEvent {
                    thread: thread.clone(),
                    sequence,
                    invalidated: named,
                    invalidated_more,
                });
            }
            if let Some(waker) = queue.waker.take() {
                wakers.push(waker);
            }
        }
        wakers
    }
}

/// The [`INVALIDATED_NAMED`] smallest distinct targets, sorted and copied out, and
/// how many further invalidated entries were not named.
///
/// A bounded set rather than a full sort: a cut that invalidates thousands of entries
/// runs this under the cache lock, and sorting every target to keep sixty-four cost
/// more than the scan that found them (measured: ~175 µs of a ~205 µs cut over 4096
/// entries), and so did a set lookup per target. Visiting in hash order, almost every
/// target past the first few dozen is rejected by one comparison against the largest
/// kept.
fn name_the_first(targets: Vec<&str>) -> (Vec<String>, usize) {
    let mut kept: BTreeSet<&str> = BTreeSet::new();
    let mut more = 0;
    for target in targets {
        if kept.len() < INVALIDATED_NAMED {
            kept.insert(target);
            continue;
        }
        // Full: one comparison rejects almost everything, before any set lookup.
        let largest = *kept.last().expect("a full set has a largest");
        if target > largest {
            more += 1;
        } else if !kept.contains(target) {
            kept.remove(largest);
            kept.insert(target);
            more += 1;
        }
    }
    (kept.into_iter().map(str::to_string).collect(), more)
}
