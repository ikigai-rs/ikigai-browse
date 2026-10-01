//! **Spaces have identity, and the arrangement is a resource** — the paper's §8.2
//! and Theorem 4(b), end to end (`docs/formalism/README.md` §1, "Metadata verb,
//! self-description", and R7.3; ledger #515). One test per claim: a space that
//! names itself is injected under its own name and shares one cache entry across
//! rebuilds; renaming a self-named space at injection is refused; a hit reports the
//! innermost named space on its path and every core combinator forwards the
//! report; a named corridor answers for an anonymous space and the trace says so
//! on every event; `urn:kernel:topology` renders the chain and every core
//! combinator as IRIs with ordered layers, answers the chain it is asked in, and is
//! keyed by it; and the paper's §12.5 check — is the personal family reachable
//! without passing the limiter? — is a walk over that graph, NO with the limiter
//! and YES without it. The walk answers for a FRAGMENT and says so (ledger #552):
//! an alias's visible rules are expanded, so a rule into the family behind the wall
//! is reported as the leak it is; a closure rewrite behind the wall, an opaque
//! space, and a template the prefix test cannot place are "unknown", never "no".
//! And since levels (ledger #563) the walk PUSHES: an endpoint found inside a
//! `Level` resolves from its own level outward, so a door behind a module's guard
//! is reachable through the module's own endpoints, and the walk follows that.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use ikigai_core::{
    Alias, AliasTable, AsyncFnEndpoint, Capability, Confine, Description, EndpointSpace, Error,
    Exact, Fallback, FnEndpoint, Iri, Kernel, Level, Limit, MetaRenderer, Mount, ReprType,
    Representation, Request, Resolution, Rewrite, Scope, Space, SpaceKind, TraceEvent, Tracer,
    UriTemplate, Verb, ANSWERED_NOTE, BINDINGS_THREAD, DENIED_NOTE, LIMITED_NOTE,
};
use oxrdf::{NamedOrBlankNode, Term, Triple};

const IK: &str = "https://ikigai-rs.dev/ns#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

fn iri(s: &str) -> Iri {
    Iri::parse(s).unwrap()
}

fn text(bytes: &[u8]) -> Representation {
    Representation::new(ReprType::new("text/plain"), bytes.to_vec())
}

fn source(target: &str) -> Request {
    Request::new(Verb::Source, iri(target))
}

/// A cacheable constant with a description id of its own.
fn door(id: &'static str) -> FnEndpoint {
    FnEndpoint::new(id, move |_inv| Ok(text(id.as_bytes()).cacheable()))
        .with_description(Description::new(id).verb(Verb::Source))
}

/// A door that demands a scope, so a denial can be traced.
fn gated(id: &'static str, scope: &'static str) -> FnEndpoint {
    FnEndpoint::new(id, move |_inv| Ok(text(id.as_bytes()).cacheable()))
        .with_description(Description::new(id).verb(Verb::Source).requires(scope))
}

/// Renders a description as its id.
struct IdRenderer;

impl MetaRenderer for IdRenderer {
    fn render(
        &self,
        description: &Description,
        _target: &ReprType,
    ) -> ikigai_core::Result<Representation> {
        Ok(text(description.id.as_bytes()))
    }
}

fn kernel(root: Arc<dyn Space>) -> Kernel {
    Kernel::with_meta_renderer(root, Arc::new(IdRenderer))
}

#[derive(Default)]
struct Collect(Mutex<Vec<TraceEvent>>);
impl Tracer for Collect {
    fn record(&self, event: TraceEvent) {
        self.0.lock().unwrap().push(event);
    }
}

fn note<'a>(event: &'a TraceEvent, key: &str) -> Option<&'a str> {
    event
        .notes
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// A space that says nothing about itself: the default `topology()`.
struct Foreign(Arc<dyn Space>);
impl Space for Foreign {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        self.0.resolve(request, scope)
    }
}

// ---- 1. Identity on the space ------------------------------------------------

#[test]
fn a_space_that_names_itself_is_injected_under_its_own_name_and_shares_one_cache_entry() {
    let name = "urn:example:ctx:time:2026-09-25T18:00Z";
    let pinned = |named: bool| -> Arc<dyn Space> {
        let space = EndpointSpace::new().bind(Exact::new("urn:time:now"), door("pinned"));
        Arc::new(if named { space.named(iri(name)) } else { space })
    };
    assert_eq!(pinned(true).id().unwrap().as_str(), name);
    assert!(
        pinned(false).id().is_none(),
        "a space is anonymous unless it says otherwise"
    );

    let kernel = kernel(Arc::new(EndpointSpace::new()));
    let cap = Capability::root();
    let now = || source("urn:time:now");

    // Two requests, two freshly built self-named spaces, injected with `with`:
    // ONE entry, and the chain renders under the space's own name.
    let scope = || Scope::empty().with(pinned(true));
    assert_eq!(scope().to_string(), format!("{name} root"));
    block_on(kernel.issue_in(now(), &cap, scope())).unwrap();
    block_on(kernel.issue_in(now(), &cap, scope())).unwrap();
    assert_eq!(kernel.cache_len(), 1);
    assert!(kernel.is_cached_in(&now(), &cap, &scope()));

    // The same two requests through anonymous spaces: an entry each, as before.
    block_on(kernel.issue_in(now(), &cap, Scope::empty().with(pinned(false)))).unwrap();
    block_on(kernel.issue_in(now(), &cap, Scope::empty().with(pinned(false)))).unwrap();
    assert_eq!(kernel.cache_len(), 3);

    // `with_named` agreeing with the space's own name is the same corridor.
    block_on(kernel.issue_in(
        now(),
        &cap,
        Scope::empty().with_named(iri(name), pinned(true)),
    ))
    .unwrap();
    assert_eq!(kernel.cache_len(), 3);
}

#[test]
#[should_panic(expected = "a self-named space is injected under its own identity")]
fn renaming_a_self_named_space_at_injection_is_refused() {
    // The two-partitions bug, reintroduced on purpose: refused rather than
    // resolved either way (the space's name would drop what the injector's
    // carried; the injector's would put one set of doors under two names).
    let space: Arc<dyn Space> = Arc::new(EndpointSpace::new().named(iri("urn:example:space:a")));
    let _ = Scope::empty().with_named(iri("urn:example:space:b"), space);
}

#[test]
#[should_panic(expected = "a self-named space is injected under its own identity")]
fn confining_to_a_self_named_space_under_another_name_is_refused() {
    let space: Arc<dyn Space> = Arc::new(EndpointSpace::new().named(iri("urn:example:space:a")));
    let _ = Confine::new(iri("urn:example:space:b"), space, Arc::new(door("x")));
}

#[test]
fn every_core_combinator_can_be_named_and_reports_its_name() {
    let leaf = || -> Arc<dyn Space> { Arc::new(EndpointSpace::new()) };
    let named: Vec<(&str, Arc<dyn Space>)> = vec![
        (
            "EndpointSpace",
            Arc::new(EndpointSpace::new().named(iri("urn:example:s:1"))),
        ),
        (
            "Fallback",
            Arc::new(Fallback::new(vec![leaf()]).named(iri("urn:example:s:2"))),
        ),
        (
            "Mount",
            Arc::new(Mount::new("urn:x:", leaf()).named(iri("urn:example:s:3"))),
        ),
        (
            "Rewrite",
            Arc::new(Rewrite::new(leaf(), |_| None).named(iri("urn:example:s:4"))),
        ),
        (
            "Alias",
            Arc::new(Alias::new(Arc::new(AliasTable::new()), leaf()).named(iri("urn:example:s:5"))),
        ),
        (
            "Limit",
            Arc::new(Limit::new("urn:x:").named(iri("urn:example:s:6"))),
        ),
    ];
    for (i, (label, space)) in named.iter().enumerate() {
        assert_eq!(
            space.id().map(|id| id.as_str().to_string()),
            Some(format!("urn:example:s:{}", i + 1)),
            "{label} does not report the name it was given"
        );
        assert_eq!(
            space.topology().id,
            space.id(),
            "{label}'s node is not named by its id"
        );
    }
    // …and the blanket impl forwards it through an `Arc`.
    let erased: Arc<dyn Space> = Arc::clone(&named[0].1);
    assert_eq!(Arc::new(erased).id().unwrap().as_str(), "urn:example:s:1");
}

// ---- 2. Who answered -----------------------------------------------------------

#[test]
fn a_hit_reports_the_innermost_named_space_and_every_combinator_forwards_it() {
    let personal = || -> Arc<dyn Space> {
        Arc::new(
            EndpointSpace::new()
                .bind(Exact::new("urn:personal:x"), door("personal-x"))
                .named(iri("urn:example:space:personal")),
        )
    };
    let public = || -> Arc<dyn Space> {
        Arc::new(EndpointSpace::new().bind(Exact::new("urn:public:y"), door("public-y")))
    };
    let answered = |space: &dyn Space, target: &str| -> Option<String> {
        match space.resolve(&source(target), &Scope::empty()) {
            Resolution::Hit(hit) => hit.answered_by.map(|id| id.as_str().to_string()),
            Resolution::Miss => panic!("{target} missed"),
        }
    };
    let s = |x: &str| Some(x.to_string());

    // A leaf reports itself; an anonymous leaf reports nothing.
    assert_eq!(
        answered(&*personal(), "urn:personal:x"),
        s("urn:example:space:personal")
    );
    assert_eq!(answered(&*public(), "urn:public:y"), None);

    // A named combinator FILLS an absence and never overwrites an inner report.
    let root = Fallback::new(vec![
        Arc::new(Mount::new("urn:personal:", personal()).named(iri("urn:example:space:m1"))),
        Arc::new(Mount::new("urn:public:", public()).named(iri("urn:example:space:m2"))),
    ])
    .named(iri("urn:example:space:root"));
    assert_eq!(
        answered(&root, "urn:personal:x"),
        s("urn:example:space:personal")
    );
    assert_eq!(answered(&root, "urn:public:y"), s("urn:example:space:m2"));

    // The same rule through every combinator that rewrites or wraps.
    let rewrite = |inner: Arc<dyn Space>| {
        Rewrite::new(inner, |t: &Iri| {
            t.as_str()
                .strip_prefix("urn:alias:")
                .map(|rest| iri(&format!("urn:personal:{rest}")))
        })
        .named(iri("urn:example:space:rewrite"))
    };
    assert_eq!(
        answered(&rewrite(personal()), "urn:alias:x"),
        s("urn:example:space:personal")
    );
    let anonymous_personal: Arc<dyn Space> =
        Arc::new(EndpointSpace::new().bind(Exact::new("urn:personal:x"), door("personal-x")));
    assert_eq!(
        answered(&rewrite(Arc::clone(&anonymous_personal)), "urn:alias:x"),
        s("urn:example:space:rewrite")
    );
    let table = Arc::new(AliasTable::new().prefix("urn:alias:", "urn:personal:"));
    let alias = |inner: Arc<dyn Space>| {
        Alias::new(Arc::clone(&table), inner).named(iri("urn:example:space:alias"))
    };
    assert_eq!(
        answered(&alias(personal()), "urn:alias:x"),
        s("urn:example:space:personal")
    );
    assert_eq!(
        answered(&alias(Arc::clone(&anonymous_personal)), "urn:alias:x"),
        s("urn:example:space:alias")
    );
    // A named limiter names itself on the hit on ⊥.
    let limited = Fallback::new(vec![
        Arc::new(Limit::new("urn:personal:").named(iri("urn:example:space:limit"))),
        personal(),
    ]);
    match limited.resolve(&source("urn:personal:x"), &Scope::empty()) {
        Resolution::Hit(hit) => {
            assert!(hit.endpoint.is_limiter());
            assert_eq!(hit.answered_by.unwrap().as_str(), "urn:example:space:limit");
        }
        Resolution::Miss => panic!("in the family"),
    }
    // Decoration keeps it, as it keeps the canonical.
    let decorated = personal()
        .resolve(&source("urn:personal:x"), &Scope::empty())
        .map_endpoint(|endpoint| endpoint);
    match decorated {
        Resolution::Hit(hit) => {
            assert_eq!(
                hit.answered_by.as_ref().unwrap().as_str(),
                "urn:example:space:personal"
            );
            let swapped = hit.with_endpoint(Arc::new(door("other")));
            assert_eq!(
                swapped.answered_by.unwrap().as_str(),
                "urn:example:space:personal"
            );
        }
        Resolution::Miss => unreachable!(),
    }
}

#[test]
fn a_named_corridor_answers_for_an_anonymous_space_and_every_traced_event_says_who() {
    // Root: a named leaf with an open door and a gated one; an anonymous corridor
    // shadowing nothing but binding its own name.
    let root: Arc<dyn Space> = Arc::new(
        Fallback::new(vec![
            Arc::new(Limit::new("urn:hole:").named(iri("urn:example:space:hole"))),
            Arc::new(
                EndpointSpace::new()
                    .bind(Exact::new("urn:open"), door("open"))
                    .bind(
                        Exact::new("urn:gated"),
                        gated("gated", "urn:cap:example:gated"),
                    )
                    .named(iri("urn:example:space:root")),
            ),
        ])
        .named(iri("urn:example:space:top")),
    );
    let corridor: Arc<dyn Space> =
        Arc::new(EndpointSpace::new().bind(Exact::new("urn:ctx:doc"), door("doc")));
    let kernel = kernel(root);
    let tracer = Arc::new(Collect::default());
    kernel.set_tracer(tracer.clone());
    let cap = Capability::root();
    let chain = || Scope::empty().with_named(iri("urn:example:ctx:7"), Arc::clone(&corridor));

    // A hit through the corridor names the corridor; through the root, the
    // innermost named space (the leaf, not the fallback around it).
    block_on(kernel.issue_in(source("urn:ctx:doc"), &cap, chain())).unwrap();
    block_on(kernel.issue_in(source("urn:open"), &cap, chain())).unwrap();
    // A cache hit carries it too.
    block_on(kernel.issue_in(source("urn:open"), &cap, chain())).unwrap();
    // A denial names the space whose door refused…
    let denied = block_on(kernel.issue(
        source("urn:gated"),
        &Capability::scoped(Vec::<String>::new()),
    ))
    .unwrap_err();
    assert!(matches!(denied, Error::Denied(_)));
    // …and a hit on a named limiter names the limiter.
    let limited = block_on(kernel.issue(source("urn:hole:x"), &cap)).unwrap_err();
    assert!(matches!(limited, Error::Unresolved(_)));

    let events = tracer.0.lock().unwrap();
    let by_target = |target: &str| -> Vec<&TraceEvent> {
        events.iter().filter(|e| e.target == target).collect()
    };
    assert_eq!(
        note(by_target("urn:ctx:doc")[0], ANSWERED_NOTE),
        Some("urn:example:ctx:7"),
        "{events:?}"
    );
    let open = by_target("urn:open");
    assert_eq!(open.len(), 2);
    assert!(!open[0].cache_hit && open[1].cache_hit);
    for event in &open {
        assert_eq!(note(event, ANSWERED_NOTE), Some("urn:example:space:root"));
    }
    let denied = by_target("urn:gated");
    assert_eq!(note(denied[0], DENIED_NOTE), Some("urn:cap:example:gated"));
    assert_eq!(
        note(denied[0], ANSWERED_NOTE),
        Some("urn:example:space:root")
    );
    let limited = by_target("urn:hole:x");
    assert_eq!(note(limited[0], LIMITED_NOTE), Some("urn:hole:x"));
    assert_eq!(
        note(limited[0], ANSWERED_NOTE),
        Some("urn:example:space:hole")
    );
    drop(events);

    // And a kernel whose spaces claim nothing records no such note at all.
    let plain = Kernel::new(Arc::new(
        EndpointSpace::new().bind(Exact::new("urn:open"), door("open")),
    ));
    let quiet = Arc::new(Collect::default());
    plain.set_tracer(quiet.clone());
    block_on(plain.issue(source("urn:open"), &cap)).unwrap();
    block_on(plain.issue_in(
        source("urn:open"),
        &cap,
        Scope::empty().with(Arc::clone(&corridor)),
    ))
    .unwrap();
    for event in quiet.0.lock().unwrap().iter() {
        assert_eq!(note(event, ANSWERED_NOTE), None, "{event:?}");
    }
}

// ---- 3. The arrangement as a resource ----------------------------------------

/// A parsed topology graph, walked by IRI.
struct Graph(Vec<Triple>);

impl Graph {
    fn parse(turtle: &str) -> Graph {
        let triples: Vec<Triple> = oxttl::TurtleParser::new()
            .for_reader(turtle.as_bytes())
            .map(|t| t.unwrap_or_else(|e| panic!("{e}\n{turtle}")))
            .collect();
        for t in &triples {
            assert!(
                matches!(t.subject, NamedOrBlankNode::NamedNode(_))
                    && !matches!(t.object, Term::BlankNode(_)),
                "a blank node in the topology: {t}"
            );
        }
        Graph(triples)
    }

    fn objects(&self, s: &str, p: &str) -> Vec<&Term> {
        self.0
            .iter()
            .filter(|t| matches!(&t.subject, NamedOrBlankNode::NamedNode(n) if n.as_str() == s))
            .filter(|t| t.predicate.as_str() == p)
            .map(|t| &t.object)
            .collect()
    }

    fn iri(&self, s: &str, p: &str) -> Option<String> {
        self.objects(s, p).first().map(|o| match o {
            Term::NamedNode(n) => n.as_str().to_string(),
            other => panic!("{s} {p} is not an IRI: {other}"),
        })
    }

    fn strs(&self, s: &str, p: &str) -> Vec<String> {
        self.objects(s, p)
            .into_iter()
            .map(|o| match o {
                Term::Literal(l) => l.value().to_string(),
                other => panic!("{s} {p} is not a literal: {other}"),
            })
            .collect()
    }

    fn kind(&self, s: &str) -> String {
        let types = self
            .iri(s, &format!("{RDF}type"))
            .unwrap_or_else(|| panic!("{s} has no type"));
        types.strip_prefix(IK).expect("an ik: kind").to_string()
    }

    /// The members of `s`'s `ik:layers`, in list order.
    fn layers(&self, s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cell = self
            .iri(s, &format!("{IK}layers"))
            .unwrap_or_else(|| panic!("{s} has no layers"));
        while cell != format!("{RDF}nil") {
            out.push(self.iri(&cell, &format!("{RDF}first")).expect("rdf:first"));
            cell = self.iri(&cell, &format!("{RDF}rest")).expect("rdf:rest");
        }
        out
    }

    fn space(&self, s: &str) -> String {
        self.iri(s, &format!("{IK}space"))
            .unwrap_or_else(|| panic!("{s} encloses nothing"))
    }

    fn subjects(&self) -> BTreeSet<String> {
        self.0
            .iter()
            .map(|t| match &t.subject {
                NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
                _ => unreachable!(),
            })
            .collect()
    }
}

fn topology(kernel: &Kernel, cap: &Capability, scope: Scope) -> Graph {
    let rep = block_on(kernel.issue_in(source("urn:kernel:topology"), cap, scope)).unwrap();
    assert_eq!(rep.repr_type.media_type, "text/turtle");
    Graph::parse(std::str::from_utf8(&rep.bytes).unwrap())
}

#[test]
fn the_topology_renders_the_chain_and_every_core_combinator_as_iris_with_ordered_layers() {
    let table = Arc::new(
        AliasTable::new()
            .prefix("urn:alias:", "urn:personal:")
            .exact("urn:old", "urn:public:y"),
    );
    let root: Arc<dyn Space> = Arc::new(
        Fallback::new(vec![
            Arc::new(Limit::matching(
                UriTemplate::parse("urn:doc:{id}:secret").unwrap(),
            )),
            Arc::new(Mount::new(
                "urn:personal:",
                Arc::new(
                    EndpointSpace::new()
                        .bind(Exact::new("urn:personal:x"), door("personal-x"))
                        .bind(
                            UriTemplate::parse("urn:personal:doc/{id}").unwrap(),
                            door("personal-doc"),
                        )
                        .named(iri("urn:example:space:personal")),
                ),
            )),
            Arc::new(Alias::new(
                Arc::clone(&table),
                Arc::new(Rewrite::new(
                    Arc::new(Foreign(Arc::new(EndpointSpace::new()))),
                    |_| None,
                )),
            )),
        ])
        .named(iri("urn:example:space:root")),
    );
    let kernel = kernel(Arc::clone(&root));
    let cap = Capability::root();

    // The empty chain: one layer, the root, under its own name.
    let g = topology(&kernel, &cap, Scope::empty());
    assert_eq!(g.kind("urn:ikigai:chain:root"), "Chain");
    assert_eq!(
        g.strs("urn:ikigai:chain:root", &format!("{IK}severed")),
        ["false"]
    );
    assert_eq!(
        g.layers("urn:ikigai:chain:root"),
        ["urn:example:space:root"]
    );
    // The root fallback: three layers in consultation order, anonymous ones
    // skolemized in pre-order.
    assert_eq!(g.kind("urn:example:space:root"), "Fallback");
    let layers = g.layers("urn:example:space:root");
    assert_eq!(
        layers,
        [
            "urn:ikigai:space:_:1",
            "urn:ikigai:space:_:2",
            "urn:ikigai:space:_:3"
        ]
    );
    assert_eq!(g.kind(&layers[0]), "Limit");
    assert_eq!(
        g.strs(&layers[0], &format!("{IK}family")),
        ["urn:doc:{id}:secret"]
    );
    assert_eq!(g.kind(&layers[1]), "Mount");
    assert_eq!(
        g.strs(&layers[1], &format!("{IK}prefix")),
        ["urn:personal:"]
    );
    assert_eq!(g.space(&layers[1]), "urn:example:space:personal");
    assert_eq!(g.kind("urn:example:space:personal"), "EndpointSpace");
    let mut patterns = g.strs("urn:example:space:personal", &format!("{IK}pattern"));
    patterns.sort();
    assert_eq!(patterns, ["urn:personal:doc/{id}", "urn:personal:x"]);
    // The alias carries its table; the rewrite encloses and says nothing of τ; the
    // foreign space is opaque — the graph says where knowledge stops.
    assert_eq!(g.kind(&layers[2]), "Alias");
    let rules: Vec<String> = g
        .objects(&layers[2], &format!("{IK}rewrites"))
        .iter()
        .map(|o| o.to_string())
        .collect();
    assert_eq!(
        rules,
        [
            "<urn:ikigai:space:_:3:rule:1>",
            "<urn:ikigai:space:_:3:rule:2>"
        ]
    );
    assert_eq!(g.kind("urn:ikigai:space:_:3:rule:1"), "RewriteRule");
    assert_eq!(
        g.strs("urn:ikigai:space:_:3:rule:1", &format!("{IK}ruleKind")),
        ["prefix"]
    );
    assert_eq!(
        g.strs("urn:ikigai:space:_:3:rule:1", &format!("{IK}logical")),
        ["urn:alias:"]
    );
    assert_eq!(
        g.strs("urn:ikigai:space:_:3:rule:1", &format!("{IK}canonical")),
        ["urn:personal:"]
    );
    assert_eq!(
        g.strs("urn:ikigai:space:_:3:rule:2", &format!("{IK}ruleKind")),
        ["exact"]
    );
    let rewrite = g.space(&layers[2]);
    assert_eq!(g.kind(&rewrite), "Rewrite");
    assert_eq!(g.kind(&g.space(&rewrite)), "OpaqueSpace");
    // The Rust face is the same tree.
    let tree = kernel.topology();
    assert!(matches!(tree.kind, SpaceKind::Chain { severed: false }));
    assert_eq!(tree.children.len(), 1);
    assert!(matches!(tree.children[0].kind, SpaceKind::Fallback));
    assert_eq!(tree.children[0].children.len(), 3);

    // Inside a chain, the resource answers THE CHAIN: a named corridor first,
    // an anonymous one skolemized, then the root — and severed, no root.
    let corridor = |name: Option<&str>| -> Arc<dyn Space> {
        let space = EndpointSpace::new().bind(Exact::new("urn:ctx:doc"), door("doc"));
        Arc::new(match name {
            Some(name) => space.named(iri(name)),
            None => space,
        })
    };
    let chain = Scope::empty()
        .with(corridor(None))
        .with_named(iri("urn:example:ctx:7"), corridor(None));
    let g = topology(&kernel, &cap, chain.clone());
    let entry = format!("urn:ikigai:chain:{:016x}", chain.fingerprint());
    assert_eq!(g.kind(&entry), "Chain");
    assert_eq!(
        g.layers(&entry),
        [
            "urn:example:ctx:7",
            "urn:ikigai:space:_:1",
            "urn:example:space:root"
        ]
    );
    assert_eq!(g.kind("urn:example:ctx:7"), "EndpointSpace");
    let severed = chain.clone().sever();
    let g = topology(&kernel, &cap, severed.clone());
    let entry = format!("urn:ikigai:chain:{:016x}", severed.fingerprint());
    assert_eq!(g.strs(&entry, &format!("{IK}severed")), ["true"]);
    assert_eq!(
        g.layers(&entry),
        ["urn:example:ctx:7", "urn:ikigai:space:_:1"]
    );
    assert!(
        !g.subjects().contains("urn:example:space:root"),
        "a severed chain has no root"
    );

    // Keyed by the chain (three chains, three entries), hanging from the bindings
    // thread (a cut forgets all three), and gated like the catalog.
    assert_eq!(kernel.cache_len(), 3);
    assert!(kernel.is_cached_in(&source("urn:kernel:topology"), &cap, &chain));
    assert!(kernel.is_cached_in(&source("urn:kernel:topology"), &cap, &Scope::empty()));
    kernel.cut(BINDINGS_THREAD);
    assert!(!kernel.is_cached_in(&source("urn:kernel:topology"), &cap, &chain));
    assert!(!kernel.is_cached_in(&source("urn:kernel:topology"), &cap, &Scope::empty()));
    let denied = block_on(kernel.issue(
        source("urn:kernel:topology"),
        &Capability::scoped(["urn:cap:kernel:cut"]),
    ))
    .unwrap_err();
    assert!(matches!(denied, Error::Denied(_)));
    assert!(block_on(kernel.issue(
        source("urn:kernel:topology"),
        &Capability::scoped(["urn:cap:kernel:inspect"]),
    ))
    .is_ok());
}

/// The answer the gatekeeper check gives. Three-valued, because the graph can hold
/// things the check does not evaluate — R7.3's fragment: an `ik:OpaqueSpace`, a
/// closure `ik:Rewrite` behind a wall over the family, a template door whose `{`
/// falls inside the family's length, a template family on a limiter that touches
/// it. `Unknown` is the check saying so rather than answering.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Reach {
    Reachable,
    Unreachable,
    Unknown,
}

/// Where a pattern's literal head — the text before the first `{` — stands relative
/// to the family. The check does not evaluate templates; it places them by their
/// head, which is sound in two of the three cases: a head inside the family means
/// every expansion is (`urn:personal:doc/{id}`); a head that diverges from it means
/// no expansion can start with it (`urn:file:{path}`); a head the family extends
/// (`urn:{ns}:inbox` against `urn:personal:`) may or may not, and that is the case
/// the check does not answer.
#[derive(Debug, PartialEq, Eq)]
enum Head {
    Inside,
    Outside,
    Astride,
}

fn head(text: &str, family: &str) -> Head {
    let (lit, template) = match text.find('{') {
        Some(at) => (&text[..at], true),
        None => (text, false),
    };
    if lit.starts_with(family) {
        Head::Inside
    } else if template && family.starts_with(lit) {
        Head::Astride
    } else {
        Head::Outside
    }
}

/// Two prefixes share names: one extends the other.
fn touches(a: &str, b: &str) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// **Theorem 4(b) as a walk** — the formal document's §1.1 and R7.3. The walk
/// follows the order the kernel consults the tree, because that order IS the rule:
/// `Fallback` returns the first hit and a hit on ⊥ is a hit, so a plain limiter met
/// earlier in pre-order — on a path that admits the name — ends resolution for every
/// later door under its family, whichever list that door sits in. `walls`
/// accumulates those families as the walk meets them, narrowed to the mount the
/// limiter sits under (`gate`: a limiter stops only what its mount admits, and one
/// under a mount that admits none of the family is dead). A limiter met inside a
/// mapper (`Rewrite`, `Alias`) walls only the mapper's own subtree: the closure or
/// the table may rename the family away before it, so exporting it would wall doors
/// it never stops. `maybe` is set by a template family the check cannot place,
/// after which a door found is at best "unknown".
///
/// **Levels push.** A tree without an `ik:Level` contributes no pushes, and the walk
/// is the path query it always was. An endpoint found inside a level runs in its
/// resolved scope — the host's corridors, then its level's space WITHOUT the guard it
/// was entered through, then each enclosing level, then the root — so a level whose
/// doors an outside request can reach is ENTERED (`entered`, with its enclosing
/// levels), and every entered level is PUSHED: its space and its enclosing levels'
/// spaces are walked again from the level itself, with no gate and only the walls
/// the host's corridors put ahead of everything (`corridor_walls`). Pushes discover
/// further entered levels; each stack is pushed once. The stacks are the tree's own
/// level paths, so the closure is bounded by the level nesting. The root is not
/// walked again from a push: it is consulted after the frames, behind at least the
/// walls the entry walk met, so it can only reach less.
///
/// **Sealed families** have exactly today's reach: a family a host seals
/// (`host_sealed`, from `Kernel::sealed()`) is never answered inside a level, and one
/// a level seals (`ik:seals`) only inside that level — so a door of a sealed family
/// counts only where its owner is. Doors met through a non-owner frame on the way to
/// a nested owner are counted (an over-approximation, in the safe direction).
///
/// The SPARQL form of the same question — two queries — is in
/// `docs/formalism/README.md` (R7.3). It follows no pushes, and its second question
/// says so: an `ik:Level` on a reachable path makes the first answer untrusted.
#[derive(Default)]
struct Walk {
    walls: Vec<String>,
    maybe: bool,
    reachable: bool,
    unknown: bool,
    /// The levels enclosing the node being walked, innermost LAST.
    stack: Vec<String>,
    /// Every entered level's stack, innermost FIRST — the resolved scope's level
    /// stack an endpoint found in it runs with.
    entered: Vec<Vec<String>>,
    /// The walls the chain's host corridors put ahead of the root, captured as the
    /// entry walk passes them.
    corridor_walls: Vec<String>,
    /// Who seals the family: `Some(None)` the host, `Some(Some(level))` a level,
    /// `None` nobody.
    sealed: Option<Option<String>>,
}

impl Walk {
    fn walk(&mut self, g: &Graph, node: &str, family: &str, gate: &str) {
        let p = |name: &str| format!("{IK}{name}");
        match g.kind(node).as_str() {
            "Chain" | "Fallback" => {
                let chain = g.kind(node) == "Chain";
                let layers = g.layers(node);
                let root_at = (chain && g.strs(node, &p("severed")) != ["true"])
                    .then(|| layers.len().saturating_sub(1));
                let mut captured = false;
                for (at, layer) in layers.iter().enumerate() {
                    // The host's corridors come first in a chain, then (in a resolved
                    // scope) its level stack, then the root: a push starts behind the
                    // corridors' walls and nothing else.
                    if chain && !captured && (Some(at) == root_at || g.kind(layer) == "Level") {
                        self.corridor_walls = self.walls.clone();
                        captured = true;
                    }
                    if g.kind(layer) == "Limit" {
                        for limited in g.strs(layer, &p("family")) {
                            match limited.find('{') {
                                // A plain family is a prefix (`Limit::new`): a wall over
                                // everything under it that the mount above admits.
                                None if limited.starts_with(gate) => self.walls.push(limited),
                                None if gate.starts_with(&limited) => {
                                    self.walls.push(gate.to_string())
                                }
                                None => {} // dead: the mount above admits none of it
                                // A template family (`Limit::matching`) is not evaluated:
                                // one whose head touches the family may wall part of it.
                                Some(at) if touches(&limited[..at], family) => {
                                    self.maybe = true;
                                }
                                Some(_) => {}
                            }
                        }
                    } else {
                        self.walk(g, layer, family, gate);
                    }
                }
                if chain && !captured {
                    self.corridor_walls = self.walls.clone();
                }
            }
            "Mount" => {
                let prefix = g.strs(node, &p("prefix")).remove(0);
                // The mount admits nothing the mount above it admits: nothing below
                // it is reached. (Whether it admits the FAMILY is asked at each door,
                // because a level below it may be entered by a name outside the family.)
                if !touches(&prefix, gate) {
                    return;
                }
                let gate = if prefix.starts_with(gate) {
                    prefix
                } else {
                    gate.to_string()
                };
                self.walk(g, &g.space(node), family, &gate);
            }
            "EndpointSpace" => {
                for door in g.strs(node, &p("pattern")) {
                    let lit = door.split('{').next().unwrap_or(&door);
                    if !touches(lit, gate) {
                        continue; // the mount above never admits a name of this door
                    }
                    // Any door an outside name can reach enters the level it is in.
                    if !self.walls.iter().any(|w| lit.starts_with(w)) {
                        self.enter();
                    }
                    if !self.counts_here() {
                        continue; // a sealed family is answered only where its owner is
                    }
                    match head(&door, family) {
                        Head::Inside if !self.walls.iter().any(|w| door.starts_with(w)) => {
                            if self.maybe {
                                self.unknown = true;
                            } else {
                                self.reachable = true;
                            }
                        }
                        Head::Astride if !self.walls.iter().any(|w| family.starts_with(w)) => {
                            self.unknown = true;
                        }
                        _ => {}
                    }
                }
            }
            // A level is transparent from outside; it is ENTERED when a door inside it
            // is reached, and the push that entering implies is taken in `check_walk`.
            "Level" => {
                self.stack.push(node.to_string());
                self.walk(g, &g.space(node), family, gate);
                self.stack.pop();
            }
            // τ is a closure. Behind a wall that touches the family it may map an
            // admitted name into the family — the §12.5 leak, invisible here — so the
            // walk cannot answer. Above every such wall it is walked through: τ can
            // only choose among the doors the enclosed space has.
            "Rewrite" => {
                if self.walls.iter().any(|w| touches(w, family)) {
                    self.unknown = true;
                    self.enter(); // it may hold a door, and so enter its level
                } else {
                    self.scoped(g, &g.space(node), family, gate);
                }
            }
            // A table is visible, so it is EXPANDED. A rule whose canonical is
            // prefix-related to the family maps into it; the family's names as the
            // table admits them are `logical ++ family[|canonical|..]`; if no wall
            // ahead is a prefix of those, and the mount above admits them, the
            // canonical is followed inside the enclosed space with the walls the
            // logical name passed left behind. Then the enclosed space is walked as
            // it is, for the names the table passes through unchanged.
            "Alias" => {
                let inner = g.space(node);
                for rule in g.objects(node, &p("rewrites")) {
                    let rule = match rule {
                        Term::NamedNode(n) => n.as_str().to_string(),
                        other => panic!("{node} rewrites a non-IRI: {other}"),
                    };
                    let logical = g.strs(&rule, &p("logical")).remove(0);
                    let canonical = g.strs(&rule, &p("canonical")).remove(0);
                    let (sub, under) = if canonical.starts_with(family) {
                        (canonical.clone(), logical)
                    } else if family.starts_with(&canonical) {
                        (
                            family.to_string(),
                            format!("{logical}{}", &family[canonical.len()..]),
                        )
                    } else {
                        continue; // the rule maps nothing into the family
                    };
                    if !touches(&under, gate) || self.walls.iter().any(|w| under.starts_with(w)) {
                        continue; // the admitted names never reach the alias
                    }
                    let mut followed = Walk {
                        maybe: self.maybe,
                        stack: self.stack.clone(),
                        sealed: self.sealed.clone(),
                        ..Walk::default()
                    };
                    followed.walk(g, &inner, &sub, "");
                    self.absorb(followed);
                }
                self.scoped(g, &inner, family, gate);
            }
            // A limiter reached on its own admits nothing; an opaque space is exactly
            // that — it may hold a door, or be a mapper, and the walk cannot see. It
            // may also enter the level it is in.
            "Limit" => {}
            "OpaqueSpace" => {
                self.unknown = true;
                self.enter();
            }
            "Confine" => self.walk(g, &g.space(node), family, gate),
            other => panic!("unknown node kind {other}"),
        }
    }

    /// Record that the level being walked (if any) is entered.
    fn enter(&mut self) {
        if self.stack.is_empty() {
            return;
        }
        let frames: Vec<String> = self.stack.iter().rev().cloned().collect();
        if !self.entered.contains(&frames) {
            self.entered.push(frames);
        }
    }

    /// Whether a door of the family counts where the walk is: everywhere unless the
    /// family is sealed; outside every level for a host seal; inside the owning
    /// level for a level's.
    fn counts_here(&self) -> bool {
        match &self.sealed {
            None => true,
            Some(None) => self.stack.is_empty(),
            Some(Some(owner)) => self.stack.last() == Some(owner),
        }
    }

    /// Fold a sub-walk's findings into this one.
    fn absorb(&mut self, other: Walk) {
        self.reachable |= other.reachable;
        self.unknown |= other.unknown;
        for frames in other.entered {
            if !self.entered.contains(&frames) {
                self.entered.push(frames);
            }
        }
    }

    /// Walk `node` with this walk's walls, keeping any it meets inside to itself.
    fn scoped(&mut self, g: &Graph, node: &str, family: &str, gate: &str) {
        let mut inner = Walk {
            walls: self.walls.clone(),
            maybe: self.maybe,
            stack: self.stack.clone(),
            sealed: self.sealed.clone(),
            ..Walk::default()
        };
        inner.walk(g, node, family, gate);
        self.absorb(inner);
    }
}

/// Who seals `family`, if anyone: the host (from `host_sealed`) or a level
/// (`ik:seals` in the graph).
fn sealer(g: &Graph, family: &str, host_sealed: &[&str]) -> Option<Option<String>> {
    if host_sealed.iter().any(|p| family.starts_with(p)) {
        return Some(None);
    }
    g.subjects()
        .into_iter()
        .filter(|s| {
            g.objects(s, &format!("{RDF}type"))
                .iter()
                .any(|t| matches!(t, Term::NamedNode(n) if n.as_str() == format!("{IK}Level")))
        })
        .find(|level| {
            g.strs(level, &format!("{IK}seals"))
                .iter()
                .any(|p| family.starts_with(p.as_str()))
        })
        .map(Some)
}

/// The entry walk from `node`, then the pushdown closure over every entered level.
fn check_walk(g: &Graph, node: &str, family: &str, host_sealed: &[&str]) -> Walk {
    let mut walk = Walk {
        sealed: sealer(g, family, host_sealed),
        ..Walk::default()
    };
    walk.walk(g, node, family, "");
    let mut pushed: BTreeSet<Vec<String>> = BTreeSet::new();
    let mut at = 0;
    while at < walk.entered.len() {
        let frames = walk.entered[at].clone();
        at += 1;
        if !pushed.insert(frames.clone()) {
            continue;
        }
        // The resolved scope: the frames innermost first, each walked as its own
        // space, behind the host corridors' walls and whatever earlier frames wall.
        let mut push = Walk {
            walls: walk.corridor_walls.clone(),
            maybe: walk.maybe,
            sealed: walk.sealed.clone(),
            ..Walk::default()
        };
        for (i, level) in frames.iter().enumerate() {
            push.stack = frames[i..].iter().rev().cloned().collect();
            push.walk(g, &g.space(level), family, "");
        }
        walk.absorb(push);
    }
    walk
}

/// The first question: is any door of `family` reachable from `node` without a
/// limiter over it standing ahead of it — following every level's push?
fn reach(g: &Graph, node: &str, family: &str) -> Reach {
    reach_sealed(g, node, family, &[])
}

/// [`reach`] for a kernel that seals `host_sealed` (`Kernel::sealed()`).
fn reach_sealed(g: &Graph, node: &str, family: &str, host_sealed: &[&str]) -> Reach {
    let walk = check_walk(g, node, family, host_sealed);
    if walk.reachable {
        Reach::Reachable
    } else if walk.unknown {
        Reach::Unknown
    } else {
        Reach::Unreachable
    }
}

/// The second question: did the walk meet something it does not evaluate? `true`
/// means the first answer is not to be trusted — the two-query protocol of R7.3,
/// where "safe" is the first `Unreachable` (the ASK's `false`) AND this `false`.
fn unanswered(g: &Graph, node: &str, family: &str) -> bool {
    check_walk(g, node, family, &[]).unknown
}

#[test]
fn the_papers_12_5_check_is_a_walk_over_the_topology_no_with_the_limiter_and_yes_without() {
    // The served root: `Fallback([Limit("urn:personal:"), root])`, where `root`
    // mounts a personal space and a public one.
    let root = || -> Arc<dyn Space> {
        Arc::new(
            Fallback::new(vec![
                Arc::new(Mount::new(
                    "urn:personal:",
                    Arc::new(
                        EndpointSpace::new()
                            .bind(Exact::new("urn:personal:calendar"), door("calendar"))
                            .named(iri("urn:example:space:personal")),
                    ),
                )),
                Arc::new(Mount::new(
                    "urn:public:",
                    Arc::new(
                        EndpointSpace::new()
                            .bind(Exact::new("urn:public:hello"), door("hello"))
                            .named(iri("urn:example:space:public")),
                    ),
                )),
            ])
            .named(iri("urn:example:space:root")),
        )
    };
    let served: Arc<dyn Space> = Arc::new(Fallback::new(vec![
        Arc::new(Limit::new("urn:personal:").named(iri("urn:example:space:gatekeeper"))),
        root(),
    ]));
    let cap = Capability::root();

    // With the limiter: the personal family is NOT reachable from the entry, the
    // public one is — and the graph says so before any request is made.
    let g = topology(&kernel(served), &cap, Scope::empty());
    assert_eq!(
        reach(&g, "urn:ikigai:chain:root", "urn:personal:"),
        Reach::Unreachable
    );
    assert_eq!(
        reach(&g, "urn:ikigai:chain:root", "urn:public:"),
        Reach::Reachable
    );

    // Remove the limiter: reachable.
    let g = topology(&kernel(root()), &cap, Scope::empty());
    assert_eq!(
        reach(&g, "urn:ikigai:chain:root", "urn:personal:"),
        Reach::Reachable
    );

    // The limiter at a position — injected as a corridor — is a layer of the
    // chain, and the same walk finds it.
    let limited = Scope::empty().with(Arc::new(
        Limit::new("urn:personal:").named(iri("urn:example:space:gatekeeper")),
    ));
    let entry = format!("urn:ikigai:chain:{:016x}", limited.fingerprint());
    let g = topology(&kernel(root()), &cap, limited);
    assert_eq!(reach(&g, &entry, "urn:personal:"), Reach::Unreachable);
    assert_eq!(reach(&g, &entry, "urn:public:"), Reach::Reachable);

    // A limiter over a NARROWER family does not close the whole family, and a
    // foreign space on the path makes the answer "unknown", never "no".
    let partial: Arc<dyn Space> = Arc::new(Fallback::new(vec![
        Arc::new(Limit::new("urn:personal:calendar")),
        Arc::new(Foreign(root())),
    ]));
    let g = topology(&kernel(partial), &cap, Scope::empty());
    assert_eq!(
        reach(&g, "urn:ikigai:chain:root", "urn:personal:"),
        Reach::Unknown
    );
}

#[test]
fn a_confinement_reports_the_corridor_it_severs_into() {
    let corridor: Arc<dyn Space> =
        Arc::new(EndpointSpace::new().bind(Exact::new("urn:doc:1"), door("doc")));
    let confined = Confine::new(iri("urn:example:ctx:doc:1"), corridor, Arc::new(door("x")));
    let tree = confined.topology();
    assert!(matches!(tree.kind, SpaceKind::Confine));
    assert_eq!(tree.id.as_ref().unwrap().as_str(), "urn:example:ctx:doc:1");
    assert!(
        matches!(&tree.children[0].kind, SpaceKind::EndpointSpace { patterns } if patterns == &["urn:doc:1"])
    );
    let g = Graph::parse(&tree.to_turtle());
    assert_eq!(g.kind("urn:example:ctx:doc:1"), "Confine");
    assert_eq!(g.kind(&g.space("urn:example:ctx:doc:1")), "EndpointSpace");
}

// ---- 4. The check answers for a fragment, and says so --------------------------
//
// Ledger #552: the 0.1.78 check called two arrangements guarded that leak (a mapper
// behind the wall), answered `false` through an opaque space in its SPARQL form, and
// matched templates as prefixes. Each test below pins one shape of the fragment
// R7.3 now states, on the same walk the check above runs. `served` is the paper's
// §12.5 arrangement with one member swapped in behind the gatekeeper.

/// The root of the §12.5 arrangement: a personal space and a public one, mounted.
fn personal_and_public() -> Arc<dyn Space> {
    Arc::new(
        Fallback::new(vec![
            Arc::new(Mount::new(
                "urn:personal:",
                Arc::new(
                    EndpointSpace::new()
                        .bind(Exact::new("urn:personal:calendar"), door("calendar"))
                        .named(iri("urn:example:space:personal")),
                ),
            )),
            Arc::new(Mount::new(
                "urn:public:",
                Arc::new(
                    EndpointSpace::new()
                        .bind(Exact::new("urn:public:hello"), door("hello"))
                        .named(iri("urn:example:space:public")),
                ),
            )),
        ])
        .named(iri("urn:example:space:root")),
    )
}

/// `Fallback([Limit("urn:personal:"), behind])`: the gatekeeper, then whatever is
/// placed behind it.
fn served(behind: Arc<dyn Space>) -> Arc<dyn Space> {
    Arc::new(Fallback::new(vec![
        Arc::new(Limit::new("urn:personal:").named(iri("urn:example:space:gatekeeper"))),
        behind,
    ]))
}

/// Both questions over the root chain, for the personal family.
fn check(space: Arc<dyn Space>) -> (Reach, bool) {
    let g = topology(&kernel(space), &Capability::root(), Scope::empty());
    (
        reach(&g, "urn:ikigai:chain:root", "urn:personal:"),
        unanswered(&g, "urn:ikigai:chain:root", "urn:personal:"),
    )
}

#[test]
fn an_alias_into_the_family_behind_the_wall_resolves_because_the_wall_is_over_the_name_not_the_door(
) {
    // The paper's §12.5: one added import — here one alias rule — opens a path to the
    // vault that never passes the gatekeeper. `Fallback([Limit("urn:personal:"),
    // Alias{urn:other → urn:personal:calendar} over root])`: a request for
    // `urn:personal:calendar` is limited; a request for `urn:other` is not in the
    // family, so it PASSES the wall, is rewritten into the family behind it, and hits
    // the door. The kernel is RIGHT to resolve it: Def. 7's limiter admits
    // identifiers, the wall stands over the NAME, and the name that reached it was
    // `urn:other`. The kernel's limiter branch fires on a hit on ⊥ itself, and this
    // hit is on the calendar door (kernel.rs, "THE LIMITER"). What was wrong is a
    // check that called this arrangement guarded — the next test.
    let table = Arc::new(AliasTable::new().exact("urn:other", "urn:personal:calendar"));
    let kernel = kernel(served(Arc::new(Alias::new(table, personal_and_public()))));
    let cap = Capability::root();
    let limited = block_on(kernel.issue(source("urn:personal:calendar"), &cap)).unwrap_err();
    assert!(
        matches!(limited, Error::Unresolved(ref t) if t.as_str() == "urn:personal:calendar"),
        "the family's own name is limited: {limited:?}"
    );
    let leaked = block_on(kernel.issue(source("urn:other"), &cap)).unwrap();
    assert_eq!(leaked.bytes, b"calendar", "the alias walked round the wall");
}

#[test]
fn the_check_reports_an_alias_into_the_family_behind_the_wall_by_its_logical_name() {
    // (a) The arrangement above: the alias's rules are visible, so the walk expands
    // them — `urn:other` passes the wall, its canonical is in the family, and the door
    // is there. REACHABLE, and nothing on the path is unevaluated.
    let table = Arc::new(AliasTable::new().exact("urn:other", "urn:personal:calendar"));
    let behind: Arc<dyn Space> = Arc::new(Alias::new(table, personal_and_public()));
    assert_eq!(check(served(behind)), (Reach::Reachable, false));

    // A prefix rule whose canonical is BROADER than the family maps into it too:
    // `urn:a:` → `urn:` carries `urn:a:personal:calendar` to the door.
    let table = Arc::new(AliasTable::new().prefix("urn:a:", "urn:"));
    let behind: Arc<dyn Space> = Arc::new(Alias::new(table, personal_and_public()));
    assert_eq!(check(served(behind)), (Reach::Reachable, false));

    // The same rule behind a wall over the LOGICAL names is no leak: the admitted
    // names hit ⊥ before the alias.
    let table = Arc::new(AliasTable::new().exact("urn:other", "urn:personal:calendar"));
    let walled_logical: Arc<dyn Space> = Arc::new(Fallback::new(vec![
        Arc::new(Limit::new("urn:personal:")),
        Arc::new(Limit::new("urn:other")),
        Arc::new(Alias::new(table, personal_and_public())),
    ]));
    assert_eq!(check(walled_logical), (Reach::Unreachable, false));

    // And an alias ABOVE the wall is walked through: the canonical meets the wall
    // inside, as the kernel's canonical adoption makes it (the limiter tests).
    let table = Arc::new(AliasTable::new().exact("urn:other", "urn:personal:calendar"));
    let above: Arc<dyn Space> = Arc::new(Alias::new(table, served(personal_and_public())));
    assert_eq!(check(above), (Reach::Unreachable, false));
}

#[test]
fn a_rewrite_behind_the_wall_is_not_answered() {
    // (b) τ is a closure: the graph shows an `ik:Rewrite` enclosing the root and
    // nothing of τ. Behind a wall over the family it may do exactly what the alias
    // above does, so the walk says UNKNOWN — never "unreachable" — and the second
    // question says why.
    let behind: Arc<dyn Space> = Arc::new(Rewrite::new(personal_and_public(), |t: &Iri| {
        (t.as_str() == "urn:other").then(|| iri("urn:personal:calendar"))
    }));
    assert_eq!(check(served(behind)), (Reach::Unknown, true));

    // Above every wall it is walked through, and answered: τ can only choose among
    // the doors the enclosed space has, and the wall inside meets its canonical.
    let above: Arc<dyn Space> = Arc::new(Rewrite::new(served(personal_and_public()), |_| None));
    assert_eq!(check(above), (Reach::Unreachable, false));
    let open: Arc<dyn Space> = Arc::new(Rewrite::new(personal_and_public(), |_| None));
    assert_eq!(check(open), (Reach::Reachable, false));
}

#[test]
fn an_alias_behind_the_wall_whose_every_canonical_is_outside_the_family_is_unreachable() {
    // (c) The rules are visible and none maps into the family; the names the table
    // passes through unchanged meet the wall. UNREACHABLE, and answered — the one
    // shape with a mapper behind the wall that the check calls safe, because it can
    // see the whole table.
    let table = Arc::new(
        AliasTable::new()
            .exact("urn:other", "urn:public:hello")
            .prefix("urn:alias:", "urn:public:"),
    );
    let behind: Arc<dyn Space> = Arc::new(Alias::new(table, personal_and_public()));
    assert_eq!(check(served(behind)), (Reach::Unreachable, false));
}

#[test]
fn an_opaque_space_behind_the_wall_is_not_answered_and_the_second_question_says_so() {
    // (d) A foreign space reports nothing: it may hold a door of the family, or be a
    // mapper into it. Behind the wall or above it, the walk says UNKNOWN and the
    // second question `true`. The SPARQL form's first ASK answers `false` here —
    // which is why the protocol runs both and calls only `false` AND `false` safe.
    assert_eq!(
        check(served(Arc::new(Foreign(personal_and_public())))),
        (Reach::Unknown, true)
    );
    let above: Arc<dyn Space> = Arc::new(Fallback::new(vec![
        Arc::new(Foreign(Arc::new(EndpointSpace::new()))),
        served(personal_and_public()),
    ]));
    assert_eq!(check(above), (Reach::Unknown, true));
    // The §12.5 arrangement itself: unreachable, and nothing unevaluated — SAFE.
    assert_eq!(
        check(served(personal_and_public())),
        (Reach::Unreachable, false)
    );
}

#[test]
fn a_template_door_is_placed_by_its_literal_head_and_answered_only_when_that_is_sound() {
    // (e) The check does not evaluate templates. It places a template door by the
    // text before its first `{`: inside the family, every expansion is a name of the
    // family; diverging from it, none can be; the family EXTENDING it —
    // `urn:{ns}:inbox` against `urn:personal:` — is the case it does not answer.
    let doors = |patterns: &[&str]| -> Arc<dyn Space> {
        let mut space = EndpointSpace::new();
        for pattern in patterns {
            space = space.bind(UriTemplate::parse(*pattern).unwrap(), door("t"));
        }
        Arc::new(space)
    };
    assert_eq!(
        check(doors(&["urn:personal:doc/{id}"])),
        (Reach::Reachable, false),
        "a head inside the family is a door of it"
    );
    assert_eq!(
        check(doors(&["urn:file:{path}"])),
        (Reach::Unreachable, false),
        "a head that diverges is not"
    );
    assert_eq!(
        check(doors(&["urn:{ns}:inbox"])),
        (Reach::Unknown, true),
        "a head the family extends is not answered"
    );
    // A door found wins over one not answered; a wall over the family stops both.
    assert_eq!(
        check(doors(&["urn:{ns}:inbox", "urn:personal:doc/{id}"])),
        (Reach::Reachable, true)
    );
    assert_eq!(
        check(served(doors(&["urn:{ns}:inbox", "urn:personal:doc/{id}"]))),
        (Reach::Unreachable, false)
    );
}

#[test]
fn a_template_family_on_a_limiter_may_wall_the_family_and_is_not_answered() {
    // (f) `Limit::matching(template)` renders its template as `ik:family`. One whose
    // head touches the family may stop some of it and not the rest; the check does
    // not evaluate it, so a door found after it is at best UNKNOWN. One whose head
    // diverges from the family stops none of it and is ignored.
    let walled = |family: &str| -> Arc<dyn Space> {
        Arc::new(Fallback::new(vec![
            Arc::new(Limit::matching(UriTemplate::parse(family).unwrap())),
            personal_and_public(),
        ]))
    };
    assert_eq!(check(walled("urn:personal:{x}")), (Reach::Unknown, true));
    assert_eq!(check(walled("urn:{ns}:calendar")), (Reach::Unknown, true));
    assert_eq!(
        check(walled("urn:doc:{id}:secret")),
        (Reach::Reachable, false)
    );
}

#[test]
fn a_named_space_shared_behind_the_wall_and_beside_it_is_answered_per_path() {
    // (g) One space, one IRI, stated twice: the topology renders it at each
    // occurrence by design, and the walk answers per path in the order the kernel
    // consults it. Beside the wall FIRST: the kernel serves the door and the walk
    // says REACHABLE. This is the shape the SPARQL form cannot answer: a property
    // path has no path identity, so its `NOT EXISTS` finds the guarded occurrence
    // and answers `false` — R7.3 states it as the query's limit.
    let personal: Arc<dyn Space> = Arc::new(
        EndpointSpace::new()
            .bind(Exact::new("urn:personal:calendar"), door("calendar"))
            .named(iri("urn:example:space:personal")),
    );
    let cap = Capability::root();
    let beside_first: Arc<dyn Space> = Arc::new(Fallback::new(vec![
        Arc::clone(&personal),
        served(Arc::clone(&personal)),
    ]));
    let kernel_beside = kernel(Arc::clone(&beside_first));
    assert_eq!(
        block_on(kernel_beside.issue(source("urn:personal:calendar"), &cap))
            .unwrap()
            .bytes,
        b"calendar"
    );
    let g = topology(&kernel_beside, &cap, Scope::empty());
    assert_eq!(
        g.strs("urn:example:space:personal", &format!("{IK}pattern"))
            .len(),
        2,
        "the shared space is rendered at both occurrences"
    );
    assert_eq!(check(beside_first), (Reach::Reachable, false));

    // Behind the wall FIRST: `Fallback` returns the first hit and a hit on ⊥ is a
    // hit, so the inner limiter ends resolution before the outer occurrence is
    // consulted — the kernel limits, and the walk, following pre-order, says
    // UNREACHABLE. A wall scoped to its own list would have called this reachable:
    // a false alarm, which is the direction the SPARQL form errs in.
    let wall_first: Arc<dyn Space> = Arc::new(Fallback::new(vec![
        served(Arc::clone(&personal)),
        Arc::clone(&personal),
    ]));
    let kernel_wall = kernel(Arc::clone(&wall_first));
    assert!(matches!(
        block_on(kernel_wall.issue(source("urn:personal:calendar"), &cap)).unwrap_err(),
        Error::Unresolved(_)
    ));
    assert_eq!(check(wall_first), (Reach::Unreachable, false));
}

#[test]
fn a_limiter_walls_only_what_the_mount_above_it_admits() {
    // A limiter over the family inside a mount that admits none of it is dead, and a
    // door elsewhere stays reachable; one inside a mount narrower than the family
    // walls only that part, and a door outside the part stays reachable.
    let dead: Arc<dyn Space> = Arc::new(Fallback::new(vec![
        Arc::new(Mount::new(
            "urn:other:",
            Arc::new(Fallback::new(vec![Arc::new(Limit::new("urn:personal:"))])),
        )),
        personal_and_public(),
    ]));
    assert_eq!(check(dead), (Reach::Reachable, false));
    let narrow: Arc<dyn Space> = Arc::new(Fallback::new(vec![
        Arc::new(Mount::new(
            "urn:personal:cal",
            Arc::new(Fallback::new(vec![Arc::new(Limit::new("urn:personal:"))])),
        )),
        Arc::new(
            EndpointSpace::new()
                .bind(Exact::new("urn:personal:calendar"), door("calendar"))
                .bind(Exact::new("urn:personal:mail"), door("mail")),
        ),
    ]));
    let g = topology(
        &kernel(Arc::clone(&narrow)),
        &Capability::root(),
        Scope::empty(),
    );
    assert_eq!(
        reach(&g, "urn:ikigai:chain:root", "urn:personal:cal"),
        Reach::Unreachable
    );
    assert_eq!(
        reach(&g, "urn:ikigai:chain:root", "urn:personal:mail"),
        Reach::Reachable
    );
    assert_eq!(check(narrow), (Reach::Reachable, false));
}

// ---- 5. Levels push, and the walk follows the pushes ----------------------------
//
// Ledger #563: an endpoint found inside a `Level` resolves its sub-requests at its
// level first — the level's own space, without the guard it was entered through —
// so reachability over a tree with levels in it follows those pushes. Each test
// below states what the kernel does, then that the walk says the same.

/// Sources `target` and answers with what came back, or the error as text.
fn reader(id: &'static str, target: &'static str) -> AsyncFnEndpoint {
    AsyncFnEndpoint::new(id, move |inv| {
        Box::pin(async move {
            match inv.source(&iri(target)).await {
                Ok(inner) => Ok(text(&inner.bytes)),
                Err(e) => Ok(text(format!("error: {e}").as_bytes())),
            }
        })
    })
}

fn answer(kernel: &Kernel, target: &str) -> String {
    match block_on(kernel.issue(source(target), &Capability::root())) {
        Ok(r) => String::from_utf8_lossy(&r.bytes).into_owned(),
        Err(e) => format!("error: {e}"),
    }
}

/// `Fallback([Limit("urn:personal:"), Mount("urn:mod:", Level(M, doors))])` — the
/// gatekeeper at the root, and a module whose level binds a personal door beside
/// its public one.
fn module_behind_the_wall() -> Arc<dyn Space> {
    served(Arc::new(Mount::new(
        "urn:mod:",
        Arc::new(Level::new(
            iri("urn:example:level:mod"),
            Arc::new(
                EndpointSpace::new()
                    .bind(
                        Exact::new("urn:mod:public"),
                        reader("public", "urn:personal:calendar"),
                    )
                    .bind(Exact::new("urn:personal:calendar"), door("calendar")),
            ),
        )),
    )))
}

#[test]
fn a_door_behind_a_modules_guard_is_reachable_through_the_modules_own_endpoint() {
    // The kernel: from outside the personal door is limited; through the module's
    // public endpoint, whose sub-request resolves at the module's level BEFORE the
    // root (and its limiter) is consulted, it answers.
    let kernel = kernel(module_behind_the_wall());
    assert!(answer(&kernel, "urn:personal:calendar").starts_with("error: no endpoint"));
    assert_eq!(answer(&kernel, "urn:mod:public"), "calendar");

    // The walk: without the push it would say "unreachable" — the limiter stands
    // ahead of the door in the root's list, and the mount admits none of the
    // family. The module is ENTERED (its public door is reachable), the push walks
    // its space from the level, and the door is there: REACHABLE.
    let g = topology(&kernel, &Capability::root(), Scope::empty());
    assert_eq!(g.kind("urn:example:level:mod"), "Level");
    assert_eq!(
        reach(&g, "urn:ikigai:chain:root", "urn:personal:"),
        Reach::Reachable
    );
    // With no door of the module reachable from outside, nothing enters it.
    let sealed_off: Arc<dyn Space> = served(Arc::new(Mount::new(
        "urn:mod:",
        Arc::new(Level::new(
            iri("urn:example:level:mod"),
            Arc::new(EndpointSpace::new().bind(Exact::new("urn:personal:calendar"), door("c"))),
        )),
    )));
    assert_eq!(check(sealed_off), (Reach::Unreachable, false));
}

#[test]
fn a_limiter_injected_as_a_host_corridor_still_walls_the_push() {
    // The host's corridors come FIRST in a resolved scope: a limiter injected as one
    // stops the module's sub-request before its level is consulted.
    let kernel = kernel(module_behind_the_wall());
    let walled = Scope::empty().with(Arc::new(
        Limit::new("urn:personal:").named(iri("urn:example:space:gatekeeper")),
    ));
    let got = block_on(kernel.issue_in(
        source("urn:mod:public"),
        &Capability::root(),
        walled.clone(),
    ))
    .unwrap();
    assert!(
        String::from_utf8_lossy(&got.bytes).starts_with("error: no endpoint"),
        "{got:?}"
    );
    let entry = format!("urn:ikigai:chain:{:016x}", walled.fingerprint());
    let g = topology(&kernel, &Capability::root(), walled);
    assert_eq!(reach(&g, &entry, "urn:personal:"), Reach::Unreachable);
}

#[test]
fn the_push_goes_outward_through_the_enclosing_levels_and_is_bounded_by_their_nesting() {
    // Level O guards a personal door behind its own mount; level I, nested in O, has
    // one public door. From outside the vault is limited and guarded twice over; an
    // endpoint found in I resolves at I, then at O (without O's guard), and reaches it.
    let inner = Arc::new(Mount::new(
        "urn:o:in:",
        Arc::new(Level::new(
            iri("urn:example:level:inner"),
            Arc::new(EndpointSpace::new().bind(
                Exact::new("urn:o:in:door"),
                reader("door", "urn:personal:vault"),
            )),
        )),
    ));
    let outer: Arc<dyn Space> = served(Arc::new(Mount::new(
        "urn:o:",
        Arc::new(Level::new(
            iri("urn:example:level:outer"),
            Arc::new(Fallback::new(vec![
                inner,
                Arc::new(
                    EndpointSpace::new().bind(Exact::new("urn:personal:vault"), door("vault")),
                ),
            ])),
        )),
    )));
    let kernel = kernel(Arc::clone(&outer));
    assert!(answer(&kernel, "urn:personal:vault").starts_with("error: no endpoint"));
    assert_eq!(answer(&kernel, "urn:o:in:door"), "vault");
    assert_eq!(check(outer), (Reach::Reachable, false));
}

#[test]
fn a_host_sealed_family_has_exactly_the_reach_it_had_without_levels() {
    // A host that seals the family cannot build the arrangement above at all — a
    // level binding a host-sealed name is refused at build …
    let refused =
        Kernel::check_sealing(module_behind_the_wall().as_ref(), ["urn:personal:"]).unwrap_err();
    assert!(
        refused.to_string().contains("urn:example:level:mod"),
        "{refused}"
    );
    // … and the walk states the same rule on any graph: a door of a host-sealed
    // family counts only outside every level, so no push can reach one — the reach
    // a family had before levels existed. (The graph is the unsealed kernel's; the
    // walk is told the host's seals, as a doctor reads them from `Kernel::sealed()`.)
    let g = topology(
        &kernel(module_behind_the_wall()),
        &Capability::root(),
        Scope::empty(),
    );
    assert_eq!(
        reach_sealed(
            &g,
            "urn:ikigai:chain:root",
            "urn:personal:",
            &["urn:personal:"]
        ),
        Reach::Unreachable
    );
    assert_eq!(
        reach_sealed(&g, "urn:ikigai:chain:root", "urn:personal:", &[]),
        Reach::Reachable
    );
}

#[test]
fn a_modules_own_frame_reaches_its_sealed_name_behind_a_wall_over_it() {
    // M seals its key, and the host walls the key at the root. Outside requests are
    // limited; M's own endpoint resolves the key at M — the owner's frame — first.
    let module: Arc<dyn Space> = Arc::new(Fallback::new(vec![
        Arc::new(Limit::new("urn:mod:key")),
        Arc::new(Mount::new(
            "urn:mod:",
            Arc::new(
                Level::new(
                    iri("urn:example:level:mod"),
                    Arc::new(
                        EndpointSpace::new()
                            .bind(Exact::new("urn:mod:key"), door("key"))
                            .bind(Exact::new("urn:mod:read"), reader("read", "urn:mod:key")),
                    ),
                )
                .sealing(["urn:mod:key"]),
            ),
        )),
    ]));
    let kernel = kernel(Arc::clone(&module));
    assert!(answer(&kernel, "urn:mod:key").starts_with("error: no endpoint"));
    assert_eq!(answer(&kernel, "urn:mod:read"), "key");
    let g = topology(&kernel, &Capability::root(), Scope::empty());
    assert_eq!(
        g.strs("urn:example:level:mod", &format!("{IK}seals")),
        ["urn:mod:key"]
    );
    assert_eq!(
        reach(&g, "urn:ikigai:chain:root", "urn:mod:key"),
        Reach::Reachable
    );
}
