//! The `.git` directory is never served (ledger #1126).
//!
//! A repository's `.git/config` can carry a credentialed remote URL
//! (`https://user:token@host/…`), and nothing else under `.git` is source the
//! browse family exists to show. Before this test, `list_entries` was a raw
//! `read_dir` and the jail only kept paths inside the root, so an ordinary
//! `urn:cap:browse:read:{repo}` grant read `.git/config` byte for byte, the tree
//! offered `.git` as a directory to click into, and the LLM doors (explain,
//! review, judge) would feed the file to a model and archive what it said.
//!
//! The rule now: a path any of whose components is `.git` (ASCII
//! case-insensitively, because the default macOS volume is case-insensitive and
//! `.GIT/config` opens the same file there) is a NotFound at every door that
//! touches the filesystem, judged on the path as written AND on its canonical
//! form, so a symlink inside the tree that points into `.git` is refused too.
//! The listing hides the same entries, so the enumerator never offers what the
//! resolver refuses.
//!
//! The fixture is hermetic: a scratch directory with a hand-made `.git` holding a
//! FAKE credentialed remote. No `git` process, no real credential.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, EndpointSpace, Error, Exact, Fallback, FnEndpoint, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Verb,
};
use oxigraph::store::Store;

/// The fake secret. Every assertion below is, at bottom, "this string never
/// leaves the process through browse".
const TOKEN: &str = "FAKE-TOKEN-1126";

fn scratch_root() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "browse-gitdir-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(root.join(".git/refs/heads")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(
        root.join(".git/config"),
        format!(
            "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\t\
             url = https://x-access-token:{TOKEN}@example.invalid/owner/repo.git\n"
        ),
    )
    .unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "fn visible() {}\n").unwrap();
    // A submodule or linked worktree has a `.git` FILE naming its git dir.
    std::fs::write(root.join("sub/.git"), format!("gitdir: ../.git/{TOKEN}\n")).unwrap();
    // Symlinks INSIDE the tree that point into `.git`: the lexical half of the
    // jail cannot see these; only the canonical half can.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(".git/config", root.join("cfg")).unwrap();
        std::os::unix::fs::symlink(".git", root.join("gitdir")).unwrap();
        std::os::unix::fs::symlink("../.git", root.join("src/up")).unwrap();
    }
    root
}

/// Every prompt any fake model was asked, so a test can say "the secret never
/// reached a model".
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

/// Browse wired the way a host wires it: the explain mount (which carries the
/// annotation, review, judge and PR families) over a shared store, with the
/// IGNORE SET EMPTIED — a host may configure it, and `.git`'s exclusion must
/// not depend on that knob, which exists for hash churn, not for secrecy.
fn kernel(root: &Path, prompts: &Arc<Prompts>) -> Kernel {
    let store = Arc::new(Store::new().unwrap());
    let config = ikigai_browse::ExplainConfig::new(store).ignore(Vec::<String>::new());
    let browse = ikigai_browse::space_with_explain(vec![("demo".to_string(), root.into())], config);
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(browse),
        Arc::new(fake_llm(prompts)),
    ])))
}

/// The ordinary grant: read this one root, reach the network for the model.
fn grant() -> Capability {
    Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:net:localhost"])
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

fn source(k: &Kernel, iri: &str, args: &[(&str, &str)], cap: &Capability) -> Result<String, Error> {
    issue(k, Verb::Source, iri, args, cap)
}

/// Refused, and refused as ABSENT: a NotFound (or, for `..`, the jail's typed
/// argument error) — never content, and never a message that echoes the token.
fn assert_not_served(result: Result<String, Error>, what: &str) {
    match result {
        Ok(body) => panic!("{what}: served {} bytes: {body}", body.len()),
        Err(e) => {
            let text = e.to_string();
            assert!(
                !text.contains(TOKEN),
                "{what}: the refusal leaks the token: {text}"
            );
            assert!(
                matches!(e, Error::NotFound(_) | Error::InvalidArgument { .. }),
                "{what}: expected NotFound, got {e:?}"
            );
        }
    }
}

/// Every spelling of a path that names `.git` or something under it.
fn git_paths() -> Vec<&'static str> {
    let mut paths = vec![
        ".git/config",
        "./.git/config",
        ".git/./config",
        ".git//config",
        "%2Egit/config",
        "%2egit%2fconfig",
        ".GIT/config",
        ".Git/config",
        ".git",
        ".git/",
        ".git/HEAD",
        "src/../.git/config",
        "sub/.git",
    ];
    if cfg!(unix) {
        paths.extend(["cfg", "gitdir", "gitdir/config", "src/up/config"]);
    }
    paths
}

#[test]
fn the_file_door_never_serves_anything_under_git() {
    let root = scratch_root();
    let prompts = Arc::new(Prompts::default());
    let k = kernel(&root, &prompts);
    // The control: an ordinary file is served, so the refusals below are the
    // rule and not a broken fixture.
    assert!(source(&k, "urn:repo:demo:file:src/lib.rs", &[], &grant())
        .unwrap()
        .contains("visible"));
    for cap in [grant(), Capability::root()] {
        for p in git_paths() {
            for face in ["application/octet-stream", "text/plain", "text/html"] {
                assert_not_served(
                    source(
                        &k,
                        &format!("urn:repo:demo:file:{p}"),
                        &[("as", face)],
                        &cap,
                    ),
                    &format!("file:{p} as={face}"),
                );
            }
        }
    }
}

#[test]
fn the_tree_and_hash_doors_neither_list_nor_open_git() {
    let root = scratch_root();
    let prompts = Arc::new(Prompts::default());
    let k = kernel(&root, &prompts);
    for face in ["text/plain", "text/html", "text/turtle"] {
        let listing = source(&k, "urn:repo:demo:tree", &[("as", face)], &grant()).unwrap();
        assert!(listing.contains("src"), "the control: {listing}");
        assert!(
            !listing.contains(".git"),
            "the root listing (as={face}) offers .git:\n{listing}"
        );
        let sub = source(&k, "urn:repo:demo:tree:sub", &[("as", face)], &grant()).unwrap();
        assert!(
            !sub.contains(".git"),
            "sub/ (as={face}) offers its .git file:\n{sub}"
        );
        if cfg!(unix) {
            // The symlinks into `.git` are refused on click, so they are not
            // offered either.
            for link in ["cfg", "gitdir"] {
                assert!(
                    !listing.contains(link),
                    "the root listing (as={face}) offers the link `{link}`:\n{listing}"
                );
            }
            let src = source(&k, "urn:repo:demo:tree:src", &[("as", face)], &grant()).unwrap();
            assert!(src.contains("lib.rs"), "the control: {src}");
            let offers_up = src.contains("src/up") || src.lines().any(|l| l.starts_with("up\t"));
            assert!(!offers_up, "src/ (as={face}) offers the link `up`:\n{src}");
        }
    }
    for p in git_paths() {
        assert_not_served(
            source(&k, &format!("urn:repo:demo:tree:{p}"), &[], &grant()),
            &format!("tree:{p}"),
        );
        assert_not_served(
            source(&k, &format!("urn:repo:demo:hash:{p}"), &[], &grant()),
            &format!("hash:{p}"),
        );
    }
}

#[test]
fn the_root_hash_does_not_cover_git_even_with_an_empty_ignore_set() {
    // The merkle hash of the root must not move when only `.git` changes: it is
    // the explanation archive's key, and `.git` churns on every commit. With the
    // ignore set emptied, only the `.git` rule keeps it out.
    let root = scratch_root();
    let prompts = Arc::new(Prompts::default());
    let k = kernel(&root, &prompts);
    let before = source(&k, "urn:repo:demo:hash", &[], &grant()).unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
    let after = source(&k, "urn:repo:demo:hash", &[], &grant()).unwrap();
    assert_eq!(before, after, "a change under .git re-keyed the root");
}

#[test]
fn no_model_ever_sees_git_and_the_llm_doors_refuse_it() {
    let root = scratch_root();
    let prompts = Arc::new(Prompts::default());
    let k = kernel(&root, &prompts);
    let cap = Capability::root();
    for p in git_paths() {
        for door in ["explain", "explain-status", "review", "review-status"] {
            assert_not_served(
                source(&k, &format!("urn:repo:demo:{door}:{p}"), &[], &cap),
                &format!("{door}:{p}"),
            );
        }
        assert_not_served(
            source(
                &k,
                &format!("urn:repo:demo:judge:{p}"),
                &[("quote", "url"), ("claim", "it holds a token")],
                &cap,
            ),
            &format!("judge:{p}"),
        );
    }
    // The root rollup recurses into every LISTED child through the kernel, so
    // with the ignore set empty it is the one walk that would reach `.git` on
    // its own. It still explains the root — just never `.git`.
    let rollup = source(&k, "urn:repo:demo:explain", &[], &cap);
    assert!(rollup.is_ok(), "the root rollup fails: {rollup:?}");
    let asked = prompts.0.lock().unwrap();
    assert!(!asked.is_empty(), "the control: the rollup asked a model");
    for prompt in asked.iter() {
        assert!(!prompt.contains(TOKEN), "a model was shown .git: {prompt}");
        assert!(
            !prompt.contains("repositoryformatversion"),
            "a model was shown .git/config"
        );
    }
}

#[test]
fn an_annotation_cannot_anchor_in_git() {
    let root = scratch_root();
    let prompts = Arc::new(Prompts::default());
    let k = kernel(&root, &prompts);
    let result = issue(
        &k,
        Verb::Sink,
        "urn:iki:annotation",
        &[
            ("target", "urn:repo:demo:file:.git/config"),
            ("exact", TOKEN),
            ("body", "this anchors on the token"),
        ],
        &Capability::root(),
    );
    match result {
        Ok(body) => panic!("an annotation anchored in .git/config: {body}"),
        Err(e) => assert!(!e.to_string().contains("x-access-token"), "{e}"),
    }
}
