//! The raw file face as a **file-backed body** (ledger #2281): a seekable
//! [`Replay`] that knows its length and its strong validator, so a door can
//! answer a byte range (`206`) over it and a downstream stage handed the body
//! caches on the validator, while nothing holds the file whole.
//!
//! - **The validator** is the [`ContentId`] of exactly the bytes served, computed
//!   by one bounded-chunk pass ([`ContentHasher`]) when the body is opened. The
//!   same pass sniffs UTF-8 (std's validator, chunk by chunk) for the media type
//!   an unknown extension needs. Browse's own `sha256:` hash (`hash.rs`) is the
//!   explanation archive's key and stays as it is; a validator must be core's
//!   content id, the form an attachment is keyed by.
//! - **The bound** is the per-file budget the bytes path keeps
//!   ([`MAX_FILE_BYTES`]): a larger file is `OverBudget` before a byte is read,
//!   and one that grows past it while hashed is too. Never a prefix.
//! - **Stability**: the file's version (size, mtime and, on unix, device, inode
//!   and ctime) is recorded at open. Every reader reopens the path, is refused
//!   unless it finds that same version, and checks it again at its end; a file
//!   changed while read ends the stream with [`Error::Conflict`], never a short
//!   or mixed body. That is [`Replay`]'s contract for a file written in place.
//! - **No copy**: a reader holds one chunk ([`CHUNK`]) at a time, each poll one
//!   bounded blocking read on the polling thread, as browse does all its file I/O.
//!
//! The html, annotations and proposals faces are not bodies: they render the
//! whole text, and are answered by the bytes path, collected under the run's
//! byte budget like any buffer.

use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use ikigai_core::{
    Body, ByteStream, Bytes, ContentHasher, ContentId, Error, ReprType, Representation, Result,
};

use crate::file_read::{over_budget, MAX_FILE_BYTES};

/// The largest chunk a [`FileReplay`] reader yields.
pub(crate) const CHUNK: u64 = 64 * 1024;

/// One version of one file: what a reader must still find to read it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Version {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    ctime: (i64, i64),
    #[cfg(unix)]
    nlink: u64,
}

impl Version {
    fn of(meta: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Version {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            dev: meta.dev(),
            #[cfg(unix)]
            ino: meta.ino(),
            #[cfg(unix)]
            ctime: (meta.ctime(), meta.ctime_nsec()),
            #[cfg(unix)]
            nlink: meta.nlink(),
        }
    }

    /// Whether `self` and `other` are one version of one file. A write moves
    /// mtime and ctime; resetting mtime afterwards moves ctime again. A link
    /// count that moved (a rename over the file, a hard link made) moves ctime
    /// without touching the bytes, so ctime is compared only when it did not.
    fn same(&self, other: &Version) -> bool {
        let same = self.len == other.len && self.modified == other.modified;
        #[cfg(unix)]
        let same = same
            && self.dev == other.dev
            && self.ino == other.ino
            && (self.nlink != other.nlink || self.ctime == other.ctime);
        same
    }
}

/// A file body's file changed between its open and a reader's end.
fn changed_while_read(shown: &str) -> Error {
    Error::Conflict(format!(
        "browse: `{shown}` changed while read; read it again"
    ))
}

fn io_error(what: &str, shown: &str, e: std::io::Error) -> Error {
    Error::Endpoint(format!("browse: {what} `{shown}`: {e}"))
}

/// The raw face of the file at `path` (already resolved and judged inside its
/// root; `shown` is its root-relative name for messages) as a seekable body,
/// with `media_type` choosing its type from whether it is UTF-8. Live: the
/// answer is never cached as such (the file can change), but its validator lets
/// whatever the body is handed to cache on it.
pub(crate) fn raw_face(
    path: &Path,
    shown: &str,
    media_type: impl FnOnce(&Path, bool) -> ReprType,
) -> Result<Representation<Body>> {
    let (replay, utf8) = FileReplay::open(path, shown)?;
    Ok(Representation::from_body(
        media_type(path, utf8),
        Body::replay(replay),
    ))
}

/// A file as a [`Replay`]: its path, the version recorded at open, and the
/// content id and UTF-8 verdict of the bytes of that version.
pub(crate) struct FileReplay {
    path: PathBuf,
    shown: String,
    at_open: Version,
    validator: ContentId,
}

impl FileReplay {
    /// Open the file, refuse it past [`MAX_FILE_BYTES`], and hash it (and sniff
    /// UTF-8) in one bounded-chunk pass that holds no more than a chunk.
    pub(crate) fn open(path: &Path, shown: &str) -> Result<(Self, bool)> {
        let file = File::open(path).map_err(|e| io_error("open", shown, e))?;
        let meta = file
            .metadata()
            .map_err(|e| io_error("metadata", shown, e))?;
        if meta.len() > MAX_FILE_BYTES {
            return Err(over_budget(path));
        }
        let at_open = Version::of(&meta);
        let mut hasher = ContentHasher::new();
        let mut utf8 = Utf8Sniff::default();
        let mut reader = FileReader::new(file, at_open, 0, shown);
        while let Some(chunk) = reader.next_chunk() {
            let chunk = chunk?;
            hasher.update(&chunk);
            utf8.feed(&chunk);
        }
        let replay = FileReplay {
            path: path.to_path_buf(),
            shown: shown.to_string(),
            at_open,
            validator: hasher.finish(),
        };
        Ok((replay, utf8.finish()))
    }

    /// A reader from `offset`: the path opened afresh, refused unless it is the
    /// version recorded at open.
    fn reader(&self, offset: u64) -> FileReader {
        let fresh = File::open(&self.path)
            .map_err(|_| changed_while_read(&self.shown))
            .and_then(|file| {
                let now = file
                    .metadata()
                    .map_err(|e| io_error("metadata", &self.shown, e))?;
                if Version::of(&now).same(&self.at_open) {
                    Ok(file)
                } else {
                    Err(changed_while_read(&self.shown))
                }
            });
        match fresh {
            Ok(file) => FileReader::new(file, self.at_open, offset, &self.shown),
            Err(e) => FileReader::failed(e),
        }
    }
}

impl ikigai_core::Replay for FileReplay {
    fn open(&self) -> ByteStream {
        Box::pin(self.reader(0))
    }

    /// A reader from byte `offset`: the file is seeked, never read up to it.
    /// Past the end it yields nothing (still checked for a change at its end).
    fn open_at(&self, offset: u64) -> Option<ByteStream> {
        Some(Box::pin(self.reader(offset)))
    }

    fn len_hint(&self) -> Option<u64> {
        Some(self.at_open.len)
    }

    fn validator(&self) -> Option<ContentId> {
        Some(self.validator)
    }
}

/// One reader of a [`FileReplay`]: the open file, how many bytes the version at
/// open leaves to read, and that version to check at the end.
struct FileReader {
    /// The file and the version it must still be at its end; `None` once the
    /// stream ended.
    file: Option<(File, Version)>,
    remaining: u64,
    shown: String,
    failed: Option<Error>,
}

impl FileReader {
    fn new(mut file: File, expected: Version, offset: u64, shown: &str) -> Self {
        let mut reader = FileReader {
            file: None,
            remaining: expected.len.saturating_sub(offset),
            shown: shown.to_string(),
            failed: None,
        };
        match file.seek(SeekFrom::Start(offset)) {
            Ok(_) => reader.file = Some((file, expected)),
            Err(e) => reader.failed = Some(io_error("seek", shown, e)),
        }
        reader
    }

    fn failed(error: Error) -> Self {
        FileReader {
            file: None,
            remaining: 0,
            shown: String::new(),
            failed: Some(error),
        }
    }

    /// The next chunk, the error that ends the stream, or `None` at a clean end.
    fn next_chunk(&mut self) -> Option<Result<Bytes>> {
        if let Some(error) = self.failed.take() {
            return Some(Err(error));
        }
        let (file, expected) = self.file.as_mut()?;
        // One byte past what is left, so growth shows without a second read.
        let mut buf = vec![0u8; (self.remaining.min(CHUNK - 1) + 1) as usize];
        let read = loop {
            match file.read(&mut buf) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                other => break other,
            }
        };
        match read {
            Ok(0) => {
                let unchanged = self.remaining == 0
                    && file
                        .metadata()
                        .is_ok_and(|now| Version::of(&now).same(expected));
                self.file = None;
                (!unchanged).then(|| Err(changed_while_read(&self.shown)))
            }
            Ok(n) if n as u64 > self.remaining => {
                self.file = None;
                Some(Err(changed_while_read(&self.shown)))
            }
            Ok(n) => {
                self.remaining -= n as u64;
                buf.truncate(n);
                Some(Ok(buf.into()))
            }
            Err(e) => {
                self.file = None;
                Some(Err(io_error("read", &self.shown, e)))
            }
        }
    }
}

impl Stream for FileReader {
    type Item = Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.get_mut().next_chunk())
    }
}

/// Whether bytes fed in chunks are UTF-8, through std's validator: a sequence
/// cut by a chunk boundary (at most three bytes) is carried into the next chunk.
#[derive(Default)]
struct Utf8Sniff {
    carry: Vec<u8>,
    invalid: bool,
}

impl Utf8Sniff {
    fn feed(&mut self, chunk: &[u8]) {
        if self.invalid {
            return;
        }
        let joined;
        let bytes = if self.carry.is_empty() {
            chunk
        } else {
            joined = [self.carry.as_slice(), chunk].concat();
            &joined
        };
        let carry = match std::str::from_utf8(bytes) {
            Ok(_) => Vec::new(),
            // Cut at the end of the chunk: the rest comes with the next one.
            Err(e) if e.error_len().is_none() => bytes[e.valid_up_to()..].to_vec(),
            Err(_) => {
                self.invalid = true;
                Vec::new()
            }
        };
        self.carry = carry;
    }

    fn finish(self) -> bool {
        !self.invalid && self.carry.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_sniff_agrees_with_std_across_every_cut() {
        let samples: [&[u8]; 4] = [
            "héllo wörld — ✓ 𝄞".as_bytes(),
            b"plain ascii",
            b"bad \xff byte",
            "cut at the end \u{e9}".as_bytes().split_last().unwrap().1,
        ];
        for sample in samples {
            let expected = std::str::from_utf8(sample).is_ok();
            for size in 1..=sample.len().max(1) {
                let mut sniff = Utf8Sniff::default();
                for chunk in sample.chunks(size) {
                    sniff.feed(chunk);
                }
                assert_eq!(sniff.finish(), expected, "{sample:?} in {size}-byte chunks");
            }
        }
    }
}
