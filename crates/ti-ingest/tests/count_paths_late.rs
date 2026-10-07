use ti_contracts::{ShardSink, TiConfig, EPOCH};
use ti_ingest::{NormalizedValue, WatermarkBucketer};
use ti_store::Store;

#[test]
fn evicted_late_event_requires_explicit_history_repair() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(dir.path(), 10).unwrap();
    let catalog = store.catalog().clone();
    let config = TiConfig { ingest: ti_contracts::IngestConfig { count_paths: vec!["cycles".into()] }, ..Default::default() };
    let mut bucketer = WatermarkBucketer::new(&config);
    for bucket in 0..130 {
        bucketer.ingest_point("robots.urn:fleet:late", "cycles", "a",
            EPOCH + bucket * 10 + 1, NormalizedValue::Double(99.0),
            &config, catalog.as_ref(), &mut store).unwrap();
        bucketer.flush_all(&config, catalog.as_ref(), &mut store).unwrap();
    }
    let before = store.seal(ti_contracts::ShardKey { vessel: 0, shard: 0 }).unwrap();
    let error = bucketer.ingest_point("robots.urn:fleet:late", "cycles", "a",
        EPOCH + 2, NormalizedValue::Double(99.0), &config, catalog.as_ref(), &mut store).unwrap_err();
    assert!(error.to_string().contains("repair with historical backfill"));
    assert_eq!(store.manifest().get(before.key).unwrap().hash, before.hash);
}
