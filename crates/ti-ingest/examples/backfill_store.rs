//! Backfill a signalk-parquet raw tier into a fresh Lume TI store, seal every shard, and
//! report raw vs index bytes.
//!
//! ```text
//! cargo run --release -p ti-ingest --example backfill_store -- <raw_dir> <store_root> [self_urn]
//! ```
//!
//! `<raw_dir>` may be the whole `tier=raw` directory or a single `context=...` subdirectory
//! (one vessel), which keeps disk use small. The store root must not already exist.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use ti_contracts::{ShardKey, ShardSink, TiConfig};
use ti_ingest::parquet::{backfill_directory, BackfillStatus};
use ti_store::Store;

fn dir_bytes(p: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(p) {
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                total += dir_bytes(&path);
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}

fn shard_keys(root: &Path) -> Vec<ShardKey> {
    let mut keys = Vec::new();
    if let Ok(vessels) = std::fs::read_dir(root.join("shards")) {
        for v in vessels.flatten() {
            let Ok(vessel) = v.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            if let Ok(shards) = std::fs::read_dir(v.path()) {
                for s in shards.flatten() {
                    if let Ok(shard) = s.file_name().to_string_lossy().parse::<u32>() {
                        keys.push(ShardKey { vessel, shard });
                    }
                }
            }
        }
    }
    keys.sort_by_key(|k| (k.vessel, k.shard));
    keys
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: backfill_store <raw_dir> <store_root> [self_urn]");
        std::process::exit(2);
    }
    let raw = Path::new(&args[0]);
    let root = Path::new(&args[1]);
    let self_urn = args
        .get(2)
        .map(String::as_str)
        .unwrap_or("vessels.urn:mrn:imo:mmsi:367000000");
    assert!(!root.exists(), "store root {root:?} already exists");

    let config = TiConfig::default();
    let mut store = Store::open_or_create(root, config.width_seconds).expect("open store");
    let catalog = Arc::clone(store.catalog());

    let t0 = Instant::now();
    let results = backfill_directory(raw, self_urn, None, &config, catalog.as_ref(), &mut store)
        .expect("backfill");
    let backfill_s = t0.elapsed().as_secs_f64();
    let rows: u64 = results
        .iter()
        .map(|r| match r {
            BackfillStatus::Ingested { rows_read, .. } => *rows_read as u64,
            BackfillStatus::Skipped { .. } => 0,
        })
        .sum();

    store.flush().expect("flush");
    let t1 = Instant::now();
    let keys = shard_keys(root);
    for key in &keys {
        store.seal(*key).expect("seal");
    }
    let seal_s = t1.elapsed().as_secs_f64();
    drop(store);

    let raw_bytes = dir_bytes(raw);
    let index_bytes = dir_bytes(root);
    println!("files ingested : {}", results.len());
    println!("raw rows       : {rows}");
    println!(
        "backfill       : {backfill_s:.1} s ({:.0} rows/s)",
        rows as f64 / backfill_s.max(1e-9)
    );
    println!("shards sealed  : {} in {seal_s:.1} s", keys.len());
    println!("raw parquet    : {:.1} MB", raw_bytes as f64 / 1e6);
    println!("index on disk  : {:.1} MB", index_bytes as f64 / 1e6);
    println!(
        "index / raw    : {:.2}",
        index_bytes as f64 / raw_bytes.max(1) as f64
    );
}
