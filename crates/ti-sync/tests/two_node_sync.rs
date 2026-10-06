use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ti_contracts::{
    Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, RoaringBitmap, ShardKey,
    ShardSink, ShardSource, VesselSpec,
};
use ti_store::Store;
use ti_sync::{
    LoopbackTransport, LossyTransport, ShoreReceiver, SyncClient, SyncReport, DEFAULT_CHUNK_SIZE,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new(prefix: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup_populated_local_store(root: &std::path::Path) -> Store {
    let mut store = Store::open_or_create(root, 10).unwrap();

    let urn = "vessels.urn:mrn:imo:mmsi:230999999";
    let vessel_ord = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: urn.into(),
            name: Some("PV-1".into()),
            mmsi: Some("230999999".into()),
        })
        .unwrap();

    let field_speed = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "navigation.speedOverGround".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 2 },
            units: Some("m/s".into()),
        })
        .unwrap();

    let field_state = store
        .catalog()
        .register_field(&FieldSpec {
            id: 1,
            path: "propulsion.port.state".into(),
            agg: None,
            kind: FieldKind::Set,
            units: None,
        })
        .unwrap();

    let row_motoring = store
        .catalog()
        .register_set_value(field_state, "motoring")
        .unwrap();

    // Data in Shard 0 (buckets 10..20)
    let records_shard0 = vec![
        BucketRecord {
            vessel: vessel_ord,
            bucket: 10,
            field: field_speed,
            value: FieldValue::Int(550), // 5.50 m/s
            rewrite: false,
        },
        BucketRecord {
            vessel: vessel_ord,
            bucket: 10,
            field: field_state,
            value: FieldValue::SetValue(row_motoring),
            rewrite: false,
        },
        BucketRecord {
            vessel: vessel_ord,
            bucket: 15,
            field: field_speed,
            value: FieldValue::Int(620), // 6.20 m/s
            rewrite: false,
        },
    ];

    // Data in Shard 1 (buckets 65536 + 10..20)
    let base_shard1 = 65536;
    let records_shard1 = vec![
        BucketRecord {
            vessel: vessel_ord,
            bucket: base_shard1 + 10,
            field: field_speed,
            value: FieldValue::Int(710), // 7.10 m/s
            rewrite: false,
        },
        BucketRecord {
            vessel: vessel_ord,
            bucket: base_shard1 + 10,
            field: field_state,
            value: FieldValue::SetValue(row_motoring),
            rewrite: false,
        },
    ];

    store.apply(&records_shard0).unwrap();
    store
        .seal(ShardKey {
            vessel: vessel_ord,
            shard: 0,
        })
        .unwrap();

    store.apply(&records_shard1).unwrap();
    store
        .seal(ShardKey {
            vessel: vessel_ord,
            shard: 1,
        })
        .unwrap();

    store
}

#[test]
fn test_two_node_sync_lossy_link_and_outage() {
    let local_scratch = Scratch::new("sync-boat");
    let shore_scratch = Scratch::new("sync-shore");

    let local_store = setup_populated_local_store(&local_scratch.0);
    let local_entries = local_store.manifest().entries();
    assert_eq!(local_entries.len(), 2, "local store has 2 sealed shards");

    let shore_store = Store::open_or_create(&shore_scratch.0, 10).unwrap();
    assert!(
        shore_store.manifest().entries().is_empty(),
        "shore starts empty"
    );

    let shore_arc = Arc::new(Mutex::new(shore_store));
    let receiver = Arc::new(ShoreReceiver::new(shore_arc.clone()));
    let loopback = LoopbackTransport::new(receiver.clone());

    // Lossy transport with 20% chunk drop rate
    let lossy = LossyTransport::new(loopback, 0.20, 42);

    // Sync client using small 512-byte chunks so each shard is split into multiple chunks
    let local_arc = Arc::new(Mutex::new(local_store));
    let client = SyncClient::new(local_arc.clone(), lossy)
        .with_chunk_size(512)
        .with_max_retries(200);

    // 1. Initial diff sees 2 missing shards
    let missing = client.diff().unwrap();
    assert_eq!(missing.len(), 2);
    assert_eq!(missing[0].local_key.shard, 0);
    assert_eq!(missing[1].local_key.shard, 1);

    // 2. Sync first shard through the 20% lossy link
    let mut report = SyncReport::default();
    let entry0 = client.sync_shard(&missing[0], &mut report).unwrap();
    assert_eq!(entry0.key.shard, 0);
    assert_eq!(entry0.hash, local_entries[0].hash);

    // Verify chunks were attempted, some dropped, and retried successfully
    assert!(client.transport().chunks_attempted() > 0);
    assert!(
        client.transport().chunks_dropped() > 0,
        "lossy link must have dropped chunks: attempted {}, dropped {}",
        client.transport().chunks_attempted(),
        client.transport().chunks_dropped()
    );

    // Shore now has exactly 1 shard installed in its manifest
    {
        let shore = shore_arc.lock().unwrap();
        assert_eq!(shore.manifest().entries().len(), 1);
        assert_eq!(shore.manifest().entries()[0].hash, local_entries[0].hash);
    }

    // 3. Simulate a 30-minute outage
    println!("[Link] Starlink/cellular disconnected: starting simulated 30-minute outage");
    client.transport().set_offline(true);
    assert!(client.transport().is_offline());

    // While offline, sync attempts fail cleanly with network error
    let offline_err = client.diff();
    assert!(offline_err.is_err(), "sync calls during outage must fail");

    // 4. Restore link after 30-minute outage
    println!("[Link] Starlink connection restored: resuming sync");
    client.transport().set_offline(false);
    assert!(!client.transport().is_offline());

    // 5. Resume sync: diff now sees only shard 1 missing (resumption!), and finishes
    let diff_after = client.diff().unwrap();
    assert_eq!(
        diff_after.len(),
        1,
        "shard 0 is already on shore; only shard 1 missing"
    );
    assert_eq!(diff_after[0].local_key.shard, 1);

    let full_report = client.sync_all().unwrap();
    assert_eq!(full_report.shards_synced, 1);

    // 6. Verify convergence: shore manifest matches local byte-for-byte
    let shore = shore_arc.lock().unwrap();
    let shore_entries = shore.manifest().entries();
    assert_eq!(shore_entries.len(), 2, "shore now has both shards");

    for (local_e, shore_e) in local_entries.iter().zip(shore_entries.iter()) {
        assert_eq!(shore_e.key.shard, local_e.key.shard);
        assert_eq!(shore_e.version, local_e.version);
        assert_eq!(shore_e.from, local_e.from);
        assert_eq!(shore_e.to, local_e.to);
        assert_eq!(shore_e.bytes, local_e.bytes);
        assert_eq!(
            shore_e.hash, local_e.hash,
            "shard hashes must be byte-identical"
        );
    }

    // Compare raw manifest JSON bytes
    let local_manifest_bytes = std::fs::read(local_scratch.0.join("manifest.json")).unwrap();
    let shore_manifest_bytes = std::fs::read(shore_scratch.0.join("manifest.json")).unwrap();
    assert_eq!(
        local_manifest_bytes, shore_manifest_bytes,
        "manifest.json on shore must be byte-identical to local"
    );

    // Verify shore store can query data from imported shards
    let all_cols = RoaringBitmap::from_sorted_iter(0..1000).unwrap();
    let batch = shore
        .read(
            ShardKey {
                vessel: 0,
                shard: 0,
            },
            &all_cols,
            &[0, 1],
        )
        .unwrap();
    assert!(batch.num_rows() > 0, "shore store can read imported data");
}

#[test]
fn test_vessel_ordinal_remapping_by_urn() {
    let local_scratch = Scratch::new("sync-remap-local");
    let shore_scratch = Scratch::new("sync-remap-shore");

    // Local boat has vessel "vessels.urn:mrn:imo:mmsi:230999999" as vessel 0
    let local_store = setup_populated_local_store(&local_scratch.0);
    let local_entries = local_store.manifest().entries();

    // Shore already has a PRE-EXISTING vessel at ordinal 0!
    let shore_store = Store::open_or_create(&shore_scratch.0, 10).unwrap();
    let pre_existing_ord = shore_store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:mrn:imo:mmsi:111111111".into(),
            name: Some("Other Boat".into()),
            mmsi: Some("111111111".into()),
        })
        .unwrap();
    assert_eq!(
        pre_existing_ord, 0,
        "pre-existing vessel takes ordinal 0 on shore"
    );

    let shore_arc = Arc::new(Mutex::new(shore_store));
    let receiver = Arc::new(ShoreReceiver::new(shore_arc.clone()));
    let loopback = LoopbackTransport::new(receiver.clone());
    let client = SyncClient::new(Arc::new(Mutex::new(local_store)), loopback);

    // Sync all shards to shore
    let report = client.sync_all().unwrap();
    assert_eq!(report.shards_synced, 2);

    let shore = shore_arc.lock().unwrap();
    let shore_entries = shore.manifest().entries();
    assert_eq!(shore_entries.len(), 2);

    // Verify imported shards on shore were re-mapped to ordinal 1!
    for shore_e in &shore_entries {
        assert_eq!(
            shore_e.key.vessel, 1,
            "vessel must be remapped to shore ordinal 1"
        );
    }

    // Hashes, version, and coverage remain identical
    for (local_e, shore_e) in local_entries.iter().zip(shore_entries.iter()) {
        assert_eq!(shore_e.key.shard, local_e.key.shard);
        assert_eq!(shore_e.version, local_e.version);
        assert_eq!(shore_e.from, local_e.from);
        assert_eq!(shore_e.to, local_e.to);
        assert_eq!(shore_e.bytes, local_e.bytes);
        assert_eq!(
            shore_e.hash, local_e.hash,
            "hashes match despite vessel remapping"
        );
    }
}

#[test]
fn test_tampered_chunk_rejection() {
    let local_scratch = Scratch::new("sync-tamper-local");
    let shore_scratch = Scratch::new("sync-tamper-shore");

    let local_store = setup_populated_local_store(&local_scratch.0);
    let shore_store = Store::open_or_create(&shore_scratch.0, 10).unwrap();
    let receiver = ShoreReceiver::new(Arc::new(Mutex::new(shore_store)));

    let missing = ti_sync::MissingShard {
        vessel_urn: "vessels.urn:mrn:imo:mmsi:230999999".into(),
        local_key: ShardKey {
            vessel: 0,
            shard: 0,
        },
        version: 1,
        from: 0,
        to: 10,
        bytes: 100,
        hash: [0u8; 32],
    };

    let entry = local_store.manifest().get(missing.local_key).unwrap();
    let package = ti_sync::package_sealed_shard(
        local_store.root(),
        0,
        0,
        1,
        local_store.catalog().as_ref(),
        local_store.width_seconds(),
        &entry,
    )
    .unwrap();

    let chunks = ti_sync::chunk_package(&package, DEFAULT_CHUNK_SIZE);
    let mut bad_chunk = chunks[0].clone();
    // Tamper with data without updating chunk_hash
    bad_chunk.data[0] ^= 0xff;

    let res = receiver.receive_chunk(&bad_chunk);
    assert!(res.is_err(), "receiver must reject tampered chunk data");
}
