use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use ti_contracts::{Agg, Catalog, Predicate, Result, ShardKey, ShardManifestEntry, ShardSink, ShardSource, TiConfig, EPOCH};
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
        values.extend(PATHS.iter().zip(VALUES).map(|(path,value)| serde_json::json!({"path":path,"value":value})));
    }
    serde_json::json!({
        "context": VESSEL,
        "updates": [{
            "$source":"n2k.115",
            "timestamp":chrono::DateTime::from_timestamp(EPOCH + second, 0).unwrap().to_rfc3339(),
            "values": values
        }]
    }).to_string()
}

fn ingest(second: i64, receive_second: i64, bucketer: &mut WatermarkBucketer, config: &TiConfig, catalog: &dyn Catalog, sink: &mut dyn ShardSink) {
    process_message(
        &delta(second, second % 3 == 1), VESSEL,
        UNIX_EPOCH + Duration::from_secs((EPOCH + receive_second) as u64),
        bucketer, config, catalog, sink, &mut None,
    ).unwrap();
}

fn assert_battery_buckets(store: &Store, expected: &[u32]) {
    let key = ShardKey { vessel:0, shard:0 };
    for path in PATHS {
        let field = store.catalog().fields().unwrap().into_iter()
            .find(|field| field.path == path && field.agg == Some(Agg::Mean)).unwrap();
        let actual: Vec<_> = store.eval(key, &Predicate::Present(field.id)).unwrap().iter().collect();
        assert_eq!(actual, expected, "{path}: missing a bucket");
    }
}

#[test]
fn three_second_cadence_survives_flush_and_wal_ticks_at_bucket_boundaries() {
    // Run both maintenance orders, including receive/event times on opposite sides
    // of the boundary. Maintenance never closes the active bucket prematurely.
    for before in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(root.path(),10).unwrap();
        let catalog = Arc::clone(store.catalog());
        let config = TiConfig::default();
        let mut bucketer = WatermarkBucketer::new(&config);
        for second in 0..90 {
            if before {
                store.tick().unwrap();
                if second % 5 == 0 { store.flush().unwrap(); }
            }
            ingest(second, second + 2, &mut bucketer, &config, catalog.as_ref(), &mut store);
            if !before {
                store.tick().unwrap();
                if second % 5 == 0 { store.flush().unwrap(); }
            }
        }
        // A late battery delta after bucket 0 closed must remain a rewrite,
        // rather than silently disappearing.
        process_message(&delta(7,true), VESSEL,
            UNIX_EPOCH + Duration::from_secs((EPOCH + 92) as u64),
            &mut bucketer, &config, catalog.as_ref(), &mut store, &mut None).unwrap();
        bucketer.flush_all(&config,catalog.as_ref(),&mut store).unwrap();
        assert_battery_buckets(&store,&(0..9).collect::<Vec<_>>());
        drop(store);
        let reopened = Store::open_or_create(root.path(),10).unwrap();
        assert_battery_buckets(&reopened,&(0..9).collect::<Vec<_>>());
    }
}

struct FailOnce<'a> { store: &'a mut Store, fail: bool }
impl ShardSink for FailOnce<'_> {
    fn apply(&mut self, records: &[ti_contracts::BucketRecord]) -> Result<()> {
        if self.fail {
            self.fail = false;
            return Err(std::io::Error::other("injected WAL/apply failure at bucket close").into());
        }
        self.store.apply(records)
    }
    fn flush(&mut self) -> Result<()> { self.store.flush() }
    fn seal(&mut self, key: ShardKey) -> Result<ShardManifestEntry> { self.store.seal(key) }
}

#[test]
fn failed_boundary_apply_keeps_samples_until_sink_acknowledges() {
    // The old implementation removes the window before apply and loses all
    // three battery paths in bucket 0 after the injected maintenance failure.
    for explicit_flush in [false,true] {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(root.path(),10).unwrap();
        let catalog = Arc::clone(store.catalog());
        let config = TiConfig::default();
        let mut bucketer = WatermarkBucketer::new(&config);
        for second in 0..40 {
            ingest(second,second,&mut bucketer,&config,catalog.as_ref(),&mut store);
        }
        store.tick().unwrap();
        let mut sink = FailOnce { store:&mut store, fail:true };
        let result = if explicit_flush {
            bucketer.flush_all(&config,catalog.as_ref(),&mut sink)
        } else {
            bucketer.advance_watermark(EPOCH+10,&config,catalog.as_ref(),&mut sink)
        };
        assert!(result.is_err());
        assert!(!bucketer.is_bucket_closed(0,0),"failed apply must not acknowledge close");
        bucketer.flush_all(&config,catalog.as_ref(),&mut sink).unwrap();
        assert_battery_buckets(&store,&[0,1,2,3]);
    }
}
