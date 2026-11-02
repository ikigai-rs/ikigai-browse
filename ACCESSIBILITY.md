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
| Last reviewed | 2026-11-02 |

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

- `src/lib.rs`: the style tests (both scheme blocks present, no theme rule outside one, the floor
  pass complete on both and its two unrepairable dark rules named, every syntax color and the
  gutter clearing the floor on the line tints, a raised floor in `a11y.toml` reaching the sheet);
  the file face's gutter links out of the tab order; `src/watch.rs`: an edited `a11y.toml`
  reaching the next read. `src/layout.rs`: every emitted `browse-*` class has a rule, the
  palette's five foreground colors per scheme clear 4.5:1 on an assumed white and `#0d1117`
  ground, no rule uses `opacity`, and the tints parse. `src/annotate.rs`: a mark's name in words
  and the note field's visible label. `src/progress.rs`: the live regions and `aria-describedby`; `src/annotate.rs`
  and `src/pr.rs`: the new card focused, and the annotate acknowledgement focused with no live
  role; `src/finding.rs`: every decision form's answer is HTML
  with the answer line focused, and hostile text in it is escaped. All ran on main at `528192f` on 2026-10-15 and passed,
  with and without feature `migrate`; CI runs them on every pull request.
- Once, by hand, on 2026-10-15 (not a gate): axe-core 4.10.3 in Chrome 153.0.8010.54 over
  `cargo run --example page-preview`, light and dark, with the page ground set to white and
  `#0d1117`.
- Again by hand on 2026-11-02 (not a gate, ledger #1653): axe-core 4.13.0 in Playwright 1.64.0's
  headless Chromium over the same preview and grounds, WCAG 2.x A and AA plus best practice.
  Before the fix: 2,131 color-contrast nodes light and 2,117 dark, 6 link-in-text-block, unnamed
  crumbs, and 2,224 tab stops on the page (708 in one file's `<pre>`). After: no color-contrast or
  link-in-text-block failure in either scheme, 106 tab stops (2 in that `<pre>`), and no repeated
  landmark within one view. What remains is the preview page's own shell (no `<main>`, content
  outside landmarks, a heading level skipped by the stacked views), which a host owns.
- **No screen reader, magnifier or other assistive technology has been used**, and gonk's real
  pages have not been checked by any tool.

## Known limitations

From that axe run and from reading the source on 2026-10-15:

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

## Change history

| Date | Change |
|---|---|
| 2026-08-09 | Class-based highlighting and `urn:repo:style`, light and dark ([PR 14](https://github.com/ikigai-rs/ikigai-browse/pull/14)). |
| 2026-08-15 | Each theme in its own scheme block; the floor pass from `ikigai-a11y` ([PR 15](https://github.com/ikigai-rs/ikigai-browse/pull/15), [PR 17](https://github.com/ikigai-rs/ikigai-browse/pull/17)). |
| 2026-10-05 | Cost lines and live progress notes ([PR 78](https://github.com/ikigai-rs/ikigai-browse/pull/78)). |
| 2026-10-15 | This file ([PR 111](https://github.com/ikigai-rs/ikigai-browse/pull/111)). |
| 2026-11-01 | Finding decisions answer an HTML page with the answer focused, not a bare IRI (0.20.5, ledger #1988). |
| 2026-11-02 | Muted text by color, not opacity; the floor kept on line tints; one tab stop per file; marks named in words; in-text links underlined; fields labeled; navs named (0.20.6, ledger #1653). |

## Contributing

Read this file before changing any face's markup or either stylesheet. Keep meaning in words or
shapes, not color; give a new control a visible label; check a new color in both schemes, and with
any `opacity` applied. Say in the pull request what was tested, with what, and what was not.

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-browse/issues/new) and give it the
`accessibility` label (if GitHub does not let you set labels here, put "accessibility" in the
title and a maintainer will add it): what you were doing, which page, what happened, and your
browser and any assistive technology, if you are comfortable sharing them. Not sure whether it is
this repository's or the host's? Report it here. Anything security-sensitive goes through
[private vulnerability reporting](https://github.com/ikigai-rs/ikigai-browse/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.
