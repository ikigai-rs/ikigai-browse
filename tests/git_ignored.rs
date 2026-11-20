//! The content hash honors the repository's own ignore rules, and a file over
//! the per-file limit marks only itself (ledger #3839).
//!
//! Browsing a root whose `.gitignore` excludes an mdbook build directory holding
//! a 24 MB wasm showed `over budget: … exceeds the 16777216-byte per-file
//! limit` where the root's explanation state should be: the directory hash read
//! the ignored artifact and refused the whole root over it. These fixtures are
//! real git repositories made in scratch directories; the files over the limit
//! are sparse, so they cost no disk.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use ikigai_browse::MAX_FILE_BYTES;
use ikigai_core::{
    ArgRef, Capability, Description, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Verb,
};
use oxigraph::store::Store;
use sha2::{Digest, Sha256};

fn scratch() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "browse-gitignored-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A sparse file of `size` bytes.
fn sized(root: &Path, rel: &str, size: u64) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::File::create(path).unwrap().set_len(size).unwrap();
}

/// The book root of the report: sources tracked, the build output ignored by
/// the repository's own `.gitignore`.
fn book_root() -> PathBuf {
    let root = scratch();
    git(&root, &["init", "-q"]);
    write(&root, ".gitignore", "books/*/book\n");
    write(&root, "README.md", "# tutorial\n");
    write(&root, "books/ikigai/src/intro.md", "# intro\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "sources"]);
    root
}

#[derive(Default)]
struct Prompts(Mutex<Vec<String>>);

fn fake_llm(prompts: &Arc<Prompts>) -> EndpointSpace {
    let mut space = EndpointSpace::new();
    for provider in ["urn:llm:coder:ask", "urn:llm:ask"] {
        let prompts = Arc::clone(prompts);
        space = space.bind(
            Exact::new(provider),
            FnEndpoint::new("fake-llm", move |inv: &Invocation<'_>| {
                let prompt = inv.inline_str("prompt").unwrap_or("").to_string();
                prompts.0.lock().unwrap().push(prompt);
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    b"an explanation".to_vec(),
                ))
            })
            .with_description(
                Description::new("fake-llm")
                    .verb(Verb::Source)
                    .requires("urn:cap:net:*"),
            ),
        );
    }
    space
}

fn kernel(root: &Path, prompts: &Arc<Prompts>) -> Kernel {
    let store = Arc::new(Store::new().unwrap());
    let config = ikigai_browse::ExplainConfig::new(store);
    let browse = ikigai_browse::space_with_explain(vec![("demo".to_string(), root.into())], config);
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(browse),
        Arc::new(fake_llm(prompts)),
    ])))
}

fn source(k: &Kernel, iri: &str) -> Result<String, Error> {
    let cap = Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:net:localhost"]);
    let request = Request::new(Verb::Source, Iri::parse(iri.to_string()).unwrap())
        .with_arg("as", ArgRef::Inline(b"text/plain".to_vec()));
    block_on(k.issue(request, &cap)).map(|r| String::from_utf8_lossy(&r.bytes).to_string())
}

/// The directory construction, computed independently of the crate: one
/// `name NUL kind NUL child-hash LF` row per entry, in name order.
fn dir_hash(rows: &[(&str, &str, &str)]) -> String {
    let mut hasher = Sha256::new();
    for (name, kind, child) in rows {
        hasher.update(format!("{name}\0{kind}\0{child}\n").as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn file_hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[test]
fn an_ignored_oversized_build_artifact_does_not_fail_the_root() {
    let root = book_root();
    let prompts = Arc::new(Prompts::default());
    let k = kernel(&root, &prompts);
    let before = source(&k, "urn:repo:demo:hash").unwrap();

    sized(
        &root,
        "books/ikigai/book/wasm/book_wasm_bg.wasm",
        MAX_FILE_BYTES + 1,
    );
    write(&root, "books/ikigai/book/index.html", "<html></html>\n");
    // The root, the directory holding the build output, and the explanation
    // state the tree page shows for each, all answer.
    for path in ["", ":books", ":books/ikigai"] {
        let hash = source(&k, &format!("urn:repo:demo:hash{path}"));
        assert!(hash.is_ok(), "hash{path}: {hash:?}");
        let status = source(&k, &format!("urn:repo:demo:explain-status{path}"));
        assert!(status.is_ok(), "explain-status{path}: {status:?}");
    }
    // Ignored build output does not key the tree at all.
    assert_eq!(source(&k, "urn:repo:demo:hash").unwrap(), before);

    // The rollup builds, and never recurses into what the hash leaves out.
    let rollup = source(&k, "urn:repo:demo:explain:books/ikigai");
    assert!(rollup.is_ok(), "the rollup fails: {rollup:?}");
    let asked = prompts.0.lock().unwrap();
    assert!(!asked.is_empty(), "the control: the rollup asked a model");
    for prompt in asked.iter() {
        assert!(!prompt.contains("book (dir)"), "a model was shown book/");
        assert!(!prompt.contains("index.html"), "a model was shown book/");
    }
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn every_ignore_source_git_reads_is_honored() {
    let root = book_root();
    let prompts = Arc::new(Prompts::default());
    let k = kernel(&root, &prompts);
    let before = source(&k, "urn:repo:demo:hash").unwrap();

    // A nested `.gitignore`.
    write(&root, "books/ikigai/.gitignore", "*.log\n");
    git(&root, &["add", "books/ikigai/.gitignore"]);
    git(&root, &["commit", "-q", "-m", "nested ignore"]);
    let with_nested = source(&k, "urn:repo:demo:hash").unwrap();
    assert_ne!(with_nested, before, "the control: a tracked file keys");
    write(&root, "books/ikigai/src/build.log", "noise\n");
    // `.git/info/exclude`.
    write(&root, ".git/info/exclude", "scratch/\n");
    write(&root, "scratch/notes.txt", "mine\n");
    // `core.excludesFile`, the setting a global excludes file is named by.
    let excludes = scratch().join("excludes");
    std::fs::write(&excludes, "*.swp\n").unwrap();
    git(
        &root,
        &["config", "core.excludesFile", excludes.to_str().unwrap()],
    );
    write(&root, "README.md.swp", "swap\n");
    assert_eq!(source(&k, "urn:repo:demo:hash").unwrap(), with_nested);
    assert_eq!(
        source(&k, "urn:repo:demo:hash:books/ikigai/src").unwrap(),
        dir_hash(&[("intro.md", "f", &file_hash(b"# intro\n"))]),
        "a subdirectory hashed by its own path applies the same rules"
    );

    // A tracked file matching a pattern is still content.
    write(&root, "books/ikigai/src/kept.log", "tracked\n");
    git(&root, &["add", "-f", "books/ikigai/src/kept.log"]);
    assert_ne!(source(&k, "urn:repo:demo:hash").unwrap(), with_nested);
    std::fs::remove_dir_all(root).ok();
    std::fs::remove_file(excludes).ok();
}

#[test]
fn a_tracked_file_over_the_limit_fails_only_its_own_entry() {
    let root = book_root();
    let prompts = Arc::new(Prompts::default());
    let k = kernel(&root, &prompts);
    sized(&root, "books/ikigai/src/huge.bin", MAX_FILE_BYTES + 1);

    // Its own hash and explanation refuse, as they always have.
    for door in ["hash", "explain", "explain-status"] {
        let own = source(
            &k,
            &format!("urn:repo:demo:{door}:books/ikigai/src/huge.bin"),
        );
        assert!(
            matches!(own, Err(Error::OverBudget { .. })),
            "{door}: {own:?}"
        );
    }
    // Its directory records the marker row in its place, and everything above
    // it hashes.
    let src = source(&k, "urn:repo:demo:hash:books/ikigai/src").unwrap();
    assert_eq!(
        src,
        dir_hash(&[
            (
                "huge.bin",
                "f",
                &format!("over-budget:{}", MAX_FILE_BYTES + 1)
            ),
            ("intro.md", "f", &file_hash(b"# intro\n")),
        ])
    );
    let top = source(&k, "urn:repo:demo:hash").unwrap();
    assert_eq!(source(&k, "urn:repo:demo:hash").unwrap(), top, "stable");
    assert!(source(&k, "urn:repo:demo:explain-status").is_ok());

    // The rollup explains the rest and says why the one entry is missing.
    let rollup = source(&k, "urn:repo:demo:explain:books/ikigai/src");
    assert!(rollup.is_ok(), "the rollup fails: {rollup:?}");
    assert!(prompts
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|p| p.contains(&format!(
        "### huge.bin (file)\nNot explained: larger than the {MAX_FILE_BYTES}-byte per-file limit."
    ))));

    // The marker moves with the file's size.
    sized(&root, "books/ikigai/src/huge.bin", MAX_FILE_BYTES + 2);
    assert_ne!(source(&k, "urn:repo:demo:hash").unwrap(), top);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn a_root_with_nothing_ignored_hashes_as_before() {
    // Pinned against the construction itself, for a git work tree and for a
    // plain directory: nothing about this change moves either.
    for in_git in [true, false] {
        let root = scratch();
        if in_git {
            git(&root, &["init", "-q"]);
        }
        write(&root, "a.txt", "one\n");
        write(&root, "sub/b.txt", "two\n");
        let prompts = Arc::new(Prompts::default());
        let k = kernel(&root, &prompts);
        let sub = dir_hash(&[("b.txt", "f", &file_hash(b"two\n"))]);
        assert_eq!(
            source(&k, "urn:repo:demo:hash").unwrap(),
            dir_hash(&[("a.txt", "f", &file_hash(b"one\n")), ("sub", "d", &sub)]),
            "in_git={in_git}"
        );
        std::fs::remove_dir_all(root).ok();
    }
}
