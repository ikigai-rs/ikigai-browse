//! List the records a browse store holds against a `.git` path (ledger #1130).
//! Reads only, and never opens a live store: it loads an N-Quads DUMP into
//! memory, so it runs against a host's backup while the host keeps serving.
//!
//! ```text
//! gunzip -c ~/.ikigai/backups/gonk-store-<stamp>.nq.gz \
//!     | cargo run --release --example git-dir-doctor
//! cargo run --release --example git-dir-doctor -- dump.nq
//! ```
//!
//! Prints one block per record (subject, graph, types, the terms that name
//! `.git`, the children a removal would take) and a total line. Exit status 0
//! when there is nothing to list, 1 when there is, 2 on a read error, so it can
//! gate a script. What to do with a listed record is a person's decision; see
//! `ikigai_browse::doctor`.

use std::io::{BufReader, Read};

use oxigraph::io::RdfFormat;
use oxigraph::store::Store;

fn main() {
    let source = std::env::args().nth(1);
    let reader: Box<dyn Read> = match &source {
        Some(path) => match std::fs::File::open(path) {
            Ok(file) => Box::new(BufReader::new(file)),
            Err(e) => {
                eprintln!("git-dir-doctor: {path}: {e}");
                std::process::exit(2);
            }
        },
        None => Box::new(BufReader::new(std::io::stdin())),
    };
    let store = Store::new().expect("an in-memory store");
    if let Err(e) = store.load_from_reader(RdfFormat::NQuads, reader) {
        eprintln!("git-dir-doctor: not N-Quads: {e}");
        std::process::exit(2);
    }
    let quads = store.len().unwrap_or(0);
    let records = match ikigai_browse::doctor::git_dir_records(&store) {
        Ok(records) => records,
        Err(e) => {
            eprintln!("git-dir-doctor: {e}");
            std::process::exit(2);
        }
    };
    let mut total = 0;
    for r in &records {
        total += r.quads;
        println!("{}", r.subject);
        println!("  graph:    {}", r.graph.as_deref().unwrap_or("(default)"));
        if !r.types.is_empty() {
            println!("  types:    {}", r.types.join(", "));
        }
        for e in &r.evidence {
            println!("  names:    {e}");
        }
        for c in &r.children {
            println!("  child:    {c}");
        }
        println!("  quads:    {}", r.quads);
    }
    println!(
        "{} record(s) name a .git path, {total} quad(s) of {quads} loaded",
        records.len()
    );
    std::process::exit(if records.is_empty() { 0 } else { 1 });
}
