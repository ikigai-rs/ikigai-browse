//! The pending-findings queue: rows a human triages before anything is
//! published.
//!
//! ⚠ Nothing leaves this queue except by a human decision. A pass writes
//! `pending` rows and never `published` ones, so an automated reviewer cannot
//! put a word in front of a reader without someone approving it.
//!
//! ⚠ A decision is APPENDED, never overwritten: the proposal and the decision
//! live on different nodes, so "what did the machine say before I declined it"
//! stays answerable.

use std::collections::HashMap;

/// A queued finding.
#[derive(Clone, Debug)]
pub struct Pending {
    pub id: String,
    pub path: String,
    pub severity: String,
}

/// The queue, keyed by id.
pub type Queue = HashMap<String, Pending>;

/// Look one row up.
///
/// Returns `None` for an id the queue does not hold — an id can vanish between
/// a page render and a click, because a decision removes the row, and every
/// caller here handles the absence rather than assuming the row is still there.
pub fn get<'a>(queue: &'a Queue, id: &str) -> Option<&'a Pending> {
    queue.get(id)
}

/// The severity a face shows for one row.
pub fn severity_of(queue: &Queue, id: &str) -> String {
    get(queue, id).unwrap().severity.clone()
}

/// The rows for one path, in a stable order.
///
/// ⚠ Sorted by id and not by insertion: a HashMap iteration order is arbitrary
/// and would make the page reorder itself between two refreshes that changed
/// nothing.
pub fn rows_for<'a>(queue: &'a Queue, path: &str) -> Vec<&'a Pending> {
    let mut rows: Vec<&Pending> = queue.values().filter(|p| p.path == path).collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

/// How many rows a path has waiting, for the badge on the file page.
pub fn count_for(queue: &Queue, path: &str) -> usize {
    rows_for(queue, path).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_filtered_by_path() {
        let mut q = Queue::new();
        q.insert(
            "a".to_string(),
            Pending {
                id: "a".to_string(),
                path: "src/lib.rs".to_string(),
                severity: "minor".to_string(),
            },
        );
        assert_eq!(count_for(&q, "src/lib.rs"), 1);
        assert_eq!(count_for(&q, "src/other.rs"), 0);
    }
}
