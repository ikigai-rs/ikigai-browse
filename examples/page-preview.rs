//! Render the browse faces to a standalone HTML file, so a human can LOOK at
//! them — the check a green test suite cannot make.
//!
//!   cargo run --example page-preview [out.html]
//!
//! Writes `browse-preview.html` (or the path given) and prints where. Open it
//! in a browser, and again with the OS in dark mode: the whole point of the
//! two stylesheets is that both schemes read.
//!
//! It mounts THIS checkout, resolves the real tree, file, annotations and
//! review-menu faces through the kernel, and inlines the two stylesheets
//! resources — `urn:repo:style` (the syntax theme) and `urn:repo:style:layout`
//! (the page furniture). Nothing here writes markup of its own beyond the page
//! shell and the section headings, so what you see is what a door serves.
//!
//! ⚠ **The htmx affordances are inert in a file.** Every navigation here is an
//! `hx-get` against a host's `/k/` route; with no host, buttons do nothing and
//! the lazily-loaded blocks (the disclosure menus' bodies, a tree's recent-PR
//! listing) stay on their placeholder. The menu PANEL is therefore resolved
//! and inlined separately below, so its rules are visible too. The
//! pull-request faces are not rendered at all: they resolve `urn:repo:pr:*`
//! through the kernel and nothing mounts those here.

use std::path::PathBuf;
use std::sync::Arc;

use ikigai_browse::{ExplainConfig, Mount, CAP_ANNOTATE};
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use oxigraph::store::Store;

fn main() {
    let out = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("browse-preview.html"), PathBuf::from);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let store = Arc::new(Store::new().expect("an in-memory store"));
    let space = Mount::new([("self".to_string(), root)])
        .annotations(Arc::clone(&store))
        // The explain family mounted is what makes the file face render its
        // action strip and both disclosure menus — the richest markup here.
        .explain(ExplainConfig::new(Arc::clone(&store)))
        // No config home: the themes below are the BUILT-IN ones, not whatever
        // `a11y.toml` this machine happens to hold.
        .config_home(None)
        .space();
    let kernel = Kernel::new(Arc::new(space));
    let cap = Capability::scoped([
        "urn:cap:browse:read:self",
        CAP_ANNOTATE,
        "urn:cap:net:localhost",
    ]);

    // One real human annotation, so a card and its inline line marker render.
    let mint = Request::new(Verb::Sink, iri("urn:iki:annotation"))
        .with_arg("target", inline("urn:repo:self:file:src/layout.rs"))
        .with_arg("exact", inline("pub const LAYOUT_IRI"))
        .with_arg(
            "content",
            inline("The sibling of urn:repo:style. This note is real: it was minted through the kernel and anchored in the file above."),
        );
    if let Err(e) = futures::executor::block_on(kernel.issue(mint, &cap)) {
        eprintln!("could not mint the sample annotation: {e:?}");
    }

    let style = source(&kernel, &cap, "urn:repo:style", &[]);
    let layout = source(&kernel, &cap, "urn:repo:style:layout", &[]);

    let sections: Vec<(&str, String)> = vec![
        (
            "tree face — urn:repo:self:tree",
            source(&kernel, &cap, "urn:repo:self:tree", &[("as", "text/html")]),
        ),
        (
            "file face — urn:repo:self:file:src/layout.rs (annotations included)",
            source(
                &kernel,
                &cap,
                "urn:repo:self:file:src/layout.rs",
                &[("as", "text/html"), ("annotations", "include")],
            ),
        ),
        (
            "review menu panel — urn:repo:self:review-options:src/layout.rs",
            source(
                &kernel,
                &cap,
                "urn:repo:self:review-options:src/layout.rs",
                &[("as", "text/html")],
            ),
        ),
    ];

    let mut body = String::new();
    for (label, html) in &sections {
        body.push_str(&format!(
            "<h2 class=\"preview-label\">{label}</h2><div class=\"preview-frame\">{html}</div>"
        ));
    }
    // ⚠ The read-only posture is a DOOR's statement, and the sheet keys on it.
    // Rendered twice — the same file face, once with the attribute — so the one
    // behavioural rule in the layout sheet is visible rather than asserted.
    let readonly = source(
        &kernel,
        &cap,
        "urn:repo:self:file:src/layout.rs",
        &[("as", "text/html"), ("annotations", "include")],
    );
    body.push_str(&format!(
        "<h2 class=\"preview-label\">the same file face under \
         data-browse-posture=\"read-only\" — the create form is gone, the cards stay</h2>\
         <div class=\"preview-frame\" data-browse-posture=\"read-only\">{readonly}</div>"
    ));

    let page = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>ikigai-browse faces — preview</title>\
         <style>{style}</style><style>{layout}</style>\
         <style>:root{{color-scheme:light dark}}\
         body{{margin:0 auto;max-width:60rem;padding:1rem;\
         font:15px/1.5 -apple-system,system-ui,sans-serif}}\
         .preview-label{{margin:2rem 0 .5rem;font-size:.85rem;font-weight:600;\
         text-transform:uppercase;letter-spacing:.04em;opacity:.55}}\
         .preview-frame{{border:1px dashed rgba(128,128,128,.4);border-radius:8px;\
         padding:1rem}}</style></head><body>\
         <h1>ikigai-browse faces</h1>\
         <p>Real faces, resolved through the kernel, dressed by \
         <code>urn:repo:style</code> and <code>urn:repo:style:layout</code>. \
         The dashed frames and the labels are this example's; everything inside \
         them is what a door serves. Buttons are inert — there is no host here.</p>\
         {body}</body></html>\n"
    );
    std::fs::write(&out, page).expect("writing the preview page");
    println!("wrote {}", out.display());
}

fn iri(s: &str) -> Iri {
    Iri::parse(s.to_string()).expect("a valid IRI")
}

fn inline(value: &str) -> ArgRef {
    ArgRef::Inline(value.as_bytes().to_vec())
}

fn source(kernel: &Kernel, cap: &Capability, target: &str, args: &[(&str, &str)]) -> String {
    let mut request = Request::new(Verb::Source, iri(target));
    for (name, value) in args {
        request = request.with_arg(*name, inline(value));
    }
    match futures::executor::block_on(kernel.issue(request, cap)) {
        Ok(repr) => String::from_utf8_lossy(&repr.bytes).into_owned(),
        Err(e) => format!("<p><strong>{target} did not resolve:</strong> {e:?}</p>"),
    }
}
