//! The backup family, walked over the composition `main` serves.
//!
//! ⚠ **This file composes WITH the backup family**, which `tests/conformance.rs`
//! deliberately does not: that one asserts the store-and-ledger catalog exactly, and
//! keeping the two apart is what lets it go on doing so. The same split
//! `tests/browse.rs` uses for the browse composition.
//!
//! The load-bearing test here is [`a_backup_restores_to_the_same_graphs_and_per_graph_counts`]
//! — a backup that has not been restored is not a backup, and a total quad count is exactly
//! the number a Turtle dump preserves while destroying the graph partition. Everything
//! compared here is compared PER GRAPH.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_conformance::{Fixture, Suite};
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use ikigai_gonk::backup::{self, Backups};
use ikigai_gonk::{compose_with, doors};
use ikigai_store::DurableStore;

/// The stopgap the hub took on 2026-09-16, through `urn:iki:store:select` with no downtime,
/// and recorded as a backup on the strength of STRUCTURAL checks alone — 2850 rows became
/// 2850 terminated, graph-qualified lines, and 290 item IRIs matched the live store exactly.
/// It was never loaded, because the installed `ikigai` carries no `store` feature.
///
/// ★ Loading it is the first thing this arc does. When the file is here, this suite proves
/// the claim; when it is not (CI, another machine), the round-trip tests below still prove
/// the code, and this one says out loud that it did not run.
const STOPGAP: &str = "gonk-store-2026-09-16T172813Z.nq";

const STORE_IDS: [&str; 13] = [
    "store-select",
    "store-ask",
    "store-construct",
    "store-describe",
    "store-graph-select",
    "store-graph-ask",
    "store-graph-construct",
    "store-graph-describe",
    "store-info",
    "store-graphs",
    "store-update",
    "store-graph-update",
    "store-load",
];

const LEDGER_IDS: [&str; 14] = [
    "ledger-append",
    "ledger-claim",
    "ledger-close",
    "ledger-comment",
    "ledger-defer",
    "ledger-item",
    "ledger-items",
    "ledger-label",
    "ledger-ledgers",
    "ledger-link",
    "ledger-next",
    "ledger-policy",
    "ledger-purge",
    "ledger-reopen",
];

/// gonk's own hub resource: the page renderer's chunk transform, in every composition
/// (`tests/conformance.rs` walks and pins it; here it is only counted).
const HUB_IDS: [&str; 1] = ["gonk-render"];

const BACKUP_IDS: [&str; 4] = [
    "gonk-backup",
    "gonk-backup-status",
    "gonk-backup-archive",
    "gonk-restore",
];

const COMPRESS_IDS: [&str; 4] = [
    "compress-gzip",
    "decompress-gzip",
    "compress-zlib",
    "decompress-zlib",
];

/// A hub with the backup family bound over an in-memory store, rotating into `dir`.
fn hub(dir: &Path, keep: usize) -> Arc<Kernel> {
    Arc::new(compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        Vec::new(),
        Vec::new(),
        Some(Backups {
            settings: Arc::new(backup::Settings {
                dir: dir.to_path_buf(),
                keep,
                every: Some(std::time::Duration::from_secs(86_400)),
                // Nothing in these tests opens it; it is the path a restore refuses.
                store_path: PathBuf::from("/nonexistent/live/store"),
            }),
            jobs: None,
        }),
    ))
}

fn issue(kernel: &Kernel, verb: Verb, iri: &str, args: &[(&str, &[u8])]) -> String {
    String::from_utf8_lossy(&try_issue(kernel, verb, iri, args).unwrap_or_else(|e| {
        panic!("{verb:?} {iri}: {e}");
    }))
    .into_owned()
}

fn try_issue(
    kernel: &Kernel,
    verb: Verb,
    iri: &str,
    args: &[(&str, &[u8])],
) -> Result<Vec<u8>, Error> {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.to_vec())),
    );
    block_on(kernel.issue(request, &Capability::root())).map(|repr| repr.bytes)
}

/// Three named graphs and the default graph, so every comparison below has a partition to
/// lose. A single-graph dataset — which is what this store holds today — would pass a
/// Turtle round trip by luck.
const SEED: &str = "\
<urn:item:1> <urn:ik:title> \"one\" <urn:iki:ledger:graph:default> .
<urn:item:2> <urn:ik:title> \"two\" <urn:iki:ledger:graph:default> .
<urn:item:3> <urn:ik:title> \"tenant a\" <urn:iki:ledger:graph:acme> .
<urn:item:4> <urn:ik:title> \"he said \\\"stop\\\".\\nThen .\" <urn:iki:ledger:graph:acme> .
<urn:item:5> <urn:ik:title> \"gone\" <urn:iki:ledger:graph:default:deleted> .
<urn:note:1> <urn:ik:body> \"in the default graph\" .
";

fn seed(kernel: &Kernel) {
    issue(
        kernel,
        Verb::Sink,
        "urn:iki:store:load",
        &[
            ("content", SEED.as_bytes()),
            ("format", b"application/n-quads"),
        ],
    );
}

/// The expected partition of [`SEED`] — four graphs, the unnamed one included.
fn seeded_counts() -> BTreeMap<String, u64> {
    BTreeMap::from([
        ("urn:iki:ledger:graph:default".to_string(), 2),
        ("urn:iki:ledger:graph:acme".to_string(), 2),
        ("urn:iki:ledger:graph:default:deleted".to_string(), 1),
        (String::new(), 1),
    ])
}

/// The per-graph counts of a store directory, read by opening it — deliberately NOT through
/// the code that wrote it.
fn counts_on_disk(dir: &Path) -> BTreeMap<String, u64> {
    let store = DurableStore::open(dir).expect("the restored store opens");
    let kernel = Kernel::new(Arc::new(ikigai_store::space(store)));
    let json = String::from_utf8(
        block_on(
            kernel.issue(
                Request::new(Verb::Source, Iri::parse("urn:iki:store:select").unwrap())
                    .with_arg(
                        "query",
                        ArgRef::Inline(
                            b"SELECT ?g (COUNT(*) AS ?n) WHERE { { GRAPH ?g { ?s ?p ?o } } \
                              UNION { ?s ?p ?o } } GROUP BY ?g"
                                .to_vec(),
                        ),
                    )
                    .with_arg(
                        "as",
                        ArgRef::Inline(b"application/sparql-results+json".to_vec()),
                    ),
                &Capability::root(),
            ),
        )
        .expect("the restored store answers")
        .bytes,
    )
    .expect("utf-8");
    let value: serde_json::Value = serde_json::from_str(&json).expect("results json");
    let mut counts = BTreeMap::new();
    for binding in value["results"]["bindings"].as_array().expect("bindings") {
        let graph = binding
            .get("g")
            .and_then(|g| g["value"].as_str())
            .unwrap_or("")
            .to_string();
        let count: u64 = binding["n"]["value"]
            .as_str()
            .expect("a count")
            .parse()
            .expect("a number");
        counts.insert(graph, count);
    }
    counts
}

/// The newest archive's bytes, through `urn:iki:gonk:backup:archive:{name}` — never by
/// reading the directory, because reading an archive back out is itself a resource this
/// family has to get right.
fn newest_archive(kernel: &Kernel, dir: &Path) -> Vec<u8> {
    let name = backup::archives(dir)
        .last()
        .expect("an archive was written")
        .name
        .clone();
    try_issue(
        kernel,
        Verb::Source,
        &format!("urn:iki:gonk:backup:archive:{name}"),
        &[],
    )
    .expect("the archive reads back")
}

// ------------------------------------------------------------------ the catalog

/// The linkage gate, extended: composing the backup family puts EIGHT more resources behind
/// every door this binary opens — four of gonk's own and the four `ikigai-compress` binds,
/// which are here because a backup compresses by resolving `urn:compress:gzip` through the
/// kernel rather than by calling a gzip crate. A dependency bump that widens either set is a
/// red test.
#[test]
fn the_backup_composition_serves_the_store_the_ledger_the_family_and_compress() {
    let dir = tempfile::tempdir().expect("tempdir");
    let hub = hub(dir.path(), 5);
    let expected: std::collections::BTreeSet<String> = STORE_IDS
        .iter()
        .chain(LEDGER_IDS.iter())
        .chain(HUB_IDS.iter())
        .chain(BACKUP_IDS.iter())
        .chain(COMPRESS_IDS.iter())
        .map(|id| id.to_string())
        .collect();
    assert_eq!(served_ids(&hub), expected);
    let door = doors::door_kernel(Arc::clone(&hub));
    assert_eq!(
        served_ids(&door),
        expected,
        "a door serves exactly the hub's catalog"
    );
}

/// ⚠ **`urn:time:*` is NOT in the catalog, and that is the point of the check.**
/// `ikigai-time` is linked and the backup job runs in one of its registries, but nothing here
/// needs a caller to schedule: the target set is fixed at startup by `main`. Through
/// ikigai-time 0.3.0 a bound `urn:time:schedule` would also have fired any target at the
/// registry's own capability (ledger #79); 0.4.0 closes that in the crate, and this server
/// still binds none of the control plane, because every bound endpoint is surface.
#[test]
fn the_timer_control_plane_is_not_served() {
    let dir = tempfile::tempdir().expect("tempdir");
    let hub = hub(dir.path(), 5);
    for iri in ["urn:time:schedule", "urn:time:cancel", "urn:time:jobs"] {
        assert!(
            hub.describe(&Iri::parse(iri).unwrap()).is_none(),
            "`{iri}` must not be bound in this server"
        );
    }
}

fn served_ids(kernel: &Kernel) -> std::collections::BTreeSet<String> {
    kernel
        .entries()
        .expect("an enumerable root")
        .iter()
        .filter(|entry| !entry.pattern.starts_with("urn:kernel:"))
        .map(|entry| {
            kernel
                .describe_pattern(&entry.pattern)
                .unwrap_or_else(|| panic!("`{}` describes itself", entry.pattern))
                .id
        })
        .collect()
}

/// The module recipe over the composition that carries the backup family.
#[test]
fn the_backup_composition_conforms() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A pre-seeded archive so the walk's `{name}` fixture names a real file. It is not a
    // valid gzip stream and does not need to be: the endpoint's job is to hand back bytes.
    let planted = format!("gonk-store-2000-01-01T000000Z{}", backup::ARCHIVE_SUFFIX);
    std::fs::write(dir.path().join(&planted), b"planted").expect("plant an archive");
    let hub = hub(dir.path(), 5);
    seed(&hub);
    let mut suite = STORE_IDS.iter().fold(Suite::new(), |suite, id| {
        suite.opt_out(
            *id,
            None,
            "ikigai-store's own conformance suite walks it with real SPARQL; it is bound \
                 here because every ledger read and write composes over it",
        )
    });
    // The ledger is walked with its own fixtures over the SAME composition by
    // `tests/conformance.rs::the_hub_conforms`. Duplicating forty lines of fixtures here
    // would give two places for the ledger's contract to be asserted and one of them to
    // drift; this file exists to walk the backup family.
    for id in LEDGER_IDS {
        suite = suite.opt_out(
            id,
            None,
            "walked with ikigai-ledger's own fixtures by tests/conformance.rs over this same \
             composition; this file walks the backup family",
        );
    }
    // `ikigai-compress`'s own suite makes exactly these declarations, and they are repeated
    // rather than assumed because binding a module is this server taking responsibility for
    // what it serves: the compressors are pure functions of their inputs, and no fixture can
    // express a decompressor's input (a gzip stream is not UTF-8 and `Fixture::arg` takes a
    // String).
    for id in ["compress-gzip", "compress-zlib"] {
        suite = suite
            .pure(id)
            .cacheable(id)
            .fixture(Fixture::new(id, Verb::Source).arg("content", "hello"));
    }
    for id in ["decompress-gzip", "decompress-zlib"] {
        suite = suite.opt_out(
            id,
            None,
            "no fixture can express this input: a gzip/zlib stream is not UTF-8 and \
             Fixture::arg takes a String. Walked end to end by the round-trip tests below, \
             which decompress a real archive.",
        );
    }
    for id in HUB_IDS {
        suite = suite.opt_out(
            id,
            None,
            "walked with a chunk document, and declared pure, by tests/conformance.rs over \
             this same composition; this file walks the backup family",
        );
    }
    let suite = suite
        .opt_out(
            "gonk-restore",
            None,
            "a synthesized `into` would build a RocksDB directory wherever the walk's \
             argument happened to point; it is walked with real archives by \
             `a_backup_restores_to_the_same_graphs_and_per_graph_counts` below",
        )
        .fixture(Fixture::new("gonk-backup-archive", Verb::Source).binding("name", &planted))
        .fixture(Fixture::new("gonk-backup-archive", Verb::Exists).binding("name", &planted))
        .fixture(Fixture::new("gonk-backup-status", Verb::Source))
        .fixture(Fixture::new("gonk-backup", Verb::Source));
    let report = suite.run_blocking(&hub);
    println!("--- backup composition ---\n{report}");
    assert!(report.is_clean(), "{report}");
}

// ------------------------------------------------------------------ the round trip

/// ★★ **The test this whole feature turns on.** Back up a partitioned dataset, read the
/// archive back out through its own resource, restore it into a fresh store, and compare the
/// GRAPH SET and the PER-GRAPH counts — never just a total, because a total is exactly what
/// a Turtle or N-Triples dump preserves while collapsing every tenant into one graph.
#[test]
fn a_backup_restores_to_the_same_graphs_and_per_graph_counts() {
    let backups = tempfile::tempdir().expect("tempdir");
    let hub = hub(backups.path(), 5);
    seed(&hub);

    let report = issue(&hub, Verb::Source, backup::BACKUP, &[]);
    assert!(report.contains("6 quads in 4 graph(s)"), "{report}");
    assert!(report.contains("urn:iki:ledger:graph:acme"), "{report}");
    assert!(report.contains("(default graph)"), "{report}");

    let archive = newest_archive(&hub, backups.path());
    assert_eq!(&archive[..2], b"\x1f\x8b", "the archive is gzip");

    let into = tempfile::tempdir().expect("tempdir");
    let restored = into.path().join("store");
    let answer = issue(
        &hub,
        Verb::Sink,
        backup::RESTORE,
        &[
            ("content", &archive),
            ("into", restored.to_string_lossy().as_bytes()),
        ],
    );
    assert!(answer.contains("restored 6 quads"), "{answer}");
    assert!(
        answer.contains("graph set and per-graph counts match"),
        "{answer}"
    );
    assert_eq!(
        counts_on_disk(&restored),
        seeded_counts(),
        "the restored dataset must be the same partition, not the same total"
    );
}

/// A literal with a quotation mark, an escaped newline and a trailing period is the row a
/// hand-rolled N-Quads writer corrupts. It comes back byte for byte.
#[test]
fn an_awkward_literal_survives_the_whole_round_trip() {
    let backups = tempfile::tempdir().expect("tempdir");
    let hub = hub(backups.path(), 5);
    seed(&hub);
    issue(&hub, Verb::Source, backup::BACKUP, &[]);
    let archive = newest_archive(&hub, backups.path());
    let into = tempfile::tempdir().expect("tempdir");
    let restored = into.path().join("store");
    issue(
        &hub,
        Verb::Sink,
        backup::RESTORE,
        &[
            ("content", &archive),
            ("into", restored.to_string_lossy().as_bytes()),
        ],
    );
    let store = DurableStore::open(&restored).expect("opens");
    let kernel = Kernel::new(Arc::new(ikigai_store::space(store)));
    let answer = String::from_utf8(
        block_on(
            kernel.issue(
                Request::new(Verb::Source, Iri::parse("urn:iki:store:select").unwrap())
                    .with_arg(
                        "query",
                        ArgRef::Inline(
                            b"SELECT ?t WHERE { GRAPH <urn:iki:ledger:graph:acme> { \
                          <urn:item:4> <urn:ik:title> ?t } }"
                                .to_vec(),
                        ),
                    )
                    .with_arg(
                        "as",
                        ArgRef::Inline(b"application/sparql-results+json".to_vec()),
                    ),
                &Capability::root(),
            ),
        )
        .expect("the restored store answers")
        .bytes,
    )
    .expect("utf-8");
    let value: serde_json::Value = serde_json::from_str(&answer).expect("json");
    assert_eq!(
        value["results"]["bindings"][0]["t"]["value"]
            .as_str()
            .expect("the title"),
        "he said \"stop\".\nThen ."
    );
}

/// An archive of unchanged data is byte-identical to the one before it, because
/// `urn:compress:gzip` pins its header and the dump is ordered. That is what makes
/// "has anything changed since the last backup" a digest comparison.
#[test]
fn two_backups_of_unchanged_data_are_the_same_bytes() {
    let backups = tempfile::tempdir().expect("tempdir");
    let hub = hub(backups.path(), 5);
    seed(&hub);
    issue(&hub, Verb::Source, backup::BACKUP, &[]);
    let first = newest_archive(&hub, backups.path());
    // A second dump of the same data, compared as bytes rather than by taking a second
    // archive — two archives a second apart share a name at this stamp's resolution.
    let second_report = issue(&hub, Verb::Source, backup::BACKUP, &[]);
    let second = newest_archive(&hub, backups.path());
    assert_eq!(first, second, "identical data, identical archive");
    assert!(
        second_report.contains("identical to the previous backup")
            || backup::archives(backups.path()).len() == 1,
        "{second_report}"
    );
}

// ------------------------------------------------------------------ a merge is not a restore

/// ★ Restoring into a dataset that already holds quads is a UNION: it resurrects everything
/// either side deleted and can never reproduce the backed-up state. It is refused.
#[test]
fn restoring_into_a_non_empty_store_is_refused() {
    let backups = tempfile::tempdir().expect("tempdir");
    let hub = hub(backups.path(), 5);
    seed(&hub);
    issue(&hub, Verb::Source, backup::BACKUP, &[]);
    let archive = newest_archive(&hub, backups.path());
    let into = tempfile::tempdir().expect("tempdir");
    let restored = into.path().join("store");
    issue(
        &hub,
        Verb::Sink,
        backup::RESTORE,
        &[
            ("content", &archive),
            ("into", restored.to_string_lossy().as_bytes()),
        ],
    );
    let again = try_issue(
        &hub,
        Verb::Sink,
        backup::RESTORE,
        &[
            ("content", &archive),
            ("into", restored.to_string_lossy().as_bytes()),
        ],
    )
    .expect_err("a second restore into the same directory is a merge");
    let message = again.to_string();
    assert!(message.contains("not empty"), "{message}");
    assert!(message.contains("MERGE"), "{message}");
}

/// And the live store is refused by name, whatever else is true of it.
#[test]
fn restoring_over_the_live_store_is_refused() {
    let backups = tempfile::tempdir().expect("tempdir");
    let hub = Arc::new(compose_with(
        DurableStore::in_memory().expect("an in-memory store"),
        None,
        Vec::new(),
        Vec::new(),
        Some(Backups {
            settings: Arc::new(backup::Settings {
                dir: backups.path().to_path_buf(),
                keep: 5,
                every: None,
                store_path: PathBuf::from("/Users/nobody/.ikigai/store"),
            }),
            jobs: None,
        }),
    ));
    let error = try_issue(
        &hub,
        Verb::Sink,
        backup::RESTORE,
        &[
            ("content", b"<urn:a> <urn:b> <urn:c> .\n"),
            ("into", b"/Users/nobody/.ikigai/store"),
        ],
    )
    .expect_err("the live store is not a restore target");
    let message = error.to_string();
    assert!(message.contains("LIVE store"), "{message}");
}

// ------------------------------------------------------------------ retention and status

#[test]
fn the_rotation_keeps_the_last_five_and_prunes_the_oldest() {
    let backups = tempfile::tempdir().expect("tempdir");
    let hub = hub(backups.path(), 5);
    seed(&hub);
    // Six archives, planted with distinct stamps because a real run would not produce six
    // within one second, and the name is the sort key.
    for day in 1..=6 {
        let name = format!(
            "gonk-store-2026-09-0{day}T000000Z{}",
            backup::ARCHIVE_SUFFIX
        );
        std::fs::write(backups.path().join(&name), b"planted").expect("plant");
        std::fs::write(
            backups
                .path()
                .join(format!("{name}{}", backup::META_SUFFIX)),
            b"{}",
        )
        .expect("plant a sidecar");
    }
    assert_eq!(backup::archives(backups.path()).len(), 6);
    let report = issue(&hub, Verb::Source, backup::BACKUP, &[]);
    // Seven now exist; five are kept.
    assert_eq!(backup::archives(backups.path()).len(), 5, "{report}");
    let left: Vec<String> = backup::archives(backups.path())
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert!(
        !left.iter().any(|name| name.contains("2026-09-01")),
        "the oldest went first: {left:?}"
    );
    assert!(report.contains("pruned"), "{report}");
}

/// ★ `STATUS` is what makes a 24-hour schedule worth having: a job that HANGS reads STALE
/// forever and never reads FAILING, so "when was the last good backup" has to be a query
/// against what is actually on disk.
#[test]
fn the_status_answers_from_what_is_actually_on_disk() {
    let backups = tempfile::tempdir().expect("tempdir");
    let hub = hub(backups.path(), 5);
    let before = issue(&hub, Verb::Source, backup::STATUS, &[]);
    assert!(before.contains("NO BACKUP HAS EVER BEEN TAKEN"), "{before}");

    seed(&hub);
    issue(&hub, Verb::Source, backup::BACKUP, &[]);
    let after = issue(&hub, Verb::Source, backup::STATUS, &[]);
    assert!(after.contains("quads          6"), "{after}");
    assert!(after.contains("urn:iki:ledger:graph:acme"), "{after}");
    assert!(after.contains("nquads_sha256"), "{after}");

    let json = issue(
        &hub,
        Verb::Source,
        backup::STATUS,
        &[("as", b"application/json")],
    );
    let value: serde_json::Value = serde_json::from_str(&json).expect("the JSON face parses");
    assert_eq!(value["newest"]["quads"], 6);
    assert_eq!(value["rotation"].as_array().expect("rotation").len(), 1);
    assert_eq!(value["keep"], 5);
    // The timer half is null here because these tests bind no registry; `main` builds one.
    assert!(value["timer"].is_null(), "{json}");
}

/// Startup asks the rotation directory when the last backup was, so a daily job on a daemon
/// that restarts nightly is not a job that never fires.
#[test]
fn the_recorded_instant_is_what_startup_reads_not_the_file_mtime() {
    let backups = tempfile::tempdir().expect("tempdir");
    assert_eq!(backup::newest_taken_at_ms(backups.path()), None);
    let hub = hub(backups.path(), 5);
    seed(&hub);
    issue(&hub, Verb::Source, backup::BACKUP, &[]);
    let taken = backup::newest_taken_at_ms(backups.path()).expect("recorded");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(
        now.saturating_sub(taken) < 60_000,
        "the sidecar records the instant the backup was taken: {taken} vs {now}"
    );
}

// ------------------------------------------------------------------ the real artifact

/// ★★ **The stopgap archive, actually loaded.** The hub recorded
/// `~/.ikigai/backups/gonk-store-2026-09-16T172813Z.nq` as a backup on structural checks
/// alone and said so: 2850 rows became 2850 terminated, graph-qualified lines, 290 item
/// IRIs matched the live store — and none of that proves the file LOADS.
///
/// When the file is present this restores it into a scratch store and reports the graph set
/// and the per-graph counts. When it is not (CI, a different machine) the test says so
/// rather than passing quietly: a skipped proof that reads like a passing one is the shape
/// this whole item is about.
#[test]
fn the_stopgap_archive_the_hub_took_actually_restores() {
    let Some(home) = std::env::var_os("HOME") else {
        println!("SKIPPED: no HOME");
        return;
    };
    let path = PathBuf::from(home).join(".ikigai/backups").join(STOPGAP);
    let Ok(nquads) = std::fs::read(&path) else {
        println!(
            "SKIPPED: {} is not on this machine — the round-trip tests above still cover the \
             code; this one covers THAT FILE",
            path.display()
        );
        return;
    };
    let backups = tempfile::tempdir().expect("tempdir");
    let hub = hub(backups.path(), 5);
    let into = tempfile::tempdir().expect("tempdir");
    let restored = into.path().join("store");
    let answer = issue(
        &hub,
        Verb::Sink,
        backup::RESTORE,
        &[
            ("content", &nquads),
            ("into", restored.to_string_lossy().as_bytes()),
        ],
    );
    println!("--- the stopgap archive restored ---\n{answer}");
    let counts = counts_on_disk(&restored);
    let total: u64 = counts.values().sum();
    assert!(total > 0, "the archive is not empty");
    assert!(
        counts.contains_key("urn:iki:ledger:graph:default"),
        "the ledger's graph survived as a NAMED graph, not collapsed into the default one: \
         {counts:?}"
    );
    println!("restored {total} quads in {} graph(s)", counts.len());
}
