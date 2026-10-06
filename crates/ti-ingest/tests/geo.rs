use std::{collections::BTreeSet, sync::Arc};
use ti_contracts::{Catalog, ShardKey, ShardSink, TiConfig, EPOCH};
use ti_ingest::{normalize_point, NormalizedValue, RawDataPoint, WatermarkBucketer};
use ti_store::Store;

#[test]
fn live_positions_persist_cells_at_three_resolutions() {
    let tmp = tempfile::tempdir().unwrap();
    let config = TiConfig::default();
    let mut store = Store::open_or_create(tmp.path(), config.width_seconds).unwrap();
    let catalog = Arc::clone(store.catalog());
    let mut bucketer = WatermarkBucketer::new(&config);
    for (offset, lat, lon) in [(1, 36.0, -122.0), (2, 36.01, -122.01)] {
        let raw = RawDataPoint {
            context: "vessels.urn:geo:1".into(),
            path: "navigation.position".into(),
            source: "gps".into(),
            timestamp: EPOCH + offset,
            value: serde_json::json!({"latitude":lat,"longitude":lon}),
        };
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
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut store)
        .unwrap();
    store.flush().unwrap();
    let field = catalog
        .fields()
        .unwrap()
        .into_iter()
        .find(|f| f.path == "navigation.position")
        .unwrap();
    let key = ShardKey {
        vessel: 0,
        shard: 0,
    };
    store.seal(key).unwrap();
    let sealed = store.sealed_shard(&key).unwrap();
    let expected: BTreeSet<_> = [(36.0, -122.0), (36.01, -122.01)]
        .into_iter()
        .flat_map(|(lat, lon)| ti_geo::cells_for(lat, lon).unwrap())
        .collect();
    for cell in expected {
        let hits = sealed
            .data
            .eval_masks(
                &ti_contracts::Predicate::GeoCover {
                    field: field.id,
                    cells: vec![cell],
                },
                None,
            )
            .unwrap()
            .truth;
        assert_eq!(hits.iter().collect::<Vec<_>>(), vec![0]);
    }
    let zero = sealed
        .data
        .eval_masks(
            &ti_contracts::Predicate::GeoCover {
                field: field.id,
                cells: vec![0],
            },
            None,
        )
        .unwrap()
        .truth;
    assert!(zero.is_empty());
    assert!(bucketer
        .ingest_point(
            "vessels.urn:geo:1",
            "navigation.position",
            "gps",
            EPOCH + 21,
            NormalizedValue::Geo {
                lat: 91.0,
                lon: 0.0
            },
            &config,
            catalog.as_ref(),
            &mut store
        )
        .is_err());
}
