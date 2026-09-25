//! `superseded` — the state of an UNDECIDED finding whose file's current
//! review does not stand behind it (ledger #504).
//!
//! ## The leak this closes
//!
//! The region memo (0.8.0) bounds re-review of UNCHANGED regions: they carry.
//! Nothing retired the findings of a CHANGED region: when a pass re-derived a
//! region, the findings the previous pass minted there stayed pending forever,
//! because the only thing that ever moved a finding out of `pending` was a
//! human decision. So pending grew with COMMIT COUNT, without bound, on a
//! codebase that was not growing. Measured 2026-09-24 over the live store:
//! 1697 pending, of which 561 (191 serious) were in no standing set of their
//! file's newest pass.
//!
//! ## The rule — a pure function of the store, so it is computed, never stored
//!
//! A pass's STANDING SET is what it stands behind: the findings it minted
//! (`prov:generated`) plus the members of every region memo it carried forward
//! (`prov:used <memo>` → `prov:hadMember`) — exactly
//! [`crate::review::PassEntry::findings`], loaded through the one loader, so
//! there is one definition of it in this crate.
//!
//! An undecided finding on a file is `pending` iff it is in the standing set of
//! that file's CURRENT reading, and `superseded` otherwise. Nothing is written:
//!
//! * no migration and no sweep — the stock retires the moment this is read;
//! * no drift — there is no stored state to keep in step with the passes;
//! * ★ a REVERT is right for free. A → B → A makes the pass over A current
//!   again (an archive hit, no new derivation), and A's findings read as
//!   pending again with no code for it. A STORED transition would get that
//!   wrong, or need an un-supersede path.
//!
//! ## What "the current reading" is — rule (a), measured
//!
//! The passes whose `ik:contentHash` is the file's CURRENT content hash, when
//! any exist; otherwise the passes on the content hash of the most recently
//! derived pass (`ik:derivedAt`, ties broken by IRI; a pass archived before
//! the term existed sorts oldest).
//!
//! * The current hash costs NOTHING extra on the findings face: the listing's
//!   drift pass already fetches every target's content and hashes it
//!   ([`crate::annotate::CurrentContent`]), and the same map is handed to
//!   this module and then reused for the drift pass, so no file is read
//!   twice. The file face has the text in hand.
//! * ★ It is a SET of passes, not one: every pass on the current hash, under
//!   every tag. `provider=` is documented as "a second pass, not a
//!   replacement" — two reviewers' margins on one file — so a second model's
//!   pass must not retire the first's findings on the same bytes. The same
//!   holds across a prompt-version re-key on unchanged content. The bound
//!   this gives is codebase size × the tags that reviewed the CURRENT bytes,
//!   never commit count; the moment the file moves on and one model reviews
//!   the new bytes, the other's findings are superseded by it.
//! * The fallback is what makes an EDITED-but-not-yet-reviewed file sensible:
//!   the newest reading stands until a pass over the new bytes lands. It is
//!   also the one place rule (a) guesses — a file edited A → B → C with only
//!   A and B reviewed reads B's findings, whichever of the two C resembles.
//!
//! `superseded_by` names the NEWEST pass of the current reading, so a reader
//! can open the pass that does not carry the finding.
//!
//! ## What is never superseded
//!
//! * A DECIDED finding. Published and declined are records of a human act and
//!   never change state.
//! * A PR-family finding. It is keyed to a PR page's diff, not to a file's
//!   pass, so "the file's current review" has no meaning for it.
//! * A finding on a file with no pass on record at all — nothing to stand
//!   behind or not, so it stays pending rather than guessing.
//!
//! ⚠ **Superseded is not declined.** It asserts no human judgment — "the code
//! it was about changed, and the current review does not carry it" — so it
//! never feeds the recurrence mark: [`crate::annotate::declined_twins`]
//! consults DECLINED decisions only, and a superseded finding has no decision
//! by construction. A like claim raised later arrives unmarked.
//!
//! ⚠ A pass whose coverage is INCOMPLETE (a region's answer collapsed)
//! supersedes like any other: a collapsed region is by construction a region
//! whose bytes moved (an unchanged one would have been a memo hit, which asks
//! nothing), so its earlier findings are about bytes that changed. The pass
//! already declares the gap in its coverage note.

use std::collections::{BTreeMap, BTreeSet};

use ikigai_core::{Error, Result};
use oxigraph::model::{Literal, NamedNode, NamedOrBlankNode, Term};

use crate::annotate::{Annotation, Family, TargetRef};
use crate::archive::Archive;
use crate::explain::ik;
use crate::review::{load_pass, PASS_PREFIX};

fn store_err(e: impl std::fmt::Display) -> Error {
    Error::Endpoint(format!("browse: supersession: {e}"))
}

/// One archived pass over a file, as much of it as choosing a reading needs.
struct PassHead {
    iri: String,
    hash: String,
    derived_at: Option<String>,
}

/// The reading of one file that stands: the passes that are current and the
/// findings they stand behind.
pub(crate) struct Standing {
    /// The newest pass of the current reading — what a superseded finding
    /// names as `superseded_by`.
    pub(crate) by: String,
    /// Every finding IRI the current passes stand behind (minted or carried).
    pub(crate) members: BTreeSet<String>,
}

/// Every archived FILE pass on `(repo, rel)`, found through the `ik:path`
/// literal (one indexed pattern — the path's findings and memos come back too
/// and are dropped by prefix), each with its content hash and derivation time.
///
/// ★ The prefix carries the repo AND its trailing colon, so a root named
/// `ikigai-web` never reads the passes of `ikigai-web-demo`, and
/// [`PASS_PREFIX`]'s own colon keeps region memos out.
fn passes_on(archive: &Archive, repo: &str, rel: &str) -> Result<Vec<PassHead>> {
    let prefix = format!("{PASS_PREFIX}{repo}:");
    let path = Literal::new_simple_literal(rel);
    let mut iris = BTreeSet::new();
    for quad in
        archive.quads_for_pattern(None, Some(ik("path").as_ref()), Some(path.as_ref().into()))
    {
        let quad = quad.map_err(store_err)?;
        if let NamedOrBlankNode::NamedNode(subject) = &quad.subject {
            if subject.as_str().starts_with(&prefix) {
                iris.insert(subject.as_str().to_string());
            }
        }
    }
    let literal = |subject: &NamedNode, term: &str| -> Result<Option<String>> {
        let predicate = ik(term);
        match archive
            .quads_for_pattern(
                Some(subject.as_ref().into()),
                Some(predicate.as_ref()),
                None,
            )
            .next()
        {
            Some(quad) => Ok(match quad.map_err(store_err)?.object {
                Term::Literal(l) => Some(l.value().to_string()),
                _ => None,
            }),
            None => Ok(None),
        }
    };
    let mut heads = Vec::with_capacity(iris.len());
    for iri in iris {
        let subject = NamedNode::new(&iri).map_err(store_err)?;
        // A subject under the prefix with no content hash is not a pass this
        // crate wrote; it cannot be anyone's current reading.
        let Some(hash) = literal(&subject, "contentHash")? else {
            continue;
        };
        heads.push(PassHead {
            derived_at: literal(&subject, "derivedAt")?,
            iri,
            hash,
        });
    }
    Ok(heads)
}

/// The reading of `(repo, rel)` that stands, given the file's current content
/// hash (`None` when the content could not be read) — or `None` when no pass
/// over the file is on record, in which case nothing is superseded.
pub(crate) fn standing(
    archive: &Archive,
    repo: &str,
    rel: &str,
    current_hash: Option<&str>,
) -> Result<Option<Standing>> {
    let heads = passes_on(archive, repo, rel)?;
    // Newest first: derivation time, then IRI — a total order, so the choice
    // (and `superseded_by`) is deterministic. `None` sorts oldest.
    let newest_first = |a: &&PassHead, b: &&PassHead| {
        b.derived_at
            .cmp(&a.derived_at)
            .then_with(|| b.iri.cmp(&a.iri))
    };
    let Some(newest) = heads.iter().min_by(newest_first) else {
        return Ok(None);
    };
    let hash = match current_hash {
        Some(current) if heads.iter().any(|pass| pass.hash == current) => current,
        _ => newest.hash.as_str(),
    };
    let mut current: Vec<&PassHead> = heads.iter().filter(|pass| pass.hash == hash).collect();
    current.sort_by(newest_first);
    let mut members = BTreeSet::new();
    for pass in &current {
        if let Some(entry) = load_pass(archive, &pass.iri)? {
            members.extend(entry.findings());
        }
    }
    Ok(Some(Standing {
        by: current[0].iri.clone(),
        members,
    }))
}

/// Whether supersession can apply to `finding` at all: an UNDECIDED finding
/// on a FILE. Decisions are records; a PR finding has no file pass.
fn eligible(finding: &Annotation) -> bool {
    finding.family == Family::Finding
        && finding.decision.is_none()
        && !finding.rel.is_empty()
        && matches!(finding.target_ref(), TargetRef::File(_))
}

/// Set [`Annotation::superseded_by`] on every eligible finding its file's
/// current reading does not stand behind. `current_hash` answers a finding's
/// file's current content hash; it is asked once per file.
///
/// The standing set is computed once per `(repo, path)`, and only for files
/// that carry an eligible finding — a listing whose findings are all decided
/// reads no pass at all.
pub(crate) fn mark(
    archive: &Archive,
    findings: &mut [Annotation],
    current_hash: impl Fn(&Annotation) -> Option<String>,
) -> Result<()> {
    let mut readings: BTreeMap<(String, String), Option<Standing>> = BTreeMap::new();
    for finding in findings.iter_mut() {
        if !eligible(finding) {
            continue;
        }
        let key = (finding.repo.clone(), finding.rel.clone());
        if !readings.contains_key(&key) {
            let hash = current_hash(finding);
            let reading = standing(archive, &finding.repo, &finding.rel, hash.as_deref())?;
            readings.insert(key.clone(), reading);
        }
        if let Some(reading) = &readings[&key] {
            if !reading.members.contains(&finding.iri()) {
                finding.superseded_by = Some(reading.by.clone());
            }
        }
    }
    Ok(())
}

/// How many undecided findings on `(repo, rel)` are superseded when the
/// file's current content hash is `hash` — the count a review pass's
/// statement carries. Reads the file's findings once and marks them.
pub(crate) fn superseded_on(archive: &Archive, repo: &str, rel: &str, hash: &str) -> Result<usize> {
    let mut findings = crate::annotate::list_findings(archive, repo, Some(rel))?;
    mark(archive, &mut findings, |_| Some(hash.to_string()))?;
    Ok(findings
        .iter()
        .filter(|finding| finding.superseded_by.is_some())
        .count())
}

#[cfg(test)]
mod tests {
    //! Planted passes and findings, no model: every pass here is written with
    //! the same `store_pass` / `store_region` / `mint_pending_finding` a real
    //! pass uses, so the store shape is the real one and only the model's
    //! answer is skipped.

    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use futures::executor::block_on;
    use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
    use oxigraph::model::GraphName;
    use oxigraph::store::Store;

    use crate::annotate::{self, content_hash, Mint, Surface, CAP_ANNOTATE};
    use crate::review::{pass_iri, region_iri, store_pass, store_region, PassEntry, RegionEntry};

    /// The file at three moments. B changes only the second line; C is an
    /// edit nobody has reviewed; D brings `beta` back on a new line.
    const A: &str = "fn alpha() {}\nfn beta() {}\nfn gamma() {}\nfn delta() {}\nfn eps() {}\n";
    const B: &str = "fn alpha() {}\nfn beta2() {}\nfn gamma() {}\nfn delta() {}\nfn eps() {}\n";
    const C: &str = "fn alpha() {}\nfn beta3() {}\nfn gamma() {}\nfn delta() {}\nfn eps() {}\n";
    const TAG: &str = "review-v5@r1";

    fn temp_root(content: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-supersede-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), content).unwrap();
        dir
    }

    /// The findings face over the store, with no LLM mounted at all.
    fn kernel(root: &std::path::Path, store: &Arc<Store>) -> Kernel {
        Kernel::new(Arc::new(crate::space_with_annotations(
            vec![("demo".to_string(), root.to_path_buf())],
            Arc::clone(store),
        )))
    }

    fn cap() -> Capability {
        Capability::scoped(["urn:cap:browse:read:demo", CAP_ANNOTATE])
    }

    fn issue(k: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> String {
        let mut request = Request::new(verb, Iri::parse(iri.to_string()).unwrap());
        for (name, value) in args {
            request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
        }
        let repr = block_on(k.issue(request, &cap())).unwrap();
        String::from_utf8_lossy(&repr.bytes).to_string()
    }

    fn rows(k: &Kernel, state: &str) -> Vec<serde_json::Value> {
        let body = issue(
            k,
            Verb::Source,
            "urn:repo:demo:findings:a.rs",
            &[("state", state), ("as", "application/json")],
        );
        serde_json::from_str::<serde_json::Value>(&body)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    }

    /// `state=` on the file, as the set of quotes it lists.
    fn quotes(k: &Kernel, state: &str) -> BTreeSet<String> {
        rows(k, state)
            .iter()
            .map(|r| r["exact"].as_str().unwrap().to_string())
            .collect()
    }

    fn set(words: &[&str]) -> BTreeSet<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn archive(store: &Arc<Store>) -> Archive {
        Archive::new(Arc::clone(store), GraphName::DefaultGraph)
    }

    /// Mint one pending finding on `a.rs`, quoting `exact` in `text`, as pass
    /// `pass` would — the real mint path, declined-twin check included.
    fn mint(archive: &Archive, text: &str, pass: &str, exact: &str) -> String {
        match mint_noted(archive, text, pass, exact, "a note") {
            Mint::Minted(iri) => iri,
            other => panic!("{exact} did not mint: {other:?}"),
        }
    }

    fn mint_noted(archive: &Archive, text: &str, pass: &str, exact: &str, note: &str) -> Mint {
        annotate::mint_pending_finding(
            archive,
            "urn:repo:demo:file:a.rs",
            "demo",
            "a.rs",
            text,
            &content_hash(text.as_bytes()),
            exact,
            note,
            Some("major"),
            "r1",
            pass,
            Some("2026-09-24T00:00:00.000Z".to_string()),
            Surface::File,
        )
        .unwrap()
    }

    /// Archive a pass over `text` under `tag`, minting `minted` and carrying
    /// the members of `reused` region memos.
    fn plant_pass(
        archive: &Archive,
        text: &str,
        tag: &str,
        minted: Vec<String>,
        reused: Vec<String>,
        derived_at: &str,
    ) -> String {
        let hash = content_hash(text.as_bytes());
        let iri = pass_iri("demo", "a.rs", &hash, tag);
        store_pass(
            archive,
            &PassEntry {
                iri: iri.clone(),
                repo: "demo".to_string(),
                rel: "a.rs".to_string(),
                target_iri: "urn:repo:demo:file:a.rs".to_string(),
                hash,
                tag: tag.to_string(),
                model: "r1".to_string(),
                minted,
                carried: Vec::new(),
                reused_regions: reused,
                derived_regions: Vec::new(),
                orphaned_items: 0,
                suppressed_items: 0,
                reviewed_bytes: Some(text.len() as u64),
                total_bytes: Some(text.len() as u64),
                derived_at: Some(derived_at.to_string()),
                superseded: 0,
            },
        )
        .unwrap();
        iri
    }

    /// A region memo over `bytes` that `pass` derived, standing behind
    /// `members`.
    fn plant_region(archive: &Archive, bytes: &str, pass: &str, members: Vec<String>) -> String {
        let hash = content_hash(bytes.as_bytes());
        let entry = RegionEntry {
            iri: region_iri("demo", "a.rs", &hash, TAG),
            tag: TAG.to_string(),
            hash,
            len: bytes.len() as u64,
            findings: members,
            generated_by: Some(pass.to_string()),
        };
        store_region(archive, &entry, "demo", "a.rs", "r1").unwrap();
        entry.iri
    }

    /// The brief's planted history: P1 over A mints {alpha, beta, delta, eps};
    /// its region memo over line 1 stands behind alpha. P2 over B carries that
    /// memo and mints gamma. Returns (P1, P2, alpha, beta, gamma, delta, eps).
    struct History {
        p1: String,
        p2: String,
        alpha: String,
        beta: String,
        gamma: String,
        delta: String,
        eps: String,
    }

    fn history(archive: &Archive) -> History {
        let p1_iri = pass_iri("demo", "a.rs", &content_hash(A.as_bytes()), TAG);
        let alpha = mint(archive, A, &p1_iri, "fn alpha() {}");
        let beta = mint(archive, A, &p1_iri, "fn beta() {}");
        let delta = mint(archive, A, &p1_iri, "fn delta() {}");
        let eps = mint(archive, A, &p1_iri, "fn eps() {}");
        let p1 = plant_pass(
            archive,
            A,
            TAG,
            vec![alpha.clone(), beta.clone(), delta.clone(), eps.clone()],
            Vec::new(),
            "2026-09-01T00:00:00.000Z",
        );
        assert_eq!(p1, p1_iri);
        let memo = plant_region(archive, "fn alpha() {}\n", &p1, vec![alpha.clone()]);
        let p2_iri = pass_iri("demo", "a.rs", &content_hash(B.as_bytes()), TAG);
        let gamma = mint(archive, B, &p2_iri, "fn gamma() {}");
        let p2 = plant_pass(
            archive,
            B,
            TAG,
            vec![gamma.clone()],
            vec![memo],
            "2026-09-02T00:00:00.000Z",
        );
        History {
            p1,
            p2,
            alpha,
            beta,
            gamma,
            delta,
            eps,
        }
    }

    /// ★★ **The headline.** P1 minted {a, b}; P2 carried a through a memo and
    /// minted c. Pending is {a, c} — what the current review stands behind —
    /// and b is superseded, naming P2 as the pass that does not carry it.
    #[test]
    fn a_pass_supersedes_what_it_neither_minted_nor_carried() {
        let root = temp_root(B);
        let store = Arc::new(Store::new().unwrap());
        let h = history(&archive(&store));
        let k = kernel(&root, &store);

        assert_eq!(
            quotes(&k, "pending"),
            set(&["fn alpha() {}", "fn gamma() {}"]),
            "carried + minted"
        );
        let superseded = rows(&k, "superseded");
        let by_quote: BTreeMap<&str, &serde_json::Value> = superseded
            .iter()
            .map(|r| (r["exact"].as_str().unwrap(), r))
            .collect();
        assert_eq!(
            by_quote.keys().copied().collect::<Vec<_>>(),
            ["fn beta() {}", "fn delta() {}", "fn eps() {}"]
        );
        let beta = by_quote["fn beta() {}"];
        assert_eq!(beta["iri"], h.beta.as_str());
        assert_eq!(beta["state"], "superseded");
        assert_eq!(beta["superseded_by"], h.p2.as_str());
        // A pending row says it is not superseded, in the same key.
        for row in rows(&k, "pending") {
            assert!(row["superseded_by"].is_null(), "{row}");
        }
        // The single finding's own face agrees with its file's listing.
        let one: serde_json::Value = serde_json::from_str(&issue(
            &k,
            Verb::Source,
            &h.beta,
            &[("as", "application/json")],
        ))
        .unwrap();
        assert_eq!(one["state"], "superseded");
        assert_eq!(one["superseded_by"], h.p2.as_str());
        let _ = (&h.alpha, &h.gamma, &h.delta, &h.eps, &h.p1);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A DECISION is a record: a published and a declined finding from P1
    /// stay published and declined after P2 lands, and a superseded finding
    /// can still be decided — it is undecided, not closed.
    #[test]
    fn decided_findings_are_untouched_and_superseded_ones_stay_decidable() {
        let root = temp_root(A);
        let store = Arc::new(Store::new().unwrap());
        let h = history(&archive(&store));
        let k = kernel(&root, &store);
        // Decide while P1 is current (the file is A)…
        issue(&k, Verb::Sink, &h.delta, &[("decision", "publish")]);
        issue(&k, Verb::Sink, &h.eps, &[("decision", "decline")]);
        // …then the file moves to B, where P2 is current.
        std::fs::write(root.join("a.rs"), B).unwrap();
        assert_eq!(quotes(&k, "published"), set(&["fn delta() {}"]));
        assert_eq!(quotes(&k, "declined"), set(&["fn eps() {}"]));
        assert_eq!(quotes(&k, "superseded"), set(&["fn beta() {}"]));
        for row in rows(&k, "all") {
            if row["state"] == "published" || row["state"] == "declined" {
                assert!(row["superseded_by"].is_null(), "{row}");
            }
        }
        // Superseded is decidable: declining it records a decision like any
        // other, and it leaves the superseded list for the declined one.
        issue(
            &k,
            Verb::Sink,
            &h.beta,
            &[("decision", "decline"), ("reason", "no-issue")],
        );
        assert!(quotes(&k, "superseded").is_empty());
        assert_eq!(
            quotes(&k, "declined"),
            set(&["fn beta() {}", "fn eps() {}"])
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ **The revert, rule (a).** A → B → A: the file is A again, so P1 —
    /// the pass over A's bytes — is current again with no new derivation, and
    /// P1's findings are pending again; P2's gamma is the superseded one now.
    /// A stored transition would have left b superseded forever.
    #[test]
    fn a_revert_makes_the_older_pass_current_again() {
        let root = temp_root(B);
        let store = Arc::new(Store::new().unwrap());
        let h = history(&archive(&store));
        let k = kernel(&root, &store);
        assert!(quotes(&k, "superseded").contains("fn beta() {}"));

        std::fs::write(root.join("a.rs"), A).unwrap();
        assert_eq!(
            quotes(&k, "pending"),
            set(&[
                "fn alpha() {}",
                "fn beta() {}",
                "fn delta() {}",
                "fn eps() {}"
            ])
        );
        let superseded = rows(&k, "superseded");
        assert_eq!(superseded.len(), 1);
        assert_eq!(superseded[0]["iri"], h.gamma.as_str());
        assert_eq!(superseded[0]["superseded_by"], h.p1.as_str());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The file page's `proposals=` overlay draws PENDING findings, and a
    /// superseded one is not pending: it is about bytes that changed, so it
    /// is not drawn beside the current text.
    #[test]
    fn the_file_page_draws_no_superseded_proposal() {
        let root = temp_root(B);
        let store = Arc::new(Store::new().unwrap());
        let h = history(&archive(&store));
        let k = kernel(&root, &store);
        let html = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:file:a.rs",
            &[("as", "text/html"), ("proposals", "major")],
        );
        assert!(html.contains("proposals (2)"), "{html}");
        for drawn in [&h.alpha, &h.gamma] {
            let id = drawn.rsplit(':').next().unwrap();
            assert!(html.contains(&format!("id=\"proposal-{id}\"")), "{html}");
        }
        let beta = h.beta.rsplit(':').next().unwrap();
        assert!(!html.contains(&format!("proposal-{beta}")), "{html}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// An edit nobody has reviewed yet: no pass over C exists, so the most
    /// recently derived reading stands (P2) until one lands.
    #[test]
    fn an_unreviewed_edit_reads_the_newest_pass() {
        let root = temp_root(C);
        let store = Arc::new(Store::new().unwrap());
        let h = history(&archive(&store));
        let k = kernel(&root, &store);
        assert_eq!(
            quotes(&k, "pending"),
            set(&["fn alpha() {}", "fn gamma() {}"])
        );
        assert!(rows(&k, "superseded")
            .iter()
            .all(|r| r["superseded_by"] == h.p2.as_str()));
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ Two passes on the CURRENT bytes — a second backend via `provider=`,
    /// or a re-keyed prompt — are both the current reading: the second is a
    /// second reviewer's margin, not a replacement, so neither retires the
    /// other's findings. `superseded_by` names the newer of the two.
    #[test]
    fn every_pass_on_the_current_bytes_stands() {
        let root = temp_root(B);
        let store = Arc::new(Store::new().unwrap());
        let archive = archive(&store);
        let h = history(&archive);
        let other = "review-v5@r2";
        let p3_iri = pass_iri("demo", "a.rs", &content_hash(B.as_bytes()), other);
        let delta2 = mint(&archive, B, &p3_iri, "fn delta() {}");
        let p3 = plant_pass(
            &archive,
            B,
            other,
            vec![delta2],
            Vec::new(),
            "2026-09-03T00:00:00.000Z",
        );
        let k = kernel(&root, &store);
        let pending = rows(&k, "pending");
        let pending: BTreeSet<&str> = pending.iter().map(|r| r["iri"].as_str().unwrap()).collect();
        assert!(pending.contains(h.gamma.as_str()), "P2's mint stands");
        assert!(pending.contains(h.alpha.as_str()), "P2's carry stands");
        assert_eq!(pending.len(), 3, "and P3's own mint: {pending:?}");
        assert!(rows(&k, "superseded")
            .iter()
            .all(|r| r["superseded_by"] == p3.as_str()));
        std::fs::remove_dir_all(&root).ok();
    }

    /// `state=all` lists every state, superseded included; `summary=states`
    /// counts them, per root and per file; the contract declares the state.
    #[test]
    fn the_state_is_in_the_contract_and_counted() {
        let root = temp_root(B);
        let store = Arc::new(Store::new().unwrap());
        let h = history(&archive(&store));
        let k = kernel(&root, &store);
        issue(&k, Verb::Sink, &h.eps, &[("decision", "decline")]);

        let all = rows(&k, "all");
        let states: BTreeMap<String, usize> = all.iter().fold(BTreeMap::new(), |mut m, r| {
            *m.entry(r["state"].as_str().unwrap().to_string())
                .or_default() += 1;
            m
        });
        assert_eq!(states["pending"], 2);
        assert_eq!(states["superseded"], 2);
        assert_eq!(states["declined"], 1);

        // Counts for the whole repo, whatever state= shows.
        let summary: serde_json::Value = serde_json::from_str(&issue(
            &k,
            Verb::Source,
            "urn:repo:demo:findings",
            &[("state", "published"), ("summary", "states")],
        ))
        .unwrap();
        assert_eq!(summary["rows"].as_array().unwrap().len(), 0);
        let counts = &summary["states"];
        assert_eq!(counts["pending"], 2);
        assert_eq!(counts["superseded"], 2);
        assert_eq!(counts["published"], 0);
        assert_eq!(counts["declined"], 1);
        assert_eq!(counts["files"][0]["path"], "a.rs");
        assert_eq!(counts["files"][0]["superseded"], 2);
        assert_eq!(counts["files"][0]["superseded_by"], h.p2.as_str());

        // A pending listing says what it is not showing.
        let plain = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:findings:a.rs",
            &[("as", "text/plain")],
        );
        assert!(
            plain.contains("2 superseded findings not listed"),
            "{plain}"
        );
        let html = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:findings:a.rs",
            &[("as", "text/html")],
        );
        assert!(html.contains("browse-findings-superseded"), "{html}");
        assert!(html.contains("state=superseded"), "the nav offers the tab");

        // The contract gonk builds its state nav from.
        let description = crate::finding::findings_description();
        let state = description
            .inputs
            .iter()
            .find(|i| i.name == "state")
            .expect("state is declared");
        assert_eq!(
            state.one_of,
            ["pending", "superseded", "published", "declined", "all"]
        );
        let summary = description
            .inputs
            .iter()
            .find(|i| i.name == "summary")
            .expect("summary is declared");
        assert_eq!(summary.one_of, ["declined", "states"]);
        std::fs::remove_dir_all(&root).ok();
    }

    /// ⚠ **Superseded is not declined.** A superseded twin carries no human
    /// judgment, so a fresh like claim on its line arrives UNMARKED — while a
    /// declined twin on another line still marks its like claim (the control:
    /// the mark is live, and superseded simply is not an input to it).
    #[test]
    fn a_superseded_twin_leaves_no_prior_decision() {
        let root = temp_root(B);
        let store = Arc::new(Store::new().unwrap());
        let archive = archive(&store);
        let h = history(&archive);
        let k = kernel(&root, &store);
        issue(&k, Verb::Sink, &h.eps, &[("decision", "decline")]);
        assert!(quotes(&k, "superseded").contains("fn beta() {}"));

        // A later pass over new bytes raises `beta` and `eps` again, each with
        // the SAME note and severity as the first time.
        let d =
            "// moved\nfn alpha() {}\nfn beta() {}\nfn gamma() {}\nfn delta() {}\nfn eps() {}\n";
        let p4 = pass_iri("demo", "a.rs", &content_hash(d.as_bytes()), TAG);
        let load = |iri: &str| {
            annotate::load_record(&archive, Family::Finding, iri.rsplit(':').next().unwrap())
                .unwrap()
                .unwrap()
        };
        // The superseded twin neither withholds the exact repeat nor marks it:
        // it is minted, unmarked, as if no earlier claim existed.
        let beta_again = load(&mint(&archive, d, &p4, "fn beta() {}"));
        assert!(
            beta_again.prior.is_none(),
            "a superseded twin is not a prior decision"
        );
        // The controls, on the declined twin: the exact repeat is withheld…
        assert_eq!(
            mint_noted(&archive, d, &p4, "fn eps() {}", "a note"),
            Mint::Withheld(h.eps.clone()),
            "the control: a declined twin withholds its exact repeat"
        );
        // …and a like claim with a different note arrives marked.
        let Mint::Minted(eps_again) = mint_noted(&archive, d, &p4, "fn eps() {}", "another note")
        else {
            panic!("a like claim mints");
        };
        assert_eq!(
            load(&eps_again).prior.as_ref().map(|p| p.finding.as_str()),
            Some(h.eps.as_str()),
            "the control: a declined twin still marks"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A PR-page finding is keyed to the PR's diff, not to a file's pass, so
    /// it is never superseded — even in a repo whose files' passes supersede.
    #[test]
    fn pr_family_findings_are_never_superseded() {
        let root = temp_root(B);
        let store = Arc::new(Store::new().unwrap());
        let archive = archive(&store);
        history(&archive);
        let diff = "+fn beta() {}\n";
        let pr_pass = pass_iri("demo", "pr:3", "deadbeef", TAG);
        let Mint::Minted(pr_finding) = annotate::mint_pending_finding(
            &archive,
            "urn:repo:demo:pr:3",
            "demo",
            "",
            diff,
            &content_hash(diff.as_bytes()),
            "+fn beta() {}",
            "a PR note",
            Some("minor"),
            "r1",
            &pr_pass,
            None,
            Surface::Diff,
        )
        .unwrap() else {
            panic!("the PR finding did not mint");
        };
        let mut findings = annotate::list_findings(&archive, "demo", None).unwrap();
        mark(&archive, &mut findings, |_| {
            Some(content_hash(B.as_bytes()))
        })
        .unwrap();
        let pr = findings
            .iter()
            .find(|f| f.iri() == pr_finding)
            .expect("listed");
        assert_eq!(pr.state(), Some("pending"));
        assert!(pr.superseded_by.is_none());
        assert!(
            findings.iter().any(|f| f.state() == Some("superseded")),
            "the file findings beside it are superseded"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A file with findings but NO pass on record is left alone: there is
    /// no reading to stand behind anything or not.
    #[test]
    fn a_file_with_no_pass_on_record_supersedes_nothing() {
        let store = Arc::new(Store::new().unwrap());
        let archive = archive(&store);
        let orphan_pass = pass_iri("demo", "a.rs", "sha256:gone", TAG);
        mint(&archive, A, &orphan_pass, "fn alpha() {}");
        let mut findings = annotate::list_findings(&archive, "demo", Some("a.rs")).unwrap();
        mark(&archive, &mut findings, |_| {
            Some(content_hash(B.as_bytes()))
        })
        .unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].state(), Some("pending"));
    }
}
