//! The module recipe as one test: `ikigai-conformance` walks every endpoint
//! [`ikigai_browse::Mount`] binds and reports every violation at once.
//!
//! ## The walk is a remote-code-execution surface, so the opt-out list was
//! written from the CATALOG, not from the first report
//!
//! (conformance PENDING #139/#145.) The suite FIRES every non-opted-out Source
//! under `Capability::root()`, and this crate binds a `git` seam, five `gh`-backed
//! pull-request facades and two LLM derivation endpoints. So the first run here was
//! `kernel.entries()` with no suite at all, followed by
//! `Checks::ARGSPECS | REQUIRES_VERB | NAMES` — the whole static picture (40 of the
//! 40 findings, as it turned out) with **zero invocation** — and only then was
//! anything fired. Nothing below reaches the network or an inference server.
//!
//! ## What is fired, and against what
//!
//! A hermetic scratch tree with one commit, a scratch config home, and an
//! in-memory store. Two deliberate subprocess spawns, both of `git`, both with a
//! fixed argument vector against a directory this test created:
//! [`Scratch::new`]'s `git init` and `urn:repo:demo:state`'s own oracle. That is
//! the same posture `ikigai-repo`'s adoption took, and it is the opposite of
//! #139's hazard: the hazard is a walk spawning a process by SURPRISE, in a tree
//! the test does not own.
//!
//! The two LLM-derived endpoints (`browse-explain`, `browse-review`) are fired
//! against a **deterministic stub** bound at the configured provider IRIs, rather
//! than opted out as `ikigai-dev-server`'s composing walk had to opt them out.
//! That is the whole reason their `text/turtle` faces are probed here at all —
//! the review pass's `prov:`/`dcterms:` graph had never been under an RDF check.
//!
//! ## What is NOT fired, and why
//!
//! The five pull-request facades. They resolve `urn:repo:pr:*` / `urn:repo:log`
//! **through the kernel**, so in this fixture (where those are unbound) they would
//! reach nothing — but on any real host they shell out to `gh`, which is network
//! and GitHub auth, and an opt-out that is true of the module rather than of this
//! fixture is the honest one. `Suite::opt_out` also drops ENFORCED, which is the
//! one invoking check that never reaches `gh` (the capability floor is checked
//! before anything is spawned), so
//! [`the_pull_request_family_is_denied_under_no_grants`] pins that half by hand
//! (conformance PENDING #21/#46).
//!
//! ## Declarations
//!
//! * `namespace("http://www.w3.org/ns/oa#")` — the W3C Web Annotation Data Model,
//!   which the annotation overlay speaks end to end. It is not in the suite's
//!   well-known list, and without it eleven standard `oa:` terms are reported as
//!   invented on every face that serves an annotation (conformance PENDING #141:
//!   31 findings of pure noise). `Suite::namespace` documents itself as "a
//!   vocabulary this module serves"; this one is published by the W3C and merely
//!   spoken here, so the use is a stated deviation rather than a clean fit.
//! * `cacheable("browse-style")` — the one representation in the crate that is
//!   `.cacheable()`. Everything else reads a working tree and is `Expiry::Always`,
//!   which is the honest spelling for a read of a tree nothing here watches; the
//!   day one of them is marked cacheable without a thread a host cuts, this test
//!   goes red. Nothing is declared `pure`: no endpoint here is a function of its
//!   inputs alone.
//!
//! ## Seeded, so the RDF checks are not vacuous
//!
//! A face over an EMPTY store passes SKOLEM-RDF and VOCABULARY without seeing a
//! triple (conformance PENDING #26/#142): before the seed, `annotation` and
//! `browse-annotations` were clean because one could not resolve and the other
//! served an empty graph. Two annotations are seeded — one the listing faces read,
//! one the `annotation` entry's own fixture binds and the pipeline probe rewrites.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use ikigai_browse::{ExplainConfig, Mount, StyleWatch, CAP_ANNOTATE, CAP_WILDCARD};
use ikigai_conformance::{Fixture, Suite};
use ikigai_core::{
    ArgRef, Capability, Description, EndpointSpace, Error, Exact, Expiry, Fallback, FnEndpoint,
    Iri, Kernel, ReprType, Representation, Request, Verb,
};
use oxigraph::store::Store;

/// The one configured root every fixture below names.
const ROOT: &str = "demo";

/// The file the annotation fixtures target, and the quote they anchor on.
const TARGET: &str = "urn:repo:demo:file:README.md";
const QUOTE: &str = "A fixture tree.";

/// The annotation the LISTING faces read: seeded, never rewritten by the walk.
const SEED_ID: &str = "seed";
/// The annotation the `annotation` entry's fixture binds: the Source probe reads
/// it and the pipeline probe rewrites its body to the probe's `content`.
const WALK_ID: &str = "walk";

/// The W3C Web Annotation Data Model, which the suite's well-known list omits.
const OA: &str = "http://www.w3.org/ns/oa#";

/// The provider IRIs `ExplainConfig`'s defaults ask, bound here to a stub.
const PROVIDERS: [&str; 2] = ["urn:llm:coder:ask", "urn:llm:ask"];

/// The pull-request facades, by description id and one bound IRI each.
const PR_FACADES: [(&str, &str); 5] = [
    ("browse-prs", "urn:repo:demo:prs"),
    ("browse-prs-scoped", "urn:repo:demo:prs:src"),
    ("browse-pr", "urn:repo:demo:pr:1"),
    ("browse-pr-explain", "urn:repo:demo:pr:1:explain"),
    ("browse-pr-review", "urn:repo:demo:pr:1:review"),
];

/// Why they are not fired: true of the module, not of this fixture.
const REACHES_GITHUB: &str = "resolves urn:repo:pr:* / urn:repo:log, which shell out to `gh`";

/// Every description id this crate binds. A sixteenth endpoint bound without a
/// line here is held to a weaker standard than the fifteen; a listed id that binds
/// nothing is a stale list.
const ENDPOINTS: [&str; 15] = [
    "annotation",
    "browse-annotations",
    "browse-explain",
    "browse-explain-versions",
    "browse-file",
    "browse-hash",
    "browse-pr",
    "browse-pr-explain",
    "browse-pr-review",
    "browse-prs",
    "browse-prs-scoped",
    "browse-review",
    "browse-state",
    "browse-style",
    "browse-tree",
];

// ---------------------------------------------------------------------------
// The fixture: a hermetic tree, a hermetic config home, an in-memory store.
// ---------------------------------------------------------------------------

fn scratch(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ikigai-browse-conformance-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// A scratch working tree with one commit, and the scratch config home
/// `urn:repo:style` layers its `a11y.toml` within.
///
/// It is a real git repository because `urn:repo:demo:state` is a `git` oracle and
/// a walk that fires it against a non-repository proves only that the error path
/// works. The identity is fixed so nothing here depends on the machine's `.gitconfig`.
struct Scratch {
    tree: PathBuf,
    home: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let tree = scratch("tree");
        std::fs::write(tree.join("README.md"), "# demo\n\nA fixture tree.\n").expect("README.md");
        std::fs::create_dir_all(tree.join("src")).expect("src/");
        std::fs::write(tree.join("src/lib.rs"), "pub fn demo() {}\n").expect("src/lib.rs");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-C", tree.to_str().expect("UTF-8 path")])
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .output()
                .expect("git is on PATH");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "conformance"]);
        Scratch {
            tree,
            home: scratch("home"),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.tree).ok();
        std::fs::remove_dir_all(&self.home).ok();
    }
}

/// A deterministic stand-in for `urn:llm:*:ask`.
///
/// Its one answer is in the review pass's `QUOTE:` / `NOTE:` grammar, so the same
/// stub drives both derivations: an explanation is whatever text came back, and a
/// review of `src/lib.rs` mints exactly one machine annotation from it. It declares
/// the same net capability the real module declares, so `ENFORCED` sees the same
/// floor the real composition would.
const STUB_ANSWER: &str = "QUOTE: pub fn demo() {}\nNOTE: the fixture model's one note.\n";

fn stub_llm() -> EndpointSpace {
    let mut space = EndpointSpace::new();
    for provider in PROVIDERS {
        space = space.bind(
            Exact::new(provider),
            FnEndpoint::new("stub-llm", |_| {
                Ok(Representation::new(
                    ReprType::new("text/plain").with_param("charset", "utf-8"),
                    STUB_ANSWER.as_bytes().to_vec(),
                ))
            })
            .with_description(
                Description::new("stub-llm")
                    .verb(Verb::Source)
                    .requires("urn:cap:net:*")
                    .output("text/plain;charset=utf-8"),
            ),
        );
    }
    space
}

/// The kernel the walk runs over, plus the watch `urn:repo:style`'s threads promise.
fn kernel_over(scratch: &Scratch) -> (Kernel, StyleWatch) {
    let store = Arc::new(Store::new().expect("in-memory store"));
    // Every model label is pinned: unset, the version tag resolves
    // `urn:llm:{provider}:model` through the kernel, and the stub is an `:ask`.
    let config = ExplainConfig::new(store)
        .file_model_label("fixture")
        .dir_model_label("fixture")
        .review_model_label("fixture")
        .pr_model_label("fixture");
    let (space, watch) = Mount::new([(ROOT.to_string(), scratch.tree.clone())])
        .app("conformance")
        .config_home(Some(scratch.home.clone()))
        .explain(config)
        .space_watched();
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(space),
        Arc::new(stub_llm()),
    ])));
    (kernel, watch)
}

/// The kernel with the two annotations seeded — the state every RDF face reads.
fn seeded(scratch: &Scratch) -> (Kernel, StyleWatch) {
    let (kernel, watch) = kernel_over(scratch);
    for id in [SEED_ID, WALK_ID] {
        issue(
            &kernel,
            request(
                Verb::Sink,
                &format!("urn:iki:annotation:{id}"),
                &[
                    ("target", TARGET),
                    ("body", "a seeded note"),
                    ("exact", QUOTE),
                ],
            ),
            &Capability::root(),
        )
        .unwrap_or_else(|e| panic!("seeding `{id}`: {e}"));
    }
    (kernel, watch)
}

// ---------------------------------------------------------------------------
// The suite
// ---------------------------------------------------------------------------

fn suite() -> Suite {
    let mut suite = Suite::new()
        // Bindings are per ENTRY and the verb on a binding-only fixture is
        // ignored (conformance PENDING #2): one binding per template variable.
        .fixture(Fixture::new("browse-tree", Verb::Source).binding("path", "src"))
        .fixture(Fixture::new("browse-file", Verb::Source).binding("path", "README.md"))
        .fixture(Fixture::new("browse-hash", Verb::Source).binding("path", "README.md"))
        .fixture(Fixture::new("browse-explain", Verb::Source).binding("path", "README.md"))
        .fixture(Fixture::new("browse-explain-versions", Verb::Source).binding("path", "README.md"))
        .fixture(Fixture::new("browse-annotations", Verb::Source).binding("path", "README.md"))
        // The review's quote is in `src/lib.rs`, so the pass mints a finding
        // rather than reporting an unanchorable one.
        .fixture(Fixture::new("browse-review", Verb::Source).binding("path", "src/lib.rs"))
        // The annotation entry: the Source probe reads `walk`, and the pipeline
        // probe rewrites it (`content` is the body's piped form). `target` and
        // `exact` are per (id, verb), so the Sink gets a call that anchors —
        // the suite's own sample for an `ik:File` input is `urn:example:conformance`,
        // which is not a browse target.
        .fixture(Fixture::new("annotation", Verb::Source).binding("id", WALK_ID))
        .fixture(
            Fixture::new("annotation", Verb::Sink)
                .arg("target", TARGET)
                .arg("exact", QUOTE),
        )
        .namespace(OA)
        .cacheable("browse-style");
    for (id, _) in PR_FACADES {
        suite = suite.opt_out(id, None, REACHES_GITHUB);
    }
    suite
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn iri(s: &str) -> Iri {
    Iri::parse(s.to_string()).unwrap_or_else(|e| panic!("`{s}` is a valid IRI: {e}"))
}

fn request(verb: Verb, target: &str, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(verb, iri(target));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

fn issue(
    kernel: &Kernel,
    request: Request,
    capability: &Capability,
) -> Result<Representation, Error> {
    futures::executor::block_on(kernel.issue(request, capability))
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// The walk, clean, with the shape it walked pinned.
#[test]
fn conforms() {
    let scratch = Scratch::new();
    let (kernel, _watch) = seeded(&scratch);
    let report = suite().run_blocking(&kernel);
    // Printed even when clean (`--nocapture`): the report is the record.
    eprintln!("{report}");
    assert!(report.is_clean(), "{report}");

    // The stub provider is walked as a module endpoint (conformance PENDING #17):
    // fifteen of this crate's, one of the fixture's.
    assert_eq!(
        report.endpoints,
        ENDPOINTS.len() + 1,
        "the catalog changed; add the id to ENDPOINTS: {report}"
    );
    assert_eq!(
        report.checks.skipped().count(),
        0,
        "every check runs: {report}"
    );
    assert_eq!(
        report.declared.opted_out.len(),
        PR_FACADES.len(),
        "only the pull-request family is opted out: {report}"
    );
}

/// Every id in [`ENDPOINTS`] is bound, and nothing else is.
#[test]
fn the_catalog_is_exactly_the_endpoints_this_crate_documents() {
    let scratch = Scratch::new();
    let (kernel, _watch) = kernel_over(&scratch);
    let ids: BTreeSet<String> = kernel
        .entries()
        .expect("an enumerable root")
        .iter()
        .filter(|e| !e.pattern.starts_with("urn:kernel:"))
        .map(|e| {
            kernel
                .describe_pattern(&e.pattern)
                .unwrap_or_else(|| panic!("`{}` describes itself", e.pattern))
                .id
        })
        .filter(|id| id != "stub-llm")
        .collect();
    let expected: BTreeSet<String> = ENDPOINTS.iter().map(|s| s.to_string()).collect();
    assert_eq!(ids, expected);
}

/// ★ The ENFORCED half [`Suite::opt_out`] blinds (conformance PENDING #21/#46).
///
/// Under a capability holding no grants every pull-request facade refuses with a
/// typed, permanent `Denied` — and it does so at the kernel's own floor, before
/// anything is resolved through it, so this test reaches no process and no socket.
/// That is precisely why the opt-out is safe for the OTHER checks and not for this
/// one: the check that matters most for a network-backed action is the one the
/// opt-out drops.
#[test]
fn the_pull_request_family_is_denied_under_no_grants() {
    let scratch = Scratch::new();
    let (kernel, _watch) = kernel_over(&scratch);
    let none = Capability::scoped(Vec::<String>::new());
    for (id, target) in PR_FACADES {
        let err = issue(&kernel, request(Verb::Source, target, &[]), &none)
            .err()
            .unwrap_or_else(|| panic!("`{id}` resolved under no grants"));
        assert!(matches!(err, Error::Denied(_)), "{id}: {err:?}");
        assert!(!err.is_transient(), "{id}: {err:?}");
    }
}

/// ★ **`urn:repo:style` is the one representation here that carries a thread, and
/// since 0.3.2 something cuts it.**
///
/// `CACHEABLE` checks a thread set is non-empty, not that anything cuts it — "a
/// declared thread is a promise a host must keep", and the suite cannot see the
/// host (conformance PENDING #9). This crate is now both halves: [`Mount::space_watched`]
/// hands back the watch over the same config home the endpoint layers within, from
/// ONE resolution of it. So the promise is checkable here, and this pins it:
/// the declared threads and the watch's threads are the same list, by name.
///
/// Everything else the family serves is `Expiry::Always`. That is a decision, not
/// an omission: a working-tree read could only be cached under a thread naming a
/// file the HOST watches with a root only the host knows (core PENDING §18), so a
/// thread minted here would be a promise no host keeps.
#[test]
fn the_style_sheet_is_the_only_threaded_representation_and_the_watch_names_its_threads() {
    let scratch = Scratch::new();
    let (kernel, watch) = seeded(&scratch);
    let root = Capability::root();
    let probes = [
        "urn:repo:demo:tree",
        "urn:repo:demo:tree:src",
        "urn:repo:demo:file:README.md",
        "urn:repo:demo:state",
        "urn:repo:demo:hash",
        "urn:repo:demo:hash:README.md",
        "urn:repo:demo:annotations",
        "urn:repo:demo:annotations:README.md",
        "urn:repo:demo:explain-versions",
        "urn:iki:annotation:seed",
        "urn:repo:style",
    ];
    let mut threaded: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut cached_without_a_thread: Vec<&str> = Vec::new();
    for target in probes {
        let repr = issue(&kernel, request(Verb::Source, target, &[]), &root)
            .unwrap_or_else(|e| panic!("`{target}` resolves in the scratch mount: {e}"));
        let threads: Vec<String> = repr.threads().iter().map(|t| t.to_string()).collect();
        if !threads.is_empty() {
            threaded.insert(target, threads);
        } else if repr.expiry != Expiry::Always {
            cached_without_a_thread.push(target);
        }
    }
    assert_eq!(
        threaded.keys().copied().collect::<Vec<_>>(),
        vec!["urn:repo:style"],
        "the set of threaded representations changed"
    );
    assert!(
        cached_without_a_thread.is_empty(),
        "cacheable with nothing to cut: {cached_without_a_thread:?}"
    );
    assert_eq!(
        threaded["urn:repo:style"],
        watch.threads(),
        "the sheet's declared threads and the watch's are one list, or the promise \
         the thread makes is kept by nothing"
    );
}

/// What `ikigai-conformance` 0.1.0 does not check (its PENDING #11, landed in the
/// unpublished 0.1.1 as `OUTPUTS`): a declared output is never compared with what
/// the action serves.
///
/// Read by hand for every face the module announces: each `as=` value serves a
/// media type the description declares, and every declared output is served by
/// some call. The pull-request facades are excluded for the same reason they are
/// opted out of the walk.
#[test]
fn declared_outputs_are_the_media_types_served() {
    let scratch = Scratch::new();
    let (kernel, _watch) = seeded(&scratch);
    let root = Capability::root();
    let calls: [(&str, &[(&str, &str)]); 26] = [
        ("urn:repo:demo:tree", &[]),
        ("urn:repo:demo:tree", &[("as", "text/html")]),
        ("urn:repo:demo:tree", &[("as", "text/turtle")]),
        ("urn:repo:demo:file:src/lib.rs", &[("as", "text/html")]),
        ("urn:repo:demo:review:src/lib.rs", &[]),
        (
            "urn:repo:demo:review:src/lib.rs",
            &[("as", "application/json")],
        ),
        ("urn:repo:demo:review:src/lib.rs", &[("as", "text/html")]),
        ("urn:repo:demo:review:src/lib.rs", &[("as", "text/turtle")]),
        ("urn:repo:demo:state", &[]),
        ("urn:repo:demo:state", &[("as", "application/json")]),
        ("urn:repo:demo:hash", &[]),
        ("urn:repo:demo:hash", &[("as", "application/json")]),
        ("urn:repo:style", &[]),
        ("urn:repo:demo:annotations", &[]),
        ("urn:repo:demo:annotations", &[("as", "text/html")]),
        ("urn:repo:demo:annotations", &[("as", "text/turtle")]),
        ("urn:iki:annotation:seed", &[]),
        ("urn:iki:annotation:seed", &[("as", "application/json")]),
        ("urn:iki:annotation:seed", &[("as", "text/turtle")]),
        ("urn:repo:demo:explain-versions", &[]),
        (
            "urn:repo:demo:explain-versions",
            &[("as", "application/json")],
        ),
        ("urn:repo:demo:explain-versions", &[("as", "text/html")]),
        ("urn:repo:demo:explain:README.md", &[]),
        (
            "urn:repo:demo:explain:README.md",
            &[("as", "application/json")],
        ),
        ("urn:repo:demo:explain:README.md", &[("as", "text/html")]),
        ("urn:repo:demo:explain:README.md", &[("as", "text/turtle")]),
    ];
    let mut served: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for (target, args) in calls {
        let repr = issue(&kernel, request(Verb::Source, target, args), &root)
            .unwrap_or_else(|e| panic!("`{target}` {args:?}: {e}"));
        let got = bare(&repr.repr_type.media_type);
        let declared = declared_outputs(&kernel, target);
        assert!(
            declared.contains(&got),
            "`{target}` {args:?} served `{got}`, declared only {declared:?}"
        );
        served.entry(target).or_default().insert(got);
    }
    for (target, got) in served {
        // The raw file face is the pass-through exception: two of its three
        // declared outputs are reached only through the extension map, which
        // `the_raw_file_face_serves_the_extension_mapped_type` covers.
        if target.starts_with("urn:repo:demo:file:") {
            continue;
        }
        for face in declared_outputs(&kernel, target) {
            assert!(
                got.contains(&face),
                "`{target}` declares `{face}` but no call above served it"
            );
        }
    }
}

/// ★ **`browse-file`'s raw face is a PASS-THROUGH output, and core has no spelling
/// for one** — so its `outputs` cannot be the whole truth and
/// [`declared_outputs_are_the_media_types_served`] deliberately does not cover it.
///
/// With no `as=`, the endpoint serves the file's EXTENSION-MAPPED media type: a
/// `.md` is `text/markdown`, a `.png` is `image/png`. That set is closed and
/// enumerable (unlike `urn:httpGet`'s, conformance PENDING #48, which is the other
/// module to hit this), so declaring it is mechanically possible — and it would be
/// WRONG under this suite, because three of those types are RDF faces
/// (`text/turtle`, `application/n-triples`, `application/ld+json`) and declaring
/// them makes `SKOLEM-RDF` / `VOCABULARY` resolve the file resource with
/// `as=text/turtle` and try to parse whatever file the fixture bound as a graph.
/// The face is real — `source urn:repo:{repo}:file:vocabulary.ttl` really does
/// serve Turtle — but it is a property of the PATH, not of the endpoint, and
/// `Description::outputs` is a closed list with no way to say so.
///
/// So the declaration stays the three faces the endpoint chooses for itself, and
/// the contract the pass-through actually keeps is pinned here instead: the served
/// type is the extension's, and `charset=utf-8` rides on the textual ones. When
/// `ikigai-conformance` 0.1.1's `OUTPUTS` check lands, this endpoint is where this
/// crate will have to subtract it and say why — which is the suite README's own
/// answer for a pass-through output.
#[test]
fn the_raw_file_face_serves_the_extension_mapped_type() {
    let scratch = Scratch::new();
    for (name, bytes) in [
        ("a.md", &b"# demo\n"[..]),
        ("a.ttl", &b"<urn:a> <urn:b> <urn:c> .\n"[..]),
        ("a.png", &[0x89, b'P', b'N', b'G'][..]),
        ("a.txt", &b"plain\n"[..]),
        ("a.unknown", &[0xff, 0xfe, 0x00][..]),
    ] {
        std::fs::write(scratch.tree.join(name), bytes).expect("fixture file");
    }
    let (kernel, _watch) = kernel_over(&scratch);
    let root = Capability::root();
    for (name, expected) in [
        ("a.md", "text/markdown"),
        ("a.ttl", "text/turtle"),
        ("a.png", "image/png"),
        ("a.txt", "text/plain"),
        ("a.unknown", "application/octet-stream"),
    ] {
        let target = format!("urn:repo:demo:file:{name}");
        let repr = issue(&kernel, request(Verb::Source, &target, &[]), &root)
            .unwrap_or_else(|e| panic!("`{target}`: {e}"));
        assert_eq!(bare(&repr.repr_type.media_type), expected, "{target}");
    }
    // And the three the endpoint chooses for itself are what it declares.
    assert_eq!(
        declared_outputs(&kernel, "urn:repo:demo:file:README.md"),
        ["application/octet-stream", "text/html", "text/plain"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<String>>()
    );
}

/// ★ **The review graph introduces no undefined term — and the graph the walk
/// checks that over is not empty, and is live.**
///
/// Until `ikigai-vocab` 0.1.69 this face used four `ik:` terms nothing defined —
/// `ik:Review`, `ik:orphanedItems`, `ik:reviewedBytes`, `ik:totalBytes` — and
/// `browse-review` was the one endpoint here opted out of the INVOKING checks
/// solely because of them. They are published now (`ikigai-core` #107), the
/// `opt_out` in [`suite()`] is gone, and ENFORCED / CACHEABLE / SKOLEM-RDF /
/// VOCABULARY reach the review pass for the first time. The hand-written
/// ENFORCED (`Denied` under no grants) and SKOLEM-RDF (no blank nodes) this test
/// used to carry went with the opt-out: the walk does exactly those, over exactly
/// this face.
///
/// What the walk still cannot say, and this does:
///
/// * **`OWED` is EMPTY, pinned.** VOCABULARY says the same thing across every face
///   at once and names the term in a report; this says it about THIS face, and it
///   is the guard for the next term the review pass invents — the failure arrives
///   with the instruction ("define it in `ikigai-vocab` first") attached.
/// * **The graph is NON-EMPTY.** A face over an empty store passes SKOLEM-RDF and
///   VOCABULARY without ever seeing a triple (conformance PENDING #26/#142). What
///   makes those two non-vacuous here is that the stub model's one `QUOTE:`
///   anchors in `src/lib.rs` and the pass mints a finding; nothing in the suite
///   asserts that it did.
/// * **The result is LIVE.** `CACHEABLE` reports an endpoint DECLARED cacheable
///   that is not; it is silent about one that is neither declared nor cacheable.
///   "A derivation over a working tree is `Expiry::Always`" is a decision of this
///   module (the same one [`the_style_sheet_is_the_only_threaded_representation_and_the_watch_names_its_threads`]
///   pins for every other face, and whose probe list this target cannot join —
///   it needs a model), so it is pinned by hand.
#[test]
fn the_review_graph_introduces_no_undefined_term() {
    const OWED: [&str; 0] = [];
    let scratch = Scratch::new();
    let (kernel, _watch) = seeded(&scratch);
    let target = "urn:repo:demo:review:src/lib.rs";

    // A derivation over a working tree is live: nothing here watches the tree.
    let root = Capability::root();
    let turtle = issue(
        &kernel,
        request(Verb::Source, target, &[("as", "text/turtle")]),
        &root,
    )
    .expect("the review pass over the stub model");
    assert_eq!(turtle.expiry, Expiry::Always);
    assert!(turtle.threads().is_empty());

    let triples = ikigai_conformance::rdf::parse("text/turtle", &turtle.bytes)
        .expect("the review face parses");
    assert!(!triples.is_empty(), "the pass minted a graph to check");
    let undefined: BTreeSet<String> = ikigai_conformance::rdf::terms(&triples)
        .into_iter()
        .filter(|t| !ikigai_conformance::rdf::is_defined(t, &[OA.to_string()]))
        .collect();
    assert_eq!(
        undefined,
        OWED.into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>(),
        "the review face invented a term: define it in `ikigai-vocab` (a core arc) \
         before it ships, or the whole walk goes red on VOCABULARY with it"
    );
}

fn bare(media: &str) -> String {
    media
        .split(';')
        .next()
        .unwrap_or(media)
        .trim()
        .to_ascii_lowercase()
}

/// The outputs a description announces for its `Source`.
///
/// ⚠ Two places, not one: a SINGLE-verb description authors flat (`Description::
/// outputs`), and a MULTI-verb one declares per-verb `ActionSpec`s carrying their
/// own. `annotation` is the multi-verb case here, and its flat `outputs` is EMPTY —
/// a hand-written outputs check that reads only `description.outputs` (as
/// `ikigai-repo`'s does, correctly, because every endpoint there is flat) reports
/// "declared only {}" on it. Worth knowing before 0.1.1's `OUTPUTS` check arrives.
fn declared_outputs(kernel: &Kernel, target: &str) -> BTreeSet<String> {
    let description = kernel
        .describe_pattern(target)
        .or_else(|| kernel.describe(&iri(target)))
        .unwrap_or_else(|| panic!("`{target}` describes itself"));
    let mut declared: BTreeSet<String> = description.outputs.iter().map(|o| bare(o)).collect();
    for spec in description.action_specs() {
        if spec.verb == Verb::Source {
            declared.extend(spec.outputs.iter().map(|o| bare(o)));
        }
    }
    declared
}

/// The capability contract the manifold announces, read back from the descriptions.
///
/// `ENFORCED` proves the kernel's wildcard floor under NO grants; what it cannot
/// reach is this module's own per-root rule, which only fires under a capability
/// holding a grant for a DIFFERENT root (conformance PENDING #46). Pinned here.
#[test]
fn a_grant_for_another_root_is_still_denied() {
    let scratch = Scratch::new();
    let (kernel, _watch) = kernel_over(&scratch);
    let elsewhere = Capability::scoped(["urn:cap:browse:read:other", CAP_ANNOTATE]);
    for target in [
        "urn:repo:demo:tree",
        "urn:repo:demo:file:README.md",
        "urn:repo:demo:state",
        "urn:repo:demo:hash",
        "urn:repo:demo:annotations",
    ] {
        let err = issue(&kernel, request(Verb::Source, target, &[]), &elsewhere)
            .err()
            .unwrap_or_else(|| panic!("`{target}` resolved under a grant for another root"));
        assert!(matches!(err, Error::Denied(_)), "{target}: {err:?}");
    }
    // And the declared wildcard is what the manifold offers.
    let description = kernel
        .describe_pattern("urn:repo:demo:tree")
        .expect("browse-tree describes itself");
    assert!(description
        .action_specs()
        .iter()
        .all(|spec| spec.requires.iter().any(|r| r == CAP_WILDCARD)));
}

/// The pipeline half of the annotation Sink: `content` is declared **and read**.
///
/// `PIPELINE`'s invoking half asserts the probe's `content` was not rejected; what
/// it cannot assert is that the bytes LANDED (conformance PENDING #13). Read back
/// out of the store here, which is also the assertion that `… | sink
/// urn:iki:annotation:{id}` is the write path the manifold now advertises.
#[test]
fn a_piped_body_lands_as_the_annotation_body() {
    let scratch = Scratch::new();
    let (kernel, _watch) = seeded(&scratch);
    let root = Capability::root();
    issue(
        &kernel,
        request(
            Verb::Sink,
            "urn:iki:annotation:piped",
            &[
                ("target", TARGET),
                ("exact", QUOTE),
                ("content", "a piped note"),
            ],
        ),
        &root,
    )
    .expect("the piped write path");
    let body = issue(
        &kernel,
        request(Verb::Source, "urn:iki:annotation:piped", &[]),
        &root,
    )
    .expect("reading it back");
    assert_eq!(String::from_utf8(body.bytes).unwrap(), "a piped note");
}

/// The bare minting IRI is bound and deliberately not enumerated, so no walk sees
/// the annotation family's mint-a-fresh-id path. Pinned because a host's `mount`
/// line is written against this fact (it carries no trailing colon).
#[test]
fn the_bare_minting_iri_is_bound_but_not_enumerated() {
    let scratch = Scratch::new();
    let (kernel, _watch) = kernel_over(&scratch);
    let listed = kernel
        .entries()
        .expect("an enumerable root")
        .iter()
        .any(|e| e.pattern == "urn:iki:annotation");
    assert!(!listed, "the bare minting IRI is not an enumerated entry");
    let description = kernel
        .describe(&iri("urn:iki:annotation"))
        .expect("but it is bound");
    assert_eq!(description.id, "annotation");
}

/// A last cheap guard: the scratch tree really is what the fixtures assume.
#[test]
fn the_scratch_tree_is_a_repository_with_the_files_the_fixtures_name() {
    let scratch = Scratch::new();
    assert!(Path::new(&scratch.tree).join(".git").is_dir());
    let (kernel, _watch) = kernel_over(&scratch);
    let state = issue(
        &kernel,
        request(Verb::Source, "urn:repo:demo:state", &[]),
        &Capability::root(),
    )
    .expect("the git oracle over the scratch repository");
    let line = String::from_utf8(state.bytes).unwrap();
    assert!(
        !line.contains("not a git repository"),
        "the state oracle read a real repository: {line}"
    );
}
