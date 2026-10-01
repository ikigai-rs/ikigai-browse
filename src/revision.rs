//! Revised and unconfirmed decisions (ledger #653) — whether a recorded
//! decline is one a human evidently MEANT, and the listing a human walks to
//! confirm, retract or reverse the ones that are not.
//!
//! ## Why this exists
//!
//! Measured 2026-10-01 over the live store: the 43 distinct declines that
//! pending recurrences pointed at carried ZERO reason words, and 39 of the 43
//! were made in same-second bursts (17 at 02:42:38, 13 at 03:18:10, 9 across
//! 03:24:10-11) — before the batch view existed. Each still marked its
//! recurrences (`prior_decision`) and, by the 2026-09-25 rule, counted as
//! EVIDENCE that pre-ticks a recurrence batch row in a host. One mis-click
//! propagated, and a decision could not be revisited.
//!
//! The record stays append-only ([`crate::annotate::append_decision`]): a
//! decision can now be REVISED by a later one that names it, and both are
//! kept. This module is the read side: a computed `confirmed` on every
//! decision and every `prior_decision`, and `summary=unconfirmed` on the
//! findings face. ★ Nothing is hidden or dropped by it — `confirmed` is
//! information for a host's pre-tick rule and display, never a filter here.
//!
//! ## The rule
//!
//! A decision is UNCONFIRMED when it is a decline with no reason word that
//! was made in a batch (recorded provenance, `made=batch`) or — for a decline
//! whose provenance was never recorded, which is every one before this
//! module — in a BURST. Everything else is confirmed: a publish, a
//! retraction, a decline with a word, a decline recorded as made singly. A
//! revision that confirms (a word, or a decline made singly) is a new
//! decision and reads confirmed by the same rule; the old node keeps its own
//! reading, because nothing about it changed.
//!
//! ## The burst, and why its threshold
//!
//! A burst is [`BURST_MIN_DECLINES`] or more declines whose timestamps all
//! fall within [`BURST_WINDOW_MS`] of each other, among declines with NO
//! recorded provenance, counted across the whole graph (a host's batch
//! spanned repos). Overlapping windows merge into one burst, named by its
//! first timestamp.
//!
//! ★ One second, because a decline that a human made by READING the finding
//! cannot be followed by another inside a second: opening a finding, reading
//! its quote and note and choosing takes several. Three, because two inside
//! a second is a double-submit (which the Sink already absorbs as a no-op) or
//! one quick correction, while three is a loop or a fan-out. Both numbers
//! are deliberately conservative in the direction that matters: a burst only
//! WITHHOLDS evidence a host would otherwise pre-tick from, and nothing is
//! discarded. A decision with a recorded provenance is never inferred, in
//! either direction — the record wins over the clock.

use std::collections::{BTreeMap, BTreeSet};

use ikigai_core::Result;
use oxigraph::model::{NamedNode, Term};

use crate::annotate::{
    self, annotation_json, Annotation, Decision, Family, Made, Outcome, DCTERMS_CREATED,
    DCTERMS_PROVENANCE, DCTERMS_TYPE,
};
use crate::archive::Archive;
use crate::esc;

/// How many declines inside [`BURST_WINDOW_MS`] make a burst — see the
/// module doc for why three.
pub(crate) const BURST_MIN_DECLINES: usize = 3;

/// The window a burst's declines all fall inside, in milliseconds — see the
/// module doc for why one second.
pub(crate) const BURST_WINDOW_MS: u64 = 1000;

/// Whether `decision` is one a human evidently meant, given whether it sits
/// in a burst. The ONE place the rule is spelled.
///
/// `in_burst` is consulted only for a wordless decline with no recorded
/// provenance; the loader passes `true` until [`mark`] knows (see
/// [`Decision::confirmed`]).
pub(crate) fn confirmed(decision: &Decision, in_burst: bool) -> bool {
    if decision.outcome != Outcome::Declined || decision.reason.is_some() {
        return true;
    }
    match &decision.made {
        Some(Made::Single) => true,
        Some(Made::Batch(_)) => false,
        None => !in_burst,
    }
}

/// Every burst in the graph: which decline nodes are in one, and each
/// burst's first timestamp and size.
pub(crate) struct Bursts {
    /// Decision node IRI → the first timestamp of its burst.
    start_of: BTreeMap<String, String>,
    /// Burst start → how many declines it holds, graph-wide.
    sizes: BTreeMap<String, usize>,
}

impl Bursts {
    /// Find them: every DECLINED decision node in the archive's graph with no
    /// recorded provenance and a timestamp. One indexed pattern for the
    /// declines plus one subject lookup each — never a walk over findings.
    pub(crate) fn of(archive: &Archive) -> Result<Bursts> {
        let store_err =
            |e: oxigraph::store::StorageError| ikigai_core::Error::Endpoint(format!("browse: {e}"));
        let declined = NamedNode::new(Outcome::Declined.iri()).expect("an outcome IRI is valid");
        let type_ = NamedNode::new(DCTERMS_TYPE).expect("a dcterms IRI is valid");
        let mut times = Vec::new();
        for quad in
            archive.quads_for_pattern(None, Some(type_.as_ref()), Some(declined.as_ref().into()))
        {
            let quad = quad.map_err(store_err)?;
            let subject = quad.subject.to_string();
            let node = subject.trim_start_matches('<').trim_end_matches('>');
            if !node.starts_with(Family::Finding.prefix()) || !node.contains(":decision") {
                continue;
            }
            let Ok(subject) = NamedNode::new(node) else {
                continue;
            };
            let mut at = None;
            let mut recorded = false;
            for quad in archive.quads_for_pattern(Some(subject.as_ref().into()), None, None) {
                let quad = quad.map_err(store_err)?;
                match quad.predicate.as_str() {
                    DCTERMS_CREATED => {
                        if let Term::Literal(l) = &quad.object {
                            at = Some(l.value().to_string());
                        }
                    }
                    DCTERMS_PROVENANCE => {
                        recorded |= matches!(&quad.object,
                            Term::NamedNode(n) if Made::from_iri(n.as_str()).is_some());
                    }
                    _ => {}
                }
            }
            if recorded {
                continue;
            }
            if let Some(at) = at {
                if let Some(ms) = parse_millis(&at) {
                    times.push((ms, at, node.to_string()));
                }
            }
        }
        Ok(Bursts::of_times(times))
    }

    /// The window rule over `(millis, timestamp, node)` triples — pure, so the
    /// edges are tested without a store.
    fn of_times(mut times: Vec<(u64, String, String)>) -> Bursts {
        times.sort();
        let mut start_of = BTreeMap::new();
        let mut sizes = BTreeMap::new();
        // Each qualifying window is an index range; overlapping ranges merge
        // into one burst, named by its first member's timestamp.
        let mut current: Option<(usize, usize)> = None;
        let mut flush = |range: (usize, usize)| {
            let start = times[range.0].1.clone();
            sizes.insert(start.clone(), range.1 - range.0 + 1);
            for (_, _, node) in &times[range.0..=range.1] {
                start_of.insert(node.clone(), start.clone());
            }
        };
        let mut end = 0;
        for i in 0..times.len() {
            end = end.max(i);
            while end + 1 < times.len() && times[end + 1].0 - times[i].0 < BURST_WINDOW_MS {
                end += 1;
            }
            if end - i + 1 < BURST_MIN_DECLINES {
                continue;
            }
            current = match current {
                Some((first, last)) if i <= last => Some((first, last.max(end))),
                Some(done) => {
                    flush(done);
                    Some((i, end))
                }
                None => Some((i, end)),
            };
        }
        if let Some(done) = current {
            flush(done);
        }
        Bursts { start_of, sizes }
    }

    /// The burst a decision node is in — its first timestamp.
    pub(crate) fn of_node(&self, node: &str) -> Option<&str> {
        self.start_of.get(node).map(String::as_str)
    }

    /// How many declines the burst starting at `start` holds, graph-wide.
    pub(crate) fn size(&self, start: &str) -> Option<usize> {
        self.sizes.get(start).copied()
    }

    /// Settle `confirmed` and `burst` on every decision a finding carries: its
    /// chain, its current answer, and its recurrence mark's twin.
    pub(crate) fn apply(&self, findings: &mut [Annotation]) {
        let settle = |d: &mut Decision| {
            d.burst = match d.made {
                None => self.of_node(&d.iri).map(str::to_string),
                Some(_) => None,
            };
            d.confirmed = confirmed(d, d.burst.is_some());
        };
        for finding in findings {
            finding.history.iter_mut().for_each(settle);
            finding.settle();
            if let Some(decision) = finding.prior.as_mut().and_then(|p| p.decision.as_mut()) {
                settle(decision);
            }
        }
    }
}

/// The confirmation pass every face that renders a decision runs: find the
/// bursts once, settle every finding.
pub(crate) fn mark(archive: &Archive, findings: &mut [Annotation]) -> Result<()> {
    Bursts::of(archive)?.apply(findings);
    Ok(())
}

/// Milliseconds since the epoch for the timestamp shape this crate writes
/// (`YYYY-MM-DDTHH:MM:SS.sssZ`, [`crate::explain::iso8601`]) — and without
/// the fraction. `None` for anything else: a decline whose time cannot be
/// read is never placed in a burst.
pub(crate) fn parse_millis(at: &str) -> Option<u64> {
    let (date, time) = at.strip_suffix('Z')?.split_once('T')?;
    let mut ymd = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let (clock, frac) = match time.split_once('.') {
        Some((clock, frac)) => (clock, frac),
        None => (time, "0"),
    };
    let mut hms = clock.splitn(3, ':').map(|p| p.parse::<u64>().ok());
    let (h, m, s) = (hms.next()??, hms.next()??, hms.next()??);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || h > 23 || m > 59 || s > 60 {
        return None;
    }
    let ms: u64 = format!("{frac:0<3}").get(..3)?.parse().ok()?;
    // Days from the civil date (the inverse of `iso8601`'s algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = u64::try_from(days).ok()? * 86_400 + h * 3600 + m * 60 + s;
    Some(secs * 1000 + ms)
}

// --- the walk: unconfirmed declines that still steer ------------------------

/// The `summary=unconfirmed` reading of one listing: the UNCONFIRMED declines
/// that still STEER — each has at least one pending finding re-raising its
/// claim (the same target, the same quote: the key the mark and the
/// recurrence group both use) — grouped by the burst or batch they were made
/// in, OLDEST FIRST, so a human walks them in the order they were made:
/// confirm with a word, retract, or reverse.
pub(crate) struct Unconfirmed<'a> {
    groups: Vec<Group<'a>>,
}

struct Group<'a> {
    /// `burst` or `batch`.
    by: &'static str,
    /// The burst's first timestamp, or the batch's group key.
    key: String,
    /// The earliest decision time among the group's declines.
    first: Option<String>,
    /// How many declines the whole burst holds, graph-wide (bursts only).
    size: Option<usize>,
    /// Each decline, with the pending findings that re-raise it.
    declines: Vec<(&'a Annotation, Vec<String>)>,
}

impl<'a> Unconfirmed<'a> {
    /// Read it from a listing's findings — every state, already through the
    /// supersession and confirmation passes.
    pub(crate) fn of(findings: &'a [Annotation], bursts: &Bursts) -> Unconfirmed<'a> {
        let mut pending: BTreeMap<(&str, &str), Vec<String>> = BTreeMap::new();
        for f in findings.iter().filter(|f| f.state() == Some("pending")) {
            pending
                .entry((f.target_iri.as_str(), f.exact.as_str()))
                .or_default()
                .push(f.iri());
        }
        let mut groups: BTreeMap<(&'static str, String), Group<'a>> = BTreeMap::new();
        for f in findings {
            let Some(d) = &f.decision else { continue };
            if d.outcome != Outcome::Declined || d.confirmed {
                continue;
            }
            let Some(raised) = pending.get(&(f.target_iri.as_str(), f.exact.as_str())) else {
                continue;
            };
            let (by, key, size) = match (&d.made, &d.burst) {
                (Some(Made::Batch(key)), _) => ("batch", key.clone(), None),
                (_, Some(start)) => ("burst", start.clone(), bursts.size(start)),
                // Unreachable by the rule (an unconfirmed decline is a batch's
                // or a burst's); kept honest rather than dropped.
                _ => ("burst", String::new(), None),
            };
            let group = groups.entry((by, key.clone())).or_insert_with(|| Group {
                by,
                key,
                first: None,
                size,
                declines: Vec::new(),
            });
            let earlier = match (d.at.as_deref().and_then(parse_millis), &group.first) {
                (Some(t), Some(first)) => parse_millis(first).is_none_or(|f| t < f),
                (Some(_), None) => true,
                (None, _) => false,
            };
            if earlier {
                group.first.clone_from(&d.at);
            }
            let mut raised = raised.clone();
            raised.sort();
            group.declines.push((f, raised));
        }
        let mut groups: Vec<Group<'a>> = groups.into_values().collect();
        // ⚠ Ordered by TIME, not by the timestamp's text: the store serves an
        // xsd:dateTime in canonical form (`…:38Z`, `…:38.5Z`), whose text
        // order is not its time order within a second.
        let time = |at: &Option<String>| at.as_deref().and_then(parse_millis);
        for group in &mut groups {
            group.declines.sort_by(|(a, _), (b, _)| {
                let at = |f: &Annotation| time(&f.decision.as_ref().and_then(|d| d.at.clone()));
                (at(a).is_none(), at(a), &a.id).cmp(&(at(b).is_none(), at(b), &b.id))
            });
        }
        // Oldest first; a group with no time on record sorts last.
        groups.sort_by(|a, b| {
            let (ta, tb) = (time(&a.first), time(&b.first));
            (ta.is_none(), ta, &a.key).cmp(&(tb.is_none(), tb, &b.key))
        });
        Unconfirmed { groups }
    }

    fn count(&self) -> usize {
        self.groups.iter().map(|g| g.declines.len()).sum()
    }

    fn steered(&self) -> usize {
        let all: BTreeSet<&String> = self
            .groups
            .iter()
            .flat_map(|g| g.declines.iter().flat_map(|(_, raised)| raised))
            .collect();
        all.len()
    }

    /// `{count, steered, groups: [{by, key, first_decided_at, size, count,
    /// declines: [<row> + pending]}]}` — each decline is the listing's own
    /// row shape plus `pending`, the IRIs of the findings that re-raise it.
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "count": self.count(),
            "steered": self.steered(),
            "groups": self.groups.iter().map(|g| serde_json::json!({
                "by": g.by,
                "key": g.key,
                "first_decided_at": g.first,
                "size": g.size,
                "count": g.declines.len(),
                "declines": g.declines.iter().map(|(f, raised)| {
                    let mut row = annotation_json(f, None);
                    row["pending"] = serde_json::json!(raised);
                    row
                }).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }

    /// The headline in words — also the plain and html faces' first line.
    fn words(&self) -> String {
        let n = self.count();
        match n {
            0 => "no unconfirmed decline steers a pending finding here".to_string(),
            _ => format!(
                "{n} unconfirmed decline{} still steer{} {} pending finding{} — oldest first; \
                 confirm each with a word, retract it, or reverse it (Sink the declined \
                 finding with revises=<its decision>)",
                if n == 1 { "" } else { "s" },
                if n == 1 { "s" } else { "" },
                self.steered(),
                if self.steered() == 1 { "" } else { "s" },
            ),
        }
    }

    fn group_words(group: &Group<'_>) -> String {
        let mut out = match group.by {
            "batch" => format!("batch {}", group.key),
            _ => format!("burst at {}", group.key),
        };
        if let Some(size) = group.size {
            out.push_str(&format!(" ({size} declines inside one second)"));
        }
        out
    }

    pub(crate) fn plain(&self) -> String {
        let mut out = format!("--- unconfirmed declines ---\n{}", self.words());
        for group in &self.groups {
            out.push_str(&format!("\n{}", Self::group_words(group)));
            for (f, raised) in &group.declines {
                let decision = f.decision.as_ref().map(|d| d.iri.as_str()).unwrap_or("");
                // The row (it carries its path), then what to name to revise
                // it and how much it steers.
                out.push_str(&format!(
                    "\n  {}\n    revises={decision} · re-raised by {} pending",
                    crate::finding::plain_row(f, None),
                    raised.len()
                ));
            }
        }
        out
    }

    pub(crate) fn html(&self) -> String {
        let mut out = format!(
            "<div class=\"browse-findings-unconfirmed\"><p>{}</p>",
            esc(&self.words())
        );
        for group in &self.groups {
            out.push_str(&format!(
                "<h3 class=\"browse-findings-unconfirmed-group\">{}</h3>\
                 <div class=\"browse-annotations\">",
                esc(&Self::group_words(group))
            ));
            for (f, _) in &group.declines {
                out.push_str(&annotate::annotation_card_html(f, None, true));
            }
            out.push_str("</div>");
        }
        out.push_str("</div>");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> (u64, String, String) {
        let iso = crate::explain::iso8601(ms);
        (ms, iso.clone(), format!("urn:iki:finding:n{ms}:decision"))
    }

    /// The timestamp reader is the exact inverse of the writer, across a
    /// leap day, a year boundary, and the fraction-less form.
    #[test]
    fn the_timestamp_reader_inverts_the_writer() {
        for ms in [
            0,
            1_727_058_158_123,
            1_582_934_400_000 + 86_399_999, // 2020-02-29T23:59:59.999Z
            1_735_689_599_999,              // 2024-12-31T23:59:59.999Z
            1_790_000_000_001,
        ] {
            assert_eq!(parse_millis(&crate::explain::iso8601(ms)), Some(ms), "{ms}");
        }
        assert_eq!(
            parse_millis("2026-09-23T02:42:38Z"),
            parse_millis("2026-09-23T02:42:38.000Z")
        );
        for bad in ["", "2026-09-23", "2026-13-01T00:00:00Z", "yesterday"] {
            assert_eq!(parse_millis(bad), None, "{bad}");
        }
    }

    /// ★ The window rule's edges: three inside a second is a burst, two is
    /// not, three spread wider than a second is not, and overlapping windows
    /// merge into ONE burst named by its first member.
    #[test]
    fn three_declines_inside_one_second_are_a_burst() {
        let base = 1_790_000_000_000;
        let b = Bursts::of_times(vec![at(base), at(base + 400), at(base + 999)]);
        assert_eq!(b.start_of.len(), 3, "three inside a second");
        assert_eq!(b.size(&crate::explain::iso8601(base)), Some(3));

        let b = Bursts::of_times(vec![at(base), at(base + 999)]);
        assert!(b.start_of.is_empty(), "two is a double-submit, not a burst");

        let b = Bursts::of_times(vec![at(base), at(base + 500), at(base + 1000)]);
        assert!(
            b.start_of.is_empty(),
            "exactly a second apart is outside it"
        );

        // 0, 300, 600 qualify; 900 extends the second window (300..900);
        // 5000 stands alone; 9000, 9001, 9002 are a second burst.
        let b = Bursts::of_times(vec![
            at(base),
            at(base + 300),
            at(base + 600),
            at(base + 900),
            at(base + 5000),
            at(base + 9000),
            at(base + 9001),
            at(base + 9002),
        ]);
        let first = crate::explain::iso8601(base);
        assert_eq!(b.size(&first), Some(4), "overlapping windows merge");
        assert_eq!(
            b.of_node(&format!("urn:iki:finding:n{}:decision", base + 900)),
            Some(first.as_str())
        );
        assert_eq!(
            b.of_node(&format!("urn:iki:finding:n{}:decision", base + 5000)),
            None
        );
        assert_eq!(b.size(&crate::explain::iso8601(base + 9000)), Some(3));
        assert_eq!(b.sizes.len(), 2);
    }
}
