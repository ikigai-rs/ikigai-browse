//! What `urn:repo:style` costs per read, and what the cache is worth.
//!
//!     cargo run --release --example style_cache
//!
//! The stylesheet is generated work: two syntax themes turned into CSS and run
//! through a contrast-floor pass. It is `.cacheable()`, so a host pays that once
//! — but **effective expiry propagates from dependencies**, and this resource
//! now depends on the layered `a11y.toml`. An accessibility config that had been
//! marked uncacheable ("it reads a file") would make every read of this
//! stylesheet a cold generation, on the hot path of a browse page, with every
//! test still passing.
//!
//! So: measure it. The example reports the cached read, then CUTS the config's
//! golden threads and times the recomputation — which is both the payoff (edit
//! `a11y.toml`, the sheet refreshes, nothing polls) and the price a lost cache
//! would charge on every single read.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use ikigai_core::{Capability, Iri, Kernel, Request, Verb};

const READS: u32 = 200;

fn main() {
    // One root is enough: the stylesheet is root-independent.
    let root = std::env::temp_dir().join(format!("ikigai-browse-style-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("scratch root");
    // Named as an application, so both candidate config files show up as
    // threads — including `style-demo.a11y.toml`, which does not exist. That is
    // the point of declaring it: creating it must invalidate this sheet.
    let kernel = Kernel::new(Arc::new(
        ikigai_browse::Mount::new([("demo".to_string(), PathBuf::from(&root))])
            .app("style-demo")
            .space(),
    ));
    let cap = Capability::scoped([ikigai_browse::CAP_WILDCARD]);
    let iri = Iri::parse(ikigai_browse::STYLE_IRI).expect("a valid IRI");

    let read = || {
        futures::executor::block_on(kernel.issue(Request::new(Verb::Source, iri.clone()), &cap))
            .expect("the stylesheet resolves")
    };

    // The first read is the cold one — the generation a cache exists to avoid.
    let start = Instant::now();
    let first = read();
    let cold = start.elapsed().as_secs_f64() * 1e6;

    let start = Instant::now();
    for _ in 0..READS {
        let _ = read();
    }
    let warm = start.elapsed().as_secs_f64() * 1e6 / f64::from(READS);

    let threads: Vec<String> = first.threads().iter().map(|t| t.to_string()).collect();
    println!("urn:repo:style — {} bytes of CSS", first.bytes.len());
    println!("  first (cold) read     {cold:>10.1} µs");
    println!(
        "  cached read, mean of {READS}  {warm:>7.1} µs   {:.0}×",
        cold / warm
    );
    if threads.is_empty() {
        println!("\n  no golden threads declared — a pure function of the build");
    } else {
        println!("\n  golden threads:");
        for thread in &threads {
            println!("    {thread}");
        }
        for thread in &threads {
            kernel.cut(thread.as_str());
        }
        let start = Instant::now();
        let _ = read();
        let recomputed = start.elapsed().as_secs_f64() * 1e6;
        println!(
            "\n  after cutting them, the next read recomputes: {recomputed:.1} µs\n  \
             that is what EVERY read would cost if the config were uncacheable."
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
