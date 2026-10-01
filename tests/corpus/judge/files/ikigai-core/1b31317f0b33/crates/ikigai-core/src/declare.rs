//! **A space declared as data** — the inverse of `urn:kernel:topology` (ledger #633).
//!
//! Core 0.1.78 made the arrangement a resource going OUT: every space reports a
//! [`Topology`], rendered as Turtle over the `ik:` vocabulary. This module takes the
//! same graph coming IN. A **declaration** is a resource whose representation is that
//! Turtle; `Topology::from_turtle` parses it (behind the `declare` feature, which
//! brings the parser), and [`build`] turns the tree into a live
//! [`Arc<dyn Space>`](Space) from a [`Registry`] of endpoints the host provides.
//!
//! **A declaration arranges; it never mints.** Endpoints stay code. A door binds an
//! endpoint the host registered, by the name the endpoint gives itself, and nothing
//! else: there is no factory, no configuration, no way to conjure an endpoint from
//! the file. What becomes data is everything around them — the order of doors, the
//! patterns, fallbacks, mounts, aliases, limiters, confinements, levels, seals and
//! namespaces.
//!
//! **Never skip.** A builder that drops what it does not understand builds a kernel
//! that answers less than its declaration says, and nothing would notice. So every
//! node is either rebuilt with its public constructor or REFUSED, with an error that
//! names the node's IRI (the one the Turtle uses, skolems included) and why:
//!
//! | refused | why |
//! |---|---|
//! | `ik:OpaqueSpace` | a remote peer or a hand-written resolver: there is nothing to rebuild (a declared remote mount is ledger #630) |
//! | `ik:Rewrite` | its rule is a closure, not a table: not data |
//! | `ik:Chain` as the root | a declaration describes a space; the chain is per request |
//! | `ik:Confine` as a space | a confinement lives at a door: declare it as the door's `ik:confinedTo` |
//! | a `custom` door or family | a grammar written outside core is described by its pattern, not defined by it |
//! | an endpoint name the registry does not hold | a declaration binds only what the host registered |
//! | a seal the host, core or another level already owns | [`Kernel::check_sealing`] on the result |
//!
//! And the one ambiguity the topology cannot see: two endpoints may share a
//! [`name`](Endpoint::name). The [`Registry`] refuses the second one registered
//! under a name already taken, so a door can never bind "whichever `x` came first".
//!
//! **Bounded.** A declaration is operator input, so it is read and built within three
//! bounds, each REFUSED with [`DeclarationError::TooLarge`] rather than crashed on or
//! truncated: nesting depth ([`MAX_DECLARATION_DEPTH`]), nodes once every reference is
//! expanded ([`MAX_DECLARATION_NODES`]), and the text those nodes carry
//! ([`MAX_DECLARATION_TEXT`]). A named node is expanded at every place it is used —
//! [`Topology`] is a value tree — so a few kilobytes of Turtle whose named nodes each
//! reference the next several times would otherwise grow exponentially.
//!
//! The round trip is the acceptance test: for a kernel K,
//! `build(from_turtle(K.topology().to_turtle()))` renders the same topology and answers
//! every name K answers, the same way (`tests/declare.rs`). The design note is
//! `docs/design/space-declarations.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::alias::{Alias, AliasTable, RuleKind};
use crate::confine::Confine;
use crate::endpoint::Endpoint;
use crate::grammar::{Exact, UriTemplate};
use crate::kernel::Kernel;
use crate::seal::SealError;
use crate::space::{EndpointSpace, Fallback, Level, Limit, Mount, Space};
use crate::topology::{Door, MatchKind, SpaceKind, Topology, SKOLEM_PREFIX};

/// **How deeply a declaration may nest spaces**: the root is depth 1, and each space
/// it encloses — a layer, a mounted or aliased space, a level's space, a door's
/// confined corridor — is one deeper. `Topology::from_turtle` and [`build`] refuse a
/// deeper tree with [`DeclarationError::TooLarge`].
///
/// Chosen by measurement, for the smallest stack a host is likely to run on: every
/// walk over a declaration recurses once per level (parsing it, building it, rendering
/// it, comparing, cloning and dropping the tree, the built space's own
/// [`topology`](Space::topology), and a request resolving through every level), and at
/// this depth all of them together fit a **1 MiB** thread in a **debug** build — the
/// wasm32 default, and a small worker thread — more than four times over. Measured
/// 2026-09-30 (aarch64, debug, a chain of fallbacks and a chain of confined doors, the
/// two costliest shapes): on 1 MiB, rendering overflows first, past 218 levels;
/// parsing past 269; building past 323; everything else past 450. The same value
/// ikigai-sexpr chose for the same reason, so a surface and core refuse alike.
/// `tests/declare_bounds.rs` runs the whole pipeline at this depth on a 1 MiB thread.
/// A real arrangement is a handful of levels deep.
pub const MAX_DECLARATION_DEPTH: usize = 48;

/// **How many nodes a declaration may hold once every reference is expanded**: each
/// space, each door and each alias rule counts, and a named node counts again at every
/// place it is used, because that is what the tree holds. `Topology::from_turtle`
/// counts as it goes, so a document that expands exponentially is refused after this
/// many nodes, not after the expansion; [`build`] counts the tree it is given the same
/// way.
pub const MAX_DECLARATION_NODES: usize = 65_536;

/// **How much text a declaration may carry once expanded**, in bytes: every IRI a node
/// claims and every pattern, endpoint name, prefix, family, rule, seal and namespace,
/// counted at every place its node is used. The node bound alone would still let one
/// large literal in a node referenced thousands of times expand to gigabytes.
pub const MAX_DECLARATION_TEXT: usize = 16 * 1024 * 1024;

/// Which bound a [`DeclarationError::TooLarge`] declaration exceeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DeclarationBound {
    /// Nesting depth, [`MAX_DECLARATION_DEPTH`].
    Depth,
    /// Expanded nodes, [`MAX_DECLARATION_NODES`].
    Nodes,
    /// Expanded text, [`MAX_DECLARATION_TEXT`].
    Text,
}

impl DeclarationBound {
    /// The limit this bound holds a declaration to.
    pub fn limit(self) -> usize {
        match self {
            DeclarationBound::Depth => MAX_DECLARATION_DEPTH,
            DeclarationBound::Nodes => MAX_DECLARATION_NODES,
            DeclarationBound::Text => MAX_DECLARATION_TEXT,
        }
    }
}

impl fmt::Display for DeclarationBound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DeclarationBound::Depth => "depth",
            DeclarationBound::Nodes => "node",
            DeclarationBound::Text => "text",
        })
    }
}

/// The running size of a declaration, charged one node at a time as it is read or
/// measured — one accounting for `Topology::from_turtle` and [`build`], so a tree the
/// parser produced is always one the builder accepts.
#[derive(Default)]
struct Budget {
    nodes: usize,
    text: usize,
}

impl Budget {
    /// Charge `node`'s own share — itself, its doors and its rules, and the text they
    /// carry; never its children or its doors' corridors, which are charged as nodes
    /// of their own — refusing at `at` once a bound is passed.
    fn charge(&mut self, node: &Topology, at: &str) -> Result<(), DeclarationError> {
        let mut nodes = 1;
        let mut text = node.id.as_ref().map_or(0, |id| id.as_str().len());
        match &node.kind {
            SpaceKind::EndpointSpace { doors } => {
                nodes += doors.len();
                text += doors
                    .iter()
                    .map(|d| d.pattern.len() + d.endpoint.len())
                    .sum::<usize>();
            }
            SpaceKind::Alias { rules, .. } => {
                nodes += rules.len();
                text += rules
                    .iter()
                    .map(|r| r.from.len() + r.to.len())
                    .sum::<usize>();
            }
            SpaceKind::Mount { prefix } => text += prefix.len(),
            SpaceKind::Limit { family, .. } => text += family.len(),
            SpaceKind::Level { seals, namespace } => {
                text += seals.iter().map(String::len).sum::<usize>();
                text += namespace.as_ref().map_or(0, String::len);
            }
            _ => {}
        }
        self.nodes = self.nodes.saturating_add(nodes);
        self.text = self.text.saturating_add(text);
        let bound = if self.nodes > MAX_DECLARATION_NODES {
            DeclarationBound::Nodes
        } else if self.text > MAX_DECLARATION_TEXT {
            DeclarationBound::Text
        } else {
            return Ok(());
        };
        Err(too_large(bound, at))
    }
}

/// The refusal for a declaration past `bound`, found at the node `at`.
fn too_large(bound: DeclarationBound, at: &str) -> DeclarationError {
    DeclarationError::TooLarge {
        bound,
        limit: bound.limit(),
        node: at.to_string(),
    }
}

/// Check `tree` against the three bounds WITHOUT recursing — an explicit stack, so a
/// tree far deeper than the bound is refused rather than overflowing the check itself.
/// Nodes are named as [`Topology::to_turtle`] names them: skolems in pre-order, and a
/// named node met again (rendered once, where it is first met) standing for everything
/// under it.
fn measure(tree: &Topology) -> Result<(), DeclarationError> {
    // (node, its depth, the named node met again that it lies under, if any)
    let mut stack: Vec<(&Topology, usize, Option<String>)> = vec![(tree, 1, None)];
    let mut skolem = 0;
    let mut seen = BTreeSet::new();
    let mut budget = Budget::default();
    while let Some((node, depth, again)) = stack.pop() {
        let (me, again) = match (again, &node.id) {
            (Some(outer), _) => (outer.clone(), Some(outer)),
            (None, Some(id)) => {
                let me = id.as_str().to_string();
                if seen.insert(me.clone()) {
                    (me, None)
                } else {
                    (me.clone(), Some(me))
                }
            }
            (None, None) => {
                skolem += 1;
                (format!("{SKOLEM_PREFIX}{skolem}"), None)
            }
        };
        if depth > MAX_DECLARATION_DEPTH {
            return Err(too_large(DeclarationBound::Depth, &me));
        }
        budget.charge(node, &me)?;
        // Pushed in reverse, so they pop in render order: the children, then each
        // door's corridor in door order.
        if let SpaceKind::EndpointSpace { doors } = &node.kind {
            for corridor in doors.iter().rev().filter_map(|d| d.confined.as_deref()) {
                stack.push((corridor, depth + 1, again.clone()));
            }
        }
        for child in node.children.iter().rev() {
            stack.push((child, depth + 1, again.clone()));
        }
    }
    Ok(())
}

/// The endpoints a declaration may bind, by name — the host's side of the contract.
///
/// An endpoint is registered under its own [`name`](Endpoint::name), because that
/// is what a door in the topology reports (`ik:endpointName`), so an arrangement
/// rendered from a kernel binds back to the same endpoints. **A second, different
/// endpoint under a name already taken is refused**: names are not unique
/// (`FnEndpoint::new("x", …)` is always `x`), and a registry that kept the first or
/// the last would bind a door to whichever one it happened to keep. Registering the
/// same endpoint (the same `Arc`) twice is not a second endpoint, and is allowed.
///
/// The registry also carries the prefixes the HOST seals
/// ([`sealing`](Self::sealing)), which [`build`] checks the declaration against — a
/// declaration cannot seal its way into a name the host sealed.
///
/// ```
/// use std::sync::Arc;
/// use ikigai_core::{DeclarationError, FnEndpoint, ReprType, Registry, Representation};
///
/// let hello = || {
///     Arc::new(FnEndpoint::new("hello", |_| {
///         Ok(Representation::new(ReprType::new("text/plain"), b"hi".to_vec()))
///     }))
/// };
/// let mut registry = Registry::new();
/// let first = hello();
/// registry.register(first.clone()).unwrap();
/// registry.register(first).unwrap(); // the same endpoint again: fine
/// let refused = registry.register(hello()).unwrap_err(); // a different `hello`
/// assert!(matches!(refused, DeclarationError::DuplicateEndpoint { ref id } if id == "hello"));
/// ```
#[derive(Clone, Default)]
pub struct Registry {
    endpoints: BTreeMap<String, Arc<dyn Endpoint>>,
    sealed: Vec<String>,
}

impl Registry {
    /// An empty registry, sealing nothing.
    pub fn new() -> Self {
        Registry::default()
    }

    /// Register `endpoint` under its own [`name`](Endpoint::name). Refused when a
    /// different endpoint already holds that name.
    pub fn register(&mut self, endpoint: Arc<dyn Endpoint>) -> Result<(), DeclarationError> {
        let id = endpoint.name().to_string();
        match self.endpoints.get(&id) {
            Some(held) if Arc::ptr_eq(held, &endpoint) => Ok(()),
            Some(_) => Err(DeclarationError::DuplicateEndpoint { id }),
            None => {
                self.endpoints.insert(id, endpoint);
                Ok(())
            }
        }
    }

    /// The endpoint registered under `id`.
    pub fn get(&self, id: &str) -> Option<&Arc<dyn Endpoint>> {
        self.endpoints.get(id)
    }

    /// Every registered name, in order.
    pub fn ids(&self) -> impl Iterator<Item = &str> + '_ {
        self.endpoints.keys().map(String::as_str)
    }

    /// How many endpoints are registered.
    pub fn len(&self) -> usize {
        self.endpoints.len()
    }

    /// Whether nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }

    /// Seal `prefixes` for the host (builder): what the kernel will be built
    /// [`with_sealed`](Kernel::with_sealed), so [`build`] refuses a declaration whose
    /// levels bind or seal under them.
    pub fn sealing<I, S>(mut self, prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.sealed.extend(prefixes.into_iter().map(Into::into));
        self
    }

    /// The prefixes the host seals.
    pub fn sealed(&self) -> &[String] {
        &self.sealed
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("endpoints", &self.endpoints.keys().collect::<Vec<_>>())
            .field("sealed", &self.sealed)
            .finish()
    }
}

/// Why a declaration was refused — every variant that concerns a node names it by
/// the IRI the declaration's Turtle uses for it (a skolem `urn:ikigai:space:_:{n}`
/// for an anonymous one, numbered as [`Topology::to_turtle`] numbers them).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeclarationError {
    /// The document is not a declaration: not Turtle, a blank node, a triple no
    /// arrangement writes, a property missing or stated twice, an unknown kind, an
    /// ill-formed list, a cycle, or no single root.
    Malformed {
        /// The node the problem was found at, when there is one.
        node: Option<String>,
        /// What is wrong.
        reason: String,
    },
    /// Two different endpoints were registered under one name.
    DuplicateEndpoint {
        /// The name.
        id: String,
    },
    /// A door binds an endpoint name the registry does not hold.
    UnknownEndpoint {
        /// The door.
        door: String,
        /// The name it binds.
        id: String,
    },
    /// An `ik:OpaqueSpace`: a remote peer or a hand-written resolver, with nothing
    /// to rebuild from.
    Opaque {
        /// The node.
        node: String,
        /// The prefix it is mounted under, when it is the space of an `ik:Mount` —
        /// the shape a remote peer takes (declared remote mounts are ledger #630).
        mounted_at: Option<String>,
    },
    /// An `ik:Rewrite`: its rule is a closure, so there is no table to rebuild.
    Rewrite {
        /// The node.
        node: String,
    },
    /// An `ik:Chain`: the arrangement seen from one request, not a space.
    Chain {
        /// The node.
        node: String,
    },
    /// An `ik:Confine` where a space belongs: a confinement is declared on the door
    /// it confines (`ik:confinedTo`).
    Confine {
        /// The node.
        node: String,
    },
    /// A door or a limiter whose pattern core cannot rebuild: a `custom` grammar, a
    /// `prefix` door, or a template that does not parse.
    Pattern {
        /// The door or limiter.
        node: String,
        /// The pattern.
        pattern: String,
        /// Why it cannot be rebuilt.
        reason: String,
    },
    /// The built arrangement breaks the seal rules — the refusal
    /// [`Kernel::check_sealing`] gives.
    Sealing(SealError),
    /// The declaration is past one of the bounds core reads and builds within:
    /// deeper than [`MAX_DECLARATION_DEPTH`], or more nodes than
    /// [`MAX_DECLARATION_NODES`] or more text than [`MAX_DECLARATION_TEXT`] once
    /// every reference is expanded. Refused whole, never read or built partway.
    TooLarge {
        /// Which bound.
        bound: DeclarationBound,
        /// Its limit.
        limit: usize,
        /// The node the bound was passed at: for depth, the first node too deep;
        /// for size, the node whose share passed it. Inside a named node met again,
        /// that named node.
        node: String,
    },
}

impl fmt::Display for DeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeclarationError::Malformed {
                node: Some(node),
                reason,
            } => write!(f, "not a declaration: at <{node}>: {reason}"),
            DeclarationError::Malformed { node: None, reason } => {
                write!(f, "not a declaration: {reason}")
            }
            DeclarationError::DuplicateEndpoint { id } => write!(
                f,
                "two different endpoints are named `{id}`: a door binds by name, so the \
                 name must say which one — register one of them, or rename one"
            ),
            DeclarationError::UnknownEndpoint { door, id } => write!(
                f,
                "<{door}> binds `{id}`, which the host did not register: a declaration \
                 arranges registered endpoints and never mints one"
            ),
            DeclarationError::Opaque {
                node,
                mounted_at: Some(prefix),
            } => write!(
                f,
                "<{node}> is an opaque space mounted at `{prefix}` — a remote peer or a \
                 hand-written resolver, with nothing to rebuild from. A declaration \
                 arranges local endpoints only; declared remote mounts are ledger #630"
            ),
            DeclarationError::Opaque {
                node,
                mounted_at: None,
            } => write!(
                f,
                "<{node}> is an opaque space — a remote peer or a hand-written resolver, \
                 with nothing to rebuild from"
            ),
            DeclarationError::Rewrite { node } => write!(
                f,
                "<{node}> is a closure rewrite: its rule is code, not a table, so it \
                 cannot be declared (a table-driven rewrite is an ik:Alias)"
            ),
            DeclarationError::Chain { node } => write!(
                f,
                "<{node}> is a resolution chain: a declaration describes a space, and the \
                 chain is per request (declare the root layer)"
            ),
            DeclarationError::Confine { node } => write!(
                f,
                "<{node}> is a confinement where a space belongs: a confinement lives at a \
                 door — declare it as that door's ik:confinedTo"
            ),
            DeclarationError::Pattern {
                node,
                pattern,
                reason,
            } => write!(f, "<{node}> matches `{pattern}`, which {reason}"),
            DeclarationError::Sealing(error) => write!(f, "the declared arrangement: {error}"),
            DeclarationError::TooLarge {
                bound: DeclarationBound::Depth,
                limit,
                node,
            } => write!(
                f,
                "<{node}> nests deeper than {limit} spaces (MAX_DECLARATION_DEPTH): a \
                 declaration past a bound is refused whole, never read partway"
            ),
            DeclarationError::TooLarge { bound, limit, node } => write!(
                f,
                "at <{node}> the declaration passes its {bound} bound of {limit} \
                 (MAX_DECLARATION_{}), counting a named node at every place it is used: a \
                 declaration past a bound is refused whole, never read partway",
                match bound {
                    DeclarationBound::Text => "TEXT, in bytes",
                    _ => "NODES",
                }
            ),
        }
    }
}

impl std::error::Error for DeclarationError {}

/// **Build the live space a declaration describes** — the inverse of
/// [`Space::topology`]. Pure: no I/O, no runtime, wasm-clean.
///
/// Each node is rebuilt with its public constructor, keeping its identity:
/// `EndpointSpace` (each door's pattern as an [`Exact`] or a [`UriTemplate`] by its
/// [`MatchKind`], its endpoint from `registry` by name, wrapped in a [`Confine`] when
/// the door declares `ik:confinedTo`), [`Fallback`] (layer order kept), [`Mount`] (of
/// a declared child), [`Alias`] (the rules and the hop bound), [`Limit`] (a prefix, an
/// exact name or a template) and [`Level`] (its seals and namespace). What cannot be
/// rebuilt is refused — see the table in `docs/design/space-declarations.md` — never
/// skipped. The result is
/// then checked with [`Kernel::check_sealing`] against the host's seals
/// ([`Registry::sealing`]).
///
/// **Bounded like `Topology::from_turtle`.** `build` accepts any tree, not only a
/// parsed one, so before it recurses it measures the tree without recursing and
/// refuses one deeper than [`MAX_DECLARATION_DEPTH`], or larger than
/// [`MAX_DECLARATION_NODES`] or [`MAX_DECLARATION_TEXT`] counting every occurrence
/// ([`DeclarationError::TooLarge`]). The same accounting as the parser's, so every
/// tree `from_turtle` returns passes it. What `build` returns therefore nests no
/// deeper than the bound, and so does its [`topology`](Space::topology): every walk
/// over it — rendering, sealing, resolving a request through every level — stays
/// within the depth the bound was measured for.
///
/// ```
/// use std::sync::Arc;
/// use futures::executor::block_on;
/// use ikigai_core::{
///     build, Capability, EndpointSpace, Exact, Fallback, FnEndpoint, Iri, Kernel, Limit,
///     Registry, ReprType, Representation, Request, Space, Verb,
/// };
///
/// let hello = Arc::new(FnEndpoint::new("hello", |_| {
///     Ok(Representation::new(ReprType::new("text/plain"), b"hi".to_vec()))
/// }));
/// // A space written in code …
/// let coded: Arc<dyn Space> = Arc::new(Fallback::new(vec![
///     Arc::new(Limit::new("urn:personal:")),
///     Arc::new(EndpointSpace::new().bind_arc(Exact::new("urn:public:hello"), hello.clone())),
/// ]));
/// // … and the same space built from its own description, with the endpoint registered.
/// let mut registry = Registry::new();
/// registry.register(hello).unwrap();
/// let declared = build(&coded.topology(), &registry).unwrap();
/// assert_eq!(declared.topology(), coded.topology());
///
/// let kernel = Kernel::new(declared);
/// let get = |name: &str| Request::new(Verb::Source, Iri::parse(name).unwrap());
/// let hi = block_on(kernel.issue(get("urn:public:hello"), &Capability::root())).unwrap();
/// assert_eq!(hi.bytes, b"hi");
/// assert!(block_on(kernel.issue(get("urn:personal:x"), &Capability::root())).is_err());
/// ```
pub fn build(
    declaration: &Topology,
    registry: &Registry,
) -> Result<Arc<dyn Space>, DeclarationError> {
    measure(declaration)?;
    let mut builder = Builder {
        registry,
        skolem: 0,
        named: BTreeMap::new(),
    };
    let space = builder.space(declaration, None)?;
    Kernel::check_sealing(space.as_ref(), registry.sealed()).map_err(DeclarationError::Sealing)?;
    Ok(space)
}

/// The walk [`build`] makes, allocating skolems in the order
/// [`Topology::to_turtle`] does so an error names a node as the Turtle does.
struct Builder<'r> {
    registry: &'r Registry,
    skolem: usize,
    /// Every named node built so far, with what it was built from. A name is a
    /// claim — same name, same doors — so a name met again is the SAME space (one
    /// `Arc`, as a shared space is in code), and a name met again over a different
    /// arrangement is refused rather than built twice as two things.
    named: BTreeMap<String, (Topology, Arc<dyn Space>)>,
}

impl Builder<'_> {
    /// The IRI `node` is rendered under — its own, or the next skolem.
    fn iri(&mut self, node: &Topology) -> String {
        match &node.id {
            Some(id) => id.as_str().to_string(),
            None => {
                self.skolem += 1;
                format!("{SKOLEM_PREFIX}{}", self.skolem)
            }
        }
    }

    /// Build one node. `mounted_at` is the prefix of the `ik:Mount` whose space this
    /// node is, if any — so an opaque space there can be named as the remote it is.
    ///
    /// ⚠ The only recursive frame of the build, kept small on purpose (as the
    /// parser's is): what a node earns by itself is refused by
    /// [`check`](Self::check), and the node is put together by
    /// [`assemble`](Self::assemble), and neither is on the stack while this descends.
    /// With the three in one function a level cost about 10 KB of a debug build's
    /// stack (12 KB through a confined door).
    fn space(
        &mut self,
        node: &Topology,
        mounted_at: Option<&str>,
    ) -> Result<Arc<dyn Space>, DeclarationError> {
        let me = self.iri(node);
        if let Some(shared) = self.met_again(node, &me)? {
            return Ok(shared);
        }
        self.check(node, &me, mounted_at)?;
        let prefix = match &node.kind {
            SpaceKind::Mount { prefix } => Some(prefix.as_str()),
            _ => None,
        };
        let mut inner = Vec::with_capacity(node.children.len());
        for child in &node.children {
            inner.push(self.space(child, prefix)?);
        }
        let mut corridors = Vec::new();
        if let SpaceKind::EndpointSpace { doors } = &node.kind {
            for door in doors {
                corridors.push(match &door.confined {
                    Some(corridor) => Some(self.space(corridor, None)?),
                    None => None,
                });
            }
        }
        let space = self.assemble(node, &me, inner, corridors)?;
        if node.id.is_some() {
            self.named.insert(me, (node.clone(), Arc::clone(&space)));
        }
        Ok(space)
    }

    /// The space already built under `node`'s name, when the name was met before
    /// over the same arrangement; refused when it was met over a different one.
    fn met_again(
        &self,
        node: &Topology,
        me: &str,
    ) -> Result<Option<Arc<dyn Space>>, DeclarationError> {
        if node.id.is_none() {
            return Ok(None);
        }
        match self.named.get(me) {
            None => Ok(None),
            Some((first, space)) if first == node => Ok(Some(Arc::clone(space))),
            Some(_) => Err(DeclarationError::Malformed {
                node: Some(me.to_string()),
                reason: "two different arrangements claim this one name (a name is a \
                         claim: same name, same doors)"
                    .into(),
            }),
        }
    }

    /// Every refusal `node` earns by itself, before anything it encloses is built:
    /// a kind that is not data, the wrong number of enclosed spaces, a hop bound of
    /// zero, an unnamed level, and for each door (in order) an endpoint the host did
    /// not register, an unnamed corridor, or a pattern core cannot rebuild.
    fn check(
        &self,
        node: &Topology,
        me: &str,
        mounted_at: Option<&str>,
    ) -> Result<(), DeclarationError> {
        let encloses = |n: usize| {
            if node.children.len() == n {
                Ok(())
            } else {
                Err(DeclarationError::Malformed {
                    node: Some(me.to_string()),
                    reason: format!(
                        "encloses {} spaces, and this kind encloses {n}",
                        node.children.len()
                    ),
                })
            }
        };
        match &node.kind {
            SpaceKind::Opaque => Err(DeclarationError::Opaque {
                node: me.to_string(),
                mounted_at: mounted_at.map(str::to_string),
            }),
            SpaceKind::Rewrite => Err(DeclarationError::Rewrite {
                node: me.to_string(),
            }),
            SpaceKind::Chain { .. } => Err(DeclarationError::Chain {
                node: me.to_string(),
            }),
            SpaceKind::Confine => Err(DeclarationError::Confine {
                node: me.to_string(),
            }),
            SpaceKind::EndpointSpace { doors } => {
                encloses(0)?;
                for (i, door) in doors.iter().enumerate() {
                    self.check_door(&format!("{me}:door:{}", i + 1), door)?;
                }
                Ok(())
            }
            SpaceKind::Fallback => Ok(()),
            SpaceKind::Mount { .. } => encloses(1),
            SpaceKind::Alias { max_hops, .. } => {
                encloses(1)?;
                if *max_hops == 0 {
                    return Err(DeclarationError::Malformed {
                        node: Some(me.to_string()),
                        reason: "an alias table follows at least one hop (ik:maxHops ≥ 1)".into(),
                    });
                }
                Ok(())
            }
            SpaceKind::Limit { family, kind } => {
                encloses(0)?;
                match kind {
                    MatchKind::Template => template(me, family).map(|_| ()),
                    MatchKind::Custom => Err(custom(me, family)),
                    _ => Ok(()),
                }
            }
            SpaceKind::Level { .. } => {
                encloses(1)?;
                if node.id.is_none() {
                    return Err(DeclarationError::Malformed {
                        node: Some(me.to_string()),
                        reason: "a level is always named".into(),
                    });
                }
                Ok(())
            }
        }
    }

    /// The refusals one door earns: its endpoint by name, its corridor's name, and
    /// its grammar by kind.
    fn check_door(&self, at: &str, door: &Door) -> Result<(), DeclarationError> {
        if self.registry.get(&door.endpoint).is_none() {
            return Err(DeclarationError::UnknownEndpoint {
                door: at.to_string(),
                id: door.endpoint.clone(),
            });
        }
        if door.confined.as_ref().is_some_and(|c| c.id.is_none()) {
            return Err(DeclarationError::Malformed {
                node: Some(at.to_string()),
                reason: "a confined corridor is named by its confinement".into(),
            });
        }
        match door.kind {
            MatchKind::Exact => Ok(()),
            MatchKind::Template => template(at, &door.pattern).map(|_| ()),
            MatchKind::Prefix => Err(DeclarationError::Pattern {
                node: at.to_string(),
                pattern: door.pattern.clone(),
                reason: "is a prefix: a door is an exact name or a template, and a \
                         prefix is a Mount's or a limiter's"
                    .into(),
            }),
            MatchKind::Custom => Err(custom(at, &door.pattern)),
        }
    }

    /// Put `node` together from what it encloses, already built: `inner` its
    /// children in order, `corridors` each door's confined corridor. Everything
    /// [`check`](Self::check) refuses has been refused.
    fn assemble(
        &self,
        node: &Topology,
        me: &str,
        inner: Vec<Arc<dyn Space>>,
        corridors: Vec<Option<Arc<dyn Space>>>,
    ) -> Result<Arc<dyn Space>, DeclarationError> {
        let id = node.id.clone();
        let only = || Arc::clone(&inner[0]);
        Ok(match &node.kind {
            SpaceKind::EndpointSpace { doors } => {
                let mut space = EndpointSpace::new();
                for (i, (door, corridor)) in doors.iter().zip(corridors).enumerate() {
                    let at = format!("{me}:door:{}", i + 1);
                    let endpoint = self.endpoint(&at, door)?;
                    let endpoint: Arc<dyn Endpoint> =
                        match (corridor, door.confined.as_ref().and_then(|c| c.id.clone())) {
                            (Some(corridor), Some(name)) => {
                                Arc::new(Confine::new(name, corridor, endpoint))
                            }
                            _ => endpoint,
                        };
                    space = match door.kind {
                        MatchKind::Template => {
                            space.bind_arc(template(&at, &door.pattern)?, endpoint)
                        }
                        _ => space.bind_arc(Exact::new(door.pattern.clone()), endpoint),
                    };
                }
                Arc::new(match id {
                    Some(id) => space.named(id),
                    None => space,
                })
            }
            SpaceKind::Fallback => {
                let fallback = Fallback::new(inner);
                Arc::new(match id {
                    Some(id) => fallback.named(id),
                    None => fallback,
                })
            }
            SpaceKind::Mount { prefix } => {
                let mount = Mount::new(prefix.clone(), only());
                Arc::new(match id {
                    Some(id) => mount.named(id),
                    None => mount,
                })
            }
            SpaceKind::Alias { rules, max_hops } => {
                let mut table = AliasTable::new().with_max_hops(*max_hops);
                for rule in rules {
                    table = match rule.kind {
                        RuleKind::Exact => table.exact(rule.from.clone(), rule.to.clone()),
                        RuleKind::Prefix => table.prefix(rule.from.clone(), rule.to.clone()),
                    };
                }
                let alias = Alias::new(Arc::new(table), only());
                Arc::new(match id {
                    Some(id) => alias.named(id),
                    None => alias,
                })
            }
            SpaceKind::Limit { family, kind } => {
                let limit = match kind {
                    MatchKind::Template => Limit::matching(template(me, family)?),
                    MatchKind::Exact => Limit::matching(Exact::new(family.clone())),
                    _ => Limit::new(family.clone()),
                };
                Arc::new(match id {
                    Some(id) => limit.named(id),
                    None => limit,
                })
            }
            SpaceKind::Level { seals, namespace } => {
                let Some(name) = id else {
                    return Err(not_built(me));
                };
                let mut level = Level::new(name, only());
                if !seals.is_empty() {
                    level = level.sealing(seals.iter().cloned());
                }
                if let Some(namespace) = namespace {
                    level = level.in_namespace(namespace.clone());
                }
                Arc::new(level)
            }
            // Refused by `check`, before anything was built.
            _ => return Err(not_built(me)),
        })
    }

    /// The registered endpoint a door binds (checked present by
    /// [`check_door`](Self::check_door)).
    fn endpoint(&self, at: &str, door: &Door) -> Result<Arc<dyn Endpoint>, DeclarationError> {
        self.registry.get(&door.endpoint).cloned().ok_or_else(|| {
            DeclarationError::UnknownEndpoint {
                door: at.to_string(),
                id: door.endpoint.clone(),
            }
        })
    }
}

/// What [`Builder::assemble`] answers for a node [`Builder::check`] should already
/// have refused — never reached, and a refusal rather than a panic if it ever is.
fn not_built(me: &str) -> DeclarationError {
    DeclarationError::Malformed {
        node: Some(me.to_string()),
        reason: "not a node a declaration builds".into(),
    }
}

/// A template pattern, or the refusal naming why it does not parse.
fn template(node: &str, pattern: &str) -> Result<UriTemplate, DeclarationError> {
    UriTemplate::parse(pattern).map_err(|e| DeclarationError::Pattern {
        node: node.to_string(),
        pattern: pattern.to_string(),
        reason: format!("is not a URI template: {e}"),
    })
}

/// The refusal for a grammar written outside core.
fn custom(node: &str, pattern: &str) -> DeclarationError {
    DeclarationError::Pattern {
        node: node.to_string(),
        pattern: pattern.to_string(),
        reason: "is a custom grammar's description, not its definition: the grammar is \
                 code, and a declaration cannot rebuild it from the text"
            .into(),
    }
}

#[cfg(feature = "declare")]
mod turtle {
    //! `Topology::from_turtle`: parse exactly what `to_turtle` writes.

    use std::collections::{BTreeMap, BTreeSet};

    use oxrdf::{NamedOrBlankNode, Term, Triple};

    use super::{too_large, Budget, DeclarationBound, DeclarationError, MAX_DECLARATION_DEPTH};
    use crate::alias::RuleKind;
    use crate::iri::Iri;
    use crate::topology::{Door, MatchKind, SpaceKind, Topology, TopologyRule, SKOLEM_PREFIX};

    const IK: &str = "https://ikigai-rs.dev/ns#";
    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
    const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
    const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

    /// The kinds a space node may have.
    const SPACE_KINDS: [&str; 10] = [
        "OpaqueSpace",
        "EndpointSpace",
        "Fallback",
        "Mount",
        "Rewrite",
        "Alias",
        "Limit",
        "Confine",
        "Level",
        "Chain",
    ];

    impl Topology {
        /// **Parse a declaration** — exactly what [`to_turtle`](Self::to_turtle)
        /// writes, and nothing it does not. The inverse of rendering:
        /// `Topology::from_turtle(&t.to_turtle()) == Ok(t)` for every tree core
        /// renders. Behind the `declare` feature.
        ///
        /// The document's root is the one space node nothing else encloses. Every
        /// triple must belong to the arrangement under it; a blank node, an unknown
        /// kind or property, a property missing or stated twice, an ill-formed list
        /// cell, a cycle, or the flat `ik:pattern`s of a leaf disagreeing with its
        /// `ik:doors` is refused ([`DeclarationError::Malformed`]), naming the node.
        /// A node under `urn:ikigai:space:_:` is anonymous (a skolem); any other IRI
        /// is the identity the space claims.
        ///
        /// ```
        /// use std::sync::Arc;
        /// use ikigai_core::{EndpointSpace, Exact, FnEndpoint, Iri, Mount, Space, Topology};
        ///
        /// let space = Mount::new(
        ///     "urn:mod:",
        ///     Arc::new(EndpointSpace::new().bind(
        ///         Exact::new("urn:mod:x"),
        ///         FnEndpoint::new("x", |_| unreachable!()),
        ///     )),
        /// )
        /// .named(Iri::parse("urn:example:space:mod").unwrap());
        /// let turtle = space.topology().to_turtle();
        /// assert_eq!(Topology::from_turtle(&turtle).unwrap(), space.topology());
        ///
        /// // A kind no arrangement writes is refused, not skipped.
        /// let odd = turtle.replace("ik:Mount", "ik:Tunnel");
        /// assert!(Topology::from_turtle(&odd).unwrap_err().to_string().contains("ik:Tunnel"));
        /// ```
        ///
        /// **Bounded, and a bound refuses.** A declaration is operator input, so the
        /// parse holds it to [`MAX_DECLARATION_DEPTH`](crate::MAX_DECLARATION_DEPTH)
        /// levels of nesting, checked before each descent so the parse itself cannot
        /// overflow the stack, and to
        /// [`MAX_DECLARATION_NODES`](crate::MAX_DECLARATION_NODES) nodes and
        /// [`MAX_DECLARATION_TEXT`](crate::MAX_DECLARATION_TEXT) bytes of text counted
        /// as they are produced, a named node at every place it is used — so a small
        /// document that would expand exponentially is refused after the bound, not
        /// after the expansion. Past any of them: [`DeclarationError::TooLarge`],
        /// naming the bound, its limit and the node.
        ///
        /// ```
        /// use ikigai_core::{DeclarationBound, DeclarationError, Topology, MAX_DECLARATION_DEPTH};
        ///
        /// // `n` mounts, each over the next, ending in an empty leaf.
        /// let nested = |n: usize| {
        ///     let mut t = String::from("@prefix ik: <https://ikigai-rs.dev/ns#> .\n");
        ///     for i in 1..n {
        ///         let next = i + 1;
        ///         t += &format!("<urn:x:{i}> a ik:Mount ; ik:prefix \"urn:\" ; ik:space <urn:x:{next}> .\n");
        ///     }
        ///     t + &format!("<urn:x:{n}> a ik:EndpointSpace ; ik:doors <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> .\n")
        /// };
        /// assert!(Topology::from_turtle(&nested(MAX_DECLARATION_DEPTH)).is_ok());
        /// let refused = Topology::from_turtle(&nested(MAX_DECLARATION_DEPTH + 1)).unwrap_err();
        /// assert!(matches!(
        ///     refused,
        ///     DeclarationError::TooLarge { bound: DeclarationBound::Depth, limit: 48, ref node }
        ///         if node == "urn:x:49"
        /// ));
        /// ```
        pub fn from_turtle(turtle: &str) -> Result<Topology, DeclarationError> {
            let mut graph = Graph::parse(turtle)?;
            let root = graph.root()?;
            let tree = graph.node(&root, &mut Vec::new())?;
            graph.all_used()?;
            Ok(tree)
        }
    }

    fn malformed(node: &str, reason: impl Into<String>) -> DeclarationError {
        DeclarationError::Malformed {
            node: Some(node.to_string()),
            reason: reason.into(),
        }
    }

    /// Shorten a predicate or class IRI to `ik:x` / `rdf:x` for a message.
    fn short(iri: &str) -> String {
        if let Some(local) = iri.strip_prefix(IK) {
            format!("ik:{local}")
        } else if let Some(local) = iri.strip_prefix("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
        {
            format!("rdf:{local}")
        } else {
            format!("<{iri}>")
        }
    }

    /// The parsed triples, indexed by subject, with a mark on every triple the walk
    /// has read.
    struct Graph {
        triples: Vec<(String, String, Term)>,
        by_subject: BTreeMap<String, Vec<usize>>,
        used: Vec<bool>,
        /// What the walk has produced so far, against the size bounds.
        budget: Budget,
    }

    impl Graph {
        fn parse(turtle: &str) -> Result<Graph, DeclarationError> {
            let mut triples = Vec::new();
            // A graph is a SET: a triple stated twice is one triple.
            let mut stated = BTreeSet::new();
            for triple in oxttl::TurtleParser::new().for_reader(turtle.as_bytes()) {
                let Triple {
                    subject,
                    predicate,
                    object,
                } = triple.map_err(|e| DeclarationError::Malformed {
                    node: None,
                    reason: format!("not Turtle: {e}"),
                })?;
                let subject = match subject {
                    NamedOrBlankNode::NamedNode(n) => n.into_string(),
                    other => {
                        return Err(DeclarationError::Malformed {
                            node: None,
                            reason: format!(
                                "a blank node ({other}) — every node of an arrangement is an IRI"
                            ),
                        })
                    }
                };
                if let Term::BlankNode(b) = &object {
                    return Err(malformed(
                        &subject,
                        format!("a blank node ({b}) — every node of an arrangement is an IRI"),
                    ));
                }
                if stated.insert((subject.clone(), predicate.to_string(), object.to_string())) {
                    triples.push((subject, predicate.into_string(), object));
                }
            }
            let mut by_subject: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for (i, (s, _, _)) in triples.iter().enumerate() {
                by_subject.entry(s.clone()).or_default().push(i);
            }
            let used = vec![false; triples.len()];
            Ok(Graph {
                triples,
                by_subject,
                used,
                budget: Budget::default(),
            })
        }

        /// The one typed node that no other node encloses.
        fn root(&self) -> Result<String, DeclarationError> {
            let referenced: BTreeSet<&str> = self
                .triples
                .iter()
                .filter_map(|(_, p, o)| match o {
                    Term::NamedNode(n) if p != RDF_TYPE => Some(n.as_str()),
                    _ => None,
                })
                .collect();
            let roots: Vec<&str> = self
                .triples
                .iter()
                // Any typed node nothing encloses: a kind no arrangement writes is
                // then refused BY NAME when the walk reads it, rather than leaving
                // the document rootless.
                .filter(|(s, p, _)| p == RDF_TYPE && !referenced.contains(s.as_str()))
                .map(|(s, _, _)| s.as_str())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            match roots.as_slice() {
                [root] => Ok(root.to_string()),
                [] => Err(DeclarationError::Malformed {
                    node: None,
                    reason: "no root: no space node that nothing else encloses (an empty \
                             document, or every space inside a cycle)"
                        .into(),
                }),
                many => Err(DeclarationError::Malformed {
                    node: None,
                    reason: format!(
                        "more than one root ({}): a declaration is one arrangement",
                        many.join(", ")
                    ),
                }),
            }
        }

        /// Every object of `(s, p)`, marking those triples read.
        fn take(&mut self, s: &str, p: &str) -> Vec<Term> {
            let Some(indices) = self.by_subject.get(s) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for &i in indices {
                if self.triples[i].1 == p {
                    self.used[i] = true;
                    out.push(self.triples[i].2.clone());
                }
            }
            out
        }

        fn one(&mut self, s: &str, p: &str) -> Result<Term, DeclarationError> {
            let mut objects = self.take(s, p);
            match objects.len() {
                1 => Ok(objects.remove(0)),
                0 => Err(malformed(s, format!("{} is missing", short(p)))),
                n => Err(malformed(s, format!("{} is stated {n} times", short(p)))),
            }
        }

        fn optional(&mut self, s: &str, p: &str) -> Result<Option<Term>, DeclarationError> {
            let mut objects = self.take(s, p);
            match objects.len() {
                0 => Ok(None),
                1 => Ok(Some(objects.remove(0))),
                n => Err(malformed(s, format!("{} is stated {n} times", short(p)))),
            }
        }

        fn iri_of(s: &str, p: &str, term: Term) -> Result<String, DeclarationError> {
            match term {
                Term::NamedNode(n) => Ok(n.into_string()),
                other => Err(malformed(s, format!("{} is not an IRI: {other}", short(p)))),
            }
        }

        /// A literal of the given XSD datatype (`string` for a plain literal).
        fn literal_of(
            s: &str,
            p: &str,
            term: Term,
            datatype: &str,
        ) -> Result<String, DeclarationError> {
            match term {
                Term::Literal(l)
                    if l.language().is_none()
                        && l.datatype().as_str() == format!("{XSD}{datatype}") =>
                {
                    Ok(l.value().to_string())
                }
                other => Err(malformed(
                    s,
                    format!("{} is not an xsd:{datatype} literal: {other}", short(p)),
                )),
            }
        }

        fn one_iri(&mut self, s: &str, p: &str) -> Result<String, DeclarationError> {
            let term = self.one(s, p)?;
            Self::iri_of(s, p, term)
        }

        fn one_str(&mut self, s: &str, p: &str) -> Result<String, DeclarationError> {
            let term = self.one(s, p)?;
            Self::literal_of(s, p, term, "string")
        }

        fn strs(&mut self, s: &str, p: &str) -> Result<Vec<String>, DeclarationError> {
            self.take(s, p)
                .into_iter()
                .map(|term| Self::literal_of(s, p, term, "string"))
                .collect()
        }

        /// The members of an ordered property, walking its explicit cells.
        fn list(&mut self, s: &str, p: &str) -> Result<Vec<String>, DeclarationError> {
            let mut cell = self.one_iri(s, &format!("{IK}{p}"))?;
            let mut seen = BTreeSet::new();
            let mut out = Vec::new();
            while cell != RDF_NIL {
                if !seen.insert(cell.clone()) {
                    return Err(malformed(
                        s,
                        format!("ik:{p} is a list that loops at <{cell}>"),
                    ));
                }
                out.push(self.one_iri(&cell, RDF_FIRST)?);
                let rest = self.one_iri(&cell, RDF_REST)?;
                self.no_more(&cell)?;
                cell = rest;
            }
            Ok(out)
        }

        /// The one ik: kind `s` is typed with.
        fn kind(&mut self, s: &str, expected: &[&str]) -> Result<String, DeclarationError> {
            let class = self.one_iri(s, RDF_TYPE)?;
            match class.strip_prefix(IK) {
                Some(kind) if expected.contains(&kind) => Ok(kind.to_string()),
                _ => Err(malformed(
                    s,
                    format!(
                        "{} is not a kind of node this position holds",
                        short(&class)
                    ),
                )),
            }
        }

        /// Refuse any triple about `s` the walk has not read.
        fn no_more(&self, s: &str) -> Result<(), DeclarationError> {
            for &i in self.by_subject.get(s).map(Vec::as_slice).unwrap_or(&[]) {
                if !self.used[i] {
                    let (_, p, o) = &self.triples[i];
                    return Err(malformed(
                        s,
                        format!("{} {o} is not part of an arrangement", short(p)),
                    ));
                }
            }
            Ok(())
        }

        /// Refuse any triple the walk from the root never reached.
        fn all_used(&self) -> Result<(), DeclarationError> {
            match self.used.iter().position(|used| !used) {
                None => Ok(()),
                Some(i) => {
                    let (s, p, o) = &self.triples[i];
                    Err(malformed(
                        s,
                        format!(
                            "{} {o} is not reached from the root: every triple of a \
                             declaration belongs to its one arrangement",
                            short(p)
                        ),
                    ))
                }
            }
        }

        /// The identity a node's IRI claims: none for a skolem.
        fn id(s: &str) -> Result<Option<Iri>, DeclarationError> {
            if s.starts_with(SKOLEM_PREFIX) {
                return Ok(None);
            }
            Iri::parse(s)
                .map(Some)
                .map_err(|e| malformed(s, format!("not an IRI a space can claim: {e}")))
        }

        /// Parse the space node `s`, with `path` the nodes enclosing it (for cycles,
        /// and for depth).
        ///
        /// ⚠ This is the ONLY recursive frame of the parse, and it is kept small on
        /// purpose: everything a node states about itself is read by
        /// [`shallow`](Self::shallow), whose frame is gone before the recursion
        /// descends. Reading a node and descending in one function cost about 19 KB of
        /// stack per level in a debug build (24 KB through a confined door), which put
        /// 1 MiB's overflow near 41 levels, under [`MAX_DECLARATION_DEPTH`].
        fn node(&mut self, s: &str, path: &mut Vec<String>) -> Result<Topology, DeclarationError> {
            if path.iter().any(|p| p == s) {
                return Err(malformed(s, "a cycle: the node encloses itself"));
            }
            // `s` would be at depth `path.len() + 1`: refused BEFORE descending.
            if path.len() >= MAX_DECLARATION_DEPTH {
                return Err(too_large(DeclarationBound::Depth, s));
            }
            let Shallow {
                mut tree,
                children,
                corridors,
            } = self.shallow(s)?;
            self.budget.charge(&tree, s)?;
            path.push(s.to_string());
            let below = self.below(&mut tree, &children, &corridors, path);
            path.pop();
            below.map(|()| tree)
        }

        /// Parse what `tree` encloses into it: its children, then each door's corridor
        /// in door order.
        fn below(
            &mut self,
            tree: &mut Topology,
            children: &[String],
            corridors: &[(usize, String)],
            path: &mut Vec<String>,
        ) -> Result<(), DeclarationError> {
            for child in children {
                let child = self.node(child, path)?;
                tree.children.push(child);
            }
            for (door, corridor) in corridors {
                let corridor = Box::new(self.node(corridor, path)?);
                if let SpaceKind::EndpointSpace { doors } = &mut tree.kind {
                    doors[*door].confined = Some(corridor);
                }
            }
            Ok(())
        }

        /// Everything the node `s` states about itself, without descending: the node
        /// with no children and its doors with no corridors, and the IRIs of what it
        /// encloses. Refuses any triple about `s` (or one of its doors or rules) it does
        /// not read.
        fn shallow(&mut self, s: &str) -> Result<Shallow, DeclarationError> {
            let kind = self.kind(s, &SPACE_KINDS)?;
            let id = Self::id(s)?;
            let p = |local: &str| format!("{IK}{local}");
            let mut children = Vec::new();
            let mut corridors = Vec::new();
            let kind = match kind.as_str() {
                "OpaqueSpace" => SpaceKind::Opaque,
                "EndpointSpace" => {
                    let flat: BTreeSet<String> = self.strs(s, &p("pattern"))?.into_iter().collect();
                    let mut doors = Vec::new();
                    for (i, door) in self.list(s, "doors")?.into_iter().enumerate() {
                        let (door, corridor) = self.door(&door)?;
                        if let Some(corridor) = corridor {
                            corridors.push((i, corridor));
                        }
                        doors.push(door);
                    }
                    let listed: BTreeSet<String> =
                        doors.iter().map(|d| d.pattern.clone()).collect();
                    if flat != listed {
                        return Err(malformed(
                            s,
                            "its ik:pattern set and the patterns of its ik:doors disagree",
                        ));
                    }
                    SpaceKind::EndpointSpace { doors }
                }
                "Fallback" => {
                    children = self.list(s, "layers")?;
                    SpaceKind::Fallback
                }
                "Chain" => {
                    let term = self.one(s, &p("severed"))?;
                    let severed =
                        match Self::literal_of(s, &p("severed"), term, "boolean")?.as_str() {
                            "true" => true,
                            "false" => false,
                            other => {
                                return Err(malformed(
                                    s,
                                    format!("ik:severed {other} is not a boolean"),
                                ))
                            }
                        };
                    children = self.list(s, "layers")?;
                    SpaceKind::Chain { severed }
                }
                "Mount" => SpaceKind::Mount {
                    prefix: self.one_str(s, &p("prefix"))?,
                },
                "Rewrite" => SpaceKind::Rewrite,
                "Confine" => SpaceKind::Confine,
                "Alias" => {
                    let term = self.one(s, &p("maxHops"))?;
                    let hops = Self::literal_of(s, &p("maxHops"), term, "integer")?;
                    let max_hops =
                        hops.parse::<usize>()
                            .ok()
                            .filter(|h| *h >= 1)
                            .ok_or_else(|| {
                                malformed(s, format!("ik:maxHops {hops} is not a positive count"))
                            })?;
                    let mut rules = Vec::new();
                    let named: BTreeSet<String> = self
                        .take(s, &p("rewrites"))
                        .into_iter()
                        .map(|t| Self::iri_of(s, &p("rewrites"), t))
                        .collect::<Result<_, _>>()?;
                    // The order is the {n} in each rule's IRI: `{alias}:rule:{n}`, 1..=k.
                    for n in 1..=named.len() {
                        let rule = format!("{s}:rule:{n}");
                        if !named.contains(&rule) {
                            return Err(malformed(
                                s,
                                format!(
                                    "its ik:rewrites are not <{s}:rule:1> … <{s}:rule:{}> (the \
                                     number in a rule's IRI is its place in the table)",
                                    named.len()
                                ),
                            ));
                        }
                        self.kind(&rule, &["RewriteRule"])?;
                        let kind = match self.one_str(&rule, &p("ruleKind"))?.as_str() {
                            "exact" => RuleKind::Exact,
                            "prefix" => RuleKind::Prefix,
                            other => {
                                return Err(malformed(
                                    &rule,
                                    format!("ik:ruleKind {other:?} is not exact or prefix"),
                                ))
                            }
                        };
                        let from = self.one_str(&rule, &p("logical"))?;
                        let to = self.one_str(&rule, &p("canonical"))?;
                        self.no_more(&rule)?;
                        rules.push(TopologyRule::new(kind, from, to));
                    }
                    SpaceKind::Alias { rules, max_hops }
                }
                "Limit" => SpaceKind::Limit {
                    family: self.one_str(s, &p("family"))?,
                    kind: self.match_kind(s)?,
                },
                "Level" => {
                    if id.is_none() {
                        return Err(malformed(s, "a level is always named, never a skolem"));
                    }
                    SpaceKind::Level {
                        seals: self.strs(s, &p("seals"))?,
                        namespace: match self.optional(s, &p("namespace"))? {
                            Some(term) => {
                                Some(Self::literal_of(s, &p("namespace"), term, "string")?)
                            }
                            None => None,
                        },
                    }
                }
                other => return Err(malformed(s, format!("ik:{other} is not a space kind"))),
            };
            // The one-child kinds enclose their `ik:space`.
            if matches!(
                kind,
                SpaceKind::Mount { .. }
                    | SpaceKind::Rewrite
                    | SpaceKind::Alias { .. }
                    | SpaceKind::Confine
                    | SpaceKind::Level { .. }
            ) {
                children.push(self.one_iri(s, &p("space"))?);
            }
            self.no_more(s)?;
            Ok(Shallow {
                tree: Topology::new(kind).with_id(id),
                children,
                corridors,
            })
        }

        fn match_kind(&mut self, s: &str) -> Result<MatchKind, DeclarationError> {
            let keyword = self.one_str(s, &format!("{IK}matchKind"))?;
            MatchKind::from_keyword(&keyword).ok_or_else(|| {
                malformed(
                    s,
                    format!("ik:matchKind {keyword:?} is not a kind of match"),
                )
            })
        }

        /// A door, and the IRI of the corridor it is confined to, if any.
        fn door(&mut self, s: &str) -> Result<(Door, Option<String>), DeclarationError> {
            self.kind(s, &["Door"])?;
            let pattern = self.one_str(s, &format!("{IK}pattern"))?;
            let kind = self.match_kind(s)?;
            let endpoint = self.one_str(s, &format!("{IK}endpointName"))?;
            let door = Door::new(pattern, kind, endpoint);
            let mut corridor = None;
            if let Some(term) = self.optional(s, &format!("{IK}confinedTo"))? {
                let iri = Self::iri_of(s, &format!("{IK}confinedTo"), term)?;
                if iri.starts_with(SKOLEM_PREFIX) {
                    return Err(malformed(
                        s,
                        "a confined corridor is named by its confinement, never a skolem",
                    ));
                }
                corridor = Some(iri);
            }
            self.no_more(s)?;
            Ok((door, corridor))
        }
    }

    /// One node as [`Graph::shallow`] reads it: the node, with nothing it encloses
    /// yet, and the IRIs of what it encloses.
    struct Shallow {
        tree: Topology,
        /// Its children, in order.
        children: Vec<String>,
        /// Each confined door's corridor: the door's index, and the corridor's IRI.
        corridors: Vec<(usize, String)>,
    }
}
