//! Build a throwaway, plasma-shaped, PRE-0.3.0 browse store on disk so the
//! migration can be rehearsed against a real RocksDB store before it is run
//! against a real one.
//!
//! ```text
//! cargo run --features migrate --example migration-rehearsal -- /tmp/rehearsal
//! cargo run --features migrate --bin migrate-annotation-ns -- /tmp/rehearsal
//! cargo run --features migrate --bin migrate-annotation-ns -- /tmp/rehearsal --commit
//!
//! # …and to see the lock refusal the tool is supposed to give a live store:
//! cargo run --features migrate --example migration-rehearsal -- /tmp/rehearsal --hold 30 &
//! cargo run --features migrate --bin migrate-annotation-ns -- /tmp/rehearsal
//! ```
//!
//! ⚠ It writes 14 annotations in the OLD namespace — the shape plasma is
//! stuck in: 28 `oa:hasSelector` and 14 `prov:generated` references, and one
//! `oa:exact` literal that quotes a line of source code containing the old
//! prefix. Point it at a scratch directory, never at `~/.ikigai/browse-store`.

use std::path::PathBuf;

use oxigraph::model::{GraphName, Literal, NamedNode, Quad, Term};
use oxigraph::store::Store;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const OA: &str = "http://www.w3.org/ns/oa#";
const IK: &str = "https://ikigai-rs.dev/ns#";
const PROV_GENERATED: &str = "http://www.w3.org/ns/prov#generated";
const PASS: &str = "urn:ikigai:browse:review:ikigai-browse:sha256:abc:review-v1:pr:11";

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).expect("fixture IRIs are valid")
}

fn quad(s: &str, p: &str, o: Term) -> Quad {
    Quad::new(iri(s), iri(p), o, GraphName::DefaultGraph)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .expect("usage: migration-rehearsal <scratch-dir> [--hold <seconds>]");
    let hold: Option<u64> = match args.next().as_deref() {
        Some("--hold") => Some(
            args.next()
                .and_then(|s| s.parse().ok())
                .expect("--hold wants a number of seconds"),
        ),
        Some(other) => panic!("unexpected argument {other}"),
        None => None,
    };

    let store = Store::open(&path).expect("open scratch store");
    let lit = |s: &str| Term::Literal(Literal::new_simple_literal(s));

    for n in 0..14 {
        let ann = format!("urn:annotation:m{n}");
        let quote = format!("{ann}:selector:quote");
        let position = format!("{ann}:selector:position");
        let exact = if n == 0 {
            r#"let Some(id) = iri.strip_prefix("urn:annotation:") else {"#
        } else {
            "fn list_annotations(store: &Store)"
        };
        for q in [
            quad(
                &ann,
                RDF_TYPE,
                Term::NamedNode(iri(&format!("{OA}Annotation"))),
            ),
            quad(&ann, &format!("{OA}bodyValue"), lit("a finding")),
            quad(
                &ann,
                &format!("{IK}annotates"),
                Term::NamedNode(iri("urn:repo:ikigai-browse:pr:11")),
            ),
            quad(&ann, &format!("{IK}repo"), lit("ikigai-browse")),
            quad(&ann, &format!("{IK}contentHash"), lit("sha256:abc")),
            quad(
                &ann,
                &format!("{OA}hasSelector"),
                Term::NamedNode(iri(&quote)),
            ),
            quad(
                &ann,
                &format!("{OA}hasSelector"),
                Term::NamedNode(iri(&position)),
            ),
            quad(
                &quote,
                RDF_TYPE,
                Term::NamedNode(iri(&format!("{OA}TextQuoteSelector"))),
            ),
            quad(&quote, &format!("{OA}exact"), lit(exact)),
            quad(
                &position,
                RDF_TYPE,
                Term::NamedNode(iri(&format!("{OA}TextPositionSelector"))),
            ),
            quad(&position, &format!("{OA}start"), lit("120")),
            quad(PASS, PROV_GENERATED, Term::NamedNode(iri(&ann))),
        ] {
            store.insert(q.as_ref()).expect("insert fixture quad");
        }
    }
    store.flush().expect("flush");
    println!(
        "wrote {} quads of pre-0.3.0 annotation data to {}",
        store.len().expect("len"),
        path.display()
    );

    if let Some(seconds) = hold {
        println!(
            "holding the RocksDB lock for {seconds}s — run the migration now to see it refuse"
        );
        std::thread::sleep(std::time::Duration::from_secs(seconds));
    }
}
