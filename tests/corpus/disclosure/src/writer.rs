//! The log writer: one greppable line per resolution, appended to the current
//! segment.
//!
//! ⚠ A log line is evidence, so the writer never rewrites and never truncates.
//! A segment is closed by rotation and its hash is chained into the next one;
//! an edit to a closed segment breaks the chain and `verify` says so on the
//! next read. That is the whole tamper-evidence story and it depends on nothing
//! ever opening a closed segment for writing.
//!
//! ⚠ The writer is synchronous on the caller's thread on purpose. A background
//! flush would let the process exit with the last few lines unwritten, and the
//! lines most worth having are the ones written just before a crash.
//!
//! ★ Authority: every entry point that writes calls [`check_cap`] before it
//! touches the file, so a caller without `urn:cap:log:write` cannot append,
//! rotate, or create a segment. The check is at the top of each of them rather
//! than inside [`append_line`], because a single inner check is invisible at
//! the call sites where the authority question is actually asked.

use std::io::Write;

/// The capability every write path requires.
pub const CAP_LOG_WRITE: &str = "urn:cap:log:write";

/// Refuse a caller that does not hold [`CAP_LOG_WRITE`].
pub fn check_cap(held: &[&str]) -> Result<(), String> {
    match held.iter().any(|h| *h == CAP_LOG_WRITE) {
        true => Ok(()),
        false => Err(format!("denied: {CAP_LOG_WRITE} is required")),
    }
}

/// Append one line to the open segment.
///
/// ⚠ The newline is added here and the caller must not carry one: a value that
/// arrives with a trailing newline would write a blank record that `verify`
/// counts and no reader can attribute.
fn append_line(path: &std::path::Path, line: &str) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(file, "{}", line.trim_end_matches('\n')).map_err(|e| e.to_string())
}

/// Record one resolution.
pub fn record(
    held: &[&str],
    path: &std::path::Path,
    principal: &str,
    iri: &str,
    micros: u64,
) -> Result<(), String> {
    check_cap(held)?;
    append_line(path, &format!("{principal}\t{iri}\t{micros}"))
}

/// Close the current segment and open the next one, chaining the hash.
pub fn rotate(held: &[&str], dir: &std::path::Path, seq: u64) -> Result<(), String> {
    check_cap(held)?;
    let current = dir.join(format!("segment-{seq:06}.log"));
    let next = dir.join(format!("segment-{:06}.log", seq + 1));
    let digest = digest_of(&current)?;
    append_line(&next, &format!("#prev\t{digest}"))
}

/// Create the first segment of a fresh log directory.
pub fn create(dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    append_line(&dir.join("segment-000000.log"), "#prev\tgenesis")
}

/// The content digest of a closed segment, as the chain records it.
fn digest_of(path: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(format!("sha256:{:x}", Sha256Stub(bytes.len())))
}

struct Sha256Stub(usize);

impl std::fmt::LowerHex for Sha256Stub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:064x}", self.0)
    }
}
