//! The three doors onto the one kernel, and what each hands a caller.
//!
//! | door | transport | who can reach it | capability |
//! |---|---|---|---|
//! | HTTP | `ikigai-web`, loopback TCP | any local process | [`http_cap`]: the configured ledgers' narrow read+write tokens for an anonymous loopback caller, plus the grant of a signed-in passkey; nothing for a non-loopback peer, a foreign `Host`, or a cross-site write. And a NAME beside the authority — [`http_principal`]: the signed-in passkey's stable IRI, stamped on every write as `principal` |
//! | socket | `ikigai-ipc`, `0600` Unix socket, peer UID checked | this user only | root — the owner, who can read the dataset's files anyway |
//! | QUIC | `ikigai-quic`, mutual TLS | a certificate this server trusts | the grant that certificate's fingerprint maps to in `clients.json`; refused when it maps to none |
//!
//! # ★ One cache for the whole process
//!
//! `ikigai_ipc::serve` and `ikigai_quic::serve_with` each take a [`Kernel`] **by value**, and
//! a kernel owns its cache and its golden threads. Three kernels over one store would be
//! three caches, and a write through one door cuts threads in that kernel only — so the
//! other two would go on serving the read they cached before it, indefinitely, with nothing
//! wrong in any log.
//!
//! So there is one kernel, the **hub** ([`crate::compose`]), shared as `Arc<Kernel>`. Every
//! door gets a kernel whose space FORWARDS to the hub ([`HubSpace`]) under a cache policy that
//! admits nothing ([`NoCache`]). Every read and every write passes through the hub's cache
//! and the hub's threads, whichever door it came in by.
//!
//! The HTTP door's kernel ([`http_kernel`]) is the same shape with two more spaces around the
//! hub: gonk's own pages in front ([`crate::web`]), and a catch-all behind ([`NotFound`]). Its
//! pages never cache — they are cheap to render and a cached page is exactly the second
//! cache this design exists to avoid — and every ledger read a page makes is a hub read.
//!
//! ⚠ **Why not `ikigai_resolve::RemoteSpace` over the hub**, which is the mount machinery
//! and would have been the obvious reuse: its forwarding endpoint calls the synchronous
//! `Resolver::issue_as`, and `impl Resolver for Kernel` implements that with
//! `futures::executor::block_on`. The socket door already runs each call inside
//! `ikigai_resolve::issue_traced_as`'s own `block_on`, and futures' executor panics when
//! entered twice on one thread. [`HubSpace`] awaits the hub instead.

use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    Bindings, CachePolicy, Capability, Description, Endpoint, EndpointSpace, EntryFacts, Error,
    Fallback, Invocation, Iri, Kernel, Representation, Request, Resolution, Resolved, Result,
    Scope, Space, SpaceEntry, SpaceKind, SystemClock, Topology, Verb,
};
use ikigai_vocab::TurtleRenderer;
use ikigai_web::{CapFn, EdgeConfig, HttpRequest, PrincipalFn, Route, RouteTable};

use crate::identity::{self, Passkeys};
use crate::spaces;

/// A space whose every binding is the hub's: it resolves what the hub binds, describes it
/// with the hub's own description, enumerates the hub's catalog, and forwards invocation to
/// the hub under the caller's capability.
///
/// # Identity and structure: the hub's, forwarded
///
/// This space claims no name of its own. It holds exactly the doors the hub's root space
/// holds — that is its whole definition — so under core's rule (*same name ⇒ same doors*)
/// the honest identity is the root's ([`crate::spaces::HUB`]), and the honest structure is
/// the root's tree: `urn:kernel:topology` from the socket or QUIC door renders the same
/// `ik:Fallback` under the same IRI the hub renders for itself, rather than an
/// `ik:OpaqueSpace` standing where the arrangement actually continues. An overlay that
/// encloses one space forwards, as [`Arc<dyn Space>`] does; the kernel boundary this space
/// crosses changes which cache answers and which capability check runs first, and neither
/// is structure.
///
/// ⚠ A [`Kernel`] hands out no root space, so both are read off the arrangement resource
/// ([`Kernel::topology`]): for the empty chain that is one `ik:Chain` node whose single
/// layer is the root (`hub_root`). The identity is read ONCE, at construction — a space's
/// structure is fixed once composed, and `id` is consulted on every hit.
pub struct HubSpace {
    hub: Arc<Kernel>,
    /// The hub's root space's own identity, read once.
    id: Option<Iri>,
}

impl HubSpace {
    /// Forward to `hub`.
    pub fn new(hub: Arc<Kernel>) -> Self {
        let id = hub_root(&hub).id;
        HubSpace { hub, id }
    }
}

/// The hub's ROOT space's topology: what [`Kernel::topology`] answers, with the chain
/// unwrapped. The empty chain has exactly one layer, the root; any other shape is not one
/// this function is asked about, and it says nothing rather than guess.
fn hub_root(hub: &Kernel) -> Topology {
    let mut layers = hub.topology().children.into_iter();
    match (layers.next(), layers.next()) {
        (Some(root), None) => root,
        _ => Topology::opaque(None),
    }
}

impl Space for HubSpace {
    fn resolve(&self, request: &Request, _scope: &Scope) -> Resolution {
        // A miss when the hub binds nothing here, so the door reports the kernel's own
        // "no endpoint" rather than a forwarding endpoint that fails on invoke.
        match self.hub.describe(&request.target) {
            // The constructor, not a literal: `Resolved` gains public fields (0.1.64 added
            // `canonical`, 0.1.78 adds `answered_by`), and a literal is E0063 at each one.
            // Nothing is rewritten here — no `.with_canonical` — because the hub
            // canonicalizes, caches and cuts under its own names.
            Some(description) => {
                let resolved = Resolved::new(
                    Arc::new(Forward {
                        hub: Arc::clone(&self.hub),
                        description,
                    }),
                    Bindings::new(),
                );
                // Answered by the hub, under the hub's name: which space INSIDE the hub
                // answers is decided on the hub's own issue path, out of sight of this one.
                Resolution::Hit(match &self.id {
                    Some(id) => resolved.with_answered_by(id.clone()),
                    None => resolved,
                })
            }
            None => Resolution::Miss,
        }
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        // The hub's catalog, minus the hub's own `urn:kernel:*` operations: the door kernel
        // answers those itself, and listing both would name every one twice.
        self.hub.entries().map(|entries| {
            entries
                .into_iter()
                .filter(|entry| !entry.pattern.starts_with("urn:kernel:"))
                .collect()
        })
    }

    fn id(&self) -> Option<Iri> {
        self.id.clone()
    }

    fn topology(&self) -> Topology {
        // The root's tree, not a node enclosing it — see the type's doc. It already omits
        // `urn:kernel:*`, as `entries` does: the kernel's operations are composed in front of
        // the root for description only, and are no part of the root's structure.
        hub_root(&self.hub)
    }
}

/// The endpoint a [`HubSpace`] resolves to.
struct Forward {
    hub: Arc<Kernel>,
    description: Description,
}

#[async_trait]
impl Endpoint for Forward {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // The CALLER's capability, unchanged: the door kernel has already checked the
        // declared floor against the same description, and the hub checks it again along
        // with everything the endpoint enforces inside.
        self.hub.issue(inv.request.clone(), inv.capability).await
    }

    fn name(&self) -> &str {
        &self.description.id
    }

    fn describe(&self) -> Description {
        self.description.clone()
    }
}

/// A cache policy that stores nothing — for the door kernels, whose caches could never be
/// invalidated (the hub is where writes cut threads).
pub struct NoCache;

impl CachePolicy for NoCache {
    fn admit(&self, _candidate: &EntryFacts<'_>) -> bool {
        false
    }

    fn victim(&self, _resident: &[EntryFacts<'_>]) -> Option<usize> {
        None
    }
}

/// A kernel for a transport that takes one by value: a [`HubSpace`] over `hub`, a Meta
/// renderer (a mounting client reads contracts through this kernel's JSON Meta face), and
/// [`NoCache`].
pub fn door_kernel(hub: Arc<Kernel>) -> Kernel {
    Kernel::with_meta_renderer(Arc::new(HubSpace::new(hub)), Arc::new(TurtleRenderer))
        .with_clock(Arc::new(SystemClock))
        .with_cache_policy(Arc::new(NoCache))
}

/// The HTTP door's kernel: gonk's pages ([`crate::web::space`]) in front of the hub, and
/// [`NotFound`] behind it — still storing nothing, still one cache in the process.
pub fn http_kernel(hub: Arc<Kernel>, pages: EndpointSpace) -> Kernel {
    let space = Fallback::new(vec![
        Arc::new(pages) as Arc<dyn Space>,
        Arc::new(HubSpace::new(hub)) as Arc<dyn Space>,
        Arc::new(NotFound) as Arc<dyn Space>,
    ])
    .named(spaces::iri(spaces::HTTP_DOOR));
    Kernel::with_meta_renderer(Arc::new(space), Arc::new(TurtleRenderer))
        .with_clock(Arc::new(SystemClock))
        .with_cache_policy(Arc::new(NoCache))
}

/// The HTTP door's last space: anything nothing else binds is a typed `NotFound`.
///
/// ⚠ **This exists to route around `ikigai-web`'s status mapping**, and it is worth saying
/// where the defect is. A path that resolves to no endpoint surfaces from the kernel as
/// `Error::Unresolved`, and `ikigai-web` maps every error it does not name to `500` — so
/// `GET /favicon.ico` (or a typo) answered `500 no endpoint resolved for urn:favicon.ico`.
/// Here it resolves, to an endpoint that answers `NotFound`, which the library maps to `404`.
/// The body is still `text/plain`: that library writes every error response as plain text,
/// so a 404 cannot be an HTML page from this side of it.
///
/// It enumerates nothing, so it adds no entry to the catalog and cannot be walked or
/// offered as an action.
///
/// # In the topology: a hole over every name
///
/// Named [`crate::spaces::NOT_FOUND`] and reported as `ik:Limit` with the empty family —
/// the prefix of every identifier — because that is what it is for resolution: not a door
/// (it lists none, offers none, and every name it "resolves" is refused) but the floor of
/// the door's chain, past which nothing is reached. Core's `Limit` is the same shape with
/// one difference of rendering: a limiter answers the kernel's ⊥, which `ikigai-web` would
/// map to a 500, and this answers a typed `NotFound`, which it maps to a 404 — the whole
/// reason the type exists. The §12.5 reachability walk over the graph treats the two alike:
/// a family with no door ahead of this layer is unreachable, and a family with one is
/// reached before the walk gets here.
pub struct NotFound;

impl NotFound {
    fn identity() -> Iri {
        spaces::iri(spaces::NOT_FOUND)
    }
}

impl Space for NotFound {
    fn resolve(&self, _request: &Request, _scope: &Scope) -> Resolution {
        Resolution::Hit(
            Resolved::new(Arc::new(NotFoundEndpoint), Bindings::new())
                .with_answered_by(Self::identity()),
        )
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        Some(Vec::new())
    }

    fn id(&self) -> Option<Iri> {
        Some(Self::identity())
    }

    fn topology(&self) -> Topology {
        Topology::new(SpaceKind::Limit {
            family: String::new(),
        })
        .with_id(self.id())
    }
}

struct NotFoundEndpoint;

#[async_trait]
impl Endpoint for NotFoundEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        Err(Error::NotFound(format!(
            "nothing is served at `{}`. The ledger's pages start at / , the SPARQL page is \
             /sparql, and the ledger's own resources are under /iki/ledger/ (for example \
             /iki/ledger/items).",
            inv.request.target
        )))
    }

    fn name(&self) -> &str {
        "gonk-not-found"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-not-found")
            .title("Nothing here")
            .summary("Answers NotFound for every name nothing else binds.")
            .verb(Verb::Source)
            .verb(Verb::Sink)
            .verb(Verb::Delete)
            .verb(Verb::Exists)
    }
}

/// Whether `ip` is loopback, counting an IPv4-mapped IPv6 loopback (`::ffff:127.0.0.1`) —
/// the form a dual-stack listener reports an IPv4 client in.
pub fn is_loopback(ip: IpAddr) -> bool {
    ip.to_canonical().is_loopback()
}

/// What the HTTP door's capability is computed from.
#[derive(Clone)]
pub struct HttpDoor {
    /// What an ANONYMOUS loopback caller holds — the first arc's grant, unchanged: read and
    /// write on the ledgers in `gonk.http.ledger`.
    pub anonymous: Vec<String>,
    /// The port the door is bound to, which the `Host` and `Origin` checks require.
    pub port: u16,
    /// The passkey sessions, when passkeys are on.
    pub passkeys: Option<Arc<Passkeys>>,
}

/// The HTTP door's capability: a function of the REQUEST, not a constant.
///
/// ```text
/// Host not this server's loopback name?        → nothing  (DNS rebinding)
/// a write from another origin or site?         → nothing  (cross-site request forgery)
/// otherwise  (loopback peer ? anonymous : ∅)  ∪  (a live passkey session ? its grant : ∅)
/// ```
///
/// ★ **An anonymous caller is strictly weaker than any identity**, because an identity's
/// capability is the anonymous one PLUS its grant, and `ikigai-gonk passkey invite` refuses a
/// grant that adds nothing. And the identity half reads the SAME `grants.json` the QUIC door
/// reads, through the same fail-closed [`crate::quic::scopes_for_grant`].
///
/// ⚠ **The two browser checks are new with the HTML face, and they close a hole the first arc
/// had.** A door that grants writes to whatever reaches loopback also grants them to any web
/// page open in a browser on the machine: a page on any site can send a form-encoded `POST`
/// to `http://127.0.0.1:1060/iki/ledger/append` without a preflight. A browser always labels
/// such a write with `Origin` (and `Sec-Fetch-Site`), which a local non-browser process —
/// `curl`, a script — does not send, so refusing a foreign one costs those callers nothing.
/// And a rebinding attack reaches loopback under a foreign `Host`, which is refused on every
/// method, reads included.
pub fn http_cap(door: HttpDoor) -> CapFn {
    Arc::new(move |request: &HttpRequest| {
        Capability::scoped(http_scopes(&door, request, identity::now_seconds()))
    })
}

/// The HTTP door's principal: WHO a request is from, as a function of the same cookie
/// [`http_cap`] reads — the identity twin of the capability, wired through
/// [`EdgeConfig::principal_fn`] by [`edge_config`].
///
/// A live passkey session names its passkey's stable IRI ([`identity::passkey_iri`] — the
/// credential id, never the label, so a relabel keeps its history). An anonymous loopback
/// caller is nobody, exactly as before: `None`, and the transport stamps nothing. The
/// transport attaches it to WRITES only, as the argument `principal`, and drops a
/// `?principal=…` a submitter sends — so a value that reaches an endpoint under that name
/// was computed here or not at all.
///
/// ★ It is never authority. What a request may do is [`http_cap`]'s decision alone; this
/// only says who asked, and a resource that declares no argument for it (every ledger
/// action declares `author`, none declares `principal`) ignores it. The form adapter in
/// [`crate::web`] (`Act`) is what turns the one into the other, and only where the target's
/// contract declares `author`.
pub fn http_principal(door: HttpDoor) -> PrincipalFn {
    Arc::new(move |request: &HttpRequest| {
        http_principal_of(&door, request, identity::now_seconds())
    })
}

/// [`http_principal`], with the clock as an argument.
pub fn http_principal_of(door: &HttpDoor, request: &HttpRequest, now: u64) -> Option<String> {
    let passkeys = door.passkeys.as_ref()?;
    let token = session_token(request)?;
    let identity = passkeys.identity(&token, now)?;
    Some(identity::passkey_iri(&identity.enrolled.credential_id))
}

/// [`http_cap`]'s scope list, with the clock as an argument.
pub fn http_scopes(door: &HttpDoor, request: &HttpRequest, now: u64) -> Vec<String> {
    if !host_is_ours(request.header("host"), door.port) {
        return Vec::new();
    }
    let safe = matches!(request.method.as_str(), "GET" | "HEAD" | "OPTIONS");
    if !safe && !same_origin(request, door.port) {
        return Vec::new();
    }
    let mut scopes: Vec<String> = match request.peer {
        Some(peer) if is_loopback(peer) => door.anonymous.clone(),
        _ => Vec::new(),
    };
    if let (Some(passkeys), Some(token)) = (&door.passkeys, session_token(request)) {
        if let Some(identity) = passkeys.identity(&token, now) {
            for scope in identity.scopes {
                if !scopes.contains(&scope) {
                    scopes.push(scope);
                }
            }
        }
    }
    scopes
}

/// The loopback names this door answers to, with or without the port.
fn host_is_ours(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else {
        return false;
    };
    let host = host.trim().to_ascii_lowercase();
    ["localhost", "127.0.0.1", "[::1]"]
        .iter()
        .any(|name| host == *name || host == format!("{name}:{port}"))
}

/// A browser's `Origin` and `Sec-Fetch-Site`, when present, name this server.
fn same_origin(request: &HttpRequest, port: u16) -> bool {
    if let Some(site) = request.header("sec-fetch-site") {
        if !matches!(site.trim(), "same-origin" | "none") {
            return false;
        }
    }
    match request.header("origin") {
        None => true,
        Some(origin) => ["localhost", "127.0.0.1", "[::1]"]
            .iter()
            .any(|name| origin.trim() == format!("http://{name}:{port}")),
    }
}

/// The session token in the request's `Cookie` header.
fn session_token(request: &HttpRequest) -> Option<String> {
    request.header("cookie")?.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == identity::SESSION_COOKIE && !value.is_empty()).then(|| value.to_string())
    })
}

/// The HTTP door's edge policy: `ikigai-web`'s strict defaults, an explicit route table, and
/// the door's principal ([`http_principal`] over the same `door` its capability is computed
/// from — the two seams read one cookie, so a request's name and its authority cannot come
/// from different sessions).
///
/// The page routes are rows here, each onto one of gonk's own resources ([`crate::web`]);
/// every other path takes the mechanical mapping (`POST /iki/ledger/append` → `Sink
/// urn:iki:ledger:append`), exactly as before. The default CSP — `default-src 'self'`, no
/// framing, forms to self — needs no loosening: there is no inline script or style anywhere
/// in the face, and htmx is served from this origin. That holds for the browse family's
/// faces too: `ikigai-browse` asserts in its own tests that it emits no `style=` attribute,
/// and its affordances are htmx attributes rather than script.
///
/// The last two rows are the browse door ([`crate::k`]): `/k` for the adapter every browse
/// affordance targets, and `/browse/…` for the page they render inside. Neither takes a
/// per-route `cap`, deliberately — a route ceiling would REPLACE the per-request capability
/// [`http_cap`] computes, and with it the `Host` and cross-site checks that make a POST to
/// `/k` safe on a machine with a browser open.
pub fn edge_config(door: HttpDoor) -> EdgeConfig {
    let route = |pattern: &str, iri_template: &str| Route {
        pattern: pattern.to_string(),
        iri_template: iri_template.to_string(),
        cap: None,
        cors: None,
        csp: None,
    };
    EdgeConfig {
        routes: RouteTable::new(
            vec![
                route("/", "urn:iki:gonk:page:home"),
                route("/l/{ledger}", "urn:iki:gonk:page:ledger:{ledger}"),
                route("/l/{ledger}/items", "urn:iki:gonk:fragment:items:{ledger}"),
                route(
                    "/l/{ledger}/item/{id}",
                    "urn:iki:gonk:page:item:{ledger}:{id}",
                ),
                route(
                    "/l/{ledger}/item/{id}/card",
                    "urn:iki:gonk:fragment:item:{ledger}:{id}",
                ),
                route("/act", "urn:iki:gonk:act"),
                route("/sparql", "urn:iki:gonk:sparql"),
                route("/sparql/results", "urn:iki:gonk:fragment:sparql"),
                route("/render-rules", "urn:iki:gonk:render-rules"),
                route("/auth/{op}", "urn:iki:gonk:passkey:{op}"),
                route("/static/{name}", "urn:iki:gonk:asset:{name}"),
                route("/k", crate::k::K_IRI),
                // The review queue (ledger #444): the page, the section it swaps, and the
                // one write a human makes on it. `/queue/decide` takes no per-route `cap`
                // for the reason the browse rows do not — a route ceiling would REPLACE the
                // per-request capability, and with it the `Host` and cross-site checks that
                // make a POST safe on a machine with a browser open.
                route("/queue", crate::queue::QUEUE_IRI),
                route("/queue/rows", crate::queue::ROWS_IRI),
                route("/queue/decide", crate::queue::DECIDE_IRI),
                // Ledger #506: one batch decline, fanned out to the same Sink per member — no
                // per-route `cap`, for the same reason as `/queue/decide`.
                route(crate::batch::BATCH_PATH, crate::batch::BATCH_IRI),
                // The header's depth badge, polled every `queue::BADGE_EVERY`. It is a GET
                // of a fragment like `/queue/rows`, and it is the trigger's only liveness
                // signal — see `queue::Badge`.
                route("/queue/depth", crate::queue::BADGE_IRI),
                // The browse family's landing page. ⚠ It must be listed BEFORE the
                // `/browse/{p1}…` arities below — not for precedence (the patterns cannot
                // both match: this one has no segment after `/browse`) but because reading
                // the table top to bottom should meet the door before its depths.
                route("/browse", crate::k::ROOTS_IRI),
            ]
            .into_iter()
            .chain(browse_routes())
            .collect(),
        ),
        routes_only: false,
        principal_fn: Some(http_principal(door)),
        ..EdgeConfig::default()
    }
}

/// How many path segments a `/browse/` URL may carry — see `browse_routes` below.
pub const BROWSE_DEPTH: usize = 16;

/// `/browse/{iri}` → `urn:iki:gonk:page:browse:{iri}`, enumerated once per path DEPTH.
///
/// ⚠ **A route pattern's `{var}` captures exactly ONE path segment**, and the browse family's
/// file IRIs carry slashes (`urn:repo:ikigai-core:file:crates/ikigai-vocab/src/lib.rs`), so
/// a single route cannot match them and `ikigai-web`'s table has no trailing-segment
/// capture. The arity is therefore written out — `{p1}`, `{p1}/{p2}`, … — and the IRI
/// template rejoins the captures with `/`, which is lossless: an IRI has no spaces, so
/// unlike the `/k/` command (which is why [`crate::k`] carries its command as an argument)
/// it survives the trip through a decoded path.
///
/// Past [`BROWSE_DEPTH`] no route matches, the path takes the library's mechanical mapping
/// and the answer is a 404 — the bound REFUSES rather than serving a truncated name. Sixteen
/// against the four the deepest repository here needs; the ceiling is a report to the hub
/// (the library wants a trailing-segment capture), not a number worth tuning.
fn browse_routes() -> Vec<Route> {
    (1..=BROWSE_DEPTH)
        .map(|depth| {
            let captures = (1..=depth)
                .map(|n| format!("{{p{n}}}"))
                .collect::<Vec<_>>()
                .join("/");
            Route {
                pattern: format!("/browse/{captures}"),
                iri_template: crate::k::BROWSE_PAGE_TEMPLATE.replace("{start}", &captures),
                cap: None,
                cors: None,
                csp: None,
            }
        })
        .collect()
}
