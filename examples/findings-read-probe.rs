//! The findings-face measurement: what the queue read gonk's badge makes every
//! ten seconds costs, against a COPY of a live browse graph and the live
//! checkouts — and what each root's queue holds, state by state.
//!
//!   cargo run --release --example findings-read-probe -- <dump.nt> <reps> <name=path>…
//!
//! `<dump.nt>` is the browse graph as N-Triples. From a running host, read-only:
//!
//!   ikigai -c 'source urn:iki:store:graph-select graph=urn:iki:browse:graph:default \
//!     as=text/tab-separated-values query="SELECT ?s ?p ?o WHERE { ?s ?p ?o }"' > browse.tsv
//!   tail -n +2 browse.tsv | awk NF | awk -F'\t' -v X=http://www.w3.org/2001/XMLSchema# '
//!     { o = $3
//!       if (o ~ /^(true|false)$/) o = "\"" o "\"^^<" X "boolean>"
//!       else if (o ~ /^[-+]?[0-9]+$/) o = "\"" o "\"^^<" X "integer>"
//!       print $1 "\t" $2 "\t" o " ." }' > browse.nt
//!
//! ⚠ SPARQL TSV writes IRIs and quoted literals in N-Triples syntax but
//! abbreviates `xsd:boolean` and `xsd:integer` literals to bare Turtle tokens
//! (`true`, `0`), which N-Triples does not allow — hence the second `awk`, which
//! writes them back out typed. A row plus ` .` is then a triple. The dump is loaded into an in-memory store's default graph, so
//! nothing here can touch the live one, and each `<name=path>` is mounted as a
//! root exactly as the host names it — the current content hash of every file
//! is read from the checkout, which is what decides a file's current reading.
//!
//! Per root it reports the badge read — `urn:repo:{name}:findings`, the json
//! face at `state=pending`, the call gonk's queue makes — as rows and the
//! median / min / max wall time over `<reps>` reads after one warm-up, then the
//! `summary=states` counts when the build serves them (0.11.0 on), and the
//! file face's `proposals=` read for the root's busiest file (the other read
//! supersession joins), and — 0.12.0 on — each `group=` kind's groups, member
//! findings and read time per root. Run it on two builds to compare them: the
//! probe declares no argument the older build lacks except `summary=states` and
//! `group=`, which it reports as unavailable rather than failing on.

use std::collections::BTreeMap;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use oxigraph::model::{GraphName, Quad};
use oxigraph::store::Store;

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: findings-read-probe <dump.nt> <reps> <name=path>…";
    let dump = PathBuf::from(args.next().expect(usage));
    let reps: usize = args.next().and_then(|n| n.parse().ok()).expect(usage);
    let roots: Vec<(String, PathBuf)> = args
        .map(|arg| {
            let (name, path) = arg.split_once('=').expect("a root is name=path");
            (name.to_string(), PathBuf::from(path))
        })
        .collect();
    assert!(!roots.is_empty(), "{usage}");

    let store = Arc::new(Store::new().expect("in-memory store"));
    let started = Instant::now();
    let reader = BufReader::new(std::fs::File::open(&dump).expect("open the dump"));
    let mut triples = 0usize;
    for triple in oxttl::NTriplesParser::new().for_reader(reader) {
        let triple = triple.expect("a well-formed N-Triples line");
        store
            .insert(&Quad::new(
                triple.subject,
                triple.predicate,
                triple.object,
                GraphName::DefaultGraph,
            ))
            .expect("insert");
        triples += 1;
    }
    println!(
        "loaded {triples} triples from {} in {:?}",
        dump.display(),
        started.elapsed()
    );

    let names: Vec<String> = roots.iter().map(|(name, _)| name.clone()).collect();
    let kernel = Kernel::new(Arc::new(ikigai_browse::space_with_annotations(
        roots,
        Arc::clone(&store),
    )));
    let cap = Capability::scoped(
        names
            .iter()
            .map(|name| format!("urn:cap:browse:read:{name}"))
            .collect::<Vec<_>>(),
    );
    let read = |iri: &str, extra: &[(&str, &str)]| -> Result<(Vec<u8>, Duration), String> {
        let mut request = Request::new(Verb::Source, Iri::parse(iri.to_string()).unwrap());
        for (name, value) in extra {
            request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
        }
        let started = Instant::now();
        let repr =
            futures::executor::block_on(kernel.issue(request, &cap)).map_err(|e| format!("{e}"))?;
        Ok((repr.bytes, started.elapsed()))
    };

    println!(
        "\n{:<18} {:>6} {:>10} {:>10} {:>10}   states (pending / superseded / published / declined)",
        "root", "rows", "median", "min", "max"
    );
    for name in &names {
        let iri = format!("urn:repo:{name}:findings");
        let badge = [("state", "pending"), ("as", "application/json")];
        // One warm-up: the first read re-anchors whatever drifted since the
        // dump and persists it, which a live badge read has already done.
        let (bytes, _) = read(&iri, &badge).expect("the badge read");
        let rows = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|v| v.as_array().map(Vec::len))
            .unwrap_or(0);
        let mut times: Vec<Duration> = (0..reps)
            .map(|_| read(&iri, &badge).expect("the badge read").1)
            .collect();
        times.sort();
        let states = match read(&iri, &[("state", "pending"), ("summary", "states")]) {
            Ok((bytes, _)) => {
                let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
                let s = &v["states"];
                format!(
                    "{} / {} / {} / {}",
                    s["pending"], s["superseded"], s["published"], s["declined"]
                )
            }
            Err(_) => "n/a (this build serves no summary=states)".to_string(),
        };
        println!(
            "{name:<18} {rows:>6} {:>10.1?} {:>10.1?} {:>10.1?}   {states}",
            times[times.len() / 2],
            times[0],
            times[times.len() - 1],
        );
    }

    // `group=` (0.12.0 on): per root, per kind, the size of each lever — the
    // groups and the member findings — and what the grouped read costs. Not on
    // the badge path (a host fetches it when a person opens a batch view), but
    // it is the pending listing plus one in-memory pass per kind, so it should
    // cost what the badge read costs.
    println!(
        "\n{:<18} {:<16} {:>7} {:>9} {:>7} {:>10} {:>10}",
        "root", "group=", "groups", "findings", "marked", "median", "max"
    );
    for name in &names {
        let iri = format!("urn:repo:{name}:findings");
        for kind in ["recurrence", "near-duplicate", "comment-shape", "file"] {
            let args = [("group", kind), ("as", "application/json")];
            let Ok((bytes, _)) = read(&iri, &args) else {
                println!("{name:<18} {kind:<16} n/a (this build serves no group=)");
                break;
            };
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let groups = v["groups"].as_array().map(Vec::len).unwrap_or(0);
            let members: Vec<&serde_json::Value> = v["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|g| g["members"].as_array().into_iter().flatten())
                .collect();
            let findings = members.len();
            // Members that already arrived carrying the 0.9.0 mark — the rest
            // are what `recurrence` adds over the mark.
            let marked = members
                .iter()
                .filter(|m| !m["prior_decision"].is_null())
                .count();
            let mut times: Vec<Duration> = (0..reps)
                .map(|_| read(&iri, &args).expect("the group read").1)
                .collect();
            times.sort();
            println!(
                "{name:<18} {kind:<16} {groups:>7} {findings:>9} {marked:>7} {:>10.1?} {:>10.1?}",
                times[times.len() / 2],
                times[times.len() - 1],
            );
        }
    }

    // The file face's `proposals=` read — the other read supersession joins —
    // on each root's file with the most findings on record.
    println!(
        "\n{:<18} {:<48} {:>10} {:>10}",
        "root", "busiest file (proposals=critical,major)", "median", "max"
    );
    for name in &names {
        let (bytes, _) = read(
            &format!("urn:repo:{name}:findings"),
            &[("state", "all"), ("as", "application/json")],
        )
        .expect("the listing");
        let rows: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        let mut per_file: BTreeMap<String, usize> = BTreeMap::new();
        for row in rows.as_array().into_iter().flatten() {
            if let Some(path) = row["path"].as_str().filter(|p| !p.is_empty()) {
                *per_file.entry(path.to_string()).or_default() += 1;
            }
        }
        let Some((path, _)) = per_file.iter().max_by_key(|(_, n)| **n) else {
            continue;
        };
        let iri = format!("urn:repo:{name}:file:{}", ikigai_iri_encode(path));
        let face = [("as", "text/html"), ("proposals", "critical,major")];
        if read(&iri, &face).is_err() {
            println!("{name:<18} {path:<48} (file face refused — gone from the checkout?)");
            continue;
        }
        let mut times: Vec<Duration> = (0..reps)
            .map(|_| read(&iri, &face).expect("the file face").1)
            .collect();
        times.sort();
        println!(
            "{name:<18} {path:<48} {:>10.1?} {:>10.1?}",
            times[times.len() / 2],
            times[times.len() - 1],
        );
    }
}

/// The path segment of a browse IRI — the crate's own `iri_encode` rule,
/// restated because the probe sits outside the crate: alphanumerics and
/// `-._~/!$&'()*+,;=:@` stay, every other byte is percent-encoded.
fn ikigai_iri_encode(path: &str) -> String {
    const SAFE: &[u8] = b"-._~/!$&'()*+,;=:@";
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || SAFE.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
