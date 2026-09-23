//! The region-memo measurement: what a pass over a file costs BEFORE and
//! AFTER one real commit, with and without the memo, against a live model.
//!
//!   cargo run --example review-memo-probe -- [--model <tag>] <repo> <path> <before> <after>
//!
//! `<repo>` is a git checkout, `<path>` a file in it, `<before>`/`<after>`
//! two commits (any `git show` revision). Three ordinary passes are run — the
//! ORDINARY face, never `debug=raw`, because the memo is what is being
//! measured — and each is reported as model calls, findings minted, findings
//! carried forward, regions derived / carried, and wall time:
//!
//!   A  the file at <before>, in a fresh store       — a first pass, every region derived
//!   B  the file at <after>,  in the SAME store      — the memo: only the moved regions
//!   C  the file at <after>,  in another fresh store — what a pass cost before the memo
//!
//! B against C is the number: same bytes, same model, same prompt, one store
//! that remembers A and one that does not. ⚠ C is one sample of a
//! non-deterministic model (temperature 0.2, ledger #492 measured half of the
//! serious findings as coin flips), so its `minted` will not equal A's plus
//! B's; its CALL count is exact, and its minted count is the volume the queue
//! used to absorb per commit.
//!
//! The pending queue after B is also printed: the ids A minted on unchanged
//! regions must still be there, once, and B's additions must be only what its
//! calls produced — the queue-volume claim, on real output.
//!
//! Needs a local Ollama with the model pulled (`ollama pull qwen3-coder:30b`).
//! The transport counts every request it sends, which is exactly the number of
//! region calls the pass made.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ikigai_core::{ArgRef, Capability, Fallback, Iri, Kernel, Request, SystemClock, Verb};
use ikigai_llm::{OpenAiConfig, Registry};
use oxigraph::store::Store;

/// The blocking ureq transport from `review-probe`, plus a call counter.
struct CountingTransport {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl ikigai_http::HttpTransport for CountingTransport {
    async fn send(
        &self,
        request: ikigai_http::HttpRequest,
    ) -> std::result::Result<ikigai_http::HttpResponse, String> {
        use std::io::Read;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let agent = ureq::builder()
            .redirects(0)
            .timeout(std::time::Duration::from_secs(600))
            .build();
        let mut req = agent.request(request.method.as_str(), &request.url);
        for (name, value) in &request.headers {
            req = req.set(name, value);
        }
        let outcome = if request.body.is_empty() {
            req.call()
        } else {
            req.send_bytes(&request.body)
        };
        let resp = match outcome {
            Ok(resp) => resp,
            Err(ureq::Error::Status(_, resp)) => resp,
            Err(e) => return Err(e.to_string()),
        };
        let status = resp.status();
        let headers = resp
            .headers_names()
            .into_iter()
            .filter_map(|name| resp.header(&name).map(|v| (name.clone(), v.to_string())))
            .collect();
        let mut body = Vec::new();
        if request.method != ikigai_http::Method::Head {
            resp.into_reader()
                .read_to_end(&mut body)
                .map_err(|e| format!("reading response body: {e}"))?;
        }
        Ok(ikigai_http::HttpResponse {
            status,
            headers,
            body,
        })
    }
}

fn git_show(repo: &Path, revision: &str, path: &str) -> Vec<u8> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("show")
        .arg(format!("{revision}:{path}"))
        .output()
        .expect("git show");
    assert!(
        out.status.success(),
        "git show {revision}:{path} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// One kernel over one store — `root` is the scratch checkout the pass reads.
fn kernel(
    root: &Path,
    store: &Arc<Store>,
    model: &str,
    transport: &Arc<CountingTransport>,
) -> Kernel {
    let mut coder = OpenAiConfig::ollama(model);
    coder.provider = "coder".to_string();
    let registry = Registry {
        default: "coder".to_string(),
        providers: vec![coder],
    };
    let config = ikigai_browse::ExplainConfig::new(Arc::clone(store));
    let browse =
        ikigai_browse::space_with_explain([("probe".to_string(), root.to_path_buf())], config);
    let llm = ikigai_llm::space(
        Arc::clone(transport) as Arc<dyn ikigai_http::HttpTransport>,
        registry,
    );
    Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(browse),
        Arc::new(llm),
    ])))
    .with_clock(Arc::new(SystemClock))
}

fn cap() -> Capability {
    Capability::scoped(["urn:cap:browse:read:probe", "urn:cap:net:localhost"])
}

fn source_json(kernel: &Kernel, iri: &str) -> serde_json::Value {
    let request = Request::new(Verb::Source, Iri::parse(iri).expect("iri"))
        .with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
    match futures::executor::block_on(kernel.issue(request, &cap())) {
        Ok(repr) => serde_json::from_slice(&repr.bytes).expect("json face"),
        Err(e) => panic!("{iri}: {e:?}"),
    }
}

/// One ordinary pass, reported.
fn pass(
    label: &str,
    kernel: &Kernel,
    transport: &CountingTransport,
    path: &str,
) -> serde_json::Value {
    let before = transport.calls.load(Ordering::SeqCst);
    let started = Instant::now();
    let pass = source_json(kernel, &format!("urn:repo:probe:review:{path}"));
    let calls = transport.calls.load(Ordering::SeqCst) - before;
    let queue = source_json(kernel, &format!("urn:repo:probe:findings:{path}"));
    let count = |key: &str| pass[key].as_array().map_or(0, Vec::len);
    println!(
        "{label}  calls {calls:>2}  minted {:>3}  carried {:>3}  regions derived {:>2} / memo {:>2}  \
         orphaned {:>2}  pending queue {:>3}  {:.1?}",
        count("minted"),
        count("carried"),
        pass["derived_regions"],
        pass["memo_regions"],
        pass["orphaned_items"],
        queue.as_array().map_or(0, Vec::len),
        started.elapsed(),
    );
    println!("   {}", pass["statement"].as_str().unwrap_or("?"));
    pass
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut model = "qwen3-coder:30b".to_string();
    if let Some(i) = args.iter().position(|a| a == "--model") {
        let Some(tag) = args.get(i + 1).cloned() else {
            eprintln!("--model needs a tag");
            std::process::exit(2);
        };
        model = tag;
        args.drain(i..=i + 1);
    }
    let [repo, path, before, after] = args.as_slice() else {
        eprintln!("usage: review-memo-probe [--model <tag>] <repo> <path> <before> <after>");
        std::process::exit(2);
    };
    let repo = PathBuf::from(repo);
    let before_bytes = git_show(&repo, before, path);
    let after_bytes = git_show(&repo, after, path);

    let root = std::env::temp_dir().join(format!("review-memo-probe-{}", std::process::id()));
    let file = root.join(path);
    std::fs::create_dir_all(file.parent().expect("a path has a parent")).expect("scratch root");

    let transport = Arc::new(CountingTransport {
        calls: AtomicUsize::new(0),
    });
    println!(
        "{model}  {path}  {before} ({} bytes) -> {after} ({} bytes)",
        before_bytes.len(),
        after_bytes.len()
    );

    // A then B share a store: B is the memo.
    let store = Arc::new(Store::new().expect("store"));
    let k = kernel(&root, &store, &model, &transport);
    std::fs::write(&file, &before_bytes).expect("write before");
    let a = pass("A  before, fresh store  ", &k, &transport, path);
    std::fs::write(&file, &after_bytes).expect("write after");
    let b = pass("B  after,  same store   ", &k, &transport, path);

    // C: the after file in a store that remembers nothing — 0.7.0's cost.
    let control = Arc::new(Store::new().expect("store"));
    let k2 = kernel(&root, &control, &model, &transport);
    let c = pass("C  after,  fresh store  ", &k2, &transport, path);

    // The queue-volume claim on real output: every id A minted on a region B
    // carried is still pending after B, exactly once.
    let ids = |v: &serde_json::Value, key: &str| -> Vec<String> {
        v[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let a_minted = ids(&a, "minted");
    let b_carried = ids(&b, "carried");
    let b_minted = ids(&b, "minted");
    let carried_from_a = b_carried.iter().filter(|i| a_minted.contains(i)).count();
    println!(
        "\nB carried {} finding(s), {} of them ids A minted; B minted {} new; C (no memo) minted {}.",
        b_carried.len(),
        carried_from_a,
        b_minted.len(),
        ids(&c, "minted").len(),
    );
    println!(
        "Queue growth for this commit: {} with the memo, {} without.",
        b_minted.len(),
        ids(&c, "minted").len()
    );
    std::fs::remove_dir_all(&root).ok();
}
