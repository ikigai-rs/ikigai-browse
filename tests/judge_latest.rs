//! A finding's `judge` is its LATEST verdict by time (ledger #736, minor).
//! Ported from the review-value experiment's reproduction (ledger #723),
//! which fails on 0b1ec3e: verdicts were ordered by the TEXT of
//! `dcterms:created`, and an xsd:dateTime read back in canonical form
//! (`…:00Z`, `…:00.5Z`) does not sort by time within a second.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
use oxigraph::store::Store;

fn temp_root(files: &[(&str, &str)]) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "browse-judge-latest-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, body) in files {
        std::fs::write(dir.join(name), body).unwrap();
    }
    dir
}

fn issue(
    k: &Kernel,
    verb: Verb,
    iri: &str,
    args: &[(&str, &str)],
    cap: &Capability,
) -> Result<String, Error> {
    let mut request = Request::new(verb, Iri::parse(iri.to_string()).unwrap());
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    block_on(k.issue(request, cap)).map(|r| String::from_utf8_lossy(&r.bytes).to_string())
}

fn only(repo: &str) -> Capability {
    Capability::scoped([
        format!("urn:cap:browse:read:{repo}"),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ])
}

/// Verdicts are ordered by the TEXT of `dcterms:created`
/// (`load_verdicts`), but the store serves an xsd:dateTime in canonical form
/// (`…:00Z`, `…:00.5Z`), whose text order is not its time order within a
/// second — so `judge` (documented as the LATEST verdict, and what
/// `verdict=` filters on) names the OLDER one.
#[test]
fn judge_names_the_latest_verdict() {
    let root = temp_root(&[("x.rs", "fn alpha() {}\n")]);
    let store = Arc::new(Store::new().unwrap());
    let f = "urn:iki:finding:f3";
    let nn = |s: &str| NamedNode::new(s).unwrap();
    let mut quads = vec![
        Quad::new(
            nn(f),
            nn("http://purl.org/dc/terms/description"),
            Literal::new_simple_literal("a claim"),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            nn(f),
            nn("https://ikigai-rs.dev/ns#repo"),
            Literal::new_simple_literal("demo"),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            nn(f),
            nn("https://ikigai-rs.dev/ns#path"),
            Literal::new_simple_literal("x.rs"),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            nn(f),
            nn("https://ikigai-rs.dev/ns#annotates"),
            nn("urn:repo:demo:file:x.rs"),
            GraphName::DefaultGraph,
        ),
    ];
    // judge-a at 10:00:00.000 (older) says refuted; judge-b at 10:00:00.500 (newer) says confirmed.
    for (tag, at, word) in [
        ("judge-a", "2026-10-01T10:00:00.000Z", "refuted"),
        ("judge-b", "2026-10-01T10:00:00.500Z", "confirmed"),
    ] {
        let v = format!("{f}:judge:{tag}");
        quads.push(Quad::new(
            nn(&v),
            nn("http://www.w3.org/ns/prov#used"),
            nn(f),
            GraphName::DefaultGraph,
        ));
        quads.push(Quad::new(
            nn(&v),
            nn("http://purl.org/dc/terms/type"),
            nn(&format!("urn:iki:judge:verdict:{word}")),
            GraphName::DefaultGraph,
        ));
        quads.push(Quad::new(
            nn(&v),
            nn("https://ikigai-rs.dev/ns#versionTag"),
            Literal::new_simple_literal(tag),
            GraphName::DefaultGraph,
        ));
        quads.push(Quad::new(
            nn(&v),
            nn("http://purl.org/dc/terms/created"),
            Literal::new_typed_literal(at, nn("http://www.w3.org/2001/XMLSchema#dateTime")),
            GraphName::DefaultGraph,
        ));
    }
    for q in &quads {
        store.insert(q).unwrap();
    }
    let k = Kernel::new(Arc::new(ikigai_browse::space_with_annotations(
        vec![("demo".to_string(), root)],
        Arc::clone(&store),
    )));
    let json: serde_json::Value = serde_json::from_str(
        &issue(
            &k,
            Verb::Source,
            f,
            &[("as", "application/json")],
            &only("demo"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        json["judge"]["tag"], "judge-b",
        "latest verdict is judge-b (…00.500Z); judges read back as {}",
        json["judges"]
    );
}
