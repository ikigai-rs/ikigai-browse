//! Rehearse a cross-store root migration and then **READ** it — the acceptance
//! the counts cannot give you.
//!
//! ```text
//! cargo run --release --features migrate --example root-migration-rehearsal -- \
//!     ~/.ikigai/browse-store /tmp/rehearsal-store \
//!     --root ikigai-core=core --drop folio … \
//!     --graph urn:iki:browse:graph:default \
//!     core=~/git-personal/ikigai-core cli=~/git-personal/ikigai-cli …
//! ```
//!
//! ★ **A count proves a copy happened; only a resolution proves the rewrite was
//! right.** The failure this migration can produce is silent: the quads land,
//! SPARQL finds them, and every real read misses, because a read builds its IRI
//! from the TARGET host's root name. So this builds the migrated store for real
//! (a fresh RocksDB at `<scratch>`, nothing of the live target touched), mounts
//! browse over it under the target host's own root names, and asks it for every
//! explanation it just carried.
//!
//! **No LLM is bound.** That is the point: an archive hit answers, and a MISS
//! cannot quietly derive a replacement and look like success — it fails with
//! "no endpoint resolved", which this counts separately.
//!
//! A miss is not automatically a defect. An entry whose content hash no longer
//! matches the working tree is the archive's HISTORY (what `explain-versions`
//! shows) and is kept on purpose; it simply is not what today's tree resolves
//! to. The split between `hit` and `stale` is therefore the honest measure of
//! how much of a migrated archive is live, and it is a number worth knowing
//! before a one-shot.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_browse::migrate::{self, RootMap};
use ikigai_browse::{ExplainConfig, Mount};
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use oxigraph::model::{GraphName, NamedNode, Term};
use oxigraph::store::Store;

const IK: &str = "https://ikigai-rs.dev/ns#";

fn main() {
    if let Err(message) = run() {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let source_path = PathBuf::from(args.next().ok_or("usage: <source-store> <scratch> …")?);
    let scratch = PathBuf::from(args.next().ok_or("usage: <source-store> <scratch> …")?);
    let mut map = RootMap::new();
    let mut graph = GraphName::DefaultGraph;
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    let mut pending: Option<String> = None;
    for arg in args {
        match pending.take().as_deref() {
            Some("--root") => {
                let (from, to) = arg.split_once('=').ok_or("--root wants <from>=<to>")?;
                map = map.rename(from, to);
                continue;
            }
            Some("--drop") => {
                map = map.dropped(&arg);
                continue;
            }
            Some("--graph") => {
                graph = GraphName::NamedNode(
                    NamedNode::new(&arg).map_err(|e| format!("--graph: {e}"))?,
                );
                continue;
            }
            Some(other) => return Err(format!("unhandled option {other}")),
            None => {}
        }
        match arg.as_str() {
            "--root" | "--drop" | "--graph" => pending = Some(arg),
            other => {
                let (name, dir) = other
                    .split_once('=')
                    .ok_or_else(|| format!("expected <root>=<dir>, got {other}"))?;
                roots.push((name.to_string(), expand(dir)));
            }
        }
    }
    if roots.is_empty() {
        return Err("name at least one target root as <root>=<dir>".to_string());
    }
    if scratch.exists() {
        return Err(format!(
            "{} exists; point this at a path that does not, so the rehearsal cannot \
             be confused with a live store",
            scratch.display()
        ));
    }

    migrate::require_store_dir(&source_path)?;
    let source = Store::open_read_only(&source_path)
        .map_err(|e| format!("cannot read {}: {e}", source_path.display()))?;
    let transfer = migrate::plan_transfer(&source, &map, &graph).map_err(|e| e.to_string())?;
    let undecided = transfer.unmapped();
    if !undecided.is_empty() {
        return Err(format!("undecided roots: {}", undecided.join(", ")));
    }

    let target = Arc::new(
        Store::open(&scratch).map_err(|e| format!("cannot create {}: {e}", scratch.display()))?,
    );
    migrate::apply_transfer(&target, &transfer).map_err(|e| e.to_string())?;
    println!(
        "  rehearsed: {} quads into {}\n",
        transfer.carried(),
        scratch.display()
    );

    // Mount browse over the migrated store under the TARGET host's root names,
    // with no LLM anywhere in the space.
    let config = match &graph {
        GraphName::NamedNode(g) => ExplainConfig::new(Arc::clone(&target)).graph(g.clone()),
        _ => ExplainConfig::new(Arc::clone(&target)),
    };
    let mut mount = Mount::new(roots.clone());
    if let GraphName::NamedNode(g) = &graph {
        mount = mount.graph(g.clone());
    }
    let kernel = Kernel::new(Arc::new(mount.explain(config).space()));
    let capability = Capability::scoped(
        roots
            .iter()
            .map(|(name, _)| format!("urn:cap:browse:read:{name}"))
            .chain(["urn:cap:net:localhost".to_string()]),
    );

    // What gets probed, and why the entries split:
    //
    //   Every entry's `ik:about` must name a root THIS host has. That is the
    //   object rewrite, checked against the mount's own root list rather than
    //   against the map — a rename to a name the target does not serve would
    //   pass every count and fail here.
    //
    //   For a file or tree entry, `explain-versions:{path}` joins on `ik:about`
    //   and never touches the working tree, so a zero there is a migration
    //   defect. `explain:{path} version={tag}` additionally keys on the CURRENT
    //   content hash, so a miss there is the archive's HISTORY — retained on
    //   purpose — and not a defect.
    //
    //   A PR entry's `ik:about` is `urn:repo:{root}:pr:{n}`, which no
    //   `explain-versions:{path}` lists and whose own read resolves a head
    //   commit over the network. It gets the `ik:about` check and a column of
    //   its own; a rehearsal must not call GitHub.
    let names: BTreeSet<&str> = roots.iter().map(|(n, _)| n.as_str()).collect();
    let mut listed: BTreeMap<String, u64> = BTreeMap::new();
    let mut live: BTreeMap<String, u64> = BTreeMap::new();
    let mut stale: BTreeMap<String, u64> = BTreeMap::new();
    let mut pr: BTreeMap<String, u64> = BTreeMap::new();
    let mut failed: Vec<String> = Vec::new();

    for entry in migrated_entries(&target, &graph)? {
        let Entry {
            repo,
            path,
            tag,
            about,
        } = entry;
        match about
            .strip_prefix("urn:repo:")
            .and_then(|rest| rest.split(':').next())
        {
            Some(root) if names.contains(root) => {}
            _ => {
                failed.push(format!(
                    "{about}: ik:about names no root this host serves (entry of {repo})"
                ));
                continue;
            }
        }
        if path.starts_with("pr:") {
            *pr.entry(repo).or_default() += 1;
            continue;
        }

        let suffix = if path.is_empty() {
            String::new()
        } else {
            format!(":{path}")
        };
        match json(
            &kernel,
            &format!("urn:repo:{repo}:explain-versions{suffix}"),
            &capability,
            &[],
        ) {
            Ok(rows) if rows.as_array().is_some_and(|r| !r.is_empty()) => {
                *listed.entry(repo.clone()).or_default() += 1;
            }
            Ok(_) => failed.push(format!(
                "urn:repo:{repo}:explain-versions{suffix}: carried, and listed by nothing"
            )),
            Err(e) => failed.push(format!("urn:repo:{repo}:explain-versions{suffix}: {e}")),
        }
        match json(
            &kernel,
            &format!("urn:repo:{repo}:explain{suffix}"),
            &capability,
            &[("version", tag.as_str())],
        ) {
            Ok(hit) if hit["derived"] == false => *live.entry(repo).or_default() += 1,
            // Unreachable without an LLM bound, and a derivation that somehow
            // succeeded is not an archive hit.
            Ok(_) => failed.push(format!("urn:repo:{repo}:explain{suffix}: DERIVED")),
            Err(_) => *stale.entry(repo).or_default() += 1,
        }
    }

    println!(
        "  {:<16} {:>7} {:>6} {:>6} {:>4}",
        "target root", "listed", "live", "stale", "pr"
    );
    let mut rows: Vec<&String> = listed
        .keys()
        .chain(live.keys())
        .chain(stale.keys())
        .chain(pr.keys())
        .collect();
    rows.sort();
    rows.dedup();
    for name in rows {
        println!(
            "  {:<16} {:>7} {:>6} {:>6} {:>4}",
            name,
            listed.get(name).copied().unwrap_or(0),
            live.get(name).copied().unwrap_or(0),
            stale.get(name).copied().unwrap_or(0),
            pr.get(name).copied().unwrap_or(0)
        );
    }
    println!(
        "\n  {} file/tree entries listed by explain-versions, of which {} still describe \
         today's content and {} are history; {} PR entries carried",
        listed.values().sum::<u64>(),
        live.values().sum::<u64>(),
        stale.values().sum::<u64>(),
        pr.values().sum::<u64>(),
    );
    for line in failed.iter().take(20) {
        println!("      {line}");
    }
    println!("\n  ⚠ remove {} when you are done.", scratch.display());
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} entries the migration carried are not reachable by the reads that matter",
            failed.len()
        ))
    }
}

/// One JSON read through the kernel.
fn json(
    kernel: &Kernel,
    iri: &str,
    capability: &Capability,
    extra: &[(&str, &str)],
) -> Result<serde_json::Value, String> {
    let mut request = Request::new(Verb::Source, Iri::parse(iri).map_err(|e| e.to_string())?)
        .with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
    for (k, v) in extra {
        request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
    }
    let repr = block_on(kernel.issue(request, capability)).map_err(|e| e.to_string())?;
    serde_json::from_slice(&repr.bytes).map_err(|e| e.to_string())
}

/// One migrated explanation, read back from the store — so what gets probed is
/// what actually LANDED rather than what the plan said it would.
struct Entry {
    repo: String,
    path: String,
    tag: String,
    about: String,
}

fn migrated_entries(store: &Store, graph: &GraphName) -> Result<Vec<Entry>, String> {
    let mut repos: BTreeMap<String, String> = BTreeMap::new();
    let mut paths: BTreeMap<String, String> = BTreeMap::new();
    let mut tags: BTreeMap<String, String> = BTreeMap::new();
    let mut abouts: BTreeMap<String, String> = BTreeMap::new();
    let mut explanations = Vec::new();
    for quad in store.quads_for_pattern(None, None, None, Some(graph.as_ref())) {
        let quad = quad.map_err(|e| e.to_string())?;
        let subject = quad.subject.to_string();
        let literal = match &quad.object {
            Term::Literal(l) => Some(l.value().to_string()),
            _ => None,
        };
        match quad.predicate.as_str().strip_prefix(IK) {
            Some("repo") => {
                if let Some(value) = literal {
                    repos.insert(subject, value);
                }
            }
            Some("path") => {
                if let Some(value) = literal {
                    paths.insert(subject, value);
                }
            }
            Some("versionTag") => {
                if let Some(value) = literal {
                    tags.insert(subject, value);
                }
            }
            Some("about") => {
                if let Term::NamedNode(node) = &quad.object {
                    abouts.insert(subject, node.as_str().to_string());
                }
            }
            _ => {
                if quad.predicate.as_str().ends_with("22-rdf-syntax-ns#type")
                    && matches!(&quad.object, Term::NamedNode(n) if n.as_str() == format!("{IK}Explanation"))
                {
                    explanations.push(subject);
                }
            }
        }
    }
    Ok(explanations
        .into_iter()
        .filter_map(|subject| {
            Some(Entry {
                repo: repos.get(&subject)?.clone(),
                tag: tags.get(&subject)?.clone(),
                about: abouts.get(&subject)?.clone(),
                path: paths.get(&subject).cloned().unwrap_or_default(),
            })
        })
        .collect())
}

/// `~` is the shell's, and this example is run with paths typed by hand.
fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    }
}
