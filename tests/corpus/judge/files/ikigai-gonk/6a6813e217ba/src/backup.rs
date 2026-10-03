//! Backup and restore — the one export door this dataset has, and the only one it can
//! have.
//!
//! # Why this is a gonk capability and not an ops script
//!
//! RocksDB permits one writer per directory, so on a machine that runs gonk, gonk holds
//! `~/.ikigai/store` and nothing else may open it ([`crate::compose`]'s decision of record).
//! Two consequences, and together they decide the whole design:
//!
//! - **No second process can export the dataset.** There is no `ikigai -c 'source
//!   urn:iki:store:…'` that opens the files; there is only this process.
//! - **`cp -r` of the live directory is a torn snapshot.** SST files and the write-ahead
//!   log mutate under the copy, and the result may not open, or may open and be quietly
//!   short.
//!
//! So the export has to be a face this server offers, and it is [`BACKUP`].
//!
//! # ⚠ The format is N-Quads, and the easy mistake is Turtle
//!
//! **CONSTRUCT returns TRIPLES.** This dataset is partitioned by named graph — a graph per
//! named ledger, its graveyard beside it, `ikigai-browse`'s own graphs, the reserved set —
//! and that partition is what every per-graph capability
//! ([`crate::grants`]) is written against. A backup serialized as Turtle or N-Triples
//! collapses every graph into one: the quad count still matches, every triple round-trips,
//! and the restored dataset has every tenant's statements in the default graph with the
//! tenancy boundary gone. Nothing in a count would tell you.
//!
//! Measured 2026-09-16 on this machine: one named graph,
//! `urn:iki:ledger:graph:default`, holding all 2850 quads, and a default graph holding
//! zero — so today's data would survive a Turtle dump by luck. The format does not depend
//! on today's data shape.
//!
//! # ★ The dump is the engine's own words in both directions
//!
//! A backup is `urn:iki:store:select` over `{ GRAPH ?g { ?s ?p ?o } } UNION { ?s ?p ?o }`,
//! read back with **oxigraph's own SPARQL-results parser** and written with **oxigraph's
//! own N-Quads serializer**. Neither end is string surgery. That matters more than it
//! looks: this dataset holds ledger item bodies with newlines, quotation marks and
//! trailing periods in them, and a hand-rolled `?s ?p ?o .` writer corrupts exactly those
//! rows — in the one file nobody reads until the day they need it.
//!
//! `ORDER BY ?g ?s ?p ?o` is on the query on purpose. `urn:compress:gzip` pins its header
//! fields, so identical bytes compress to identical archives; a stable row order is the
//! other half of making "the data has not changed since the last backup" a digest
//! comparison rather than a belief. The cost is that the whole result is sorted — which is
//! fine at this dataset's scale (megabytes) and is the first thing to revisit if it is not.
//!
//! # Capability posture — the two most dangerous grants in the system
//!
//! - **A backup reads EVERY graph**, which is exactly the authority the per-graph boundary
//!   exists to avoid handing out. [`BACKUP`] therefore declares `urn:cap:store:read` (the
//!   broad token) *and* [`CAP_BACKUP`], which names the second authority it exercises and
//!   `urn:cap:store:read` does not: writing into and pruning gonk's backup directory.
//! - **A restore writes EVERY graph.** [`RESTORE`] declares [`CAP_RESTORE`].
//! - **Neither is reachable from the HTTP door, and that is structural rather than
//!   careful.** The HTTP door's capability is exactly the per-ledger token list
//!   [`crate::grants::grants_for_all`] computes, plus a signed-in passkey's grant; none of
//!   those lists can contain any of these three tokens, because
//!   [`crate::quic::check_grants`] refuses a `grants.json` that names one. So the
//!   privileged half of this feature lives behind the owner-only socket — its own door, not
//!   a flag on the public one — and `tests/doors.rs` proves the public door still cannot
//!   reach even `urn:iki:store:info`.
//!
//! # A backup you have not restored is not a backup
//!
//! [`RESTORE`] is the verifier as well as the restore path. It loads an archive into a
//! **new** store directory and compares the graph set and the per-graph quad counts of the
//! restored dataset against the archive it was built from — not just a total, because a
//! total is exactly the number the Turtle trap preserves. `tests/backup.rs` runs it against
//! the real stopgap archive the hub took before this feature existed.
//!
//! # A merge is not a restore
//!
//! Loading a backup into a non-empty store yields a UNION: deleted items come back, and the
//! result can never be the state that was backed up. So [`RESTORE`] **refuses** a target
//! that already holds quads, and refuses the live store outright. What it produces is a
//! complete store directory beside the live one; swapping them is an operator's act, with
//! the server stopped, which is the one moment when the destructive step is visible.
//!
//! # ★ The schedule is untrustworthy without [`STATUS`]
//!
//! gonk sits inside the accepted heartbeat-coverage gap, and a scheduled job that HANGS
//! reads STALE forever and never reads FAILING. At a 24-hour cadence a wedged backup is
//! invisible for a day at best and permanently at worst. [`STATUS`] is what makes the
//! schedule worth having: it answers from BOTH sides — the newest archive actually on disk
//! with its counts and digests, and the live timer's own health (`runs`, time since the
//! last run, time since the last SUCCESS, failures in a row). A job that has not fired
//! shows as a growing `since_last`; a job that fires and fails shows as
//! `failures_in_a_row`. Neither reads as "fine".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Result, UriTemplate, Verb,
};
use ikigai_time::JobRegistry;
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{GraphName, NamedOrBlankNode, Quad, Term};
use oxigraph::sparql::results::{
    QueryResultsFormat, QueryResultsParser, ReaderQueryResultsParserOutput,
};
use sha2::{Digest, Sha256};

/// Take a backup, read [`STATUS`], and read an archive back out.
///
/// One token for all three reads of the family, deliberately: the status readout names
/// every graph in the dataset with its size, which is the tenancy map. A caller who may
/// not read the graphs has no business reading their shape either.
pub const CAP_BACKUP: &str = "urn:cap:gonk:backup";

/// Load an archive into a new store directory.
pub const CAP_RESTORE: &str = "urn:cap:gonk:restore";

/// `ikigai-store`'s whole-dataset read token. Spelled through the crate rather than
/// transcribed, because a grant file elsewhere has to match it exactly.
pub fn cap_store_read() -> &'static str {
    ikigai_store::CAP_READ
}

/// Take a backup now: `Source urn:iki:gonk:backup`.
pub const BACKUP: &str = "urn:iki:gonk:backup";

/// The last successful backup, and the timer's health: `Source urn:iki:gonk:backup:status`.
pub const STATUS: &str = "urn:iki:gonk:backup:status";

/// One archive's bytes, by file name: `Source urn:iki:gonk:backup:archive:{name}`.
pub const ARCHIVE: &str = "urn:iki:gonk:backup:archive:{name}";

/// Load an archive into a new store: `Sink urn:iki:gonk:restore into=<dir>`.
pub const RESTORE: &str = "urn:iki:gonk:restore";

/// Every scope the scheduled backup job fires under — and nothing else.
///
/// ★ This is why the timer is a private [`JobRegistry`] rather than a bound
/// `urn:time:schedule`. A registry fires under ONE capability, whatever target it is given;
/// binding the control plane would mean any caller who could reach `urn:time:schedule`
/// could have this server issue any request under this list. The list is narrow, but the
/// principle is that the target set is fixed at startup by this server, not chosen at call
/// time by a caller.
pub const JOB_SCOPES: [&str; 2] = [CAP_BACKUP, ikigai_store::CAP_READ];

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const N_QUADS: &str = "application/n-quads";
const GZIP: &str = "application/gzip";
const JSON: &str = "application/json";

/// The archive suffix: plain N-Quads, gzipped.
pub const ARCHIVE_SUFFIX: &str = ".nq.gz";
/// The sidecar suffix, carrying one archive's counts and digests.
pub const META_SUFFIX: &str = ".meta.json";

/// The ceiling handed to `urn:decompress:gzip` on a restore: 4 GiB of N-Quads, which is
/// above the module's own 1 GiB ceiling and therefore means "the module's maximum".
///
/// ⚠ The bound REFUSES rather than truncates, which is the property that matters: a
/// truncated restore would load cleanly and be short, and the per-graph comparison below
/// would be the only thing that noticed.
const RESTORE_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// Where backups land, how many are kept, and how often one is taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The rotation directory. gonk owns it and writes nothing else there.
    pub dir: PathBuf,
    /// How many archives to keep. Pruned oldest-first, after a successful write.
    pub keep: usize,
    /// The cadence, or `None` for "no schedule" — the resources are still bound, because
    /// an operator who has turned the timer off still has a dataset to export.
    pub every: Option<Duration>,
    /// The live store's directory, so a restore can refuse to be pointed at it.
    pub store_path: PathBuf,
}

/// What the backup family is composed against: the settings, and the timer whose health
/// [`STATUS`] reports (absent when nothing is scheduled).
#[derive(Clone)]
pub struct Backups {
    /// Where and how often.
    pub settings: Arc<Settings>,
    /// The registry the scheduled job lives in, for the health half of [`STATUS`].
    pub jobs: Option<JobRegistry>,
}

/// The backup family's space: [`BACKUP`], [`STATUS`], [`ARCHIVE`] and [`RESTORE`].
pub fn space(backups: Backups) -> EndpointSpace {
    EndpointSpace::new()
        .bind(
            Exact::new(BACKUP),
            TakeBackup {
                backups: backups.clone(),
            },
        )
        .bind(
            Exact::new(STATUS),
            Status {
                backups: backups.clone(),
            },
        )
        .bind(
            UriTemplate::parse(ARCHIVE).expect("a constant template"),
            ReadArchive {
                backups: backups.clone(),
            },
        )
        .bind(Exact::new(RESTORE), Restore { backups })
        .named(crate::spaces::iri(crate::spaces::BACKUP))
}

// ---------------------------------------------------------------- the dump

/// One graph's name as it appears in a count table: the IRI, or `""` for the default graph.
const DEFAULT_GRAPH_KEY: &str = "";

/// A dump: the N-Quads bytes, and the per-graph counts measured while writing them.
struct Dump {
    nquads: Vec<u8>,
    graphs: BTreeMap<String, u64>,
}

impl Dump {
    fn quads(&self) -> u64 {
        self.graphs.values().sum()
    }
}

/// The whole dataset, every graph, as N-Quads — through the kernel, under the caller's
/// capability.
///
/// The UNION is not decoration. `{ ?s ?p ?o }` alone reads the DEFAULT graph only (this
/// store does not evaluate a bare pattern over the union of graphs — `ikigai-store`'s
/// `a_property_path_cannot_walk_out_of_the_scope` pins that), and `GRAPH ?g { ?s ?p ?o }`
/// alone never sees the default graph. A backup missing either half is a backup that
/// restores to a different dataset.
async fn dump(inv: &Invocation<'_>) -> Result<Dump> {
    let query = "SELECT ?g ?s ?p ?o WHERE { { GRAPH ?g { ?s ?p ?o } } UNION { ?s ?p ?o } } \
                 ORDER BY ?g ?s ?p ?o";
    let results = inv
        .issue(
            Request::new(Verb::Source, iri(BACKUP_SELECT)?)
                .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
                .with_arg(
                    "as",
                    ArgRef::Inline(b"application/sparql-results+json".to_vec()),
                ),
        )
        .await?;
    quads_from_results(&results.bytes)
}

/// `urn:iki:store:select`, the one resource a backup reads.
const BACKUP_SELECT: &str = "urn:iki:store:select";

/// Parse a SPARQL SELECT result set into N-Quads, counting per graph as it goes.
fn quads_from_results(bytes: &[u8]) -> Result<Dump> {
    let parser = QueryResultsParser::from_format(QueryResultsFormat::Json);
    let solutions = match parser.for_reader(bytes) {
        Ok(ReaderQueryResultsParserOutput::Solutions(solutions)) => solutions,
        Ok(ReaderQueryResultsParserOutput::Boolean(_)) => {
            return Err(Error::Endpoint(
                "the store answered a boolean to a SELECT — the backup query was rewritten \
                 somewhere it should not have been"
                    .to_string(),
            ))
        }
        Err(e) => return Err(Error::Endpoint(format!("reading the store's results: {e}"))),
    };
    let mut graphs: BTreeMap<String, u64> = BTreeMap::new();
    let mut out: Vec<u8> = Vec::new();
    let mut writer = RdfSerializer::from_format(RdfFormat::NQuads).for_writer(&mut out);
    for solution in solutions {
        let solution = solution.map_err(|e| Error::Endpoint(format!("a result row: {e}")))?;
        let quad = quad_from_solution(&solution)?;
        let key = match &quad.graph_name {
            GraphName::DefaultGraph => DEFAULT_GRAPH_KEY.to_string(),
            GraphName::NamedNode(node) => node.as_str().to_string(),
            GraphName::BlankNode(node) => format!("_:{}", node.as_str()),
        };
        *graphs.entry(key).or_default() += 1;
        writer
            .serialize_quad(&quad)
            .map_err(|e| Error::Endpoint(format!("writing a quad: {e}")))?;
    }
    writer
        .finish()
        .map_err(|e| Error::Endpoint(format!("finishing the dump: {e}")))?;
    Ok(Dump {
        nquads: out,
        graphs,
    })
}

/// One `?g ?s ?p ?o` row as a [`Quad`].
///
/// Every refusal here is a shape the store cannot actually produce (a literal subject, a
/// blank-node predicate). They are errors rather than skips because a backup that silently
/// dropped a row would be a backup with a hole in it, and the count would agree with
/// itself.
fn quad_from_solution(solution: &oxigraph::sparql::QuerySolution) -> Result<Quad> {
    let subject = match solution.get("s") {
        Some(Term::NamedNode(node)) => NamedOrBlankNode::NamedNode(node.clone()),
        Some(Term::BlankNode(node)) => NamedOrBlankNode::BlankNode(node.clone()),
        other => {
            return Err(Error::Endpoint(format!(
                "a row's subject is not an IRI or a blank node: {other:?}"
            )))
        }
    };
    let predicate = match solution.get("p") {
        Some(Term::NamedNode(node)) => node.clone(),
        other => {
            return Err(Error::Endpoint(format!(
                "a row's predicate is not an IRI: {other:?}"
            )))
        }
    };
    let object = solution
        .get("o")
        .ok_or_else(|| Error::Endpoint("a row has no object".to_string()))?
        .clone();
    let graph_name = match solution.get("g") {
        None => GraphName::DefaultGraph,
        Some(Term::NamedNode(node)) => GraphName::NamedNode(node.clone()),
        Some(Term::BlankNode(node)) => GraphName::BlankNode(node.clone()),
        other => {
            return Err(Error::Endpoint(format!(
                "a row's graph is not an IRI: {other:?}"
            )))
        }
    };
    Ok(Quad {
        subject,
        predicate,
        object,
        graph_name,
    })
}

/// The per-graph counts of an N-Quads document, measured by parsing it — the independent
/// half of the restore comparison.
///
/// ⚠ It must not come from the same code path that produced the dump. A dump and a
/// comparison that share a counter agree with each other about a dataset neither of them
/// read.
fn counts_in_nquads(bytes: &[u8]) -> Result<BTreeMap<String, u64>> {
    let mut graphs: BTreeMap<String, u64> = BTreeMap::new();
    for quad in RdfParser::from_format(RdfFormat::NQuads).for_reader(bytes) {
        let quad = quad.map_err(|e| Error::InvalidArgument {
            name: "content".to_string(),
            detail: format!("parsing N-Quads: {e}"),
        })?;
        let key = match &quad.graph_name {
            GraphName::DefaultGraph => DEFAULT_GRAPH_KEY.to_string(),
            GraphName::NamedNode(node) => node.as_str().to_string(),
            GraphName::BlankNode(node) => format!("_:{}", node.as_str()),
        };
        *graphs.entry(key).or_default() += 1;
    }
    Ok(graphs)
}

// ---------------------------------------------------------------- archives

/// One archive on disk, with whatever its sidecar recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    /// The file name, which is also the `{name}` [`ARCHIVE`] captures.
    pub name: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// The sidecar's JSON text, when there is one.
    pub meta: Option<String>,
}

/// The rotation set, oldest first. Names sort chronologically because the stamp in them
/// does — one fewer thing depending on a file system's idea of mtime.
pub fn archives(dir: &Path) -> Vec<Archive> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Archive> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(ARCHIVE_SUFFIX) {
                return None;
            }
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let meta = std::fs::read_to_string(dir.join(format!("{name}{META_SUFFIX}"))).ok();
            Some(Archive { name, bytes, meta })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// When the newest archive in `dir` was taken, from its sidecar — `None` when there is no
/// archive, or none whose sidecar says.
///
/// This is what `main` asks at startup to decide whether a backup is already overdue. It
/// reads the RECORDED instant rather than the file's mtime, because a rotation directory
/// that was copied, restored or rsynced keeps its names and contents and loses its mtimes —
/// and the wrong answer here is "a backup was taken recently", which is the answer mtime
/// gives on a freshly copied directory.
pub fn newest_taken_at_ms(dir: &Path) -> Option<u64> {
    archives(dir)
        .last()?
        .meta
        .as_deref()
        .and_then(|meta| field(meta, "taken_at_ms"))
        .and_then(|value| value.parse().ok())
}

/// Reject anything that is not a bare file name in the rotation directory.
///
/// `{name}` arrives from a caller, and the endpoint reads the file it names. A segment with
/// a separator or a `..` in it would make this an arbitrary-file read behind a capability
/// that says "backups".
fn archive_path(dir: &Path, name: &str) -> Result<PathBuf> {
    let bad = name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name.ends_with(ARCHIVE_SUFFIX);
    if bad {
        return Err(Error::InvalidArgument {
            name: "name".to_string(),
            detail: format!(
                "`{name}` is not an archive in this server's backup directory — a bare file \
                 name ending `{ARCHIVE_SUFFIX}`, as `{STATUS}` lists them"
            ),
        });
    }
    Ok(dir.join(name))
}

// ---------------------------------------------------------------- taking one

struct TakeBackup {
    backups: Backups,
}

#[async_trait]
impl Endpoint for TakeBackup {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match inv.request.verb {
            Verb::Source => self.take(inv).await,
            other => Err(unsupported("gonk-backup", other)),
        }
    }

    fn name(&self) -> &str {
        "gonk-backup"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-backup")
            .title("Take a backup of the whole dataset")
            .summary(
                "Serializes EVERY graph as N-Quads through `urn:iki:store:select`, gzips it \
                 by resolving `urn:compress:gzip`, writes it into this server's backup \
                 directory with a sidecar recording the per-graph counts and both digests, \
                 and prunes the rotation set oldest-first. Quad-shaped on purpose: a Turtle \
                 or N-Triples dump collapses every named graph into one and destroys the \
                 per-graph tenancy boundary while the quad count still matches.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_BACKUP)
            .requires(ikigai_store::CAP_READ)
            .output("text/plain")
    }
}

impl TakeBackup {
    async fn take(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let settings = &self.backups.settings;
        let dump = dump(inv).await?;
        let quads = dump.quads();
        let nquads_digest = digest(&dump.nquads);
        let nquads_bytes = dump.nquads.len();

        // Compression through the kernel, not through a gzip crate: the archive is a
        // composition of resources, and `urn:compress:gzip` is where determinism and the
        // decompression bound are argued.
        let archive = inv
            .issue(
                Request::new(Verb::Source, iri("urn:compress:gzip")?)
                    .with_arg("content", ArgRef::Inline(dump.nquads.clone())),
            )
            .await?;
        let archive_digest = digest(&archive.bytes);

        // Re-count the store AFTER the dump. A SELECT is one snapshot, so this cannot
        // "fix" a torn dump — it is evidence about whether the dataset was moving while
        // the backup was taken, which is worth recording next to a number a restore will
        // be compared against.
        let after = self.count(inv).await?;

        let now = inv
            .now()
            .map(|t| t.as_millis())
            .ok_or_else(|| Error::Endpoint("no clock: a backup must be able to say when".into()))?;
        let stamp = stamp_compact(now);
        let name = format!("gonk-store-{stamp}{ARCHIVE_SUFFIX}");
        std::fs::create_dir_all(&settings.dir)
            .map_err(|e| Error::Endpoint(format!("creating {}: {e}", settings.dir.display())))?;

        let meta = meta_json(&MetaFields {
            archive: &name,
            taken_at: &stamp_iso(now),
            taken_at_ms: now,
            quads,
            graphs: &dump.graphs,
            nquads_bytes,
            nquads_sha256: &nquads_digest,
            archive_bytes: archive.bytes.len(),
            archive_sha256: &archive_digest,
            store_quads_after_dump: after,
        });

        // Write the archive first and the sidecar second, each through a temporary name.
        // A crash between them leaves an archive without a sidecar, which `STATUS` reports
        // as exactly that; the reverse order would leave a sidecar describing a file that
        // does not exist, which reads as a good backup.
        write_atomic(&settings.dir.join(&name), &archive.bytes)?;
        write_atomic(
            &settings.dir.join(format!("{name}{META_SUFFIX}")),
            meta.as_bytes(),
        )?;

        let pruned = prune(&settings.dir, settings.keep);
        let previous = archives(&settings.dir)
            .iter()
            .rev()
            .filter(|a| a.name != name)
            .find_map(|a| a.meta.as_deref().and_then(|m| field(m, "nquads_sha256")));
        let unchanged = previous.as_deref() == Some(nquads_digest.as_str());

        let mut report = String::new();
        report.push_str(&format!("backup {name}\n"));
        report.push_str(&format!(
            "  {quads} quads in {} graph(s), {nquads_bytes} bytes of N-Quads -> {} bytes gzipped\n",
            dump.graphs.len(),
            archive.bytes.len()
        ));
        for (graph, count) in &dump.graphs {
            let shown = if graph.is_empty() {
                "(default graph)"
            } else {
                graph.as_str()
            };
            report.push_str(&format!("  {count:>8}  {shown}\n"));
        }
        report.push_str(&format!("  sha256 (n-quads) {nquads_digest}\n"));
        report.push_str(&format!("  sha256 (archive) {archive_digest}\n"));
        if after != quads {
            report.push_str(&format!(
                "  ⚠ the store held {after} quads after the dump, not {quads} — the dataset \
                 was written while the backup was taken. The archive is one consistent \
                 snapshot; this is a note about what changed after it.\n"
            ));
        }
        if unchanged {
            report.push_str("  identical to the previous backup (same N-Quads digest)\n");
        }
        report.push_str(&format!("  kept {}", settings.keep));
        if pruned.is_empty() {
            report.push_str(", pruned nothing\n");
        } else {
            report.push_str(&format!(", pruned {}\n", pruned.join(", ")));
        }
        Ok(plain(report))
    }

    /// The store's quad count, through the same door the dump used.
    async fn count(&self, inv: &Invocation<'_>) -> Result<u64> {
        let query = "SELECT (COUNT(*) AS ?n) WHERE { { GRAPH ?g { ?s ?p ?o } } UNION \
                     { ?s ?p ?o } }";
        let results = inv
            .issue(
                Request::new(Verb::Source, iri(BACKUP_SELECT)?)
                    .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
                    .with_arg(
                        "as",
                        ArgRef::Inline(b"application/sparql-results+json".to_vec()),
                    ),
            )
            .await?;
        let parser = QueryResultsParser::from_format(QueryResultsFormat::Json);
        let Ok(ReaderQueryResultsParserOutput::Solutions(solutions)) =
            parser.for_reader(results.bytes.as_slice())
        else {
            return Err(Error::Endpoint(
                "counting the store: not a solution set".into(),
            ));
        };
        for solution in solutions {
            let solution = solution.map_err(|e| Error::Endpoint(format!("counting: {e}")))?;
            if let Some(Term::Literal(literal)) = solution.get("n") {
                return literal
                    .value()
                    .parse()
                    .map_err(|_| Error::Endpoint("counting: not a number".into()));
            }
        }
        Err(Error::Endpoint("counting the store: no rows".into()))
    }
}

/// Keep the newest `keep` archives, deleting the rest with their sidecars. Returns what
/// went.
fn prune(dir: &Path, keep: usize) -> Vec<String> {
    let found = archives(dir);
    if found.len() <= keep {
        return Vec::new();
    }
    let mut gone = Vec::new();
    for archive in &found[..found.len() - keep] {
        // The sidecar goes FIRST. A sidecar left behind describes a file that is not there,
        // which is the one state that reads like a backup and is not.
        let _ = std::fs::remove_file(dir.join(format!("{}{META_SUFFIX}", archive.name)));
        if std::fs::remove_file(dir.join(&archive.name)).is_ok() {
            gone.push(archive.name.clone());
        }
    }
    gone
}

/// Write through a temporary name in the same directory, then rename.
///
/// A backup interrupted halfway is the failure this exists for: a rename is atomic within
/// a directory, so the rotation set never holds a half-written archive that a later restore
/// would discover was short.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("partial");
    std::fs::write(&temporary, bytes)
        .map_err(|e| Error::Endpoint(format!("writing {}: {e}", temporary.display())))?;
    restrict(&temporary);
    std::fs::rename(&temporary, path)
        .map_err(|e| Error::Endpoint(format!("renaming into {}: {e}", path.display())))
}

/// `0600`. A backup is the whole dataset in one file, including every graph the per-graph
/// capabilities exist to separate; it is owner-only or it has undone them.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

// ---------------------------------------------------------------- the status

struct Status {
    backups: Backups,
}

#[async_trait]
impl Endpoint for Status {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match inv.request.verb {
            Verb::Source => {
                let json = matches!(inv.inline_str("as"), Ok(JSON));
                if json {
                    Ok(Representation::new(
                        ReprType::new(JSON).with_param("charset", "utf-8"),
                        self.json().into_bytes(),
                    ))
                } else {
                    Ok(plain(self.text()))
                }
            }
            other => Err(unsupported("gonk-backup-status", other)),
        }
    }

    fn name(&self) -> &str {
        "gonk-backup-status"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-backup-status")
            .title("When the last good backup was, and whether the timer is still alive")
            .summary(
                "Answers from both sides: the newest archive actually on disk with its \
                 per-graph counts and digests, the whole rotation set, and — when a backup \
                 is scheduled — the timer's own health (runs, time since the last run, time \
                 since the last SUCCESS, failures in a row). A job that HANGS reads STALE \
                 forever and never reads FAILING, so a schedule without this manufactures \
                 confidence rather than providing it.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_BACKUP)
            .input(
                ArgSpec::new("as")
                    .summary("the representation to answer in")
                    .class(XSD_STRING)
                    .one_of(["text/plain", JSON])
                    .default_value("text/plain")
                    .optional(),
            )
            .output("text/plain")
            .output(JSON)
    }
}

impl Status {
    fn text(&self) -> String {
        let settings = &self.backups.settings;
        let found = archives(&settings.dir);
        let mut out = format!("backups in {}\n", settings.dir.display());
        match settings.every {
            Some(every) => out.push_str(&format!(
                "  schedule  every {} — keep {}\n",
                humanize(every),
                settings.keep
            )),
            None => out.push_str(&format!(
                "  schedule  OFF (gonk.backup.every = \"off\") — keep {}\n",
                settings.keep
            )),
        }
        match found.last() {
            None => out
                .push_str("  ⚠ NO BACKUP HAS EVER BEEN TAKEN — there is nothing here to restore\n"),
            Some(newest) => {
                out.push_str(&format!("  newest    {}\n", newest.name));
                match &newest.meta {
                    None => out.push_str(
                        "    ⚠ no sidecar: this archive was written but its counts were not \
                         recorded. Treat it as unverified.\n",
                    ),
                    Some(meta) => {
                        for key in [
                            "taken_at",
                            "quads",
                            "nquads_bytes",
                            "archive_bytes",
                            "nquads_sha256",
                        ] {
                            if let Some(value) = field(meta, key) {
                                out.push_str(&format!("    {key:<14} {value}\n"));
                            }
                        }
                        out.push_str(&format!("    graphs         {}\n", graph_line(meta)));
                    }
                }
            }
        }
        out.push_str(&format!("  rotation  {} archive(s)\n", found.len()));
        for archive in &found {
            out.push_str(&format!("    {:>10}  {}\n", archive.bytes, archive.name));
        }
        out.push_str(&self.health_text());
        out
    }

    /// The timer half. Facts, not a verdict — but the facts are chosen so that the two
    /// failure shapes a 24-hour job has are both visible: never fired (`runs 0`, `since
    /// last` growing) and firing but failing (`failures in a row`).
    fn health_text(&self) -> String {
        let Some(jobs) = &self.backups.jobs else {
            return "  timer     not scheduled in this process\n".to_string();
        };
        let health = jobs.health();
        let Some(job) = health.iter().find(|job| job.target == BACKUP) else {
            return "  ⚠ timer  a registry exists but NO JOB fires urn:iki:gonk:backup — \
                    nothing is taking backups\n"
                .to_string();
        };
        let mut out = format!(
            "  timer     job #{} every {} — {} run(s)\n",
            job.id,
            humanize(job.interval),
            job.runs
        );
        match job.since_last {
            None => out.push_str(
                "    ⚠ has never fired since this server started. At a daily cadence that \
                 is normal for the first day and a wedged timer after that.\n",
            ),
            Some(since) => out.push_str(&format!("    since last run      {}\n", humanize(since))),
        }
        match job.since_last_success {
            None if job.runs > 0 => out.push_str(
                "    ⚠ has NEVER SUCCEEDED — every run so far failed\n"
                    .to_string()
                    .as_str(),
            ),
            None => {}
            Some(since) => out.push_str(&format!("    since last success  {}\n", humanize(since))),
        }
        if job.failures_in_a_row > 0 {
            out.push_str(&format!(
                "    ⚠ {} failure(s) in a row\n",
                job.failures_in_a_row
            ));
        }
        if !job.last_output.is_empty() {
            out.push_str(&format!("    last                {}\n", job.last_output));
        }
        out
    }

    /// The JSON face: the newest sidecar verbatim under `newest`, the rotation set, and the
    /// timer's health. The sidecar is passed through rather than re-rendered so that what a
    /// query reads is exactly what the backup recorded.
    fn json(&self) -> String {
        let settings = &self.backups.settings;
        let found = archives(&settings.dir);
        let newest = found
            .last()
            .and_then(|a| a.meta.clone())
            .unwrap_or_else(|| "null".to_string());
        let rotation: Vec<String> = found
            .iter()
            .map(|a| format!("{{\"name\":{},\"bytes\":{}}}", quote(&a.name), a.bytes))
            .collect();
        let schedule = match settings.every {
            Some(every) => quote(&humanize(every)),
            None => "null".to_string(),
        };
        let timer = match &self.backups.jobs {
            None => "null".to_string(),
            Some(jobs) => match jobs.health().into_iter().find(|job| job.target == BACKUP) {
                None => "null".to_string(),
                Some(job) => format!(
                    "{{\"id\":{},\"runs\":{},\"interval_seconds\":{},\
                     \"seconds_since_last_run\":{},\"seconds_since_last_success\":{},\
                     \"failures_in_a_row\":{},\"last_output\":{}}}",
                    job.id,
                    job.runs,
                    job.interval.as_secs(),
                    job.since_last
                        .map(|d| d.as_secs().to_string())
                        .unwrap_or_else(|| "null".to_string()),
                    job.since_last_success
                        .map(|d| d.as_secs().to_string())
                        .unwrap_or_else(|| "null".to_string()),
                    job.failures_in_a_row,
                    quote(&job.last_output),
                ),
            },
        };
        format!(
            "{{\"directory\":{},\"keep\":{},\"schedule\":{},\"newest\":{},\
             \"rotation\":[{}],\"timer\":{}}}\n",
            quote(&settings.dir.display().to_string()),
            settings.keep,
            schedule,
            newest,
            rotation.join(","),
            timer
        )
    }
}

// ---------------------------------------------------------------- reading one back

struct ReadArchive {
    backups: Backups,
}

#[async_trait]
impl Endpoint for ReadArchive {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let name = inv
            .bindings
            .get("name")
            .ok_or_else(|| Error::MissingArgument("name".to_string()))?;
        match inv.request.verb {
            Verb::Exists => {
                let there = archive_path(&self.backups.settings.dir, name)
                    .map(|path| path.is_file())
                    .unwrap_or(false);
                Ok(plain(format!("{there}\n")))
            }
            Verb::Source => {
                let path = archive_path(&self.backups.settings.dir, name)?;
                let bytes = std::fs::read(&path).map_err(|e| {
                    Error::NotFound(format!(
                        "no archive `{name}` in {}: {e}",
                        self.backups.settings.dir.display()
                    ))
                })?;
                Ok(Representation::new(ReprType::new(GZIP), bytes))
            }
            other => Err(unsupported("gonk-backup-archive", other)),
        }
    }

    fn name(&self) -> &str {
        "gonk-backup-archive"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-backup-archive")
            .title("One archive's bytes, by name")
            .summary(
                "Reads one archive out of this server's backup directory as \
                 `application/gzip`. This is what makes a backup reachable at all without \
                 linking a filesystem family into a server that holds the dataset: pipe it \
                 into `urn:iki:gonk:restore` to verify it, or off the machine to have a \
                 copy somewhere the disk holding the data is not. `{name}` is a bare file \
                 name in that directory — `urn:iki:gonk:backup:status` lists them.",
            )
            .verb(Verb::Source)
            .verb(Verb::Exists)
            .verb(Verb::Meta)
            .requires(CAP_BACKUP)
            .input(
                ArgSpec::new("name")
                    .summary("the archive's file name, as `urn:iki:gonk:backup:status` lists it")
                    .class(XSD_STRING)
                    .binding(),
            )
            .output(GZIP)
            // `Exists` answers `true`/`false` as text, which is a second face and therefore
            // a second declaration: a face the manifold does not announce is one no caller
            // can plan for.
            .output("text/plain")
    }
}

// ---------------------------------------------------------------- restoring

struct Restore {
    backups: Backups,
}

#[async_trait]
impl Endpoint for Restore {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match inv.request.verb {
            Verb::Sink => self.restore(inv).await,
            other => Err(unsupported("gonk-restore", other)),
        }
    }

    fn name(&self) -> &str {
        "gonk-restore"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-restore")
            .title("Load a backup into a NEW store directory, and verify it")
            .summary(
                "Expands the piped archive (gzip or plain N-Quads), opens a store at \
                 `into`, loads every quad, and then compares the restored dataset's GRAPH \
                 SET and PER-GRAPH counts against the archive — not just a total, because a \
                 total is exactly what a collapsed-to-one-graph dump preserves. `into` must \
                 not exist or must be empty, and may not be the live store: a merge into a \
                 non-empty dataset is a UNION, which resurrects deleted items and can never \
                 reproduce the state that was backed up. Swapping the restored directory \
                 into place is an operator's act with this server stopped.",
            )
            .verb(Verb::Sink)
            .verb(Verb::Meta)
            .requires(CAP_RESTORE)
            .input(
                ArgSpec::new("content")
                    .summary("the archive: a gzip stream or plain N-Quads; filled from the pipe")
                    .class(XSD_STRING),
            )
            .input(
                ArgSpec::new("into")
                    .summary(
                        "the NEW store directory to build — absent or empty, never the live one",
                    )
                    .class(XSD_STRING),
            )
            .output("text/plain")
    }
}

impl Restore {
    async fn restore(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let into = PathBuf::from(inv.inline_str("into")?);
        self.check_target(&into)?;
        let content = inv.inline_arg("content")?;
        let (nquads, how) = if content.starts_with(&[0x1f, 0x8b]) {
            let expanded = inv
                .issue(
                    Request::new(Verb::Source, iri("urn:decompress:gzip")?)
                        .with_arg("content", ArgRef::Inline(content.to_vec()))
                        .with_arg("as", ArgRef::Inline(N_QUADS.as_bytes().to_vec()))
                        .with_arg(
                            "max-bytes",
                            ArgRef::Inline(RESTORE_MAX_BYTES.to_string().into_bytes()),
                        ),
                )
                .await?;
            (expanded.bytes, "gzip")
        } else {
            (content.to_vec(), "plain n-quads")
        };
        let expected = counts_in_nquads(&nquads)?;
        let expected_total: u64 = expected.values().sum();

        std::fs::create_dir_all(&into)
            .map_err(|e| Error::Endpoint(format!("creating {}: {e}", into.display())))?;
        let store = ikigai_store::DurableStore::open(&into)
            .map_err(|e| Error::Endpoint(format!("opening a store at {}: {e}", into.display())))?;
        // A kernel of its own over the NEW store. The restore goes through
        // `urn:iki:store:load` — the same door a bulk load comes in by anywhere else —
        // rather than through a private back entrance, and it runs under exactly the write
        // token that door enforces.
        let restored = Kernel::new(Arc::new(ikigai_store::space(store)));
        let write = Capability::scoped([ikigai_store::CAP_WRITE]);
        restored
            .issue(
                Request::new(Verb::Sink, iri("urn:iki:store:load")?)
                    .with_arg("content", ArgRef::Inline(nquads.clone()))
                    .with_arg("format", ArgRef::Inline(N_QUADS.as_bytes().to_vec())),
                &write,
            )
            .await?;

        let read = Capability::scoped([ikigai_store::CAP_READ]);
        let actual = graph_counts(&restored, &read).await?;
        let actual_total: u64 = actual.values().sum();
        // Drop the store so the RocksDB lock is released before the caller is told the
        // directory is ready to be moved into place.
        drop(restored);

        let mut differences: Vec<String> = Vec::new();
        for (graph, count) in &expected {
            match actual.get(graph) {
                Some(got) if got == count => {}
                Some(got) => {
                    differences.push(format!("{}: archive {count}, restored {got}", shown(graph)))
                }
                None => differences.push(format!("{}: in the archive, MISSING", shown(graph))),
            }
        }
        for graph in actual.keys() {
            if !expected.contains_key(graph) {
                differences.push(format!("{}: restored, NOT in the archive", shown(graph)));
            }
        }
        if !differences.is_empty() {
            return Err(Error::Endpoint(format!(
                "the restored dataset does not match the archive:\n  {}\n  (archive \
                 {expected_total} quads in {} graph(s); restored {actual_total} in {})",
                differences.join("\n  "),
                expected.len(),
                actual.len()
            )));
        }

        let mut report = format!(
            "restored {expected_total} quads into {} ({how}, sha256 {})\n",
            into.display(),
            digest(content)
        );
        for (graph, count) in &actual {
            report.push_str(&format!("  {count:>8}  {}\n", shown(graph)));
        }
        report.push_str(
            "  graph set and per-graph counts match the archive exactly\n  this server still \
             holds the LIVE store; to adopt this one, stop gonk and swap the directories\n",
        );
        Ok(plain(report))
    }

    /// `into` is not the live store, and is not a directory that already holds something.
    fn check_target(&self, into: &Path) -> Result<()> {
        let live = &self.backups.settings.store_path;
        let same = into == live
            || match (std::fs::canonicalize(into), std::fs::canonicalize(live)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
        if same {
            return Err(Error::InvalidArgument {
                name: "into".to_string(),
                detail: format!(
                    "{} is the LIVE store this server holds. A restore replaces; loading an \
                     archive over a live dataset merges, which resurrects deleted items and \
                     can never reproduce the backed-up state. Restore into a new directory \
                     and swap it in with the server stopped.",
                    live.display()
                ),
            });
        }
        if into.is_dir() {
            let empty = std::fs::read_dir(into)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if !empty {
                return Err(Error::InvalidArgument {
                    name: "into".to_string(),
                    detail: format!(
                        "{} is not empty. Restoring into a non-empty store is a MERGE, not a \
                         restore: the union of two datasets brings back everything either of \
                         them deleted. Name a directory that does not exist yet.",
                        into.display()
                    ),
                });
            }
        } else if into.exists() {
            return Err(Error::InvalidArgument {
                name: "into".to_string(),
                detail: format!("{} exists and is not a directory", into.display()),
            });
        }
        Ok(())
    }
}

/// The per-graph counts of a dataset, read through `urn:iki:store:select`.
async fn graph_counts(kernel: &Kernel, capability: &Capability) -> Result<BTreeMap<String, u64>> {
    let query = "SELECT ?g (COUNT(*) AS ?n) WHERE { { GRAPH ?g { ?s ?p ?o } } UNION \
                 { ?s ?p ?o } } GROUP BY ?g";
    let results = kernel
        .issue(
            Request::new(Verb::Source, iri(BACKUP_SELECT)?)
                .with_arg("query", ArgRef::Inline(query.as_bytes().to_vec()))
                .with_arg(
                    "as",
                    ArgRef::Inline(b"application/sparql-results+json".to_vec()),
                ),
            capability,
        )
        .await?;
    let parser = QueryResultsParser::from_format(QueryResultsFormat::Json);
    let Ok(ReaderQueryResultsParserOutput::Solutions(solutions)) =
        parser.for_reader(results.bytes.as_slice())
    else {
        return Err(Error::Endpoint(
            "counting the restored graphs: not a solution set".into(),
        ));
    };
    let mut counts = BTreeMap::new();
    for solution in solutions {
        let solution = solution.map_err(|e| Error::Endpoint(format!("counting: {e}")))?;
        let key = match solution.get("g") {
            None => DEFAULT_GRAPH_KEY.to_string(),
            Some(Term::NamedNode(node)) => node.as_str().to_string(),
            Some(Term::BlankNode(node)) => format!("_:{}", node.as_str()),
            other => {
                return Err(Error::Endpoint(format!(
                    "a graph that is not an IRI: {other:?}"
                )))
            }
        };
        let Some(Term::Literal(literal)) = solution.get("n") else {
            return Err(Error::Endpoint("a count that is not a literal".into()));
        };
        let count: u64 = literal
            .value()
            .parse()
            .map_err(|_| Error::Endpoint("a count that is not a number".into()))?;
        counts.insert(key, count);
    }
    Ok(counts)
}

// ---------------------------------------------------------------- odds and ends

/// How a graph name reads in a report.
fn shown(graph: &str) -> &str {
    if graph.is_empty() {
        "(default graph)"
    } else {
        graph
    }
}

fn iri(text: &str) -> Result<Iri> {
    Iri::parse(text).map_err(|e| Error::Endpoint(format!("`{text}` is not an IRI: {e}")))
}

fn plain(text: String) -> Representation {
    Representation::new(
        ReprType::new("text/plain").with_param("charset", "utf-8"),
        text.into_bytes(),
    )
}

fn unsupported(id: &str, verb: Verb) -> Error {
    Error::Endpoint(format!("{id} does not support the {verb:?} verb"))
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A duration as an operator reads it.
fn humanize(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0 => "less than a second".to_string(),
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h{}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d{}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// The fields a sidecar records.
struct MetaFields<'a> {
    archive: &'a str,
    taken_at: &'a str,
    taken_at_ms: u64,
    quads: u64,
    graphs: &'a BTreeMap<String, u64>,
    nquads_bytes: usize,
    nquads_sha256: &'a str,
    archive_bytes: usize,
    archive_sha256: &'a str,
    store_quads_after_dump: u64,
}

fn meta_json(fields: &MetaFields<'_>) -> String {
    let graphs: Vec<String> = fields
        .graphs
        .iter()
        .map(|(graph, count)| format!("{}:{count}", quote(graph)))
        .collect();
    format!(
        "{{\"archive\":{},\"taken_at\":{},\"taken_at_ms\":{},\"quads\":{},\
         \"graphs\":{{{}}},\"nquads_bytes\":{},\"nquads_sha256\":{},\
         \"archive_bytes\":{},\"archive_sha256\":{},\"store_quads_after_dump\":{}}}\n",
        quote(fields.archive),
        quote(fields.taken_at),
        fields.taken_at_ms,
        fields.quads,
        graphs.join(","),
        fields.nquads_bytes,
        quote(fields.nquads_sha256),
        fields.archive_bytes,
        quote(fields.archive_sha256),
        fields.store_quads_after_dump,
    )
}

/// A JSON string literal. The only values that reach it are IRIs, file names, digests and
/// one line of a job's last output, but the last of those is an error message from
/// anywhere, so it is escaped properly rather than trusted to be tame.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One scalar field out of a sidecar, by name — enough to render a status line without
/// taking a JSON parser as a dependency for four keys this module wrote itself.
///
/// ⚠ It reads gonk's OWN sidecars, which this module is the only writer of. It is not a
/// JSON parser and must not be pointed at anyone else's document.
fn field(meta: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let rest = &meta[meta.find(&needle)? + needle.len()..];
    let rest = rest.trim_start();
    if let Some(body) = rest.strip_prefix('"') {
        let end = body.find('"')?;
        Some(body[..end].to_string())
    } else {
        let end = rest.find([',', '}'])?;
        Some(rest[..end].trim().to_string())
    }
}

/// The `graphs` object of a sidecar, rendered as one line.
fn graph_line(meta: &str) -> String {
    let Some(start) = meta.find("\"graphs\":{") else {
        return "(not recorded)".to_string();
    };
    let body = &meta[start + "\"graphs\":{".len()..];
    let Some(end) = body.find('}') else {
        return "(not recorded)".to_string();
    };
    let body = &body[..end];
    if body.is_empty() {
        return "(none)".to_string();
    }
    body.replace("\":", "\" ").replace(',', ", ")
}

// ---------------------------------------------------------------- the clock

/// `2026-09-16T172813Z` — a file name that sorts chronologically and has no colons in it,
/// because a colon in a file name is a path separator on one platform and an escape on
/// several tools.
pub fn stamp_compact(millis: u64) -> String {
    let (y, mo, d, h, mi, s) = civil(millis);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// `2026-09-16T17:28:13Z` — the same instant as an operator and an `xsd:dateTime` read it.
pub fn stamp_iso(millis: u64) -> String {
    let (y, mo, d, h, mi, s) = civil(millis);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Milliseconds since the epoch → UTC civil time.
///
/// ⚠ **This is the THIRD copy of Howard Hinnant's `civil_from_days` in the ecosystem**
/// (`ikigai-ledger`'s `sparql.rs` and `ikigai-log`'s `line.rs` hold the others), and it is
/// a copy rather than a shared seam on purpose: three correct copies of a closed-form
/// algorithm with no drift is count without drift, not duplication that justifies an API.
/// What would justify one is a branch none of them has taken. Reported to the hub instead
/// of fixed sideways.
fn civil(millis: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (millis / 86_400_000) as i64;
    let rest = millis % 86_400_000;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        m,
        d,
        rest / 3_600_000,
        (rest % 3_600_000) / 60_000,
        (rest % 60_000) / 1000,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ The stamp is a file name an operator reads and a sort key the rotation depends
    /// on, so it is pinned at literals rather than round-tripped against itself.
    #[test]
    fn a_stamp_is_the_instant_it_claims() {
        // 2026-09-16T17:28:13Z — the instant the hub's stopgap archive was taken.
        let millis = 1_789_579_693_000;
        assert_eq!(stamp_iso(millis), "2026-09-16T17:28:13Z");
        assert_eq!(stamp_compact(millis), "2026-09-16T172813Z");
        assert_eq!(stamp_iso(0), "1970-01-01T00:00:00Z");
        // A leap day, which is where a civil-time algorithm goes wrong if it is going to.
        assert_eq!(stamp_iso(1_709_164_800_000), "2024-02-29T00:00:00Z");
    }

    /// Archive names sort chronologically as plain strings — which is what makes
    /// "prune the oldest" a sort rather than a stat of every file's mtime.
    #[test]
    fn archive_names_sort_by_time() {
        let mut names = [
            format!(
                "gonk-store-{}{ARCHIVE_SUFFIX}",
                stamp_compact(1_789_579_693_000)
            ),
            format!(
                "gonk-store-{}{ARCHIVE_SUFFIX}",
                stamp_compact(1_700_000_000_000)
            ),
            format!(
                "gonk-store-{}{ARCHIVE_SUFFIX}",
                stamp_compact(1_789_666_093_000)
            ),
        ];
        names.sort();
        assert_eq!(names[0], "gonk-store-2023-11-14T221320Z.nq.gz");
        assert_eq!(names[2], "gonk-store-2026-09-17T172813Z.nq.gz");
    }

    /// ★ `{name}` comes from a caller and names a file this endpoint reads. Every escape
    /// out of the backup directory is refused, not sanitized.
    #[test]
    fn an_archive_name_cannot_leave_the_backup_directory() {
        let dir = Path::new("/tmp/backups");
        assert!(archive_path(dir, "../../etc/passwd").is_err());
        assert!(archive_path(dir, "a/b.nq.gz").is_err());
        assert!(archive_path(dir, "..nq.gz").is_err(), "contains ..");
        assert!(archive_path(dir, "").is_err());
        assert!(archive_path(dir, "notes.txt").is_err(), "not an archive");
        assert_eq!(
            archive_path(dir, "gonk-store-2026-09-16T172813Z.nq.gz").unwrap(),
            dir.join("gonk-store-2026-09-16T172813Z.nq.gz")
        );
    }

    #[test]
    fn a_sidecar_reads_back_the_fields_it_wrote() {
        let mut graphs = BTreeMap::new();
        graphs.insert("urn:iki:ledger:graph:default".to_string(), 2850u64);
        graphs.insert(DEFAULT_GRAPH_KEY.to_string(), 0u64);
        let json = meta_json(&MetaFields {
            archive: "gonk-store-2026-09-16T172813Z.nq.gz",
            taken_at: "2026-09-16T17:28:13Z",
            taken_at_ms: 1_789_579_693_000,
            quads: 2850,
            graphs: &graphs,
            nquads_bytes: 784_085,
            nquads_sha256: "abc",
            archive_bytes: 91_234,
            archive_sha256: "def",
            store_quads_after_dump: 2850,
        });
        assert_eq!(field(&json, "quads").as_deref(), Some("2850"));
        assert_eq!(
            field(&json, "taken_at").as_deref(),
            Some("2026-09-16T17:28:13Z")
        );
        assert_eq!(field(&json, "nquads_sha256").as_deref(), Some("abc"));
        assert_eq!(field(&json, "archive_bytes").as_deref(), Some("91234"));
        assert!(graph_line(&json).contains("urn:iki:ledger:graph:default"));
    }

    /// A ledger body carries newlines, quotation marks and trailing periods; a JSON writer
    /// that did not escape them would write a sidecar nothing could read.
    #[test]
    fn a_json_string_escapes_what_it_must() {
        assert_eq!(quote("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        assert_eq!(quote("\u{1}"), "\"\\u0001\"");
    }

    /// ★ The N-Quads writer is oxigraph's, and this is the row it exists for: a literal
    /// holding a quotation mark, a newline and a trailing period, in a named graph.
    #[test]
    fn a_literal_with_a_trailing_period_survives_the_dump() {
        let results = br#"{"head":{"vars":["g","s","p","o"]},"results":{"bindings":[
            {"g":{"type":"uri","value":"urn:iki:ledger:graph:default"},
             "s":{"type":"uri","value":"urn:iki:ledger:default:item:1"},
             "p":{"type":"uri","value":"urn:ik:body"},
             "o":{"type":"literal","value":"he said \"stop\".\nThen ."}},
            {"s":{"type":"uri","value":"urn:a"},
             "p":{"type":"uri","value":"urn:b"},
             "o":{"type":"uri","value":"urn:c"}}]}}"#;
        let dump = quads_from_results(results).expect("parses");
        assert_eq!(dump.quads(), 2);
        assert_eq!(dump.graphs["urn:iki:ledger:graph:default"], 1);
        assert_eq!(dump.graphs[DEFAULT_GRAPH_KEY], 1, "the default-graph row");
        // The round trip is the assertion: re-reading the bytes gives the same shape back.
        let back = counts_in_nquads(&dump.nquads).expect("re-parses");
        assert_eq!(back, dump.graphs);
        let text = String::from_utf8(dump.nquads.clone()).unwrap();
        assert_eq!(text.lines().count(), 2, "one line per quad: {text}");
        assert!(
            text.contains("<urn:iki:ledger:graph:default> ."),
            "the named-graph row is graph-qualified: {text}"
        );
    }

    /// The whole point of the format, as one test: a graph-qualified dump keeps the
    /// partition, and the count alone would not have noticed if it had not.
    #[test]
    fn the_dump_keeps_the_graph_partition_that_a_triple_format_would_collapse() {
        let results = br#"{"head":{"vars":["g","s","p","o"]},"results":{"bindings":[
            {"g":{"type":"uri","value":"urn:iki:ledger:graph:a"},
             "s":{"type":"uri","value":"urn:x"},"p":{"type":"uri","value":"urn:p"},
             "o":{"type":"uri","value":"urn:y"}},
            {"g":{"type":"uri","value":"urn:iki:ledger:graph:b"},
             "s":{"type":"uri","value":"urn:x"},"p":{"type":"uri","value":"urn:p"},
             "o":{"type":"uri","value":"urn:y"}}]}}"#;
        let dump = quads_from_results(results).expect("parses");
        assert_eq!(dump.quads(), 2);
        assert_eq!(dump.graphs.len(), 2, "two tenants, two graphs");
        let back = counts_in_nquads(&dump.nquads).expect("re-parses");
        assert_eq!(back.len(), 2, "and the file still says so");
    }

    #[test]
    fn pruning_keeps_the_newest_and_takes_the_sidecar_with_the_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        for stamp in [
            "2026-09-10T000000Z",
            "2026-09-11T000000Z",
            "2026-09-12T000000Z",
        ] {
            let name = format!("gonk-store-{stamp}{ARCHIVE_SUFFIX}");
            std::fs::write(dir.path().join(&name), b"x").unwrap();
            std::fs::write(dir.path().join(format!("{name}{META_SUFFIX}")), b"{}").unwrap();
        }
        assert_eq!(archives(dir.path()).len(), 3);
        let gone = prune(dir.path(), 2);
        assert_eq!(gone, ["gonk-store-2026-09-10T000000Z.nq.gz"]);
        let left = archives(dir.path());
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].name, "gonk-store-2026-09-11T000000Z.nq.gz");
        assert!(
            !dir.path()
                .join(format!(
                    "gonk-store-2026-09-10T000000Z{ARCHIVE_SUFFIX}{META_SUFFIX}"
                ))
                .exists(),
            "the sidecar goes with its archive — one left behind reads like a backup"
        );
    }

    /// An archive with no sidecar is listed and flagged, never counted as verified.
    #[test]
    fn an_archive_without_a_sidecar_is_reported_as_unverified() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path()
                .join(format!("gonk-store-2026-09-10T000000Z{ARCHIVE_SUFFIX}")),
            b"x",
        )
        .unwrap();
        let status = Status {
            backups: Backups {
                settings: Arc::new(Settings {
                    dir: dir.path().to_path_buf(),
                    keep: 5,
                    every: Some(Duration::from_secs(86_400)),
                    store_path: PathBuf::from("/nonexistent/store"),
                }),
                jobs: None,
            },
        };
        let text = status.text();
        assert!(text.contains("no sidecar"), "{text}");
        assert!(text.contains("every 1d0h"), "{text}");
    }

    #[test]
    fn an_empty_backup_directory_says_so_loudly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = Status {
            backups: Backups {
                settings: Arc::new(Settings {
                    dir: dir.path().to_path_buf(),
                    keep: 5,
                    every: None,
                    store_path: PathBuf::from("/nonexistent/store"),
                }),
                jobs: None,
            },
        };
        let text = status.text();
        assert!(text.contains("NO BACKUP HAS EVER BEEN TAKEN"), "{text}");
        assert!(text.contains("schedule  OFF"), "{text}");
        let json = status.json();
        assert!(json.contains("\"newest\":null"), "{json}");
        assert!(json.contains("\"timer\":null"), "{json}");
    }
}
