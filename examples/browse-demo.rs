//! ikigai-browse browsing its own repository through the kernel.
//!
//!   cargo run --example browse-demo
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use std::sync::Arc;

fn main() {
    // This crate's own checkout, mounted as the root named "self".
    let kernel = Kernel::new(Arc::new(ikigai_browse::space([(
        "self".to_string(),
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    )])));
    // A capability scoped to exactly "may browse self" — nothing else.
    let cap = Capability::scoped(["urn:cap:browse:read:self"]);

    for (iri, as_type) in [
        ("urn:repo:self:state", None),
        ("urn:repo:self:tree", None),
        ("urn:repo:self:tree:src", None),
        ("urn:repo:self:tree", Some("text/turtle")),
    ] {
        let mut req = Request::new(Verb::Source, Iri::parse(iri).unwrap());
        if let Some(t) = as_type {
            req = req.with_arg("as", ArgRef::Inline(t.as_bytes().to_vec()));
        }
        match futures::executor::block_on(kernel.issue(req, &cap)) {
            Ok(repr) => {
                let body = String::from_utf8_lossy(&repr.bytes);
                let face = as_type.map(|t| format!(" as={t}")).unwrap_or_default();
                println!("\n$ source {iri}{face}\n{}", body.trim_end());
            }
            Err(e) => println!("\n$ source {iri}\n  error: {e:?}"),
        }
    }
}
