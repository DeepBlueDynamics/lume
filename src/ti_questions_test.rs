use super::*;
use std::{collections::BTreeMap, sync::Arc};
use ti_contracts::{Agg, BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey, EPOCH};
fn fixture() -> Value {
    serde_json::from_str(include_str!("../tests/golden/fleet_questions.json")).unwrap()
}
fn compare(actual: &Value, expected: &Value, tolerance: &Value) {
    fn normalized(v: &Value) -> Value {
        match v {
            Value::String(s) => json!(s.replace(' ', "T").trim_end_matches('Z')),
            Value::Array(a) => json!(a.iter().map(normalized).collect::<Vec<_>>()),
            Value::Object(o) => {
                Value::Object(o.iter().map(|(k, v)| (k.clone(), normalized(v))).collect())
            }
            v => v.clone(),
        }
    }
    let mut a = normalized(actual).as_array().unwrap().clone();
    let mut e = normalized(expected).as_array().unwrap().clone();
    // Align on the oracle's exact keys, never on approximate BSI values.
    let sort = |rows: &mut Vec<Value>| {
        rows.sort_by_key(|row| {
            tolerance["sort"]
                .as_array()
                .unwrap()
                .iter()
                .map(|key| row[key.as_str().unwrap()].to_string())
                .collect::<Vec<_>>()
        })
    };
    sort(&mut a);
    sort(&mut e);
    assert_eq!(a.len(), e.len());
    for (a, e) in a.iter().zip(&e) {
        for (name, expected) in e.as_object().unwrap() {
            let actual = &a[name];
            if let Some(scale) = tolerance["bsi"][name].as_i64() {
                match (actual.as_f64(), expected.as_f64()) {
                    (Some(a), Some(e)) => assert!(
                        (a - e).abs() <= 10f64.powi(-(scale as i32)) + 1e-9,
                        "{name}: {a} vs {e}"
                    ),
                    _ => assert_eq!(actual, expected, "{name}"),
                }
            } else {
                assert_eq!(actual, expected, "{name}");
            }
        }
    }
}
async fn run(engine: &ti_sql::TiEngine, fixture: &Value, regression: bool) {
    let entries = fixture["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 20);
    let sources = dispatch(
        engine,
        "ti_resolve",
        &json!({"phrase":"port propulsion state reporting sources","limit":1}),
    )
    .await
    .unwrap();
    assert_eq!(
        sources["candidates"][0]["column"],
        "propulsion.port.state$source"
    );
    for entry in entries {
        let resolved = dispatch(
            engine,
            "ti_resolve",
            &json!({"phrase":entry["resolve_phrase"],"limit":1}),
        )
        .await
        .unwrap();
        let column = resolved["candidates"][0]["column"]
            .as_str()
            .unwrap_or_else(|| panic!("No resolve candidate: {}", entry["id"]));
        assert_eq!(
            column,
            entry["expected_column"].as_str().unwrap(),
            "{}: {}",
            entry["id"],
            entry["question"]
        );
        let sql = entry["sql_template"]
            .as_str()
            .unwrap()
            .replace("{column}", &column.replace('"', "\"\""));
        let answer = dispatch(engine, "ti_query", &json!({"sql":sql}))
            .await
            .unwrap();
        assert_eq!(answer["truncated"], false, "{}", entry["id"]);
        let expected = if regression {
            &fixture["regression"]["expected"][entry["id"].as_str().unwrap()]
        } else {
            &entry["expected"]
        };
        compare(&answer["rows"], expected, &entry["tolerance"]);
        println!("PASS {}: {}", entry["id"], entry["question"]);
    }
}
#[test]
fn twenty_scripted_questions_resolve_top_one_and_query() {
    let fixture = fixture();
    let mut names: BTreeMap<String, (String, Option<Agg>)> = BTreeMap::new();
    for e in fixture["entries"].as_array().unwrap() {
        let n = e["expected_column"].as_str().unwrap();
        let (path, agg) = n.split_once('@').map_or((n, None), |(p, a)| {
            (
                p,
                Some(match a {
                    "mean" => Agg::Mean,
                    "min" => Agg::Min,
                    "max" => Agg::Max,
                    _ => panic!("{a}"),
                }),
            )
        });
        names.insert(n.into(), (path.into(), agg));
        // Real stores also contain reporting-source columns; they must not steal value phrases.
        names.insert(format!("{path}$source"), (format!("{path}$source"), None));
    }
    for (path, agg) in [
        ("propulsion.main.state", None),
        ("propulsion.starboard.state", None),
        ("navigation.state", None),
        ("electrical.solar.house.panelPower", Some(Agg::Max)),
    ] {
        let name = if agg.is_some() {
            format!("{path}@max")
        } else {
            path.into()
        };
        names.insert(name, (path.into(), agg));
    }
    let fields: Vec<_> = names
        .values()
        .enumerate()
        .map(|(id, (path, agg))| FieldSpec {
            id: id as u32,
            path: path.clone(),
            agg: agg.clone(),
            kind: if path.ends_with(".state") || path.ends_with("$source") {
                FieldKind::Set
            } else {
                FieldKind::Bsi { scale: 3 }
            },
            units: None,
        })
        .collect();
    let urn = "vessels.urn:mrn:imo:mmsi:367000000";
    let ts = 1777636800i64; // 2026-05-01 12:00:00 UTC
    let bucket = ((ts - EPOCH) / 10) as u32;
    let key = ShardKey {
        vessel: 0,
        shard: bucket / 65536,
    };
    let mut shard = ti_core::MemoryShard::new(key).unwrap();
    let mut dictionaries = BTreeMap::new();
    for f in &fields {
        shard.register_field(f.clone()).unwrap();
        if f.kind == FieldKind::Set {
            shard.register_set_value(f.id, 0, "started").unwrap();
            shard.register_set_value(f.id, 1, "stopped").unwrap();
            dictionaries.insert(
                f.id,
                BTreeMap::from([(0, "started".into()), (1, "stopped".into())]),
            );
        }
        let value = match f.path.as_str() {
            "environment.wind.speedTrue" => Some(FieldValue::Int(4000)),
            "electrical.batteries.house.voltage" => Some(FieldValue::Int(24000)),
            "environment.depth.belowTransducer" => Some(FieldValue::Int(2000)),
            "navigation.speedOverGround" => Some(FieldValue::Int(5000)),
            "electrical.batteries.house.stateOfCharge" => Some(FieldValue::Int(850)),
            "environment.outside.temperature" => Some(FieldValue::Int(280000)),
            "electrical.batteries.house.current" => Some(FieldValue::Int(3000)),
            "propulsion.port.state" => Some(FieldValue::SetValue(0)),
            "propulsion.main.state" => Some(FieldValue::SetValue(1)),
            _ => None,
        };
        if let Some(value) = value {
            shard
                .apply(
                    &(0..3)
                        .map(|i| BucketRecord {
                            vessel: 0,
                            bucket: bucket + i,
                            field: f.id,
                            value: value.clone(),
                            rewrite: false,
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap();
        }
    }
    let vessels = vec![ti_sql::VesselInfo {
        ord: 0,
        urn: urn.into(),
        name: None,
        mmsi: None,
        first_seen: ts,
        last_seen: ts + 20,
    }];
    let catalog = ti_sql::SqlCatalog::new(10, fields, vessels, dictionaries).unwrap();
    let mut memory = ti_core::MemorySource::new();
    memory.insert(shard);
    let source = ti_sql::FixtureSource {
        memory,
        catalog: catalog.clone(),
    };
    let runtime = ti_sql::surface_runtime().unwrap();
    let session = runtime
        .block_on(ti_sql::SqlSession::new(Arc::new(source), catalog))
        .unwrap();
    let engine = ti_sql::TiEngine::from_session(session, Default::default());
    runtime.block_on(run(&engine, &fixture, true));
}
#[test]
#[ignore = "requires TI_QUESTIONS_STORE pointing at the golden store-full; run explicitly"]
fn twenty_questions_against_golden_store_oracle() {
    let root = std::env::var("TI_QUESTIONS_STORE").expect("TI_QUESTIONS_STORE");
    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        let engine = ti_sql::TiEngine::open(std::path::Path::new(&root), None, None)
            .await
            .unwrap();
        run(&engine, &fixture(), false).await;
    });
}
