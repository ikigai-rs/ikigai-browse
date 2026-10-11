//! A read right after many browse writes, each from a thread of its own, answers as fast
//! as the same read on a fresh copy of the same data (ledger #1131, after #1127).
//!
//! # What happened
//!
//! RocksDB draws each memtable skiplist node's height from a THREAD-LOCAL random generator
//! seeded from a hash of the thread's id (`Random::GetTLSInstance`). A thread spawned after
//! the previous one exited usually gets the SAME id back, so a host that runs each request on
//! a thread of its own (the socket door, `ikigai-ipc`, spawns one per connection) had every
//! browse write draw the same short run of heights: the skiplists stopped growing upper
//! levels and every later read walked them until the memtable flushed. `ikigai-store` 0.2.12
//! fixed its own doors (ikigai-rs/ikigai-store PR #32); browse writes through the RAW handle
//! a host injects, so it did not inherit that fix.
//!
//! **The fix** (`archive::Writer`): every quad browse inserts or removes commits on the
//! mount's one long-lived writer thread, whatever thread the request runs on.
//!
//! # What this file can and cannot promise
//!
//! Each side is the BEST of several reads, against a bound well above the fixed code's ~1x,
//! so a slow runner has to be slow on every read to fail it. It cannot catch the regression on
//! every run: how bad the degeneration gets depends on which seed a run reuses. The
//! deterministic half is `archive`'s unit test that every write commits on one thread whatever
//! thread asked (`every_write_commits_on_one_thread_whatever_thread_asks`). This file measures
//! the symptom; that one pins the mechanism.
//!
//! The measurement, kept so it can be re-run when oxigraph or RocksDB moves:
//!
//! ```text
//! cargo test --features migrate --test post_bulk_read -- --ignored --nocapture
//! WRITES=1500 cargo test --release --features migrate --test post_bulk_read -- --ignored --nocapture
//! ```
#![cfg(feature = "migrate")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use oxigraph::io::RdfFormat;
use oxigraph::model::Term;
use oxigraph::store::Store;

/// The most a read right after the writes may take, as a multiple of the same read on a
/// fresh copy of the same data. The fixed code measures about 1x.
const MAX_RATIO: f64 = 3.0;

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "ikigai-browse-post-bulk-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

/// A root with one file whose every line is a distinct anchor.
fn root(tag: &str, lines: usize) -> PathBuf {
    let dir = scratch(&format!("{tag}-root"));
    std::fs::create_dir_all(&dir).unwrap();
    let text: String = (0..lines)
        .map(|i| format!("fn line_number_{i:06}() {{ /* a line long enough to anchor */ }}\n"))
        .collect();
    std::fs::write(dir.join("lib.rs"), text).unwrap();
    dir
}

fn kernel(root: &Path, store: &Arc<Store>) -> Arc<Kernel> {
    Arc::new(Kernel::new(Arc::new(
        ikigai_browse::Mount::new([("demo".to_string(), root.to_path_buf())])
            .annotations(Arc::clone(store))
            .space(),
    )))
}

/// One annotation, issued the way a socket door issues it: on a thread spawned for the
/// request, which exits when the request is answered.
fn annotate_on_a_fresh_thread(kernel: &Arc<Kernel>, i: usize) {
    let kernel = Arc::clone(kernel);
    std::thread::spawn(move || annotate(&kernel, i))
        .join()
        .unwrap();
}

/// One annotation, on the calling thread.
fn annotate(kernel: &Kernel, i: usize) {
    let request = [
        ("target", "urn:repo:demo:file:lib.rs".to_string()),
        ("exact", format!("fn line_number_{i:06}()")),
        (
            "body",
            format!("note number {i:06}, long enough for the string table"),
        ),
    ]
    .into_iter()
    .fold(
        Request::new(
            Verb::Sink,
            Iri::parse(format!("urn:iki:annotation:postbulk{i:06}")).unwrap(),
        ),
        |request, (name, value)| request.with_arg(name, ArgRef::Inline(value.into_bytes())),
    );
    let cap = Capability::scoped([
        "urn:cap:browse:read:demo".to_string(),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ]);
    block_on(kernel.issue(request, &cap)).unwrap_or_else(|e| panic!("annotation {i}: {e}"));
}

/// The best of `rounds` full scans that decode every term: each term is a lookup in the
/// store's string table, which is exactly where a degenerated skiplist costs the time.
fn best_read(store: &Store, rounds: usize) -> Duration {
    (0..rounds)
        .map(|_| {
            let started = Instant::now();
            let mut bytes = 0usize;
            for quad in store.iter() {
                let quad = quad.unwrap();
                bytes += quad.subject.to_string().len() + quad.predicate.as_str().len();
                bytes += match &quad.object {
                    Term::Literal(l) => l.value().len(),
                    other => other.to_string().len(),
                };
            }
            assert!(bytes > 0);
            started.elapsed()
        })
        .min()
        .unwrap()
}

struct Measured {
    writes: Duration,
    post_bulk: Duration,
    fresh: Duration,
}

impl Measured {
    fn ratio(&self) -> f64 {
        self.post_bulk.as_secs_f64() / self.fresh.as_secs_f64()
    }
}

/// `fresh` writes each annotation from a thread of its own; `same` writes them all from
/// the calling thread, the control.
fn measure(tag: &str, writes: usize, rounds: usize, threads: &str) -> Measured {
    let root = root(tag, writes);
    let (bulk_path, fresh_path) = (
        scratch(&format!("{tag}-bulk")),
        scratch(&format!("{tag}-fresh")),
    );
    let store = Arc::new(Store::open(&bulk_path).unwrap());
    let kernel = kernel(&root, &store);
    let started = Instant::now();
    for i in 0..writes {
        match threads {
            "same" => annotate(&kernel, i),
            _ => annotate_on_a_fresh_thread(&kernel, i),
        }
    }
    let writes_took = started.elapsed();
    let post_bulk = best_read(&store, rounds);

    let mut quads = Vec::new();
    store.dump_to_writer(RdfFormat::NQuads, &mut quads).unwrap();
    let copy = Store::open(&fresh_path).unwrap();
    copy.load_from_slice(RdfFormat::NQuads, &quads).unwrap();
    assert_eq!(copy.len().unwrap(), store.len().unwrap());
    let fresh = best_read(&copy, rounds);
    drop((kernel, store, copy));
    for path in [root, bulk_path, fresh_path] {
        let _ = std::fs::remove_dir_all(path);
    }
    Measured {
        writes: writes_took,
        post_bulk,
        fresh,
    }
}

/// ★ THE REGRESSION (ledger #1131): after 1,500 annotations, each written from a thread of
/// its own, a read answers within [`MAX_RATIO`] times what it takes on a fresh copy.
#[test]
fn a_read_after_writes_from_fresh_threads_answers_like_a_fresh_copy() {
    let measured = measure("pin", 1_500, 3, "fresh");
    assert!(
        measured.ratio() <= MAX_RATIO,
        "a read right after 1,500 annotations, each from a fresh thread, took {:?}, {:.1}x the \
         {:?} it takes on a fresh copy of the same data (the bound is {MAX_RATIO}x). This is the \
         ledger #1131 shape: see this file's header, and check that every browse write still \
         commits on the archive's writer thread",
        measured.post_bulk,
        measured.ratio(),
        measured.fresh,
    );
}

/// The measurement behind this file's header. `WRITES` (default 750), `ROUNDS` (default
/// 3) and `THREADS` (`fresh`, the default, or `same`, the control) set it.
#[test]
#[ignore = "a measurement: run with --ignored --nocapture"]
fn measure_a_read_after_writes_from_fresh_threads() {
    let var = |name: &str, default: &str| std::env::var(name).unwrap_or(default.to_string());
    let writes: usize = var("WRITES", "750").parse().unwrap();
    let rounds: usize = var("ROUNDS", "3").parse().unwrap();
    let threads = var("THREADS", "fresh");
    let measured = measure("measure", writes, rounds, &threads);
    println!(
        "{} build, {writes} annotations from {threads} threads in {:?}: read after them {:?}, \
         on a fresh copy {:?} ({:.2}x)",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        measured.writes,
        measured.post_bulk,
        measured.fresh,
        measured.ratio(),
    );
}
