//! The per-root grant on the annotation family's MUTATING verbs (ledger #736,
//! item 1; reproduced first by the review-value experiment, ledger #723).
//!
//! Reading an annotation is gated on the annotation's own repo, so a tenant
//! granted root `a` cannot read root `b`'s note. Delete and a Sink that
//! UPDATES an existing id must be gated on the same repo: the existing
//! annotation's, not only the new target's. Without that, a tenant who cannot
//! read a note can still destroy or overwrite it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};
use oxigraph::store::Store;

fn temp_root(files: &[(&str, &str)]) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "browse-tenancy-{}-{}",
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

fn two_roots() -> Kernel {
    let a = temp_root(&[("x.rs", "fn alpha() {}\n")]);
    let b = temp_root(&[("x.rs", "fn secret_b() {}\n")]);
    let store = Arc::new(Store::new().unwrap());
    Kernel::new(Arc::new(ikigai_browse::space_with_annotations(
        vec![("a".to_string(), a), ("b".to_string(), b)],
        store,
    )))
}

/// A tenant: the annotate key plus a browse grant on one root.
fn only(repo: &str) -> Capability {
    Capability::scoped([
        format!("urn:cap:browse:read:{repo}"),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ])
}

fn b_annotates(k: &Kernel, id: &str) {
    issue(
        k,
        Verb::Sink,
        &format!("urn:iki:annotation:{id}"),
        &[
            ("target", "urn:repo:b:file:x.rs"),
            ("exact", "fn secret_b() {}"),
            ("body", "b's note"),
        ],
        &only("b"),
    )
    .expect("b's owner annotates b");
}

#[test]
fn delete_of_another_roots_annotation_is_denied() {
    let k = two_roots();
    b_annotates(&k, "note-b");

    let read = issue(
        &k,
        Verb::Source,
        "urn:iki:annotation:note-b",
        &[],
        &only("a"),
    );
    assert!(
        matches!(read, Err(Error::Denied(_))),
        "precondition: a cannot read b's note: {read:?}"
    );

    let deleted = issue(
        &k,
        Verb::Delete,
        "urn:iki:annotation:note-b",
        &[],
        &only("a"),
    );
    assert!(
        matches!(deleted, Err(Error::Denied(_))),
        "a caller with no grant on root `b` deleted b's annotation: {deleted:?}"
    );
    let after = issue(
        &k,
        Verb::Source,
        "urn:iki:annotation:note-b",
        &[],
        &only("b"),
    );
    assert_eq!(after.as_deref(), Ok("b's note"), "b's note survives");
}

#[test]
fn sink_update_cannot_overwrite_another_roots_annotation() {
    let k = two_roots();
    b_annotates(&k, "note-b");

    let overwrite = issue(
        &k,
        Verb::Sink,
        "urn:iki:annotation:note-b",
        &[
            ("target", "urn:repo:a:file:x.rs"),
            ("exact", "fn alpha() {}"),
            ("body", "clobbered by a"),
        ],
        &only("a"),
    );
    assert!(
        matches!(overwrite, Err(Error::Denied(_))),
        "a caller with no grant on `b` replaced b's annotation: {overwrite:?}"
    );
    let after = issue(
        &k,
        Verb::Source,
        "urn:iki:annotation:note-b",
        &[],
        &only("b"),
    );
    assert_eq!(after.as_deref(), Ok("b's note"), "b's note survives");
}

/// The other half: the gate must not refuse the owner. A tenant still
/// updates and deletes its own root's annotation.
#[test]
fn the_owning_tenant_still_updates_and_deletes() {
    let k = two_roots();
    b_annotates(&k, "note-b");

    issue(
        &k,
        Verb::Sink,
        "urn:iki:annotation:note-b",
        &[
            ("target", "urn:repo:b:file:x.rs"),
            ("exact", "fn secret_b() {}"),
            ("body", "b's revised note"),
        ],
        &only("b"),
    )
    .expect("b updates its own note");
    let after = issue(
        &k,
        Verb::Source,
        "urn:iki:annotation:note-b",
        &[],
        &only("b"),
    );
    assert_eq!(after.as_deref(), Ok("b's revised note"));

    issue(
        &k,
        Verb::Delete,
        "urn:iki:annotation:note-b",
        &[],
        &only("b"),
    )
    .expect("b deletes its own note");
    let gone = issue(
        &k,
        Verb::Source,
        "urn:iki:annotation:note-b",
        &[],
        &only("b"),
    );
    assert!(matches!(gone, Err(Error::NotFound(_))), "{gone:?}");
}

/// A caller holding every browse root (the wildcard) is not a tenant: it may
/// move an annotation from one root to another, as before.
#[test]
fn the_wildcard_holder_may_retarget_across_roots() {
    let k = two_roots();
    b_annotates(&k, "note-b");
    let all = Capability::scoped([
        "urn:cap:browse:read:*".to_string(),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ]);
    issue(
        &k,
        Verb::Sink,
        "urn:iki:annotation:note-b",
        &[
            ("target", "urn:repo:a:file:x.rs"),
            ("exact", "fn alpha() {}"),
            ("body", "moved to a"),
        ],
        &all,
    )
    .expect("the wildcard holder retargets");
    let after = issue(
        &k,
        Verb::Source,
        "urn:iki:annotation:note-b",
        &[],
        &only("a"),
    );
    assert_eq!(after.as_deref(), Ok("moved to a"));
}
