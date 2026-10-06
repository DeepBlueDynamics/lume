use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tempfile::{tempdir, NamedTempFile};

use ti_contracts::{
    bucket_of, to_fixed, Agg, BucketIx, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue,
    Result, ShardKey, ShardManifestEntry, ShardSink, TiConfig, VesselSpec,
};
use ti_ingest::{
    backfill_directory, normalize_point, read_parquet_points, BackfillStatus, DeltaRecorder,
    DeltaReplay, NormalizedValue, RawDataPoint, SignalKDelta, WatermarkBucketer,
};
use ti_store::Store;

struct RecordingSink {
    records: Vec<BucketRecord>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }
}

impl ShardSink for RecordingSink {
    fn apply(&mut self, recs: &[BucketRecord]) -> Result<()> {
        self.records.extend_from_slice(recs);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn seal(&mut self, _key: ShardKey) -> Result<ShardManifestEntry> {
        Err(ti_contracts::Error::Unsupported(
            "seal not supported on recording sink".into(),
        ))
    }
}

fn get_peak_rss_mb() -> Option<f64> {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if line.starts_with("VmHWM:") || line.starts_with("VmRSS:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Ok(kb) = parts[1].parse::<f64>() {
                        return Some(kb / 1024.0);
                    }
                }
            }
        }
    }
    None
}

fn get_raw_dir() -> PathBuf {
    if let Ok(val) = std::env::var("TI_DATA_DIR") {
        let p = PathBuf::from(&val);
        if p.join("tier=raw").exists() {
            p.join("tier=raw")
        } else if p.exists() && p.file_name().and_then(|n| n.to_str()) == Some("tier=raw") {
            p
        } else {
            panic!(
                "TI_DATA_DIR was set to {:?}, but neither that directory nor its tier=raw subdirectory exists!",
                val
            );
        }
    } else {
        let default_p = PathBuf::from("/workspace/lume/.lanes/data/w3-smoke/tier=raw");
        if !default_p.exists() {
            panic!(
                "TI_DATA_DIR not set and default smoke directory does not exist at {:?}. Run with TI_DATA_DIR=<path>.",
                default_p
            );
        }
        default_p
    }
}

fn get_open_shard_keys(root: &Path) -> Vec<ShardKey> {
    let mut keys = Vec::new();
    let shards_root = root.join("shards");
    if let Ok(v_entries) = std::fs::read_dir(shards_root) {
        for ve in v_entries.flatten() {
            if let Ok(v) = ve.file_name().to_string_lossy().parse::<u32>() {
                if let Ok(s_entries) = std::fs::read_dir(ve.path()) {
                    for se in s_entries.flatten() {
                        if let Ok(s) = se.file_name().to_string_lossy().parse::<u32>() {
                            keys.push(ShardKey {
                                vessel: v,
                                shard: s,
                            });
                        }
                    }
                }
            }
        }
    }
    keys.sort_by_key(|k| (k.vessel, k.shard));
    keys
}

#[test]
#[ignore = "needs TI_DATA_DIR"]
fn test_m2_parquet_backfill_idempotence() {
    let raw_dir = get_raw_dir();
    let config = TiConfig::default();
    let self_urn = "vessels.urn:mrn:imo:mmsi:367000000";

    // 1. First backfill run into Store 1
    let tmp1 = tempdir().unwrap();
    let mut store1 = Store::open_or_create(tmp1.path(), config.width_seconds).unwrap();
    let catalog1 = Arc::clone(store1.catalog());

    let results1 = backfill_directory(
        &raw_dir,
        self_urn,
        None,
        &config,
        catalog1.as_ref(),
        &mut store1,
    )
    .unwrap();

    assert!(
        !results1.is_empty(),
        "Must ingest at least one parquet file"
    );
    let mut file_hashes = HashSet::new();
    let mut total_rows = 0;
    for status in &results1 {
        match status {
            BackfillStatus::Ingested {
                hash, rows_read, ..
            } => {
                file_hashes.insert(*hash);
                total_rows += rows_read;
            }
            BackfillStatus::Skipped { .. } => panic!("First run should not skip any files"),
        }
    }
    println!(
        "Store 1 ingested {} parquet files, {} total raw rows",
        results1.len(),
        total_rows
    );

    let shard_keys1 = get_open_shard_keys(tmp1.path());
    assert!(
        !shard_keys1.is_empty(),
        "Store 1 must have at least one open shard"
    );
    for &shard_key in &shard_keys1 {
        store1.seal(shard_key).unwrap();
    }
    let manifest1_entries = store1.manifest().entries();

    // 2. Second backfill run into fresh Store 2
    let tmp2 = tempdir().unwrap();
    let mut store2 = Store::open_or_create(tmp2.path(), config.width_seconds).unwrap();
    let catalog2 = Arc::clone(store2.catalog());

    let results2 = backfill_directory(
        &raw_dir,
        self_urn,
        None,
        &config,
        catalog2.as_ref(),
        &mut store2,
    )
    .unwrap();

    assert_eq!(results1.len(), results2.len());
    let shard_keys2 = get_open_shard_keys(tmp2.path());
    assert_eq!(shard_keys1, shard_keys2);
    for &shard_key in &shard_keys2 {
        store2.seal(shard_key).unwrap();
    }
    let manifest2_entries = store2.manifest().entries();

    // Manifest hashes must be completely identical between two fresh stores!
    assert_eq!(
        manifest1_entries, manifest2_entries,
        "Full manifest entries must be identical across fresh store runs"
    );

    // 3. Test skipping when manifest hashes are supplied
    let skip_results = backfill_directory(
        &raw_dir,
        self_urn,
        Some(&file_hashes),
        &config,
        catalog1.as_ref(),
        &mut store1,
    )
    .unwrap();
    assert_eq!(skip_results.len(), results1.len());
    for s in &skip_results {
        assert!(matches!(s, BackfillStatus::Skipped { .. }));
    }

    // 4. Test re-ingesting into Store 1 with clear-and-rewrite (idempotent in-place update)
    let rewrite_results = backfill_directory(
        &raw_dir,
        self_urn,
        None,
        &config,
        catalog1.as_ref(),
        &mut store1,
    )
    .unwrap();
    assert_eq!(rewrite_results.len(), results1.len());

    for &shard_key in &shard_keys1 {
        store1.seal(shard_key).unwrap();
    }
    let manifest1_re_entries = store1.manifest().entries();
    // Re-ingesting into already-sealed shards goes through repair (spec/06), which publishes a
    // new version. Idempotence means the CONTENT is unchanged: same key, range, bytes and hash.
    assert_eq!(manifest1_re_entries.len(), manifest1_entries.len());
    for (re, orig) in manifest1_re_entries.iter().zip(manifest1_entries.iter()) {
        assert_eq!(
            (re.key, re.from, re.to, re.bytes, re.hash),
            (orig.key, orig.from, orig.to, orig.bytes, orig.hash),
            "In-place clear-and-rewrite backfill must reproduce identical sealed shard content"
        );
        assert!(
            re.version >= orig.version,
            "repair must not move a shard version backwards"
        );
    }

    println!(
        "M2 Backfill Idempotence PASSED: manifest entries count = {}, first shard hash = {}",
        manifest1_entries.len(),
        manifest1_entries
            .first()
            .map(|e| ti_ingest::hash_to_hex(&e.hash))
            .unwrap_or_default()
    );
}

#[test]
#[ignore = "needs TI_DATA_DIR"]
fn test_m2_oracle_replay_24h() {
    let raw_dir = get_raw_dir();
    let config = TiConfig::default();
    let self_urn = "vessels.urn:mrn:imo:mmsi:367000000";

    // 1. Find the first day present in raw_dir
    fn find_first_day_tag(dir: &Path) -> Option<String> {
        let mut days = Vec::new();
        fn scan(d: &Path, days: &mut Vec<String>) {
            if let Ok(entries) = std::fs::read_dir(d) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        let name = e.file_name().to_string_lossy().to_string();
                        if name.starts_with("day=") {
                            days.push(name);
                        } else {
                            scan(&p, days);
                        }
                    }
                }
            }
        }
        scan(dir, &mut days);
        days.sort();
        days.dedup();
        days.into_iter().next()
    }

    let day_tag = std::env::var("TI_REPLAY_DAY")
        .map(|d| {
            if d.starts_with("day=") {
                d
            } else {
                format!("day={:0>3}", d)
            }
        })
        .unwrap_or_else(|_| {
            find_first_day_tag(&raw_dir).expect("No day= directory found in raw_dir")
        });
    println!("Selected 24h dataset day tag: {}", day_tag);

    // Collect all parquet files for this vessel and day
    let mut files = Vec::new();
    fn collect_day_files(dir: &Path, day_tag: &str, out: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_day_files(&path, day_tag, out);
                } else if path.to_string_lossy().contains(day_tag)
                    && path.extension().and_then(|e| e.to_str()) == Some("parquet")
                {
                    out.push(path);
                }
            }
        }
    }

    let vessel_dir_candidates = [
        raw_dir.join("context=vessels__urn-mrn-imo-mmsi-367000000"),
        raw_dir.clone(),
    ];
    let search_dir = vessel_dir_candidates
        .iter()
        .find(|d| d.exists())
        .unwrap_or(&raw_dir);

    collect_day_files(search_dir, &day_tag, &mut files);
    files.sort();
    assert!(!files.is_empty(), "Day files must exist for {}", day_tag);

    // Read all raw points from the selected day
    let mut all_points = Vec::new();
    for f in &files {
        let pts = read_parquet_points(f, self_urn).unwrap();
        all_points.extend(pts);
    }
    // Chronological order
    all_points.sort_by_key(|p| p.timestamp);
    println!(
        "Read {} raw points for 24h {} dataset ({} parquet files)",
        all_points.len(),
        day_tag,
        files.len()
    );
    assert!(!all_points.is_empty());

    // 2. Synthesize delta log to a temporary NDJSON file
    let tmp_log = NamedTempFile::new().unwrap();
    {
        let mut recorder = DeltaRecorder::create(tmp_log.path()).unwrap();
        for p in &all_points {
            let ts_iso = chrono::DateTime::from_timestamp(p.timestamp, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default();

            let delta: SignalKDelta = serde_json::from_value(serde_json::json!({
                "context": self_urn,
                "updates": [{
                    "$source": p.source,
                    "timestamp": ts_iso,
                    "values": [{
                        "path": p.path,
                        "value": p.value
                    }]
                }]
            }))
            .unwrap();
            recorder.record_delta(&delta).unwrap();
        }
    }

    // 3. Replay delta log through WatermarkBucketer into Store and RecordingSink
    let tmp_store = tempdir().unwrap();
    let store = Store::open_or_create(tmp_store.path(), config.width_seconds).unwrap();
    let catalog = Arc::clone(store.catalog());

    let mut bucketer = WatermarkBucketer::new(&config);
    let mut sink = RecordingSink::new();

    let replay = DeltaReplay::open(tmp_log.path()).unwrap();
    let replayed_points = replay
        .replay_all(
            self_urn,
            &mut bucketer,
            &config,
            catalog.as_ref(),
            &mut sink,
        )
        .unwrap();

    println!(
        "Replayed {} points, emitted {} BucketRecords",
        replayed_points,
        sink.records.len()
    );
    assert!(!sink.records.is_empty());

    // 4. Reference Oracle Bucketing:
    let _vessel = catalog
        .register_vessel(&VesselSpec {
            urn: self_urn.to_string(),
            name: None,
            mmsi: None,
        })
        .unwrap();

    // Map: (bucket_ix, field_id) -> expected FieldValue
    let mut oracle_expectations: BTreeMap<(BucketIx, u32), FieldValue> = BTreeMap::new();

    // Group normalized samples by (bucket_ix, path) -> list of (ts, source, value)
    type SampleEntry = (i64, String, NormalizedValue);
    let mut bucket_samples: BTreeMap<(BucketIx, String), Vec<SampleEntry>> = BTreeMap::new();

    for pt in all_points {
        let norm_pts = normalize_point(pt, &config.allow_paths, &config.deny_paths);
        for np in norm_pts {
            if let Ok(b_ix) = bucket_of(np.timestamp, config.width_seconds) {
                bucket_samples.entry((b_ix, np.path)).or_default().push((
                    np.timestamp,
                    np.source,
                    np.value,
                ));
            }
        }
    }

    let all_catalog_fields = catalog.fields().unwrap();
    let find_field = |p: &str, a: Option<Agg>| -> Option<&FieldSpec> {
        all_catalog_fields
            .iter()
            .find(|f| f.path == p && f.agg == a)
    };

    for ((b_ix, path), samples) in bucket_samples {
        let any_field = all_catalog_fields.iter().find(|f| f.path == path);
        let field_spec = match any_field {
            Some(spec) => spec,
            None => continue,
        };

        match &field_spec.kind {
            FieldKind::Bsi { scale } => {
                let nums: Vec<(i64, String, f64)> = samples
                    .iter()
                    .filter_map(|(ts, src, v)| match v {
                        NormalizedValue::Double(d) => Some((*ts, src.clone(), *d)),
                        _ => None,
                    })
                    .collect();

                if nums.is_empty() {
                    continue;
                }

                let count = nums.len() as f64;
                let sum: f64 = nums.iter().map(|s| s.2).sum();
                let min = nums.iter().map(|s| s.2).fold(f64::INFINITY, f64::min);
                let max = nums.iter().map(|s| s.2).fold(f64::NEG_INFINITY, f64::max);
                let first_ts = nums.iter().min_by_key(|s| s.0).unwrap().0;
                let last = nums.iter().max_by_key(|s| s.0).unwrap().2;
                let last_ts = nums.iter().max_by_key(|s| s.0).unwrap().0;
                let last_src = nums.iter().max_by_key(|s| s.0).unwrap().1.clone();

                let is_slow =
                    (last_ts - first_ts) >= (config.width_seconds as i64) || nums.len() == 1;
                let aggs_to_emit = if is_slow && config.profiles.slow.contains(&"last".to_string())
                {
                    &config.profiles.slow
                } else {
                    &config.profiles.default
                };

                for agg_name in aggs_to_emit {
                    match agg_name.as_str() {
                        "mean" => {
                            if let Some(f) = find_field(&path, Some(Agg::Mean)) {
                                let expected_int = to_fixed(sum / count, *scale).unwrap();
                                oracle_expectations
                                    .insert((b_ix, f.id), FieldValue::Int(expected_int));
                            }
                        }
                        "min" => {
                            if let Some(f) = find_field(&path, Some(Agg::Min)) {
                                let expected_int = to_fixed(min, *scale).unwrap();
                                oracle_expectations
                                    .insert((b_ix, f.id), FieldValue::Int(expected_int));
                            }
                        }
                        "max" => {
                            if let Some(f) = find_field(&path, Some(Agg::Max)) {
                                let expected_int = to_fixed(max, *scale).unwrap();
                                oracle_expectations
                                    .insert((b_ix, f.id), FieldValue::Int(expected_int));
                            }
                        }
                        "last" => {
                            if let Some(f) = find_field(&path, Some(Agg::Last)) {
                                let expected_int = to_fixed(last, *scale).unwrap();
                                oracle_expectations
                                    .insert((b_ix, f.id), FieldValue::Int(expected_int));
                            }
                        }
                        _ => {}
                    }
                }

                // Source field
                let src_path = format!("{}$source", path);
                if let Some(f) = find_field(&src_path, None) {
                    if let Ok(row) = catalog.register_set_value(f.id, &last_src) {
                        oracle_expectations.insert((b_ix, f.id), FieldValue::SetValue(row));
                    }
                }
            }
            FieldKind::Set => {
                let last_set = samples
                    .iter()
                    .filter_map(|(ts, src, v)| match v {
                        NormalizedValue::String(s) => Some((*ts, src.clone(), s.clone())),
                        NormalizedValue::Bool(b) => Some((
                            *ts,
                            src.clone(),
                            if *b { "true" } else { "false" }.to_string(),
                        )),
                        _ => None,
                    })
                    .max_by_key(|(ts, _, _)| *ts);

                if let Some((_, src, s)) = last_set {
                    if let Some(f) = find_field(&path, None) {
                        if let Ok(row) = catalog.register_set_value(f.id, &s) {
                            oracle_expectations.insert((b_ix, f.id), FieldValue::SetValue(row));
                        }
                    }
                    let src_path = format!("{}$source", path);
                    if let Some(f) = find_field(&src_path, None) {
                        if let Ok(row) = catalog.register_set_value(f.id, &src) {
                            oracle_expectations.insert((b_ix, f.id), FieldValue::SetValue(row));
                        }
                    }
                }
            }
            FieldKind::Count => {
                let cnt = samples.len() as i64;
                if let Some(f) = find_field(&path, Some(Agg::Count)) {
                    oracle_expectations.insert((b_ix, f.id), FieldValue::Int(cnt));
                }
            }
            FieldKind::Presence => {
                if let Some(f) = find_field(&path, None) {
                    oracle_expectations.insert((b_ix, f.id), FieldValue::Present);
                }
            }
            FieldKind::Geo { .. } => {
                if let Some(f) = find_field(&path, None) {
                    let cells: std::collections::BTreeSet<_> = samples
                        .iter()
                        .filter_map(|(_, _, value)| match value {
                            NormalizedValue::Geo { lat, lon } => {
                                Some(ti_geo::cells_for(*lat, *lon).unwrap())
                            }
                            _ => None,
                        })
                        .flatten()
                        .collect();
                    oracle_expectations
                        .insert((b_ix, f.id), FieldValue::Cells(cells.into_iter().collect()));
                }
            }
        }
    }

    // Compare replayed BucketRecords against oracle expectations
    let mut matched = 0;
    let mut unmatched = 0;
    for rec in &sink.records {
        if let Some(expected_val) = oracle_expectations.get(&(rec.bucket, rec.field)) {
            assert_eq!(
                &rec.value, expected_val,
                "Record value mismatch at bucket {}, field {}",
                rec.bucket, rec.field
            );
            matched += 1;
        } else {
            if unmatched < 10 {
                let f = catalog.field(rec.field).unwrap();
                println!(
                    "Unmatched record: bucket={}, field={}, path={}, agg={:?}, kind={:?}, val={:?}",
                    rec.bucket, rec.field, f.path, f.agg, f.kind, rec.value
                );
            }
            unmatched += 1;
        }
    }

    println!(
        "M2 Oracle Replay: matched={}/{}, unmatched={}",
        matched,
        sink.records.len(),
        unmatched
    );
    assert!(matched > 0, "Must have verified records");
    assert_eq!(
        unmatched, 0,
        "All emitted BucketRecords must match oracle expectations"
    );
}

#[test]
fn test_m2_throughput_and_rss() {
    let config = TiConfig::default();
    let self_urn = "vessels.urn:mrn:imo:mmsi:367000000";

    let tmp = tempdir().unwrap();
    let mut store = Store::open_or_create(tmp.path(), config.width_seconds).unwrap();
    let catalog = Arc::clone(store.catalog());
    let mut bucketer = WatermarkBucketer::new(&config);

    // Warm up catalog with standard paths
    let test_paths = [
        "environment.wind.speedTrue",
        "navigation.speedOverGround",
        "electrical.batteries.house.voltage",
        "propulsion.port.temperature",
        "navigation.state",
    ];

    let total_points = 100_000usize;
    println!(
        "Generating {} stream points for throughput benchmark...",
        total_points
    );

    let start_ts = 1_770_739_200i64;
    let mut raw_batch = Vec::with_capacity(total_points);
    for i in 0..total_points {
        let ts = start_ts + (i as i64 / 10);
        let path = test_paths[i % test_paths.len()];
        let value = if path == "navigation.state" {
            serde_json::Value::String("sailing".to_string())
        } else {
            serde_json::Value::Number(
                serde_json::Number::from_f64(10.0 + (i as f64) * 0.01).unwrap(),
            )
        };

        raw_batch.push(RawDataPoint {
            context: self_urn.to_string(),
            path: path.to_string(),
            source: "can0.115".to_string(),
            timestamp: ts,
            value,
        });
    }

    let start_bench = Instant::now();
    for raw in raw_batch {
        let norm_pts = normalize_point(raw, &config.allow_paths, &config.deny_paths);
        for p in norm_pts {
            bucketer
                .ingest_point(
                    &p.context,
                    &p.path,
                    &p.source,
                    p.timestamp,
                    p.value,
                    &config,
                    catalog.as_ref(),
                    &mut store,
                )
                .unwrap();
        }
    }
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut store)
        .unwrap();
    store.flush().unwrap();

    let elapsed = start_bench.elapsed();
    let throughput = (total_points as f64) / elapsed.as_secs_f64();
    let peak_rss_mb = get_peak_rss_mb();

    println!("--------------------------------------------------");
    println!("M2 Throughput & Memory Benchmark Results:");
    println!("  Total points:      {}", total_points);
    println!("  Elapsed time:      {:.3} s", elapsed.as_secs_f64());
    println!(
        "  Throughput:        {:.1} values/s (gate target: >= 20,000 values/s)",
        throughput
    );
    match peak_rss_mb {
        Some(mb) => {
            println!("  Peak RSS:          {:.2} MB (gate target: <= 400 MB)", mb);
            assert!(mb <= 400.0, "Peak RSS {:.2} MB exceeds 400 MB target", mb);
        }
        None => {
            println!("  Peak RSS:          n/a (unsupported OS)");
        }
    }
    println!("--------------------------------------------------");

    assert!(
        throughput >= 20_000.0,
        "Throughput {:.1} values/s below 20,000 values/s target",
        throughput
    );
}
