use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use ti_contracts::{
    Agg, Catalog, Predicate, Result, ShardKey, ShardManifestEntry, ShardSink, ShardSource,
    TiConfig, EPOCH,
};
use ti_ingest::{process_message, WatermarkBucketer};
use ti_store::Store;

const VESSEL: &str = "vessels.urn:mrn:signalk:uuid:bucket-gap";
const PATHS: [&str; 3] = [
    "electrical.batteries.1.capacity.nominal",
    "electrical.batteries.1.chargeEfficiencyFactor",
    "electrical.batteries.1.temperatureCoefficient",
];
const VALUES: [f64; 3] = [25_712_640_000.0, 0.95, 0.01];

fn delta(second: i64, battery: bool) -> String {
    let mut values = vec![
        serde_json::json!({"path":"environment.depth.belowTransducer","value":4.0}),
        serde_json::json!({"path":"navigation.speedOverGround","value":2.0}),
    ];
    if battery {
        values.extend(
            PATHS
                .iter()
                .zip(VALUES)
                .map(|(path, value)| serde_json::json!({"path":path,"value":value})),
        );
    }
    serde_json::json!({
        "context": VESSEL,
        "updates": [{
            "$source":"n2k.115",
            "timestamp":chrono::DateTime::from_timestamp(EPOCH + second, 0).unwrap().to_rfc3339(),
            "values": values
        }]
    })
    .to_string()
}

fn ingest(
    second: i64,
    receive_second: i64,
    bucketer: &mut WatermarkBucketer,
    config: &TiConfig,
    catalog: &dyn Catalog,
    sink: &mut dyn ShardSink,
) {
    process_message(
        &delta(second, second % 3 == 1),
        VESSEL,
        UNIX_EPOCH + Duration::from_secs((EPOCH + receive_second) as u64),
        bucketer,
        config,
        catalog,
        sink,
        &mut None,
    )
    .unwrap();
}

fn assert_battery_buckets(store: &Store, expected: &[u32]) {
    let key = ShardKey {
        vessel: 0,
        shard: 0,
    };
    for (path, value) in PATHS.into_iter().zip(VALUES) {
        let field = store
            .catalog()
            .fields()
            .unwrap()
            .into_iter()
            .find(|field| field.path == path && field.agg == Some(Agg::Mean))
            .unwrap();
        let actual: Vec<_> = store
            .eval(key, &Predicate::Present(field.id))
            .unwrap()
            .iter()
            .collect();
        assert_eq!(actual, expected, "{path}: missing a bucket");
        let ti_contracts::FieldKind::Bsi { scale } = field.kind else {
            panic!("numeric field")
        };
        let columns = expected.iter().copied().collect();
        let fixed = ti_contracts::to_fixed(value, scale).unwrap();
        for (op, wanted) in [
            (
                ti_contracts::AggOp::Min,
                ti_contracts::AggPartial::Min(Some(fixed)),
            ),
            (
                ti_contracts::AggOp::Max,
                ti_contracts::AggPartial::Max(Some(fixed)),
            ),
        ] {
            assert_eq!(
                store.agg(key, &columns, field.id, op).unwrap(),
                wanted,
                "{path}"
            );
        }
    }
}

#[test]
fn three_second_cadence_survives_flush_and_wal_ticks_at_bucket_boundaries() {
    // Run both maintenance orders, including receive/event times on opposite sides
    // of the boundary. Maintenance never closes the active bucket prematurely.
    for before in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(root.path(), 10).unwrap();
        let catalog = Arc::clone(store.catalog());
        let config = TiConfig::default();
        let mut bucketer = WatermarkBucketer::new(&config);
        for second in 0..90 {
            if before {
                store.tick().unwrap();
                if second % 5 == 0 {
                    store.flush().unwrap();
                }
            }
            ingest(
                second,
                second + 2,
                &mut bucketer,
                &config,
                catalog.as_ref(),
                &mut store,
            );
            if !before {
                store.tick().unwrap();
                if second % 5 == 0 {
                    store.flush().unwrap();
                }
            }
        }
        // A late battery delta after bucket 0 closed must remain a rewrite,
        // rather than silently disappearing.
        process_message(
            &delta(7, true),
            VESSEL,
            UNIX_EPOCH + Duration::from_secs((EPOCH + 92) as u64),
            &mut bucketer,
            &config,
            catalog.as_ref(),
            &mut store,
            &mut None,
        )
        .unwrap();
        bucketer
            .flush_all(&config, catalog.as_ref(), &mut store)
            .unwrap();
        assert_battery_buckets(&store, &(0..9).collect::<Vec<_>>());
        drop(store);
        let reopened = Store::open_or_create(root.path(), 10).unwrap();
        assert_battery_buckets(&reopened, &(0..9).collect::<Vec<_>>());
    }
}

struct FailOnce<'a> {
    store: &'a mut Store,
    fail: bool,
    publish_before_error: bool,
}
impl ShardSink for FailOnce<'_> {
    fn apply(&mut self, records: &[ti_contracts::BucketRecord]) -> Result<()> {
        if self.fail {
            self.fail = false;
            if self.publish_before_error {
                self.store.apply(records)?;
            }
            return Err(std::io::Error::other("injected WAL/apply failure at bucket close").into());
        }
        self.store.apply(records)
    }
    fn flush(&mut self) -> Result<()> {
        self.store.flush()
    }
    fn seal(&mut self, key: ShardKey) -> Result<ShardManifestEntry> {
        self.store.seal(key)
    }
}

#[test]
fn failed_boundary_apply_keeps_samples_until_sink_acknowledges() {
    // The old implementation removes the window before apply and loses all
    // three battery paths in bucket 0 after the injected maintenance failure.
    for explicit_flush in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(root.path(), 10).unwrap();
        let catalog = Arc::clone(store.catalog());
        let config = TiConfig::default();
        let mut bucketer = WatermarkBucketer::new(&config);
        for second in 0..40 {
            ingest(
                second,
                second,
                &mut bucketer,
                &config,
                catalog.as_ref(),
                &mut store,
            );
        }
        store.tick().unwrap();
        let mut sink = FailOnce {
            store: &mut store,
            fail: true,
            publish_before_error: false,
        };
        let result = if explicit_flush {
            bucketer.flush_all(&config, catalog.as_ref(), &mut sink)
        } else {
            bucketer.advance_watermark(EPOCH + 10, &config, catalog.as_ref(), &mut sink)
        };
        assert!(result.is_err());
        assert!(
            !bucketer.is_bucket_closed(0, 0),
            "failed apply must not acknowledge close"
        );
        let first = bucketer.counters();
        assert_eq!(first.apply_failures, 1);
        assert_eq!(first.apply_retries, 0);
        // Backoff suppresses immediate retries; advance monotonic time explicitly.
        assert_eq!(
            bucketer
                .retry_pending(
                    std::time::Instant::now(),
                    &config,
                    catalog.as_ref(),
                    &mut sink
                )
                .unwrap(),
            0
        );
        assert_eq!(bucketer.counters(), first);
        bucketer
            .retry_pending(
                std::time::Instant::now() + Duration::from_secs(1),
                &config,
                catalog.as_ref(),
                &mut sink,
            )
            .unwrap();
        bucketer
            .flush_all(&config, catalog.as_ref(), &mut sink)
            .unwrap();
        assert_eq!(bucketer.counters().apply_retries, 1);
        assert_battery_buckets(&store, &[0, 1, 2, 3]);
    }
}

fn fill_window(config: &TiConfig, store: &mut Store, bucketer: &mut WatermarkBucketer) {
    let catalog = Arc::clone(store.catalog());
    for second in 0..10 {
        ingest(second, second, bucketer, config, catalog.as_ref(), store);
    }
}

#[test]
fn lost_ack_retry_replaces_counts_instead_of_double_counting() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(root.path(), 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    let config = TiConfig {
        ingest: ti_contracts::IngestConfig {
            count_paths: vec![PATHS[0].into()],
        },
        ..Default::default()
    };
    let mut bucketer = WatermarkBucketer::new(&config);
    fill_window(&config, &mut store, &mut bucketer);
    let mut sink = FailOnce {
        store: &mut store,
        fail: true,
        publish_before_error: true,
    };
    assert!(bucketer
        .advance_watermark(EPOCH + 10, &config, catalog.as_ref(), &mut sink)
        .is_err());
    // Another event arrives in the retained window before retry. Its count is 4,
    // so a non-rewrite retry would fail against the already published count 3.
    process_message(
        &delta(7, true),
        VESSEL,
        UNIX_EPOCH + Duration::from_secs((EPOCH + 10) as u64),
        &mut bucketer,
        &config,
        catalog.as_ref(),
        &mut sink,
        &mut None,
    )
    .unwrap();
    bucketer
        .retry_pending(
            std::time::Instant::now() + Duration::from_secs(1),
            &config,
            catalog.as_ref(),
            &mut sink,
        )
        .unwrap();
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut sink)
        .unwrap();
    let field = catalog
        .fields()
        .unwrap()
        .into_iter()
        .find(|f| f.path == PATHS[0] && f.agg.is_none())
        .unwrap();
    let columns = [0].into_iter().collect();
    assert_eq!(
        store
            .agg(
                ShardKey {
                    vessel: 0,
                    shard: 0
                },
                &columns,
                field.id,
                ti_contracts::AggOp::Max
            )
            .unwrap(),
        ti_contracts::AggPartial::Max(Some(4))
    );
    assert_eq!(bucketer.counters().apply_failures, 1);
    assert_eq!(bucketer.counters().apply_retries, 1);
}

#[test]
fn delayed_fast_paths_cannot_repopulate_a_failed_bucket_without_its_battery_paths() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(root.path(), 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    let config = TiConfig::default();
    let mut bucketer = WatermarkBucketer::new(&config);
    fill_window(&config, &mut store, &mut bucketer);
    // An unclassified heartbeat advances event time without triggering close,
    // leaving maintenance to close bucket 0.
    bucketer
        .ingest_point(
            VESSEL,
            "heartbeat",
            "clock",
            EPOCH + 40,
            ti_ingest::NormalizedValue::Null,
            &config,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();
    let mut sink = FailOnce {
        store: &mut store,
        fail: true,
        publish_before_error: false,
    };
    assert!(bucketer
        .advance_watermark(EPOCH + 10, &config, catalog.as_ref(), &mut sink)
        .is_err());
    process_message(
        &delta(9, false),
        VESSEL,
        UNIX_EPOCH + Duration::from_secs((EPOCH + 40) as u64),
        &mut bucketer,
        &config,
        catalog.as_ref(),
        &mut sink,
        &mut None,
    )
    .unwrap();
    bucketer
        .retry_pending(
            std::time::Instant::now() + Duration::from_secs(1),
            &config,
            catalog.as_ref(),
            &mut sink,
        )
        .unwrap();
    for path in PATHS.into_iter().chain([
        "environment.depth.belowTransducer",
        "navigation.speedOverGround",
    ]) {
        let field = catalog
            .fields()
            .unwrap()
            .into_iter()
            .find(|f| f.path == path && f.agg == Some(Agg::Mean))
            .unwrap();
        assert!(
            store
                .eval(
                    ShardKey {
                        vessel: 0,
                        shard: 0
                    },
                    &Predicate::Present(field.id)
                )
                .unwrap()
                .contains(0),
            "{path}"
        );
    }
}

#[test]
fn rejected_samples_are_counted_and_serialized_without_poisoning_other_paths() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(root.path(), 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    let mut config = TiConfig {
        ingest: ti_contracts::IngestConfig {
            count_paths: vec!["cycles".into()],
        },
        ..Default::default()
    };
    config
        .source_priorities
        .insert("cycles".into(), vec!["a".into(), "b".into()]);
    let mut bucketer = WatermarkBucketer::new(&config);
    for (source, value) in [
        ("b", 1.0),
        ("a", 2.0),
        ("b", 3.0),
        ("a", f64::MAX),
        ("a", f64::NAN),
    ] {
        bucketer
            .ingest_point(
                VESSEL,
                "cycles",
                source,
                EPOCH + 1,
                ti_ingest::NormalizedValue::Double(value),
                &config,
                catalog.as_ref(),
                &mut store,
            )
            .unwrap();
    }
    // Ordinary paths get the same magnitude/nonfinite protection as count_paths.
    bucketer
        .ingest_point(
            VESSEL,
            "overflow",
            "a",
            EPOCH + 2,
            ti_ingest::NormalizedValue::Double(f64::MAX),
            &config,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();
    assert!(bucketer
        .ingest_point(
            VESSEL,
            "cycles",
            "a",
            EPOCH - 1,
            ti_ingest::NormalizedValue::Double(1.0),
            &config,
            catalog.as_ref(),
            &mut store
        )
        .is_err());
    fill_window(&config, &mut store, &mut bucketer);
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut store)
        .unwrap();
    assert_battery_buckets(&store, &[0]);
    let counters = bucketer.counters();
    assert_eq!(counters.samples_dropped_late, 1);
    assert_eq!(counters.samples_dropped_nonfinite, 1);
    assert_eq!(counters.samples_skipped_magnitude, 2);
    assert_eq!(counters.samples_rejected_source, 2);
    let mut service =
        ti_ingest::IngestService::new(config, Arc::new(std::sync::atomic::AtomicBool::new(false)));
    service.sample_counters = counters;
    service.write_status(root.path(), false);
    let status: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("ingest_status.json")).unwrap())
            .unwrap();
    for (key, value) in serde_json::to_value(counters).unwrap().as_object().unwrap() {
        assert_eq!(&status[key], value);
    }
}

#[test]
fn failure_log_worker() {
    if std::env::var_os("TI_FAILURE_LOG_WORKER").is_none() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(root.path(), 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    let config = TiConfig::default();
    let mut bucketer = WatermarkBucketer::new(&config);
    fill_window(&config, &mut store, &mut bucketer);
    let mut sink = FailOnce {
        store: &mut store,
        fail: true,
        publish_before_error: false,
    };
    assert!(bucketer
        .advance_watermark(EPOCH + 10, &config, catalog.as_ref(), &mut sink)
        .is_err());
    let now = std::time::Instant::now();
    // Repeated failures use 1,2,4,8,16,30 second delays and one log for the window.
    let mut elapsed = 0;
    for delay in [1, 2, 4, 8, 16, 30] {
        elapsed += delay;
        sink.fail = true;
        assert!(bucketer
            .retry_pending(
                now + Duration::from_secs(elapsed),
                &config,
                catalog.as_ref(),
                &mut sink
            )
            .is_err());
        assert_eq!(
            bucketer
                .retry_pending(
                    now + Duration::from_secs(elapsed),
                    &config,
                    catalog.as_ref(),
                    &mut sink
                )
                .unwrap(),
            0
        );
    }
    assert_eq!(bucketer.counters().apply_failures, 7);
    assert_eq!(bucketer.counters().apply_retries, 6);
}

#[test]
fn failures_log_one_cause_per_window() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "failure_log_worker", "--nocapture"])
        .env("TI_FAILURE_LOG_WORKER", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("Ingest window apply/emit failed:").count(),
        1,
        "{stderr}"
    );
    assert!(
        stderr.contains("injected WAL/apply failure at bucket close"),
        "{stderr}"
    );
    assert!(stderr.contains("vessel=0 bucket=0"), "{stderr}");
}

#[test]
fn failed_close_does_not_discard_the_rest_of_a_delta() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(root.path(), 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    let config = TiConfig::default();
    let mut bucketer = WatermarkBucketer::new(&config);
    fill_window(&config, &mut store, &mut bucketer);
    let mut sink = FailOnce {
        store: &mut store,
        fail: true,
        publish_before_error: false,
    };
    assert!(process_message(
        &delta(40, true),
        VESSEL,
        UNIX_EPOCH + Duration::from_secs((EPOCH + 40) as u64),
        &mut bucketer,
        &config,
        catalog.as_ref(),
        &mut sink,
        &mut None
    )
    .is_err());
    bucketer
        .retry_pending(
            std::time::Instant::now() + Duration::from_secs(1),
            &config,
            catalog.as_ref(),
            &mut sink,
        )
        .unwrap();
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut sink)
        .unwrap();
    assert_battery_buckets(&store, &[0, 4]);
}

struct SeenCloses(Arc<std::sync::Mutex<Vec<u32>>>);
impl ti_ingest::ClosedBucketObserver for SeenCloses {
    fn on_closed(&mut self, _vessel: ti_contracts::VesselOrd, from: u32, _to: u32) -> Result<()> {
        self.0.lock().unwrap().push(from);
        Ok(())
    }
}

#[test]
fn persistent_failure_blocks_at_64_windows_then_drains_in_order() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(root.path(), 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    let config = TiConfig::default();
    let mut bucketer = WatermarkBucketer::new(&config);
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    bucketer.set_closed_bucket_observer(Some(Box::new(SeenCloses(Arc::clone(&seen)))));
    let mut sink = FailOnce {
        store: &mut store,
        fail: true,
        publish_before_error: false,
    };
    let now = std::time::Instant::now();
    for bucket in 0..64 {
        sink.fail = true;
        let _ = process_message(
            &delta(bucket * 10, true),
            VESSEL,
            UNIX_EPOCH + Duration::from_secs((EPOCH + bucket * 10) as u64),
            &mut bucketer,
            &config,
            catalog.as_ref(),
            &mut sink,
            &mut None,
        );
        sink.fail = true;
        let _ = bucketer.retry_pending(
            now + Duration::from_secs(bucket as u64 * 31),
            &config,
            catalog.as_ref(),
            &mut sink,
        );
    }
    assert_eq!(bucketer.open_bucket_count(), 64);
    assert!(process_message(
        &delta(640, true),
        VESSEL,
        UNIX_EPOCH + Duration::from_secs((EPOCH + 640) as u64),
        &mut bucketer,
        &config,
        catalog.as_ref(),
        &mut sink,
        &mut None
    )
    .is_err());
    assert!(bucketer.counters().ingest_blocked);
    assert_eq!(bucketer.counters().samples_rejected_blocked, 5);
    let mut service = ti_ingest::IngestService::new(
        config.clone(),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    service.sample_counters = bucketer.counters();
    service.write_status(root.path(), true);
    let status: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("ingest_status.json")).unwrap())
            .unwrap();
    assert_eq!(status["ingest_blocked"], true);
    assert_eq!(status["samples_rejected_blocked"], 5);
    assert_eq!(bucketer.open_bucket_count(), 64);
    assert!(seen.lock().unwrap().is_empty());
    sink.fail = false;
    assert_eq!(
        bucketer
            .retry_pending(
                now + Duration::from_secs(3000),
                &config,
                catalog.as_ref(),
                &mut sink
            )
            .unwrap(),
        64
    );
    assert!(!bucketer.counters().ingest_blocked);
    assert_eq!(bucketer.pending_retry_count(), 0);
    assert_eq!(bucketer.open_bucket_count(), 0);
    assert_eq!(*seen.lock().unwrap(), (0..64).collect::<Vec<_>>());
    assert_battery_buckets(&store, &(0..64).collect::<Vec<_>>());
    process_message(
        &delta(640, true),
        VESSEL,
        UNIX_EPOCH + Duration::from_secs((EPOCH + 640) as u64),
        &mut bucketer,
        &config,
        catalog.as_ref(),
        &mut store,
        &mut None,
    )
    .unwrap();
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut store)
        .unwrap();
    assert_battery_buckets(&store, &(0..65).collect::<Vec<_>>());
}

#[test]
fn memory_budget_rejects_an_oversized_contribution_before_retaining_it() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(root.path(), 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    let config = TiConfig::default();
    let mut bucketer = WatermarkBucketer::new(&config);
    assert!(bucketer
        .ingest_point(
            VESSEL,
            "huge",
            "source",
            EPOCH + 1,
            ti_ingest::NormalizedValue::String("x".repeat(16 * 1024 * 1024)),
            &config,
            catalog.as_ref(),
            &mut store
        )
        .is_err());
    assert_eq!(bucketer.open_bucket_count(), 0);
    assert!(bucketer.counters().ingest_blocked);
    assert_eq!(bucketer.counters().samples_rejected_blocked, 1);
    bucketer
        .retry_pending(
            std::time::Instant::now(),
            &config,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();
    assert!(!bucketer.counters().ingest_blocked);
}

/// Healthy high-rate ingest must not trip the retained-memory cap: a window keeps one
/// accumulator per (path, source), so repeated samples are not charged again.
/// Regression for the Pi 20k values/s run (blocked with 21 open windows at 64 MiB).
#[test]
fn high_rate_repeated_samples_do_not_block_ingest() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(root.path(), 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    let config = TiConfig::default();
    let mut bucketer = WatermarkBucketer::new(&config);
    // 200,000 samples in one 10 s bucket over 50 paths: per-sample charging reserved
    // about 1.3 KB each (≈ 260 MB) and blocked; per-accumulator charging is ~50 KB.
    for i in 0..200_000u64 {
        let path = format!("navigation.synthetic{}", i % 50);
        bucketer
            .ingest_point(
                VESSEL,
                &path,
                "n2k.feed",
                EPOCH + 1 + (i % 9) as i64,
                ti_ingest::NormalizedValue::Double(i as f64 * 0.001),
                &config,
                catalog.as_ref(),
                &mut store,
            )
            .unwrap_or_else(|e| panic!("sample {i} rejected: {e}"));
    }
    assert!(!bucketer.counters().ingest_blocked);
    assert_eq!(bucketer.counters().samples_rejected_blocked, 0);
}

/// A healthy fleet keeps more than 64 windows open at once (vessels x watermark lag);
/// only a failing sink is held to 64. Regression for the Pi 21-vessel load run, which
/// rejected 1,000 samples at `retained_windows=64` with 4.7 MB reserved.
#[test]
fn healthy_fleet_keeps_more_than_64_windows_open() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(root.path(), 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    let config = TiConfig::default();
    let mut bucketer = WatermarkBucketer::new(&config);
    for vessel in 0..40 {
        let context = format!("vessels.urn:mrn:signalk:uuid:fleet-{vessel}");
        for bucket in 0..3 {
            bucketer
                .ingest_point(
                    &context,
                    "navigation.speedOverGround",
                    "n2k.feed",
                    EPOCH + 1 + bucket * 10,
                    ti_ingest::NormalizedValue::Double(1.0),
                    &config,
                    catalog.as_ref(),
                    &mut store,
                )
                .unwrap_or_else(|e| panic!("vessel {vessel} bucket {bucket} rejected: {e}"));
        }
    }
    assert_eq!(bucketer.open_bucket_count(), 120);
    assert!(!bucketer.counters().ingest_blocked);
    assert_eq!(bucketer.counters().samples_rejected_blocked, 0);
}

#[test]
fn a_failed_store_does_not_skip_other_stores() {
    let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let mut failing = Store::open_or_create(roots[0].path(), 10).unwrap();
    let mut healthy = Store::open_or_create(roots[1].path(), 10).unwrap();
    let catalogs_owned = [Arc::clone(failing.catalog()), Arc::clone(healthy.catalog())];
    let catalogs = [
        ("default".into(), catalogs_owned[0].as_ref() as &dyn Catalog),
        ("healthy".into(), catalogs_owned[1].as_ref() as &dyn Catalog),
    ]
    .into_iter()
    .collect();
    let store_config = ti_contracts::StoreConfig {
        width: "10s".into(),
        ..Default::default()
    };
    let config = TiConfig {
        stores: [
            ("default".into(), store_config.clone()),
            ("healthy".into(), store_config),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let mut bucketer = ti_ingest::MultiStoreBucketer::new(&config).unwrap();
    let mut failed_sink = FailOnce {
        store: &mut failing,
        fail: true,
        publish_before_error: false,
    };
    let mut sinks = [
        ("default".into(), &mut failed_sink as &mut dyn ShardSink),
        ("healthy".into(), &mut healthy as &mut dyn ShardSink),
    ]
    .into_iter()
    .collect();
    for second in [1, 40] {
        let result = bucketer.ingest_point(
            VESSEL,
            PATHS[0],
            "battery",
            EPOCH + second,
            &ti_ingest::NormalizedValue::Double(VALUES[0]),
            &config,
            &catalogs,
            &mut sinks,
        );
        if second == 1 {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err());
        }
    }
    bucketer.flush_all(&config, &catalogs, &mut sinks).unwrap();
    let field = catalogs_owned[1]
        .fields()
        .unwrap()
        .into_iter()
        .find(|f| f.path == PATHS[0] && f.agg == Some(Agg::Mean))
        .unwrap();
    assert_eq!(
        healthy
            .eval(
                ShardKey {
                    vessel: 0,
                    shard: 0
                },
                &Predicate::Present(field.id)
            )
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        vec![0, 4]
    );
    assert_eq!(
        bucketer.bucketer("default").unwrap().pending_retry_count(),
        1
    );
}
