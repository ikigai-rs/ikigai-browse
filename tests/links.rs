//! The tree never offers a link the resolver will not follow (ledger #1130).
//!
//! An enumerator must not outrun its resolver. Before this test the tree's
//! `.git` rule (ledger #1126) hid only links INTO `.git`; a link that escapes
//! the root, a dangling link and a self-loop were still listed in every face,
//! and the html row and the turtle node pointed at a `file:` IRI that the jail
//! then refused. A link to a directory INSIDE the root was offered as a
//! `file:` IRI too, and the file door refuses a directory, though `tree:` of
//! the same path serves it.
//!
//! The rule now: a link is listed iff the resolver serves its path, and it is
//! offered under the door that serves what it resolves to (`tree:` for a
//! directory, `file:` for a file). Hiding, not "listed as unreadable": nothing
//! a caller can hold opens such a link, so it is absent from the browsed space
//! in the same sense `.git` is, and every other enumerator in the crate (the
//! merkle hash, the explain rollup, the judge's test index) already skips links.
//!
//! The fixture is hermetic: scratch directories, no real credential.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Verb};

const SECRET: &str = "FAKE-OUTSIDE-1130";

fn scratch(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "browse-links-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A root holding every kind of link, plus the directory outside it that two
/// of them escape to.
fn fixture() -> (PathBuf, PathBuf) {
    let root = scratch("root");
    let outside = scratch("outside");
    std::fs::write(outside.join("secret.txt"), SECRET).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "fn visible() {}\n").unwrap();
    let link = |target: &Path, name: &str| std::os::unix::fs::symlink(target, root.join(name));
    // Served: links that land inside the root.
    link(Path::new("src/lib.rs"), "alias.rs").unwrap();
    link(Path::new("src"), "srcdir").unwrap();
    // Refused: escaping (a directory and a file), dangling, a self-loop.
    link(&outside, "out").unwrap();
    link(&outside.join("secret.txt"), "outfile").unwrap();
    link(Path::new("nowhere"), "dead").unwrap();
    link(Path::new("loop"), "loop").unwrap();
    (root, outside)
}

fn kernel(root: &Path) -> Kernel {
    Kernel::new(Arc::new(ikigai_browse::space(vec![(
        "demo".to_string(),
        root.to_path_buf(),
    )])))
}

fn source(k: &Kernel, iri: &str, args: &[(&str, &str)]) -> Result<String, Error> {
    let mut request = Request::new(Verb::Source, Iri::parse(iri.to_string()).unwrap());
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    block_on(k.issue(request, &Capability::root()))
        .map(|r| String::from_utf8_lossy(&r.bytes).to_string())
}

/// Every `hx-get` target IRI in an html fragment (`/k/source {iri} as=…`),
/// entity-decoded the way a browser decodes an attribute.
fn offered_iris(html: &str) -> Vec<String> {
    html.split("hx-get=\"")
        .skip(1)
        .map(|rest| {
            let value = rest[..rest.find('"').unwrap()]
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&amp;", "&");
            let iri = value.trim_start_matches("/k/source ");
            iri[..iri.find(' ').unwrap_or(iri.len())].to_string()
        })
        .collect()
}

/// Every entry node the turtle face names (`ik:entry <…>, <…>`).
fn turtle_entries(ttl: &str) -> Vec<String> {
    let Some(start) = ttl.find("ik:entry ") else {
        return Vec::new();
    };
    let rest = &ttl[start + "ik:entry ".len()..];
    let list = &rest[..rest.find(" .").unwrap_or(rest.len())];
    list.split(',')
        .map(|s| s.trim().trim_start_matches('<').trim_end_matches('>'))
        .map(str::to_string)
        .collect()
}

fn names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.split('\t').next())
        .map(str::to_string)
        .collect()
}

#[test]
fn the_tree_lists_exactly_the_links_the_resolver_serves() {
    let (root, outside) = fixture();
    let k = kernel(&root);
    let listing = source(&k, "urn:repo:demo:tree", &[]).unwrap();
    let listed = names(&listing);
    for served in ["src", "alias.rs", "srcdir"] {
        assert!(
            listed.iter().any(|n| n == served),
            "the control `{served}`:\n{listing}"
        );
    }
    for refused in ["out", "outfile", "dead", "loop"] {
        assert!(
            !listed.iter().any(|n| n == refused),
            "the tree lists `{refused}`, which the resolver refuses:\n{listing}"
        );
        // And the refusal it would have met is real, so hiding is agreement
        // with the resolver rather than a new rule of its own.
        assert!(
            source(&k, &format!("urn:repo:demo:file:{refused}"), &[]).is_err(),
            "file:{refused} is served, so hiding it is wrong"
        );
    }
    let html = source(&k, "urn:repo:demo:tree", &[("as", "text/html")]).unwrap();
    let ttl = source(&k, "urn:repo:demo:tree", &[("as", "text/turtle")]).unwrap();
    for face in [&html, &ttl] {
        assert!(!face.contains(SECRET));
        for refused in ["out", "outfile", "dead", "loop"] {
            assert!(
                !face.contains(&format!("file:{refused}"))
                    && !face.contains(&format!("tree:{refused}")),
                "a face offers `{refused}`:\n{face}"
            );
        }
    }
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&outside).ok();
}

#[test]
fn every_offered_iri_resolves() {
    // The property, stated once for both machine-readable faces: whatever the
    // listing points at, the kernel serves. A link to an in-root directory was
    // offered as `file:srcdir`, which the file door refuses as a directory.
    let (root, outside) = fixture();
    let k = kernel(&root);
    let html = source(&k, "urn:repo:demo:tree", &[("as", "text/html")]).unwrap();
    // The entry rows only: the listing's other controls (the PR link) name
    // facades this bare space does not mount.
    let mut offered: Vec<String> = offered_iris(&html)
        .into_iter()
        .filter(|i| i.contains(":tree:") || i.contains(":file:"))
        .collect();
    let ttl = source(&k, "urn:repo:demo:tree", &[("as", "text/turtle")]).unwrap();
    let entries = turtle_entries(&ttl);
    assert_eq!(entries.len(), 3, "src, alias.rs and srcdir: {ttl}");
    offered.extend(entries);
    assert!(
        offered.iter().any(|i| i.ends_with(":tree:srcdir")),
        "a link to an in-root directory is offered under the tree door: {offered:?}"
    );
    for iri in offered {
        let served = source(&k, &iri, &[]);
        assert!(
            served.is_ok(),
            "the tree offers {iri}, which is refused: {served:?}"
        );
    }
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&outside).ok();
}
