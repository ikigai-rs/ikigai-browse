//! The review Queue (ledger #444): what it offers, to whom, and where its menus come from.
//!
//! Five things are gonk's to get wrong on this page, and each is a test here:
//!
//! - [`an_anonymous_loopback_caller_can_neither_read_nor_decide_a_finding`] — ⚠ **the
//!   measurement**, not an assumption. Brian's rule is *"nothing gets published to Gonk
//!   except by the human"*, and an earlier arc measured that an anonymous loopback caller
//!   gets `can-write = true` on the LEDGER. This asks the kernel what that same caller can do
//!   to a FINDING, in both directions.
//! - [`the_queue_page_offers_no_decision_to_a_caller_that_could_not_make_one`] — an offer you
//!   refuse is a worse UI than no offer, and it teaches people to ignore refusals.
//! - [`the_state_nav_is_the_findings_contracts_own_one_of`] — the words on the page ARE the
//!   words in the contract, asked of the contract rather than compared to a list.
//! - [`no_severity_word_is_written_down_in_this_crate`] — the anti-drift assertion with teeth:
//!   the severity set has three copies in `ikigai-browse` served from ONE constant, and a
//!   fourth here would be the one that goes stale silently.
//! - [`the_intray_depth_tells_absent_empty_and_unreadable_apart`] — #446's shape on the one
//!   page whose job is to show process state.
//!
//! Every store here is `in_memory_shared_declaring`, so these run beside a live gonk holding
//! `~/.ikigai/store`.

use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Representation, Request, Verb};
use ikigai_gonk::grants::{browse_graph_grants, grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::queue;
use ikigai_gonk::trigger::{Depth, Trigger};
use ikigai_gonk::{browse, compose_with, doors, quic, web};
use ikigai_store::DurableStore;
use tempfile::TempDir;

/// The root every fixture here configures.
const ROOT: &str = "demo";

/// A scratch repository: one file, no git.
fn scratch_root() -> TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir(dir.path().join("src")).expect("src/");
    std::fs::write(dir.path().join("src/lib.rs"), "the first version\n").expect("lib.rs");
    dir
}

fn roots(dir: &TempDir) -> Vec<(String, PathBuf)> {
    vec![(ROOT.to_string(), dir.path().to_path_buf())]
}

/// The HTTP door's kernel over the composition `main` builds with a browse root — gonk's
/// pages in front of a hub carrying the browse family over one declared dataset.
///
/// No watcher: `ikigai-browse`'s filesystem reads are live and uncacheable exactly as it
/// declares them, and nothing here reads a file twice. The `review` argument is the trigger
/// the Queue page reports the depth of.
///
/// ⚠ A configured trigger composes the trigger's SPACES too, because the page reads the
/// depth through `urn:iki:gonk:review:depth` rather than around it (ledger #466). A fixture
/// that left them out would exercise only the "could not be read" arm, which is exactly the
/// arm that must never be reached in ordinary use.
fn door(dir: &TempDir, review: Option<Trigger>) -> (Kernel, TempDir) {
    door_watching(
        dir,
        review,
        Arc::new(ikigai_gonk::trigger::Activity::default()),
        false,
    )
}

/// The same door, with the pass counters held by the CALLER and the armed flag its own
/// argument — so a test can put a pass in flight, end it, and ask the badge what it says
/// about each, which is the whole of ledger #469's first half.
fn door_watching(
    dir: &TempDir,
    review: Option<Trigger>,
    activity: Arc<ikigai_gonk::trigger::Activity>,
    armed: bool,
) -> (Kernel, TempDir) {
    let graph = browse::Graph::chosen();
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store that declares where its sharer writes");
    let wired = browse::wire(roots(dir), handle, &[], None, &graph);
    let trigger_spaces = match &review {
        Some(t) => ikigai_gonk::trigger::space(t, activity, armed),
        None => Vec::new(),
    };
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        trigger_spaces,
        None,
    ));
    let config = tempfile::tempdir().expect("a config home");
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: vec![ROOT.to_string()],
        passkeys: Arc::new(Passkeys::new(
            quic::Layout::in_config_home(config.path()),
            1060,
        )),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
    });
    (doors::http_kernel(hub, web::space(face)), config)
}

/// ★ The grant this server hands an ANONYMOUS loopback caller, computed the way
/// `main` computes it — the configured ledgers at write authority, and nothing else.
fn anonymous() -> Capability {
    Capability::scoped(
        grants_for_all(&["default".to_string()], Authority::Write).expect("the ledger's tokens"),
    )
}

/// A grant that can read every root and publish: what `grants.json`'s `brian` identity holds.
fn reviewer() -> Capability {
    let mut scopes = vec![
        ikigai_browse::CAP_WILDCARD.to_string(),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ];
    scopes.extend(grants_for_all(&["default".to_string()], Authority::Write).expect("tokens"));
    scopes.extend(browse_graph_grants(Authority::Write).expect("a named browse graph"));
    Capability::scoped(scopes)
}

/// The same, WITHOUT the publishing token: a caller who may look and not decide.
fn onlooker() -> Capability {
    let mut scopes = vec![ikigai_browse::CAP_WILDCARD.to_string()];
    scopes.extend(grants_for_all(&["default".to_string()], Authority::Write).expect("tokens"));
    scopes.extend(browse_graph_grants(Authority::Read).expect("a named browse graph"));
    Capability::scoped(scopes)
}

fn issue(
    kernel: &Kernel,
    verb: Verb,
    iri: &str,
    args: &[(&str, &str)],
    cap: &Capability,
) -> ikigai_core::Result<Representation> {
    let mut request = Request::new(verb, Iri::parse(iri).expect("an IRI"));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    block_on(kernel.issue(request, cap))
}

/// One page, as HTML.
fn page(kernel: &Kernel, args: &[(&str, &str)], cap: &Capability) -> String {
    let answer = issue(kernel, Verb::Source, queue::QUEUE_IRI, args, cap)
        .unwrap_or_else(|e| panic!("the queue page: {e}"));
    String::from_utf8(answer.bytes).expect("utf-8")
}

// ------------------------------------------------------------------ the measurement

/// ⚠⚠ **The brief's question, asked of the kernel rather than reasoned about.**
///
/// `~/.config/ikigai/gonk/grants.json` holds only the `brian` identity, which carries
/// `urn:cap:annotate` — so if an anonymous caller could publish, the authority would be
/// coming from somewhere other than that file, and that would be a breach of the standing
/// rule rather than a UI question. It cannot: the anonymous grant is four ledger tokens per
/// configured ledger, `urn:repo:{repo}:findings` requires `urn:cap:browse:read:*`, and the
/// finding Sink requires that PLUS `urn:cap:annotate`.
///
/// ★ Both halves matter and the first is the one that is easy to skip: a caller who cannot
/// even READ the queue cannot be shown a row to decide about, so the page's refusal is not
/// the only thing standing between an anonymous browser and a published finding.
#[test]
fn an_anonymous_loopback_caller_can_neither_read_nor_decide_a_finding() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let anonymous = anonymous();

    let read = issue(
        &door,
        Verb::Source,
        &format!("urn:repo:{ROOT}:findings"),
        &[("as", "application/json")],
        &anonymous,
    );
    assert!(
        matches!(read, Err(ikigai_core::Error::Denied(_))),
        "an anonymous loopback caller must not READ the queue: {read:?}"
    );

    // The id is a position hash; this one names no finding, and that is deliberate — a
    // capability refusal must arrive BEFORE existence is consulted, or the refusal leaks
    // whether a finding is there.
    let decide = issue(
        &door,
        Verb::Sink,
        "urn:iki:finding:0123456789abcdef01234567",
        &[("decision", "publish"), ("severity", "minor")],
        &anonymous,
    );
    assert!(
        matches!(decide, Err(ikigai_core::Error::Denied(_))),
        "an anonymous loopback caller must not PUBLISH: {decide:?}"
    );

    // …and not through gonk's own form adapter either, which declares the same token so the
    // kernel refuses before dispatch rather than one hop in.
    let form = issue(
        &door,
        Verb::Sink,
        queue::DECIDE_IRI,
        &[(
            "content",
            "id=0123456789abcdef01234567&decision=publish&severity=minor",
        )],
        &anonymous,
    );
    assert!(
        matches!(form, Err(ikigai_core::Error::Denied(_))),
        "the queue's form adapter must refuse an anonymous caller: {form:?}"
    );
}

// ---------------------------------------------------------------------- the offer

/// An offer you refuse is a worse UI than no offer: no Queue link, no decision form, and a
/// sentence saying which token is missing.
#[test]
fn the_queue_page_offers_no_decision_to_a_caller_that_could_not_make_one() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);

    let anonymous = page(&door, &[], &anonymous());
    assert!(
        !anonymous.contains(">Queue</a>"),
        "the header must not offer a Queue link to a caller who cannot decide:\n{anonymous}"
    );
    assert!(
        !anonymous.contains("class='decide'"),
        "no decision form for a caller who cannot submit one:\n{anonymous}"
    );
    assert!(
        anonymous.contains(ikigai_browse::CAP_PREFIX),
        "the refusal must name the token that is missing:\n{anonymous}"
    );

    // A caller who may READ every root and not publish: the rows are legible, the form is
    // not drawn, and the page says which of the two authorities it is short of.
    let onlooker = page(&door, &[], &onlooker());
    assert!(
        !onlooker.contains(">Queue</a>"),
        "ledger #444 gates the LINK on being able to decide:\n{onlooker}"
    );
    assert!(
        onlooker.contains(ikigai_browse::CAP_ANNOTATE),
        "a read-only posture must name `{}`:\n{onlooker}",
        ikigai_browse::CAP_ANNOTATE
    );

    let reviewer = page(&door, &[], &reviewer());
    assert!(
        // ⚠ `>Queue<`, not `>Queue</a>`: the link now carries the live depth badge
        // inside it (ledger #466), so the close tag is no longer adjacent to the label.
        reviewer.contains(">Queue<"),
        "a caller holding both authorities is offered the page:\n{reviewer}"
    );
    assert!(
        !reviewer.contains(ikigai_browse::CAP_ANNOTATE),
        "…and is not told about a token it holds:\n{reviewer}"
    );
}

/// ⚠ **"No pending findings" and "this page failed" must not look alike** (ledger #446).
/// An empty queue says so in as many words, and says it about a named scope.
#[test]
fn an_empty_queue_says_so_affirmatively() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let rendered = page(&door, &[], &reviewer());
    assert!(
        rendered.contains("No pending findings in") && rendered.contains("Nothing is waiting"),
        "an empty queue must say so:\n{rendered}"
    );
    // ★ And the sentence that stops the page reading like a gate. The phrase "queued for
    // review" reads like one to anyone who has used one, and this pipeline blocks nothing.
    assert!(
        rendered.contains("A queue, not a gate"),
        "the page must say it blocks nothing:\n{rendered}"
    );
}

// ------------------------------------------------------------------- the contract

/// ★★ The state nav is the findings resource's OWN `one_of`, asked of the contract.
#[test]
fn the_state_nav_is_the_findings_contracts_own_one_of() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let hub = Iri::parse(format!("urn:repo:{ROOT}:findings")).expect("an IRI");
    let declared: Vec<String> = door
        .describe(&hub)
        .expect("the findings resource describes itself")
        .action_specs()
        .into_iter()
        .find(|spec| spec.verb == Verb::Source)
        .expect("a Source action")
        .inputs
        .iter()
        .find(|input| input.name == "state")
        .expect("a `state` input")
        .one_of
        .clone();
    assert!(
        !declared.is_empty(),
        "the findings contract must declare its states as a closed set"
    );

    let rendered = page(&door, &[], &reviewer());
    for state in &declared {
        assert!(
            rendered.contains(&format!(">{state}</a>")),
            "the state nav must carry `{state}`, which the contract declares:\n{rendered}"
        );
    }

    // …and a state the contract does NOT declare is refused, naming the set.
    let refused = issue(
        &door,
        Verb::Source,
        queue::QUEUE_IRI,
        &[("state", "whenever")],
        &reviewer(),
    );
    match refused {
        Err(ikigai_core::Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "state");
            for state in &declared {
                assert!(detail.contains(state.as_str()), "{detail}");
            }
        }
        other => panic!("an undeclared state must be refused naming the set: {other:?}"),
    }
}

/// ★★ **The anti-drift assertion, with the contract as its oracle.**
///
/// `ikigai-browse` serves ONE constant to the Sink's `one_of`, to the words the review prompt
/// hands the model, and to its own menu, so those three cannot disagree. A fourth copy here
/// would be the one that goes stale in silence: the page would go on offering a word the
/// resource had stopped accepting, and the refusal would arrive at the click.
///
/// So: no severity word the contract declares appears as a token in this crate's page code or
/// its stylesheet. ⚠ `web/gonk.css` is deliberately exempt and it is worth saying why — a
/// selector like `.badge.sev.critical` is a COLOUR, and a severity the sheet has no rule for
/// still renders with its own word in its own badge. A missing rule is a missing emphasis; a
/// missing menu entry is an option a human cannot choose.
#[test]
fn no_severity_word_is_written_down_in_this_crate() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let declared = queue::one_of(
        &door,
        "urn:iki:finding:0123456789abcdef01234567",
        Verb::Sink,
        "severity",
    )
    .expect("the finding Sink declares its severity set");
    assert!(
        declared.len() > 1,
        "a closed set of more than one: {declared:?}"
    );

    for (what, source) in [
        ("src/queue.rs", include_str!("../src/queue.rs")),
        ("web/gonk.xsl", include_str!("../web/gonk.xsl")),
    ] {
        for word in &declared {
            assert!(
                !contains_token(source, word),
                "`{what}` names the severity `{word}`. The set lives in ONE place \
                 (`ikigai-browse`'s own constant, read through the contract); a copy here is \
                 the one that goes stale silently."
            );
        }
    }
}

/// Whether `text` contains `word` as a whole alphanumeric token — so `informative` does not
/// count as a use of `info`.
fn contains_token(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| token == word)
}

// --------------------------------------------------------------------- the intray

/// ⚠ **Three answers, and two of them are not a number.** "No queue configured", "the queue
/// is empty" and "the queue could not be read" are different statements; rendering `0` for
/// all three is ledger #446's shape on the one page whose job is to show process state.
#[test]
fn the_intray_depth_tells_absent_empty_and_unreadable_apart() {
    assert_eq!(ikigai_gonk::trigger::depth(None), Depth::NotConfigured);

    let spaces = tempfile::tempdir().expect("a spaces tree");
    let trigger = Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: false,
    };

    // Configured and never prepared: the inbox is not there, and that is NOT zero.
    assert!(
        matches!(
            ikigai_gonk::trigger::depth(Some(&trigger)),
            Depth::Unreadable(_)
        ),
        "a missing inbox is unreadable, not empty"
    );

    ikigai_gonk::trigger::prepare(&trigger).expect("the tree");
    assert_eq!(
        ikigai_gonk::trigger::depth(Some(&trigger)),
        Depth::Counted {
            inbox: 0,
            outbox: 0,
            error: 0
        }
    );

    for n in 0..3 {
        std::fs::write(trigger.inbox().join(format!("{n}.tuple")), "x").expect("a tuple");
    }
    // A file that is not a tuple is not a queued request.
    std::fs::write(trigger.inbox().join("notes.txt"), "x").expect("a stray file");
    assert_eq!(
        ikigai_gonk::trigger::depth(Some(&trigger)),
        Depth::Counted {
            inbox: 3,
            outbox: 0,
            error: 0
        }
    );
}

/// The depth reaches the page as a SENTENCE, in all three shapes.
#[test]
fn the_queue_page_says_what_is_waiting() {
    let dir = scratch_root();

    let (no_queue, _c1) = door(&dir, None);
    let rendered = page(&no_queue, &[], &reviewer());
    assert!(
        rendered.contains("No review queue is configured"),
        "{rendered}"
    );

    let spaces = tempfile::tempdir().expect("a spaces tree");
    let trigger = Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: false,
    };
    ikigai_gonk::trigger::prepare(&trigger).expect("the tree");
    let (empty, _c2) = door(&dir, Some(trigger.clone()));
    let rendered = page(&empty, &[], &reviewer());
    assert!(rendered.contains("The review queue is empty"), "{rendered}");

    for n in 0..39 {
        std::fs::write(trigger.inbox().join(format!("{n}.tuple")), "x").expect("a tuple");
    }
    let (deep, _c3) = door(&dir, Some(trigger));
    let rendered = page(&deep, &[], &reviewer());
    assert!(
        rendered.contains("39 review requests are waiting"),
        "★ \"nothing has happened yet\" and \"39 still queued\" must not render \
         identically:\n{rendered}"
    );
}

// ------------------------------------------------------------- a real finding

/// The id is a POSITION in `ikigai-browse` (`sha256(pass ‖ start ‖ exact)`, 24 hex); here it
/// is just 24 hex, because nothing downstream re-derives it.
const FINDING: &str = "aaaabbbbccccddddeeee0001";

/// ★ **Plant one pending finding, through the store's own narrow door.**
///
/// A finding is normally minted by a review pass, which needs a model — so a test that
/// insisted on the real producer could not run in CI, and would be testing the model. What it
/// would test here anyway is gonk's rendering of a shape `ikigai-browse` documents: the quads
/// below are that module's own `store_annotation`, for `Family::Finding`.
///
/// ⚠ **It goes in through `urn:iki:store:graph-update` under `urn:cap:store:write:graph:`**,
/// which is a real authority an operator can mint (`passkey invite … --browse-graph write`)
/// and which the README already describes as "lets an identity mint or edit those quads
/// directly". So this is a fixture written the way a holder of that grant would write it, not
/// a back door around one.
fn plant_pending_finding(door: &Kernel, cap: &Capability) {
    let graph = browse::Graph::chosen()
        .named()
        .expect("this server writes browse's quads in a NAMED graph")
        .as_str()
        .to_string();
    let update = format!(
        r#"PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX oa: <http://www.w3.org/ns/oa#>
PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX sh: <http://www.w3.org/ns/shacl#>
PREFIX ik: <https://ikigai-rs.dev/ns#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
INSERT DATA {{ GRAPH <{graph}> {{
  <urn:iki:finding:{FINDING}> a prov:Entity ;
    dcterms:description "The bound truncates instead of refusing." ;
    dcterms:creator "a-test-reviewer" ;
    dcterms:created "2026-09-19T12:00:00Z"^^xsd:dateTime ;
    prov:wasGeneratedBy <urn:ikigai:browse:review:demo:src/lib.rs> ;
    sh:resultSeverity <urn:iki:severity:{SEVERITY}> ;
    ik:annotates <urn:repo:{ROOT}:file:src/lib.rs> ;
    ik:repo "{ROOT}" ;
    ik:path "src/lib.rs" ;
    ik:contentHash "sha256:planted" ;
    oa:hasSelector <urn:iki:finding:{FINDING}:selector:quote> ,
                   <urn:iki:finding:{FINDING}:selector:position> .
  <urn:iki:finding:{FINDING}:selector:quote> a oa:TextQuoteSelector ;
    oa:exact "first version" .
  <urn:iki:finding:{FINDING}:selector:position> a oa:TextPositionSelector ;
    oa:start "4"^^xsd:nonNegativeInteger ;
    oa:end "17"^^xsd:nonNegativeInteger .
}} }}"#
    );
    issue(
        door,
        Verb::Sink,
        "urn:iki:store:graph-update",
        &[("graph", &graph), ("content", &update)],
        cap,
    )
    .expect("the browse graph's write token plants the finding");
}

/// The severity this fixture proposes. ⚠ Not a member of a list held here: it is read back
/// out of the contract by [`a_planted_finding_renders_the_contracts_menu_and_publishes`]
/// before it is used, so this constant cannot disagree with the closed set.
const SEVERITY: &str = "major";

/// ★★ **The whole act, end to end: a pending finding, the menu the contract declares, the
/// human's decision, and the record it leaves.**
#[test]
fn a_planted_finding_renders_the_contracts_menu_and_publishes() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let reviewer = reviewer();

    let severities = queue::one_of(
        &door,
        &format!("urn:iki:finding:{FINDING}"),
        Verb::Sink,
        "severity",
    )
    .expect("the finding Sink declares its severity set");
    assert!(
        severities.iter().any(|s| s == SEVERITY),
        "this fixture proposes `{SEVERITY}`, which the contract must know: {severities:?}"
    );

    plant_pending_finding(&door, &reviewer);

    let pending = page(&door, &[], &reviewer);
    assert!(
        pending.contains("The bound truncates instead of refusing."),
        "the finding's body is the row:\n{pending}"
    );
    assert!(
        pending.contains("src/lib.rs") && pending.contains("a-test-reviewer"),
        "a finding reaches the code it is about, and says which pass found it:\n{pending}"
    );
    // ★ THE POINT OF THE ARC: every declared severity is an option, and nothing else is.
    for word in &severities {
        assert!(
            pending.contains(&format!(">{word}</option>")),
            "the menu must offer `{word}`, which the contract declares:\n{pending}"
        );
    }
    assert!(
        pending.contains("Publish to Gonk") && pending.contains("name='severity'"),
        "a caller holding the token is offered the form:\n{pending}"
    );
    // ⚠ And NO human rating is shown while the finding is pending. `effective_severity`
    // falls back to the model's proposal when there is no decision — correct for a consumer
    // wanting one number, and a lie on a badge labelled "human", on the one page whose whole
    // argument is that the two ratings are different things.
    assert!(
        !pending.contains("human:"),
        "a pending finding has no human rating to show:\n{pending}"
    );

    // …and a caller who may read and not decide sees the row and no form.
    let watched = page(&door, &[], &onlooker());
    assert!(
        watched.contains("The bound truncates instead of refusing."),
        "the rows are legible without the publishing token:\n{watched}"
    );
    assert!(
        !watched.contains("Publish to Gonk"),
        "…and the form is not drawn for a caller who could not submit it:\n{watched}"
    );

    // The human decides, through gonk's own form adapter, re-rating as the last severity the
    // contract offers rather than as a word chosen here.
    let human = severities.last().expect("a non-empty set").clone();
    let answer = issue(
        &door,
        Verb::Sink,
        queue::DECIDE_IRI,
        &[(
            "content",
            &format!(
                "id={FINDING}&decision=publish&severity={human}&content=Worth+keeping+\
                 as+a+note"
            ),
        )],
        &reviewer,
    )
    .expect("a holder of urn:cap:annotate publishes");
    let rendered = String::from_utf8(answer.bytes).expect("utf-8");
    assert!(
        rendered.contains("urn:iki:annotation:"),
        "publishing answers with the annotation it minted:\n{rendered}"
    );

    // ⚠ Both ratings survive: the model's proposal on the finding, the human's on the
    // decision. That is the only signal that could ever say whether the reviewer is
    // calibrated, and it is unrecoverable if the first write clobbers the proposal.
    let published = page(&door, &[("state", "published")], &reviewer);
    assert!(
        published.contains(&format!("model: {SEVERITY}")),
        "the model's PROPOSAL is not overwritten:\n{published}"
    );
    assert!(
        published.contains(&format!("human: {human}")),
        "the human's final rating is shown beside it:\n{published}"
    );
    assert!(
        published.contains("Worth keeping as a note"),
        "the reason is kept:\n{published}"
    );
    // ★ The asymmetry, said rather than hidden.
    assert!(
        published.contains("delete urn:iki:annotation:"),
        "undoing a publication is a separate, visible act, and the page says so:\n{published}"
    );

    // The pending queue is now affirmatively empty, and the row moved rather than vanished.
    let pending = page(&door, &[], &reviewer);
    assert!(pending.contains("No pending findings in"), "{pending}");
}

/// ⚠ **A decision is not overwritten, and the refusal is RENDERED rather than swallowed.**
///
/// htmx does not swap a non-2xx response, so returning the refusal as a status would make the
/// page silently ignore the most informative answer the system can give — "here is what is on
/// file". The capability layer has already refused; this is about how a person reads it.
#[test]
fn a_second_different_decision_is_refused_and_the_refusal_is_legible() {
    let dir = scratch_root();
    let (door, _config) = door(&dir, None);
    let reviewer = reviewer();
    plant_pending_finding(&door, &reviewer);

    let decide = |body: &str| -> String {
        let answer = issue(
            &door,
            Verb::Sink,
            queue::DECIDE_IRI,
            &[("content", body)],
            &reviewer,
        )
        .expect("the adapter renders rather than returning a status");
        String::from_utf8(answer.bytes).expect("utf-8")
    };

    decide(&format!(
        "id={FINDING}&decision=decline&severity={SEVERITY}"
    ));
    // The identical repeat: a double-clicked button is not an error.
    let again = decide(&format!(
        "id={FINDING}&decision=decline&severity={SEVERITY}"
    ));
    assert!(
        !again.contains("flash error"),
        "an identical repeat is a no-op:\n{again}"
    );
    // Anything that would CHANGE the record is refused, naming what is recorded.
    let changed = decide(&format!(
        "id={FINDING}&decision=publish&severity={SEVERITY}"
    ));
    assert!(
        changed.contains("flash error"),
        "a second, different decision must be refused:\n{changed}"
    );
}

/// ★★ **The live badge, rendered — the armed trigger's only liveness signal.**
///
/// Ledger [#466](http://localhost:1060/l/default/item/466). `SpaceReactor::watch()` catches
/// up at startup and then lives on a thread nothing else observes, and gonk runs no log
/// ([#383](http://localhost:1060/l/default/item/383)), so a depth that stops falling is the
/// whole symptom of a dead watcher. The header is where a person is already looking.
///
/// ⚠ The fragment is rendered through the SAME stylesheet as every page, and `xrust` is an
/// XSLT-1.0 subset — a template that silently produced nothing would leave a badge that is
/// permanently blank, which reads exactly like an empty queue. So this asserts the markup,
/// not just the absence of an error.
#[test]
fn the_header_badge_renders_the_depth_and_polls_for_it() {
    let dir = scratch_root();
    let spaces = tempfile::tempdir().expect("a spaces tree");
    let trigger = Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: false,
    };
    ikigai_gonk::trigger::prepare(&trigger).expect("the tree");
    for n in 0..7 {
        std::fs::write(trigger.inbox().join(format!("{n}.tuple")), "x").expect("a tuple");
    }
    let (served, _config) = door(&dir, Some(trigger));

    // The nav link carries the poll, at the interval the Rust side names — never the
    // stylesheet's own number.
    let rendered = page(&served, &[], &reviewer());
    assert!(
        rendered.contains(&format!("hx-get='{}'", ikigai_gonk::queue::BADGE_PATH)),
        "{rendered}"
    );
    assert!(
        rendered.contains(&format!(
            "hx-trigger='load, every {}'",
            ikigai_gonk::queue::BADGE_EVERY
        )),
        "{rendered}"
    );

    // …and the fragment it polls.
    let answer = issue(
        &served,
        Verb::Source,
        ikigai_gonk::queue::BADGE_IRI,
        &[],
        &reviewer(),
    )
    .expect("the badge renders");
    let badge = String::from_utf8(answer.bytes).expect("utf-8");
    assert!(
        badge.contains(">7<"),
        "the number a person came to see: {badge}"
    );
    // ⚠ Not empty, and not stuck: unarmed, nothing is supposed to be draining, so seven
    // waiting is correct rather than alarming.
    assert!(badge.contains("badge-depth count"), "{badge}");
    assert!(!badge.contains("stuck"), "{badge}");
    // ★ The whole sentence rides as the tooltip, so colour is never the message (WCAG 1.4.1)
    // and the one fact that tells a slow queue from a stuck one is readable.
    assert!(badge.contains("NOTHING IS DRAINING"), "{badge}");

    // ⚠ No queue configured: NOTHING, not a zero. A zero on the badge says "empty queue",
    // which is a different statement (ledger #446).
    let (no_queue, _c) = door(&dir, None);
    let answer = issue(
        &no_queue,
        Verb::Source,
        ikigai_gonk::queue::BADGE_IRI,
        &[],
        &reviewer(),
    )
    .expect("the badge renders without a queue too");
    assert!(
        answer.bytes.is_empty(),
        "an unconfigured queue draws no badge at all: {}",
        String::from_utf8_lossy(&answer.bytes)
    );
}

// ------------------------------------------------- liveness: ledger #469

/// Epoch milliseconds, the way `crate::trigger` counts them.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_millis() as u64
}

/// The badge fragment, rendered.
fn badge(kernel: &Kernel) -> String {
    let answer = issue(kernel, Verb::Source, queue::BADGE_IRI, &[], &reviewer())
        .unwrap_or_else(|e| panic!("the badge: {e}"));
    String::from_utf8(answer.bytes).expect("utf-8")
}

/// A trigger over its own empty scratch tree.
fn armed_trigger(spaces: &TempDir) -> Trigger {
    let trigger = Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: true,
    };
    ikigai_gonk::trigger::prepare(&trigger).expect("the tree");
    trigger
}

/// ⚠ **One cadence, two spellings, and nothing but this test holding them together.**
///
/// htmx wants `"10s"` and an activity window wants milliseconds, so the number is written
/// twice. The window must also be at least the interval — that is the guarantee the whole
/// fix rests on ([#469](http://localhost:1060/l/default/item/469)): if the window were
/// shorter than the gap between two polls, a pass could still finish entirely unseen, which
/// is precisely the failure being fixed.
#[test]
fn the_badge_cadence_has_one_number_in_two_spellings() {
    assert_eq!(
        queue::BADGE_EVERY,
        format!("{}s", queue::BADGE_EVERY_MS / 1000),
        "the htmx interval and its milliseconds are the same number, or the badge polls at \
         one rate and measures activity against another"
    );
    // ⚠ The window-is-at-least-the-interval guarantee is a `const _: () = assert!(…)` in
    // `src/queue.rs`: it is constant, so the compiler is a better place for it than a test.
}

/// ★★ **A pass shorter than the poll interval still reports itself.**
///
/// [#469](http://localhost:1060/l/default/item/469), the first live end-to-end run: a commit
/// dropped a tuple, the pass ran, the queue drained, three findings landed — in about three
/// seconds, between two ten-second polls. The badge read 0, then 0.
///
/// Two defects, both asserted here.
///
/// 1. **A pass in flight against an EMPTY inbox rendered as `empty`**, because `in_flight`
///    was only consulted on the `waiting > 0` arm — and the reactor moves the tuple OUT of
///    the inbox before it starts the work, so the ordinary case is a running pass with an
///    empty queue. The badge said "nothing is happening" during the one interval when
///    something was.
/// 2. **A pass that has ENDED left no trace at all.** It does now, for
///    [`queue::BADGE_RECENT_MS`] — and the window is the poll interval, so no phase of the
///    clock can hide a pass from every poll.
#[test]
fn a_pass_shorter_than_the_poll_interval_still_reports_itself() {
    let dir = scratch_root();
    let spaces = tempfile::tempdir().expect("a spaces tree");
    let activity = Arc::new(ikigai_gonk::trigger::Activity::default());
    let (served, _config) = door_watching(
        &dir,
        Some(armed_trigger(&spaces)),
        Arc::clone(&activity),
        true,
    );

    // Idle, and nothing has ever run: a dim zero and no spark. An armed server that has done
    // nothing yet must not claim otherwise.
    let idle = badge(&served);
    assert!(idle.contains("badge-depth empty"), "{idle}");
    assert!(!idle.contains("spark"), "nothing has happened yet: {idle}");

    // A pass, running, with NOTHING in the inbox — the shape the old code called `empty`.
    let pass = activity.begin(now_ms()).expect("the one pass slot");
    let running = badge(&served);
    assert!(
        running.contains("badge-depth working"),
        "a running pass against an empty inbox is WORKING, not empty: {running}"
    );
    assert!(
        running.contains("spark running"),
        "and it is drawn, not only coloured: {running}"
    );
    assert!(
        running.contains("A pass has been running for"),
        "the sentence rides as the tooltip, so the state is readable and not only visible: \
         {running}"
    );

    // …and when it ends, the badge goes on saying so for the window. This is the assertion
    // that would have caught the reported bug: the pass is OVER, the queue is empty, and the
    // page still has something true to say about the last few seconds.
    pass.succeeded();
    let after = badge(&served);
    assert!(
        after.contains("badge-depth recent"),
        "a pass that ended within the window is RECENT, not idle: {after}"
    );
    assert!(after.contains("spark recent"), "{after}");
    assert!(
        after.contains("the last ended"),
        "and the sentence says how long ago: {after}"
    );
}

/// ★ **The revision is what the findings list refreshes on, and it moves only when the queue
/// does.**
///
/// A token that changed every poll would make the list re-fetch every ten seconds, which is
/// the cost the single-cadence design exists to avoid; one that never changed would leave
/// the page exactly as stale as before ([#469](http://localhost:1060/l/default/item/469)).
#[test]
fn the_badge_revision_moves_only_when_the_queue_does() {
    let dir = scratch_root();
    let spaces = tempfile::tempdir().expect("a spaces tree");
    let activity = Arc::new(ikigai_gonk::trigger::Activity::default());
    let (served, _config) = door_watching(
        &dir,
        Some(armed_trigger(&spaces)),
        Arc::clone(&activity),
        true,
    );

    let rev = |markup: &str| {
        let at = markup
            .find("data-rev=")
            .expect("the badge carries a revision");
        let rest = &markup[at + "data-rev=".len() + 1..];
        rest[..rest.find('\'').expect("a closed attribute")].to_string()
    };

    let first = rev(&badge(&served));
    assert_eq!(
        first,
        rev(&badge(&served)),
        "two polls with nothing happening between them must agree, or the list re-fetches \
         for nothing every interval"
    );

    activity.begin(now_ms()).expect("the slot").succeeded();
    assert_ne!(
        first,
        rev(&badge(&served)),
        "a completed pass may have minted findings, and that is exactly when the list has \
         something new to show"
    );
}

/// ★★ **The findings list refreshes without a reload — off the badge's poll, not a clock of
/// its own.**
///
/// The second half of [#469](http://localhost:1060/l/default/item/469): `web/gonk.xsl`
/// carried exactly one `hx-trigger` for this surface, the badge's, so the one page in gonk
/// whose content changes with no user action was the one page that never refreshed itself.
///
/// ⚠ The event name is a THIRD spelling of one fact — Rust names it, the stylesheet renders
/// it, the script raises it — so this asserts the script against the constant. A rename that
/// reached two of the three would leave a page that quietly stopped updating, with nothing
/// failing anywhere.
#[test]
fn the_findings_list_listens_for_the_badges_news() {
    let dir = scratch_root();
    let spaces = tempfile::tempdir().expect("a spaces tree");
    let (served, _config) = door(&dir, Some(armed_trigger(&spaces)));

    let rendered = page(&served, &[("state", "pending")], &reviewer());
    assert!(
        rendered.contains(&format!("hx-trigger='{}'", queue::NEWS_EVENT)),
        "the queue section listens for the badge's news: {rendered}"
    );
    assert!(
        rendered.contains(&format!("hx-get='{}?state=pending'", queue::ROWS_PATH)),
        "…and re-fetches ITSELF, at the filter the human is looking at: {rendered}"
    );
    // ⚠ No second interval. The cadence is the server's one number, and a page that grew its
    // own `every Ns` would be a clock this crate does not control.
    assert!(
        !rendered.contains("hx-trigger='every"),
        "the list must not poll on a clock of its own: {rendered}"
    );

    // A human who asked to see everything keeps seeing everything across a refresh.
    let all = page(
        &served,
        &[("state", "pending"), ("limit", "all")],
        &reviewer(),
    );
    assert!(
        all.contains(&format!(
            "hx-get='{}?state=pending&amp;limit=all'",
            queue::ROWS_PATH
        )),
        "a refresh keeps the row bound the human chose: {all}"
    );

    // The held-refresh notice exists and starts hidden: the page says a refresh was held
    // rather than silently going stale.
    assert!(
        rendered.contains("id='queue-stale'") && rendered.contains("hidden='hidden'"),
        "{rendered}"
    );

    // The three spellings of the one event.
    let script = include_str!("../web/gonk.js");
    assert!(
        script.contains(&format!("\"{}\"", queue::NEWS_EVENT)),
        "`web/gonk.js` raises `{}` — the constant the stylesheet renders",
        queue::NEWS_EVENT
    );
    assert!(
        script.contains("data-rev"),
        "…and compares the revision the badge carries"
    );
}
