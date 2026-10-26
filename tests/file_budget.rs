//! File content and hashes must refuse oversized files without returning a prefix.
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};

#[test]
fn one_byte_over_file_budget_is_refused() {
    let root = std::env::temp_dir().join(format!("browse-file-budget-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let file = std::fs::File::create(root.join("large.bin")).unwrap();
    // Keep the literal boundary pinned to the documented public limit.
    assert_eq!(ikigai_browse::MAX_FILE_BYTES, 16 * 1024 * 1024);
    file.set_len(ikigai_browse::MAX_FILE_BYTES + 1).unwrap();
    let kernel = Kernel::new(Arc::new(ikigai_browse::space(vec![(
        "demo".into(),
        root.clone(),
    )])));
    let cap = Capability::scoped(["urn:cap:browse:read:demo"]);
    let mut accepted = Vec::new();
    for (iri, face) in [
        ("urn:repo:demo:file:large.bin", "application/octet-stream"),
        ("urn:repo:demo:file:large.bin", "text/html"),
        ("urn:repo:demo:hash:large.bin", "text/plain"),
        ("urn:repo:demo:hash:large.bin", "application/json"),
        ("urn:repo:demo:hash", "text/plain"),
    ] {
        let request = Request::new(Verb::Source, Iri::parse(iri).unwrap())
            .with_arg("as", ArgRef::Inline(face.as_bytes().to_vec()));
        let result = block_on(kernel.issue(request, &cap));
        if !matches!(result, Err(Error::OverBudget { .. })) {
            accepted.push(iri);
        }
    }
    assert!(
        accepted.is_empty(),
        "must refuse oversized input: {accepted:?}"
    );

    // At the boundary, preserve every byte and hash the complete content.
    file.set_len(ikigai_browse::MAX_FILE_BYTES).unwrap();
    let content = block_on(kernel.issue(
        Request::new(
            Verb::Source,
            Iri::parse("urn:repo:demo:file:large.bin").unwrap(),
        ),
        &cap,
    ))
    .unwrap();
    assert_eq!(content.bytes.len() as u64, ikigai_browse::MAX_FILE_BYTES);
    assert!(content.bytes.iter().all(|byte| *byte == 0));
    let hash = block_on(kernel.issue(
        Request::new(
            Verb::Source,
            Iri::parse("urn:repo:demo:hash:large.bin").unwrap(),
        ),
        &cap,
    ))
    .unwrap();
    use sha2::{Digest, Sha256};
    assert_eq!(
        hash.bytes,
        format!("sha256:{:x}", Sha256::digest(&content.bytes)).as_bytes()
    );
    std::fs::remove_dir_all(root).unwrap();
}
