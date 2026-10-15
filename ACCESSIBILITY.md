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
| Last reviewed | 2026-10-15 |

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
- Crumbs are a `<nav>` whose home link is named "home"; every file line is `<span id="L{n}">`
  with a link to itself, so a line can be linked to.
- Annotation, machine-annotation and proposal marks differ by shape (●, ○, ◇), not by color alone.
- A control whose click may call a model names its cost line with `aria-describedby`; the
  progress note shows inside a polite live region (`role="status"`) that is in the markup before
  the note appears, and never carries `aria-busy`. A new annotation card takes focus after it is
  created (`tabindex="-1"`).
- Both color schemes, through `@media (prefers-color-scheme: …)`, never `light-dark()`. Syntax
  colors below the configured floor (default 4.5:1) are lifted to the theme's own foreground.

## How it is tested

- `src/lib.rs`: the style tests (both scheme blocks present, no theme rule outside one, the floor
  pass complete on both, a raised floor in `a11y.toml` reaching the sheet); `src/watch.rs`: an
  edited `a11y.toml` reaching the next read. `src/layout.rs`: every emitted `browse-*` class has a
  rule, and the palette's four foreground colors per scheme clear 4.5:1 on an assumed white and
  `#0d1117` ground. `src/progress.rs`: the live regions and `aria-describedby`; `src/annotate.rs`
  and `src/pr.rs`: the new card focused. All ran on main at `528192f` on 2026-10-15 and passed,
  with and without feature `migrate`; CI runs them on every pull request.
- Once, by hand, on 2026-10-15 (not a gate): axe-core 4.10.3 in Chrome 153.0.8010.54 over
  `cargo run --example page-preview`, light and dark, with the page ground set to white and
  `#0d1117`. Its failures are under "Known limitations".
- **No screen reader, magnifier or other assistive technology has been used**, and gonk's real
  pages have not been checked by any tool.

## Known limitations

From that axe run and from reading the source on 2026-10-15:

- **Text dimmed by `opacity` fails contrast.** Line numbers (`.browse-ln`, opacity .4) measure
  2.24:1 on white and 2.5:1 on the dark theme's ground, on every line of every file; sizes, model,
  path, rater and decision labels measure 2.2 to 4.2:1. The palette test checks colors, not
  opacity, so it does not see these (SC 1.4.3).
- **Syntax colors on an annotated or proposed line's tint** fall to 3.3 to 3.8:1 in dark mode: the
  floor pass measures against the theme's ground, not the tint this sheet paints behind a marked
  line (SC 1.4.3).
- **Links inside text** (`.browse-annotation-line`, `.browse-proposal-line`) are told from the
  text around them by color alone until hover, at under 3:1 against it (SC 1.4.1).
- **Every line number is a link**, so a keyboard user tabs once per line through a whole file.
- The marks' accessible names are their glyphs; the note is only in a `title` tooltip.
- The annotation form's quote and note fields, and the findings forms' text areas, are labeled by
  placeholder only (SC 3.3.2).
- Several `<nav class="browse-actions">` and the crumbs carry no name, so landmarks repeat
  (axe `landmark-unique`, best practice).
- Rules the floor pass cannot repair remain in some themes, the default dark one included (see
  `ikigai-a11y`'s ACCESSIBILITY.md).

## Change history

| Date | Change |
|---|---|
| 2026-08-09 | Class-based highlighting and `urn:repo:style`, light and dark ([PR 14](https://github.com/ikigai-rs/ikigai-browse/pull/14)). |
| 2026-08-15 | Each theme in its own scheme block; the floor pass from `ikigai-a11y` ([PR 15](https://github.com/ikigai-rs/ikigai-browse/pull/15), [PR 17](https://github.com/ikigai-rs/ikigai-browse/pull/17)). |
| 2026-10-05 | Cost lines and live progress notes ([PR 78](https://github.com/ikigai-rs/ikigai-browse/pull/78)). |
| 2026-10-15 | This file (this pull request). |

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
