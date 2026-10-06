//! Portable read-only fixture snapshot for pre-store SQL verification.
use crate::{core_error, FixtureSource, SqlCatalog, SqlSession, VesselInfo};
use datafusion::common::{DataFusionError, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, sync::Arc};
use ti_contracts::{BucketRecord, FieldSpec, ShardKey};
use ti_core::{MemoryShard, MemorySource};
#[derive(Debug, Serialize, Deserialize)]
pub struct FixtureSnapshot {
    pub width_seconds: u64,
    pub fields: Vec<FieldSpec>,
    pub vessels: Vec<VesselInfo>,
    pub dictionaries: BTreeMap<u32, BTreeMap<u32, String>>,
    pub records: Vec<BucketRecord>,
}
pub async fn open_fixture(path: &Path) -> Result<SqlSession> {
    let snapshot: FixtureSnapshot = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|e| DataFusionError::External(Box::new(e)))?;
    let catalog = SqlCatalog::new(
        snapshot.width_seconds,
        snapshot.fields,
        snapshot.vessels,
        snapshot.dictionaries,
    )?;
    let mut groups: BTreeMap<ShardKey, Vec<BucketRecord>> = BTreeMap::new();
    for record in snapshot.records {
        groups
            .entry(ti_contracts::shard_key(record.vessel, record.bucket))
            .or_default()
            .push(record);
    }
    let mut memory = MemorySource::new();
    for (key, records) in groups {
        let mut shard = MemoryShard::new(key).map_err(core_error)?;
        for spec in &catalog.fields {
            shard.register_field(spec.clone()).map_err(core_error)?;
        }
        for (field, rows) in &catalog.dictionaries {
            for (row, value) in rows {
                shard
                    .register_set_value(*field, *row, value)
                    .map_err(core_error)?;
            }
        }
        shard.apply(&records).map_err(core_error)?;
        memory.insert(shard);
    }
    SqlSession::new(
        Arc::new(FixtureSource {
            memory,
            catalog: catalog.clone(),
        }),
        catalog,
    )
    .await
}
pub fn run_cli(args: &[String]) -> Result<()> {
    let arg = |flag: &str| {
        args.iter()
            .position(|v| v == flag)
            .and_then(|i| args.get(i + 1))
    };
    if args.first().map(String::as_str) != Some("verify") {
        return Err(DataFusionError::Plan("usage: lume ti verify (--fixture snapshot.json | --store root) --corpus tests/golden [--raw root] [--oracle duckdb --oracle-setup setup.sql]".into()));
    }
    let fixture = arg("--fixture");
    let store = arg("--store");
    if fixture.is_some() == store.is_some() {
        return Err(DataFusionError::Plan(
            "verify requires exactly one of --fixture or --store".into(),
        ));
    }
    let corpus = arg("--corpus")
        .map(String::as_str)
        .unwrap_or("tests/golden");
    let oracle = arg("--oracle");
    if oracle.is_some_and(|v| v != "duckdb") {
        return Err(DataFusionError::Plan(
            "only --oracle duckdb is supported".into(),
        ));
    }
    let setup = arg("--oracle-setup")
        .map(std::fs::read_to_string)
        .transpose()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let report = runtime.block_on(async {
        let session = if let Some(fixture) = fixture {
            open_fixture(Path::new(fixture)).await?
        } else {
            let metadata: crate::Corpus =
                serde_json::from_slice(&std::fs::read(Path::new(corpus).join("corpus.json"))?)
                    .map_err(|e| DataFusionError::External(Box::new(e)))?;
            crate::open_store(
                Path::new(store.expect("validated store option")),
                metadata.bucket_width_seconds,
            )
            .await?
        };
        if let Some(raw) = arg("--raw") {
            session.register_raw(Path::new(raw)).await?;
        }
        crate::verify(
            &session,
            Path::new(corpus),
            oracle.map(|_| Path::new("duckdb")),
            setup.as_deref(),
        )
        .await
    })?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|e| DataFusionError::External(Box::new(e)))?
    );
    if report.failed > 0 {
        return Err(DataFusionError::Execution(format!(
            "{} golden entries failed",
            report.failed
        )));
    }
    if report.passed == 0 {
        return Err(DataFusionError::Execution(
            "no M3 golden entries were checked".into(),
        ));
    }
    Ok(())
}
