//! A refusal reads as one sentence (ledger #736, minor). A string literal
//! broken across source lines without its `\` continuation carried the next
//! line's indentation into the message as a run of spaces. Ported from the
//! review-value experiment's reproduction (ledger #723), which failed on
//! 0b1ec3e.

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
use oxigraph::store::Store;

#[test]
fn the_retract_with_severity_refusal_reads_as_one_sentence() {
    let root = std::env::temp_dir().join(format!("browse-refusal-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("x.rs"), "fn alpha() {}\n").unwrap();
    let store = Arc::new(Store::new().unwrap());
    // A minimal finding record: `load_record` needs only its description;
    // the Sink checks the repo grant, then refuses on the arguments.
    let s = NamedNode::new("urn:iki:finding:f1").unwrap();
    for (p, o) in [
        ("http://purl.org/dc/terms/description", "a claim"),
        ("https://ikigai-rs.dev/ns#repo", "demo"),
        ("https://ikigai-rs.dev/ns#path", "x.rs"),
    ] {
        store
            .insert(&Quad::new(
                s.clone(),
                NamedNode::new(p).unwrap(),
                Literal::new_simple_literal(o),
                GraphName::DefaultGraph,
            ))
            .unwrap();
    }
    let k = Kernel::new(Arc::new(ikigai_browse::space_with_annotations(
        vec![("demo".to_string(), root.clone())],
        Arc::clone(&store),
    )));
    let cap = Capability::scoped([
        "urn:cap:browse:read:demo".to_string(),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ]);
    let mut request = Request::new(Verb::Sink, Iri::parse("urn:iki:finding:f1").unwrap());
    for (name, value) in [("decision", "retract"), ("severity", "major")] {
        request = request.with_arg(name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    let err = block_on(k.issue(request, &cap)).expect_err("retract with a severity is refused");
    let text = format!("{err}");
    assert!(!text.contains("  "), "garbled refusal: {text:?}");
    std::fs::remove_dir_all(&root).ok();
}
