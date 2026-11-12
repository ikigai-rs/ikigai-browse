# Accessibility

What `ikigai-browse` emits that people read, the rules that output follows, how it is tested,
what is known to fail, and how to report a barrier. It is not a conformance claim. Adapted from
the [ACCESSIBILITY.md template](https://github.com/mgifford/ACCESSIBILITY.md) (MIT).

| Field | Value |
|---|---|
| Target | WCAG 2.2 Level AA for the HTML and CSS this repository writes |
| Status | Evaluation in progress. No conformance claim. |
| Owner | the maintainers of this repository |
| Report a barrier | [a GitHub issue](https://github.com/ikigai-rs/ikigai-browse/issues/new) with the `accessibility` label; anything security-sensitive through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-browse/security/advisories/new) |
| Last reviewed | 2026-11-12 |

## What is ours, and what is the host's

- **Ours:** the HTML **fragments** of the `as=text/html` faces (tree, highlighted file with line
  anchors and annotation and proposal marks, annotation cards and forms, explain and review menus,
  cost lines and progress notes, pull-request pages, the findings queue), and two stylesheets:
  `urn:repo:style` (syntax colors, from `ikigai-a11y`'s config and floor pass) and
  `urn:repo:style:layout` (the `browse-*` classes). Also the text and Turtle faces.
- **The host's:** the page around the fragments. In ikigai-gonk that is gonk's shell (`<html
  lang>`, title, header, named `<nav>`s, `<main>`, the `#browse` container, htmx and gonk's own
  stylesheet and page ground) and gonk's own pages (the ledger, the review queue's page). A host
  also decides the heading levels around ours and the ground our colors sit on.

## The rules that output follows

- Fragments only: no `<html>`, `<body>` or page-level element, and the layout sheet styles only
  `browse-*` classes, so it cannot reach a host's markup.
- Crumbs are a `<nav>` named "Breadcrumb" whose home link is named "home"; every other `<nav>`
  the fragments emit is named too ("Actions", "Finding states", "Pages", "Group kinds"), so one
  view's landmarks never repeat.
- Every file line is `<span id="L{n}">` with a link to itself, so a line can be linked to. Those
  links are out of the tab order (`tabindex="-1"`) and the `<pre>` takes focus instead
  (`tabindex="0"`), so a file is one tab stop and a keyboard can scroll a long line. The links
  stay links: a pointer clicks them and a screen reader lists them.
- Annotation, machine-annotation and proposal marks differ by shape (●, ○, ◇), not by color alone,
  and each is named in words, its kind and its note (`aria-label="annotation: …"`), not by its
  glyph. The marks and the `L{n}` links in a card sit inside running text, so they are underlined.
- Every text field has a visible `<label>` (`browse-field`), never a placeholder alone.
- Secondary text (sizes, models, paths, raters, decisions, notes, declined and orphaned cards) is
  `--browse-muted`, never `opacity`: ikigai-ui's `--muted` token when the host defines one, so it
  follows the host's theme, else the same values (`#545d68` light, `#9ea8b3` dark). A test refuses
  any `opacity` in the layout sheet, since no contrast test can measure it.
- Explain buttons, including each tree row's compact `?`, have a minimum 24 × 24 CSS pixel
  target after the link-like button reset (SC 2.5.8), independent of the host's button defaults.
- A control whose click may call a model names its cost line with `aria-describedby`; the
  progress note shows inside a polite live region (`role="status"`) that is in the markup before
  the note appears, and never carries `aria-busy`. A new annotation card takes focus after it is
  created (`tabindex="-1"`). Until the file's view replaces it, the short "annotated" note in its
  place takes focus the same way and carries no live role either.
- A finding decision (publish, decline, a revision, a retraction, the reproduced mark) answers a
  page, not a bare IRI: every decision form asks for `as=text/html`, and the answer is the
  outcome in words ("Your answer is recorded: declined."), a button back to the file's pending
  findings, and the decided card. The answer line takes focus after the swap (`tabindex="-1"`,
  `autofocus`), since the pressed button went with its form; it carries no live role, leaving
  announcement to the host's one status region. Text from the record is escaped. A refusal stays
  a 4xx, so the form and what was typed stay put and the host shows why.
- Both color schemes, through `@media (prefers-color-scheme: …)`, never `light-dark()`. Syntax
  colors below the configured floor (default 4.5:1) are lifted to the theme's own foreground.
  The floor is also kept on the grounds this crate paints under code: an annotated, targeted or
  proposed line's tint (`--browse-mark`, `--browse-propose`, read from the layout sheet) composited
  over the theme's ground. A color that misses it there is lifted on that line only. The gutter's
  line numbers are the faintest blend of the theme's foreground toward its ground that clears the
  floor on the ground and on both tints.

## How it is tested

Every named check has a result: Pass, Fail or Blocked, with its evidence. A Pass covers only the
stated check, not overall accessibility. Every Pass below was run on main at `6f5e8f2` on
2026-11-12 on macOS (arm64), with rustc and cargo 1.99.1; the `cargo test` rows ran with and
without feature `migrate`, and CI runs them on every pull request on Linux and macOS
(`.github/workflows/ci.yml`). The unit tests read the emitted markup and CSS as text and compute
contrast ratios; they render nothing. A historical run CI does not repeat is Blocked with its
date, kept as context, not as current verification.

| Check | Result | Evidence |
|---|---|---|
| Style sheet: both scheme blocks, no theme rule outside one, floor pass complete, every syntax color and the gutter clearing the floor on the line tints, a raised `a11y.toml` floor reaching the sheet | Pass | `cargo test`: `src/lib.rs` (`the_style_face_serves_both_scheme_blocks_and_is_cacheable`, `no_theme_rule_sits_outside_a_scheme_block`, `the_floor_pass_is_complete_on_both_blocks`, `every_syntax_color_clears_the_floor_on_the_line_tints`, `a_raised_contrast_floor_reaches_the_stylesheet`) and `src/watch.rs` (`an_edited_a11y_toml_reaches_the_next_read_through_the_watcher`). Ratios are computed against the theme's own ground and this sheet's tints, not a host's. |
| Layout sheet: every emitted `browse-*` class styled, no selector outside browse's markup, palette at 4.5:1 per scheme, no `opacity`, tints parse | Pass | `cargo test`: `src/layout.rs` (`every_emitted_class_is_styled`, `no_selector_reaches_outside_the_browse_markup`, `the_palette_clears_the_contrast_floor`, `no_rule_dims_text_with_opacity`, `the_line_tints_are_readable_colors_in_both_schemes`). The palette is measured on an assumed white and `#0d1117` ground; a host's real ground is not. |
| One tab stop per file: gutter links `tabindex="-1"`, the `<pre>` `tabindex="0"` | Pass | `cargo test`: `src/lib.rs` `file_html_face_is_line_anchored`. Checks the markup; no browser tabbed through it. |
| Marks named in words; the note field's visible label | Pass | `cargo test`: `src/annotate.rs` `anchored_markers_appear_at_their_lines_and_orphans_keep_their_card`. |
| Cost line named by `aria-describedby`; the polite live region in the markup before its note | Pass | `cargo test`: `src/progress.rs` (`a_control_names_its_region_and_its_cost_line`, `the_region_is_a_polite_live_region_even_when_silent`). Whether a screen reader announces it is the Screen reader row. |
| New annotation card focused; the annotate acknowledgement focused with no live role | Pass | `cargo test`: `src/annotate.rs` `annotating_from_the_file_view_returns_to_that_view_with_the_new_card_focused`, `src/pr.rs` `annotating_a_pr_returns_to_the_pr_page_with_the_new_card_focused`. Checks `tabindex` and `autofocus` in the markup; that focus moves in a browser under htmx is not tested. |
| Finding decisions answer HTML with the answer line focused; hostile text escaped | Pass | `cargo test`: `src/finding.rs` (`every_decision_form_answers_a_page_not_an_iri`, `the_html_acknowledgement_escapes_and_the_plain_one_is_the_iri`). |
| Explain controls at least 24 × 24 CSS pixels (SC 2.5.8) | Pass | `tests/browser/explain-targets.py` (a manual regression, not a CI gate), Python 3.15.2, Playwright 1.58.0 driving Chrome for Testing 156.0.8078.4 (`--chromium`), axe-core 4.10.3 (`target-size` rule only), over `cargo run --example page-preview`: 17 controls, 0 undersized, 0 violations, 0 incomplete in all 12 combinations (gonk-default fixture and unstyled host, light and dark, 12/16/20 px root font, 320 px viewport). Covers explain-control size only. |
| Explain controls with the captured full gonk stylesheet (`--host-css`) | Blocked | Last run 2026-11-02 ([PR 141](https://github.com/ikigai-rs/ikigai-browse/pull/141)), all 12 combinations passed. The captured stylesheet is not in this repository and was not re-run. |
| Automated checker (axe, WCAG 2.x A and AA plus best practice) over the whole preview, light and dark, white and `#0d1117` grounds | Blocked | Not re-run for this update; no repeatable script for it is in this repository. Last runs: 2026-10-15 (axe-core 4.10.3, Chrome 153.0.8010.54) and 2026-11-02 ([PR 140](https://github.com/ikigai-rs/ikigai-browse/pull/140), axe-core 4.13.0, Playwright 1.64.0's headless Chromium). After the 2026-11-02 fix: no color-contrast or link-in-text-block failure in either scheme, 106 tab stops (2 in one file's `<pre>`, down from 2,224 and 708), no repeated landmark within one view; what remained was the preview page's own shell, which a host owns. |
| Fragments inside a live, signed-in gonk page | Blocked | Never audited: the live tree requires sign-in. The 2026-11-02 target-size run placed the preview tree in gonk's captured shell, a composed reproduction, not a live-page audit. |
| Manual keyboard | Blocked | No person has done a keyboard pass; no interactive browser session was available for this update. |
| Text and Turtle faces | Blocked | No check of these faces for people (for example a screen reader reading the text face) has been defined or run. |
| Screen reader | Blocked | No screen reader has been used on this output; no screen-reader session was available, and no screen-reader job runs in CI. |
| Screen magnifier | Blocked | No screen magnifier has been used on this output; no magnifier session was available. |
| Speech input | Blocked | No speech-input software has been used on this output; no speech-input session was available. |
| Switch access | Blocked | No switch-access software has been used on this output; no switch-access session was available. |
| Braille display | Blocked | No braille display has been used on this output; none was available. |

To repeat the explain-target regression (Python 3.12 or later, Playwright 1.58.0, axe-core
4.10.3):

```sh
python3 -m venv target/browser-venv
target/browser-venv/bin/python -m pip install playwright==1.58.0
target/browser-venv/bin/python -m playwright install --with-deps chromium
cargo run --example page-preview -- target/browse-preview.html
curl --fail --location https://cdnjs.cloudflare.com/ajax/libs/axe-core/4.10.3/axe.min.js --output target/axe.min.js
target/browser-venv/bin/python tests/browser/explain-targets.py target/browse-preview.html target/axe.min.js
```

Optional `--chromium PATH` uses an installed browser; `--host-css PATH` substitutes a full host
stylesheet for the small gonk-default fixture.

**ikigai-ui's per-kind criteria.** This repository does not depend on `ikigai-ui` and relies on
none of its per-kind accessibility criteria (`urn:iki:ui:criterion:kind:{kind}:target:{target}`):
its fragments are not ikigai-ui kinds, so its tests verify none of their SEE checks, and their
HEAR checks would be Blocked in any case (no screen-reader job runs yet). The only thing it takes
from ikigai-ui is the `--muted` token, read when the host defines it.

## Known limitations

From the axe runs above and from reading the source on 2026-10-15:

- Rules the floor pass cannot repair remain in some themes, the default dark one included:
  `.hl-meta.hl-separator` (4.0:1) and `.hl-invalid.hl-illegal` (3.2:1) on base16-ocean.dark, which
  repaint their own ground (see `ikigai-a11y`'s ACCESSIBILITY.md).
- The tint floor is measured against this sheet's own `--browse-mark` and `--browse-propose`. A
  door that overrides them is measured by nobody.
- With the gutter links out of the tab order, a host feature that acts on a gutter click (gonk
  fills the annotate quote from the clicked line) is not reachable by Tab; the line's URL
  (`#L{n}`) and a screen reader's links list still reach it.
- Two views stacked on one page (the preview does this; gonk shows one at a time) repeat their
  landmarks' names.
- The 24 × 24 explain-control target is checked only by the manual browser regression; no
  `cargo test` gate measures it.
- Nothing here has been tried with a screen reader, magnifier, speech input, switch access or a
  braille display.

## Change history

| Date | Change |
|---|---|
| 2026-08-09 | Class-based highlighting and `urn:repo:style`, light and dark ([PR 14](https://github.com/ikigai-rs/ikigai-browse/pull/14)). |
| 2026-08-15 | Each theme in its own scheme block; the floor pass from `ikigai-a11y` ([PR 15](https://github.com/ikigai-rs/ikigai-browse/pull/15), [PR 17](https://github.com/ikigai-rs/ikigai-browse/pull/17)). |
| 2026-10-05 | Cost lines and live progress notes ([PR 78](https://github.com/ikigai-rs/ikigai-browse/pull/78)). |
| 2026-10-15 | This file ([PR 111](https://github.com/ikigai-rs/ikigai-browse/pull/111)). |
| 2026-11-01 | Finding decisions answer an HTML page with the answer focused, not a bare IRI (0.20.5, [PR 136](https://github.com/ikigai-rs/ikigai-browse/pull/136)). |
| 2026-11-02 | Explain controls keep a 24 × 24 CSS pixel minimum after the layout reset (0.20.7, [PR 141](https://github.com/ikigai-rs/ikigai-browse/pull/141)). |
| 2026-11-02 | Muted text by color, not opacity; the floor kept on line tints; one tab stop per file; marks named in words; in-text links underlined; fields labeled; navs named (0.20.6, [PR 140](https://github.com/ikigai-rs/ikigai-browse/pull/140)). |
| 2026-11-12 | "How it is tested" as a Pass / Fail / Blocked table, each Pass re-run on main at `6f5e8f2`. |

## Contributing

Read this file before changing any face's markup or either stylesheet. Keep meaning in words or
shapes, not color; give a new control a visible label; check a new color in both schemes, and with
any `opacity` applied. In the pull request, give each check a row in a Check | Result | Evidence
table: Pass, Fail or Blocked, with the test or command, the tool and its version, or why it
could not run. Keep an unrun check as Blocked, a screen reader nobody ran included; never omit it
or call it "not applicable".

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-browse/issues/new) and give it the
`accessibility` label (if GitHub does not let you set labels here, put "accessibility" in the
title and a maintainer will add it): what you were doing, which page, what happened, and your
browser and any assistive technology, if you are comfortable sharing them. Not sure whether it is
this repository's or the host's? Report it here. Anything security-sensitive goes through
[private vulnerability reporting](https://github.com/ikigai-rs/ikigai-browse/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.
