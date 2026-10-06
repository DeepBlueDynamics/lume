//! W10 rule hook ordering and publication boundary.
use std::sync::{Arc, Mutex};
use ti_contracts::{BucketIx, Catalog, Result, ShardSink, ShardSource, TiConfig, VesselOrd};
use ti_ingest::{ClosedBucketObserver, NormalizedValue, WatermarkBucketer};
use ti_store::Store;

struct Observer {
    root: std::path::PathBuf,
    seen: Arc<Mutex<Vec<(VesselOrd, BucketIx, BucketIx)>>>,
}
impl ClosedBucketObserver for Observer {
    fn on_closed(&mut self, vessel: VesselOrd, from: BucketIx, to: BucketIx) -> Result<()> {
        let store = Store::open_or_create(&self.root, 10)?;
        let key = ti_contracts::shard_key(vessel, from);
        assert!(store
            .eval(key, &ti_contracts::Predicate::All)?
            .contains(ti_contracts::local_col(from)));
        self.seen.lock().unwrap().push((vessel, from, to));
        Ok(())
    }
}

#[test]
fn published_closes_are_ordered_and_late_rewrites_do_not_retrigger() {
    let dir = tempfile::Builder::new()
        .prefix("closed-observer-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let config = TiConfig::default();
    let mut store = Store::open_or_create(dir.path(), 10).unwrap();
    let catalog = Arc::clone(store.catalog());
    let urn = "vessels.urn:mrn:signalk:uuid:test";
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut bucketer = WatermarkBucketer::new(&config);
    bucketer.set_closed_bucket_observer(Some(Box::new(Observer {
        root: dir.path().into(),
        seen: Arc::clone(&seen),
    })));
    let t0 = 1_780_000_000;
    for ts in [t0, t0 + 10, t0 + 50] {
        bucketer
            .ingest_point(
                urn,
                "navigation.speedOverGround",
                "gps",
                ts,
                NormalizedValue::Double(2.0),
                &config,
                catalog.as_ref(),
                &mut store,
            )
            .unwrap();
    }
    let b0 = ti_contracts::bucket_of(t0, 10).unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(0, b0, b0), (0, b0 + 1, b0 + 1)]
    );
    bucketer
        .ingest_point(
            urn,
            "navigation.speedOverGround",
            "gps",
            t0,
            NormalizedValue::Double(3.0),
            &config,
            catalog.as_ref(),
            &mut store,
        )
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut store)
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 3);
    bucketer
        .flush_all(&config, catalog.as_ref(), &mut store)
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 3);
    assert_eq!(catalog.vessel_urn(0).unwrap(), urn);
    store.flush().unwrap();
}

#[test]
fn multi_store_registration_rejects_unknown_stores() {
    let mut bucketer = ti_ingest::MultiStoreBucketer::new(&TiConfig::default()).unwrap();
    assert!(bucketer
        .set_closed_bucket_observer("missing", None)
        .is_err());
    bucketer
        .set_closed_bucket_observer("default", None)
        .unwrap();
}
