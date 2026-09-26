//! The prompt-measurement harness: N fresh review passes over the named files,
//! written to disk unparsed, so two prompt wordings can be compared on
//! identical inputs.
//!
//!   cargo run --example review-probe -- [--model <tag>] [--lens <name> | --guidance <file>] \
//!       <root> <out-dir> <n> <path>...
//!
//! Each answer lands in `<out-dir>/<path with / replaced by _>.<i>.txt`.
//!
//! `--model` names the Ollama tag to review with; it defaults to the review
//! tier's own model, so every invocation written before the flag existed still
//! means what it meant. ⚠ The flag varies the MODEL, never the provider LABEL:
//! the registry entry stays `coder`, because the pass tag is derived from the
//! provider and a label asserted by this file would be a claim rather than a
//! resolution. Two arms therefore produce answers that are indistinguishable
//! by tag — keep them in separate out-dirs, which is what the arm loop does.
//! Added for the model bake-off (ledger #491): the harness built to compare
//! passes could not vary the one thing that comparison varies.
//!
//! `--lens <name>` asks the review face for a SHIPPED lens (`lens=<name>` on
//! the request — `intent` is the first; the manifold's `one_of` is the list).
//! `--guidance <file>` runs a CANDIDATE lens: the file's text goes between
//! the per-file instruction and the file through `ExplainConfig::review_guidance`,
//! which is how `intent` was measured before it shipped (ledger #456). Either
//! way the format contract, the reminder and the system prompt are untouched,
//! so a lensed arm and a plain arm differ in exactly one thing. Keep arms in
//! separate out-dirs.
//!
//! ⚠⚠ IT USES `debug=raw`, AND THAT IS THE WHOLE POINT. A pass is archived by
//! `(path, content-hash, prompt-tag, model)`, so running the ordinary face
//! twice over one file is an ARCHIVE HIT: it answers 100% agreement and zero
//! variance to any question you ask it, which reads as a strong result and is
//! the archive quoting itself. `debug=raw` derives fresh, consults nothing and
//! writes nothing — no annotation, no archive entry, no poisoned key. Three
//! prompt arcs have needed this (ledger #449, #450, #483) and each rebuilt it,
//! which is why it is committed. (It is also the only face a `--guidance` pass
//! may run under, for the same reason: candidate guidance is outside the key.)
//!
//! ⚠ The answers are UNPARSED on purpose. The pass discards a finding whose
//! quote does not occur in the file, so anything measured through the ordinary
//! face has already had its orphans removed and cannot report an orphan rate.
//! Anchor them yourself — substring containment against the file is exactly
//! what the pass does — and count both populations. `tests/corpus/intent/score.py`
//! does that for the intent corpus.
//!
//! Needs a local Ollama with the named model pulled:
//!   ollama pull qwen3-coder:30b

use std::sync::Arc;
use std::time::Instant;

use ikigai_core::{ArgRef, Capability, Fallback, Iri, Kernel, Request, SystemClock, Verb};
use ikigai_llm::{OpenAiConfig, Registry};
use oxigraph::store::Store;

/// A blocking ureq transport (the ikigai-embedded pattern): runtime-free, and
/// redirects are NOT followed here — the endpoint follows them, re-running the
/// net-capability ACL against every hop.
struct UreqTransport;

#[async_trait::async_trait]
impl ikigai_http::HttpTransport for UreqTransport {
    async fn send(
        &self,
        request: ikigai_http::HttpRequest,
    ) -> std::result::Result<ikigai_http::HttpResponse, String> {
        use std::io::Read;
        // A whole-file pass is several calls of tens of seconds each; the
        // default agent timeout is shorter than one of them on a cold model.
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

/// Strip `--<flag> <value>` from `args`, returning the value if the flag was
/// given. A flag without a value is a usage error, not a silent default.
fn take_flag(args: &mut Vec<String>, flag: &str, example: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    let Some(value) = args.get(i + 1).cloned() else {
        eprintln!("{flag} needs a value, e.g. {flag} {example}");
        std::process::exit(2);
    };
    args.drain(i..=i + 1);
    Some(value)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Flags are stripped before the positional match so the shape below is
    // unchanged. Absent, the incumbent model stands: a default here is what
    // keeps the invocations in #449, #450 and #483 meaning what they meant.
    let model = take_flag(&mut args, "--model", "qwen3-coder-next:latest")
        .unwrap_or_else(|| "qwen3-coder:30b".to_string());
    let lens = take_flag(&mut args, "--lens", "intent");
    let guidance = take_flag(&mut args, "--guidance", "candidate-lens.txt").map(|file| {
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("--guidance {file}: {e}"))
            .trim()
            .to_string();
        (file, text)
    });
    if lens.is_some() && guidance.is_some() {
        // The review face refuses the combination too; say it here first.
        eprintln!(
            "--lens and --guidance are exclusive: a candidate is measured against the plain pass"
        );
        std::process::exit(2);
    }
    let [root, out, repeats, paths @ ..] = args.as_slice() else {
        eprintln!(
            "usage: review-probe [--model <tag>] [--lens <name> | --guidance <file>] \
             <root> <out-dir> <n> <path>..."
        );
        std::process::exit(2);
    };
    let repeats: usize = repeats.parse().expect("<n> must be a count");
    assert!(!paths.is_empty(), "name at least one path to review");
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).expect("out dir");

    // One provider, named `coder`, which is the review tier's default
    // (`ExplainConfig::review_provider`). Naming it here rather than labelling
    // the model keeps the tag honest: the identity is resolved from the
    // provider, not asserted by this file.
    let mut coder = OpenAiConfig::ollama(&model);
    coder.provider = "coder".to_string();
    let registry = Registry {
        default: "coder".to_string(),
        providers: vec![coder],
    };
    // In-memory: `debug=raw` archives nothing, so the store is only here
    // because the config takes one.
    let store = Arc::new(Store::new().expect("store"));
    let mut config = ikigai_browse::ExplainConfig::new(Arc::clone(&store));
    if let Some((_, text)) = &guidance {
        config = config.review_guidance(text.clone());
    }
    let browse = ikigai_browse::space_with_explain(
        [("probe".to_string(), std::path::PathBuf::from(root))],
        config,
    );
    let llm = ikigai_llm::space(Arc::new(UreqTransport), registry);
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(browse),
        Arc::new(llm),
    ])))
    .with_clock(Arc::new(SystemClock));
    // Exactly what a review needs: read this root, reach the model, mint.
    let cap = Capability::scoped([
        "urn:cap:browse:read:probe",
        "urn:cap:net:localhost",
        "urn:cap:annotate",
    ]);
    let arm = match (&lens, &guidance) {
        (Some(name), _) => format!("{model}+{name}"),
        (None, Some((file, _))) => format!("{model}+guidance:{file}"),
        (None, None) => model.clone(),
    };

    for path in paths {
        for i in 0..repeats {
            let iri = format!("urn:repo:probe:review:{path}");
            let started = Instant::now();
            let mut request = Request::new(Verb::Source, Iri::parse(&iri).expect("iri"))
                .with_arg("debug", ArgRef::Inline(b"raw".to_vec()));
            if let Some(name) = &lens {
                request = request.with_arg("lens", ArgRef::Inline(name.as_bytes().to_vec()));
            }
            let file = out.join(format!("{}.{i}.txt", path.replace('/', "_")));
            match futures::executor::block_on(kernel.issue(request, &cap)) {
                Ok(repr) => {
                    std::fs::write(&file, &repr.bytes).expect("write");
                    // ⚠ The arm (model, and lens if any) is printed on every
                    // line, not once at the top: these runs are read from a
                    // scrollback or a tee'd log days later, and a header
                    // scrolls away while a per-line tag cannot. Wall time is
                    // printed for the same reason it is measured — ollama
                    // serves one request at a time, so a slower arm costs
                    // queue drain rate, not just patience.
                    println!(
                        "{arm}  {path} #{i}  {:.1?}  {} bytes",
                        started.elapsed(),
                        repr.bytes.len()
                    );
                }
                // ⚠ A failed pass is RECORDED rather than skipped: a missing
                // file in the output directory and a failed one look identical
                // when the analysis runs days later.
                Err(e) => {
                    std::fs::write(&file, format!("ERROR: {e:?}")).expect("write");
                    println!("{arm}  {path} #{i}  ERROR {e:?}");
                }
            }
        }
    }
}
