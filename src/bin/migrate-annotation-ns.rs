//! `migrate-annotation-ns` — the one-shot that moves a pre-0.3.0 browse
//! store's annotations from `urn:annotation:` to `urn:iki:annotation:`.
//!
//! ```text
//! migrate-annotation-ns <store-path>            # DRY RUN (the default)
//! migrate-annotation-ns <store-path> --commit   # write
//! ```
//!
//! Since 0.4.0 it is also the tool for the OTHER move a browse store can need:
//! `--graph <iri>` puts every browse-owned quad into that named graph, which is
//! what `Mount::graph` needs an existing store to have done. Both moves in one
//! pass and one transaction — see [`ikigai_browse::migrate::plan_into_graph`]
//! for why they are not two runs. The binary keeps its name because operators'
//! notes, runbooks and shell history name it; what it does has grown.
//!
//! ★ **The primitive this was a stopgap for has LANDED.** `urn:sparql:update`
//! (ikigai-sparql) and `urn:iki:store:update` (ikigai-store) are writing verbs
//! over the dev server's and gonk's stores respectively, so both moves this
//! binary makes are now expressible as one `DELETE … INSERT … WHERE` against a
//! host that binds one. This file is a convenience that predates them, and
//! retiring it is an open option rather than a hypothetical. See
//! [`ikigai_browse::migrate`] for what a writing verb still does not give.
//!
//! Everything interesting lives in that module, which is compiled and tested
//! by the DEFAULT build against an in-memory store. This file is only the
//! parts that need RocksDB (`Store::open`) and a terminal: argument handling,
//! the lock refusal, and the table. Keeping the split there is what lets the
//! transform be tested in ordinary CI without a C++ toolchain.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ikigai_browse::migrate::{self, Counts};
use oxigraph::model::{GraphName, NamedNode};
use oxigraph::store::Store;

const USAGE: &str = "\
migrate-annotation-ns — move a browse store's annotations to urn:iki:annotation:,
and optionally into a named graph (ikigai-browse 0.4.0's Mount::graph).

    migrate-annotation-ns <store-path>                  dry run (default): report only
    migrate-annotation-ns <store-path> --commit         apply the migration
    migrate-annotation-ns <store-path> --graph <iri>    ALSO move every browse-owned
                                                        quad into that named graph

--graph is what a host that calls Mount::graph(<iri>) needs run once against its
existing store: browse reads are confined to its graph from 0.4.0, so quads left
in the default graph are still there and no longer visible. Both moves happen in
one pass and one transaction.

The store must be CLOSED: RocksDB holds an exclusive lock, so stop the ikigai
server that owns it first. ⚠ Back the directory up before --commit; this is a
one-shot destructive rewrite of production RDF.
";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let (path, commit, graph) = parse_args()?;
    let store = open(&path)?;

    // One planner, one set of counts — the graph is simply absent when no one
    // asked for it, and `counts_for_graph(.., None)` is exactly `counts`.
    let before = migrate::counts_for_graph(&store, graph.as_ref()).map_err(|e| e.to_string())?;
    let plan = migrate::plan_with(&store, migrate::Scope::AllIriPositions, graph.as_ref())
        .map_err(|e| e.to_string())?;

    println!("  store    : {}", path.display());
    if let Some(GraphName::NamedNode(g)) = &graph {
        // `NamedNode`'s Display already brackets the IRI — `<{g}>` prints
        // `<<urn:…>>`, which reads like a different IRI in a deploy window.
        println!("  graph    : {g}");
    }
    println!("  moving   : {} quads", plan.len());
    println!();

    // The dry run's "after" column is the SAME counting function applied to a
    // projection of the plan — not a prediction written twice. A commit's is
    // the store itself.
    let (after, label) = if commit {
        migrate::apply(&store, &plan).map_err(|e| e.to_string())?;
        (
            migrate::counts_for_graph(&store, graph.as_ref()).map_err(|e| e.to_string())?,
            "after",
        )
    } else {
        let projected = migrate::project(&store, &plan).map_err(|e| e.to_string())?;
        (
            migrate::counts_for_graph(&projected, graph.as_ref()).map_err(|e| e.to_string())?,
            "would be",
        )
    };

    print!("{}", migrate::report(&before, &after, label));
    println!();

    let passed = Counts::passed(&before, &after);
    let clauses = if graph.is_some() {
        "annotations equal · old -> 0 · new -> old's former count · dangling -> 0 · \
         total equal · outside target graph -> 0"
    } else {
        "annotations equal · old -> 0 · new -> old's former count · dangling -> 0 · total equal"
    };
    println!("  {}: {clauses}", if passed { "PASS" } else { "FAIL" });
    if !commit {
        println!();
        println!("  DRY RUN — nothing was written. Re-run with --commit to apply.");
        println!("  ⚠ back up {} first.", path.display());
    }

    Ok(if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn parse_args() -> Result<(PathBuf, bool, Option<GraphName>), String> {
    let mut path: Option<PathBuf> = None;
    let mut commit = false;
    let mut graph: Option<GraphName> = None;
    let mut want_graph = false;
    for arg in std::env::args().skip(1) {
        if want_graph {
            want_graph = false;
            let node = NamedNode::new(&arg)
                .map_err(|e| format!("--graph {arg} is not an IRI: {e}\n\n{USAGE}"))?;
            graph = Some(GraphName::NamedNode(node));
            continue;
        }
        match arg.as_str() {
            "--commit" => commit = true,
            "--graph" => want_graph = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            // Fail loud on an unrecognized flag rather than treating it as a
            // path: `--dry-run` (which does not exist, because dry is the
            // default) must not be swallowed as a store directory.
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n\n{USAGE}"))
            }
            other if path.is_none() => path = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument {other}\n\n{USAGE}")),
        }
    }
    if want_graph {
        return Err(format!("--graph needs an IRI\n\n{USAGE}"));
    }
    let path = path.ok_or_else(|| format!("a store path is required\n\n{USAGE}"))?;
    Ok((path, commit, graph))
}

/// Open the store, refusing every way this can go quietly wrong.
///
/// The two refusals — "that is not a store" and "someone has it open" — live in
/// [`ikigai_browse::migrate`] so that this binary and `migrate-archive-roots`
/// cannot drift apart about what a safe store to touch looks like.
fn open(path: &Path) -> Result<Store, String> {
    migrate::require_store_dir(path)?;
    if let Some(holder) = migrate::lock_holder(path) {
        return Err(format!(
            "the store is open: {holder}\n\
             RocksDB holds an exclusive lock, so this migration needs the server stopped. \
             Stop it, run this again, then restart it."
        ));
    }
    Store::open(path).map_err(|e| {
        format!(
            "cannot open {}: {e}\n\
             If this says the lock is held, an ikigai process still owns the store — stop it first.",
            path.display()
        )
    })
}
