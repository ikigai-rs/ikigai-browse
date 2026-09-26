//! The three doors onto the one kernel, and what each hands a caller.
//!
//! | door | transport | who can reach it | capability |
//! |---|---|---|---|
//! | HTTP | `ikigai-web`, loopback TCP | any local process | [`http_cap`]: the configured ledgers' narrow read+write tokens, and nothing for a non-loopback peer |
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
//! So there is one kernel, the **hub** ([`crate::compose`]), shared as `Arc<Kernel>`. The
//! HTTP door takes it directly. The socket and QUIC doors each get a [`door_kernel`] whose
//! only space is a [`HubSpace`] forwarding every request to the hub, under a cache policy
//! that admits nothing ([`NoCache`]). Every read and every write passes through the hub's
//! cache and the hub's threads, whichever door it came in by.
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
    Bindings, CachePolicy, Capability, Description, Endpoint, EntryFacts, Invocation, Kernel,
    Representation, Request, Resolution, Resolved, Result, Scope, Space, SpaceEntry, SystemClock,
};
use ikigai_vocab::TurtleRenderer;
use ikigai_web::{CapFn, EdgeConfig, HttpRequest, RouteTable};

/// A space whose every binding is the hub's: it resolves what the hub binds, describes it
/// with the hub's own description, enumerates the hub's catalog, and forwards invocation to
/// the hub under the caller's capability.
pub struct HubSpace {
    hub: Arc<Kernel>,
}

impl HubSpace {
    /// Forward to `hub`.
    pub fn new(hub: Arc<Kernel>) -> Self {
        HubSpace { hub }
    }
}

impl Space for HubSpace {
    fn resolve(&self, request: &Request, _scope: &Scope) -> Resolution {
        // A miss when the hub binds nothing here, so the door reports the kernel's own
        // "no endpoint" rather than a forwarding endpoint that fails on invoke.
        match self.hub.describe(&request.target) {
            Some(description) => Resolution::Hit(Resolved {
                endpoint: Arc::new(Forward {
                    hub: Arc::clone(&self.hub),
                    description,
                }),
                bindings: Bindings::new(),
                // Nothing is rewritten here: the hub canonicalizes, caches and cuts under
                // its own names.
                canonical: None,
            }),
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

/// Whether `ip` is loopback, counting an IPv4-mapped IPv6 loopback (`::ffff:127.0.0.1`) —
/// the form a dual-stack listener reports an IPv4 client in.
pub fn is_loopback(ip: IpAddr) -> bool {
    ip.to_canonical().is_loopback()
}

/// The HTTP door's capability: a function of the REQUEST, not a constant.
///
/// Today the only identity the door can verify is where the connection came from, so that
/// is what it keys on: a loopback peer gets `grants`, anything else gets an empty
/// capability. The bind is refused beyond loopback before the server starts
/// ([`crate::config::refuse_non_loopback`]); this is the second, per-request half, so a
/// listener that somehow does face a network hands a remote peer nothing.
///
/// ★ It is shaped this way so an authenticated identity can replace the peer check without
/// moving the seam: a passkey session resolved from the request maps to a grant exactly the
/// way a certificate fingerprint does on the QUIC door.
pub fn http_cap(grants: Vec<String>) -> CapFn {
    Arc::new(move |request: &HttpRequest| match request.peer {
        Some(peer) if is_loopback(peer) => Capability::scoped(grants.clone()),
        _ => Capability::scoped(Vec::<String>::new()),
    })
}

/// The HTTP door's edge policy: `ikigai-web`'s strict defaults, and an explicit route table.
///
/// The table is empty, so every path takes the mechanical mapping
/// (`POST /iki/ledger/append` → `Sink urn:iki:ledger:append`). Pages, fragments and a SPARQL
/// route are rows added here — routed onto the ledger and the store's narrow graph doors —
/// not a second router.
pub fn edge_config() -> EdgeConfig {
    EdgeConfig {
        routes: RouteTable::new(Vec::new()),
        routes_only: false,
        ..EdgeConfig::default()
    }
}
