use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::tempdir;
use ti_contracts::{Agg, Catalog, ShardKey, ShardSink, StoreConfig, TiConfig, VesselSpec};
use ti_ingest::{MultiStoreBucketer, RawDataPoint};
use ti_store::Store;

#[test]
fn test_multi_store_sample_fanout_and_aggregates() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    let mut config = TiConfig {
        store_root: root.to_string_lossy().to_string(),
        ..Default::default()
    };

    let mut hr_aggs = BTreeMap::new();
    hr_aggs.insert("navigation.*".to_string(), vec!["last".to_string()]);
    hr_aggs.insert(
        "environment.wind.*".to_string(),
        vec!["mean".to_string(), "max".to_string()],
    );
    hr_aggs.insert("environment.depth.*".to_string(), vec!["min".to_string()]);

    let mut stores_cfg = BTreeMap::new();
    stores_cfg.insert(
        "default".to_string(),
        StoreConfig {
            width: "10s".into(),
            retention: "730d".into(),
            shore_retention: None,
            root: None,
            paths: None,
            aggs: BTreeMap::new(),
        },
    );
    stores_cfg.insert(
        "hr".to_string(),
        StoreConfig {
            width: "1s".into(),
            retention: "90d".into(),
            shore_retention: None,
            root: None,
            paths: Some(vec![
                "navigation.*".into(),
                "environment.wind.*".into(),
                "environment.depth.*".into(),
            ]),
            aggs: hr_aggs,
        },
    );
    config.stores = stores_cfg;

    // Open both stores at their resolved roots
    let resolved = config.resolved_stores();
    let default_root = resolved["default"].root.as_ref().unwrap();
    let hr_root = resolved["hr"].root.as_ref().unwrap();

    let mut default_store = Store::open_or_create(std::path::Path::new(default_root), 10).unwrap();
    let mut hr_store = Store::open_or_create(std::path::Path::new(hr_root), 1).unwrap();

    let mut multi_bucketer = MultiStoreBucketer::new(&config).unwrap();

    let self_urn = "vessels.urn:mrn:signalk:uuid:boat-fanout-1";

    // 1. Ingest a navigation sample that matches BOTH stores
    let nav_point = RawDataPoint {
        context: self_urn.to_string(),
        path: "navigation.speedOverGround".to_string(),
        source: "gps.0".to_string(),
        timestamp: 1770739205, // bucket 177073920 in 10s store, bucket 1770739205 in 1s store
        value: serde_json::json!(8.5),
    };

    {
        let default_catalog = Arc::clone(default_store.catalog());
        let hr_catalog = Arc::clone(hr_store.catalog());
        let catalogs: BTreeMap<String, &dyn Catalog> = [
            (
                "default".to_string(),
                default_catalog.as_ref() as &dyn Catalog,
            ),
            ("hr".to_string(), hr_catalog.as_ref() as &dyn Catalog),
        ]
        .into_iter()
        .collect();

        let mut sinks: BTreeMap<String, &mut dyn ShardSink> = [
            (
                "default".to_string(),
                &mut default_store as &mut dyn ShardSink,
            ),
            ("hr".to_string(), &mut hr_store as &mut dyn ShardSink),
        ]
        .into_iter()
        .collect();

        let routed = multi_bucketer
            .ingest_raw(nav_point, &config, &catalogs, &mut sinks)
            .unwrap();
        assert_eq!(
            routed, 2,
            "Navigation sample must route to BOTH default and hr stores"
        );

        // 2. Ingest a battery voltage sample that is NOT in hr allow-list
        let battery_point = RawDataPoint {
            context: self_urn.to_string(),
            path: "electrical.batteries.house.voltage".to_string(),
            source: "bms.0".to_string(),
            timestamp: 1770739205,
            value: serde_json::json!(25.4),
        };

        let routed_bat = multi_bucketer
            .ingest_raw(battery_point, &config, &catalogs, &mut sinks)
            .unwrap();
        assert_eq!(
            routed_bat, 1,
            "Battery sample must route ONLY to default store"
        );

        // 3. Flush all stores
        multi_bucketer
            .flush_all(&config, &catalogs, &mut sinks)
            .unwrap();
    }

    // Seal shard in default store (bucket 177073920 / 65536 = shard 2701)
    let v_default = default_store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: self_urn.to_string(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    let shard_default = 177073920u32 >> 16;
    default_store
        .seal(ShardKey {
            vessel: v_default,
            shard: shard_default,
        })
        .unwrap();

    // Seal shard in hr store (bucket 1770739205 / 65536 = shard 27019)
    let v_hr = hr_store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: self_urn.to_string(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    let shard_hr = 1770739205u32 >> 16;
    hr_store
        .seal(ShardKey {
            vessel: v_hr,
            shard: shard_hr,
        })
        .unwrap();

    // Verify Default Store (10 s, default profile: mean, min, max):
    let default_fields = default_store.catalog().fields().unwrap();
    let default_nav_mean = default_fields
        .iter()
        .find(|f| f.path == "navigation.speedOverGround" && f.agg == Some(Agg::Mean));
    let default_nav_min = default_fields
        .iter()
        .find(|f| f.path == "navigation.speedOverGround" && f.agg == Some(Agg::Min));
    let default_nav_max = default_fields
        .iter()
        .find(|f| f.path == "navigation.speedOverGround" && f.agg == Some(Agg::Max));
    let default_nav_last = default_fields
        .iter()
        .find(|f| f.path == "navigation.speedOverGround" && f.agg == Some(Agg::Last));
    let default_battery = default_fields
        .iter()
        .find(|f| f.path == "electrical.batteries.house.voltage");

    assert!(default_nav_mean.is_some(), "Default store must have @mean");
    assert!(default_nav_min.is_some(), "Default store must have @min");
    assert!(default_nav_max.is_some(), "Default store must have @max");
    assert!(
        default_nav_last.is_none(),
        "Default store should not have @last"
    );
    assert!(
        default_battery.is_some(),
        "Default store must have electrical.batteries"
    );

    // Verify HR Store (1 s, navigation.* profile: last ONLY):
    let hr_fields = hr_store.catalog().fields().unwrap();
    let hr_nav_last = hr_fields
        .iter()
        .find(|f| f.path == "navigation.speedOverGround" && f.agg == Some(Agg::Last));
    let hr_nav_mean = hr_fields
        .iter()
        .find(|f| f.path == "navigation.speedOverGround" && f.agg == Some(Agg::Mean));
    let hr_nav_min = hr_fields
        .iter()
        .find(|f| f.path == "navigation.speedOverGround" && f.agg == Some(Agg::Min));
    let hr_nav_max = hr_fields
        .iter()
        .find(|f| f.path == "navigation.speedOverGround" && f.agg == Some(Agg::Max));
    let hr_battery = hr_fields
        .iter()
        .find(|f| f.path == "electrical.batteries.house.voltage");

    assert!(
        hr_nav_last.is_some(),
        "HR store MUST have @last for navigation.*"
    );
    assert!(
        hr_nav_mean.is_none(),
        "HR store must NOT have @mean for navigation.*"
    );
    assert!(
        hr_nav_min.is_none(),
        "HR store must NOT have @min for navigation.*"
    );
    assert!(
        hr_nav_max.is_none(),
        "HR store must NOT have @max for navigation.*"
    );
    assert!(
        hr_battery.is_none(),
        "HR store must NOT have filtered path electrical.batteries"
    );
}
