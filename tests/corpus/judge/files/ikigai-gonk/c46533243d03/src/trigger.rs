//! The git-event review trigger: a queue, a pass, and a deliberately missing grant.
//!
//! ```text
//! a git hook  ->  ikigai-gonk review request <repo> <path>   drops a TUPLE (no server, no grant)
//!                 urn:space:{name}                           the queue: rd / out / take
//!                 urn:iki:gonk:review:pass                   ONE tuple -> ONE review pass
//! ```
//!
//! Ledger #261. What is new here is the QUEUE and the ADAPTER between a tuple and a review;
//! the review itself is `ikigai-browse`'s `urn:repo:{repo}:review:{path}`, unchanged and
//! un-wrapped.
//!
//! # ★ The invariant: one call, two causes
//!
//! Brian, 2026-09-19: *"There shouldn't be any real difference between the automated reviews
//! that we're about to trigger and the UI-driven ones."* `ikigai-browse` pinned that from its
//! own side — `the_button_and_a_trigger_are_one_call_with_two_causes` derives the button's
//! exact call and then asserts that the trigger's exact call is an archive HIT on it: same
//! version tag, same minted set, nothing paid.
//!
//! So this module does not assemble a prompt, does not post-process a finding, and does not
//! write an annotation. [`review_request`] builds the one Request the trigger issues, and it
//! is the trigger side of that test spelled in gonk:
//!
//! ```text
//! Source  urn:repo:{repo}:review:{path}   as=application/json
//! ```
//!
//! — the same verb, the same IRI, one argument, no `provider`. It is a pure function so the
//! claim is a unit test over a value rather than a belief about a call site.
//!
//! # ⚠⚠ THE TRIGGER WAS UNARMED UNTIL 2026-09-20, AND WHAT CHANGED IS THE ARITHMETIC
//!
//! Brian, 2026-09-19: **"Nothing gets published to Gonk except by the human."** Not only
//! critical findings — nothing. Until `ikigai-browse` 0.5.0 a review pass minted its
//! annotations as its terminal step, so a pass that ran with no human present would publish
//! with no human present, and the only thing standing between the two was that **no grant
//! this server could mint carried the authority a pass needed**.
//!
//! ★ **browse 0.5.0 moved that line, and moved it in the capability layer rather than in a
//! flag.** A pass now writes PENDING FINDINGS at `urn:iki:finding:{id}` and `review` no
//! longer declares `urn:cap:annotate` at all (`review.rs:953` says the absence IS the
//! point). `Sink urn:iki:finding:{id} decision=publish` is the only path into the
//! `urn:iki:annotation:` family, and that Sink still demands the token. So a reviewer that
//! holds browse-read + a narrow net grant + the browse graph's two store doors **cannot
//! publish, by arithmetic**: it is not policed into not publishing, it is unable to. That is
//! what made arming this safe, and ledger
//! [#466](http://localhost:1060/l/default/item/466) is the decision.
//!
//! ⚠ **This module's declared capabilities move WITH browse's.** [`PassEndpoint`] declares
//! exactly what `review` declares and no more — declaring `urn:cap:annotate` here would
//! refuse the very grant this design hands the reviewer, and it would do so one hop before
//! the review that no longer wants it. `tests/browse.rs` reads both contracts off a kernel
//! that composes real browse and asserts they are the same set, because the last time
//! browse's `requires` shrank, this crate's copy **compiled clean and went stale**
//! ([`reviewer_grant_shape`] had the same bug).
//!
//! # ★ What arms it: an operator's line, and an authority they wrote down
//!
//! Two facts, both required, neither implicit — `gonk.review.arm = true` **and** a
//! `gonk.review.grant` that `grants.json` can honour ([`reviewer_scopes`]). `arm` without a
//! usable grant is a startup refusal, not a degraded server; a grant without `arm` is read,
//! checked and printed, exactly as before. The grant alone is deliberately NOT the switch:
//! this server spent a release inviting operators to write one down *so they could see it*,
//! and a line written under that invitation must not silently start spending inference.
//!
//! When armed, [`arm`] gives `ikigai-intray`'s `SpaceReactor` this server's own kernel as
//! its resolver and the reviewer's capability as its ceiling, and calls `watch()`. Its
//! contract is *drain what is already pending, then watch* — the catch-up half is what makes
//! a push design survive a restart of this process.
//!
//! # ⚠ Why the reactor's own `cap` file is not how this server grants authority
//!
//! `SpaceReactor::capability_for` used to read `<root>/{space}/cap` and build
//! `Capability::scoped` from its lines — the TRUSTED minting path, not an attenuation, so
//! that file REPLACED the reactor's default and could exceed it without limit. And it sits in
//! the same directory, with the same owner and mode, as the `inbox/` a dropper writes into:
//! anything that can drop a tuple could rewrite the authority the tuple ran under, and
//! nothing validated the result.
//!
//! gonk refuses the broad store tokens, the offering wildcards and the backup family's
//! tokens on every certificate and every passkey it admits ([`crate::quic::check_grants`]),
//! at startup and again per use. A second authority file with none of those checks would be
//! the widest grant in the server and the one nothing reads. So this server names a GRANT
//! (`gonk.review.grant`, resolved through [`reviewer_scopes`]) and refuses to run a trigger
//! beside a `cap` file at all ([`refuse_cap_file`]).
//!
//! ★ **Three independent refusals of the same bad idea, and none of them is redundant.**
//! `ikigai-intray` 0.1.24 fixed its half twice over (ledger
//! [#445](http://localhost:1060/l/default/item/445)): `Capability::attenuate` makes the file
//! unable to widen, and `SpaceReactor::with_host_authority` — which this module uses — takes
//! the crate out of the business of reading authority off the dropper's tree at all. Under
//! the host seam a `cap` file is INERT, which is its own trap, so [`refuse_cap_file`] runs at
//! startup and [`arm`] additionally asks the reactor's own `ignored_cap_files()` and refuses
//! to go live beside one. A file that does nothing, that an operator believes is doing
//! something, is the failure the seam introduced while fixing the other.
//!
//! # ⚠ The `handler` file IS a control surface, and it lives in the dropper's tree
//!
//! `SpaceReactor` reads `<root>/{space}/handler` **per tuple** and fires whatever IRI it
//! names, under the capability the host supplied. So anything that can write that directory
//! can retarget the reviewer's authority at another resource — narrower than the old `cap`
//! hazard (it cannot ADD a scope) and the same shape. gonk owns this tree `0700` and is its
//! only writer outside `inbox/`, so the boundary today is the unix user; [`set_handler`]
//! re-asserts gonk's own value at every startup and removes the file when this server is not
//! armed, so an unarmed gonk leaves nothing behind that a later armed one would fire.
//! Reported up: the seam `with_host_authority` is for authority wants an exact analogue for
//! the HANDLER, so a host can say what fires as well as what it fires as.
//!
//! # The tuple
//!
//! Turtle, because the intray's associative match is a SPARQL ASK over the tuple's graph and
//! a non-RDF tuple can never be selected by one. [`tuple_turtle`] is the only writer, and it
//! is byte-deterministic on purpose: the intray content-addresses a drop, so the SAME
//! (repo, path) dropped twice lands on one filename and the inbox holds one tuple.
//!
//! ⚠ **That is why the tuple does not carry the commit.** A SHA or a timestamp would make
//! every drop unique and the queue would grow instead of collapsing. The cost is real and is
//! recorded on #261: the commit->pass edge cannot come out of the tuple. What falls out
//! instead is better — the content hash is read by the pass from the LIVE tree, so three
//! commits to one file while the queue waits collapse to one pass over the final state,
//! which is exactly what a person clicking `review` would get.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Capability, Description, Endpoint, Error, Invocation, Iri, Kernel, ReprType,
    Representation, Request, Result, Space, Verb,
};
use oxigraph::io::{RdfFormat, RdfParser};
use oxigraph::store::Store;

/// `urn:iki:gonk:review:pass` — one tuple, one review pass.
pub const PASS: &str = "urn:iki:gonk:review:pass";

/// `urn:iki:gonk:review:depth` — how deep the queue is, and whether anything is draining it.
///
/// ★ **This is the liveness signal, and it is the ONLY one this design has.** `watch()`
/// catches up at startup, so a reactor thread that dies while gonk LIVES drains nothing and
/// says nothing until a restart — the unfalsifiable shape of the dead annotation mount. gonk
/// runs no log at all ([#383](http://localhost:1060/l/default/item/383)), so a depth that
/// stops falling while the queue is armed is the visible symptom, and something has to make
/// it visible. Ledger [#464](http://localhost:1060/l/default/item/464) asked whether the
/// depth should be a resource of gonk's own or the space's read token should become
/// mintable; [#466](http://localhost:1060/l/default/item/466) chose the first, and this is it.
pub const DEPTH: &str = "urn:iki:gonk:review:depth";

/// `urn:cap:exec:gh` — the PR review tier shells out through `ikigai-repo`'s `gh` facade.
///
/// ⚠ The per-tool spelling, never `urn:cap:exec:*`: that is the OFFERING wildcard, and
/// [`crate::quic::check_grants`] refuses it as a grant. A reviewer that may run `gh` may not
/// run anything else.
pub const CAP_EXEC_GH: &str = "urn:cap:exec:gh";

/// The file `SpaceReactor` reads to decide what a dropped tuple fires — see the module doc.
pub const HANDLER_FILE: &str = "handler";

/// The directory under the data home that holds the spaces tree.
///
/// ⚠ **Not `~/.ikigai/workspace/spaces`**, which is the cli's. gonk owns this one, creates
/// it `0700`, and is the only writer of anything in it but `inbox/`.
pub const SPACES_DIR: &str = "spaces";

/// The XSD datatype every scalar here declares — a claim about the wire, not the grammar.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The face the trigger asks for: the machine one, exactly as `ikigai-browse`'s own
/// `the_button_and_a_trigger_are_one_call_with_two_causes` asks for it.
const JSON: &str = "application/json";

/// How a person runs one queued request, `{space}` left for the caller to fill — printed by
/// the banner and repeated in the README, from here so the two cannot drift.
///
/// ★ It READS the tuple rather than taking it (`source`, not `delete`), so the request stays
/// queued until a person says it is done, and a pass is spent only when someone asks for
/// one. That IS "nothing gets published to gonk except by the human".
///
/// ⚠ The later stages of an engine pipeline are BARE IRIs — `| urn:iki:gonk:review:pass`,
/// never `| source urn:…`, which fails with "invalid IRI: No scheme found" because the word
/// `source` is parsed as the stage's target. Measured against a live socket door,
/// 2026-09-19.
pub const DRAIN_ONE: &str = "source urn:space:{space} tuple=ID | urn:iki:gonk:review:pass";

// ------------------------------------------------------------------- settings

/// What an operator configured, or `None` when no `gonk.review.space` line exists.
///
/// ★ The whole feature is one line, the same switch shape as `gonk.mount`: with no line
/// this server binds no space, offers no pass, and serves exactly the catalog it served
/// before. `declared = enforced` runs both ways — an unconfigured gonk must not advertise a
/// queue nothing fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    /// The space name — one segment, the `{name}` of `urn:space:{name}`.
    pub space: String,
    /// The grant in `grants.json` a pass runs under, when one is named.
    ///
    /// ⚠ Naming it is not arming: with `arm` unset the banner prints what it resolves to so
    /// an operator can SEE the authority they have written down before anything uses it,
    /// which is what this line meant for its first release and must go on meaning.
    pub grant: Option<String>,
    /// The spaces tree, `<data home>/spaces` unless `gonk.review.root` says otherwise.
    pub root: PathBuf,
    /// `gonk.review.arm` — whether a reactor watches this queue and fires [`PASS`] on a drop.
    ///
    /// ★ Both halves are required and neither is implicit: `arm` without a usable [`grant`]
    /// is a startup refusal, because a server that says it is reviewing and is not is worse
    /// than one that refuses to start.
    ///
    /// [`grant`]: Trigger::grant
    pub arm: bool,
}

impl Trigger {
    /// This trigger's own directory, `<root>/{space}`.
    pub fn dir(&self) -> PathBuf {
        self.root.join(&self.space)
    }

    /// Where a dropped tuple lands.
    pub fn inbox(&self) -> PathBuf {
        self.dir().join("inbox")
    }
}

/// A space name is the `{name}` of `urn:space:{name}` — one segment, never a path.
///
/// Checked here, at configuration time, rather than at first drop: `ikigai-intray` refuses
/// the same shapes inside an invocation, which would make a typo in a config file surface
/// as a failing tuple nobody is watching.
pub fn check_space_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() || name.contains(['/', '\\', ':', '.']) {
        return Err(format!(
            "gonk.review.space: `{name}` is not a space name — one segment, no `/ \\ : .` \
             (it is the `{{name}}` of `urn:space:{{name}}`)"
        ));
    }
    Ok(())
}

/// ⚠ **Refuse to run a trigger beside a `SpaceReactor` `cap` file.**
///
/// `<root>/{space}/cap` is `ikigai-intray`'s own authority file, and it MINTS rather than
/// attenuates: whatever scopes it lists become the capability a handler runs under,
/// bypassing every refusal [`crate::quic::check_grants`] applies to a certificate or a
/// passkey. It also lives one directory above the `inbox/` any dropper can write.
///
/// gonk never writes one, so its presence means either an operator put it there or
/// something that can drop tuples did. Both are worth stopping for, and stopping is cheap:
/// the file is checked once at startup, before the store is open and before a door is bound.
pub fn refuse_cap_file(trigger: &Trigger) -> std::result::Result<(), String> {
    let cap = trigger.dir().join("cap");
    if cap.exists() {
        return Err(format!(
            "{} exists. That file is `ikigai-intray`'s reactor grant, and it MINTS a \
             capability rather than narrowing one — it would give a handler authority this \
             server refuses on every certificate and every passkey it admits, from a file \
             in the same directory as the inbox anything can drop into. gonk names a GRANT \
             instead (`gonk.review.grant`); remove this file",
            cap.display()
        ));
    }
    Ok(())
}

/// The scopes a named grant carries, through the same checked path every door uses.
///
/// ★ This is the authority decision of ledger #261 in one function: a headless pass runs
/// under a grant an operator WROTE DOWN in `grants.json`, beside the passkey grant a click
/// borrows — not under root, not under the dropper's, and not under anything this server
/// minted for itself. [`crate::quic::scopes_for_grant`] fails closed on an unknown grant,
/// an empty one, a broad store token and an offering wildcard.
pub fn reviewer_scopes(
    grants: &std::collections::BTreeMap<String, Vec<String>>,
    grant: &str,
) -> std::result::Result<Vec<String>, String> {
    crate::quic::scopes_for_grant(grants, grant)
}

/// The scopes a review pass actually needs, **read off the contract of the review this
/// server would run** — for an operator writing `gonk.review.grant` by hand.
///
/// # ★★ Why this reads a live kernel instead of listing constants
///
/// It used to list them, and on 2026-09-20 that list was found to be **wrong and silent**.
/// `ikigai-browse` 0.5.0 dropped `urn:cap:annotate` from what `review` declares — the whole
/// point of that release — and this helper went on emitting it. It **compiled clean** through
/// the pin bump, because the constant still exists; only the requirement went away. An
/// operator following it would have written exactly the grant this design excludes, handing
/// the headless reviewer the publish token, and the interlock would have disappeared with no
/// error anywhere. Rule 5's second half in a different costume: a pin that moves and a
/// transcription that does not.
///
/// So the browse half is `hub.describe(review_iri)`'s own `requires` for `Source`, and the
/// next capability change reaches an operator's `grants.json` without anyone remembering to
/// come here. ⚠ Two mappings are applied, and each is a real difference between an OFFERING
/// form and a GRANT form rather than an edit:
///
/// - `urn:cap:net:*` becomes `urn:cap:net:{host}`. The wildcard is what `ikigai-browse`
///   declares on every derivation to mean "holds some grant under this prefix"; as a grant it
///   means every host this kernel could dial, and [`crate::quic::check_grants`] refuses it.
///   `host` is where the mounted peer lives — for `quic://127.0.0.1:4433` that is
///   `127.0.0.1` and **not** `localhost`, because `Capability::allows` is exact string
///   containment.
/// - Nothing is dropped. A `requires` this function does not recognise passes through
///   unchanged, because an unknown token is a capability an operator needs and not one this
///   crate gets to decide about.
///
/// # What the contract cannot tell you, and is added here
///
/// Two sets, both of them authority a pass reaches through a SUB-REQUEST — a capability
/// travels down unchanged, so the caller must hold what the hop needs, and the hop's
/// declaration is not on the resource the caller named:
///
/// - the browse graph's two store doors ([`crate::grants::browse_graph_grants`]), which are
///   how a finding is written at all; and
/// - [`CAP_EXEC_GH`], which the PR review tier reaches `gh` through.
///
/// ⚠ **The space's own three tokens are NOT here, and that is measured rather than assumed.**
/// `SpaceReactor` claims, reads and settles a tuple with filesystem renames of its own and
/// issues only the HANDLER request through the kernel, so a reviewer needs no
/// `urn:cap:space:{out,read,take}` at all. Granting them would widen the reviewer to the
/// queue for nothing.
///
/// ⚠ And every one of these is needed EVEN WHEN THE PASS IS A FREE ARCHIVE HIT: the kernel
/// checks a `requires` before the endpoint runs and long before it looks in the archive.
/// There is no cheaper grant for the cheap case.
///
/// # Errors
///
/// When `review_iri` does not resolve on this kernel, or declares no `Source`. That is a gonk
/// with no `gonk.browse.root` or no `gonk.mount`, and the honest answer is to say so rather
/// than emit a remembered list — the failure this function exists to stop.
pub fn reviewer_grant_shape(
    hub: &Kernel,
    review_iri: &str,
    host: &str,
) -> std::result::Result<Vec<String>, String> {
    let target = Iri::parse(review_iri).map_err(|e| format!("{review_iri}: {e}"))?;
    let described = hub.describe(&target).ok_or_else(|| {
        format!(
            "`{review_iri}` does not resolve on this server, so its capabilities cannot be \
             read off its contract. A review is bound only with a `gonk.browse.root` for that \
             repository AND a `gonk.mount` serving `urn:llm:`"
        )
    })?;
    let requires = described
        .action_specs()
        .into_iter()
        .find(|spec| spec.verb == Verb::Source)
        .map(|spec| spec.requires)
        .ok_or_else(|| format!("`{review_iri}` declares no Source, so nothing runs a pass"))?;
    let mut scopes: Vec<String> = Vec::new();
    let mut add = |scope: String| {
        if !scopes.contains(&scope) {
            scopes.push(scope);
        }
    };
    for scope in requires {
        if scope == crate::grants::CAP_NET_ANY {
            add(format!("urn:cap:net:{host}"));
        } else {
            add(scope);
        }
    }
    for scope in crate::grants::browse_graph_grants(crate::grants::Authority::Write)? {
        add(scope);
    }
    add(CAP_EXEC_GH.to_string());
    Ok(scopes)
}

// ---------------------------------------------------------------------- the tuple

/// One queued request: which repository, and which file in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tuple {
    /// A configured `gonk.browse.root` name — the `{repo}` of `urn:repo:{repo}:…`.
    pub repo: String,
    /// The path within that root, as `git` spells it.
    pub path: String,
}

impl Tuple {
    /// The tuple's own name. Skolemized, never a blank node — the module recipe's rule, and
    /// the reason a queued request can be joined to anything else in a query.
    pub fn iri(&self) -> String {
        format!("urn:iki:gonk:review:request:{}:{}", self.repo, self.path)
    }
}

/// The ONE writer of a tuple's bytes, and the exact bytes a git hook produces.
///
/// ★ **Byte-deterministic on purpose.** `ikigai-intray` content-addresses a drop — the file
/// is named for the hash of its content — so identical bytes for the same (repo, path) mean
/// the inbox holds exactly one tuple however many times a hook fires. A timestamp, a commit
/// SHA or a re-serialization through a graph library would each break that silently, which
/// is why this is a `format!` and not a Turtle serializer.
///
/// ⚠ The two predicates are `ikigai-browse`'s own (`ik:repo`, `ik:path`, the same ones every
/// annotation carries), so nothing here mints a vocabulary term. A `ReviewRequest` CLASS
/// would be a vocabulary change and is reported rather than invented.
///
/// ```
/// use ikigai_gonk::trigger::{tuple_turtle, Tuple};
/// let t = Tuple { repo: "ikigai-gonk".into(), path: "src/k.rs".into() };
/// assert_eq!(
///     tuple_turtle(&t),
///     "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
///      <urn:iki:gonk:review:request:ikigai-gonk:src/k.rs> ik:repo \"ikigai-gonk\" ; ik:path \"src/k.rs\" .\n"
/// );
/// ```
pub fn tuple_turtle(tuple: &Tuple) -> String {
    format!(
        "@prefix ik: <{IK}> .\n<{iri}> ik:repo \"{repo}\" ; ik:path \"{path}\" .\n",
        IK = IK_NS,
        iri = tuple.iri(),
        repo = tuple.repo,
        path = tuple.path,
    )
}

/// The vocabulary namespace the tuple's two predicates come from.
const IK_NS: &str = "https://ikigai-rs.dev/ns#";

/// ⚠ **Refuse a request this module cannot express, rather than writing a tuple nothing can
/// read back.**
///
/// [`tuple_turtle`] is a `format!` and not a Turtle serializer, deliberately — a serializer
/// would be free to reorder or re-quote and would break the byte-determinism the queue's
/// deduplication rests on. The price of that choice is that it does no escaping, so a `"`,
/// a `\` or a newline in a path would emit Turtle that [`parse_tuple`] then refuses. A git
/// path may legally contain all three.
///
/// A poison tuple is the worst outcome available here: the drop succeeds, the hook reports
/// nothing, and a request sits in the inbox failing every time anyone runs it. So the check
/// is at the DROP, where a person is still watching, and it refuses rather than mangling —
/// a bound refuses, it does not truncate.
///
/// The set is the union of what Turtle's quoted literal and an IRI each forbid, so one rule
/// covers both positions a value is emitted in.
///
/// # Errors
///
/// When either field is empty or carries a character this module cannot emit.
pub fn check_request(tuple: &Tuple) -> std::result::Result<(), String> {
    for (field, value) in [("repository", &tuple.repo), ("path", &tuple.path)] {
        if value.is_empty() {
            return Err(format!("a review request's {field} may not be empty"));
        }
        if let Some(bad) = value
            .chars()
            .find(|c| c.is_whitespace() || c.is_control() || "\"\\<>{}|^`".contains(*c))
        {
            return Err(format!(
                "a review request's {field} may not contain `{}`: this server emits a \
                 request as Turtle with a `urn:iki:gonk:review:request:` IRI, and that \
                 character is legal in neither. Refused here rather than queued as a tuple \
                 nothing can read back",
                bad.escape_default()
            ));
        }
    }
    Ok(())
}

/// Read a tuple back.
///
/// Parsed as RDF rather than by string surgery, so a tuple dropped by something other than
/// [`tuple_turtle`] — a hand-written one, a richer one a later arc adds fields to — still
/// resolves, and so a tuple that is not RDF at all fails with an explanation instead of a
/// wrong repository name.
///
/// # Errors
///
/// When the bytes are not Turtle, or carry no `ik:repo`/`ik:path` pair on one subject, or
/// carry more than one such subject — a tuple is ONE request, because the queue's whole
/// bound is that a pass consumes one tuple.
pub fn parse_tuple(bytes: &[u8]) -> Result<Tuple> {
    let store = Store::new().map_err(|e| Error::Endpoint(format!("tuple: store init: {e}")))?;
    store
        .load_from_slice(RdfParser::from_format(RdfFormat::Turtle), bytes)
        .map_err(|e| Error::InvalidArgument {
            name: "content".to_string(),
            detail: format!("a review tuple is Turtle naming one `ik:repo` and one `ik:path`: {e}"),
        })?;
    let mut found: Vec<Tuple> = Vec::new();
    let repo_p = oxigraph::model::NamedNode::new(format!("{IK_NS}repo"))
        .map_err(|e| Error::Endpoint(format!("tuple: {e}")))?;
    let path_p = oxigraph::model::NamedNode::new(format!("{IK_NS}path"))
        .map_err(|e| Error::Endpoint(format!("tuple: {e}")))?;
    for quad in store.quads_for_pattern(None, Some(repo_p.as_ref()), None, None) {
        let quad = quad.map_err(|e| Error::Endpoint(format!("tuple: {e}")))?;
        let Some(repo) = literal(&quad.object) else {
            continue;
        };
        let path = store
            .quads_for_pattern(
                Some(quad.subject.as_ref()),
                Some(path_p.as_ref()),
                None,
                None,
            )
            .filter_map(|q| q.ok())
            .find_map(|q| literal(&q.object));
        if let Some(path) = path {
            found.push(Tuple { repo, path });
        }
    }
    found.sort_by(|a, b| (&a.repo, &a.path).cmp(&(&b.repo, &b.path)));
    found.dedup();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(Error::InvalidArgument {
            name: "content".to_string(),
            detail: "this tuple names no `ik:repo` and `ik:path` pair — it is not a review \
                     request"
                .to_string(),
        }),
        n => Err(Error::InvalidArgument {
            name: "content".to_string(),
            detail: format!(
                "this tuple names {n} review requests. A tuple is ONE request: the queue's \
                 whole bound is that a pass consumes one tuple at a time"
            ),
        }),
    }
}

fn literal(term: &oxigraph::model::Term) -> Option<String> {
    match term {
        oxigraph::model::Term::Literal(l) => Some(l.value().to_string()),
        _ => None,
    }
}

// ------------------------------------------------------------------- what it spends

/// What this process has spent on review passes, and whether one is running right now.
///
/// # ★★ The bound on a forty-file push, stated rather than inherited
///
/// [#308](http://localhost:1060/l/default/item/308) — nothing counts spend over time — was
/// the standing objection to arming the trigger, and a TIMER would have answered it by
/// construction: one pass per tick. A WATCHER does not. Forty changed files drop forty tuples
/// at once, and something has to say what happens then.
///
/// The answer is that **passes are serial, one per gonk process**, and this type is what
/// makes that a statement rather than a hope. `SpaceReactor::watch` drains and then reads its
/// notify channel on ONE thread, calling `process` inline, so a second tuple waits on the
/// first — but that is a property of a dependency's thread shape, invisible from here, and it
/// would change without a compile error. So [`Activity::begin`] **refuses** a pass that
/// starts while another is in flight, and the tuple dead-letters with that sentence in its
/// `.err` note. The assumption is now load-bearing AND falsifiable: if the reactor ever fires
/// concurrently, this server says so in the one place an operator is already looking instead
/// of quietly doubling what it spends.
///
/// ⚠ What is NOT bounded here, said plainly: **wall clock**. Brian is not fussed about the
/// cost of local inference, and serial passes over forty files at a minute each is still
/// forty minutes of a reviewer nobody can see. That is what [`DEPTH`] and the Queue badge are
/// for — `waiting` falling steadily is a slow queue, `waiting` not falling is a stuck one —
/// and it is why the queue's depth is a liveness mechanism rather than decoration. A per-pass
/// wall-clock ceiling would have to come from the ASK (`urn:llm:*` through the mount), which
/// bounds its describe and not its derivation; reported up rather than faked here.
#[derive(Debug, Default)]
pub struct Activity {
    record: Mutex<Record>,
}

/// The counters behind [`Activity`], taken as a snapshot so a reader never holds the lock.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Passes {
    /// When the pass now running began, in milliseconds since the epoch. `None` = idle.
    pub in_flight_since_ms: Option<u64>,
    /// Passes begun in this process.
    pub started: u64,
    /// …of which completed with an answer.
    pub succeeded: u64,
    /// …and failed, for any reason including a refusal.
    pub failed: u64,
    /// When the last pass ENDED, either way.
    pub last_end_ms: Option<u64>,
    /// How long it took.
    pub last_ms: Option<u64>,
    /// How many findings the DERIVED passes of this run minted, by the severity word the
    /// model wrote (an unrated finding counts under [`UNRATED`]). Archive hits are left out:
    /// they replay labels an earlier pass chose, and the number this feeds exists to watch
    /// the labels a model is choosing NOW ([`Status::serious_share_percent`]).
    pub by_severity: BTreeMap<String, u64>,
}

impl Passes {
    /// Every finding minted by a derived pass this run.
    pub fn findings(&self) -> u64 {
        self.by_severity.values().sum()
    }
}

/// The key an unrated finding is tallied under — a finding whose `severity` the model left
/// null. Not a severity word, and never one the contract could declare (it is not in the
/// menu), so it cannot collide with a real label.
pub const UNRATED: &str = "unrated";

#[derive(Debug, Default)]
struct Record {
    in_flight_since_ms: Option<u64>,
    started: u64,
    succeeded: u64,
    failed: u64,
    last_end_ms: Option<u64>,
    last_ms: Option<u64>,
    by_severity: BTreeMap<String, u64>,
}

impl Activity {
    /// Claim the one pass slot, or refuse.
    ///
    /// # Errors
    ///
    /// When a pass is already in flight — see the type's doc: the serialization is this
    /// server's own statement, not a belief about `SpaceReactor`'s thread shape.
    pub fn begin(self: &Arc<Self>, now_ms: u64) -> Result<Pass> {
        let mut record = self.record.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(since) = record.in_flight_since_ms {
            return Err(Error::Unavailable(format!(
                "a review pass has been running since {since}ms and this server runs ONE at a \
                 time. `SpaceReactor::watch` processes tuples on a single thread, so a second \
                 concurrent pass means that is no longer true — this refusal is where that \
                 would be noticed rather than paid for. The tuple is dead-lettered and can be \
                 re-dropped"
            )));
        }
        record.in_flight_since_ms = Some(now_ms);
        record.started += 1;
        drop(record);
        Ok(Pass {
            activity: Arc::clone(self),
            began_ms: now_ms,
            settled: false,
        })
    }

    /// The counters as they stand.
    pub fn snapshot(&self) -> Passes {
        let record = self.record.lock().unwrap_or_else(|e| e.into_inner());
        Passes {
            in_flight_since_ms: record.in_flight_since_ms,
            started: record.started,
            succeeded: record.succeeded,
            failed: record.failed,
            last_end_ms: record.last_end_ms,
            last_ms: record.last_ms,
            by_severity: record.by_severity.clone(),
        }
    }

    fn settle(&self, began_ms: u64, ok: bool, labels: &[String]) {
        let mut record = self.record.lock().unwrap_or_else(|e| e.into_inner());
        record.in_flight_since_ms = None;
        let end = now_ms();
        record.last_end_ms = Some(end);
        record.last_ms = Some(end.saturating_sub(began_ms));
        if ok {
            record.succeeded += 1;
        } else {
            record.failed += 1;
        }
        for label in labels {
            *record.by_severity.entry(label.clone()).or_insert(0) += 1;
        }
    }
}

/// The in-flight pass, released when it drops.
///
/// ⚠ **A pass that ends by `?` counts as a failure**, which is why this is a guard and not a
/// pair of calls: every early return in [`PassEndpoint::invoke`] is a pass that was begun and
/// did not answer, and a slot that leaked would wedge this server's reviewer for the life of
/// the process with no symptom but a queue that stops draining.
pub struct Pass {
    activity: Arc<Activity>,
    began_ms: u64,
    settled: bool,
}

impl Pass {
    /// Record an answer. Anything else — including a panic-free early return — is a failure.
    pub fn succeeded(self) {
        self.succeeded_with(&[]);
    }

    /// Record an answer AND the severity words of the findings it minted, one entry per
    /// finding ([`UNRATED`] for a finding the model left unrated). What
    /// [`Status::serious_share_percent`] is computed from.
    pub fn succeeded_with(mut self, labels: &[String]) {
        self.settled = true;
        self.activity.settle(self.began_ms, true, labels);
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        if !self.settled {
            self.activity.settle(self.began_ms, false, &[]);
        }
    }
}

/// The severity words of the findings a pass's JSON answer says it minted — one per row of
/// `annotations` (browse's `included_for_ids` over the pass's own `minted` set), [`UNRATED`]
/// where the model wrote none. Empty for an archive HIT (`derived: false`): those rows were
/// labelled by an earlier pass, possibly an earlier run, and counting them again would let a
/// replayed file move a number that exists to watch what the model is labelling now.
///
/// ⚠ Read from the answer this pass already holds, never from a second query: the pass
/// declares browse read and net and nothing else, and it must not grow a read it would then
/// have to declare. An answer that is not JSON in this shape tallies nothing — the pass still
/// succeeded; only this count is silent, and `passes_succeeded` beside a `findings_this_run`
/// that never moves is how that would show.
pub fn minted_labels(answer: &[u8]) -> Vec<String> {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(answer) else {
        return Vec::new();
    };
    if json.get("derived").and_then(serde_json::Value::as_bool) != Some(true) {
        return Vec::new();
    }
    json.get("annotations")
        .and_then(serde_json::Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(
                    |row| match row.get("severity").and_then(serde_json::Value::as_str) {
                        Some(word) if !word.trim().is_empty() => word.trim().to_string(),
                        _ => UNRATED.to_string(),
                    },
                )
                .collect()
        })
        .unwrap_or_default()
}

/// Wall clock in milliseconds since the epoch.
///
/// ⚠ Read from the OS rather than through the kernel's `Clock`, and only here. Everything
/// this server puts in a GRAPH is stamped by the kernel — a fixed clock in a test must move
/// those — but these are liveness counters about this process, and a test clock that made
/// "the last pass ended 40 minutes ago" say `0` would break the one readout whose whole job
/// is to be true about the wall.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------- the pass

/// ★★ **THE INVARIANT, as a value.**
///
/// The one Request a trigger issues, built here so that "the trigger and the button are the
/// same call" is a unit test over a value rather than a claim about a call site. It is the
/// trigger side of `ikigai-browse`'s own
/// `the_button_and_a_trigger_are_one_call_with_two_causes`, which derives the button's exact
/// call and then asserts this one is an archive HIT on it — same version tag, same minted
/// annotations, nothing paid.
///
/// ⚠ **One argument, and no `provider`.** A `provider=` would fold a different model
/// identity into the archive key and into every finding's `dcterms:creator`, so a trigger
/// that picked one would key a different entry from the button's. When a trigger should
/// choose, the derivation-free `urn:repo:{repo}:review-options:{path}` is where the choice
/// comes from — it requires the browse grant alone — and that is a later arc, not a default.
///
/// # Errors
///
/// When `repo` and `path` do not make a resolvable IRI — a path carrying a space, say.
pub fn review_request(repo: &str, path: &str) -> Result<Request> {
    let iri = Iri::parse(format!("urn:repo:{repo}:review:{path}")).map_err(|e| {
        Error::InvalidArgument {
            name: "path".to_string(),
            detail: format!("`urn:repo:{repo}:review:{path}` is not a resolvable IRI: {e}"),
        }
    })?;
    Ok(Request::new(Verb::Source, iri)
        .with_arg("as", ikigai_core::ArgRef::Inline(JSON.as_bytes().to_vec())))
}

/// `urn:iki:gonk:review:pass` — the adapter between a queued tuple and a review.
///
/// # ★ This is the single place a pass completes
///
/// Whatever causes it — a person piping a tuple in today, a `SpaceReactor` firing it when
/// ledger #444 has given a finding a pending state — a pass reaches its end HERE, in
/// [`PassEndpoint::invoke`], at the one `inv.issue` below. A pending state changes what this
/// function does with the pass's answer; it changes nothing else in this module and nothing
/// at all in the queue.
///
/// # Capability
///
/// The caller's, unchanged and unwidened: `inv.issue` carries it into the review. This
/// endpoint declares exactly what the review declares — browse read and net — because it can
/// never do less and must not advertise that it might. Declaring nothing would be an
/// over-offer that fails one hop in; declaring more would refuse callers the review accepts.
///
/// ⚠⚠ **`urn:cap:annotate` is NOT among them, and its absence is the interlock.** Until
/// `ikigai-browse` 0.5.0 it was, because a pass minted annotations; the day that changed,
/// this declaration became an over-declaration that would have refused the reviewer grant
/// **one hop before the review that no longer wants it** — a headless pass denied by gonk's
/// own contract, with browse perfectly willing. Declared = enforced runs in both directions,
/// and `tests/browse.rs` reads both contracts off a kernel composing real browse rather than
/// trusting either copy.
pub struct PassEndpoint {
    /// What this process has spent, and the one-at-a-time bound. See [`Activity`].
    pub activity: Arc<Activity>,
}

#[async_trait]
impl Endpoint for PassEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "gonk-review-pass answers Source (run one queued tuple), not {:?}",
                inv.request.verb
            )));
        }
        // ★ The slot is claimed BEFORE the tuple is parsed, so a malformed tuple counts as a
        // pass that failed rather than as nothing having happened: the counters are what an
        // operator reads to tell a stuck queue from a slow one, and a tuple that fails
        // instantly forty times in a row is exactly the shape they need to see.
        let pass = self.activity.begin(now_ms())?;
        // The tuple arrives as `content` — the name a pipe's value lands in, and the name
        // `ikigai-intray`'s reactor passes it under.
        let bytes = inv
            .inline_arg("content")
            .map_err(|_| Error::MissingArgument("content".to_string()))?;
        let tuple = parse_tuple(bytes)?;
        let request = review_request(&tuple.repo, &tuple.path)?;
        // ★ THE ONE CALL. Nothing before it builds a prompt; nothing after it touches a
        // finding. The review's own answer is the answer, whole — and the severity words in
        // it are tallied for the depth's serious share (ledger #496), read off the answer
        // rather than asked for again.
        let answer = inv.issue(request).await?;
        pass.succeeded_with(&minted_labels(&answer.bytes));
        Ok(answer)
    }

    fn name(&self) -> &str {
        "gonk-review-pass"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-review-pass")
            .title("Run one queued review request")
            .summary(
                "Reads a review tuple (Turtle naming one `ik:repo` and one `ik:path`) and \
                 resolves `urn:repo:{repo}:review:{path}` with `as=application/json` and \
                 nothing else — the same verb, IRI and arguments the Review button sends, so \
                 a triggered pass and a clicked one share an archive key and a set of minted \
                 annotations. Assembles no prompt and writes no annotation of its own: the \
                 answer is the review's, whole.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            // ⚠ `urn:cap:net:*` is the OFFERING wildcard browse declares on every
            // derivation, and browse does not re-export its constant — so it is taken from
            // this server's own `grants::CAP_NET_ANY`, which is the string `grants.json` is
            // checked against, rather than typed a third time.
            .requires(crate::grants::CAP_NET_ANY)
            // ⚠⚠ NO `urn:cap:annotate`. It was here until 2026-09-20, correctly, because a
            // pass used to mint annotations; browse 0.5.0 made a pass produce PENDING
            // findings instead and dropped the token from `review`'s own `requires`. Leaving
            // it would refuse the reviewer grant HERE, one hop before the review that
            // accepts it — see the struct's doc.
            .input(
                ArgSpec::new("content")
                    .class(XSD_STRING)
                    .summary("the tuple to run — where a piped value lands"),
            )
            .input(
                ArgSpec::new("in")
                    .optional()
                    .class(XSD_STRING)
                    .summary("the tuple, under the name the space reactor also offers it by"),
            )
            .input(
                ArgSpec::new("space").optional().class(XSD_STRING).summary(
                    "which space the tuple came from; recorded by the caller, unused here",
                ),
            )
            .input(
                ArgSpec::new("tuple")
                    .optional()
                    .class(XSD_STRING)
                    .summary("the tuple's id; recorded by the caller, unused here"),
            )
            .output(JSON)
    }
}

// ---------------------------------------------------------------------- composition

/// The trigger's space: the queue at `urn:space:{name}`, the pass in front of it, and the
/// depth that says whether anything is draining it.
///
/// ⚠ **The queue's three tokens are minted by nobody.** `urn:cap:space:{out,read,take}` have
/// no flag in this server's provisioning and appear in no grant it writes, so the HTTP door
/// and the QUIC door both answer a typed `Denied` — the queue is reachable from the
/// owner-only socket, which is the door a person drains it through. [`DEPTH`] is the one
/// reading of this tree that a network caller can get, and it is deliberately a different
/// authority: see [`DepthEndpoint`].
///
/// `policy` is what the depth reads the serious share against — the same
/// `gonk.queue.serious` the Queue page narrows to, so "serious" means one thing on this
/// server ([`crate::config::QueuePolicy`]).
pub fn space(
    trigger: &Trigger,
    activity: Arc<Activity>,
    armed: bool,
    policy: crate::config::QueuePolicy,
) -> Vec<Arc<dyn Space>> {
    let queue = Arc::new(trigger.clone());
    vec![
        Arc::new(
            ikigai_core::EndpointSpace::new()
                .bind(
                    ikigai_core::Exact::new(PASS),
                    PassEndpoint {
                        activity: Arc::clone(&activity),
                    },
                )
                .bind(
                    ikigai_core::Exact::new(DEPTH),
                    DepthEndpoint {
                        trigger: Some(queue),
                        activity,
                        armed,
                        policy,
                    },
                ),
        ) as Arc<dyn Space>,
        Arc::new(ikigai_intray::space(trigger.root.clone())) as Arc<dyn Space>,
    ]
}

// ------------------------------------------------------------------- going live

/// A concrete review IRI over `root`, for reading the review's contract off this kernel.
///
/// ⚠ The path is a placeholder and no file is read: `Kernel::describe` answers from the
/// endpoint's `Description`, which is the same for every path the template matches. A real
/// path would work identically and would suggest, wrongly, that this server had chosen one.
pub fn review_probe_iri(root: &str) -> String {
    format!("urn:repo:{root}:review:{PROBE_PATH}")
}

/// The placeholder [`review_probe_iri`] uses. Any path the template accepts would do.
const PROBE_PATH: &str = "README.md";

/// ⚠⚠ **Refuse to arm a reviewer whose grant cannot do the job — or can do too much.**
///
/// Run at startup, against the contract of the review this server would actually issue, so
/// both failures are a refusal to start rather than a queue that dead-letters every tuple
/// with a permission error nobody is watching.
///
/// Two directions, and the second is the one this whole design rests on:
///
/// - **Too little.** Every scope `review` declares is checked the way the kernel will check
///   it — `Capability::allows` over the capability this grant builds — so the offering
///   wildcard `urn:cap:net:*` is satisfied by the narrow `urn:cap:net:127.0.0.1` exactly as
///   it will be at dispatch, and a grant that names `localhost` for a peer at `127.0.0.1`
///   fails HERE instead of on the first commit. The browse graph's two store doors are
///   checked too: they are not on the review's contract (a finding is written through a
///   sub-request, and a capability travels down unchanged), and without them a pass derives
///   an answer and cannot record it.
/// - **Too much.** `urn:cap:annotate` in a reviewer's grant is the interlock gone. browse
///   0.5.0 made a pass unable to publish *by arithmetic*; handing the reviewer the publish
///   token restores exactly the thing Brian's rule forbids — "nothing gets published to Gonk
///   except by the human" — and it would do so silently, because everything would work.
///
/// # Errors
///
/// With the token named and the line that fixes it, in both directions.
pub fn check_reviewer(
    hub: &Kernel,
    review_iri: &str,
    host: &str,
    scopes: &[String],
) -> std::result::Result<(), String> {
    let capability = Capability::scoped(scopes.to_vec());
    if satisfies(&capability, ikigai_browse::CAP_ANNOTATE) {
        return Err(format!(
            "the reviewer grant carries `{}` — the token that PUBLISHES. A review pass \
             produces pending findings and cannot reach the annotation family on its own \
             (ikigai-browse 0.5.0), and that arithmetic is the only thing standing between \
             an armed trigger and unattended publishing. Remove it from this grant; the \
             signed-in person's grant is where it belongs",
            ikigai_browse::CAP_ANNOTATE
        ));
    }
    let target = Iri::parse(review_iri).map_err(|e| format!("{review_iri}: {e}"))?;
    let described = hub.describe(&target).ok_or_else(|| {
        format!("`{review_iri}` does not resolve on this server, so nothing would run a pass")
    })?;
    let required = described
        .action_specs()
        .into_iter()
        .find(|spec| spec.verb == Verb::Source)
        .map(|spec| spec.requires)
        .unwrap_or_default();
    let store = crate::grants::browse_graph_grants(crate::grants::Authority::Write)?;
    for scope in required.iter().chain(store.iter()) {
        if !satisfies(&capability, scope) {
            return Err(format!(
                "the reviewer grant does not satisfy `{scope}`, which a review pass needs. \
                 Every pass would dead-letter with a permission error, so this server \
                 refuses to arm instead.\n{}",
                grant_stanza(hub, review_iri, host)
            ));
        }
    }
    Ok(())
}

/// Whether `capability` satisfies a DECLARED scope, the way the kernel will.
///
/// ⚠⚠ **This is a hand copy of `ikigai_core`'s `cap_satisfies`, because that predicate is
/// `pub(crate)`.** `Capability::allows` is exact containment and is the WRONG test for a
/// declared scope: a trailing `*` is the parameterized-ACL family form — `urn:cap:net:*`
/// means "holds some grant under this prefix" — and the kernel resolves it with a prefix
/// match before dispatch. Using `allows` here would make this whole check refuse every
/// correct reviewer grant, which it did on the first draft.
///
/// ★ Reported up as friction: a host cannot pre-flight a grant against a contract without
/// re-deriving this, and a re-derivation that drifts gives a startup check that disagrees
/// with the kernel in one direction or the other. Both directions are bad, and the wrong one
/// is worse: a check that is more permissive than the kernel passes a grant every pass then
/// dead-letters on.
fn satisfies(capability: &Capability, scope: &str) -> bool {
    match scope.strip_suffix('*') {
        Some(prefix) => match capability.scopes() {
            None => true, // root
            Some(held) => held.iter().any(|s| s.starts_with(prefix)),
        },
        None => capability.allows(scope),
    }
}

/// The `grants.json` stanza this kernel's own contracts say a reviewer needs — for a refusal
/// message and for the banner.
///
/// ★ **The message an operator is reading is the one place a remembered list would do the
/// most damage**, so this is derived from [`reviewer_grant_shape`] like everything else, and
/// it prints JSON they can paste rather than prose they must translate.
pub fn grant_stanza(hub: &Kernel, review_iri: &str, host: &str) -> String {
    match reviewer_grant_shape(hub, review_iri, host) {
        Ok(scopes) => format!(
            "  This kernel's contracts say a reviewer grant is:\n    \"reviewer\": {}\n  \
             ⚠ WITHOUT `{}` — a reviewer that may publish is the interlock gone.",
            serde_json::to_string_pretty(&scopes)
                .unwrap_or_default()
                .replace('\n', "\n    "),
            ikigai_browse::CAP_ANNOTATE
        ),
        Err(e) => format!("  (the shape could not be read off this kernel: {e})"),
    }
}

/// Put a reactor over this queue: catch up on what is waiting, then watch for drops.
///
/// ★ **`with_host_authority` is the whole reason this is safe to do at all**, and it was
/// added to `ikigai-intray` 0.1.24 the day before this arc, for this
/// ([#445](http://localhost:1060/l/default/item/445), `ikigai-cli` PR #347). Without it a
/// reactor takes its handler's authority from `<root>/{space}/cap` — a file in the same
/// directory as the `inbox/` anything can drop into. With it the HOST supplies the
/// capability and the crate never reads that file, so the authority a pass runs under is the
/// one an operator wrote into `grants.json` and this server already checks on every
/// certificate and every passkey it admits.
///
/// ⚠ **A space that is not ours gets an EMPTY capability, not the reviewer's.** The watch is
/// recursive over the whole spaces tree, so a second space appearing beside this one would
/// otherwise fire its handler under the reviewer's authority. gonk creates only its own and
/// writes a `handler` only there, so this is belt-and-braces — and it is the cheap half of
/// the two.
///
/// # Errors
///
/// When the handler cannot be written, or a `cap` file is sitting in a space this reactor
/// would now IGNORE. The second is the trap the host seam introduces while closing the
/// other: under `with_host_authority` such a file does nothing at all, and an operator who
/// wrote one believes it is bounding a reviewer that it is not.
pub fn arm(
    trigger: &Trigger,
    hub: Arc<Kernel>,
    scopes: &[String],
) -> std::result::Result<(), String> {
    set_handler(trigger, true)?;
    let space = trigger.space.clone();
    let reviewer = Capability::scoped(scopes.to_vec());
    let reactor = ikigai_intray::SpaceReactor::new(
        trigger.root.clone(),
        hub as Arc<dyn ikigai_resolve::Resolver>,
        reviewer.clone(),
    )
    .with_host_authority(move |name| {
        if name == space {
            Some(reviewer.clone())
        } else {
            // Not `None`: `None` means "no opinion", and the reactor would fall back to its
            // own configured capability — which is the reviewer's. An empty capability is
            // the only way to say "this space gets nothing".
            Some(Capability::scoped(Vec::<String>::new()))
        }
    });
    let ignored = reactor.ignored_cap_files();
    if !ignored.is_empty() {
        return Err(format!(
            "{} carr{} a `cap` file, and this server supplies the reactor's authority \
             itself — so that file is INERT: it bounds nothing, and an operator who wrote it \
             is wrong about what a pass may do. Remove it; the authority a pass runs under is \
             `gonk.review.grant` in grants.json",
            ignored
                .iter()
                .map(|name| trigger.root.join(name).join("cap").display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            if ignored.len() == 1 { "ies" } else { "y" }
        ));
    }
    // ⚠⚠ **`watch()` does NOT return immediately, whatever its doc says — the startup
    // catch-up runs in the CALLING thread.** `for name in space_names() { drain(name) }`
    // happens before the watcher thread is spawned, and a drain is one full review pass per
    // waiting tuple. Called inline from `main`, a queue holding forty tuples from the last
    // push would hold this server's doors shut for as long as forty model calls take, and
    // the ledger — the thing gonk exists to serve — would be unreachable the whole time,
    // looking exactly like a hang. So the catch-up goes on a thread of its own and startup
    // continues. ⚠ The refusals above stay SYNCHRONOUS: a `cap` file or an unwritable
    // handler must stop this server, and a refusal on a background thread would not.
    // Reported up: the crate's doc says "returns immediately", and for any host with a
    // non-empty inbox that is not true.
    std::thread::spawn(move || Arc::new(reactor).watch());
    Ok(())
}

/// Write (or remove) the `handler` file that makes this space reactive.
///
/// ⚠ **Rewritten at every startup, and REMOVED when this server is not armed.** The file is
/// read per tuple by `SpaceReactor` and names what fires, so it is a control surface living
/// in the tree a dropper writes into (see the module doc). gonk owns this tree and is its
/// only writer outside `inbox/`, so the honest posture is that gonk's value is the value:
/// an unarmed gonk leaves nothing behind for a later armed one to fire, and an armed one does
/// not inherit whatever was there.
///
/// # Errors
///
/// When the file cannot be written or removed — a queue whose handler is not what this
/// server says it is must not start.
pub fn set_handler(trigger: &Trigger, armed: bool) -> std::result::Result<(), String> {
    let path = trigger.dir().join(HANDLER_FILE);
    if !armed {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!(
                "{} could not be removed, and this server is not armed — a handler left \
                 behind is what a reactor fires: {e}",
                path.display()
            )),
        };
    }
    std::fs::write(&path, format!("{PASS}\n"))
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    restrict_file(&path);
    Ok(())
}

/// `0600` on the handler file.
#[cfg(unix)]
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) {}

/// Create the trigger's directories, `0700`, before anything can write into them.
///
/// The inbox exists up front for the reason `ikigai-intray`'s own watcher creates it up
/// front: a directory that appears later races whatever writes into it, and the FIRST tuple
/// a space ever receives is the one most likely to be lost. gonk has a second reason —
/// `review request` writes into the inbox with no server running, so the tree must be there
/// whether or not this process has started.
pub fn prepare(trigger: &Trigger) -> std::result::Result<(), String> {
    for dir in [trigger.dir(), trigger.inbox()] {
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        restrict(&dir)?;
    }
    Ok(())
}

/// `0700` on a directory this server owns.
#[cfg(unix)]
fn restrict(dir: &Path) -> std::result::Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", dir.display()))
}

#[cfg(not(unix))]
fn restrict(_dir: &Path) -> std::result::Result<(), String> {
    Ok(())
}

/// Drop one tuple into the queue — `ikigai-gonk review request <repo> <path>`.
///
/// ★ **Through the space's own Sink, not by writing a file.** The endpoint computes the
/// blake3 id and publishes with a staging write plus a rename, so a reader never sees a
/// half-written tuple and an identical drop lands on its own name. Re-implementing that in
/// a shell hook would be a second content-addressing scheme, and two ids for one request is
/// exactly the duplicate this queue exists to avoid.
///
/// The kernel here is a throwaway holding only the space: no store is opened, no door is
/// bound, nothing is dialled. That is what makes this safe in a `post-commit` hook — it
/// cannot block on a model, a peer, or the running server, and it works when gonk is down.
///
/// # Errors
///
/// When the space name is not a name, the tree cannot be created, or the drop fails.
pub fn drop_tuple(trigger: &Trigger, tuple: &Tuple) -> std::result::Result<String, String> {
    check_space_name(&trigger.space)?;
    check_request(tuple)?;
    prepare(trigger)?;
    let kernel = ikigai_core::Kernel::new(Arc::new(ikigai_intray::space(trigger.root.clone())));
    let iri = Iri::parse(format!("urn:space:{}", trigger.space))
        .map_err(|e| format!("urn:space:{}: {e}", trigger.space))?;
    let request = Request::new(Verb::Sink, iri).with_arg(
        "content",
        ikigai_core::ArgRef::Inline(tuple_turtle(tuple).into_bytes()),
    );
    let capability = ikigai_core::Capability::scoped([ikigai_intray::CAP_OUT]);
    // Through `ikigai-resolve`'s blocking seam rather than an executor of our own: this runs
    // from `main` before any runtime exists, which is the one place in this binary where
    // `block_on` is not the panic `crate::mount` documents.
    let (answer, _) = ikigai_resolve::Resolver::issue_as(&kernel, request, &capability)
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&answer.bytes).trim().to_string())
}

/// How deep the queue is — or why that number is not available.
///
/// ★ **Three answers, never one, because two of them are "no number" and they mean opposite
/// things.** A page that renders `0` for an unconfigured queue, an empty queue and an
/// unreadable one says "nothing has happened" in all three cases; only one of those is true.
/// Ledger [#446](http://localhost:1060/l/default/item/446) is that shape, and the Queue page
/// is the one surface whose entire job is to show process state, so it is the last place a
/// silent absence belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Depth {
    /// No `gonk.review.space`: this server binds no queue, and nothing can drop into one.
    NotConfigured,
    /// The three stages' counts. `outbox` and `error` are the reactor's, and are zero on
    /// this server because it runs no reactor — they are read anyway, so that the day one
    /// runs the page does not have to change.
    Counted {
        /// Waiting to be reviewed.
        inbox: usize,
        /// Handled.
        outbox: usize,
        /// Dead-lettered.
        error: usize,
    },
    /// The tree is configured and could not be read. The string is the reason.
    Unreadable(String),
}

/// [`Depth`] for a configured trigger, or [`Depth::NotConfigured`] for `None`.
///
/// ⚠ **This counts FILES, where everything else in this server reads through the kernel** —
/// and the reason was a real gap rather than a shortcut. The count lives behind
/// `Source urn:space:{name}`, which requires `urn:cap:space:read`, and this server mints that
/// token for NOBODY ([`space`]): not for an anonymous caller, not for a passkey identity, not
/// for a certificate. So there is no capability any page could be rendering under that would
/// be allowed to ask the kernel, and a read through it would be a typed `Denied` on every
/// request — which is exactly the missing number the page exists to supply.
///
/// ★ Ledger [#464](http://localhost:1060/l/default/item/464) put two options on that: make
/// the depth a resource of gonk's own with its own floor, or make the space's read token
/// mintable. [#466](http://localhost:1060/l/default/item/466) chose the first — gonk is the
/// drainer now, so gonk knows the depth internally — and [`DEPTH`] is that resource. This
/// function is still the counting, because a directory listing is what there is; what
/// changed is that the number is now reachable by a caller, under a capability that is
/// argued rather than absent.
pub fn depth(trigger: Option<&Trigger>) -> Depth {
    let Some(trigger) = trigger else {
        return Depth::NotConfigured;
    };
    // The inbox is created 0700 by `prepare` at startup, so a missing one is a fact worth
    // reporting rather than a zero. The other two stages belong to a reactor that has never
    // run here, so their absence IS zero.
    let inbox = match count_tuples(&trigger.inbox()) {
        Ok(n) => n,
        Err(e) => return Depth::Unreadable(format!("{}: {e}", trigger.inbox().display())),
    };
    Depth::Counted {
        inbox,
        outbox: count_tuples(&trigger.dir().join("outbox")).unwrap_or(0),
        error: count_tuples(&trigger.dir().join("error")).unwrap_or(0),
    }
}

/// The `*.tuple` files in one stage directory.
fn count_tuples(dir: &Path) -> std::io::Result<usize> {
    Ok(std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(".tuple"))
        })
        .count())
}

// ------------------------------------------------------------------- the depth resource

/// The whole of what [`DEPTH`] answers: the queue, whether anything is draining it, and what
/// that thing has done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// What is in the three stage directories, or why that is not a number.
    pub depth: Depth,
    /// Whether a reactor is watching this queue in this process.
    pub armed: bool,
    /// What this process has spent on passes.
    pub passes: Passes,
    /// The serious set (`gonk.queue.serious`) the share below is read against.
    pub policy: crate::config::QueuePolicy,
}

impl Status {
    /// Findings minted by derived passes this run that carry a serious word.
    pub fn serious(&self) -> u64 {
        self.passes
            .by_severity
            .iter()
            .filter(|(word, _)| self.policy.is_serious(word))
            .map(|(_, n)| n)
            .sum()
    }

    /// The serious share of this run's minted findings, as a whole percentage — `None` until
    /// a derived pass has minted anything.
    ///
    /// # ★ Why a queue that filters on a label reports this number
    ///
    /// Severity is SELF-REPORTED by the model, and ledger
    /// [#449](http://localhost:1060/l/default/item/449) measured that a prompt asking for
    /// "major or worse" moved the serious share 27% → 62% by RE-LABELLING, not by finding
    /// more. Gating the Queue page on the word ([#496](http://localhost:1060/l/default/item/496))
    /// makes the word load-bearing, so the share becomes the tripwire: it was 27–33% on the
    /// incumbent model and 42% on q8 ([#491](http://localhost:1060/l/default/item/491)), and a
    /// jump with no model or prompt change is the label inflating — re-examine the gate, do
    /// not celebrate the number. The other defence is negative and lives nowhere in this crate
    /// on purpose: nothing gonk renders or sends can tell a pass that only serious findings get
    /// read.
    pub fn serious_share_percent(&self) -> Option<u64> {
        let all = self.passes.findings();
        (all > 0).then(|| self.serious() * 100 / all)
    }
    /// One sentence, and it is the sentence a human reads to tell a SLOW queue from a STUCK
    /// one.
    ///
    /// ★ The distinction is the whole point and it is not a number: a queue that is deep and
    /// has a pass in flight is working; a queue that is deep with nothing in flight and a
    /// last-pass time growing is wedged. Both render as "12 waiting" if you only print the
    /// count, and one of them needs somebody.
    pub fn sentence(&self, now_ms: u64) -> String {
        let (waiting, done) = match &self.depth {
            Depth::NotConfigured => {
                return "No review queue is configured (`gonk.review.space`), so nothing is \
                        dropping review requests here."
                    .to_string()
            }
            Depth::Unreadable(why) => {
                return format!(
                    "The review queue could not be read, so its depth is unknown: {why}"
                )
            }
            Depth::Counted {
                inbox,
                outbox,
                error,
            } => (*inbox, handled(*outbox, *error)),
        };
        // ⚠ "empty" keeps its own affirmative sentence rather than becoming "0 waiting".
        // Ledger #446: "nothing is waiting" and "the page failed to load" must not look
        // alike, and a bare zero is halfway to looking like the second.
        let mut out = if waiting == 0 {
            format!("The review queue is empty: no request is waiting to be reviewed{done}.")
        } else {
            format!(
                "{waiting} review request{} waiting to be reviewed{done}.",
                if waiting == 1 { " is" } else { "s are" }
            )
        };
        if !self.armed {
            out.push_str(
                " NOTHING IS DRAINING THIS QUEUE: this server is not armed \
                 (`gonk.review.arm`), so a request waits for a person to run it.",
            );
            return out;
        }
        match self.passes.in_flight_since_ms {
            Some(since) => out.push_str(&format!(
                " A pass has been running for {}.",
                humanize_ms(now_ms.saturating_sub(since))
            )),
            None if waiting > 0 => out.push_str(" NONE IN FLIGHT."),
            None => {}
        }
        out.push_str(&format!(
            " {} pass(es) this run, {} failed",
            self.passes.succeeded, self.passes.failed
        ));
        match self.passes.last_end_ms {
            Some(end) => out.push_str(&format!(
                "; the last ended {} ago.",
                humanize_ms(now_ms.saturating_sub(end))
            )),
            None if self.passes.in_flight_since_ms.is_none() => {
                out.push_str("; none since this server started.");
            }
            None => out.push('.'),
        }
        if self.stuck() {
            out.push_str(
                " ⚠ A queue that is not empty with nothing in flight is a STUCK reviewer: the \
                 watcher thread is gone, and only a restart of this server brings it back.",
            );
        }
        // The serious share, only once there is one: "0 of 0" is not a number a person can
        // read anything into, and the sentence is already long.
        if let Some(percent) = self.serious_share_percent() {
            let all = self.passes.findings();
            out.push_str(&format!(
                " {all} finding{} minted this run, {} serious ({percent}%).",
                if all == 1 { "" } else { "s" },
                self.serious()
            ));
        }
        out
    }

    /// The numbers, for anything that wants to compute rather than read.
    fn json(&self, now_ms: u64) -> String {
        let (configured, inbox, outbox, error, unreadable) = match &self.depth {
            Depth::NotConfigured => (false, None, None, None, None),
            Depth::Unreadable(why) => (true, None, None, None, Some(why.clone())),
            Depth::Counted {
                inbox,
                outbox,
                error,
            } => (true, Some(*inbox), Some(*outbox), Some(*error), None),
        };
        serde_json::json!({
            // ★ The prose, beside the numbers, so a face that renders the sentence and a
            // caller that computes on the numbers are reading ONE answer. The Queue badge
            // takes this string verbatim rather than re-deriving it, which is the only way
            // the header, the page and the socket cannot disagree about what is happening.
            "sentence": self.sentence(now_ms),
            "configured": configured,
            "armed": self.armed,
            "waiting": inbox,
            "handled": outbox,
            "dead_lettered": error,
            "unreadable": unreadable,
            "in_flight": self.passes.in_flight_since_ms.is_some(),
            "in_flight_ms": self
                .passes
                .in_flight_since_ms
                .map(|since| now_ms.saturating_sub(since)),
            "passes_started": self.passes.started,
            "passes_succeeded": self.passes.succeeded,
            "passes_failed": self.passes.failed,
            "since_last_pass_ms": self
                .passes
                .last_end_ms
                .map(|end| now_ms.saturating_sub(end)),
            "last_pass_ms": self.passes.last_ms,
            "stuck": self.stuck(),
            // ★ The serious share (ledger #496): the tripwire for a label that inflates
            // under a gate that reads it. See `serious_share_percent`.
            "findings_this_run": self.passes.findings(),
            "serious_this_run": self.serious(),
            "serious_share_percent": self.serious_share_percent(),
            "findings_by_severity": self.passes.by_severity,
            "serious": self.policy.serious,
        })
        .to_string()
    }

    /// The one boolean an operator's eye is looking for: armed, something waiting, nothing
    /// running.
    pub fn stuck(&self) -> bool {
        self.armed
            && self.passes.in_flight_since_ms.is_none()
            && matches!(self.depth, Depth::Counted { inbox, .. } if inbox > 0)
    }
}

/// `", 3 handled"` / `", 3 handled and 1 dead-lettered"` / `""`.
fn handled(outbox: usize, error: usize) -> String {
    match (outbox, error) {
        (0, 0) => String::new(),
        (n, 0) => format!(", {n} handled"),
        (0, n) => format!(", {n} dead-lettered"),
        (n, e) => format!(", {n} handled and {e} dead-lettered"),
    }
}

/// A duration as a badge reads it.
fn humanize_ms(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        s if s < 90 => format!("{s}s"),
        s if s < 5400 => format!("{}m", s / 60),
        s => format!("{}h{}m", s / 3600, (s % 3600) / 60),
    }
}

/// `urn:iki:gonk:review:depth` — the queue's depth and the reviewer's liveness.
///
/// # ★ Its own floor, and the argument for that floor
///
/// The space's own `urn:cap:space:read` is minted for nobody, which is what made this number
/// unreachable ([#464](http://localhost:1060/l/default/item/464)). This resource does not
/// reuse it: it declares **`urn:cap:browse:read:*`**, and the reason is what the depth
/// actually discloses. A queued request is a `(repo, path)` pair naming a file in a browse
/// root; even reduced to a count it is a statement about those roots and about what someone
/// is working on. A caller who may not read the roots has no business reading the shape of
/// the work over them — the same argument `urn:iki:gonk:backup:status` makes about naming
/// every graph.
///
/// It also lands exactly where the Queue page already stands: that page is offered only to a
/// caller who may read a root AND may decide, so the badge on it never asks for something its
/// viewer lacks. An anonymous loopback caller holds ledger tokens and nothing else, so it is
/// a typed `Denied` — the queue's depth is process state, not public.
///
/// ⚠ It is `Expiry::Always` by omission: nothing here calls `.cacheable()`, because the whole
/// value of the number is that it is the current one. A cached depth is a badge that lies
/// about a stuck queue, which is the single thing it exists to show.
pub struct DepthEndpoint {
    /// The queue, or `None` when none is configured.
    pub trigger: Option<Arc<Trigger>>,
    /// The counters [`PassEndpoint`] writes.
    pub activity: Arc<Activity>,
    /// Whether [`arm`] ran in this process.
    pub armed: bool,
    /// The serious set the share is read against (`gonk.queue.serious`).
    pub policy: crate::config::QueuePolicy,
}

impl DepthEndpoint {
    /// The status as it stands.
    pub fn status(&self) -> Status {
        Status {
            depth: depth(self.trigger.as_deref()),
            armed: self.armed,
            passes: self.activity.snapshot(),
            policy: self.policy.clone(),
        }
    }
}

#[async_trait]
impl Endpoint for DepthEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "gonk-review-depth answers Source (how deep the queue is), not {:?}",
                inv.request.verb
            )));
        }
        let status = self.status();
        let now = inv.now().map(|t| t.as_millis()).unwrap_or_else(now_ms);
        if matches!(inv.inline_str("as"), Ok(JSON)) {
            return Ok(Representation::new(
                ReprType::new(JSON).with_param("charset", "utf-8"),
                status.json(now).into_bytes(),
            ));
        }
        Ok(Representation::new(
            ReprType::new("text/plain").with_param("charset", "utf-8"),
            status.sentence(now).into_bytes(),
        ))
    }

    fn name(&self) -> &str {
        "gonk-review-depth"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-review-depth")
            .title("How deep the review queue is, and whether anything is draining it")
            .summary(
                "The git-event review queue's three stages (waiting, handled, \
                 dead-lettered), whether this process is ARMED to drain it, whether a pass \
                 is in flight right now, and what this run has spent. ⚠ It is a LIVENESS \
                 signal, not a statistic: `watch()` catches up at startup, so a watcher \
                 thread that dies while this server lives drains nothing and says nothing \
                 until a restart. A queue that is armed and not empty with nothing in \
                 flight is stuck — `stuck` in the JSON face says so directly. Four \
                 answers, and three of them are not a number: not configured, unreadable, \
                 empty and counted are different statements.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(ikigai_browse::CAP_WILDCARD)
            .input(
                ArgSpec::new("as")
                    .optional()
                    .class(XSD_STRING)
                    .one_of(["text/plain", JSON])
                    .default_value("text/plain")
                    .summary("the face: a sentence, or the numbers"),
            )
            .output("text/plain")
            .output(JSON)
    }
}

/// The tuple ids waiting in a trigger's inbox, sorted — what the banner counts.
pub fn pending(trigger: &Trigger) -> usize {
    match std::fs::read_dir(trigger.inbox()) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with(".tuple"))
            })
            .count(),
        Err(_) => 0,
    }
}
