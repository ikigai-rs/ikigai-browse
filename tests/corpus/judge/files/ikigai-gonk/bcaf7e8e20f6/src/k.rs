//! The host's `/k/` adapter, and the shell the browse family's HTML faces render inside.
//!
//! ```text
//! /k?c=source {iri} [k=v …]   urn:iki:gonk:k                    Source  one read, the caller's cap
//! /k?c=sink {iri} [k=v …]     urn:iki:gonk:k                    Sink    the annotation and finding families
//! /browse/{iri}               urn:iki:gonk:page:browse:{iri}    Source  the page those faces live in
//! /browse                     urn:iki:gonk:page:browse          Source  the roots this caller may read
//! ```
//!
//! # ★ The buttons already exist; this is the door they were missing
//!
//! Nothing here renders a browse affordance. `ikigai-browse` does: every one of its HTML
//! faces emits `hx-get="/k/source {iri} [k=v …]"` for a read and
//! `hx-post="/k/sink urn:iki:annotation"` for a write, and it calls that "the HOST's `/k/`
//! adapter". Explain, Explain With, the crumbs, the PR family and (when a released browse
//! carries it) Review are all that one shape. **So this module knows verbs and IRIs and
//! nothing about buttons** — a face that grows a new affordance tomorrow works here the day
//! gonk's pin moves, and a door that recognized "the explain button" would have to be
//! edited for each one.
//!
//! # ⚠ The one difference from the same adapter at `ikigai-web`'s port, and why it exists
//!
//! `ikigai-web` the standalone server parses the raw request-target itself, so
//! `GET /k/source urn:repo:x:file:src/lib.rs as=text/html` reaches its `k_command` intact.
//! gonk's door is a different program: the `ikigai-web` LIBRARY, which percent-decodes the
//! path and then rebuilds an IRI from it — through a route template, or by the mechanical
//! `/a/b/c` → `urn:a:b:c` join. **A command cannot survive either**, for two independent
//! reasons measured against the live server on 2026-09-19:
//!
//! - the command carries SPACES, and a space is not legal in an IRI, so the rebuilt target
//!   fails `Iri::parse` and the library answers `400 not a resource path` before any gonk
//!   code runs — `GET /k/source%20urn:repo:ikigai-core:file:README.md%20as=text/html` at
//!   1060 was exactly that 400;
//! - browse's file IRIs carry SLASHES, and a route pattern's `{var}` captures exactly one
//!   path segment, so the path is not even one capture.
//!
//! Percent-encoding does not help: the library decodes before it templates, so `%20` is a
//! space by the time a route sees it. There is no host-side seam that takes a raw path.
//!
//! ★ So **the command travels as an ARGUMENT** — `/k?c=<command>` — which the library
//! carries verbatim (a query value is decoded per value, never split). The grammar INSIDE
//! `c` is browse's own, unchanged and unextended: `source|sink <iri> [k=v …]`, whitespace
//! separated. The path spelling browse emits is folded into that query form in the browser,
//! by `web/gonk.js`, on `htmx:configRequest` — the same file, the same trust domain and the
//! same failure mode as htmx itself, which is what issues every one of these requests. A
//! caller with no browser writes the query form directly.
//!
//! ⚠ **The real fix is one layer down** and is not gonk's to make: `ikigai-web`'s route
//! table has no trailing-segment capture and no raw-path seam, so no host of that library
//! can serve a command-shaped adapter. Reported to the hub rather than worked around twice.
//!
//! # Capability: the caller's, and nothing else
//!
//! Every request through this module is issued with [`Invocation::issue`], which carries the
//! CALLER's capability — [`crate::doors::http_cap`]'s per-request one. The adapter holds no
//! authority of its own and declares none: what a reader may read and whether a click may
//! spend inference is decided by the browse resources' own `requires`, one hop in.
//!
//! Three consequences worth naming, because each is a test in `tests/browse.rs`:
//!
//! - **an anonymous loopback caller cannot derive.** `gonk.http.ledger`'s anonymous grant is
//!   ledger tokens only; every browse row declares `urn:cap:browse:read:*` and every
//!   derivation also `urn:cap:net:*`, which gonk mints for nobody. The refusal is the
//!   kernel's typed `Denied`, before dispatch, naming the token;
//! - **a cross-site POST cannot annotate.** `/k?c=sink …` is a POST like any other, so it
//!   goes through the same [`crate::doors::http_scopes`] check that closed the first arc's
//!   hole: a foreign `Origin`/`Sec-Fetch-Site` (or a foreign `Host`, on any method) computes
//!   an EMPTY capability, and `urn:iki:annotation`'s Sink requires `urn:cap:annotate`;
//! - **a Sink reaches the annotation family and the finding family, and nothing else.** The
//!   bound `ikigai-web`'s adapter draws, and the one [`crate::web`]'s `Act` draws around the
//!   ledger: the adapter never widens a door's write surface beyond what browse's own faces
//!   post. gonk serves only the `urn:iki:annotation` spelling — the manifest installs no alias
//!   for the older `urn:annotation` one, so neither does this.
//! - ★ **The finding family joined in ikigai-browse 0.14.0** (ledger #653): a declined card in
//!   browse's own faces carries a REVISE form (`hx-post="/k/sink urn:iki:finding:{id}"` with
//!   `revises=`), and so does every pending card. Its Sink requires `urn:cap:annotate` — the
//!   same authority the annotation family's does — so admitting it widens no grant. browse
//!   stamps no `made=` on those forms, by design ("how a decision was made is the host's to say
//!   at its door"): this door stamps `made=single` on every finding Sink, and refuses a
//!   command or form that names `made` or `batch` itself.
//!
//! # The answer is the target's answer, whole
//!
//! A `source` returns the representation the resource produced — its bytes, its media type,
//! its cache validity and its golden threads — not a copy of the bytes in a fresh envelope.
//! That is what makes the adapter invisible: a cacheable read stays cacheable to the hub
//! that caches it, and a watcher's thread still cuts the entry the hub holds. It is also why
//! this resource declares no `outputs` and waives three of `ikigai-conformance`'s checks
//! (see `tests/conformance.rs`): its faces, its capability floor and its cache validity all
//! belong to whatever the command names, and are not knowable until it is read.
//!
//! # ⚠ What this door does NOT bound: what one click costs
//!
//! One HTTP request here is exactly one kernel request. That is the whole of the adapter's
//! economics, and it is deliberately all of it: a directory-grain explain fans out per child
//! INSIDE `ikigai-browse` (`explain.rs::derive_dir` sources each child's own explanation
//! recursively, bottoming out at files, with unchanged children as archive hits), so one
//! click on a directory can be many model calls, and none of that is visible from here. The
//! door must not multiply it either — nothing in the shell prefetches, and no face is
//! fetched that a person did not ask for. Counting spend over time is ledger #308 and is not
//! a thing a door can do.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Description, Endpoint, Error, Invocation, Iri, Representation,
    Request, Result, Verb,
};

use crate::render::{self, element, envelope};
use crate::web::{self, Web};

/// The adapter's own name. Bound in the HTTP door's kernel only, like every page.
pub const K_IRI: &str = "urn:iki:gonk:k";

/// The shell page's grammar — a trailing capture, so the start IRI keeps its colons AND its
/// slashes (`urn:repo:ikigai-core:file:src/lib.rs`).
pub const BROWSE_PAGE_TEMPLATE: &str = "urn:iki:gonk:page:browse:{start}";

/// The capability prefix every browse row's declared floor is written against
/// (`urn:cap:browse:read:*`). Held here as the PREFIX rather than the wildcard because that
/// is how the kernel reads it: a `…:*` requirement is satisfied by any grant under the
/// prefix, so a per-root token satisfies it and a literal-string test would not.
///
/// ★ Taken from `ikigai-browse` rather than typed, so the spelling cannot drift from the
/// crate that enforces it.
const BROWSE_READ_PREFIX: &str = ikigai_browse::CAP_PREFIX;

/// `urn:iki:gonk:page:browse` — the browse family's landing page, and the only browse IRI
/// here that is not a template. See [`BrowseRoots`].
pub const ROOTS_IRI: &str = "urn:iki:gonk:page:browse";

/// Where that page is served, for the header link (`crate::web`'s `nav`).
pub const ROOTS_PATH: &str = "/browse";

/// The annotation family — one of the two a `sink` command may reach.
const ANNOTATION_ROOT: &str = "urn:iki:annotation";

/// The command argument's name.
const COMMAND: &str = "c";

// ----------------------------------------------------------------------- the adapter

/// `urn:iki:gonk:k` — the `/k/` adapter.
pub struct KAdapter {
    pub web: Arc<Web>,
}

/// One parsed command: the verb word, its target, and its `k=v` arguments in order.
struct Command {
    word: String,
    target: Iri,
    args: Vec<(String, String)>,
}

/// `source|sink <iri> [k=v …]`, whitespace separated — `ikigai-browse`'s grammar, not ours.
fn parse_command(text: &str) -> Result<Command> {
    let mut tokens = text.split_whitespace();
    let (Some(word), Some(iri)) = (tokens.next(), tokens.next()) else {
        return Err(Error::InvalidArgument {
            name: COMMAND.to_string(),
            detail: "expected `source <iri> [k=v …]` or `sink <iri> [k=v …]`".to_string(),
        });
    };
    let target = Iri::parse(iri).map_err(|e| Error::InvalidArgument {
        name: COMMAND.to_string(),
        detail: format!("`{iri}` is not a resolvable IRI: {e}"),
    })?;
    let mut args = Vec::new();
    for token in tokens {
        let Some((key, value)) = token.split_once('=') else {
            return Err(Error::InvalidArgument {
                name: COMMAND.to_string(),
                detail: format!("`{token}`: a command's arguments are `key=value`"),
            });
        };
        args.push((key.to_string(), value.to_string()));
    }
    Ok(Command {
        word: word.to_string(),
        target,
        args,
    })
}

impl KAdapter {
    /// ★ Refuse an argument the target's own contract does not name, rather than passing it
    /// to be dropped in silence — the rule [`crate::web::Act`] states for forms, for the
    /// same reason and with more at stake: on a derivation a dropped `provider=` does not
    /// fail, it spends the DEFAULT model's inference and archives the answer under that
    /// model's tag. `as` is exempt because it is the transport's own word for a face and an
    /// endpoint need not declare it to serve one.
    ///
    /// A target whose description declares no action for this verb is passed through
    /// unchecked: there is no contract to check against, and inventing a refusal from an
    /// absent declaration would make the adapter stricter than the resource.
    fn check_declared(&self, command: &Command, verb: Verb) -> Result<()> {
        let Some(description) = self.web.hub.describe(&command.target) else {
            return Ok(()); // resolution reports this, with the kernel's own words
        };
        let Some(spec) = description
            .action_specs()
            .into_iter()
            .find(|spec| spec.verb == verb)
        else {
            return Ok(());
        };
        if spec.inputs.is_empty() {
            return Ok(());
        }
        for (name, _) in &command.args {
            if name == "as" || spec.inputs.iter().any(|input| &input.name == name) {
                continue;
            }
            return Err(Error::InvalidArgument {
                name: name.clone(),
                detail: format!(
                    "`{}` does not declare this input for {verb:?}; a command may send only \
                     what the resource's own contract names",
                    command.target
                ),
            });
        }
        Ok(())
    }

    /// `source <iri> [k=v …]` — one read, under the caller's capability, answered with the
    /// representation the resource produced (its media type, its bytes, its golden threads).
    async fn source(&self, inv: &Invocation<'_>, command: Command) -> Result<Representation> {
        self.check_declared(&command, Verb::Source)?;
        // ★ Pending findings drawn on the FILE page (ledger
        // [#496](http://localhost:1060/l/default/item/496)). Brian: "Show everything other
        // than critical and major as annotations on the file page" — and, the same day,
        // "auto-publish nothing, keep it as proposals." So on the ONE command that asks
        // browse for a file page as HTML, gonk adds `proposals=<words>`: the finding
        // contract's severity set less `gonk.queue.serious`, read from the contract at call
        // time ([`crate::queue::proposal_words`]) — never a list held here. browse 0.7.0
        // draws those pending findings beside their lines as proposal marks, labelled
        // proposal and never annotation; the queue stays the place for decisions.
        //
        // Three guards, each a refusal to guess. The caller's own `proposals=` wins (a
        // person asking for a specific set gets it). The argument is added only when the
        // bound browse DECLARES it — `check_declared` above refuses a caller's undeclared
        // argument by name, and gonk holds itself to the same rule rather than sending an
        // input a 0.6.x mount would drop in silence. And a contract that cannot be read
        // sends nothing: a page with no proposals panel is honest, a page drawn from a
        // guessed list is not.
        let is_file_page_as_html = {
            let t = command.target.as_str();
            t.starts_with("urn:repo:")
                && t.contains(":file:")
                && command
                    .args
                    .iter()
                    .any(|(k, v)| k == "as" && v == "text/html")
        };
        let caller_chose = command.args.iter().any(|(k, _)| k == "proposals");
        let proposals = if is_file_page_as_html
            && !caller_chose
            && self.declares_input(&command.target, Verb::Source, "proposals")
        {
            crate::queue::proposal_words(&self.web.hub, &self.web.queue)
                .filter(|words| !words.is_empty())
                .map(|words| words.join(","))
        } else {
            None
        };
        let mut request = Request::new(Verb::Source, command.target);
        for (name, value) in command.args {
            request = request.with_arg(name, ArgRef::Inline(value.into_bytes()));
        }
        if let Some(words) = proposals {
            request = request.with_arg("proposals", ArgRef::Inline(words.into_bytes()));
        }
        inv.issue(request).await
    }

    /// Whether the target's own contract declares `name` as an input of `verb` — the
    /// question [`Self::check_declared`] asks of a caller's arguments, asked of gonk's own.
    fn declares_input(&self, target: &Iri, verb: Verb, name: &str) -> bool {
        self.web
            .hub
            .describe(target)
            .and_then(|d| d.action_specs().into_iter().find(|spec| spec.verb == verb))
            .is_some_and(|spec| spec.inputs.iter().any(|input| input.name == name))
    }

    /// `sink <iri> [k=v …]` — the annotation family, and one finding at a time.
    ///
    /// The body follows the rule both entrances to `ikigai-web`'s one write route follow:
    /// a form-encoded body becomes invocation arguments (htmx's shape, and what browse's
    /// annotate form posts), any other body arrives as the piped `content` with its
    /// `Content-Type` as `content-type`. The command's own `k=v` come first; the body wins
    /// on a collision.
    async fn sink(&self, inv: &Invocation<'_>, command: Command) -> Result<Representation> {
        let uri = command.target.as_str();
        let annotation = uri == ANNOTATION_ROOT || uri.starts_with(&format!("{ANNOTATION_ROOT}:"));
        // One finding: `urn:iki:finding:{id}`, with an id and nothing after it — a decision
        // node (`…:decision`) is not a Sink target.
        let finding = uri
            .strip_prefix(crate::queue::FINDING_PREFIX)
            .is_some_and(|id| !id.is_empty() && !id.contains(':'));
        if !annotation && !finding {
            return Err(Error::Denied(format!(
                "`{uri}`: this adapter sinks the annotation family (`{ANNOTATION_ROOT}`, or \
                 `{ANNOTATION_ROOT}:{{id}}`) and one finding (`{}{{id}}`), nothing else",
                crate::queue::FINDING_PREFIX
            )));
        }
        let content_type = inv.inline_str("content-type").unwrap_or_default();
        // ⚠ Read as BYTES and refused loudly when they are not text. This adapter's
        // arguments are the text of a command line, so a non-UTF-8 payload has nowhere to
        // go — and a body accepted and then dropped is the truncating kind of bound. An
        // annotation body is text; a caller with bytes to write has the resource's own IRI.
        let body = match inv.inline_arg("content") {
            Ok(bytes) => String::from_utf8(bytes.to_vec()).map_err(|_| Error::InvalidArgument {
                name: "content".to_string(),
                detail: "the request body is not UTF-8, and this adapter's arguments are text"
                    .to_string(),
            })?,
            Err(_) => String::new(),
        };
        let mut args: BTreeMap<String, String> = command.args.iter().cloned().collect();
        if content_type.starts_with("application/x-www-form-urlencoded") {
            args.extend(web::form(&body));
        } else if !body.is_empty() {
            args.insert("content".to_string(), body);
            if !content_type.is_empty() {
                args.insert("content-type".to_string(), content_type.to_string());
            }
        }
        // ★ The door says how a decision was made (ledger #653), as it names an author: a
        // command or form that says it is refused by name, and a finding decision posted here
        // — one finding, one press — is stamped `made=single` when its contract declares it.
        if finding {
            for name in [crate::queue::MADE_ARG, crate::queue::BATCH_ARG] {
                if args.contains_key(name) {
                    return Err(crate::queue::provenance_refused(name));
                }
            }
            if let (true, Some(word)) = (
                self.declares_input(&command.target, Verb::Sink, crate::queue::MADE_ARG),
                crate::queue::made_word(&self.web.hub, crate::queue::MADE_SINGLE),
            ) {
                args.insert(crate::queue::MADE_ARG.to_string(), word.to_string());
            }
        }
        let command = Command {
            args: args.into_iter().collect(),
            ..command
        };
        self.check_declared(&command, Verb::Sink)?;
        let mut request = Request::new(Verb::Sink, command.target);
        for (name, value) in command.args {
            request = request.with_arg(name, ArgRef::Inline(value.into_bytes()));
        }
        inv.issue(request).await
    }
}

#[async_trait]
impl Endpoint for KAdapter {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let command = parse_command(inv.inline_str(COMMAND)?)?;
        let verb = inv.request.verb;
        let expected = match verb {
            Verb::Source => "source",
            Verb::Sink => "sink",
            other => {
                return Err(Error::Endpoint(format!(
                    "`{K_IRI}` answers Source and Sink; not {other:?}"
                )))
            }
        };
        if command.word != expected {
            return Err(Error::InvalidArgument {
                name: COMMAND.to_string(),
                detail: format!(
                    "`{}` is not the command a {verb:?} carries; a {verb:?} reaches this \
                     adapter as `{expected} <iri> [k=v …]`",
                    command.word
                ),
            });
        }
        match verb {
            Verb::Source => self.source(inv, command).await,
            _ => self.sink(inv, command).await,
        }
    }

    fn name(&self) -> &str {
        "gonk-k"
    }

    fn describe(&self) -> Description {
        let command = |what: &str| {
            ArgSpec::new(COMMAND)
                .summary(format!(
                    "the command, `{what} <iri> [key=value …]` — the grammar `ikigai-browse` \
                     authors its htmx affordances against"
                ))
                .class("http://www.w3.org/2001/XMLSchema#string")
        };
        Description::new("gonk-k")
            .title("The host's /k/ adapter")
            .summary(
                "One read or one annotation write, named by a command rather than by this \
                 resource's own IRI — the shape the browse family's HTML faces emit. It runs \
                 under the CALLER's capability and declares none of its own: what may be \
                 read, and whether a click may spend inference, is the target resource's own \
                 `requires`, one hop in. A sink reaches the annotation family and one \
                 finding (`urn:iki:finding:{id}`, whose decisions this door stamps \
                 `made=single`), nothing else.",
            )
            .action(
                ActionSpec::new(Verb::Source)
                    .summary("resolve the command's target and answer its representation")
                    .input(command("source"))
                    .input(
                        ArgSpec::new("as")
                            .summary(
                                "ignored here: the face belongs in the command \
                                 (`… as=text/html`), because that is where the affordance \
                                 puts it",
                            )
                            .class("http://www.w3.org/2001/XMLSchema#string")
                            .optional(),
                    ),
            )
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary(
                        "mint or update one annotation, or decide one finding, from a \
                         form-encoded body",
                    )
                    .input(command("sink"))
                    .input(
                        ArgSpec::new("content")
                            .summary(
                                "the request body: form-encoded fields become the target's \
                                 arguments, anything else is piped as `content`",
                            )
                            .class("http://www.w3.org/2001/XMLSchema#string")
                            .optional(),
                    )
                    .input(
                        ArgSpec::new("content-type")
                            .summary("the body's media type, as the transport read it")
                            .class("http://www.w3.org/2001/XMLSchema#string")
                            .optional(),
                    ),
            )
            .verb(Verb::Meta)
    }
}

// -------------------------------------------------------------------------- the shell

/// `urn:iki:gonk:page:browse:{start}` — the page the faces render inside.
pub struct BrowseShell {
    pub web: Arc<Web>,
}

/// Percent-encode everything outside the unreserved set, so a command carrying an IRI and
/// spaces travels as ONE query value.
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

/// The URL an affordance's command is fetched at — the query form this door speaks.
pub fn k_url(command: &str) -> String {
    format!("/k?{COMMAND}={}", urlencode(command))
}

/// Whether this capability satisfies the floor every browse row declares.
///
/// ⚠ `urn:cap:browse:read:*` is a WILDCARD requirement, which the kernel reads as "holds
/// some grant under this prefix" — so a per-root token satisfies it and
/// `Capability::allows("urn:cap:browse:read:*")`, an exact-string test, does not. This is
/// presentation only: the enforcement is the kernel's, on the row itself.
fn can_browse(inv: &Invocation<'_>) -> bool {
    match inv.capability.scopes() {
        None => true, // root
        Some(held) => held.iter().any(|s| s.starts_with(BROWSE_READ_PREFIX)),
    }
}

/// Whether this capability may annotate — the posture the layout sheet keys on.
///
/// `urn:cap:annotate` is a plain scope, not a wildcard offering, so `allows` IS the test
/// and this is the crate's own constant. Presentation only: `urn:iki:annotation`'s Sink is
/// capability-gated whatever a page shows, so the worst case of getting this wrong is a
/// visible form whose submission is refused.
fn can_annotate(inv: &Invocation<'_>) -> bool {
    inv.capability.allows(ikigai_browse::CAP_ANNOTATE)
}

/// The configured browse roots this caller may READ, in configured order.
///
/// ★ This is `ikigai-browse`'s OWN enforcement read back through its public constants.
/// `granted()` passes a root when the capability holds `urn:cap:browse:read:{root}` or the
/// literal all-roots wildcard — both EXACT scopes — so two [`ikigai_core::Capability::allows`]
/// calls are the whole test and **no prefix rule is reimplemented here**.
///
/// ⚠ That distinction is the line ledger #439 draws, and it is worth stating because the
/// function directly above does the other thing. The kernel's rule for a wildcard
/// REQUIREMENT ("a `…:*` requirement is satisfied by any grant under the prefix") is
/// `pub(crate)`, so a door that wants to ask "may this caller use this affordance?" in
/// general cannot, and [`can_browse`] approximates it with `starts_with`. A ROOT LIST does
/// not need that rule and must not borrow its approximation: a caller holding exactly
/// `urn:cap:browse:read:ikigai-core` may read that one root, and an offer computed from the
/// prefix would list all seven and mean six refusals.
pub(crate) fn readable_roots(web: &Web, inv: &Invocation<'_>) -> Vec<String> {
    web.browse_roots
        .iter()
        .filter(|root| {
            inv.capability
                .allows(&format!("{BROWSE_READ_PREFIX}{root}"))
                || inv.capability.allows(ikigai_browse::CAP_WILDCARD)
        })
        .cloned()
        .collect()
}

/// The tree IRI a root's link opens on — the browse family's own entry face.
fn tree_iri(root: &str) -> String {
    format!("urn:repo:{root}:tree")
}

/// `urn:iki:gonk:page:browse` — the roots this caller may read, each a link into its tree.
///
/// ★ **Why a landing page rather than one nav entry per root.** gonk's ledger nav lists
/// every readable ledger, and the same shape was the obvious first answer here; it does not
/// survive the numbers. This machine configures seven roots and a header that carries seven
/// repository names next to `gonk`, `default` and `SPARQL` is no longer a header. There is
/// also no natural first root to make `Browse` point at — every choice is arbitrary and
/// wrong for six of them. So the header carries ONE link, and the page behind it is the
/// list. (Ledger #442, which asked the question and left it open.)
pub struct BrowseRoots {
    pub web: Arc<Web>,
}

#[async_trait]
impl Endpoint for BrowseRoots {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(
                "a browse page answers Source only".to_string(),
            ));
        }
        web::html_only(inv)?;
        let roots = readable_roots(&self.web, inv);
        let message = if roots.is_empty() && self.web.browse_roots.is_empty() {
            "This server has no browse root configured (`gonk.browse.root`), so there is \
             nothing to browse here."
                .to_string()
        } else if roots.is_empty() {
            format!(
                "This browser holds no grant naming a repository here. A grant names one \
                 root as `{BROWSE_READ_PREFIX}<root>`, or every root as \
                 `{}`. Sign in with a passkey whose grant carries one.",
                ikigai_browse::CAP_WILDCARD
            )
        } else {
            "The repositories this grant may read. Each opens on its tree; everything after \
             that is the browse family's own pages."
                .to_string()
        };
        let mut children = web::nav(&self.web, inv, &web::readable_ledgers(&self.web, inv), None);
        for root in &roots {
            children.push_str(&element(
                "root",
                &[
                    ("name", root),
                    ("href", &format!("{ROOTS_PATH}/{}", tree_iri(root))),
                    ("iri", &tree_iri(root)),
                ],
                "",
            ));
        }
        let doc = envelope(
            "page",
            &[
                ("view", "roots"),
                ("full", "true"),
                ("title", "Browse"),
                ("message", &message),
            ],
            &children,
        );
        Ok(web::html(
            render::render(&doc, true).map_err(web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-browse-roots"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-browse-roots")
            .title("The repositories this grant may browse")
            .summary(
                "The browse family's landing page: one link per configured root the caller's \
                 capability grants (`urn:cap:browse:read:{root}`, or the all-roots wildcard), \
                 into that root's tree. A root the caller cannot read is not listed — the \
                 page offers no door that answers a 403.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(web::as_html_arg())
            .output("text/html")
    }
}

#[async_trait]
impl Endpoint for BrowseShell {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(
                "a browse page answers Source only".to_string(),
            ));
        }
        web::html_only(inv)?;
        let start = web::binding(inv, "start")?;
        if !start.starts_with("urn:") || Iri::parse(&start).is_err() {
            return Err(Error::NotFound(format!(
                "`{start}` is not a browsable urn:* resource. A browse page names one \
                 (for example /browse/urn:repo:ikigai-core:tree)."
            )));
        }
        let ledgers = web::readable_ledgers(&self.web, inv);
        let mut attributes = vec![
            ("view", "browse".to_string()),
            ("full", "true".to_string()),
            ("title", start.clone()),
        ];
        // ★ Honesty of presentation, never the boundary. A caller without the browse floor
        // gets the reason and the sign-in control rather than a shell that paints itself
        // with two 403s — and `urn:repo:style` is refused to exactly the same caller, so
        // the stylesheet link is withheld with the rest.
        if can_browse(inv) {
            attributes.push(("start-url", k_url(&format!("source {start} as=text/html"))));
            attributes.push(("stylesheet", k_url("source urn:repo:style")));
            // ★ The SECOND sheet, and the one that makes the faces legible: `urn:repo:style`
            // is the syntax theme for the `hl-` classes inside a file view, and
            // `urn:repo:style:layout` is the page furniture — crumbs, entry lists, the action
            // strip, the disclosure menus, annotation cards, the PR listings. Until
            // `ikigai-browse` 0.4.2 the only copy of those rules in the world was a const
            // string inside `ikigai-web`'s binary, so this door rendered every tree entry as
            // a gonk chip (ledger #441). Both links are withheld together, under the same
            // gate, because both resources are refused to exactly the same caller.
            attributes.push((
                "layout-stylesheet",
                k_url(&format!("source {}", ikigai_browse::LAYOUT_IRI)),
            ));
            // The layout sheet hides `.browse-annotate` under this attribute. Honesty of
            // presentation, never the boundary: the annotation Sink requires
            // `urn:cap:annotate` whatever this says, so a door that set nothing would show a
            // form whose submission is refused — safe, and rude.
            if !can_annotate(inv) {
                attributes.push(("posture", "read-only".to_string()));
            }
            attributes.push(("message", format!("loading {start}…")));
        } else {
            attributes.push((
                "message",
                format!(
                    "Browsing needs a grant naming `{BROWSE_READ_PREFIX}*`, which this \
                     server gives no anonymous caller. Sign in with a passkey whose grant \
                     carries it."
                ),
            ));
        }
        let attributes: Vec<(&str, &str)> =
            attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let doc = envelope(
            "page",
            &attributes,
            &format!(
                "{}{}",
                web::nav(&self.web, inv, &ledgers, None),
                element("flash", &[], "")
            ),
        );
        Ok(web::html(
            render::render(&doc, true).map_err(web::render_err)?,
        ))
    }

    fn name(&self) -> &str {
        "gonk-browse-page"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-browse-page")
            .title("Browse a resource")
            .summary(
                "The page `ikigai-browse`'s HTML faces render inside: gonk's own header and \
                 sign-in control, and one region that loads the named resource's html face \
                 through the /k/ adapter. Everything after the first paint is the faces' own \
                 affordances.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("start")
                    .binding()
                    .summary("the urn:* resource this page opens on")
                    .class("http://www.w3.org/2001/XMLSchema#anyURI"),
            )
            .input(web::as_html_arg())
            .output("text/html")
    }
}
