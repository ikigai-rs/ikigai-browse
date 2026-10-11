//! Store checks that LIST and never write: what a browse store holds that the
//! code would no longer produce.
//!
//! ## `.git` records (ledger #1130)
//!
//! Before 0.18.2 every door that read the filesystem served `.git` (ledger
//! #1126), so an annotation, a finding, an archived explanation or review pass,
//! or a judgment could have been minted against a `.git` path, with the file's
//! own text quoted in it (`oa:exact`, a model's prose). 0.18.2 refuses the path
//! at every door, so those records now show as orphaned, but their stored text
//! is still readable through the store. [`git_dir_records`](crate::doctor::git_dir_records) lists them so a
//! person can decide whether to remove them; it removes nothing.
//!
//! ### The shapes it looks for, read from this crate's writers
//!
//! Every record kind names its target in an IRI, and most also in a literal:
//!
//! - the target IRI, `urn:repo:{root}:{door}:{path}` (`file:`, `tree:`), as the
//!   object of `ik:annotates` (annotations and findings), `ik:about` (archived
//!   explanations), or the legacy `ik:target` (pre-0.2.2 stores);
//! - the record's OWN IRI, which for review passes and their region memos ends
//!   in the path (`urn:ikigai:browse:review:{root}:…:{path}`), and the
//!   `prov:wasGeneratedBy` link a finding carries to that pass;
//! - the `ik:path` literal, the decoded root-relative path.
//!
//! So the check is deliberately broad: every IRI in any position is
//! percent-decoded and split on `:` and `/`, and every `ik:path` literal is
//! split on `/`, and a component equal to `.git` (ASCII case-insensitively, the
//! same rule the resolver applies) marks the quad's SUBJECT. Over-listing is
//! the safe direction for a check whose output a person reads before anything
//! is removed. Annotation selectors (`urn:iki:annotation:{id}:selector:…`) hang
//! under their annotation and are reported as its children, since removing a
//! record means removing them too.

use std::collections::{BTreeMap, BTreeSet};

use ikigai_core::{Error, Result};
use oxigraph::model::{GraphName, NamedOrBlankNode, Term};
use oxigraph::store::Store;

const IK_PATH: &str = "https://ikigai-rs.dev/ns#path";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// One stored record that names a `.git` path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRecord {
    /// The record's subject IRI.
    pub subject: String,
    /// The named graph it lives in (`None` for the default graph).
    pub graph: Option<String>,
    /// Its `rdf:type`s, so the listing says what kind of record it is.
    pub types: Vec<String>,
    /// Every term in the record that names `.git` (an IRI, or an `ik:path`
    /// literal), as stored.
    pub evidence: Vec<String>,
    /// Subjects whose IRI extends this one (`{subject}:…`): selectors and
    /// other parts a removal would have to take with it.
    pub children: Vec<String>,
    /// How many quads the record and its children hold together.
    pub quads: usize,
}

/// Does any `:`- or `/`-separated component of `text` equal `.git`, ASCII
/// case-insensitively?
fn names_git(text: &str) -> bool {
    text.split([':', '/'])
        .any(|c| c.as_bytes().eq_ignore_ascii_case(b".git"))
}

/// An IRI names `.git` once its percent-escapes are decoded (`%2Egit`,
/// `.git%2Fconfig`); an undecodable IRI is judged as stored.
fn iri_names_git(iri: &str) -> bool {
    names_git(iri) || crate::iri_decode(iri).is_ok_and(|decoded| names_git(&decoded))
}

fn subject_str(subject: &NamedOrBlankNode) -> String {
    match subject {
        NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
        other => other.to_string(),
    }
}

fn graph_str(graph: &GraphName) -> Option<String> {
    match graph {
        GraphName::NamedNode(n) => Some(n.as_str().to_string()),
        GraphName::DefaultGraph => None,
        other => Some(other.to_string()),
    }
}

/// Every record in `store` (any graph) whose subject, target or path names a
/// `.git` component, with its children, sorted by subject. Reads only.
pub fn git_dir_records(store: &Store) -> Result<Vec<GitRecord>> {
    let store_err = |e: oxigraph::store::StorageError| Error::Endpoint(format!("store: {e}"));
    // Pass 1: which subjects name `.git`, and by what.
    let mut hits: BTreeMap<(String, Option<String>), BTreeSet<String>> = BTreeMap::new();
    let mut quads_per: BTreeMap<String, usize> = BTreeMap::new();
    let mut types: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for quad in store.iter() {
        let quad = quad.map_err(store_err)?;
        let subject = subject_str(&quad.subject);
        *quads_per.entry(subject.clone()).or_default() += 1;
        let key = || (subject.clone(), graph_str(&quad.graph_name));
        if iri_names_git(&subject) {
            hits.entry(key()).or_default().insert(subject.clone());
        }
        match &quad.object {
            Term::NamedNode(node) => {
                if quad.predicate.as_str() == RDF_TYPE {
                    types
                        .entry(subject.clone())
                        .or_default()
                        .insert(node.as_str().to_string());
                }
                if iri_names_git(node.as_str()) {
                    hits.entry(key())
                        .or_default()
                        .insert(node.as_str().to_string());
                }
            }
            Term::Literal(lit)
                if quad.predicate.as_str() == IK_PATH && lit.value().split('/').any(names_git) =>
            {
                hits.entry(key())
                    .or_default()
                    .insert(format!("ik:path \"{}\"", lit.value()));
            }
            _ => {}
        }
    }
    // Pass 2: attach children (`{subject}:…`) and count what a removal takes.
    let all: Vec<&String> = quads_per.keys().collect();
    let mut out = Vec::with_capacity(hits.len());
    for ((subject, graph), evidence) in hits {
        let prefix = format!("{subject}:");
        let children: Vec<String> = all
            .iter()
            .filter(|s| s.starts_with(&prefix))
            .map(|s| s.to_string())
            .collect();
        let quads = quads_per.get(&subject).copied().unwrap_or(0)
            + children
                .iter()
                .map(|c| quads_per.get(c).copied().unwrap_or(0))
                .sum::<usize>();
        out.push(GitRecord {
            types: types
                .get(&subject)
                .map(|t| t.iter().cloned().collect())
                .unwrap_or_default(),
            subject,
            graph,
            evidence: evidence.into_iter().collect(),
            children,
            quads,
        });
    }
    // A child that itself matched (its IRI carries the parent's `.git` path)
    // is reported under its parent, not twice.
    let parents: BTreeSet<String> = out.iter().map(|r| r.subject.clone()).collect();
    out.retain(|r| {
        !parents
            .iter()
            .any(|p| p != &r.subject && r.subject.starts_with(&format!("{p}:")))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxigraph::model::{GraphNameRef, Literal, NamedNode, Quad};

    fn node(iri: &str) -> NamedNode {
        NamedNode::new(iri).unwrap()
    }

    fn add(store: &Store, s: &str, p: &str, o: Term, g: GraphNameRef<'_>) {
        store
            .insert(&Quad::new(node(s), node(p), o, g.into_owned()))
            .unwrap();
    }

    const IK: &str = "https://ikigai-rs.dev/ns#";
    const OA: &str = "http://www.w3.org/ns/oa#";

    /// The shapes this crate writes, one record each: an annotation (with a
    /// selector child), a finding with its pass, an archived explanation, a
    /// legacy `ik:target`, and the controls that must NOT be listed.
    fn fixture() -> Store {
        let store = Store::new().unwrap();
        let g = node("urn:iki:browse:graph:default");
        let g = GraphNameRef::NamedNode(g.as_ref());
        let ann = "urn:iki:annotation:a1";
        add(
            &store,
            ann,
            RDF_TYPE,
            node(&format!("{OA}Annotation")).into(),
            g,
        );
        add(
            &store,
            ann,
            &format!("{IK}annotates"),
            node("urn:repo:demo:file:.git/config").into(),
            g,
        );
        add(
            &store,
            ann,
            IK_PATH,
            Literal::new_simple_literal(".git/config").into(),
            g,
        );
        let sel = "urn:iki:annotation:a1:selector:quote";
        add(
            &store,
            sel,
            &format!("{OA}exact"),
            Literal::new_simple_literal("url = https://x:FAKE@example.invalid").into(),
            g,
        );
        add(
            &store,
            ann,
            &format!("{OA}hasSelector"),
            node(sel).into(),
            g,
        );
        // A finding minted by a review pass whose own IRI ends in the path,
        // spelled with a percent-escape and in upper case.
        let pass = "urn:ikigai:browse:review:demo:sha256:00:review-v5@m:%2EGIT/HEAD";
        let finding = "urn:iki:finding:f1";
        add(
            &store,
            finding,
            "http://www.w3.org/ns/prov#wasGeneratedBy",
            node(pass).into(),
            g,
        );
        add(
            &store,
            pass,
            RDF_TYPE,
            node(&format!("{IK}Review")).into(),
            g,
        );
        // An archived explanation, and a legacy ik:target in the default graph.
        add(
            &store,
            "urn:ikigai:browse:explain:e1",
            &format!("{IK}about"),
            node("urn:repo:demo:tree:sub/.git").into(),
            g,
        );
        add(
            &store,
            "urn:annotation:old",
            &format!("{IK}target"),
            node("urn:repo:demo:file:.git%2Fconfig").into(),
            GraphNameRef::DefaultGraph,
        );
        // Controls: ordinary paths, and names that merely CONTAIN `.git`.
        for (s, target) in [
            ("urn:iki:annotation:ok1", "urn:repo:demo:file:src/lib.rs"),
            ("urn:iki:annotation:ok2", "urn:repo:demo:file:.gitignore"),
            ("urn:iki:annotation:ok3", "urn:repo:demo:file:docs/repo.git"),
            (
                "urn:iki:annotation:ok4",
                "urn:repo:demo:file:.github/ci.yml",
            ),
        ] {
            add(&store, s, &format!("{IK}annotates"), node(target).into(), g);
        }
        store
    }

    #[test]
    fn every_shape_that_names_git_is_listed_and_nothing_else() {
        let records = git_dir_records(&fixture()).unwrap();
        let subjects: Vec<&str> = records.iter().map(|r| r.subject.as_str()).collect();
        assert_eq!(
            subjects,
            [
                "urn:annotation:old",
                "urn:iki:annotation:a1",
                "urn:iki:finding:f1",
                "urn:ikigai:browse:explain:e1",
                "urn:ikigai:browse:review:demo:sha256:00:review-v5@m:%2EGIT/HEAD",
            ],
            "{records:#?}"
        );
        let ann = &records[1];
        assert_eq!(ann.types, [format!("{OA}Annotation")]);
        assert_eq!(ann.children, ["urn:iki:annotation:a1:selector:quote"]);
        assert_eq!(ann.quads, 5, "four on the annotation, one on its selector");
        assert!(ann
            .evidence
            .contains(&"ik:path \".git/config\"".to_string()));
        assert_eq!(
            records[0].graph, None,
            "the legacy row is in the default graph"
        );
        assert_eq!(
            records[1].graph.as_deref(),
            Some("urn:iki:browse:graph:default")
        );
    }

    #[test]
    fn a_store_with_no_git_records_lists_nothing() {
        let store = Store::new().unwrap();
        let g = node("urn:g");
        add(
            &store,
            "urn:iki:annotation:ok",
            &format!("{IK}annotates"),
            node("urn:repo:demo:file:src/main.rs").into(),
            GraphNameRef::NamedNode(g.as_ref()),
        );
        assert!(git_dir_records(&store).unwrap().is_empty());
    }
}
