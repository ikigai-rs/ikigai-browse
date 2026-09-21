//! The reviewer's grant: the scopes a headless review pass is issued before it
//! runs, and nothing wider.
//!
//! ⚠ A grant is not a suggestion. The kernel clamps to the intersection of the
//! caller's authority and this set, so a scope missing here is a denial at the
//! first sub-request rather than a degraded pass — the pass stops, and the
//! reason reaches the log with the scope named. That is deliberate: a review
//! that quietly reviews less is a false all-clear, which is worse than an
//! outage because it is trusted.
//!
//! ⚠ These scopes are also the widest authority any automated actor holds on
//! this host. Nothing here should be copied into a human's grant without
//! narrowing the root wildcard first.

/// Read authority over every mounted repository root.
pub const CAP_BROWSE_READ: &str = "urn:cap:browse:read:*";
/// Reach the model backend on loopback.
pub const CAP_NET: &str = "urn:cap:net:localhost";
/// Write the pending finding rows.
pub const CAP_FINDING_WRITE: &str = "urn:cap:finding:write";
/// Read the pending queue back, so a pass can tell a re-derive from a hit.
pub const CAP_FINDING_READ: &str = "urn:cap:finding:read";
/// Mint an annotation in the shared store.
pub const CAP_ANNOTATE: &str = "urn:cap:annotate";
/// Read the archive entries the pass keys on.
pub const CAP_ARCHIVE_READ: &str = "urn:cap:archive:read";

/// The five scopes a review pass actually needs — browse read, loopback net,
/// the two halves of the finding queue, and the archive read the key is looked
/// up through.
///
/// ★ It is written out rather than assembled from a wildcard on purpose: a
/// reader of this list should be able to say what the reviewer can do without
/// running anything.
pub fn reviewer_grant_shape() -> Vec<&'static str> {
    vec![
        CAP_BROWSE_READ,
        CAP_NET,
        CAP_FINDING_WRITE,
        CAP_FINDING_READ,
        CAP_ANNOTATE,
        CAP_ARCHIVE_READ,
    ]
}

/// Whether `held` covers every scope in the reviewer's shape.
///
/// ⚠ Exact string equality, deliberately: a prefix match here would make
/// `urn:cap:finding:` cover `urn:cap:finding:write`, and a grant checker that
/// is more generous than the kernel produces passes that pre-flight clean and
/// are denied mid-run.
pub fn covers(held: &[&str]) -> bool {
    reviewer_grant_shape()
        .iter()
        .all(|scope| held.iter().any(|h| h == scope))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_missing_one_scope_does_not_cover() {
        let held = [CAP_BROWSE_READ, CAP_NET, CAP_FINDING_WRITE];
        assert!(!covers(&held));
    }
}
