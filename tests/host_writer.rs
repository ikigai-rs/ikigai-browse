//! Two writers on one store can lose a browse record; the host's ONE writer cannot (ledger
//! #1252).
//!
//! # What happened
//!
//! A host that opens its store with `DurableStore::open_shared_declaring` (gonk does) hands
//! browse the raw `Arc<Store>`, and through 0.18.5 `Mount::annotations` was the only way in:
//! it builds browse's own `SharedWriter::over` on that handle, a second writer thread and a
//! second write lock beside the store's own. `ikigai-store` warns against exactly that: a
//! write through a store door can then lose an update made through `over`.
//!
//! # The reproduction
//!
//! RocksDB (the backing every durable host uses) gives a transaction a SNAPSHOT to read and a
//! write batch to commit, and nothing else: two transactions on two threads are not excluded
//! from each other, and the later commit applies its batch over whatever the earlier one did.
//! What keeps a read-modify-write whole is that every write to the store commits on ONE
//! writer thread under ONE lock.
//!
//! So the host's writer here runs a read-modify-write the way a store door does: it reads
//! browse's graph, and then removes what it read (a scoped `CLEAR GRAPH`, in effect). Between
//! the read and the commit it waits, and in that window browse rewrites an annotation:
//!
//! - **On browse's own writer** (`Mount::annotations`), the rewrite commits in the window, and
//!   the door's commit then removes the quads the rewrite stored unchanged: what is left is
//!   half a record, which no serial order of the two writes produces (one order leaves the
//!   whole new record, the other leaves nothing). That is the lost update.
//! - **On the host's writer** (`Mount::annotations_through`), the rewrite waits for the door,
//!   then commits whole.
//!
//! The window is held open on purpose, so neither outcome depends on timing: the first case
//! WAITS for browse's rewrite to finish before the door commits, and the second asserts the
//! rewrite has NOT finished while the door holds the writer, which nothing can make it do.
//!
//! The host's writer here is a `SharedWriter::over` the test makes itself, standing in for
//! `DurableStore::shared_writer` (the same type, one thread and one lock per writer): opening
//! a `DurableStore` on disk needs `ikigai-store`'s `persistent` feature, which this crate does
//! not otherwise build.
//!
//! ⚠ Why RocksDB and not the in-memory store: oxigraph's memory backend takes a lock of its
//! own for the life of every transaction, so there two transactions never overlap and this
//! shape cannot lose anything. The hazard there is narrower (a door that reads outside its
//! transaction, as `urn:iki:store:graph-update`'s copy does) and cannot be held open from
//! outside the door, so it is not reproduced here.
//!
//! The race needs RocksDB, so it runs with `--features migrate` (as CI does); the mount-time
//! checks after it run on the in-memory store, everywhere.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_store::{DurableStore, SharerWrites};
use oxigraph::model::NamedNode;
use oxigraph::store::Store;

const GRAPH: &str = "urn:iki:browse:graph:host-writer-test";

fn scratch(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "ikigai-browse-host-writer-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

/// A root with one file to annotate.
fn root(tag: &str) -> PathBuf {
    let root = scratch(&format!("{tag}-root"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("lib.rs"), "fn annotated_line() {}\n").unwrap();
    root
}

fn graph() -> NamedNode {
    NamedNode::new(GRAPH).unwrap()
}

/// Sink annotation `id` with `body`: a create the first time, a rewrite after.
fn annotate(kernel: &Kernel, id: &str, body: &str) {
    let request = [
        ("target", "urn:repo:demo:file:lib.rs"),
        ("exact", "fn annotated_line() {}"),
        ("body", body),
    ]
    .into_iter()
    .fold(
        Request::new(
            Verb::Sink,
            Iri::parse(format!("urn:iki:annotation:{id}")).unwrap(),
        ),
        |request, (name, value)| request.with_arg(name, ArgRef::Inline(value.as_bytes().to_vec())),
    );
    let cap = Capability::scoped([
        "urn:cap:browse:read:demo".to_string(),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ]);
    block_on(kernel.issue(request, &cap)).unwrap_or_else(|e| panic!("annotate {id}: {e}"));
}

/// The body annotation `id` reads back with.
fn body_of(kernel: &Kernel, id: &str) -> String {
    let request = Request::new(
        Verb::Source,
        Iri::parse(format!("urn:iki:annotation:{id}")).unwrap(),
    )
    .with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
    let cap = Capability::scoped(["urn:cap:browse:read:demo".to_string()]);
    let read = block_on(kernel.issue(request, &cap)).unwrap_or_else(|e| panic!("read {id}: {e}"));
    let json: serde_json::Value = serde_json::from_slice(&read.bytes).unwrap();
    json["body"].as_str().unwrap_or_default().to_string()
}

#[cfg(feature = "migrate")]
mod race {
    use super::*;
    use ikigai_store::{SharedWriter, StorageError};
    use oxigraph::model::{GraphName, GraphNameRef};
    use std::sync::mpsc;
    use std::time::Duration;

    /// A durable (RocksDB) store.
    fn durable(tag: &str) -> Arc<Store> {
        Arc::new(Store::open(scratch(&format!("{tag}-store"))).unwrap())
    }

    /// The writer the HOST commits through, declaring what it lets its sharer write.
    fn host_writer(store: &Arc<Store>) -> SharedWriter {
        SharedWriter::over(
            Arc::clone(store),
            SharerWrites::only_the_default_graph().and_named_graph(GRAPH),
        )
    }

    /// Every quad in browse's graph.
    fn quads_in_graph(store: &Store) -> usize {
        store
            .quads_for_pattern(
                None,
                None,
                None,
                Some(GraphNameRef::NamedNode(graph().as_ref())),
            )
            .count()
    }

    /// How many quads one whole annotation record is, measured rather than assumed.
    fn one_record() -> usize {
        let store = durable("measure");
        let kernel = Kernel::new(Arc::new(
            ikigai_browse::Mount::new([("demo".to_string(), root("measure"))])
                .annotations(Arc::clone(&store))
                .graph(graph())
                .space(),
        ));
        annotate(&kernel, "measured", "the first body");
        annotate(&kernel, "measured", "the edited body");
        quads_in_graph(&store)
    }

    /// The door: on the host's writer, read browse's graph, say so, wait for `go`, then
    /// remove what was read, in one transaction. Answers how many quads it read.
    fn door(
        host: SharedWriter,
        read: mpsc::Sender<()>,
        go: mpsc::Receiver<()>,
    ) -> std::thread::JoinHandle<Result<usize, StorageError>> {
        std::thread::spawn(move || {
            host.write_declared(move |txn| {
                let graph = GraphName::NamedNode(graph());
                let seen = txn
                    .quads_for_pattern(None, None, None, Some(graph.as_ref()))
                    .collect::<Result<Vec<_>, _>>()?;
                read.send(()).unwrap();
                go.recv().unwrap();
                for quad in &seen {
                    txn.remove(quad)?;
                }
                Ok(seen.len())
            })
        })
    }

    /// Rewrite annotation `id` on a thread of its own, reporting when it has committed.
    fn rewrite_in_the_background(
        kernel: &Arc<Kernel>,
        id: &'static str,
    ) -> (mpsc::Receiver<()>, std::thread::JoinHandle<()>) {
        let (done_tx, done_rx) = mpsc::channel();
        let kernel = Arc::clone(kernel);
        let rewriting = std::thread::spawn(move || {
            annotate(&kernel, id, "the edited body");
            done_tx.send(()).unwrap();
        });
        (done_rx, rewriting)
    }

    /// ★ THE REPRODUCTION (ledger #1252): browse's own writer beside the host's loses half a
    /// record to a door's read-modify-write. Pinned as it stands so the warning on
    /// `Mount::annotations` stays true: if this ever stops reproducing, that warning is
    /// stale.
    #[test]
    fn browse_own_writer_beside_the_hosts_loses_half_a_record() {
        let whole = one_record();
        let store = durable("over");
        let kernel = Arc::new(Kernel::new(Arc::new(
            ikigai_browse::Mount::new([("demo".to_string(), root("over"))])
                .annotations(Arc::clone(&store))
                .graph(graph())
                .space(),
        )));
        annotate(&kernel, "raced", "the first body");
        assert_eq!(quads_in_graph(&store), whole, "one record before the race");

        let (read_tx, read_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel();
        let door = door(host_writer(&store), read_tx, go_rx);
        read_rx.recv().unwrap();

        // browse's writer is not the host's, so the rewrite commits while the door waits.
        let (done, rewriting) = rewrite_in_the_background(&kernel, "raced");
        done.recv_timeout(Duration::from_secs(60))
            .expect("browse's own writer does not wait for the host's");
        rewriting.join().unwrap();

        go_tx.send(()).unwrap();
        assert_eq!(door.join().unwrap().unwrap(), whole);

        let left = quads_in_graph(&store);
        eprintln!("one record is {whole} quads; after the race, {left} remain");
        assert!(
            left > 0 && left < whole,
            "expected half a record (neither 0 nor {whole} quads, the two serial outcomes); \
             found {left}"
        );
    }

    /// ★ THE FIX (ledger #1252): through the host's writer the same rewrite waits for the
    /// door, then commits whole, so the record is the one a serial order produces.
    #[test]
    fn through_the_hosts_writer_the_rewrite_waits_and_lands_whole() {
        let whole = one_record();
        let store = durable("through");
        let host = host_writer(&store);
        let kernel = Arc::new(Kernel::new(Arc::new(
            ikigai_browse::Mount::new([("demo".to_string(), root("through"))])
                .annotations_through(host.clone())
                .graph(graph())
                .space(),
        )));
        annotate(&kernel, "raced", "the first body");
        assert_eq!(quads_in_graph(&store), whole, "one record before the race");

        let (read_tx, read_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel();
        let door = door(host, read_tx, go_rx);
        read_rx.recv().unwrap();

        // The door holds the one writer, so the rewrite cannot commit. A negative wait: no
        // load can make it finish early, only a regression can.
        let (done, rewriting) = rewrite_in_the_background(&kernel, "raced");
        assert!(
            done.recv_timeout(Duration::from_millis(300)).is_err(),
            "browse committed while the host's writer was held: it is not writing through it"
        );

        go_tx.send(()).unwrap();
        assert_eq!(door.join().unwrap().unwrap(), whole);
        rewriting.join().unwrap();

        assert_eq!(quads_in_graph(&store), whole, "the record is not whole");
        assert_eq!(body_of(&kernel, "raced"), "the edited body");
    }
}

/// The new constructor end to end on the in-memory store: an annotation stored through the
/// host's writer reads back, and lands in the graph the host declared.
#[test]
fn a_mount_through_the_hosts_writer_stores_and_reads_back() {
    let (store, handle) = DurableStore::in_memory_shared_declaring(
        SharerWrites::only_the_default_graph().and_named_graph(GRAPH),
    )
    .unwrap();
    let kernel = Kernel::new(Arc::new(
        ikigai_browse::Mount::new([("demo".to_string(), root("e2e"))])
            .annotations_through(store.shared_writer().unwrap())
            .graph(graph())
            .space(),
    ));
    annotate(&kernel, "e2e", "a note");
    assert_eq!(body_of(&kernel, "e2e"), "a note");
    assert!(handle.contains_named_graph(graph().as_ref()).unwrap());
}

/// The agreement check at the mount: a graph the host's declaration does not name is refused
/// when the space is built, naming both.
#[test]
#[should_panic(
    expected = "this mount writes graph <urn:iki:browse:graph:host-writer-test>, \
                           but the host's writer declares the default graph only"
)]
fn a_mount_whose_graph_the_host_did_not_declare_is_refused() {
    let (store, _handle) =
        DurableStore::in_memory_shared_declaring(SharerWrites::only_the_default_graph()).unwrap();
    let _ = ikigai_browse::Mount::new([("demo".to_string(), root("undeclared"))])
        .annotations_through(store.shared_writer().unwrap())
        .graph(graph())
        .space();
}

/// An `ExplainConfig` built on ANOTHER store than the writer's is refused: a mount writes one
/// store.
#[test]
#[should_panic(expected = "two different store handles")]
fn an_explain_config_on_another_store_is_refused() {
    let (store, _handle) =
        DurableStore::in_memory_shared_declaring(SharerWrites::only_the_default_graph()).unwrap();
    let elsewhere = Arc::new(Store::new().unwrap());
    let _ = ikigai_browse::Mount::new([("demo".to_string(), root("two-stores"))])
        .annotations_through(store.shared_writer().unwrap())
        .explain(ikigai_browse::ExplainConfig::new(elsewhere))
        .space();
}

/// …and one built on the writer's own store is the shape a host wires (gonk's: the
/// annotation family and the explanation family over one handle).
#[test]
fn an_explain_config_on_the_writers_store_is_accepted() {
    let (store, handle) = DurableStore::in_memory_shared_declaring(
        SharerWrites::only_the_default_graph().and_named_graph(GRAPH),
    )
    .unwrap();
    let writer = store.shared_writer().unwrap();
    assert!(Arc::ptr_eq(writer.store(), &handle));
    let kernel = Kernel::new(Arc::new(
        ikigai_browse::Mount::new([("demo".to_string(), root("one-store"))])
            .annotations_through(writer)
            .graph(graph())
            .explain(ikigai_browse::ExplainConfig::new(Arc::clone(&handle)))
            .space(),
    ));
    annotate(&kernel, "explained", "a note beside the explanations");
    assert_eq!(
        body_of(&kernel, "explained"),
        "a note beside the explanations"
    );
}
