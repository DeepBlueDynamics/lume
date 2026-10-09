#![cfg(feature = "ti")]

use std::path::PathBuf;
use ti_contracts::{
    BucketRecord, Catalog, Document, FieldKind, FieldSpec, FieldValue, ShardSink, TiConfig,
    VesselSpec, EPOCH,
};
use ti_store::{DocStore, Store};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn match_notes_never_crosses_vessel_even_at_the_same_bucket() {
    let fixture = Fixture(
        PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("text-vessels-{}", std::process::id())),
    );
    std::fs::create_dir_all(&fixture.0).unwrap();
    let owner = "vessels.urn:mrn:imo:mmsi:367000000";
    let other = "vessels.urn:mrn:imo:mmsi:367000001";
    let mut store = Store::open_or_create(&fixture.0, 10).unwrap();
    let field = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "environment.depth.belowTransducer".into(),
            agg: None,
            kind: FieldKind::Bsi { scale: 2 },
            units: Some("m".into()),
        })
        .unwrap();
    for urn in [owner, other] {
        let vessel = store
            .catalog()
            .register_vessel(&VesselSpec {
                urn: urn.into(),
                name: None,
                mmsi: None,
            })
            .unwrap();
        store
            .apply(&[BucketRecord {
                vessel,
                bucket: 1,
                field,
                value: FieldValue::Int(100),
                rewrite: false,
            }])
            .unwrap();
    }
    store.shutdown().unwrap();
    let mut documents = DocStore::open(&fixture.0).unwrap();
    documents
        .upsert_all([Document {
            id: "only-owner".into(),
            vessel: owner.into(),
            kind: "notes".into(),
            ts_start: EPOCH + 10,
            ts_end: Some(EPOCH + 20),
            title: "Leak".into(),
            body: "water in bilge".into(),
        }])
        .unwrap();
    ti_sql::surface_runtime().unwrap().block_on(async {
        let (session, _) = ti_sql::rules::open_session(
            &fixture.0,
            TiConfig::default().width_seconds,
            &lume::ti_rules::index_factory,
            documents,
        )
        .await
        .unwrap();
        // Warm the text cache from the owner, then query the identical timestamp
        // on the other vessel, and repeat to exercise cached empty results.
        for _ in 0..2 {
            let all = session
                .query("SELECT vessel FROM telemetry WHERE match(notes, 'leak OR water')")
                .await
                .unwrap();
            let rows = ti_sql::rows_json(&all).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["vessel"], owner);
            let sql = format!(
                "SELECT vessel FROM telemetry WHERE vessel = '{other}' AND match(notes, 'leak OR water')"
            );
            assert!(ti_sql::rows_json(&session.query(&sql).await.unwrap())
                .unwrap()
                .is_empty());
        }
    });
}
