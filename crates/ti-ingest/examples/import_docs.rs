//! Import signalk-parquet documents into an existing Lume TI store's `docs/`.
//!
//! ```text
//! cargo run --release -p ti-ingest --example import_docs -- <docs_dir> <store_root>
//! ```
//!
//! Re-running is idempotent: document ids are content-addressed (`ti_ingest::docs`).

use std::path::Path;
use ti_store::DocStore;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: import_docs <docs_dir> <store_root>");
        std::process::exit(2);
    }
    let docs = ti_ingest::docs::read_docs_dir(Path::new(&args[0])).expect("read docs parquet");
    let mut store = DocStore::open(Path::new(&args[1])).expect("open doc store");
    let before = store.version();
    store.upsert_all(docs).expect("store docs");
    println!(
        "documents: {} stored ({})",
        store.len(),
        if store.version() == before {
            "unchanged"
        } else {
            "updated"
        }
    );
}
