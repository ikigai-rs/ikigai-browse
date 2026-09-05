//! `migrate-annotation-ns` — the one-shot that moves a pre-0.3.0 browse
//! store's annotations from `urn:annotation:` to `urn:iki:annotation:`.
//!
//! ```text
//! migrate-annotation-ns <store-path>            # DRY RUN (the default)
//! migrate-annotation-ns <store-path> --commit   # write
//! ```
//!
//! ★ **This binary is a stopgap for a missing primitive.** The ecosystem's
//! SPARQL surface is `urn:sparql:ask` / `construct` / `describe` / `select` —
//! there is no writing verb anywhere. When `urn:sparql:update` exists this is
//! one `DELETE … INSERT … WHERE` against a bound store, and this file should
//! be deleted rather than generalized. See [`ikigai_browse::migrate`].
//!
//! Everything interesting lives in that module, which is compiled and tested
//! by the DEFAULT build against an in-memory store. This file is only the
//! parts that need RocksDB (`Store::open`) and a terminal: argument handling,
//! the lock refusal, and the table. Keeping the split there is what lets the
//! transform be tested in ordinary CI without a C++ toolchain.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use ikigai_browse::migrate::{self, Counts};
use oxigraph::store::Store;

const USAGE: &str = "\
migrate-annotation-ns — move a browse store's annotations to urn:iki:annotation:

    migrate-annotation-ns <store-path>            dry run (default): report only
    migrate-annotation-ns <store-path> --commit   apply the migration

The store must be CLOSED: RocksDB holds an exclusive lock, so stop the ikigai
server that owns it first. ⚠ Back the directory up before --commit; this is a
one-shot destructive rewrite of production RDF.
";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let (path, commit) = parse_args()?;
    let store = open(&path)?;

    let before = migrate::counts(&store).map_err(|e| e.to_string())?;
    let plan = migrate::plan(&store).map_err(|e| e.to_string())?;

    println!("  store    : {}", path.display());
    println!("  moving   : {} quads", plan.len());
    println!();

    // The dry run's "after" column is the SAME counting function applied to a
    // projection of the plan — not a prediction written twice. A commit's is
    // the store itself.
    let (after, label) = if commit {
        migrate::apply(&store, &plan).map_err(|e| e.to_string())?;
        (migrate::counts(&store).map_err(|e| e.to_string())?, "after")
    } else {
        let projected = migrate::project(&store, &plan).map_err(|e| e.to_string())?;
        (
            migrate::counts(&projected).map_err(|e| e.to_string())?,
            "would be",
        )
    };

    print!("{}", migrate::report(&before, &after, label));
    println!();

    let passed = Counts::passed(&before, &after);
    println!(
        "  {}: annotations equal · old -> 0 · new -> old's former count · dangling -> 0",
        if passed { "PASS" } else { "FAIL" }
    );
    if !commit {
        println!();
        println!("  DRY RUN — nothing was written. Re-run with --commit to apply.");
        println!("  ⚠ back up {} first.", path.display());
    }

    Ok(if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn parse_args() -> Result<(PathBuf, bool), String> {
    let mut path: Option<PathBuf> = None;
    let mut commit = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--commit" => commit = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            // Fail loud on an unrecognized flag rather than treating it as a
            // path: `--dry-run` (which does not exist, because dry is the
            // default) must not be swallowed as a store directory.
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n\n{USAGE}"))
            }
            other if path.is_none() => path = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument {other}\n\n{USAGE}")),
        }
    }
    let path = path.ok_or_else(|| format!("a store path is required\n\n{USAGE}"))?;
    Ok((path, commit))
}

/// Open the store, refusing every way this can go quietly wrong.
fn open(path: &Path) -> Result<Store, String> {
    // ⚠ `Store::open` CREATES a store at a path that has none. A typo would
    // otherwise produce a brand-new empty store and a serene 0/0/0/0 PASS —
    // the exact shape of a successful migration, reported over data that was
    // never touched. Require the RocksDB marker instead.
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    if !path.join("CURRENT").exists() {
        return Err(format!(
            "{} does not look like an Oxigraph/RocksDB store (no CURRENT file). \
             Refusing rather than creating an empty one.",
            path.display()
        ));
    }
    if let Some(holder) = lock_holder(path) {
        return Err(format!(
            "the store is open: {holder}\n\
             RocksDB holds an exclusive lock, so this migration needs the server stopped. \
             Stop it, run this again, then restart it."
        ));
    }
    Store::open(path).map_err(|e| {
        format!(
            "cannot open {}: {e}\n\
             If this says the lock is held, an ikigai process still owns the store — stop it first.",
            path.display()
        )
    })
}

/// Who holds the store's RocksDB lock, if anyone.
///
/// A locked store is the *expected* failure here: somebody in a hurry runs
/// this against a live deployment, and "IO error: lock hold by current
/// process" is not an answer they can act on. Ask the operating system which
/// process has the LOCK file open and name it. Best-effort by design — no
/// `lsof`, or a platform where this does not work, degrades to letting
/// `Store::open` produce its own error rather than blocking the migration.
fn lock_holder(path: &Path) -> Option<String> {
    let lock = path.join("LOCK");
    if !lock.exists() {
        return None;
    }
    let out = Command::new("lsof")
        .args(["-t", "--"])
        .arg(&lock)
        .output()
        .ok()?;
    let pids: Vec<&str> = std::str::from_utf8(&out.stdout)
        .ok()?
        .split_whitespace()
        .collect();
    if pids.is_empty() {
        return None;
    }
    let described: Vec<String> = pids
        .iter()
        .map(|pid| {
            match Command::new("ps")
                .args(["-o", "command=", "-p", pid])
                .output()
            {
                Ok(o) => {
                    let command = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if command.is_empty() {
                        format!("pid {pid}")
                    } else {
                        format!("pid {pid} ({command})")
                    }
                }
                Err(_) => format!("pid {pid}"),
            }
        })
        .collect();
    Some(described.join(", "))
}
