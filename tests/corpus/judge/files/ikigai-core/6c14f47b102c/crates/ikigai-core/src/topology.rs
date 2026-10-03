//! **The arrangement as a resource** — the paper's §8.2: the doors, corridors and
//! rules of resolution are themselves a resource among resources.
//!
//! A [`Space`](crate::Space) is a resolver behind `dyn`, and the kernel cannot see
//! inside one. So every space *reports* its own structure through
//! [`Space::topology`](crate::Space::topology): a [`Topology`] tree whose nodes say
//! what kind of space they are ([`SpaceKind`]) and, for the kinds that compose,
//! what they enclose. A space that says nothing — a remote, a hand-written
//! resolver — answers an [`Opaque`](SpaceKind::Opaque) node, which is an honest
//! answer: the graph then states that this is where structural knowledge stops.
//!
//! `urn:kernel:topology` renders the tree the request's **chain** sees — each
//! injected corridor innermost first, then the root unless the chain is severed —
//! as Turtle over the `ik:` vocabulary (`ik:Chain`, `ik:Fallback` with `ik:layers`,
//! `ik:Mount` with `ik:prefix` and `ik:space`, `ik:Limit` with `ik:family` and
//! `ik:matchKind`, `ik:EndpointSpace` with its `ik:pattern`s and its ordered
//! `ik:doors`, `ik:Alias` with its `ik:rewrites` and `ik:maxHops`, `ik:Rewrite`,
//! `ik:Confine`, `ik:Level` with `ik:space`, `ik:OpaqueSpace`). A node whose space
//! claims an [`id`](crate::Space::id) is named by it, and an anonymous one is
//! skolemized under `urn:ikigai:space:_:{n}` in render order — no blank nodes, so
//! the graph diffs and every list cell can be addressed. Order is explicit where it
//! is meaning: `ik:layers` is an `rdf:List` in the order resolution consults, which
//! is the order the chain's fingerprint hashes, and `ik:doors` is one in the order a
//! leaf tries its doors (first match wins).
//!
//! **Each door names the endpoint that answers it** (ledger #608): an `ik:Door`
//! carries the door's pattern, how the pattern matches (`ik:matchKind`: an exact
//! name, a URI template, or a grammar the graph cannot state), the endpoint's
//! [`name`](crate::Endpoint::name) (`ik:endpointName`), and — when the endpoint is
//! a [`Confine`](crate::Confine) — the corridor it confines its sub-requests to
//! (`ik:confinedTo`). So the graph says what to BIND as well as where, which is
//! what a declaration read back needs, and what tells a generic composer from
//! bespoke code. Names are not guaranteed unique (every `FnEndpoint::new("x", …)`
//! is named `x`): the topology renders what is there, and whoever rebuilds from it
//! refuses the ambiguity.
//!
//! What this buys is the paper's Theorem 4(b) as a **walk over this graph**.
//! Gatekeeper completeness over the static tree is decidable by pushdown
//! reachability over imports and ownership. A tree with no `ik:Level` in it
//! contributes no pushes (the formal document's §1.1), so over its graph the check
//! is a path query — "is any door of the personal family reachable from the entry
//! without a limiter over that family standing ahead of it?" — that a host doctor
//! runs before any request is made. An `ik:Level` is the one node that pushes: an
//! endpoint found under it resolves its sub-requests at the level's own space,
//! without the guard it was entered through, so reachability follows those pushes,
//! bounded by the level nesting. `tests/topology.rs` runs that walk, pushes
//! included, over the rendered graph; the formal document carries the level-free
//! form as SPARQL, whose second question reports any level it cannot follow.

use std::fmt::Write as _;
use std::sync::Arc;

use crate::alias::RuleKind;
use crate::iri::Iri;
use crate::space::Space;

/// Where an anonymous node is skolemized: `{SKOLEM_PREFIX}{n}`, `n` in pre-order.
/// A node under it is anonymous when a declaration is read back.
pub(crate) const SKOLEM_PREFIX: &str = "urn:ikigai:space:_:";

/// What kind of space a [`Topology`] node describes, with the structure that kind
/// carries. `#[non_exhaustive]`: a kind that does not exist yet (a remote that
/// reports its transport, say) is additive.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpaceKind {
    /// A space that reports nothing about its structure — the default for any
    /// [`Space`] that does not implement [`topology`](Space::topology). Rendered
    /// `ik:OpaqueSpace`: the graph says where knowledge stops rather than
    /// implying the space is empty.
    Opaque,
    /// A leaf: its doors, in declaration order (first match wins).
    EndpointSpace {
        /// Each door: its pattern, how the pattern matches, and the endpoint that
        /// answers it.
        doors: Vec<Door>,
    },
    /// An ordered first-hit list; the node's children are its layers in the order
    /// they are consulted.
    Fallback,
    /// A prefix-guarded import; the node's one child is the mounted space.
    Mount {
        /// The identifier prefix the mount admits.
        prefix: String,
    },
    /// A closure-driven rewrite (`τ` is a function, so the table is opaque); the
    /// node's one child is the enclosed space.
    Rewrite,
    /// A table-driven rewrite; the node's one child is the enclosed space.
    Alias {
        /// The table's rules, in table order.
        rules: Vec<TopologyRule>,
        /// How many rewrites one canonicalization follows before refusing
        /// ([`AliasTable::max_hops`](crate::AliasTable::max_hops)).
        max_hops: usize,
    },
    /// A limiter over a family of identifiers — a hole, not a door.
    Limit {
        /// The family: a prefix, or a grammar's pattern.
        family: String,
        /// Which of the two the family is: [`MatchKind::Prefix`] for
        /// [`Limit::new`](crate::Limit::new), the grammar's own kind for
        /// [`Limit::matching`](crate::Limit::matching). Without it `Limit::new("i")`
        /// and `Limit::matching(Exact::new("i"))` render alike and wall different
        /// names.
        kind: MatchKind,
    },
    /// A confinement: the corridor an endpoint's sub-requests are severed into.
    /// The node's one child is the enclosed space.
    Confine,
    /// A level ([`Level`](crate::Level)): where an endpoint found inside it runs.
    /// Always named (the node's id is the level's name); the node's one child is
    /// the enclosed space. An endpoint found under it resolves its sub-requests at
    /// this node — its enclosed space, not the path that led to it — then at each
    /// enclosing level, then at the root: the one construct in the tree that
    /// PUSHES, so reachability over a tree with levels in it follows those pushes.
    /// In a chain's `ik:layers` a level is a member of the resolved scope's level
    /// stack.
    Level {
        /// The prefixes the level seals, in declaration order (`ik:seals`).
        seals: Vec<String>,
        /// The namespace the host accepted for it (`ik:namespace`); `None` when it
        /// seals under the prefix it is mounted under.
        namespace: Option<String>,
    },
    /// The resolution chain itself — the entry node `urn:kernel:topology` answers:
    /// its children are the corridors innermost first, then the root unless the
    /// chain is severed. Not a space; the arrangement seen from one request.
    Chain {
        /// Whether the root has been cut off the chain.
        severed: bool,
    },
}

/// One rule of an [`Alias`](crate::Alias) table, as the topology reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TopologyRule {
    /// Exact or prefix.
    pub kind: RuleKind,
    /// The logical name (or prefix) the rule matches.
    pub from: String,
    /// The backing name (or prefix) it rewrites to.
    pub to: String,
}

impl TopologyRule {
    /// A rule summary.
    pub fn new(kind: RuleKind, from: impl Into<String>, to: impl Into<String>) -> Self {
        TopologyRule {
            kind,
            from: from.into(),
            to: to.into(),
        }
    }
}

/// How a door's pattern, or a limiter's family, matches a name — `ik:matchKind`.
///
/// A pattern is text, and text alone cannot say how it matches: `urn:x:` is a
/// prefix under [`Limit::new`](crate::Limit::new) and one exact name under
/// [`Exact`](crate::Exact), and a grammar written outside core (one that adds a
/// binding its pattern does not show, say) may match nothing like what its pattern
/// reads as. So every door and every limiter states its kind, and a kind this crate
/// cannot rebuild from text is [`Custom`](Self::Custom) — the honest answer, as
/// [`Opaque`](SpaceKind::Opaque) is for a space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchKind {
    /// The name must equal the pattern ([`Exact`](crate::Exact)).
    Exact,
    /// The pattern is a URI template, RFC 6570 level 1
    /// ([`UriTemplate`](crate::UriTemplate)).
    Template,
    /// Every name under the pattern, as a prefix ([`Limit::new`](crate::Limit::new)).
    Prefix,
    /// A grammar core does not know: its pattern describes it, and does not define
    /// it. The default [`Grammar::match_kind`](crate::Grammar::match_kind).
    Custom,
}

impl MatchKind {
    /// The literal this kind is written as in the graph: `exact`, `template`,
    /// `prefix` or `custom`.
    ///
    /// ```
    /// use ikigai_core::MatchKind;
    /// assert_eq!(MatchKind::Template.keyword(), "template");
    /// assert_eq!(MatchKind::from_keyword("prefix"), Some(MatchKind::Prefix));
    /// assert_eq!(MatchKind::from_keyword("regex"), None);
    /// ```
    pub fn keyword(&self) -> &'static str {
        match self {
            MatchKind::Exact => "exact",
            MatchKind::Template => "template",
            MatchKind::Prefix => "prefix",
            MatchKind::Custom => "custom",
        }
    }

    /// The kind a [`keyword`](Self::keyword) names, or `None` for any other text.
    pub fn from_keyword(keyword: &str) -> Option<MatchKind> {
        match keyword {
            "exact" => Some(MatchKind::Exact),
            "template" => Some(MatchKind::Template),
            "prefix" => Some(MatchKind::Prefix),
            "custom" => Some(MatchKind::Custom),
            _ => None,
        }
    }
}

/// One door of an [`EndpointSpace`](SpaceKind::EndpointSpace): where a name comes
/// in, how it is matched, and the endpoint that answers it — `ik:Door`.
///
/// `#[non_exhaustive]`: construct with [`new`](Self::new) and the builders, so a
/// field added later is not a flag day for a consumer that builds doors.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Door {
    /// The grammar's pattern — an exact IRI, or a URI template (`ik:pattern`).
    pub pattern: String,
    /// How the pattern matches (`ik:matchKind`).
    pub kind: MatchKind,
    /// The bound endpoint's [`name`](crate::Endpoint::name) (`ik:endpointName`).
    /// Not guaranteed unique: two endpoints may share a name.
    pub endpoint: String,
    /// When the endpoint is a [`Confine`](crate::Confine): the corridor its
    /// sub-requests are confined to, named by the confinement's name
    /// (`ik:confinedTo`).
    pub confined: Option<Box<Topology>>,
}

impl Door {
    /// A door: its pattern, how it matches, and the name of the endpoint it binds.
    pub fn new(pattern: impl Into<String>, kind: MatchKind, endpoint: impl Into<String>) -> Self {
        Door {
            pattern: pattern.into(),
            kind,
            endpoint: endpoint.into(),
            confined: None,
        }
    }

    /// The corridor the door's endpoint confines its sub-requests to (builder).
    /// The corridor's [`id`](Topology::id) is the confinement's name.
    pub fn confined_to(mut self, corridor: Topology) -> Self {
        self.confined = Some(Box::new(corridor));
        self
    }
}

/// One node of the arrangement: a space's identity (if it claims one), its kind,
/// and the spaces it encloses. Built by [`Space::topology`]; rendered by
/// [`to_turtle`](Self::to_turtle).
///
/// `#[non_exhaustive]`, so a field this crate adds later is not a flag day for a
/// consumer that builds nodes (a remote reporting its own arrangement): construct
/// with [`new`](Self::new) and the builders.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Topology {
    /// The space's own IRI, when it claims one ([`Space::id`]).
    pub id: Option<Iri>,
    /// What kind of space this is, and what that kind carries.
    pub kind: SpaceKind,
    /// The spaces this node encloses, in the order they are consulted.
    pub children: Vec<Topology>,
}

impl Topology {
    /// A node of the given kind, anonymous, with no children.
    pub fn new(kind: SpaceKind) -> Self {
        Topology {
            id: None,
            kind,
            children: Vec::new(),
        }
    }

    /// The node a space that reports nothing answers: opaque, carrying the id it
    /// claims (if any). The default [`Space::topology`].
    pub fn opaque(id: Option<Iri>) -> Self {
        Topology {
            id,
            kind: SpaceKind::Opaque,
            children: Vec::new(),
        }
    }

    /// Name the node (builder).
    pub fn with_id(mut self, id: Option<Iri>) -> Self {
        self.id = id;
        self
    }

    /// Append an enclosed space (builder).
    pub fn child(mut self, child: Topology) -> Self {
        self.children.push(child);
        self
    }

    /// The node an `Arc<dyn Space>` reports, named by its own id.
    pub fn of(space: &Arc<dyn Space>) -> Self {
        space.topology()
    }

    /// Render this node and everything under it as Turtle. When this node is a
    /// [`Chain`](SpaceKind::Chain) (what `urn:kernel:topology` answers), its IRI
    /// is the graph's entry point.
    ///
    /// Anonymous nodes are skolemized `urn:ikigai:space:_:{n}` in pre-order; a
    /// named node is its own IRI, so the same named space reached twice in a tree
    /// (a shared `Arc` under two mounts, or one confinement at two doors) is ONE
    /// node, rendered where it is first met and only referenced after that. A name
    /// is a claim — same name, same doors — so the second occurrence has nothing
    /// to add; and rendering it again would skolemize its anonymous descendants a
    /// second time under new numbers, stating two different members for one list
    /// cell of the named node (it did, before 0.1.83).
    /// `ik:layers` is written as explicit `rdf:first`/`rdf:rest` cells under
    /// `{node}:layer:{i}`, never as `( … )`: the collection syntax parses to
    /// blank nodes, and the house rule is none. `ik:doors` is written the same way,
    /// its cells under `{node}:doors:{i}` and each door an `ik:Door` at
    /// `{node}:door:{i}`; a door's confined corridor is skolemized after the node,
    /// in door order.
    pub fn to_turtle(&self) -> String {
        let mut out = String::from(
            "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
             @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
             @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
        );
        self.render(&mut out, &mut Render::default());
        out
    }

    /// The IRI this node is rendered under, allocating a skolem for an anonymous one.
    fn iri_for(&self, state: &mut Render) -> String {
        match &self.id {
            Some(id) => id.as_str().to_string(),
            None => {
                state.skolem += 1;
                format!("{SKOLEM_PREFIX}{}", state.skolem)
            }
        }
    }

    /// Render this node's triples, then its children's, into `out`. Returns the
    /// IRI the node was rendered under, so a parent can point at it. The node
    /// takes its skolem before its children take theirs (pre-order), and its block
    /// is written ahead of theirs, so a reader meets a node before what it encloses.
    fn render(&self, out: &mut String, state: &mut Render) -> String {
        let me = self.iri_for(state);
        if self.id.is_some() && !state.seen.insert(me.clone()) {
            return me; // rendered where it was first met
        }
        let mut below = String::new();
        let children: Vec<String> = self
            .children
            .iter()
            .map(|child| child.render(&mut below, state))
            .collect();
        match &self.kind {
            SpaceKind::Opaque => {
                let _ = write!(out, "\n<{me}> a ik:OpaqueSpace .\n");
            }
            SpaceKind::EndpointSpace { doors } => {
                // A door's corridor is rendered first, in door order (pre-order after
                // the node), so the door's block can point at it.
                let corridors: Vec<Option<String>> = doors
                    .iter()
                    .map(|door| {
                        door.confined
                            .as_ref()
                            .map(|corridor| corridor.render(&mut below, state))
                    })
                    .collect();
                let _ = write!(out, "\n<{me}> a ik:EndpointSpace");
                // The flat `ik:pattern` per door stays: it is the membership every
                // reader since 0.1.78 queries, and `ik:doors` adds the order and the
                // endpoint without taking it away.
                for door in doors {
                    let _ = write!(out, " ;\n    ik:pattern \"{}\"", escape(&door.pattern));
                }
                let cells: Vec<String> = (1..=doors.len())
                    .map(|i| format!("{me}:door:{i}"))
                    .collect();
                write_list(out, &me, "doors", "doors", &cells);
                for ((door, cell), corridor) in doors.iter().zip(&cells).zip(&corridors) {
                    let _ = write!(
                        out,
                        "\n<{cell}> a ik:Door ;\n    ik:pattern \"{}\" ;\n    ik:matchKind \"{}\" ;\n    \
                         ik:endpointName \"{}\"",
                        escape(&door.pattern),
                        door.kind.keyword(),
                        escape(&door.endpoint)
                    );
                    if let Some(corridor) = corridor {
                        let _ = write!(out, " ;\n    ik:confinedTo <{corridor}>");
                    }
                    out.push_str(" .\n");
                }
            }
            SpaceKind::Fallback => {
                let _ = write!(out, "\n<{me}> a ik:Fallback");
                write_list(out, &me, "layers", "layer", &children);
            }
            SpaceKind::Mount { prefix } => {
                let _ = write!(
                    out,
                    "\n<{me}> a ik:Mount ;\n    ik:prefix \"{}\"",
                    escape(prefix)
                );
                write_space(out, &children);
            }
            SpaceKind::Rewrite => {
                let _ = write!(out, "\n<{me}> a ik:Rewrite");
                write_space(out, &children);
            }
            SpaceKind::Alias { rules, max_hops } => {
                let _ = write!(out, "\n<{me}> a ik:Alias ;\n    ik:maxHops {max_hops}");
                for i in 1..=rules.len() {
                    let _ = write!(out, " ;\n    ik:rewrites <{me}:rule:{i}>");
                }
                write_space(out, &children);
                for (i, rule) in rules.iter().enumerate() {
                    let _ = write!(
                        out,
                        "\n<{me}:rule:{}> a ik:RewriteRule ;\n    ik:ruleKind \"{}\" ;\n    \
                         ik:logical \"{}\" ;\n    ik:canonical \"{}\" .\n",
                        i + 1,
                        rule.kind.keyword(),
                        escape(&rule.from),
                        escape(&rule.to)
                    );
                }
            }
            SpaceKind::Limit { family, kind } => {
                let _ = write!(
                    out,
                    "\n<{me}> a ik:Limit ;\n    ik:family \"{}\" ;\n    ik:matchKind \"{}\" .\n",
                    escape(family),
                    kind.keyword()
                );
            }
            SpaceKind::Confine => {
                let _ = write!(out, "\n<{me}> a ik:Confine");
                write_space(out, &children);
            }
            SpaceKind::Level { seals, namespace } => {
                let _ = write!(out, "\n<{me}> a ik:Level");
                for prefix in seals {
                    let _ = write!(out, " ;\n    ik:seals \"{}\"", escape(prefix));
                }
                if let Some(namespace) = namespace {
                    let _ = write!(out, " ;\n    ik:namespace \"{}\"", escape(namespace));
                }
                write_space(out, &children);
            }
            SpaceKind::Chain { severed } => {
                let _ = write!(out, "\n<{me}> a ik:Chain ;\n    ik:severed {severed}");
                write_list(out, &me, "layers", "layer", &children);
            }
        }
        out.push_str(&below);
        me
    }
}

/// What a render carries down the tree: the last skolem allocated, and the named
/// nodes already written.
#[derive(Default)]
struct Render {
    skolem: usize,
    seen: std::collections::BTreeSet<String>,
}

/// `ik:space <child>` for a one-child node, or close the block for a node whose
/// child reported nothing.
fn write_space(block: &mut String, children: &[String]) {
    for child in children {
        let _ = write!(block, " ;\n    ik:space <{child}>");
    }
    block.push_str(" .\n");
}

/// An ordered property (`ik:layers`, `ik:doors`) as explicit list cells:
/// `<me:{cell}:1> rdf:first <a> ; rdf:rest <me:{cell}:2>` … ending in `rdf:nil`. An
/// empty list is `rdf:nil` directly. Closes the node's block.
fn write_list(block: &mut String, me: &str, property: &str, cell: &str, items: &[String]) {
    if items.is_empty() {
        let _ = write!(block, " ;\n    ik:{property} rdf:nil .\n");
        return;
    }
    let _ = write!(block, " ;\n    ik:{property} <{me}:{cell}:1> .\n");
    for (i, item) in items.iter().enumerate() {
        let next = if i + 1 == items.len() {
            "rdf:nil".to_string()
        } else {
            format!("<{me}:{cell}:{}>", i + 2)
        };
        let _ = write!(
            block,
            "<{me}:{cell}:{}> rdf:first <{item}> ;\n    rdf:rest {next} .\n",
            i + 1
        );
    }
}

/// Escape a literal for a Turtle `"…"` position.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}
