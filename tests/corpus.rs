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

/// `tests/corpus/judge/corpus.json`: decided review findings, labeled, each with
/// the file version its pass reviewed (see that directory's README). Pins what
/// a judge run relies on: every recovered entry's file is the bytes the
/// reviewer saw (its sha256 is the pass's content hash), the quote sits where
/// the pass anchored it, every label is one the scorer knows, and nothing on
/// disk is unreferenced.
///
/// ⚠ The directory is excluded from the published crate (5 MB of other repos'
/// file versions), so outside a git checkout it is absent and this returns.
/// Inside one — `.git` beside the manifest — it must be there.
#[test]
fn every_judge_corpus_entry_is_the_reviewed_bytes_with_its_quote_at_its_anchor() {
    use sha2::{Digest, Sha256};

    let dir = corpus_dir("judge");
    let in_checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join(".git").exists();
    if !dir.join("corpus.json").exists() {
        assert!(!in_checkout, "tests/corpus/judge/corpus.json is missing");
        return;
    }
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("corpus.json")).unwrap())
            .expect("json");
    let entries = manifest["entries"].as_array().expect("entries");
    assert!(!entries.is_empty());
    let mut referenced = std::collections::BTreeSet::new();
    let mut labels = std::collections::BTreeMap::<String, usize>::new();
    for entry in entries {
        let name = entry["entry"].as_str().expect("entry");
        let label = entry["label"].as_str().expect("label");
        assert!(
            ["known-false", "known-real"].contains(&label),
            "{name}: label `{label}`"
        );
        *labels.entry(label.to_string()).or_default() += 1;
        if label == "known-false" {
            let reason = entry["reason"].as_str().unwrap_or_default();
            assert!(
                ["misread", "restates", "no-issue"].contains(&reason),
                "{name}: a known-false entry must carry the decline word that says why, got `{reason}`"
            );
        } else {
            let basis = entry["basis"].as_str().unwrap_or_default();
            assert!(
                [
                    "verified-real",
                    "published",
                    "sweep-partly-real",
                    "intent-corpus"
                ]
                .contains(&basis),
                "{name}: known-real basis `{basis}` is not one the scorer groups"
            );
        }
        for key in ["repo", "path", "quote", "claim", "split", "basis"] {
            assert!(
                entry[key].as_str().is_some_and(|v| !v.is_empty()),
                "{name}: {key} missing"
            );
        }
        // Named by repo and id, never by the defect: the id is opaque.
        let id = entry["id"].as_str().expect("id");
        assert!(
            name.ends_with(id) || name.starts_with("intent:"),
            "{name}: not repo:id"
        );
        let Some(file) = entry["file"].as_str() else {
            assert!(
                entry["unrecovered"].as_str().is_some(),
                "{name}: no file and no reason it could not be recovered"
            );
            continue;
        };
        let bytes = std::fs::read(dir.join(file))
            .unwrap_or_else(|e| panic!("{name}: {file} unreadable: {e}"));
        if file.starts_with("files/") {
            referenced.insert(file.to_string());
            let hash = format!("sha256:{:x}", Sha256::digest(&bytes));
            assert_eq!(
                Some(hash.as_str()),
                entry["content_hash"].as_str(),
                "{name}: {file} is not the version the pass reviewed"
            );
        }
        let text = String::from_utf8_lossy(&bytes);
        let quote = entry["quote"].as_str().unwrap();
        let start = entry["char_start"]
            .as_u64()
            .unwrap_or_else(|| panic!("{name}: no char_start")) as usize;
        let at: String = text
            .chars()
            .skip(start)
            .take(quote.chars().count())
            .collect();
        assert_eq!(at, quote, "{name}: the quote is not at char {start}");
    }
    assert!(labels.contains_key("known-false") && labels.contains_key("known-real"));
    // Every file on disk is some entry's: an unreferenced version is dead weight.
    let mut stack = vec![dir.join("files")];
    while let Some(d) = stack.pop() {
        for child in std::fs::read_dir(&d).unwrap() {
            let path = child.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                assert!(referenced.contains(&rel), "{rel} is referenced by no entry");
            }
        }
    }
}

/// The exporter's LABELS, pinned (ledger #696): a publish marked reproduced is
/// `verified-real`, a plain publish stays `published`, a decline is
/// known-false only with a word that says the claim does not hold — and the
/// scorer files `verified-real` with the verified half. `export.py
/// --self-test` holds the cases; this runs it, so a change to what a decision
/// row means for the eval set fails here rather than in a re-export nobody
/// reads. ⚠ It needs `python3`, and FAILS without it rather than skipping: a
/// check that silently does not run is the gate this exists to be.
#[test]
fn the_judge_exporter_labels_a_reproduced_publish_verified_real() {
    let dir = corpus_dir("judge");
    let in_checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join(".git").exists();
    if !dir.join("export.py").exists() {
        assert!(!in_checkout, "tests/corpus/judge/export.py is missing");
        return;
    }
    let out = std::process::Command::new("python3")
        .arg(dir.join("export.py"))
        .arg("--self-test")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("python3 runs (the exporter's label check needs it)");
    assert!(
        out.status.success(),
        "export.py --self-test failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("ok:"));
}
