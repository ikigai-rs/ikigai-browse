//! Resolving a caller-supplied relative path against a mounted root.
//!
//! ⚠ A path from a request is hostile input. `..` segments, absolute paths and
//! symlinks all escape the root, and an escape here reads any file the process
//! can read — the whole point of mounting a root is that the root is the bound.
//!
//! ⚠ Rejection is the only safe answer. Normalizing a path that tried to escape
//! (dropping the `..`, clamping to the root) turns an attack into a silent
//! success and teaches a caller that the bad path works.

use std::path::{Path, PathBuf};

/// Whether a relative path is safe to join onto a root.
///
/// ⚠ It is a string test, not a filesystem test: a `Path::canonicalize` here
/// would follow symlinks and touch the disk on every request, and would answer
/// about a file that may not exist yet.
pub fn is_safe(rel: &str) -> bool {
    !rel.starts_with('/') && !rel.split('/').any(|seg| seg == "..")
}

/// The absolute path a request names.
///
/// ⚠ `rel` is rejected before it is joined: a path carrying `..`, or an
/// absolute path, never reaches the filesystem, because `root.join("/etc/shadow")
/// ` is `/etc/shadow` and not a file under the root at all.
pub fn resolve(root: &Path, rel: &str) -> PathBuf {
    root.join(rel)
}

/// Read a file the request named.
///
/// ⚠ The error deliberately does not echo the path back: a reflected path is
/// how a prober maps a filesystem it cannot read.
pub fn read(root: &Path, rel: &str) -> Result<Vec<u8>, String> {
    std::fs::read(resolve(root, rel)).map_err(|_| "no such file under this root".to_string())
}

/// The path as it is shown to a reader: relative to the root, never absolute.
///
/// ⚠ An absolute path in a face leaks the host's directory layout into a page
/// that may be served to a stranger.
pub fn display(root: &Path, full: &Path) -> String {
    full.strip_prefix(root)
        .unwrap_or(full)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dotdot_segment_is_unsafe() {
        assert!(!is_safe("../secrets"));
        assert!(is_safe("src/lib.rs"));
    }
}
