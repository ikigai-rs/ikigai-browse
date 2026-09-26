//! The fourteen resources this crate binds, each of them **per ledger**.
//!
//! ```text
//! urn:iki:ledger:{ledger}:items          Source              the list, filtered
//! urn:iki:ledger:{ledger}:item:{id}      Source Sink Delete  one item, edit it, delete it
//! urn:iki:ledger:{ledger}:append         Sink                file a new item
//! urn:iki:ledger:{ledger}:comment        Sink                append a comment
//! urn:iki:ledger:{ledger}:close          Sink                close with a reason
//! urn:iki:ledger:{ledger}:reopen         Sink                undo a close
//! urn:iki:ledger:{ledger}:claim          Sink Delete         take it / hand it back
//! urn:iki:ledger:{ledger}:defer          Sink Delete         not now / now again
//! urn:iki:ledger:{ledger}:link           Sink Delete         blocks / parent / related
//! urn:iki:ledger:{ledger}:label          Sink Delete         tag / untag
//! urn:iki:ledger:{ledger}:purge          Delete              destroy, leaving a tombstone
//! urn:iki:ledger:{ledger}:next           Source              the ready set, ranked
//! urn:iki:ledger:ledgers                 Source              which ledgers exist
//! urn:iki:ledger:policy:{name}           Source              what a policy weighs
//! ```
//!
//! Omit the `{ledger}` segment and you address the ledger called `default`, through the
//! same grammar match and therefore the same capability — see [`crate::ledger`].
//!
//! The last two have no `{ledger}` segment because neither is a ledger's own state:
//! `ledgers` is the inventory across them and a policy is a property of the host's
//! configuration.
//!
//! # Why `purge` is a resource and not an argument
//!
//! A destructive delete needs a capability an ordinary delete must not carry. A
//! `purge=true` argument could only be enforced at runtime — and an action that enforces
//! a scope it does not declare makes the manifold lie, which the field guide calls worse
//! than the converse. Declaring both on one action would demand purge authority for every
//! ordinary delete. So the authority difference gets its own IRI, exactly as
//! `ikigai-store` gives each SPARQL form its own.
//!
//! The same reasoning is why the ledger is a name and not a `ledger=` argument: an
//! argument is a value, and a value cannot be what a capability binds to.
//!
//! # Capabilities: a wildcard is declared, an exact grant is enforced
//!
//! The ledger is in the IRI, and the kernel's capability pre-check runs before `invoke`
//! can read it. So each action declares the **family** — `urn:cap:ledger:write:*`,
//! meaning "holds some ledger write grant" — and the exact scope for the ledger actually
//! named is checked here, in `ledger_for`. Declared and enforced are the same scope;
//! the wildcard is the only form the pre-check can express. `ikigai-store`'s per-graph
//! write door is shaped identically, and for the same reason.
//!
//! ⚠ **The name goes LAST in the token** — `urn:cap:ledger:read:acme`, not
//! `urn:cap:ledger:acme:read` — because `ikigai-core` matches a wildcard only as a
//! trailing `*`. There is no infix form, so a parameter that is not last cannot be
//! declared as a family at all.
//!
//! # The store scopes every action also declares, and the boundary they now make
//!
//! A sub-request carries the **caller's** capability unchanged — `Invocation::issue` has
//! no attenuating or elevating form — so whatever this module asks the store for, the
//! caller must hold. It asks only the **narrow** doors (`urn:iki:store:graph-{select,ask}`
//! and `urn:iki:store:graph-update`, each naming one graph), so what a caller must hold is
//! `urn:cap:store:{read,write}:graph:urn:iki:ledger:graph:{ledger}` — plus the graveyard's
//! write token for delete and purge. Every action declares those families too, because an
//! action that enforces a scope it does not declare makes the manifold over-offer.
//!
//! ★ **So the ledger capabilities are a tenancy boundary now**, where before 0.2.0 they
//! segmented this module's own doors and the store underneath was one open room. The
//! exception is one resource and it is documented on it: `urn:iki:ledger:ledgers` asks
//! *which graphs exist*, which no scoped read can answer, and resolves the broad
//! `urn:iki:store:select` **only under a root capability**.
//!
//! ⚠ A host that hands a ledger caller `urn:cap:store:read` anyway has still given it every
//! graph in the store. Nothing here asks for it, so that is a configuration decision rather
//! than a requirement — the distinction, and both halves as tests, are in `README.md`.
//!
//! # Freshness
//!
//! Reads are `.cacheable()` and depend on the store's **three** write threads —
//! `urn:iki:store:{update,load,graph-update}` — the third because that is the door this
//! module's own writes go through. This module cuts **no thread of its own**, deliberately:
//! the kernel cuts the thread named after a mutating request's target, so
//! `urn:iki:ledger:append` is already cut on every append, and the store cuts its own on the
//! write this module actually performs. A ledger-specific thread could only be cut by
//! resolving `urn:kernel:cut`, which needs `urn:cap:kernel:cut` — authority this module has
//! no business holding for a name nothing would gain by.
//!
//! ⚠ The `depends_on` calls in `endpoints::face` are belt-and-braces, not the mechanism: a read here
//! is derived from a sub-request to the store, whose own representation declares all three
//! threads, and the kernel unions a dependency's threads into the derived one. Measured
//! 2026-09-13 — removing the declarations leaves every freshness test green. They stay
//! because they are correct and because the day a read stops going through the store is the
//! day they become load-bearing, but do not read that list as what keeps the cache honest.

use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, ReprType,
    Representation, Result, UriTemplate, Verb,
};
use oxrdf::Graph;
use sha2::{Digest, Sha256};

use crate::ledger::{Ledger, LedgerGrammar};
use crate::model::{self, Deferred, Filter, Holder, Item, Status};
use crate::policy::{OrderingPolicy, Policies};
use crate::select;
use crate::sparql::{self, boolean, datetime, integer, literal, StoreClient};
use crate::vocabulary as v;

/// Reading a ledger, as **declared**: the family, meaning "holds some ledger read grant".
/// A held grant names one ledger — [`Ledger::cap_read`].
///
/// A ledger holds whatever anyone filed, so an unrestricted read is not free — the same
/// argument `ikigai-store` makes for gating its query face, now per partition.
pub const CAP_READ: &str = "urn:cap:ledger:read:*";

/// Every ordinary mutation — filing, commenting, closing, claiming, linking, labelling,
/// deferring, editing — as declared. A held grant names one ledger:
/// [`Ledger::cap_write`].
pub const CAP_WRITE: &str = "urn:cap:ledger:write:*";

/// Removing an item from view. Separate from [`CAP_WRITE`] because a delete takes work
/// OUT of the ledger, and the everyday grant should not carry it.
pub const CAP_DELETE: &str = "urn:cap:ledger:delete:*";

/// Destroying an item's content irreversibly. Separate again, and the narrowest grant in
/// this module: a tombstone survives a purge, but nothing else does.
pub const CAP_PURGE: &str = "urn:cap:ledger:purge:*";

/// What an action needs from the ledger it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    Read,
    Write,
    Delete,
    Purge,
}

impl Need {
    /// The exact grant for one ledger.
    fn scope(self, ledger: &Ledger) -> String {
        match self {
            Need::Read => ledger.cap_read(),
            Need::Write => ledger.cap_write(),
            Need::Delete => ledger.cap_delete(),
            Need::Purge => ledger.cap_purge(),
        }
    }

    /// The family this crate declares for it.
    fn declared(self) -> &'static str {
        match self {
            Need::Read => CAP_READ,
            Need::Write => CAP_WRITE,
            Need::Delete => CAP_DELETE,
            Need::Purge => CAP_PURGE,
        }
    }
}

/// The ledger a request named, once the caller's grant for **that ledger** is checked.
///
/// ★ This is the parameterized half of the capability, and it is the reason named
/// ledgers are a boundary rather than a filter. The kernel's pre-check saw only
/// [`Need::declared`] — that the caller holds *some* grant under the family — because the
/// ledger is in the IRI and the check runs before `invoke` can read it. Declared and
/// enforced name the same scope; only the exactness differs, and the exact half is here.
fn ledger_for(inv: &Invocation<'_>, need: Need) -> Result<Ledger> {
    let ledger = Ledger::from_bindings(inv.bindings)?;
    let scope = need.scope(&ledger);
    if !inv.capability.allows(&scope) {
        return Err(Error::Denied(format!(
            "this capability does not hold `{scope}`. A ledger grant names exactly one \
             ledger — holding a grant over another ledger satisfies the declared family \
             `{}` but not this resource, which is the point of naming them",
            need.declared()
        )));
    }
    Ok(ledger)
}

/// `text/plain` — the default face, and what "view ASAP" actually needs.
const PLAIN: &str = "text/plain";
/// `text/turtle` — the graph face.
const TURTLE: &str = "text/turtle";
const FACES: [&str; 2] = [PLAIN, TURTLE];

/// Bind the ledger with the built-in ordering policies (`priority-recency`, then
/// `leverage`).
pub fn space() -> EndpointSpace {
    space_with_policies(Policies::default().all().to_vec())
}

/// Bind the ledger with a host's own ordering policies. **The first is the default.**
///
/// This is the `CachePolicy` shape: configured at execution time, in the host's manifest,
/// rather than compiled in — because the right ordering for a solo backlog, a review
/// queue and a team are different.
///
/// # Panics
///
/// On an empty policy list: a `next` with nothing to rank with should fail where the
/// manifest is, not at the first request.
pub fn space_with_policies(policies: Vec<Arc<dyn OrderingPolicy>>) -> EndpointSpace {
    let policies = Arc::new(Policies::new(policies));
    // ⚠ **Bind order is resolution order** (`EndpointSpace` takes the first grammar that
    // matches), and the two ledger-less resources go FIRST. `urn:iki:ledger:policy:next`
    // would otherwise be read as the `next` resource of a ledger called `policy` — which
    // is also why `policy` and `ledgers` are in `ledger::RESERVED`. Putting them ahead
    // makes the tie deterministic instead of alphabetical.
    EndpointSpace::new()
        .bind(Exact::new("urn:iki:ledger:ledgers"), LedgersEndpoint)
        .bind(
            UriTemplate::parse("urn:iki:ledger:policy:{name}").expect("a constant template"),
            PolicyEndpoint {
                policies: Arc::clone(&policies),
            },
        )
        .bind(LedgerGrammar::action("items"), ItemsEndpoint)
        .bind(LedgerGrammar::with_id("item"), ItemEndpoint)
        .bind(LedgerGrammar::action("append"), AppendEndpoint)
        .bind(LedgerGrammar::action("comment"), CommentEndpoint)
        .bind(LedgerGrammar::action("close"), CloseEndpoint)
        .bind(LedgerGrammar::action("reopen"), ReopenEndpoint)
        .bind(LedgerGrammar::action("claim"), ClaimEndpoint)
        .bind(LedgerGrammar::action("defer"), DeferEndpoint)
        .bind(LedgerGrammar::action("link"), LinkEndpoint)
        .bind(LedgerGrammar::action("label"), LabelEndpoint)
        .bind(LedgerGrammar::action("purge"), PurgeEndpoint)
        .bind(LedgerGrammar::action("next"), NextEndpoint { policies })
}

// ------------------------------------------------------------------------- helpers

/// A `text/plain` representation.
fn plain(text: impl Into<String>) -> Representation {
    Representation::new(
        ReprType::new(PLAIN).with_param("charset", "utf-8"),
        text.into().into_bytes(),
    )
}

/// A representation in the face the caller asked for, cacheable under the store's write
/// threads.
fn face(item_text: String, graph: Option<Graph>, want: &str) -> Result<Representation> {
    let repr = match want {
        TURTLE => Representation::new(
            ReprType::new(TURTLE).with_param("charset", "utf-8"),
            model::turtle(&graph.unwrap_or_default())?,
        ),
        _ => plain(item_text),
    };
    Ok(repr
        .cacheable()
        .depends_on(ikigai_store::UPDATE_THREAD)
        .depends_on(ikigai_store::LOAD_THREAD)
        // ★ The third writing IRI means a third thread, and this crate's own writes all go
        // through it. Depending only on the first two would leave every cacheable read
        // here serving stale bytes after every ledger write — silently, on the branch that
        // looks like success. `a_write_through_the_narrow_door_invalidates_a_cached_read`
        // in `tests/endpoints.rs` is what keeps it true.
        .depends_on(ikigai_store::GRAPH_UPDATE_THREAD))
}

/// Which face was asked for. An `as` this endpoint cannot serve is **refused**, never
/// substituted — the caller asked a question and a different answer is not a better one.
fn wanted_face(inv: &Invocation<'_>) -> Result<&'static str> {
    match inv.inline_str("as").ok() {
        None => Ok(PLAIN),
        Some(asked) => FACES
            .iter()
            .find(|face| bare(asked) == **face)
            .copied()
            .ok_or_else(|| Error::InvalidArgument {
                name: "as".to_string(),
                detail: format!(
                    "`{asked}` is not a face this resource serves; one of {}",
                    FACES.join(", ")
                ),
            }),
    }
}

/// A media type without its parameters.
fn bare(media_type: &str) -> &str {
    media_type.split(';').next().unwrap_or(media_type).trim()
}

/// Now, in milliseconds, from the kernel's injected clock.
///
/// ⚠ **Refuses when the kernel has no clock.** Every item, comment, claim and tombstone
/// here is stamped, and an entry without a timestamp is not a ledger entry: a store that
/// quietly accepted unstamped writes would be discovered as a hole in the record much
/// later, by someone trying to answer "when". Fail loud on missing configuration.
fn now_ms(inv: &Invocation<'_>) -> Result<u64> {
    inv.now().map(|t| t.as_millis()).ok_or_else(|| {
        Error::Endpoint(
            "this kernel has no clock, so a ledger write cannot be stamped — and an entry \
             without a timestamp is not a ledger entry. Build the kernel with \
             `Kernel::with_clock(Arc::new(SystemClock))` (or a fixed clock in tests)"
                .to_string(),
        )
    })
}

/// Mint an item's opaque, time-ordered id.
///
/// ★ **Identity is the IRI; `#12` is a label on it.** The id is 10 characters of
/// Crockford base32 over the low 48 bits of the millisecond clock — so ids sort in
/// filing order, which makes a listing of raw IRIs readable — followed by 6 characters
/// derived from a SHA-256 of the minting inputs. The digest half is not decoration: one
/// store has one writer, so the clock alone is unique *here*, and the digest is what
/// keeps two ledgers merged later from colliding on a shared millisecond.
fn mint_id(now: u64, title: &str, author: &str) -> String {
    const CROCKFORD: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let mut id = String::with_capacity(16);
    for shift in (0..10).rev() {
        id.push(CROCKFORD[((now >> (shift * 5)) & 31) as usize] as char);
    }
    let mut hasher = Sha256::new();
    hasher.update(now.to_be_bytes());
    hasher.update(title.as_bytes());
    hasher.update([0]);
    hasher.update(author.as_bytes());
    let digest = hasher.finalize();
    let mut bits = u64::from(digest[0]) << 24
        | u64::from(digest[1]) << 16
        | u64::from(digest[2]) << 8
        | u64::from(digest[3]);
    for _ in 0..6 {
        id.push(CROCKFORD[(bits & 31) as usize] as char);
        bits >>= 5;
    }
    id
}

/// `title\n\nbody` — the git-commit convention, because it is the one everybody already
/// types and it needs no second argument for the commonest case.
fn split_content(content: &str) -> Result<(String, String)> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidArgument {
            name: "content".to_string(),
            detail: "an item needs at least a title: the first line is the title and \
                     everything after the first blank line is the body"
                .to_string(),
        });
    }
    let mut lines = trimmed.splitn(2, '\n');
    let title = lines.next().unwrap_or_default().trim().to_string();
    let body = lines.next().unwrap_or_default().trim().to_string();
    Ok((title, body))
}

/// Resolve an item reference — `#12`, `12`, `acme#12`, an opaque id, or a full IRI — to
/// its IRI, and fail with a sentence naming what was looked for when there is no such
/// item.
///
/// ★ **Every reference resolves inside THIS client's ledger, and a reference to another
/// one is refused rather than looked up.** That is where the cross-ledger question is
/// actually settled: a `blocks` edge to an item in another ledger cannot be filed,
/// because the object cannot be named here. The reasoning is in
/// [`LinkEndpoint::describe`]; the refusal is here, once, for every endpoint that takes
/// an item.
async fn require_item(client: &StoreClient<'_, '_>, reference: &str) -> Result<Item> {
    let here = client.ledger();
    let iri = if reference.starts_with("urn:") {
        match Ledger::item_iri(reference) {
            Some((other, _)) if &other != here => {
                return Err(Error::InvalidArgument {
                    name: "item".to_string(),
                    detail: format!(
                        "`{reference}` is an item of the ledger `{}`, and this resource is \
                         `{}`. A ledger is a boundary: an item is read, changed and linked \
                         within its own ledger, and `ledger:about` is how a reference \
                         crosses one. Address it at `{}`",
                        other.name(),
                        here.name(),
                        other.prefix()
                    ),
                })
            }
            Some((_, canonical)) => canonical,
            // Not a ledger item IRI. Left verbatim so the error can name what was asked
            // for rather than a guess at what was meant.
            None => reference.to_string(),
        }
    } else {
        model::resolve_id(client, reference).await?
    };
    if let Some(item) = model::load_item(client, &iri).await? {
        return Ok(item);
    }
    // Present but unreadable is a different fact from absent, and only one of them is
    // fixable by the person reading the error.
    let missing = model::defects_of(client, &iri).await?;
    if missing.is_empty() {
        Err(Error::NotFound(format!("no ledger item at `{iri}`")))
    } else {
        Err(Error::Endpoint(format!(
            "`{iri}` is in the ledger graph but is missing {} — so nothing here can read \
             it. Some write did not pass through a ledger Sink (an editor, a merge, a bulk \
             load); add the missing propert{} or delete the subject",
            missing.join(", "),
            if missing.len() == 1 { "y" } else { "ies" }
        )))
    }
}

/// Several SPARQL operations as ONE update request.
///
/// ★ Not a round-trip optimization (though it is one): a state change that takes three
/// statements can be observed half-applied between them — an item closed but with no
/// reason, a claim held by nobody. A SPARQL 1.1 update request is a SEQUENCE of
/// operations and the store applies the request as one, so the intermediate states are
/// never visible and a parse error in the third operation means the first two did not
/// happen either.
fn batch(operations: &[String]) -> String {
    operations
        .iter()
        .filter(|op| !op.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ;\n")
}

/// Remove every value of a property.
fn retract(client: &StoreClient<'_, '_>, item: &str, predicate: &str) -> String {
    let pattern = client.in_graph(&format!("<{item}> <{predicate}> ?was"));
    format!("DELETE {{ {pattern} }} WHERE {{ {pattern} }}")
}

/// Stamp an item as modified, as part of the same update that changed it.
fn touch(client: &StoreClient<'_, '_>, item: &str, now: u64) -> String {
    format!(
        "DELETE {{ {} }} INSERT {{ {} }} WHERE {{ {} }}",
        client.in_graph(&format!("<{item}> <{}> ?was", v::ext::MODIFIED)),
        client.in_graph(&format!(
            "<{item}> <{}> {}",
            v::ext::MODIFIED,
            datetime(now)
        )),
        client.in_graph(&format!(
            "OPTIONAL {{ <{item}> <{}> ?was }}",
            v::ext::MODIFIED
        ))
    )
}

/// Append a comment — the shared path behind `urn:iki:ledger:comment` and behind the
/// optional note on close / reopen / defer / claim, so *why* is always recorded in one
/// place and one shape.
async fn write_comment(
    client: &StoreClient<'_, '_>,
    item: &str,
    text: &str,
    author: Option<&str>,
    now: u64,
) -> Result<String> {
    let id = mint_id(now, text, author.unwrap_or_default());
    let comment = client.ledger().comment(&id);
    let mut triples = format!(
        "<{comment}> <{type_}> <{class}> ; <{on}> <{item}> ; <{body}> {text} ; <{created}> {when} .",
        type_ = v::ext::TYPE,
        class = v::COMMENT_CLASS,
        on = v::ON_ITEM,
        body = v::BODY,
        text = literal(text),
        created = v::ext::CREATED,
        when = datetime(now),
    );
    if let Some(author) = author {
        triples.push_str(&format!(
            "\n<{comment}> <{}> {} .",
            v::AUTHOR,
            literal(author)
        ));
    }
    client
        .update(&batch(&[
            format!("INSERT DATA {{ {} }}", client.in_graph(&triples)),
            touch(client, item, now),
        ]))
        .await?;
    Ok(comment)
}

/// The store scopes an action that reads the ledger transitively needs.
///
/// ★ **The per-graph family, never the broad `urn:cap:store:read`.** A read here is one
/// `urn:iki:store:graph-select` naming this ledger's graph, so the grant a caller must
/// actually hold is `urn:cap:store:read:graph:urn:iki:ledger:graph:{name}` — which grants
/// nothing over any other ledger. The wildcard is the *declared* half (the kernel's
/// pre-check runs before an endpoint can read the ledger out of its own IRI); the exact
/// half is enforced by the store, against the graph named in the sub-request.
fn read_scopes(desc: Description) -> Description {
    desc.requires(CAP_READ)
        .requires(ikigai_store::CAP_READ_GRAPH)
}

/// The store scopes a mutating action transitively needs. Every write here reads first
/// (to resolve `#12`, to check the item exists), so both store scopes are real.
///
/// ⚠ **Delete and purge need a write grant for a SECOND graph** — the ledger's graveyard
/// — because archiving is a write into it. One family covers both in the declaration; the
/// operator's grant list does not, and the README's grant table says so per verb.
fn write_scopes(spec: ikigai_core::ActionSpec, own: &str) -> ikigai_core::ActionSpec {
    spec.requires(own)
        .requires(ikigai_store::CAP_READ_GRAPH)
        .requires(ikigai_store::CAP_WRITE_GRAPH)
}

/// The `{ledger}` binding every per-ledger resource declares.
///
/// ★ Declared as a binding with a DEFAULT, which is exactly what the bare-form sugar is:
/// the segment may be absent, and absent means `default`. A caller reading only the
/// contract — the engine, the MCP projection, an agent — can form either spelling from
/// it, and a catalog probe expands the template with the default rather than a
/// placeholder.
fn ledger_arg() -> ArgSpec {
    ArgSpec::new("ledger")
        .summary(
            "Which ledger. Omit the segment entirely (`urn:iki:ledger:append`) for the \
             ledger called `default` — the same resource under a shorter name, gated by \
             the same capability. Lowercase letters, digits, `-` and `_`.",
        )
        .class(v::ext::XSD_STRING)
        .default_value(crate::ledger::DEFAULT)
        .binding()
        .optional()
}

/// The `as` ArgSpec every read face declares.
fn as_arg() -> ArgSpec {
    ArgSpec::new("as")
        .summary(format!(
            "The face to serve; one of {}. An `as` this resource cannot answer in is \
             refused, never substituted.",
            FACES.join(", ")
        ))
        .class(v::ext::XSD_STRING)
        .one_of(FACES)
        .default_value(PLAIN)
        .optional()
}

/// The `item` ArgSpec: a short number, an opaque id, or a full IRI.
fn item_arg(summary: &str) -> ArgSpec {
    ArgSpec::new("item")
        .summary(format!(
            "{summary} Accepts `#12`, `12`, an opaque id, or the full \
             `urn:iki:ledger:item:{{id}}` IRI — a human types the number and a machine \
             carries the IRI."
        ))
        .class(v::ext::XSD_STRING)
}

fn unsupported(id: &str, verb: Verb) -> Error {
    Error::Endpoint(format!("`{id}` does not answer {verb:?}"))
}

// --------------------------------------------------------------------------- items

#[derive(Clone)]
struct ItemsEndpoint;

#[async_trait]
impl Endpoint for ItemsEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(unsupported("ledger-items", inv.request.verb));
        }
        let client = StoreClient::new(inv, ledger_for(inv, Need::Read)?);
        let want = wanted_face(inv)?;
        let filter = Filter {
            status: match inv.inline_str("status").unwrap_or("open") {
                "closed" => Status::Closed,
                "all" => Status::All,
                "open" => Status::Open,
                other => {
                    return Err(Error::InvalidArgument {
                        name: "status".to_string(),
                        detail: format!("`{other}` is not one of open, closed, all"),
                    })
                }
            },
            kind: inv.inline_str("kind").ok().map(str::to_string),
            labels: split_labels(inv.inline_str("labels").ok()),
            without: split_labels(inv.inline_str("without").ok()),
            about: inv.inline_str("about").ok().map(str::to_string),
            holder: match inv.inline_str("holder").ok() {
                None | Some("any") => Holder::Any,
                Some("none") => Holder::None,
                Some(name) => Holder::Named(name.to_string()),
            },
            deferred: match inv.inline_str("deferred").unwrap_or("include") {
                "exclude" => Deferred::Exclude,
                "only" => Deferred::Only,
                _ => Deferred::Include,
            },
            text: inv.inline_str("text").ok().map(str::to_string),
            limit: inv
                .inline_str("limit")
                .ok()
                .and_then(|l| l.parse().ok())
                .unwrap_or(50),
        };
        let items = model::load_items(&client, &filter).await?;
        if want == TURTLE {
            let mut graph = Graph::new();
            for item in &items {
                item.triples(&mut graph);
            }
            return face(String::new(), Some(graph), want);
        }
        let mut text = if items.is_empty() {
            "no items match\n".to_string()
        } else {
            let mut out: String = items
                .iter()
                .map(|item| format!("{}\n", item.line()))
                .collect();
            out.push_str(&format!("\n{} item(s)\n", items.len()));
            out
        };
        // ★ The corpus check, on the read side. Anyone holding the store's write scope can
        // change this graph without passing through a ledger Sink — an editor, a merge, a
        // bulk load — and that is a supported path, not corruption. What is NOT supported
        // is an item quietly disappearing from every listing because a hand edit left it
        // unreadable, so a malformed item is REPORTED here rather than skipped.
        let broken = model::defects(&client).await?;
        if !broken.is_empty() {
            text.push_str(&format!(
                "\n⚠ {} item(s) in the graph could not be read and are not listed above \
                 (a write that did not pass through a ledger Sink):\n",
                broken.len()
            ));
            for (iri, missing) in &broken {
                text.push_str(&format!("  {iri} — missing {}\n", missing.join(", ")));
            }
        }
        face(text, None, want)
    }

    fn name(&self) -> &str {
        "ledger-items"
    }

    fn describe(&self) -> Description {
        read_scopes(
            Description::new("ledger-items")
                .title("The ledger, filtered")
                .summary(
                    "Every item matching the filter, most recently updated first. The \
                     filters are evaluated in the query, which is why the ordering policy \
                     behind `urn:iki:ledger:next` can stay small.",
                )
                .verb(Verb::Source)
                .verb(Verb::Meta)
                .input(ledger_arg())
                .input(
                    ArgSpec::new("status")
                        .summary("Which items: open (the default), closed, or all.")
                        .class(v::ext::XSD_STRING)
                        .one_of(["open", "closed", "all"])
                        .default_value("open")
                        .optional(),
                )
                .input(
                    ArgSpec::new("kind")
                        .summary(
                            "An item class IRI — the LEVEL. Items of every level assert \
                             the base class, so omitting this asks the whole ledger and \
                             naming one scopes to that level.",
                        )
                        .class(v::ext::XSD_ANY_URI)
                        .optional(),
                )
                .input(
                    ArgSpec::new("labels")
                        .summary("Comma-separated; an item must carry ALL of them.")
                        .class(v::ext::XSD_STRING)
                        .optional(),
                )
                .input(
                    ArgSpec::new("without")
                        .summary("Comma-separated; an item must carry NONE of them.")
                        .class(v::ext::XSD_STRING)
                        .optional(),
                )
                .input(
                    ArgSpec::new("about")
                        .summary(
                            "A resource IRI: what is filed against this file, commit, PR \
                             or brief. The join the ledger exists to make cheap.",
                        )
                        .class(v::ext::XSD_ANY_URI)
                        .optional(),
                )
                .input(
                    ArgSpec::new("holder")
                        .summary("`any` (default), `none` for unclaimed, or a holder's name.")
                        .class(v::ext::XSD_STRING)
                        .default_value("any")
                        .optional(),
                )
                .input(
                    ArgSpec::new("deferred")
                        .summary("include (default), exclude, or only.")
                        .class(v::ext::XSD_STRING)
                        .one_of(["include", "exclude", "only"])
                        .default_value("include")
                        .optional(),
                )
                .input(
                    ArgSpec::new("text")
                        .summary("A case-insensitive substring of the title.")
                        .class(v::ext::XSD_STRING)
                        .optional(),
                )
                .input(
                    ArgSpec::new("limit")
                        .summary("At most this many items (default 50).")
                        .class(v::ext::XSD_INTEGER)
                        .default_value("50")
                        .optional(),
                )
                .input(as_arg())
                .output(PLAIN)
                .output(TURTLE),
        )
    }
}

fn split_labels(value: Option<&str>) -> Vec<String> {
    value
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------- item

#[derive(Clone)]
struct ItemEndpoint;

#[async_trait]
impl Endpoint for ItemEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let need = match inv.request.verb {
            Verb::Source | Verb::Exists => Need::Read,
            Verb::Sink => Need::Write,
            // ⚠ A Delete needs the delete grant, and an unsupported verb must not be
            // able to probe a ledger's existence under a read grant — so it is refused
            // here, before any capability is checked at all.
            Verb::Delete => Need::Delete,
            other => return Err(unsupported("ledger-item", other)),
        };
        let client = StoreClient::new(inv, ledger_for(inv, need)?);
        let id = inv.bindings.get("id").ok_or_else(|| {
            Error::Endpoint(
                "no `id` captured: this endpoint is bound to \
                 `urn:iki:ledger:{ledger}:item:{id}` and was invoked without the grammar's \
                 capture"
                    .to_string(),
            )
        })?;
        match inv.request.verb {
            Verb::Source => {
                let want = wanted_face(inv)?;
                let item = require_item(&client, id).await?;
                let comments = model::load_comments(&client, &item.iri).await?;
                if want == TURTLE {
                    let mut graph = Graph::new();
                    item.triples(&mut graph);
                    for comment in &comments {
                        comment.triples(&mut graph);
                    }
                    return face(String::new(), Some(graph), want);
                }
                let mut text = item.detail();
                if !comments.is_empty() {
                    text.push_str(&format!("\n{} comment(s):\n", comments.len()));
                    for comment in &comments {
                        text.push_str(&comment.render());
                    }
                }
                face(text, None, want)
            }
            Verb::Exists => {
                let item = require_item(&client, id).await;
                face(
                    if item.is_ok() { "true\n" } else { "false\n" }.to_string(),
                    None,
                    PLAIN,
                )
            }
            Verb::Sink => {
                let now = now_ms(inv)?;
                let item = require_item(&client, id).await?;
                let mut updates = Vec::new();
                if let Ok(content) = inv.inline_str("content") {
                    let (title, body) = split_content(content)?;
                    updates.push(replace_one(
                        &client,
                        &item.iri,
                        v::ext::TITLE,
                        &literal(&title),
                    ));
                    updates.push(replace_one(&client, &item.iri, v::BODY, &literal(&body)));
                }
                if let Ok(priority) = inv.inline_str("priority") {
                    updates.push(replace_one(
                        &client,
                        &item.iri,
                        v::PRIORITY,
                        &integer(parse_priority(priority)?),
                    ));
                }
                if let Ok(revision) = inv.inline_str("revision") {
                    updates.push(replace_one(
                        &client,
                        &item.iri,
                        v::REVISION,
                        &literal(revision),
                    ));
                }
                if let Ok(about) = inv.inline_str("about") {
                    for target in about.split_whitespace() {
                        updates.push(format!(
                            "INSERT DATA {{ {} }}",
                            client.in_graph(&format!(
                                "<{}> <{}> {} .",
                                item.iri,
                                v::ABOUT,
                                sparql::iri(target, "about")?
                            ))
                        ));
                    }
                }
                if updates.is_empty() {
                    return Err(Error::MissingArgument("content".to_string()));
                }
                updates.push(touch(&client, &item.iri, now));
                client.update(&batch(&updates)).await?;
                Ok(plain(format!("updated {} {}\n", item.short(), item.iri)))
            }
            Verb::Delete => {
                let now = now_ms(inv)?;
                let item = require_item(&client, id).await?;
                let reason = inv.inline_str("reason").unwrap_or("").to_string();
                let author = inv.inline_str("author").ok().map(str::to_string);
                let removed =
                    delete_item(&client, &item, &reason, author.as_deref(), now, false).await?;
                Ok(plain(format!(
                    "deleted {} {}\n  {} quad(s) moved to <{}> and recoverable\n  tombstone: \
                     {}\n",
                    item.short(),
                    item.iri,
                    removed.quads,
                    client.ledger().deleted_graph(),
                    removed.tombstone
                )))
            }
            other => Err(unsupported("ledger-item", other)),
        }
    }

    fn name(&self) -> &str {
        "ledger-item"
    }

    fn describe(&self) -> Description {
        let id_input = || {
            ArgSpec::new("id")
                .summary(
                    "The item's opaque id or its short number — both resolve, because a \
                     human types `12` and a machine carries the IRI.",
                )
                .class(v::ext::XSD_STRING)
                .binding()
        };
        Description::new("ledger-item")
            .title("One ledger item")
            .summary(
                "Read an item with its comments and links, edit its fields, or delete it \
                 (leaving a tombstone and its content recoverable).",
            )
            .verb(Verb::Meta)
            .action(read_action(
                ikigai_core::ActionSpec::new(Verb::Source)
                    .input(ledger_arg())
                    .summary("The item, its metadata, its links and its comments.")
                    .input(id_input())
                    .input(as_arg())
                    .output(PLAIN)
                    .output(TURTLE),
            ))
            .action(read_action(
                ikigai_core::ActionSpec::new(Verb::Exists)
                    .input(ledger_arg())
                    .summary("Whether the item exists.")
                    .input(id_input())
                    .output(PLAIN),
            ))
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Sink)
                    .input(ledger_arg())
                    .summary(
                        "Edit the item: `content` replaces the title and body (first line, \
                         then the rest), `priority` replaces the priority, `about` ADDS a \
                         target, `revision` replaces the filed-against revision.",
                    )
                    .input(id_input())
                    .input(
                        ArgSpec::new("content")
                            .summary("The new title and body: first line, blank line, rest.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .input(
                        ArgSpec::new("priority")
                            .summary("0 highest … 4 lowest.")
                            .class(v::ext::XSD_INTEGER)
                            .one_of(["0", "1", "2", "3", "4"])
                            .optional(),
                    )
                    .input(
                        ArgSpec::new("about")
                            .summary(
                                "Whitespace-separated resource IRIs to ADD. There is no \
                                 remove yet; see the README's gaps.",
                            )
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .input(
                        ArgSpec::new("revision")
                            .summary("The revision this item is filed against.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .output(PLAIN),
                CAP_WRITE,
            ))
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Delete)
                    .input(ledger_arg())
                    .summary(
                        "Delete the item: its quads and its comments MOVE to \
                         `urn:iki:ledger:graph:{ledger}:deleted`, and a tombstone stays behind \
                         carrying the number, the time, the actor, the reason, the quad \
                         count and the sha256 of what was removed. Recoverable. To destroy \
                         the content, `urn:iki:ledger:purge` — a different resource because \
                         it is a different authority.",
                    )
                    .input(id_input())
                    .input(
                        ArgSpec::new("reason")
                            .summary("Why — free text, kept on the tombstone.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .input(
                        ArgSpec::new("author")
                            .summary("Who deleted it.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .output(PLAIN),
                CAP_DELETE,
            ))
    }
}

/// The ledger + store read scopes on an explicit action.
fn read_action(spec: ikigai_core::ActionSpec) -> ikigai_core::ActionSpec {
    spec.requires(CAP_READ).requires(ikigai_store::CAP_READ)
}

/// Replace a single-valued property.
fn replace_one(client: &StoreClient<'_, '_>, item: &str, predicate: &str, value: &str) -> String {
    format!(
        "DELETE {{ {} }} INSERT {{ {} }} WHERE {{ {} }}",
        client.in_graph(&format!("<{item}> <{predicate}> ?was")),
        client.in_graph(&format!("<{item}> <{predicate}> {value}")),
        client.in_graph(&format!("OPTIONAL {{ <{item}> <{predicate}> ?was }}"))
    )
}

fn parse_priority(value: &str) -> Result<i64> {
    match value.trim().parse::<i64>() {
        Ok(p) if (0..=4).contains(&p) => Ok(p),
        _ => Err(Error::InvalidArgument {
            name: "priority".to_string(),
            detail: format!("`{value}` is not 0 (highest) … 4 (lowest)"),
        }),
    }
}

/// What a delete removed.
struct Removed {
    /// The tombstone's IRI, in the ledger the item was in.
    tombstone: String,
    quads: usize,
}

/// Move (or destroy) an item's quads and write its tombstone.
///
/// ★ The three sets that go together, because leaving any behind is a lie about what was
/// deleted: the item's own triples, the triples of its comments, and every edge POINTING
/// AT it (a `blocks` from another item would otherwise name something that no longer
/// resolves).
///
/// # ★ Why this is TWO updates, and what a reader sees between them
///
/// A delete moves quads between the ledger's graph and its graveyard, and **a scoped
/// update can neither read nor write across graphs** — that is the whole point of the
/// narrow door (`ikigai_store::confine`). So the single
/// `DELETE { GRAPH live } INSERT { GRAPH deleted } WHERE { GRAPH live }` this used to be
/// cannot exist, and the move becomes:
///
/// 1. a scoped **read** of the live graph — the quads, which this already did for the
///    tombstone's hash;
/// 2. a scoped **write to the graveyard**, inserting them as data;
/// 3. a scoped **write to the live graph**, removing them and writing the tombstone —
///    which is still one update, so *that* pair is atomic.
///
/// ⚠ **The graveyard is touched first and the live graph last, deliberately: the live
/// graph is the commit point.** Every read in this crate looks at the live graph and none
/// looks at the graveyard, so a crash between the two writes leaves the item *entirely
/// present and undeleted* — with a copy already archived, which no reader can see. A
/// reader therefore never observes a half-deleted item: it sees the item, whole, until the
/// moment it does not. The state is re-runnable rather than merely recoverable, because
/// step 2 is `INSERT DATA` of quads the graph may already hold, and a store is a set.
/// The other order — remove first, archive second — would put the window on the side where
/// a crash destroys data, which is not a trade worth making for one fewer sentence here.
///
/// The same argument runs backwards for `purge`, which must clear both graphs: it empties
/// the **graveyard** first and the live graph second, so an interrupted purge leaves the
/// item fully live and re-purgeable, rather than leaving an orphan in a graveyard that no
/// resource can name once the live item it belonged to is gone.
///
/// ⚠ The one wrinkle the re-runnability does not cover: if the item is *edited* between an
/// interrupted delete and its retry, the graveyard ends up holding the union of both
/// versions. The tombstone's hash then describes the retry's quads, which is what was
/// removed and is the honest answer; the archive is a superset. There is no in-band signal
/// for it, and inventing one would mean a marker quad written before the move that a
/// reader of the live graph would have to learn to ignore.
async fn delete_item(
    client: &StoreClient<'_, '_>,
    item: &Item,
    reason: &str,
    author: Option<&str>,
    now: u64,
    destroy: bool,
) -> Result<Removed> {
    let subject = sparql::iri(&item.iri, "item")?;
    let selector = format!(
        "?s ?p ?o . FILTER(?s = {subject} || ?o = {subject} || EXISTS {{ ?s <{on}> {subject} }})",
        on = v::ON_ITEM,
    );
    let rows = client
        .select(&format!(
            "SELECT ?s ?p ?o WHERE {{ {} }} ORDER BY ?s ?p ?o",
            client.in_graph(&selector)
        ))
        .await?;

    // ★ The hash is over a canonical form of exactly what is going: sorted triples, each
    // term with its kind and datatype. There are no blank nodes in this graph — every node
    // this module writes is skolemized — so sorted triples ARE a canonical form here, and
    // the heavyweight RDF Dataset Canonicalization would buy nothing.
    let mut hasher = Sha256::new();
    for row in &rows {
        for var in ["s", "p", "o"] {
            if let Some(binding) = row.get(var) {
                hasher.update(binding.kind.as_bytes());
                hasher.update([0x1f]);
                hasher.update(binding.value.as_bytes());
                hasher.update([0x1f]);
                hasher.update(binding.datatype.clone().unwrap_or_default().as_bytes());
                hasher.update([0x1f]);
                // The language tag is in the canonical form because it is in the term:
                // `"titre"@fr` and `"titre"` are different statements, and a digest that
                // could not tell them apart would certify the wrong one.
                hasher.update(binding.lang.clone().unwrap_or_default().as_bytes());
                hasher.update([0x1e]);
            }
        }
        hasher.update([0x1d]);
    }
    let digest = format!("sha256:{:x}", hasher.finalize());

    // Step 1 of 2 — the graveyard, which no read in this crate can see, so nothing a
    // reader observes has changed yet.
    //
    // ⚠ The DELETE TEMPLATE is a quad pattern and may not carry a FILTER — the selector
    // belongs in the WHERE clause only. Putting it in both is a parse error at the store,
    // which is at least loud.
    if destroy {
        // Everything an earlier recoverable delete quarantined.
        client
            .update_deleted(&format!(
                "DELETE {{ {} }} WHERE {{ {} }}",
                client.in_deleted_graph("?s ?p ?o"),
                client.in_deleted_graph(&selector),
            ))
            .await?;
    } else {
        // The quads read above, as data: a scoped update cannot read the live graph from
        // inside the graveyard's scope, so the WHERE clause that used to do this work is
        // now the SELECT that already ran, and the terms go back out through the store's
        // own serializer rather than through any spelling of our own.
        if !rows.is_empty() {
            let mut triples = String::new();
            for row in &rows {
                let (Some(s), Some(p), Some(o)) = (row.get("s"), row.get("p"), row.get("o")) else {
                    continue;
                };
                triples.push_str(&format!(
                    "{} {} {} .\n",
                    sparql::term(&s.term("s")?),
                    sparql::term(&p.term("p")?),
                    sparql::term(&o.term("o")?)
                ));
            }
            client
                .update_deleted(&format!(
                    "INSERT DATA {{ {} }}",
                    client.in_deleted_graph(&triples)
                ))
                .await?;
        }
    }

    let id = item.iri.rsplit(':').next().unwrap_or("unknown").to_string();
    let tombstone = client.ledger().tombstone(&id);
    let mut triples = format!(
        "<{tombstone}> <{type_}> <{class}> ; <{deleted}> <{item_iri}> ; <{number}> {n} ; \
         <{invalidated}> {when} ; <{recoverable}> {recoverable_value} ; <{quads}> {count} ; \
         <{hash}> {digest} .",
        type_ = v::ext::TYPE,
        class = v::TOMBSTONE_CLASS,
        deleted = v::DELETED_ITEM,
        item_iri = item.iri,
        number = v::NUMBER,
        n = integer(item.number),
        invalidated = v::ext::INVALIDATED_AT,
        when = datetime(now),
        recoverable = v::RECOVERABLE,
        recoverable_value = boolean(!destroy),
        quads = v::QUAD_COUNT,
        count = integer(rows.len() as i64),
        hash = v::ext::CONTENT_HASH,
        digest = literal(&digest),
    );
    if !reason.is_empty() {
        triples.push_str(&format!(
            "\n<{tombstone}> <{}> {} .",
            v::REASON,
            literal(reason)
        ));
    }
    if let Some(author) = author {
        triples.push_str(&format!(
            "\n<{tombstone}> <{}> {} .",
            v::AUTHOR,
            literal(author)
        ));
    }
    // Step 2 of 2 — the live graph, which IS the commit point. Removing the item's quads
    // and writing its tombstone are one scoped update and therefore atomic: there is no
    // instant in which the item is gone and unaccounted for.
    //
    // ★ The order of the two statements is load-bearing. The selector matches `?o =
    // <item>`, and the tombstone's `ledger:deletedItem <item>` is exactly that shape — so
    // an INSERT before the DELETE would write the tombstone and then remove it. SPARQL
    // runs `;`-separated operations in order, which is what makes this safe and also what
    // makes it fragile enough to say out loud.
    client
        .update(&batch(&[
            format!(
                "DELETE {{ {} }} WHERE {{ {} }}",
                client.in_graph("?s ?p ?o"),
                client.in_graph(&selector)
            ),
            format!("INSERT DATA {{ {} }}", client.in_graph(&triples)),
        ]))
        .await?;
    Ok(Removed {
        tombstone,
        quads: rows.len(),
    })
}

// -------------------------------------------------------------------------- append

#[derive(Clone)]
struct AppendEndpoint;

#[async_trait]
impl Endpoint for AppendEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(unsupported("ledger-append", inv.request.verb));
        }
        let client = StoreClient::new(inv, ledger_for(inv, Need::Write)?);
        let now = now_ms(inv)?;
        let (title, body) = split_content(inv.inline_str("content")?)?;
        let author = inv.inline_str("author").ok();
        let id = mint_id(now, &title, author.unwrap_or_default());
        let iri = client.ledger().item(&id);

        let mut triples = format!(
            "<{iri}> <{type_}> <{class}> ; <{number}> ?new ; <{title_p}> {title_v} ; \
             <{status}> <{open}> ; <{created}> {when} ; <{modified}> {when} .",
            type_ = v::ext::TYPE,
            class = v::ITEM_CLASS,
            number = v::NUMBER,
            title_p = v::ext::TITLE,
            title_v = literal(&title),
            status = v::STATUS,
            open = v::OPEN,
            created = v::ext::CREATED,
            modified = v::ext::MODIFIED,
            when = datetime(now),
        );
        if let Ok(kind) = inv.inline_str("kind") {
            triples.push_str(&format!(
                "\n<{iri}> <{}> {} .",
                v::ext::TYPE,
                sparql::iri(kind, "kind")?
            ));
        }
        if !body.is_empty() {
            triples.push_str(&format!("\n<{iri}> <{}> {} .", v::BODY, literal(&body)));
        }
        if let Ok(priority) = inv.inline_str("priority") {
            triples.push_str(&format!(
                "\n<{iri}> <{}> {} .",
                v::PRIORITY,
                integer(parse_priority(priority)?)
            ));
        }
        if let Some(author) = author {
            triples.push_str(&format!("\n<{iri}> <{}> {} .", v::AUTHOR, literal(author)));
        }
        if let Ok(revision) = inv.inline_str("revision") {
            triples.push_str(&format!(
                "\n<{iri}> <{}> {} .",
                v::REVISION,
                literal(revision)
            ));
        }
        for label in split_labels(inv.inline_str("labels").ok()) {
            triples.push_str(&format!("\n<{iri}> <{}> {} .", v::LABEL, literal(&label)));
        }
        for target in inv.inline_str("about").unwrap_or("").split_whitespace() {
            triples.push_str(&format!(
                "\n<{iri}> <{}> {} .",
                v::ABOUT,
                sparql::iri(target, "about")?
            ));
        }

        // ★ ONE statement, so the number cannot be allocated twice. The counter is read,
        // incremented, rewritten and stamped onto the new item inside a single SPARQL
        // UPDATE — a read-modify-write across two round trips would race two concurrent
        // appends in the same process, and the store's one-writer rule says nothing about
        // that (it excludes other PROCESSES, not other requests).
        //
        // ★ The counter is PER LEDGER — a subject in the ledger's own graph — so numbers
        // restart at 1 in each one, which is why a display number carries the ledger's
        // name (`acme#12`) as soon as there is more than the default. Sharing one counter
        // across ledgers would have leaked the other ledgers' activity through the gaps
        // in the sequence, which is a small thing to leak and an unnecessary one.
        let counter = client.ledger().counter();
        let update = format!(
            "DELETE {{ {delete} }}\nINSERT {{ {insert} }}\nWHERE {{\n  \
             {{ {{ SELECT ?last WHERE {{ {last_q} }} ORDER BY DESC(?last) LIMIT 1 }}\n    \
             UNION\n    {{ BIND({zero} AS ?last) FILTER NOT EXISTS {{ {any_q} }} }} }}\n  \
             OPTIONAL {{ {old_q} }}\n  BIND(?last + 1 AS ?new)\n}}",
            delete = client.in_graph(&format!("<{counter}> <{}> ?old", v::LAST_NUMBER)),
            insert = client.in_graph(&format!(
                "<{counter}> <{type_}> <{class}> ; <{last}> ?new .\n{triples}",
                type_ = v::ext::TYPE,
                class = v::COUNTER_CLASS,
                last = v::LAST_NUMBER,
            )),
            last_q = client.in_graph(&format!("<{counter}> <{}> ?last", v::LAST_NUMBER)),
            zero = integer(0),
            any_q = client.in_graph(&format!("<{counter}> <{}> ?any", v::LAST_NUMBER)),
            old_q = client.in_graph(&format!("<{counter}> <{}> ?old", v::LAST_NUMBER)),
        );
        client.update(&update).await?;

        let item = model::load_item(&client, &iri).await?.ok_or_else(|| {
            Error::Endpoint(format!(
                "the item was written but does not read back at {iri}: the store accepted \
                 an update that changed nothing, which should be impossible"
            ))
        })?;
        Ok(plain(format!("{} {}\n", item.short(), item.iri)))
    }

    fn name(&self) -> &str {
        "ledger-append"
    }

    fn describe(&self) -> Description {
        let spec = write_scopes(
            ikigai_core::ActionSpec::new(Verb::Sink)
                .input(ledger_arg())
                .summary(
                    "File a new item. The number is allocated from the ledger's counter in \
                     the same statement that writes the item, so two concurrent appends \
                     cannot collide.",
                )
                .input(
                    ArgSpec::new("content")
                        .summary(
                            "The item: first line is the title, everything after the first \
                             blank line is the body. This is where a pipe's value lands.",
                        )
                        .class(v::ext::XSD_STRING),
                )
                .input(
                    ArgSpec::new("kind")
                        .summary(
                            "An item class IRI for this item's LEVEL — a decision record, a \
                             review finding, a state transition. It is asserted ALONGSIDE \
                             the base class, so a new level needs no change here and every \
                             query over the ledger still sees it.",
                        )
                        .class(v::ext::XSD_ANY_URI)
                        .optional(),
                )
                .input(
                    ArgSpec::new("priority")
                        .summary("0 highest … 4 lowest. Absent means unset, which is not 4.")
                        .class(v::ext::XSD_INTEGER)
                        .one_of(["0", "1", "2", "3", "4"])
                        .optional(),
                )
                .input(
                    ArgSpec::new("labels")
                        .summary("Comma-separated tags.")
                        .class(v::ext::XSD_STRING)
                        .optional(),
                )
                .input(
                    ArgSpec::new("about")
                        .summary(
                            "Whitespace-separated resource IRIs this item is ABOUT — a file, \
                             a commit, a PR, a brief. What makes \"what is open against \
                             this file\" a query rather than an index.",
                        )
                        .class(v::ext::XSD_STRING)
                        .optional(),
                )
                .input(
                    ArgSpec::new("revision")
                        .summary(
                            "The revision this is filed against — a commit sha, a tag. The \
                             pin that keeps a finding about line 40 from rotting.",
                        )
                        .class(v::ext::XSD_STRING)
                        .optional(),
                )
                .input(
                    ArgSpec::new("author")
                        .summary("Who is filing it.")
                        .class(v::ext::XSD_STRING)
                        .optional(),
                )
                .output(PLAIN),
            CAP_WRITE,
        );
        Description::new("ledger-append")
            .title("File a ledger item")
            .summary("Append a new item to the ledger and answer with its number and IRI.")
            .verb(Verb::Meta)
            .action(spec)
    }
}

// ------------------------------------------------------------------------- comment

#[derive(Clone)]
struct CommentEndpoint;

#[async_trait]
impl Endpoint for CommentEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(unsupported("ledger-comment", inv.request.verb));
        }
        let client = StoreClient::new(inv, ledger_for(inv, Need::Write)?);
        let now = now_ms(inv)?;
        let item = require_item(&client, inv.inline_str("item")?).await?;
        let text = inv.inline_str("content")?;
        if text.trim().is_empty() {
            return Err(Error::InvalidArgument {
                name: "content".to_string(),
                detail: "an empty comment says nothing and cannot be edited afterwards".to_string(),
            });
        }
        let comment = write_comment(
            &client,
            &item.iri,
            text.trim(),
            inv.inline_str("author").ok(),
            now,
        )
        .await?;
        Ok(plain(format!(
            "commented on {} ({comment})\n",
            item.short()
        )))
    }

    fn name(&self) -> &str {
        "ledger-comment"
    }

    fn describe(&self) -> Description {
        Description::new("ledger-comment")
            .title("Comment on a ledger item")
            .summary(
                "Append a comment. Comments are append-only — there is no edit and no \
                 delete short of deleting the item, because a record that can be quietly \
                 revised is not a record.",
            )
            .verb(Verb::Meta)
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Sink)
                    .input(ledger_arg())
                    .summary("Append a stamped, attributed comment.")
                    .input(item_arg("The item to comment on."))
                    .input(
                        ArgSpec::new("content")
                            .summary("The comment text — where a pipe's value lands.")
                            .class(v::ext::XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("author")
                            .summary("Who is commenting.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .output(PLAIN),
                CAP_WRITE,
            ))
    }
}

// --------------------------------------------------------------------- close/reopen

#[derive(Clone)]
struct CloseEndpoint;

#[async_trait]
impl Endpoint for CloseEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(unsupported("ledger-close", inv.request.verb));
        }
        let client = StoreClient::new(inv, ledger_for(inv, Need::Write)?);
        let now = now_ms(inv)?;
        let item = require_item(&client, inv.inline_str("item")?).await?;
        let name = inv.inline_str("reason").unwrap_or("done");
        let reason = v::close_reason(name).ok_or_else(|| Error::InvalidArgument {
            name: "reason".to_string(),
            detail: format!(
                "`{name}` is not one of {}",
                v::CLOSE_REASONS
                    .iter()
                    .map(|(short, _)| *short)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })?;
        client
            .update(&batch(&[
                replace_one(&client, &item.iri, v::STATUS, &format!("<{}>", v::CLOSED)),
                replace_one(&client, &item.iri, v::CLOSED_REASON, &format!("<{reason}>")),
                touch(&client, &item.iri, now),
            ]))
            .await?;
        if let Ok(note) = inv.inline_str("content") {
            if !note.trim().is_empty() {
                write_comment(
                    &client,
                    &item.iri,
                    note.trim(),
                    inv.inline_str("author").ok(),
                    now,
                )
                .await?;
            }
        }
        Ok(plain(format!("closed {} ({name})\n", item.short())))
    }

    fn name(&self) -> &str {
        "ledger-close"
    }

    fn describe(&self) -> Description {
        Description::new("ledger-close")
            .title("Close a ledger item")
            .summary(
                "Close with a reason. Closing is NOT deleting: the item stays in the graph, \
                 stays queryable, and stops blocking whatever it blocked.",
            )
            .verb(Verb::Meta)
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Sink)
                    .input(ledger_arg())
                    .summary("Set the status to closed and record why.")
                    .input(item_arg("The item to close."))
                    .input(
                        ArgSpec::new("reason")
                            .summary(
                                "Why it is closed. `audit-no-change` is a real verdict — \
                                 \"looked, found it fine\" recorded as `done` loses the \
                                 finding.",
                            )
                            .class(v::ext::XSD_STRING)
                            .one_of(v::CLOSE_REASONS.iter().map(|(short, _)| *short))
                            .default_value("done")
                            .optional(),
                    )
                    .input(
                        ArgSpec::new("content")
                            .summary(
                                "An optional closing note, recorded as a comment — where a \
                                 pipe's value lands.",
                            )
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .input(
                        ArgSpec::new("author")
                            .summary("Who is closing it.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .output(PLAIN),
                CAP_WRITE,
            ))
    }
}

#[derive(Clone)]
struct ReopenEndpoint;

#[async_trait]
impl Endpoint for ReopenEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(unsupported("ledger-reopen", inv.request.verb));
        }
        let client = StoreClient::new(inv, ledger_for(inv, Need::Write)?);
        let now = now_ms(inv)?;
        let item = require_item(&client, inv.inline_str("item")?).await?;
        client
            .update(&batch(&[
                replace_one(&client, &item.iri, v::STATUS, &format!("<{}>", v::OPEN)),
                retract(&client, &item.iri, v::CLOSED_REASON),
                touch(&client, &item.iri, now),
            ]))
            .await?;
        if let Ok(note) = inv.inline_str("content") {
            if !note.trim().is_empty() {
                write_comment(
                    &client,
                    &item.iri,
                    note.trim(),
                    inv.inline_str("author").ok(),
                    now,
                )
                .await?;
            }
        }
        Ok(plain(format!("reopened {}\n", item.short())))
    }

    fn name(&self) -> &str {
        "ledger-reopen"
    }

    fn describe(&self) -> Description {
        Description::new("ledger-reopen")
            .title("Reopen a ledger item")
            .summary("Undo a close: the status goes back to open and the reason is dropped.")
            .verb(Verb::Meta)
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Sink)
                    .input(ledger_arg())
                    .summary("Set the status back to open.")
                    .input(item_arg("The item to reopen."))
                    .input(
                        ArgSpec::new("content")
                            .summary("An optional note, recorded as a comment.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .input(
                        ArgSpec::new("author")
                            .summary("Who is reopening it.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .output(PLAIN),
                CAP_WRITE,
            ))
    }
}

// --------------------------------------------------------------------------- claim

#[derive(Clone)]
struct ClaimEndpoint;

#[async_trait]
impl Endpoint for ClaimEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let client = StoreClient::new(inv, ledger_for(inv, Need::Write)?);
        let now = now_ms(inv)?;
        let item = require_item(&client, inv.inline_str("item")?).await?;
        match inv.request.verb {
            Verb::Sink => {
                let holder = inv.inline_str("content")?.trim().to_string();
                if holder.is_empty() {
                    return Err(Error::InvalidArgument {
                        name: "content".to_string(),
                        detail: "a claim needs a holder: an unattributed claim is \
                                 indistinguishable from no claim"
                            .to_string(),
                    });
                }
                // ⚠ Read-then-write, so two claimants in one process can interleave. The
                // fence this ledger will eventually replace is enforced by a partial unique
                // index in kata's database; RDF has no such constraint and SHACL cannot see
                // a race. Stated rather than pretended away — it is safe for one operator
                // and is the first thing to harden when a second one appears.
                if let Some(held) = &item.claimed_by {
                    if held != &holder {
                        return Err(Error::Unavailable(format!(
                            "{} is already claimed by {held}. Release it first \
                             (`delete urn:iki:ledger:claim item={}`), or take it up with \
                             them — a claim is a fence, and stealing one silently is how two \
                             workers end up in the same tree",
                            item.short(),
                            item.short()
                        )));
                    }
                }
                let mut operations = vec![
                    replace_one(&client, &item.iri, v::CLAIMED_BY, &literal(&holder)),
                    replace_one(&client, &item.iri, v::CLAIMED_AT, &datetime(now)),
                ];
                if let Ok(purpose) = inv.inline_str("purpose") {
                    operations.push(replace_one(
                        &client,
                        &item.iri,
                        v::PURPOSE,
                        &literal(purpose),
                    ));
                }
                operations.push(touch(&client, &item.iri, now));
                client.update(&batch(&operations)).await?;
                Ok(plain(format!("{} claimed by {holder}\n", item.short())))
            }
            Verb::Delete => {
                client
                    .update(&batch(&[
                        retract(&client, &item.iri, v::CLAIMED_BY),
                        retract(&client, &item.iri, v::CLAIMED_AT),
                        retract(&client, &item.iri, v::PURPOSE),
                        touch(&client, &item.iri, now),
                    ]))
                    .await?;
                if let Ok(note) = inv.inline_str("content") {
                    if !note.trim().is_empty() {
                        write_comment(&client, &item.iri, note.trim(), None, now).await?;
                    }
                }
                Ok(plain(format!("{} released\n", item.short())))
            }
            other => Err(unsupported("ledger-claim", other)),
        }
    }

    fn name(&self) -> &str {
        "ledger-claim"
    }

    fn describe(&self) -> Description {
        Description::new("ledger-claim")
            .title("Claim or release a ledger item")
            .summary(
                "Take an item, or hand it back. The ready set never offers a claimed item, \
                 which is most of what \"what should I do next\" means once more than one \
                 worker shares a ledger.",
            )
            .verb(Verb::Meta)
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Sink)
                    .input(ledger_arg())
                    .summary("Claim the item for a holder; refuses if someone else holds it.")
                    .input(item_arg("The item to claim."))
                    .input(
                        ArgSpec::new("content")
                            .summary("The holder — where a pipe's value lands.")
                            .class(v::ext::XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("purpose")
                            .summary("Why the holder took it — a brief name, a session id.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .output(PLAIN),
                CAP_WRITE,
            ))
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Delete)
                    .input(ledger_arg())
                    .summary("Release the claim, whoever holds it.")
                    .input(item_arg("The item to release."))
                    .input(
                        ArgSpec::new("content")
                            .summary("An optional release note, recorded as a comment.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .output(PLAIN),
                CAP_WRITE,
            ))
    }
}

// --------------------------------------------------------------------------- defer

#[derive(Clone)]
struct DeferEndpoint;

#[async_trait]
impl Endpoint for DeferEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let client = StoreClient::new(inv, ledger_for(inv, Need::Write)?);
        let now = now_ms(inv)?;
        let item = require_item(&client, inv.inline_str("item")?).await?;
        let (update, said) = match inv.request.verb {
            Verb::Sink => (
                replace_one(&client, &item.iri, v::DEFERRED, &boolean(true)),
                "deferred",
            ),
            Verb::Delete => (retract(&client, &item.iri, v::DEFERRED), "resumed"),
            other => return Err(unsupported("ledger-defer", other)),
        };
        client
            .update(&batch(&[update, touch(&client, &item.iri, now)]))
            .await?;
        if let Ok(note) = inv.inline_str("content") {
            if !note.trim().is_empty() {
                write_comment(&client, &item.iri, note.trim(), None, now).await?;
            }
        }
        Ok(plain(format!("{} {said}\n", item.short())))
    }

    fn name(&self) -> &str {
        "ledger-defer"
    }

    fn describe(&self) -> Description {
        Description::new("ledger-defer")
            .title("Defer a ledger item, or resume it")
            .summary(
                "Not now: open, unblocked, unclaimed, and deliberately not offered by the \
                 ready set. A modelled property rather than kata's `someday` flag inside a \
                 metadata blob that one query understands — and a boolean rather than a \
                 date, because readiness that turns on the wall clock goes stale in the \
                 cache with no thread to cut it.",
            )
            .verb(Verb::Meta)
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Sink)
                    .input(ledger_arg())
                    .summary("Mark the item deferred.")
                    .input(item_arg("The item to defer."))
                    .input(
                        ArgSpec::new("content")
                            .summary("An optional note, recorded as a comment.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .output(PLAIN),
                CAP_WRITE,
            ))
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Delete)
                    .input(ledger_arg())
                    .summary("Undefer: the item becomes eligible for the ready set again.")
                    .input(item_arg("The item to resume."))
                    .input(
                        ArgSpec::new("content")
                            .summary("An optional note, recorded as a comment.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .output(PLAIN),
                CAP_WRITE,
            ))
    }
}

// ---------------------------------------------------------------------------- link

#[derive(Clone)]
struct LinkEndpoint;

#[async_trait]
impl Endpoint for LinkEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let client = StoreClient::new(inv, ledger_for(inv, Need::Write)?);
        let now = now_ms(inv)?;
        let from = require_item(&client, inv.inline_str("item")?).await?;
        let to = require_item(&client, inv.inline_str("content")?).await?;
        let name = inv.inline_str("type").unwrap_or("related");
        let predicate = v::link_predicate(name).ok_or_else(|| Error::InvalidArgument {
            name: "type".to_string(),
            detail: format!(
                "`{name}` is not one of {}",
                v::LINK_TYPES
                    .iter()
                    .map(|(short, _)| *short)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })?;
        if from.iri == to.iri && name == "blocks" {
            return Err(Error::InvalidArgument {
                name: "content".to_string(),
                detail: format!(
                    "{} cannot block itself: a self-block is a cycle, and the ready set \
                     would refuse to compute at all",
                    from.short()
                ),
            });
        }
        let triple = format!("<{}> <{predicate}> <{}> .", from.iri, to.iri);
        let (update, said) = match inv.request.verb {
            Verb::Sink => (
                format!("INSERT DATA {{ {} }}", client.in_graph(&triple)),
                "linked",
            ),
            Verb::Delete => (
                format!("DELETE DATA {{ {} }}", client.in_graph(&triple)),
                "unlinked",
            ),
            other => return Err(unsupported("ledger-link", other)),
        };
        client
            .update(&batch(&[update, touch(&client, &from.iri, now)]))
            .await?;
        Ok(plain(format!(
            "{said} {} {name} {}\n",
            from.short(),
            to.short()
        )))
    }

    fn name(&self) -> &str {
        "ledger-link"
    }

    fn describe(&self) -> Description {
        let type_arg = || {
            ArgSpec::new("type")
                .summary(
                    "blocks (the object cannot start until the subject finishes), parent \
                     (structural, does not block), or related.",
                )
                .class(v::ext::XSD_STRING)
                .one_of(v::LINK_TYPES.iter().map(|(short, _)| *short))
                .default_value("related")
                .optional()
        };
        Description::new("ledger-link")
            .title("Link two ledger items, or unlink them")
            .summary(
                "The item graph. `blocks` edges are what the ready set reads; they are \
                 meant to be a DAG and nothing enforces it, so `urn:iki:ledger:next` walks \
                 them and refuses with the cycle named.",
            )
            .verb(Verb::Meta)
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Sink)
                    .input(ledger_arg())
                    .summary("Add the edge.")
                    .input(item_arg("The subject item."))
                    .input(
                        ArgSpec::new("content")
                            .summary(
                                "The object item — where a pipe's value lands. Same forms as \
                                 `item`.",
                            )
                            .class(v::ext::XSD_STRING),
                    )
                    .input(type_arg())
                    .output(PLAIN),
                CAP_WRITE,
            ))
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Delete)
                    .input(ledger_arg())
                    .summary("Remove the edge — how a block cycle is broken.")
                    .input(item_arg("The subject item."))
                    .input(
                        ArgSpec::new("content")
                            .summary("The object item.")
                            .class(v::ext::XSD_STRING),
                    )
                    .input(type_arg())
                    .output(PLAIN),
                CAP_WRITE,
            ))
    }
}

// --------------------------------------------------------------------------- label

#[derive(Clone)]
struct LabelEndpoint;

#[async_trait]
impl Endpoint for LabelEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let client = StoreClient::new(inv, ledger_for(inv, Need::Write)?);
        let now = now_ms(inv)?;
        let item = require_item(&client, inv.inline_str("item")?).await?;
        let label = inv.inline_str("content")?.trim();
        if label.is_empty() {
            return Err(Error::InvalidArgument {
                name: "content".to_string(),
                detail: "a label needs text".to_string(),
            });
        }
        let triple = format!("<{}> <{}> {} .", item.iri, v::LABEL, literal(label));
        let (update, said) = match inv.request.verb {
            Verb::Sink => (
                format!("INSERT DATA {{ {} }}", client.in_graph(&triple)),
                "labelled",
            ),
            Verb::Delete => (
                format!("DELETE DATA {{ {} }}", client.in_graph(&triple)),
                "unlabelled",
            ),
            other => return Err(unsupported("ledger-label", other)),
        };
        client
            .update(&batch(&[update, touch(&client, &item.iri, now)]))
            .await?;
        Ok(plain(format!("{said} {} {label}\n", item.short())))
    }

    fn name(&self) -> &str {
        "ledger-label"
    }

    fn describe(&self) -> Description {
        Description::new("ledger-label")
            .title("Tag a ledger item, or untag it")
            .summary(
                "Labels are what the ready query filters on — must have ALL of these, must \
                 have NONE of those — which is why the ordering policy does not need to \
                 know about them.",
            )
            .verb(Verb::Meta)
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Sink)
                    .input(ledger_arg())
                    .summary("Add the label.")
                    .input(item_arg("The item to tag."))
                    .input(
                        ArgSpec::new("content")
                            .summary("The label — where a pipe's value lands.")
                            .class(v::ext::XSD_STRING),
                    )
                    .output(PLAIN),
                CAP_WRITE,
            ))
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Delete)
                    .input(ledger_arg())
                    .summary("Remove the label.")
                    .input(item_arg("The item to untag."))
                    .input(
                        ArgSpec::new("content")
                            .summary("The label to remove.")
                            .class(v::ext::XSD_STRING),
                    )
                    .output(PLAIN),
                CAP_WRITE,
            ))
    }
}

// --------------------------------------------------------------------------- purge

#[derive(Clone)]
struct PurgeEndpoint;

#[async_trait]
impl Endpoint for PurgeEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Delete {
            return Err(unsupported("ledger-purge", inv.request.verb));
        }
        let client = StoreClient::new(inv, ledger_for(inv, Need::Purge)?);
        let now = now_ms(inv)?;
        let reference = inv.inline_str("content")?;
        let item = require_item(&client, reference).await?;
        let reason = inv.inline_str("reason").unwrap_or("");
        let removed = delete_item(
            &client,
            &item,
            reason,
            inv.inline_str("author").ok(),
            now,
            true,
        )
        .await?;
        Ok(plain(format!(
            "purged {} {}\n  {} quad(s) destroyed, NOT recoverable\n  tombstone: {}\n",
            item.short(),
            item.iri,
            removed.quads,
            removed.tombstone
        )))
    }

    fn name(&self) -> &str {
        "ledger-purge"
    }

    fn describe(&self) -> Description {
        Description::new("ledger-purge")
            .title("Destroy a ledger item's content")
            .summary(
                "Irreversible: the item's quads, its comments and the edges pointing at it \
                 are removed from both the live graph and the quarantine graph. A tombstone \
                 stays, carrying the number, the time, the actor, the reason, the quad count \
                 and the sha256 of what was destroyed — so a later claim about what it said \
                 is checkable, and a ledger that forgets still leaves evidence that it did.",
            )
            .verb(Verb::Meta)
            .action(write_scopes(
                ikigai_core::ActionSpec::new(Verb::Delete)
                    .input(ledger_arg())
                    .summary("Destroy the item's content, leaving only its tombstone.")
                    .input(
                        ArgSpec::new("content")
                            .summary(
                                "The item — where a pipe's value lands. `#12`, an opaque id, \
                                 or the full IRI.",
                            )
                            .class(v::ext::XSD_STRING),
                    )
                    .input(
                        ArgSpec::new("reason")
                            .summary("Why — kept on the tombstone, which is all that is left.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .input(
                        ArgSpec::new("author")
                            .summary("Who purged it.")
                            .class(v::ext::XSD_STRING)
                            .optional(),
                    )
                    .output(PLAIN),
                CAP_PURGE,
            ))
    }
}

// ---------------------------------------------------------------------------- next

#[derive(Clone)]
struct NextEndpoint {
    policies: Arc<Policies>,
}

#[async_trait]
impl Endpoint for NextEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(unsupported("ledger-next", inv.request.verb));
        }
        let client = StoreClient::new(inv, ledger_for(inv, Need::Read)?);
        let want = wanted_face(inv)?;
        let now = now_ms(inv)?;
        let name = inv
            .inline_str("policy")
            .unwrap_or_else(|_| self.policies.default_name());
        let policy = self
            .policies
            .get(name)
            .ok_or_else(|| Error::InvalidArgument {
                name: "policy".to_string(),
                detail: format!(
                    "no ordering policy `{name}`; this host offers {}",
                    self.policies.names().join(", ")
                ),
            })?;
        let filter = Filter {
            status: Status::Open,
            kind: inv.inline_str("kind").ok().map(str::to_string),
            labels: split_labels(inv.inline_str("labels").ok()),
            without: split_labels(inv.inline_str("without").ok()),
            about: inv.inline_str("about").ok().map(str::to_string),
            ..Filter::default()
        };
        let limit = inv
            .inline_str("limit")
            .ok()
            .and_then(|l| l.parse().ok())
            .unwrap_or(3);
        let set = select::ready(&client, &filter).await?;
        let selection = select::rank(client.ledger(), set, policy.as_ref(), now, limit);
        if want == TURTLE {
            return Ok(Representation::new(
                ReprType::new(TURTLE).with_param("charset", "utf-8"),
                selection.turtle()?,
            )
            .cacheable()
            .depends_on(ikigai_store::UPDATE_THREAD)
            .depends_on(ikigai_store::LOAD_THREAD)
            .depends_on(ikigai_store::GRAPH_UPDATE_THREAD));
        }
        face(selection.plain(), None, want)
    }

    fn name(&self) -> &str {
        "ledger-next"
    }

    fn describe(&self) -> Description {
        read_scopes(
            Description::new("ledger-next")
                .title("What to do next")
                .summary(
                    "The ready set — open, unclaimed, not deferred, not blocked by an open \
                     item — ranked by an ordering policy, with the reasons. Refuses, naming \
                     the cycle, when the `blocks` graph is not a DAG. ⚠ Selection OFFERS; it \
                     never authorizes: acting on the answer still requires the actor's own \
                     capability.",
                )
                .verb(Verb::Source)
                .verb(Verb::Meta)
                .input(ledger_arg())
                .input(
                    ArgSpec::new("policy")
                        .summary(format!(
                            "Which ordering policy ranks the ready set. Each is a resource \
                             at `urn:iki:ledger:policy:{{name}}` that says what it weighs. \
                             This host offers: {}.",
                            self.policies.names().join(", ")
                        ))
                        .class(v::ext::XSD_STRING)
                        .one_of(self.policies.names())
                        .default_value(self.policies.default_name())
                        .optional(),
                )
                .input(
                    ArgSpec::new("kind")
                        .summary(
                            "An item class IRI — ask \"what is next\" at one LEVEL. \"What is \
                             next\" among decisions is a different question from \"what is \
                             next\" among findings.",
                        )
                        .class(v::ext::XSD_ANY_URI)
                        .optional(),
                )
                .input(
                    ArgSpec::new("labels")
                        .summary("Comma-separated; a candidate must carry ALL of them.")
                        .class(v::ext::XSD_STRING)
                        .optional(),
                )
                .input(
                    ArgSpec::new("without")
                        .summary("Comma-separated; a candidate must carry NONE of them.")
                        .class(v::ext::XSD_STRING)
                        .optional(),
                )
                .input(
                    ArgSpec::new("about")
                        .summary("Only candidates filed against this resource IRI.")
                        .class(v::ext::XSD_ANY_URI)
                        .optional(),
                )
                .input(
                    ArgSpec::new("limit")
                        .summary(
                            "How many ranked candidates to return (default 3). Applied AFTER \
                             ranking, never before.",
                        )
                        .class(v::ext::XSD_INTEGER)
                        .default_value("3")
                        .optional(),
                )
                .input(as_arg())
                .output(PLAIN)
                .output(TURTLE),
        )
    }
}

// -------------------------------------------------------------------------- policy

#[derive(Clone)]
struct PolicyEndpoint {
    policies: Arc<Policies>,
}

#[async_trait]
impl Endpoint for PolicyEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let name = inv.bindings.get("name").ok_or_else(|| {
            Error::Endpoint("no `name` captured from `urn:iki:ledger:policy:{name}`".to_string())
        })?;
        let policy = self.policies.get(name).ok_or_else(|| {
            Error::NotFound(format!(
                "no ordering policy `{name}`; this host offers {}",
                self.policies.names().join(", ")
            ))
        })?;
        match inv.request.verb {
            Verb::Exists => Ok(plain("true\n")),
            Verb::Source => {
                let want = wanted_face(inv)?;
                if want == TURTLE {
                    let iri = format!("{}{}", v::iri::POLICY, policy.name());
                    let subject = oxrdf::NamedNode::new(&iri)
                        .map_err(|e| Error::Endpoint(format!("policy IRI: {e}")))?;
                    let mut graph = Graph::new();
                    let mut push = |p: &str, o: oxrdf::Term| {
                        if let Ok(predicate) = oxrdf::NamedNode::new(p) {
                            graph.insert(&oxrdf::Triple::new(subject.clone(), predicate, o));
                        }
                    };
                    push(v::ext::TYPE, model::named(v::POLICY_CLASS));
                    push(v::ext::LABEL, model::plain(policy.name()));
                    push(v::ext::COMMENT, model::plain(policy.summary()));
                    for signal in policy.weighs() {
                        push(v::WEIGHS, model::plain(signal));
                    }
                    // Pure: a policy resource reads no state, so it is cacheable with no
                    // thread — the one endpoint here that is a function of its inputs alone.
                    return Ok(Representation::new(
                        ReprType::new(TURTLE).with_param("charset", "utf-8"),
                        model::turtle(&graph)?,
                    )
                    .cacheable());
                }
                Ok(plain(format!(
                    "{}\n  {}\n  weighs: {}\n",
                    policy.name(),
                    policy.summary(),
                    policy.weighs().join(", ")
                ))
                .cacheable())
            }
            other => Err(unsupported("ledger-policy", other)),
        }
    }

    fn name(&self) -> &str {
        "ledger-policy"
    }

    fn describe(&self) -> Description {
        let name_input = || {
            ArgSpec::new("name")
                .summary("The policy's name.")
                .class(v::ext::XSD_STRING)
                .binding()
        };
        Description::new("ledger-policy")
            .title("An ordering policy")
            .summary(
                "What one `urn:iki:ledger:next` policy weighs, and in what order. A policy \
                 is a resource rather than a config key so the manifold advertises which \
                 orderings exist, and so one can be versioned, diffed and handed to another \
                 host.",
            )
            .verb(Verb::Meta)
            // Per-verb, because the two verbs really do serve different faces: `Exists`
            // answers `true` in text/plain and nothing else, and declaring the graph face
            // on it would promise a Turtle document that no `as=` could ever produce.
            .action(
                ikigai_core::ActionSpec::new(Verb::Source)
                    .summary("What this policy weighs, and in what order.")
                    .requires(CAP_READ)
                    .input(name_input())
                    .input(as_arg())
                    .output(PLAIN)
                    .output(TURTLE),
            )
            .action(
                ikigai_core::ActionSpec::new(Verb::Exists)
                    .summary("Whether this host offers a policy by that name.")
                    .requires(CAP_READ)
                    .input(name_input())
                    .output(PLAIN),
            )
    }
}

// ------------------------------------------------------------------------- ledgers

/// `urn:iki:ledger:ledgers` — which ledgers exist, filtered to the ones this caller may
/// read.
///
/// ★ **Naming ledgers costs discoverability, and this is what buys it back.** The
/// per-ledger resources are bound by a grammar, and a grammar does not enumerate: a
/// catalog can say `urn:iki:ledger:{ledger}:items` exists, and cannot say that `acme` and
/// `bosatsu` are the ledgers it can be asked about. Without this resource a caller who
/// was not *told* a name could not find one, which would make the partition a secret
/// rather than a boundary.
///
/// ⚠ The listing is filtered by the caller's own read grants, so it says what this
/// capability may read and not what the store holds. That is the honest answer to the
/// question asked, and it is also the only one that does not leak a client list to a
/// caller granted one ledger.
///
/// # ★ The only resource here that cannot be answered by one scoped read
///
/// Every other read in this crate names its graph and asks the store for that graph alone.
/// This one asks *which graphs are there* — and a graph-scoped read is confined to a graph
/// the caller already named, so it can enumerate nothing. Two paths, asking **the same
/// query**, differing only in which door:
///
/// - A **scoped** capability carries the answer already: its `urn:cap:ledger:read:{name}`
///   grants ARE the set of ledgers it may see. So the candidates come from the capability,
///   and each one is confirmed with one narrow read of its own graph. A ledger this caller
///   holds no grant for is never asked about, which is the same answer the old filter gave
///   and reaches it without the store ever seeing a cross-graph query.
/// - A **root** capability enumerates nothing — `Capability::scopes()` is `None` for root,
///   which is what root means — so this resolves the broad `urn:iki:store:select` once.
///   Root holds every grant there is, so that widens no authority; it is simply the only
///   way the question can be answered under it. [`crate::sparql::select_every_graph`] is
///   the one function in this crate that names a broad door, and it says so at length.
///
/// ⚠ A ledger granted at THIS module but not at the store **refuses the whole listing**,
/// naming the token that is missing. A half-granted config is the most likely operator
/// error now that a ledger needs two tokens per direction, and a ledger quietly left out of
/// the answer is indistinguishable from one with nothing filed in it.
#[derive(Clone)]
struct LedgersEndpoint;

#[async_trait]
impl Endpoint for LedgersEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(unsupported("ledger-ledgers", inv.request.verb));
        }
        let want = wanted_face(inv)?;
        // A ledger exists once something has been filed in it — the counter is what
        // survives closing and deleting everything, so it, and not the item count, is
        // what says a ledger is there at all.
        //
        // ★ ONE query text for both paths. Through the narrow door the dataset is exactly
        // `FROM <G> FROM NAMED <G>`, so `GRAPH ?g` can only bind G and the grouping
        // yields the one row for that ledger — the same shape the broad door yields per
        // graph. The branch below is about which door, never about what is asked.
        let query = format!(
            "SELECT ?g (COUNT(?item) AS ?items) (SUM(IF(?status = <{open}>, 1, 0)) AS ?open) \
             WHERE {{\n  GRAPH ?g {{ ?counter <{type_}> <{counter_class}> }}\n  \
             OPTIONAL {{ GRAPH ?g {{ ?item <{type_}> <{item_class}> ; <{status}> ?status }} }}\n\
             }} GROUP BY ?g ORDER BY ?g",
            open = v::OPEN,
            type_ = v::ext::TYPE,
            counter_class = v::COUNTER_CLASS,
            item_class = v::ITEM_CLASS,
            status = v::STATUS,
        );

        let mut found: Vec<(Ledger, i64, i64)> = Vec::new();

        if inv.capability.is_root() {
            for row in &crate::sparql::select_every_graph(inv, &query).await? {
                let Some(graph) = row.get("g") else { continue };
                // The graph IRI is the ledger's name spelled one way; a graveyard graph
                // and anything else the host keeps in this store are not ledgers and are
                // skipped rather than guessed at.
                let Some(ledger) = ledger_of_graph(&graph.value) else {
                    continue;
                };
                found.push((ledger, count(row, "items"), count(row, "open")));
            }
        } else {
            for ledger in granted_ledgers(inv) {
                // ⚠ **A half-granted ledger stops the listing rather than thinning it.**
                // Two tokens per ledger per direction is the shape an operator will get
                // half right, and a ledger left out of the answer is indistinguishable
                // from one with nothing filed in it — a wrong answer that looks right.
                // The alternative considered was a warning line beside the results, and it
                // was rejected because `as=text/turtle` has nowhere to put one without
                // inventing a class for "a ledger you cannot see", so the two faces would
                // have disagreed about the same question. This is a host misconfiguration
                // with a mechanical fix, and the message names the exact token.
                let scope = ikigai_store::cap_read_graph(&ledger.graph());
                if !inv.capability.allows(&scope) {
                    return Err(Error::Denied(format!(
                        "this capability holds `{}` but not `{scope}`, so the ledger `{}` can \
                         be addressed here and not read in the store, and this listing would \
                         have to omit it — which is indistinguishable from an empty ledger. \
                         Grant the store scope too, or drop the ledger scope: a ledger needs \
                         both halves, and delete and purge need `{}` as well",
                        ledger.cap_read(),
                        ledger.name(),
                        ikigai_store::cap_write_graph(&ledger.deleted_graph()),
                    )));
                }
                let client = StoreClient::new(inv, ledger.clone());
                // At most one row: the scoped dataset holds one graph.
                if let Some(row) = client.select(&query).await?.first() {
                    found.push((ledger, count(row, "items"), count(row, "open")));
                }
            }
            found.sort_by(|a, b| a.0.name().cmp(b.0.name()));
        }

        if want == TURTLE {
            let mut graph = Graph::new();
            for (ledger, items, open) in &found {
                let Ok(subject) = oxrdf::NamedNode::new(ledger.prefix().trim_end_matches(':'))
                else {
                    continue;
                };
                let mut push = |p: &str, o: oxrdf::Term| {
                    if let Ok(predicate) = oxrdf::NamedNode::new(p) {
                        graph.insert(&oxrdf::Triple::new(subject.clone(), predicate, o));
                    }
                };
                push(v::ext::TYPE, model::named(v::LEDGER_CLASS));
                push(v::ext::LABEL, model::plain(ledger.name()));
                push(v::LEDGER_GRAPH, model::named(&ledger.graph()));
                push(
                    v::ITEM_COUNT,
                    model::typed(&items.to_string(), v::ext::XSD_INTEGER),
                );
                push(
                    v::OPEN_COUNT,
                    model::typed(&open.to_string(), v::ext::XSD_INTEGER),
                );
            }
            return face(String::new(), Some(graph), want);
        }

        let mut text = if found.is_empty() {
            "no ledgers this capability may read\n".to_string()
        } else {
            found
                .iter()
                .map(|(ledger, items, open)| {
                    format!(
                        "{:<24}  {open:>5} open  {items:>5} total  {}\n",
                        ledger.name(),
                        ledger.graph()
                    )
                })
                .collect()
        };
        text.push_str(&format!("\n{} ledger(s)\n", found.len()));
        face(text, None, want)
    }

    fn name(&self) -> &str {
        "ledger-ledgers"
    }

    fn describe(&self) -> Description {
        read_scopes(
            Description::new("ledger-ledgers")
                .title("Which ledgers exist")
                .summary(
                    "Every ledger this capability may read, with how many items each holds \
                     and the named graph it lives in. A ledger exists once something has \
                     been filed in it; there is no create and no destroy, because the \
                     capability is what makes one real. The listing is FILTERED by the \
                     caller's own read grants, so it answers what you may read rather than \
                     what the store holds.",
                )
                .verb(Verb::Source)
                .verb(Verb::Meta)
                .input(as_arg())
                .output(PLAIN)
                .output(TURTLE),
        )
    }
}

/// An aggregate column, or zero. An aggregate over no solutions is unbound rather than 0.
fn count(row: &crate::sparql::Row, var: &str) -> i64 {
    row.get(var).and_then(|b| b.as_i64()).unwrap_or(0)
}

/// The ledgers a non-root capability names in its own read grants, in name order.
///
/// ★ **The grant list IS the candidate list**, and that is not a shortcut — it is the
/// same answer `urn:iki:ledger:ledgers` has always given (it filtered a store-wide query
/// by exactly this predicate), reached without asking the store a question that crosses
/// graphs. A ledger with no grant was never going to be listed.
///
/// ⚠ A **family** grant (`urn:cap:ledger:read:*`) enumerates nothing and is skipped: a
/// wildcard says what may be reached, not what exists, and guessing names from it is not
/// a thing this can do. A host granting the family and nothing else gets an empty listing
/// — which is why the family is a *declaration* form in this ecosystem and not a grant
/// anyone should hold.
fn granted_ledgers(inv: &Invocation<'_>) -> Vec<Ledger> {
    let Some(scopes) = inv.capability.scopes() else {
        return Vec::new();
    };
    let prefix = format!("{}:", CAP_READ.trim_end_matches('*').trim_end_matches(':'));
    let mut ledgers: Vec<Ledger> = scopes
        .iter()
        .filter_map(|scope| scope.strip_prefix(&prefix))
        .filter_map(|name| Ledger::parse(name).ok())
        .collect();
    ledgers.sort_by(|a, b| a.name().cmp(b.name()));
    ledgers.dedup_by(|a, b| a.name() == b.name());
    ledgers
}

/// The ledger a graph IRI names, if it names one at all.
///
/// ⚠ A graveyard (`…:graph:{name}:deleted`) is deliberately NOT a ledger: it is one
/// ledger's quarantine, and listing it would invent a partition that has no resources,
/// no counter and no capability.
fn ledger_of_graph(graph: &str) -> Option<Ledger> {
    let name = graph.strip_prefix(&format!("{}graph:", crate::ledger::PREFIX))?;
    Ledger::parse(name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_time_ordered_and_unique_per_millisecond() {
        let earlier = mint_id(1_757_700_000_000, "a", "brian");
        let later = mint_id(1_757_700_000_001, "a", "brian");
        assert!(earlier < later, "{earlier} should sort before {later}");
        assert_eq!(earlier.len(), 16);
        // Same millisecond, different content: the digest half separates them, which is
        // what keeps two merged ledgers from colliding.
        assert_ne!(
            mint_id(1_757_700_000_000, "a", "brian"),
            mint_id(1_757_700_000_000, "b", "brian")
        );
    }

    #[test]
    fn content_splits_into_title_and_body() {
        let (title, body) = split_content("Fix the thing\n\nIt is broken.\nBadly.").unwrap();
        assert_eq!(title, "Fix the thing");
        assert_eq!(body, "It is broken.\nBadly.");
        let (title, body) = split_content("  Just a title  ").unwrap();
        assert_eq!(title, "Just a title");
        assert!(body.is_empty());
        assert!(split_content("   ").is_err());
    }

    #[test]
    fn an_unserveable_face_is_refused_not_substituted() {
        assert_eq!(bare("text/turtle; charset=utf-8"), "text/turtle");
    }

    #[test]
    fn a_priority_outside_the_range_is_refused() {
        assert!(parse_priority("0").is_ok());
        assert!(parse_priority("4").is_ok());
        assert!(parse_priority("5").is_err());
        assert!(parse_priority("high").is_err());
    }
}
