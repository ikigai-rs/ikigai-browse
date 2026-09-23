//! The store handle a browse mount reads and writes through: an injected
//! Oxigraph `Store` paired with **the one graph this mount owns**.
//!
//! # Why this is a type and not two parameters
//!
//! Before 0.4.0 every write site named `GraphName::DefaultGraph` literally and
//! every read site passed `None` for the graph — and those two are not
//! opposites. `None` in `quads_for_pattern` means *match every graph in the
//! store*, so browse read the whole dataset while writing one corner of it.
//! On a store browse is the only writer of, the two coincide and nothing shows.
//! On a SHARED store they do not, and the gap is a tenancy hole: a mount
//! confined to its own graph for writing would still answer out of anyone's.
//!
//! Threading a `graph` argument beside the `&Store` would have fixed the
//! thirteen writes and left the reads exactly as easy to get wrong — including
//! for the next read someone adds. So the store handle is **private to this
//! type**, and the only read this crate can express is one this type confines:
//! [`Archive::quads_for_pattern`] takes no graph argument because there is no
//! graph to choose. The default is still `GraphName::DefaultGraph`, so a host
//! that says nothing gets byte-for-byte what it had.
//!
//! # What browse owns
//!
//! Every quad browse writes has a browse-MINTED subject — `urn:iki:annotation:…`
//! (annotations and their two selector children) or `urn:ikigai:browse:…` (the
//! explanation archive, the review passes and their region memos). It writes no quad about a
//! subject someone else minted. That is what makes the graph migration a
//! complete one (`crate::migrate`), and it is pinned by
//! `every_quad_browse_writes_has_a_browse_minted_subject`.

use std::sync::Arc;

use oxigraph::model::{GraphName, NamedNodeRef, NamedOrBlankNodeRef, Quad, TermRef};
use oxigraph::store::{QuadIter, StorageError, Store};

/// A store handle bound to one graph.
///
/// Cheap to clone-by-`Arc`: a mount builds exactly one and shares it with the
/// annotation family, the explanation archive, the review passes and the
/// pull-request layers, which is the same single-store sharing those four had
/// before — now with the graph carried along instead of re-chosen at each site.
pub(crate) struct Archive {
    /// PRIVATE, and the point of the type. Nothing in this crate can reach the
    /// handle to make an unconfined read; see [`Archive::store`] for the one
    /// exception and why it is not one.
    store: Arc<Store>,
    graph: GraphName,
}

impl Archive {
    /// Bind `store`'s `graph` — `GraphName::DefaultGraph` for a host that has
    /// not asked for a named one.
    pub(crate) fn new(store: Arc<Store>, graph: GraphName) -> Self {
        Archive { store, graph }
    }

    /// The graph this mount owns — the graph name every stored quad is built
    /// with, and what the confinement tests assert against.
    pub(crate) fn graph(&self) -> &GraphName {
        &self.graph
    }

    /// The raw handle — for **re-pairing it with a graph** and nothing else
    /// (`Mount` resolves one graph for the whole mount and rebuilds the
    /// archive). It is deliberately not a way to run a query: a read written
    /// against this would escape the confinement the type exists for.
    pub(crate) fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Insert a quad — one built with [`Archive::graph`] as its graph name.
    ///
    /// The debug assertion is the cheap half of the confinement claim: in a
    /// debug build (so, in every test and every CI run) a quad that reached
    /// here carrying some other graph name fails loudly rather than landing
    /// quietly outside the mount's graph.
    pub(crate) fn insert(&self, quad: &Quad) -> Result<(), StorageError> {
        debug_assert_eq!(
            quad.graph_name, self.graph,
            "browse: a quad not built with Archive::graph() reached Archive::insert"
        );
        self.store.insert(quad)
    }

    /// Remove a quad — in practice one that came back from
    /// [`Archive::quads_for_pattern`], so it is already in this graph.
    pub(crate) fn remove(&self, quad: &Quad) -> Result<(), StorageError> {
        self.store.remove(quad)
    }

    /// Every matching quad **in this mount's graph**.
    ///
    /// There is no `graph_name` parameter on purpose. `Store::quads_for_pattern`
    /// has one whose `None` matches every graph in the dataset, and that `None`
    /// is exactly the bug this type removes.
    pub(crate) fn quads_for_pattern(
        &self,
        subject: Option<NamedOrBlankNodeRef<'_>>,
        predicate: Option<NamedNodeRef<'_>>,
        object: Option<TermRef<'_>>,
    ) -> QuadIter<'static> {
        self.store
            .quads_for_pattern(subject, predicate, object, Some(self.graph.as_ref()))
    }
}
