//! What the host's own environment can make browse's git do (ledger #3492, the
//! class of ikigai-repo PR 31). This binary holds ONE test because it sets
//! process environment variables, which every thread of a test binary shares.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{Capability, Iri, Kernel, Request, Verb};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env_remove("GIT_DIR")
        .output()
        .expect("git is on PATH");
    assert!(out.status.success(), "{args:?}: {out:?}");
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ikigai-browse-host-env-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn head(dir: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .env_remove("GIT_DIR")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A host started from a git hook, or from a shell with `GIT_DIR` exported and
/// a GitHub token in its environment, passes both to every child it spawns.
/// The served root must still be the repository browse reads, and nothing git
/// runs (here the served repository's own, host-configured `core.fsmonitor`)
/// may see the token.
#[test]
fn the_host_environment_neither_redirects_nor_leaks() {
    let served = scratch("served");
    let outside = scratch("outside");
    let seen = served.with_extension("token-seen");
    git(&served, &["init", "-q"]);
    std::fs::write(served.join("a.txt"), "served\n").unwrap();
    git(&served, &["add", "a.txt"]);
    git(&served, &["commit", "-q", "-m", "served"]);
    // The host's OWN configuration of the repository it serves (trusted): a
    // probe standing in for any program git starts.
    git(
        &served,
        &[
            "config",
            "core.fsmonitor",
            &format!(
                "echo \"${{GH_TOKEN:-unset}}\" > '{}'; false",
                seen.display()
            ),
        ],
    );
    git(&outside, &["init", "-q"]);
    git(
        &outside,
        &["commit", "-q", "--allow-empty", "-m", "outside"],
    );
    let (served_head, outside_head) = (head(&served), head(&outside));

    std::env::set_var("GIT_DIR", outside.join(".git"));
    std::env::set_var("GH_TOKEN", "probe-github-token");
    let kernel = Kernel::new(Arc::new(ikigai_browse::space([(
        "demo".to_string(),
        served.clone(),
    )])));
    let request = Request::new(Verb::Source, Iri::parse("urn:repo:demo:state").unwrap());
    let cap = Capability::scoped(["urn:cap:browse:read:demo"]);
    let state = block_on(kernel.issue(request, &cap))
        .map(|answer| String::from_utf8_lossy(&answer.bytes).into_owned());
    std::env::remove_var("GIT_DIR");
    std::env::remove_var("GH_TOKEN");
    let token = std::fs::read_to_string(&seen).unwrap_or_default();
    std::fs::remove_dir_all(&served).ok();
    std::fs::remove_dir_all(&outside).ok();
    std::fs::remove_file(&seen).ok();

    let state = state.expect("state answers");
    assert!(
        !state.contains(&outside_head),
        "an inherited GIT_DIR chose the repository: {state}"
    );
    assert!(state.starts_with(&served_head), "{state}");
    // "unset" also proves the probe ran, so the check is not vacuous.
    assert_eq!(token.trim(), "unset", "git's children saw GH_TOKEN");
}
