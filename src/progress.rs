//! What a model-backed control will cost BEFORE the click, and what it is
//! doing DURING the wait — the markup both halves share.
//!
//! ## Why this exists
//!
//! Measured through gonk, 2026-10-05: a file view answered in 0.18 s, then its
//! `explain` took **17.4 s** because nothing was archived for that version and
//! the model wrote one live; the next read came from the archive in 0.07 s.
//! Nothing on the page said which of the two a click would be, or that anything
//! was happening during the seventeen seconds, so a first explanation read as a
//! hang.
//!
//! ## The two halves
//!
//! - **The cost line, before the click.** Each status resource
//!   (`urn:repo:{repo}:explain-status[:{path}]`, `urn:repo:{repo}:review-status:{path}`)
//!   answers what a plain click on its control would do right now: open an
//!   archived entry (no model call) or derive one (a model call, named). The
//!   face fetches it LAZILY (`hx-trigger="load"`), so the file and tree faces
//!   stay free of the archive and of the model inventory, exactly as the option
//!   menus are. The control points at the line with `aria-describedby`, so a
//!   screen reader hears the cost with the control's name.
//! - **The progress note, during the wait.** A polite live region
//!   (`role="status"`) that is ALWAYS rendered and holds a note hidden by the
//!   layout sheet. The control names the region in `hx-indicator`; htmx adds
//!   `htmx-request` to it for the life of the request, and the sheet shows the
//!   note while that class is present. The region existing before the note
//!   appears is what makes the appearance an announcement.
//!
//! ## What this cannot do, and why
//!
//! ⚠ **No `aria-busy`.** htmx 2 never sets it, browse ships no script, and gonk
//! runs htmx with `allowEval:false`, so `hx-on` is off as well. A static
//! `aria-busy="true"` on the live region would be worse than none: assistive
//! technology holds a busy region's announcements until it clears, which would
//! mute exactly the note this module exists to announce. Setting `aria-busy`
//! on the swap target for the life of a request is a door's one-line script
//! (gonk.js already listens to `htmx:beforeRequest`).
//!
//! ⚠ **No number.** Nothing in this crate records how long a derivation took,
//! so the notes say "may take a while" and never a figure: an estimate written
//! into code is a promise the next model swap breaks silently.
//!
//! ⚠ **The layout sheet hides the note, not htmx.** gonk turns htmx's own
//! indicator styles off (`includeIndicatorStyles:false`), so the `.htmx-indicator`
//! convention would be inert there. The rule is `.browse-busy.htmx-request
//! .browse-busy-note`, in [`crate::layout`], which every door serving these faces
//! already links.

use crate::esc;

/// The id of a family's progress region on a page — one per page and family,
/// because a page carries one header control per family.
pub(crate) fn busy_id(kind: &str) -> String {
    format!("browse-{kind}-busy")
}

/// The id of a family's cost line on a page, which its control's
/// `aria-describedby` names.
pub(crate) fn cost_id(kind: &str) -> String {
    format!("browse-{kind}-cost")
}

/// The attributes a header control carries: the progress region htmx marks
/// while the request is in flight, and the cost line that describes it.
pub(crate) fn control_attrs(kind: &str) -> String {
    format!(
        " hx-indicator=\"#{busy}\" aria-describedby=\"{cost}\"",
        busy = busy_id(kind),
        cost = cost_id(kind),
    )
}

/// A polite live region: always rendered, so a note appearing inside it is
/// announced. With `note = None` the region is present and stays silent, which
/// is what an archive hit wants (it answers in milliseconds, and a note that
/// flashes and is announced for an instant answer is noise).
pub(crate) fn busy_region(id: &str, note: Option<&str>) -> String {
    let note = note
        .map(|n| format!("<span class=\"browse-busy-note\">{}</span>", esc(n)))
        .unwrap_or_default();
    format!("<p class=\"browse-busy\" id=\"{id}\" role=\"status\" aria-live=\"polite\">{note}</p>")
}

/// The slot a face renders where a family's status goes: an empty cost line
/// and a progress region carrying a GENERIC note, both replaced on load by the
/// status resource's html face ([`status_html`]). The generic note is what a
/// click shows if it lands before the status does, or if the status fetch
/// fails: true in every case, specific in none.
pub(crate) fn status_slot(kind: &str, status_iri: &str, generic_note: &str) -> String {
    format!(
        "<div class=\"browse-status\" hx-get=\"/k/source {iri} as=text/html\" \
         hx-trigger=\"load\" hx-swap=\"outerHTML\">\
         <p class=\"browse-cost\" id=\"{cost}\"></p>{busy}</div>",
        iri = esc(status_iri),
        cost = cost_id(kind),
        busy = busy_region(&busy_id(kind), Some(generic_note)),
    )
}

/// What a status resource's html face answers, and what the PR page renders
/// inline: the cost line, then the progress region with the note a click will
/// show (`None` when the click answers from the archive or needs no model).
pub(crate) fn status_html(kind: &str, cost: &str, note: Option<&str>) -> String {
    format!(
        "<div class=\"browse-status\"><p class=\"browse-cost\" id=\"{id}\">{cost}</p>{busy}</div>",
        id = cost_id(kind),
        cost = esc(cost),
        busy = busy_region(&busy_id(kind), note),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_control_names_its_region_and_its_cost_line() {
        assert_eq!(
            control_attrs("explain"),
            " hx-indicator=\"#browse-explain-busy\" aria-describedby=\"browse-explain-cost\""
        );
    }

    #[test]
    fn the_region_is_a_polite_live_region_even_when_silent() {
        assert_eq!(
            busy_region("x", None),
            "<p class=\"browse-busy\" id=\"x\" role=\"status\" aria-live=\"polite\"></p>"
        );
        let loud = busy_region("x", Some("a < b"));
        assert!(
            loud.contains("<span class=\"browse-busy-note\">a &lt; b</span>"),
            "{loud}"
        );
        // ⚠ Never aria-busy: a busy live region holds its announcements.
        assert!(!loud.contains("aria-busy"), "{loud}");
    }

    #[test]
    fn the_slot_loads_its_status_in_place_and_carries_a_generic_note() {
        let slot = status_slot("review", "urn:repo:demo:review-status:a&b.rs", "Working…");
        assert!(
            slot.contains(
                "hx-get=\"/k/source urn:repo:demo:review-status:a&amp;b.rs as=text/html\" \
                 hx-trigger=\"load\" hx-swap=\"outerHTML\""
            ),
            "{slot}"
        );
        assert!(slot.contains("id=\"browse-review-cost\""), "{slot}");
        assert!(slot.contains("id=\"browse-review-busy\""), "{slot}");
        assert!(slot.contains("Working…"), "{slot}");
    }
}
