//! The review corpora under `tests/corpus/` are INSTRUMENTS, not tests: text the
//! model is shown by `examples/review-probe.rs`, with a ground truth written
//! beside it. This file checks only that the instrument is intact — every entry
//! is tagged, every blob is present, every anchor still occurs in its blob —
//! and never that a model finds anything. Whether it does is a measurement,
//! recorded in the corpus README with the run that produced it.

use std::path::{Path, PathBuf};

fn corpus_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("corpus")
        .join(name)
}

/// `tests/corpus/intent/corpus.json`: each entry names its repo, path, the
/// commit BEFORE the fix, which of the four intent questions it belongs to,
/// verbatim anchors a correct finding would sit on, and what that finding says.
#[test]
fn every_intent_corpus_entry_is_tagged_and_its_anchors_occur_in_its_blob() {
    let dir = corpus_dir("intent");
    let manifest = std::fs::read_to_string(dir.join("corpus.json")).expect("corpus.json");
    let entries: Vec<serde_json::Value> = serde_json::from_str(&manifest).expect("json");
    assert!(!entries.is_empty());

    let mut named = std::collections::BTreeSet::new();
    for entry in &entries {
        let name = entry["entry"].as_str().expect("entry");
        named.insert(name.to_string());
        for key in ["repo", "path", "sha", "fixed_by", "finding"] {
            let value = entry[key]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: {key} missing"));
            assert!(!value.trim().is_empty(), "{name}: {key} is empty");
        }
        let sha = entry["sha"].as_str().unwrap();
        assert!(
            sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()),
            "{name}: sha `{sha}` is not a commit"
        );
        let question = entry["question"]
            .as_u64()
            .unwrap_or_else(|| panic!("{name}: question"));
        assert!(
            (1..=4).contains(&question),
            "{name}: question {question} is not one of the four"
        );
        let blob = dir.join(name).join(entry["path"].as_str().unwrap());
        let text = std::fs::read_to_string(&blob)
            .unwrap_or_else(|e| panic!("{name}: blob {} unreadable: {e}", blob.display()));
        assert!(!text.is_empty(), "{name}: blob is empty");
        let anchors = entry["anchors"].as_array().expect("anchors");
        assert!(!anchors.is_empty(), "{name}: no anchors");
        for anchor in anchors {
            let anchor = anchor.as_str().unwrap();
            assert!(
                text.contains(anchor),
                "{name}: anchor `{anchor}` does not occur in {}",
                blob.display()
            );
        }
    }

    // Every directory under the corpus is an entry in the manifest — an
    // untagged blob would be shown to the model and scored against nothing.
    for child in std::fs::read_dir(&dir).unwrap() {
        let child = child.unwrap();
        if child.file_type().unwrap().is_dir() {
            let name = child.file_name().to_string_lossy().to_string();
            assert!(named.contains(&name), "{name}/ has no entry in corpus.json");
        }
    }
}

/// The disclosure corpus has no manifest — its table lives in its README — so
/// this only pins that the nine fixtures named there are still on disk.
#[test]
fn the_disclosure_corpus_fixtures_are_present() {
    let dir = corpus_dir("disclosure");
    for path in [
        "src/paths.rs",
        "src/cache.rs",
        "src/queue.rs",
        "src/writer.rs",
        "pins.toml",
        "src/grant.rs",
        "src/anchor.rs",
        "src/gates.rs",
    ] {
        assert!(
            dir.join(path).is_file(),
            "{path} missing from the disclosure corpus"
        );
    }
}
