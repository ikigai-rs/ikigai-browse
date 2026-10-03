//! The repository browse family, wired onto gonk's one dataset — and cached, because gonk
//! watches the disk.
//!
//! ```text
//! urn:repo:{root}:tree[:{path}]        Source  a directory listing
//! urn:repo:{root}:file:{path}          Source  one file
//! urn:repo:{root}:hash[:{path}]        Source  the content hash the archive keys on
//! urn:repo:{root}:state                Source  git HEAD and the dirty set
//! urn:repo:{root}:prs[:{path}], :pr:{n}  Source  the pull-request family (through ikigai-repo)
//! urn:repo:style                       Source  the a11y-layered stylesheet
//! urn:iki:annotation[:{id}]            Source/Sink/Delete  W3C annotations, in THIS dataset
//! ```
//!
//! # One dataset, two named graphs
//!
//! The annotation family takes an `Arc<Store>` — the same dataset the ledger's named graphs
//! live in, handed over by `DurableStore::open_shared_declaring`. That is the whole point of
//! the arc: a ledger item's `ledger:about <urn:repo:…>` and an annotation on that file are
//! two graphs in ONE store, so the join is a local SPARQL query and not a federation problem.
//! What the shared handle costs the LEDGER's read cache is **nothing**, and the reason is the
//! declaration on that call: [`Graph::sharer_writes`] names where this family writes, so
//! `ikigai-store` keeps every scoped read of every other graph cacheable under its own write
//! threads (`src/main.rs`, and `tests/browse.rs` prints the numbers).
//!
//! ★ **Where browse's quads land is THIS SERVER's decision since `ikigai-browse` 0.4.0**, and
//! it is [`Graph`] — one value, flowing into the mount and into the store's coverage promise,
//! so the two cannot say different things. Read that type before changing either.
//!
//! ★★ **Since 2026-09-16 that decision is a NAMED graph** — [`Graph::chosen`] —
//! so browse's quads are inside the per-graph capability boundary: `urn:cap:store:read:graph:`
//! `urn:iki:browse:graph:default` is a token an operator can mint
//! ([`crate::grants::browse_graph_grants`]), where the default graph had no IRI and could be
//! named by no token at all. What that costs is this graph's cacheability, and nothing else:
//! see obligation 3 on [`Graph`].
//!
//! ⚠ **It is a data migration, not a setting.** A binary carrying this choice, run against a
//! store whose browse quads are still in the default graph, reads an EMPTY archive with no
//! error anywhere — so [`unmigrated_quads`] is checked at startup and `main` says so loudly.
//!
//! # Explanations, and what binds them
//!
//! `Mount::explain` binds `urn:repo:{root}:{explain,explain-versions,review}` and the
//! PR-derived layers, every one of which derives through `urn:llm:{provider}:ask`. gonk
//! links no LLM client and never will — it MOUNTS one ([`crate::mount`]) — so those rows are
//! bound **only when a `gonk.mount` line names the peer that serves them**
//! ([`crate::config::Settings::explains`]). With no mount they would be actions the manifold
//! offers and the kernel can never satisfy: an over-offer, which is the one direction the
//! module recipe calls worse than a missing feature.
//!
//! ## ⚠ The spend gate is a capability, and it is `ikigai-browse`'s, not gonk's
//!
//! Every derivation declares TWO capabilities: `urn:cap:browse:read:*` (the wildcard
//! offering — enforcement checks the target's root) and `urn:cap:net:*`, because calling a
//! model is a network act even against localhost. `urn:repo:{root}:review:{path}` declares a
//! third, `urn:cap:annotate`, because its findings are minted as real annotations in this
//! dataset. Since `declared = enforced`, the kernel refuses before dispatch, and
//! `urn:kernel:actions` — capability-scoped by construction — does not offer an explain row
//! to a caller who could not invoke it.
//!
//! So the whole gate is which door's capability carries a net grant. gonk mints none:
//! `ikigai-gonk grants`, `client add` and `passkey invite` write per-ledger tokens and — since
//! the graph decision — the browse graph's two STORE tokens, which are authority over quads in
//! one graph and over nothing else. `grants.json` refuses the wildcard `urn:cap:net:*` as a
//! GRANT the way it refuses
//! `urn:cap:exec:*` ([`crate::grants::unbounded_net_scopes`]). The per-door table is in the
//! README; the short form is that **an anonymous HTTP caller cannot reach a browse row at
//! all**, so it can neither derive an explanation nor read an archived one, and the socket
//! door's root can do both.
//!
//! ⚠ **The archive read and the derivation are ONE action to a capability.** `version=`
//! addresses an archived entry and provably derives nothing (`ikigai-browse` returns
//! `NotFound` on a miss rather than falling back to a model), but it is the same
//! `urn:repo:{root}:explain` row and therefore carries the same `urn:cap:net:*`
//! requirement — so "free to read what was already paid for" cannot be granted separately
//! **through the browse row**. That matters for the HTTP browse face (#258), where an
//! anonymous reader is exactly the caller who should see archived text and never spend.
//!
//! ★ The graph decision opens the other route, and it is worth saying plainly because it
//! reads like a hole and is not: `urn:cap:store:read:graph:urn:iki:browse:graph:default`
//! makes every archived explanation, annotation and review finding readable as QUADS through
//! `urn:iki:store:graph-select`, with no net grant and no `urn:cap:browse:read:*` — the
//! archive without the spend, which is what #258 asked for. What it does NOT carry is the
//! rest of the browse family: no file contents, no tree, no `gh`, and no way to derive
//! anything. The two authorities are genuinely separate, and this is the first spelling of
//! either that an operator can write down.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_browse::{ExplainConfig, Mount, StyleWatch};
use ikigai_core::{
    Description, Endpoint, EndpointSpace, Invocation, Iri, Representation, Request, Resolution,
    Result, Scope, Space, SpaceEntry, Topology, Verb,
};
use ikigai_store::{SharerWrites, Store};
use oxigraph::model::{GraphName, NamedNode};

use crate::config::ExplainTiers;
use crate::watch::{
    in_build_output, root_thread, wide_thread, Epochs, RootIgnore, RootWatch, Seen, Watched,
};

/// **Where the browse family's quads live — ONE value, read by both the people who must agree
/// about it.**
///
/// # ★ Why this is a type and not a line in `main`
///
/// `main` hands `ikigai-browse` a handle on gonk's dataset, and in the same breath tells
/// `ikigai-store` where that sharer writes ([`SharerWrites`]). Those two statements must agree
/// or the store caches scoped reads against threads the sharer's write never cuts — silent,
/// unbounded staleness, the worst failure `ikigai-store` has.
///
/// Until `ikigai-browse` 0.4.0 they agreed by TRANSCRIPTION: `main` wrote
/// `SharerWrites::only_the_default_graph()` because a person had read browse's source and
/// found `GraphName::DefaultGraph` hard-coded in its three writers. Nothing in either crate
/// connected the sentence to the behaviour — the promise was a fact about a dependency's
/// internals, re-typed here, with a comment asking the next person to keep it true
/// (ledger #282). A transcribed invariant drifts, and this one drifts silently.
///
/// `Mount::graph(NamedNode)` inverts the direction. gonk CHOOSES, so gonk KNOWS, and the two
/// statements are two readings of this one value: [`wire`] puts it on the mount (through the
/// private `Graph::on`, so a caller cannot put a different one there) and
/// [`Graph::sharer_writes`] puts it in the declaration. There is no second place to edit, and
/// `graph_choice_and_promise_cannot_disagree` walks both arms to say so. It is the same
/// argument that deleted the wrapper's transcribed `SCOPED_READS` array in PR #7: a value
/// copied out of another crate is a value that goes stale.
///
/// ⚠ **One fact still comes from browse and cannot be derived here**: that a mount which
/// never calls `Mount::graph` writes the DEFAULT graph. That is browse's documented default
/// (`Mount::graph`'s doc comment: *"A mount that never calls this uses the store's default
/// graph, which is exactly what every host had before 0.4.0, byte for byte"*), and it is one
/// stated contract rather than three hard-coded constants in three modules — but it is still
/// a fact this server takes on trust, and `tests/browse.rs::a_browse_write_touches_no_reserved
/// _graph` is why it is checked rather than believed. The residual would go away if browse
/// exposed the graph a built mount resolved to; it does not today (reported to the hub).
///
/// # ★ The decision, taken 2026-09-16: a named graph, and what it obliged
///
/// [`chosen`](Graph::chosen) answers `urn:iki:browse:graph:default`. Naming a graph is not a
/// config change; it is a data migration, and these are the four obligations it carries, in
/// the order they have to happen — written as they were paid, because the next person who
/// changes that IRI owes the same four again:
///
/// 1. **run `ikigai-browse`'s `migrate-annotation-ns <store> --graph <iri> --commit`**, or
///    the archive reads EMPTY — quads left in the default graph are still there and no longer
///    visible, with no error anywhere. An OPERATOR's step, with this server STOPPED (RocksDB
///    holds one writer lock), and it cannot be undone by restarting. gonk no longer takes it
///    on trust: [`unmigrated_quads`] counts what is stranded, `main` runs it before the doors
///    open, and a non-zero answer is a banner nobody can miss. That check is browse's own
///    counting function, so it reports the same number as the migration's dry run.
/// 2. **mint `urn:cap:store:{read,write}:graph:<iri>`**, which is the entire point: the
///    default graph has no IRI, so no scoped token could name it and every query over
///    browse's quads was a ROOT one. [`crate::grants::browse_graph_grants`] computes those two
///    tokens FROM this choice, and `--browse-graph read|write` mints them onto a certificate
///    or a passkey. What that buys, exactly: an identity can read (or write) browse's quads
///    through `urn:iki:store:graph-{select,ask,construct,describe}` — the archive without the
///    spend — and reaches no browse endpoint, no file and no `gh` by doing so.
///    ⚠ **The anonymous HTTP caller is deliberately NOT given it.** Its grant is exactly
///    `gonk.http.ledger`'s ledgers, computed in `main`, and widening that silently in a
///    version bump is not something a config file could take back. A signed-in passkey is the
///    HTTP door's spelling of "a caller who may".
/// 3. **redo the freshness argument.** Done, and it is in [`cached_reads`]'s table and in
///    `tests/browse.rs`: a promised graph is not covered, so `ikigai-store` answers every
///    scoped read of THIS graph `Expiry::Always` by itself — there is no second place where
///    this server would have to declare it, and a wrapper that did would be the ledger #282
///    failure again. The LEDGER graph's exemption is untouched, which is the ~1000× this
///    server measured, and `the_shared_handle_costs_the_ledger_nothing` measures it against
///    this shipped choice rather than against the one that was cheap.
/// 4. **wrap the browse half of every cross-graph query in `GRAPH <iri> { … }`.** In this
///    repo that is `tests/browse.rs`'s join and the `/sparql` page's samples; an operator's
///    own queries and any consumer's are theirs, and the failure mode is an empty result
///    rather than an error, which is why it is called out in the README's migration steps.
///
/// # ⚠ Changing the IRI is a SECOND migration
///
/// The literal is asserted by `the_choice_this_server_ships_is_its_own_named_graph`, so a new
/// spelling is a red test rather than a silent re-strand. Why this one:
/// `urn:iki:browse:graph:default` sits beside `urn:iki:ledger:graph:default` in the same
/// shape (family · `graph` · name), leaves the last segment free for a per-root or per-tenant
/// browse graph later, and — deliberately — does **not** name gonk. The dev server's archive
/// (ledger #244) holds the same kind of quads written by the same crate; a host-neutral name
/// makes absorbing it a union rather than a rename.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Graph {
    /// The store's **default graph** — where every quad browse wrote on this server before
    /// 2026-09-16, and where a host that never calls `Mount::graph` still writes.
    ///
    /// It has no IRI, so no `urn:cap:store:{read,write}:graph:` token can name it and no
    /// graph-scoped query can see it: browse's data sits OUTSIDE the per-graph capability
    /// boundary entirely. That is what the decision above moved away from. Not reached by
    /// [`chosen`](Graph::chosen) any more; still walked by
    /// `graph_choice_and_promise_cannot_disagree` and by
    /// `the_default_arm_still_promises_only_the_default_graph`, because the arm a server can
    /// go back to is an arm that has to keep working.
    TheDefault,
    /// A **named graph**, inside the tenancy boundary — what this server ships, and what the
    /// four obligations above are about.
    Named(NamedNode),
}

/// The graph [`Graph::chosen`] names — the one literal this decision is written as.
///
/// Private on purpose: a second public name for it is a second place a caller could build
/// the choice from, and the whole shape of ledger #282's fix is that there is one value.
/// Everything that needs the IRI reads it off the choice ([`Graph::named`]).
const CHOSEN_GRAPH: &str = "urn:iki:browse:graph:default";

impl Graph {
    /// ★ **gonk's choice, made in exactly one place.**
    ///
    /// `main` calls this once, before the store opens, and passes the result to both
    /// [`sharer_writes`](Graph::sharer_writes) and [`wire`]. Changing this line is the whole
    /// of the graph decision on this side — and the four obligations on the type's docs are
    /// the whole of it on the other.
    ///
    /// ⚠ `new_unchecked` because `CHOSEN_GRAPH` is a literal in this file, and
    /// `the_choice_this_server_ships_is_its_own_named_graph` parses it for real. A `.expect()`
    /// here would move a compile-time-checkable fact into a startup panic.
    #[must_use]
    pub fn chosen() -> Self {
        Graph::Named(NamedNode::new_unchecked(CHOSEN_GRAPH))
    }

    /// The promise `main` hands `ikigai-store` when the handle leaves — **derived from the
    /// choice, never written beside it.**
    ///
    /// The default graph is never named in a [`SharerWrites`] (it has no IRI to name), so
    /// [`TheDefault`](Graph::TheDefault) is the bare `only_the_default_graph()` and a named
    /// choice is that plus the one graph. `only_the_default_graph` is not a weaker promise in
    /// the named case — it is the base every declaration carries, because a sharer can always
    /// write a graph no scoped read can see.
    #[must_use]
    pub fn sharer_writes(&self) -> SharerWrites {
        let base = SharerWrites::only_the_default_graph();
        match self {
            Graph::TheDefault => base,
            Graph::Named(graph) => base.and_named_graph(graph.as_str()),
        }
    }

    /// The same choice, put on a browse mount.
    ///
    /// Not calling `Mount::graph` is a DIFFERENT statement from calling it with the default
    /// graph — browse's knob is `Option<NamedNode>` precisely so a host that never spoke can
    /// be told from one that did — so [`TheDefault`](Graph::TheDefault) leaves the mount
    /// alone rather than passing anything.
    #[must_use]
    fn on(&self, mount: Mount) -> Mount {
        match self {
            Graph::TheDefault => mount,
            Graph::Named(graph) => mount.graph(graph.clone()),
        }
    }

    /// The chosen graph's IRI, or `None` for the default graph — for a banner line, a
    /// `GRAPH <…>` clause or a capability token ([`crate::grants::browse_graph_grants`]),
    /// never for rebuilding either statement above.
    #[must_use]
    pub fn named(&self) -> Option<&NamedNode> {
        match self {
            Graph::TheDefault => None,
            Graph::Named(graph) => Some(graph),
        }
    }

    /// The same choice as oxigraph's own graph name — what [`unmigrated_quads`] asks the
    /// store about.
    #[must_use]
    pub fn graph_name(&self) -> GraphName {
        match self {
            Graph::TheDefault => GraphName::DefaultGraph,
            Graph::Named(graph) => GraphName::NamedNode(graph.clone()),
        }
    }
}

/// ★ **Obligation 1, checked instead of assumed: how many quads `ikigai-browse` wrote that
/// this binary can no longer see.**
///
/// The choice on [`Graph`] confines browse's reads to one graph. A store whose browse quads
/// are somewhere else — the default graph, before the migration; a previously chosen name,
/// after someone edits the IRI — still holds every one of them on disk, and every browse read
/// answers as if the archive were empty. **There is no error to observe, at any layer**: an
/// empty annotation list and an empty archive are legitimate answers, and that is exactly the
/// failure `ikigai-browse`'s own `Mount::graph` docs warn a host about.
///
/// So `main` asks, once, before the doors open, and says so loudly when the answer is not
/// zero. The counting is `ikigai-browse`'s own `migrate::counts_for_graph` rather than a
/// query written here, for two reasons that are the same reason: the set of subject prefixes
/// browse mints is browse's fact (`BROWSE_SUBJECT_PREFIXES`, pinned in that crate against its
/// real writers), and **the number this reports is then the same number the migration's dry
/// run reports** — an operator comparing the banner with `migrate-annotation-ns` is comparing
/// one function with itself.
///
/// It is a pass over the dataset, which is why it is a startup check and not a per-read one.
///
/// # Errors
///
/// When the store cannot be iterated.
pub fn unmigrated_quads(store: &Store, graph: &Graph) -> std::result::Result<u64, String> {
    let counts = ikigai_browse::migrate::counts_for_graph(store, Some(&graph.graph_name()))
        .map_err(|e| e.to_string())?;
    // `None` is impossible here — a graph was named — but a panic in a startup check would be
    // a worse answer than the honest zero.
    Ok(counts.outside_target_graph.unwrap_or_default())
}

/// Root names this server refuses, because `ikigai-repo`'s own resources start with the same
/// segment and a reader could not tell which family answered.
///
/// `ikigai-browse` would accept them (its rows are per-root and would simply sit beside the
/// facades), so this is gonk's composition talking, not browse's grammar.
pub const RESERVED_ROOTS: [&str; 2] = ["pr", "style"];

/// Check a root name the way `ikigai-browse` does — before it can panic at mount time.
///
/// `build_roots` asserts on an empty name, a name containing `:`, `/`, `{` or `}`, and a
/// duplicate. An assert is the right shape for a library and the wrong one for a server: it
/// arrives as a panic in the banner's place, with a Rust backtrace instead of the config line
/// to edit.
///
/// # Errors
///
/// When the name could not be a root, or is one of [`RESERVED_ROOTS`].
pub fn check_root_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() || name.contains([':', '/', '{', '}']) {
        return Err(format!(
            "browse root `{name}`: a root name must be non-empty and contain no `:`, `/`, \
             `{{` or `}}` — it is spliced into `urn:repo:<name>:…`"
        ));
    }
    if RESERVED_ROOTS.contains(&name) {
        return Err(format!(
            "browse root `{name}`: that name is reserved — `urn:repo:{name}:…` is already \
             ikigai-repo's, so a root by this name would answer under a name a reader would \
             read as the other family's"
        ));
    }
    Ok(())
}

/// Wire the family: the space to compose, and the watch that keeps `urn:repo:style` fresh.
///
/// `watch` is the [`RootWatch`] `main` started, when there is one: the reads of the roots it
/// actually got, and only those, are made cacheable ([`cached_reads`]), and every write to the
/// family touches its [`Epochs`] so the header badge knows to recount.
///
/// `explain` is `Some` when a peer serves `urn:llm:*` — see the module docs for why that is
/// the switch. The tiers ride in as [`crate::config::ExplainTiers`] rather than as an
/// `ExplainConfig`, so the one place that turns an operator's ceilings into browse's builder
/// is here, beside the store handle the archive needs.
///
/// ★ `graph` is the SAME value `main` derived the store's `SharerWrites` from ([`Graph`]).
/// It is a parameter rather than a call to [`Graph::chosen`] here for exactly that reason: a
/// second call would be a second decision, and two decisions can differ.
///
/// ⚠ Only `Mount::graph` is set, never `ExplainConfig::graph`. browse resolves the two into
/// ONE archive and panics at mount time if they disagree — so the way to keep them agreeing
/// is to have one of them, not to set both carefully.
///
/// ⚠ Nothing here configures which PENDING findings the file page draws: browse 0.6.1's file
/// face renders published annotations only, and the argument a browse arc is adding for that
/// (`proposals=`, the complement of `gonk.queue.serious`) is a per-request argument, not a
/// mount setting. The seam is where gonk forwards a file-page command —
/// [`crate::k::KAdapter`]'s `source`, ledger [#496](http://localhost:1060/l/default/item/496).
pub fn wire(
    roots: Vec<(String, PathBuf)>,
    store: Arc<Store>,
    watch: Option<&RootWatch>,
    explain: Option<&ExplainTiers>,
    graph: &Graph,
) -> Wired {
    let mount = graph.on(Mount::new(roots)
        .annotations(Arc::clone(&store))
        // The PROCESS's name: it selects the `gonk.a11y.toml` layer `urn:repo:style`
        // reads its themes and its contrast floor from. Without it that file would sit
        // on disk doing nothing — the quietest kind of wrong.
        .app("gonk"));
    let mount = match explain {
        Some(tiers) => mount.explain(explain_config(store, tiers)),
        None => mount,
    };
    let (space, style) = mount.space_watched();
    Wired {
        space: match watch {
            Some(watch) => cached_reads(space, watch.watched(), Some(watch.epochs())),
            None => cached_reads(space, &[], None),
        }
        .writing_to(graph),
        style,
    }
}

/// The operator's tiers as `ikigai-browse`'s config.
///
/// ★ **No `allow_provider` call, deliberately.** A `provider=` argument may name only what
/// this host already asks with — the two configured tiers — and widening that set is
/// documented by that crate as THE authority boundary: `explain`'s declared capability
/// cannot vary by argument value, so a caller who may derive at all could otherwise point
/// this server at any backend the peer's registry happens to hold. gonk's tiers are the
/// operator's choice already; a caller does not get a second one.
///
/// The model LABELS are left unset on purpose too. Unset, `ikigai-browse` resolves the true
/// configured model id through `urn:llm:{provider}:model` — over the mount — at explain
/// time, so swapping the peer's model re-keys the archive with no gonk-side config. An
/// operator override here would pin tags to a string that can silently stop being true.
///
/// ★ The JUDGE (`gonk.review.judge`, ledger #696) is set here too, and it is NOT made
/// selectable: browse's `selectable()` leaves the judge provider out, so a backfill that named
/// it as `provider=` would be Denied — the backfill omits `provider=` and gets this one.
fn explain_config(store: Arc<Store>, tiers: &ExplainTiers) -> ExplainConfig {
    let config = ExplainConfig::new(store)
        .file_provider(&tiers.file.provider)
        .file_max_tokens(tiers.file.max_tokens)
        .dir_provider(&tiers.dir.provider)
        .dir_max_tokens(tiers.dir.max_tokens)
        .review_provider(&tiers.review.provider)
        .review_max_tokens(tiers.review.max_tokens)
        .pr_provider(&tiers.pr.provider)
        .pr_max_tokens(tiers.pr.max_tokens)
        .max_prompt_bytes(tiers.max_prompt_bytes)
        // ★ ledger #702 item 6: a reasoning judge answers empty at browse's 400.
        .judge_max_tokens(tiers.judge_max_tokens);
    match &tiers.judge {
        Some(provider) => config.judge_provider(provider),
        None => config.no_judge(),
    }
}

/// What [`wire`] hands back.
pub struct Wired {
    /// The family, with the cacheable overlay in front.
    pub space: CachedReads,
    /// browse's own watch over the config home's `a11y.toml` layers. A declared thread is a
    /// promise that something cuts it, and this is that something: the host starts it once
    /// its kernel exists.
    pub style: StyleWatch,
}

/// Declare the filesystem reads of a WATCHED root cacheable, under the thread
/// [`crate::watch::RootWatch`] cuts.
///
/// # ★ What is cached, and what is deliberately not
///
/// | read | cached? | why |
/// |---|---|---|
/// | `Source urn:repo:{root}:{tree,file}`, **no arguments**, watched root | yes, under [`root_thread`] | a pure function of bytes under the root, and the watcher cuts for every change a listing or a file can show |
/// | `Source urn:repo:{root}:{hash,state}`, **no arguments**, watched root | yes, under [`wide_thread`] | each can see a file a `.gitignore` hides (`hash` walks it, `git status` reports a tracked one), so they hang from the thread only build output does not cut |
/// | any of those whose path is IN build output (`target/`, `.git/`), or a `tree`/`file` of a path the watch does not cut for ([`Seen`]) | no | the watch ignores changes there (ledger #667), so a cache entry would be a promise nothing keeps |
/// | the same with any argument (`as=text/html`, `annotations=include`, `version=`, …) | no | those faces read the annotation overlay out of the store — and `ikigai-browse` REWRITES an annotation during a Source when the file it anchors to has drifted, so the answer depends on state this thread does not track |
/// | `urn:repo:{root}:prs`, `:pr:{n}` | no | they resolve `urn:repo:pr:*` through the kernel, which runs `gh`: the input is GitHub, not the disk, and a filesystem thread would hold a stale PR list until someone touched a file |
/// | `urn:repo:{root}:annotations[:{path}]`, `urn:iki:annotation:{id}` | no | store-derived, and the same drift rewrite applies |
/// | `urn:repo:{root}:explain[:{path}]`, `:explain-versions`, `:review:{path}` | no | the ARCHIVE is what makes these cheap the second time — keyed on `(path, content-hash, version-tag)` in the store, which re-keys on an edit by construction. A kernel cache entry in front of it would be a second, weaker copy of that, and one whose thread this watcher could not cut when the model changed |
/// | `urn:repo:style` | already | `ikigai-browse` declares it cacheable under one thread per `a11y.toml` candidate, and ships the watch that cuts them ([`Wired::style`]) |
/// | any read of an UNWATCHED root | no | fail closed: no watcher, no cache — the two halves move together or this is ledger #246 again |
///
/// The argument test is deliberately blunt ("no arguments at all") rather than a list of the
/// arguments that are safe. A new face in a later `ikigai-browse` arrives as a new argument,
/// and the failure of the permissive rule is a silently stale read; the failure of this one
/// is a read that is merely not cached.
///
/// # ★ Obligation 3, and why this table did not have to change for it
///
/// Naming a graph ([`Graph`]) forfeits that graph's cacheability: `ikigai-store` is promised
/// the sharer writes it, so `read_is_covered` is false for it and every scoped read of it is
/// `Expiry::Always` — answered by the store, per read, from the promise this server derived.
/// **Nothing here had to be re-declared, and that is the point.** Every row above that
/// touches the store is already `no`, and a wrapper in this crate that declared the browse
/// graph uncacheable would be a second place saying what is fresh — ledger #282's failure
/// with the sign flipped, and the thing `crate::compose_with` deleted a `freshness` module to
/// be rid of.
///
/// The obligation is therefore *checked* rather than *paid* here:
/// `tests/browse.rs::the_browse_graphs_scoped_reads_are_not_cached_and_the_ledgers_still_are`
/// issues both reads through a real kernel and looks at what the cache holds.
///
/// # ★ And the writes: the badge's epochs
///
/// With `epochs`, every request through the family that can change a pending-findings count
/// is wrapped so that, AFTER it has run, it touches the root it names: a Sink or
/// Delete anywhere in the family, and a `review` Source (a pass mints findings). A write that
/// names no root — a decision posts `urn:iki:finding:{id}` — touches the id's root when the
/// badge has seen it, and every root when not. After, never before: a poll that reads in
/// between stamps its count with the earlier epoch, so the next poll recounts.
pub fn cached_reads(
    inner: EndpointSpace,
    watched: &[Watched],
    epochs: Option<Arc<Epochs>>,
) -> CachedReads {
    CachedReads {
        // ★ The NAME goes on browse's own space, not on this overlay — see the type's doc.
        // `ikigai-browse` (0.13.0) claims none for it; if a later release does, this line
        // would overwrite it, and the right move then is to stop naming here and forward
        // whatever it claims.
        inner: inner.named(crate::spaces::iri(crate::spaces::BROWSE)),
        roots: watched
            .iter()
            .map(|root| (root.name.clone(), Arc::clone(&root.ignore)))
            .collect(),
        epochs,
        graph: None,
        reviews: None,
    }
}

/// The space [`cached_reads`] builds.
///
/// # In the topology: transparent
///
/// This overlay forwards [`Space::id`] and [`Space::topology`] to the space it encloses,
/// as [`Arc<dyn Space>`] does, rather than reporting a node of its own around it. Two
/// reasons. No [`ikigai_core::SpaceKind`] describes what it does — it adds no door, rewrites
/// no name, admits no prefix and limits no family; what it changes is the *representation*
/// (cacheable, under a thread), and the topology is the structure of resolution, not of
/// caching. And the identity claim (*same name ⇒ same doors*) holds exactly of the inner
/// space, which is why [`cached_reads`] names THAT — `urn:iki:gonk:space:browse` is the
/// browse family as gonk composes it, and a hit through this overlay reports it as
/// `answered_by` because [`Resolution::map_endpoint`] keeps everything but the endpoint.
///
/// Should core grow a kind for an interception overlay, this is the one place to report it.
pub struct CachedReads {
    inner: EndpointSpace,
    roots: std::collections::BTreeMap<String, Arc<RootIgnore>>,
    epochs: Option<Arc<Epochs>>,
    /// The graph browse writes (`None`: the default graph) — what a store-door write must
    /// name to move a pending count ([`Self::observe_store_writes`]).
    graph: Option<String>,
    /// Where every review Source through the family is counted while it runs
    /// ([`Self::observing_reviews`]).
    reviews: Option<Arc<crate::trigger::Activity>>,
}

impl CachedReads {
    /// Say which graph browse writes, for [`Self::observe_store_writes`].
    #[must_use]
    pub fn writing_to(mut self, graph: &Graph) -> Self {
        self.graph = graph.named().map(|g| g.as_str().to_string());
        self
    }

    /// Count every review Source through the family on `activity` while it runs
    /// ([`crate::trigger::Passes::reviews_in_flight`]) — whoever issued it.
    ///
    /// ★ **Why here, and not where a pass begins** (ledger
    /// [#702](http://localhost:1060/l/default/item/702) item 4): the review queue's passes
    /// claim [`crate::trigger::Activity::begin`]'s slot, but the page's Review button does
    /// not go through the queue — browse's face emits `/k/source urn:repo:{root}:review:{path}
    /// as=text/html`, and the `/k/` adapter resolves that under the caller's grant, straight
    /// into this family. A person on the socket does the same. This overlay is the one place
    /// every review passes, so it is where "a review is running" is true for all of them, and
    /// the judge backfill yields to that. It changes no answer, declares nothing and refuses
    /// nothing; it counts.
    #[must_use]
    pub fn observing_reviews(mut self, activity: Arc<crate::trigger::Activity>) -> Self {
        self.reviews = Some(activity);
        self
    }

    /// The badge's epochs, when the roots are watched.
    pub fn epochs(&self) -> Option<Arc<Epochs>> {
        self.epochs.clone()
    }

    /// `store`, with every WRITE through it that could reach browse's graph touching every
    /// root's epoch once it has run — and every read, and every write naming another graph
    /// (the ledger's), passed through untouched. `store` itself when nothing is watched.
    ///
    /// ★ Why the store needs this at all: a pending finding can be written without the
    /// browse family. `urn:iki:store:graph-update` under the browse graph's write token is a
    /// real authority an operator mints (`passkey invite … --browse-graph write`), and the
    /// badge's promise — "a pending count is a fact about the store, whoever wrote it" —
    /// covers it. ⚠ It changes NO answer and declares nothing fresh: the store still answers
    /// freshness itself, from the promise `main` made ([`crate::compose_with`]'s note).
    pub fn observe_store_writes(&self, store: Arc<dyn Space>) -> Arc<dyn Space> {
        match &self.epochs {
            Some(epochs) => Arc::new(StoreWrites {
                inner: store,
                epochs: Arc::clone(epochs),
                graph: self.graph.clone(),
            }),
            None => store,
        }
    }
}

/// The store, observed for writes that can move a pending count — see
/// [`CachedReads::observe_store_writes`]. Transparent in the topology, for the reason
/// [`CachedReads`] is.
struct StoreWrites {
    inner: Arc<dyn Space>,
    epochs: Arc<Epochs>,
    graph: Option<String>,
}

impl StoreWrites {
    /// Whether a request through the store can write browse's graph: a mutating verb whose
    /// `graph` argument is absent (an unscoped update or load can write anything) or names
    /// browse's graph.
    fn can_write_browse(&self, request: &Request) -> bool {
        if matches!(request.verb, Verb::Source | Verb::Exists | Verb::Meta) {
            return false;
        }
        match request.args.get("graph") {
            Some(ikigai_core::ArgRef::Inline(bytes)) => {
                std::str::from_utf8(bytes).ok() == self.graph.as_deref()
            }
            _ => true,
        }
    }
}

impl Space for StoreWrites {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        let resolution = self.inner.resolve(request, scope);
        if !self.can_write_browse(request) {
            return resolution;
        }
        let epochs = Arc::clone(&self.epochs);
        resolution.map_endpoint(move |endpoint| {
            Arc::new(Touching {
                inner: endpoint,
                epochs: Arc::clone(&epochs),
                touch: Touch::All,
            }) as Arc<dyn Endpoint>
        })
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        self.inner.entries()
    }

    fn id(&self) -> Option<Iri> {
        self.inner.id()
    }

    fn topology(&self) -> Topology {
        self.inner.topology()
    }
}

impl Space for CachedReads {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        let resolution = self.resolve_touching(request, scope);
        match &self.reviews {
            Some(activity) if is_review(request) => {
                let activity = Arc::clone(activity);
                resolution.map_endpoint(move |endpoint| {
                    Arc::new(Reviewed {
                        inner: endpoint,
                        activity: Arc::clone(&activity),
                    }) as Arc<dyn Endpoint>
                })
            }
            _ => resolution,
        }
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        // The catalog is browse's, unchanged.
        self.inner.entries()
    }

    fn id(&self) -> Option<Iri> {
        self.inner.id()
    }

    fn topology(&self) -> Topology {
        self.inner.topology()
    }
}

/// Whether `request` is a review pass: a Source of `urn:repo:{root}:review:{path}`.
///
/// ⚠ Only that family. `judge-finding:{id}` is the backfill's OWN call (counting it would
/// make the backfill yield to itself forever), and the explain and PR families spend the same
/// model without being review passes.
fn is_review(request: &Request) -> bool {
    request.verb == Verb::Source
        && split_repo_iri(request.target.as_str())
            .is_some_and(|(_, rest)| rest.starts_with("review:"))
}

/// One review through the family, counted in flight while it runs — see
/// [`CachedReads::observing_reviews`].
struct Reviewed {
    inner: Arc<dyn Endpoint>,
    activity: Arc<crate::trigger::Activity>,
}

#[async_trait]
impl Endpoint for Reviewed {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let _counted = self.activity.reviewing();
        self.inner.invoke(inv).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn describe(&self) -> Description {
        self.inner.describe()
    }
}

impl CachedReads {
    /// The family's own resolution, with the badge's epochs touched by a write and a cached
    /// read hung on its thread — everything [`cached_reads`] describes.
    fn resolve_touching(&self, request: &Request, scope: &Scope) -> Resolution {
        let resolution = self.inner.resolve(request, scope);
        if let (Some(epochs), Some(touch)) = (&self.epochs, touch_for(request)) {
            let epochs = Arc::clone(epochs);
            return resolution.map_endpoint(move |endpoint| {
                Arc::new(Touching {
                    inner: endpoint,
                    epochs: Arc::clone(&epochs),
                    touch: touch.clone(),
                }) as Arc<dyn Endpoint>
            });
        }
        match self.thread_for(request) {
            Some(thread) => resolution.map_endpoint(move |endpoint| {
                Arc::new(Cached {
                    inner: endpoint,
                    thread: thread.clone(),
                }) as Arc<dyn Endpoint>
            }),
            None => resolution,
        }
    }

    /// The golden thread this request's answer depends on, when it is one of the reads this
    /// server caches. See [`cached_reads`] for the table.
    fn thread_for(&self, request: &Request) -> Option<String> {
        if request.verb != Verb::Source || !request.args.is_empty() {
            return None;
        }
        let (root, rest) = split_repo_iri(request.target.as_str())?;
        let ignore = self.roots.get(root)?;
        match rest {
            "tree" => return Some(root_thread(root)),
            "hash" | "state" => return Some(wide_thread(root)),
            _ => {}
        }
        let (family, encoded) = rest.split_once(':')?;
        let rel = decode_rel(encoded)?;
        match family {
            // A listing's entries are the directory's children: cached only when a change to
            // any of them cuts, which is when the directory itself is not ignored.
            "tree" => (!in_build_output(&rel) && ignore.classify(&rel, true) == Seen::Visible)
                .then(|| root_thread(root)),
            // A file's own change must cut: visible, or an ignored entry in a visible
            // directory.
            "file" => matches!(ignore.classify(&rel, false), Seen::Visible | Seen::Edge)
                .then(|| root_thread(root)),
            // A hash sees everything below the path but build output, which the wide thread
            // cuts for — unless the path is IN build output.
            "hash" => (!in_build_output(&rel)).then(|| wide_thread(root)),
            _ => None,
        }
    }
}

/// A `{path}` segment of a browse IRI decoded back to a root-relative path, the inverse of
/// `ikigai-browse`'s percent-encoding. `None` — and so no cache — for anything that is not a
/// plain relative path once decoded: a malformed escape, non-UTF-8, `..`, or a leading `/`.
fn decode_rel(encoded: &str) -> Option<PathBuf> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let rel = PathBuf::from(String::from_utf8(out).ok()?);
    rel.components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
        .then_some(rel)
}

/// Which roots' pending counts a request through the family may move — see
/// [`cached_reads`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Touch {
    /// One root, named by the request.
    Root(String),
    /// One finding, whose root the badge may have seen.
    Finding(String),
    /// Somewhere; every root.
    All,
}

/// The [`Touch`] a request makes, or `None` for a read that changes nothing.
fn touch_for(request: &Request) -> Option<Touch> {
    let target = request.target.as_str();
    let root = || split_repo_iri(target).map(|(root, _)| Touch::Root(root.to_string()));
    match request.verb {
        Verb::Exists | Verb::Meta => None,
        // A review pass mints findings during a READ — and judges the serious ones; and the
        // backfill's `judge-finding:{id}` archives a verdict during a read too. A verdict now
        // moves the badge's count, since the Queue hides what the judge refuted (ledger #704),
        // so both touch the root they name.
        Verb::Source => split_repo_iri(target)
            .filter(|(_, rest)| rest.starts_with("review") || rest.starts_with("judge-finding:"))
            .and_then(|_| root()),
        _ => {
            if let Some(id) = target.strip_prefix(crate::queue::FINDING_PREFIX) {
                if !id.is_empty() && !id.contains(':') {
                    return Some(Touch::Finding(id.to_string()));
                }
            }
            // An annotation's own `target` argument names its file, and so its root.
            let annotated = request.args.get("target").and_then(|arg| match arg {
                ikigai_core::ArgRef::Inline(bytes) => std::str::from_utf8(bytes).ok(),
                _ => None,
            });
            Some(
                annotated
                    .and_then(split_repo_iri)
                    .map(|(root, _)| Touch::Root(root.to_string()))
                    .or_else(root)
                    .unwrap_or(Touch::All),
            )
        }
    }
}

/// One write through the family, touching the badge's epochs once it has run.
struct Touching {
    inner: Arc<dyn Endpoint>,
    epochs: Arc<Epochs>,
    touch: Touch,
}

#[async_trait]
impl Endpoint for Touching {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let answer = self.inner.invoke(inv).await;
        // Success or not: a refused write may still have written part of what it meant to.
        match &self.touch {
            Touch::Root(root) => self.epochs.touch(root),
            Touch::Finding(id) => self.epochs.touch_finding(id),
            Touch::All => self.epochs.touch_all(),
        }
        answer
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn describe(&self) -> Description {
        self.inner.describe()
    }
}

/// `urn:repo:{root}:{rest}` split into its two halves. `None` for anything else — including
/// `ikigai-repo`'s own `urn:repo:status`, which has no second colon after the prefix.
fn split_repo_iri(target: &str) -> Option<(&str, &str)> {
    target.strip_prefix("urn:repo:")?.split_once(':')
}

/// One browse read, cached under its root's thread.
struct Cached {
    inner: Arc<dyn Endpoint>,
    thread: String,
}

#[async_trait]
impl Endpoint for Cached {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        Ok(self
            .inner
            .invoke(inv)
            .await?
            .cacheable()
            .depends_on(self.thread.as_str()))
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn describe(&self) -> Description {
        // Unchanged — including the summaries that say these reads are live. They still are:
        // what makes them so is now a watcher rather than a recomputation, and a contract
        // that started describing this server's caching would be describing the wrong layer.
        self.inner.describe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikigai_core::{ArgRef, Iri};

    /// A root named `core` over a scratch directory whose `.gitignore` hides `book/`.
    fn space() -> (CachedReads, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("a scratch root");
        std::fs::write(dir.path().join(".gitignore"), "/book\n*.log\n").expect(".gitignore");
        let dir_path = dir.path().canonicalize().expect("canonical");
        let watched = vec![Watched {
            name: "core".to_string(),
            ignore: Arc::new(RootIgnore::load(&dir_path)),
            dir: dir_path,
        }];
        (cached_reads(EndpointSpace::new(), &watched, None), dir)
    }

    fn request(verb: Verb, iri: &str) -> Request {
        Request::new(verb, Iri::parse(iri).expect("a test IRI"))
    }

    #[test]
    fn the_filesystem_reads_of_a_watched_root_are_cached() {
        let (space, _dir) = space();
        for (iri, thread) in [
            ("urn:repo:core:tree", "urn:iki:gonk:browse:root:core"),
            ("urn:repo:core:tree:src", "urn:iki:gonk:browse:root:core"),
            (
                "urn:repo:core:file:src/lib.rs",
                "urn:iki:gonk:browse:root:core",
            ),
            (
                "urn:repo:core:file:src%20dir/a%2Bb.rs",
                "urn:iki:gonk:browse:root:core",
            ),
            // An ignored file in a listed directory: its change still cuts (an `Edge`).
            (
                "urn:repo:core:file:src/debug.log",
                "urn:iki:gonk:browse:root:core",
            ),
            // The two that can see what a `.gitignore` hides hang from the wide thread.
            ("urn:repo:core:hash", "urn:iki:gonk:browse:root:core:wide"),
            (
                "urn:repo:core:hash:src",
                "urn:iki:gonk:browse:root:core:wide",
            ),
            (
                "urn:repo:core:hash:book",
                "urn:iki:gonk:browse:root:core:wide",
            ),
            ("urn:repo:core:state", "urn:iki:gonk:browse:root:core:wide"),
        ] {
            assert_eq!(
                space.thread_for(&request(Verb::Source, iri)).as_deref(),
                Some(thread),
                "{iri}"
            );
        }
    }

    /// ★ The other half of ledger #667: what the watch ignores, no cached read may see.
    #[test]
    fn a_read_of_what_the_watch_ignores_is_not_cached() {
        let (space, _dir) = space();
        for iri in [
            // Build output: no change under it cuts anything.
            "urn:repo:core:tree:target",
            "urn:repo:core:tree:target/debug",
            "urn:repo:core:file:target/debug/build.log",
            "urn:repo:core:hash:target",
            "urn:repo:core:hash:crates/a/target",
            "urn:repo:core:file:.git/HEAD",
            "urn:repo:core:tree:.git",
            // Inside a directory the `.gitignore` hides: only the wide thread is cut there.
            "urn:repo:core:tree:book",
            "urn:repo:core:file:book/index.html",
            // Not a plain relative path once decoded.
            "urn:repo:core:file:../outside",
            "urn:repo:core:file:%2Fetc/passwd",
        ] {
            assert_eq!(space.thread_for(&request(Verb::Source, iri)), None, "{iri}");
        }
    }

    /// Which roots a request through the family touches for the badge.
    #[test]
    fn a_write_touches_the_root_it_names_and_a_read_touches_nothing() {
        let touch = |verb: Verb, iri: &str, target: Option<&str>| {
            let mut request = request(verb, iri);
            if let Some(target) = target {
                request = request.with_arg("target", ArgRef::Inline(target.as_bytes().to_vec()));
            }
            touch_for(&request)
        };
        let root = |r: &str| Some(Touch::Root(r.to_string()));
        assert_eq!(touch(Verb::Source, "urn:repo:core:file:a.rs", None), None);
        assert_eq!(touch(Verb::Source, "urn:repo:core:findings", None), None);
        assert_eq!(touch(Verb::Meta, "urn:iki:finding:f1", None), None);
        assert_eq!(
            touch(Verb::Source, "urn:repo:core:review:a.rs", None),
            root("core")
        );
        // The backfill archives a verdict during a read (ledger #704); asking the judge about
        // a claim by argument archives nothing.
        assert_eq!(
            touch(Verb::Source, "urn:repo:core:judge-finding:f1", None),
            root("core")
        );
        assert_eq!(touch(Verb::Source, "urn:repo:core:judge:a.rs", None), None);
        assert_eq!(
            touch(Verb::Sink, "urn:iki:finding:f1", None),
            Some(Touch::Finding("f1".to_string()))
        );
        assert_eq!(
            touch(
                Verb::Sink,
                "urn:iki:annotation",
                Some("urn:repo:core:file:a.rs")
            ),
            root("core")
        );
        assert_eq!(
            touch(Verb::Delete, "urn:iki:annotation:a1", None),
            Some(Touch::All)
        );
        assert_eq!(
            touch(Verb::Sink, "urn:repo:core:anything", None),
            root("core")
        );
    }

    #[test]
    fn nothing_else_is() {
        let (space, _dir) = space();
        for iri in [
            // Another root's name — unwatched, so uncached: the fail-closed half.
            "urn:repo:other:tree",
            // Store-derived, and rewritten during a read when the anchor drifts.
            "urn:repo:core:annotations",
            "urn:repo:core:annotations:src/lib.rs",
            "urn:iki:annotation:n",
            // `gh`, not the disk.
            "urn:repo:core:prs",
            "urn:repo:core:pr:12",
            // The archive is the cache for these — see the table.
            "urn:repo:core:explain",
            "urn:repo:core:explain:src/lib.rs",
            "urn:repo:core:explain-versions:src/lib.rs",
            "urn:repo:core:review:src/lib.rs",
            "urn:repo:core:pr:12:explain",
            // browse declares this one itself, under its own threads.
            "urn:repo:style",
            // ikigai-repo's facades.
            "urn:repo:status",
            "urn:repo:pr:list",
        ] {
            assert_eq!(space.thread_for(&request(Verb::Source, iri)), None, "{iri}");
        }
    }

    /// ★ An argument means a face whose inputs this thread does not track — the HTML and
    /// `annotations=include` faces read the annotation overlay, which a Source can rewrite.
    #[test]
    fn a_read_with_any_argument_is_not_cached() {
        let (space, _dir) = space();
        let html = request(Verb::Source, "urn:repo:core:file:src/lib.rs")
            .with_arg("as", ArgRef::Inline(b"text/html".to_vec()));
        assert_eq!(space.thread_for(&html), None);
    }

    #[test]
    fn a_meta_or_a_write_is_not_cached() {
        let (space, _dir) = space();
        for verb in [Verb::Meta, Verb::Sink, Verb::Delete, Verb::Exists] {
            assert_eq!(
                space.thread_for(&request(verb, "urn:repo:core:file:a.txt")),
                None,
                "{verb:?}"
            );
        }
    }

    /// ★ The derivation, walked over BOTH arms: whatever graph this server chooses, the
    /// promise it hands `ikigai-store` names exactly that graph and no other.
    ///
    /// This is the compile-time half of ledger #282 — one value, two readings, checked
    /// against each other rather than against a comment. The runtime half (that browse
    /// actually writes where the choice says) is `tests/browse.rs`, because it needs a real
    /// store and a real write.
    #[test]
    fn graph_choice_and_promise_cannot_disagree() {
        let promised = |graph: &Graph| {
            graph
                .sharer_writes()
                .named_graphs()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };

        let default = Graph::TheDefault;
        assert_eq!(default.named(), None);
        assert!(
            promised(&default).is_empty(),
            "the default graph has no IRI to name, so a declaration about it is the empty one"
        );

        let iri = "urn:iki:gonk:browse:graph";
        let named = Graph::Named(NamedNode::new(iri).expect("a test IRI"));
        assert_eq!(named.named().map(NamedNode::as_str), Some(iri));
        assert_eq!(
            promised(&named),
            vec![iri.to_string()],
            "the named choice is promised, and nothing else is — a promise wider than the \
             choice would de-cache a graph browse never touches, and a narrower one is the \
             silent-staleness failure"
        );
    }

    /// ★ What this server ships, said out loud where a diff can see it — **as a literal**,
    /// because this string is not a setting: it is where the quads on disk are.
    ///
    /// ⚠ This test going red is not a bug — it is the signal that someone changed the graph,
    /// and that the four obligations on [`Graph`]'s docs are owed AGAIN. In particular
    /// obligation 1: a store migrated into the old name strands every browse quad under the
    /// new one, silently, exactly as an unmigrated store does.
    ///
    /// It also parses the literal, which is what lets [`Graph::chosen`] use `new_unchecked`.
    #[test]
    fn the_choice_this_server_ships_is_its_own_named_graph() {
        assert!(
            NamedNode::new(CHOSEN_GRAPH).is_ok(),
            "`{CHOSEN_GRAPH}` must be an IRI — `chosen()` builds it unchecked"
        );
        assert_eq!(
            Graph::chosen().named().map(NamedNode::as_str),
            Some("urn:iki:browse:graph:default"),
            "the graph gonk's store is migrated into"
        );
    }

    /// The arm this server no longer ships, kept honest: a host that chooses the default
    /// graph promises nothing named, writes no `Mount::graph` call, and — the half that
    /// matters for a rollback — has NOTHING stranded by its own choice once its quads are
    /// there.
    #[test]
    fn the_default_arm_still_promises_only_the_default_graph() {
        let default = Graph::TheDefault;
        assert_eq!(default.named(), None);
        assert_eq!(default.graph_name(), GraphName::DefaultGraph);
        assert_eq!(default.sharer_writes().named_graphs().len(), 0);
    }

    /// A quad shaped like one `ikigai-browse` writes — a subject under one of its minted
    /// prefixes — in `graph`.
    fn browse_quad(store: &Store, graph: GraphName, subject: &str) {
        let quad = oxigraph::model::Quad::new(
            NamedNode::new(subject).expect("a subject IRI"),
            NamedNode::new("https://ikigai-rs.dev/ns#repo").expect("a predicate"),
            oxigraph::model::Literal::new_simple_literal("demo"),
            graph,
        );
        store.insert(quad.as_ref()).expect("the quad lands");
    }

    /// ★ Obligation 1's tripwire, on the case it exists for: a store written by a binary that
    /// had not taken the decision, read by one that has.
    ///
    /// The count is what an operator will see in the banner, and it is the same function the
    /// migration's dry run prints — so this test is also the statement that the two agree.
    #[test]
    fn quads_left_in_the_default_graph_are_counted_as_invisible() {
        let store = Store::new().expect("an in-memory store");
        let chosen = Graph::chosen();
        assert_eq!(
            unmigrated_quads(&store, &chosen),
            Ok(0),
            "an empty store strands nothing"
        );

        // What a pre-decision gonk wrote: an explanation in the default graph.
        browse_quad(
            &store,
            GraphName::DefaultGraph,
            "urn:ikigai:browse:explain:gonk:sha256:abc:note-v1@m:README.md",
        );
        // …and an annotation, under the other prefix browse mints.
        browse_quad(&store, GraphName::DefaultGraph, "urn:iki:annotation:n1");
        assert_eq!(
            unmigrated_quads(&store, &chosen),
            Ok(2),
            "both are browse-minted, both are outside the chosen graph, and neither is \
             visible to a read confined to it"
        );

        // A quad nobody's migration should move: not browse's subject.
        browse_quad(
            &store,
            GraphName::DefaultGraph,
            "urn:iki:ledger:default:item:x",
        );
        assert_eq!(
            unmigrated_quads(&store, &chosen),
            Ok(2),
            "the count is browse-owned quads, not everything in the default graph — a \
             migration would not move this one and the banner must not claim it would"
        );

        // Where the same quads belong after the migration.
        let migrated = Store::new().expect("an in-memory store");
        browse_quad(&migrated, chosen.graph_name(), "urn:iki:annotation:n1");
        assert_eq!(unmigrated_quads(&migrated, &chosen), Ok(0));
    }

    /// The tripwire works on the arm this server does not ship, which is what makes it a
    /// check on the CHOICE rather than on one value of it: a host that went back to the
    /// default graph would have everything stranded in the named one.
    #[test]
    fn the_tripwire_reads_a_rollback_the_same_way() {
        let store = Store::new().expect("an in-memory store");
        browse_quad(
            &store,
            Graph::chosen().graph_name(),
            "urn:iki:annotation:n1",
        );
        assert_eq!(unmigrated_quads(&store, &Graph::chosen()), Ok(0));
        assert_eq!(
            unmigrated_quads(&store, &Graph::TheDefault),
            Ok(1),
            "rolling the binary back without rolling the data back strands it just as badly"
        );
    }

    #[test]
    fn a_root_name_that_would_read_as_another_family_is_refused() {
        assert!(check_root_name("core").is_ok());
        assert!(check_root_name("pr").is_err(), "ikigai-repo's own segment");
        assert!(check_root_name("style").is_err(), "browse's own row");
        assert!(check_root_name("a:b").is_err(), "would forge an IRI");
        assert!(check_root_name("").is_err());
    }
}
