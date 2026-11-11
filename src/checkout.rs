//! The checkout a root serves, read from disk: the ONE place browse touches the
//! filesystem or runs `git` (Hypothesis H, ledger #2417).
//!
//! Every read goes through a [`Jailed`] path, and there are only three ways to
//! get one: [`Jailed::resolve`] (a configured root and a root-relative path,
//! judged by the jail), [`Jailed::root`] (the root itself) and
//! [`Jailed::child`] (one entry name below a jailed directory, judged again).
//! Nothing in this module reads, lists, opens or runs `git` in a bare path, so
//! the two named exceptions below cover reads CONFINED to a configured root,
//! and a filesystem or process call anywhere else in the crate is a violation
//! the ambient gate names. Keep it that way: a method added here that takes a
//! `&Path` to read would turn the exception back into a thin wrapper.
//!
//! Previously the jail checked a canonical path and later read it by name;
//! a concurrent symlink swap could escape. Canonicalization now only selects
//! an in-root, non-`.git` name. Every use walks that name from a pinned root
//! directory handle with `openat(O_NOFOLLOW)` on EVERY component. A swap can
//! fail the read or select another in-root entry, but cannot redirect it through
//! a symlink. This is name confinement, not a snapshot or a defense against
//! hard links, mount changes, or an attacker controlling the configured root's
//! ancestors when the root is initially opened. Git's own repository reads are
//! outside this filesystem jail; its working directory is pinned separately.
//! Every git run also refuses a bare repository it would discover (tracked
//! files can spell one), and drops inherited repository-selecting variables and
//! GitHub tokens: see [`harden_git`] (ledger #3492).
//! Platforms without the Unix handle-relative implementation refuse checkout
//! access explicitly; they never fall back to pathname-based reads.

use std::fs::File;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::path::Component;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
#[cfg(unix)]
use std::sync::Arc;

use ikigai_core::{Error, Result};
#[cfg(unix)]
use rustix::fs::{openat, statat, AtFlags, Dir, FileType, Mode, OFlags};

use crate::Entry;
#[cfg(unix)]
use crate::{bad_path, is_git_dir_name, Kind};
use crate::{git_dir_refusal, lexical_jail, names_git_dir};

/// A canonical path inside a configured root, judged by the jail. Its fields
/// are private to this module, so a `Jailed` cannot be made from an arbitrary
/// path anywhere else.
#[derive(Clone, Debug)]
pub(crate) struct Jailed {
    /// The canonical root this path was judged against.
    #[cfg(unix)]
    root: Arc<Path>,
    /// Opened once; later pathname replacements cannot redirect this root.
    #[cfg(unix)]
    root_handle: Arc<File>,
    /// The canonical path, within `root`.
    path: PathBuf,
}

impl Jailed {
    /// The path, for messages and for naming its type by extension. Reading
    /// through it is the violation this module exists to prevent.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

// ambient: fs: browse serves a checkout's tree; every read is of a Jailed path confined to its configured root
#[allow(clippy::disallowed_methods)]
impl Jailed {
    /// Select an in-root canonical name and validate it through the root
    /// handle. Canonicalization alone does not authorize a read: every use
    /// walks again without following symlinks. `.git` is refused both as
    /// written and as resolved.
    #[cfg(unix)]
    pub(crate) fn resolve(root: &Path, rel: &str) -> Result<Jailed> {
        lexical_jail(rel)?;
        // Judged on the path AS WRITTEN first, before the filesystem is touched,
        // so a `.git` path that exists and one that does not answer alike.
        if names_git_dir(Path::new(rel)) {
            return Err(git_dir_refusal(rel));
        }
        let canonical_root = root
            .canonicalize()
            .map_err(|e| Error::Endpoint(format!("browse: root `{}`: {e}", root.display())))?;
        let root_handle = Arc::new(File::from(
            openat(
                rustix::fs::CWD,
                &canonical_root,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| Error::Endpoint(format!("browse: open root: {e}")))?,
        ));
        let target = canonical_root.join(rel);
        let canonical = target.canonicalize().map_err(|e| path_error(rel, e))?;
        let Ok(inside) = canonical.strip_prefix(&canonical_root) else {
            return Err(Error::Endpoint(format!(
                "browse: `{rel}` resolves outside its root"
            )));
        };
        // And on the CANONICAL path: a symlink inside the tree (`cfg ->
        // .git/config`, `gitdir -> .git`) passes the lexical check and lands here.
        if names_git_dir(inside) {
            return Err(git_dir_refusal(rel));
        }
        let jailed = Jailed {
            root: Arc::from(canonical_root.as_path()),
            root_handle,
            path: canonical,
        };
        jailed.open().map_err(|e| path_error(rel, e))?;
        Ok(jailed)
    }

    /// The configured root itself, canonical.
    pub(crate) fn root(root: &Path) -> Result<Jailed> {
        Jailed::resolve(root, "")
    }

    /// One entry below this directory, by its name as [`Jailed::list`] gave
    /// it: a single path segment, never `.git`. Links are selected by their
    /// canonical in-root name, then opened without following links.
    #[cfg(unix)]
    pub(crate) fn child(&self, name: &str) -> Result<Jailed> {
        let mut parts = Path::new(name).components();
        let Some(Component::Normal(segment)) = parts.next() else {
            return Err(bad_path("an entry name is one path segment"));
        };
        if parts.next().is_some() {
            return Err(bad_path("an entry name is one path segment"));
        }
        if is_git_dir_name(segment) {
            return Err(git_dir_refusal(name));
        }
        let path = self.path.join(segment);
        let canonical = path.canonicalize().map_err(|e| path_error(name, e))?;
        let Ok(inside) = canonical.strip_prefix(&self.root) else {
            return Err(Error::Endpoint(format!(
                "browse: `{name}` resolves outside its root"
            )));
        };
        if names_git_dir(inside) {
            return Err(git_dir_refusal(name));
        }
        let jailed = Jailed {
            root: Arc::clone(&self.root),
            root_handle: Arc::clone(&self.root_handle),
            path: canonical,
        };
        jailed.open().map_err(|e| path_error(name, e))?;
        Ok(jailed)
    }

    /// Whether this path is a directory.
    pub(crate) fn is_dir(&self) -> bool {
        self.open()
            .and_then(|f| f.metadata())
            .is_ok_and(|m| m.is_dir())
    }

    /// List a directory: directories first, then files and links, each
    /// alphabetical. Non-UTF-8 names are skipped rather than mangled, and a
    /// `.git` entry (directory or file) is never listed: [`Jailed::resolve`]
    /// refuses it, and this listing is what the tree, the hash's merkle walk
    /// and the explain rollup all enumerate, so none of them can offer or fold
    /// in what the resolver will not serve. Whether a symlink is served needs
    /// the root to judge, so the tree face resolves each one itself
    /// (`tree_endpoint`); every other enumerator skips links outright.
    #[cfg(unix)]
    pub(crate) fn list(&self) -> Result<Vec<Entry>> {
        let directory = self
            .open()
            .map_err(|e| Error::Endpoint(format!("browse: open {}: {e}", self.path.display())))?;
        let read = Dir::read_from(&directory)
            .map_err(|e| Error::Endpoint(format!("browse: read {}: {e}", self.path.display())))?;
        let mut entries = Vec::new();
        for entry in read.flatten() {
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if name == "." || name == ".." || is_git_dir_name(std::ffi::OsStr::new(name)) {
                continue;
            }
            // Metadata is relative to the SAME directory we enumerated, and
            // never follows a link swapped into an entry name.
            let Ok(meta) = statat(&directory, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW) else {
                continue;
            };
            let kind = match FileType::from_raw_mode(meta.st_mode) {
                FileType::Directory => Kind::Dir,
                FileType::Symlink => Kind::Link,
                _ => Kind::File,
            };
            let size = (kind == Kind::File).then_some(meta.st_size as u64);
            entries.push(Entry {
                name: name.to_owned(),
                kind,
                size,
                to_dir: false,
            });
        }
        entries.sort_by(|a, b| {
            let rank = |e: &Entry| (e.kind != Kind::Dir) as u8;
            (rank(a), a.name.as_str()).cmp(&(rank(b), b.name.as_str()))
        });
        Ok(entries)
    }

    /// Open the file for reading.
    #[cfg(unix)]
    pub(crate) fn open(&self) -> std::io::Result<File> {
        // Canonical names contain only normal components. Open each one
        // separately: NOFOLLOW on only the final component is insufficient.
        let relative = self
            .path
            .strip_prefix(&self.root)
            .map_err(|_| std::io::Error::other("path outside checkout root"))?;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let mut handle = openat(
            &*self.root_handle,
            ".",
            flags | OFlags::DIRECTORY,
            Mode::empty(),
        )?;
        let mut parts = relative.components().peekable();
        while let Some(part) = parts.next() {
            let Component::Normal(name) = part else {
                return Err(std::io::Error::other("invalid canonical checkout path"));
            };
            let flags = if parts.peek().is_some() {
                flags | OFlags::DIRECTORY
            } else {
                flags
            };
            handle = openat(&handle, name, flags, Mode::empty())?;
        }
        Ok(File::from(handle))
    }

    /// The whole file as UTF-8, unbounded: the caller bounds it by the size the
    /// listing reported.
    pub(crate) fn read_to_string(&self) -> std::io::Result<String> {
        let mut text = String::new();
        self.open()?.read_to_string(&mut text)?;
        Ok(text)
    }
}

// ambient: process: git reads checkout history with its working directory pinned by a Jailed handle
#[allow(clippy::disallowed_methods)]
impl Jailed {
    /// Pin git's working directory before spawning, rather than letting git
    /// resolve a pathname that an attacker could replace after our check.
    #[cfg(unix)]
    fn git_command(&self) -> std::io::Result<Command> {
        let directory = self.open()?;
        let mut command = Command::new("git");
        harden_git(&mut command);
        // SAFETY: the child callback only calls async-signal-safe fchdir and
        // converts its errno without allocation or locks. The captured File
        // keeps the directory descriptor alive until exec.
        unsafe {
            command.pre_exec(move || {
                rustix::process::fchdir(&directory).map_err(std::io::Error::from)
            });
        }
        Ok(command)
    }

    /// Run git here with an argument vector, never a shell string.
    pub(crate) fn git(&self, args: &[&str]) -> std::io::Result<Output> {
        self.git_command()?.args(args).output()
    }

    /// Run git here with `input` on its stdin, for the
    /// `--batch` commands, and collect its stdout (stderr is dropped).
    pub(crate) fn git_batch(
        &self,
        args: &[&str],
        input: String,
    ) -> std::result::Result<Vec<u8>, String> {
        use std::io::{Read, Write};
        let mut child = self
            .git_command()
            .map_err(|e| format!("could not open git directory: {e}"))?
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not run git: {e}"))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        // Written from its own thread: git answers as it reads, and a full
        // stdout pipe would otherwise stall both sides.
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let mut out = Vec::new();
        child
            .stdout
            .take()
            .expect("stdout is piped")
            .read_to_end(&mut out)
            .map_err(|e| format!("git output: {e}"))?;
        let _ = writer.join();
        child.wait().map_err(|e| format!("git: {e}"))?;
        Ok(out)
    }
}

/// Variables that make git use a repository, index or object store other than
/// the one it discovers from the jailed root. Removed so the root decides.
const REPOSITORY_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

/// GitHub credentials. browse's git reads local history only, so neither git
/// nor anything a repository's configuration starts ever needs them.
const TOKEN_ENV: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];

/// Keep repository content and the host's inherited environment from choosing
/// what browse's git reads or runs (ledger #3492).
///
/// MIRRORS ikigai-repo PR 31 (`refuse_implicit_bare` and its `REPOSITORY_ENV` /
/// `TOKEN_ENV`), which exposes no helper to reuse; the two copies must move
/// together until a shared one exists. A root below its work tree's top can be
/// tracked content whose files spell a bare repository (`HEAD`, `config`,
/// `objects/`, `refs/`, or a `commondir` naming a repository outside every
/// root); git, finding no `.git` in the root, would adopt it, run its
/// configuration (`core.fsmonitor`) and read the repository it names.
/// `safe.bareRepository=explicit` refuses that, and is honored only from
/// protected configuration, which the command scope (`GIT_CONFIG_COUNT`, after
/// any entries the host already passes) is.
fn harden_git(command: &mut Command) {
    for name in REPOSITORY_ENV.iter().chain(TOKEN_ENV) {
        command.env_remove(name);
    }
    let count = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|count| count.parse::<usize>().ok())
        .unwrap_or(0);
    command
        .env(format!("GIT_CONFIG_KEY_{count}"), "safe.bareRepository")
        .env(format!("GIT_CONFIG_VALUE_{count}"), "explicit")
        .env("GIT_CONFIG_COUNT", (count + 1).to_string());
}

/// Missing names and refused symlink loops share the missing-path response.
/// Operational failures retain their cause instead of masquerading as absence.
#[cfg(unix)]
fn path_error(name: &str, error: std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::NotFound
        || error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
    {
        Error::NotFound(format!("browse: no such path `{name}`: {error}"))
    } else {
        Error::Endpoint(format!("browse: path `{name}`: {error}"))
    }
}

// A platform without a handle-relative jail must refuse, never fall back to
// checking a pathname and then opening it unconfined.
#[cfg(not(unix))]
impl Jailed {
    fn unsupported() -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "browse: checkout jail requires Unix handle-relative reads; refused on this platform",
        )
    }

    pub(crate) fn resolve(_root: &Path, rel: &str) -> Result<Jailed> {
        lexical_jail(rel)?;
        if names_git_dir(Path::new(rel)) {
            return Err(git_dir_refusal(rel));
        }
        Err(Error::Endpoint(Self::unsupported().to_string()))
    }

    pub(crate) fn child(&self, _name: &str) -> Result<Jailed> {
        Err(Error::Endpoint(Self::unsupported().to_string()))
    }

    pub(crate) fn list(&self) -> Result<Vec<Entry>> {
        Err(Error::Endpoint(Self::unsupported().to_string()))
    }

    pub(crate) fn open(&self) -> std::io::Result<File> {
        Err(Self::unsupported())
    }

    fn git_command(&self) -> std::io::Result<Command> {
        Err(Self::unsupported())
    }
}

#[cfg(all(test, not(unix)))]
mod unsupported_tests {
    use super::*;

    #[test]
    fn checkout_operations_refuse_without_a_handle_relative_jail() {
        let error = Jailed::resolve(Path::new("."), "file").unwrap_err();
        assert!(matches!(error, Error::Endpoint(message) if message.contains("requires Unix")));
        // Constructed only here to exercise the otherwise unreachable methods.
        let jailed = Jailed {
            path: PathBuf::from("."),
        };
        assert!(Jailed::root(jailed.path()).is_err());
        assert!(jailed.child("file").is_err());
        assert!(jailed.list().is_err());
        assert!(!jailed.is_dir());
        assert_eq!(
            jailed.open().unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
        assert_eq!(
            jailed.read_to_string().unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
        assert_eq!(
            jailed.git(&["status"]).unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
        assert!(jailed
            .git_batch(&["status"], String::new())
            .unwrap_err()
            .contains("requires Unix"));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "browse-checkout-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn path_errors_preserve_operational_failures() {
        for errno in [rustix::io::Errno::NOENT, rustix::io::Errno::LOOP] {
            assert!(matches!(
                path_error("data", errno.into()),
                Error::NotFound(_)
            ));
        }
        for errno in [rustix::io::Errno::ACCESS, rustix::io::Errno::MFILE] {
            let cause = std::io::Error::from(errno).to_string();
            assert!(
                matches!(path_error("data", errno.into()), Error::Endpoint(message) if message.contains(&cause))
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_is_not_missing() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("permission-denied");
        std::fs::create_dir(root.join("private")).unwrap();
        std::fs::write(root.join("private/data"), "inside").unwrap();
        let top = Jailed::root(&root).unwrap();
        std::fs::set_permissions(root.join("private"), std::fs::Permissions::from_mode(0o0))
            .unwrap();
        let resolved = Jailed::resolve(&root, "private/data");
        let child = top.child("private");
        std::fs::set_permissions(root.join("private"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(matches!(resolved, Err(Error::Endpoint(_))), "{resolved:?}");
        assert!(matches!(child, Err(Error::Endpoint(_))), "{child:?}");
    }

    #[test]
    fn a_child_is_one_segment_and_never_git() {
        let root = scratch("child-segment");
        std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let top = Jailed::root(&root).unwrap();
        assert!(top.child("sub").unwrap().is_dir());
        for bad in ["sub/deeper", "..", "/etc", "./sub", ""] {
            assert!(
                matches!(top.child(bad), Err(Error::InvalidArgument { .. })),
                "`{bad}` is not one entry name"
            );
        }
        assert!(matches!(top.child(".git"), Err(Error::NotFound(_))));
        assert!(matches!(top.child(".GIT"), Err(Error::NotFound(_))));
        assert!(matches!(top.child("absent"), Err(Error::NotFound(_))));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_child_link_is_judged_like_a_resolved_one() {
        let root = scratch("child-link");
        let outside = scratch("child-link-outside");
        std::fs::write(outside.join("secret.txt"), "hidden").unwrap();
        std::fs::write(root.join("a.txt"), "inside").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "[core]\n").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("away")).unwrap();
        std::os::unix::fs::symlink(root.join("a.txt"), root.join("near")).unwrap();
        std::os::unix::fs::symlink(root.join(".git/config"), root.join("cfg")).unwrap();
        let top = Jailed::root(&root).unwrap();
        assert!(matches!(top.child("away"), Err(Error::Endpoint(_))));
        assert!(matches!(top.child("cfg"), Err(Error::NotFound(_))));
        let near = top.child("near").unwrap();
        assert_eq!(near.read_to_string().unwrap(), "inside");
        assert_eq!(near.path(), top.child("a.txt").unwrap().path());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// Force the attacker's swap into the check/use window, rather than
    /// depending on scheduler luck to hit it in a stress loop.
    #[cfg(unix)]
    #[test]
    fn link_swap_after_resolution_cannot_escape() {
        let root = scratch("swap");
        let outside = scratch("swap-outside");
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/data"), "inside").unwrap();
        std::fs::write(outside.join("data"), "secret").unwrap();
        std::fs::write(outside.join("outside-only"), "secret").unwrap();
        let file = Jailed::resolve(&root, "sub/data").unwrap();
        let dir = Jailed::resolve(&root, "sub").unwrap();
        assert_eq!(file.read_to_string().unwrap(), "inside");
        assert_eq!(dir.list().unwrap()[0].name, "data");
        let attacker_root = root.clone();
        let attacker_outside = outside.clone();
        std::thread::spawn(move || {
            std::fs::rename(attacker_root.join("sub"), attacker_root.join("saved")).unwrap();
            std::os::unix::fs::symlink(attacker_outside, attacker_root.join("sub")).unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(
            file.read_to_string().unwrap_err().raw_os_error(),
            Some(rustix::io::Errno::NOTDIR.raw_os_error())
        );
        assert_eq!(
            file.open().unwrap_err().raw_os_error(),
            Some(rustix::io::Errno::NOTDIR.raw_os_error())
        );
        assert!(matches!(dir.list(), Err(Error::Endpoint(_))));
        assert!(matches!(dir.child("data"), Err(Error::Endpoint(_))));
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn leaf_swap_cannot_expose_git_or_outside_bytes() {
        let root = scratch("leaf-swap");
        let outside = scratch("leaf-outside");
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "credential").unwrap();
        std::fs::write(outside.join("secret"), "secret").unwrap();
        for target in [root.join(".git/config"), outside.join("secret")] {
            std::fs::write(root.join("data"), "inside").unwrap();
            let file = Jailed::resolve(&root, "data").unwrap();
            assert_eq!(file.read_to_string().unwrap(), "inside");
            let swapped = root.join("data");
            std::thread::spawn(move || {
                std::fs::remove_file(&swapped).unwrap();
                std::os::unix::fs::symlink(target, swapped).unwrap();
            })
            .join()
            .unwrap();
            assert_eq!(
                file.read_to_string().unwrap_err().raw_os_error(),
                Some(rustix::io::Errno::LOOP.raw_os_error())
            );
            assert_eq!(
                file.open().unwrap_err().raw_os_error(),
                Some(rustix::io::Errno::LOOP.raw_os_error())
            );
            std::fs::remove_file(root.join("data")).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replacing_root_does_not_redirect_reads_lists_or_git() {
        let base = scratch("root-swap");
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("data"), "inside").unwrap();
        std::fs::write(outside.join("data"), "secret").unwrap();
        std::fs::write(outside.join("outside-only"), "secret").unwrap();
        assert!(Command::new("git")
            .arg("init")
            .arg(&outside)
            .output()
            .unwrap()
            .status
            .success());
        let top = Jailed::root(&root).unwrap();
        let file = top.child("data").unwrap();
        std::fs::rename(&root, base.join("saved")).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();
        assert_eq!(file.read_to_string().unwrap(), "inside");
        assert!(top.is_dir());
        let entries = top.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "data");
        // The original directory is not a repository. A pathname-based git
        // invocation would instead succeed in the attacker's repository.
        assert!(!top
            .git(&["rev-parse", "--is-inside-work-tree"])
            .unwrap()
            .status
            .success());
        assert!(top
            .git_batch(&["rev-parse", "--is-inside-work-tree"], String::new())
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn git_runs_in_the_jailed_root() {
        let root = scratch("git");
        let top = Jailed::root(&root).unwrap();
        let out = top.git(&["rev-parse", "--is-inside-work-tree"]).unwrap();
        // Not a repository: git ran THERE, and said so.
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("not a git repository"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
