//! Anchoring a quoted snippet back into the file it came from.
//!
//! ⚠ The anchor is the proof the model read the file, so a miss must never be
//! repaired by a fuzzy match. A finding whose quote does not occur verbatim is
//! discarded and counted, never re-anchored at the closest line — a review that
//! silently relocates its evidence is a review nobody can audit.
//!
//! ⚠ A quote that occurs TWICE is ambiguous and is treated as a miss for the
//! same reason: the second occurrence is as good a claim as the first, and
//! picking the earlier one invents a certainty the answer did not carry.

/// Where `quote` occurs in `text`, as a byte range.
///
/// Returns `None` when the quote is absent and when it occurs more than once.
pub fn locate(text: &str, quote: &str) -> Option<(usize, usize)> {
    let first = text.find(quote)?;
    if text[first + 1..].contains(quote) {
        return None;
    }
    Some((first, first + quote.len()))
}

/// The 1-based line a byte offset falls on, counting from line 1 for the first
/// line of the file — the number a reader sees in an editor gutter and the
/// number the finding rows and the digest face both print.
///
/// ⚠ Byte offset, not char offset. Every caller here works in bytes because
/// the store records byte ranges, and mixing the two silently shifts every
/// anchor in a file with one non-ASCII character in it.
pub fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count()
}

/// The finding as it is recorded: the quote, where it landed, and the note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchored {
    pub quote: String,
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub note: String,
}

/// Anchor every finding that can be anchored; the rest are counted as orphans.
///
/// ⚠ One bad item must not kill the pass. A model that misquotes once has still
/// read the file, and discarding the other findings would spend the whole call
/// again on the next ask.
pub fn anchor_all(text: &str, items: &[(String, String)]) -> (Vec<Anchored>, usize) {
    let mut anchored = Vec::new();
    let mut orphans = 0usize;
    for (quote, note) in items {
        match locate(text, quote) {
            Some((start, end)) => anchored.push(Anchored {
                quote: quote.clone(),
                start,
                end,
                line: line_of(text, start),
                note: note.clone(),
            }),
            None => orphans += 1,
        }
    }
    (anchored, orphans)
}

/// One row of the triage digest.
///
/// ⚠ The digest is read by a human triaging a queue, so it shows the line the
/// finding is ON and never the region index: a region number changes when the
/// chunk size is tuned, and a stable reference is the whole point of recording
/// a line at all.
pub fn digest_line(f: &Anchored) -> String {
    format!("line {}: {}", f.line, f.note)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unique_quote_anchors() {
        let text = "alpha\nbeta\ngamma\n";
        assert_eq!(locate(text, "beta"), Some((6, 10)));
    }

    #[test]
    fn a_repeated_quote_is_a_miss() {
        let text = "beta\nbeta\n";
        assert_eq!(locate(text, "beta"), None);
    }
}
