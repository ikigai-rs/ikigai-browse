//! Every Turtle face states the STORED value (ledger #736, item 3): a
//! carriage return is escaped, never dropped. Ported from the review-value
//! experiment's reproduction (ledger #723), which failed on 0b1ec3e — the
//! json face kept the `\r` and the Turtle face, read from the same record,
//! did not.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use oxigraph::model::NamedNode;
use oxigraph::store::Store;

fn temp_root(files: &[(&str, &str)]) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "browse-crlf-{}-{}",
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

#[test]
fn the_turtle_face_keeps_a_crlf_quote() {
    let root = temp_root(&[("w.txt", "line one\r\nline two\r\n")]);
    let store = Arc::new(Store::new().unwrap());
    let k = Kernel::new(Arc::new(ikigai_browse::space_with_annotations(
        vec![("demo".to_string(), root)],
        Arc::clone(&store),
    )));
    let cap = Capability::scoped([
        "urn:cap:browse:read:demo".to_string(),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ]);
    issue(
        &k,
        Verb::Sink,
        "urn:iki:annotation:crlf",
        &[
            ("target", "urn:repo:demo:file:w.txt"),
            ("exact", "line one\r\nline two"),
            ("body", "spans a CRLF"),
        ],
        &cap,
    )
    .expect("annotate");
    let json: serde_json::Value = serde_json::from_str(
        &issue(
            &k,
            Verb::Source,
            "urn:iki:annotation:crlf",
            &[("as", "application/json")],
            &cap,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        json["exact"], "line one\r\nline two",
        "json keeps the stored quote"
    );
    let ttl = issue(
        &k,
        Verb::Source,
        "urn:iki:annotation:crlf",
        &[("as", "text/turtle")],
        &cap,
    )
    .unwrap();
    let parsed = Store::new().unwrap();
    parsed
        .load_from_reader(oxigraph::io::RdfFormat::Turtle, ttl.as_bytes())
        .unwrap();
    let exact = NamedNode::new("http://www.w3.org/ns/oa#exact").unwrap();
    let values: Vec<String> = parsed
        .quads_for_pattern(None, Some(exact.as_ref()), None, None)
        .map(|q| match q.unwrap().object {
            oxigraph::model::Term::Literal(l) => l.value().to_string(),
            other => other.to_string(),
        })
        .collect();
    assert_eq!(
        values,
        vec!["line one\r\nline two".to_string()],
        "turtle face: {ttl}"
    );
}
