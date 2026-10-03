//! The watch over the browse roots: what makes a cached filesystem read safe to serve.
//!
//! # Why a server needs this and a library cannot have it
//!
//! `ikigai-browse` declares its browsing reads **live and uncacheable** — `tree`, `file`,
//! `hash`, `state` return a bare `Representation`, so the kernel recomputes each one on every
//! resolution. That is the only honest declaration a LIBRARY can make: cacheability is a
//! promise that something will notice when the answer changes, and a library linked into an
//! unknown host cannot know whether anything is watching the disk.
//!
//! A server can know, because a server is where the watcher lives. So gonk runs one
//! ([`RootWatch`]), and [`crate::browse::cached_reads`] declares the reads of a **watched**
//! root cacheable under the threads this module cuts. The two halves are written to fail
//! closed together: a root whose platform watcher does not start is not in the watched set,
//! and its reads keep browse's own uncacheable declaration. There is no path on which a read
//! is cached and unwatched.
//!
//! This is the live bug ledger #246 named, taken at the other end. There, `urn:file:*` was
//! `.cacheable().depends_on(target)` on a kernel with no watcher, so a replaced stylesheet
//! was served stale until the unit restarted — a declared thread nothing cut. Here nothing is
//! declared until something cuts it.
//!
//! # The shape
//!
//! Reimplemented from the design in closed `ikigai-cli` PR #321 (`WorkspaceWatch`:
//! `start` sets up the platform watch, `apply` cuts what a change names, `run` drives it for
//! the process's lifetime), which is in a workspace this crate cannot depend on. Two
//! differences, both from the setting rather than from taste:
//!
//! - **Many roots, one channel.** gonk serves a set of named roots, so `start` takes them
//!   all and reports which ones it got, and the path→thread map is "which root contains this
//!   path" rather than one root-relative rewrite.
//! - **Threads per ROOT, not per file.** A change anywhere under `core` cuts that root's
//!   threads, and every cached read of that root recomputes. Coarse on purpose: the finer
//!   thread would have to be the file's own IRI, and building that from a filesystem path means
//!   `ikigai-browse`'s percent-encoding, which is private to that crate (`iri_encode`). A thread
//!   computed one way at the read and another way at the cut is a cache that looks fresh and is
//!   not — strictly worse than invalidating too much.
//!
//! # ★ What the watch ignores, and why the reads had to learn the same rule (ledger #667)
//!
//! With every ikigai repository a root, the watch saw every file a cargo build wrote under any
//! `target/`, and each one cut its root — 28,467 cuts in 90 seconds of one build in one root,
//! measured. So the watch now IGNORES WHAT GIT IGNORES, plus two things always: anything inside
//! a `target/` directory and anything inside `.git/` except the files `git status` reads. And
//! an ignored change cutting nothing is only safe if no cached read can SEE that change — the
//! other half of "no path on which a read is cached and unwatched". So every path is sorted
//! into one of five kinds ([`Seen`]), and the reads in [`crate::browse::cached_reads`] are
//! cached only where the changes that could alter them still cut:
//!
//! | change at… | [`Seen`] | cuts |
//! |---|---|---|
//! | a path git does not ignore | `Visible` | both threads |
//! | an ignored path whose parent is not ignored (`Cargo.lock` in a library, `target` itself) | `Edge` | both — it is an ENTRY in a listing that is cached, with a size |
//! | inside an ignored directory, by `.gitignore` (an mdbook `book/`, a `pkg/`) | `Hidden` | the wide thread only |
//! | inside `target/` or `.git/`, at any depth | `Build` | nothing |
//! | `.git/HEAD`, `index`, `packed-refs`, `config`, `info/exclude`, `refs/…` of the root | `GitState` | both |
//!
//! The two threads ([`root_thread`], [`wide_thread`]) exist because the reads differ in what
//! they can see. `tree` and `file` show exactly what a listing shows, so they hang from the
//! narrow thread and a change deep inside an ignored directory leaves them cached. `hash` walks
//! every file under a directory except the names `ikigai-browse` itself skips (`target`,
//! `.git` and a few more, `hash::default_ignore`), and `state` is `git status`, which reports a
//! tracked file wherever it is — so both hang from the wide thread, which only build output
//! does not cut. `Build` is safe to drop for every read because each read that could see a
//! path inside `target/` or `.git/` is refused the cache by the same predicate
//! ([`in_build_output`]).
//!
//! ⚠ The `.gitignore` files are read once per root at start (a walk that skips what they
//! ignore) and again when one of them, or `.git/info/exclude`, changes — BEFORE the cut that
//! change makes, so no read can be cached under the old rule after it. The global excludes
//! file (`core.excludesFile`) is read at start only: editing it moves what `git status` hides,
//! and the watch catches up at the next restart. Matching is `ignore`'s (the ripgrep
//! implementation); where it and git disagree it errs toward NOT ignored, which costs a cut and
//! never a stale read.
//!
//! # The badge's epochs
//!
//! The header badge counts pending findings across every root on every poll, and a findings
//! read is not cacheable by the kernel (it reads a graph the sharer writes, so the store
//! answers it `Expiry::Always`, and that propagates into anything composed over it). So the
//! badge keeps its own per-root memo ([`crate::queue::Badge`]), and [`Epochs`] is what tells it
//! a root's count may have moved: this watch bumps a root whenever it cuts that root's narrow
//! thread, and the browse overlay bumps one root (or every root, when a write does not say
//! which) after any write to the browse family.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::Match;
use ikigai_core::Kernel;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// The narrow golden thread of `root`: what a listing or a file read can see. Every cached
/// `tree` and `file` read of the root depends on it.
pub fn root_thread(root: &str) -> String {
    format!("urn:iki:gonk:browse:root:{root}")
}

/// The wide golden thread of `root`: every change but build output. The cached `hash` and
/// `state` reads depend on it, because each can see a file a `.gitignore` hides.
pub fn wide_thread(root: &str) -> String {
    format!("urn:iki:gonk:browse:root:{root}:wide")
}

/// Directory names whose CONTENTS no cached read may see and no change may cut for: build
/// output and git's own store. Both are in `ikigai-browse`'s `hash::default_ignore`, which is
/// what makes them safe to drop for the hash reads as well as the listings.
pub const BUILD_DIRS: [&str; 2] = ["target", ".git"];

/// The files under the root's own `.git/` that `git status` (and so `urn:repo:{root}:state`)
/// reads, besides everything under `refs/`. A change to one of these cuts.
const GIT_STATE: [&str; 5] = ["HEAD", "index", "packed-refs", "config", "info/exclude"];

/// What a change at a path is, for the threads it cuts — see the module's table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// Not ignored: every cached read may see it.
    Visible,
    /// Ignored, in a directory that is not: an entry in a cached listing.
    Edge,
    /// Inside a directory a `.gitignore` ignores: only `hash` and `state` can see it.
    Hidden,
    /// Inside `target/` or `.git/` (bar [`Seen::GitState`]): no cached read sees it.
    Build,
    /// One of the root's own git state files.
    GitState,
}

/// Whether a root-relative path lies in, or IS, build output: any component in
/// [`BUILD_DIRS`]. A read of such a path is never cached (its contents change without a cut).
pub fn in_build_output(rel: &Path) -> bool {
    rel.components()
        .any(|c| matches!(c, Component::Normal(name) if BUILD_DIRS.iter().any(|d| name == *d)))
}

/// One root's ignore rules: its `.gitignore` files, `.git/info/exclude` and the global
/// excludes file, deepest first — the order git consults them in.
pub struct RootIgnore {
    dir: PathBuf,
    global: Gitignore,
    rules: RwLock<Vec<Gitignore>>,
}

impl std::fmt::Debug for RootIgnore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rules = self.rules.read().map(|r| r.len()).unwrap_or(0);
        f.debug_struct("RootIgnore")
            .field("dir", &self.dir)
            .field("files", &rules)
            .finish()
    }
}

impl RootIgnore {
    /// Read the rules of the root at `dir` (canonical).
    pub fn load(dir: &Path) -> RootIgnore {
        let ignore = RootIgnore {
            dir: dir.to_path_buf(),
            global: Gitignore::global().0,
            rules: RwLock::new(Vec::new()),
        };
        ignore.reload();
        ignore
    }

    /// Re-read every `.gitignore` under the root and its `.git/info/exclude`.
    ///
    /// The walk honors the rules it is collecting (so a `.gitignore` inside an ignored
    /// directory is not read, as git does not read it) and never enters build output.
    pub fn reload(&self) {
        let mut files: Vec<PathBuf> = ignore::WalkBuilder::new(&self.dir)
            .hidden(false)
            .ignore(false)
            .parents(false)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(true)
            .require_git(false)
            .filter_entry(|entry| {
                !(entry.depth() > 0
                    && entry.file_type().is_some_and(|t| t.is_dir())
                    && BUILD_DIRS.iter().any(|d| entry.file_name() == *d))
            })
            .build()
            .flatten()
            .filter(|entry| entry.file_name() == ".gitignore")
            .map(|entry| entry.into_path())
            .collect();
        // Deepest first: a nested file overrides the root's, as in git.
        files.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        let mut rules: Vec<Gitignore> = files
            .iter()
            .filter_map(|file| {
                let mut builder = GitignoreBuilder::new(file.parent()?);
                builder.add(file);
                builder.build().ok()
            })
            .collect();
        let exclude = self.dir.join(".git/info/exclude");
        if exclude.is_file() {
            let mut builder = GitignoreBuilder::new(&self.dir);
            builder.add(&exclude);
            if let Ok(rules_) = builder.build() {
                rules.push(rules_);
            }
        }
        if let Ok(mut held) = self.rules.write() {
            *held = rules;
        }
    }

    /// Whether git would ignore `rel` (root-relative). The first file, deepest first, that
    /// says anything decides; a file whose directory does not contain `rel` is skipped.
    pub fn ignored(&self, rel: &Path, is_dir: bool) -> bool {
        let abs = self.dir.join(rel);
        let rules = match self.rules.read() {
            Ok(rules) => rules,
            // A poisoned lock means a reload panicked mid-way: say NOT ignored, which cuts.
            Err(_) => return false,
        };
        for file in rules.iter() {
            // Relative to that file's own directory, so the matcher never sees a path outside
            // its root (which it would panic on).
            let Ok(local) = abs.strip_prefix(file.path()) else {
                continue;
            };
            if let Some(decided) = decide(file, local, is_dir) {
                return decided;
            }
        }
        // The global excludes last, and root-relative: its own directory is wherever this
        // process was started, which says nothing about the repository.
        decide(&self.global, rel, is_dir).unwrap_or(false)
    }

    /// Sort a root-relative path into the kind of change it is.
    pub fn classify(&self, rel: &Path, is_dir: bool) -> Seen {
        if let Some(git) = rel
            .strip_prefix(".git")
            .ok()
            .filter(|g| !g.as_os_str().is_empty())
        {
            let state = GIT_STATE.iter().any(|f| git == Path::new(f)) || git.starts_with("refs");
            return if state { Seen::GitState } else { Seen::Build };
        }
        if rel.parent().is_some_and(in_build_output) {
            return Seen::Build;
        }
        if !self.ignored(rel, is_dir) {
            return Seen::Visible;
        }
        match rel.parent() {
            Some(parent) if !parent.as_os_str().is_empty() && self.ignored(parent, true) => {
                Seen::Hidden
            }
            _ => Seen::Edge,
        }
    }
}

/// What one ignore file says about a path relative to its directory: `Some(true)` ignored,
/// `Some(false)` re-included by a `!` rule, `None` silent.
fn decide(file: &Gitignore, local: &Path, is_dir: bool) -> Option<bool> {
    if local.as_os_str().is_empty() || local.has_root() {
        return None;
    }
    match file.matched_path_or_any_parents(local, is_dir) {
        Match::Ignore(_) => Some(true),
        Match::Whitelist(_) => Some(false),
        Match::None => None,
    }
}

/// A root that is being watched: its name, the CANONICAL directory (symlinks resolved —
/// macOS reports `/private/var` where the config said `/var`, and a prefix test against the
/// un-canonicalized path would match nothing), and its ignore rules.
#[derive(Debug, Clone)]
pub struct Watched {
    /// The root's name, as it appears in `urn:repo:{name}:…`.
    pub name: String,
    /// The canonical directory.
    pub dir: PathBuf,
    /// What the watch ignores under it — shared with the reads it decides the cache for.
    pub ignore: Arc<RootIgnore>,
}

/// A root the platform watcher refused, and why — reported at startup, never fatal.
#[derive(Debug, Clone)]
pub struct Unwatched {
    /// The root's name.
    pub name: String,
    /// What the platform said.
    pub reason: String,
}

/// Per-root counters the header badge reads to know whether a memoized count may have moved.
///
/// A root's epoch rises when the watch cuts its narrow thread, when a browse write names it,
/// and when a write that names no root ([`Epochs::touch_all`]) or a finding decision this
/// table cannot place happens. The badge stamps a count with the epoch it read BEFORE reading,
/// so a write that lands during the read leaves the count stale-stamped and the next poll
/// recounts.
#[derive(Debug, Default)]
pub struct Epochs {
    roots: BTreeMap<String, AtomicU64>,
    /// Finding id → root, learned from the rows the badge counted: a decision posts
    /// `urn:iki:finding:{id}`, which does not say which repository it belongs to.
    owners: Mutex<HashMap<String, String>>,
}

impl Epochs {
    /// Counters for these roots, all at zero.
    pub fn new<I: IntoIterator<Item = String>>(roots: I) -> Epochs {
        Epochs {
            roots: roots.into_iter().map(|r| (r, AtomicU64::new(0))).collect(),
            owners: Mutex::default(),
        }
    }

    /// The root's epoch now, or `None` for a root this table does not hold (never memoized).
    pub fn stamp(&self, root: &str) -> Option<u64> {
        self.roots.get(root).map(|e| e.load(Ordering::SeqCst))
    }

    /// Something under `root` changed.
    pub fn touch(&self, root: &str) {
        if let Some(epoch) = self.roots.get(root) {
            epoch.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Something changed and it is not known where.
    pub fn touch_all(&self) {
        for epoch in self.roots.values() {
            epoch.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A finding was written: its root if this table has seen the id, every root if not.
    pub fn touch_finding(&self, id: &str) {
        let owner = self.owners.lock().ok().and_then(|o| o.get(id).cloned());
        match owner {
            Some(root) => self.touch(&root),
            None => self.touch_all(),
        }
    }

    /// Remember which root these finding ids belong to.
    pub fn learn<'a, I: IntoIterator<Item = &'a str>>(&self, root: &str, ids: I) {
        if let Ok(mut owners) = self.owners.lock() {
            for id in ids {
                owners.insert(id.to_string(), root.to_string());
            }
        }
    }
}

/// A live watch over a set of browse roots.
///
/// Split from the thread that drives it ([`Self::spawn`]) so a test can drive it instead:
/// [`Self::apply_next`] blocks on the platform's own notification and applies the cut, which
/// is the only way to assert "the read recomputes after the change" without a sleep that
/// passes by luck on a fast machine and fails by luck on a slow one.
pub struct RootWatch {
    watched: Vec<Watched>,
    epochs: Arc<Epochs>,
    events: Receiver<notify::Result<notify::Event>>,
    /// Held for the watch's lifetime: dropping these ends the platform watches and closes
    /// `events`. One per root — `notify` will watch several paths on one watcher, but a
    /// per-root watcher is what makes one root's failure leave the others watched.
    _watchers: Vec<RecommendedWatcher>,
}

impl RootWatch {
    /// Start watching `roots`, recursively. Returns the watch and the roots it did NOT get;
    /// [`Self::watched`] is the set whose reads may be cached.
    pub fn start(roots: &[(String, PathBuf)]) -> (RootWatch, Vec<Unwatched>) {
        let (tx, events) = std::sync::mpsc::channel();
        let (mut watched, mut refused, mut watchers) = (Vec::new(), Vec::new(), Vec::new());
        for (name, dir) in roots {
            let dir = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            let tx = tx.clone();
            let started = notify::recommended_watcher(move |res| {
                let _ = tx.send(res);
            })
            .and_then(|mut watcher| {
                watcher.watch(&dir, RecursiveMode::Recursive)?;
                Ok(watcher)
            });
            match started {
                Ok(watcher) => {
                    watchers.push(watcher);
                    watched.push(Watched {
                        name: name.clone(),
                        ignore: Arc::new(RootIgnore::load(&dir)),
                        dir,
                    });
                }
                Err(e) => refused.push(Unwatched {
                    name: name.clone(),
                    reason: e.to_string(),
                }),
            }
        }
        let epochs = Arc::new(Epochs::new(watched.iter().map(|w| w.name.clone())));
        (
            RootWatch {
                watched,
                epochs,
                events,
                _watchers: watchers,
            },
            refused,
        )
    }

    /// The roots this watch actually holds — the set whose reads may be declared cacheable.
    pub fn watched(&self) -> &[Watched] {
        &self.watched
    }

    /// The badge's epochs for the watched roots — bumped by this watch and by the browse
    /// overlay, read by the badge.
    pub fn epochs(&self) -> Arc<Epochs> {
        Arc::clone(&self.epochs)
    }

    /// Cut the threads of every root a change names, per the module's table. Returns the
    /// threads cut — empty for an access (a read changes no content), a path under no watched
    /// root, a change only build output saw, or a watcher error (which names no path).
    pub fn apply(&self, kernel: &Kernel, event: notify::Result<notify::Event>) -> Vec<String> {
        let Ok(event) = event else {
            return Vec::new();
        };
        if event.kind.is_access() {
            return Vec::new();
        }
        let folder = matches!(
            event.kind,
            notify::EventKind::Create(notify::event::CreateKind::Folder)
                | notify::EventKind::Remove(notify::event::RemoveKind::Folder)
        );
        let mut cut: Vec<String> = Vec::new();
        for path in &event.paths {
            let is_dir = folder || std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
            for root in self.watched.iter().filter(|r| path.starts_with(&r.dir)) {
                let Ok(rel) = path.strip_prefix(&root.dir) else {
                    continue;
                };
                // A rule file changed: re-read the rules BEFORE cutting, so nothing is cached
                // under the old ones after this.
                if rel.file_name().is_some_and(|n| n == ".gitignore")
                    || rel == Path::new(".git/info/exclude")
                {
                    root.ignore.reload();
                }
                let threads: &[fn(&str) -> String] = match root.ignore.classify(rel, is_dir) {
                    Seen::Visible | Seen::Edge | Seen::GitState => &[root_thread, wide_thread],
                    Seen::Hidden => &[wide_thread],
                    Seen::Build => &[],
                };
                for thread in threads.iter().map(|t| t(&root.name)) {
                    if !cut.contains(&thread) {
                        kernel.cut(thread.as_str());
                        if thread == root_thread(&root.name) {
                            self.epochs.touch(&root.name);
                        }
                        cut.push(thread);
                    }
                }
            }
        }
        cut
    }

    /// Wait up to `timeout` for the next notification and apply it. `None` when nothing
    /// arrived in time or the watch has ended; otherwise what [`Self::apply`] cut.
    ///
    /// Public, not `#[cfg(test)]`: gonk's watch test is an integration test, which is a
    /// separate crate and cannot see a test-gated item. The seam is the same one
    /// `ikigai-browse`'s own `StyleWatch` publishes, for the same reason.
    pub fn apply_next(&self, kernel: &Kernel, timeout: Duration) -> Option<Vec<String>> {
        self.events
            .recv_timeout(timeout)
            .ok()
            .map(|event| self.apply(kernel, event))
    }

    /// Drive the watch until its channel closes — in practice, for the process's lifetime.
    pub fn run(self, kernel: Arc<Kernel>) {
        while let Ok(event) = self.events.recv() {
            self.apply(&kernel, event);
        }
    }

    /// [`Self::run`] on its own thread.
    pub fn spawn(self, kernel: Arc<Kernel>) {
        std::thread::spawn(move || self.run(kernel));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_that_cannot_be_watched_is_reported_and_left_out() {
        let missing = std::env::temp_dir().join("ikigai-gonk-no-such-root-9f3a");
        let _ = std::fs::remove_dir_all(&missing);
        let (watch, refused) = RootWatch::start(&[("ghost".to_string(), missing)]);
        assert!(watch.watched().is_empty(), "nothing is watched");
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0].name, "ghost");
        assert_eq!(watch.epochs().stamp("ghost"), None, "and it has no epoch");
    }

    #[test]
    fn a_path_under_two_nested_roots_names_both() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let outer = dir.path().to_path_buf();
        let inner = outer.join("inner");
        std::fs::create_dir(&inner).expect("the inner root");
        let (watch, refused) = RootWatch::start(&[
            ("outer".to_string(), outer),
            ("inner".to_string(), inner.clone()),
        ]);
        assert!(refused.is_empty(), "{refused:?}");
        // Canonical, because that is the shape `notify` reports and the shape `start`
        // matches against — on macOS a temp dir is `/var/…`, which is a symlink to
        // `/private/var/…`, and a raw path would match no root at all.
        let inner = inner.canonicalize().expect("the inner root exists");
        let kernel = Kernel::new(Arc::new(ikigai_core::EndpointSpace::new()));
        let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
            .add_path(inner.join("file.txt"));
        let cut = watch.apply(&kernel, Ok(event));
        for root in ["outer", "inner"] {
            assert!(
                cut.contains(&root_thread(root)),
                "a nested change cuts both roots: {cut:?}"
            );
        }
    }

    #[test]
    fn the_threads_name_the_root() {
        assert_eq!(root_thread("core"), "urn:iki:gonk:browse:root:core");
        assert_eq!(wide_thread("core"), "urn:iki:gonk:browse:root:core:wide");
    }

    /// The table in the module doc, one row per kind, against a root with a `.gitignore`.
    #[test]
    fn every_kind_of_path_is_sorted_as_the_table_says() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join(".gitignore"), "/book\nCargo.lock\n*.log\n")
            .expect(".gitignore");
        std::fs::create_dir_all(dir.path().join("book/html")).expect("book/");
        std::fs::create_dir_all(dir.path().join("docs")).expect("docs/");
        std::fs::write(dir.path().join("docs/.gitignore"), "scratch/\n").expect("nested");
        std::fs::create_dir_all(dir.path().join("docs/scratch")).expect("docs/scratch/");
        let ignore = RootIgnore::load(&dir.path().canonicalize().expect("canonical"));
        let seen = |rel: &str, is_dir: bool| ignore.classify(Path::new(rel), is_dir);

        assert_eq!(seen("src/lib.rs", false), Seen::Visible);
        assert_eq!(
            seen("target", true),
            Seen::Visible,
            "not in this .gitignore"
        );
        assert_eq!(seen("target/debug/gonk", false), Seen::Build);
        assert_eq!(
            seen("crates/a/target/x.o", false),
            Seen::Build,
            "at any depth"
        );
        assert_eq!(seen(".git/objects/ab/cdef", false), Seen::Build);
        assert_eq!(seen(".git/index", false), Seen::GitState);
        assert_eq!(seen(".git/HEAD", false), Seen::GitState);
        assert_eq!(seen(".git/refs/heads/main", false), Seen::GitState);
        assert_eq!(seen("Cargo.lock", false), Seen::Edge, "listed in the root");
        assert_eq!(seen("src/debug.log", false), Seen::Edge, "listed in src/");
        assert_eq!(
            seen("book", true),
            Seen::Edge,
            "the ignored directory's own entry"
        );
        assert_eq!(seen("book/html/index.html", false), Seen::Hidden);
        assert_eq!(
            seen("docs/scratch/a.md", false),
            Seen::Hidden,
            "a nested .gitignore"
        );
        assert_eq!(seen("docs/guide.md", false), Seen::Visible);
    }

    #[test]
    fn build_output_is_any_component_named_target_or_dot_git() {
        assert!(in_build_output(Path::new("target")));
        assert!(in_build_output(Path::new("a/target/b")));
        assert!(in_build_output(Path::new(".git/HEAD")));
        assert!(!in_build_output(Path::new("src/targets.rs")));
        assert!(!in_build_output(Path::new("")));
    }

    #[test]
    fn a_finding_decision_touches_its_root_when_known_and_every_root_when_not() {
        let epochs = Epochs::new(["a".to_string(), "b".to_string()]);
        epochs.learn("a", ["f1"]);
        epochs.touch_finding("f1");
        assert_eq!((epochs.stamp("a"), epochs.stamp("b")), (Some(1), Some(0)));
        epochs.touch_finding("unknown");
        assert_eq!((epochs.stamp("a"), epochs.stamp("b")), (Some(2), Some(1)));
        assert_eq!(epochs.stamp("c"), None);
    }
}
