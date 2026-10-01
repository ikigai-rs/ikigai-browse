//! `cargo run --release --example watch-cost -- <backup.nq.gz> [--root name=path …]
//! [--seconds N] [--poll-ms M] [--build DIR]` — what the root watch and the header badge cost
//! a gonk with MANY browse roots, idle and while a cargo build runs in one of them (ledger #667).
//!
//! ```sh
//! cargo run --release --example watch-cost -- ~/.ikigai/backups/<archive>.nq.gz \
//!     --seconds 60 --poll-ms 2000 --build <a scratch clone that is also one of the roots>
//! ```
//!
//! The composition is the one `main` builds for a gonk with browse roots and a review queue —
//! the store loaded from a backup archive into RAM, browse wired over the WATCHED roots (so
//! their file reads are cached under the root threads), the real [`RootWatch`] spawned on the
//! hub, the trigger's depth bound over a scratch spaces tree, and gonk's pages in front — so a
//! badge poll here walks the same path a signed-in browser's does. Nothing opens
//! `~/.ikigai/store`, and the roots are only ever read; with `--build`, the build writes only
//! into that directory's own `target/`, which is why it should be a scratch clone.
//!
//! ★ In-process rather than a binary on a port, because the badge counts findings only for a
//! caller who can READ the roots, which over HTTP is a signed-in passkey: a script cannot hold
//! one without the ceremony. The badge is issued through the HTTP door's kernel under the
//! grant a signed-in reviewer holds, which is the path that request takes once the door has
//! minted it. ⚠ So the CPU column is this process — hub, watch, badge — and never the build.
//!
//! Two phases, each `--seconds` long, polling the badge every `--poll-ms` (an open page
//! polls every 10 s, so 2000 is about five pages): **idle**, and **build** — `cargo build`
//! running in `--build DIR` (skipped without it). Each
//! prints the process CPU over the phase, the badge latency (min / median / max), how many
//! times the watch cut a root thread (from `urn:kernel:threads`), the polls made and the clean
//! builds completed. The build phase runs `cargo clean` and `cargo build` back to back for the
//! whole phase, so a small crate keeps a build RUNNING rather than finishing in seconds. The
//! badge's tooltip is printed after the first poll and after the last, so a change to how it
//! counts is visible as a change in what it says (`WATCH_COST_RAW=1` prints its whole markup).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use ikigai_gonk::config::QueuePolicy;
use ikigai_gonk::grants::{browse_graph_grants, grants_for_all, Authority};
use ikigai_gonk::identity::Passkeys;
use ikigai_gonk::trigger::{self, Activity, Trigger};
use ikigai_gonk::watch::RootWatch;
use ikigai_gonk::{browse, compose_with, doors, queue, quic, web};
use ikigai_store::DurableStore;

fn main() {
    let mut args = std::env::args().skip(1);
    let archive = PathBuf::from(args.next().expect(
        "usage: watch-cost <backup.nq.gz> [--root name=path …] [--seconds N] [--poll-ms M] \
         [--build DIR]",
    ));
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    let (mut seconds, mut poll_ms, mut build) = (60u64, 2000u64, None::<PathBuf>);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| panic!("{arg} takes a value"));
        match arg.as_str() {
            "--root" => {
                let spec = value();
                let (name, path) = spec.split_once('=').expect("--root name=path");
                roots.push((name.to_string(), expand(path)));
            }
            "--seconds" => seconds = value().parse().expect("--seconds N"),
            "--poll-ms" => poll_ms = value().parse().expect("--poll-ms M"),
            "--build" => build = Some(expand(&value())),
            other => panic!("`{other}`: --root, --seconds, --poll-ms or --build"),
        }
    }
    if roots.is_empty() {
        roots = configured_roots();
    }

    let t = Instant::now();
    let nquads = std::process::Command::new("gzip")
        .arg("-dc")
        .arg(&archive)
        .output()
        .expect("gzip -dc");
    assert!(nquads.status.success(), "gzip -dc {}", archive.display());

    // The watch first, as `main` does: only the roots it holds are declared cacheable.
    let t_watch = Instant::now();
    let (watch, refused) = RootWatch::start(&roots);
    println!(
        "roots            {} configured, {} watched, {} refused ({:.1?} to start the watch)",
        roots.len(),
        watch.watched().len(),
        refused.len(),
        t_watch.elapsed()
    );
    let graph = browse::Graph::chosen();
    let (store, handle) = DurableStore::in_memory_shared_declaring(graph.sharer_writes())
        .expect("a shared in-memory store");
    let wired = browse::wire(roots.clone(), handle, Some(&watch), None, &graph);
    let spaces = tempfile::tempdir().expect("a scratch spaces tree");
    let review = Trigger {
        space: "reviews".to_string(),
        grant: None,
        root: spaces.path().to_path_buf(),
        arm: false,
    };
    trigger::prepare(&review).expect("the scratch review tree");
    let hub = Arc::new(compose_with(
        store,
        Some(Arc::new(wired.space)),
        Vec::new(),
        trigger::space(
            &review,
            Arc::new(Activity::default()),
            false,
            QueuePolicy::default(),
        ),
        None,
    ));
    issue(
        &hub,
        Verb::Sink,
        "urn:iki:store:load",
        &[
            ("content", nquads.stdout.as_slice()),
            ("format", b"application/n-quads"),
        ],
        &Capability::root(),
    )
    .expect("the archive loads");
    println!(
        "loaded           {:.1?}   {} bytes of N-Quads",
        t.elapsed(),
        nquads.stdout.len()
    );
    let epochs = watch.epochs();
    watch.spawn(Arc::clone(&hub));

    let config = tempfile::tempdir().expect("a config home");
    let face = Arc::new(web::Web {
        hub: Arc::clone(&hub),
        ledgers: vec!["default".to_string()],
        browse_roots: roots.iter().map(|(name, _)| name.clone()).collect(),
        passkeys: Arc::new(Passkeys::new(
            quic::Layout::in_config_home(config.path()),
            1060,
        )),
        rules: ikigai_gonk::rules::DEFAULT_RULES.into(),
        queue: QueuePolicy::default(),
        epochs: Some(epochs),
    });
    let door = doors::http_kernel(Arc::clone(&hub), web::space(face));

    // What a signed-in reviewer holds: every root, the publish token, the ledger.
    let mut scopes = vec![
        ikigai_browse::CAP_WILDCARD.to_string(),
        ikigai_browse::CAP_ANNOTATE.to_string(),
    ];
    scopes.extend(grants_for_all(&["default".to_string()], Authority::Write).expect("tokens"));
    scopes.extend(browse_graph_grants(Authority::Write).expect("a named browse graph"));
    let reviewer = Capability::scoped(scopes);

    // The first poll fills whatever there is to fill; it is reported, not counted.
    let t = Instant::now();
    let first =
        issue(&door, Verb::Source, queue::BADGE_IRI, &[], &reviewer).expect("the badge answers");
    println!(
        "first poll       {:>8.1} ms   {}",
        t.elapsed().as_secs_f64() * 1000.0,
        badge_summary(&first)
    );
    // Let the platform deliver whatever the watch's own startup and the load stirred up.
    std::thread::sleep(Duration::from_secs(2));

    println!(
        "{:<8} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9} {:>7} {:>6} {:>7}",
        "phase",
        "wall s",
        "cpu s",
        "cpu %",
        "min ms",
        "p50 ms",
        "max ms",
        "cuts",
        "polls",
        "builds"
    );
    let phase = |name: &str, build: Option<&PathBuf>| {
        let (cpu0, cuts0, t0) = (cpu_seconds(), root_cuts(&hub), Instant::now());
        let (mut latencies, mut builds) = (Vec::new(), 0usize);
        let mut child: Option<std::process::Child> = None;
        while t0.elapsed() < Duration::from_secs(seconds) {
            // A clean build, again and again: one build of a small crate is over in seconds,
            // and the phase is about a build that is RUNNING.
            if let Some(dir) = build {
                let finished = match child.as_mut() {
                    None => true,
                    Some(c) => c.try_wait().expect("the build's status").is_some(),
                };
                if finished {
                    builds += usize::from(child.is_some());
                    cargo(dir, "clean").wait().expect("cargo clean");
                    child = Some(cargo(dir, "build"));
                }
            }
            let t = Instant::now();
            issue(&door, Verb::Source, queue::BADGE_IRI, &[], &reviewer)
                .expect("the badge answers");
            latencies.push(t.elapsed().as_secs_f64() * 1000.0);
            std::thread::sleep(Duration::from_millis(poll_ms).saturating_sub(t.elapsed()));
        }
        if let Some(mut c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
        let (wall, cpu) = (t0.elapsed().as_secs_f64(), cpu_seconds() - cpu0);
        latencies.sort_by(f64::total_cmp);
        let at = |q: f64| latencies[((latencies.len() - 1) as f64 * q) as usize];
        println!(
            "{name:<8} {wall:>7.1} {cpu:>9.2} {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>7} {:>6} {builds:>7}",
            100.0 * cpu / wall,
            at(0.0),
            at(0.5),
            at(1.0),
            root_cuts(&hub) - cuts0,
            latencies.len(),
        );
    };
    phase("idle", None);
    if let Some(dir) = &build {
        phase("build", Some(dir));
    }
    let last =
        issue(&door, Verb::Source, queue::BADGE_IRI, &[], &reviewer).expect("the badge answers");
    println!("last poll        {}", badge_summary(&last));
}

/// `cargo <verb>` in `dir`, quietly, writing only into that directory's own `target/`.
fn cargo(dir: &PathBuf, verb: &str) -> std::process::Child {
    std::process::Command::new("cargo")
        .arg(verb)
        .arg("--quiet")
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("cargo {verb}: {e}"))
}

/// What the badge says: its tooltip (the depth sentence and the two counts) and its revision.
fn badge_summary(html: &str) -> String {
    if std::env::var_os("WATCH_COST_RAW").is_some() {
        return html.to_string();
    }
    let attr = |name: &str| {
        let key = format!("{name}='");
        html.find(&key)
            .map(|at| {
                html[at + key.len()..]
                    .chars()
                    .take_while(|c| *c != '\'')
                    .collect::<String>()
            })
            .unwrap_or_default()
    };
    format!("{} [rev {}]", attr("title"), attr("data-rev"))
}

/// How many times a root thread has been cut, summed over roots (`urn:kernel:threads`).
fn root_cuts(hub: &Kernel) -> u64 {
    issue(
        hub,
        Verb::Source,
        "urn:kernel:threads",
        &[],
        &Capability::root(),
    )
    .expect("urn:kernel:threads")
    .lines()
    .filter_map(|line| {
        let mut words = line.split_whitespace();
        let thread = words.next()?;
        thread
            .starts_with("urn:iki:gonk:browse:root:")
            .then_some(())?;
        words.nth(1)?.parse::<u64>().ok()
    })
    .sum()
}

/// This process's CPU time so far (user + system), from `ps` — `[[dd-]hh:]mm:ss.cc`.
fn cpu_seconds() -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "time=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps");
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    let (days, rest) = match text.split_once('-') {
        Some((d, rest)) => (d.parse::<f64>().unwrap_or(0.0), rest),
        None => (0.0, text),
    };
    rest.split(':').fold(0.0, |acc, part| {
        acc * 60.0 + part.parse::<f64>().unwrap_or(0.0)
    }) + days * 86_400.0
}

fn issue(
    kernel: &Kernel,
    verb: Verb,
    iri: &str,
    args: &[(&str, &[u8])],
    cap: &Capability,
) -> Result<String, String> {
    let request = args.iter().fold(
        Request::new(verb, Iri::parse(iri).expect("an IRI")),
        |request, (name, value)| request.with_arg(*name, ArgRef::Inline(value.to_vec())),
    );
    block_on(kernel.issue(request, cap))
        .map(|repr| String::from_utf8_lossy(&repr.bytes).into_owned())
        .map_err(|e| format!("{verb:?} {iri}: {e}"))
}

/// `gonk.browse.root = "name=~/path"` lines of the operator's config, `~` expanded.
fn configured_roots() -> Vec<(String, PathBuf)> {
    let home = std::env::var_os("HOME").map(PathBuf::from).expect("HOME");
    let text = std::fs::read_to_string(home.join(".config/ikigai/config.toml"))
        .expect("~/.config/ikigai/config.toml, or --root name=path");
    let mut roots: Vec<(String, PathBuf)> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("gonk.browse.root") else {
            continue;
        };
        let Some(value) = rest.trim().strip_prefix('=') else {
            continue;
        };
        if let Some((name, path)) = value.trim().trim_matches('"').split_once('=') {
            if !roots.iter().any(|(n, _)| n == name) {
                roots.push((name.to_string(), expand(path)));
            }
        }
    }
    roots
}

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME")
            .join(rest),
        None => PathBuf::from(path),
    }
}
