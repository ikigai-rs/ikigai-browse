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
//! # How a write commits: one record, one transaction, one thread
//!
//! Every write goes through [`Archive::write`], which runs it as **one transaction** on the
//! archive's [`SharedWriter`] (`ikigai-store`, ledger #1207):
//!
//! - **A record is one commit.** An annotation, a decision, an explanation, a review pass, a
//!   region memo, a verdict: each is stored (or replaced, or removed) in one transaction, so a
//!   reader sees all of it or none of it. Before 0.18.5 every quad was a commit of its own, 8
//!   to 34 per record, and a rewrite removed the old record before it stored the new one, so a
//!   reader in between found no record at all (`every_record_is_one_commit` and
//!   `a_reader_never_sees_a_record_missing_or_half_rewritten` pin both).
//! - **On one long-lived writer thread**, never the thread the request happens to run on
//!   (ledger #1131). RocksDB draws each memtable skiplist node's height from a thread-local
//!   generator seeded from a hash of the thread's id, and a thread spawned after the previous
//!   one exited usually gets the same id back. A host that runs each request on a thread of
//!   its own (the socket door spawns one per connection) therefore had every browse write
//!   draw the same few heights, and every later read walked the degenerated skiplists until
//!   the memtable flushed: `tests/post_bulk_read.rs` measured 6x to 68x after 750
//!   annotations, against 1.0x when the same writes came from one thread. 0.18.4 carried a
//!   writer thread of its own for this; the store's `SharedWriter::over` is the same thread,
//!   kept in one place for every raw-handle host. Reads stay on the caller's thread.
//! - **Only into the graphs this mount writes.** The writer is declared with the default graph
//!   (always writable) and, for a mount that names one, that graph; a write anywhere else is
//!   refused and the whole transaction rolled back. [`Record::insert`]'s debug assertion
//!   catches the same mistake earlier, in every test.
//!
//! # Whose writer: browse's own, or the host's (ledger #1252)
//!
//! [`Archive::new`] builds browse's OWN writer (`SharedWriter::over`) on the handle it is
//! given, which is right for a host that opened the `Store` itself and holds nothing else.
//! It is WRONG for a handle a `DurableStore` gave out (`open_shared_declaring`, gonk's case):
//! that store has a writer thread and a write lock of its own, and a second pair beside it
//! orders nothing against it. On RocksDB a transaction reads a snapshot and commits a batch,
//! with no exclusion between two threads, so a door's read-modify-write can then commit over a
//! browse record written in between and leave half of it (`tests/host_writer.rs` reproduces
//! exactly that). [`Archive::through`] takes the host's `DurableStore::shared_writer` instead,
//! so every browse write commits on the store's one thread under its one lock.

use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

use ikigai_store::{DeclaredTransaction, SharedWriter, SharerWrites};
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
    /// Where every [`Archive::write`] commits; see the module doc. Its thread starts on the
    /// first write, so an archive that never writes (the one [`crate::ExplainConfig::new`]
    /// builds before a mount replaces it) never starts one.
    writer: SharedWriter,
    /// The thread each write committed on, in order: what the tests pin.
    #[cfg(test)]
    committed_on: Arc<Mutex<Vec<std::thread::ThreadId>>>,
}

impl Archive {
    /// Bind `store`'s `graph` — `GraphName::DefaultGraph` for a host that has
    /// not asked for a named one — writing through a writer of browse's OWN (see the module
    /// doc for when that is wrong, and [`Archive::through`] for the other way).
    pub(crate) fn new(store: Arc<Store>, graph: GraphName) -> Self {
        // The default graph is always writable (it has no IRI to declare). A blank-node graph
        // can be declared by nothing, so a mount bound to one would have every write refused;
        // no `Mount` builds one.
        let declared = match &graph {
            GraphName::NamedNode(iri) => {
                SharerWrites::only_the_default_graph().and_named_graph(iri.as_str())
            }
            GraphName::DefaultGraph | GraphName::BlankNode(_) => {
                SharerWrites::only_the_default_graph()
            }
        };
        Archive {
            writer: SharedWriter::over(Arc::clone(&store), declared),
            store,
            graph,
            #[cfg(test)]
            committed_on: Arc::default(),
        }
    }

    /// Bind `graph` in the store `writer` writes, committing through THAT writer: the host's
    /// (`DurableStore::shared_writer`), so browse's writes share the store's one writer thread
    /// and its one write lock (ledger #1252).
    ///
    /// Refused, naming both, when the writer carries a declaration that does not let it write
    /// `graph`: every browse write would be refused at the store, one record at a time, long
    /// after the host built the mount. A declaration that names MORE than `graph` is accepted:
    /// it is the host's promise about everything that writes through its handle, and a wider
    /// one costs only cacheability. A writer that declares nothing refuses nothing.
    pub(crate) fn through(writer: SharedWriter, graph: GraphName) -> Result<Self, String> {
        if let Some(declared) = writer.declared() {
            let agrees = match &graph {
                GraphName::DefaultGraph => true,
                GraphName::NamedNode(iri) => declared.may_write(iri.as_str()),
                // No declaration can name a blank-node graph, and no `Mount` builds one.
                GraphName::BlankNode(_) => false,
            };
            if !agrees {
                return Err(format!(
                    "browse: this mount writes {}, but the host's writer declares {}, so every \
                     browse write would be refused. Declare the mount's graph where the store \
                     is opened (`SharerWrites::and_named_graph`), or give the mount the graph \
                     the store declares (`Mount::graph`)",
                    describe_graph(&graph),
                    describe_declared(declared),
                ));
            }
        }
        Ok(Archive {
            store: Arc::clone(writer.store()),
            writer,
            graph,
            #[cfg(test)]
            committed_on: Arc::default(),
        })
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

    /// Run `record` as ONE transaction on the writer thread, and commit it if it returns `Ok`
    /// (see the module doc). On `Err`, or a write outside the mount's graphs, nothing it asked
    /// for is written.
    ///
    /// `record` runs on the writer thread, so it takes what it needs by value and must not
    /// write through this archive itself: the store refuses a write asked for from inside one.
    pub(crate) fn write<T: Send + 'static>(
        &self,
        record: impl FnOnce(&mut Record<'_, '_>) -> Result<T, StorageError> + Send + 'static,
    ) -> Result<T, StorageError> {
        let graph = self.graph.clone();
        #[cfg(test)]
        let committed_on = Arc::clone(&self.committed_on);
        self.writer
            .write_declared(move |txn: &mut DeclaredTransaction<'_>| {
                #[cfg(test)]
                committed_on
                    .lock()
                    .unwrap()
                    .push(std::thread::current().id());
                record(&mut Record { txn, graph: &graph })
            })
    }

    /// Store `quads` — one record's, each built with [`Archive::graph`] — as one transaction.
    pub(crate) fn insert_all(&self, quads: Vec<Quad>) -> Result<(), StorageError> {
        self.write(move |record| quads.iter().try_for_each(|quad| record.insert(quad)))
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

    /// How many writes have committed through this archive: what the record pins count.
    #[cfg(test)]
    pub(crate) fn commits(&self) -> usize {
        self.committed_on.lock().unwrap().len()
    }
}

/// A graph name in a sentence, for a refusal.
fn describe_graph(graph: &GraphName) -> String {
    match graph {
        GraphName::DefaultGraph => "the default graph".to_string(),
        GraphName::NamedNode(iri) => format!("graph <{}>", iri.as_str()),
        GraphName::BlankNode(node) => format!("blank-node graph {node}"),
    }
}

/// A declaration in a sentence, for a refusal.
fn describe_declared(declared: &SharerWrites) -> String {
    if declared.named_graphs().len() == 0 {
        "the default graph only".to_string()
    } else {
        format!(
            "the default graph and {}",
            declared
                .named_graphs()
                .map(|g| format!("<{g}>"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// One record's transaction, confined to the mount's graph the way [`Archive`] is: a read
/// sees only that graph (and this transaction's own writes), and a write must be built with
/// it.
pub(crate) struct Record<'t, 'a> {
    txn: &'t mut DeclaredTransaction<'a>,
    graph: &'t GraphName,
}

impl Record<'_, '_> {
    /// Add a quad built with [`Archive::graph`]. The debug assertion is the cheap half of the
    /// confinement claim, as it has been since 0.4.0: in a debug build (every test and every
    /// CI run) a quad carrying some other graph name fails loudly here. In a release build the
    /// writer's declaration refuses it, and the record with it.
    pub(crate) fn insert(&mut self, quad: &Quad) -> Result<(), StorageError> {
        debug_assert_eq!(
            &quad.graph_name, self.graph,
            "browse: a quad not built with Archive::graph() reached Record::insert"
        );
        self.txn.insert(quad)
    }

    /// Remove a quad, in practice one [`Record::quads_for_pattern`] found.
    pub(crate) fn remove(&mut self, quad: &Quad) -> Result<(), StorageError> {
        self.txn.remove(quad)
    }

    /// Every matching quad in the mount's graph, as this transaction sees it.
    pub(crate) fn quads_for_pattern(
        &self,
        subject: Option<NamedOrBlankNodeRef<'_>>,
        predicate: Option<NamedNodeRef<'_>>,
        object: Option<TermRef<'_>>,
    ) -> Result<Vec<Quad>, StorageError> {
        self.txn
            .quads_for_pattern(subject, predicate, object, Some(self.graph.as_ref()))
            .collect()
    }

    /// Remove every quad whose subject is `subject`, in the mount's graph.
    pub(crate) fn remove_subject(
        &mut self,
        subject: NamedOrBlankNodeRef<'_>,
    ) -> Result<(), StorageError> {
        for quad in self.quads_for_pattern(Some(subject), None, None)? {
            self.remove(&quad)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxigraph::model::NamedNode;
    use std::thread::ThreadId;

    /// ★ THE PIN (ledger #1131): writes asked for from many short-lived threads, the way a
    /// socket door asks, all commit on ONE thread that is none of them, inserts and removes
    /// alike, and land in order. Fails if a write stops going through the writer (nothing is
    /// recorded) or the writer runs work where it is asked (the threads match).
    #[test]
    fn writes_from_fresh_threads_all_commit_on_one_writer_thread() {
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
                archive
                    .write(move |record| {
                        if remove {
                            record.remove(&quad)
                        } else {
                            record.insert(&quad)
                        }
                    })
                    .unwrap();
                std::thread::current().id()
            })
            .join()
            .unwrap()
        };
        let mut asked = Vec::new();
        for round in 0..4 {
            asked.push(from_a_fresh_thread(false));
            assert!(archive.store().contains(&quad).unwrap(), "round {round}");
            asked.push(from_a_fresh_thread(true));
            assert!(!archive.store().contains(&quad).unwrap(), "round {round}");
        }

        let committed_on = archive.committed_on.lock().unwrap().clone();
        assert_eq!(
            committed_on.len(),
            asked.len(),
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
        assert_ne!(committed_on[0], std::thread::current().id());
    }

    /// A panic in a write reaches the caller, writes nothing, and the writer lives on: the
    /// next write commits on the same thread.
    #[test]
    fn a_panicking_write_reaches_its_caller_and_the_writer_lives_on() {
        let archive = Archive::new(Arc::new(Store::new().unwrap()), GraphName::DefaultGraph);
        let quad = Quad::new(
            NamedNode::new("urn:ikigai:browse:test:s").unwrap(),
            NamedNode::new("urn:ikigai:browse:test:p").unwrap(),
            NamedNode::new("urn:ikigai:browse:test:o").unwrap(),
            GraphName::DefaultGraph,
        );
        let on_writer =
            || -> ThreadId { archive.write(|_| Ok(std::thread::current().id())).unwrap() };
        let before = on_writer();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let quad = quad.clone();
            archive.write(move |record| -> Result<(), StorageError> {
                record.insert(&quad)?;
                panic!("a write that panics")
            })
        }));
        assert!(caught.is_err(), "the panic did not reach the caller");
        assert!(
            !archive.store().contains(&quad).unwrap(),
            "a panicked record was committed"
        );
        assert_eq!(on_writer(), before);
    }

    /// A record that fails part way writes nothing: the quads it stored before the failure
    /// are rolled back with it.
    #[test]
    fn a_record_that_fails_part_way_writes_nothing() {
        let archive = Archive::new(Arc::new(Store::new().unwrap()), GraphName::DefaultGraph);
        let quad = Quad::new(
            NamedNode::new("urn:ikigai:browse:test:s").unwrap(),
            NamedNode::new("urn:ikigai:browse:test:p").unwrap(),
            NamedNode::new("urn:ikigai:browse:test:o").unwrap(),
            GraphName::DefaultGraph,
        );
        let refused = archive.write(move |record| {
            record.insert(&quad)?;
            Err::<(), _>(StorageError::Io(std::io::Error::other(
                "the next quad failed",
            )))
        });
        assert!(refused.is_err());
        assert!(
            archive.store().is_empty().unwrap(),
            "a failed record left quads behind"
        );
    }

    /// A write outside the graphs the mount writes is refused by the writer, and rolls back
    /// the record it was part of. The bad quad goes straight to the transaction, past
    /// `Record::insert`'s debug assertion (the other half of the same claim), because a
    /// release build has no assertion and this refusal is all there is.
    #[test]
    fn a_write_outside_the_mounts_graph_is_refused_with_its_record() {
        let mine = NamedNode::new("urn:ikigai:browse:test:mine").unwrap();
        let archive = Archive::new(
            Arc::new(Store::new().unwrap()),
            GraphName::NamedNode(mine.clone()),
        );
        let node = NamedNode::new("urn:ikigai:browse:test:s").unwrap();
        let (inside, outside) = (
            Quad::new(node.clone(), node.clone(), node.clone(), mine),
            Quad::new(
                node.clone(),
                node.clone(),
                node,
                NamedNode::new("urn:ikigai:browse:test:other").unwrap(),
            ),
        );
        let refused = archive.write(move |record| {
            record.insert(&inside)?;
            record.txn.insert(&outside)
        });
        assert!(ikigai_store::UndeclaredGraph::of(&refused.unwrap_err()).is_some());
        assert!(archive.store().is_empty().unwrap());
    }

    /// One of each record browse writes, as the faces build them. The fixtures only need to
    /// be well-formed: what the pins below count is commits and what a reader can see.
    mod records {
        use crate::annotate::{
            record_iri, revision_iri, Annotation, Decision, Family, Made, Outcome,
        };
        use crate::explain::ArchiveEntry;
        use crate::judge::{Answer, Cannot, Verdict};
        use crate::review::{PassEntry, RegionEntry};

        pub(super) const AT: &str = "2026-10-11T00:00:00Z";

        pub(super) fn annotation(family: Family, id: &str, body: &str) -> Annotation {
            Annotation {
                family,
                id: id.to_string(),
                body: body.to_string(),
                target_iri: "urn:repo:demo:file:a.rs".to_string(),
                repo: "demo".to_string(),
                rel: "a.rs".to_string(),
                hash: "sha256:00".to_string(),
                prefix: "// a\\n".to_string(),
                exact: "fn target()".to_string(),
                suffix: " {}".to_string(),
                start: 5,
                end: 16,
                created: Some(AT.to_string()),
                reanchored: false,
                orphaned: false,
                creator: Some("someone".to_string()),
                motivation: None,
                generated_by: None,
                severity: (family == Family::Finding).then(|| "major".to_string()),
                derived_from: None,
                decision: None,
                history: Vec::new(),
                prior: None,
                superseded_by: None,
                judges: Vec::new(),
            }
        }

        pub(super) fn decision(finding_id: &str, minted: &str) -> Decision {
            Decision {
                iri: revision_iri(finding_id, 0),
                outcome: Outcome::Published,
                severity: Some("major".to_string()),
                at: Some(AT.to_string()),
                revises: None,
                made: Some(Made::Single),
                confirmed: false,
                burst: None,
                note: Some("a note".to_string()),
                reason: None,
                minted: Some(minted.to_string()),
                reproduced: true,
            }
        }

        pub(super) fn entry() -> ArchiveEntry {
            ArchiveEntry {
                iri: crate::explain::entry_iri("demo", "a.rs", "sha256:00", "file-v1@m"),
                repo: "demo".to_string(),
                rel: "a.rs".to_string(),
                target_iri: "urn:repo:demo:file:a.rs".to_string(),
                hash: "sha256:00".to_string(),
                tag: "file-v1@m".to_string(),
                model: "m".to_string(),
                kind: "code".to_string(),
                text: "an explanation".to_string(),
                derived_at: Some(AT.to_string()),
            }
        }

        pub(super) fn pass(minted: &str) -> PassEntry {
            PassEntry {
                iri: crate::review::pass_iri("demo", "a.rs", "sha256:00", "review-v1@m"),
                repo: "demo".to_string(),
                rel: "a.rs".to_string(),
                target_iri: "urn:repo:demo:file:a.rs".to_string(),
                hash: "sha256:00".to_string(),
                tag: "review-v1@m".to_string(),
                model: "m".to_string(),
                minted: vec![minted.to_string()],
                carried: Vec::new(),
                reused_regions: Vec::new(),
                derived_regions: Vec::new(),
                memo_bytes: 0,
                orphaned_items: 0,
                suppressed_items: 0,
                reviewed_bytes: Some(16),
                total_bytes: Some(16),
                derived_at: Some(AT.to_string()),
                superseded: 0,
            }
        }

        pub(super) fn region(minted: &str, pass: &str) -> RegionEntry {
            RegionEntry {
                iri: crate::review::region_iri("demo", "a.rs", "sha256:00", "review-v1@m"),
                tag: "review-v1@m".to_string(),
                hash: "sha256:00".to_string(),
                len: 16,
                findings: vec![minted.to_string()],
                generated_by: Some(pass.to_string()),
            }
        }

        pub(super) fn cannot(finding: &str) -> Cannot {
            Cannot {
                iri: crate::judge::verdict_iri(finding, "judge-v2@m"),
                reason: "the version it was reviewed against is gone".to_string(),
                basis: "sha256:00".to_string(),
                tag: "judge-v2@m".to_string(),
                model: "m".to_string(),
                at: Some(AT.to_string()),
            }
        }

        pub(super) fn verdict(finding: &str) -> Verdict {
            Verdict {
                iri: crate::judge::verdict_iri(finding, "judge-v2@m"),
                verdict: "refuted".to_string(),
                stated: Some("refuted".to_string()),
                tag: "judge-v2@m".to_string(),
                model: "m".to_string(),
                judged_at: Some(AT.to_string()),
                test_code: false,
                answers: vec![Answer {
                    question: "code".to_string(),
                    answer: "no".to_string(),
                    reason: "it is not".to_string(),
                }],
                raw: "the raw answer".to_string(),
            }
        }

        pub(super) fn finding_iri(id: &str) -> String {
            record_iri(Family::Finding, id)
        }
    }

    /// ★ THE RECORD PIN (ledger #1207): every record browse stores, replaces or removes is ONE
    /// commit, so a reader sees all of it or none of it. Before 0.18.5 each quad was a commit
    /// of its own: an annotation took one per quad, and a rewrite one per quad removed plus
    /// one per quad stored, with the record absent or half-written in between.
    #[test]
    fn every_record_is_one_commit() {
        use crate::annotate::{
            append_decision, remove_annotation, rewrite_annotation, store_annotation, Family,
        };
        let archive = Archive::new(Arc::new(Store::new().unwrap()), GraphName::DefaultGraph);
        let mut counted: Vec<(&str, usize)> = Vec::new();
        let mut count = |label: &'static str, write: &mut dyn FnMut()| {
            let before = archive.commits();
            write();
            counted.push((label, archive.commits() - before));
        };
        let finding = records::annotation(Family::Finding, "f1", "a claim");
        let finding_iri = records::finding_iri("f1");
        let mut published = records::annotation(Family::Annotation, "f1", "a claim");
        count("store a finding", &mut || {
            store_annotation(&archive, &finding).unwrap()
        });
        count("store an annotation", &mut || {
            store_annotation(&archive, &published).unwrap()
        });
        published.body = "a claim, reworded".to_string();
        count("rewrite an annotation", &mut || {
            rewrite_annotation(&archive, &published).unwrap()
        });
        count("append a decision", &mut || {
            let decision = records::decision("f1", &published.iri());
            append_decision(&archive, &finding_iri, &decision).unwrap()
        });
        count("remove an annotation", &mut || {
            remove_annotation(&archive, Family::Annotation, "f1").unwrap()
        });
        count("store an explanation", &mut || {
            crate::explain::store_entry(&archive, &records::entry()).unwrap()
        });
        let pass = records::pass(&finding_iri);
        count("store a review pass", &mut || {
            crate::review::store_pass(&archive, &pass).unwrap()
        });
        count("store a region memo", &mut || {
            let region = records::region(&finding_iri, &pass.iri);
            crate::review::store_region(&archive, &region, "demo", "a.rs", "m").unwrap()
        });
        count("store a cannot", &mut || {
            crate::judge::store_cannot(&archive, &finding_iri, &records::cannot(&finding_iri))
                .unwrap()
        });
        count("store a verdict over a cannot", &mut || {
            crate::judge::store_verdict(&archive, &finding_iri, &records::verdict(&finding_iri))
                .unwrap()
        });
        assert!(
            counted.iter().all(|(_, commits)| *commits == 1),
            "a record took other than one commit: {counted:?}"
        );
    }

    /// ★ A rewrite is atomic (ledger #1207): a reader looking while a record is replaced, over
    /// and over, finds it every time, in its old form or its new one. Before 0.18.5 the old
    /// quads were removed one commit at a time and the new ones stored after, so a reader in
    /// the gap found no record, or one with some of its quads.
    #[test]
    fn a_reader_never_sees_a_record_missing_or_half_rewritten() {
        use crate::annotate::{load_annotation, rewrite_annotation, store_annotation, Family};
        use std::sync::atomic::{AtomicBool, Ordering};
        let archive = Arc::new(Archive::new(
            Arc::new(Store::new().unwrap()),
            GraphName::DefaultGraph,
        ));
        let mut ann = records::annotation(Family::Annotation, "n", "body 0");
        store_annotation(&archive, &ann).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let (archive, done) = (Arc::clone(&archive), Arc::clone(&done));
            std::thread::spawn(move || {
                let (mut reads, mut torn) = (0usize, Vec::new());
                while !done.load(Ordering::Relaxed) || reads == 0 {
                    reads += 1;
                    match load_annotation(&archive, "n").unwrap() {
                        None => torn.push("missing".to_string()),
                        Some(seen) if seen.exact != "fn target()" || seen.end != 16 => {
                            torn.push(format!("half-written: {seen:?}"))
                        }
                        Some(_) => {}
                    }
                }
                (reads, torn)
            })
        };
        for n in 1..=300 {
            ann.body = format!("body {n}");
            rewrite_annotation(&archive, &ann).unwrap();
        }
        done.store(true, Ordering::Relaxed);
        let (reads, torn) = reader.join().unwrap();
        assert!(
            torn.is_empty(),
            "{} of {reads} reads saw a torn record, first: {}",
            torn.len(),
            torn[0]
        );
    }

    /// The host's writer (ledger #1252): an archive built [`Archive::through`] a store's
    /// `shared_writer` commits on the SAME thread as the host's own writes, where one built
    /// [`Archive::new`] on the same handle commits on a second thread of its own.
    #[test]
    fn an_archive_through_the_hosts_writer_commits_on_the_hosts_thread() {
        use ikigai_store::DurableStore;
        let (store, handle) =
            DurableStore::in_memory_shared_declaring(SharerWrites::only_the_default_graph())
                .unwrap();
        let host = store.shared_writer().expect("a shared store has a writer");
        let host_thread: ThreadId = host
            .write_declared(|_| Ok::<_, StorageError>(std::thread::current().id()))
            .unwrap();

        let through = Archive::through(host.clone(), GraphName::DefaultGraph).unwrap();
        assert!(Arc::ptr_eq(through.store(), &handle));
        let through_thread = through.write(|_| Ok(std::thread::current().id())).unwrap();
        assert_eq!(
            through_thread, host_thread,
            "browse wrote off the host's thread"
        );

        let own = Archive::new(Arc::clone(&handle), GraphName::DefaultGraph);
        let own_thread = own.write(|_| Ok(std::thread::current().id())).unwrap();
        assert_ne!(
            own_thread, host_thread,
            "browse's own writer is a second thread: the reason `through` exists"
        );
    }

    /// The agreement check (ledger #1252): a mount whose graph the host's declaration does not
    /// name is refused at construction, and the refusal names both.
    #[test]
    fn an_archive_through_a_writer_that_does_not_declare_its_graph_is_refused() {
        use ikigai_store::DurableStore;
        let (store, _handle) = DurableStore::in_memory_shared_declaring(
            SharerWrites::only_the_default_graph().and_named_graph("urn:iki:test:declared"),
        )
        .unwrap();
        let host = store.shared_writer().unwrap();
        let named = |iri: &str| GraphName::NamedNode(NamedNode::new(iri).unwrap());

        let refused = Archive::through(host.clone(), named("urn:iki:test:mounted"))
            .err()
            .expect("an undeclared graph was accepted");
        assert!(
            refused.contains("graph <urn:iki:test:mounted>")
                && refused.contains("the default graph and <urn:iki:test:declared>"),
            "the refusal does not name both: {refused}"
        );

        // What agrees is accepted: the declared graph, and the default graph, which every
        // declaration lets its sharer write.
        assert!(Archive::through(host.clone(), named("urn:iki:test:declared")).is_ok());
        assert!(Archive::through(host, GraphName::DefaultGraph).is_ok());
    }

    /// A store opened with `open_shared` declares nothing, so its writer refuses nothing, and
    /// there is nothing for a mount's graph to disagree with.
    #[test]
    fn an_archive_through_a_writer_that_declares_nothing_takes_any_graph() {
        let (store, _handle) = ikigai_store::DurableStore::in_memory_shared().unwrap();
        let host = store.shared_writer().unwrap();
        assert!(host.declared().is_none());
        let graph = GraphName::NamedNode(NamedNode::new("urn:iki:test:anything").unwrap());
        assert!(Archive::through(host, graph).is_ok());
    }
}
