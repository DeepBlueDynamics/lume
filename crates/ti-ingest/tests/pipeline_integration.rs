use std::sync::Arc;
use tempfile::tempdir;
use ti_contracts::{
    validate_ordinary_set_rows, Catalog, DerivedKind, DerivedRule, ShardKey, ShardSink, TiConfig,
};
use ti_ingest::{decode_delta, normalize_point, SignalKDelta, WatermarkBucketer};
use ti_store::Store;

#[test]
fn test_end_to_end_pipeline_and_watermark() {
    let tmp = tempdir().unwrap();
    let mut config = TiConfig {
        width_seconds: 10,
        ..Default::default()
    };
    config.source_priorities.insert(
        "navigation.state".to_string(),
        vec!["n2k.115".to_string(), "nmea0183.0".to_string()],
    );
    config.derived.push(DerivedRule {
        path: "propulsion.main.state".to_string(),
        output: "propulsion.main.starts".to_string(),
        kind: DerivedKind::Transition,
        state: Some("running".to_string()),
    });

    let mut store = Store::open_or_create(tmp.path(), config.width_seconds).unwrap();
    let catalog = Arc::clone(store.catalog());
    let mut bucketer = WatermarkBucketer::new(&config);

    let self_urn = "vessels.urn:mrn:imo:mmsi:230999999";

    // 1. Initial stopped state at t = 1577836801 (Bucket 0: 1577836800..1577836810)
    let delta1: SignalKDelta = serde_json::from_value(serde_json::json!({
        "context": "vessels.self",
        "updates": [{
            "$source": "n2k.115",
            "timestamp": "2020-01-01T00:00:01.000Z",
            "values": [
                { "path": "navigation.speedOverGround", "value": 5.5 },
                { "path": "navigation.state", "value": "anchored" },
                { "path": "propulsion.main.state", "value": "stopped" }
            ]
        }]
    }))
    .unwrap();

    let (points1, _) = decode_delta(&delta1, self_urn, 1577836801);
    for raw in points1 {
        for p in normalize_point(raw, &config.allow_paths, &config.deny_paths) {
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

    // 2. Competing lower priority source for navigation.state in bucket 0
    let delta1_competing: SignalKDelta = serde_json::from_value(serde_json::json!({
        "context": "vessels.self",
        "updates": [{
            "$source": "nmea0183.0",
            "timestamp": "2020-01-01T00:00:02.000Z",
            "values": [
                { "path": "navigation.state", "value": "motoring" }
            ]
        }]
    }))
    .unwrap();

    let (points1_c, _) = decode_delta(&delta1_competing, self_urn, 1577836802);
    for raw in points1_c {
        for p in normalize_point(raw, &config.allow_paths, &config.deny_paths) {
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

    // Bucket 0 is still open because max_event_time is 1577836802, watermark is 1577836802 - 30 < 1577836810
    assert_eq!(bucketer.open_bucket_count(), 1);

    // 3. Transition to running at t = 1577836812 (Bucket 1: 1577836810..1577836820)
    let delta2: SignalKDelta = serde_json::from_value(serde_json::json!({
        "context": "vessels.self",
        "updates": [{
            "$source": "n2k.115",
            "timestamp": "2020-01-01T00:00:12.000Z",
            "values": [
                { "path": "navigation.speedOverGround", "value": 6.0 },
                { "path": "propulsion.main.state", "value": "running" }
            ]
        }]
    }))
    .unwrap();

    let (points2, _) = decode_delta(&delta2, self_urn, 1577836812);
    for raw in points2 {
        for p in normalize_point(raw, &config.allow_paths, &config.deny_paths) {
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

    assert_eq!(bucketer.open_bucket_count(), 2);

    // 4. Time advances past watermark for bucket 0:
    // Bucket 0 end is 1577836810. Watermark = max_event_time - 30.
    // To close bucket 0, watermark >= 1577836810 -> max_event_time >= 1577836840.
    let delta_adv: SignalKDelta = serde_json::from_value(serde_json::json!({
        "context": "vessels.self",
        "updates": [{
            "$source": "n2k.115",
            "timestamp": "2020-01-01T00:00:45.000Z",
            "values": [
                { "path": "navigation.speedOverGround", "value": 6.2 }
            ]
        }]
    }))
    .unwrap();

    let (points_adv, _) = decode_delta(&delta_adv, self_urn, 1577836845);
    for raw in points_adv {
        for p in normalize_point(raw, &config.allow_paths, &config.deny_paths) {
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

    // Bucket 0 has closed!
    assert!(bucketer.is_bucket_closed(0, 0));

    // 5. Late data arrives for bucket 0!
    let delta_late: SignalKDelta = serde_json::from_value(serde_json::json!({
        "context": "vessels.self",
        "updates": [{
            "$source": "n2k.115",
            "timestamp": "2020-01-01T00:00:05.000Z",
            "values": [
                { "path": "navigation.speedOverGround", "value": 5.8 }
            ]
        }]
    }))
    .unwrap();

    let (points_late, _) = decode_delta(&delta_late, self_urn, 1577836846);
    for raw in points_late {
        for p in normalize_point(raw, &config.allow_paths, &config.deny_paths) {
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

    // Flush remaining open buckets and store
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut store)
        .unwrap();
    store.flush().unwrap();

    // Verify D21 ordinary set invariant on navigation.state
    let all_fields = catalog.fields().unwrap();
    let nav_state_field = all_fields
        .iter()
        .find(|f| f.path == "navigation.state")
        .unwrap();

    let shard_key = ShardKey {
        vessel: 0,
        shard: 0,
    };

    let shard = store.open_shard(&shard_key).unwrap();
    if let ti_store::row::FieldData::Set(set_field) =
        shard.data.fields.get(&nav_state_field.id).unwrap()
    {
        let presence_bm = set_field.presence();
        let rows: Vec<_> = set_field.rows().values().cloned().collect();
        validate_ordinary_set_rows(presence_bm, &rows).unwrap();

        let anchored_id = set_field.dictionary().get("anchored").unwrap();
        let anchored_bm = set_field.rows().get(anchored_id).unwrap();
        assert!(anchored_bm.contains(0)); // bucket 0 contains preferred source value

        // Lower priority source value "motoring" was excluded during bucketing, never registered
        assert!(set_field.dictionary().get("motoring").is_none());
    } else {
        panic!("expected Set field data");
    }
}
