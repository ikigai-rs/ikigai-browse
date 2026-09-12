//! Keeping `urn:repo:style` fresh: the watch over the config home that cuts
//! the golden threads the stylesheet declares.
//!
//! [`STYLE_IRI`](crate::STYLE_IRI) is `.cacheable()` and `depends_on` one
//! `urn:file:<absolute path>` thread per candidate `a11y.toml` layer — exactly
//! what `ikigai_a11y::load::threads_in` names. Declaring a thread is a promise
//! that SOMETHING cuts it, and nothing did: an edited `a11y.toml` was served
//! stale until the process restarted. ikigai-a11y #8 (2026-09-09) pinned both
//! halves of that — an edit with no cut is served stale, and `kernel.cut()` on
//! exactly those names recomputes — and this module is the host's half of the
//! promise: the platform watcher (FSEvents / inotify) over the config home
//! directory, cutting **by name**. The kernel is neither restarted nor rebuilt;
//! the stylesheet, and whatever a host composed over it, recomputes on its next
//! read and nothing else notices.
//!
//! Why the config home is watched HERE rather than by a workspace watcher: it
//! is an absolute path outside every fs jail — which is why `ikigai-a11y` reads
//! it through `std::fs` in the first place — so a watcher rooted at a workspace
//! never sees it, and its threads are named by absolute path so the two
//! namespaces never collide. Same shape as `ikigai-embedded`'s `WorkspaceWatch`:
//! the watch is split from the thread that drives it, so a test can drive the
//! notification path ([`ConfigWatch::apply_next`]) instead of sleeping.
//!
//! What is watched is the DIRECTORY, non-recursively, and what is matched is the
//! candidate's file NAME. Two reasons. An editor's atomic save (write a
//! temporary, rename it over the target) and an operator CREATING an override
//! that did not exist at mount time both arrive as events on the directory, not
//! on a file that was there to watch. And the platform reports canonical paths
//! (macOS maps `/var` to `/private/var`) while the thread is named after the
//! home the mount was GIVEN — comparing names sidesteps a mismatch that would
//! otherwise cut a thread nothing declared.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

use ikigai_core::Kernel;
use notify::{RecursiveMode, Watcher};

/// What keeps `urn:repo:style` fresh, before it is started: the config home the
/// [`Mount`](crate::Mount) resolved and the app whose override layer it reads.
///
/// Returned by [`Mount::space_watched`](crate::Mount::space_watched) beside the
/// space, from the SAME resolution of the config home — a watch over a different
/// directory than the endpoint reads would cut threads nothing declared. A host
/// starts it once its kernel exists ([`spawn`](Self::spawn)); a test holds the
/// running watch and drives it ([`start`](Self::start)).
#[derive(Debug, Clone)]
pub struct StyleWatch {
    home: Option<PathBuf>,
    app: Option<String>,
}

impl StyleWatch {
    pub(crate) fn new(home: Option<PathBuf>, app: Option<String>) -> Self {
        StyleWatch { home, app }
    }

    /// The config home the stylesheet layers within — `None` when the mount
    /// stated this process has none.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// The application whose `{app}.a11y.toml` layer applies, if the mount named
    /// one.
    pub fn app(&self) -> Option<&str> {
        self.app.as_deref()
    }

    /// The golden threads `urn:repo:style` declares — `ikigai_a11y::load::
    /// threads_in` for this home and app, one `urn:file:` IRI per candidate
    /// layer whether or not the file exists. Empty without a config home: no
    /// candidate files, nothing declared, nothing to watch.
    pub fn threads(&self) -> Vec<String> {
        self.home
            .as_deref()
            .map(|home| ikigai_a11y::load::threads_in(home, self.app()))
            .unwrap_or_default()
    }

    /// Start the watch on `kernel`'s behalf and hand it back to be driven.
    ///
    /// Fails, rather than silently watching nothing, when there is no config home
    /// to watch or the platform watcher cannot be pointed at it — a directory
    /// that does not exist yet is the common case of the second. The host
    /// decides what that means; for a served process it is a warning worth
    /// printing, since the one symptom (an edit that does not land) is silent.
    pub fn start(self, kernel: Arc<Kernel>) -> Result<ConfigWatch, WatchError> {
        let Some(home) = self.home.clone() else {
            return Err(WatchError::NoConfigHome);
        };
        let candidates = ikigai_a11y::load::paths_in(&home, self.app())
            .into_iter()
            .map(|path| {
                let thread = ikigai_a11y::config::file_iri(&path);
                let name = path
                    .file_name()
                    .map(OsString::from)
                    .expect("a layered config path ends in a file name");
                (name, thread)
            })
            .collect();
        let (tx, events) = std::sync::mpsc::channel();
        let platform = |e: notify::Error| WatchError::Platform {
            home: home.clone(),
            reason: e.to_string(),
        };
        let mut watcher = notify::recommended_watcher(move |res| {
            // A closed receiver means the watch was dropped; nothing to report to.
            let _ = tx.send(res);
        })
        .map_err(platform)?;
        watcher
            .watch(&home, RecursiveMode::NonRecursive)
            .map_err(platform)?;
        Ok(ConfigWatch {
            kernel,
            candidates,
            events,
            _watcher: watcher,
        })
    }

    /// [`start`](Self::start), then drive the watch on a detached thread for the
    /// process's lifetime — the served host's one line.
    pub fn spawn(self, kernel: Arc<Kernel>) -> Result<(), WatchError> {
        let watch = self.start(kernel)?;
        std::thread::spawn(move || watch.run());
        Ok(())
    }
}

/// A running watch over a config home: the platform watcher, the channel it
/// reports on, and the kernel whose style threads it cuts.
///
/// Dropping it ends the platform watch.
pub struct ConfigWatch {
    kernel: Arc<Kernel>,
    /// `(file name, thread name)` per candidate layer — the name the platform
    /// reports against, and the name the cut is keyed on.
    candidates: Vec<(OsString, String)>,
    events: Receiver<notify::Result<notify::Event>>,
    /// Held for the watch's lifetime: dropping it ends the platform watch and
    /// closes `events`, which ends [`run`](Self::run).
    _watcher: notify::RecommendedWatcher,
}

/// By hand: the platform watcher and the channel have no useful `Debug`, and the
/// candidates are what a reader of a failure wants to see.
impl fmt::Debug for ConfigWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigWatch")
            .field("threads", &self.threads())
            .finish_non_exhaustive()
    }
}

impl ConfigWatch {
    /// The threads this watch can cut — the same set [`StyleWatch::threads`]
    /// named.
    pub fn threads(&self) -> Vec<String> {
        self.candidates
            .iter()
            .map(|(_, thread)| thread.clone())
            .collect()
    }

    /// The threads a set of changed paths names: a path whose file name is a
    /// candidate layer's names that layer's thread; anything else names nothing.
    /// Pure — the decision, separated from the platform that feeds it.
    fn threads_for<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) -> Vec<String> {
        let mut cut = Vec::new();
        for path in paths {
            let Some(name) = path.file_name() else {
                continue;
            };
            for (candidate, thread) in &self.candidates {
                if candidate == name && !cut.contains(thread) {
                    cut.push(thread.clone());
                }
            }
        }
        cut
    }

    /// Cut the thread of every candidate a change names. Returns the threads
    /// cut — empty for an access (a read changes no content), a path that is not
    /// a candidate layer, or a watcher error (which names no path).
    fn apply(&self, event: notify::Result<notify::Event>) -> Vec<String> {
        let Ok(event) = event else {
            return Vec::new();
        };
        if event.kind.is_access() {
            return Vec::new();
        }
        let cut = self.threads_for(event.paths.iter().map(PathBuf::as_path));
        for thread in &cut {
            self.kernel.cut(thread.as_str());
        }
        cut
    }

    /// Wait up to `timeout` for the watcher's next notification and apply it.
    /// `None` when nothing arrived in time or the watch has ended; otherwise the
    /// threads that notification cut (possibly none — a burst may name other
    /// files first).
    ///
    /// This is the test's way in: blocking on the watcher's OWN notification is
    /// the only way to assert "the read recomputes after the edit" without a
    /// sleep that passes by luck on a fast machine and fails by luck on a slow
    /// one. The deadline is an upper bound on platform latency, not a guess at
    /// it.
    pub fn apply_next(&self, timeout: Duration) -> Option<Vec<String>> {
        self.events
            .recv_timeout(timeout)
            .ok()
            .map(|event| self.apply(event))
    }

    /// Drive the watch until its channel closes — in practice, for the process's
    /// lifetime.
    pub fn run(self) {
        for event in self.events.iter() {
            self.apply(event);
        }
    }
}

/// Why a [`StyleWatch`] could not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchError {
    /// The mount stated this process has no config home: no candidate files,
    /// nothing declared, so there is nothing to watch. Not a defect — the shape
    /// of a CI runner — but a host that expected edits to land should know.
    NoConfigHome,
    /// The platform watcher could not be created or pointed at the config home.
    /// The usual cause is a home directory that does not exist yet: `ikigai-a11y`
    /// treats an absent LAYER as stating nothing, but a watch needs the
    /// DIRECTORY the layers would appear in.
    Platform {
        /// The config home that was to be watched.
        home: PathBuf,
        /// The platform's own words.
        reason: String,
    },
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WatchError::NoConfigHome => {
                write!(
                    f,
                    "no config home: urn:repo:style declares no threads, nothing to watch"
                )
            }
            WatchError::Platform { home, reason } => write!(
                f,
                "cannot watch the config home {} for a11y.toml edits: {reason}",
                home.display()
            ),
        }
    }
}

impl std::error::Error for WatchError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mount, STYLE_IRI};
    use futures::executor::block_on;
    use ikigai_core::{ArgRef, Capability, Fallback, Iri, Request, Verb};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Long enough to be an upper bound on FSEvents / inotify delivery, short
    /// enough that a platform that never delivers fails the suite rather than
    /// hanging it.
    const DEADLINE: Duration = Duration::from_secs(10);
    const APP: &str = "browse-test";

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-watch-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Both readers of the config, over ONE scratch home: this crate's
    /// stylesheet and `ikigai-a11y`'s own `urn:a11y:config`. They declare the
    /// same thread names, so one cut must refresh both — which is the point of
    /// cutting by name rather than rebuilding anything.
    fn kernel_over(home: &Path) -> (Arc<Kernel>, StyleWatch) {
        let (space, style) = Mount::new(Vec::<(String, PathBuf)>::new())
            .app(APP)
            .config_home(Some(home.to_path_buf()))
            .space_watched();
        let a11y = ikigai_a11y::space_with(Arc::new(ikigai_a11y::A11yHandle::new(
            Some(home.to_path_buf()),
            Some(APP.to_string()),
        )));
        let kernel = Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(space),
            Arc::new(a11y),
        ])));
        (Arc::new(kernel), style)
    }

    fn cap() -> Capability {
        Capability::scoped(["urn:cap:browse:read:demo", ikigai_a11y::CAP_READ])
    }

    fn css(kernel: &Kernel) -> String {
        let request = Request::new(Verb::Source, Iri::parse(STYLE_IRI).unwrap());
        let repr = block_on(kernel.issue(request, &cap())).expect("the stylesheet resolves");
        String::from_utf8_lossy(&repr.bytes).into_owned()
    }

    /// The effective dark theme per `urn:a11y:config` (the app layer applies).
    fn dark_theme(kernel: &Kernel) -> String {
        let request = Request::new(Verb::Source, Iri::parse(ikigai_a11y::CONFIG_IRI).unwrap())
            .with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
        let repr = block_on(kernel.issue(request, &cap())).expect("the config resolves");
        let json: serde_json::Value = serde_json::from_slice(&repr.bytes).unwrap();
        json["theme"]["dark"]
            .as_str()
            .expect("theme.dark is a string")
            .to_string()
    }

    /// Drain the watcher's notifications until one cuts `thread`. A burst may
    /// name other files first (a write is two events on some platforms), so this
    /// is bounded by count AND per-wait, never by a clock.
    fn cut_arrives(watch: &ConfigWatch, thread: &str) -> bool {
        for _ in 0..64 {
            match watch.apply_next(DEADLINE) {
                Some(threads) if threads.iter().any(|t| t == thread) => return true,
                Some(_) => continue,
                None => return false,
            }
        }
        false
    }

    /// The chip's claim, end to end: an edited `a11y.toml` reaches the NEXT read
    /// of both the stylesheet and `urn:a11y:config`, through the watcher's own
    /// notification path, with the kernel neither restarted nor rebuilt.
    ///
    /// The stale reads in the middle are the premise, not a wish — they prove
    /// the recompute at the end is the watcher's doing and not an uncached
    /// endpoint's.
    #[test]
    fn an_edited_a11y_toml_reaches_the_next_read_through_the_watcher() {
        let home = temp_dir();
        let shared = home.join("a11y.toml");
        std::fs::write(&shared, "[theme]\ndark = \"Nord\"\n").unwrap();
        let (kernel, style) = kernel_over(&home);
        let watch = style
            .start(Arc::clone(&kernel))
            .expect("the platform watcher must start on the scratch home");

        assert_eq!(dark_theme(&kernel), "Nord");
        let nord = css(&kernel);

        // std::fs on purpose: this IS the out-of-band edit the kernel cannot see.
        std::fs::write(&shared, "[theme]\ndark = \"Dracula\"\n").unwrap();
        assert_eq!(
            dark_theme(&kernel),
            "Nord",
            "the cached config must hold until its thread is cut"
        );
        assert_eq!(css(&kernel), nord, "and so must the stylesheet");

        let thread = ikigai_a11y::config::file_iri(&shared);
        assert!(
            cut_arrives(&watch, &thread),
            "the watcher never reported {thread} within its deadline"
        );
        assert_eq!(dark_theme(&kernel), "Dracula", "the next read recomputes");
        assert_ne!(css(&kernel), nord, "the stylesheet follows the edit");
        std::fs::remove_dir_all(&home).ok();
    }

    /// An override CREATED after mount — the candidate that did not exist when
    /// the thread was declared — lands too. This is why the candidates are
    /// declared before they are written, and why the DIRECTORY is what is
    /// watched: a file that is not there yet cannot be.
    #[test]
    fn a_created_app_override_reaches_the_next_read_through_the_watcher() {
        let home = temp_dir();
        std::fs::write(home.join("a11y.toml"), "[theme]\ndark = \"Nord\"\n").unwrap();
        let (kernel, style) = kernel_over(&home);
        let watch = style
            .start(Arc::clone(&kernel))
            .expect("the platform watcher must start on the scratch home");
        assert_eq!(dark_theme(&kernel), "Nord");

        let override_file = home.join(format!("{APP}.a11y.toml"));
        std::fs::write(&override_file, "[theme]\ndark = \"Dracula\"\n").unwrap();
        assert_eq!(dark_theme(&kernel), "Nord", "stale until cut — the premise");

        let thread = ikigai_a11y::config::file_iri(&override_file);
        assert!(
            cut_arrives(&watch, &thread),
            "the watcher never reported {thread} within its deadline"
        );
        assert_eq!(dark_theme(&kernel), "Dracula", "the new layer applies");
        std::fs::remove_dir_all(&home).ok();
    }

    /// The decision, apart from the platform: a candidate's file name cuts its
    /// thread whatever directory prefix the platform reports it under; a
    /// neighbour in the home, the home itself, and another app's override cut
    /// nothing; one event naming a file twice cuts it once.
    #[test]
    fn only_a_candidate_layers_name_cuts_and_it_cuts_once() {
        let home = temp_dir();
        let (kernel, style) = kernel_over(&home);
        let watch = style.start(kernel).expect("watch starts");
        let shared = ikigai_a11y::config::file_iri(&home.join("a11y.toml"));
        let app = ikigai_a11y::config::file_iri(&home.join(format!("{APP}.a11y.toml")));
        assert_eq!(watch.threads(), vec![shared.clone(), app.clone()]);

        // The canonical spelling of a home under /var: a different prefix, the
        // same file name, the declared thread.
        let canonical = Path::new("/private").join(home.strip_prefix("/").unwrap_or(&home));
        assert_eq!(
            watch.threads_for([canonical.join("a11y.toml").as_path()]),
            vec![shared.clone()]
        );
        assert_eq!(
            watch.threads_for([
                home.join("a11y.toml").as_path(),
                home.join("a11y.toml").as_path(),
                home.join(format!("{APP}.a11y.toml")).as_path(),
            ]),
            vec![shared, app]
        );
        assert!(watch
            .threads_for([
                home.as_path(),
                home.join("cms.toml").as_path(),
                home.join("other-app.a11y.toml").as_path(),
                home.join("a11y.toml.swp").as_path(),
            ])
            .is_empty());
        std::fs::remove_dir_all(&home).ok();
    }

    /// A mount that stated it has NO config home declares nothing and has
    /// nothing to watch — and says so, rather than watching nothing quietly.
    #[test]
    fn no_config_home_means_nothing_to_watch_and_start_says_so() {
        let (space, style) = Mount::new(Vec::<(String, PathBuf)>::new())
            .app(APP)
            .config_home(None)
            .space_watched();
        assert!(style.threads().is_empty());
        assert_eq!(style.home(), None);
        assert_eq!(style.app(), Some(APP));
        let kernel = Arc::new(Kernel::new(Arc::new(space)));
        assert_eq!(style.start(kernel).err(), Some(WatchError::NoConfigHome));
    }

    /// A config home that does not exist yet cannot be watched, and the error
    /// names the directory — the operator's next step is to create it.
    #[test]
    fn a_missing_config_home_is_a_named_error_not_a_silent_no_watch() {
        let home = temp_dir().join("not-yet");
        let (kernel, style) = kernel_over(&home);
        // Declared regardless: the threads are the promise, the watch is the keeping.
        assert_eq!(style.threads().len(), 2);
        match style.start(kernel) {
            Err(WatchError::Platform {
                home: named,
                reason,
            }) => {
                assert_eq!(named, home);
                assert!(!reason.is_empty());
            }
            other => panic!("expected a Platform error naming the home, got {other:?}"),
        }
    }

    /// `space()` is `space_watched()` with the watch dropped: the same space,
    /// the same threads declared, and a host that keeps calling it keeps the
    /// pre-0.3.2 behaviour (a restart to pick up an edit) rather than a new one.
    #[test]
    fn space_is_space_watched_without_the_watch() {
        let home = temp_dir();
        let a = Kernel::new(Arc::new(
            Mount::new(Vec::<(String, PathBuf)>::new())
                .app(APP)
                .config_home(Some(home.clone()))
                .space(),
        ));
        let (space, style) = Mount::new(Vec::<(String, PathBuf)>::new())
            .app(APP)
            .config_home(Some(home.clone()))
            .space_watched();
        let b = Kernel::new(Arc::new(space));
        let request = || Request::new(Verb::Source, Iri::parse(STYLE_IRI).unwrap());
        let ra = block_on(a.issue(request(), &cap())).unwrap();
        let rb = block_on(b.issue(request(), &cap())).unwrap();
        assert_eq!(ra.bytes, rb.bytes);
        let declared: Vec<String> = rb.threads().iter().map(|t| t.to_string()).collect();
        let mut named = style.threads();
        named.sort();
        assert_eq!(declared, named, "the watch names what the sheet declared");
        std::fs::remove_dir_all(&home).ok();
    }
}
