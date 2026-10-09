//! Same profiling harness for f6c47b4 and the current ingest pipeline.
//! Run with --features backfill-profile; JSON is printed on stdout.
use arrow_array::{Array, StringArray, UInt8Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use ti_contracts::{
    BucketRecord, Catalog, Result, ShardKey, ShardManifestEntry, ShardSink, ShardSource, TiConfig,
    VesselSpec,
};
use ti_store::Store;
struct TimedSink<'a>(&'a mut Store);
impl ShardSink for TimedSink<'_> {
    fn apply(&mut self, records: &[BucketRecord]) -> Result<()> {
        let _scope = ti_ingest::profile::Scope::new(6);
        self.0.apply(records)
    }
    fn flush(&mut self) -> Result<()> {
        let _scope = ti_ingest::profile::Scope::new(7);
        self.0.flush()
    }
    fn seal(&mut self, key: ShardKey) -> Result<ShardManifestEntry> {
        self.0.seal(key)
    }
}
fn files(dir: &Path, out: &mut Vec<PathBuf>, days: usize) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, out, days);
        } else if path.extension().is_some_and(|e| e == "parquet")
            && path.components().any(|c| {
                c.as_os_str().to_str().is_some_and(|s| {
                    s.strip_prefix("day=")
                        .and_then(|s| s.parse::<usize>().ok())
                        .is_some_and(|d| d >= 60 && d < 60 + days)
                })
            })
        {
            out.push(path);
        }
    }
}
fn bootstrap(raw: &Path, config: &mut TiConfig, catalog: &dyn Catalog) {
    let root = raw
        .ancestors()
        .find(|a| a.join("catalog").is_dir())
        .unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(
        std::fs::File::open(root.join("catalog/paths/paths.parquet")).unwrap(),
    )
    .unwrap()
    .build()
    .unwrap();
    for batch in reader {
        let batch = batch.unwrap();
        let paths = batch
            .column_by_name("path")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let scales = batch
            .column_by_name("scale")
            .unwrap()
            .as_any()
            .downcast_ref::<UInt8Array>()
            .unwrap();
        for i in 0..batch.num_rows() {
            if !scales.is_null(i) {
                config
                    .path_scales
                    .insert(paths.value(i).into(), scales.value(i));
            }
        }
    }
    let reader = ParquetRecordBatchReaderBuilder::try_new(
        std::fs::File::open(root.join("catalog/vessels/vessels.parquet")).unwrap(),
    )
    .unwrap()
    .build()
    .unwrap();
    for batch in reader {
        let batch = batch.unwrap();
        let text = |name: &str, i: usize| {
            batch
                .column_by_name(name)
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .filter(|c| !c.is_null(i))
                .map(|c| c.value(i).to_string())
        };
        for i in 0..batch.num_rows() {
            catalog
                .register_vessel(&VesselSpec {
                    urn: text("urn", i).unwrap(),
                    name: text("name", i),
                    mmsi: text("mmsi", i),
                })
                .unwrap();
        }
    }
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(
        args.len() >= 3,
        "profile_backfill raw_dir fresh_store [days=7]"
    );
    let raw = Path::new(&args[1]);
    let root = Path::new(&args[2]);
    assert!(!root.exists(), "store must be fresh");
    let days = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(7);
    let mut config = TiConfig {
        store_root: root.to_string_lossy().into_owned(),
        ..Default::default()
    };
    config.profiles.opt_in = vec!["last".into()];
    let mut store = Store::open_or_create(root, 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    bootstrap(raw, &mut config, catalog.as_ref());
    let mut paths = vec![];
    files(raw, &mut paths, days);
    paths.sort();
    assert!(!paths.is_empty());
    ti_ingest::profile::reset();
    let start = Instant::now();
    let results = ti_ingest::parquet::profile_backfill_files(
        &paths,
        "vessels.urn:mrn:imo:mmsi:367000000",
        &config,
        catalog.as_ref(),
        &mut TimedSink(&mut store),
    )
    .unwrap();
    let rows: usize = results
        .iter()
        .map(|s| match s {
            ti_ingest::parquet::BackfillStatus::Ingested { rows_read, .. } => *rows_read,
            _ => 0,
        })
        .sum();
    let elapsed = start.elapsed().as_secs_f64();
    let stages = ti_ingest::profile::seconds();
    let seal_start = Instant::now();
    let mut hashes = std::collections::BTreeMap::new();
    for key in store.shards(None, 0, u32::MAX) {
        let entry = store.seal(key).unwrap();
        hashes.insert(
            format!("{}:{}", key.vessel, key.shard),
            ti_ingest::parquet::hash_to_hex(&entry.hash),
        );
    }
    let seal_s = seal_start.elapsed().as_secs_f64();
    store.shutdown().unwrap();
    let stage_map: std::collections::BTreeMap<_, _> =
        ti_ingest::profile::NAMES.into_iter().zip(stages).collect();
    println!(
        "{}",
        serde_json::json!({"files":paths.len(),"rows":rows,"elapsed_s":elapsed,"rows_per_s":rows as f64/elapsed,"stages_s":stage_map,"bucketer_exclusive_s":stages[5]-stages[3]-stages[4]-stages[6],"seal_s":seal_s,"hashes":hashes})
    );
}
