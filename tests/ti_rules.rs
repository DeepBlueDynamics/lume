#![cfg(feature = "ti")]
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use ti_contracts::{
    Agg, AlertRule, BucketRecord, Catalog, Document, FieldKind, FieldSpec, FieldValue, ShardSink,
    TiConfig, VesselSpec,
};
use ti_store::{DocStore, Store};
static NEXT: AtomicU64 = AtomicU64::new(0);
const URN: &str = "vessels.urn:mrn:signalk:uuid:rules";
const PATH: &str = "electrical.batteries.house.voltage@min";
struct Fixture {
    root: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    fn new(values: &[i64]) -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "rules-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut store = Store::open_or_create(&root, 10).unwrap();
        let vessel = store
            .catalog()
            .register_vessel(&VesselSpec {
                urn: URN.into(),
                name: None,
                mmsi: None,
            })
            .unwrap();
        let field = store
            .catalog()
            .register_field(&FieldSpec {
                id: 0,
                path: "electrical.batteries.house.voltage".into(),
                agg: Some(Agg::Min),
                kind: FieldKind::Bsi { scale: 3 },
                units: Some("V".into()),
            })
            .unwrap();
        let records: Vec<_> = values
            .iter()
            .enumerate()
            .map(|(i, &value)| BucketRecord {
                vessel,
                bucket: i as u32 + 1,
                field,
                value: FieldValue::Int(value),
                rewrite: false,
            })
            .collect();
        store.apply(&records).unwrap();
        if !values.is_empty() {
            store
                .seal(ti_contracts::ShardKey { vessel, shard: 0 })
                .unwrap();
        }
        store.shutdown().unwrap();
        Self { root }
    }
}
fn battery() -> AlertRule {
    AlertRule {
        name: "battery".into(),
        severity: "warn".into(),
        when: format!("\"{PATH}\" < 24.6"),
        hold: Some("20s".into()),
        vessel: None,
        message: format!("house battery {{{PATH}}} V"),
        max_per_hour: 60,
    }
}
fn time(bucket: i64) -> i64 {
    ti_contracts::EPOCH + bucket * 10
}

#[test]
fn history_ranges_equal_intervals_and_are_searchable_and_idempotent() {
    let fixture = Fixture::new(&[
        24500, 24500, 25000, 24500, 25000, 24500, 24500, 24500, 25000,
    ]);
    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        let rules = vec![battery()];
        let docs = ti_sql::rules::backfill(&fixture.root, 10, rules.clone(), &lume::ti_rules::index_factory, false).await.unwrap();
        assert_eq!(docs.iter().map(|d| (d.ts_start, d.ts_end)).collect::<Vec<_>>(),
            vec![(time(1), Some(time(3))), (time(6), Some(time(9)))]);
        assert!(docs.iter().all(|d| d.body.contains("24.5 V") && d.body.contains("when:")));
        let (session, index) = ti_sql::rules::open_session(&fixture.root, 10,
            &lume::ti_rules::index_factory, DocStore::open(&fixture.root).unwrap()).await.unwrap();
        let sql = format!("SELECT CAST(extract(epoch FROM start) AS BIGINT) AS s, CAST(extract(epoch FROM \"end\") AS BIGINT) AS e FROM intervals('{}', '20s', '0s') ORDER BY start",
            rules[0].when.replace('\'', "''"));
        let rows = ti_sql::rows_json(&session.query(&sql).await.unwrap()).unwrap();
        assert_eq!(rows.iter().map(|r| (r["s"].as_i64().unwrap(), Some(r["e"].as_i64().unwrap()))).collect::<Vec<_>>(),
            docs.iter().map(|d| (d.ts_start, d.ts_end)).collect::<Vec<_>>());
        let hits = session.query("SELECT count(*) AS n FROM telemetry WHERE match(alerts, 'battery')").await.unwrap();
        assert_eq!(ti_sql::rows_json(&hits).unwrap()[0]["n"], 5);
        let joined = session.query("SELECT count(*) AS n FROM telemetry t JOIN docs d ON t.vessel = d.vessel AND t.ts >= d.ts_start AND t.ts < d.ts_end WHERE d.kind = 'alerts' AND match(d.body, 'battery')").await.unwrap();
        assert_eq!(ti_sql::rows_json(&joined).unwrap()[0]["n"], 5);
        assert_eq!(index.documents(None, Some("alerts"), Some("battery")).unwrap()[0].num_rows(), 2);
        let replay = ti_sql::rules::backfill(&fixture.root, 10, rules, &lume::ti_rules::index_factory, false).await.unwrap();
        assert_eq!(replay, docs);
        assert_eq!(DocStore::open(&fixture.root).unwrap().len(), 2);
    });
}

#[test]
fn dry_run_is_unpersisted_and_history_uses_bitmap_masks() {
    let fixture = Fixture::new(&[24500, 24500, 25000, 24500, 24500, 25000]);
    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        let before = DocStore::open(&fixture.root).unwrap();
        let docs = ti_sql::rules::backfill(
            &fixture.root,
            10,
            vec![battery()],
            &lume::ti_rules::index_factory,
            true,
        )
        .await
        .unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(DocStore::open(&fixture.root).unwrap().len(), before.len());
        assert!(!fixture.root.join("rules/state.json").exists());
        let (session, index) = ti_sql::rules::open_session(
            &fixture.root,
            10,
            &lume::ti_rules::index_factory,
            DocStore::in_memory(),
        )
        .await
        .unwrap();
        let mut runner = ti_sql::rules::RuleRunner::new(vec![battery()]).unwrap();
        runner.history(&session, index.as_ref()).await.unwrap();
        // Only the two alert-opening template values are materialized.
        assert_eq!(
            session
                .reports()
                .unwrap()
                .iter()
                .map(|r| r.materialized_rows)
                .sum::<u64>(),
            2
        );
    });
}

#[test]
fn ordered_alert_references_and_hourly_cap_do_not_self_retrigger() {
    let fixture = Fixture::new(&[24500, 25000, 24500, 25000, 24500, 25000, 24500, 25000]);
    let mut first = battery();
    first.hold = None;
    first.max_per_hour = 2;
    first.when.push_str(" OR match(alerts, 'battery')");
    first.message = "battery alarm".into();
    let second = AlertRule {
        name: "follow".into(),
        when: "match(alerts, 'battery')".into(),
        message: "following battery alert".into(),
        hold: None,
        ..battery()
    };
    let runtime = ti_sql::surface_runtime().unwrap();
    let docs = runtime
        .block_on(ti_sql::rules::backfill(
            &fixture.root,
            10,
            vec![first, second],
            &lume::ti_rules::index_factory,
            false,
        ))
        .unwrap();
    let starts = |name: &str| {
        docs.iter()
            .filter(|d| d.id.starts_with(&format!("rules/{name}/")))
            .map(|d| d.ts_start)
            .collect::<Vec<_>>()
    };
    assert_eq!(starts("battery"), vec![time(1), time(3)]);
    assert_eq!(starts("follow"), vec![time(1), time(3)]);
    assert_eq!(docs.len(), 4);
}

#[test]
fn live_observer_restores_hold_and_skips_replayed_buckets() {
    let fixture = Fixture::new(&[]);
    let config = TiConfig {
        rules: vec![battery()],
        ..Default::default()
    };
    let mut observer = lume::ti_rules::observer(&fixture.root, &config).unwrap();
    let mut store = Store::open_or_create(&fixture.root, 10).unwrap();
    let field = store.catalog().fields().unwrap()[0].id;
    let mut publish = |bucket, value| {
        store
            .apply(&[BucketRecord {
                vessel: 0,
                bucket,
                field,
                value: FieldValue::Int(value),
                rewrite: false,
            }])
            .unwrap();
        store.flush().unwrap();
    };
    publish(1, 24500);
    observer.on_closed(0, 1, 1).unwrap();
    assert!(DocStore::open(&fixture.root).unwrap().is_empty());
    publish(2, 24500);
    observer.on_closed(0, 2, 2).unwrap();
    let open = DocStore::open(&fixture.root)
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .clone();
    assert_eq!((open.ts_start, open.ts_end), (time(1), None));
    drop(observer);
    let mut observer = lume::ti_rules::observer(&fixture.root, &config).unwrap();
    observer.on_closed(0, 2, 2).unwrap();
    publish(3, 25000);
    observer.on_closed(0, 3, 3).unwrap();
    let docs = DocStore::open(&fixture.root).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs.iter().next().unwrap().id, open.id);
    assert_eq!(docs.iter().next().unwrap().ts_end, Some(time(3)));
}

#[test]
fn status_distinguishes_open_alerts_from_cleared_points_and_cli_dry_runs() {
    let fixture = Fixture::new(&[24500, 24500, 25000]);
    let mut docs = DocStore::open(&fixture.root).unwrap();
    docs.upsert_all([
        Document {
            id: "notifications/bilge/1577836810".into(),
            vessel: URN.into(),
            kind: "alerts".into(),
            ts_start: time(1),
            ts_end: None,
            title: "bilge (alarm)".into(),
            body: format!(
                "bilge alarm\nstate: alarm\nnotification_closed_at: {}",
                time(1)
            ),
        },
        Document {
            id: "notifications/battery/1577836820".into(),
            vessel: URN.into(),
            kind: "alerts".into(),
            ts_start: time(2),
            ts_end: None,
            title: "battery (warn)".into(),
            body: "battery warning\nstate: warn".into(),
        },
    ])
    .unwrap();
    let status = ti_sql::rules::alert_status(&fixture.root).unwrap();
    assert_eq!(status["alert_count"], 2);
    assert_eq!(status["active_alert_count"], 1);
    assert_eq!(status["active_alerts"][0]["title"], "battery (warn)");
    let runtime = ti_sql::surface_runtime().unwrap();
    let engine = runtime
        .block_on(ti_sql::TiEngine::open(&fixture.root, Some(10), None))
        .unwrap();
    let status = runtime.block_on(engine.status()).unwrap();
    assert_eq!(status["alerts"], 2);
    assert_eq!(status["active_alert_count"], 1);
    assert_eq!(status["active_alerts"][0]["title"], "battery (warn)");
    let config = format!("store_root = '{}'\n[[rules]]\nname = 'battery'\nseverity = 'warn'\nwhen = '\"{PATH}\" < 24.6'\nfor = '20s'\nmessage = 'battery {{{PATH}}} V'\n", fixture.root.display());
    std::fs::write(fixture.root.join("ti.toml"), config).unwrap();
    for command in [
        vec!["ti", "rules", "list"],
        vec!["ti", "rules", "test", "battery"],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_lume"))
            .args(command)
            .arg("--store")
            .arg(&fixture.root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(reply.get("rules").is_some() || reply["dry_run"] == true);
    }
    assert_eq!(DocStore::open(&fixture.root).unwrap().len(), 2);
}

#[test]
fn statement_injection_and_residual_predicates_are_rejected() {
    let mut rule = battery();
    rule.when.push_str("; DELETE FROM telemetry");
    assert!(ti_sql::rules::RuleRunner::new(vec![rule]).is_err());
    let fixture = Fixture::new(&[24500]);
    let rule = AlertRule {
        when: format!("sqrt(\"{PATH}\") < 5"),
        ..battery()
    };
    let result = ti_sql::surface_runtime()
        .unwrap()
        .block_on(ti_sql::rules::backfill(
            &fixture.root,
            10,
            vec![rule],
            &lume::ti_rules::index_factory,
            true,
        ));
    assert!(result.is_err());
}

#[test]
#[ignore = "requires TI_RULES_STORE pointing at golden store-full; host DuckDB oracle invokes this"]
fn golden_battery_rule_ranges() {
    let root = PathBuf::from(std::env::var("TI_RULES_STORE").expect("TI_RULES_STORE required"));
    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        let width = ti_sql::store_width(&root, None).unwrap();
        let rule = AlertRule { hold: Some("30s".into()), ..battery() };
        let documents = ti_sql::rules::backfill(&root, width, vec![rule.clone()],
            &lume::ti_rules::index_factory, true).await.unwrap();
        let mut snapshot = DocStore::in_memory();
        snapshot.upsert_all(documents.clone()).unwrap();
        let (session, _) = ti_sql::rules::open_session(&root, width,
            &lume::ti_rules::index_factory, snapshot).await.unwrap();
        let sql = format!("SELECT vessel, CAST(extract(epoch FROM start) AS BIGINT) AS ts_start, CAST(extract(epoch FROM \"end\") AS BIGINT) AS ts_end FROM intervals('{}', '30s', '0s') ORDER BY vessel, start",
            rule.when.replace('\'', "''"));
        let intervals = ti_sql::rows_json(&session.query(&sql).await.unwrap()).unwrap();
        let ranges: Vec<_> = documents.iter().map(|d| serde_json::json!({
            "vessel": d.vessel, "ts_start": d.ts_start, "ts_end": d.ts_end,
        })).collect();
        assert_eq!(serde_json::json!(intervals), serde_json::json!(ranges));
        let hits = ti_sql::rows_json(&session.query(
            "SELECT count(*) AS n FROM telemetry WHERE match(alerts, 'battery')").await.unwrap()).unwrap();
        let expected_buckets: u64 = documents.iter().map(|d| ((d.ts_end.unwrap() - d.ts_start) as u64) / width).sum();
        assert_eq!(hits[0]["n"].as_u64().unwrap(), expected_buckets);
        let scale = match session.catalog.field(PATH).unwrap().kind {
            FieldKind::Bsi { scale } => scale, _ => panic!("voltage must be BSI"),
        };
        println!("TI_RULE_ORACLE_JSON {}", serde_json::json!({
            "width":width,"scale":scale,"alerts":ranges,"intervals":intervals,
            "matched_buckets":hits[0]["n"],"expected_match_buckets":expected_buckets,
        }));
    });
}
