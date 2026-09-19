//! `urn:repo:style:layout` — the **layout stylesheet** for the `browse-*`
//! classes this crate's HTML faces emit.
//!
//! ## Why it is a resource and not a host's business
//!
//! Every face here emits markup (`browse-crumbs`, `browse-entries`,
//! `browse-review-menu`, `browse-annotate`, …) and, until this resource
//! existed, defined none of it. The rules lived as a `const &str` inside one
//! particular server's binary (`ikigai-web/src/serve.rs`), which made the
//! family's premise false:
//! *browse emits the affordance, any host with a `/k/` route serves it* — but
//! the affordance arrived unstyled everywhere except that one binary, and that
//! binary is scheduled for retirement. A tree page on a second door rendered as
//! a cascade of unstyled blobs, which is exactly what happened the hour gonk's
//! browse door went live (ledger #441).
//!
//! The shape was already in the crate: [`crate::STYLE_IRI`] proves a stylesheet
//! is a legitimate resource here. This is its sibling.
//!
//! ## Sibling, not an extension of `urn:repo:style`
//!
//! They answer different questions and change on different clocks.
//! `urn:repo:style` is the **syntax theme**: generated per read from syntect's
//! theme set, layered `a11y.toml` configuration, a contrast-floor repair pass,
//! and a golden thread per candidate config file. This sheet is **page
//! furniture**: a build constant with no configuration, no derivation and
//! nothing to cut. Folding them into one IRI would have made every existing
//! consumer of `urn:repo:style` — including `ikigai-dev-server`, which pins an
//! older browse — start receiving layout rules it never asked for, and would
//! have put a static sheet behind a config-threaded one. A host that wants only
//! highlighting (it renders its own chrome around a `<pre>`) can still take only
//! that; a host serving the browse faces links both.
//!
//! ## What it deliberately does NOT style
//!
//! ★ **Only what this crate emits.** No bare `body`, `button`, `pre`, `code`,
//! `ul` or `h*` rules — `ikigai-web`'s const string had them because it *was*
//! the page, and importing them here would have this crate restyling the door's
//! own chrome. A door hosting browse's faces inside its own layout (gonk's
//! header, nav and ledger chips) must be able to link this sheet without it
//! reaching outside the fragments browse produced. Element selectors appear only
//! below a `browse-*` class.
//!
//! The one door-level footprint is the `--browse-*` custom properties on
//! `:root`. Custom properties paint nothing on their own; they exist so the
//! palette is overridable by a door that wants its own, and they are namespaced
//! so they cannot collide with the door's.
//!
//! ## Light and dark: media queries, not `light-dark()`
//!
//! `light-dark()` resolves to its LIGHT argument unless `color-scheme` is set
//! on the element or an ancestor — and `color-scheme` is the door's declaration
//! to make, not ours. A sheet that assumed it would paint dark-theme link colors
//! on a light page for any door that had not opted in. The palette is therefore
//! a top-level block plus a `@media (prefers-color-scheme: dark)` override,
//! which is also how [`crate::STYLE_IRI`] confines its themes.
//!
//! ## Posture: the door states the FACT, this sheet decides the presentation
//!
//! ⚠ `ikigai-web` computed one rule per request from the caller's posture —
//! `Posture::ReadOnly => ".browse-annotate{display:none}"` — so a read-only
//! caller was not offered a create form the annotation Sink would refuse. That
//! is behaviour, not decoration, and a static sheet cannot compute it.
//!
//! It is not dropped, and it does not stay a CSS string in every door either.
//! The sheet carries the rule keyed on an attribute the door sets:
//!
//! ```html
//! <html data-browse-posture="read-only">
//! ```
//!
//! The selector is unanchored (`[data-browse-posture="read-only"] .browse-annotate`),
//! so the attribute may sit on `<html>`, `<body>` or the element the faces are
//! swapped into — whichever the door can reach. The door now states a fact it
//! knows (this caller is read-only) instead of a class name it should not have
//! to know, and the presentation rule lives beside the markup it hides.
//!
//! ⚠ **A door that sets nothing shows the form**, which is the pre-existing
//! behaviour on every door but `ikigai-web` and is SAFE: `urn:iki:annotation`'s
//! Sink is capability-gated regardless (`urn:cap:annotate`), so the worst case
//! is a visible button whose submission is refused — rude, not permissive. The
//! hiding was only ever honesty of presentation; it has never been the
//! boundary.

use ikigai_core::{Bindings, Description, FnEndpoint, Grammar, Invocation, Iri, Verb};

use crate::{repr_utf8, CAP_WILDCARD};

/// The layout stylesheet resource — one concrete, root-independent row, beside
/// [`crate::STYLE_IRI`]. It cannot collide with a root named `style`: a root's
/// rows always carry one of this crate's family segments
/// (`urn:repo:style:tree`, `urn:repo:style:file:{path}`, …), and `layout` is
/// not one of them.
pub const LAYOUT_IRI: &str = "urn:repo:style:layout";

/// The layout rules for every `browse-*` class the HTML faces emit.
///
/// ⚠ **Every class the faces emit appears here** — pinned by
/// `every_emitted_class_is_styled`, which reads the faces' own source. A class
/// with no rule is invisible in exactly the way this whole resource exists to
/// fix, and the gap is silent: the markup is well-formed, the page just reads
/// as a blob.
pub(crate) const LAYOUT_CSS: &str = "\
/* ikigai-browse layout — the browse-* classes the HTML faces emit.\n\
   Palette only; a door may override any of these. Nothing here paints. */\n\
:root{\
--browse-link:#0b57d0;\
--browse-rule:#d0d7de;\
--browse-flag:#8a6100;\
--browse-machine:#6639ba;\
--browse-mark:rgba(255,212,0,.22)\
}\n\
@media (prefers-color-scheme:dark){:root{\
--browse-link:#8ab4f8;\
--browse-rule:#3d444d;\
--browse-flag:#d4a72c;\
--browse-machine:#b78af8;\
--browse-mark:rgba(255,212,0,.14)\
}}\n\
\
/* Navigation is buttons (every move is an hx-get), so the buttons must read as\n\
   links. Scoped to this crate's classes: a door's own buttons are its own.\n\
   The reset is deliberately WIDE (min-height, border-radius) because a door's\n\
   bare `button {}` rule reaches these too and is only ONE specificity point\n\
   below: gonk's is `min-height:2.5rem; border-radius:.375rem; background:accent`,\n\
   which turned every tree entry into a chip. Anything that rule sets and this\n\
   one does not, a door still decides. */\n\
.browse-home-link,\
.browse-crumb,\
.browse-dir,\
.browse-file,\
.browse-link,\
.browse-view-link,\
.browse-explain-link,\
.browse-review-link,\
.browse-prs-link,\
.browse-pr,\
.browse-annotation-line{\
background:none;border:0;border-radius:0;padding:0;margin:0;min-height:0;\
font:inherit;text-align:left;color:var(--browse-link);cursor:pointer;\
text-decoration:none\
}\n\
.browse-home-link:hover,\
.browse-crumb:hover,\
.browse-dir:hover,\
.browse-file:hover,\
.browse-link:hover,\
.browse-view-link:hover,\
.browse-explain-link:hover,\
.browse-review-link:hover,\
.browse-prs-link:hover,\
.browse-pr:hover,\
.browse-annotation-line:hover{text-decoration:underline}\n\
\
/* Crumbs. */\n\
.browse-crumbs{display:flex;flex-wrap:wrap;align-items:baseline;gap:.1rem;\
margin-bottom:.75rem}\n\
.browse-here{font-weight:600}\n\
.browse-sep{opacity:.5;margin:0 .15rem}\n\
\
/* The header action strip, and the disclosure menus under it. */\n\
.browse-actions{display:flex;flex-wrap:wrap;gap:.9rem;margin-bottom:.75rem}\n\
.browse-explain-menu,.browse-review-menu{margin:0 0 .75rem}\n\
.browse-explain-menu>summary,.browse-review-menu>summary{\
cursor:pointer;color:var(--browse-link);width:fit-content}\n\
.browse-explain-menu-panel,.browse-review-menu-panel{\
border:1px solid var(--browse-rule);border-radius:6px;padding:.5rem .75rem;\
margin-top:.4rem}\n\
.browse-explain-menu-heading,.browse-review-menu-heading{\
margin:0 0 .4rem;font-weight:600}\n\
.browse-explain-menu-empty,.browse-review-menu-empty{margin:.2rem 0;opacity:.7}\n\
.browse-explain-menu-note,.browse-review-menu-note{margin:.5rem 0 0}\n\
.browse-explain-menu-body,.browse-review-menu-body{display:block}\n\
.browse-explain-choices,.browse-review-choices,.browse-explain-archived{margin:0}\n\
.browse-explain-inert,.browse-review-inert{opacity:.6;cursor:not-allowed}\n\
\
/* Tree entries. */\n\
.browse-entries{list-style:none;padding-left:0;margin:0}\n\
.browse-entries li{padding:.1rem 0}\n\
.browse-dir{font-weight:600}\n\
/* browse-size is the generic muted detail, not only a byte count: a menu
   heading nests one, so it states its weight rather than inheriting 600. */\n\
.browse-size{opacity:.6;font-size:.85em;font-weight:400;margin-left:.5rem}\n\
\
/* The explanation face: prose, then its provenance line. */\n\
.browse-explain p{margin:.6rem 0;max-width:46rem}\n\
.browse-provenance{margin-top:1.25rem;opacity:.6;font-size:.85em}\n\
\
/* The file face. `.browse-line` stays INLINE: LinesWithEndings keeps each\n\
   line's own newline inside the span, so a block would double-space the file. */\n\
.browse-code{padding:.6rem .8rem;border-radius:6px;overflow-x:auto;\
font:13px/1.45 ui-monospace,SFMono-Regular,Menlo,monospace}\n\
.browse-code code{font:inherit}\n\
.browse-ln{display:inline-block;width:4ch;margin-right:1ch;text-align:right;\
color:inherit;opacity:.4;text-decoration:none;user-select:none;\
-webkit-user-select:none}\n\
.browse-ln:hover{opacity:.8}\n\
.browse-line-annotated,.browse-line:target{background:var(--browse-mark)}\n\
.browse-annotation-marker{margin-right:.35ch;opacity:.8;color:inherit;\
text-decoration:none}\n\
.browse-binary{opacity:.7;font-style:italic}\n\
\
/* Annotations: cards in reading order, then the create form. */\n\
.browse-annotations{display:grid;gap:.75rem;margin-top:1.25rem}\n\
.browse-annotation{border-left:3px solid var(--browse-rule);padding-left:.6rem}\n\
.browse-annotation-machine{border-left-color:var(--browse-machine)}\n\
.browse-annotation-orphaned{opacity:.75}\n\
.browse-annotation-model,.browse-annotation-path{opacity:.65;font-size:.85em}\n\
.browse-annotation-quote{margin:.15rem 0;padding-left:.5rem;\
border-left:2px solid var(--browse-rule);font-style:italic;opacity:.85}\n\
.browse-annotation-body{margin:.25rem 0}\n\
.browse-annotation-flag{font-size:.72em;white-space:nowrap;margin-left:.35rem;\
padding:.05em .5em;border:1px solid var(--browse-flag);border-radius:999px;\
color:var(--browse-flag)}\n\
.browse-annotate{display:grid;gap:.4rem;max-width:32rem;margin:1rem 0}\n\
.browse-annotate input,.browse-annotate textarea{font:inherit;padding:.3rem}\n\
/* The submit is a real button and reads as one — only its placement is ours, so\n\
   a door that styles buttons keeps its own. */\n\
.browse-annotate button{justify-self:start}\n\
\
/* The read-only posture the DOOR states; see this module's header. The Sink is\n\
   capability-gated regardless — this is honesty, not the boundary. */\n\
[data-browse-posture=\"read-only\"] .browse-annotate{display:none}\n\
\
/* Pull requests: the listing, the lazy per-directory block, the PR page. */\n\
.browse-prs{list-style:none;padding-left:0;margin:0}\n\
.browse-prs li{padding:.15rem 0}\n\
.browse-pr-state{font-size:.72em;white-space:nowrap;padding:.05em .5em;\
border:1px solid currentColor;border-radius:999px;opacity:.7}\n\
.browse-pr-branch,.browse-pr-updated{opacity:.6;font-size:.85em}\n\
.browse-prs-empty,.browse-prs-scope{opacity:.7}\n\
.browse-pr-meta h3{margin:.25rem 0}\n\
.browse-pr-line,.browse-pr-head{margin:.1rem 0;opacity:.7;font-size:.9em}\n\
.browse-recent-prs{margin-top:1.5rem}\n\
.browse-recent-prs h4{margin:0 0 .35rem}\n\
.browse-recent-prs-loading{opacity:.6}\n\
";

/// The layout sheet's one concrete grammar row: the bare [`LAYOUT_IRI`], no
/// bindings — it is the same sheet whatever the roots are.
pub(crate) struct LayoutRow;

impl Grammar for LayoutRow {
    fn match_iri(&self, iri: &Iri) -> Option<Bindings> {
        (iri.as_str() == LAYOUT_IRI).then(Bindings::new)
    }

    fn pattern(&self) -> String {
        LAYOUT_IRI.to_string()
    }
}

/// `urn:repo:style:layout` — a build constant, so `.cacheable()` with **no**
/// golden thread. That is the one place this resource differs in kind from
/// [`crate::style_endpoint`], and it is deliberate: a thread is a promise that
/// something cuts it, and nothing can change this sheet inside a running
/// process. Cacheable rather than uncacheable because effective expiry
/// PROPAGATES — a door that composes this sheet into a cached page shell would
/// otherwise lose that page's cache to a constant.
///
/// There is no per-root residual to enforce, so the declared wildcard IS the
/// whole check, exactly as for the theme sheet.
pub(crate) fn layout_endpoint() -> FnEndpoint {
    FnEndpoint::new("browse-layout", |_inv: &Invocation<'_>| {
        Ok(repr_utf8("text/css", LAYOUT_CSS.to_string()).cacheable())
    })
    .with_description(layout_description())
}

fn layout_description() -> Description {
    Description::new("browse-layout")
        .title("Layout stylesheet")
        .summary(
            "The layout rules for the browse-* classes this crate's HTML faces emit — \
             urn:repo:style:layout, text/css. A door serving the browse faces links this \
             BESIDE urn:repo:style: that one is the syntax theme for the hl- classes inside \
             a file view, this one is the page furniture (crumbs, entry lists, the action \
             strip, the explain and review disclosure menus, annotation cards and the \
             create form, the pull-request listings). It styles only what this crate emits \
             — no bare body, button, pre or heading rules — so a host can link it inside \
             its own chrome without this sheet reaching the host's markup. Light and dark \
             via a top-level palette plus a prefers-color-scheme override, because \
             light-dark() needs a color-scheme declaration the door owns; the palette is \
             --browse-* custom properties a door may override. A door that knows the \
             caller cannot write annotations sets data-browse-posture=\"read-only\" on any \
             ancestor and the create form is hidden — presentation only, since the \
             annotation Sink is capability-gated regardless. A build constant: cacheable, \
             no configuration, no golden thread.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .output("text/css;charset=utf-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Classes the faces build by CONCATENATION rather than as a whole
    /// `class="…"` literal, so the scan below cannot see them. Each names its
    /// site; adding one here without adding a rule fails the same assertion.
    const DYNAMIC_CLASSES: &[&str] = &[
        // `lib.rs`: `class="browse-{kind}"`, kind from `Kind::label()`.
        "browse-dir",
        "browse-file",
        "browse-link",
        // `lib.rs`: the line wrapper's class is chosen before the format.
        "browse-line",
        "browse-line-annotated",
        // `annotate.rs`: appended to `browse-annotation`.
        "browse-annotation-orphaned",
        "browse-annotation-machine",
    ];

    /// Every `browse-*` class the faces emit has a rule in [`LAYOUT_CSS`].
    ///
    /// ★ This is the regression this whole resource exists to prevent, and it
    /// cannot be caught by rendering: unstyled markup is well-formed markup.
    /// The scan reads the faces' own source at COMPILE time (`include_str!`),
    /// so it needs no fixture, no filesystem and no git history — it holds in a
    /// shallow CI checkout.
    ///
    /// ⚠ It checks only that the class is MENTIONED. A rule that mentions a
    /// class and styles it wrongly is a human's job; a class mentioned nowhere
    /// is a blob on someone's page.
    #[test]
    fn every_emitted_class_is_styled() {
        let sources = [
            include_str!("lib.rs"),
            include_str!("annotate.rs"),
            include_str!("explain.rs"),
            include_str!("review.rs"),
            include_str!("pr.rs"),
        ];
        let mut emitted: BTreeSet<&str> = DYNAMIC_CLASSES.iter().copied().collect();
        for source in sources {
            // Every `class=\"…\"` literal in the emitted markup. The escaped
            // quote is what a Rust string literal spells, so this matches the
            // markup and never a doc comment's prose.
            for attribute in source.split("class=\\\"").skip(1) {
                let Some(value) = attribute.split("\\\"").next() else {
                    continue;
                };
                for class in value.split_whitespace() {
                    // `{…}`-interpolated segments belong to DYNAMIC_CLASSES.
                    let class = class.split('{').next().unwrap_or(class);
                    if class.starts_with("browse-") && class.len() > "browse-".len() {
                        emitted.insert(class);
                    }
                }
            }
        }
        // The container class itself carries no rule on purpose: it is the
        // faces' wrapper, and a door positions it.
        emitted.remove("browse");
        let unstyled: Vec<&str> = emitted
            .iter()
            .copied()
            .filter(|class| !LAYOUT_CSS.contains(&format!(".{class}")))
            .collect();
        assert!(
            unstyled.is_empty(),
            "emitted by a face and styled nowhere: {unstyled:?} — every browse-* class \
             the markup carries needs a rule in LAYOUT_CSS, or it renders as an unstyled \
             blob on every door (ledger #441)"
        );
        // The scan itself must keep working: a refactor that stopped finding
        // classes would make the assertion above vacuously true.
        assert!(
            emitted.len() > 40,
            "the class scan found only {} classes ({emitted:?}) — it has stopped seeing \
             the markup, and this test would pass vacuously",
            emitted.len()
        );
    }

    /// ★ The sheet must not reach outside the markup this crate emits. A door
    /// links it inside its OWN page (gonk's header, nav and chrome), so a bare
    /// element selector here would restyle the door.
    ///
    /// Checked structurally: every selector in the sheet either sits below a
    /// `browse-` class, is the posture attribute, or is `:root` (custom
    /// properties, which paint nothing).
    #[test]
    fn no_selector_reaches_outside_the_browse_markup() {
        // Comments carry commas and periods of their own; strip them before
        // anything tries to read a selector out of the text.
        let mut sheet = String::new();
        let mut rest = LAYOUT_CSS;
        while let Some(open) = rest.find("/*") {
            sheet.push_str(&rest[..open]);
            rest = rest[open..]
                .find("*/")
                .map_or("", |close| &rest[open + close + 2..]);
        }
        sheet.push_str(rest);
        let mut offenders: Vec<String> = Vec::new();
        for block in sheet.split('}') {
            let Some(selectors) = block.split('{').next() else {
                continue;
            };
            for selector in selectors.split(',') {
                // Strip comments and at-rules (the media block's own line).
                let selector = selector
                    .trim()
                    .trim_start_matches("@media (prefers-color-scheme:dark)")
                    .trim();
                if selector.is_empty() || selector == ":root" {
                    continue;
                }
                let anchored =
                    selector.contains(".browse-") || selector.starts_with("[data-browse-posture");
                if !anchored {
                    offenders.push(selector.to_string());
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these selectors reach outside the browse markup and would restyle the door's \
             own page: {offenders:?}"
        );
    }

    /// The posture rule survived the move out of `ikigai-web`, keyed on the
    /// attribute a door sets rather than computed per request into a `<style>`.
    #[test]
    fn the_read_only_posture_rule_is_present_and_unanchored() {
        assert!(
            LAYOUT_CSS
                .contains("[data-browse-posture=\"read-only\"] .browse-annotate{display:none}"),
            "the read-only posture rule is the one behavioural rule in this sheet"
        );
        // Unanchored: no `:root[…]`/`html[…]` prefix, so the door may set the
        // attribute on whichever element it can reach.
        assert!(
            !LAYOUT_CSS.contains(":root[data-browse-posture")
                && !LAYOUT_CSS.contains("html[data-browse-posture"),
            "the posture selector must match the attribute on ANY ancestor"
        );
    }
}
