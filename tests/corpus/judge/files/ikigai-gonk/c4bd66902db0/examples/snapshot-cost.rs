//! `cargo run --release --example snapshot-cost -- <backup.nq.gz> [page …]` — time gonk's
//! expensive pages over a READ-ONLY copy of a live store, loaded from a backup archive into
//! an in-memory dataset. Nothing here opens `~/.ikigai/store` or writes anywhere but RAM.
//!
//! ```sh
//! cargo run --release --example snapshot-cost -- ~/.ikigai/backups/gonk-store-2026-09-25T024013Z.nq.gz
//! cargo run --release --example snapshot-cost -- <archive> queue 'queue?group=file' 'ledger?status=open&limit=500'
//! ```
//!
//! A page is `queue[?query]` (the rows view, or `group=<kind>` for a batch tab) or
//! `ledger[?query]` (the default ledger's listing). With no page named, the set ledger #519
//! measured is run: the default rows view, the comment-shape and file tabs, the ledger list.
//! `GONK_SNAPSHOT_OUT=<dir>` writes each page's HTML into that directory (one file per page),
//! which is how "the output is the same HTML" is checked across a change: run it on both
//! trees and `diff` the directories. Each page is rendered twice: the second is the POLL,
//! and the `chunks`/`hits` columns say how many chunk renders the first put in the hub's
//! cache and whether the second was served entirely from them.
//!
//! The browse roots come from `gonk.browse.root` lines in `~/.config/ikigai/config.toml`, or
//! from `--root name=path` arguments, whichever is given; the findings face reads the files
//! it re-anchors against, so the paths should exist, and they are only ever read.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::config::QueuePolicy;
use ikigai_gonk::grants::{browse_graph_grants, grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::{browse, compose_with, doors, queue, quic, web};
use ikigai_store::DurableStore;

fn main() {
    let mut args = std::env::args().skip(1);
    let archive = PathBuf::from(
        args.next()
            .expect("usage: snapshot-cost <backup.nq.gz> [--root name=path …] [page …]"),
    );
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    let mut pages: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--root" {
            let spec = args.next().expect("--root name=path");
            let (name, path) = spec.split_once('=').expect("--root name=path");
            roots.push((name.to_string(), expand(path)));
        } else {
            pages.push(arg);
        }
    }
    if roots.is_empty() {
        roots = configured_roots();
    }
    if pages.is_empty() {
        pages = vec![
            "queue".to_string(),
            "queue?group=comment-shape".to_string(),
            "queue?group=file".to_string(),
            "ledger?status=open".to_string(),
            "ledger?status=open&limit=500".to_string(),
        ];
    }

    let t = Instant::now();
    let nquads = std::process::Command::new("gzip")
        .arg("-dc")
        .arg(&archive)
        .output()
        .expect("gzip -dc");
    assert!(nquads.status.success(), "gzip -dc {}", archive.display());
    println!(
        "archive          {:>9.1?}   {} bytes of N-Quads",
        t.elapsed(),
        nquads.stdout.len()
    );

    // The composition `main` builds for a gonk with browse roots, over an in-memory store.
    let graph = browse::Graph::chosen();
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let wired = browse::wire(roots.clone(), handle, None, None, &graph);
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        Vec::new(),
        None,
    ));
    let t = Instant::now();
    let loaded = issue(
        &hub,
        Verb::Sink,
        "urn:iki:store:load",
        &[
            ("content", nquads.stdout.as_slice()),
            ("format", b"application/n-quads"),
        ],
        &Capability::root(),
    );
    println!("store:load       {:>9.1?}   {}", t.elapsed(), loaded.trim());

    let config = tempfile::tempdir().expect("a config home");
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: roots.iter().map(|(name, _)| name.clone()).collect(),
        passkeys: Arc::new(Passkeys::new(
            quic::Layout::in_config_home(config.path()),
            1060,
        )),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: QueuePolicy::default(),
        epochs: None,
    });
    let door = doors::http_kernel(Arc::clone(&hub), web::space(face));

    // What `grants.json`'s reviewer holds: every root, the publish token, the ledger.
    let mut scopes = vec![
        ikigai_browse::CAP_WILDCARD.to_string(),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ];
    scopes.extend(grants_for_all(&["default".to_string()], Authority::Write).expect("tokens"));
    scopes.extend(browse_graph_grants(Authority::Write).expect("a named browse graph"));
    let reviewer = Capability::scoped(scopes);

    let out_dir = std::env::var_os("GONK_SNAPSHOT_OUT").map(PathBuf::from);
    if let Some(dir) = &out_dir {
        std::fs::create_dir_all(dir).expect("a writable output directory");
    }
    // Where the rest of a page's time goes: the findings reads, per root, uncached
    // (`Expiry::Always` from browse) — the rows read and the file-group read.
    println!("{:<40} {:>9}", "read (per root, uncached)", "ms");
    for (root, _) in &roots {
        for (label, args) in [
            ("findings", vec![("as", "application/json")]),
            (
                "findings group=file",
                vec![("as", "application/json"), ("group", "file")],
            ),
        ] {
            let args: Vec<(&str, &[u8])> = args.iter().map(|(k, v)| (*k, v.as_bytes())).collect();
            let t = Instant::now();
            let _ = issue(
                &door,
                Verb::Source,
                &format!("urn:repo:{root}:findings"),
                &args,
                &reviewer,
            );
            println!(
                "{:<40} {:>9.0}",
                format!("{root}: {label}"),
                t.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
    println!(
        "{:<40} {:>9} {:>10} {:>9} {:>7} {:>7}",
        "page", "first ms", "bytes", "poll ms", "chunks", "hits"
    );
    for page in &pages {
        let (iri, query) = match page.split_once('?') {
            Some((name, query)) => (name, query),
            None => (page.as_str(), ""),
        };
        let iri = match iri {
            "queue" => queue::QUEUE_IRI.to_string(),
            "ledger" => "urn:iki:gonk:page:ledger:default".to_string(),
            other => panic!("`{other}` is not `queue` or `ledger`"),
        };
        let params: Vec<(String, String)> = query
            .split('&')
            .filter(|kv| !kv.is_empty())
            .map(|kv| match kv.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (kv.to_string(), String::new()),
            })
            .collect();
        let args: Vec<(&str, &[u8])> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_bytes()))
            .collect();
        let before = cached_chunks(&hub);
        let t = Instant::now();
        let html = issue(&door, Verb::Source, &iri, &args, &reviewer);
        let elapsed = t.elapsed();
        let chunks = cached_chunks(&hub) - before;
        // The poll: the same page again, nothing changed. Every chunk should be a hit,
        // so the cache holds exactly as many render entries as before.
        let t = Instant::now();
        let again = issue(&door, Verb::Source, &iri, &args, &reviewer);
        let poll = t.elapsed();
        let hits = if cached_chunks(&hub) - before == chunks && again == html {
            "all"
        } else {
            "SOME MISSED"
        };
        println!(
            "{page:<40} {:>9.0} {:>10} {:>9.0} {chunks:>7} {hits:>7}",
            elapsed.as_secs_f64() * 1000.0,
            html.len(),
            poll.as_secs_f64() * 1000.0,
        );
        if let Some(dir) = &out_dir {
            let name: String = page
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            std::fs::write(dir.join(format!("{name}.html")), &html).expect("write the page");
        }
    }
}

/// How many `urn:iki:gonk:render` entries the hub's cache holds (`urn:kernel:cache`).
fn cached_chunks(hub: &Kernel) -> usize {
    issue(
        hub,
        Verb::Source,
        "urn:kernel:cache",
        &[],
        &Capability::root(),
    )
    .lines()
    .filter(|line| line.split_whitespace().next() == Some(ikigai_gonk::render::RENDER_IRI))
    .count()
}

fn issue(
    kernel: &Kernel,
    verb: Verb,
    iri: &str,
    args: &[(&str, &[u8])],
    cap: &Capability,
) -> String {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("an IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.to_vec())),
    );
    match block_on(kernel.issue(request, cap)) {
        Ok(repr) => String::from_utf8_lossy(&repr.bytes).into_owned(),
        Err(e) => panic!("{verb:?} {iri}: {e}"),
    }
}

/// `gonk.browse.root = "name=~/path"` lines of the operator's config, `~` expanded.
fn configured_roots() -> Vec<(String, PathBuf)> {
    let home = std::env::var_os("HOME").map(PathBuf::from).expect("HOME");
    let text = std::fs::read_to_string(home.join(".config/ikigai/config.toml"))
        .expect("~/.config/ikigai/config.toml, or --root name=path");
    // In the file's order: the rows view lists roots in configuration order, and a
    // re-ordered measurement would not be the live page.
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("gonk.browse.root") else {
            continue;
        };
        let Some(value) = rest.trim().strip_prefix('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        if let Some((name, path)) = value.split_once('=') {
            if !roots.iter().any(|(n, _)| n == name) {
                roots.push((name.to_string(), expand(path)));
            }
        }
    }
    roots
}

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => {
            let home = std::env::var_os("HOME").map(PathBuf::from).expect("HOME");
            home.join(rest)
        }
        None => PathBuf::from(path),
    }
}
