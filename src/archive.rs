//! The store handle a browse mount reads and writes through: an injected
//! Oxigraph `Store` paired with **the one graph this mount owns**.
//!
//! # Why this is a type and not two parameters
//!
//! Before 0.4.0 every write site named `GraphName::DefaultGraph` literally and
//! every read site passed `None` for the graph — and those two are not
//! opposites. `None` in `quads_for_pattern` means *match every graph in the
//! store*, so browse read the whole dataset while writing one corner of it.
//! On a store browse is the only writer of, the two coincide and nothing shows.
//! On a SHARED store they do not, and the gap is a tenancy hole: a mount
//! confined to its own graph for writing would still answer out of anyone's.
//!
//! Threading a `graph` argument beside the `&Store` would have fixed the
//! thirteen writes and left the reads exactly as easy to get wrong — including
//! for the next read someone adds. So the store handle is **private to this
//! type**, and the only read this crate can express is one this type confines:
//! [`Archive::quads_for_pattern`] takes no graph argument because there is no
//! graph to choose. The default is still `GraphName::DefaultGraph`, so a host
//! that says nothing gets byte-for-byte what it had.
//!
//! # What browse owns
//!
//! Every quad browse writes has a browse-MINTED subject — `urn:iki:annotation:…`
//! (annotations and their two selector children) or `urn:ikigai:browse:…` (the
//! explanation archive, the review passes and their region memos). It writes no quad about a
//! subject someone else minted. That is what makes the graph migration a
//! complete one (`crate::migrate`), and it is pinned by
//! `every_quad_browse_writes_has_a_browse_minted_subject`.
//!
//! # Which thread a write commits on
//!
//! Every quad browse inserts or removes commits on **the archive's one long-lived writer
//! thread**, never on the thread the request happens to run on (ledger #1131). RocksDB
//! draws each memtable skiplist node's height from a thread-local generator seeded from a
//! hash of the thread's id, and a thread spawned after the previous one exited usually gets
//! the same id back. A host that runs each request on a thread of its own (the socket door
//! spawns one per connection) therefore had every browse write draw the same few heights,
//! and every later read walked the degenerated skiplists until the memtable flushed:
//! `tests/post_bulk_read.rs` measured 6x to 68x after 750 annotations, against 1.0x when the
//! same writes came from one thread. `ikigai-store` 0.2.12 fixed its own doors the same way
//! (ikigai-rs/ikigai-store PR #32); browse writes through the raw handle a host injects, so
//! it needs its own writer. Reads stay on the caller's thread.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use oxigraph::model::{GraphName, NamedNodeRef, NamedOrBlankNodeRef, Quad, TermRef};
use oxigraph::store::{QuadIter, StorageError, Store};

/// A store handle bound to one graph.
///
/// Cheap to clone-by-`Arc`: a mount builds exactly one and shares it with the
/// annotation family, the explanation archive, the review passes and the
/// pull-request layers, which is the same single-store sharing those four had
/// before — now with the graph carried along instead of re-chosen at each site.
pub(crate) struct Archive {
    /// PRIVATE, and the point of the type. Nothing in this crate can reach the
    /// handle to make an unconfined read; see [`Archive::store`] for the one
    /// exception and why it is not one.
    store: Arc<Store>,
    graph: GraphName,
    /// Where every [`Archive::insert`] and [`Archive::remove`] commits; see the module doc.
    writer: Writer,
    /// The thread each write committed on, in order: what the tests pin.
    #[cfg(test)]
    committed_on: Arc<Mutex<Vec<std::thread::ThreadId>>>,
}

impl Archive {
    /// Bind `store`'s `graph` — `GraphName::DefaultGraph` for a host that has
    /// not asked for a named one.
    pub(crate) fn new(store: Arc<Store>, graph: GraphName) -> Self {
        Archive {
            store,
            graph,
            writer: Writer::default(),
            #[cfg(test)]
            committed_on: Arc::default(),
        }
    }

    /// The graph this mount owns — the graph name every stored quad is built
    /// with, and what the confinement tests assert against.
    pub(crate) fn graph(&self) -> &GraphName {
        &self.graph
    }

    /// The raw handle — for **re-pairing it with a graph** and nothing else
    /// (`Mount` resolves one graph for the whole mount and rebuilds the
    /// archive). It is deliberately not a way to run a query: a read written
    /// against this would escape the confinement the type exists for.
    pub(crate) fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Insert a quad — one built with [`Archive::graph`] as its graph name.
    ///
    /// The debug assertion is the cheap half of the confinement claim: in a
    /// debug build (so, in every test and every CI run) a quad that reached
    /// here carrying some other graph name fails loudly rather than landing
    /// quietly outside the mount's graph.
    ///
    /// Commits on the writer thread (see the module doc) and returns once it has.
    pub(crate) fn insert(&self, quad: &Quad) -> Result<(), StorageError> {
        debug_assert_eq!(
            quad.graph_name, self.graph,
            "browse: a quad not built with Archive::graph() reached Archive::insert"
        );
        let (store, quad) = (Arc::clone(&self.store), quad.clone());
        self.commit(move || store.insert(&quad).map(drop))
    }

    /// Remove a quad — in practice one that came back from
    /// [`Archive::quads_for_pattern`], so it is already in this graph.
    ///
    /// Commits on the writer thread (see the module doc) and returns once it has.
    pub(crate) fn remove(&self, quad: &Quad) -> Result<(), StorageError> {
        let (store, quad) = (Arc::clone(&self.store), quad.clone());
        self.commit(move || store.remove(&quad).map(drop))
    }

    fn commit(
        &self,
        write: impl FnOnce() -> Result<(), StorageError> + Send + 'static,
    ) -> Result<(), StorageError> {
        #[cfg(test)]
        let write = {
            let committed_on = Arc::clone(&self.committed_on);
            move || {
                let on = std::thread::current().id();
                committed_on.lock().unwrap().push(on);
                write()
            }
        };
        self.writer.run(write).unwrap_or_else(|| {
            Err(StorageError::Io(std::io::Error::other(
                "browse: the archive's writer thread ended without answering",
            )))
        })
    }

    /// Every matching quad **in this mount's graph**.
    ///
    /// There is no `graph_name` parameter on purpose. `Store::quads_for_pattern`
    /// has one whose `None` matches every graph in the dataset, and that `None`
    /// is exactly the bug this type removes.
    pub(crate) fn quads_for_pattern(
        &self,
        subject: Option<NamedOrBlankNodeRef<'_>>,
        predicate: Option<NamedNodeRef<'_>>,
        object: Option<TermRef<'_>>,
    ) -> QuadIter<'static> {
        self.store
            .quads_for_pattern(subject, predicate, object, Some(self.graph.as_ref()))
    }
}

/// A write queued for the writer thread.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// One long-lived thread that runs every write handed to it, in order (ledger #1131).
///
/// - **Started on the first write**, so an archive that never writes (the one
///   [`crate::ExplainConfig::new`] builds before a mount replaces it) never starts one.
/// - **Gone with the archive**: dropping it drops the queue's sender, and the thread exits
///   once the queue is empty. It holds no store handle of its own between jobs, so a store
///   can be reopened at the same path as soon as the archive and the host have dropped it.
/// - **A panic in a write is the caller's**: it is caught on the writer thread, which lives
///   on, and resumed on the thread that asked.
/// - **If no thread can be started** (the process is out of threads), the write runs on the
///   caller's thread: slower reads later are better than a refused write now.
///
/// Writes were never concurrent in a way that mattered (each is one tiny RocksDB
/// transaction), so queueing them costs a channel round trip per quad, not parallelism.
#[derive(Default)]
struct Writer {
    queue: Mutex<Option<Sender<Job>>>,
}

impl Writer {
    /// Run `work` on the writer thread and return its answer, or `None` if the thread
    /// ended without giving one (which nothing here does on purpose).
    fn run<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        let (answer, answered) = mpsc::sync_channel(1);
        let job: Job = Box::new(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            // The caller waits for this; it can only be gone if it panicked itself.
            let _ = answer.send(outcome);
        });
        if let Err(job) = self.submit(job) {
            job();
        }
        match answered.recv().ok()? {
            Ok(value) => Some(value),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Queue `job`, first starting the thread if there is none (or it is gone). Hands the
    /// job back when no thread can be started.
    fn submit(&self, job: Job) -> Result<(), Job> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let job = match queue.as_ref() {
            Some(sender) => match sender.send(job) {
                Ok(()) => return Ok(()),
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        let (sender, jobs) = mpsc::channel::<Job>();
        let started = std::thread::Builder::new()
            .name("ikigai-browse-writer".to_string())
            .spawn(move || {
                for job in jobs {
                    job();
                }
            });
        if started.is_err() {
            return Err(job);
        }
        sender.send(job).map_err(|mpsc::SendError(job)| job)?;
        *queue = Some(sender);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxigraph::model::NamedNode;
    use std::thread::ThreadId;

    /// ★ THE PIN (ledger #1131): writes asked for from many short-lived threads, the way a
    /// socket door asks, all commit on ONE thread that is none of them. Without the writer
    /// every write commits on the thread that asked, and this fails on every run.
    #[test]
    fn every_write_commits_on_one_thread_whatever_thread_asks() {
        let writer = Arc::new(Writer::default());
        let on_a_fresh_thread = || {
            let writer = Arc::clone(&writer);
            std::thread::spawn(move || {
                let asked = std::thread::current().id();
                let ran: ThreadId = writer.run(|| std::thread::current().id()).unwrap();
                (asked, ran)
            })
            .join()
            .unwrap()
        };
        let (asked, first) = on_a_fresh_thread();
        assert_ne!(first, asked, "a write committed on the thread that asked");
        assert_ne!(first, std::thread::current().id());
        for _ in 0..8 {
            let (asked, ran) = on_a_fresh_thread();
            assert_ne!(ran, asked, "a write committed on the thread that asked");
            assert_eq!(ran, first, "a write committed on a thread of its own");
        }
        assert_eq!(writer.run(|| std::thread::current().id()).unwrap(), first);
    }

    /// The same pin through the archive's own doors: inserts and removes asked for from
    /// fresh threads land, in order, and every one of them committed on one thread that is
    /// none of the threads that asked. Fails if `insert` or `remove` stops going through the
    /// writer (nothing is recorded) or the writer runs work where it is asked (the threads
    /// match).
    #[test]
    fn inserts_and_removes_from_fresh_threads_commit_on_the_writer() {
        let archive = Arc::new(Archive::new(
            Arc::new(Store::new().unwrap()),
            GraphName::DefaultGraph,
        ));
        let quad = Quad::new(
            NamedNode::new("urn:ikigai:browse:test:s").unwrap(),
            NamedNode::new("urn:ikigai:browse:test:p").unwrap(),
            NamedNode::new("urn:ikigai:browse:test:o").unwrap(),
            GraphName::DefaultGraph,
        );
        let from_a_fresh_thread = |remove: bool| {
            let (archive, quad) = (Arc::clone(&archive), quad.clone());
            std::thread::spawn(move || {
                let outcome = if remove {
                    archive.remove(&quad)
                } else {
                    archive.insert(&quad)
                };
                outcome.unwrap();
                std::thread::current().id()
            })
            .join()
            .unwrap()
        };
        let mut asked = vec![from_a_fresh_thread(false)];
        assert!(archive.store().contains(&quad).unwrap());
        asked.push(from_a_fresh_thread(true));
        assert!(!archive.store().contains(&quad).unwrap());
        asked.push(from_a_fresh_thread(false));
        assert!(archive.store().contains(&quad).unwrap());

        let committed_on = archive.committed_on.lock().unwrap().clone();
        assert_eq!(
            committed_on.len(),
            3,
            "a write did not go through the writer"
        );
        assert!(
            committed_on.iter().all(|on| *on == committed_on[0]),
            "writes committed on more than one thread: {committed_on:?}"
        );
        assert!(
            asked.iter().all(|asker| *asker != committed_on[0]),
            "a write committed on the thread that asked"
        );
    }

    /// A panic in a write reaches the caller, and the writer survives it.
    #[test]
    fn a_panicking_write_reaches_its_caller_and_the_writer_lives_on() {
        let writer = Writer::default();
        let before = writer.run(|| std::thread::current().id()).unwrap();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            writer.run(|| panic!("a write that panics"))
        }));
        assert!(caught.is_err(), "the panic did not reach the caller");
        assert_eq!(writer.run(|| std::thread::current().id()).unwrap(), before);
    }
}
