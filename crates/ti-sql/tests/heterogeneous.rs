//! Fleet-wide aggregates over entities with different paths. On the Pi, Lume's own
//! `lume.urn:` entity has only `lume.*` fields, so `max(speedOverGround)` over the
//! whole table failed with "not found: field 74" from the shard without that field.

use std::{collections::BTreeMap, sync::Arc};

use ti_contracts::{Agg, BucketRecord, FieldKind, FieldSpec, FieldValue, ShardKey, EPOCH};
use ti_core::{MemoryShard, MemorySource};
use ti_sql::{FixtureSource, SqlCatalog, SqlSession, VesselInfo};

fn spec(id: u32, path: &str) -> FieldSpec {
    FieldSpec {
        id,
        path: path.into(),
        agg: Some(Agg::Mean),
        kind: FieldKind::Bsi { scale: 0 },
        units: None,
    }
}

fn shard(vessel: u32, field: &FieldSpec, values: &[i64]) -> MemoryShard {
    let mut shard = MemoryShard::new(ShardKey { vessel, shard: 0 }).unwrap();
    shard.register_field(field.clone()).unwrap();
    let records: Vec<_> = values
        .iter()
        .enumerate()
        .map(|(bucket, value)| BucketRecord {
            vessel,
            bucket: bucket as u32,
            field: field.id,
            value: FieldValue::Int(*value),
            rewrite: false,
        })
        .collect();
    shard.apply(&records).unwrap();
    shard
}

fn vessel(ord: u32, urn: &str) -> VesselInfo {
    VesselInfo {
        ord,
        urn: urn.into(),
        name: None,
        mmsi: None,
        first_seen: EPOCH,
        last_seen: EPOCH + 30,
    }
}

#[tokio::test]
async fn aggregates_skip_entities_without_the_path() {
    let sog = spec(1, "navigation.speedOverGround");
    let rss = spec(2, "lume.process.rssBytes");
    let catalog = SqlCatalog::new(
        1,
        vec![sog.clone(), rss.clone()],
        vec![
            vessel(0, "vessels.urn:mrn:signalk:uuid:boat"),
            vessel(1, "lume.urn:host:halos"),
        ],
        BTreeMap::new(),
    )
    .unwrap();
    let mut memory = MemorySource::new();
    memory.insert(shard(0, &sog, &[3, 7, 5]));
    memory.insert(shard(1, &rss, &[100, 120]));
    let session = SqlSession::new(
        Arc::new(FixtureSource {
            memory,
            catalog: catalog.clone(),
        }),
        catalog,
    )
    .await
    .unwrap();

    let batches = session
        .query(
            "SELECT max(\"navigation.speedOverGround\") AS max_sog, \
             min(\"navigation.speedOverGround\") AS min_sog, \
             sum(\"navigation.speedOverGround\") AS sum_sog, \
             count(\"navigation.speedOverGround\") AS n_sog, \
             max(\"lume.process.rssBytes\") AS max_rss, \
             count(*) AS buckets FROM telemetry",
        )
        .await
        .unwrap();
    let text = datafusion::arrow::util::pretty::pretty_format_batches(&batches)
        .unwrap()
        .to_string();
    let row = text.lines().nth(3).unwrap();
    let cells: Vec<_> = row
        .split('|')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();
    assert_eq!(cells, ["7.0", "3.0", "15.0", "3", "120.0", "5"], "{text}");
}
