//! What a HOST can learn from browse rather than mirror (ledgers #252, #377).
//!
//! * [`ikigai_browse::file_iri`] and friends are public, so a host names the
//!   resource browse serves for a path with browse's own encoder.
//! * [`Mount::graph_name`] is browse's ANSWER for the graph a mount writes, and
//!   it is the graph the mount actually writes.
//! * [`ikigai_browse::REANCHORING_READS`] declares the Sources that rewrite a
//!   stored record during a read (the drift pass), and only those do.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use ikigai_browse::{file_iri, tree_iri, ExplainConfig, Mount, REANCHORING_READS};
use ikigai_core::{
    ArgRef, Capability, Description, EndpointSpace, Exact, Fallback, FnEndpoint, Iri, Kernel,
    ReprType, Representation, Request, Verb,
};
use oxigraph::model::{GraphName, NamedNode};
use oxigraph::store::Store;

const ROOT: &str = "demo";

/// One way of stating (or not stating) a mount's graph.
type MountWay = fn(&Arc<Store>, &Path, &NamedNode) -> Mount;
/// One read: the endpoint id it reaches, the target, its arguments.
type Probe<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);
const BROWSE_GRAPH: &str = "urn:iki:browse:graph:host-seams";

/// The stub model's one answer: two findings on `src/lib.rs`, in the review
/// pass's grammar.
const STUB_ANSWER: &str = "QUOTE: pub fn demo() {}\nSEVERITY: minor\nNOTE: one.\n\
     QUOTE: pub fn other() {}\nSEVERITY: info\nNOTE: two.\n";

fn scratch() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ikigai-browse-host-seams-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("src")).expect("scratch tree");
    std::fs::write(dir.join("README.md"), "# demo\n\nA fixture tree.\n").expect("README.md");
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn demo() {}\npub fn other() {}\n",
    )
    .expect("src/lib.rs");
    let out = Command::new("git")
        .args(["-C", dir.to_str().expect("UTF-8 path"), "init", "-q"])
        .output()
        .expect("git is on PATH");
    assert!(out.status.success(), "git init: {out:?}");
    dir
}

fn stub_llm() -> EndpointSpace {
    let mut space = EndpointSpace::new();
    for provider in ["urn:llm:coder:ask", "urn:llm:ask"] {
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
                    .requires("urn:cap:net:*"),
            ),
        );
    }
    space
}

fn kernel(space: EndpointSpace) -> Kernel {
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(space),
        Arc::new(stub_llm()),
    ])))
}

fn config(store: &Arc<Store>) -> ExplainConfig {
    ExplainConfig::new(Arc::clone(store))
        .file_model_label("fixture")
        .dir_model_label("fixture")
        .review_model_label("fixture")
        .pr_model_label("fixture")
        .no_judge()
}

fn issue(kernel: &Kernel, verb: Verb, target: &str, args: &[(&str, &str)]) -> Representation {
    let mut request = Request::new(
        verb,
        Iri::parse(target.to_string()).unwrap_or_else(|e| panic!("`{target}`: {e}")),
    );
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    futures::executor::block_on(kernel.issue(request, &Capability::root()))
        .unwrap_or_else(|e| panic!("{verb:?} {target} {args:?}: {e}"))
}

fn annotate(kernel: &Kernel, id: &str) {
    issue(
        kernel,
        Verb::Sink,
        &format!("urn:iki:annotation:{id}"),
        &[
            ("target", "urn:repo:demo:file:README.md"),
            ("body", "a note"),
            ("exact", "A fixture tree."),
        ],
    );
}

/// Every quad in the store, as text: what "did this read write?" compares.
fn snapshot(store: &Store) -> BTreeSet<String> {
    store
        .iter()
        .map(|q| q.expect("a readable quad").to_string())
        .collect()
}

/// Move every anchor down a line, so the next read that runs the drift pass
/// has something to re-anchor.
fn drift(root: &Path) {
    for rel in ["README.md", "src/lib.rs"] {
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path).expect("fixture file");
        std::fs::write(&path, format!("// moved\n{text}")).expect("fixture file");
    }
}

// --- ledger #377: the graph a mount reports is the graph it writes -----------

/// ★ The host reads browse's ANSWER and that answer is what the mount does: for
/// each way of stating (or not stating) the graph, every quad an annotation
/// write puts in the store is in exactly the graph `graph_name` reported.
#[test]
fn the_graph_a_mount_reports_is_the_graph_its_writes_land_in() {
    let named = NamedNode::new(BROWSE_GRAPH).unwrap();
    let ways: [(&str, MountWay); 3] = [
        ("unstated", |store, root, _| {
            Mount::new([(ROOT.to_string(), root.to_path_buf())])
                .annotations(Arc::clone(store))
                .config_home(None)
        }),
        ("Mount::graph", |store, root, graph| {
            Mount::new([(ROOT.to_string(), root.to_path_buf())])
                .annotations(Arc::clone(store))
                .graph(graph.clone())
                .config_home(None)
        }),
        ("ExplainConfig::graph", |store, root, graph| {
            Mount::new([(ROOT.to_string(), root.to_path_buf())])
                .explain(config(store).graph(graph.clone()))
                .config_home(None)
        }),
    ];
    for (way, mount) in ways {
        let root = scratch();
        let store = Arc::new(Store::new().unwrap());
        let mount = mount(&store, &root, &named);
        let reported = mount.graph_name();
        let expected = match way {
            "unstated" => GraphName::DefaultGraph,
            _ => GraphName::NamedNode(named.clone()),
        };
        assert_eq!(reported, expected, "{way}");

        let k = kernel(mount.space());
        annotate(&k, "n1");
        let quads: Vec<GraphName> = store
            .iter()
            .map(|q| q.expect("a readable quad").graph_name)
            .collect();
        assert!(!quads.is_empty(), "{way}: the write stored nothing");
        assert!(
            quads.iter().all(|g| *g == reported),
            "{way}: the write landed outside the reported graph {reported}: {quads:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}

/// The getter refuses a disagreement exactly as building the space does, so a
/// host never derives a promise from a mount that cannot be built.
#[test]
#[should_panic(expected = "name different graphs")]
fn a_disagreeing_mount_has_no_graph_name() {
    let store = Arc::new(Store::new().unwrap());
    Mount::new([(ROOT.to_string(), std::env::temp_dir())])
        .graph(NamedNode::new(BROWSE_GRAPH).unwrap())
        .explain(config(&store).graph(NamedNode::new("urn:iki:browse:graph:other").unwrap()))
        .graph_name();
}

// --- ledger #252: the IRI helpers name what browse serves ----------------------

/// A path that needs encoding (a space, non-ASCII, a `%`) names, through the
/// public helpers, exactly the file and directory browse serves.
#[test]
fn the_public_iri_helpers_name_the_resources_browse_serves() {
    let root = scratch();
    std::fs::create_dir_all(root.join("dir x")).unwrap();
    std::fs::write(root.join("dir x/día 1%.md"), "hola\n").unwrap();
    let k = kernel(
        Mount::new([(ROOT.to_string(), root.clone())])
            .config_home(None)
            .space(),
    );

    let file = issue(&k, Verb::Source, &file_iri(ROOT, "dir x/día 1%.md"), &[]);
    assert_eq!(file.bytes, b"hola\n");
    let tree = issue(&k, Verb::Source, &tree_iri(ROOT, "dir x"), &[]);
    assert!(
        String::from_utf8_lossy(&tree.bytes).contains("día 1%.md"),
        "{}",
        String::from_utf8_lossy(&tree.bytes)
    );
    let top = issue(&k, Verb::Source, &tree_iri(ROOT, ""), &[]);
    assert!(String::from_utf8_lossy(&top.bytes).contains("dir x"));
    std::fs::remove_dir_all(&root).ok();
}

// --- ledger #252: the reads that write are declared ----------------------------

/// ★ The declaration is not over-claimed: after the content moved, a read
/// through each declared endpoint (the five this fixture can reach without a
/// derivation, which writes on its own account) rewrites the store. And the
/// reads that are not declared write nothing under the same drift.
#[test]
fn the_declared_reanchoring_reads_write_and_the_others_do_not() {
    let root = scratch();
    let store = Arc::new(Store::new().unwrap());
    let k = kernel(
        Mount::new([(ROOT.to_string(), root.clone())])
            .explain(config(&store))
            .config_home(None)
            .space(),
    );
    annotate(&k, "seed");
    let pass: serde_json::Value = serde_json::from_slice(
        &issue(
            &k,
            Verb::Source,
            "urn:repo:demo:review:src/lib.rs",
            &[("as", "application/json")],
        )
        .bytes,
    )
    .expect("the pass's json face");
    let finding = pass["minted"][0]
        .as_str()
        .expect("the stub pass minted a finding")
        .to_string();

    let writes: [Probe; 5] = [
        ("annotation", "urn:iki:annotation:seed", &[]),
        (
            "browse-annotations",
            "urn:repo:demo:annotations:README.md",
            &[],
        ),
        (
            "browse-file",
            "urn:repo:demo:file:README.md",
            &[("as", "text/html")],
        ),
        ("finding", &finding, &[("as", "application/json")]),
        ("browse-findings", "urn:repo:demo:findings", &[]),
    ];
    for (id, target, args) in writes {
        assert!(REANCHORING_READS.contains(&id), "{id} is not declared");
        drift(&root);
        let before = snapshot(&store);
        issue(&k, Verb::Source, target, args);
        assert_ne!(
            snapshot(&store),
            before,
            "{id}: a read after the content moved re-anchored nothing"
        );
        // And the second read of unchanged content writes nothing.
        let settled = snapshot(&store);
        issue(&k, Verb::Source, target, args);
        assert_eq!(snapshot(&store), settled, "{id}: a repeat read wrote");
    }

    let reads: [Probe; 8] = [
        ("browse-tree", "urn:repo:demo:tree", &[]),
        ("browse-tree", "urn:repo:demo:tree", &[("as", "text/html")]),
        ("browse-file", "urn:repo:demo:file:README.md", &[]),
        ("browse-hash", "urn:repo:demo:hash:README.md", &[]),
        ("browse-state", "urn:repo:demo:state", &[]),
        (
            "browse-review-options",
            "urn:repo:demo:review-options:src/lib.rs",
            &[],
        ),
        (
            "browse-explain-status",
            "urn:repo:demo:explain-status:README.md",
            &[],
        ),
        (
            "browse-review-status",
            "urn:repo:demo:review-status:src/lib.rs",
            &[],
        ),
    ];
    for (id, target, args) in reads {
        drift(&root);
        let before = snapshot(&store);
        issue(&k, Verb::Source, target, args);
        assert_eq!(snapshot(&store), before, "{id} {args:?} wrote on a read");
    }
    std::fs::remove_dir_all(&root).ok();
}

/// Every declared id is an endpoint a full mount binds: a stale entry would be
/// a declaration about nothing.
#[test]
fn every_declared_reanchoring_read_is_bound() {
    let root = scratch();
    let store = Arc::new(Store::new().unwrap());
    let k = kernel(
        Mount::new([(ROOT.to_string(), root.clone())])
            .explain(config(&store))
            .config_home(None)
            .space(),
    );
    let ids: BTreeSet<String> = k
        .entries()
        .expect("an enumerable root")
        .iter()
        .filter_map(|e| k.describe_pattern(&e.pattern).map(|d| d.id))
        .collect();
    for id in REANCHORING_READS {
        assert!(
            ids.contains(id),
            "`{id}` is declared but not bound: {ids:?}"
        );
    }
    std::fs::remove_dir_all(&root).ok();
}
