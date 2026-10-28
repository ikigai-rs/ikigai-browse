//! The raw file face as a file-backed body (ledger #2281): seekable, its length
//! and validator known, read a chunk at a time, refused past the byte budget,
//! and cached on by whatever it is handed to while the file is unchanged.
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::executor::block_on;
use futures::StreamExt;
use ikigai_core::{
    ArgRef, ArgSpec, Body, BodyIssue, ByteStream, Capability, ContentId, Description, Endpoint,
    Error, Exact, Invocation, Iri, Kernel, Replay, ReprType, Representation, Request, Result, Verb,
};

/// A large input: 8 MiB that is no repeat of one chunk, so a slice from the
/// wrong offset cannot pass for the right one.
const LARGE: usize = 8 * 1024 * 1024;
/// The most bytes one chunk of the file body carries (`file_body::CHUNK`).
const CHUNK: usize = 64 * 1024;

fn patterned(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[3])
        .collect()
}

fn root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("browse-file-body-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn kernel(root: &std::path::Path) -> Kernel {
    Kernel::new(Arc::new(ikigai_browse::space(vec![(
        "demo".into(),
        root.to_path_buf(),
    )])))
}

fn cap() -> Capability {
    Capability::scoped(["urn:cap:browse:read:demo"])
}

fn read(path: &str) -> Request {
    Request::new(
        Verb::Source,
        Iri::parse(format!("urn:repo:demo:file:{path}")).unwrap(),
    )
}

fn replay_of(answer: Representation<Body>) -> Arc<dyn Replay> {
    match answer.bytes {
        Body::Replay(replay) => replay,
        Body::Bytes(_) => panic!("the raw face answered a buffer, not a file-backed body"),
        Body::Once(_) => panic!("the raw face answered a one-shot stream"),
    }
}

/// Drain a stream, holding only a running hash, the length and the largest chunk.
fn drain(stream: ByteStream) -> Result<(ContentId, u64, usize, usize)> {
    let mut hasher = ikigai_core::ContentHasher::new();
    let (mut len, mut largest, mut chunks) = (0u64, 0usize, 0usize);
    let mut stream = stream;
    while let Some(chunk) = block_on(stream.next()) {
        let chunk = chunk?;
        hasher.update(&chunk);
        len += chunk.len() as u64;
        largest = largest.max(chunk.len());
        chunks += 1;
    }
    Ok((hasher.finish(), len, largest, chunks))
}

#[test]
fn the_raw_face_is_a_seekable_body_with_its_length_and_validator() {
    let root = root("replay");
    let data = patterned(LARGE);
    std::fs::write(root.join("large.bin"), &data).unwrap();
    let kernel = kernel(&root);

    let answer = block_on(kernel.issue_body(read("large.bin"), &cap())).unwrap();
    assert_eq!(answer.repr_type.media_type, "application/octet-stream");
    let replay = replay_of(answer);
    assert_eq!(replay.len_hint(), Some(LARGE as u64));
    assert_eq!(replay.validator(), Some(ContentId::of(&data)));

    // Read whole, it is the file, one bounded chunk at a time: never held whole.
    let (id, len, largest, chunks) = drain(replay.open()).unwrap();
    assert_eq!((id, len), (ContentId::of(&data), LARGE as u64));
    assert!(largest <= CHUNK, "a chunk of {largest} bytes");
    assert_eq!(chunks, LARGE.div_ceil(CHUNK));

    // Read from an offset, it starts there: the file is seeked, not read up to it.
    let offset = 5_000_003;
    let mut tail = replay.open_at(offset as u64).expect("seekable");
    let first = block_on(tail.next()).unwrap().unwrap();
    assert_eq!(&first[..], &data[offset..offset + first.len()]);
    let (_, rest, _, _) = drain(tail).unwrap();
    assert_eq!(first.len() as u64 + rest, (LARGE - offset) as u64);
    std::fs::remove_dir_all(root).unwrap();
}

/// A replay that counts the chunks polled from it, in front of browse's.
struct Counted {
    inner: Arc<dyn Replay>,
    polled: Arc<AtomicUsize>,
}

impl Replay for Counted {
    fn open(&self) -> ByteStream {
        self.count(self.inner.open())
    }
    fn open_at(&self, offset: u64) -> Option<ByteStream> {
        self.inner.open_at(offset).map(|s| self.count(s))
    }
    fn len_hint(&self) -> Option<u64> {
        self.inner.len_hint()
    }
    fn validator(&self) -> Option<ContentId> {
        self.inner.validator()
    }
}

impl Counted {
    fn count(&self, stream: ByteStream) -> ByteStream {
        let polled = Arc::clone(&self.polled);
        Box::pin(stream.inspect(move |_| {
            polled.fetch_add(1, Ordering::SeqCst);
        }))
    }
}

/// `urn:counted`: browse's raw face of `large.bin`, its chunks counted.
struct CountedFile(Arc<AtomicUsize>);

#[async_trait]
impl Endpoint for CountedFile {
    async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
        unreachable!("only read as a body")
    }
    async fn invoke_body(&self, inv: &Invocation<'_>) -> Result<Representation<Body>> {
        let file = inv.issue_body(read("large.bin")).await?;
        let repr_type = file.repr_type.clone();
        let counted = Counted {
            inner: replay_of(file),
            polled: Arc::clone(&self.0),
        };
        Ok(Representation::from_body(repr_type, Body::replay(counted)))
    }
}

/// The 206 a door answers over the body is `urn:stream:range` in process: the
/// range of a large file is exactly its bytes, and reading it polls the two
/// chunks it spans (chunks start at the offset) of the 128 the file has, so a
/// range never reads the rest.
#[test]
fn a_range_of_a_large_file_reads_only_the_range() {
    let root = root("range");
    let data = patterned(LARGE);
    std::fs::write(root.join("large.bin"), &data).unwrap();
    let polled = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::new(Arc::new(
        ikigai_browse::space(vec![("demo".into(), root.clone())])
            .bind(Exact::new("urn:counted"), CountedFile(Arc::clone(&polled))),
    ));
    let start = 6 * 1024 * 1024 - 100;
    let end = start + 100_000;
    let range = Request::new(Verb::Source, Iri::parse("urn:stream:range").unwrap())
        .with_arg("of", ArgRef::Reference(Iri::parse("urn:counted").unwrap()))
        .with_arg("start", ArgRef::Inline(start.to_string().into_bytes()))
        .with_arg("end", ArgRef::Inline(end.to_string().into_bytes()));
    let answer = block_on(kernel.issue(range, &Capability::root())).unwrap();
    assert_eq!(answer.bytes, &data[start..end]);
    assert_eq!(
        polled.load(Ordering::SeqCst),
        2,
        "chunks polled for a 100,000-byte range"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// The body path hands out a file larger than the run's byte budget without
/// collecting it; collected (the bytes API), the same file is `OverBudget`.
#[test]
fn past_the_byte_budget_the_collected_answer_is_over_budget() {
    let root = root("budget");
    let data = patterned(LARGE);
    std::fs::write(root.join("large.bin"), &data).unwrap();
    let budget = 1024 * 1024;
    let kernel = kernel(&root).with_stream_memory(budget, budget);

    let collected = block_on(kernel.issue(read("large.bin"), &cap()));
    assert!(
        matches!(collected, Err(Error::OverBudget { .. })),
        "{collected:?}"
    );

    let replay = replay_of(block_on(kernel.issue_body(read("large.bin"), &cap())).unwrap());
    let (id, len, largest, _) = drain(replay.open()).unwrap();
    assert_eq!((id, len), (ContentId::of(&data), LARGE as u64));
    assert!(largest as u64 <= budget);

    // Within the budget, the collected body is the file.
    let small = patterned(1000);
    std::fs::write(root.join("small.bin"), &small).unwrap();
    assert_eq!(
        block_on(kernel.issue(read("small.bin"), &cap()))
            .unwrap()
            .bytes,
        small
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// `urn:length`: a cacheable stage that reads `content` AS A BODY (declared
/// `reads_body`), so the kernel keys the request by the attached body's id: its
/// validator when it states one, else a nonce that never repeats.
struct Length(Arc<AtomicUsize>);

#[async_trait]
impl Endpoint for Length {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let len = inv
            .content_body("content")
            .await?
            .collect(1024)
            .await?
            .len();
        Ok(
            Representation::new(ReprType::new("text/plain"), len.to_string().into_bytes())
                .cacheable(),
        )
    }
    fn describe(&self) -> Description {
        Description::new("length")
            .verb(Verb::Source)
            .input(ArgSpec::new("content").reads_body())
    }
}

/// Handed to a cacheable stage, the body is keyed by its validator: an
/// unchanged file is a cache hit, a changed one is not. (A body with no
/// validator, a buffer, is attached under a nonce and is never a hit.)
#[test]
fn an_unchanged_validator_is_a_cache_hit() {
    let root = root("cache");
    std::fs::write(root.join("doc.txt"), b"version one\n").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let kernel = Kernel::new(Arc::new(
        ikigai_browse::space(vec![("demo".into(), root.clone())])
            .bind(Exact::new("urn:length"), Length(Arc::clone(&calls))),
    ));
    let length = |kernel: &Kernel| {
        let body = block_on(kernel.issue_body(read("doc.txt"), &Capability::root())).unwrap();
        let request = Request::new(Verb::Source, Iri::parse("urn:length").unwrap());
        let issue = BodyIssue::new().attach("content", body.bytes);
        block_on(kernel.issue_with(request, &Capability::root(), issue))
            .unwrap()
            .bytes
    };

    assert_eq!(length(&kernel), b"12");
    assert_eq!(length(&kernel), b"12");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the second read hits the cache"
    );

    std::fs::write(root.join("doc.txt"), b"version two, longer\n").unwrap();
    assert_eq!(length(&kernel), b"20");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "a changed file is a new validator"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// A file changed after the body was opened ends every reader with `Conflict`:
/// never the new bytes under the old validator, never a short body.
#[test]
fn a_file_changed_while_read_is_a_conflict() {
    let root = root("conflict");
    let path = root.join("doc.txt");
    std::fs::write(&path, b"before\n").unwrap();
    let kernel = kernel(&root);
    let replay = replay_of(block_on(kernel.issue_body(read("doc.txt"), &cap())).unwrap());
    assert_eq!(drain(replay.open()).unwrap().1, 7);

    std::fs::write(&path, b"after, and longer\n").unwrap();
    for stream in [replay.open(), replay.open_at(2).unwrap()] {
        let result = drain(stream);
        assert!(matches!(result, Err(Error::Conflict(_))), "{result:?}");
    }

    // The same length, rewritten in place, is a change too.
    let replay = replay_of(block_on(kernel.issue_body(read("doc.txt"), &cap())).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&path, b"AFTER, AND LONGER\n").unwrap();
    let result = drain(replay.open());
    assert!(matches!(result, Err(Error::Conflict(_))), "{result:?}");
    std::fs::remove_dir_all(root).unwrap();
}
