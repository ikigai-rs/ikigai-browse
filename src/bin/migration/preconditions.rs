//! Operational checks shared by the migration binaries and rehearsal.
//! Kept outside the library: these inspect disk and spawn operator tools.

/// Refuse a path that is not an existing Oxigraph/RocksDB store.
///
/// ⚠ `Store::open` CREATES a store at a path that has none. A typo would
/// otherwise produce a brand-new empty store and a serene all-zero PASS — the
/// exact shape of a successful migration, reported over data that was never
/// touched. Require the RocksDB marker instead.
pub fn require_store_dir(path: &std::path::Path) -> std::result::Result<(), String> {
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
    Ok(())
}

/// Who holds a store's RocksDB lock, if anyone.
///
/// A locked store is the *expected* failure for a migration: somebody in a hurry
/// runs one against a live deployment, and "IO error: lock hold by current
/// process" is not an answer they can act on. Ask the operating system which
/// process has the LOCK file open and name it. Best-effort by design — no
/// `lsof`, or a platform where this does not work, degrades to letting
/// `Store::open` produce its own error rather than blocking the migration.
pub fn lock_holder(path: &std::path::Path) -> Option<String> {
    let lock = path.join("LOCK");
    if !lock.exists() {
        return None;
    }
    let out = std::process::Command::new("lsof")
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
            match std::process::Command::new("ps")
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
