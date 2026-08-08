//! `urn:repo:{repo}:hash[:{path}]` — the **content hash**, the backbone of the
//! explanation archive's key (S1).
//!
//! - A **file** hashes its raw bytes: `sha256:{hex}`.
//! - A **directory** hashes its entries' `(name, kind, child-hash)` rows —
//!   a **merkle** construction, so one file edit re-keys exactly the path from
//!   that file to the root and nothing else. Entries named in the **ignore
//!   policy** (`.git`, `target`, …), symlinks, and non-UTF-8 names are
//!   excluded — churn under `target/` must not re-key the tree.
//!
//! sha-256 (the `sha2` crate) over blake3: pure Rust, so CI needs no C
//! toolchain. The hash is a live read — deliberately uncacheable, like
//! `state`; it exists to be cheap enough to recompute on every probe.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use ikigai_core::{ArgSpec, Description, Error, FnEndpoint, Invocation, Result, Verb};
use sha2::{Digest, Sha256};

use crate::{
    file_iri, granted, path_binding, repo_root, repr, repr_utf8, resolve, tree_iri, Roots,
    CAP_WILDCARD,
};

/// Directory names excluded from directory hashing and from the explanation
/// hierarchy (config; these are the defaults from the design of record).
pub(crate) fn default_ignore() -> BTreeSet<String> {
    [
        ".git",
        "target",
        "node_modules",
        "venv",
        ".venv",
        "dist",
        "__pycache__",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// The hash IRI for a repo + root-relative path (`""` = the whole root).
pub(crate) fn hash_iri(repo: &str, rel: &str) -> String {
    if rel.is_empty() {
        format!("urn:repo:{repo}:hash")
    } else {
        format!("urn:repo:{repo}:hash:{}", crate::iri_encode(rel))
    }
}

/// Hash a file's bytes — `sha256:{hex}`.
fn hash_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .map_err(|e| Error::Endpoint(format!("browse: read {}: {e}", path.display())))?;
    Ok(format!("sha256:{:x}", Sha256::digest(&bytes)))
}

/// Hash a directory: one row per kept entry — `name NUL kind NUL child-hash LF`
/// in name order — hashed together. Ignored names, symlinks (never followed,
/// so no cycles), and non-UTF-8 names are excluded. An empty directory is the
/// hash of the empty input: stable, and distinct from any file.
fn hash_dir(dir: &Path, ignore: &BTreeSet<String>) -> Result<String> {
    let mut entries = crate::list_entries(dir)?;
    entries.retain(|e| !ignore.contains(&e.name) && e.kind != crate::Kind::Link);
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut hasher = Sha256::new();
    for entry in entries {
        let child = dir.join(&entry.name);
        let child_hash = match entry.kind {
            crate::Kind::Dir => hash_dir(&child, ignore)?,
            _ => hash_file(&child)?,
        };
        hasher.update(entry.name.as_bytes());
        hasher.update([0]);
        hasher.update(match entry.kind {
            crate::Kind::Dir => b"d",
            _ => b"f",
        });
        hasher.update([0]);
        hasher.update(child_hash.as_bytes());
        hasher.update(*b"\n");
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// Hash whatever `path` resolves to within the jail — file bytes or the merkle
/// directory construction.
pub(crate) fn hash_of(root: &Path, rel: &str, ignore: &BTreeSet<String>) -> Result<String> {
    let target = resolve(root, rel)?;
    if target.is_dir() {
        hash_dir(&target, ignore)
    } else {
        hash_file(&target)
    }
}

pub(crate) fn hash_endpoint(roots: &Roots, ignore: &Arc<BTreeSet<String>>) -> FnEndpoint {
    let held = Arc::clone(roots);
    let ignore = Arc::clone(ignore);
    FnEndpoint::new("browse-hash", move |inv: &Invocation<'_>| {
        let (repo, root) = repo_root(inv, &held)?;
        granted(inv, repo)?;
        let rel = path_binding(inv)?;
        let hash = hash_of(root, &rel, &ignore)?;
        match inv.inline_str("as").unwrap_or("text/plain") {
            t if t.starts_with("application/json") => {
                let target = if resolve(root, &rel)?.is_dir() {
                    tree_iri(repo, &rel)
                } else {
                    file_iri(repo, &rel)
                };
                let json = serde_json::json!({
                    "algorithm": "sha256",
                    "hash": hash,
                    "target": target,
                });
                Ok(repr("application/json", json.to_string()))
            }
            _ => Ok(repr_utf8("text/plain", hash)),
        }
    })
    .with_description(hash_description(roots))
}

fn hash_description(roots: &Roots) -> Description {
    Description::new("browse-hash")
        .title("Content hash (archive key oracle)")
        .summary(
            "The sha-256 content hash of a path within a configured browse root — \
             urn:repo:{repo}:hash[:{path}]. Files hash their bytes; directories hash their \
             entries' (name, kind, child-hash) rows — a merkle construction, so one file \
             edit re-keys exactly the path to the root. The ignore policy (.git, target, \
             node_modules, …), symlinks, and non-UTF-8 names are excluded. text/plain \
             (default) is `sha256:{hex}`; as=application/json adds algorithm and target. \
             Live and uncacheable: this is the cheap probe the explanation archive keys on.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .input(
            ArgSpec::new("repo")
                .binding()
                .summary("a configured root name")
                .one_of(roots.keys().cloned()),
        )
        .input(
            ArgSpec::new("path")
                .binding()
                .optional()
                .summary("path within the root, percent-encoded (omitted = the whole root)"),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .summary("application/json for the structured form")
                .one_of(["application/json"])
                .default_value("text/plain"),
        )
        .output("text/plain;charset=utf-8")
        .output("application/json")
}

// --- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{ArgRef, Capability, Iri, Kernel, Representation, Request};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-hash-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn kernel(root: &Path) -> Kernel {
        Kernel::new(Arc::new(crate::space(vec![(
            "demo".to_string(),
            root.to_path_buf(),
        )])))
    }

    fn cap() -> Capability {
        Capability::scoped(["urn:cap:browse:read:demo"])
    }

    fn source(kernel: &Kernel, iri: &str, args: &[(&str, &str)]) -> String {
        let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
        for (k, v) in args {
            request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
        }
        let repr: Representation = block_on(kernel.issue(request, &cap())).unwrap();
        String::from_utf8_lossy(&repr.bytes).into_owned()
    }

    #[test]
    fn file_hashes_are_stable_and_content_addressed() {
        let root = temp_dir();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        let k = kernel(&root);
        let first = source(&k, "urn:repo:demo:hash:a.txt", &[]);
        // Exactly the sha-256 of the bytes, independently computed.
        assert_eq!(first, format!("sha256:{:x}", Sha256::digest(b"one\n")));
        assert_eq!(source(&k, "urn:repo:demo:hash:a.txt", &[]), first);
        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        assert_ne!(source(&k, "urn:repo:demo:hash:a.txt", &[]), first);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn directory_hashes_cascade_merkle_style() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("sub2")).unwrap();
        std::fs::write(root.join("sub/b.rs"), "// b\n").unwrap();
        std::fs::write(root.join("sub2/c.rs"), "// c\n").unwrap();
        let k = kernel(&root);
        let top = source(&k, "urn:repo:demo:hash", &[]);
        let sub = source(&k, "urn:repo:demo:hash:sub", &[]);
        let sub2 = source(&k, "urn:repo:demo:hash:sub2", &[]);

        // One nested edit re-keys exactly the path to the root: sub and the
        // top change, the sibling is untouched.
        std::fs::write(root.join("sub/b.rs"), "// b, edited\n").unwrap();
        assert_ne!(source(&k, "urn:repo:demo:hash:sub", &[]), sub);
        assert_ne!(source(&k, "urn:repo:demo:hash", &[]), top);
        assert_eq!(source(&k, "urn:repo:demo:hash:sub2", &[]), sub2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ignored_names_and_symlinks_do_not_key_the_tree() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("a.rs"), "// a\n").unwrap();
        std::fs::write(root.join("target/junk"), "junk\n").unwrap();
        let k = kernel(&root);
        let before = source(&k, "urn:repo:demo:hash", &[]);

        // Build churn under an ignored name must not re-key the tree.
        std::fs::write(root.join("target/junk"), "different junk\n").unwrap();
        assert_eq!(source(&k, "urn:repo:demo:hash", &[]), before);

        // A symlink appearing is likewise not part of the key (never followed).
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("a.rs"), root.join("link.rs")).unwrap();
            assert_eq!(source(&k, "urn:repo:demo:hash", &[]), before);
        }

        // A real file appearing IS.
        std::fs::write(root.join("b.rs"), "// b\n").unwrap();
        assert_ne!(source(&k, "urn:repo:demo:hash", &[]), before);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_json_face_names_the_hashed_target() {
        let root = temp_dir();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        let k = kernel(&root);
        let parsed: serde_json::Value = serde_json::from_str(&source(
            &k,
            "urn:repo:demo:hash:src/lib.rs",
            &[("as", "application/json")],
        ))
        .unwrap();
        assert_eq!(parsed["algorithm"], "sha256");
        assert_eq!(parsed["target"], "urn:repo:demo:file:src/lib.rs");
        assert!(parsed["hash"].as_str().unwrap().starts_with("sha256:"));
        let dir: serde_json::Value = serde_json::from_str(&source(
            &k,
            "urn:repo:demo:hash:src",
            &[("as", "application/json")],
        ))
        .unwrap();
        assert_eq!(dir["target"], "urn:repo:demo:tree:src");
        std::fs::remove_dir_all(&root).ok();
    }
}
