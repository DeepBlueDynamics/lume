//! Lume's own metrics land in the store as a `lume.urn:` entity with numeric `lume.*` fields.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ti_contracts::{Agg, Catalog, Predicate, ShardKey, ShardSource, TiConfig, EPOCH};
use ti_ingest::self_telemetry::{SelfStats, SelfTelemetry, SOURCE};
use ti_ingest::watermark::WatermarkBucketer;
use ti_ingest::NormalizedValue;
use ti_store::Store;

#[test]
fn self_samples_are_stored_under_a_lume_entity() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(root.path(), 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    let config = TiConfig::default();
    let mut bucketer = WatermarkBucketer::new(&config);
    let mut telemetry = SelfTelemetry::new("test-box", Duration::from_secs(10));

    let start = Instant::now();
    for (step, records) in [(0u64, 1_000u64), (1, 201_000)] {
        let ts = EPOCH + 1 + step as i64 * 10;
        let stats = SelfStats {
            records_ingested: records,
            ..Default::default()
        };
        for (path, value) in telemetry.sample(start + Duration::from_secs(step * 10), &stats) {
            bucketer
                .ingest_point(
                    telemetry.context(),
                    path,
                    SOURCE,
                    ts,
                    NormalizedValue::Double(value),
                    &config,
                    catalog.as_ref(),
                    &mut store,
                )
                .unwrap_or_else(|e| panic!("{path} rejected: {e}"));
        }
    }
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut store)
        .unwrap();

    assert_eq!(catalog.vessel_urn(0).unwrap(), "lume.urn:host:test-box");
    let fields = catalog.fields().unwrap();
    for path in ["lume.ingest.recordsIngested", "lume.ingest.valuesPerSecond"] {
        let field = fields
            .iter()
            .find(|f| f.path == path && f.agg == Some(Agg::Mean))
            .unwrap_or_else(|| panic!("no mean field for {path}"));
        let key = ShardKey {
            vessel: 0,
            shard: 0,
        };
        let present: Vec<u32> = store
            .eval(key, &Predicate::Present(field.id))
            .unwrap()
            .iter()
            .collect();
        let expected = if path.ends_with("valuesPerSecond") {
            vec![1]
        } else {
            vec![0, 1]
        };
        assert_eq!(present, expected, "{path}");
    }
}
