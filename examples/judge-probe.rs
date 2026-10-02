//! The judge measurement harness: run the judge over the eval set
//! (`tests/corpus/judge/`) and write one verdict per entry, in the format
//! `tests/corpus/judge/score.py` scores.
//!
//!   cargo run --example judge-probe -- [--model <ollama tag>] [--base-url <url>] \
//!       [--split dev|holdout|all] [--scope serious|all] [--limit <n>] [--entry <name>]... \
//!       [--repos ~/git-personal] [--trees <dir>] <out.jsonl>
//!
//! Writes `<out.jsonl>` (the RULE's verdict, which is what ships) and
//! `<out>.stated.jsonl` (the model's own `VERDICT:` line), both from the same
//! calls, so the two can be compared without paying twice.
//!
//! ★ IT GOES THROUGH THE RESOURCE. Each entry is judged by `Source
//! urn:repo:probe:judge:{path}` — the same site, test index, prompt and rule the
//! review pass runs on its serious findings — so what is measured is what
//! ships, not a transliteration of it.
//!
//! The ROOT is the entry's repository AT the commit the exporter recorded
//! (`git archive <commit>` into `--trees`, one directory per repo@commit,
//! reused across runs), because the judge's test index reads the whole
//! repository and must see it as it was. The reviewed file inside it is then
//! replaced by the corpus's own copy when the two differ, so the text judged is
//! byte for byte what the reviewer saw (it never differs on today's export).
//!
//! Needs the local repos (for `git archive`) and a local Ollama (or any
//! OpenAI-compatible server via `--base-url`) serving `--model`. Each entry is
//! one model call; the count is printed per line and summed at the end — the
//! backend is shared with the live review passes, so bound a run with
//! `--split`/`--limit` and say how many calls it took.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use ikigai_core::{ArgRef, Capability, Fallback, Iri, Kernel, Request, SystemClock, Verb};
use ikigai_llm::{OpenAiConfig, Registry};
use oxigraph::store::Store;

/// The blocking ureq transport the review probe uses (redirects not followed;
/// a long timeout, because a cold 30B model can take a minute to load).
struct UreqTransport;

#[async_trait::async_trait]
impl ikigai_http::HttpTransport for UreqTransport {
    async fn send(
        &self,
        request: ikigai_http::HttpRequest,
    ) -> std::result::Result<ikigai_http::HttpResponse, String> {
        use std::io::Read;
        let agent = ureq::builder()
            .redirects(0)
            .timeout(std::time::Duration::from_secs(900))
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

fn take_flag(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    let Some(value) = args.get(i + 1).cloned() else {
        eprintln!("{flag} needs a value");
        std::process::exit(2);
    };
    args.drain(i..=i + 1);
    Some(value)
}

fn take_all(args: &mut Vec<String>, flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(v) = take_flag(args, flag) {
        out.push(v);
    }
    out
}

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").expect("HOME")).join(rest),
        None => PathBuf::from(path),
    }
}

/// `repos/<repo>` at `commit`, extracted once into `trees/<repo>@<commit>`.
fn tree_at(repos: &Path, trees: &Path, repo: &str, commit: &str) -> Result<PathBuf, String> {
    let dir = trees.join(format!("{repo}@{commit}"));
    if dir.join(".extracted").exists() {
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut archive = Command::new("git")
        .arg("-C")
        .arg(repos.join(repo))
        .args(["archive", commit])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git archive: {e}"))?;
    let tar = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(&dir)
        .stdin(archive.stdout.take().expect("piped"))
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    let git = archive.wait().map_err(|e| e.to_string())?;
    if !git.success() || !tar.success() {
        return Err(format!("could not extract {repo}@{commit}"));
    }
    std::fs::write(dir.join(".extracted"), b"").map_err(|e| e.to_string())?;
    Ok(dir)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let model =
        take_flag(&mut args, "--model").unwrap_or_else(|| "qwen3-coder:30b-a3b-q8_0".to_string());
    let base_url = take_flag(&mut args, "--base-url");
    let split = take_flag(&mut args, "--split").unwrap_or_else(|| "all".to_string());
    let scope = take_flag(&mut args, "--scope").unwrap_or_else(|| "serious".to_string());
    let limit: usize = take_flag(&mut args, "--limit")
        .map(|n| n.parse().expect("--limit is a count"))
        .unwrap_or(usize::MAX);
    let only = take_all(&mut args, "--entry");
    let repos = expand(&take_flag(&mut args, "--repos").unwrap_or_else(|| "~/git-personal".into()));
    let corpus_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/judge");
    let trees = take_flag(&mut args, "--trees")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("ikigai-browse-judge-trees"));
    let [out] = args.as_slice() else {
        eprintln!(
            "usage: judge-probe [--model <tag>] [--base-url <url>] [--split dev|holdout|all] \
             [--scope serious|all] [--limit <n>] [--entry <name>]... [--repos <dir>] \
             [--trees <dir>] <out.jsonl>"
        );
        std::process::exit(2);
    };
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(corpus_dir.join("corpus.json")).expect("corpus.json"),
    )
    .expect("json");
    let entries: Vec<&serde_json::Value> = manifest["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter(|e| e["file"].is_string())
        .filter(|e| split == "all" || e["split"] == split.as_str())
        .filter(|e| scope == "all" || matches!(e["severity"].as_str(), Some("critical" | "major")))
        .filter(|e| only.is_empty() || only.iter().any(|o| e["entry"] == o.as_str()))
        .take(limit)
        .collect();

    let mut coder = OpenAiConfig::ollama(&model);
    coder.provider = "coder".to_string();
    if let Some(url) = base_url {
        coder.base_url = url;
    }
    let registry = Registry {
        default: "coder".to_string(),
        providers: vec![coder],
    };
    let mut rule = std::fs::File::create(out).expect("out");
    let mut stated = std::fs::File::create(format!(
        "{}.stated.jsonl",
        out.strip_suffix(".jsonl").unwrap_or(out)
    ))
    .expect("stated out");
    let cap = Capability::scoped(["urn:cap:browse:read:probe", "urn:cap:net:localhost"]);
    let (mut calls, started_all) = (0usize, Instant::now());
    for entry in &entries {
        let name = entry["entry"].as_str().unwrap();
        let repo = entry["repo"].as_str().unwrap();
        let path = entry["path"].as_str().unwrap();
        let commit = entry["commit"].as_str().unwrap();
        let root = match tree_at(&repos, &trees, repo, commit) {
            Ok(root) => root,
            Err(e) => {
                println!("{name}  SKIPPED  {e}");
                continue;
            }
        };
        // The bytes the reviewer saw, exactly.
        let reviewed = std::fs::read(corpus_dir.join(entry["file"].as_str().unwrap())).unwrap();
        let target = root.join(path);
        if std::fs::read(&target).ok().as_deref() != Some(&reviewed[..]) {
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, &reviewed).unwrap();
        }
        let store = Arc::new(Store::new().expect("store"));
        let browse = ikigai_browse::space_with_explain(
            [("probe".to_string(), root.clone())],
            ikigai_browse::ExplainConfig::new(store),
        );
        let llm = ikigai_llm::space(Arc::new(UreqTransport), registry.clone());
        let kernel = Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(llm),
        ])))
        .with_clock(Arc::new(SystemClock));
        let mut request = Request::new(
            Verb::Source,
            Iri::parse(format!("urn:repo:probe:judge:{}", path.replace(' ', "%20"))).expect("iri"),
        )
        .with_arg(
            "quote",
            ArgRef::Inline(entry["quote"].as_str().unwrap().into()),
        )
        .with_arg(
            "claim",
            ArgRef::Inline(entry["claim"].as_str().unwrap().into()),
        )
        .with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
        if let Some(severity) = entry["severity"].as_str() {
            request = request.with_arg("severity", ArgRef::Inline(severity.into()));
        }
        if let Some(start) = entry["char_start"].as_u64() {
            request = request.with_arg("start", ArgRef::Inline(start.to_string().into_bytes()));
        }
        let started = Instant::now();
        calls += 1;
        let (row, stated_row) = match futures::executor::block_on(kernel.issue(request, &cap)) {
            Ok(repr) => {
                let mut v: serde_json::Value =
                    serde_json::from_slice(&repr.bytes).expect("json verdict");
                v["entry"] = name.into();
                v["calls"] = 1.into();
                v["seconds"] = started.elapsed().as_secs_f64().into();
                let mut s = v.clone();
                s["verdict"] = v["stated"].as_str().unwrap_or("unsure").into();
                (v, s)
            }
            Err(e) => {
                let v = serde_json::json!({
                    "entry": name, "verdict": "unsure", "calls": 1,
                    "error": format!("{e:?}"), "seconds": started.elapsed().as_secs_f64(),
                });
                (v.clone(), v)
            }
        };
        println!(
            "{model}  {name}  {:5.1}s  {} (stated {})  [{}]",
            started.elapsed().as_secs_f64(),
            row["verdict"].as_str().unwrap_or("?"),
            row["stated"].as_str().unwrap_or("-"),
            entry["label"].as_str().unwrap_or("?"),
        );
        writeln!(rule, "{row}").unwrap();
        writeln!(stated, "{stated_row}").unwrap();
    }
    println!(
        "{model}: {} entries, {calls} calls, {:.0?}",
        entries.len(),
        started_all.elapsed()
    );
}
