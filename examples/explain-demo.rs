//! ikigai-browse explaining its own repository with REAL models — the S1
//! machinery end to end: content hashes through the kernel, per-grain prompts
//! to tiered providers, and the archive proving its economics (run it twice —
//! the second pass derives nothing).
//!
//!   cargo run --example explain-demo [path ...]
//!
//! Needs a local Ollama with the bake-off models pulled:
//!   ollama pull qwen3-coder:30b     # file grain
//!   ollama pull llama3.3:latest     # directory rollup
//!
//! With no arguments it explains `src/lib.rs` and `src` — one file, one
//! directory — then lists what the archive holds for each.

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
        let agent = ureq::builder().redirects(0).build();
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

fn main() {
    // The bake-off tiers, as local Ollama providers: `coder` binds
    // urn:llm:coder:ask (the file grain's default provider); `rollup` is the
    // registry default, so the facade urn:llm:ask (the dir grain's default
    // provider) routes to it.
    let mut coder = OpenAiConfig::ollama("qwen3-coder:30b");
    coder.provider = "coder".to_string();
    let mut rollup = OpenAiConfig::ollama("llama3.3:latest");
    rollup.provider = "rollup".to_string();
    let registry = Registry {
        default: "rollup".to_string(),
        providers: vec![coder, rollup],
    };

    // ONE shared store — in-memory here, so the archive lives for the run;
    // a host wanting it to outlive the process injects a persistent handle.
    let store = Arc::new(Store::new().expect("store"));
    let config = ikigai_browse::ExplainConfig::new(Arc::clone(&store))
        .file_model_label("qwen3-coder:30b")
        .dir_model_label("llama3.3:70b");

    let browse = ikigai_browse::space_with_explain(
        [(
            "self".to_string(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        )],
        config,
    );
    let llm = ikigai_llm::space(Arc::new(UreqTransport), registry);
    // The clock gives archive entries their ik:derivedAt provenance.
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(browse),
        Arc::new(llm),
    ])))
    .with_clock(Arc::new(SystemClock));

    // Exactly what the work needs: browse this root, reach localhost.
    let cap = Capability::scoped(["urn:cap:browse:read:self", "urn:cap:net:localhost"]);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths: Vec<String> = if args.is_empty() {
        vec!["src/lib.rs".to_string(), "src".to_string()]
    } else {
        args
    };

    for path in &paths {
        for pass in ["derive", "archive hit"] {
            let iri = format!("urn:repo:self:explain:{path}");
            let started = Instant::now();
            let request = Request::new(Verb::Source, Iri::parse(&iri).unwrap())
                .with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
            match futures::executor::block_on(kernel.issue(request, &cap)) {
                Ok(repr) => {
                    let body = String::from_utf8_lossy(&repr.bytes).into_owned();
                    let row: serde_json::Value = serde_json::from_str(&body).unwrap();
                    println!(
                        "\n$ source {iri}   [{pass}: {:.1?}, derived={} tag={}]",
                        started.elapsed(),
                        row["derived"],
                        row["version_tag"],
                    );
                    println!("{}", row["text"].as_str().unwrap_or(""));
                }
                Err(e) => {
                    println!("\n$ source {iri}\n  error: {e:?}");
                    break;
                }
            }
        }
        let versions = format!("urn:repo:self:explain-versions:{path}");
        let request = Request::new(Verb::Source, Iri::parse(&versions).unwrap());
        if let Ok(repr) = futures::executor::block_on(kernel.issue(request, &cap)) {
            println!(
                "\n$ source {versions}\n{}",
                String::from_utf8_lossy(&repr.bytes)
            );
        }
    }
}
