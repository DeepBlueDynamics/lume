//! Backfill a signalk-parquet raw tier into a fresh Lume TI store, seal every shard, and
//! report raw vs index bytes.
//!
//! ```text
//! cargo run --release -p ti-ingest --example backfill_store -- <raw_dir> <store_root> [self_urn]
//! ```
//!
//! `<raw_dir>` may be the whole `tier=raw` directory or a single `context=...` subdirectory
//! (one vessel), which keeps disk use small. The store root must not already exist.
//!
//! If `<raw_dir>/../catalog/paths/paths.parquet` exists (written by `ti-bench gen`), its
//! per-path `scale` column seeds `TiConfig::path_scales`, standing in for the Signal K
//! `meta.units` that a raw-tier backfill does not have. Likewise `catalog/vessels` pre-registers
//! vessel names and MMSIs, standing in for the Signal K `name` delta. `TI_OPT_IN=last` adds
//! opt-in aggregates (the golden corpus queries `@last`). `<ancestor>/docs/*.parquet`
//! (notes, logbook, alerts) is imported into the store's `docs/` for `match()`.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use arrow_array::{Array, StringArray, UInt8Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use ti_contracts::{Catalog, ShardKey, ShardSink, TiConfig, VesselSpec};
use ti_ingest::parquet::{backfill_directory, BackfillStatus};
use ti_store::{DocStore, Store};

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

/// Read `path -> scale` from a ti-bench `catalog/paths/paths.parquet`, if present.
/// Vessels from `<ancestor>/catalog/vessels/vessels.parquet`, in `ord` order.
fn catalog_vessels(raw: &Path) -> Vec<VesselSpec> {
    let mut out = Vec::new();
    let Some(root) = raw.ancestors().find(|a| a.join("catalog").is_dir()) else {
        return out;
    };
    let Ok(file) = std::fs::File::open(root.join("catalog/vessels/vessels.parquet")) else {
        return out;
    };
    let Ok(reader) = ParquetRecordBatchReaderBuilder::try_new(file).and_then(|b| b.build()) else {
        return out;
    };
    let text = |batch: &arrow_array::RecordBatch, name: &str, i: usize| {
        batch
            .column_by_name(name)
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .filter(|c| !c.is_null(i))
            .map(|c| c.value(i).to_string())
    };
    for batch in reader.flatten() {
        for i in 0..batch.num_rows() {
            if let Some(urn) = text(&batch, "urn", i) {
                out.push(VesselSpec {
                    urn,
                    name: text(&batch, "name", i),
                    mmsi: text(&batch, "mmsi", i),
                });
            }
        }
    }
    out
}

fn catalog_scales(raw: &Path) -> Vec<(String, u8)> {
    let mut out = Vec::new();
    let Some(root) = raw.ancestors().find(|a| a.join("catalog").is_dir()) else {
        return out;
    };
    let Ok(file) = std::fs::File::open(root.join("catalog/paths/paths.parquet")) else {
        return out;
    };
    let Ok(reader) = ParquetRecordBatchReaderBuilder::try_new(file).and_then(|b| b.build()) else {
        return out;
    };
    for batch in reader.flatten() {
        let (Some(paths), Some(scales)) = (
            batch
                .column_by_name("path")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>()),
            batch
                .column_by_name("scale")
                .and_then(|c| c.as_any().downcast_ref::<UInt8Array>()),
        ) else {
            continue;
        };
        for i in 0..batch.num_rows() {
            if !scales.is_null(i) {
                out.push((paths.value(i).to_string(), scales.value(i)));
            }
        }
    }
    out
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

    let mut config = TiConfig {
        store_root: root.to_string_lossy().into_owned(),
        ..Default::default()
    };
    // Bucket width in seconds (default 10), e.g. TI_WIDTH=1 for a full-rate store.
    if let Ok(width) = std::env::var("TI_WIDTH") {
        config.width_seconds = width.parse().expect("TI_WIDTH must be whole seconds");
    }
    println!("bucket width   : {} s", config.width_seconds);
    let scales = catalog_scales(raw);
    println!("path scales    : {} from catalog/paths", scales.len());
    for (path, scale) in scales {
        config.path_scales.insert(path, scale);
    }
    // Opt-in aggregates (spec/05), e.g. TI_OPT_IN=last for the golden corpus's @last queries.
    if let Ok(list) = std::env::var("TI_OPT_IN") {
        config.profiles.opt_in = list
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(String::from)
            .collect();
    }
    println!("opt-in aggs    : {:?}", config.profiles.opt_in);
    let mut store = Store::open_or_create(root, config.width_seconds).expect("open store");
    let catalog = Arc::clone(store.catalog());
    if let Some(dir) = raw.ancestors().map(|a| a.join("docs")).find(|d| d.is_dir()) {
        let docs = ti_ingest::docs::read_docs_dir(&dir).expect("read docs parquet");
        let mut store_docs = DocStore::open(root).expect("open doc store");
        store_docs.upsert_all(docs).expect("store docs");
        println!(
            "documents      : {} from {}",
            store_docs.len(),
            dir.display()
        );
    }
    let vessels = catalog_vessels(raw);
    println!("vessels        : {} from catalog/vessels", vessels.len());
    for vessel in &vessels {
        catalog.register_vessel(vessel).expect("register vessel");
    }

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
