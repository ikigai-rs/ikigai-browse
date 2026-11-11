//! Findings cache coverage across roots, graph writes, and composed reads (ledger #528).
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_browse::{ExplainConfig, Mount};
use ikigai_core::{
    ArgRef, AsyncFnEndpoint, Capability, EndpointSpace, Exact, Expiry, Fallback, FnEndpoint, Iri,
    Kernel, ReprType, Representation, Request, Verb,
};
use ikigai_store::{DurableStore, SharerWrites};
use oxigraph::model::NamedNode;

const A: &str = "urn:test:findings:a";
const B: &str = "urn:test:findings:b";

fn request(verb: Verb, target: &str, args: &[(&str, &str)]) -> Request {
    args.iter().fold(
        Request::new(verb, Iri::parse(target).unwrap()),
        |r, (name, value)| r.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec())),
    )
}

fn text(value: &str) -> Representation {
    Representation::new(ReprType::new("text/plain"), value.as_bytes().to_vec())
}

#[test]
fn graph_threads_refresh_composites_and_leave_other_graphs_cached() {
    let root = std::env::temp_dir().join(format!("browse-findings-cache-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.rs"), "fn alpha() {}\n").unwrap();
    let (store, handle) = DurableStore::in_memory_shared_declaring(
        SharerWrites::only_the_default_graph().and_named_graphs([A, B]),
    )
    .unwrap();
    let config = || {
        ExplainConfig::new(Arc::clone(&handle))
            .review_model_label("test")
            .no_judge()
    };
    let first = Mount::new([("one".into(), root.clone())])
        .explain(config())
        .graph(NamedNode::new(A).unwrap())
        .space();
    let same_graph = Mount::new([("two".into(), root.clone())])
        .explain(config())
        .graph(NamedNode::new(A).unwrap())
        .space();
    let second = Mount::new([("three".into(), root.clone())])
        .explain(config())
        .graph(NamedNode::new(B).unwrap())
        .space();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let consumers = EndpointSpace::new()
        .bind(
            Exact::new("urn:test:queue"),
            AsyncFnEndpoint::new("queue", move |inv| {
                let count = Arc::clone(&count);
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    let rows = inv
                        .source(&Iri::parse("urn:repo:one:findings").unwrap())
                        .await?;
                    Ok(text(std::str::from_utf8(&rows.bytes).unwrap()).cacheable())
                })
            }),
        )
        .bind(
            Exact::new("urn:llm:coder:ask"),
            FnEndpoint::new("model", |_| {
                Ok(text(
                    "QUOTE: fn alpha() {}\nSEVERITY: major\nNOTE: no caller.\n",
                ))
            }),
        );
    let k = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(first),
        Arc::new(same_graph),
        Arc::new(second),
        Arc::new(consumers),
        Arc::new(ikigai_store::space(store)),
    ])));
    let cap = Capability::root();
    let queues: Vec<_> = ["one", "two", "three"]
        .into_iter()
        .map(|repo| {
            request(
                Verb::Source,
                &format!("urn:repo:{repo}:findings"),
                &[("state", "published")],
            )
        })
        .collect();
    let warm = || {
        for r in &queues {
            assert_eq!(
                block_on(k.issue(r.clone(), &cap)).unwrap().expiry,
                Expiry::Never
            );
        }
    };
    let page = request(Verb::Source, "urn:test:queue", &[]);
    for _ in 0..3 {
        block_on(k.issue(page.clone(), &cap)).unwrap();
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the composite is a cache hit"
    );
    warm();
    // Review runs with ordinary browse + network grants, with no kernel cut or store write authority.
    let reviewer = Capability::scoped(["urn:cap:browse:read:one", "urn:cap:net:localhost"]);
    block_on(k.issue(
        request(Verb::Source, "urn:repo:one:review:a.rs", &[]),
        &reviewer,
    ))
    .unwrap();
    assert!(!k.is_cached(&page, &cap));
    assert!(!k.is_cached(&queues[0], &cap));
    assert!(!k.is_cached(&queues[1], &cap), "confirmation is graph-wide");
    assert!(
        k.is_cached(&queues[2], &cap),
        "another archive graph stays cached"
    );
    // A second mount of the same graph shares the thread even with disjoint roots.
    let reviewer = Capability::scoped(["urn:cap:browse:read:two", "urn:cap:net:localhost"]);
    block_on(k.issue(
        request(Verb::Source, "urn:repo:two:review:a.rs", &[]),
        &reviewer,
    ))
    .unwrap();
    // Empty published projections have no file dependency, so they cache even after a pass.
    let published = request(
        Verb::Source,
        "urn:repo:one:findings",
        &[("state", "published")],
    );
    assert_eq!(
        block_on(k.issue(published.clone(), &cap)).unwrap().expiry,
        Expiry::Never
    );
    warm();
    block_on(k.issue(
        request(
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[
                ("graph", B),
                ("content", "CLEAR SILENT GRAPH <urn:test:findings:b>"),
            ],
        ),
        &cap,
    ))
    .unwrap();
    assert!(
        k.is_cached(&published, &cap),
        "a scoped write to another graph leaves this one cached"
    );
    assert!(k.is_cached(&queues[1], &cap));
    assert!(!k.is_cached(&queues[2], &cap));
    block_on(k.issue(
        request(
            Verb::Sink,
            "urn:iki:store:graph-update",
            &[
                ("graph", A),
                ("content", "CLEAR GRAPH <urn:test:findings:a>"),
            ],
        ),
        &cap,
    ))
    .unwrap();
    assert!(!k.is_cached(&published, &cap));
    assert!(!k.is_cached(&queues[1], &cap));
    block_on(k.issue(page.clone(), &cap)).unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the composed view refreshed after writes"
    );
    std::fs::remove_dir_all(root).unwrap();
}
