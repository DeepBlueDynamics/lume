#![cfg(feature="ti")]
use serde_json::json;
use std::{path::PathBuf,sync::Arc};
use ti_contracts::{Agg,BucketRecord,Catalog,DocumentIndex,FieldKind,FieldSpec,FieldValue,ShardKey,ShardSink,TextIndex,VesselSpec};
use ti_ingest::resources::{ResourceClient,ResourceDocuments};
#[path="../crates/ti-ingest/tests/support/resources.rs"]
mod mock;
#[test]
fn resource_notes_updates_and_deletes_invalidate_match_notes() {
    let server=mock::MockSignalK::new();
    let root=PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("resources-match-{}",std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{
        let urn="vessels.urn:mrn:signalk:uuid:notes";
        let mut store=ti_store::Store::open_or_create(&root,10).unwrap();
        let vessel=store.catalog().register_vessel(&VesselSpec{urn:urn.into(),name:None,mmsi:None}).unwrap();
        let field=store.catalog().register_field(&FieldSpec{id:0,path:"navigation.speedOverGround".into(),
            agg:Some(Agg::Mean),kind:FieldKind::Bsi{scale:3},units:Some("m/s".into())}).unwrap();
        let start=1_791_288_000; // 2026-10-06T12:00:00Z
        let bucket=ti_contracts::bucket_of(start,10).unwrap();
        store.apply(&[BucketRecord{vessel,bucket,field,value:FieldValue::Int(1000),rewrite:false},
            BucketRecord{vessel,bucket:bucket+1,field,value:FieldValue::Int(2000),rewrite:false}]).unwrap();
        store.seal(ShardKey{vessel,shard:bucket>>16}).unwrap();
        store.shutdown().unwrap();
        let index=Arc::new(lume::ti_text::LumeText::open(&root,Arc::clone(store.catalog()) as Arc<dyn Catalog>,10).unwrap());
        let factory_index=Arc::clone(&index);
        let factory=move |_:&std::path::Path,_:&ti_store::Store,_:u64|->ti_contracts::Result<Arc<dyn DocumentIndex>>{Ok(factory_index.clone())};
        let runtime=ti_sql::surface_runtime().unwrap();
        let engine=runtime.block_on(ti_sql::TiEngine::open(&root,Some(10),Some(&factory))).unwrap();
        let live_server=lume::ti_http::TiServer::open_with_width(&root,Some(10)).unwrap();
        live_server.reload_engine().unwrap();
        let matches=|word:&str| {
            let sql=format!("SELECT ts FROM telemetry WHERE match(notes, '{word}')");
            let direct=runtime.block_on(engine.query(&sql,500)).unwrap()["row_count"].as_u64().unwrap();
            // Exercises the shared RwLock engine, including debounced reloads.
            live_server.reload_engine().unwrap();
            let reply:serde_json::Value=serde_json::from_str(&live_server.mcp("ti_query",&json!({"sql":sql})).unwrap()).unwrap();
            assert_eq!(reply["row_count"].as_u64().unwrap(),direct);
            direct
        };
        assert_eq!(matches("reef"),0);
        let client=ResourceClient::new(&server.url,None).unwrap();
        let mut docs=ResourceDocuments::open(&root).unwrap();
        *server.notes.lock().unwrap()=(200,json!({"id":{"title":"Reef","timestamp":"2026-10-06T12:00:00Z","description":"reef mainsail"}}));
        docs.apply(client.notes().unwrap().unwrap(),urn,1780000000).unwrap();
        assert_eq!(index.match_buckets(vessel,"notes","reef",0,u32::MAX).unwrap().len(),1);
        assert_eq!(matches("reef"),1);
        *server.notes.lock().unwrap()=(200,json!({"id":{"timestamp":"2026-10-06T12:00:00Z","text":"anchorage calm","timeRange":{"start":"2026-10-06T12:00:00Z","end":"2026-10-06T12:00:20Z"}}}));
        docs.apply(client.notes().unwrap().unwrap(),urn,1780000000).unwrap();
        assert!(index.match_buckets(vessel,"notes","reef",0,u32::MAX).unwrap().is_empty());
        assert_eq!(index.match_buckets(vessel,"notes","anchorage",0,u32::MAX).unwrap().len(),2);
        assert_eq!(matches("reef"),0);
        assert_eq!(matches("anchorage"),2);
        *server.notes.lock().unwrap()=(200,json!({}));
        docs.apply(client.notes().unwrap().unwrap(),urn,1780000000).unwrap();
        assert!(index.match_buckets(vessel,"notes","anchorage",0,u32::MAX).unwrap().is_empty());
        assert_eq!(matches("anchorage"),0);
        std::fs::write(root.join("ingest_status.json"),r#"{"documents_rejected_pre_epoch":3}"#).unwrap();
        assert_eq!(runtime.block_on(engine.status()).unwrap()["documents_rejected_pre_epoch"],3);
        *server.logbook.lock().unwrap()=(200,json!({"entry":{"datetime":"2026-10-06T12:00:00Z","text":"reef sail"}}));
        docs.apply(client.logbook().unwrap().unwrap(),urn,1780000000).unwrap();
        assert_eq!(index.match_buckets(vessel,"logbook","reef",0,u32::MAX).unwrap().len(),1);
        assert!(server.requests.lock().unwrap().iter().all(|r|r.starts_with("GET ")));
        drop(store);
    }));
    std::fs::remove_dir_all(root).unwrap();
    if let Err(error)=result{std::panic::resume_unwind(error);}
}
