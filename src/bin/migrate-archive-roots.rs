//! `migrate-archive-roots` — move a browse explanation archive from ONE store
//! into ANOTHER, renaming repository roots on the way.
//!
//! ```text
//! migrate-archive-roots <source> <target> --root ikigai-core=core …        # DRY RUN
//! migrate-archive-roots <source> <target> --root … --graph <iri> --commit  # write
//! ```
//!
//! ## Why this is a second binary and not a flag on the first
//!
//! `migrate-annotation-ns` rewrites ONE store in place: it opens a path
//! read-write, refuses if anything holds the lock, and every count it prints is
//! a before/after of the same dataset. This is a different operation on every
//! one of those axes — two stores, only one of them written; a source that can
//! stay live because it is read-only; a target whose OTHER tenants (a ledger, a
//! vocabulary) make a whole-store total meaningless, so the columns are a graph
//! count and a per-root table instead. Folding it in would have given one
//! `--commit` two meanings and one usage text two contracts. The transform they
//! share lives in `ikigai_browse::migrate`, which is where sharing belongs.
//!
//! ★ **A writing verb would not replace this one.** Its sibling's "stopgap"
//! note is obsolete — `urn:sparql:update` and `urn:iki:store:update` exist —
//! but they write the store their own host owns, one at a time, and the rename
//! here spans four IRI positions plus a literal and has to assign each
//! annotation's selector children to a root named only on their parent. The
//! refusals (an undecided root, an unassignable subject, a rename collision, a
//! dangling root reference) are the substance, and an UPDATE has nowhere to put
//! them. `ikigai_browse::migrate` argues this in full.
//!
//! ## The hazard it exists for
//!
//! Two hosts named the same directories differently — a dev server whose root
//! names came from the path basename (`ikigai-core`) and a host that names them
//! explicitly (`core`). The root name is inside the data in three places (the
//! subject IRI, the `ik:repo` literal, the `ik:about` / `ik:annotates` /
//! `prov:used` object) and a fourth the obvious list misses (an annotation's
//! `prov:wasGeneratedBy`, pointing at a review pass whose IRI also carries the
//! root). Rewriting some of them produces an archive that is present, countable,
//! SPARQL-visible and **unreachable by every actual read**, because a read
//! builds its IRI from the TARGET host's root name. See
//! `ikigai_browse::migrate::RootScope`, whose two ablations are how the suite
//! demonstrates each of those failures instead of asserting a comment.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ikigai_browse::migrate::{self, RootMap, RootScope};
use oxigraph::model::{GraphName, NamedNode};
use oxigraph::store::Store;

const USAGE: &str = "\
migrate-archive-roots — copy a browse archive between stores, renaming roots.

    migrate-archive-roots <source-store> <target-store> [options]

    --root <from>=<to>   carry <from>'s quads as <to>. Repeatable. <from>=<from>
                         is the identity: carry it under the name it has.
    --drop <root>        leave that root's quads in the source.
    --graph <iri>        the graph in the TARGET the quads land in (default: the
                         target's default graph). A host calling Mount::graph
                         names the same IRI here.
    --commit             write. Without it this is a dry run and touches nothing.

EVERY root the source mentions must be decided — mapped or dropped. A run that
leaves one undecided is refused and prints the option lines that would fix it,
because an unmapped root carried silently would land an archive under a name no
root of the target produces: present, countable, and answered by nothing.

The SOURCE is opened read-only and is never written, so it may stay live; a
holder is named as a warning, and because the transfer is idempotent a re-run
picks up anything written meanwhile. The TARGET is written, so it must be
CLOSED — RocksDB holds an exclusive writer lock. ⚠ Back the target up before
--commit.
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

struct Args {
    source: PathBuf,
    target: PathBuf,
    map: RootMap,
    graph: GraphName,
    commit: bool,
}

fn run() -> Result<ExitCode, String> {
    let args = parse_args()?;
    let source = open_source(&args.source)?;
    let target = open_target(&args.target, args.commit)?;

    let transfer = migrate::plan_transfer_with(&source, &args.map, &args.graph, RootScope::All)
        .map_err(|e| e.to_string())?;

    println!("  source   : {} (read-only)", args.source.display());
    println!("  target   : {}", args.target.display());
    match &args.graph {
        // `NamedNode`'s Display already brackets the IRI — `<{g}>` prints
        // `<<urn:…>>`, which reads like a different IRI in a deploy window.
        GraphName::NamedNode(g) => println!("  graph    : {g}"),
        _ => println!("  graph    : (the target's default graph)"),
    }
    println!();
    print!("{}", migrate::transfer_report(&transfer));
    println!();

    // Refused BEFORE any landing arithmetic: an undecided root is not a number
    // to weigh against other numbers, it is a question. The table above is the
    // survey, so a first run with no `--root` at all is how you take it.
    let unmapped = transfer.unmapped();
    if !unmapped.is_empty() {
        eprintln!(
            "  {} root(s) undecided. Add one line for each:",
            unmapped.len()
        );
        for root in &unmapped {
            eprintln!("      --root {root}=<target-root>   (or --drop {root})");
        }
        return Err("refusing to carry a root nobody decided".to_string());
    }
    if !transfer.unassigned.is_empty() {
        eprintln!("  browse-minted subjects belonging to no root:");
        for subject in &transfer.unassigned {
            eprintln!("      {subject}");
        }
        return Err("refusing: a browse subject could not be assigned to a root".to_string());
    }

    let land = migrate::landing(&target, &transfer).map_err(|e| e.to_string())?;
    let before = migrate::quads_in_graph(&target, &args.graph).map_err(|e| e.to_string())?;

    // The dry run's "would be" column is the SAME counting function applied to a
    // projection of the plan — not a prediction written twice. A commit's is the
    // store itself.
    let (after, label) = if args.commit {
        migrate::apply_transfer(&target, &transfer).map_err(|e| e.to_string())?;
        (
            migrate::quads_in_graph(&target, &args.graph).map_err(|e| e.to_string())?,
            "after",
        )
    } else {
        let projected = migrate::project_transfer(&target, &transfer).map_err(|e| e.to_string())?;
        (
            migrate::quads_in_graph(&projected, &args.graph).map_err(|e| e.to_string())?,
            "would be",
        )
    };

    print!("{}", migrate::landing_report(&land, before, after, label));
    if !transfer.dangling_root_refs.is_empty() {
        println!();
        println!(
            "  ⚠ {} carried quad(s) point at a root that is NOT carried:",
            transfer.dangling_root_refs.len()
        );
        for quad in transfer.dangling_root_refs.iter().take(10) {
            println!("      {quad}");
        }
    }
    println!();

    let passed = migrate::transfer_passed(&transfer, &land, before, after);
    println!(
        "  {}: every root decided · every subject assigned · no dangling root reference · \
         nothing collapsed by a rename · target graph grew by exactly what was new",
        if passed { "PASS" } else { "FAIL" }
    );
    if !args.commit {
        println!();
        println!("  DRY RUN — nothing was written. Re-run with --commit to apply.");
        println!("  ⚠ back up {} first.", args.target.display());
    }

    Ok(if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// A root name is one path segment of a URN. Anything else would let a `--root`
/// argument change the SHAPE of an IRI rather than one of its segments, which is
/// what makes the rewrite's `new_unchecked` sound.
fn check_root_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a root name may not be empty".to_string());
    }
    if let Some(bad) = name.chars().find(|c| *c == ':' || c.is_whitespace()) {
        return Err(format!(
            "root name {name:?} contains {bad:?}; a root is one URN path segment"
        ));
    }
    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut map = RootMap::new();
    let mut graph = GraphName::DefaultGraph;
    let mut commit = false;
    let mut pending: Option<&'static str> = None;

    for arg in std::env::args().skip(1) {
        match pending.take() {
            Some("--root") => {
                let (from, to) = arg
                    .split_once('=')
                    .ok_or_else(|| format!("--root wants <from>=<to>, got {arg}\n\n{USAGE}"))?;
                check_root_name(from)?;
                check_root_name(to)?;
                map = map.rename(from, to);
                continue;
            }
            Some("--drop") => {
                check_root_name(&arg)?;
                map = map.dropped(&arg);
                continue;
            }
            Some("--graph") => {
                let node = NamedNode::new(&arg)
                    .map_err(|e| format!("--graph {arg} is not an IRI: {e}\n\n{USAGE}"))?;
                graph = GraphName::NamedNode(node);
                continue;
            }
            Some(other) => unreachable!("unhandled pending option {other}"),
            None => {}
        }
        match arg.as_str() {
            "--root" => pending = Some("--root"),
            "--drop" => pending = Some("--drop"),
            "--graph" => pending = Some("--graph"),
            "--commit" => commit = true,
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
            other => paths.push(PathBuf::from(other)),
        }
    }
    if let Some(option) = pending {
        return Err(format!("{option} needs a value\n\n{USAGE}"));
    }
    if paths.len() != 2 {
        return Err(format!(
            "expected exactly a source and a target store path, got {}\n\n{USAGE}",
            paths.len()
        ));
    }
    let target = paths.pop().expect("two paths");
    let source = paths.pop().expect("two paths");
    if source == target {
        return Err(
            "source and target are the same store; `migrate-annotation-ns --graph` is the \
             in-place tool"
                .to_string(),
        );
    }
    Ok(Args {
        source,
        target,
        map,
        graph,
        commit,
    })
}

/// The source is READ-ONLY and never written, so a live holder is a warning
/// rather than a refusal: RocksDB's read-only open takes no writer lock, the
/// read is a point-in-time snapshot, and the transfer is idempotent — anything
/// the holder writes afterwards is picked up by running this again.
fn open_source(path: &Path) -> Result<Store, String> {
    migrate::require_store_dir(path)?;
    if let Some(holder) = migrate::lock_holder(path) {
        eprintln!("  note     : the source is open ({holder}).");
        eprintln!("             Reading a snapshot; re-run to pick up anything written since.");
    }
    Store::open_read_only(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// The target is WRITTEN, so it must be closed for a commit. A dry run opens it
/// read-only, which is what lets the whole survey run against a live host.
fn open_target(path: &Path, commit: bool) -> Result<Store, String> {
    migrate::require_store_dir(path)?;
    if !commit {
        return Store::open_read_only(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()));
    }
    if let Some(holder) = migrate::lock_holder(path) {
        return Err(format!(
            "the target store is open: {holder}\n\
             RocksDB holds an exclusive writer lock, so the commit needs that server stopped. \
             Stop it, run this again, then restart it."
        ));
    }
    Store::open(path).map_err(|e| {
        format!(
            "cannot open {} for writing: {e}\n\
             If this says the lock is held, an ikigai process still owns the store — stop it first.",
            path.display()
        )
    })
}
