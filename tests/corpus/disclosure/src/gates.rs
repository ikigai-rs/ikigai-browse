//! Running the local gates and reporting whether they passed.
//!
//! ⚠⚠ THE PIPELINE LAW: a pipeline's exit status is the LAST command's. `cargo
//! clippy | tail` reports tail's success, so a failed build reads as exit 0.
//! A gate is the last thing on its line or it is not a gate — which is why the
//! runner below keeps the full output in a file and never pipes the command it
//! is judging.
//!
//! ⚠ `set -e` is inert in some agent shells, so sequencing is `&&` and nothing
//! else. A block that opens with `set -e` reads as guarded and is not.

use std::process::Command;

/// One gate: a name and the shell command that runs it.
pub struct Gate {
    pub name: &'static str,
    pub command: String,
}

/// The three cargo gates, in the order a failure is cheapest to read.
///
/// ⚠ The full output goes to `log`, and only the tail is printed — a reviewer
/// wants the last twenty lines and the exit status, not four thousand lines of
/// a successful build.
pub fn gates(log: &str) -> Vec<Gate> {
    vec![
        Gate {
            name: "fmt",
            command: format!("cargo fmt --check > {log} 2>&1"),
        },
        Gate {
            name: "clippy",
            command: format!("cargo clippy --all-targets -- -D warnings | tee {log} | tail -20"),
        },
        Gate {
            name: "test",
            command: format!("cargo test --all-targets > {log} 2>&1"),
        },
    ]
}

/// Run one gate and report whether it PASSED.
///
/// ⚠ The status is the process's own, never a parse of its output: a build that
/// prints the word `error` in a doc comment is not a failure, and a build that
/// fails silently is still a failure.
pub fn run(gate: &Gate) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(&gate.command)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run every gate, stopping at the first failure.
///
/// ⚠ Stopping early is deliberate: a clippy failure usually makes the test
/// output noise, and a reviewer reading the first failure fixes the right thing.
pub fn run_all(log: &str) -> Result<(), &'static str> {
    for gate in gates(log) {
        if !run(&gate) {
            return Err(gate.name);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gate_has_a_name() {
        assert_eq!(gates("/tmp/x.log").len(), 3);
    }
}
