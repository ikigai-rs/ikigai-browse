//! `urn:repo:{repo}:findings[:{path}] group=<kind>` — **finding groups**: the
//! machine PROPOSES a set of pending findings, a human decides it once
//! (ledger #506).
//!
//! ## Why a proposal and never a decision
//!
//! The bounded queue (0.11.0) still held ~1100 undecided findings on
//! 2026-09-24, and a human was deciding them one row at a time. An
//! auto-decline was the obvious lever and the wrong one: a decline records a
//! HUMAN judgment, and 0.9.0's recurrence mark treats every decline as a prior
//! "no" — so an auto-decline would fill the mark with verdicts nobody made.
//! The lever instead is a GROUP: a query over the pending set that names a
//! shape, suggests a reason word, and hands the members to a person.
//!
//! ★ **Nothing here decides anything, and there is no batch Sink.** A host
//! (gonk's queue page) renders a group, lets a person untick members and pick
//! the word, and fans the batch out to the EXISTING finding Sink
//! (`urn:iki:finding:{id}`), one call per member — so every member gets an
//! ordinary decision node, a batch decline reads back exactly like a single
//! one, and the recurrence mark treats it as what it is: the human's decision,
//! made once for many.
//!
//! ## The kinds — [`GROUP_KINDS`], in the contract's order
//!
//! Each is a pure function of the PENDING rows (superseded, published and
//! declined never appear as members) plus, for `recurrence`, the declined
//! findings already loaded for the listing. Within one kind a finding is in at
//! most one group; across kinds the groups overlap, because each kind is a
//! different lens on the same queue.
//!
//! * `recurrence` — pending findings quoting a line where a like claim on the
//!   same target was already DECLINED: 0.9.0's mark computed backwards, over
//!   the findings minted before the mark existed (and over any decline made
//!   after a finding was minted). One group per declined twin — the most
//!   recently decided, the same rule the mint applies. Suggested reason: the
//!   twin's word, when it has one.
//! * `near-duplicate` — two or more pending findings on the same target, the
//!   same current line and the same proposed severity, reworded. The OLDEST is
//!   proposed to keep (`kept`) and is not a member; the rest are. Orphaned rows
//!   never group here — their line is a projection, not an anchor. Suggested
//!   reason: `duplicate`.
//! * `comment-shape` — one target's pending findings whose quote is a comment
//!   or doc line ([`is_comment_quote`]): the `restates` shape (ledger #483).
//!   ⚠ Sampled 2026-09-24 as MIXED — restates, no-issue, misread, and at least
//!   one plausibly real claim about an unenforced documented constraint — so
//!   it is a proposal to READ, which is why its suggestion is only a
//!   suggestion. Suggested reason: `restates`.
//! * `file` — every pending finding on one target. No suggestion: "nothing on
//!   this file is worth a decision" has no single reason.
//!
//! ★ Every suggested word is read from [`DECLINE_REASONS`] — the Sink's own
//! `one_of` — through [`contract_reason`], never spelled a second time.

use std::collections::BTreeMap;

use ikigai_core::{Error, Result};

use crate::annotate::{self, Annotation, TargetRef};
use crate::finding::{plain_row, DECLINE_REASONS, NOT_A_GATE};
use crate::{crumbs_html, esc};

/// **The group kinds**, in the order a picker shows them — the `one_of` on the
/// findings face's `group` argument, and the order of the `kinds` counts.
/// gonk builds its batch view's kind menu from the contract, never from a list
/// of its own.
pub(crate) const GROUP_KINDS: [&str; 4] = ["recurrence", "near-duplicate", "comment-shape", "file"];

/// What each of [`GROUP_KINDS`] proposes, in the same order — the `group`
/// ArgSpec's summary is built from this.
pub(crate) const GROUP_KIND_MEANINGS: [&str; 4] = [
    "pending findings quoting a line where a like claim on the same file was already declined, \
     one group per declined twin (the twin rides along; its reason word is the suggestion)",
    "pending findings on the same file, line and proposed severity, reworded — the oldest is \
     proposed to KEEP (kept) and is not a member; suggestion: duplicate",
    "one file's pending findings whose quote is a comment or doc line (the restates shape) — \
     a MIXED set to read, not to rubber-stamp; suggestion: restates",
    "every pending finding on one file; no suggestion",
];

/// The suggestion each kind carries, as a word of [`DECLINE_REASONS`] — or
/// `None` where it has no fixed one (`recurrence` takes its twin's word).
fn fixed_suggestion(kind: &str) -> Option<&'static str> {
    match kind {
        "near-duplicate" => Some(contract_reason("duplicate")),
        "comment-shape" => Some(contract_reason("restates")),
        _ => None,
    }
}

/// `word` as the CONTRACT spells it — the `&'static str` inside
/// [`DECLINE_REASONS`], so what a group emits is the constant's bytes, not a
/// second spelling. Panics if the word has left the set, which every test that
/// builds a group would hit first.
fn contract_reason(word: &str) -> &'static str {
    DECLINE_REASONS
        .iter()
        .find(|w| **w == word)
        .copied()
        .unwrap_or_else(|| panic!("`{word}` is not in DECLINE_REASONS"))
}

/// Validate a `group=` value against [`GROUP_KINDS`], naming the set.
pub(crate) fn kind_arg(word: &str) -> Result<&'static str> {
    GROUP_KINDS
        .iter()
        .find(|k| **k == word)
        .copied()
        .ok_or_else(|| Error::InvalidArgument {
            name: "group".to_string(),
            detail: format!(
                "`{word}` is not a group kind — one of: {}",
                GROUP_KINDS.join(", ")
            ),
        })
}

/// The `group` ArgSpec summary: the rule, then every kind with its meaning.
pub(crate) fn group_summary() -> String {
    let kinds: Vec<String> = GROUP_KINDS
        .iter()
        .zip(GROUP_KIND_MEANINGS)
        .map(|(kind, meaning)| format!("{kind}: {meaning}"))
        .collect();
    format!(
        "propose groups of PENDING findings for one human decision each — nothing is decided \
         here; a host fans a batch out to the finding Sink, one call per member. The json \
         face becomes {{repo, path, state, group, kinds: [{{kind, groups, findings}}], \
         groups: [{{kind, key, label, repo, path, annotates, reason, twin, kept, members}}]}}. \
         Only with state=pending (the default) and no summary=. {}.",
        kinds.join("; ")
    )
}

/// One reconciled listing row: the finding and the line it renders at.
type Row = (Annotation, Option<u64>);

/// One proposed group. Rows borrow from the listing; nothing is cloned until
/// a face renders.
pub(crate) struct Group<'a> {
    pub(crate) kind: &'static str,
    /// Stable across reads while the group's defining record stands: the
    /// twin's id, the kept finding's id, or the target's path.
    pub(crate) key: String,
    pub(crate) label: String,
    /// The target all members share.
    pub(crate) target: &'a Annotation,
    /// A word of [`DECLINE_REASONS`], or `None`. A SUGGESTION: never applied.
    pub(crate) reason: Option<&'static str>,
    /// `recurrence`: the declined twin the members re-raise.
    pub(crate) twin: Option<&'a Annotation>,
    /// `near-duplicate`: the oldest finding, proposed to keep — not a member.
    pub(crate) kept: Option<&'a Row>,
    pub(crate) members: Vec<&'a Row>,
}

/// Every group of `kind` over `pending` (the reconciled rows of the pending
/// listing, in triage order) and `declined` (the declined findings the listing
/// loaded). Larger groups first — the biggest lever leads — then place, then
/// key, so the order is total.
///
/// Rows not in the `pending` state are dropped here too, so a caller cannot
/// hand a superseded finding into a group by mistake.
pub(crate) fn groups<'a>(
    kind: &str,
    pending: &'a [Row],
    declined: &'a [Annotation],
) -> Vec<Group<'a>> {
    let pending: Vec<&'a Row> = pending
        .iter()
        .filter(|(f, _)| f.state() == Some("pending"))
        .collect();
    let mut out = match kind {
        "recurrence" => recurrence(&pending, declined),
        "near-duplicate" => near_duplicates(&pending),
        "comment-shape" => by_target("comment-shape", &pending, is_comment_quote),
        "file" => by_target("file", &pending, |_| true),
        _ => Vec::new(),
    };
    out.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then_with(|| a.target.place().cmp(&b.target.place()))
            .then_with(|| a.key.cmp(&b.key))
    });
    out
}

/// `(groups, member findings)` for every kind, in [`GROUP_KINDS`] order — the
/// size of each lever over this listing.
pub(crate) fn counts(
    pending: &[Row],
    declined: &[Annotation],
) -> Vec<(&'static str, usize, usize)> {
    GROUP_KINDS
        .iter()
        .map(|kind| {
            let groups = groups(kind, pending, declined);
            let findings = groups.iter().map(|g| g.members.len()).sum();
            (*kind, groups.len(), findings)
        })
        .collect()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn recurrence<'a>(pending: &[&'a Row], declined: &'a [Annotation]) -> Vec<Group<'a>> {
    // The declined findings by (target, quote), most recently decided first —
    // the order `annotate::declined_twins` gives the mint, so the twin a group
    // names is the one a fresh mint would name.
    let mut twins: BTreeMap<(&str, &str), Vec<&'a Annotation>> = BTreeMap::new();
    for finding in declined.iter().filter(|f| f.state() == Some("declined")) {
        twins
            .entry((finding.target_iri.as_str(), finding.exact.as_str()))
            .or_default()
            .push(finding);
    }
    for list in twins.values_mut() {
        list.sort_by(|a, b| {
            let at = |f: &Annotation| f.decision.as_ref().and_then(|d| d.at.clone());
            at(b).cmp(&at(a)).then_with(|| a.id.cmp(&b.id))
        });
    }
    let mut by_twin: BTreeMap<&str, (&'a Annotation, Vec<&'a Row>)> = BTreeMap::new();
    for row in pending {
        let finding = &row.0;
        let Some(twin) = twins
            .get(&(finding.target_iri.as_str(), finding.exact.as_str()))
            .and_then(|list| list.first())
        else {
            continue;
        };
        by_twin
            .entry(twin.id.as_str())
            .or_insert_with(|| (twin, Vec::new()))
            .1
            .push(row);
    }
    by_twin
        .into_values()
        .map(|(twin, members)| {
            let decision = twin.decision.as_ref();
            // The twin's word, re-read through the contract: a loader already
            // maps an unknown term to None, and this keeps the emitted bytes
            // the constant's.
            let reason = decision
                .and_then(|d| d.reason.as_deref())
                .and_then(|word| DECLINE_REASONS.iter().find(|w| **w == word).copied());
            let mut why = Vec::new();
            if let Some(word) = reason {
                why.push(word.to_string());
            }
            if let Some(at) = decision.and_then(|d| d.at.as_deref()) {
                why.push(at.get(..10).unwrap_or(at).to_string());
            }
            let n = members.len();
            let label = format!(
                "{} on {} {} a line where a like claim was already declined{}",
                plural(n, "finding", "findings"),
                twin.place(),
                if n == 1 { "quotes" } else { "quote" },
                match why.is_empty() {
                    true => String::new(),
                    false => format!(" ({})", why.join(", ")),
                },
            );
            Group {
                kind: "recurrence",
                key: format!("recurrence:{}", twin.id),
                label,
                target: twin,
                reason,
                twin: Some(twin),
                kept: None,
                members,
            }
        })
        .collect()
}

fn near_duplicates<'a>(pending: &[&'a Row]) -> Vec<Group<'a>> {
    let mut lines: BTreeMap<(&str, u64, Option<&str>), Vec<&'a Row>> = BTreeMap::new();
    for row in pending {
        let (finding, line) = (&row.0, row.1);
        // An orphaned row's line is its recorded position projected onto the
        // current text — a guess, and two guesses agreeing is not two claims
        // about one line.
        let Some(line) = line.filter(|_| !finding.orphaned) else {
            continue;
        };
        lines
            .entry((
                finding.target_iri.as_str(),
                line,
                finding.effective_severity(),
            ))
            .or_default()
            .push(row);
    }
    lines
        .into_iter()
        .filter(|(_, rows)| rows.len() > 1)
        .map(|((_, line, severity), mut rows)| {
            // Oldest first: mint time, then id. A finding with no recorded
            // time sorts oldest — it predates the stamp.
            rows.sort_by(|(a, _), (b, _)| (&a.created, &a.id).cmp(&(&b.created, &b.id)));
            let kept = rows.remove(0);
            let n = rows.len();
            let label = format!(
                "{} on {} L{line} ({}) {} an older one on the same line",
                plural(n, "finding", "findings"),
                kept.0.place(),
                severity.unwrap_or("unrated"),
                if n == 1 { "re-raises" } else { "re-raise" },
            );
            Group {
                kind: "near-duplicate",
                key: format!("near-duplicate:{}", kept.0.id),
                label,
                target: &kept.0,
                reason: fixed_suggestion("near-duplicate"),
                twin: None,
                kept: Some(kept),
                members: rows,
            }
        })
        .collect()
}

fn by_target<'a>(
    kind: &'static str,
    pending: &[&'a Row],
    keep: impl Fn(&Annotation) -> bool,
) -> Vec<Group<'a>> {
    let mut targets: BTreeMap<&str, Vec<&'a Row>> = BTreeMap::new();
    for row in pending.iter().filter(|(f, _)| keep(f)) {
        targets
            .entry(row.0.target_iri.as_str())
            .or_default()
            .push(row);
    }
    targets
        .into_values()
        .map(|members| {
            let target = &members[0].0;
            let n = members.len();
            let label = match kind {
                "comment-shape" => format!(
                    "{} on {} whose quote is a comment or doc line",
                    plural(n, "finding", "findings"),
                    target.place()
                ),
                _ => format!(
                    "{} on {}",
                    plural(n, "pending finding", "pending findings"),
                    target.place()
                ),
            };
            Group {
                kind,
                key: format!("{kind}:{}", target.place()),
                label,
                target,
                reason: fixed_suggestion(kind),
                twin: None,
                kept: None,
                members,
            }
        })
        .collect()
}

// --- the comment shape ----------------------------------------------------------

/// Files whose every line is documentation — a quote from one is a doc line
/// by where it lives, whatever its first character.
const DOC_EXTENSIONS: [&str; 7] = ["md", "markdown", "org", "txt", "rst", "adoc", "asciidoc"];

/// Whether a finding's quote is a comment or doc line: every non-blank line of
/// the quote opens with a comment marker ([`is_comment_line`]), or the file is
/// documentation ([`DOC_EXTENSIONS`]). A PR-page quote is a diff line, so its
/// one leading marker (`+`, `-`, space) is stripped first.
///
/// ⚠ A HEURISTIC, and a lexical one: it does not parse, so a line inside a
/// block comment that does not open with `*` is missed, and a C preprocessor
/// line is not a comment (`#include` is refused by the `#` rule). A trailing
/// comment (`x = 1; // why`) is CODE — the claim may be about the code.
pub(crate) fn is_comment_quote(finding: &Annotation) -> bool {
    let is_pr = matches!(finding.target_ref(), TargetRef::Pr(_));
    if !is_pr && is_doc_file(&finding.rel) {
        return !finding.exact.trim().is_empty();
    }
    let mut lines = finding
        .exact
        .lines()
        .map(|line| match is_pr {
            true => line.strip_prefix(['+', '-', ' ']).unwrap_or(line),
            false => line,
        })
        .filter(|line| !line.trim().is_empty())
        .peekable();
    lines.peek().is_some() && lines.all(is_comment_line)
}

fn is_doc_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        DOC_EXTENSIONS
            .iter()
            .any(|doc| doc.eq_ignore_ascii_case(ext))
    })
}

/// Whether one line opens with a comment marker, after its indentation:
/// `//` (and `///`, `//!`), `/*`, a block-comment continuation `*` (alone,
/// `* `, `*/`), `#` alone or before whitespace or another `#` (so `#[derive]`,
/// `#![…]` and `#include` are code), `--` alone or before whitespace, `;`
/// alone or before whitespace or `;`, `<!--`, and a `"""` / `'''` docstring
/// opener.
pub(crate) fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    let after = |marker: &str| t.strip_prefix(marker).map(|rest| rest.chars().next());
    let ends_or_space = |next: Option<Option<char>>| {
        matches!(next, Some(None)) || matches!(next, Some(Some(c)) if c.is_whitespace())
    };
    t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with("<!--")
        || t.starts_with("\"\"\"")
        || t.starts_with("'''")
        || ends_or_space(after("*"))
        || t.starts_with("*/")
        || ends_or_space(after("#"))
        || t.starts_with("##")
        || ends_or_space(after("--"))
        || ends_or_space(after(";"))
        || t.starts_with(";;")
}

// --- faces ----------------------------------------------------------------------

/// The standing note every group face carries, beside [`NOT_A_GATE`].
const A_PROPOSAL: &str = "A group is a proposal. Nothing here decides anything: each member is \
                          decided through its own finding, and a suggested reason is only a \
                          suggestion.";

fn row_json(row: &Row) -> serde_json::Value {
    annotate::annotation_json(&row.0, row.1)
}

fn target_path(target: &Annotation) -> Option<&str> {
    (!target.rel.is_empty()).then_some(target.rel.as_str())
}

pub(crate) fn json(
    repo: &str,
    path: Option<&str>,
    kind: &str,
    groups: &[Group<'_>],
    counts: &[(&'static str, usize, usize)],
) -> serde_json::Value {
    serde_json::json!({
        "repo": repo,
        "path": path,
        "state": "pending",
        "group": kind,
        "kinds": counts.iter().map(|(kind, groups, findings)| serde_json::json!({
            "kind": kind,
            "groups": groups,
            "findings": findings,
        })).collect::<Vec<_>>(),
        "groups": groups.iter().map(|g| serde_json::json!({
            "kind": g.kind,
            "key": g.key,
            "label": g.label,
            "repo": g.target.repo,
            "path": target_path(g.target),
            "annotates": g.target.target_iri,
            "reason": g.reason,
            "twin": g.twin.map(|t| annotate::annotation_json(t, None)),
            "kept": g.kept.map(row_json),
            "members": g.members.iter().map(|row| row_json(row)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn counts_words(counts: &[(&'static str, usize, usize)]) -> String {
    counts
        .iter()
        .map(|(kind, groups, findings)| {
            format!(
                "{kind} {} / {}",
                plural(*groups, "group", "groups"),
                plural(*findings, "finding", "findings")
            )
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

pub(crate) fn plain(
    kind: &str,
    groups: &[Group<'_>],
    counts: &[(&'static str, usize, usize)],
) -> String {
    let findings: usize = groups.iter().map(|g| g.members.len()).sum();
    let mut out = format!(
        "--- finding groups: {kind} ({}, {}) ---\n{NOT_A_GATE}\n{A_PROPOSAL}\nby kind: {}",
        plural(groups.len(), "group", "groups"),
        plural(findings, "finding", "findings"),
        counts_words(counts),
    );
    for group in groups {
        out.push_str(&format!("\n[{}] {}", group.kind, group.label));
        if let Some(word) = group.reason {
            out.push_str(&format!(" · suggested reason: {word}"));
        }
        if let Some(twin) = group.twin {
            out.push_str(&format!("\n  twin: {}", plain_row(twin, None)));
        }
        if let Some((kept, line)) = group.kept {
            out.push_str(&format!("\n  keep: {}", plain_row(kept, *line)));
        }
        for (finding, line) in &group.members {
            out.push_str(&format!("\n  {}", plain_row(finding, *line)));
        }
    }
    out
}

pub(crate) fn html(
    repo: &str,
    rel: &str,
    kind: &str,
    groups: &[Group<'_>],
    counts: &[(&'static str, usize, usize)],
) -> String {
    let iri = crate::finding::findings_iri(repo, rel);
    let mut out = String::from("<div class=\"browse\">");
    out.push_str(&crumbs_html(repo, rel));
    out.push_str(&format!(
        "<p class=\"browse-findings-note\">{NOT_A_GATE}</p>\
         <p class=\"browse-findings-groups-note\">{A_PROPOSAL}</p>\
         <nav class=\"browse-findings-group-kinds\">"
    ));
    for (option, groups, findings) in counts {
        let current = match *option == kind {
            true => " browse-findings-group-kind-current",
            false => "",
        };
        out.push_str(&format!(
            "<button class=\"browse-findings-group-kind{current}\" hx-get=\"/k/source {iri} \
             as=text/html group={option}\" hx-target=\"#browse\" hx-swap=\"innerHTML\">\
             {option} <span class=\"browse-findings-group-count\">{groups} / {findings}</span>\
             </button>",
            iri = esc(&iri),
        ));
    }
    out.push_str("</nav>");
    if groups.is_empty() {
        out.push_str(&format!(
            "<p class=\"browse-findings-empty\">No {} groups{}.</p>",
            esc(kind),
            match rel.is_empty() {
                true => String::new(),
                false => format!(" for {}", esc(rel)),
            }
        ));
    }
    for group in groups {
        out.push_str(&format!(
            "<section class=\"browse-findings-group\" data-key=\"{key}\">\
             <h3 class=\"browse-findings-group-label\">{label}</h3>",
            key = esc(&group.key),
            label = esc(&group.label),
        ));
        if let Some(word) = group.reason {
            out.push_str(&format!(
                "<p class=\"browse-findings-group-reason\">suggested reason: \
                 <code>{}</code></p>",
                esc(word)
            ));
        }
        let show_path = rel.is_empty();
        if let Some(twin) = group.twin {
            out.push_str(&format!(
                "<div class=\"browse-findings-group-twin\"><p>the declined twin</p>{}</div>",
                annotate::annotation_card_html(twin, None, show_path)
            ));
        }
        if let Some((kept, line)) = group.kept {
            out.push_str(&format!(
                "<div class=\"browse-findings-group-kept\"><p>proposed to keep (the oldest)</p>\
                 {}</div>",
                annotate::annotation_card_html(kept, *line, show_path)
            ));
        }
        out.push_str("<div class=\"browse-annotations\">");
        for (finding, line) in &group.members {
            out.push_str(&annotate::annotation_card_html(finding, *line, show_path));
        }
        out.push_str("</div></section>");
    }
    out.push_str("</div>");
    out
}

#[cfg(test)]
mod tests {
    //! Planted passes and findings, no model — the `crate::supersede` harness
    //! shape: every record is written through the same `store_pass` /
    //! `mint_pending_finding` a real pass uses, and every read goes through
    //! the kernel, so the store shape and the face are the real ones.

    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use futures::executor::block_on;
    use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
    use oxigraph::model::GraphName;
    use oxigraph::store::Store;

    use crate::annotate::{content_hash, Mint, Surface, CAP_ANNOTATE};
    use crate::archive::Archive;
    use crate::review::{pass_iri, store_pass, PassEntry};

    /// `a.rs` at two moments. A → B changes line 5 only, so a pass over B
    /// that does not carry P1's line-5 finding supersedes it.
    const A: &str =
        "// Frobs the widget.\nfn alpha() {}\n/// The beta.\nfn beta() {}\nfn gamma() {}\n";
    const B: &str =
        "// Frobs the widget.\nfn alpha() {}\n/// The beta.\nfn beta() {}\nfn gamma2() {}\n";
    const NOTES: &str = "# Notes\nThe queue is not a gate.\n";

    fn temp_root() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-group-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), B).unwrap();
        std::fs::write(dir.join("NOTES.md"), NOTES).unwrap();
        dir
    }

    fn kernel(root: &std::path::Path, store: &Arc<Store>) -> Kernel {
        Kernel::new(Arc::new(crate::space_with_annotations(
            vec![("demo".to_string(), root.to_path_buf())],
            Arc::clone(store),
        )))
    }

    fn cap() -> Capability {
        Capability::scoped(["urn:cap:browse:read:demo", CAP_ANNOTATE])
    }

    fn issue(k: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> Result<String> {
        let mut request = Request::new(verb, Iri::parse(iri.to_string()).unwrap());
        for (name, value) in args {
            request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
        }
        let repr = block_on(k.issue(request, &cap()))?;
        Ok(String::from_utf8_lossy(&repr.bytes).to_string())
    }

    fn grouped(k: &Kernel, iri: &str, kind: &str) -> serde_json::Value {
        serde_json::from_str(&issue(k, Verb::Source, iri, &[("group", kind)]).unwrap()).unwrap()
    }

    fn archive(store: &Arc<Store>) -> Archive {
        Archive::new(Arc::clone(store), GraphName::DefaultGraph)
    }

    /// Mint one pending finding on `rel`, quoting `exact` in `text`, as pass
    /// `pass` would — the real mint path, twin check included.
    #[allow(clippy::too_many_arguments)] // Reason: a test fixture naming every field a mint varies.
    fn mint(
        archive: &Archive,
        rel: &str,
        text: &str,
        pass: &str,
        exact: &str,
        note: &str,
        severity: &str,
        created: &str,
    ) -> String {
        match annotate::mint_pending_finding(
            archive,
            &format!("urn:repo:demo:file:{rel}"),
            "demo",
            rel,
            text,
            &content_hash(text.as_bytes()),
            exact,
            note,
            Some(severity),
            "r1",
            pass,
            Some(created.to_string()),
            Surface::File,
        )
        .unwrap()
        {
            Mint::Minted(iri) => iri,
            other => panic!("{exact} did not mint: {other:?}"),
        }
    }

    fn plant_pass(
        archive: &Archive,
        rel: &str,
        text: &str,
        tag: &str,
        minted: Vec<String>,
        at: &str,
    ) -> String {
        let hash = content_hash(text.as_bytes());
        let iri = pass_iri("demo", rel, &hash, tag);
        store_pass(
            archive,
            &PassEntry {
                iri: iri.clone(),
                repo: "demo".to_string(),
                rel: rel.to_string(),
                target_iri: format!("urn:repo:demo:file:{rel}"),
                hash,
                tag: tag.to_string(),
                model: "r1".to_string(),
                minted,
                carried: Vec::new(),
                reused_regions: Vec::new(),
                derived_regions: Vec::new(),
                orphaned_items: 0,
                suppressed_items: 0,
                reviewed_bytes: Some(text.len() as u64),
                total_bytes: Some(text.len() as u64),
                derived_at: Some(at.to_string()),
                superseded: 0,
            },
        )
        .unwrap();
        iri
    }

    fn decline(k: &Kernel, iri: &str, reason: Option<&str>) {
        let mut args = vec![("decision", "decline")];
        if let Some(word) = reason {
            args.push(("reason", word));
        }
        issue(k, Verb::Sink, iri, &args).unwrap();
    }

    fn publish(k: &Kernel, iri: &str) {
        issue(k, Verb::Sink, iri, &[("decision", "publish")]).unwrap();
    }

    /// The planted history every test reads.
    ///
    /// P1 over A (superseded once P2 lands — its line-5 finding is not
    /// carried) mints:
    /// * `old_alpha` on `fn alpha() {}`, later DECLINED `misread`;
    /// * `old_gamma` on `fn gamma() {}` and `old_beta` on `fn beta() {}` —
    ///   undecided, so SUPERSEDED by P2, which carries neither. `old_beta`'s
    ///   quote is still in the file and `beta` re-raises it: a like claim that
    ///   is merely superseded, never a twin.
    ///
    /// P2 over B (the current content) mints, before anyone declined anything
    /// — so every one of them arrived UNMARKED, like a pre-0.9.0 finding:
    /// * `alpha_1`, `alpha_2` on `fn alpha() {}` (major) — reworded twins of
    ///   the declined one, and near-duplicates of each other (alpha_1 older);
    /// * `alpha_minor` on `fn alpha()` (minor) — same line, another severity;
    /// * `comment` on `// Frobs the widget.` and `doc` on `/// The beta.`;
    /// * `beta` on `fn beta() {}`, and `published` on `fn gamma2() {}`,
    ///   which is then published;
    /// * `declined_beta` on `fn beta()` — declined, no reason.
    ///
    /// P3 over NOTES.md mints `notes` on `The queue is not a gate.`.
    struct History {
        old_alpha: String,
        old_gamma: String,
        old_beta: String,
        alpha_1: String,
        alpha_2: String,
        alpha_minor: String,
        comment: String,
        doc: String,
        beta: String,
        declined_beta: String,
        notes: String,
    }

    fn history(k: &Kernel, archive: &Archive) -> History {
        let tag = "review-v5@r1";
        let p1 = pass_iri("demo", "a.rs", &content_hash(A.as_bytes()), tag);
        let old_alpha = mint(
            archive,
            "a.rs",
            A,
            &p1,
            "fn alpha() {}",
            "no caller",
            "major",
            "2026-09-01T00:00:00.000Z",
        );
        let old_gamma = mint(
            archive,
            "a.rs",
            A,
            &p1,
            "fn gamma() {}",
            "gamma",
            "minor",
            "2026-09-01T00:00:00.000Z",
        );
        let old_beta = mint(
            archive,
            "a.rs",
            A,
            &p1,
            "fn beta() {}",
            "beta, first time",
            "minor",
            "2026-09-01T00:00:00.000Z",
        );
        plant_pass(
            archive,
            "a.rs",
            A,
            tag,
            vec![old_alpha.clone(), old_gamma.clone(), old_beta.clone()],
            "2026-09-01T00:00:00.000Z",
        );

        let p2 = pass_iri("demo", "a.rs", &content_hash(B.as_bytes()), tag);
        let alpha_2 = mint(
            archive,
            "a.rs",
            B,
            &p2,
            "fn alpha() {}",
            "alpha is never called",
            "major",
            "2026-09-03T00:00:00.000Z",
        );
        // A different quote of the same line, so the id differs within a pass.
        let alpha_1 = mint(
            archive,
            "a.rs",
            B,
            &p2,
            "fn alpha() {",
            "nothing calls alpha",
            "major",
            "2026-09-02T00:00:00.000Z",
        );
        let alpha_minor = mint(
            archive,
            "a.rs",
            B,
            &p2,
            "fn alpha()",
            "name it better",
            "minor",
            "2026-09-02T00:00:00.000Z",
        );
        let comment = mint(
            archive,
            "a.rs",
            B,
            &p2,
            "// Frobs the widget.",
            "frobbing is risky",
            "major",
            "2026-09-02T00:00:00.000Z",
        );
        let doc = mint(
            archive,
            "a.rs",
            B,
            &p2,
            "/// The beta.",
            "say more",
            "info",
            "2026-09-02T00:00:00.000Z",
        );
        let beta = mint(
            archive,
            "a.rs",
            B,
            &p2,
            "fn beta() {}",
            "beta is empty",
            "minor",
            "2026-09-02T00:00:00.000Z",
        );
        let declined_beta = mint(
            archive,
            "a.rs",
            B,
            &p2,
            "fn beta()",
            "beta again",
            "minor",
            "2026-09-02T00:00:00.000Z",
        );
        let published = mint(
            archive,
            "a.rs",
            B,
            &p2,
            "fn gamma2() {}",
            "good",
            "praise",
            "2026-09-02T00:00:00.000Z",
        );
        plant_pass(
            archive,
            "a.rs",
            B,
            tag,
            vec![
                alpha_1.clone(),
                alpha_2.clone(),
                alpha_minor.clone(),
                comment.clone(),
                doc.clone(),
                beta.clone(),
                declined_beta.clone(),
                published.clone(),
            ],
            "2026-09-02T00:00:00.000Z",
        );

        let p3 = pass_iri("demo", "NOTES.md", &content_hash(NOTES.as_bytes()), tag);
        let notes = mint(
            archive,
            "NOTES.md",
            NOTES,
            &p3,
            "The queue is not a gate.",
            "it is",
            "critical",
            "2026-09-02T00:00:00.000Z",
        );
        plant_pass(
            archive,
            "NOTES.md",
            NOTES,
            tag,
            vec![notes.clone()],
            "2026-09-02T00:00:00.000Z",
        );

        // The human acts AFTER the mints, so nothing above carries a mark.
        decline(k, &old_alpha, Some("misread"));
        decline(k, &declined_beta, None);
        publish(k, &published);
        History {
            old_alpha,
            old_gamma,
            old_beta,
            alpha_1,
            alpha_2,
            alpha_minor,
            comment,
            doc,
            beta,
            declined_beta,
            notes,
        }
    }

    fn setup() -> (PathBuf, Arc<Store>, Kernel, History) {
        let root = temp_root();
        let store = Arc::new(Store::new().unwrap());
        let k = kernel(&root, &store);
        let h = history(&k, &archive(&store));
        (root, store, k, h)
    }

    fn member_iris(group: &serde_json::Value) -> BTreeSet<String> {
        group["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["iri"].as_str().unwrap().to_string())
            .collect()
    }

    fn set(iris: &[&String]) -> BTreeSet<String> {
        iris.iter().map(|s| s.to_string()).collect()
    }

    /// Every member of every group of every kind is PENDING — never
    /// superseded, published or declined.
    fn assert_only_pending(v: &serde_json::Value, h: &History) {
        for group in v["groups"].as_array().unwrap() {
            for member in group["members"].as_array().unwrap() {
                assert_eq!(member["state"], "pending", "{group}");
                let iri = member["iri"].as_str().unwrap();
                assert_ne!(iri, h.old_gamma, "superseded in a group: {group}");
                assert_ne!(iri, h.old_beta, "superseded in a group: {group}");
                assert_ne!(iri, h.old_alpha);
                assert_ne!(iri, h.declined_beta);
            }
        }
    }

    #[test]
    fn the_planted_history_has_the_states_it_claims() {
        let (root, _store, k, h) = setup();
        let all: serde_json::Value = serde_json::from_str(
            &issue(
                &k,
                Verb::Source,
                "urn:repo:demo:findings",
                &[("state", "all")],
            )
            .unwrap(),
        )
        .unwrap();
        let state = |iri: &str| {
            all.as_array()
                .unwrap()
                .iter()
                .find(|r| r["iri"] == iri)
                .map(|r| r["state"].as_str().unwrap().to_string())
                .unwrap()
        };
        assert_eq!(state(&h.old_gamma), "superseded");
        assert_eq!(state(&h.old_beta), "superseded");
        assert_eq!(state(&h.old_alpha), "declined");
        assert_eq!(state(&h.alpha_2), "pending");
        assert!(
            all.as_array()
                .unwrap()
                .iter()
                .all(|r| r["prior_decision"].is_null()),
            "every mint predates every decline"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ `recurrence` is the 0.9.0 mark computed backwards: the UNMARKED
    /// pending findings that quote the declined line, grouped under the twin,
    /// the twin's word as the suggestion — and the twin itself rides along
    /// with its decision.
    #[test]
    fn recurrence_groups_the_unmarked_under_the_declined_twin() {
        let (root, _store, k, h) = setup();
        let v = grouped(&k, "urn:repo:demo:findings", "recurrence");
        assert_only_pending(&v, &h);
        let groups = v["groups"].as_array().unwrap();
        // alpha_2 quotes `fn alpha() {}` exactly as the declined twin did;
        // alpha_1 (`fn alpha() {`) is another quote and not a like claim by
        // the mark's key. `declined_beta` is a twin with no pending re-raise
        // of its exact quote, so it forms nothing.
        assert_eq!(groups.len(), 1, "{v}");
        let group = &groups[0];
        assert_eq!(group["kind"], "recurrence");
        assert_eq!(
            group["key"],
            format!("recurrence:{}", &h.old_alpha["urn:iki:finding:".len()..])
        );
        assert_eq!(member_iris(group), set(&[&h.alpha_2]));
        assert_eq!(group["reason"], "misread", "the twin's word");
        assert_eq!(group["twin"]["iri"], h.old_alpha.as_str());
        assert_eq!(group["twin"]["decision"]["reason"], "misread");
        assert_eq!(group["twin"]["state"], "declined");
        assert_eq!(group["path"], "a.rs");
        assert_eq!(group["annotates"], "urn:repo:demo:file:a.rs");
        assert!(group["kept"].is_null());
        // The twin's word, and its date when the decision carries one (this
        // kernel mounts no clock, so it does not).
        assert_eq!(
            group["label"],
            "1 finding on a.rs quotes a line where a like claim was already declined (misread)"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A twin with no reason word suggests none, and a SUPERSEDED like claim
    /// is not a twin at all: it asserts no judgment, so it forms no group.
    #[test]
    fn a_superseded_twin_forms_no_group_and_a_wordless_twin_suggests_nothing() {
        let (root, store, k, h) = setup();
        let archive = archive(&store);
        // A pending re-raise of the declined-without-a-word `fn beta()`, and
        // one of the SUPERSEDED `fn gamma() {}` — both under a second tag on
        // the current bytes, so both stand.
        let p4 = pass_iri("demo", "a.rs", &content_hash(B.as_bytes()), "other@r2");
        let beta_again = match annotate::mint_pending_finding(
            &archive,
            "urn:repo:demo:file:a.rs",
            "demo",
            "a.rs",
            B,
            &content_hash(B.as_bytes()),
            "fn beta()",
            "beta, reworded",
            Some("minor"),
            "r2",
            &p4,
            Some("2026-09-05T00:00:00.000Z".to_string()),
            Surface::File,
        )
        .unwrap()
        {
            Mint::Minted(iri) => iri,
            other => panic!("{other:?}"),
        };
        plant_pass(
            &archive,
            "a.rs",
            B,
            "other@r2",
            vec![beta_again.clone()],
            "2026-09-05T00:00:00.000Z",
        );
        let v = grouped(&k, "urn:repo:demo:findings", "recurrence");
        assert_only_pending(&v, &h);
        let groups = v["groups"].as_array().unwrap();
        let beta_group = groups
            .iter()
            .find(|g| g["twin"]["iri"] == h.declined_beta.as_str())
            .unwrap_or_else(|| panic!("no group under the wordless twin: {v}"));
        assert_eq!(member_iris(beta_group), set(&[&beta_again]));
        assert!(beta_group["reason"].is_null(), "{beta_group}");
        // Nothing groups under the SUPERSEDED like claim: `beta` quotes
        // `fn beta() {}` exactly as the superseded `old_beta` did, and is in
        // no recurrence group — superseded is not declined.
        assert!(
            groups
                .iter()
                .all(|g| g["twin"]["iri"] != h.old_beta.as_str()),
            "{v}"
        );
        assert!(
            groups.iter().all(|g| !member_iris(g).contains(&h.beta)),
            "{v}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ `near-duplicate`: same file, same current line, same proposed
    /// severity. The OLDEST is kept out of the group; a different severity on
    /// the same line is not a duplicate; a single finding is no group.
    #[test]
    fn near_duplicates_keep_the_oldest_out_of_the_group() {
        let (root, _store, k, h) = setup();
        let v = grouped(&k, "urn:repo:demo:findings", "near-duplicate");
        assert_only_pending(&v, &h);
        let groups = v["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{v}");
        let group = &groups[0];
        assert_eq!(
            group["kept"]["iri"],
            h.alpha_1.as_str(),
            "the oldest is kept"
        );
        assert_eq!(member_iris(group), set(&[&h.alpha_2]));
        assert!(
            !member_iris(group).contains(&h.alpha_minor),
            "another severity"
        );
        assert_eq!(group["reason"], "duplicate");
        assert_eq!(
            group["key"],
            format!("near-duplicate:{}", &h.alpha_1["urn:iki:finding:".len()..])
        );
        assert_eq!(
            group["label"],
            "1 finding on a.rs L2 (major) re-raises an older one on the same line"
        );
        assert!(group["twin"].is_null());
        std::fs::remove_dir_all(&root).ok();
    }

    /// `comment-shape`: per file, the pending findings whose quote is a
    /// comment or doc line — every quote of a doc file, the comment-led quotes
    /// of a source file, and no code line.
    #[test]
    fn comment_shape_groups_comment_and_doc_quotes_per_file() {
        let (root, _store, k, h) = setup();
        let v = grouped(&k, "urn:repo:demo:findings", "comment-shape");
        assert_only_pending(&v, &h);
        let groups = v["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "{v}");
        // Larger first.
        assert_eq!(groups[0]["path"], "a.rs");
        assert_eq!(member_iris(&groups[0]), set(&[&h.comment, &h.doc]));
        assert_eq!(
            groups[0]["label"],
            "2 findings on a.rs whose quote is a comment or doc line"
        );
        assert_eq!(groups[0]["reason"], "restates");
        assert_eq!(groups[0]["key"], "comment-shape:a.rs");
        assert_eq!(groups[1]["path"], "NOTES.md");
        assert_eq!(member_iris(&groups[1]), set(&[&h.notes]));
        std::fs::remove_dir_all(&root).ok();
    }

    /// `file`: every pending finding on one file, no suggestion — and a
    /// path-scoped listing groups that file alone.
    #[test]
    fn file_groups_everything_pending_per_file() {
        let (root, _store, k, h) = setup();
        let v = grouped(&k, "urn:repo:demo:findings", "file");
        assert_only_pending(&v, &h);
        let groups = v["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "{v}");
        assert_eq!(
            member_iris(&groups[0]),
            set(&[
                &h.alpha_1,
                &h.alpha_2,
                &h.alpha_minor,
                &h.comment,
                &h.doc,
                &h.beta
            ])
        );
        assert!(groups[0]["reason"].is_null());
        assert_eq!(groups[0]["label"], "6 pending findings on a.rs");
        // Members carry the full row shape the findings face serves.
        let member = &groups[0]["members"][0];
        for field in [
            "iri",
            "exact",
            "line",
            "severity",
            "state",
            "superseded_by",
            "prior_decision",
        ] {
            assert!(member.get(field).is_some(), "{field} missing: {member}");
        }

        let scoped = grouped(&k, "urn:repo:demo:findings:NOTES.md", "file");
        assert_eq!(scoped["path"], "NOTES.md");
        let groups = scoped["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{scoped}");
        assert_eq!(member_iris(&groups[0]), set(&[&h.notes]));
        std::fs::remove_dir_all(&root).ok();
    }

    /// The size of each lever, per root: every kind in contract order with
    /// its group and member counts, whichever kind was asked for.
    #[test]
    fn every_group_read_counts_every_kind() {
        let (root, _store, k, _h) = setup();
        let v = grouped(&k, "urn:repo:demo:findings", "file");
        assert_eq!(v["group"], "file");
        assert_eq!(v["state"], "pending");
        assert_eq!(
            v["kinds"],
            serde_json::json!([
                {"kind": "recurrence", "groups": 1, "findings": 1},
                {"kind": "near-duplicate", "groups": 1, "findings": 1},
                {"kind": "comment-shape", "groups": 2, "findings": 3},
                {"kind": "file", "groups": 2, "findings": 7},
            ])
        );
        let plain = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:findings",
            &[("group", "near-duplicate"), ("as", "text/plain")],
        )
        .unwrap();
        assert!(
            plain.contains("by kind: recurrence 1 group / 1 finding"),
            "{plain}"
        );
        assert!(plain.contains("suggested reason: duplicate"), "{plain}");
        assert!(plain.contains("  keep: a.rs L2"), "{plain}");
        assert!(plain.contains(A_PROPOSAL), "{plain}");
        let html = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:findings",
            &[("group", "recurrence"), ("as", "text/html")],
        )
        .unwrap();
        assert!(
            html.contains("browse-findings-group-kind-current"),
            "{html}"
        );
        assert!(html.contains("the declined twin"), "{html}");
        assert!(
            html.contains("suggested reason: <code>misread</code>"),
            "{html}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ The kinds are the CONTRACT's, read the way gonk reads its menus:
    /// Meta on the findings face, the `group` input, its `one_of` — in order.
    /// An unknown kind is refused naming the set; a group read beside another
    /// state, a summary, or the turtle face is refused by name.
    #[test]
    fn the_kinds_are_declared_in_order_and_everything_else_is_refused() {
        let (root, _store, k, _h) = setup();
        let description = k
            .describe(&Iri::parse("urn:repo:demo:findings".to_string()).unwrap())
            .expect("the findings face describes itself");
        let source = description
            .action_specs()
            .into_iter()
            .find(|s| s.verb == Verb::Source)
            .expect("a Source action");
        let group = source
            .inputs
            .iter()
            .find(|i| i.name == "group")
            .expect("group is declared");
        assert_eq!(
            group.one_of,
            ["recurrence", "near-duplicate", "comment-shape", "file"]
        );
        assert!(!group.required);
        for (kind, meaning) in GROUP_KINDS.iter().zip(GROUP_KIND_MEANINGS) {
            assert!(group.summary.contains(&format!("{kind}: {meaning}")));
        }

        let refused = |args: &[(&str, &str)]| {
            issue(&k, Verb::Source, "urn:repo:demo:findings", args)
                .expect_err("refused")
                .to_string()
        };
        let unknown = refused(&[("group", "all")]);
        assert!(
            unknown.contains("recurrence, near-duplicate, comment-shape, file"),
            "{unknown}"
        );
        assert!(refused(&[("group", "file"), ("state", "declined")]).contains("state"));
        assert!(refused(&[("group", "file"), ("summary", "states")]).contains("summary"));
        assert!(refused(&[("group", "file"), ("as", "text/turtle")]).contains("as"));
        // state=pending, spelled out, is the same read.
        assert_eq!(
            grouped(&k, "urn:repo:demo:findings", "file"),
            serde_json::from_str::<serde_json::Value>(
                &issue(
                    &k,
                    Verb::Source,
                    "urn:repo:demo:findings",
                    &[("group", "file"), ("state", "pending")]
                )
                .unwrap()
            )
            .unwrap()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Every suggestion is a word of the Sink's own `one_of`.
    #[test]
    fn every_suggestion_is_a_contract_word() {
        for kind in GROUP_KINDS {
            if let Some(word) = fixed_suggestion(kind) {
                assert!(crate::finding::is_decline_reason(word), "{kind}: {word}");
            }
        }
        assert_eq!(fixed_suggestion("near-duplicate"), Some("duplicate"));
        assert_eq!(fixed_suggestion("comment-shape"), Some("restates"));
        assert_eq!(fixed_suggestion("file"), None);
    }

    #[test]
    fn the_comment_line_rule() {
        for comment in [
            "// a",
            "/// doc",
            "//! inner",
            "  /* block",
            " * continued",
            "*/",
            "*",
            "# toml or python",
            "#",
            "## heading-ish",
            "-- sql",
            ";; lisp",
            "; ini",
            "<!-- html -->",
            "\"\"\"docstring",
        ] {
            assert!(is_comment_line(comment), "{comment:?} is a comment");
        }
        for code in [
            "fn alpha() {}",
            "#[derive(Debug)]",
            "#![allow(x)]",
            "#include <stdio.h>",
            "x = 1; // trailing",
            "*ptr = 1;",
            "---",
            "-x",
            "",
        ] {
            assert!(!is_comment_line(code), "{code:?} is code");
        }
    }
}
