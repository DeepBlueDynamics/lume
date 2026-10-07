use ti_contracts::{Agg, Catalog, FieldKind, FieldValue, TiConfig};
use ti_ingest::{BucketWindow, Classifier, NormalizedValue};

#[test]
fn exact_strict_config_and_finite_numeric_leaves() {
    let config = TiConfig::from_toml("[ingest]\ncount_paths=['electrical.bilge.pumpCycles']").unwrap();
    for input in [
        "[ingest]\ncount_path=['x']",
        "[ingest]\ncount_paths=['x','x']",
        "[ingest]\ncount_paths=['x.*']",
        "[ingest]\ncount_paths=['x@count']",
        "[ingest]\ncount_paths=['']",
    ] {
        assert!(TiConfig::from_toml(input).is_err(), "{input}");
    }
    let mut classifier = Classifier::new(&config);
    for value in [
        NormalizedValue::String("1".into()), NormalizedValue::Bool(true),
        NormalizedValue::Null, NormalizedValue::Double(f64::NAN),
        NormalizedValue::Double(f64::INFINITY), NormalizedValue::Geo { lat: 1.0, lon: 2.0 },
    ] {
        assert!(classifier.classify("robots.urn:fleet:one", "electrical.bilge.pumpCycles", &value).is_none());
    }
    assert!(classifier.classify("robots.urn:fleet:one", "electrical.bilge.pumpCycles", &NormalizedValue::Double(99.0)).is_some());
    // A parent listed explicitly does not select its numeric leaves.
    assert!(classifier.classify("robots.urn:fleet:one", "electrical.bilge.pumpCycles.value", &NormalizedValue::String("text".into())).is_some());
}

#[test]
fn preferred_source_counts_events_instead_of_magnitudes_and_preserves_at_count() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = ti_store::DiskCatalog::open_or_create(dir.path()).unwrap();
    let mut window = BucketWindow::default();
    let config = TiConfig { profiles: ti_contracts::AggregateProfiles { opt_in: vec!["count".into()], ..Default::default() }, ..Default::default() };
    for (source, priority, magnitude) in [("backup", 1, 99.0), ("backup", 1, 99.0), ("preferred", 0, 0.0), ("backup", 1, 99.0), ("preferred", 0, -12.0)] {
        window.add_numeric("cycles", magnitude, 3, source, 0);
        window.add_event_sample("cycles", source, priority).unwrap();
    }
    let records = window.emit_records(0, 0, false, &config, &catalog).unwrap();
    let fields = catalog.fields().unwrap();
    let bare = fields.iter().find(|f| f.path == "cycles" && f.agg.is_none()).unwrap();
    assert_eq!(bare.kind, FieldKind::Count);
    assert_eq!(records.iter().find(|r| r.field == bare.id).unwrap().value, FieldValue::Int(2));
    let count = fields.iter().find(|f| f.agg == Some(Agg::Count)).unwrap();
    assert_eq!(records.iter().find(|r| r.field == count.id).unwrap().value, FieldValue::Int(5));
    let mut first_seen = BucketWindow::default();
    first_seen.add_event_sample("cycles", "a", usize::MAX).unwrap();
    first_seen.add_event_sample("cycles", "b", usize::MAX).unwrap();
    first_seen.add_event_sample("cycles", "a", usize::MAX).unwrap();
    assert_eq!(first_seen.event_counts["cycles"].count, 2);
}

#[test]
fn unrepresentable_only_bucket_keeps_counts_and_a_skipped_magnitude_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = ti_store::Store::open_or_create(dir.path(), 10).unwrap();
    let catalog = store.catalog().clone();
    let config = TiConfig {
        ingest: ti_contracts::IngestConfig { count_paths: vec!["cycles".into()] },
        profiles: ti_contracts::AggregateProfiles { opt_in: vec!["count".into()], ..Default::default() },
        ..TiConfig::default()
    };
    let mut bucketer = ti_ingest::WatermarkBucketer::new(&config);
    bucketer.ingest_point(
        "robots.urn:fleet:one", "cycles", "a", ti_contracts::EPOCH + 1,
        NormalizedValue::Double(f64::MAX), &config, catalog.as_ref(), &mut store,
    ).unwrap();
    bucketer.flush_all(&config, catalog.as_ref(), &mut store).unwrap();
    let fields = catalog.fields().unwrap();
    assert!(fields.iter().any(|field| field.path == "cycles" && field.agg.is_none() && field.kind == FieldKind::Count));
    assert!(fields.iter().any(|field| field.path == "cycles" && field.agg == Some(Agg::Count)));
    assert!(fields.iter().any(|field| field.path == "cycles@skipped_magnitudes" && field.kind == FieldKind::Count));
    assert!(!fields.iter().any(|field| field.path == "cycles" && matches!(field.kind, FieldKind::Bsi { .. })));
}
