//! Spike (ledger #2496): time browse's git READS in-process through gitoxide
//! (`gix`) against the git CLI, on the same repository, and check that both
//! give the same answer. Research only; no library code calls `gix`.
//!
//! ```text
//! cargo run --release --example gix-read-spike --features gix-spike -- <repo> [path] [iterations]
//! ```
//!
//! `path` is a tracked file, root-relative (default `Cargo.toml`). Each row is
//! the median over `iterations` runs (default 50). "gix warm" reuses one
//! opened repository (what a long-lived module would hold); "gix cold" opens
//! it again on every call, the fair comparison with a CLI spawn that
//! rediscovers the repository each time. Findings: docs/research/gix-reads.md.

// ambient: process, fs: a measurement tool, run by hand against a named repository
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(
        args.next()
            .expect("usage: gix-read-spike <repo> [path] [n]"),
    );
    let path = args.next().unwrap_or_else(|| "Cargo.toml".to_string());
    let n: usize = args.next().map_or(50, |n| n.parse().expect("iterations"));

    let repo = gix::open(&root).expect("gix open");
    // Claim 5: run ONLY gix's status, so a hook a repository's config names
    // (`core.fsmonitor`) can be seen to run or not without the CLI in the way.
    if std::env::var_os("SPIKE_GIX_STATUS_ONLY").is_some() {
        println!("gix status: {} paths", status_count(&repo));
        return;
    }
    println!(
        "repo {} | object hash {:?} | path {path} | n={n}",
        root.display(),
        repo.object_hash()
    );
    println!(
        "{:<34} {:>11} {:>11} {:>11}  parity",
        "read", "cli", "gix warm", "gix cold"
    );

    row(
        "rev-parse HEAD",
        n,
        || cli(&root, &["rev-parse", "HEAD"]).trim().to_string(),
        || head(&repo),
        || head(&gix::open(&root).unwrap()),
    );
    row(
        "ls-tree -r HEAD (entry count)",
        n,
        || {
            cli(&root, &["ls-tree", "-r", "HEAD"])
                .lines()
                .count()
                .to_string()
        },
        || ls_tree(&repo).to_string(),
        || ls_tree(&gix::open(&root).unwrap()).to_string(),
    );
    let spec = format!("HEAD:{path}");
    row(
        "cat-file -s HEAD:<path> (size)",
        n,
        || cli(&root, &["cat-file", "-s", &spec]).trim().to_string(),
        || blob_size(&repo, &path),
        || blob_size(&gix::open(&root).unwrap(), &path),
    );
    row(
        "cat-file -p HEAD:<path> (bytes)",
        n,
        || cli(&root, &["cat-file", "-p", &spec]).len().to_string(),
        || blob_len(&repo, &path),
        || blob_len(&gix::open(&root).unwrap(), &path),
    );
    row(
        "log -1 -- <path>",
        n,
        || {
            cli(&root, &["log", "-1", "--format=%H", "--", &path])
                .trim()
                .to_string()
        },
        || last_touch(&repo, &path),
        || last_touch(&gix::open(&root).unwrap(), &path),
    );
    row(
        "status --porcelain (dirty count)",
        n,
        || {
            cli(&root, &["status", "--porcelain"])
                .lines()
                .count()
                .to_string()
        },
        || status_count(&repo).to_string(),
        || status_count(&gix::open(&root).unwrap()).to_string(),
    );
    // browse's git-state endpoint today: three spawns per page.
    row(
        "git-state page (3 reads)",
        n,
        || {
            // Status is timed here; its parity is the status row's.
            let _ = cli(&root, &["status", "--porcelain", "-z", "--", "."]);
            let p = cli(&root, &["rev-parse", "--show-prefix"]);
            let h = cli(&root, &["rev-parse", "HEAD"]);
            format!("{}|{}", p.trim(), &h.trim()[..7.min(h.trim().len())])
        },
        || state(&repo),
        || state(&gix::open(&root).unwrap()),
    );
}

fn cli(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn head(repo: &gix::Repository) -> String {
    repo.head_id().map(|id| id.to_string()).unwrap_or_default()
}

fn ls_tree(repo: &gix::Repository) -> usize {
    let Ok(tree) = repo.head_tree() else { return 0 };
    let mut recorder = gix::traverse::tree::Recorder::default();
    tree.traverse().breadthfirst(&mut recorder).unwrap();
    recorder
        .records
        .iter()
        .filter(|e| !e.mode.is_tree())
        .count()
}

fn entry(repo: &gix::Repository, path: &str) -> Option<gix::ObjectId> {
    let tree = repo.head_tree().ok()?;
    let entry = tree.lookup_entry_by_path(path).ok()??;
    Some(entry.object_id())
}

/// The size from the object header alone: the bound a body is refused by
/// before a byte of it is read (claim 3).
fn blob_size(repo: &gix::Repository, path: &str) -> String {
    entry(repo, path)
        .and_then(|id| repo.find_header(id).ok())
        .map(|h| h.size().to_string())
        .unwrap_or_default()
}

fn blob_len(repo: &gix::Repository, path: &str) -> String {
    entry(repo, path)
        .and_then(|id| repo.find_object(id).ok())
        .map(|o| o.data.len().to_string())
        .unwrap_or_default()
}

/// The newest commit that changed `path`, with git's default simplification:
/// a commit is skipped when its entry for `path` matches ANY parent's.
fn last_touch(repo: &gix::Repository, path: &str) -> String {
    let Ok(head) = repo.head_id() else {
        return String::new();
    };
    let at = |commit: &gix::Commit<'_>| -> Option<gix::ObjectId> {
        let tree = commit.tree().ok()?;
        Some(tree.lookup_entry_by_path(path).ok()??.object_id())
    };
    let walk = repo
        .rev_walk([head.detach()])
        .sorting(gix::revision::walk::Sorting::ByCommitTime(
            Default::default(),
        ))
        .all()
        .unwrap();
    for info in walk {
        let info = info.unwrap();
        let commit = info.object().unwrap();
        let mine = at(&commit);
        let parents: Vec<_> = commit.parent_ids().collect();
        let changed = if parents.is_empty() {
            mine.is_some()
        } else {
            parents.iter().all(|p| {
                let parent = repo.find_commit(*p).unwrap();
                at(&parent) != mine
            })
        };
        if changed {
            return commit.id.to_string();
        }
    }
    String::new()
}

/// Head-to-index and index-to-worktree changes plus untracked files: what
/// `git status --porcelain` lists, as a count.
fn status_count(repo: &gix::Repository) -> usize {
    let Ok(status) = repo.status(gix::progress::Discard) else {
        return 0;
    };
    let iter = status
        .untracked_files(gix::status::UntrackedFiles::Collapsed)
        .into_iter(None)
        .unwrap();
    let mut paths = std::collections::BTreeSet::new();
    for item in iter {
        let item = item.unwrap();
        paths.insert(item.location().to_string());
    }
    paths.len()
}

fn state(repo: &gix::Repository) -> String {
    let _ = status_count(repo);
    let prefix = repo
        .prefix()
        .ok()
        .flatten()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let h = head(repo);
    format!("{prefix}|{}", &h[..7.min(h.len())])
}

fn row(
    name: &str,
    n: usize,
    mut cli: impl FnMut() -> String,
    mut warm: impl FnMut() -> String,
    mut cold: impl FnMut() -> String,
) {
    let (c, a) = median(n, &mut cli);
    let (w, b) = median(n, &mut warm);
    let (k, _) = median(n.min(20), &mut cold);
    let parity = if a == b {
        "same".to_string()
    } else {
        format!("DIFFER cli={a:?} gix={b:?}")
    };
    println!(
        "{name:<34} {:>11} {:>11} {:>11}  {parity}",
        fmt(c),
        fmt(w),
        fmt(k)
    );
}

fn median(n: usize, f: &mut impl FnMut() -> String) -> (Duration, String) {
    let mut last = f(); // warm-up, and the value compared for parity
    let mut times = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        last = f();
        times.push(t.elapsed());
    }
    times.sort();
    (times[times.len() / 2], last)
}

fn fmt(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1000.0)
}
