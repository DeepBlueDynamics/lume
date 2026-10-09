#![cfg(feature = "ti")]
use lume::ti_resolve::PathsResolver;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use ti_contracts::{Agg, BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey, EPOCH};
#[path = "support/resolve_eval.rs"]
mod live_evaluation;
#[test]
fn one_hundred_phrases_reach_ninety_percent_top_three() {
    let bundle: Value =
        serde_json::from_str(include_str!("../src/ti_resolve/signalk_paths.json")).unwrap();
    let fields: Vec<_> = bundle["entries"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let pattern = e["pattern"].as_str().unwrap();
            let instance = if pattern.starts_with("electrical.batteries.") {
                "house"
            } else if pattern.starts_with("propulsion.") {
                "port"
            } else {
                "0"
            };
            FieldSpec {
                id: i as u32,
                path: pattern.replace('*', instance),
                agg: Some(Agg::Mean),
                kind: FieldKind::Bsi { scale: 3 },
                units: e["units"].as_str().map(String::from),
            }
        })
        .collect();
    let catalog = ti_sql::SqlCatalog::new(10, fields, vec![], BTreeMap::new()).unwrap();
    let resolver = PathsResolver::new(&catalog);
    let cases: Vec<Value> = serde_json::from_str(include_str!("golden/resolve.json")).unwrap();
    assert_eq!(cases.len(), 100);
    assert_eq!(
        cases
            .iter()
            .map(|c| c["phrase"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        100
    );
    let mut passed = 0;
    for case in &cases {
        let phrase = case["phrase"].as_str().unwrap();
        let path = case["expected_path"].as_str().unwrap();
        assert!(
            catalog.fields.iter().any(|f| f.path == path),
            "unknown expected path: {path}"
        );
        let ranked = resolver.rank(phrase, 3);
        if ranked
            .iter()
            .any(|(c, _)| c.strip_suffix("@mean") == Some(path))
        {
            passed += 1;
        } else {
            eprintln!("MISS {phrase:?} => {path}; got {ranked:?}");
        }
    }
    println!(
        "resolve top-3: {passed}/100 against {} catalog columns",
        catalog.fields.len()
    );
    assert!(passed >= 90, "resolve top-3 {passed}/100");
}
#[test]
fn canonical_columns_latest_nonnull_and_vessel_filters() {
    let fields = vec![
        FieldSpec {
            id: 7,
            path: "navigation.speedOverGround".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 3 },
            units: Some("m/s".into()),
        },
        FieldSpec {
            id: 8,
            path: "custom.bilgeLevel".into(),
            agg: None,
            kind: FieldKind::Bsi { scale: 3 },
            units: Some("m".into()),
        },
        FieldSpec {
            id: 9,
            path: "custom.condition".into(),
            agg: None,
            kind: FieldKind::Set,
            units: None,
        },
    ];
    let vessels = (0..2)
        .map(|ord| ti_sql::VesselInfo {
            ord,
            urn: format!("vessels.urn:test:{ord}"),
            name: Some(format!("boat {ord}")),
            mmsi: None,
            first_seen: EPOCH,
            last_seen: EPOCH + 700000,
        })
        .collect();
    let dictionaries = BTreeMap::from([(9, BTreeMap::from([(0, "dry".into())]))]);
    let catalog = ti_sql::SqlCatalog::new(10, fields.clone(), vessels, dictionaries).unwrap();
    let mut memory = ti_core::MemorySource::new();
    for (vessel, shard, bucket, value) in [(0, 0, 2, 1000), (1, 0, 3, 2000), (0, 1, 65537, 3000)] {
        let mut s = ti_core::MemoryShard::new(ShardKey { vessel, shard }).unwrap();
        for field in &fields {
            s.register_field(field.clone()).unwrap();
        }
        s.register_set_value(9, 0, "dry").unwrap();
        s.apply(&[BucketRecord {
            vessel,
            bucket,
            field: 7,
            value: FieldValue::Int(value),
            rewrite: false,
        }])
        .unwrap();
        if vessel == 1 {
            s.apply(&[
                BucketRecord {
                    vessel,
                    bucket,
                    field: 8,
                    value: FieldValue::Int(4000),
                    rewrite: false,
                },
                BucketRecord {
                    vessel,
                    bucket,
                    field: 9,
                    value: FieldValue::SetValue(0),
                    rewrite: false,
                },
            ])
            .unwrap();
        }
        memory.insert(s);
    }
    let source = ti_sql::FixtureSource {
        memory,
        catalog: catalog.clone(),
    };
    let runtime = ti_sql::surface_runtime().unwrap();
    let session = runtime
        .block_on(ti_sql::SqlSession::new(Arc::new(source), catalog))
        .unwrap();
    let engine = ti_sql::TiEngine::from_session(session, Default::default());
    let resolver = PathsResolver::new(&engine.session.catalog);
    runtime.block_on(async {
        let result = resolver
            .resolve(&engine, &json!({"phrase":"SOG","limit":1}))
            .await
            .unwrap();
        let c = &result["candidates"][0];
        assert_eq!(c["path"], "navigation.speedOverGround");
        assert_eq!(c["column"], "navigation.speedOverGround@mean");
        assert_eq!(c["agg"], "mean");
        assert_eq!(c["units"], "m/s");
        assert_eq!(c["last_value"], 3.0);
        assert_eq!(c["last_vessel"], "vessels.urn:test:0");
        assert!(c["last_ts"].is_string());
        assert!(c["description"]
            .as_str()
            .unwrap()
            .contains("speed over ground"));
        let inferred = resolver
            .resolve(&engine, &json!({"phrase":"boat 1 SOG","limit":1}))
            .await
            .unwrap();
        assert_eq!(
            inferred["candidates"][0]["last_vessel"],
            "vessels.urn:test:1"
        );
        let result = resolver
            .resolve(&engine, &json!({"phrase":"SOG","vessel":"boat 1"}))
            .await
            .unwrap();
        assert_eq!(result["candidates"][0]["last_value"], 2.0);
        let result = resolver
            .resolve(
                &engine,
                &json!({"phrase":"custom bilge level","vessel":"boat 0"}),
            )
            .await
            .unwrap();
        assert!(!result["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["path"] == "custom.bilgeLevel"));
        let result = resolver
            .resolve(
                &engine,
                &json!({"phrase":"custom condition","vessel":"boat 1","limit":1}),
            )
            .await
            .unwrap();
        assert_eq!(result["candidates"][0]["last_value"], json!("dry"));
        assert!(resolver
            .resolve(&engine, &json!({"phrase":"SOG","vessel":"unknown"}))
            .await
            .is_err());
        assert!(resolver
            .resolve(&engine, &json!({"phrase":"SOG","limit":0}))
            .await
            .is_err());
        assert!(resolver
            .resolve(&engine, &json!({"phrase":""}))
            .await
            .is_err());
        let none = resolver
            .resolve(&engine, &json!({"phrase":"unrecognizedxyz"}))
            .await
            .unwrap();
        assert!(!none["candidates"].as_array().unwrap().is_empty());
        assert_eq!(none["match_mode"], "catalog_fallback");
        assert!(none["hint"]
            .as_str()
            .unwrap()
            .contains("No lexical matches"));
    });
}

#[test]
fn vocabulary_units_typos_and_distinct_paths_work_without_boat_specific_names() {
    let mut fields = Vec::new();
    for path in [
        "environment.depth.belowTransducer",
        "electrical.batteries.reserve.voltage",
        "electrical.batteries.reserve.current",
        "navigation.position.latitude",
    ] {
        for agg in [Agg::Mean, Agg::Min, Agg::Max, Agg::Last] {
            fields.push(FieldSpec {
                id: fields.len() as u32,
                path: path.into(),
                agg: Some(agg),
                kind: FieldKind::Bsi { scale: 3 },
                units: None,
            });
        }
    }
    let catalog = ti_sql::SqlCatalog::new(10, fields, vec![], BTreeMap::new()).unwrap();
    let resolver = PathsResolver::new(&catalog);
    for (phrase, path) in [
        ("reserve bank volts", "electrical.batteries.reserve.voltage"),
        ("reserve bank amps", "electrical.batteries.reserve.current"),
        ("deep beneath sounder", "environment.depth.belowTransducer"),
        ("latitdue", "navigation.position.latitude"),
    ] {
        let ranked = resolver.rank(phrase, 3);
        assert!(
            ranked
                .iter()
                .any(|(c, _)| c.split('@').next() == Some(path)),
            "{phrase}: {ranked:?}"
        );
        let unique: std::collections::BTreeSet<_> = ranked
            .iter()
            .map(|(c, _)| c.split('@').next().unwrap())
            .collect();
        assert_eq!(unique.len(), ranked.len(), "{ranked:?}");
    }
    assert_eq!(
        resolver.rank("minimum depht", 1)[0].0,
        "environment.depth.belowTransducer@min"
    );
    assert!(!resolver.rank("zzyyxxyy", 3).is_empty());
}

#[test]
fn nautical_idioms_depth_intent_and_provenance_filtering() {
    let fields = [
        (
            "environment.depth.belowTransducer",
            Some(Agg::Mean),
            FieldKind::Bsi { scale: 2 },
        ),
        (
            "environment.outside.pressure",
            Some(Agg::Mean),
            FieldKind::Bsi { scale: 0 },
        ),
        (
            "tanks.freshWater.reserve.currentLevel",
            Some(Agg::Mean),
            FieldKind::Bsi { scale: 3 },
        ),
        (
            "navigation.speedOverGround",
            Some(Agg::Mean),
            FieldKind::Bsi { scale: 3 },
        ),
        ("navigation.speedOverGround$source", None, FieldKind::Set),
        ("electrical.bilge.pumpCycles$source", None, FieldKind::Set),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (path, agg, kind))| FieldSpec {
        id: i as u32,
        path: path.into(),
        agg,
        kind,
        units: None,
    })
    .collect();
    let catalog = ti_sql::SqlCatalog::new(10, fields, vec![], BTreeMap::new()).unwrap();
    let resolver = PathsResolver::new(&catalog);
    assert!(resolver.rank("is the glass dropping", 3)[0]
        .0
        .starts_with("environment.outside.pressure"));
    for phrase in [
        "how much water under the hull",
        "water under the keel",
        "water under us",
        "water beneath",
    ] {
        let ranked = resolver.rank(phrase, 3);
        assert!(
            ranked[0].0.starts_with("environment.depth."),
            "{phrase}: {ranked:?}"
        );
        assert!(ranked.iter().all(|(c, _)| !c.contains("$source")));
    }
    for phrase in ["SOG", "bilge pump", "unrecognizedxyz"] {
        let ranked = resolver.rank(phrase, 20);
        assert!(!ranked.is_empty());
        assert!(
            ranked.iter().all(|(c, _)| !c.contains("$source")),
            "{phrase}: {ranked:?}"
        );
    }
    for phrase in ["SOG source", "SOG sensor"] {
        assert!(
            resolver
                .rank(phrase, 20)
                .iter()
                .any(|(c, _)| c.contains("$source")),
            "{phrase}"
        );
    }
}
