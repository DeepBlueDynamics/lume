use std::{sync::Arc, path::PathBuf};
use ti_contracts::{Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, ShardKey, ShardSink, VesselSpec};
use ti_store::Store;

struct Scratch(PathBuf);
impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
#[tokio::test]
async fn mixed_entity_store_alias_pushdown_and_frozen_physical_schema() {
    let scratch = Scratch(PathBuf::from(std::env::var("CARGO_TARGET_TMPDIR").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/test-output").into())).join(format!("entities-{}", std::process::id())));
    std::fs::create_dir_all(&scratch.0).unwrap();
    let mut store = Store::open_or_create(&scratch.0, 10).unwrap();
    let field = store.catalog().register_field(&FieldSpec { id: 0, path: "current".into(), agg: Some(Agg::Mean), kind: FieldKind::Bsi { scale: 2 }, units: Some("A".into()) }).unwrap();
    let mut hashes = Vec::new();
    for urn in ["vessels.urn:mrn:signalk:uuid:boat", "robots.urn:fleet:one"] {
        let ord = store.catalog().register_vessel(&VesselSpec { urn: urn.into(), name: None, mmsi: None }).unwrap();
        assert_eq!(store.catalog().vessel_urn(ord).unwrap(), urn);
        store.apply(&[BucketRecord { vessel: ord, bucket: 1, field, value: FieldValue::Int(125), rewrite: false }]).unwrap();
        let key = ShardKey { vessel: ord, shard: 0 };
        let first = store.seal(key).unwrap();
        let second = store.seal(key).unwrap();
        assert_eq!(first.hash, second.hash);
        hashes.push(first.hash);
    }
    assert_ne!(hashes[0], hashes[1]);
    let session = ti_sql::session_from_store(Arc::new(store), 10).await.unwrap();
    let grouped = session.query("SELECT vessel, count(*) AS n FROM telemetry GROUP BY vessel ORDER BY vessel").await.unwrap();
    let alias = session.query("SELECT entity AS vessel, count(*) AS n FROM telemetry GROUP BY entity ORDER BY entity").await.unwrap();
    assert_eq!(ti_sql::rows_json(&grouped).unwrap(), ti_sql::rows_json(&alias).unwrap());
    assert_eq!(ti_sql::rows_json(&grouped).unwrap().len(), 2);
    let rows = session.query("SELECT vessel, entity FROM telemetry WHERE entity IN ('robots.urn:fleet:one') AND entity IS NOT NULL").await.unwrap();
    let rows = ti_sql::rows_json(&rows).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["vessel"], rows[0]["entity"]);
    assert!(session.explain("SELECT entity FROM telemetry WHERE entity='robots.urn:fleet:one'").await.unwrap().contains("Exact"));
    assert!(session.query("SELECT entity FROM docs").await.is_ok());
    assert!(ti_contracts::telemetry_schema(&[]).unwrap().index_of("entity").is_err());
    assert!(ti_contracts::docs_schema().index_of("entity").is_err());
}
