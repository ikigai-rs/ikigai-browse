//! The browse family over gonk's ONE dataset: what it serves, where its quads land, what
//! the shared handle costs, and what the watcher buys.
//!
//! Five things are gonk's to get wrong here, and each is a test:
//!
//! - [`the_served_catalog_is_exactly_what_this_manifest_links`] — linking two more crates put
//!   twenty more resources behind the doors; the catalog is pinned id by id, as it was.
//! - [`a_browse_write_touches_no_reserved_graph`] — the PROMISE `main` makes to
//!   `ikigai-store`, checked at the boundary with the store's own tripwire rather than
//!   trusted in a comment.
//! - [`a_browse_read_touches_no_reserved_graph`] — the same tripwire around a READ, which is
//!   the reason the manifest takes `ikigai-browse` 0.4.0: before it, a read could relocate
//!   another writer's quads (ledger #265).
//! - [`a_named_graph_choice_moves_the_promise_with_it`] — the graph this server CHOSE, walked
//!   end to end: browse's quads land in it, none in the default graph, and the promise moved
//!   with the choice rather than beside it (ledger #282).
//! - [`the_browse_graphs_scoped_reads_are_not_cached_and_the_ledgers_still_are`] — obligation
//!   3 of the graph decision, asked of the kernel rather than of the promise.
//! - [`a_bare_pattern_select_sees_nothing_because_every_quad_is_in_a_named_graph`] — the
//!   other half of that: where the quads land is a fact about the data, and what an UNSCOPED
//!   query sees is a fact about the dataset. A `0` from the broad door is the default graph,
//!   not an empty store, and the README's import-verification table rests on it.
//! - [`a_scoped_token_reads_browse_and_both_tokens_run_the_join`] — obligation 2: what
//!   `urn:cap:store:read:graph:<browse graph>` buys, the boundary it does not cross, and the
//!   ledger↔browse join below root that `ikigai-store` 0.2.5 made expressible.
//! - [`the_join_runs_through_the_http_door_under_a_signed_in_grant`] — the same join asked
//!   for the way a caller outside this process asks: over loopback HTTP, under a passkey
//!   identity's grant.
//! - [`a_ledger_item_joins_an_annotation_on_a_repo_file`] — the point of one dataset, with
//!   both halves naming their graph (obligation 4).
//! - [`the_shared_handle_costs_the_ledger_nothing`] — the naive composition beside the one
//!   this server builds, and the default-graph composition beside the named one, with the
//!   read timings printed.
//! - [`a_watched_file_read_recomputes_after_the_file_changes_on_disk`] — the cache that
//!   watcher makes safe, driven through the watcher's own notification path.
//!
//! Every store here is an `in_memory_*` one, which carries the SAME coverage as its durable
//! twin — `in_memory_shared_declaring` for the composition this server builds, plain
//! `in_memory_shared` for the naive one it refused — and takes no RocksDB lock, so these run
//! beside a live gonk holding `~/.ikigai/store`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod common;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Fallback, Iri, Kernel, Representation, Request, Space, SystemClock, Verb,
};
use ikigai_gonk::config::ExplainTiers;
use ikigai_gonk::grants::{browse_graph_grants, grants_for, grants_for_all, Authority};
use ikigai_gonk::identity::{self, Passkeys};
use ikigai_gonk::mount::{self, Mount};
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with, doors, quic, web};
use ikigai_store::DurableStore;
use ikigai_vocab::TurtleRenderer;
use oxigraph::model::{Literal, NamedNode, Term};
use tempfile::TempDir;

/// A scratch repository: two files, no git.
fn scratch_root() -> TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir(dir.path().join("src")).expect("src/");
    std::fs::write(dir.path().join("src/lib.rs"), "the first version\n").expect("lib.rs");
    std::fs::write(dir.path().join("README.md"), "# scratch\n").expect("README.md");
    dir
}

fn roots(dir: &TempDir) -> Vec<(String, PathBuf)> {
    vec![("demo".to_string(), dir.path().to_path_buf())]
}

/// The composition `main` builds when `gonk.browse.root` names a root: a shared store that
/// DECLARES where its sharer writes, and the browse family over the same dataset.
fn served(dir: &TempDir) -> (Arc<Kernel>, RootWatch) {
    let served = served_with(dir, None);
    (served.hub, served.watch)
}

/// ★ The same, plus a `gonk.mount` — pointed at a socket that does not exist.
///
/// That is not a shortcut around a fixture: **binding the explanation families is a function
/// of the CONFIG, never of whether the peer answers**, which is the whole of `prefer` on a
/// server whose other job must not depend on a model. A test that needed a live peer could
/// not run in CI, and would be testing the peer rather than this composition.
fn served_explaining(dir: &TempDir) -> (Arc<Kernel>, RootWatch) {
    let served = served_with(dir, Some(dead_mount()));
    (served.hub, served.watch)
}

/// A mount whose peer can never answer: a Unix socket path that is not there, so the dial
/// fails immediately rather than waiting out a network timeout.
fn dead_mount() -> Mount {
    mount::parse(
        "prefer urn:llm:=/nonexistent/ikigai-gonk-tests/llm.sock",
        std::path::Path::new("/home/nobody"),
    )
    .expect("a mount line")
}

/// ⚠ The third return is the SAME store, cloned before it was composed — a `DurableStore` is
/// a handle, so the clone reads the one dataset the kernel serves. It exists for the two
/// tripwire tests, which need `reserved_graphs_fingerprint` after `compose_with` has taken
/// the store by value; every other test drops it.
///
/// The fourth is the sharer's own handle, cloned before it was wired: planting a decoy in
/// another writer's graph is not something any bound endpoint of this server does, and
/// [`a_browse_read_touches_no_reserved_graph`] has to be able to do it.
fn served_with(dir: &TempDir, mount: Option<Mount>) -> Served {
    served_in(dir, mount, browse::Graph::chosen())
}

/// The whole composition, parameterized by the ONE thing that decides where browse's quads
/// live and what the store is promised about them.
///
/// ★ `graph` is used **twice and constructed once**, exactly as `main` does it: once as
/// `sharer_writes()` on the store open, once as browse's mount. Passing it in rather than
/// calling `Graph::chosen()` twice here is the point of the test fixture as much as of the
/// server — a fixture that made the two statements separately could not fail the way the
/// server would.
fn served_in(dir: &TempDir, mount: Option<Mount>, graph: browse::Graph) -> Served {
    // ★ `_declaring`, matching `main`: the promise that the sharer (`ikigai-browse`) writes
    // where `graph` says and nowhere else is what keeps every scoped read — which is every
    // read the ledger makes — cacheable under the store's own write threads. Plain
    // `in_memory_shared` here would test a composition this server does not build, and would
    // do it by being slower and passing.
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store that declares where its sharer writes");
    let sharer = Arc::clone(&handle);
    let (watch, refused) = RootWatch::start(&roots(dir));
    assert!(refused.is_empty(), "{refused:?}");
    let tiers = ExplainTiers::default();
    let wired = browse::wire(
        roots(dir),
        handle,
        watch.watched(),
        mount.is_some().then_some(&tiers),
        &graph,
    );
    let mounted = mount.iter().map(mount::space).collect();
    let hub = Arc::new(compose_with(
        store.clone(),
        Some(Arc::new(wired.space)),
        mounted,
        Vec::new(),
        None,
    ));
    Served {
        hub,
        watch,
        store,
        sharer,
    }
}

/// What [`served_in`] hands back.
struct Served {
    hub: Arc<Kernel>,
    watch: RootWatch,
    /// The kernel's own store, as a second handle — for `reserved_graphs_fingerprint`.
    store: DurableStore,
    /// The SHARER's handle, for planting quads no endpoint of this server would write.
    sharer: Arc<ikigai_store::Store>,
}

/// ★ The composition this arc REFUSED: `open_shared` with NO declaration — the handle leaves
/// and the store is told nothing about where its holder writes, so it must assume the worst
/// and make every read `Expiry::Always`. It exists so the cost of the easy path is a number
/// in the test output rather than an assertion about a counterfactual.
fn naive(dir: &TempDir) -> Arc<Kernel> {
    let (store, handle) = DurableStore::in_memory_shared().expect("a shared in-memory store");
    // No watcher either: a host that accepts the blanket has no reason to run one, and
    // `ikigai-browse`'s reads are live and uncacheable exactly as it declares them.
    let wired = browse::wire(roots(dir), handle, &[], None, &browse::Graph::chosen());
    let space = Fallback::new(vec![
        Arc::new(ikigai_store::space(store)) as Arc<dyn Space>,
        Arc::new(ikigai_ledger::space()) as Arc<dyn Space>,
        Arc::new(wired.space) as Arc<dyn Space>,
    ]);
    Arc::new(
        Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer))
            .with_clock(Arc::new(SystemClock)),
    )
}

/// Today's gonk without a browse face: an OWNED store, whose reads `ikigai-store` declares
/// cacheable itself.
fn owned() -> Arc<Kernel> {
    Arc::new(ikigai_gonk::compose(
        DurableStore::in_memory().expect("an owned in-memory store"),
    ))
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("a test IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn issue(kernel: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> Representation {
    block_on(Kernel::issue(
        kernel,
        request(verb, iri, args),
        &Capability::root(),
    ))
    .unwrap_or_else(|e| panic!("{verb:?} {iri} {args:?}: {e}"))
}

fn text(kernel: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> String {
    String::from_utf8_lossy(&issue(kernel, verb, iri, args).bytes).into_owned()
}

/// The IRIs the kernel currently holds cached, from `urn:kernel:cache`.
fn cached(kernel: &Kernel) -> Vec<String> {
    text(kernel, Verb::Source, "urn:kernel:cache", &[])
        .lines()
        .skip(3) // `cache`, `entries`, `size`
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect()
}

/// File one annotation on a repo file, through the kernel.
fn annotate(kernel: &Kernel, id: &str, target: &str, quote: &str, note: &str) {
    text(
        kernel,
        Verb::Sink,
        &format!("urn:iki:annotation:{id}"),
        &[("target", target), ("exact", quote), ("body", note)],
    );
}

// ---------------------------------------------------------------- the catalog

/// Every resource the browse composition puts behind the doors WITHOUT a mount. `explain`,
/// `review` and the PR-derived layers are deliberately absent: they derive through
/// `urn:llm:*`, which nothing binds until a `gonk.mount` line names the peer that serves it,
/// and an action the kernel can never satisfy is an over-offer.
const BROWSE_IDS: [&str; 10] = [
    "annotation",
    "browse-annotations",
    "browse-file",
    "browse-hash",
    "browse-pr",
    "browse-prs",
    "browse-prs-scoped",
    "browse-state",
    "browse-style",
    "browse-tree",
];

/// The five the mount adds, over seven patterns per root (`explain`, `explain:{path}`,
/// `explain-versions`, `explain-versions:{path}`, `review:{path}`, `pr:{n}:explain`,
/// `pr:{n}:review`). Each derives through `urn:llm:{provider}:ask`; each therefore declares
/// `urn:cap:net:*` on top of the browse root grant, and `browse-review` and
/// `browse-pr-review` declare `urn:cap:annotate` as well, because their findings are minted
/// as annotations in this dataset.
const EXPLAIN_IDS: [&str; 5] = [
    "browse-explain",
    "browse-explain-versions",
    "browse-pr-explain",
    "browse-pr-review",
    "browse-review",
];

/// `ikigai-repo`'s facades — bound only alongside a browse face, because that is what needs
/// them (browse's PR rows resolve `urn:repo:pr:*` through the kernel).
const REPO_IDS: [&str; 10] = [
    "repo-branch",
    "repo-list",
    "repo-log",
    "repo-pr-checks",
    "repo-pr-diff",
    "repo-pr-files",
    "repo-pr-list",
    "repo-pr-view",
    "repo-status",
    "system-exec",
];

/// The new families' description ids in a kernel's catalog, sorted and deduplicated (browse
/// enumerates per-root rows, so one endpoint appears under several patterns).
fn new_family_ids(kernel: &Kernel, expected: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = kernel
        .entries()
        .expect("an enumerable root")
        .iter()
        .filter(|entry| !entry.pattern.starts_with("urn:kernel:"))
        .filter_map(|entry| kernel.describe_pattern(&entry.pattern).map(|d| d.id))
        .filter(|id| expected.contains(id))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[test]
fn the_served_catalog_is_exactly_what_this_manifest_links() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let mut expected: Vec<String> = BROWSE_IDS
        .iter()
        .chain(REPO_IDS.iter())
        .map(|id| id.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        new_family_ids(&hub, &expected),
        expected,
        "the browse composition's catalog changed: a dependency bump gained or lost a \
         resource, and it is now behind every door this binary opens"
    );
    // The socket and QUIC doors forward to the hub, so they serve the same twenty — a
    // forwarding space that dropped a row would be clean above and dirty here.
    let door = ikigai_gonk::doors::door_kernel(Arc::clone(&hub));
    assert_eq!(new_family_ids(&door, &expected), expected);
}

/// ★ **A mount line is what binds the explanation families, and nothing else is.** The same
/// composition with a `gonk.mount` gains exactly five description ids and loses none — so a
/// gonk with no peer configured cannot offer an action no kernel in this process could
/// satisfy, and a gonk with one offers every row that peer makes satisfiable.
///
/// The peer here is unreachable on purpose (see [`served_explaining`]): binding is a
/// property of the config, reachability of the moment.
#[test]
fn a_mount_binds_the_explanation_families_and_nothing_else_does() {
    let dir = scratch_root();
    let mut everything: Vec<String> = BROWSE_IDS
        .iter()
        .chain(REPO_IDS.iter())
        .chain(EXPLAIN_IDS.iter())
        .map(|id| id.to_string())
        .collect();
    everything.sort();

    let (unmounted, _watch) = served(&dir);
    let found = new_family_ids(&unmounted, &everything);
    for id in EXPLAIN_IDS {
        assert!(
            !found.contains(&id.to_string()),
            "`{id}` is an over-offer without a mount: {found:?}"
        );
    }

    let (mounted, _watch) = served_explaining(&dir);
    assert_eq!(
        new_family_ids(&mounted, &everything),
        everything,
        "a mounted gonk serves the browse composition plus the five derived families"
    );
    let door = ikigai_gonk::doors::door_kernel(Arc::clone(&mounted));
    assert_eq!(new_family_ids(&door, &everything), everything);
}

// ------------------------------------------------------------- the spend gate

/// ★★ **The spend gate, per capability — the centre of this arc.**
///
/// Deriving an explanation calls a model, and this server's pages are meant to be browsed.
/// So the question is not whether explain works but who can make it spend, and the answer is
/// a capability rather than a door check:
///
/// - **Anonymous HTTP** holds per-ledger tokens only. It reaches no browse row at all, so it
///   can neither derive an explanation nor read an archived one, and the manifold does not
///   offer it either.
/// - **A browse grant with no net grant** — the shape of a `grants.json` entry for a client
///   that may read a repository — reads every browse row and `explain-versions` (which
///   derives nothing and declares no network), and is DENIED on `explain` itself.
/// - **Root** (the socket door) may derive.
///
/// The second case is the interesting one, and the last assertion is the uncomfortable
/// truth in it: `version=` addresses an archived entry and provably derives nothing, but it
/// is the same action and carries the same requirement, so "may read what was already paid
/// for" is not grantable today. That is browse's grain, not gonk's, and it is what the HTTP
/// browse face will have to answer (#258).
#[test]
fn deriving_needs_a_net_grant_and_listing_the_archive_does_not() {
    let dir = scratch_root();
    let (hub, _watch) = served_explaining(&dir);
    let anonymous = Capability::scoped(
        grants_for_all(&["default".to_string()], Authority::Write).expect("the ledger tokens"),
    );
    let reader = Capability::scoped(["urn:cap:browse:read:demo"]);
    let deriver = Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:net:localhost"]);

    let explain = "urn:repo:demo:explain:src/lib.rs";
    let versions = "urn:repo:demo:explain-versions:src/lib.rs";

    for (who, capability) in [("anonymous", &anonymous), ("a browse reader", &reader)] {
        let answer = block_on(Kernel::issue(
            &hub,
            request(Verb::Source, explain, &[]),
            capability,
        ));
        assert!(
            matches!(answer, Err(ikigai_core::Error::Denied(_))),
            "{who} must not be able to make this server spend a token: {answer:?}"
        );
        // …and the archived entry is behind the same wall, for the same reason.
        let archived = block_on(Kernel::issue(
            &hub,
            request(
                Verb::Source,
                explain,
                &[("version", "code-v1@qwen3-coder:30b")],
            ),
            capability,
        ));
        assert!(
            matches!(archived, Err(ikigai_core::Error::Denied(_))),
            "{who} on an archived entry: {archived:?}"
        );
    }

    // Anonymous cannot even ask what the archive holds — it holds no browse grant.
    assert!(matches!(
        block_on(Kernel::issue(
            &hub,
            request(Verb::Source, versions, &[]),
            &anonymous
        )),
        Err(ikigai_core::Error::Denied(_))
    ));
    // A browse reader can: listing the archive derives nothing and declares no network.
    let listed = block_on(Kernel::issue(
        &hub,
        request(Verb::Source, versions, &[]),
        &reader,
    ))
    .expect("explain-versions requires no net grant");
    assert!(listed.bytes.is_empty(), "nothing is archived yet");

    // The manifold tells the same story it enforces: no explain row for a caller who could
    // not invoke one, and the free row for the one who can.
    let offered = |capability: &Capability| -> String {
        String::from_utf8_lossy(
            &block_on(Kernel::issue(
                &hub,
                request(Verb::Source, "urn:kernel:actions", &[]),
                capability,
            ))
            .expect("the manifold answers any capability about itself")
            .bytes,
        )
        .into_owned()
    };
    let to_anonymous = offered(&anonymous);
    assert!(!to_anonymous.contains("explain"), "{to_anonymous}");
    let to_reader = offered(&reader);
    assert!(
        to_reader.contains("urn:repo:demo:explain-versions"),
        "the free row is offered: {to_reader}"
    );
    assert!(
        !to_reader.contains("urn:repo:demo:explain:"),
        "the spending row is not: {to_reader}"
    );
    assert!(
        offered(&deriver).contains("urn:repo:demo:explain:"),
        "a net grant is what puts the spending row on offer"
    );
}

/// ★ **Explain is an addition, never a dependency.** With the peer unreachable, a caller who
/// MAY derive gets a TRANSIENT failure — not a denial, not an unresolved name — and every
/// other resource this server exists for answers exactly as before.
#[test]
fn a_peer_that_is_down_costs_explain_and_nothing_else() {
    let dir = scratch_root();
    let (hub, _watch) = served_explaining(&dir);
    let filed = text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "the ledger does not depend on a model")],
    );
    assert!(filed.contains("urn:iki:ledger:default:item:"), "{filed}");
    assert!(text(&hub, Verb::Source, "urn:iki:ledger:items", &[]).contains("model"));
    assert_eq!(
        text(&hub, Verb::Source, "urn:repo:demo:file:src/lib.rs", &[]),
        "the first version\n",
        "a browse read is untouched by an absent peer"
    );

    let derived = block_on(Kernel::issue(
        &hub,
        request(Verb::Source, "urn:repo:demo:explain:src/lib.rs", &[]),
        &Capability::root(),
    ));
    let error = derived.expect_err("there is no peer to derive against");
    assert!(
        error.is_transient(),
        "an absent peer is transient — a Retry or a Failover above this mount can act on it, \
         and a caller can tell it from `the model refused`: {error:?}"
    );
    // The mount answered; the name is not unbound. (`Unresolved` would say the opposite, and
    // would be what a caller saw if the mount space were composed behind the local ones.)
    assert!(
        !matches!(error, ikigai_core::Error::Unresolved(_)),
        "{error:?}"
    );
    // And the model face itself, asked directly, says the same thing.
    let asked = block_on(Kernel::issue(
        &hub,
        request(Verb::Source, "urn:llm:ask", &[("in", "hello")]),
        &Capability::root(),
    ));
    assert!(
        asked
            .as_ref()
            .err()
            .is_some_and(ikigai_core::Error::is_transient),
        "{asked:?}"
    );
}

// ------------------------------------------------- where browse's quads land

/// ★ The PROMISE `main` makes when it hands the handle out — the [`browse::Graph`] choice
/// read as `SharerWrites` — pinned at the boundary with `ikigai-store`'s own tripwire.
/// Every quad `ikigai-browse` writes goes into the graph this server chose (and, before
/// 2026-09-16, into the default one), so no graph this store RESERVES from the sharer can
/// move when browse writes; and a graph that cannot move is a graph whose scoped reads stay
/// cacheable, which is every read the ledger makes.
///
/// ★ Since the graph decision the promise names one graph, so the browse graph is not among
/// the reserved ones and the ledger's still is — the assertion on `before.len()` below is
/// that sentence, and it would have been 2 with the promise left at the bare default.
///
/// ⚠ **The fingerprint is over QUADS, not over the set of graph names**, and that is the
/// difference that matters: this server wrote the name-set version by hand until
/// `ikigai-store` 0.2.4, and it would have missed a sharer writing into a named graph that
/// already existed — which is exactly the case a running gonk is in, where the ledger's graph
/// is always there. It also *errors* on a store that declared nothing, rather than passing
/// vacuously.
///
/// ⚠ Both fingerprints are taken around the SHARER's write and nothing else. A write through
/// `ikigai-store`'s own endpoints changes a reserved graph legitimately — it cut a thread when
/// it did — and would read here as a broken promise.
///
/// The day browse writes a graph the promise does not name, this test is red HERE, where the
/// promise is made, instead of silently stale in the cache of a running server.
#[test]
fn a_browse_write_touches_no_reserved_graph() {
    let dir = scratch_root();
    let Served {
        hub, store, watch, ..
    } = served_with(&dir, None);
    let _watch = watch;
    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "an item, so a reserved graph exists at all")],
    );
    let before = store
        .reserved_graphs_fingerprint()
        .expect("this store declares where its sharer writes");
    assert_eq!(
        before.len(),
        1,
        "the ledger's graph is reserved from the sharer, and it is the only named one — a \
         fingerprint over nothing would pass whatever browse did"
    );

    annotate(
        &hub,
        "n1",
        "urn:repo:demo:file:src/lib.rs",
        "first",
        "a note",
    );
    // The annotation IS in the dataset — the broad read sees it, so the tripwire below is
    // measuring a write that really happened…
    //
    // ⚠ `GRAPH ?g` rather than a bare pattern, and that is obligation 4 in one line: browse
    // writes a NAMED graph now, so a bare `{ ?a ik:annotates ?t }` reads the default graph and
    // finds nothing. It would not have failed — it would have passed the tripwire below
    // vacuously, measuring a write it could not see.
    let answer = text(
        &hub,
        Verb::Source,
        "urn:iki:store:select",
        &[(
            "query",
            "SELECT ?a WHERE { GRAPH ?g { ?a <https://ikigai-rs.dev/ns#annotates> ?t } }",
        )],
    );
    assert!(answer.contains("urn:iki:annotation:n1"), "{answer}");
    // …and it moved no reserved graph, which is why a scoped read of one stays cacheable.
    let changed = store
        .reserved_graphs_fingerprint()
        .expect("still declaring")
        .changed_since(&before);
    assert!(
        changed.is_empty(),
        "a browse write moved {changed:?}, which `main` promised the sharer would not touch — \
         every scoped read of those graphs is now cached against threads that write never cut"
    );
}

/// The graph an annotation-shaped decoy is planted in: a name no part of this server writes,
/// standing in for another writer's tenancy.
const ANOTHER_WRITER: &str = "urn:iki:graph:another-writer";

/// Plant a quad browse would recognize as one of its own — in someone else's graph, through
/// the sharer's handle, because no bound endpoint of this server would ever write there.
fn plant_decoy(sharer: &ikigai_store::Store, graph: &str, id: &str) {
    let ik = |t: &str| NamedNode::new(format!("https://ikigai-rs.dev/ns#{t}")).expect("an IRI");
    let oa = |t: &str| NamedNode::new(format!("http://www.w3.org/ns/oa#{t}")).expect("an IRI");
    let graph = NamedNode::new(graph).expect("a graph IRI");
    let decoy = NamedNode::new(id).expect("an annotation IRI");
    for (p, o) in [
        (
            NamedNode::new("http://www.w3.org/1999/02/22-rdf-syntax-ns#type").expect("rdf:type"),
            Term::NamedNode(oa("Annotation")),
        ),
        (
            oa("bodyValue"),
            Term::Literal(Literal::new_simple_literal("not this server's note")),
        ),
        (
            ik("repo"),
            Term::Literal(Literal::new_simple_literal("demo")),
        ),
        (
            ik("path"),
            Term::Literal(Literal::new_simple_literal("src/lib.rs")),
        ),
        (
            ik("annotates"),
            Term::NamedNode(NamedNode::new("urn:repo:demo:file:src/lib.rs").expect("a file IRI")),
        ),
        // ⚠ A quote that is NOT in the file, so browse's drift re-anchoring has something to
        // do. That is the half that makes a read able to WRITE: `annotate::refresh` rewrites
        // an annotation whose anchor has moved, during a Source.
        (
            oa("exact"),
            Term::Literal(Literal::new_simple_literal("a line this file never had")),
        ),
    ] {
        sharer
            .insert(oxigraph::model::Quad::new(decoy.clone(), p, o, graph.clone()).as_ref())
            .expect("the decoy plants");
    }
}

/// ★ **The other end of the same promise: a browse READ moves no reserved graph either.**
///
/// This is why the manifest takes `ikigai-browse` 0.4.0 rather than a version number's worth
/// of tidiness. Through 0.3.2 browse's annotation reads passed no graph at all — which in
/// `quads_for_pattern` means EVERY graph — and `annotate::refresh` rewrites a drifted
/// annotation *during a Source*. Together those made a plain read destructive across a graph
/// boundary: it re-anchored an annotation-shaped quad sitting in another writer's named graph
/// and PERSISTED the move (measured against 0.3.2 in ledger #265).
///
/// gonk is exactly the host with something to lose there — one dataset, browse beside a
/// graph-scoped ledger — and the loss would be silent twice over: the other writer's quads
/// relocate, AND this server's coverage promise becomes false, so every scoped read of the
/// robbed graph goes on being served from cache against threads that write never cut.
///
/// ⚠ The mechanism is deliberately the SAME one [`a_browse_write_touches_no_reserved_graph`]
/// uses — `reserved_graphs_fingerprint`, over quads — rather than a new assertion shape. The
/// promise is about what the sharer writes; a read that writes is still a write, and it is
/// the fingerprint's business whichever verb caused it. What is new here is only the verb
/// between the two fingerprints.
///
/// `ikigai-browse`'s own `a_default_mount_does_not_read_another_graph` covers the VISIBILITY
/// half (asserted below too, cheaply). Nothing stated the write-back half until this test —
/// ledger #265 asked for it in so many words.
#[test]
fn a_browse_read_touches_no_reserved_graph() {
    let dir = scratch_root();
    let Served {
        hub,
        store,
        sharer,
        watch,
    } = served_with(&dir, None);
    let _watch = watch;
    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "an item, so a reserved graph exists at all")],
    );
    // One of this server's own annotations, so the read has real work to do and is not
    // answering out of an empty archive.
    annotate(
        &hub,
        "ours",
        "urn:repo:demo:file:src/lib.rs",
        "first",
        "this server's own note",
    );
    plant_decoy(&sharer, ANOTHER_WRITER, "urn:iki:annotation:theirs");

    let before = store
        .reserved_graphs_fingerprint()
        .expect("this store declares where its sharer writes");
    assert_eq!(
        before.len(),
        2,
        "the ledger's graph and the decoy's, both reserved from the sharer — a fingerprint \
         over nothing would pass whatever the read did"
    );

    // Every browse read that touches the annotation overlay, plain and through the faces that
    // fold it in. The blunt argument test in `browse::cached_reads` means the faces are not
    // cached, so each of these really reaches the store.
    for (iri, args) in [
        ("urn:repo:demo:annotations", &[][..]),
        ("urn:repo:demo:annotations:src/lib.rs", &[]),
        (
            "urn:repo:demo:file:src/lib.rs",
            &[("annotations", "include")][..],
        ),
        ("urn:repo:demo:file:src/lib.rs", &[("as", "text/html")][..]),
    ] {
        let listed = text(&hub, Verb::Source, iri, args);
        assert!(
            !listed.contains("urn:iki:annotation:theirs"),
            "a read of {iri} reached into <{ANOTHER_WRITER}>: {listed}"
        );
    }

    // ★ And the half nothing stated before: it did not MOVE them either.
    let changed = store
        .reserved_graphs_fingerprint()
        .expect("still declaring")
        .changed_since(&before);
    assert!(
        changed.is_empty(),
        "a browse READ moved {changed:?}. On `ikigai-browse` 0.3.2 that is <{ANOTHER_WRITER}>, \
         whose decoy the read re-anchored into the default graph and persisted — another \
         writer's quads relocated by a Source, and this server's `SharerWrites` promise false \
         from that moment on"
    );
    // The decoy is still whole, said plainly: the fingerprint above is a digest, and a reader
    // should not have to trust that a digest of five quads means five quads.
    let still = text(
        &hub,
        Verb::Source,
        "urn:iki:store:select",
        &[(
            "query",
            &format!(
                "SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{ANOTHER_WRITER}> \
                 {{ <urn:iki:annotation:theirs> ?p ?o }} }}"
            ),
        )],
    );
    assert!(still.contains("\"value\":\"6\""), "{still}");
}

/// ★ **The derivation on its other arm: choose a named graph and the promise goes with it.**
///
/// [`browse::Graph`] is one value read two ways — `sharer_writes()` for `ikigai-store` and
/// `Mount::graph` for `ikigai-browse` — and the whole claim of ledger #282's fix is that
/// those cannot disagree. `src/browse.rs::graph_choice_and_promise_cannot_disagree` checks
/// that the two READINGS match; this checks that they match *the store*, with a real write
/// through a real kernel.
///
/// ★ Since 2026-09-16 this is the arm the server SHIPS, and the test takes the shipped
/// choice rather than an IRI of its own: it is now the end-to-end statement that a real write
/// through a real kernel lands in the graph an operator was told to migrate into, and in no
/// other. (It was written a version earlier, against a made-up IRI, so that the day the
/// decision was taken the promise would already be known to move with it rather than be
/// discovered to. It was.)
///
/// It also prices obligation 3 on `Graph`'s docs, from the store's own mouth rather than by
/// argument: a promised graph is NOT covered, so its scoped reads stop being cacheable. That
/// is the ~1000× this server measured, and it is the cost of putting browse inside the
/// tenancy boundary.
#[test]
fn a_named_graph_choice_moves_the_promise_with_it() {
    const LEDGER_GRAPH: &str = "urn:iki:ledger:graph:default";

    let dir = scratch_root();
    let graph = browse::Graph::chosen();
    let browse_graph: String = graph
        .named()
        .expect("this server chooses a named browse graph")
        .as_str()
        .to_string();
    let Served {
        hub,
        store,
        sharer,
        watch,
    } = served_in(&dir, None, graph);
    let _watch = watch;
    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "an item, so the ledger's graph exists")],
    );
    plant_decoy(&sharer, ANOTHER_WRITER, "urn:iki:annotation:theirs");
    let before = store
        .reserved_graphs_fingerprint()
        .expect("this store declares where its sharer writes");
    assert_eq!(
        before.len(),
        2,
        "the ledger's graph and the decoy's are reserved — the BROWSE graph is not, because \
         the promise derived from this choice names it"
    );

    annotate(
        &hub,
        "n1",
        "urn:repo:demo:file:src/lib.rs",
        "first",
        "a note in its own graph",
    );

    // Where it landed: the named graph, and not the default one.
    let count = |query: &str| {
        let answer = text(
            &hub,
            Verb::Source,
            "urn:iki:store:select",
            &[("query", query)],
        );
        let json: serde_json::Value = serde_json::from_str(&answer).expect("SPARQL results JSON");
        json["results"]["bindings"][0]["n"]["value"]
            .as_str()
            .unwrap_or_default()
            .parse::<usize>()
            .unwrap_or_else(|_| panic!("a count: {answer}"))
    };
    let annotates = "<https://ikigai-rs.dev/ns#annotates>";
    assert_eq!(
        count(&format!(
            "SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{browse_graph}> {{ ?a {annotates} ?t }} }}"
        )),
        1,
        "the mount took the choice: browse's quads are in <{browse_graph}>"
    );
    assert_eq!(
        count(&format!(
            "SELECT (COUNT(*) AS ?n) WHERE {{ ?a {annotates} ?t }}"
        )),
        0,
        "…and none stayed in the default graph, which is the whole of what a migration has \
         to move for a store that already has some"
    );

    // The promise took the choice too: the browse graph moved, and it was allowed to.
    let changed = store
        .reserved_graphs_fingerprint()
        .expect("still declaring")
        .changed_since(&before);
    assert!(
        changed.is_empty(),
        "the sharer wrote {changed:?}, outside the graph this choice promised it"
    );

    // ★ And what that costs, from `ikigai-store` rather than from a comment: a graph the
    // sharer may write is not covered, so a scoped read of it can no longer be cached.
    assert!(
        !store.read_is_covered(Some(&browse_graph)),
        "obligation 3 on `browse::Graph`: naming a graph forfeits its cacheability, and the \
         host owes a fresh freshness argument for it"
    );
    assert!(
        store.read_is_covered(Some(LEDGER_GRAPH)),
        "…and the LEDGER's exemption is untouched, which is why this is a cost and not a cliff"
    );
    assert!(
        store.read_is_covered(Some(ANOTHER_WRITER)),
        "as is every other graph in the dataset"
    );
}

/// The point of one dataset: a ledger item that is ABOUT a repo file joins an annotation on
/// that same file, in one query, with no federation.
///
/// ★ **Obligation 4, in the one query this repo owns.** Both halves name their graph now —
/// before the graph decision the browse half was a bare pattern, because browse's quads were
/// in the default graph. The failure mode of forgetting is an empty result set, never an
/// error, which is why the README's migration steps say it out loud for an operator's own
/// saved queries.
///
/// ⚠ This one runs under ROOT, through the broad `urn:iki:store:select`, and it stays that
/// way on purpose: it is the WHOLE-DATASET statement of the property — both halves in one
/// store, named, with no federation — and root is what the socket door hands the owner.
/// The same join **below** root, through `urn:iki:store:graph-select` with both graphs in one
/// `graph=` value and nothing but the two read tokens, is
/// `a_scoped_token_reads_browse_and_both_tokens_run_the_join`. Until `ikigai-store` 0.2.5 that
/// second test could not exist: `confine` set the query's whole dataset to the one graph the
/// read was issued for, so each HALF was grantable and the join was not (ledger #380).
#[test]
fn a_ledger_item_joins_an_annotation_on_a_repo_file() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let file = "urn:repo:demo:file:src/lib.rs";
    let filed = text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "lib.rs needs a doc comment"), ("about", file)],
    );
    assert!(filed.contains("urn:iki:ledger:default:item:"), "{filed}");
    annotate(&hub, "n1", file, "first", "this line is the one");

    let chosen = browse::Graph::chosen();
    // ⚠ `.as_str()`: `NamedNode`'s Display brackets the IRI itself, so `<{node}>` writes
    // `<<urn:…>>` and the query fails to parse.
    let browse_graph = chosen.named().expect("a named browse graph").as_str();
    let answer = text(
        &hub,
        Verb::Source,
        "urn:iki:store:select",
        &[(
            "query",
            &format!(
                "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
             PREFIX ik: <https://ikigai-rs.dev/ns#>
             PREFIX oa: <http://www.w3.org/ns/oa#>
             SELECT ?item ?file ?note WHERE {{
               GRAPH <urn:iki:ledger:graph:default> {{ ?item ledger:about ?file }}
               GRAPH <{browse_graph}> {{ ?annotation ik:annotates ?file ; oa:bodyValue ?note }}
             }}"
            ),
        )],
    );
    println!("--- the join ---\n{answer}");
    let json: serde_json::Value = serde_json::from_str(&answer).expect("SPARQL results JSON");
    let rows = json["results"]["bindings"].as_array().expect("bindings");
    assert_eq!(rows.len(), 1, "one item, one annotation, one row: {answer}");
    assert_eq!(rows[0]["file"]["value"], file);
    assert_eq!(rows[0]["note"]["value"], "this line is the one");
    assert!(
        rows[0]["item"]["value"]
            .as_str()
            .unwrap_or_default()
            .starts_with("urn:iki:ledger:default:item:"),
        "{answer}"
    );
}

/// ★ **Obligation 3, from the running kernel rather than from the store's promise.**
///
/// `a_named_graph_choice_moves_the_promise_with_it` asks `ikigai-store` whether the browse
/// graph is covered. This asks the KERNEL what it actually did with two reads — the one thing
/// an operator would notice — and the pair is the whole of the freshness redo:
///
/// - a scoped read of the BROWSE graph is `Expiry::Always` and leaves nothing in the cache,
///   because the sharer (`ikigai-browse`) may write it behind the store's back;
/// - a scoped read of the LEDGER graph is cacheable and is cached, exactly as before the
///   graph decision — which is the ~1000× this server refuses to give up.
///
/// ⚠ Both reads are the same IRI with different arguments, so `urn:kernel:cache`'s rows
/// cannot tell them apart: the count of rows for that IRI is what moves, and it moves once.
#[test]
fn the_browse_graphs_scoped_reads_are_not_cached_and_the_ledgers_still_are() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let chosen = browse::Graph::chosen();
    let browse_graph = chosen.named().expect("a named browse graph").as_str();
    let ledger_graph = "urn:iki:ledger:graph:default";

    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "an item, so the ledger's graph has quads")],
    );
    annotate(
        &hub,
        "n1",
        "urn:repo:demo:file:src/lib.rs",
        "first",
        "a note",
    );
    let any = "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1";
    let rows = |kernel: &Kernel| {
        cached(kernel)
            .iter()
            .filter(|iri| *iri == "urn:iki:store:graph-select")
            .count()
    };
    // The writes above already read the ledger's graph through the scoped door, so the
    // baseline is whatever they left behind: what this test measures is which of the two
    // reads below ADDS a row.
    let baseline = rows(&hub);

    let ledger = issue(
        &hub,
        Verb::Source,
        "urn:iki:store:graph-select",
        &[("graph", ledger_graph), ("query", any)],
    );
    assert_ne!(
        ledger.expiry,
        ikigai_core::Expiry::Always,
        "the ledger's graph is outside the sharer's promise, so its scoped reads stay \
         cacheable — this is the exemption the graph decision must not have cost"
    );
    let after_the_ledger = rows(&hub);
    assert_eq!(
        after_the_ledger,
        baseline + 1,
        "…and it is in the cache: {:?}",
        cached(&hub)
    );

    let browse_read = issue(
        &hub,
        Verb::Source,
        "urn:iki:store:graph-select",
        &[("graph", browse_graph), ("query", any)],
    );
    assert_eq!(
        browse_read.expiry,
        ikigai_core::Expiry::Always,
        "obligation 3: a graph the sharer may write cannot be cached, and `ikigai-store` \
         answers that per read from the promise `main` derived — there is no declaration in \
         this crate that could have got it wrong, and none that could have got it right"
    );
    assert_eq!(
        rows(&hub),
        after_the_ledger,
        "the browse read added no cache entry: {:?}",
        cached(&hub)
    );

    // ★ And the SET, since `ikigai-store` 0.2.5: a multi-graph read is covered only if EVERY
    // member is, so the ledger↔browse join inherits the browse graph's exposure and is
    // uncached — while the ledger's own scoped read, one line above, is not. That is the
    // honest shape of the payoff and it is the reason it is asserted rather than assumed: the
    // two reads are the same IRI with different arguments, so nothing but this distinguishes
    // "the join is cheap" from "the join is a fresh query every time".
    let together = issue(
        &hub,
        Verb::Source,
        "urn:iki:store:graph-select",
        &[
            ("graph", &format!("{ledger_graph} {browse_graph}")),
            ("query", any),
        ],
    );
    assert_eq!(
        together.expiry,
        ikigai_core::Expiry::Always,
        "a set is covered only if every graph is: the join reads a graph the sharer may \
         write, so it cannot be cached even though the ledger half could be"
    );
    assert_eq!(
        rows(&hub),
        after_the_ledger,
        "…and it left nothing in the cache either: {:?}",
        cached(&hub)
    );
}

/// ★ **A bare `{ ?s ?p ?o }` through the broad door answers `0` here, and that is not an
/// empty store** — it is the store's DEFAULT graph, which this server never writes. Every
/// quad it holds is in a named graph (a ledger's, or browse's), and `urn:iki:store:select`
/// does not union named graphs into the default one.
///
/// This is asserted rather than left to prose because the README now hands an operator a
/// table of three reasons a `0` lies while verifying an archive import, and this is the row
/// nothing else in the suite covers: the sibling
/// [`a_named_graph_choice_moves_the_promise_with_it`] checks where the quads LAND, which is a
/// fact about the data; this checks what the unscoped QUERY sees, which is a fact about
/// `ikigai-store`'s dataset construction and is exactly the kind of claim that drifts
/// silently when a dependency changes its default. If a future `ikigai-store` unions, this
/// fails and the README's table is wrong in the same commit.
///
/// ⚠ It is also the more dangerous direction of the two. A wrong answer here reads as
/// "nothing was imported" and invites an operator to run a destructive step again.
#[test]
fn a_bare_pattern_select_sees_nothing_because_every_quad_is_in_a_named_graph() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let browse_graph = browse::Graph::chosen()
        .named()
        .expect("a named browse graph")
        .as_str()
        .to_string();

    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "an item, so the ledger's graph has quads")],
    );
    annotate(
        &hub,
        "n1",
        "urn:repo:demo:file:src/lib.rs",
        "first",
        "a note",
    );

    let count = "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }";
    let unscoped = text(
        &hub,
        Verb::Source,
        "urn:iki:store:select",
        &[("query", count), ("as", "text/csv")],
    );
    assert!(
        unscoped.lines().any(|line| line.trim() == "0"),
        "the broad door's bare pattern reads the default graph, which holds nothing — a \
         non-zero here means either this server started writing the default graph or \
         `ikigai-store` began unioning, and the README's verification table is wrong either \
         way. Got:\n{unscoped}"
    );

    // The control, so the zero above cannot be a store this test failed to populate: the SAME
    // query, named at the graph, finds the annotation's quads.
    let scoped = text(
        &hub,
        Verb::Source,
        "urn:iki:store:graph-select",
        &[
            ("graph", &browse_graph),
            ("query", count),
            ("as", "text/csv"),
        ],
    );
    assert!(
        !scoped.lines().any(|line| line.trim() == "0"),
        "the control: browse's graph is not empty, so the zero above is about the DATASET \
         and not about the data. Got:\n{scoped}"
    );
}

/// ★ **Obligation 2, end to end: what a per-graph token over browse's data buys, the
/// boundary it does not cross — and, since `ikigai-store` 0.2.5, what TWO tokens buy
/// together.**
///
/// Before the graph decision this test could not have been written. Browse's quads were in
/// the default graph, which has no IRI, so no `urn:cap:store:read:graph:` token could name
/// them and every read of an annotation or an archived explanation needed root.
///
/// The assertions are the grant's shape:
///
/// 1. the token READS browse's quads — the archive without the spend, which is what the
///    `urn:cap:net:*` on `urn:repo:{root}:explain` makes impossible through the browse row;
/// 2. it reaches no other graph, so it is not a ledger grant in disguise;
/// 3. it reaches no browse ENDPOINT — no file, no tree, no `gh`;
/// 4. ★ **and the two read tokens together RUN THE JOIN** — the motivating query of the
///    graph decision, of ledger #250 and of #376, under a grant an operator can mint
///    (`ikigai-gonk client add … --ledger default=read --browse-graph read`, whose two halves
///    are the two lists composed below). Until `ikigai-store` 0.2.5 this test asserted the
///    opposite: `confine` set the query's whole dataset to the ONE graph the read was issued
///    for, so the join was inexpressible through the narrow door however many tokens were
///    held, and — worse — came back EMPTY rather than refused (ledger #380);
/// 5. ⚠ **and the ledger's token alone is REFUSED on that same query, naming the browse
///    graph's token.** Refused, not answered over the half it holds: an answer computed from
///    a narrower dataset than the one asked for returns rows that look right and are not.
///
/// ⚠ **The set is ONE whitespace-separated `graph` value.** `graph=A graph=B` is last-wins in
/// the engine — silently, with no error (ledger #387) — so the repeated form asks for the
/// LAST graph alone and a join written that way comes back empty. The final block below pins
/// that, because it is the same silent narrowing assertion 4 exists to have closed.
#[test]
fn a_scoped_token_reads_browse_and_both_tokens_run_the_join() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let chosen = browse::Graph::chosen();
    let browse_graph = chosen.named().expect("a named browse graph").as_str();
    let file = "urn:repo:demo:file:src/lib.rs";
    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "lib.rs needs a doc comment"), ("about", file)],
    );
    annotate(&hub, "n1", file, "first", "this line is the one");

    // EXACTLY the tokens `ikigai-gonk grants --browse-graph read` prints, and nothing else.
    let reader = Capability::scoped(
        ikigai_gonk::grants::browse_graph_grants(Authority::Read).expect("a named browse graph"),
    );
    let scoped = |graph: &str, query: &str| {
        block_on(Kernel::issue(
            &hub,
            request(
                Verb::Source,
                "urn:iki:store:graph-select",
                &[("graph", graph), ("query", query)],
            ),
            &reader,
        ))
    };

    // 1. The archive, as data, to a caller holding one narrow token.
    let note = scoped(
        browse_graph,
        "PREFIX oa: <http://www.w3.org/ns/oa#>
         SELECT ?note WHERE { ?a oa:bodyValue ?note }",
    )
    .expect("the browse graph's own token reads the browse graph");
    let note = String::from_utf8_lossy(&note.bytes).into_owned();
    assert!(note.contains("this line is the one"), "{note}");

    // 2. One graph, not the dataset.
    assert!(
        matches!(
            scoped(
                "urn:iki:ledger:graph:default",
                "SELECT ?s WHERE { ?s ?p ?o }"
            ),
            Err(ikigai_core::Error::Denied(_))
        ),
        "a browse-graph token must not read the ledger's graph"
    );
    for broad in ["urn:iki:store:select", "urn:iki:store:info"] {
        assert!(
            matches!(
                block_on(Kernel::issue(
                    &hub,
                    request(
                        Verb::Source,
                        broad,
                        &[("query", "SELECT ?s WHERE { ?s ?p ?o }")]
                    ),
                    &reader,
                )),
                Err(ikigai_core::Error::Denied(_))
            ),
            "{broad} must stay root-only: it can see every graph, including the default one"
        );
    }

    // 3. The data, not the family: no file, no tree, no `gh`, no annotation endpoint.
    for denied in [
        file,
        "urn:repo:demo:tree",
        "urn:repo:demo:annotations",
        "urn:iki:annotation:n1",
        "urn:repo:status",
    ] {
        assert!(
            matches!(
                block_on(Kernel::issue(
                    &hub,
                    request(Verb::Source, denied, &[]),
                    &reader
                )),
                Err(ikigai_core::Error::Denied(_))
            ),
            "`{denied}` is the browse FAMILY, which this grant does not carry"
        );
    }

    // 3b. One graph is still one graph: naming the LEDGER's graph inside a query scoped to
    // the browse graph alone matches nothing, because the available named graphs are the set
    // the read was issued for and this set has one member. The narrowing is what a token
    // means; what 0.2.5 changed is that a caller may now ASK for both.
    let unasked = scoped(
        browse_graph,
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
         SELECT ?item WHERE { GRAPH <urn:iki:ledger:graph:default> { ?item ledger:about ?f } }",
    )
    .expect("the query is legal; the graph it names is simply not in the set");
    assert!(
        rows_of(&unasked).is_empty(),
        "a graph outside the set matches nothing rather than erroring — ikigai-store's own \
         `GRAPH <other>` row, and the reason the set is named in `graph=` and not in the query"
    );

    // 4. ★ THE JOIN, under the two read tokens an operator can mint — and nothing else.
    // `grants_for(.., Read)` and `browse_graph_grants(Read)` are the two halves `main`'s
    // `scopes_for` composes for `client add`/`passkey invite`.
    let mut both = grants_for("default", Authority::Read).expect("the ledger's read tokens");
    both.extend(browse_graph_grants(Authority::Read).expect("a named browse graph"));
    let both = Capability::scoped(both);
    // ⚠ ONE value, whitespace-separated. Ledger first, which is NOT the canonical spelling
    // (the store sorts the set and re-issues a non-canonical spelling under the canonical
    // one, so every spelling shares one computed entry) — the answer must not depend on it.
    let set = format!("urn:iki:ledger:graph:default {browse_graph}");
    let join = "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
         PREFIX ik: <https://ikigai-rs.dev/ns#>
         PREFIX oa: <http://www.w3.org/ns/oa#>
         SELECT DISTINCT ?item ?file ?note WHERE {
           GRAPH <urn:iki:ledger:graph:default> { ?item ledger:about ?file }
           GRAPH <URN_BROWSE> { ?a ik:annotates ?file ; oa:bodyValue ?note }
         }"
    .replace("URN_BROWSE", browse_graph);
    let joined = block_on(Kernel::issue(
        &hub,
        request(
            Verb::Source,
            "urn:iki:store:graph-select",
            &[("graph", &set), ("query", &join)],
        ),
        &both,
    ))
    .expect("both read tokens run the join — ikigai-store 0.2.5");
    let rows = rows_of(&joined);
    assert_eq!(
        rows.len(),
        1,
        "one item, one annotation, one row — the ledger↔browse join, below root at last: {}",
        String::from_utf8_lossy(&joined.bytes)
    );
    assert_eq!(rows[0]["file"]["value"], file);
    assert_eq!(rows[0]["note"]["value"], "this line is the one");
    assert!(
        rows[0]["item"]["value"]
            .as_str()
            .unwrap_or_default()
            .starts_with("urn:iki:ledger:default:item:"),
        "{rows:?}"
    );

    // 5. ⚠ The same query under the LEDGER's tokens alone is REFUSED, and the refusal names
    // the token that is missing — never answered over the ledger half, which would be rows
    // that look right and are not.
    let ledger_only = Capability::scoped(
        grants_for("default", Authority::Read).expect("the ledger's read tokens"),
    );
    let denied = block_on(Kernel::issue(
        &hub,
        request(
            Verb::Source,
            "urn:iki:store:graph-select",
            &[("graph", &set), ("query", &join)],
        ),
        &ledger_only,
    ));
    let Err(ikigai_core::Error::Denied(detail)) = denied else {
        panic!("a graph the caller holds no token for must be refused: {denied:?}");
    };
    assert!(
        detail.contains(&ikigai_store::cap_read_graph(browse_graph)),
        "the refusal must name the browse graph's own token, because that is what an \
         operator has to mint: {detail}"
    );

    // ⚠ And the trap this spelling avoids. `graph=A graph=B` is not a set: `Request::args`
    // holds ONE value per name and the engine inserts named arguments last-wins, silently
    // (ledger #387), so the repeated form asks for the LAST graph alone. Holding both tokens
    // does not save it — the read is legal, and the join is empty with nothing said.
    let repeated = block_on(Kernel::issue(
        &hub,
        request(
            Verb::Source,
            "urn:iki:store:graph-select",
            &[
                ("graph", "urn:iki:ledger:graph:default"),
                ("graph", browse_graph),
                ("query", &join),
            ],
        ),
        &both,
    ))
    .expect("last-wins leaves a legal one-graph read, which is exactly the problem");
    assert!(
        rows_of(&repeated).is_empty(),
        "the repeated form silently narrows to the last value — pinned here so that a fix \
         upstream (a refusal, or a declared repeatable argument) shows up as a red test in \
         the repo that would otherwise go on spelling it the safe way by folklore"
    );
}

/// The `results.bindings` of a SPARQL results JSON representation.
fn rows_of(repr: &Representation) -> Vec<serde_json::Value> {
    let text = String::from_utf8_lossy(&repr.bytes).into_owned();
    let json: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("SPARQL results JSON ({e}): {text}"));
    json["results"]["bindings"]
        .as_array()
        .unwrap_or_else(|| panic!("bindings: {text}"))
        .clone()
}

// ------------------------------------------------------------ what it costs

/// A live-sized corpus: 247 items, the size of the ledger this server actually serves.
fn seed_items(kernel: &Kernel, n: usize) {
    for i in 0..n {
        text(
            kernel,
            Verb::Sink,
            "urn:iki:ledger:append",
            &[(
                "content",
                &format!("Item {i}\n\nA body long enough to be a real row in the graph."),
            )],
        );
    }
}

/// Time `n` reads of `iri`, after one warm-up read.
fn time_reads(kernel: &Kernel, iri: &str, n: u32) -> Duration {
    issue(kernel, Verb::Source, iri, &[]);
    let start = Instant::now();
    for _ in 0..n {
        issue(kernel, Verb::Source, iri, &[]);
    }
    start.elapsed() / n
}

/// ★ Sharing the store handle must not de-cache the ledger — measured, because the types are
/// identical either way and 68 passing tests is exactly what the cms-web regression looked
/// like.
///
/// Three kernels over the same 247-item corpus: the owned store this server composed before
/// it had a browse face, the shared store with no declaration (what calling `open_shared` and
/// accepting the blanket would have shipped), and the composition `main` builds — shared, and
/// declaring where the sharer writes.
#[test]
fn the_shared_handle_costs_the_ledger_nothing() {
    const ITEMS: usize = 247;
    let dir = scratch_root();
    let (served, _watch) = served(&dir);
    let naive = naive(&dir);
    let owned = owned();
    // ★ The CONTROL for the graph decision: the same composition with the choice this server
    // shipped until 2026-09-16. `served` above is the named graph — so the two rows printed
    // below are exactly "before" and "after" for the ledger's hot read, and the claim that
    // naming a graph cost the ledger nothing is a number rather than an argument.
    let before_the_decision = served_in(&dir, None, browse::Graph::TheDefault);
    let default_graph = before_the_decision.hub;
    let _watch = before_the_decision.watch;
    for kernel in [&owned, &naive, &served, &default_graph] {
        seed_items(kernel, ITEMS);
    }

    println!("--- {ITEMS} items, mean of 20 reads ---");
    for (label, kernel) in [
        ("owned     (open — no browse face)", &owned),
        ("shared    (open_shared, undeclared)", &naive),
        ("declared  (browse in the DEFAULT graph)", &default_graph),
        ("declared  (browse in its NAMED graph — shipped)", &served),
    ] {
        let items = time_reads(kernel, "urn:iki:ledger:items", 20);
        let next = time_reads(kernel, "urn:iki:ledger:next", 20);
        println!("  {label:38}  items {items:>10.2?}  next {next:>10.2?}");
    }
    // The browse half: the same reads on the root this server watches (cached) and on the
    // composition that declares them live (uncached), which is `ikigai-browse`'s own default.
    println!("--- the watched root's filesystem reads ---");
    for (label, kernel) in [("live (unwatched)", &naive), ("watched", &served)] {
        let tree = time_reads(kernel, "urn:repo:demo:tree", 20);
        let file = time_reads(kernel, "urn:repo:demo:file:src/lib.rs", 20);
        println!("  {label:38}  tree  {tree:>10.2?}  file {file:>10.2?}");
    }

    // The assertion is not the timing — it is WHETHER the read is in the cache at all, which
    // is what the timing follows from and what cannot flake on a loaded machine.
    let hot = cached(&served);
    assert!(
        hot.iter().any(|iri| iri == "urn:iki:ledger:items"),
        "the declared composition must cache a ledger read: {hot:?}"
    );
    assert!(
        hot.iter().any(|iri| iri == "urn:iki:store:graph-select"),
        "…and the scoped store read under it: {hot:?}"
    );
    let cold = cached(&naive);
    assert!(
        !cold.iter().any(|iri| iri == "urn:iki:ledger:items"),
        "the naive composition caches nothing store-derived — that is the cost this arc \
         refused to pay: {cold:?}"
    );
    // ★ And the control, asserted rather than only printed: the ledger's read is cached on
    // BOTH sides of the graph decision. If naming the browse graph had de-cached the ledger —
    // the regression obligation 3 exists to prevent — this is where it would show, with the
    // timings above as the size of it.
    let before = cached(&default_graph);
    assert!(
        before.iter().any(|iri| iri == "urn:iki:ledger:items")
            && before.iter().any(|iri| iri == "urn:iki:store:graph-select"),
        "the composition as it shipped before the graph decision: {before:?}"
    );
    // And the broad face stays uncacheable in BOTH, by name: it can see the default graph,
    // where browse's invisible writes land.
    for (label, kernel) in [("declared", &served), ("naive", &naive)] {
        issue(
            kernel,
            Verb::Source,
            "urn:iki:store:select",
            &[("query", "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1")],
        );
        assert!(
            !cached(kernel)
                .iter()
                .any(|iri| iri == "urn:iki:store:select"),
            "{label}: the broad read door must not be cached over a shared store"
        );
    }
}

// ------------------------------------------------- what each door can reach

/// ★ The capability argument, re-made for the families this arc links, as a test rather than
/// as a README paragraph.
///
/// An anonymous loopback caller on the HTTP door holds exactly the configured ledgers' narrow
/// read+write tokens (`doors::HttpDoor::anonymous`). Browse and the facades are now COMPILED
/// IN and reachable — but reachable is a function of the grant, and that grant carries
/// neither `urn:cap:browse:read:*` nor `urn:cap:exec:*`. Both halves are checked, because
/// they fail differently: the manifold must not OFFER what the caller cannot invoke
/// (over-offer), and the invocation must be DENIED rather than served.
#[test]
fn the_anonymous_http_caller_is_offered_and_allowed_nothing_from_the_new_families() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let anonymous = Capability::scoped(
        grants_for_all(&["default".to_string()], Authority::Write).expect("the default ledger"),
    );

    let manifold = String::from_utf8_lossy(
        &block_on(Kernel::issue(
            &hub,
            request(Verb::Source, "urn:kernel:actions", &[]),
            &anonymous,
        ))
        .expect("the manifold answers any capability about itself")
        .bytes,
    )
    .into_owned();
    // The manifold names the templated rows (`{ledger}` is an argument of the action, not a
    // separate offer), and the store's SCOPED faces — which is exactly the pair of grants
    // `grants_for` hands out.
    for offered in [
        "urn:iki:ledger:{ledger}:items",
        "urn:iki:store:graph-select",
    ] {
        assert!(
            manifold.contains(offered),
            "`{offered}` is what this caller was granted: {manifold}"
        );
    }
    for absent in [
        "urn:repo:",
        "urn:iki:annotation",
        "urn:system:exec",
        "urn:iki:store:select",
    ] {
        assert!(
            !manifold.contains(absent),
            "`{absent}` must not be offered to an anonymous loopback caller:\n{manifold}"
        );
    }

    for denied in [
        "urn:repo:demo:tree",
        "urn:repo:demo:file:src/lib.rs",
        "urn:repo:status",
        "urn:system:exec",
    ] {
        let answer = block_on(Kernel::issue(
            &hub,
            request(Verb::Source, denied, &[]),
            &anonymous,
        ));
        assert!(
            matches!(answer, Err(ikigai_core::Error::Denied(_))),
            "`{denied}` must be a typed Denied for an anonymous caller, got {answer:?}"
        );
    }
}

// ----------------------------------------------------------- what it watches

/// ★ A cached filesystem read must recompute when the file changes on disk — driven through
/// the watcher's OWN notification path rather than a sleep, so a slow platform fails loudly
/// instead of passing by luck.
///
/// The stale read in the middle is the premise, not a wish: it proves the recompute at the
/// end is the cut's doing and not an uncached endpoint's. This is ledger #246 in miniature,
/// with the halves the other way round — here the thread is declared only because something
/// cuts it.
#[test]
fn a_watched_file_read_recomputes_after_the_file_changes_on_disk() {
    let dir = scratch_root();
    let (hub, watch) = served(&dir);
    let file = "urn:repo:demo:file:src/lib.rs";
    assert_eq!(text(&hub, Verb::Source, file, &[]), "the first version\n");

    // std::fs on purpose: this IS the out-of-band edit the kernel cannot see.
    std::fs::write(dir.path().join("src/lib.rs"), "the second version\n").expect("the edit");
    assert_eq!(
        text(&hub, Verb::Source, file, &[]),
        "the first version\n",
        "the cached read must hold until its thread is cut"
    );

    let thread = "urn:iki:gonk:browse:root:demo".to_string();
    let mut cut = false;
    for _ in 0..64 {
        match watch.apply_next(&hub, Duration::from_secs(10)) {
            Some(threads) if threads.contains(&thread) => {
                cut = true;
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    assert!(
        cut,
        "the watcher never reported {thread} within its deadline"
    );
    assert_eq!(
        text(&hub, Verb::Source, file, &[]),
        "the second version\n",
        "a cut thread must make the next read recompute"
    );
}

/// The fail-closed half: a root nobody is watching is served live and uncached, so there is
/// no composition in which a stale file read can be served at all.
#[test]
fn an_unwatched_roots_reads_are_not_cached() {
    let dir = scratch_root();
    let graph = browse::Graph::chosen();
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    // Wired with an EMPTY watched set — what `main` builds for a root whose platform watcher
    // refused to start.
    let wired = browse::wire(roots(&dir), handle, &[], None, &graph);
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        Vec::new(),
        None,
    ));
    let file = "urn:repo:demo:file:src/lib.rs";
    assert_eq!(text(&hub, Verb::Source, file, &[]), "the first version\n");
    assert!(
        !cached(&hub).iter().any(|iri| iri == file),
        "an unwatched root must not be cached"
    );
    std::fs::write(dir.path().join("src/lib.rs"), "the second version\n").expect("the edit");
    assert_eq!(
        text(&hub, Verb::Source, file, &[]),
        "the second version\n",
        "…and its reads are therefore never stale"
    );
}

// ------------------------------------------------- the join, through the HTTP door

/// ★ **The question the arc was dispatched to answer honestly: can a caller holding both
/// read tokens run the ledger↔browse join through the HTTP door, or only through the socket?**
///
/// It can — and not through a page. The `/sparql` editor picks a LEDGER (its box names a
/// ledger, and the query runs against that ledger's graph alone), so the join is not
/// reachable from the editor whatever tokens the caller holds; that is gonk's "Not built"
/// list, unchanged by 0.2.5. What is reachable is the STORE's own resource under
/// `ikigai-web`'s mechanical path mapping: `/iki/store/graph-select` → the IRI, query
/// parameters → arguments, `Accept` → the face. So a client that speaks HTTP rather than a
/// browser gets the join, under exactly the capability the door computed for its request.
///
/// Three assertions, in the order an operator meets them:
///
/// 1. the ANONYMOUS loopback caller — `gonk.http.ledger`'s ledgers, and nothing else — is
///    REFUSED, and the refusal names the browse graph's token. The browse token is never
///    anonymous authority on this door;
/// 2. a passkey identity enrolled with `--ledger default=read --browse-graph read` runs the
///    join and gets the row;
/// 3. ⚠ and the natural repeated spelling `graph=A&graph=B` comes back **200 with no rows**,
///    because query parameters become named arguments one per name (ledger #387) — the
///    engine's last-wins narrowing, reachable from a URL. Same value, one door further out.
#[test]
fn the_join_runs_through_the_http_door_under_a_signed_in_grant() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let chosen = browse::Graph::chosen();
    let browse_graph = chosen
        .named()
        .expect("a named browse graph")
        .as_str()
        .to_string();
    let file = "urn:repo:demo:file:src/lib.rs";
    text(
        &hub,
        Verb::Sink,
        "urn:iki:ledger:append",
        &[("content", "lib.rs needs a doc comment"), ("about", file)],
    );
    annotate(&hub, "n1", file, "first", "this line is the one");

    let door = HttpDoorHarness::start(Arc::clone(&hub));
    let query = format!(
        "PREFIX ledger: <https://ikigai-rs.dev/ns/ledger#>
         PREFIX ik: <https://ikigai-rs.dev/ns#>
         PREFIX oa: <http://www.w3.org/ns/oa#>
         SELECT DISTINCT ?item ?file ?note WHERE {{
           GRAPH <urn:iki:ledger:graph:default> {{ ?item ledger:about ?file }}
           GRAPH <{browse_graph}> {{ ?a ik:annotates ?file ; oa:bodyValue ?note }}
         }}"
    );
    // ONE `graph` parameter, the two IRIs separated by whitespace — the same wire shape the
    // socket door takes, because it is the same resource behind both.
    let path = format!(
        "/iki/store/graph-select?graph={}&query={}",
        urlencode(&format!("urn:iki:ledger:graph:default {browse_graph}")),
        urlencode(&query)
    );

    // 1. Anonymous: the ledgers' tokens only.
    let refused = door.get(&path, None);
    assert_eq!(
        refused.0, 403,
        "an anonymous loopback caller holds `gonk.http.ledger`'s ledgers and NOT the browse \
         graph — the join must be refused: {}",
        refused.1
    );
    assert!(
        refused
            .1
            .contains(&ikigai_store::cap_read_graph(&browse_graph)),
        "…and the refusal names the token that is missing: {}",
        refused.1
    );

    // 2. Signed in with both read tokens: the row.
    let token = door.enrol_and_sign_in();
    let (status, body) = door.get(&path, Some(&token));
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("SPARQL results JSON");
    let rows = json["results"]["bindings"].as_array().expect("bindings");
    assert_eq!(rows.len(), 1, "the ledger↔browse join, over HTTP: {body}");
    assert_eq!(rows[0]["file"]["value"], file);
    assert_eq!(rows[0]["note"]["value"], "this line is the one");

    // 3. ⚠ The repeated form, which is what anyone writes first in a URL.
    let repeated = format!(
        "/iki/store/graph-select?graph={}&graph={}&query={}",
        urlencode("urn:iki:ledger:graph:default"),
        urlencode(&browse_graph),
        urlencode(&query)
    );
    let (status, body) = door.get(&repeated, Some(&token));
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("SPARQL results JSON");
    assert!(
        json["results"]["bindings"]
            .as_array()
            .expect("bindings")
            .is_empty(),
        "`graph=A&graph=B` is one argument, last value wins: the join silently narrows to \
         one graph and answers 200 with nothing (ledger #387) — {body}"
    );
}

// --------------------------------------------------------------- the browse door

/// A grant that can actually browse: the ledger it lands on, the browse graph read AND
/// write, the browse family's read floor, and the authority to annotate. Every one of these
/// is a scope this server mints for NO anonymous caller — which is the whole design, and
/// what the two refusal tests below are about.
///
/// ⚠ No `urn:cap:net:*` of any spelling, so nothing here can derive. That is not an omission
/// in the fixture: gonk mints no net grant, and a test that granted one would be testing a
/// server nobody runs.
fn browsing_scopes() -> Vec<String> {
    let mut scopes = grants_for("default", Authority::Read).expect("the ledger's tokens");
    scopes.extend(browse_graph_grants(Authority::Write).expect("a named browse graph"));
    scopes.push(ikigai_browse::CAP_WILDCARD.to_string());
    scopes.push(ikigai_browse::CAP_ANNOTATE.to_string());
    scopes
}

/// The command form this door speaks — `/k?c=<command>`, the command percent-encoded whole.
fn k(command: &str) -> String {
    format!("/k?c={}", urlencode(command))
}

/// ★ The door itself: the page, the adapter, and the affordances arriving UNCHANGED.
///
/// The last assertion is the one that matters most, and it is deliberately an assertion
/// about `ikigai-browse`'s bytes rather than about gonk's: the face gonk serves still says
/// `hx-get="/k/source …"`, in the path spelling, because nothing here rewrites what browse
/// emits. Folding that path into this door's query form happens in the browser
/// (`web/gonk.js`), which is why a face can grow a Review button — or any other — without a
/// line changing here.
#[test]
fn the_browse_door_serves_the_faces_and_their_own_affordances() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let door = HttpDoorHarness::start(Arc::clone(&hub));
    let token = door.enrol_and_sign_in_with(browsing_scopes());

    // 1. The page: gonk's own chrome, and ONE region that loads the named face.
    let (status, page) = door.get_html("/browse/urn:repo:demo:tree", Some(&token));
    assert_eq!(status, 200, "{page}");
    assert!(
        page.contains("auth-login"),
        "the browse page is a gonk page — same header, same sign-in control: {page}"
    );
    assert!(
        page.contains(&urlencode("source urn:repo:demo:tree as=text/html")),
        "…whose one region loads the start resource through the adapter: {page}"
    );
    assert!(
        page.contains(&urlencode("source urn:repo:style")),
        "…and links the browse family's own stylesheet, through the same adapter: {page}"
    );

    // 2. A file IRI carries SLASHES, so the page's path is several segments.
    let (status, deep) = door.get_html("/browse/urn:repo:demo:file:src/lib.rs", Some(&token));
    assert_eq!(status, 200, "{deep}");
    assert!(
        deep.contains(&urlencode(
            "source urn:repo:demo:file:src/lib.rs as=text/html"
        )),
        "the start IRI keeps its slashes across the route: {deep}"
    );
    // ★ …and an ENCODED slash reaches the same IRI (ikigai-web 0.1.30). A `%2F` is data inside
    // its segment now, so this path is ONE segment and takes the depth-1 route, where the
    // decoded `/` joins the capture instead of splitting it — the same start IRI either way.
    // Through 0.1.29 the decoder split on it and the two spellings met by accident.
    let (status, encoded) = door.get_html("/browse/urn:repo:demo:file:src%2Flib.rs", Some(&token));
    assert_eq!(status, 200, "{encoded}");
    assert!(
        encoded.contains(&urlencode(
            "source urn:repo:demo:file:src/lib.rs as=text/html"
        )),
        "an encoded slash is lossless across the route: {encoded}"
    );

    // 3. The face itself, through the adapter — and its affordances, verbatim.
    let (status, face) = door.get_html(&k("source urn:repo:demo:tree as=text/html"), Some(&token));
    assert_eq!(status, 200, "{face}");
    assert!(
        face.contains("hx-get=\"/k/source urn:repo:demo:file:README.md as=text/html\""),
        "browse's own affordance, in browse's own path spelling, untouched: {face}"
    );

    let (status, file) = door.get_html(
        &k("source urn:repo:demo:file:src/lib.rs as=text/html"),
        Some(&token),
    );
    assert_eq!(status, 200, "{file}");
    assert!(file.contains("the first version"), "{file}");
    assert!(
        file.contains("hx-post=\"/k/sink urn:iki:annotation\""),
        "the annotate affordance is browse's too: {file}"
    );

    // 4. ⚠ The depth ceiling REFUSES; it does not serve a truncated name.
    let too_deep = format!("/browse/urn:repo:demo:file:{}", vec!["d"; 20].join("/"));
    let (status, _) = door.get_html(&too_deep, Some(&token));
    assert_eq!(
        status, 404,
        "past `doors::BROWSE_DEPTH` no route matches and the answer is a refusal"
    );
}

/// ★ **Both of the browse family's stylesheets, and the LAYOUT one actually resolves.**
///
/// `urn:repo:style` is the syntax theme for the `hl-` classes inside a file view;
/// `urn:repo:style:layout` (ikigai-browse 0.4.2) is the page furniture every other face is
/// made of. Linking one and not the other is what made this door render a tree as a cascade
/// of gonk chips (ledger #441), and the interesting half of this test is the second: the
/// link is fetched through the same adapter a browser would use, so a pin that went
/// backwards would fail here rather than in someone's eyes.
#[test]
fn the_browse_page_links_the_layout_stylesheet_and_it_resolves() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let door = HttpDoorHarness::start(Arc::clone(&hub));
    let token = door.enrol_and_sign_in_with(browsing_scopes());

    let (status, page) = door.get_html("/browse/urn:repo:demo:tree", Some(&token));
    assert_eq!(status, 200, "{page}");
    for sheet in ["source urn:repo:style", "source urn:repo:style:layout"] {
        assert!(
            page.contains(&urlencode(sheet)),
            "the page links `{sheet}`: {page}"
        );
    }

    let (status, css) = door.get_html(
        &k(&format!("source {}", ikigai_browse::LAYOUT_IRI)),
        Some(&token),
    );
    assert_eq!(status, 200, "{css}");
    assert!(
        css.contains(".browse-entries") && css.contains(".browse-crumbs"),
        "…and that link answers the layout rules for the classes the faces emit: {css}"
    );
    assert!(
        css.contains("data-browse-posture"),
        "…including the read-only posture rule this door drives: {css}"
    );
}

/// The posture the door states, and the two ways it can be wrong.
///
/// ⚠ Presentation only — `urn:iki:annotation`'s Sink requires `urn:cap:annotate` whatever
/// the page says. What this pins is that a caller who cannot write is not shown a form that
/// will refuse it, and that a caller who CAN write is not quietly denied the affordance.
#[test]
fn the_read_only_posture_is_stated_only_when_the_caller_cannot_annotate() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let door = HttpDoorHarness::start(Arc::clone(&hub));

    let writer = door.enrol_and_sign_in_with(browsing_scopes());
    let (_, page) = door.get_html("/browse/urn:repo:demo:tree", Some(&writer));
    assert!(
        !page.contains("data-browse-posture"),
        "a caller holding `urn:cap:annotate` states no posture, and sees the form: {page}"
    );

    let read_only: Vec<String> = browsing_scopes()
        .into_iter()
        .filter(|s| s != ikigai_browse::CAP_ANNOTATE)
        .collect();
    let reader = door.enrol_and_sign_in_as("read-only", read_only);
    let (_, page) = door.get_html("/browse/urn:repo:demo:tree", Some(&reader));
    assert!(
        page.contains("data-browse-posture='read-only'"),
        "a caller who cannot annotate is not shown the create form: {page}"
    );
}

/// ★ The way IN (ledger #442): a header link, and the landing page behind it — both built
/// from the roots this caller may READ, never from the roots the server has.
///
/// ⚠ The last block is the one that would have been wrong the easy way. A grant naming ONE
/// root by name is not the all-roots wildcard, and it is not a prefix match either: the
/// offer is `ikigai-browse`'s own two exact tests (`readable_roots`), so a token for a root
/// this server does not configure offers nothing, and a token for `demo` offers `demo`.
#[test]
fn the_header_offers_browse_only_where_a_root_is_readable() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let door = HttpDoorHarness::start(Arc::clone(&hub));

    // 1. Anonymous: gonk mints no browse token for anyone, so there is no way in and no
    //    link to one — but the page behind it still says which grant would open it.
    let (status, page) = door.get_html("/", None);
    assert_eq!(status, 200, "{page}");
    assert!(
        !page.contains(">Browse<") && !page.contains("href='/browse'"),
        "no readable root, no link into a refusal: {page}"
    );
    let (status, roots) = door.get_html("/browse", None);
    assert_eq!(status, 200, "{roots}");
    assert!(
        !roots.contains("/browse/urn:repo:demo:tree"),
        "…and the landing page offers no root either: {roots}"
    );
    assert!(
        roots.contains(ikigai_browse::CAP_PREFIX),
        "…while naming the grant that would: {roots}"
    );

    // 2. The all-roots wildcard: the link is there on an ordinary ledger page, and the
    //    landing page lists the configured root.
    let token = door.enrol_and_sign_in_with(browsing_scopes());
    let (_, page) = door.get_html("/", Some(&token));
    assert!(
        page.contains("href='/browse'"),
        "the header carries the way in: {page}"
    );
    let (status, roots) = door.get_html("/browse", Some(&token));
    assert_eq!(status, 200, "{roots}");
    assert!(
        roots.contains("/browse/urn:repo:demo:tree"),
        "…and the landing page opens on the root's tree: {roots}"
    );

    // 3. A PER-ROOT grant, both ways round.
    let mut named = grants_for("default", Authority::Read).expect("the ledger's tokens");
    named.push(format!("{}demo", ikigai_browse::CAP_PREFIX));
    let token = door.enrol_and_sign_in_as("one-root", named);
    let (_, roots) = door.get_html("/browse", Some(&token));
    assert!(
        roots.contains("/browse/urn:repo:demo:tree"),
        "a grant naming one root offers exactly that root: {roots}"
    );

    let mut elsewhere = grants_for("default", Authority::Read).expect("the ledger's tokens");
    elsewhere.push(format!("{}not-configured-here", ikigai_browse::CAP_PREFIX));
    let token = door.enrol_and_sign_in_as("another-root", elsewhere);
    let (_, page) = door.get_html("/", Some(&token));
    assert!(
        !page.contains("href='/browse'"),
        "a grant naming a root this server does not configure offers nothing: {page}"
    );
}

/// ★★ **An anonymous caller cannot derive**, which is the whole economic boundary of this
/// door: a click on Explain spends inference, and gonk mints no net grant for anybody, let
/// alone for whoever reaches loopback.
///
/// The shell is still served — and says why it is empty, with the sign-in control on the
/// page — because a 403 in plain text would take the only affordance that fixes it away.
/// Enforcement is the kernel's, one hop in, on the row itself.
#[test]
fn an_anonymous_caller_cannot_derive_or_even_read_through_the_adapter() {
    let dir = scratch_root();
    // With a mount, so the explanation families are BOUND: a refusal that came from an
    // unbound resource would prove nothing about the capability.
    let (hub, _watch) = served_explaining(&dir);
    let door = HttpDoorHarness::start(Arc::clone(&hub));

    let (status, page) = door.get_html("/browse/urn:repo:demo:tree", None);
    assert_eq!(status, 200, "{page}");
    assert!(
        !page.contains("hx-get"),
        "a page that cannot read the family must not OFFER to load it: {page}"
    );
    assert!(
        page.contains("urn:cap:browse:read:"),
        "…and must say which grant it is short of: {page}"
    );

    for command in [
        "source urn:repo:demo:tree as=text/html",
        "source urn:repo:demo:file:src/lib.rs as=text/html",
        "source urn:repo:demo:explain:src/lib.rs as=text/html",
        "source urn:repo:demo:explain as=text/html",
    ] {
        let (status, body) = door.get_html(&k(command), None);
        assert_eq!(
            status, 403,
            "`{command}` must be refused anonymously: {body}"
        );
        assert!(
            body.contains("urn:cap:"),
            "…by a typed refusal naming the token: {body}"
        );
    }
}

/// ⚠⚠ The dangerous half of the port. `/k?c=sink …` is a POST that MINTS, and the first arc
/// of this server let any web page POST to loopback 1060 — closed by computing an EMPTY
/// capability for a foreign `Host` or a cross-site `Origin`. This adapter is inside that
/// check, not around it, and the proof is that the same session cookie that mints from this
/// origin writes nothing from another one.
#[test]
fn a_cross_site_post_cannot_annotate_through_the_adapter() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let door = HttpDoorHarness::start(Arc::clone(&hub));
    let token = door.enrol_and_sign_in_with(browsing_scopes());
    let command = k("sink urn:iki:annotation");
    let form = |note: &str| {
        format!(
            "target={}&exact={}&body={}",
            urlencode("urn:repo:demo:file:src/lib.rs"),
            urlencode("the first version"),
            urlencode(note)
        )
    };
    let listed = || {
        text(
            &hub,
            Verb::Source,
            "urn:repo:demo:annotations:src/lib.rs",
            &[],
        )
    };

    // 1. Same origin, signed in: the annotation is minted through browse's own Sink.
    let (status, body) = door.post_form(&command, &form("a note from this page"), Some(&token));
    assert_eq!(status, 200, "{body}");
    assert!(
        listed().contains("a note from this page"),
        "the adapter's write is the family's own write: {}",
        listed()
    );

    // 2. The SAME cookie, from somewhere else: nothing.
    let (status, body) = door.post_form_from(
        &command,
        &form("a note from another site"),
        Some(&token),
        "https://evil.example",
    );
    assert_eq!(
        status, 403,
        "a cross-site write computes an EMPTY capability: {body}"
    );
    assert!(
        !listed().contains("a note from another site"),
        "…and nothing reached the store: {}",
        listed()
    );

    // 3. A rebound `Host` is refused on every method, reads included.
    let (status, body) = door.get_from_host(
        &k("source urn:repo:demo:tree as=text/html"),
        "gonk.evil.example",
    );
    assert_eq!(status, 403, "a foreign Host holds nothing: {body}");
}

/// ⚠ **The reason `web/gonk.js` folds the path spelling, as an assertion rather than a
/// comment.** The affordance browse emits is `/k/source {iri} [k=v …]`, and at this door
/// that is a `400` from the `ikigai-web` library — it percent-decodes the path and then
/// rebuilds a target IRI from it, and a command has spaces in it. The query form is not.
///
/// So both halves are pinned here: the refusal that makes the fold necessary, and the fold
/// itself travelling in the script this server actually serves. If the first assertion ever
/// fails because the path form started working, the fold has become removable and someone
/// should decide whether to remove it.
#[test]
fn the_path_spelling_is_refused_by_this_door_and_the_script_folds_it() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let door = HttpDoorHarness::start(Arc::clone(&hub));
    let token = door.enrol_and_sign_in_with(browsing_scopes());

    let (status, body) = door.get_html(
        "/k/source%20urn:repo:demo:tree%20as=text/html",
        Some(&token),
    );
    assert_eq!(
        status, 400,
        "the path spelling cannot be parsed back into a resource here: {body}"
    );

    let (status, face) = door.get_html(&k("source urn:repo:demo:tree as=text/html"), Some(&token));
    assert_eq!(
        status, 200,
        "…and the query form is the one that works: {face}"
    );

    let (status, script) = door.get_html("/static/gonk.js", Some(&token));
    assert_eq!(status, 200, "{script}");
    assert!(
        script.contains("/k?c=") && script.contains("htmx:configRequest"),
        "the script this server serves is what folds one into the other: {script}"
    );
}

/// The bound on the write surface: an adapter may not widen a door. `source` reaches
/// whatever the caller's capability reaches — which is exactly what the direct route already
/// reaches — but `sink` reaches the annotation family and nothing else, and a verb word that
/// disagrees with the HTTP method is refused rather than reinterpreted.
#[test]
fn the_adapter_sinks_the_annotation_family_and_nothing_else() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let door = HttpDoorHarness::start(Arc::clone(&hub));
    let token = door.enrol_and_sign_in_with(browsing_scopes());

    let (status, body) = door.post_form(
        &k("sink urn:iki:ledger:append"),
        "content=filed%20through%20the%20browse%20door",
        Some(&token),
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("annotation family"), "{body}");

    let (status, body) = door.get_html(&k("sink urn:iki:annotation"), Some(&token));
    assert_eq!(status, 400, "a GET does not carry a sink: {body}");

    let (status, body) = door.post_form(&k("source urn:repo:demo:tree"), "", Some(&token));
    assert_eq!(status, 400, "a POST does not carry a source: {body}");
}

/// Percent-encode everything but the unreserved set, so a query parameter carries an IRI,
/// a space and a newline unchanged.
fn urlencode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The REAL HTTP door over one hub: `doors::http_kernel` with gonk's pages in front,
/// `doors::http_cap`'s per-request capability, and `doors::edge_config`'s route table — the
/// three things `main` wires. Only the listener is a test's (an ephemeral loopback port).
struct HttpDoorHarness {
    addr: SocketAddr,
    layout: quic::Layout,
    _config: TempDir,
}

impl HttpDoorHarness {
    fn start(hub: Arc<Kernel>) -> HttpDoorHarness {
        let runtime = tokio::runtime::Runtime::new().expect("a tokio runtime");
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("a loopback listener");
        let addr = listener.local_addr().expect("a local address");
        let config = tempfile::tempdir().expect("a config home");
        let layout = quic::Layout::in_config_home(config.path());
        let passkeys = Arc::new(Passkeys::new(layout.clone(), addr.port()));
        let face = Arc::new(web::Web {
            hub: Arc::clone(&hub),
            ledgers: vec!["default".to_string()],
            // The one root every fixture here configures — see `served`.
            browse_roots: vec!["demo".to_string()],
            passkeys: Arc::clone(&passkeys),
            rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
            queue: ikigai_gonk::config::QueuePolicy::default(),
        });
        let http = Arc::new(doors::http_kernel(hub, web::space(face)));
        // ★ The anonymous grant this server ships: the configured ledgers, read and write.
        // It does NOT include the browse graph, which is the whole of assertion 1.
        let door = doors::HttpDoor {
            anonymous: grants_for("default", Authority::Write).expect("the ledger's tokens"),
            port: addr.port(),
            passkeys: Some(passkeys),
        };
        std::thread::spawn(move || {
            runtime.block_on(ikigai_web::serve_with_listener(
                http,
                doors::http_cap(door.clone()),
                listener,
                doors::edge_config(door),
            ))
        });
        HttpDoorHarness {
            addr,
            layout,
            _config: config,
        }
    }

    fn origin(&self) -> String {
        format!("http://localhost:{}", self.addr.port())
    }

    fn raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, String) {
        self.raw_to(
            method,
            path,
            &format!("localhost:{}", self.addr.port()),
            headers,
            body,
        )
    }

    /// [`raw`](Self::raw) with the `Host` header spelled out — what a DNS-rebinding attempt
    /// looks like from this side of the socket.
    fn raw_to(
        &self,
        method: &str,
        path: &str,
        host: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(self.addr).expect("connect");
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n");
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ));
        stream.write_all(head.as_bytes()).expect("write head");
        stream.write_all(body.as_bytes()).expect("write body");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read");
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line: {response}"));
        (status, body.to_string())
    }

    /// A GET asking for the store's own result format, with an optional session cookie.
    fn get(&self, path: &str, session: Option<&str>) -> (u16, String) {
        let mut headers = vec![
            ("Accept", "application/sparql-results+json".to_string()),
            ("Sec-Fetch-Site", "none".to_string()),
        ];
        if let Some(session) = session {
            headers.push(("Cookie", format!("{}={session}", identity::SESSION_COOKIE)));
        }
        self.raw("GET", path, &headers, "")
    }

    /// A GET the way a browser asks for a page — Chrome's `Accept`, so the face negotiated
    /// is the one a person would be served.
    fn get_html(&self, path: &str, session: Option<&str>) -> (u16, String) {
        let mut headers = vec![
            (
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string(),
            ),
            ("Sec-Fetch-Site", "same-origin".to_string()),
        ];
        if let Some(session) = session {
            headers.push(("Cookie", format!("{}={session}", identity::SESSION_COOKIE)));
        }
        self.raw("GET", path, &headers, "")
    }

    /// The same read arriving under a `Host` this server does not answer to.
    fn get_from_host(&self, path: &str, host: &str) -> (u16, String) {
        self.raw_to(
            "GET",
            path,
            host,
            &[("Accept", "text/html".to_string())],
            "",
        )
    }

    /// What htmx posts: a form-encoded body, from this origin.
    fn post_form(&self, path: &str, body: &str, session: Option<&str>) -> (u16, String) {
        self.post_form_from(path, body, session, &self.origin())
    }

    /// …and the same post claiming another origin — a page on any site, in a browser on
    /// this machine.
    fn post_form_from(
        &self,
        path: &str,
        body: &str,
        session: Option<&str>,
        origin: &str,
    ) -> (u16, String) {
        let cross = origin != self.origin();
        let mut headers = vec![
            ("Accept", "text/html".to_string()),
            (
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            ),
            ("Origin", origin.to_string()),
            (
                "Sec-Fetch-Site",
                if cross { "cross-site" } else { "same-origin" }.to_string(),
            ),
        ];
        if let Some(session) = session {
            headers.push(("Cookie", format!("{}={session}", identity::SESSION_COOKIE)));
        }
        self.raw("POST", path, &headers, body)
    }

    fn post_json(&self, path: &str, body: &str) -> (u16, String) {
        self.raw(
            "POST",
            path,
            &[
                ("Accept", "application/json".to_string()),
                ("Content-Type", "application/json".to_string()),
                ("Origin", self.origin()),
                ("Sec-Fetch-Site", "same-origin".to_string()),
            ],
            body,
        )
    }

    fn challenge(&self, op: &str) -> String {
        let (status, body) = self.post_json(&format!("/auth/{op}"), "{}");
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).expect("a challenge");
        v["challenge"].as_str().expect("challenge").to_string()
    }

    /// Enrol an identity whose grant is `--ledger default=read --browse-graph read` — the two
    /// halves `main`'s `scopes_for` composes — and sign in. Returns the session token.
    fn enrol_and_sign_in(&self) -> String {
        let mut scopes = grants_for("default", Authority::Read).expect("the ledger's tokens");
        scopes.extend(browse_graph_grants(Authority::Read).expect("a named browse graph"));
        self.enrol_and_sign_in_with(scopes)
    }

    /// The same ceremony under an arbitrary grant — what the browse door's tests need, since
    /// reading a repository and annotating one are scopes this server mints for nobody by
    /// default.
    fn enrol_and_sign_in_with(&self, scopes: Vec<String>) -> String {
        self.enrol_and_sign_in_as("reader", scopes)
    }

    /// ⚠ The same, under a NAMED grant. A grant name is a key in `grants.json`, so enrolling
    /// a second identity in one harness needs a second name — re-using one with different
    /// scopes is refused by design (it would change every identity already under it).
    fn enrol_and_sign_in_as(&self, grant: &str, scopes: Vec<String>) -> String {
        let authenticator = common::Authenticator::named(grant);
        let invite = identity::invite(
            &self.layout,
            grant,
            &scopes,
            false,
            30,
            identity::now_seconds(),
        )
        .expect("an invite");
        let c = self.challenge("register-options");
        let (status, body) = self.post_json(
            "/auth/register",
            &authenticator.register_body(&c, &invite, &self.origin()),
        );
        assert_eq!(status, 200, "{body}");
        let c = self.challenge("login-options");
        let (status, body) = self.post_json(
            "/auth/login",
            &authenticator.login_body(&c, &self.origin(), 1),
        );
        assert_eq!(status, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).expect("a session");
        v["session"].as_str().expect("session").to_string()
    }
}

// ------------------------------------------------- the armed trigger's contract

/// ★★ **THE ANTI-DRIFT DEVICE: gonk's pass and browse's review, read off ONE kernel.**
///
/// This is the test that would have caught the bug this arc found. `ikigai-browse` 0.5.0
/// dropped `urn:cap:annotate` from what `review` requires — a pass writes PENDING findings
/// and cannot publish, which is the entire safety interlock behind arming the git-event
/// trigger (ledger [#466](http://localhost:1060/l/default/item/466)). Two places in this
/// crate went on naming that constant and **compiled clean**, because the constant still
/// exists; only the requirement went away:
///
/// - `trigger::PassEndpoint::describe` would have DENIED the reviewer grant one hop before
///   the review that accepts it, and
/// - `trigger::reviewer_grant_shape` would have told an operator to write the publish token
///   into the reviewer's grant — the interlock gone, silently, with everything working.
///
/// `tests/trigger.rs` cannot catch either: its review is a stub, and a stub is a copy. This
/// one composes REAL browse over a real root with a mount, and asks the kernel for both
/// contracts. The day browse's `requires` moves again, this goes red.
#[test]
fn the_pass_requires_exactly_what_the_real_review_requires() {
    let dir = scratch_root();
    let graph = browse::Graph::chosen();
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let (watch, refused) = RootWatch::start(&roots(&dir));
    assert!(refused.is_empty(), "{refused:?}");
    let tiers = ExplainTiers::default();
    let wired = browse::wire(roots(&dir), handle, watch.watched(), Some(&tiers), &graph);
    let spaces = tempfile::tempdir().expect("a spaces tree");
    let queue = ikigai_gonk::trigger::Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: false,
    };
    ikigai_gonk::trigger::prepare(&queue).expect("the tree");
    let hub = compose_with(
        store,
        Some(Arc::new(wired.space)),
        vec![mount::space(&dead_mount())],
        ikigai_gonk::trigger::space(
            &queue,
            Arc::new(ikigai_gonk::trigger::Activity::default()),
            false,
            ikigai_gonk::config::QueuePolicy::default(),
        ),
        None,
    );

    let review = "urn:repo:demo:review:src/lib.rs".to_string();
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v.dedup();
        v
    };
    let described = |iri: &str| {
        sorted(
            hub.describe(&Iri::parse(iri).expect("an IRI"))
                .unwrap_or_else(|| panic!("`{iri}` describes itself"))
                .requires,
        )
    };
    let pass = described(ikigai_gonk::trigger::PASS);
    let real = described(&review);
    assert_eq!(
        pass, real,
        "the pass must declare EXACTLY what the review it issues declares — more refuses \
         callers browse accepts, less is an over-offer that fails one hop in"
    );
    assert!(
        !real.contains(&ikigai_browse::CAP_ANNOTATE.to_string()),
        "⚠⚠ THE INTERLOCK: browse 0.5.0 stopped a review pass from minting annotations, and \
         that absence is what makes an ARMED headless trigger safe. If this is red, browse \
         can publish again and `gonk.review.arm` must not be on: {real:?}"
    );

    // ★ And the grant an operator is told to write is that contract, mapped from the
    // offering forms to the grant forms — not a list, and never carrying the publish token.
    let shape = ikigai_gonk::trigger::reviewer_grant_shape(&hub, &review, "127.0.0.1")
        .expect("the review is bound");
    assert!(
        !shape.contains(&ikigai_browse::CAP_ANNOTATE.to_string()),
        "a reviewer that may publish is the interlock gone: {shape:?}"
    );
    assert!(
        shape.contains(&"urn:cap:net:127.0.0.1".to_string()),
        "⚠ the NARROW net form, and the host asked for — `Capability::allows` is exact \
         containment, so `localhost` is a different host from `127.0.0.1`: {shape:?}"
    );
    assert!(
        !shape.contains(&"urn:cap:net:*".to_string()),
        "`quic::check_grants` refuses the offering wildcard as a grant: {shape:?}"
    );
    for token in browse_graph_grants(Authority::Write).expect("the browse graph") {
        assert!(
            shape.contains(&token),
            "a pass writes its findings through the store's per-graph door, and a \
             capability travels down unchanged: {token} is missing from {shape:?}"
        );
    }

    // ⚠ And the whole shape actually satisfies the kernel: this is the check `main` runs
    // before it arms, so a shape that passed here and failed there would be a startup
    // refusal on a grant this crate itself derived.
    ikigai_gonk::trigger::check_reviewer(&hub, &review, "127.0.0.1", &shape)
        .expect("the derived shape is the one that arms");
}

// ------------------------------------------------------- the arrangement as a resource

/// `Source urn:kernel:topology` under the one capability it declares, as Turtle text.
fn topology_turtle(hub: &Kernel) -> String {
    let inspect = Capability::scoped(["urn:cap:kernel:inspect"]);
    let repr = block_on(hub.issue(request(Verb::Source, "urn:kernel:topology", &[]), &inspect))
        .expect("the topology renders");
    assert_eq!(repr.repr_type.media_type, "text/turtle");
    String::from_utf8(repr.bytes.to_vec()).expect("Turtle is UTF-8")
}

/// ★ With a browse face composed, `urn:kernel:topology` names the browse family as ONE
/// space — `urn:iki:gonk:space:browse`, an `ik:EndpointSpace` carrying browse's own
/// patterns — and gonk's cacheable overlay in front of it is invisible: it adds no door and
/// changes no name, so it reports the structure it encloses rather than a node of its own
/// (ledger [#546](http://localhost:1060/l/default/item/546)). Nothing in this composition is
/// opaque; `ikigai-repo`'s facades beside it are a plain `ik:EndpointSpace` too.
#[test]
fn the_browse_family_is_one_named_space_and_the_cache_overlay_is_invisible() {
    let dir = scratch_root();
    let (hub, _watch) = served(&dir);
    let turtle = topology_turtle(&hub);

    let browse_node = "<urn:iki:gonk:space:browse> a ik:EndpointSpace";
    let start = turtle
        .find(browse_node)
        .unwrap_or_else(|| panic!("no named browse space in\n{turtle}"));
    let block = &turtle[start..turtle[start..].find(" .\n").expect("a block end") + start];
    assert!(
        block.contains("ik:pattern \"urn:repo:"),
        "the browse space carries browse's patterns:\n{block}"
    );
    assert!(
        turtle.contains("<urn:iki:gonk:space:hub> a ik:Fallback"),
        "the hub is named:\n{turtle}"
    );
    assert!(
        !turtle.contains("ik:OpaqueSpace"),
        "an overlay of this crate is reporting nothing:\n{turtle}"
    );
}

/// ⚠ The one opaque node this server can still render, stated so it is not mistaken for
/// gonk's: a `gonk.mount` is `ikigai-resolve`'s `MountedRemote`, which (through 0.1.28)
/// claims no identity and reports no structure — so it renders as an anonymous
/// `ik:OpaqueSpace`, skolemized, as the hub's FIRST layer (a mount is composed in front of
/// everything local). That is the honest answer for a remote whose arrangement lives in
/// another process; forwarding the peer's own `urn:kernel:topology` is that crate's share
/// of ledger [#546](http://localhost:1060/l/default/item/546), not this one's.
#[test]
fn a_mount_is_the_one_opaque_node_and_it_is_the_hubs_first_layer() {
    let dir = scratch_root();
    let (hub, _watch) = served_explaining(&dir);
    let turtle = topology_turtle(&hub);

    assert_eq!(
        turtle.matches("ik:OpaqueSpace").count(),
        1,
        "exactly the mount is opaque:\n{turtle}"
    );
    // Pre-order skolems, and the two named nodes ahead of it take none: the mount is `_:1`.
    assert!(
        turtle.contains("<urn:iki:gonk:space:hub:layer:1> rdf:first <urn:ikigai:space:_:1>"),
        "the mount is the hub's first layer:\n{turtle}"
    );
    assert!(
        turtle.contains("<urn:ikigai:space:_:1> a ik:OpaqueSpace ."),
        "and it is the opaque one:\n{turtle}"
    );
}
