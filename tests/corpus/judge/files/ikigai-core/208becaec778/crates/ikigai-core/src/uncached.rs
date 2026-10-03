//! **Why a read was not cached** (NetKernel News 2.47).
//!
//! Effective expiry propagates: a composite is no fresher than its most volatile
//! part, so joining one uncacheable source into an expensive cached composite makes
//! the composite uncacheable too — on every read, with identical types and every
//! test still green. That is how cms-web's books graph went from ~20 µs to ~1 s per
//! read (2026-08-13), and until this module nothing said so. The kernel now names
//! the cause on the computed event ([`UNCACHED_NOTE`](crate::UNCACHED_NOTE)) and
//! remembers the last few uncached resources, with that reason, for
//! `urn:kernel:uncached` — which needs no tracer installed, because the question
//! "why is this slow" arrives unannounced.
//!
//! The reason is **structured here and text at the edges**: the kernel compares
//! reasons structurally (an unchanged reason costs no string), and renders the one
//! grammar documented on [`UNCACHED_NOTE`](crate::UNCACHED_NOTE) for the trace and
//! the readout alike.

use std::collections::HashMap;
use std::fmt;

use crate::iri::Iri;
use crate::repr::Time;
use crate::request::RequestId;

/// How many sub-request names one reason carries before it counts the rest. A
/// fan-out over a thousand volatile names would otherwise make a thousand-name note;
/// the count says the list is not whole, so nothing reads as complete that is not.
pub(crate) const NAMED_DEPENDENCIES: usize = 8;

/// How many resources `urn:kernel:uncached` remembers. The least recently computed
/// is forgotten first.
pub(crate) const UNCACHED_LOG: usize = 64;

/// How a sub-request made its issuer uncacheable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DepKind {
    /// It answered, and its answer was `Always`.
    Volatile,
    /// It was refused on capability ([`Error::Denied`](crate::Error::Denied)): a
    /// grant change has no thread, so a result built on a refusal is never cached.
    Denied,
    /// It failed with any other error but a miss (`Unresolved` / `NotFound` hang a
    /// thread instead): the kernel cannot name what would make the failure go away.
    Failed,
}

impl DepKind {
    fn word(self) -> &'static str {
        match self {
            DepKind::Volatile => "dependency",
            DepKind::Denied => "denied",
            DepKind::Failed => "failed",
        }
    }
}

/// The sub-requests that made an invocation uncacheable, by the name the endpoint
/// requested, first seen first, each named once; past [`NAMED_DEPENDENCIES`] only
/// counted.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct VolatileDeps {
    names: Vec<(DepKind, Iri)>,
    more: usize,
}

impl VolatileDeps {
    pub(crate) fn note(&mut self, kind: DepKind, requested: &Iri) {
        if self
            .names
            .iter()
            .any(|(k, name)| *k == kind && name == requested)
        {
            return;
        }
        if self.names.len() < NAMED_DEPENDENCIES {
            self.names.push((kind, requested.clone()));
        } else {
            self.more += 1;
        }
    }

    fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// Why a computed, cacheable-verb result was not stored though its expiry allowed
/// it — the store's side of the question, after the expiry's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Declined {
    /// An `At` deadline on a kernel with no clock: nothing could ever tell when it
    /// expired.
    NoClock,
    /// An `At` deadline already past on the kernel's clock when the result came back.
    Expired(Time),
    /// A thread the result depends on was cut while it was computed, so it was
    /// already stale.
    CutInFlight,
    /// The cache policy did not admit it.
    Policy,
}

/// Why one computed result was not cached. Every field that is set is a cause; the
/// rendering lists them in a fixed order (see [`UNCACHED_NOTE`](crate::UNCACHED_NOTE)).
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Uncached {
    /// The endpoint's own answer was `Always` — it declared no caching.
    pub(crate) declared: bool,
    /// Sub-requests that were `Always`, refused or failed.
    pub(crate) deps: VolatileDeps,
    /// The piped input the stage folded in was `Always`.
    pub(crate) upstream: bool,
    /// The expiry allowed storing and the store did not.
    pub(crate) declined: Option<Declined>,
}

impl Uncached {
    /// Whether nothing was found to blame — never true of a result the kernel
    /// reports, and the guard that keeps an empty note off an event.
    pub(crate) fn is_empty(&self) -> bool {
        !self.declared && self.deps.is_empty() && !self.upstream && self.declined.is_none()
    }
}

impl fmt::Display for Uncached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut clauses: Vec<String> = Vec::new();
        if self.declared {
            clauses.push("declared".to_string());
        }
        for kind in [DepKind::Volatile, DepKind::Denied, DepKind::Failed] {
            let names: Vec<&str> = self
                .deps
                .names
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, name)| name.as_str())
                .collect();
            if !names.is_empty() {
                clauses.push(format!("{} {}", kind.word(), names.join(" ")));
            }
        }
        if self.deps.more > 0 {
            clauses.push(format!("+{} more", self.deps.more));
        }
        if self.upstream {
            clauses.push("upstream".to_string());
        }
        match self.declined {
            None => {}
            Some(Declined::NoClock) => clauses.push("no-clock".to_string()),
            Some(Declined::Expired(at)) => clauses.push(format!("expired {}", at.as_millis())),
            Some(Declined::CutInFlight) => clauses.push("cut-in-flight".to_string()),
            Some(Declined::Policy) => clauses.push("policy".to_string()),
        }
        f.write_str(&clauses.join("; "))
    }
}

/// One remembered resource: the last reason it was not cached, how many times it
/// has been computed uncached since it entered the log, and when last.
struct Row {
    target: String,
    scope: String,
    reason: Uncached,
    count: u64,
    tick: u64,
}

/// The last [`UNCACHED_LOG`] resources computed and not cached, keyed by request
/// identity and chain — the memory behind `urn:kernel:uncached`.
#[derive(Default)]
pub(crate) struct UncachedLog {
    rows: HashMap<(RequestId, u64), Row>,
    tick: u64,
}

impl UncachedLog {
    /// Record one uncached computation. `scope` renders the chain only when the row
    /// is new, so a repeat costs a hash lookup and a structural compare.
    pub(crate) fn record(
        &mut self,
        id: RequestId,
        scope_fingerprint: u64,
        target: &str,
        scope: impl FnOnce() -> String,
        reason: Uncached,
    ) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(row) = self.rows.get_mut(&(id, scope_fingerprint)) {
            if row.reason != reason {
                row.reason = reason;
            }
            row.count += 1;
            row.tick = tick;
            return;
        }
        if self.rows.len() >= UNCACHED_LOG {
            if let Some(oldest) = self
                .rows
                .iter()
                .min_by_key(|(_, row)| row.tick)
                .map(|(key, _)| *key)
            {
                self.rows.remove(&oldest);
            }
        }
        self.rows.insert(
            (id, scope_fingerprint),
            Row {
                target: target.to_string(),
                scope: scope(),
                reason,
                count: 1,
                tick,
            },
        );
    }

    /// The readout: a header, then one line per remembered resource, most recently
    /// computed first — its target, how many times it was computed uncached, the
    /// reason, and the chain it was computed in, bracketed last (a chain and a reason
    /// both contain spaces; the brackets keep them apart).
    pub(crate) fn render(&self) -> String {
        let mut rows: Vec<&Row> = self.rows.values().collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.tick));
        let mut body = format!(
            "uncached (why a computed read was not cached; the last {UNCACHED_LOG}, most recent first)\n"
        );
        if rows.is_empty() {
            body.push_str("  (nothing computed uncached)\n");
            return body;
        }
        let width = rows
            .iter()
            .map(|row| row.target.chars().count())
            .max()
            .unwrap_or(0)
            .min(48);
        for row in rows {
            let (target, count, scope, reason) = (&row.target, row.count, &row.scope, &row.reason);
            let count = format!("×{count}");
            body.push_str(&format!(
                "  {target:<width$}  {count:>6}  {reason}  [{scope}]\n"
            ));
        }
        body
    }
}
