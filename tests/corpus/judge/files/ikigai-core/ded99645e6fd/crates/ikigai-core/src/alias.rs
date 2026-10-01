//! **Logical rewrite** — a stable logical URI that resolves to a different backing
//! resource, without the caller knowing.
//!
//! `urn:log:config` → `file:/logConfig.yaml` (an *exact* alias: one name, one
//! backing thing). `urn:fn:` → `urn:iki:fn:` (a *prefix* alias: a whole namespace
//! moves, and every name under it follows). Same mechanism; exact is the
//! degenerate prefix.
//!
//! ## Why it exists
//!
//! An audit found 98 unregistered `urn:` namespace identifiers across the
//! ecosystem, with an IANA-registered one (`urn:isbn:`) already arriving in the
//! same graph from Zotero. Consolidating under a single `urn:iki:` namespace
//! without this primitive is a flag day: nine repos, seventeen published crates,
//! two live machines and a deployed browser demo all changing identity at once.
//! With it, old names keep resolving through a transition window and each repo
//! moves at its own pace.
//!
//! ## Shape
//!
//! [`Alias`] is a `Space` decorator in the **interception-overlay family** — the
//! same shape as `ikigai-throttle`'s `RateLimit`/`Timeout`: wrap a space, add a
//! cross-cutting behavior to every resolution flowing through it, leave the
//! wrapped space unaware. It is deliberately *not* a second composition
//! mechanism.
//!
//! Hosts that resolve against a bare `Space` wrap it directly. Hosts running a
//! [`Kernel`](crate::Kernel) install the table with
//! [`Kernel::with_aliases`](crate::Kernel::with_aliases), which both wraps the
//! root **and** teaches the kernel the table — the kernel wants it in its own
//! hands for the readout at `urn:kernel:aliases`, for refusing a cyclic table
//! before dispatch, and for attributing a miss to the rule that moved the name.
//!
//! It is no longer needed for *identity*. This overlay reports every rewrite it
//! performs on [`Resolved::canonical`](crate::Resolved::canonical), and the kernel
//! adopts the reported name before it computes the cache key or fires the
//! golden-thread cut. So an `Alias` composed by hand under another overlay —
//! `RateLimit::new(Alias::new(table, space))` — shares one cache entry and one
//! thread across both names, which it did not before 0.1.64.
//!
//! ★ **`urn:fn:` → `urn:iki:fn:` is the one real migration, and it stays.** Every
//! other namespace in this crate's examples and tests is fictional and lives under
//! the IANA-reserved `urn:example:` (RFC 6963), because `grep -r urn:iki:` is how
//! the ecosystem answers "what has actually moved?" — a fixture naming a namespace
//! that does not exist makes that grep lie, and a fixture naming the migrated form
//! of `urn:kernel:` would one day be **ambiguous with production usage** rather
//! than merely fictional. Teaching the alias with
//! the thing it was actually built for is worth more than grep purity, so do not
//! "finish the job" by moving this pair too. `tests/namespaces.rs` enforces the
//! split.
//!
//! ```
//! use std::sync::Arc;
//! use ikigai_core::{AliasTable, Iri};
//!
//! let table = AliasTable::new().prefix("urn:fn:", "urn:iki:fn:");
//! let hop = table.canonicalize(&Iri::parse("urn:fn:toUpper").unwrap());
//! assert_eq!(hop.canonical().unwrap().as_str(), "urn:iki:fn:toUpper");
//! # let _ = Arc::new(table);
//! ```
//!
//! ## ★ The five decisions, and why
//!
//! A rewrite primitive goes wrong in five specific places. None of them is
//! answerable by "whatever falls out of the implementation", so each is forced
//! here and enforced by a test.
//!
//! **1. Rewrite happens BEFORE the capability check.** Authority is *also* named
//! by URN (`urn:cap:fs:read:…`), so the ordering is a security decision, not a
//! plumbing one. The kernel canonicalizes the target first and evaluates the
//! declared-capability floor against the **backing** resource — the thing that
//! will actually be touched. The other order is an escalation device: check
//! `urn:public:thing`, then rewrite to `urn:secret:thing`, and the grant that was
//! examined is not the grant that was needed. A *silent grant* is strictly worse
//! than a silent denial, so the order is fixed in the safe direction.
//!
//! The cost of that choice is the failure the ecosystem has already paid for
//! twice this month: a caller holding a grant that names the **pre-alias** scope
//! now fails a check against the post-alias target, by simply not holding it,
//! with nothing in any log. So this module refuses to let that be silent — see
//! *Observability* below. Note what is deliberately **not** done: the table is
//! never applied to capability scopes. Rewriting authority to follow a name is
//! how you manufacture the silent grant; migrating grants is an explicit act by
//! whoever mints them.
//!
//! **2. `Meta` reports the BACKING name.** Self-description that lies breaks the
//! catalog, selection and the MCP projection downstream. The canonical target is
//! adopted before dispatch, so the description an endpoint returns — and the
//! `Description::id` the catalog and `urn:kernel:actions` key on — is the
//! backing one, everywhere, with no special case. The alias is disclosed as
//! *provenance* (a trace note), never by editing the description.
//!
//! **3. The two names SHARE a cache entry and a golden thread.** They must: a
//! `Sink` through one name that left the other serving a stale representation is
//! the invalidation bug that is hardest to see, because both answers look
//! plausible. The representation cache keys on [`Request::id`](crate::Request::id)
//! and the auto-cut on `request.target`, so identity is settled at one point on
//! the resolution path — for the cache key, the auto-cut, the trace event and the
//! `Invocation` the endpoint sees alike.
//!
//! It is settled by the **resolver reporting what it rewrote**, not by the kernel
//! knowing the table in advance. This overlay stamps
//! [`Resolved::canonical`](crate::Resolved::canonical); the kernel adopts it
//! before the id is computed. The earlier design — canonicalize at the top from a
//! table the kernel holds — is still there (it is what refuses a cycle *before*
//! dispatch and what attributes a miss), but it was the *only* mechanism, and it
//! only ever fired for a table installed through
//! [`Kernel::with_aliases`](crate::Kernel::with_aliases). Composed by hand under
//! another overlay, the rewrite happened and the canonicalization did not: two
//! cache entries and two golden threads over one resource, with
//! `(no rewrite table installed)` in a readout nobody reads as the only signal.
//! Reporting closes that, because the report travels with the resolution however
//! the space was assembled.
//!
//! **4. The catalog lists the BACKING name, once.** [`Alias::entries`] is
//! transparent — it forwards the wrapped space's entries unchanged. Listing both
//! names over-offers: two IRIs for one resource, an agent picking arbitrarily,
//! and selection dedup (which keys on description id) quietly collapsing them
//! anyway. Listing the alias *instead* would hide the name the system is moving
//! to. The migration property this buys is the one that matters: during the
//! transition the catalog advertises only the **new** names, so every consumer
//! that discovers resources by reading the catalog migrates forward on its own,
//! while every consumer still holding an old name keeps working. Aliases are not
//! hidden — they are a resource of their own, at `urn:kernel:aliases`.
//!
//! **5. Chains resolve; cycles are refused, loudly.** `A→B→C` is followed
//! transitively up to [`AliasTable::max_hops`] (default
//! [`DEFAULT_MAX_HOPS`]) — a namespace migration layered on an earlier one is a
//! real shape, and refusing it would force operators to pre-compose their tables
//! by hand. `A→B, B→A` is detected by a visited set and **refused** with an error
//! naming the trail; it is never silently truncated to a partial rewrite, which
//! would resolve to whichever name the loop happened to stop on. Over-long and
//! malformed rewrites take the same refusal path, for the same reason: a
//! half-applied rewrite is a wrong answer that looks like a right one.
//!
//! ## Observability
//!
//! A rewrite the operator cannot see is a bad afternoon waiting to happen
//! ("it resolves under the old name but not the new one"). Three channels, in
//! increasing order of how much has to be switched on:
//!
//! - **Always on** — per-rule counters (`hops`, `unresolved`, `refused`),
//!   readable at `urn:kernel:aliases` beside the rules themselves. A rule that
//!   fired and then missed says exactly that, with no tracer installed.
//! - **When a tracer is installed** — every [`TraceEvent`](crate::TraceEvent) for
//!   an aliased request carries a note under
//!   [`ALIAS_NOTE`](crate::ALIAS_NOTE) reading `logical -> canonical`, including
//!   the denial event (which names something that never ran) and the miss.
//! - **In the error text** — a capability denial on an aliased target names the
//!   hop, and adds an explicit hint when the caller's capability holds a scope
//!   mentioning the pre-alias prefix. That is the exact silent-failure shape from
//!   decision 1, turned into a sentence.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::describe::Description;
use crate::endpoint::{Endpoint, FnEndpoint};
use crate::error::Error;
use crate::grammar::Bindings;
use crate::iri::Iri;
use crate::request::Request;
use crate::space::{answered, Resolution, Resolved, Scope, Space, SpaceEntry};
use crate::topology::{SpaceKind, Topology, TopologyRule};
use crate::verb::Verb;

/// How many rewrites one canonicalization will follow before refusing. Chains are
/// legitimate (a namespace migrated twice); an unbounded walk is not.
pub const DEFAULT_MAX_HOPS: usize = 8;

/// The reserved kernel-behavior namespace. Targets under it are **never**
/// aliased: the kernel resolves `urn:kernel:*` itself, ahead of the root space,
/// precisely so nothing can shadow it — and a rewrite that could re-point
/// `urn:kernel:cut` would be exactly that shadowing, arriving through a different
/// door. Aliasing *to* a kernel resource is fine; aliasing one *away* is not.
const KERNEL_NS: &str = "urn:kernel:";

/// How a rule matches: the whole name, or a leading namespace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RuleKind {
    /// The target must equal `from` exactly. `urn:log:config` → `file:/logConfig.yaml`.
    Exact,
    /// The target must start with `from`; the remainder is carried over.
    /// `urn:fn:` → `urn:iki:fn:` rewrites `urn:fn:toUpper` to `urn:iki:fn:toUpper`.
    Prefix,
}

impl RuleKind {
    /// The keyword this kind is written with in the table's text form.
    pub fn keyword(&self) -> &'static str {
        match self {
            RuleKind::Exact => "exact",
            RuleKind::Prefix => "prefix",
        }
    }
}

/// One rewrite rule, with the live counters that make it observable.
#[derive(Debug)]
pub struct AliasRule {
    kind: RuleKind,
    from: String,
    to: String,
    hops: AtomicU64,
    unresolved: AtomicU64,
    refused: AtomicU64,
}

impl AliasRule {
    /// How this rule matches.
    pub fn kind(&self) -> RuleKind {
        self.kind
    }

    /// The logical name (or namespace) being rewritten.
    pub fn from(&self) -> &str {
        &self.from
    }

    /// The backing name (or namespace) it is rewritten to.
    pub fn to(&self) -> &str {
        &self.to
    }

    /// How many rewrites this rule has performed.
    pub fn hops(&self) -> u64 {
        self.hops.load(Ordering::Relaxed)
    }

    /// How many of those rewrites landed on a target nothing was bound to. The
    /// always-on answer to "it resolves under the old name but not the new one":
    /// a nonzero count here names the rule that moved it.
    pub fn unresolved(&self) -> u64 {
        self.unresolved.load(Ordering::Relaxed)
    }

    /// How many canonicalizations this rule participated in that were refused (a
    /// cycle, an over-long chain, or a substitution that is not a valid IRI).
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    /// Apply this rule to `target`, or `None` if it does not match.
    fn apply(&self, target: &str) -> Option<String> {
        match self.kind {
            RuleKind::Exact => (target == self.from).then(|| self.to.clone()),
            RuleKind::Prefix => target
                .strip_prefix(self.from.as_str())
                .map(|rest| format!("{}{rest}", self.to)),
        }
    }
}

/// A completed rewrite: the name the caller used, the name it resolves to, and
/// the rules it traveled through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AliasHop {
    logical: Iri,
    canonical: Iri,
    rules: Vec<usize>,
}

impl AliasHop {
    /// A rewrite a `Space` performed and **reported** on its
    /// [`Resolved::canonical`](crate::Resolved::canonical), rather than one this
    /// table walked. It carries no rules — nothing in this table did it, so there
    /// is nothing to attribute a counter to — and exists so the trace note, the
    /// denial message and the miss path read the same whichever half of the system
    /// did the rewriting.
    pub fn reported(logical: Iri, canonical: Iri) -> Self {
        AliasHop {
            logical,
            canonical,
            rules: Vec::new(),
        }
    }

    /// The name as the caller wrote it.
    pub fn logical(&self) -> &Iri {
        &self.logical
    }

    /// The backing name it resolves to — the kernel's identity for this request.
    pub fn canonical(&self) -> &Iri {
        &self.canonical
    }

    /// The indices (into [`AliasTable::rules`]) of the rules applied, in order.
    pub fn rules(&self) -> &[usize] {
        &self.rules
    }
}

impl fmt::Display for AliasHop {
    /// `urn:fn:toUpper -> urn:iki:fn:toUpper` — the form carried in a trace note
    /// and in a denial message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -> {}", self.logical, self.canonical)
    }
}

/// A rewrite that could not be completed, and is therefore **refused** rather
/// than half-applied. A partial rewrite resolves to whichever name the walk
/// happened to stop on — a wrong answer wearing a right answer's face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AliasRefusal {
    /// The name the caller used.
    pub logical: Iri,
    /// Why the walk stopped: a cycle, the hop limit, or a malformed substitution.
    pub reason: String,
    /// Every name visited, in order — enough to read the loop off the message.
    pub trail: Vec<String>,
    /// The rules that participated, for counter attribution.
    rules: Vec<usize>,
}

impl fmt::Display for AliasRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.reason, self.trail.join(" -> "))
    }
}

/// The outcome of canonicalizing one target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Canonical {
    /// No rule matched — resolve under the name as given. The overwhelmingly
    /// common case, and the one that must cost nothing.
    Direct,
    /// Rewritten, terminating.
    Aliased(AliasHop),
    /// Deliberately refused; see [`AliasRefusal`].
    Refused(AliasRefusal),
}

impl Canonical {
    /// The name to resolve under, if the rewrite completed. `None` for
    /// [`Direct`](Canonical::Direct) (nothing changed) and for
    /// [`Refused`](Canonical::Refused) (nothing may be resolved).
    pub fn canonical(&self) -> Option<&Iri> {
        match self {
            Canonical::Aliased(hop) => Some(hop.canonical()),
            _ => None,
        }
    }

    /// The hop, if one was taken.
    pub fn hop(&self) -> Option<&AliasHop> {
        match self {
            Canonical::Aliased(hop) => Some(hop),
            _ => None,
        }
    }
}

/// An error parsing an alias table's text form.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AliasParseError {
    /// The 1-based line number.
    pub line: usize,
    /// What was wrong with it.
    pub detail: String,
}

impl fmt::Display for AliasParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "alias table line {}: {}", self.line, self.detail)
    }
}

impl std::error::Error for AliasParseError {}

/// The rewrite table: an ordered set of rules plus the hop budget.
///
/// **The table is a resource, not a hardcode.** It round-trips through a
/// line-oriented text form ([`parse`](AliasTable::parse) /
/// [`to_text`](AliasTable::to_text)) so a host can source it from a bound IRI
/// like anything else, and a kernel serves its live contents — counters and all —
/// at `urn:kernel:aliases`.
///
/// ```
/// use ikigai_core::AliasTable;
/// let table = AliasTable::parse(
///     "# the urn:iki: migration\nprefix urn:fn: urn:iki:fn:\nexact urn:log:config file:/logConfig.yaml\n",
/// )
/// .unwrap();
/// assert_eq!(table.rules().len(), 2);
/// ```
#[derive(Debug)]
pub struct AliasTable {
    rules: Vec<AliasRule>,
    max_hops: usize,
}

impl Default for AliasTable {
    fn default() -> Self {
        AliasTable::new()
    }
}

impl AliasTable {
    /// An empty table.
    pub fn new() -> Self {
        AliasTable {
            rules: Vec::new(),
            max_hops: DEFAULT_MAX_HOPS,
        }
    }

    /// Rewrite the single name `from` to `to` (builder).
    pub fn exact(self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.rule(RuleKind::Exact, from, to)
    }

    /// Rewrite every name under the namespace `from` to the same name under `to`
    /// (builder) — the case a namespace migration needs.
    pub fn prefix(self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.rule(RuleKind::Prefix, from, to)
    }

    fn rule(mut self, kind: RuleKind, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.rules.push(AliasRule {
            kind,
            from: from.into(),
            to: to.into(),
            hops: AtomicU64::new(0),
            unresolved: AtomicU64::new(0),
            refused: AtomicU64::new(0),
        });
        // Most specific first: the longest `from` wins, and at equal length an
        // exact rule outranks a prefix one. So a table carrying both
        // `prefix urn:fn:` and `exact urn:fn:toUpper` sends `toUpper` to its own
        // destination and everything else under the namespace rule, without the
        // author having to think about declaration order.
        self.rules.sort_by(|a, b| {
            b.from
                .len()
                .cmp(&a.from.len())
                .then_with(|| a.kind.keyword().cmp(b.kind.keyword()))
                .then_with(|| a.from.cmp(&b.from))
        });
        self
    }

    /// Set the hop budget for a chain (builder). `0` is clamped to `1`: a table
    /// with rules that never applies any of them is a configuration that lies.
    pub fn with_max_hops(mut self, hops: usize) -> Self {
        self.max_hops = hops.max(1);
        self
    }

    /// The hop budget.
    pub fn max_hops(&self) -> usize {
        self.max_hops
    }

    /// The rules, most specific first.
    pub fn rules(&self) -> &[AliasRule] {
        &self.rules
    }

    /// Whether the table holds no rules.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Resolve `target` to the name it should be resolved under, following chains
    /// and refusing cycles.
    ///
    /// Not a pure function: it bumps the per-rule counters that make a rewrite
    /// observable without a tracer. That is deliberate — the counters have to be
    /// on the path everything takes, or they only count the paths someone
    /// remembered to instrument.
    pub fn canonicalize(&self, target: &Iri) -> Canonical {
        // The reserved namespace is not aliasable; see `KERNEL_NS`.
        if target.as_str().starts_with(KERNEL_NS) {
            return Canonical::Direct;
        }
        let mut current = target.clone();
        let mut trail = vec![target.as_str().to_string()];
        let mut applied: Vec<usize> = Vec::new();
        for _ in 0..self.max_hops {
            let Some((index, next)) = self.step(current.as_str()) else {
                // No rule matches: the walk has terminated.
                return if applied.is_empty() {
                    Canonical::Direct
                } else {
                    for &index in &applied {
                        self.rules[index].hops.fetch_add(1, Ordering::Relaxed);
                    }
                    Canonical::Aliased(AliasHop {
                        logical: target.clone(),
                        canonical: current,
                        rules: applied,
                    })
                };
            };
            applied.push(index);
            let Ok(next_iri) = Iri::parse(next.clone()) else {
                return self.refuse(
                    target,
                    format!("rewrite produced `{next}`, which is not a valid IRI"),
                    trail,
                    applied,
                );
            };
            if trail.iter().any(|seen| seen == next_iri.as_str()) {
                trail.push(next_iri.as_str().to_string());
                return self.refuse(target, "alias cycle".to_string(), trail, applied);
            }
            trail.push(next_iri.as_str().to_string());
            current = next_iri;
        }
        self.refuse(
            target,
            format!("alias chain exceeded {} hops", self.max_hops),
            trail,
            applied,
        )
    }

    /// The most specific rule matching `target`, applied once.
    fn step(&self, target: &str) -> Option<(usize, String)> {
        self.rules
            .iter()
            .enumerate()
            .find_map(|(index, rule)| rule.apply(target).map(|next| (index, next)))
    }

    fn refuse(
        &self,
        logical: &Iri,
        reason: String,
        trail: Vec<String>,
        rules: Vec<usize>,
    ) -> Canonical {
        for &index in &rules {
            self.rules[index].refused.fetch_add(1, Ordering::Relaxed);
        }
        Canonical::Refused(AliasRefusal {
            logical: logical.clone(),
            reason,
            trail,
            rules,
        })
    }

    /// Record that a rewritten target resolved to nothing. Called by the kernel on
    /// [`crate::Error::Unresolved`] so the miss is attributed to
    /// the rule that moved the name.
    pub fn record_unresolved(&self, hop: &AliasHop) {
        for &index in &hop.rules {
            self.rules[index].unresolved.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Whether any granted scope mentions a rewritten namespace — the hint that
    /// turns decision 1's silent denial into a sentence. A capability naming
    /// `urn:cap:fs:read:urn:example:fs:ws/x` while `urn:example:fs:` is being migrated is almost
    /// certainly a grant that was not migrated with the resource.
    ///
    /// Deliberately a *hint*, not a decision: nothing here widens or rewrites
    /// authority. It only reports a coincidence worth looking at, and the caller
    /// labels it as such.
    pub fn scopes_naming_stale_namespaces<'a, I>(&self, scopes: I) -> Vec<String>
    where
        I: IntoIterator<Item = &'a String>,
    {
        let mut hits = Vec::new();
        for scope in scopes {
            if self
                .rules
                .iter()
                .any(|rule| scope.contains(rule.from.as_str()))
            {
                hits.push(scope.clone());
            }
        }
        hits
    }

    /// Parse the line-oriented text form:
    ///
    /// ```text
    /// # the urn:iki: migration
    /// prefix  urn:fn:         urn:iki:fn:
    /// exact   urn:log:config  file:/logConfig.yaml
    /// max-hops 8
    /// ```
    ///
    /// Blank lines and `#` comments are ignored. Whitespace-separated, so neither
    /// field may contain a space — IRIs may not either, so nothing is lost.
    pub fn parse(text: &str) -> Result<AliasTable, AliasParseError> {
        let mut table = AliasTable::new();
        for (offset, raw) in text.lines().enumerate() {
            let line = offset + 1;
            let content = raw.split('#').next().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let fields: Vec<&str> = content.split_whitespace().collect();
            match fields.as_slice() {
                ["max-hops", hops] => {
                    let hops = hops.parse::<usize>().map_err(|_| AliasParseError {
                        line,
                        detail: format!("`{hops}` is not a hop count"),
                    })?;
                    table = table.with_max_hops(hops);
                }
                [kind @ ("exact" | "prefix"), from, to] => {
                    // Both sides must be IRIs (a prefix is one too: `urn:fn:`
                    // parses). Validating here means a malformed table fails when
                    // it is read, not on the one request that happens to hit the
                    // bad rule.
                    for (label, value) in [("from", from), ("to", to)] {
                        Iri::parse(*value).map_err(|e| AliasParseError {
                            line,
                            detail: format!("{label} `{value}`: {e}"),
                        })?;
                    }
                    table = if *kind == "exact" {
                        table.exact(*from, *to)
                    } else {
                        table.prefix(*from, *to)
                    };
                }
                _ => {
                    return Err(AliasParseError {
                        line,
                        detail: format!(
                            "expected `exact <from> <to>`, `prefix <from> <to>` or \
                             `max-hops <n>`, got `{content}`"
                        ),
                    })
                }
            }
        }
        Ok(table)
    }

    /// Render the table back to its text form — round-trips through
    /// [`parse`](AliasTable::parse). Counters are *not* rendered: this is the
    /// table's definition, not its readout (that is `urn:kernel:aliases`).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if self.max_hops != DEFAULT_MAX_HOPS {
            out.push_str(&format!("max-hops {}\n", self.max_hops));
        }
        for rule in &self.rules {
            out.push_str(&format!(
                "{} {} {}\n",
                rule.kind.keyword(),
                rule.from,
                rule.to
            ));
        }
        out
    }
}

/// A `Space` decorator that rewrites logical names to backing ones before
/// delegating — the interception-overlay form of [`AliasTable`].
///
/// Composing it by hand is fine, including underneath another overlay: the
/// rewrite is reported on [`Resolved::canonical`](crate::Resolved::canonical) and
/// the kernel adopts it, so the logical and backing names share a cache entry and
/// a golden thread either way (decision 3). What
/// [`Kernel::with_aliases`](crate::Kernel::with_aliases) adds on top is the
/// operator's half: the live readout at `urn:kernel:aliases`, refusal of a cyclic
/// or over-long chain *before* dispatch rather than at invoke, and per-rule
/// attribution of a rewrite that landed on nothing.
///
/// One thing only the kernel-held table can do: rewrite a name *into* the reserved
/// `urn:kernel:` namespace. The kernel dispatches its own builtins ahead of the
/// root space, so a rewrite performed inside the root space arrives too late — the
/// backing name is looked for in the space, where no builtin is bound, and misses.
/// Aliasing one *away* is refused outright, by both routes.
///
/// Resolving an already-canonical target through this space is a no-op: the rules
/// do not match their own destinations in a well-formed table, so the kernel's
/// canonicalize-then-resolve is idempotent and costs one failed scan.
pub struct Alias {
    table: Arc<AliasTable>,
    inner: Arc<dyn Space>,
    id: Option<Iri>,
}

impl Alias {
    /// Wrap `inner`, rewriting through `table`.
    pub fn new(table: Arc<AliasTable>, inner: Arc<dyn Space>) -> Self {
        Alias {
            table,
            inner,
            id: None,
        }
    }

    /// Claim an identity for this overlay (builder) — a claim, same name ⇒ same
    /// doors; see [`EndpointSpace::named`](crate::EndpointSpace::named).
    pub fn named(mut self, id: Iri) -> Self {
        self.id = Some(id);
        self
    }

    /// The table this overlay rewrites through.
    pub fn table(&self) -> &Arc<AliasTable> {
        &self.table
    }
}

impl Space for Alias {
    fn resolve(&self, request: &Request, scope: &Scope) -> Resolution {
        // The innermost named space answered; this overlay only fills an absence.
        answered(self.resolve_through(request, scope), &self.id)
    }

    fn entries(&self) -> Option<Vec<SpaceEntry>> {
        // Decision 4: transparent to enumeration. The catalog and the action
        // manifold see the BACKING names, once each; the aliases are their own
        // resource at `urn:kernel:aliases`.
        self.inner.entries()
    }

    fn id(&self) -> Option<Iri> {
        self.id.clone()
    }

    /// The table is data, so the node carries it: one `ik:RewriteRule` per rule,
    /// in table order — the preimage a reachability query needs to see.
    fn topology(&self) -> Topology {
        Topology::new(SpaceKind::Alias {
            rules: self
                .table
                .rules()
                .iter()
                .map(|rule| TopologyRule::new(rule.kind(), rule.from(), rule.to()))
                .collect(),
        })
        .with_id(self.id.clone())
        .child(self.inner.topology())
    }
}

impl Alias {
    fn resolve_through(&self, request: &Request, scope: &Scope) -> Resolution {
        match self.table.canonicalize(&request.target) {
            Canonical::Direct => self.inner.resolve(request, scope),
            Canonical::Aliased(hop) => {
                let mut rewritten = request.clone();
                rewritten.target = hop.canonical().clone();
                // ★ REPORT THE REWRITE. This is what makes decision 3 hold for an
                // `Alias` the kernel did not install itself: the canonical rides
                // back on the `Resolved`, and the kernel adopts it before it
                // computes the cache key or fires the auto-cut. A space nested
                // deeper that rewrote further has already named the resource
                // actually reached, so `with_canonical` keeps that one.
                match self.inner.resolve(&rewritten, scope) {
                    Resolution::Hit(hit) => {
                        Resolution::Hit(hit.with_canonical(hop.canonical().clone()))
                    }
                    Resolution::Miss => Resolution::Miss,
                }
            }
            // A `Space` cannot return an error, and a refusal must not read as a
            // miss (a miss says "nothing is bound there", which is a different
            // and misleading fact). So it resolves to an endpoint that fails on
            // invoke with the trail — the same idiom `RateLimit` uses for an
            // over-budget request. A kernel refuses earlier, before dispatch.
            // No canonical is reported: a refusal resolved to nothing, and a
            // half-applied rewrite is exactly what decision 5 forbids.
            Canonical::Refused(refusal) => Resolution::Hit(Resolved::new(
                refused_endpoint(&refusal),
                Bindings::default(),
            )),
        }
    }
}

/// The endpoint a refused rewrite resolves to: it errors on invoke with the trail.
fn refused_endpoint(refusal: &AliasRefusal) -> Arc<dyn Endpoint> {
    let message = format!("alias: {refusal}");
    let summary = message.clone();
    Arc::new(
        FnEndpoint::new("alias-refused", move |_inv| {
            Err(Error::Endpoint(message.clone()))
        })
        .with_description(
            Description::new("alias-refused")
                .title("Alias rewrite refused")
                .summary(summary)
                .verb(Verb::Source)
                .verb(Verb::Meta)
                .output("text/plain;charset=utf-8"),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins;
    use crate::grammar::Exact;
    use crate::space::EndpointSpace;

    fn iri(s: &str) -> Iri {
        Iri::parse(s).unwrap()
    }

    #[test]
    fn a_prefix_rule_carries_the_remainder_over() {
        let table = AliasTable::new().prefix("urn:fn:", "urn:iki:fn:");
        let canonical = table.canonicalize(&iri("urn:fn:toUpper"));
        assert_eq!(
            canonical.canonical().map(Iri::as_str),
            Some("urn:iki:fn:toUpper")
        );
        assert_eq!(table.rules()[0].hops(), 1);
    }

    #[test]
    fn an_exact_rule_rewrites_only_the_whole_name() {
        let table = AliasTable::new().exact("urn:log:config", "file:/logConfig.yaml");
        assert_eq!(
            table
                .canonicalize(&iri("urn:log:config"))
                .canonical()
                .map(Iri::as_str),
            Some("file:/logConfig.yaml")
        );
        // Not a prefix: a longer name under it is untouched.
        assert_eq!(
            table.canonicalize(&iri("urn:log:config:extra")),
            Canonical::Direct
        );
    }

    #[test]
    fn the_most_specific_rule_wins_regardless_of_declaration_order() {
        let table = AliasTable::new()
            .prefix("urn:fn:", "urn:iki:fn:")
            .exact("urn:fn:toUpper", "urn:legacy:upper");
        assert_eq!(
            table
                .canonicalize(&iri("urn:fn:toUpper"))
                .canonical()
                .map(Iri::as_str),
            Some("urn:legacy:upper")
        );
        assert_eq!(
            table
                .canonicalize(&iri("urn:fn:reverseList"))
                .canonical()
                .map(Iri::as_str),
            Some("urn:iki:fn:reverseList")
        );
    }

    #[test]
    fn a_chain_resolves_transitively() {
        // Decision 5, first half: A -> B -> C resolves.
        let table = AliasTable::new()
            .prefix("urn:a:", "urn:b:")
            .prefix("urn:b:", "urn:c:");
        let canonical = table.canonicalize(&iri("urn:a:thing"));
        assert_eq!(canonical.canonical().map(Iri::as_str), Some("urn:c:thing"));
        assert_eq!(canonical.hop().unwrap().rules().len(), 2);
    }

    #[test]
    fn a_cycle_is_refused_not_truncated() {
        // Decision 5, second half: A -> B, B -> A terminates with a refusal that
        // names the loop — never a partial rewrite to whichever name it stopped on.
        let table = AliasTable::new()
            .prefix("urn:a:", "urn:b:")
            .prefix("urn:b:", "urn:a:");
        let Canonical::Refused(refusal) = table.canonicalize(&iri("urn:a:thing")) else {
            panic!("a cycle must be refused");
        };
        assert_eq!(refusal.reason, "alias cycle");
        assert_eq!(refusal.trail, ["urn:a:thing", "urn:b:thing", "urn:a:thing"]);
        assert!(table.rules().iter().any(|rule| rule.refused() > 0));
    }

    #[test]
    fn an_over_long_chain_is_refused() {
        // A self-extending rule never revisits a name, so the visited set cannot
        // catch it — the hop budget is what stops it.
        let table = AliasTable::new()
            .prefix("urn:a:", "urn:a:x")
            .with_max_hops(3);
        let Canonical::Refused(refusal) = table.canonicalize(&iri("urn:a:thing")) else {
            panic!("an unbounded chain must be refused");
        };
        assert!(refusal.reason.contains("exceeded 3 hops"), "{refusal}");
    }

    #[test]
    fn the_kernel_namespace_is_not_aliasable() {
        let table = AliasTable::new().prefix("urn:kernel:", "urn:example:moved:kernel:");
        assert_eq!(
            table.canonicalize(&iri("urn:kernel:cut")),
            Canonical::Direct
        );
    }

    #[test]
    fn an_unmatched_name_costs_nothing_and_reports_direct() {
        let table = AliasTable::new().prefix("urn:fn:", "urn:iki:fn:");
        assert_eq!(table.canonicalize(&iri("urn:text:wc")), Canonical::Direct);
        assert_eq!(table.rules()[0].hops(), 0);
    }

    #[test]
    fn the_table_round_trips_through_its_text_form() {
        let text = "# comment\nprefix urn:fn: urn:iki:fn:\nexact urn:log:config file:/logConfig.yaml\nmax-hops 3\n";
        let table = AliasTable::parse(text).unwrap();
        assert_eq!(table.max_hops(), 3);
        let again = AliasTable::parse(&table.to_text()).unwrap();
        assert_eq!(again.to_text(), table.to_text());
        assert_eq!(again.max_hops(), 3);
    }

    #[test]
    fn a_malformed_table_fails_when_it_is_read() {
        let err = AliasTable::parse("prefix not-an-iri urn:iki:fn:").unwrap_err();
        assert_eq!(err.line, 1);
        assert!(err.detail.contains("from"), "{err}");
        assert!(AliasTable::parse("rewrite a b").is_err());
        assert!(AliasTable::parse("max-hops lots").is_err());
    }

    #[test]
    fn the_overlay_resolves_the_old_name_to_the_backing_endpoint() {
        let inner = Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:iki:fn:toUpper"), builtins::to_upper()),
        );
        let table = Arc::new(AliasTable::new().prefix("urn:fn:", "urn:iki:fn:"));
        let space = Alias::new(table, inner);
        let request = Request::new(Verb::Source, iri("urn:fn:toUpper"));
        assert!(matches!(
            space.resolve(&request, &Scope::empty()),
            Resolution::Hit(_)
        ));
    }

    #[test]
    fn the_overlay_is_transparent_to_enumeration() {
        // Decision 4: the catalog sees the backing names, once each.
        let inner = Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:iki:fn:toUpper"), builtins::to_upper()),
        );
        let table = Arc::new(AliasTable::new().prefix("urn:fn:", "urn:iki:fn:"));
        let entries = Alias::new(table, inner).entries().expect("enumerable");
        let patterns: Vec<&str> = entries.iter().map(|e| e.pattern.as_str()).collect();
        assert_eq!(patterns, ["urn:iki:fn:toUpper"]);
    }

    #[test]
    fn a_refused_rewrite_resolves_to_an_endpoint_that_says_so() {
        // Not a miss: "nothing is bound there" would be a different, misleading fact.
        let inner = Arc::new(EndpointSpace::new());
        let table = Arc::new(
            AliasTable::new()
                .prefix("urn:a:", "urn:b:")
                .prefix("urn:b:", "urn:a:"),
        );
        let space = Alias::new(table, inner);
        let Resolution::Hit(hit) = space.resolve(
            &Request::new(Verb::Source, iri("urn:a:thing")),
            &Scope::empty(),
        ) else {
            panic!("a refusal must not read as a miss");
        };
        assert_eq!(hit.endpoint.describe().id, "alias-refused");
    }

    #[test]
    fn stale_scope_hints_name_grants_that_did_not_migrate() {
        let table = AliasTable::new().prefix("urn:example:fs:", "urn:example:moved:fs:");
        let held = [
            "urn:cap:fs:read:urn:example:fs:ws/x".to_string(),
            "urn:cap:kernel:inspect".to_string(),
        ];
        assert_eq!(
            table.scopes_naming_stale_namespaces(held.iter()),
            ["urn:cap:fs:read:urn:example:fs:ws/x"]
        );
    }
}
