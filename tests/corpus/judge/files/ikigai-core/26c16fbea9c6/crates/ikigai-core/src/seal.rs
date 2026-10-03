//! **Sealed names** — prefixes a [`Level`](crate::Level) can never answer for
//! unless it owns them (ledger #563, Brian 2026-09-28).
//!
//! Levels make a module's own names win inside it: an endpoint found in a level
//! resolves its sub-requests at its level first. A `Mount` guard limits what
//! ENTERS a module, not what a module binds, so without this a module could bind
//! `urn:sign:trust-set` or `urn:secret:get` and every sub-request resolved at its
//! level would get the module's answer. That cannot raise authority (the fake runs
//! under the same attenuated capability), outside callers never see it, and the
//! cache keeps it out of everyone else's entries — but it is a confused-deputy hole
//! wherever TRUSTED code runs inside a module's level: an imported library, a
//! runtime, a shared verifier resolving its trust set.
//!
//! So a kernel holds a set of sealed prefixes, each with exactly one owner:
//!
//! - **core** seals `urn:kernel:` — as it always has, by answering that namespace
//!   ahead of every chain;
//! - **the host** adds its own ([`Kernel::with_sealed`](crate::Kernel::with_sealed));
//! - **a level** seals names in its own namespace ([`Level::sealing`](crate::Level::sealing)).
//!
//! A sealed name is answered by its owner or not at all: a core- or host-sealed
//! name skips every level (the host's injected corridors, then the root — whose
//! path to the door must cross no level); a name sealed by level M skips every
//! level but M, and from the root it is answered only through M. The host's
//! injected corridors may still stand in for any of them, because injection is
//! already host authority ([`Kernel::issue_in`](crate::Kernel::issue_in)); a
//! confined corridor may not, because a confinement is an endpoint's choice.
//!
//! What the topology shows is checked when the kernel is BUILT, and refused
//! there, naming the level, the binding and the prefix; what it cannot show (an
//! opaque space inside a level, a closure rewrite) is checked on every resolution
//! and refused then. Never a silent skip.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::alias::RuleKind;
use crate::iri::Iri;
use crate::space::LevelPath;
use crate::topology::{MatchKind, SpaceKind, Topology};

/// Whether any [`Level`](crate::Level) in this process has declared a seal. A
/// kernel whose root holds none has no level seals to discover, so construction
/// walks the topology only once one exists — a kernel built before levels existed
/// is built exactly as cheaply as it was. Monotone: set, never cleared.
pub(crate) static SEALING_LEVELS: AtomicBool = AtomicBool::new(false);

/// The prefix core seals: the kernel's own namespace, answered ahead of every
/// chain.
pub(crate) const CORE_SEAL: &str = "urn:kernel:";

/// Who owns a sealed prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SealOwner {
    /// This crate: `urn:kernel:`.
    Core,
    /// The host that built the kernel ([`Kernel::with_sealed`](crate::Kernel::with_sealed)).
    Host,
    /// A level, by name ([`Level::sealing`](crate::Level::sealing)).
    Level(Iri),
}

impl std::fmt::Display for SealOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SealOwner::Core => f.write_str("core"),
            SealOwner::Host => f.write_str("the host"),
            SealOwner::Level(name) => write!(f, "level `{}`", name.as_str()),
        }
    }
}

/// Why a kernel's seals were refused at build.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SealError {
    /// A door that can answer names under a sealed prefix sits where the owner is
    /// not: inside a level that does not own it (a core or host seal inside any
    /// level; a level's seal inside another level), or, for a level's seal,
    /// outside every level.
    Binds {
        /// The level the door is in; `None` for a door outside every level.
        level: Option<Iri>,
        /// The door: a binding's pattern, or an alias rule's logical name.
        door: String,
        /// The sealed prefix it can answer names under.
        prefix: String,
        /// Who sealed it.
        owner: SealOwner,
    },
    /// A level sealed a prefix outside its own namespace — the prefix it is mounted
    /// under, or the one the host accepted for it ([`Level::in_namespace`](crate::Level::in_namespace)).
    OutsideNamespace {
        /// The level.
        level: Iri,
        /// The prefix it tried to seal.
        prefix: String,
        /// Its namespace; `None` when it is mounted under no prefix and declares none.
        namespace: Option<String>,
    },
    /// Two owners claimed prefixes that share names — equal, one inside the other.
    Overlaps {
        /// The claim checked first (core, then the host, then levels in the order
        /// the topology states them).
        prefix: String,
        /// Its owner.
        owner: SealOwner,
        /// The claim that collided with it.
        other: String,
        /// Its owner.
        other_owner: SealOwner,
    },
}

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SealError::Binds {
                level: Some(level),
                door,
                prefix,
                owner,
            } => write!(
                f,
                "level `{}` binds `{door}`, which can answer names under `{prefix}`, sealed \
                 by {owner}: refused — a sealed name is answered only by its owner; bind it \
                 outside the level",
                level.as_str()
            ),
            SealError::Binds {
                level: None,
                door,
                prefix,
                owner,
            } => write!(
                f,
                "the root binds `{door}` outside every level, and it can answer names under \
                 `{prefix}`, sealed by {owner}: refused — a sealed name is answered only by \
                 its owner"
            ),
            SealError::OutsideNamespace {
                level,
                prefix,
                namespace: Some(namespace),
            } => write!(
                f,
                "level `{}` seals `{prefix}`, outside its namespace `{namespace}`: refused — a \
                 module seals only its own names",
                level.as_str()
            ),
            SealError::OutsideNamespace {
                level,
                prefix,
                namespace: None,
            } => write!(
                f,
                "level `{}` seals `{prefix}` but is mounted under no prefix and was accepted \
                 into no namespace: refused — a module seals only its own names",
                level.as_str()
            ),
            SealError::Overlaps {
                prefix,
                owner,
                other,
                other_owner,
            } => write!(
                f,
                "`{other}` (sealed by {other_owner}) overlaps `{prefix}` (sealed by {owner}): \
                 refused — every sealed prefix has exactly one owner"
            ),
        }
    }
}

impl std::error::Error for SealError {}

/// A resolution the seals refuse at runtime: a sealed name answered where its
/// owner is not, through something the topology could not show.
pub(crate) struct SealBreach {
    pub(crate) name: Iri,
    pub(crate) owner: SealOwner,
    /// The innermost level on the found path; `None` when the breach is a
    /// level's seal answered outside every level.
    pub(crate) level: Option<Iri>,
    /// Set when the level answering declared seals the kernel never registered.
    pub(crate) unregistered: bool,
}

impl SealBreach {
    pub(crate) fn message(&self) -> String {
        if self.unregistered {
            return format!(
                "`{}` was answered through level `{}`, which declares sealed names this \
                 kernel never registered — the level is hidden from the topology (an overlay \
                 that does not forward `Space::topology`?), so its seals cannot be enforced: \
                 refused",
                self.name.as_str(),
                self.level.as_ref().map_or("?", Iri::as_str)
            );
        }
        match &self.level {
            Some(level) => format!(
                "sealed name `{}` (sealed by {}) was answered by level `{}`, which does not \
                 own it — through something the topology cannot show (an opaque space or a \
                 closure rewrite inside the level): refused",
                self.name.as_str(),
                self.owner,
                level.as_str()
            ),
            None => format!(
                "sealed name `{}` (sealed by {}) was answered outside that level — through \
                 something the topology cannot show: refused",
                self.name.as_str(),
                self.owner
            ),
        }
    }
}

/// The kernel's seal table: every sealed prefix and its one owner.
#[derive(Clone, Debug)]
pub(crate) struct Seals {
    /// Core's claim first, then the host's, then each level's.
    claims: Vec<(String, SealOwner)>,
    /// No host or level claims — the runtime checks have nothing to do.
    trivial: bool,
}

impl Default for Seals {
    fn default() -> Self {
        Seals {
            claims: vec![(CORE_SEAL.to_string(), SealOwner::Core)],
            trivial: true,
        }
    }
}

impl Seals {
    /// The table for a root, with `host` sealed besides core — every rule checked.
    /// With no host seals and no sealing level anywhere in the process, the default
    /// table, without walking anything.
    pub(crate) fn build(root: &dyn crate::Space, host: &[String]) -> Result<Seals, SealError> {
        if host.is_empty() && !SEALING_LEVELS.load(Ordering::Relaxed) {
            return Ok(Seals::default());
        }
        Seals::from_topology(&root.topology(), host)
    }

    /// The table for an arrangement's topology — see [`build`](Self::build).
    pub(crate) fn from_topology(tree: &Topology, host: &[String]) -> Result<Seals, SealError> {
        let mut found = Found::default();
        found.walk(tree, &mut Vec::new(), Some(String::new()));

        let mut claims: Vec<(String, SealOwner)> = vec![(CORE_SEAL.to_string(), SealOwner::Core)];
        claims.extend(host.iter().map(|p| (p.clone(), SealOwner::Host)));
        for level in &found.levels {
            for prefix in &level.seals {
                match &level.namespace {
                    Some(ns) if !ns.is_empty() && prefix.starts_with(ns.as_str()) => {}
                    namespace => {
                        return Err(SealError::OutsideNamespace {
                            level: level.name.clone(),
                            prefix: prefix.clone(),
                            namespace: namespace.clone().filter(|ns| !ns.is_empty()),
                        })
                    }
                }
                // One level reached twice (a shared `Arc` under two mounts) is one
                // owner: each occurrence is checked against its own namespace above,
                // and its claims are recorded once.
                let claim = (prefix.clone(), SealOwner::Level(level.name.clone()));
                if !claims.contains(&claim) {
                    claims.push(claim);
                }
            }
        }
        // Core and the host are checked first: a later claim that collides with an
        // earlier one is the one named as refused, so a level can never take theirs.
        for (i, (prefix, owner)) in claims.iter().enumerate() {
            for (earlier, earlier_owner) in &claims[..i] {
                if earlier_owner != owner && touches(prefix, earlier) {
                    return Err(SealError::Overlaps {
                        prefix: earlier.clone(),
                        owner: earlier_owner.clone(),
                        other: prefix.clone(),
                        other_owner: owner.clone(),
                    });
                }
            }
        }
        for door in &found.doors {
            for (prefix, owner) in &claims {
                if let Some(error) = door.breach(prefix, owner) {
                    return Err(error);
                }
            }
        }
        let trivial = claims.len() == 1;
        Ok(Seals { claims, trivial })
    }

    /// The owner of the sealed prefix `name` falls under, if any.
    pub(crate) fn owner_of(&self, name: &str) -> Option<&SealOwner> {
        self.claims
            .iter()
            .find(|(prefix, _)| name.starts_with(prefix.as_str()))
            .map(|(_, owner)| owner)
    }

    /// Whether a sealed name owned by `owner` may be looked up in the level-stack
    /// frame named `level`: only its owner's.
    pub(crate) fn frame_admits(owner: &SealOwner, level: &Iri) -> bool {
        matches!(owner, SealOwner::Level(name) if name == level)
    }

    /// Whether this table has anything to enforce at runtime.
    pub(crate) fn is_trivial(&self) -> bool {
        self.trivial
    }

    /// Whether a level of this name registered seals.
    fn registered(&self, level: &Iri) -> bool {
        self.claims
            .iter()
            .any(|(_, owner)| matches!(owner, SealOwner::Level(name) if name == level))
    }

    /// Check a hit found at `path` for `names` (the target, and a reported
    /// canonical): a core- or host-sealed name must have been found outside every
    /// level; a level's must have been found in that level itself; and every
    /// sealing level on the path must be one this kernel registered.
    pub(crate) fn admit<'a>(
        &self,
        names: impl IntoIterator<Item = &'a Iri>,
        path: &LevelPath,
    ) -> Result<(), SealBreach> {
        if let Some(hidden) = path.unregistered_sealing(|name| self.registered(name)) {
            return Err(SealBreach {
                name: names
                    .into_iter()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| hidden.clone()),
                owner: SealOwner::Level(hidden.clone()),
                level: Some(hidden.clone()),
                unregistered: true,
            });
        }
        if self.trivial {
            return Ok(());
        }
        for name in names {
            let Some(owner) = self.owner_of(name.as_str()) else {
                continue;
            };
            let innermost = path.names().next();
            let admitted = match owner {
                // Answered by the kernel ahead of every chain; never reaches here.
                SealOwner::Core => true,
                SealOwner::Host => innermost.is_none(),
                SealOwner::Level(owner) => innermost == Some(owner),
            };
            if !admitted {
                return Err(SealBreach {
                    name: name.clone(),
                    owner: owner.clone(),
                    level: innermost.cloned(),
                    unregistered: false,
                });
            }
        }
        Ok(())
    }

    /// Every claim, core's first.
    pub(crate) fn claims(&self) -> &[(String, SealOwner)] {
        &self.claims
    }
}

/// Two prefixes share names: one extends the other.
fn touches(a: &str, b: &str) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Narrow a route's gate by a mount's prefix: `None` once nothing is admitted.
fn narrow(gate: &Option<String>, prefix: &str) -> Option<String> {
    let gate = gate.as_ref()?;
    if prefix.starts_with(gate.as_str()) {
        Some(prefix.to_string())
    } else if gate.starts_with(prefix) {
        Some(gate.clone())
    } else {
        None
    }
}

/// A level as the topology states it.
struct FoundLevel {
    name: Iri,
    seals: Vec<String>,
    /// What it may seal under: the prefix it accepted, else the mounts between it
    /// and its enclosing level (or the root). `Some("")` is no mount at all.
    namespace: Option<String>,
}

/// A door as the topology states it, with every route a request can reach it by.
struct FoundDoor {
    text: String,
    /// How `text` matches, as the topology states it (`ik:matchKind`; an alias rule's
    /// kind). Only a template's pattern — or a grammar core does not know, read the
    /// same way — has a `{` that opens an expansion: an exact name may carry a
    /// literal brace.
    kind: MatchKind,
    /// The levels enclosing it, innermost first, each with the gate from that level
    /// down — the route a frame of that level's stack walk takes.
    levels: Vec<(Iri, Option<String>)>,
    /// The gate from the root down — the route every request from outside takes.
    root_gate: Option<String>,
}

impl FoundDoor {
    /// The literal head of the pattern: for a template (or a grammar core does not
    /// know, read conservatively as one), everything before the first expansion; for
    /// an exact name or a prefix, all of it.
    fn head(&self) -> &str {
        match self.kind {
            MatchKind::Template | MatchKind::Custom => {
                self.text.split('{').next().unwrap_or(&self.text)
            }
            _ => &self.text,
        }
    }

    /// Whether this door can answer a name under `prefix`, by its literal head.
    fn may_match(&self, prefix: &str) -> bool {
        let expands = self.head().len() < self.text.len();
        match self.kind {
            MatchKind::Prefix => touches(&self.text, prefix),
            _ if expands => touches(self.head(), prefix),
            _ => self.text.starts_with(prefix),
        }
    }

    fn reaches(gate: &Option<String>, prefix: &str) -> bool {
        gate.as_deref().is_some_and(|gate| touches(gate, prefix))
    }

    /// Whether this door's literal head lies INSIDE `prefix` — it binds names of
    /// the sealed family by name, whatever expands after the head.
    fn inside(&self, prefix: &str) -> bool {
        self.head().starts_with(prefix)
    }

    /// The refusal this door earns against one claim, if any.
    ///
    /// Two cases, because a door can stand to a sealed family in two ways. A door
    /// whose literal head is INSIDE the family binds the sealed name by name: held
    /// anywhere but by its owner it is refused outright — reachable or not, since
    /// a binding the seal makes dead is exactly the silent skip the rule refuses.
    /// A template whose head the family EXTENDS (`urn:{ns}:{id}` against
    /// `urn:sign:`) binds nothing by name and may expand into anything; it is
    /// refused only where a request for a sealed name can actually reach it — from
    /// the root through every mount above it, or through the owner's own frame when
    /// it sits inside the owner. (Level frames never look up a sealed name they do
    /// not own, so no other route exists.)
    fn breach(&self, prefix: &str, owner: &SealOwner) -> Option<SealError> {
        let innermost = self.levels.first().map(|(name, _)| name);
        let placed = match owner {
            // The kernel answers `urn:kernel:*` ahead of everything: no door can.
            SealOwner::Core => true,
            // The host's own bindings sit outside every level.
            SealOwner::Host => innermost.is_none(),
            SealOwner::Level(owner) => innermost == Some(owner),
        };
        if placed {
            return None;
        }
        let refused = if self.inside(prefix) {
            true
        } else if self.may_match(prefix) {
            match owner {
                SealOwner::Core => false,
                SealOwner::Host => Self::reaches(&self.root_gate, prefix),
                SealOwner::Level(owner) => {
                    Self::reaches(&self.root_gate, prefix)
                        || self
                            .levels
                            .iter()
                            .any(|(name, gate)| name == owner && Self::reaches(gate, prefix))
                }
            }
        } else {
            false
        };
        refused.then(|| SealError::Binds {
            level: innermost.cloned(),
            door: self.text.clone(),
            prefix: prefix.to_string(),
            owner: owner.clone(),
        })
    }
}

#[derive(Default)]
struct Found {
    levels: Vec<FoundLevel>,
    doors: Vec<FoundDoor>,
}

impl Found {
    /// Walk a topology tree, collecting levels and doors. `levels` holds the
    /// enclosing levels innermost LAST while walking, each with its gate;
    /// `root_gate` is the gate from the root.
    fn walk(
        &mut self,
        node: &Topology,
        levels: &mut Vec<(Iri, Option<String>)>,
        root_gate: Option<String>,
    ) {
        match &node.kind {
            SpaceKind::Level { seals, namespace } => {
                let Some(name) = node.id.clone() else {
                    return; // a level is always named; an unnamed node is not one of ours
                };
                let gate = levels.last().map_or(&root_gate, |(_, gate)| gate);
                self.levels.push(FoundLevel {
                    name: name.clone(),
                    seals: seals.clone(),
                    namespace: namespace.clone().or_else(|| gate.clone()),
                });
                levels.push((name, Some(String::new())));
                for child in &node.children {
                    self.walk(child, levels, root_gate.clone());
                }
                levels.pop();
            }
            SpaceKind::Mount { prefix } => {
                let root_gate = narrow(&root_gate, prefix);
                let saved: Vec<Option<String>> = levels.iter().map(|(_, g)| g.clone()).collect();
                for (_, gate) in levels.iter_mut() {
                    *gate = narrow(gate, prefix);
                }
                for child in &node.children {
                    self.walk(child, levels, root_gate.clone());
                }
                for ((_, gate), was) in levels.iter_mut().zip(saved) {
                    *gate = was;
                }
            }
            // A door's confined corridor is not walked: a confinement is an
            // endpoint's choice, and a confined corridor never stands in for a
            // sealed name (checked on resolution, as before it was rendered).
            SpaceKind::EndpointSpace { doors } => {
                for door in doors {
                    self.door(&door.pattern, door.kind, levels, &root_gate);
                }
            }
            // A limiter is a hole, not a door: it answers nothing, so it can fake
            // nothing, and a host's limiter over a module's sealed name is a
            // gatekeeper, not a squatter. Never counted.
            SpaceKind::Limit { .. } => {}
            SpaceKind::Alias { rules, .. } => {
                for rule in rules {
                    let kind = match rule.kind {
                        RuleKind::Exact => MatchKind::Exact,
                        RuleKind::Prefix => MatchKind::Prefix,
                    };
                    self.door(&rule.from, kind, levels, &root_gate);
                }
                for child in &node.children {
                    self.walk(child, levels, root_gate.clone());
                }
            }
            _ => {
                for child in &node.children {
                    self.walk(child, levels, root_gate.clone());
                }
            }
        }
    }

    fn door(
        &mut self,
        text: &str,
        kind: MatchKind,
        levels: &[(Iri, Option<String>)],
        root_gate: &Option<String>,
    ) {
        self.doors.push(FoundDoor {
            text: text.to_string(),
            kind,
            levels: levels.iter().rev().cloned().collect(),
            root_gate: root_gate.clone(),
        });
    }
}
