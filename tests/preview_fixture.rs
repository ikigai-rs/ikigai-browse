//! The page preview's finding fixture still renders (ledger #458).
//!
//! `examples/page-preview.rs` is where a human LOOKS at the faces, and a finding
//! can only come from a review pass, so the preview loads a fixture graph of two
//! findings instead of calling a model. This test loads the same graph over this
//! checkout, the way the preview does, and checks that both cards render: a change
//! to the record format, or a fixture that drifted out of the standing set, would
//! otherwise empty the preview without a sound.

use std::path::PathBuf;
use std::sync::Arc;

use ikigai_browse::{ExplainConfig, Mount};
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use oxigraph::io::RdfFormat;
use oxigraph::store::Store;

const FIXTURE: &str = include_str!("../examples/fixtures/preview-findings.ttl");
const PENDING: &str = "urn:iki:finding:4afdddf1bc3d4b44f1b0444b";
const DECLINED: &str = "urn:iki:finding:2f8cfa5eb5169797bd6d74ac";

fn kernel() -> Kernel {
    let store = Arc::new(Store::new().expect("an in-memory store"));
    store
        .load_from_reader(RdfFormat::Turtle, FIXTURE.as_bytes())
        .expect("the fixture is Turtle");
    let space = Mount::new([(
        "self".to_string(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    )])
    .explain(ExplainConfig::new(Arc::clone(&store)))
    .config_home(None)
    .space();
    Kernel::new(Arc::new(space))
}

fn source(kernel: &Kernel, target: &str, args: &[(&str, &str)]) -> String {
    let mut request = Request::new(Verb::Source, Iri::parse(target.to_string()).unwrap());
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    let repr = futures::executor::block_on(
        kernel.issue(request, &Capability::scoped(["urn:cap:browse:read:self"])),
    )
    .unwrap_or_else(|e| panic!("{target}: {e}"));
    String::from_utf8(repr.bytes).expect("UTF-8")
}

/// The fixture's two findings read back in the states the preview shows them
/// in: one pending (not superseded), one declined with its reason.
#[test]
fn the_fixture_findings_read_back_pending_and_declined() {
    let k = kernel();
    let pending: serde_json::Value =
        serde_json::from_str(&source(&k, PENDING, &[("as", "application/json")])).unwrap();
    assert_eq!(pending["state"], "pending", "{pending}");
    assert_eq!(pending["severity"], "major", "{pending}");
    let declined: serde_json::Value =
        serde_json::from_str(&source(&k, DECLINED, &[("as", "application/json")])).unwrap();
    assert_eq!(declined["state"], "declined", "{declined}");
    assert_eq!(declined["decision"]["reason"], "no-issue", "{declined}");
}

/// ★ The cards the preview exists to show: the queue page renders the pending
/// card with its severity badge and decision form, and the declined card with
/// its record and the revise forms.
#[test]
fn the_preview_queue_renders_both_finding_cards() {
    let k = kernel();
    let queue = source(
        &k,
        "urn:repo:self:findings",
        &[("as", "text/html"), ("state", "pending")],
    );
    assert!(queue.contains("browse-finding-pending"), "{queue}");
    assert!(queue.contains("browse-finding-severity-major"), "{queue}");
    assert!(
        queue.contains("name=\"decision\" value=\"publish\""),
        "{queue}"
    );
    let declined = source(
        &k,
        "urn:repo:self:findings",
        &[("as", "text/html"), ("state", "declined")],
    );
    assert!(declined.contains("browse-finding-declined"), "{declined}");
    assert!(declined.contains("revise this decision"), "{declined}");
}
