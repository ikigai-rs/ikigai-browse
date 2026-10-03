//! The **judge**: a second, separate call per SERIOUS review finding that
//! confirms or refutes the claim with the context the reviewer did not have
//! (ledger #483, the plan Brian approved 2026-10-01).
//!
//! ## Why a second call, and why this input
//!
//! Critical sweep 1 (ledger #655) verified all 24 critical pending findings by
//! reproduction: 1 partly real, 23 not defects. The misses share a cause. The
//! reviewer judges a region of at most `max_prompt_bytes` it cannot see around,
//! so it flags a default without its data source, a "panic" a test disproves,
//! a `?` without the caller, "won't compile" on green code, an `.expect` inside
//! a test. Two prompt lines aimed at the class were measured and both failed
//! (the table on `REVIEW_PROMPT_VERSION`): what the reviewer lacks is not an
//! instruction but CONTEXT, and context is something code can hand it.
//!
//! So the judge sees, for one claim:
//!
//! * the WHOLE enclosing item (function, impl, struct, `def`), not the tile,
//!   numbered, with the quoted line marked — and its leading doc comment and
//!   attributes;
//! * the comments at the site, pulled out on their own: the block immediately
//!   above the quoted line, a trailing comment on it, the item's doc;
//! * whether the site is TEST code, and why (a `tests/` path, a `#[cfg(test)]`
//!   module, a `#[test]` function);
//! * the tests in the repository that MENTION the claim's symbols — each test's
//!   name and its asserting lines;
//! * the claim itself: its severity, its quote and its body.
//!
//! And it answers four narrow questions, each with a short reason, instead of
//! being asked to review again: does the cited code do what the claim says;
//! is the hazard already disclosed at the site; would the claimed failure
//! actually occur; is this test code where failing loudly is the intent.
//!
//! ## The verdict is a RULE over the answers, and the model's own word rides beside it
//!
//! [`verdict_of`] maps the four answers to `confirmed` / `refuted` / `unsure`
//! deterministically, so a refutation names the answer that refuted it and a
//! router can key on the answers rather than on a word. The model's own
//! `VERDICT:` line is kept as `stated` — measured beside the rule on the eval
//! set (`tests/corpus/judge/`), never used to override it.
//!
//! ## What a verdict does: NOTHING, by itself
//!
//! ★ **No finding is dropped, withheld or re-rated because of a verdict.** The
//! rule ledger #475 rejected — a review that silently withholds a finding is
//! worse than one that repeats a false one — stands. The verdict is ATTACHED to
//! the finding (`judge` on the json row, the verdict node on the Turtle face)
//! and routing on it is the host's (gonk's) decision, made in the open.
//!
//! ## Separation from the review prompt (ledger #449)
//!
//! The REVIEW prompt never learns the judge exists, the decline words, or any
//! decision: telling a model how its output is filtered is the measured way to
//! make it write past the filter. The judge is its own call, with its own system
//! prompt, its own version tag ([`JUDGE_PROMPT_VERSION`]), at temperature 0.
//!
//! ## Archived, keyed by (finding, judge tag)
//!
//! A verdict is stored once under `urn:iki:finding:{id}:judge:{tag}` and read
//! back with the finding, so a re-read never repays and a second judge (a
//! larger tier, tried as a measured choice) adds its own verdict beside the
//! first instead of replacing it.
//!
//! ## Judging a queued finding on demand (ledger #696)
//!
//! A pass judges only what it MINTS, so the queue that predates the judge has
//! no verdicts. `urn:repo:{repo}:judge-finding:{id}` judges one queued finding
//! by id and archives the verdict under the same key, so a host can backfill:
//! a re-read under the same tag is an archive hit with no call, and Exists
//! asks whether there is one without judging. It reads the version the
//! finding's pass reviewed (recovered from git history when the file has
//! moved), so a backfilled verdict means what a pass-minted one means — see
//! [`JudgeFindingEndpoint`].
//!
//! ## No judge, no judgment; and a `cannot` is a record (ledger #702)
//!
//! With the judge OFF ([`ExplainConfig::no_judge`]) and no `provider=` named,
//! judge-finding REFUSES with a typed [`Error::Conflict`] rather than judging
//! with the review tier, and Exists answers `false`. The review pass already
//! judged nothing then; neither path falls back.
//!
//! A finding judge-finding cannot judge for a LASTING reason is archived as a
//! [`Cannot`] at the verdict IRI, with its reason and the search basis it was
//! found under, and is retried only when the judge tag or that basis changes.
//! The verdict words themselves are [`VERDICTS`], one spelling, published as
//! the `one_of` of the findings listing's `verdict=` filter.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Invocation, Representation,
    Request, Result, Verb,
};
use oxigraph::model::{Literal, NamedNode, Quad, Term};

use crate::annotate::{self, PROV};
use crate::archive::Archive;
use crate::explain::{ik, iso8601, parse_iri, provider_label, resolve_model, CAP_NET};
use crate::finding::SEVERITIES;
use crate::{
    file_iri, granted, iri_encode, path_binding, repo_root, repr_utf8, resolve, ExplainConfig,
    Roots, CAP_WILDCARD,
};

/// Version of the judge prompt pair, folded into the verdict key. A prompt
/// edit bumps this; verdicts under an older tag stay on file beside the new.
///
/// **judge-v2** (ledger #483, measured on findings sweep 2's reproduced
/// labels, ledger #706; the table is in `tests/corpus/judge/README.md`):
///
/// * the RULE — `occurs: n/a` never confirms, and a stated `refuted`/`unsure`
///   beside four supporting answers is `unsure` ([`verdict_of`]);
/// * OCCURS asks for one concrete input the code can receive and the line where
///   it goes wrong — "could" and "might", or a failure that depends on code not
///   shown, are `unclear` — and counts an error the code returns or reports on
///   purpose as the code working, not a failure.
///
/// Measured and DROPPED: a 30-word cap on each reason (fewer truncated
/// answers, but more false confirmations and a reproduced real finding lost),
/// and a DISCLOSED that counts a comment JUSTIFYING the behavior as a
/// disclosure (it lost 2 of 6 verified-real findings on the dev split, the
/// failure judge-v1's narrower wording was written to prevent).
pub(crate) const JUDGE_PROMPT_VERSION: &str = "judge-v2";

/// The verdict words — each spelled ONCE, here: [`verdict_of`] returns them,
/// the archive stores them, and [`VERDICTS`] is the `one_of` a consumer reads
/// (the findings listing's `verdict=` filter, ledger #702).
pub(crate) const CONFIRMED: &str = "confirmed";
pub(crate) const REFUTED: &str = "refuted";
pub(crate) const UNSURE: &str = "unsure";

/// The closed set of verdicts, in the order a reader triages them.
///
/// ⚠ **`cannot` is not in it, on purpose.** A verdict is the judge's answer
/// about a CLAIM; [`CANNOT`] says the judge was never asked, because the
/// version the reviewer saw could not be had. It never rides on a finding's
/// `judge` / `judges`, and a filter on it would match nothing.
pub(crate) const VERDICTS: [&str; 3] = [CONFIRMED, REFUTED, UNSURE];

/// `judge-finding`'s answer when a finding cannot be judged (its `status`),
/// and the type its archived record carries — see [`JudgeFindingEndpoint`].
pub(crate) const CANNOT: &str = "cannot";

/// The four questions, by the key each answer is stored and served under.
pub(crate) const QUESTIONS: [&str; 4] = ["code", "disclosed", "occurs", "test"];

/// How many lines of an enclosing item the judge is shown before it is cut to
/// a window around the quote. Large enough for nearly every function; an item
/// longer than this is a module-sized `impl` whose middle is not the context.
const MAX_ITEM_LINES: usize = 240;
/// Lines either side of the quote when there is no item to show (prose, a
/// manifest) or the item is longer than [`MAX_ITEM_LINES`].
const WINDOW_LINES: usize = 40;
/// Tests listed, and asserting lines quoted per test.
const MAX_TESTS: usize = 8;
const MAX_ASSERTS: usize = 3;
/// What the repository walk reads at most — the test index is context, not a
/// crawl, and a pathological tree must not make a pass unbounded.
const MAX_INDEX_FILES: usize = 4000;
const MAX_INDEX_FILE_BYTES: u64 = 512 * 1024;

const JUDGE_SYSTEM_PROMPT: &str =
    "You check one claim a code reviewer made about a file. The reviewer was shown only a \
     slice of the file and could not see around it. You are shown the whole enclosing item, \
     the comments at the quoted site, whether the site is test code, and the tests in the \
     repository that mention it. Check the claim against that code: do not review the code \
     again, do not raise new problems, and do not agree with a claim because it sounds \
     plausible. Reason from what the code shown actually does. Answer every question, each on \
     its own line, in exactly the format asked for.";

// --- the site: what the judge is shown ----------------------------------------

/// The judge's view of one quoted site in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Site {
    /// 1-based lines shown, inclusive — the enclosing item with its doc and
    /// attributes, or a window around the quote.
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
    /// The quote's own lines (1-based, inclusive).
    pub(crate) quote_first: usize,
    pub(crate) quote_last: usize,
    /// The enclosing item's name, when one was found.
    pub(crate) item: Option<String>,
    /// Lines elided from the middle of a long item: `(from, to)` inclusive.
    pub(crate) elided: Option<(usize, usize)>,
    /// Why the site is test code, or `None` when it is not.
    pub(crate) test_code: Option<&'static str>,
    /// The comments at the site: the block right above the quote, a trailing
    /// comment on it, the item's doc — in file order, deduplicated.
    pub(crate) comments: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    /// `//` and `/* */` comments, braces delimit items (Rust, C, JS, Go…).
    Brace,
    /// `#` comments, braces delimit shell functions; Python indents.
    Hash,
    /// Prose and data: no items, a window is the context.
    Plain,
}

fn lang_of(rel: &str) -> Lang {
    let ext = rel.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "js" | "mjs" | "ts" | "tsx" | "jsx" | "go"
        | "java" | "kt" | "swift" | "scala" | "cs" | "el" => Lang::Brace,
        "py" | "sh" | "bash" | "zsh" | "fish" | "rb" | "pl" => Lang::Hash,
        _ => Lang::Plain,
    }
}

fn indent(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// The name of the item a line OPENS, if it opens one — a Rust/C-like/Python
/// declaration at the start of the (trimmed) line. Deliberately syntactic and
/// language-agnostic in the way the region tiling is: a parser per language is
/// not what this needs, and a miss falls back to a window, never to nothing.
pub(crate) fn item_name(line: &str) -> Option<String> {
    let mut rest = line.trim_start();
    if let Some(after) = rest.strip_prefix("pub(") {
        rest = after.split_once(')').map(|(_, r)| r.trim_start())?;
    }
    for prefix in [
        "pub ", "async ", "unsafe ", "const ", "default ", "export ", "static ",
    ] {
        while let Some(r) = rest.strip_prefix(prefix) {
            rest = r.trim_start();
        }
    }
    if let Some(r) = rest.strip_prefix("extern \"C\" ") {
        rest = r.trim_start();
    }
    let ident = |s: &str| -> Option<String> {
        let name: String = s
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '!')
            .collect();
        (!name.is_empty()).then_some(name)
    };
    for keyword in [
        "fn ",
        "struct ",
        "enum ",
        "trait ",
        "mod ",
        "union ",
        "type ",
        "macro_rules! ",
        "def ",
        "class ",
        "function ",
    ] {
        if let Some(r) = rest.strip_prefix(keyword) {
            return ident(r);
        }
    }
    if rest.starts_with("impl") && matches!(rest.as_bytes().get(4), Some(b' ' | b'<')) {
        let head = rest.split('{').next().unwrap_or(rest).trim();
        return Some(head.to_string());
    }
    // A constant or static item at column zero (`const NAME: T = …;`) — the
    // prefixes above consumed `const`/`static`, so what is left is `NAME:`.
    if line.starts_with("const ")
        || line.starts_with("pub const ")
        || line.starts_with("static ")
        || line.starts_with("pub static ")
        || line.starts_with("pub(crate) const ")
        || line.starts_with("pub(crate) static ")
    {
        let name = ident(rest)?;
        return rest[name.len()..]
            .trim_start()
            .starts_with(':')
            .then_some(name);
    }
    // A shell function: `name() {`.
    let trimmed = rest.trim_end();
    if let Some(head) = trimmed
        .strip_suffix("() {")
        .or_else(|| trimmed.strip_suffix("(){"))
    {
        if !head.is_empty()
            && head
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            return Some(head.to_string());
        }
    }
    None
}

/// Where the brace block opened on line `start` closes (0-based line index),
/// skipping braces inside strings (multi-line and raw strings included),
/// character literals and comments. `None` when it never closes; when the item
/// ends at a `;` before any `{` (a declaration with no body), that line.
fn brace_end(lines: &[&str], start: usize, lang: Lang) -> Option<usize> {
    #[derive(Clone, Copy, PartialEq)]
    enum In {
        Code,
        /// A string; `Some(n)` is a Rust raw string closed by `"` and n `#`.
        Str(Option<usize>),
        Block(u32),
    }
    let mut state = In::Code;
    let mut depth = 0i64;
    let mut opened = false;
    for (index, line) in lines.iter().enumerate().skip(start) {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            match state {
                In::Block(n) => {
                    if c == '*' && next == Some('/') {
                        state = if n <= 1 { In::Code } else { In::Block(n - 1) };
                        i += 2;
                        continue;
                    }
                    if c == '/' && next == Some('*') {
                        state = In::Block(n + 1);
                        i += 2;
                        continue;
                    }
                }
                In::Str(None) => {
                    if c == '\\' {
                        i += 2;
                        continue;
                    }
                    if c == '"' {
                        state = In::Code;
                    }
                }
                In::Str(Some(hashes)) => {
                    if c == '"' && (1..=hashes).all(|k| chars.get(i + k) == Some(&'#')) {
                        state = In::Code;
                        i += 1 + hashes;
                        continue;
                    }
                }
                In::Code => match (lang, c) {
                    (Lang::Brace, '/') if next == Some('/') => break,
                    (Lang::Brace, '/') if next == Some('*') => {
                        state = In::Block(1);
                        i += 2;
                        continue;
                    }
                    (Lang::Hash, '#') => break,
                    (_, '"') => {
                        // A Rust raw string: `r"`, `r#"`, `br##"` …
                        let mut k = i;
                        let mut hashes = 0;
                        while k > 0 && chars[k - 1] == '#' {
                            hashes += 1;
                            k -= 1;
                        }
                        let raw = lang == Lang::Brace && k > 0 && chars[k - 1] == 'r';
                        state = In::Str(raw.then_some(hashes));
                    }
                    (Lang::Brace, '\'') => {
                        // A char literal (`'{'`, `'\n'`) — not a lifetime (`'a`).
                        if next == Some('\\') {
                            i += 2;
                            while i < chars.len() && chars[i] != '\'' {
                                i += 1;
                            }
                            i += 1;
                            continue;
                        }
                        if chars.get(i + 2) == Some(&'\'') {
                            i += 3;
                            continue;
                        }
                    }
                    (_, '{') => {
                        depth += 1;
                        opened = true;
                    }
                    (_, '}') => {
                        depth -= 1;
                        if opened && depth <= 0 {
                            return Some(index);
                        }
                    }
                    (_, ';') if !opened && depth == 0 => return Some(index),
                    _ => {}
                },
            }
            i += 1;
        }
        // A `#`-comment language's string does not span lines in practice
        // (Python's triple quotes aside); a Rust string does, and is carried.
        if lang != Lang::Brace && matches!(state, In::Str(_)) {
            state = In::Code;
        }
    }
    None
}

/// Where a Python block opened on line `start` ends: the last line before the
/// next non-blank line indented no deeper than the opener.
fn indent_end(lines: &[&str], start: usize) -> usize {
    let base = indent(lines[start]);
    let mut end = start;
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if indent(line) <= base {
            break;
        }
        end = index;
    }
    end
}

fn is_comment(line: &str, lang: Lang) -> bool {
    let t = line.trim_start();
    match lang {
        Lang::Brace => {
            t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') || t.starts_with("*/")
        }
        Lang::Hash => t.starts_with('#'),
        Lang::Plain => false,
    }
}

/// A line that belongs ABOVE an item: its doc comment, an attribute, a
/// decorator.
fn is_leading(line: &str, lang: Lang) -> bool {
    let t = line.trim_start();
    is_comment(line, lang) || t.starts_with("#[") || t.starts_with("#![") || t.starts_with('@')
}

/// The trailing comment on a line, if any (`code // why`).
fn trailing_comment(line: &str, lang: Lang) -> Option<String> {
    let marker = match lang {
        Lang::Brace => "//",
        Lang::Hash => " #",
        Lang::Plain => return None,
    };
    let (code, comment) = line.split_once(marker)?;
    (!code.trim().is_empty() && !code.contains('"')).then(|| comment.trim().to_string())
}

/// The judge's view of the quote at `[byte_start, byte_end)` in `text`.
pub(crate) fn site(rel: &str, text: &str, byte_start: usize, byte_end: usize) -> Site {
    let lang = lang_of(rel);
    let lines: Vec<&str> = text.split('\n').collect();
    let quote_first = text[..byte_start].matches('\n').count();
    let quote_last = text[..byte_end.max(byte_start)].matches('\n').count();
    let anchor_indent = indent(lines[quote_first]);

    // The innermost item that encloses the quote: walk up from the quoted line
    // to the first opener at or outside the quote's indentation whose block
    // reaches it.
    let mut item: Option<(usize, usize, String)> = None;
    if lang != Lang::Plain {
        for start in (0..=quote_first).rev() {
            let line = lines[start];
            if start != quote_first && indent(line) > anchor_indent {
                continue;
            }
            let Some(name) = item_name(line) else {
                continue;
            };
            let end = match (lang, line.trim_end().ends_with(':')) {
                (Lang::Hash, true) => Some(indent_end(&lines, start)),
                _ => brace_end(&lines, start, lang),
            };
            if let Some(end) = end {
                if end >= quote_last {
                    item = Some((start, end, name));
                    break;
                }
            }
        }
    }

    let (mut first, mut last, name) = match item {
        Some((start, end, name)) => (start, end, Some(name)),
        None => (
            quote_first.saturating_sub(WINDOW_LINES),
            (quote_last + WINDOW_LINES).min(lines.len().saturating_sub(1)),
            None,
        ),
    };
    let item_start = first;
    if name.is_some() {
        // Its doc comment and attributes, which are part of what it says.
        while first > 0 && is_leading(lines[first - 1], lang) && item_start - first < 60 {
            first -= 1;
        }
    }
    // A long item is shown as its head and a window around the quote.
    let mut elided = None;
    if last - first + 1 > MAX_ITEM_LINES {
        let from = (item_start + 2).max(quote_first.saturating_sub(WINDOW_LINES * 2));
        if from > item_start + 2 {
            elided = Some((item_start + 3, from));
        }
        last = last.min(quote_last + WINDOW_LINES * 2);
    }

    // The comments at the site.
    let mut comments = Vec::new();
    let mut above = Vec::new();
    let mut index = quote_first;
    while index > 0 && is_comment(lines[index - 1], lang) && above.len() < 30 {
        index -= 1;
        above.push(lines[index].trim().to_string());
    }
    above.reverse();
    if name.is_some() {
        for line in &lines[first..item_start] {
            if is_comment(line, lang) {
                comments.push(line.trim().to_string());
            }
        }
    }
    for line in above {
        if !comments.contains(&line) {
            comments.push(line);
        }
    }
    for line in &lines[quote_first..=quote_last.min(lines.len() - 1)] {
        if let Some(c) = trailing_comment(line, lang) {
            comments.push(c);
        }
    }

    let test_code = test_reason(
        rel,
        &lines,
        lang,
        first,
        item_start,
        quote_first,
        name.as_deref(),
    );

    Site {
        first_line: first + 1,
        last_line: last + 1,
        quote_first: quote_first + 1,
        quote_last: quote_last + 1,
        item: name,
        elided: elided.map(|(a, b)| (a + 1, b)),
        test_code,
        comments,
    }
}

/// Whether a path names a test file — a `tests/` directory, a `*_test.*` or
/// `test_*.py` file, a `.test.`/`.spec.` script, a `tests.rs` module.
pub(crate) fn is_test_path(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    let file = parts.last().copied().unwrap_or("");
    parts[..parts.len().saturating_sub(1)]
        .iter()
        .any(|p| *p == "tests" || *p == "test" || *p == "testdata")
        || file == "tests.rs"
        || file.ends_with("_tests.rs")
        || file.ends_with("_test.rs")
        || file.ends_with("_test.go")
        || file.ends_with("_test.py")
        || (file.starts_with("test_") && file.ends_with(".py"))
        || file.contains(".test.")
        || file.contains(".spec.")
}

fn test_reason(
    rel: &str,
    lines: &[&str],
    lang: Lang,
    lead: usize,
    item_start: usize,
    quote: usize,
    item: Option<&str>,
) -> Option<&'static str> {
    if is_test_path(rel) {
        return Some("the file is a test file (its path)");
    }
    if item.is_some()
        && lines[lead..item_start]
            .iter()
            .any(|l| l.trim_start().starts_with("#[test]") || l.contains("::test]"))
    {
        return Some("the enclosing function is a #[test]");
    }
    if item.is_some_and(|n| n.starts_with("test_")) && lang == Lang::Hash {
        return Some("the enclosing function is a test_ function");
    }
    // Inside a `#[cfg(test)]` module: the attribute, then the `mod` it gates,
    // whose block reaches the quote.
    for (index, line) in lines.iter().enumerate().take(quote) {
        if !line.trim_start().starts_with("#[cfg(test)]") {
            continue;
        }
        let Some(module) = (index + 1..(index + 4).min(lines.len()))
            .find(|&i| item_name(lines[i]).is_some_and(|_| lines[i].contains("mod ")))
        else {
            continue;
        };
        if brace_end(lines, module, lang).is_some_and(|end| end >= quote) {
            return Some("the site is inside a #[cfg(test)] module");
        }
    }
    None
}

// --- the tests that mention the claim's symbols --------------------------------

/// The words a claim is ABOUT, for finding the tests that exercise it: the
/// enclosing item's name and the most specific identifiers in the quote.
/// Keywords, std vocabulary and short words would match every test.
pub(crate) fn symbols(quote: &str, item: Option<&str>) -> Vec<String> {
    const COMMON: &[&str] = &[
        "let",
        "mut",
        "pub",
        "crate",
        "self",
        "Self",
        "super",
        "use",
        "mod",
        "impl",
        "struct",
        "enum",
        "trait",
        "type",
        "const",
        "static",
        "async",
        "await",
        "move",
        "ref",
        "return",
        "match",
        "else",
        "for",
        "while",
        "loop",
        "break",
        "continue",
        "where",
        "true",
        "false",
        "Some",
        "None",
        "Ok",
        "Err",
        "Option",
        "Result",
        "String",
        "Vec",
        "Box",
        "Arc",
        "Rc",
        "new",
        "std",
        "str",
        "bool",
        "usize",
        "u64",
        "u32",
        "i64",
        "clone",
        "into",
        "from",
        "unwrap",
        "expect",
        "iter",
        "map",
        "collect",
        "len",
        "push",
        "get",
        "and",
        "the",
        "def",
        "class",
        "import",
        "return",
        "None",
        "this",
        "function",
        "echo",
        "then",
        "local",
        "format",
        "assert",
        "assert_eq",
        "to_string",
        "as_str",
        "as_ref",
        "default",
        "main",
        "test",
        "tests",
        "fmt",
        "with",
        "not",
        "Error",
    ];
    let mut found: Vec<(i64, usize, String)> = Vec::new();
    let mut word = String::new();
    // Only CODE-SHAPED words: an identifier with an underscore, a camelCase
    // or PascalCase one, or a call (`name(`) long enough to be specific. A
    // plain English word in a quoted comment (`store`, `version`, `ONLY`)
    // would match half the tests in a repository and say nothing.
    let mut flush = |word: &mut String, at: usize| {
        let w = std::mem::take(word);
        let call = quote[at..].starts_with('(') && w.len() >= 6;
        let mixed =
            w.chars().any(|c| c.is_lowercase()) && w.chars().skip(1).any(|c| c.is_uppercase());
        let specific = w.contains('_') || mixed;
        if w.len() >= 4
            && !w.chars().next().is_some_and(|c| c.is_ascii_digit())
            && !COMMON.contains(&w.as_str())
            && (specific || call)
        {
            let score = w.len() as i64 + if specific { 10 } else { 0 };
            found.push((-score, at, w));
        }
    };
    for (at, c) in quote.char_indices() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, at);
        }
    }
    flush(&mut word, quote.len());
    found.sort();
    let mut out: Vec<String> = Vec::new();
    // The enclosing item's name is the strongest symbol there is: the tests
    // that call the function are the ones that say what it does.
    if let Some(name) = item {
        let bare: String = name
            .trim_start_matches("impl")
            .trim()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if bare.len() >= 4 && !COMMON.contains(&bare.as_str()) {
            out.push(bare);
        }
    }
    for (_, _, w) in found {
        if out.len() >= 3 {
            break;
        }
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// One text file of the repository, for the test index.
pub(crate) struct RepoFile {
    pub(crate) rel: String,
    pub(crate) text: String,
}

/// Every text file under `root` that holds tests — a test path, or a file with
/// a `#[cfg(test)]`/`#[test]` in it — skipping the host's ignore set, dot
/// directories, links and anything larger than [`MAX_INDEX_FILE_BYTES`].
pub(crate) fn test_files(root: &Path, ignore: &BTreeSet<String>) -> Vec<RepoFile> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    let mut seen = 0usize;
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(entries) = crate::list_entries(&dir) else {
            continue;
        };
        for entry in entries.into_iter().rev() {
            let name = entry.name.clone();
            if ignore.contains(&name) || name.starts_with('.') {
                continue;
            }
            let rel = match prefix.is_empty() {
                true => name.clone(),
                false => format!("{prefix}/{name}"),
            };
            match entry.kind {
                crate::Kind::Dir => stack.push((dir.join(&name), rel)),
                crate::Kind::File => {
                    seen += 1;
                    if seen > MAX_INDEX_FILES {
                        return out;
                    }
                    if entry.size.unwrap_or(0) > MAX_INDEX_FILE_BYTES {
                        continue;
                    }
                    let Ok(text) = std::fs::read_to_string(dir.join(&name)) else {
                        continue;
                    };
                    if is_test_path(&rel)
                        || text.contains("#[cfg(test)]")
                        || text.contains("#[test]")
                        || text.contains("def test_")
                    {
                        out.push(RepoFile { rel, text });
                    }
                }
                crate::Kind::Link => {}
            }
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// One test that mentions a symbol: where, its name, and its asserting lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mention {
    pub(crate) rel: String,
    pub(crate) line: usize,
    pub(crate) test: String,
    pub(crate) asserts: Vec<String>,
}

fn has_word(line: &str, word: &str) -> bool {
    line.match_indices(word).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + word.len()..].chars().next();
        let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        boundary(before) && boundary(after)
    })
}

/// The tests that mention any of `symbols`, at most [`MAX_TESTS`]: the file
/// under review first, then the rest by path.
pub(crate) fn mentions(files: &[RepoFile], symbols: &[String], under_review: &str) -> Vec<Mention> {
    if symbols.is_empty() {
        return Vec::new();
    }
    let mut ordered: Vec<&RepoFile> = files.iter().filter(|f| f.rel == under_review).collect();
    ordered.extend(files.iter().filter(|f| f.rel != under_review));
    let mut out: Vec<Mention> = Vec::new();
    for file in ordered {
        let lang = lang_of(&file.rel);
        let lines: Vec<&str> = file.text.split('\n').collect();
        // The test REGION: the whole of a test file, else from the first
        // `#[cfg(test)]` on.
        let region_start = match is_test_path(&file.rel) {
            true => 0,
            false => match lines
                .iter()
                .position(|l| l.trim_start().starts_with("#[cfg(test)]"))
            {
                Some(at) => at,
                None => continue,
            },
        };
        let mut index = region_start;
        while index < lines.len() {
            let line = lines[index];
            let Some(name) = item_name(line).filter(|_| {
                let t = line.trim_start();
                t.contains("fn ") || t.starts_with("def ")
            }) else {
                index += 1;
                continue;
            };
            let end = match (lang, line.trim_end().ends_with(':')) {
                (Lang::Hash, true) => indent_end(&lines, index),
                _ => brace_end(&lines, index, lang).unwrap_or(index),
            };
            let body = &lines[index..=end.min(lines.len() - 1)];
            if body.iter().any(|l| symbols.iter().any(|s| has_word(l, s))) {
                let clip = |l: &str| -> String {
                    let t = l.trim();
                    match t.char_indices().nth(160) {
                        Some((at, _)) => format!("{}…", &t[..at]),
                        None => t.to_string(),
                    }
                };
                let asserting: Vec<&str> = body
                    .iter()
                    .copied()
                    .filter(|l| l.contains("assert") || l.contains("should_panic"))
                    .collect();
                let mut asserts: Vec<String> = asserting
                    .iter()
                    .filter(|l| symbols.iter().any(|s| has_word(l, s)))
                    .map(|l| clip(l))
                    .collect();
                for l in &asserting {
                    if asserts.len() >= MAX_ASSERTS {
                        break;
                    }
                    let c = clip(l);
                    if !asserts.contains(&c) {
                        asserts.push(c);
                    }
                }
                asserts.truncate(MAX_ASSERTS);
                out.push(Mention {
                    rel: file.rel.clone(),
                    line: index + 1,
                    test: name,
                    asserts,
                });
                if out.len() >= MAX_TESTS {
                    return out;
                }
            }
            index = end.max(index) + 1;
        }
    }
    out
}

// --- the prompt and its answer -----------------------------------------------

/// The claim under judgment.
pub(crate) struct Claim<'a> {
    pub(crate) severity: Option<&'a str>,
    pub(crate) quote: &'a str,
    pub(crate) body: &'a str,
}

/// The per-claim prompt: the site, the tests, the claim, the four questions.
pub(crate) fn prompt(
    rel: &str,
    text: &str,
    site: &Site,
    tests: &[Mention],
    symbols: &[String],
    claim: &Claim<'_>,
) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut code = String::new();
    for n in site.first_line..=site.last_line.min(lines.len()) {
        if let Some((from, to)) = site.elided {
            if n == from {
                code.push_str(&format!("      | … lines {from} to {to} not shown …\n"));
            }
            if (from..=to).contains(&n) {
                continue;
            }
        }
        let mark = match (site.quote_first..=site.quote_last).contains(&n) {
            true => '>',
            false => ' ',
        };
        code.push_str(&format!(
            "{mark}{n:>5} | {}\n",
            lines[n - 1].trim_end_matches('\r')
        ));
    }
    let what = match &site.item {
        Some(name) => format!("The enclosing item `{name}`"),
        None => "The lines around the quote".to_string(),
    };
    let test_code = match site.test_code {
        Some(why) => format!("yes — {why}"),
        None => "no".to_string(),
    };
    let comments = match site.comments.is_empty() {
        true => "(none)".to_string(),
        false => site.comments.join("\n"),
    };
    let tests_text = match (tests.is_empty(), symbols.is_empty()) {
        (_, true) => "(no symbol to search for)".to_string(),
        (true, false) => "(none found)".to_string(),
        (false, false) => tests
            .iter()
            .map(|m| {
                let mut s = format!("- {} line {}: {}", m.rel, m.line, m.test);
                for a in &m.asserts {
                    s.push_str(&format!("\n    {a}"));
                }
                s
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    format!(
        "Path: {rel}\n\
         Test code: {test_code}\n\n\
         THE CLAIM (rated {severity} by the reviewer)\n\
         QUOTE: {quote}\n\
         CLAIM: {body}\n\n\
         {what}, lines {first} to {last} (the quoted line is marked >):\n\
         ```\n{code}```\n\n\
         Comments at the quoted site:\n{comments}\n\n\
         Tests in the repository that mention {symbols}:\n{tests_text}\n\n\
         Answer these four questions about the claim, each on one line, in exactly this form: \
         the question's label, a colon, one answer word, a dash, and one short sentence of \
         reason. Answer from the code shown: where the answer depends on code that is not \
         shown, say unclear rather than guess either way.\n\
         CODE: yes|no|unclear - Does the quoted code actually do what the claim says it does? \
         no only if the code shown contradicts the claim.\n\
         DISCLOSED: yes|no - Does a comment or doc at this site already WARN about this same \
         problem, so that the claim only repeats the author's own warning? A comment that \
         describes or justifies the behavior the claim criticizes is not a warning about the \
         problem: answer no for it.\n\
         OCCURS: yes|no|unclear|n/a - Would the consequence the claim predicts (a panic, a \
         failure, a wrong result, a compile error, a hole) actually follow, for the inputs this \
         code can really receive? yes only if you can name one concrete input this code can \
         receive and the line where it goes wrong; a failure that only could or might happen, \
         or that depends on code or callers not shown, is unclear. no if the code shown, its \
         types or a test shown prevents it. An error the code returns or reports on purpose \
         (a `?`, an Err, a refusal, a message and a non-zero exit) is the code working, not a \
         failure: no, unless the claim shows that error is the wrong answer. n/a if the claim \
         predicts no concrete consequence (a matter of style, documentation or a preferred \
         design).\n\
         TEST: yes|no - Is the quoted code test code whose only problem, per the claim, is \
         that it fails loudly (a panic, an expect, an assert), which is how a test reports a \
         failure?\n\
         Then one final line:\n\
         VERDICT: confirmed|refuted|unsure",
        severity = claim.severity.unwrap_or("unrated"),
        quote = claim.quote,
        body = claim.body,
        first = site.first_line,
        last = site.last_line,
        symbols = match symbols.is_empty() {
            true => "the claim".to_string(),
            false => symbols
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", "),
        },
    )
}

/// One answer: the word (`yes`, `no`, `unclear`, `n/a`) and its reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    pub(crate) question: String,
    pub(crate) answer: String,
    pub(crate) reason: String,
}

/// What a judge answer parses to: one [`Answer`] per question it gave (in
/// [`QUESTIONS`] order) and the model's own `VERDICT:` word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub(crate) answers: Vec<Answer>,
    pub(crate) stated: Option<String>,
}

/// Parse the judge's answer. Tolerant of the decoration models add (bold, a
/// bullet, a trailing period), strict about the words: anything but the
/// allowed words reads as `unclear`, never as a guess at what was meant.
pub(crate) fn parse(answer: &str) -> Parsed {
    let mut answers: Vec<Answer> = Vec::new();
    let mut stated = None;
    for line in answer.lines() {
        let line = line
            .trim()
            .trim_start_matches(['-', '*', '#', ' '])
            .replace("**", "");
        let Some((label, rest)) = line.split_once(':') else {
            continue;
        };
        let label = label.trim().to_ascii_lowercase();
        let rest = rest.trim();
        let word_end = rest
            .find(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == '-' || c == '—')
            .unwrap_or(rest.len());
        // `n/a` contains a `/` and no separator; `-` ends a word, so `n/a -`
        // and `yes - why` both split at the dash.
        let word = rest[..word_end]
            .trim_end_matches(['.', ':'])
            .to_ascii_lowercase();
        let reason = rest[word_end..]
            .trim_start_matches(|c: char| c.is_whitespace() || c == '-' || c == '—' || c == ',')
            .trim()
            .to_string();
        if label == "verdict" {
            if VERDICTS.contains(&word.as_str()) {
                stated = Some(word);
            }
            continue;
        }
        if !QUESTIONS.contains(&label.as_str()) || answers.iter().any(|a| a.question == label) {
            continue;
        }
        let word = match word.as_str() {
            "yes" | "no" | "unclear" => word,
            "n/a" | "na" | "none" if label == "occurs" => "n/a".to_string(),
            _ => "unclear".to_string(),
        };
        answers.push(Answer {
            question: label,
            answer: word,
            reason,
        });
    }
    answers.sort_by_key(|a| QUESTIONS.iter().position(|q| *q == a.question));
    Parsed { answers, stated }
}

/// The verdict the four answers amount to.
///
/// * **refuted** when an answer refutes the claim on the code's own terms: the
///   predicted consequence would NOT follow (`occurs: no`), the site already
///   WARNS about the same problem (`disclosed: yes`), or it is TEST code whose
///   only fault is failing loudly (`test: yes`);
/// * **confirmed** when every answer supports it: the code does it, the
///   consequence WOULD follow (`occurs: yes`), nothing at the site already
///   warns about it, it is not a test failing as tests do — and the model's
///   own `VERDICT:` line (`stated`), when it gave one, does not say otherwise;
/// * **unsure** otherwise — an `unclear`, a question left unanswered,
///   `code: no` on its own, `occurs: n/a`, or a stated verdict that dissents
///   from four supporting answers.
///
/// ⚠ Two judge-v1 confirmations are now `unsure`, both measured on findings
/// sweep 2 (ledger #706, every judge-v1@qwen3-coder-next CONFIRMED major
/// verified by reproduction): `occurs: n/a` confirmed 12 findings that were
/// not defects and no real one — a claim the judge itself says predicts no
/// consequence has nothing left to confirm — and a stated `refuted`/`unsure`
/// beside four supporting answers confirmed 4 more, also none real. `stated`
/// only ever WITHHOLDS a confirmation: it never refutes and never confirms by
/// itself, so the verdict stays the rule's.
///
/// ⚠ `code: no` does NOT refute, and that is a measured choice, not an
/// oversight. On the eval set's dev split (`tests/corpus/judge/`, 70 entries,
/// `judge-v1@qwen3-coder:30b-a3b-q8_0`) the judge answered `code: no` on real
/// defects whose evidence lies outside the item it was shown — a constant
/// declared at the top of a file and enforced 1400 lines below it — while the
/// known-false findings it caught were caught by the other three answers
/// anyway: with `code` as a refuter the rule lost 3 of 6 verified-real findings
/// and caught 18 of 21 known-false; without it, 1 of 6 and the same 18. The
/// rule was chosen on the dev split only; the held-out split is the result.
pub(crate) fn verdict_of(answers: &[Answer], stated: Option<&str>) -> &'static str {
    let said = |q: &str| {
        answers
            .iter()
            .find(|a| a.question == q)
            .map(|a| a.answer.as_str())
    };
    if said("occurs") == Some("no")
        || said("disclosed") == Some("yes")
        || said("test") == Some("yes")
    {
        return REFUTED;
    }
    if said("code") == Some("yes")
        && said("occurs") == Some("yes")
        && said("disclosed") == Some("no")
        && said("test") == Some("no")
        && matches!(stated, None | Some(CONFIRMED))
    {
        return CONFIRMED;
    }
    UNSURE
}

// --- the record ----------------------------------------------------------------

/// One archived verdict on one finding, by one judge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub(crate) iri: String,
    pub(crate) verdict: String,
    pub(crate) stated: Option<String>,
    pub(crate) tag: String,
    pub(crate) model: String,
    pub(crate) judged_at: Option<String>,
    /// Whether the judge was told the site is test code.
    pub(crate) test_code: bool,
    pub(crate) answers: Vec<Answer>,
    /// The judge's raw answer, for audit.
    pub(crate) raw: String,
}

/// `urn:iki:finding:{id}:judge:{tag}` — keyed by (finding, judge tag).
pub(crate) fn verdict_iri(finding_iri: &str, tag: &str) -> String {
    format!("{finding_iri}:judge:{}", iri_encode(tag))
}

const VERDICT_PREFIX: &str = "urn:iki:judge:verdict:";
const ANSWER_PREFIX: &str = "urn:iki:judge:answer:";
const SITE_TEST: &str = "urn:iki:judge:site:test";
const SITE_CODE: &str = "urn:iki:judge:site:code";
const DCTERMS: &str = "http://purl.org/dc/terms/";

fn dcterms(term: &str) -> NamedNode {
    NamedNode::new(format!("{DCTERMS}{term}")).expect("dcterms terms are valid IRIs")
}

fn store_err(e: impl std::fmt::Display) -> Error {
    Error::Endpoint(format!("browse: judge store: {e}"))
}

fn answer_iri(verdict: &str, question: &str) -> String {
    format!("{verdict}:answer:{question}")
}

/// Store one verdict on `finding_iri`. Written once: a second store of the same
/// (finding, tag) is refused by the caller's lookup, never overwritten here.
/// ★ The one thing it does replace is a [`Cannot`] at the same IRI — the
/// record that this judge could not judge it, which a judgment supersedes.
pub(crate) fn store_verdict(archive: &Archive, finding_iri: &str, v: &Verdict) -> Result<()> {
    use oxigraph::model::vocab::{rdf, xsd};
    clear_cannot(archive, &v.iri)?;
    let g = archive.graph().clone();
    let subject = NamedNode::new(&v.iri).map_err(store_err)?;
    let node = |iri: &str| NamedNode::new(iri).map_err(store_err);
    let mut quads = vec![
        Quad::new(
            subject.clone(),
            rdf::TYPE,
            node(&format!("{PROV}Activity"))?,
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            node(&format!("{PROV}used"))?,
            node(finding_iri)?,
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            dcterms("type"),
            node(&format!("{VERDICT_PREFIX}{}", v.verdict))?,
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            ik("versionTag"),
            Literal::new_simple_literal(&v.tag),
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            dcterms("creator"),
            Literal::new_simple_literal(&v.model),
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            dcterms("subject"),
            node(if v.test_code { SITE_TEST } else { SITE_CODE })?,
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            dcterms("description"),
            Literal::new_simple_literal(&v.raw),
            g.clone(),
        ),
    ];
    if let Some(at) = &v.judged_at {
        quads.push(Quad::new(
            subject.clone(),
            dcterms("created"),
            Literal::new_typed_literal(at, xsd::DATE_TIME),
            g.clone(),
        ));
    }
    let mut parts: Vec<(String, &str, &str)> = v
        .answers
        .iter()
        .map(|a| (a.question.clone(), a.answer.as_str(), a.reason.as_str()))
        .collect();
    if let Some(stated) = &v.stated {
        parts.push(("stated".to_string(), stated.as_str(), ""));
    }
    for (question, word, reason) in parts {
        let part = node(&answer_iri(&v.iri, &question))?;
        let word = word.replace('/', "-");
        quads.push(Quad::new(
            subject.clone(),
            dcterms("hasPart"),
            part.clone(),
            g.clone(),
        ));
        quads.push(Quad::new(
            part.clone(),
            dcterms("identifier"),
            Literal::new_simple_literal(&question),
            g.clone(),
        ));
        quads.push(Quad::new(
            part.clone(),
            dcterms("type"),
            node(&format!("{ANSWER_PREFIX}{word}"))?,
            g.clone(),
        ));
        if !reason.is_empty() {
            quads.push(Quad::new(
                part,
                dcterms("description"),
                Literal::new_simple_literal(reason),
                g.clone(),
            ));
        }
    }
    for quad in &quads {
        archive.insert(quad).map_err(store_err)?;
    }
    Ok(())
}

/// Every verdict on a finding, oldest first (then by tag) — found from the
/// finding inward along `prov:used`, the way its decisions are.
pub(crate) fn load_verdicts(archive: &Archive, finding_iri: &str) -> Result<Vec<Verdict>> {
    let Ok(finding) = NamedNode::new(finding_iri) else {
        return Ok(Vec::new());
    };
    let used = NamedNode::new(format!("{PROV}used")).map_err(store_err)?;
    let prefix = format!("{finding_iri}:judge:");
    let mut iris = BTreeSet::new();
    for quad in archive.quads_for_pattern(None, Some(used.as_ref()), Some(finding.as_ref().into()))
    {
        let quad = quad.map_err(store_err)?;
        let subject = quad.subject.to_string();
        let iri = subject.trim_start_matches('<').trim_end_matches('>');
        if iri.starts_with(&prefix) && !iri.contains(":answer:") {
            iris.insert(iri.to_string());
        }
    }
    let mut out = Vec::new();
    for iri in iris {
        if let Some(v) = load_verdict(archive, &iri)? {
            out.push(v);
        }
    }
    // By TIME, not text (ledger #736): `judge` is the LATEST verdict.
    fn at(v: &Verdict) -> (Option<u64>, Option<&str>) {
        crate::revision::time_key(v.judged_at.as_deref())
    }
    out.sort_by(|a, b| (at(a), &a.tag).cmp(&(at(b), &b.tag)));
    Ok(out)
}

fn literal(term: &Term) -> String {
    match term {
        Term::Literal(l) => l.value().to_string(),
        other => other.to_string(),
    }
}

fn load_verdict(archive: &Archive, iri: &str) -> Result<Option<Verdict>> {
    let subject = NamedNode::new(iri).map_err(store_err)?;
    let mut v = Verdict {
        iri: iri.to_string(),
        verdict: String::new(),
        stated: None,
        tag: String::new(),
        model: String::new(),
        judged_at: None,
        test_code: false,
        answers: Vec::new(),
        raw: String::new(),
    };
    let mut parts = Vec::new();
    for quad in archive.quads_for_pattern(Some(subject.as_ref().into()), None, None) {
        let quad = quad.map_err(store_err)?;
        let named = match &quad.object {
            Term::NamedNode(n) => Some(n.as_str().to_string()),
            _ => None,
        };
        match quad.predicate.as_str().strip_prefix(DCTERMS) {
            Some("type") => {
                if let Some(word) = named
                    .as_deref()
                    .and_then(|n| n.strip_prefix(VERDICT_PREFIX))
                {
                    v.verdict = word.to_string();
                }
            }
            Some("creator") => v.model = literal(&quad.object),
            Some("created") => v.judged_at = Some(literal(&quad.object)),
            Some("description") => v.raw = literal(&quad.object),
            Some("subject") => v.test_code = named.as_deref() == Some(SITE_TEST),
            Some("hasPart") => parts.extend(named),
            _ if quad.predicate.as_str().ends_with("#versionTag") => v.tag = literal(&quad.object),
            _ => {}
        }
    }
    if !VERDICTS.contains(&v.verdict.as_str()) {
        return Ok(None);
    }
    for part in parts {
        let node = NamedNode::new(&part).map_err(store_err)?;
        let (mut question, mut word, mut reason) = (String::new(), String::new(), String::new());
        for quad in archive.quads_for_pattern(Some(node.as_ref().into()), None, None) {
            let quad = quad.map_err(store_err)?;
            match quad.predicate.as_str().strip_prefix(DCTERMS) {
                Some("identifier") => question = literal(&quad.object),
                Some("type") => {
                    if let Term::NamedNode(n) = &quad.object {
                        if let Some(w) = n.as_str().strip_prefix(ANSWER_PREFIX) {
                            word = match w {
                                "n-a" => "n/a".to_string(),
                                other => other.to_string(),
                            };
                        }
                    }
                }
                Some("description") => reason = literal(&quad.object),
                _ => {}
            }
        }
        match question.as_str() {
            "stated" => v.stated = Some(word),
            q if QUESTIONS.contains(&q) => v.answers.push(Answer {
                question,
                answer: word,
                reason,
            }),
            _ => {}
        }
    }
    v.answers
        .sort_by_key(|a| QUESTIONS.iter().position(|q| *q == a.question));
    Ok(Some(v))
}

// --- the record of a finding that could not be judged (ledger #702) -------------

/// A `cannot` on file: judge-finding found the finding unjudgeable, for a
/// LASTING reason, under this judge's tag. Stored at the verdict IRI
/// (`urn:iki:finding:{id}:judge:{tag}`) so a later verdict there replaces it,
/// and typed apart from every verdict (`dcterms:type <urn:iki:judge:cannot>`)
/// so [`load_verdicts`] never reads one as a verdict.
///
/// ```turtle
/// <urn:iki:finding:{id}:judge:judge-v2@m> a prov:Activity ;
///     prov:used <urn:iki:finding:{id}> ;
///     dcterms:type <urn:iki:judge:cannot> ;
///     ik:versionTag "judge-v2@m" ;
///     dcterms:creator "m" ;
///     dcterms:description "the version it was reviewed against (…) is neither …" ;
///     <urn:iki:judge:basis> "sha256:…" ;
///     dcterms:created "…"^^xsd:dateTime .
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cannot {
    pub(crate) iri: String,
    pub(crate) reason: String,
    /// The search basis it was found under — `JudgeFindingEndpoint::basis`.
    pub(crate) basis: String,
    pub(crate) tag: String,
    pub(crate) model: String,
    pub(crate) at: Option<String>,
}

/// The type of a `cannot` record — [`CANNOT`], in browse's judge namespace
/// beside the verdict and answer terms.
const CANNOT_TYPE: &str = "urn:iki:judge:cannot";
/// The predicate a `cannot` record's basis is stored under.
const BASIS: &str = "urn:iki:judge:basis";

/// Whether `iri` is a `cannot` record.
fn is_cannot(archive: &Archive, subject: &NamedNode) -> Result<bool> {
    let cannot = NamedNode::new(CANNOT_TYPE).map_err(store_err)?;
    Ok(archive
        .quads_for_pattern(
            Some(subject.as_ref().into()),
            Some(dcterms("type").as_ref()),
            Some(cannot.as_ref().into()),
        )
        .next()
        .is_some())
}

/// Remove the `cannot` record at `iri`, if there is one — and ONLY one: a
/// verdict at that IRI is never touched.
fn clear_cannot(archive: &Archive, iri: &str) -> Result<()> {
    let subject = NamedNode::new(iri).map_err(store_err)?;
    if !is_cannot(archive, &subject)? {
        return Ok(());
    }
    let quads: Vec<Quad> = archive
        .quads_for_pattern(Some(subject.as_ref().into()), None, None)
        .collect::<std::result::Result<_, _>>()
        .map_err(store_err)?;
    for quad in &quads {
        archive.remove(quad).map_err(store_err)?;
    }
    Ok(())
}

/// Store a `cannot` on `finding_iri`, replacing an older one at the same IRI
/// (a retry under a changed basis that still cannot judge).
pub(crate) fn store_cannot(archive: &Archive, finding_iri: &str, c: &Cannot) -> Result<()> {
    use oxigraph::model::vocab::{rdf, xsd};
    clear_cannot(archive, &c.iri)?;
    let g = archive.graph().clone();
    let subject = NamedNode::new(&c.iri).map_err(store_err)?;
    let node = |iri: &str| NamedNode::new(iri).map_err(store_err);
    let mut quads = vec![
        Quad::new(
            subject.clone(),
            rdf::TYPE,
            node(&format!("{PROV}Activity"))?,
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            node(&format!("{PROV}used"))?,
            node(finding_iri)?,
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            dcterms("type"),
            node(CANNOT_TYPE)?,
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            ik("versionTag"),
            Literal::new_simple_literal(&c.tag),
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            dcterms("creator"),
            Literal::new_simple_literal(&c.model),
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            dcterms("description"),
            Literal::new_simple_literal(&c.reason),
            g.clone(),
        ),
        Quad::new(
            subject.clone(),
            node(BASIS)?,
            Literal::new_simple_literal(&c.basis),
            g.clone(),
        ),
    ];
    if let Some(at) = &c.at {
        quads.push(Quad::new(
            subject,
            dcterms("created"),
            Literal::new_typed_literal(at, xsd::DATE_TIME),
            g,
        ));
    }
    for quad in &quads {
        archive.insert(quad).map_err(store_err)?;
    }
    Ok(())
}

/// The `cannot` record at `iri`, if that is what is there.
pub(crate) fn load_cannot(archive: &Archive, iri: &str) -> Result<Option<Cannot>> {
    let subject = NamedNode::new(iri).map_err(store_err)?;
    if !is_cannot(archive, &subject)? {
        return Ok(None);
    }
    let mut c = Cannot {
        iri: iri.to_string(),
        reason: String::new(),
        basis: String::new(),
        tag: String::new(),
        model: String::new(),
        at: None,
    };
    for quad in archive.quads_for_pattern(Some(subject.as_ref().into()), None, None) {
        let quad = quad.map_err(store_err)?;
        match quad.predicate.as_str() {
            BASIS => c.basis = literal(&quad.object),
            p => match p.strip_prefix(DCTERMS) {
                Some("description") => c.reason = literal(&quad.object),
                Some("creator") => c.model = literal(&quad.object),
                Some("created") => c.at = Some(literal(&quad.object)),
                _ if p.ends_with("#versionTag") => c.tag = literal(&quad.object),
                _ => {}
            },
        }
    }
    Ok(Some(c))
}

/// A verdict as JSON — the shape of `judge` (and each entry of `judges`) on a
/// finding row, and of the judge resource's answer.
///
/// ```json
/// {"iri": "urn:iki:finding:{id}:judge:judge-v2@qwen3-coder:30b",
///  "verdict": "refuted", "stated": "refuted",
///  "tag": "judge-v2@qwen3-coder:30b", "model": "qwen3-coder:30b",
///  "judged_at": "2026-10-01T23:00:00.000Z", "test_code": false,
///  "answers": {"code": {"answer": "no", "reason": "…"},
///              "disclosed": {"answer": "no", "reason": "…"},
///              "occurs": {"answer": "no", "reason": "…"},
///              "test": {"answer": "no", "reason": "…"}}}
/// ```
pub(crate) fn verdict_json(v: &Verdict) -> serde_json::Value {
    let mut answers = serde_json::Map::new();
    for a in &v.answers {
        answers.insert(
            a.question.clone(),
            serde_json::json!({"answer": a.answer, "reason": a.reason}),
        );
    }
    serde_json::json!({
        "iri": v.iri,
        "verdict": v.verdict,
        "stated": v.stated,
        "tag": v.tag,
        "model": v.model,
        "judged_at": v.judged_at,
        "test_code": v.test_code,
        "answers": answers,
    })
}

/// The verdict nodes as Turtle, for the finding's graph face.
pub(crate) fn verdict_turtle(finding_iri: &str, v: &Verdict) -> String {
    let mut props = vec![
        "a prov:Activity".to_string(),
        format!("prov:used <{finding_iri}>"),
        format!("dcterms:type <{VERDICT_PREFIX}{}>", v.verdict),
        format!("ik:versionTag {}", crate::ttl_str(&v.tag)),
        format!("dcterms:creator {}", crate::ttl_str(&v.model)),
        format!(
            "dcterms:subject <{}>",
            if v.test_code { SITE_TEST } else { SITE_CODE }
        ),
        format!("dcterms:description {}", crate::ttl_str(&v.raw)),
    ];
    if let Some(at) = &v.judged_at {
        props.push(format!("dcterms:created \"{at}\"^^xsd:dateTime"));
    }
    let mut parts: Vec<(String, String, String)> = v
        .answers
        .iter()
        .map(|a| (a.question.clone(), a.answer.clone(), a.reason.clone()))
        .collect();
    if let Some(stated) = &v.stated {
        parts.push(("stated".to_string(), stated.clone(), String::new()));
    }
    if !parts.is_empty() {
        props.push(format!(
            "dcterms:hasPart {}",
            parts
                .iter()
                .map(|(q, _, _)| format!("<{}>", answer_iri(&v.iri, q)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut out = format!("\n<{}> {} .\n", v.iri, props.join(" ;\n    "));
    for (question, word, reason) in parts {
        let mut p = vec![
            format!("dcterms:identifier {}", crate::ttl_str(&question)),
            format!("dcterms:type <{ANSWER_PREFIX}{}>", word.replace('/', "-")),
        ];
        if !reason.is_empty() {
            p.push(format!("dcterms:description {}", crate::ttl_str(&reason)));
        }
        out.push_str(&format!(
            "\n<{}> {} .\n",
            answer_iri(&v.iri, &question),
            p.join(" ;\n    ")
        ));
    }
    out
}

// --- asking --------------------------------------------------------------------

/// Everything one judgment needs, assembled — the prompt and what it was built
/// from (the site flag is recorded with the verdict).
pub(crate) struct Prepared {
    pub(crate) prompt: String,
    pub(crate) test_code: bool,
}

/// Build the judge's prompt for the quote at character offset `char_start`.
pub(crate) fn prepare(
    rel: &str,
    text: &str,
    char_start: u64,
    claim: &Claim<'_>,
    files: &[RepoFile],
) -> Option<Prepared> {
    let byte_start = text
        .char_indices()
        .nth(char_start as usize)
        .map(|(b, _)| b)
        .or_else(|| (char_start as usize == text.chars().count()).then_some(text.len()))?;
    if !text[byte_start..].starts_with(claim.quote) {
        return None;
    }
    let site = site(rel, text, byte_start, byte_start + claim.quote.len());
    let symbols = symbols(claim.quote, site.item.as_deref());
    let tests = mentions(files, &symbols, rel);
    Some(Prepared {
        prompt: prompt(rel, text, &site, &tests, &symbols, claim),
        test_code: site.test_code.is_some(),
    })
}

/// The judge's identity: its tag and model, resolved the way the review pass
/// resolves its own — the operator's label only while the judge asks the
/// configured review backend, else the backend's own `:model`.
pub(crate) async fn identity(
    inv: &Invocation<'_>,
    config: &ExplainConfig,
    provider: &str,
) -> (String, String) {
    let explicit = match provider == config.review_provider {
        true => config.review_model_label.clone(),
        false => None,
    };
    let model = match explicit {
        Some(label) => label,
        None => resolve_model(inv, provider)
            .await
            .unwrap_or_else(|| provider_label(provider)),
    };
    (format!("{JUDGE_PROMPT_VERSION}@{model}"), model)
}

/// Ask the judge once and parse what it said.
pub(crate) async fn ask(
    inv: &Invocation<'_>,
    config: &ExplainConfig,
    provider: &str,
    prepared: &Prepared,
) -> Result<(String, Parsed)> {
    let request = Request::new(Verb::Source, parse_iri(provider)?)
        .with_arg(
            "prompt",
            ArgRef::Inline(prepared.prompt.clone().into_bytes()),
        )
        .with_arg(
            "system",
            ArgRef::Inline(JUDGE_SYSTEM_PROMPT.as_bytes().to_vec()),
        )
        // Temperature 0: a verdict must be reproducible, and the local backend
        // is deterministic there (measured, ledger #449's noise-floor comment).
        .with_arg("temperature", ArgRef::Inline(b"0".to_vec()))
        .with_arg(
            "max_tokens",
            ArgRef::Inline(config.judge_max_tokens.to_string().into_bytes()),
        );
    let answer = inv.issue(request).await?;
    let raw = String::from_utf8_lossy(&answer.bytes).trim().to_string();
    let parsed = parse(&raw);
    Ok((raw, parsed))
}

/// Judge every SERIOUS finding in `finding_iris` that has no verdict under this
/// judge's tag yet — the review pass's last step. Never fails the pass: a
/// judgment that cannot be made (a transport error, an anchor that no longer
/// matches) records nothing and is counted, so the next pass can try again.
///
/// Returns `(judged, failed)`.
pub(crate) async fn judge_findings(
    inv: &Invocation<'_>,
    config: &ExplainConfig,
    root: &Path,
    rel: &str,
    text: &str,
    finding_iris: &[String],
) -> Result<(usize, usize)> {
    let Some(provider) = config.judge_provider.clone() else {
        return Ok((0, 0));
    };
    let mut todo = Vec::new();
    for iri in finding_iris {
        let Some(id) = iri.strip_prefix(annotate::Family::Finding.prefix()) else {
            continue;
        };
        let Some(finding) = annotate::load_record(&config.archive, annotate::Family::Finding, id)?
        else {
            continue;
        };
        if !finding
            .severity
            .as_deref()
            .is_some_and(|s| SEVERITIES[..crate::finding::SERIOUS_SEVERITIES].contains(&s))
        {
            continue;
        }
        todo.push(finding);
    }
    if todo.is_empty() {
        return Ok((0, 0));
    }
    let (tag, model) = identity(inv, config, &provider).await;
    let files = test_files(root, &config.ignore);
    let (mut judged, mut failed) = (0, 0);
    for finding in todo {
        let iri = verdict_iri(&finding.iri(), &tag);
        if finding.judges.iter().any(|v| v.iri == iri) {
            continue;
        }
        let claim = Claim {
            severity: finding.severity.as_deref(),
            quote: &finding.exact,
            body: &finding.body,
        };
        let Some(prepared) = prepare(rel, text, finding.start, &claim, &files) else {
            failed += 1;
            continue;
        };
        let (raw, parsed) = match ask(inv, config, &provider, &prepared).await {
            Ok(answer) => answer,
            // A denial is the caller's authority, identical for every finding:
            // stop asking, but the pass it rides on stands.
            Err(Error::Denied(_)) => return Ok((judged, failed + 1)),
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        let verdict = Verdict {
            iri,
            verdict: verdict_of(&parsed.answers, parsed.stated.as_deref()).to_string(),
            stated: parsed.stated,
            tag: tag.clone(),
            model: model.clone(),
            judged_at: inv.now().map(|t| iso8601(t.as_millis())),
            test_code: prepared.test_code,
            answers: parsed.answers,
            raw,
        };
        store_verdict(&config.archive, &finding.iri(), &verdict)?;
        judged += 1;
    }
    Ok((judged, failed))
}

// --- the resource: judge any claim against a file ---------------------------------

pub(crate) fn bind(
    space: EndpointSpace,
    roots: &Roots,
    config: &Arc<ExplainConfig>,
) -> EndpointSpace {
    let judge: Arc<dyn Endpoint> = Arc::new(JudgeEndpoint {
        roots: Arc::clone(roots),
        config: Arc::clone(config),
    });
    let space = crate::bind_family(space, roots, judge, None, Some("judge:{path}"));
    let one: Arc<dyn Endpoint> = Arc::new(JudgeFindingEndpoint {
        roots: Arc::clone(roots),
        config: Arc::clone(config),
    });
    crate::bind_family(space, roots, one, None, Some("judge-finding:{id}"))
}

/// `urn:repo:{repo}:judge:{path}` — the judge, asked about ANY claim against
/// the file's current content: the same site, the same test index, the same
/// prompt and the same rule the review pass runs on its serious findings, with
/// nothing archived (a claim passed by argument has no finding to attach to).
///
/// ★ It is the measurement seam as much as a tool: `examples/judge-probe.rs`
/// runs the eval set through it, so what is measured is what ships.
struct JudgeEndpoint {
    roots: Roots,
    config: Arc<ExplainConfig>,
}

#[async_trait]
impl Endpoint for JudgeEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(format!(
                "browse-judge does not support the {:?} verb",
                inv.request.verb
            )));
        }
        let (repo, root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        let rel = path_binding(inv)?;
        if rel.is_empty() {
            return Err(Error::MissingArgument("path".to_string()));
        }
        let target = resolve(root, &rel)?;
        if target.is_dir() {
            return Err(Error::NotFound(format!(
                "browse: `{rel}` is a directory — a claim is judged against one file"
            )));
        }
        let quote = inv
            .inline_str("quote")
            .map_err(|_| Error::MissingArgument("quote".to_string()))?
            .to_string();
        let body = inv
            .inline_str("claim")
            .map_err(|_| Error::MissingArgument("claim".to_string()))?
            .to_string();
        let severity = match inv.inline_str("severity") {
            Ok(s) if crate::finding::is_severity(s) => Some(s.to_string()),
            Ok(other) => {
                return Err(Error::InvalidArgument {
                    name: "severity".to_string(),
                    detail: format!("`{other}` is not one of {}", SEVERITIES.join(", ")),
                })
            }
            Err(_) => None,
        };
        let provider = match inv.inline_str("provider") {
            Ok(requested) => {
                let selectable = self.config.selectable();
                if !selectable.contains(requested) {
                    return Err(Error::Denied(format!(
                        "browse: `{requested}` is not a provider this host offers to judge \
                         with; selectable here: {}",
                        selectable.into_iter().collect::<Vec<_>>().join(", ")
                    )));
                }
                requested.to_string()
            }
            Err(_) => self
                .config
                .judge_provider
                .clone()
                .unwrap_or_else(|| self.config.review_provider.clone()),
        };
        let content = inv.source(&parse_iri(&file_iri(repo, &rel))?).await?;
        let Ok(text) = String::from_utf8(content.bytes.clone()) else {
            return Err(Error::InvalidArgument {
                name: "path".to_string(),
                detail: format!("`{rel}` is binary — there is nothing to judge"),
            });
        };
        // Which occurrence: `start` (a character offset, as a finding row
        // carries it), else the first.
        let char_start = match inv.inline_str("start") {
            Ok(s) => s.parse::<u64>().map_err(|_| Error::InvalidArgument {
                name: "start".to_string(),
                detail: format!("`{s}` is not a character offset"),
            })?,
            Err(_) => match text.find(&quote) {
                Some(at) => text[..at].chars().count() as u64,
                None => {
                    return Err(Error::InvalidArgument {
                        name: "quote".to_string(),
                        detail: format!("the quote does not occur in `{rel}`"),
                    })
                }
            },
        };
        let claim = Claim {
            severity: severity.as_deref(),
            quote: &quote,
            body: &body,
        };
        let files = test_files(root, &self.config.ignore);
        let Some(prepared) = prepare(&rel, &text, char_start, &claim, &files) else {
            return Err(Error::InvalidArgument {
                name: "start".to_string(),
                detail: format!("the quote is not at character {char_start} of `{rel}`"),
            });
        };
        let (tag, model) = identity(inv, &self.config, &provider).await;
        let (raw, parsed) = ask(inv, &self.config, &provider, &prepared).await?;
        let verdict = Verdict {
            iri: String::new(),
            verdict: verdict_of(&parsed.answers, parsed.stated.as_deref()).to_string(),
            stated: parsed.stated,
            tag,
            model,
            judged_at: inv.now().map(|t| iso8601(t.as_millis())),
            test_code: prepared.test_code,
            answers: parsed.answers,
            raw,
        };
        let as_text = inv
            .inline_str("as")
            .is_ok_and(|a| a.starts_with("text/plain"));
        if as_text {
            let mut out = format!("{} ({})\n", verdict.verdict, verdict.tag);
            for a in &verdict.answers {
                out.push_str(&format!("{}: {} - {}\n", a.question, a.answer, a.reason));
            }
            return Ok(repr_utf8("text/plain", out));
        }
        let mut json = verdict_json(&verdict);
        json["iri"] = serde_json::Value::Null;
        json["raw"] = serde_json::Value::String(verdict.raw.clone());
        json["prompt_bytes"] = serde_json::Value::from(prepared.prompt.len());
        Ok(crate::repr(
            "application/json",
            serde_json::to_string(&json).unwrap_or_default(),
        ))
    }

    fn name(&self) -> &str {
        "browse-judge"
    }

    fn describe(&self) -> Description {
        judge_description(&self.config)
    }
}

fn judge_description(config: &ExplainConfig) -> Description {
    Description::new("browse-judge")
        .title("Judge a review claim against a file")
        .summary(
            "Confirm or refute ONE review claim (a quote and a claim body) against a file's \
             current content, with the context the reviewer lacked: the whole enclosing item, \
             the comments at the site, whether it is test code, and the repository's tests that \
             mention the claim's symbols. Four answers, each with a reason (code: does the cited \
             code do what the claim says; disclosed: is the hazard already stated at the site; \
             occurs: would the predicted failure happen; test: is this test code where failing \
             loudly is the intent) and a verdict BY RULE: refuted when any answer refutes, \
             confirmed when all four support it (occurs: yes, never n/a) and the model's own \
             VERDICT line does not dissent, else unsure. That VERDICT line rides along as \
             `stated`. Temperature 0. Nothing is archived: the review pass runs the same \
             judgment on each serious finding it mints and attaches the verdict to the finding \
             (`judge` on its json row), and nothing is dropped or withheld because of one. \
             application/json (default) is the verdict object plus `raw` (the answer) and \
             `prompt_bytes`; text/plain is a digest.",
        )
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .requires(CAP_NET)
        .input(
            ArgSpec::new("path")
                .binding()
                .class(crate::XSD_STRING)
                .summary("file path within the root, percent-encoded"),
        )
        .input(
            ArgSpec::new("quote").class(crate::XSD_STRING).summary(
                "the claim's quote: text that occurs in the file, character for character",
            ),
        )
        .input(
            ArgSpec::new("claim")
                .class(crate::XSD_STRING)
                .summary("the claim's body: what the reviewer says is wrong at the quote"),
        )
        .input(
            ArgSpec::new("severity")
                .optional()
                .class(crate::XSD_STRING)
                .summary("the severity the reviewer proposed")
                .one_of(SEVERITIES),
        )
        .input(
            ArgSpec::new("start")
                .optional()
                .class("http://www.w3.org/2001/XMLSchema#nonNegativeInteger")
                .summary(
                    "which occurrence: the quote's character offset in the file (a finding \
                     row's `start`); default the first occurrence",
                ),
        )
        .input(
            ArgSpec::new("provider")
                .optional()
                .class("http://www.w3.org/2001/XMLSchema#anyURI")
                .summary(
                    "the LLM provider IRI to judge with instead of the configured judge \
                     provider; one_of is what this host allows",
                )
                .one_of(config.selectable()),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .class(crate::XSD_STRING)
                .summary("the face to render")
                .one_of(["application/json", "text/plain"])
                .default_value("application/json"),
        )
        .output("application/json")
        .output("text/plain;charset=utf-8")
}

// --- the resource: judge ONE finding by id, archived (ledger #696) -------------

/// `urn:repo:{repo}:judge-finding:{id}` — the judge, asked about one FINDING
/// already in the queue, its verdict archived on the finding exactly as the
/// review pass archives the verdicts it makes. It exists so a host can
/// BACKFILL: only freshly minted serious findings are judged, so the queue
/// that predates the judge has no verdicts at all.
///
/// ★ **Idempotent by the archive, keyed by (finding, judge tag).** A finding
/// that already carries a verdict under this judge's tag answers it with no
/// call (`status: "archived"`, `calls: 0`), so a backfill over the whole
/// queue repays nothing it already paid for; Exists asks the same question
/// without judging. A different judge (`provider=`, or a prompt-version bump)
/// is a different tag and judges beside, never over.
///
/// ★ **Judged against the bytes the REVIEWER saw.** The verdict must mean what
/// a pass-minted verdict means, or the archive holds two kinds of verdict
/// under one key: so the site is the version the finding's pass reviewed (the
/// `sha256` its pass IRI names), at the offset it was minted at — the current
/// file when it still has those bytes, else that version recovered from the
/// repository's git history by the hash of its bytes (the way
/// `tests/corpus/judge/export.py` recovers it). A finding whose version cannot
/// be recovered (bytes never committed, history rewritten, a pull-request
/// diff) is ANSWERED that way — `status: "cannot"` with the reason, as a
/// value, never as an error. ⚠ The test index is the repository AS IT STANDS,
/// not as it was at the recovered commit: a test written since the review
/// counts as context.
///
/// ★ **A `cannot` is archived, and when it is retried is a KEY, not a
/// timer** (ledger #702). A lasting reason is stored as a [`Cannot`] at the
/// verdict IRI, so it survives a restart and a re-read answers it from the
/// archive (`archived: true`) — no git recovery, no call. It is retried when
/// something that could change it changes, and nothing else:
///
/// * the judge TAG — a new judge or prompt version is a different IRI, so it
///   starts with nothing on file;
/// * the SEARCH BASIS — the sha256 of the file's current content hash and
///   every commit, on any ref, that touches its path ([`Self::basis`]). Those
///   are exactly where the reviewed bytes are looked for, so a commit that
///   lands them (on any branch) or a working tree restored to them changes
///   the basis and the next read looks again. A pull-request finding's basis
///   is a constant: no repository change makes a diff a file.
///
/// A failure to LOOK (git would not run, the history is unreadable, a read
/// broke off) is never archived: it says nothing lasting about the finding.
/// A verdict stored later at the same IRI replaces the `cannot`; a verdict is
/// never replaced. A `cannot` is not a verdict: it is not on the finding's
/// `judge` / `judges`, and Exists stays `false`.
///
/// ★ **No judge, no judgment.** With no judge configured and no `provider=`
/// named, Source refuses with [`no_judge`]'s typed `Conflict` and Exists
/// answers `false`. It used to ask the REVIEW tier instead, silently — a host
/// that had turned the judge off got verdicts it never asked for.
///
/// Why its own name and not `finding=` on [`JudgeEndpoint`]: that resource
/// judges a claim passed BY ARGUMENT against a path and archives nothing, and
/// its `quote` and `claim` are required. A `finding=` there would make both
/// optional (which removes the resource from every pipeline: a Source with no
/// required by-value input cannot be piped into) and make the path a second,
/// contradictable copy of what the finding already names. And not
/// `judge:finding:{id}`: that is a path under `judge:{path}`, so the two rows
/// would both match a file named `finding:…` and resolution order would
/// decide — the same reason `explain-versions:{path}` is not
/// `explain:versions:{path}`.
struct JudgeFindingEndpoint {
    roots: Roots,
    config: Arc<ExplainConfig>,
}

/// The version of a finding's file the judge reads, and where it came from.
struct Reviewed {
    text: String,
    char_start: u64,
    /// `current` or `recovered`.
    content: &'static str,
    /// `recorded` (the finding's own offset), or `minted` (recovered from the
    /// finding id: the one occurrence that reproduces it).
    anchor: &'static str,
    /// The commit the recovered version was read from.
    commit: Option<String>,
    hash: String,
}

/// The reviewed content hash a pass IRI carries: `sha256:{64 hex}` after
/// `{PASS_PREFIX}{repo}:`.
fn pass_hash(pass: &str, repo: &str) -> Option<String> {
    let rest = pass
        .strip_prefix(crate::review::PASS_PREFIX)?
        .strip_prefix(repo)?
        .strip_prefix(':')?;
    let hash = rest.get(..71)?;
    (hash.starts_with("sha256:") && hash[7..].bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| hash.to_string())
}

/// The character offset of the occurrence of `exact` in `text` whose
/// position reproduces the finding id — `sha256(pass ‖ char_start ‖ exact)`
/// names exactly one.
fn minted_offset(text: &str, pass: &str, exact: &str, id: &str) -> Option<u64> {
    if exact.is_empty() {
        return None;
    }
    let mut chars = 0u64;
    let mut last = 0usize;
    for byte in annotate::occurrences(text, exact) {
        chars += text[last..byte].chars().count() as u64;
        last = byte;
        if annotate::finding_id(pass, chars, exact) == id {
            return Some(chars);
        }
    }
    None
}

/// One version of `rel` from the repository's git history, found by the
/// sha256 of its bytes, with a commit that carries it — `Ok(None)` when no
/// commit does, `Err(why)` when the history cannot be read at all. Two `git`
/// processes stream the work (one `cat-file --batch-check` over every commit
/// that touched the path, one `cat-file --batch` over the distinct blobs),
/// never one per version.
fn recover_version(
    root: &Path,
    rel: &str,
    hash: &str,
) -> std::result::Result<Option<(String, String)>, Unjudgeable> {
    use std::io::{BufRead, Read, Write};
    use std::process::{Command, Stdio};

    let commits = history(root, rel).map_err(Unjudgeable::transient)?;
    let commits: Vec<String> = commits.split_whitespace().map(str::to_string).collect();
    if commits.is_empty() {
        return Ok(None);
    }
    let batch = |args: &[&str], input: String| -> std::result::Result<Vec<u8>, String> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not run git: {e}"))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        // Written from its own thread: git answers as it reads, and a full
        // stdout pipe would otherwise stall both sides.
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let mut out = Vec::new();
        child
            .stdout
            .take()
            .expect("stdout is piped")
            .read_to_end(&mut out)
            .map_err(|e| format!("git output: {e}"))?;
        let _ = writer.join();
        child.wait().map_err(|e| format!("git: {e}"))?;
        Ok(out)
    };
    // `rev:./path` is relative to the root, which may sit below the work
    // tree's top; a plain `rev:path` would be read from the top.
    let specs: String = commits.iter().map(|c| format!("{c}:./{rel}\n")).collect();
    let checks = batch(&["cat-file", "--batch-check"], specs).map_err(Unjudgeable::transient)?;
    let mut blobs: Vec<(String, String)> = Vec::new();
    for (commit, line) in commits.iter().zip(String::from_utf8_lossy(&checks).lines()) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 3 && parts[1] == "blob" && blobs.iter().all(|(b, _)| b != parts[0]) {
            blobs.push((parts[0].to_string(), commit.clone()));
        }
    }
    if blobs.is_empty() {
        return Ok(None);
    }
    let input: String = blobs.iter().map(|(b, _)| format!("{b}\n")).collect();
    let out = batch(&["cat-file", "--batch"], input).map_err(Unjudgeable::transient)?;
    let mut reader = std::io::Cursor::new(out);
    for (_, commit) in &blobs {
        let mut header = String::new();
        if reader
            .read_line(&mut header)
            .map_err(|e| Unjudgeable::transient(e.to_string()))?
            == 0
        {
            break;
        }
        let size: usize = header
            .split_whitespace()
            .nth(2)
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| {
                Unjudgeable::transient(format!(
                    "git cat-file: unexpected header `{}`",
                    header.trim()
                ))
            })?;
        // The blob's bytes and the newline git ends each one with.
        let mut bytes = vec![0; size + 1];
        reader
            .read_exact(&mut bytes)
            .map_err(|e| Unjudgeable::transient(format!("git cat-file: {e}")))?;
        bytes.pop();
        if annotate::content_hash(&bytes) == hash {
            return match String::from_utf8(bytes) {
                Ok(text) => Ok(Some((text, commit.clone()))),
                Err(_) => Err(Unjudgeable::lasting("the reviewed version is not UTF-8")),
            };
        }
    }
    Ok(None)
}

/// Every commit, on any ref, that touches `rel` — `git rev-list --all`, one
/// per line. `Err(why)` when the history cannot be read at all.
fn history(root: &Path, rel: &str) -> std::result::Result<String, String> {
    let commits = crate::git(root, &["rev-list", "--all", "--", rel])
        .map_err(|e| format!("could not run git: {e}"))?;
    if !commits.status.success() {
        return Err(format!(
            "git history is unreadable here: {}",
            String::from_utf8_lossy(&commits.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&commits.stdout).into_owned())
}

/// Why a finding cannot be judged, and whether that can pass by itself.
///
/// `lasting` is a fact about the finding and the repository AS THEY STAND (a
/// pull-request finding; reviewed bytes no commit carries; an anchor no
/// occurrence reproduces) — archived, and retried only when its basis
/// changes. A transient one is a failure to LOOK (git would not run, a read
/// broke off) — answered, never archived, so the next read looks again.
#[derive(Debug)]
struct Unjudgeable {
    reason: String,
    lasting: bool,
}

impl Unjudgeable {
    fn lasting(reason: impl Into<String>) -> Self {
        Unjudgeable {
            reason: reason.into(),
            lasting: true,
        }
    }

    fn transient(reason: impl Into<String>) -> Self {
        Unjudgeable {
            reason: reason.into(),
            lasting: false,
        }
    }
}

/// The search basis a `cannot` was found under — the one thing besides the
/// judge tag whose change may turn it into a judgment.
const BASIS_PR: &str = "pull-request";

impl JudgeFindingEndpoint {
    /// The finding, checked: it exists, it is this repo's, the caller may
    /// read it.
    fn finding<'a>(&'a self, inv: &'a Invocation<'_>) -> Result<(annotate::Annotation, &'a Path)> {
        let (repo, root) = repo_root(inv, &self.roots)?;
        granted(inv, repo)?;
        let id = inv
            .bindings
            .get("id")
            .ok_or_else(|| Error::MissingArgument("id".to_string()))?;
        let finding = annotate::load_record(&self.config.archive, annotate::Family::Finding, id)?
            .ok_or_else(|| {
            Error::NotFound(format!(
                "browse: no finding `{}`",
                crate::finding::finding_iri(id)
            ))
        })?;
        if finding.repo != repo {
            return Err(Error::NotFound(format!(
                "browse: `{}` is a finding on `{}`, not `{repo}` — judge it as \
                 urn:repo:{}:judge-finding:{id}",
                finding.iri(),
                finding.repo,
                finding.repo
            )));
        }
        Ok((finding, root))
    }

    /// The judge to ask and its identity — `provider=` (one this host
    /// offers), else the configured judge. `None` when neither: the judge is
    /// off and nobody named one (ledger #702). ⚠ NEVER the review tier in its
    /// place — a host that turned the judge off must not get verdicts it did
    /// not ask for, archived under a tag that reads like a judge's.
    async fn judge(&self, inv: &Invocation<'_>) -> Result<Option<(String, String, String)>> {
        let provider = match inv.inline_str("provider") {
            Ok(requested) => {
                let selectable = self.config.selectable();
                if !selectable.contains(requested) {
                    return Err(Error::Denied(format!(
                        "browse: `{requested}` is not a provider this host offers to judge \
                         with; selectable here: {}",
                        selectable.into_iter().collect::<Vec<_>>().join(", ")
                    )));
                }
                requested.to_string()
            }
            Err(_) => match self.config.judge_provider.clone() {
                Some(provider) => provider,
                None => return Ok(None),
            },
        };
        let (tag, model) = identity(inv, &self.config, &provider).await;
        Ok(Some((provider, tag, model)))
    }

    /// The search basis for `finding` as the repository stands — what a
    /// `cannot` is archived under and compared against on a re-read: the
    /// file's current content hash and every commit, on any ref, that touches
    /// its path, hashed together. `None` when the history cannot be read (a
    /// `cannot` found then is not archived). A pull-request finding's basis is
    /// a constant: no change to the repository makes its diff a file.
    fn basis(
        finding: &annotate::Annotation,
        root: &Path,
        current: &annotate::CurrentContent,
    ) -> Option<String> {
        if matches!(finding.target_ref(), annotate::TargetRef::Pr(_)) {
            return Some(BASIS_PR.to_string());
        }
        let commits = history(root, &finding.rel).ok()?;
        let now = current.hash().unwrap_or("none");
        Some(annotate::content_hash(
            format!("{now}\n{commits}").as_bytes(),
        ))
    }

    /// The version the finding's pass reviewed — `Err` when it cannot be had,
    /// which the caller ANSWERS rather than raises (and archives when it is
    /// lasting). `current` is the file as it stands, read once by the caller.
    fn reviewed(
        &self,
        finding: &annotate::Annotation,
        root: &Path,
        current: annotate::CurrentContent,
    ) -> std::result::Result<Reviewed, Unjudgeable> {
        if matches!(finding.target_ref(), annotate::TargetRef::Pr(_)) {
            return Err(Unjudgeable::lasting(
                "a pull-request finding: its text is a diff, not a file version the judge \
                 can read around",
            ));
        }
        let pass = finding.generated_by.clone().unwrap_or_default();
        // The reviewed bytes and where the quote was minted in them. Without a
        // pass IRI (a record no pass minted), the stored selector pair is the
        // only version on file.
        let (hash, recorded) = match pass_hash(&pass, &finding.repo) {
            Some(hash) => {
                let recorded = (finding.hash == hash).then_some(finding.start);
                (hash, recorded)
            }
            None => (finding.hash.clone(), Some(finding.start)),
        };
        let (text, content, commit) = match current {
            annotate::CurrentContent::Text(text, now) if now == hash => (text, "current", None),
            _ => match recover_version(root, &finding.rel, &hash)? {
                Some((text, commit)) => (text, "recovered", Some(commit)),
                None => {
                    return Err(Unjudgeable::lasting(format!(
                        "the version it was reviewed against ({hash}) is neither the current \
                         file nor any committed version of `{}`",
                        finding.rel
                    )))
                }
            },
        };
        let (char_start, anchor) =
            match recorded {
                Some(start) => (start, "recorded"),
                None => match minted_offset(&text, &pass, &finding.exact, &finding.id) {
                    Some(start) => (start, "minted"),
                    None => return Err(Unjudgeable::lasting(
                        "no occurrence of its quote in the reviewed version reproduces its id, \
                         so where it was anchored is unknown",
                    )),
                },
            };
        Ok(Reviewed {
            text,
            char_start,
            content,
            anchor,
            commit,
            hash,
        })
    }
}

/// The refusal judge-finding answers when there is no judge to ask: none
/// configured and no `provider=` named (ledger #702). A [`Error::Conflict`]:
/// the request is well-formed and authorized, the finding exists, and the
/// host's STATE refuses it — a host with a judge answers the same request.
fn no_judge(finding: &str) -> Error {
    Error::Conflict(format!(
        "browse: no judge is configured on this host, so `{finding}` cannot be judged — \
         judge-finding never falls back to the review tier. Configure a judge \
         (ExplainConfig::judge_provider), or name one this host offers with provider="
    ))
}

#[async_trait]
impl Endpoint for JudgeFindingEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let verb = inv.request.verb;
        if verb != Verb::Source && verb != Verb::Exists {
            return Err(Error::Endpoint(format!(
                "browse-judge-finding does not support the {verb:?} verb"
            )));
        }
        let (finding, root) = self.finding(inv)?;
        let judge = self.judge(inv).await?;
        // Exists: is there a VERDICT by this judge — the backfill's check,
        // which never asks the model. With no judge there is none to have; a
        // `cannot` on file is not a verdict either.
        if verb == Verb::Exists {
            let found = judge.as_ref().is_some_and(|(_, tag, _)| {
                let iri = verdict_iri(&finding.iri(), tag);
                finding.judges.iter().any(|v| v.iri == iri)
            });
            return Ok(repr_utf8(
                "text/plain",
                if found { "true" } else { "false" }.to_string(),
            ));
        }
        let Some((provider, tag, model)) = judge else {
            return Err(no_judge(&finding.iri()));
        };
        let iri = verdict_iri(&finding.iri(), &tag);
        let archived = finding.judges.iter().find(|v| v.iri == iri).cloned();
        let mut answer = serde_json::json!({
            "finding": finding.iri(),
            "repo": finding.repo,
            "path": finding.rel,
            "tag": tag,
            "status": "archived",
            "calls": 0,
            "reason": null,
            "content": null,
            "anchor": null,
            "commit": null,
            "content_hash": null,
            "judge": null,
            "archived": false,
            "record": null,
        });
        let now = || inv.now().map(|t| iso8601(t.as_millis()));
        // (status, reason, calls, archived, record)
        let (status, reason, calls, from_archive, record) = match archived {
            Some(verdict) => {
                answer["judge"] = verdict_json(&verdict);
                ("archived", None, 0, true, Some(iri))
            }
            None => {
                let current = annotate::current_content_for(
                    inv,
                    &self.roots,
                    &finding.repo,
                    &finding.target_ref(),
                )
                .await?;
                let basis = Self::basis(&finding, root, &current);
                let on_file = load_cannot(&self.config.archive, &iri)?;
                match on_file {
                    // ★ The archived `cannot`, while nothing that could change
                    // it has: no git recovery, no model call.
                    Some(c) if basis.as_deref() == Some(c.basis.as_str()) => {
                        (CANNOT, Some(c.reason), 0, true, Some(iri))
                    }
                    _ => {
                        let unjudgeable = |u: Unjudgeable| -> Result<_> {
                            // Archived only when it is lasting AND the basis it
                            // was found under is known — else the next read
                            // looks again.
                            let record = match (&basis, u.lasting) {
                                (Some(basis), true) => {
                                    store_cannot(
                                        &self.config.archive,
                                        &finding.iri(),
                                        &Cannot {
                                            iri: iri.clone(),
                                            reason: u.reason.clone(),
                                            basis: basis.clone(),
                                            tag: tag.clone(),
                                            model: model.clone(),
                                            at: now(),
                                        },
                                    )?;
                                    Some(iri.clone())
                                }
                                _ => None,
                            };
                            Ok((CANNOT, Some(u.reason), 0, false, record))
                        };
                        match self.reviewed(&finding, root, current) {
                            Err(u) => unjudgeable(u)?,
                            Ok(reviewed) => {
                                answer["content"] = reviewed.content.into();
                                answer["anchor"] = reviewed.anchor.into();
                                answer["commit"] = reviewed.commit.clone().into();
                                answer["content_hash"] = reviewed.hash.clone().into();
                                let claim = Claim {
                                    severity: finding.severity.as_deref(),
                                    quote: &finding.exact,
                                    body: &finding.body,
                                };
                                let files = test_files(root, &self.config.ignore);
                                match prepare(
                                    &finding.rel,
                                    &reviewed.text,
                                    reviewed.char_start,
                                    &claim,
                                    &files,
                                ) {
                                    None => unjudgeable(Unjudgeable::lasting(format!(
                                        "its quote is not at character {} of the reviewed \
                                         version",
                                        reviewed.char_start
                                    )))?,
                                    Some(prepared) => {
                                        let (raw, parsed) =
                                            ask(inv, &self.config, &provider, &prepared).await?;
                                        let verdict = Verdict {
                                            iri: iri.clone(),
                                            verdict: verdict_of(
                                                &parsed.answers,
                                                parsed.stated.as_deref(),
                                            )
                                            .to_string(),
                                            stated: parsed.stated,
                                            tag: tag.clone(),
                                            model: model.clone(),
                                            judged_at: now(),
                                            test_code: prepared.test_code,
                                            answers: parsed.answers,
                                            raw,
                                        };
                                        store_verdict(
                                            &self.config.archive,
                                            &finding.iri(),
                                            &verdict,
                                        )?;
                                        answer["judge"] = verdict_json(&verdict);
                                        ("judged", None, 1, false, Some(iri))
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };
        answer["status"] = status.into();
        answer["reason"] = reason.clone().into();
        answer["calls"] = calls.into();
        answer["archived"] = from_archive.into();
        answer["record"] = record.into();
        if inv
            .inline_str("as")
            .is_ok_and(|a| a.starts_with("text/plain"))
        {
            let line = match (status, &reason) {
                (CANNOT, Some(why)) => format!("cannot judge {}: {why}\n", finding.iri()),
                _ => format!(
                    "{status} {} ({tag}) {}\n",
                    answer["judge"]["verdict"].as_str().unwrap_or(""),
                    finding.iri()
                ),
            };
            return Ok(repr_utf8("text/plain", line));
        }
        Ok(crate::repr(
            "application/json",
            serde_json::to_string(&answer).unwrap_or_default(),
        ))
    }

    fn name(&self) -> &str {
        "browse-judge-finding"
    }

    fn describe(&self) -> Description {
        judge_finding_description(&self.config)
    }
}

fn judge_finding_description(config: &ExplainConfig) -> Description {
    Description::new("browse-judge-finding")
        .title("Judge one queued finding, archived")
        .summary(
            "Run the judge on ONE finding already in the review queue (urn:iki:finding:{id}) and \
             ARCHIVE the verdict on it, exactly as a review pass archives the verdicts it makes \
             (judge / judges on the finding's json row) — so a host can backfill verdicts on \
             findings minted before the judge existed. Idempotent: a finding that already \
             carries a verdict under this judge's tag answers it with no model call \
             (status archived, calls 0); Exists answers whether it does, without judging. \
             Judged against the version the finding's pass REVIEWED: the current file when it \
             still has those bytes, else that version recovered from the repository's git \
             history by its sha256 — and when it cannot be recovered (never committed, a \
             pull-request diff), the answer says so (status cannot, with the reason). A \
             cannot is archived at the verdict IRI with its reason (never as a verdict: not \
             on the finding's judge/judges, and Exists stays false) and a re-read answers it \
             from the archive; it is retried when the judge tag changes or its search basis \
             does (the file's current content and the commits that touch its path), and a \
             failure to read git history is never archived. With no judge configured and no \
             provider= named, Source REFUSES (a conflict: no judge is configured) and never \
             judges with the review tier; Exists answers false. Nothing is dropped, withheld \
             or re-rated by a verdict. application/json (default): {finding, repo, path, tag, \
             status (judged|archived|cannot), calls, reason, content (current|recovered), \
             anchor, commit, content_hash, judge (the verdict object, or null), archived (read \
             from the archive), record (the IRI it is archived at, or null)}; text/plain: one \
             line.",
        )
        .verb(Verb::Source)
        .verb(Verb::Exists)
        .verb(Verb::Meta)
        .requires(CAP_WILDCARD)
        .requires(CAP_NET)
        .input(
            ArgSpec::new("id")
                .binding()
                .class(crate::XSD_STRING)
                .summary("the finding id (urn:iki:finding:{id}), a finding on this repo"),
        )
        .input(
            ArgSpec::new("provider")
                .optional()
                .class("http://www.w3.org/2001/XMLSchema#anyURI")
                .summary(
                    "the LLM provider IRI to judge with instead of the configured judge \
                     provider (its own tag, so its verdict sits beside the default's); one_of \
                     is what this host allows. Required when no judge is configured: there is \
                     no fallback to the review tier",
                )
                .one_of(config.selectable()),
        )
        .input(
            ArgSpec::new("as")
                .optional()
                .class(crate::XSD_STRING)
                .summary("the face to render")
                .one_of(["application/json", "text/plain"])
                .default_value("application/json"),
        )
        .output("application/json")
        .output("text/plain;charset=utf-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST: &str = "\
//! A module.

/// Adds one.
///
/// ⚠ Panics on overflow, by design.
pub fn add_one(x: u8) -> u8 {
    // The caller checked.
    x + 1 // overflow is the caller's
}

impl Thing {
    fn other(&self) {
        let s = \"{ not a brace\";
        let c = '{';
        call(s, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds() {
        let v = add_one(1);
        assert_eq!(v, 2);
        add_one(255).expect(\"boom\");
    }
}
";

    fn at(text: &str, quote: &str) -> (usize, usize) {
        let s = text.find(quote).unwrap();
        (s, s + quote.len())
    }

    #[test]
    fn item_names_are_read_from_declarations_only() {
        assert_eq!(
            item_name("pub fn add_one(x: u8) -> u8 {").as_deref(),
            Some("add_one")
        );
        assert_eq!(
            item_name("    pub(crate) async fn go() {").as_deref(),
            Some("go")
        );
        assert_eq!(item_name("impl Thing {").as_deref(), Some("impl Thing"));
        assert_eq!(
            item_name("impl<T> Foo for Bar<T> {").as_deref(),
            Some("impl<T> Foo for Bar<T>")
        );
        assert_eq!(item_name("def test_x(self):").as_deref(), Some("test_x"));
        assert_eq!(item_name("my-func() {").as_deref(), Some("my-func"));
        assert_eq!(
            item_name("const LIMIT: usize = 4;").as_deref(),
            Some("LIMIT")
        );
        assert_eq!(item_name("    let f = fn_like();"), None);
        assert_eq!(item_name("    implement();"), None);
        assert_eq!(item_name("// fn commented() {}"), None);
    }

    #[test]
    fn the_site_is_the_whole_enclosing_function_with_its_doc_and_comments() {
        let (s, e) = at(RUST, "x + 1");
        let st = site("src/lib.rs", RUST, s, e);
        assert_eq!(st.item.as_deref(), Some("add_one"));
        // The doc block starts at line 3, the closing brace is line 9.
        assert_eq!((st.first_line, st.last_line), (3, 9));
        assert_eq!((st.quote_first, st.quote_last), (8, 8));
        assert!(st.comments.iter().any(|c| c.contains("Panics on overflow")));
        assert!(st.comments.iter().any(|c| c == "// The caller checked."));
        assert!(st.comments.iter().any(|c| c == "overflow is the caller's"));
        assert_eq!(st.test_code, None);
    }

    #[test]
    fn braces_in_strings_and_char_literals_do_not_end_an_item() {
        let (s, e) = at(RUST, "call(s, c);");
        let st = site("src/lib.rs", RUST, s, e);
        assert_eq!(st.item.as_deref(), Some("other"));
        assert_eq!((st.first_line, st.last_line), (12, 16));
    }

    #[test]
    fn multi_line_and_raw_strings_hide_their_braces() {
        let text = "fn a() {\n    let s = \"one {\n two\";\n    let r = r#\"}\"# ;\n    call();\n}\nfn b() {}\n";
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(brace_end(&lines, 0, Lang::Brace), Some(5));
        let (s, e) = at(text, "call();");
        assert_eq!(site("src/a.rs", text, s, e).item.as_deref(), Some("a"));
    }

    #[test]
    fn a_cfg_test_module_and_a_test_path_are_test_code() {
        let (s, e) = at(RUST, "add_one(255)");
        let st = site("src/lib.rs", RUST, s, e);
        assert_eq!(st.item.as_deref(), Some("adds"));
        assert!(st.test_code.is_some());
        let (s, e) = at(RUST, "x + 1");
        assert!(site("tests/it.rs", RUST, s, e).test_code.is_some());
        assert!(is_test_path("crates/a/tests/x.rs"));
        assert!(is_test_path("src/thing_test.go"));
        assert!(!is_test_path("src/testing.rs"));
    }

    #[test]
    fn prose_gets_a_window_not_an_item() {
        let text: String = (1..=200).map(|n| format!("line {n}\n")).collect();
        let (s, e) = at(&text, "line 100\n");
        let st = site("README.md", &text, s, e - 1);
        assert_eq!(st.item, None);
        assert_eq!((st.first_line, st.last_line), (60, 140));
    }

    #[test]
    fn symbols_prefer_the_item_and_specific_identifiers() {
        let s = symbols(
            "bursts.apply(std::slice::from_mut(ann));",
            Some("included_for_ids"),
        );
        assert_eq!(s[0], "included_for_ids");
        assert!(s.contains(&"from_mut".to_string()));
        assert!(!s.contains(&"std".to_string()));
        assert!(symbols("let x = 1;", None).is_empty());
        // Prose in a quoted comment names nothing a test would.
        assert!(symbols("the store is ONLY read once", None).is_empty());
        assert!(
            symbols("parse(kernel, stream);", None).is_empty(),
            "a short call is too common"
        );
        assert_eq!(
            symbols("dispatch(kernel);", None),
            vec!["dispatch".to_string()]
        );
    }

    #[test]
    fn mentions_name_the_test_and_its_asserting_lines() {
        let files = vec![RepoFile {
            rel: "src/lib.rs".to_string(),
            text: RUST.to_string(),
        }];
        let found = mentions(&files, &["add_one".to_string()], "src/lib.rs");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].test, "adds");
        assert_eq!(found[0].asserts, vec!["assert_eq!(v, 2);".to_string()]);
        // A function outside the test region that mentions it is not a test.
        assert!(mentions(&files, &["call".to_string()], "src/lib.rs").is_empty());
    }

    #[test]
    fn answers_parse_tolerantly_and_the_rule_decides() {
        let p = parse(
            "**CODE:** no - the operator does not panic.\n\
             DISCLOSED: no — nothing says so\n\
             - OCCURS: n/a - no failure predicted\n\
             TEST: maybe - who knows\n\
             VERDICT: refuted\n",
        );
        assert_eq!(p.stated.as_deref(), Some("refuted"));
        let words: Vec<(&str, &str)> = p
            .answers
            .iter()
            .map(|a| (a.question.as_str(), a.answer.as_str()))
            .collect();
        assert_eq!(
            words,
            vec![
                ("code", "no"),
                ("disclosed", "no"),
                ("occurs", "n/a"),
                ("test", "unclear")
            ]
        );
        assert_eq!(p.answers[0].reason, "the operator does not panic.");
        // `code: no` alone is not a refutation (see `verdict_of`).
        assert_eq!(rule(&p), "unsure");
        assert_eq!(rule(&parse("CODE: yes - x\nOCCURS: no - y")), "refuted");
        let ok = parse("CODE: yes - x\nDISCLOSED: no - x\nOCCURS: yes - x\nTEST: no - x\n");
        assert_eq!(rule(&ok), "confirmed");
        let half = parse("CODE: yes - x\nOCCURS: unclear - x\n");
        assert_eq!(rule(&half), "unsure");
        assert_eq!(rule(&parse("DISCLOSED: yes - it says so")), "refuted");
    }

    fn rule(p: &Parsed) -> &'static str {
        verdict_of(&p.answers, p.stated.as_deref())
    }

    /// Claim 1 of the judge-v2 brief (ledger #483, findings sweep 2): judge-v1
    /// CONFIRMED with `occurs: n/a` — the judge itself saying the claim predicts
    /// no consequence — on 12 findings sweep 2 then showed were not defects, and
    /// on no real one. A claim with no consequence to check has nothing to
    /// confirm: it is `unsure`.
    #[test]
    fn occurs_n_a_never_confirms() {
        let p = parse(
            "CODE: yes - the text says so\nDISCLOSED: no - nothing warns\n\
             OCCURS: n/a - a documentation inconsistency, no runtime consequence\n\
             TEST: no - not test code\nVERDICT: confirmed",
        );
        assert_eq!(rule(&p), "unsure");
        // Without the VERDICT line too: the rule alone decides.
        let bare = parse("CODE: yes - x\nDISCLOSED: no - x\nOCCURS: n/a - x\nTEST: no - x");
        assert_eq!(rule(&bare), "unsure");
    }

    /// Claim 2 (the machine-checkable half): an answer whose own VERDICT line
    /// says the claim does NOT hold is not a confirmation, however its four
    /// answers read. judge-v1 confirmed four sweep-2 findings whose model said
    /// `refuted` or `unsure` beside four supporting answers; all four were not
    /// defects. The answers still refute on their own terms (`stated` never
    /// overrides a refutation, and never confirms by itself).
    #[test]
    fn a_stated_dissent_turns_a_confirmation_into_unsure() {
        let all_yes = "CODE: yes - x\nDISCLOSED: no - x\nOCCURS: yes - x\nTEST: no - x\n";
        for stated in ["refuted", "unsure"] {
            let p = parse(&format!("{all_yes}VERDICT: {stated}"));
            assert_eq!(rule(&p), "unsure", "stated {stated}");
        }
        assert_eq!(
            rule(&parse(&format!("{all_yes}VERDICT: confirmed"))),
            "confirmed"
        );
        // No VERDICT line at all: the answers decide, as before.
        assert_eq!(rule(&parse(all_yes)), "confirmed");
        // A refutation stands whatever the model's own word says.
        let refuting = parse("CODE: yes - x\nOCCURS: no - y\nVERDICT: confirmed");
        assert_eq!(rule(&refuting), "refuted");
    }

    #[test]
    fn the_prompt_marks_the_quote_and_never_mentions_decisions() {
        let (s, e) = at(RUST, "x + 1");
        let st = site("src/lib.rs", RUST, s, e);
        let claim = Claim {
            severity: Some("critical"),
            quote: "x + 1",
            body: "This overflows.",
        };
        let p = prompt(
            "src/lib.rs",
            RUST,
            &st,
            &[],
            &["add_one".to_string()],
            &claim,
        );
        assert!(p.contains(">    8 |     x + 1 // overflow is the caller's"));
        assert!(p.contains("CLAIM: This overflows."));
        assert!(p.contains("Test code: no"));
        // judge-v2's OCCURS: a concrete trigger, and a returned error is not a
        // failure (measured on findings sweep 2; see JUDGE_PROMPT_VERSION).
        assert!(p.contains("name one concrete input this code can receive"));
        assert!(p.contains("is the code working, not a failure"));
        for word in crate::finding::DECLINE_REASONS {
            assert!(
                !p.contains(word),
                "the judge prompt names the decline word `{word}`"
            );
        }
    }

    #[test]
    fn a_verdict_round_trips_through_the_store() {
        let store = Arc::new(oxigraph::store::Store::new().unwrap());
        let archive = Archive::new(store, oxigraph::model::GraphName::DefaultGraph);
        let finding = "urn:iki:finding:abc";
        let parsed = parse(
            "CODE: no - x\nDISCLOSED: no - y\nOCCURS: n/a - z\nTEST: yes - w\nVERDICT: refuted",
        );
        let v = Verdict {
            iri: verdict_iri(finding, "judge-v1@m:1"),
            verdict: verdict_of(&parsed.answers, parsed.stated.as_deref()).to_string(),
            stated: parsed.stated.clone(),
            tag: "judge-v1@m:1".to_string(),
            model: "m:1".to_string(),
            judged_at: Some("2026-10-01T00:00:00Z".to_string()),
            test_code: true,
            answers: parsed.answers,
            raw: "the raw answer".to_string(),
        };
        store_verdict(&archive, finding, &v).unwrap();
        assert_eq!(load_verdicts(&archive, finding).unwrap(), vec![v.clone()]);
        let json = verdict_json(&v);
        assert_eq!(json["verdict"], "refuted");
        assert_eq!(json["answers"]["occurs"]["answer"], "n/a");
        assert_eq!(json["test_code"], true);
        // The Turtle face parses.
        let doc = format!(
            "@prefix prov: <{PROV}> .\n@prefix dcterms: <{DCTERMS}> .\n@prefix ik: <{}> .\n\
             @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n{}",
            crate::explain::IK,
            verdict_turtle(finding, &v)
        );
        let parser = oxttl::TurtleParser::new();
        for triple in parser.for_slice(doc.as_bytes()) {
            triple.expect("valid turtle");
        }
    }
}

/// The judge-one-finding resource end to end (ledger #696): a real git
/// repository, a review pass with the judge OFF (a queue that predates it),
/// then the judge asked about one queued finding.
#[cfg(test)]
mod finding_tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{Capability, Exact, Fallback, FnEndpoint, Iri, Kernel};
    use oxigraph::store::Store;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    const PROVIDER: &str = "urn:llm:coder:ask";
    /// The first version: the claim's quote, inside an item whose comment
    /// names the version, so a prompt says which version it was built from.
    const V1: &str = "/// Adds.\npub fn alpha() {\n    // version one\n    let x = 1;\n}\n";
    /// The second: lines inserted ABOVE, so every offset moves, and the
    /// comment changed, so a prompt built from it is told apart.
    const V2: &str =
        "// a header\n// another\n/// Adds.\npub fn alpha() {\n    // version two\n    \
                      let x = 1;\n}\n";
    const REVIEW: &str = "QUOTE: let x = 1;\nSEVERITY: critical\nNOTE: This overflows.\n";
    const JUDGE: &str = "CODE: no - it assigns a constant.\nDISCLOSED: no - nothing says so.\n\
                         OCCURS: no - a constant cannot overflow.\nTEST: no - library code.\n\
                         VERDICT: refuted\n";

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-browse-judge-finding-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `git -C dir` with a throwaway identity, asserting success.
    fn git(dir: &Path, args: &[&str]) {
        let mut all = vec![
            "-C",
            dir.to_str().unwrap(),
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
        ];
        all.extend(args);
        let out = std::process::Command::new("git")
            .args(&all)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(dir: &Path, text: &str, message: &str) {
        std::fs::write(dir.join("a.rs"), text).unwrap();
        git(dir, &["add", "a.rs"]);
        git(dir, &["commit", "-m", message]);
    }

    /// Every prompt the fake model was asked, with its system prompt.
    #[derive(Default)]
    struct Log(Mutex<Vec<(String, String)>>);

    impl Log {
        fn judge_calls(&self) -> Vec<String> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, system)| system.starts_with("You check one claim"))
                .map(|(prompt, _)| prompt.clone())
                .collect()
        }
    }

    /// A kernel over `root` whose model answers a review prompt with
    /// [`REVIEW`] and a judge prompt with [`JUDGE`]; `judge` says whether the
    /// review pass judges what it mints (off = a queue from before the judge).
    fn kernel(root: &Path, store: &Arc<Store>, log: &Arc<Log>, judge: bool) -> Kernel {
        let log = Arc::clone(log);
        let llm = EndpointSpace::new().bind(
            Exact::new(PROVIDER),
            FnEndpoint::new("fake-llm", move |inv: &Invocation<'_>| {
                let system = inv.inline_str("system").unwrap_or("").to_string();
                let prompt = inv.inline_str("prompt").unwrap_or("").to_string();
                log.0.lock().unwrap().push((prompt, system.clone()));
                let reply = match system.starts_with("You check one claim") {
                    true => JUDGE,
                    false => REVIEW,
                };
                Ok(repr_utf8("text/plain", reply.to_string()))
            })
            .with_description(
                Description::new("fake-llm")
                    .verb(Verb::Source)
                    .requires(CAP_NET),
            ),
        );
        let mut cfg = ExplainConfig::new(Arc::clone(store)).review_model_label("r1");
        if !judge {
            cfg = cfg.no_judge();
        }
        let browse = crate::space_with_explain(vec![("demo".to_string(), root.to_path_buf())], cfg);
        Kernel::new(Arc::new(Fallback::new(vec![
            Arc::new(browse),
            Arc::new(llm),
        ])))
    }

    fn cap() -> Capability {
        Capability::scoped(["urn:cap:browse:read:demo", "urn:cap:net:localhost"])
    }

    fn issue(k: &Kernel, verb: Verb, iri: &str, args: &[(&str, &str)]) -> Result<String> {
        let mut request = Request::new(verb, Iri::parse(iri.to_string()).unwrap());
        for (name, value) in args {
            request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
        }
        block_on(k.issue(request, &cap())).map(|r| String::from_utf8_lossy(&r.bytes).to_string())
    }

    fn json(k: &Kernel, iri: &str) -> serde_json::Value {
        serde_json::from_str(&issue(k, Verb::Source, iri, &[("as", "application/json")]).unwrap())
            .unwrap()
    }

    /// The review pass over `a.rs` with the judge OFF, and its one finding's id.
    fn queued(root: &Path, store: &Arc<Store>, log: &Arc<Log>) -> String {
        let k = kernel(root, store, log, false);
        let pass = json(&k, "urn:repo:demo:review:a.rs");
        let minted = pass["minted"].as_array().unwrap();
        assert_eq!(minted.len(), 1, "{pass}");
        let iri = minted[0].as_str().unwrap();
        assert!(json(&k, iri)["judge"].is_null(), "the pass judged nothing");
        assert!(log.judge_calls().is_empty());
        iri.strip_prefix("urn:iki:finding:").unwrap().to_string()
    }

    /// ★★ A queued finding is judged ONCE: the first read asks the model and
    /// archives the verdict on the finding (its row's `judge`), the second is
    /// an archive hit with no call, and Exists says which without judging.
    #[test]
    fn a_queued_finding_is_judged_once_and_a_reread_costs_no_call() {
        let root = temp_dir();
        git(&root, &["init", "-q"]);
        commit(&root, V1, "one");
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let id = queued(&root, &store, &log);
        let k = kernel(&root, &store, &log, true);
        let resource = format!("urn:repo:demo:judge-finding:{id}");

        assert_eq!(issue(&k, Verb::Exists, &resource, &[]).unwrap(), "false");
        assert!(log.judge_calls().is_empty(), "Exists never asks the model");

        let first = json(&k, &resource);
        assert_eq!(first["status"], "judged", "{first}");
        assert_eq!(first["calls"], 1);
        assert_eq!(first["content"], "current");
        assert_eq!(first["anchor"], "recorded");
        assert_eq!(first["judge"]["verdict"], "refuted");
        assert_eq!(first["judge"]["tag"], "judge-v2@r1");
        assert_eq!(first["tag"], "judge-v2@r1");
        assert_eq!(log.judge_calls().len(), 1);
        assert!(log.judge_calls()[0].contains("QUOTE: let x = 1;"));

        // Attached to the finding exactly as a pass-minted verdict is.
        let row = json(&k, &format!("urn:iki:finding:{id}"));
        assert_eq!(row["judge"]["verdict"], "refuted", "{row}");
        assert_eq!(row["judges"].as_array().unwrap().len(), 1);
        assert_eq!(row["state"], "pending", "nothing is decided by a verdict");

        let again = json(&k, &resource);
        assert_eq!(again["status"], "archived", "{again}");
        assert_eq!(again["calls"], 0);
        assert_eq!(again["judge"], first["judge"]);
        assert_eq!(log.judge_calls().len(), 1, "the re-read cost no call");
        assert_eq!(issue(&k, Verb::Exists, &resource, &[]).unwrap(), "true");

        let plain = issue(&k, Verb::Source, &resource, &[("as", "text/plain")]).unwrap();
        assert!(
            plain.starts_with("archived refuted (judge-v2@r1)"),
            "{plain}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ When the file has MOVED, the judge reads the version the reviewer
    /// saw, recovered from git history by its hash, at the offset the id was
    /// minted at — after a read has re-anchored the stored selector onto the
    /// new version, so the id is the only record of the old offset.
    #[test]
    fn a_moved_file_is_judged_against_its_recovered_version() {
        let root = temp_dir();
        git(&root, &["init", "-q"]);
        commit(&root, V1, "one");
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let id = queued(&root, &store, &log);
        commit(&root, V2, "two");
        let k = kernel(&root, &store, &log, true);
        // A read re-anchors it onto V2 (the drift pass every read runs).
        let row = json(&k, &format!("urn:iki:finding:{id}"));
        assert_eq!(row["reanchored"], true, "{row}");

        let answer = json(&k, &format!("urn:repo:demo:judge-finding:{id}"));
        assert_eq!(answer["status"], "judged", "{answer}");
        assert_eq!(answer["content"], "recovered");
        assert_eq!(answer["anchor"], "minted");
        assert_eq!(
            answer["content_hash"],
            annotate::content_hash(V1.as_bytes())
        );
        assert_eq!(answer["commit"].as_str().unwrap().len(), 40);
        let prompts = log.judge_calls();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("version one"), "{}", prompts[0]);
        assert!(!prompts[0].contains("version two"), "{}", prompts[0]);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A finding whose reviewed bytes were never committed, once the file has
    /// moved on, cannot be judged — and the answer SAYS so, as a value, with
    /// no call. (It is archived, and retried when its basis changes: see
    /// `a_cannot_is_archived_survives_a_restart_and_is_retried_when_history_changes`.)
    /// An unknown id and a finding on another repo are not found.
    #[test]
    fn a_finding_whose_version_cannot_be_recovered_says_so() {
        let root = temp_dir();
        git(&root, &["init", "-q"]);
        commit(&root, "// something else\n", "zero");
        std::fs::write(root.join("a.rs"), V1).unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let id = queued(&root, &store, &log);
        commit(&root, V2, "two");
        let k = kernel(&root, &store, &log, true);
        let resource = format!("urn:repo:demo:judge-finding:{id}");

        let answer = json(&k, &resource);
        assert_eq!(answer["status"], "cannot", "{answer}");
        assert_eq!(answer["calls"], 0);
        assert!(answer["judge"].is_null());
        let reason = answer["reason"].as_str().unwrap();
        assert!(
            reason.contains("neither the current file nor any committed version"),
            "{reason}"
        );
        assert!(log.judge_calls().is_empty(), "no call");
        assert_eq!(issue(&k, Verb::Exists, &resource, &[]).unwrap(), "false");
        let plain = issue(&k, Verb::Source, &resource, &[("as", "text/plain")]).unwrap();
        assert!(
            plain.starts_with("cannot judge urn:iki:finding:"),
            "{plain}"
        );

        let err = issue(&k, Verb::Source, "urn:repo:demo:judge-finding:nope", &[]).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_pass_hash_and_the_minted_offset_are_read_exactly() {
        let hash = format!("sha256:{}", "a".repeat(64));
        let pass = crate::review::pass_iri("demo", "src/a.rs", &hash, "review-v5@m:30b");
        assert_eq!(pass_hash(&pass, "demo"), Some(hash.clone()));
        assert_eq!(pass_hash(&pass, "other"), None);
        assert_eq!(pass_hash("urn:x", "demo"), None);
        // Two occurrences: only the one whose offset reproduces the id.
        let text = "é x\nx\n";
        let second = 4; // characters, not bytes: `é` is two bytes
        let id = annotate::finding_id(&pass, second, "x");
        assert_eq!(minted_offset(text, &pass, "x", &id), Some(second));
        assert_eq!(minted_offset(text, &pass, "x", "nope"), None);
    }

    /// Ledger #736: a finding minted at an OVERLAPPING occurrence is found
    /// again (`match_indices` skipped it, so the judge could not place it).
    #[test]
    fn the_minted_offset_of_an_overlapping_occurrence_is_found() {
        let hash = format!("sha256:{}", "a".repeat(64));
        let pass = crate::review::pass_iri("demo", "src/a.rs", &hash, "review-v5@m:30b");
        let text = "aaaa";
        let id = annotate::finding_id(&pass, 1, "aa");
        assert_eq!(minted_offset(text, &pass, "aa", &id), Some(1));
    }

    /// ★★ With NO judge configured, judge-finding REFUSES (ledger #702,
    /// item 2) — typed, saying why — and never judges with the REVIEW tier in
    /// its place. Before, it fell back to `review_provider` silently, so a
    /// host that had turned the judge off got verdicts it had not asked for,
    /// archived under a tag that looks like a judge's. Exists answers `false`:
    /// there is no verdict by a judge that does not exist. A provider NAMED
    /// with `provider=` is an explicit choice, and is honored.
    #[test]
    fn with_no_judge_configured_judge_finding_refuses_and_never_asks_the_review_tier() {
        let root = temp_dir();
        git(&root, &["init", "-q"]);
        commit(&root, V1, "one");
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let id = queued(&root, &store, &log);
        let k = kernel(&root, &store, &log, false);
        let resource = format!("urn:repo:demo:judge-finding:{id}");

        let err = issue(&k, Verb::Source, &resource, &[]).unwrap_err();
        assert!(matches!(err, Error::Conflict(_)), "{err:?}");
        assert!(err.to_string().contains("no judge is configured"), "{err}");
        assert!(log.judge_calls().is_empty(), "the review tier was asked");
        let row = json(&k, &format!("urn:iki:finding:{id}"));
        assert!(row["judges"].as_array().unwrap().is_empty(), "{row}");
        assert_eq!(issue(&k, Verb::Exists, &resource, &[]).unwrap(), "false");
        assert!(log.judge_calls().is_empty());

        let named = serde_json::from_str::<serde_json::Value>(
            &issue(&k, Verb::Source, &resource, &[("provider", PROVIDER)]).unwrap(),
        )
        .unwrap();
        assert_eq!(named["status"], "judged", "{named}");
        assert_eq!(log.judge_calls().len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★★ A `cannot` is ARCHIVED (ledger #702, item 3) at the verdict IRI,
    /// with its reason, so it survives a restart and a re-read answers it
    /// from the archive. It is RETRIED when what could change the answer
    /// changes: the judge tag (the IRI), or the search basis — the file's
    /// current bytes and the commits that touch the path. Here the reviewed
    /// bytes are committed later, on a side branch, and the retry judges.
    #[test]
    fn a_cannot_is_archived_survives_a_restart_and_is_retried_when_history_changes() {
        let root = temp_dir();
        git(&root, &["init", "-q"]);
        commit(&root, "// something else\n", "zero");
        std::fs::write(root.join("a.rs"), V1).unwrap();
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let id = queued(&root, &store, &log);
        commit(&root, V2, "two");
        let resource = format!("urn:repo:demo:judge-finding:{id}");
        let record = verdict_iri(&format!("urn:iki:finding:{id}"), "judge-v2@r1");

        let k = kernel(&root, &store, &log, true);
        let first = json(&k, &resource);
        assert_eq!(first["status"], "cannot", "{first}");
        let subject = NamedNode::new(&record).unwrap();
        assert!(
            store
                .quads_for_pattern(Some(subject.as_ref().into()), None, None, None)
                .next()
                .is_some(),
            "nothing archived at {record}"
        );
        assert_eq!(first["archived"], false, "{first}");
        assert_eq!(first["record"], record.as_str(), "{first}");

        // A restart: a new kernel over the same store.
        let k = kernel(&root, &store, &log, true);
        let again = json(&k, &resource);
        assert_eq!(again["status"], "cannot", "{again}");
        assert_eq!(again["archived"], true, "{again}");
        assert_eq!(again["calls"], 0);
        assert_eq!(again["reason"], first["reason"]);
        assert!(log.judge_calls().is_empty());
        // Not a verdict: Exists and the finding's row say there is none.
        assert_eq!(issue(&k, Verb::Exists, &resource, &[]).unwrap(), "false");
        let row = json(&k, &format!("urn:iki:finding:{id}"));
        assert!(row["judge"].is_null(), "{row}");
        assert!(row["decision"].is_null(), "{row}");

        // The history changes: the reviewed bytes land on a side branch.
        git(&root, &["checkout", "-q", "-b", "side"]);
        commit(&root, V1, "one, late");
        git(&root, &["checkout", "-q", "-"]);
        let retried = json(&k, &resource);
        assert_eq!(retried["status"], "judged", "{retried}");
        assert_eq!(retried["content"], "recovered");
        assert_eq!(retried["record"], record.as_str());
        assert_eq!(log.judge_calls().len(), 1);
        let row = json(&k, &format!("urn:iki:finding:{id}"));
        assert_eq!(row["judge"]["verdict"], "refuted", "{row}");
        assert_eq!(row["judges"].as_array().unwrap().len(), 1, "{row}");
        let reread = json(&k, &resource);
        assert_eq!(reread["status"], "archived", "{reread}");
        assert_eq!(log.judge_calls().len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    /// ★ The verdict words are a CONTRACT `one_of` (ledger #702, item 1):
    /// read the way gonk reads its menus — Meta on the findings face, the
    /// `verdict` input — with no prose parsed, and the same words the rule
    /// returns. `verdict=` narrows the rows to the findings whose latest
    /// verdict is that word; an unjudged finding matches none; a word outside
    /// the set is refused naming it.
    #[test]
    fn the_verdict_words_are_a_contract_one_of_and_filter_the_queue() {
        let root = temp_dir();
        git(&root, &["init", "-q"]);
        commit(&root, V1, "one");
        let store = Arc::new(Store::new().unwrap());
        let log = Arc::new(Log::default());
        let id = queued(&root, &store, &log);
        let k = kernel(&root, &store, &log, true);

        let description = k
            .describe(&Iri::parse("urn:repo:demo:findings".to_string()).unwrap())
            .expect("the findings face describes itself");
        let source = description
            .action_specs()
            .into_iter()
            .find(|s| s.verb == Verb::Source)
            .expect("a Source action");
        let verdict = source
            .inputs
            .iter()
            .find(|i| i.name == "verdict")
            .expect("verdict is declared");
        assert_eq!(verdict.one_of, ["confirmed", "refuted", "unsure"]);
        assert_eq!(verdict.one_of, VERDICTS);
        assert!(!verdict.required);
        assert!(!verdict.one_of.iter().any(|w| w == CANNOT));

        let rows = |word: &str| -> Vec<serde_json::Value> {
            serde_json::from_str::<serde_json::Value>(
                &issue(
                    &k,
                    Verb::Source,
                    "urn:repo:demo:findings",
                    &[("verdict", word)],
                )
                .unwrap(),
            )
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
        };
        assert!(rows(REFUTED).is_empty(), "nothing is judged yet");
        let judged = json(&k, &format!("urn:repo:demo:judge-finding:{id}"));
        assert_eq!(judged["judge"]["verdict"], REFUTED, "{judged}");
        let refuted = rows(REFUTED);
        assert_eq!(refuted.len(), 1, "{refuted:?}");
        assert_eq!(refuted[0]["id"], id.as_str());
        assert!(rows(CONFIRMED).is_empty());
        assert!(rows(UNSURE).is_empty());
        let all = json(&k, "urn:repo:demo:findings");
        assert_eq!(all.as_array().unwrap().len(), 1, "omitted = every row");

        let err = issue(
            &k,
            Verb::Source,
            "urn:repo:demo:findings",
            &[("verdict", CANNOT)],
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidArgument { name, .. } if name == "verdict"),
            "{err:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
