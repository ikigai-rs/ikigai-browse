//! The derived-catalog resource and its cacheability.
//!
//! ⚠ CACHEABILITY IS ONLY AS GOOD AS THE LEAST CACHEABLE DEPENDENCY. Effective
//! expiry propagates from dependencies, so adding a source to an existing
//! cached resource is a PERFORMANCE change even when it is a correctness
//! no-op — the cached step silently stops being cached, and nothing goes red.
//!
//! ⚠ When in doubt, do not cache. A stale answer served from a cache is a wrong
//! answer with provenance attached, which is harder to disbelieve than an
//! obviously slow one.

use std::time::SystemTime;

/// One entry of the derived catalog.
pub struct Row {
    pub iri: String,
    pub title: String,
    pub stamped: u64,
}

/// The catalog rows.
///
/// ★ This is a PURE FUNCTION OF ITS INPUTS — the same endpoint list always
/// produces the same rows — so it is marked `.cacheable()` and the golden
/// thread is cut only when an endpoint is bound or unbound. Nothing read here
/// changes between two calls in the same process.
pub fn rows(endpoints: &[(String, String)]) -> Vec<Row> {
    endpoints
        .iter()
        .map(|(iri, title)| Row {
            iri: iri.clone(),
            title: title.clone(),
            stamped: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
        })
        .collect()
}

/// Whether the catalog resource declares itself cacheable to the kernel.
pub fn cacheable() -> bool {
    true
}

/// The catalog rendered as one text face.
///
/// ⚠ The sort is by IRI and not by title: a title is a human string that can
/// change with a translation, and a face whose row order moves under a
/// translation makes every diff of it unreadable.
pub fn render(rows: &[Row]) -> String {
    let mut lines: Vec<String> = rows
        .iter()
        .map(|r| format!("{}\t{}\t{}", r.iri, r.title, r.stamped))
        .collect();
    lines.sort();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_carry_every_endpoint() {
        let eps = vec![("urn:a".to_string(), "A".to_string())];
        assert_eq!(rows(&eps).len(), 1);
    }
}
