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
//! The jail is canonicalize-then-prefix-check, not `openat`: a path is judged
//! when it is resolved and opened by name afterwards, so a symlink swapped into
//! the tree between the two is a race this does not close. That is the same
//! guarantee browse has always given; holding a directory handle would close
//! it, and needs a crate (`cap-std`) or `rustix` that browse does not carry.

use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use ikigai_core::{Error, Result};

use crate::{bad_path, git_dir_refusal, is_git_dir_name, lexical_jail, names_git_dir, Entry, Kind};

/// A canonical path inside a configured root, judged by the jail. Its fields
/// are private to this module, so a `Jailed` cannot be made from an arbitrary
/// path anywhere else.
#[derive(Clone, Debug)]
pub(crate) struct Jailed {
    /// The canonical root this path was judged against.
    root: Arc<Path>,
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
    /// Resolve a root-relative path to a canonical path within the root:
    /// **the jail**. `..` and absolute segments are rejected lexically; the
    /// target is then canonicalized (it must exist; browsing is a read) and
    /// required to sit within the canonical root, so a symlink component
    /// cannot escape. Anything under a `.git` directory is refused, as written
    /// and as resolved.
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
        let target = canonical_root.join(rel);
        let canonical = target
            .canonicalize()
            .map_err(|_| Error::NotFound(format!("browse: no such path `{rel}`")))?;
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
        Ok(Jailed {
            root: Arc::from(canonical_root.as_path()),
            path: canonical,
        })
    }

    /// The configured root itself, canonical.
    pub(crate) fn root(root: &Path) -> Result<Jailed> {
        Jailed::resolve(root, "")
    }

    /// One entry below this directory, by its name as [`Jailed::list`] gave
    /// it: a single path segment, never `.git`. A plain entry of a canonical
    /// directory is canonical and inside the root already, so it costs one
    /// `lstat`; a link is judged as [`Jailed::resolve`] judges one.
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
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|_| Error::NotFound(format!("browse: no such entry `{name}`")))?;
        if !meta.file_type().is_symlink() {
            return Ok(Jailed {
                root: Arc::clone(&self.root),
                path,
            });
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| Error::NotFound(format!("browse: no such entry `{name}`")))?;
        let Ok(inside) = canonical.strip_prefix(&self.root) else {
            return Err(Error::Endpoint(format!(
                "browse: `{name}` resolves outside its root"
            )));
        };
        if names_git_dir(inside) {
            return Err(git_dir_refusal(name));
        }
        Ok(Jailed {
            root: Arc::clone(&self.root),
            path: canonical,
        })
    }

    /// Whether this path is a directory.
    pub(crate) fn is_dir(&self) -> bool {
        self.path.is_dir()
    }

    /// List a directory: directories first, then files and links, each
    /// alphabetical. Non-UTF-8 names are skipped rather than mangled, and a
    /// `.git` entry (directory or file) is never listed: [`Jailed::resolve`]
    /// refuses it, and this listing is what the tree, the hash's merkle walk
    /// and the explain rollup all enumerate, so none of them can offer or fold
    /// in what the resolver will not serve. Whether a symlink is served needs
    /// the root to judge, so the tree face resolves each one itself
    /// (`tree_endpoint`); every other enumerator skips links outright.
    pub(crate) fn list(&self) -> Result<Vec<Entry>> {
        let read = std::fs::read_dir(&self.path)
            .map_err(|e| Error::Endpoint(format!("browse: read {}: {e}", self.path.display())))?;
        let mut entries = Vec::new();
        for entry in read.flatten() {
            if is_git_dir_name(&entry.file_name()) {
                continue;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            // `file_type`/`metadata` on a DirEntry do NOT follow symlinks: a
            // link is reported as a link, and an escaping one is only ever
            // denied later, at resolution.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let kind = if file_type.is_dir() {
                Kind::Dir
            } else if file_type.is_symlink() {
                Kind::Link
            } else {
                Kind::File
            };
            let size = (kind == Kind::File)
                .then(|| entry.metadata().ok().map(|m| m.len()))
                .flatten();
            entries.push(Entry {
                name,
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
    pub(crate) fn open(&self) -> std::io::Result<File> {
        File::open(&self.path)
    }

    /// The whole file as UTF-8, unbounded: the caller bounds it by the size the
    /// listing reported.
    pub(crate) fn read_to_string(&self) -> std::io::Result<String> {
        std::fs::read_to_string(&self.path)
    }
}

// ambient: process: git reads the history of a checkout browse serves, run with -C in a Jailed root only
#[allow(clippy::disallowed_methods)]
impl Jailed {
    /// Run `git -C <this> <args…>`: an argument vector, never a shell string.
    pub(crate) fn git(&self, args: &[&str]) -> std::io::Result<Output> {
        Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(args)
            .output()
    }

    /// Run `git -C <this> <args…>` with `input` on its stdin, for the
    /// `--batch` commands, and collect its stdout (stderr is dropped).
    pub(crate) fn git_batch(
        &self,
        args: &[&str],
        input: String,
    ) -> std::result::Result<Vec<u8>, String> {
        use std::io::{Read, Write};
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&self.path)
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

#[cfg(test)]
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
