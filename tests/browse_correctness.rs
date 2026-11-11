//! Hermetic edge cases exercised by the correctness audit (ledger #3488).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "browse-correctness-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn kernel(&self) -> Kernel {
        Kernel::new(Arc::new(ikigai_browse::space(vec![(
            "audit".into(),
            self.0.clone(),
        )])))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn read(k: &Kernel, iri: &str, face: &str) -> Vec<u8> {
    let request = Request::new(Verb::Source, Iri::parse(iri).unwrap())
        .with_arg("as", ArgRef::Inline(face.as_bytes().to_vec()));
    block_on(k.issue(request, &Capability::scoped(["urn:cap:browse:read:audit"])))
        .unwrap()
        .bytes
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Audit",
            "-c",
            "user.email=audit@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn state(k: &Kernel) -> serde_json::Value {
    serde_json::from_slice(&read(k, "urn:repo:audit:state", "application/json")).unwrap()
}

#[test]
fn unusual_names_round_trip_and_tree_order_is_stable() {
    let root = Scratch::new();
    let long = format!("{}.txt", "x".repeat(240));
    let mut names = vec![
        " leading.txt",
        "-dash.txt",
        "naïve 日本語.txt",
        "sp ace.txt",
        "trailing.txt ",
        &long,
    ];
    for name in &names {
        std::fs::write(root.0.join(name), name.as_bytes()).unwrap();
    }
    std::fs::write(root.0.join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(root.0.join("ignored.txt"), "still browsable").unwrap();
    std::fs::create_dir(root.0.join("empty")).unwrap();
    let k = root.kernel();
    for name in &names {
        let iri = ikigai_browse::file_iri("audit", name);
        assert_eq!(read(&k, &iri, "text/plain"), name.as_bytes());
        let html = String::from_utf8(read(&k, &iri, "text/html")).unwrap();
        assert!(html.contains("id=\"L1\""), "{name:?}: {html}");
    }
    names.extend([".gitignore", "ignored.txt"]);
    names.sort_unstable();
    let expected = std::iter::once("empty\tdir\t-".to_string())
        .chain(names.iter().map(|name| {
            let size = std::fs::metadata(root.0.join(name)).unwrap().len();
            format!("{name}\tfile\t{size}")
        }))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        read(&k, "urn:repo:audit:tree", "text/plain"),
        expected.as_bytes()
    );
    assert_eq!(
        read(&k, "urn:repo:audit:tree", "text/plain"),
        expected.as_bytes()
    );
    assert!(read(&k, "urn:repo:audit:tree:empty", "text/plain").is_empty());
}

#[test]
fn state_recomputes_across_unborn_staged_renamed_detached_and_checked_out_heads() {
    let root = Scratch::new();
    git(&root.0, &["init", "-b", "main"]);
    let k = root.kernel();
    assert_eq!(state(&k), serde_json::json!({"head": null, "dirty": []}));
    std::fs::write(root.0.join("-naïve name.txt"), "first\n").unwrap();
    assert_eq!(state(&k)["dirty"], serde_json::json!(["-naïve name.txt"]));
    git(&root.0, &["add", "--", "-naïve name.txt"]);
    assert_eq!(state(&k)["dirty"], serde_json::json!(["-naïve name.txt"]));
    git(&root.0, &["commit", "-m", "first"]);
    let first = git(&root.0, &["rev-parse", "HEAD"]);
    assert_eq!(state(&k), serde_json::json!({"head": first, "dirty": []}));
    git(&root.0, &["mv", "--", "-naïve name.txt", "new name.txt"]);
    assert_eq!(
        state(&k)["dirty"],
        serde_json::json!(["-naïve name.txt -> new name.txt"])
    );
    git(&root.0, &["commit", "-m", "rename"]);
    let second = git(&root.0, &["rev-parse", "HEAD"]);
    assert_ne!(first, second);
    assert_eq!(state(&k)["head"], second);
    git(&root.0, &["checkout", "--detach", &first]);
    assert_eq!(state(&k), serde_json::json!({"head": first, "dirty": []}));
    assert_eq!(
        read(
            &k,
            &ikigai_browse::file_iri("audit", "-naïve name.txt"),
            "text/plain"
        ),
        b"first\n"
    );
    git(&root.0, &["checkout", "main"]);
    assert_eq!(state(&k), serde_json::json!({"head": second, "dirty": []}));
    assert_eq!(
        read(&k, "urn:repo:audit:file:new%20name.txt", "text/plain"),
        b"first\n"
    );
}

#[test]
fn a_conflicted_merge_reports_the_path_and_keeps_head_until_commit() {
    let root = Scratch::new();
    git(&root.0, &["init", "-b", "main"]);
    std::fs::write(root.0.join("file.txt"), "base\n").unwrap();
    git(&root.0, &["add", "."]);
    git(&root.0, &["commit", "-m", "base"]);
    git(&root.0, &["checkout", "-b", "side"]);
    std::fs::write(root.0.join("file.txt"), "side\n").unwrap();
    git(&root.0, &["commit", "-am", "side"]);
    git(&root.0, &["checkout", "main"]);
    std::fs::write(root.0.join("file.txt"), "main\n").unwrap();
    git(&root.0, &["commit", "-am", "main"]);
    let head = git(&root.0, &["rev-parse", "HEAD"]);
    let k = root.kernel();
    assert_eq!(state(&k)["dirty"], serde_json::json!([]));
    let merge = Command::new("git")
        .arg("-C")
        .arg(&root.0)
        .args([
            "-c",
            "user.name=Audit",
            "-c",
            "user.email=audit@example.invalid",
            "merge",
            "side",
        ])
        .output()
        .unwrap();
    assert_eq!(merge.status.code(), Some(1), "{merge:?}");
    assert!(root.0.join(".git/MERGE_HEAD").exists());
    assert_eq!(
        state(&k),
        serde_json::json!({"head": head, "dirty": ["file.txt"]})
    );
    std::fs::write(root.0.join("file.txt"), "resolved\n").unwrap();
    git(&root.0, &["add", "."]);
    git(&root.0, &["commit", "-m", "merge"]);
    assert_ne!(state(&k)["head"], head);
    assert_eq!(state(&k)["dirty"], serde_json::json!([]));
}

#[test]
fn markdown_html_is_displayed_as_code_and_binary_content_is_labeled() {
    let root = Scratch::new();
    let markdown = b"# note\n<script>alert('example')</script>\n<div>example</div>\n";
    std::fs::write(root.0.join("note.md"), markdown).unwrap();
    std::fs::write(root.0.join("binary.bin"), [0xff, 0x00, 0xfe]).unwrap();
    let k = root.kernel();
    assert_eq!(
        read(&k, "urn:repo:audit:file:note.md", "text/plain"),
        markdown
    );
    let html = String::from_utf8(read(&k, "urn:repo:audit:file:note.md", "text/html")).unwrap();
    assert!(html.contains("browse-code"), "{html}");
    assert!(!html.contains("<script>"), "{html}");
    assert!(!html.contains("<div>example</div>"), "{html}");
    assert!(html.contains("&lt;"), "{html}");
    let binary =
        String::from_utf8(read(&k, "urn:repo:audit:file:binary.bin", "text/html")).unwrap();
    assert!(
        binary.contains("binary file — 3 B (application/octet-stream)"),
        "{binary}"
    );
    assert!(!binary.contains("browse-code"), "{binary}");
}
