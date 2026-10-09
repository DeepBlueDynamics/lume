//! Connect the SQL view to W2's durable store and authoritative catalogs.
use crate::{core_error, SqlCatalog, SqlSession, VesselInfo};
use datafusion::arrow::array::{
    Array, BooleanArray, StringArray, TimestampSecondArray, UInt32Array,
};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::common::{DataFusionError, Result};
use datafusion::datasource::MemTable;
use std::{collections::BTreeMap, path::Path, sync::Arc};
use ti_contracts::{Catalog, FieldKind};
use ti_store::Store;

fn column<T: Array + 'static>(batch: &RecordBatch, index: usize) -> Result<&T> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| DataFusionError::Execution("store catalog column type mismatch".into()))
}

/// Open an existing store. Width must match the deployment and verification corpus.
pub async fn open_store(root: &Path, width_seconds: u64) -> Result<SqlSession> {
    if !root.join("catalog").is_dir() {
        return Err(DataFusionError::Plan(
            "existing store catalog directory required".into(),
        ));
    }
    session_from_store(
        Arc::new(Store::open_readonly(root, width_seconds).map_err(core_error)?),
        width_seconds,
    )
    .await
}

/// Open or create an empty store when width is explicitly provided.
pub async fn open_or_create_store(root: &Path, width_seconds: u64) -> Result<SqlSession> {
    let store = if root.join("catalog").is_dir() {
        Store::open_readonly(root, width_seconds).map_err(core_error)?
    } else {
        Store::open_or_create(root, width_seconds).map_err(core_error)?
    };
    session_from_store(Arc::new(store), width_seconds).await
}

/// Builds the W5 document index for an opened store: `(store_root, store, width_seconds)`.
pub type DocumentsFactory =
    dyn Fn(&Path, &Store, u64) -> ti_contracts::Result<Arc<dyn ti_contracts::DocumentIndex>>;

/// `open_store`, plus a document index injected into the shard source (for
/// `match(notes|logbook|alerts, q)`) and registered as the `docs` table.
pub async fn open_store_with_documents(
    root: &Path,
    width_seconds: u64,
    documents: &DocumentsFactory,
) -> Result<SqlSession> {
    if !root.join("catalog").is_dir() {
        return Err(DataFusionError::Plan(
            "existing store catalog directory required".into(),
        ));
    }
    let store = Store::open_readonly(root, width_seconds).map_err(core_error)?;
    let index = documents(root, &store, width_seconds).map_err(core_error)?;
    let text: Arc<dyn ti_contracts::TextIndex> = index.clone();
    let session = session_from_store(Arc::new(store.with_text_index(text)), width_seconds).await?;
    session.register_documents(index)?;
    Ok(session)
}

pub async fn open_or_create_store_with_documents(
    root: &Path,
    width_seconds: u64,
    documents: &DocumentsFactory,
) -> Result<SqlSession> {
    let store = if root.join("catalog").is_dir() {
        Store::open_readonly(root, width_seconds).map_err(core_error)?
    } else {
        Store::open_or_create(root, width_seconds).map_err(core_error)?
    };
    let index = documents(root, &store, width_seconds).map_err(core_error)?;
    let text: Arc<dyn ti_contracts::TextIndex> = index.clone();
    let session = session_from_store(Arc::new(store.with_text_index(text)), width_seconds).await?;
    session.register_documents(index)?;
    Ok(session)
}

/// Build a SqlCatalog from an opened store and width.
pub fn build_sql_catalog(store: &Store, width_seconds: u64) -> Result<Arc<SqlCatalog>> {
    let disk = store.catalog();
    let fields = disk.fields().map_err(core_error)?;
    let vessels_batch = disk.vessels_record_batch().map_err(core_error)?;
    let ords = column::<UInt32Array>(&vessels_batch, 0)?;
    let urns = column::<StringArray>(&vessels_batch, 1)?;
    let names = column::<StringArray>(&vessels_batch, 2)?;
    let mmsis = column::<StringArray>(&vessels_batch, 3)?;
    let firsts = column::<TimestampSecondArray>(&vessels_batch, 4)?;
    let lasts = column::<TimestampSecondArray>(&vessels_batch, 5)?;
    let vessels = (0..vessels_batch.num_rows())
        .map(|i| VesselInfo {
            ord: ords.value(i),
            urn: urns.value(i).into(),
            name: (!names.is_null(i)).then(|| names.value(i).into()),
            mmsi: (!mmsis.is_null(i)).then(|| mmsis.value(i).into()),
            first_seen: firsts.value(i),
            last_seen: lasts.value(i),
        })
        .collect();
    let mut dictionaries = BTreeMap::new();
    for field in &fields {
        if field.kind != FieldKind::Set {
            continue;
        }
        let mut rows = BTreeMap::new();
        let mut row = 0u32;
        loop {
            match disk.set_value(field.id, row) {
                Ok(value) => {
                    rows.insert(row, value);
                }
                Err(ti_contracts::Error::NotFound(_)) => break,
                Err(error) => return Err(core_error(error)),
            }
            row = row
                .checked_add(1)
                .ok_or_else(|| DataFusionError::Execution("set dictionary ID overflow".into()))?;
        }
        dictionaries.insert(field.id, rows);
    }
    SqlCatalog::new(width_seconds, fields, vessels, dictionaries)
}

/// Create an immutable planning snapshot while retaining the Store as the read source.
pub async fn session_from_store(store: Arc<Store>, width_seconds: u64) -> Result<SqlSession> {
    let disk = store.catalog();
    let vessels_batch = disk.vessels_record_batch().map_err(core_error)?;
    let catalog = build_sql_catalog(&store, width_seconds)?;
    let session = SqlSession::new(store.clone(), catalog).await?;
    for (name, batch) in [
        ("vessels", vessels_batch),
        ("paths", disk.paths_record_batch().map_err(core_error)?),
    ] {
        session.register_derived_table(
            name,
            Arc::new(MemTable::try_new(batch.schema(), vec![vec![batch]])?),
        )?;
    }
    // Keep open shards alongside authoritative sealed manifest rows.
    let sealed = store
        .manifest()
        .shards_record_batch(disk.as_ref(), width_seconds)
        .map_err(core_error)?;
    let mut batches = vec![sealed];
    for batch in session.query("SELECT * FROM shards").await? {
        let urns = column::<StringArray>(&batch, 0)?;
        let numbers = column::<UInt32Array>(&batch, 1)?;
        let keep = BooleanArray::from(
            (0..batch.num_rows())
                .map(|i| {
                    let ord = session
                        .catalog
                        .vessels
                        .values()
                        .find(|v| v.urn == urns.value(i))
                        .map(|v| v.ord);
                    ord.is_some_and(|vessel| {
                        store
                            .manifest()
                            .get(ti_contracts::ShardKey {
                                vessel,
                                shard: numbers.value(i),
                            })
                            .is_none()
                    })
                })
                .collect::<Vec<_>>(),
        );
        batches.push(datafusion::arrow::compute::filter_record_batch(
            &batch, &keep,
        )?);
    }
    session.register_derived_table(
        "shards",
        Arc::new(MemTable::try_new(
            ti_contracts::shards_schema(),
            vec![batches],
        )?),
    )?;
    Ok(session)
}

/// Helper to determine SQL table name for a store.
/// "default" or "telemetry" maps to "telemetry". Non-default names like "hr" map to "telemetry_hr".
pub fn table_name_for_store(name: &str) -> String {
    if name == "default" || name == "telemetry" {
        "telemetry".to_string()
    } else if name.starts_with("telemetry_") {
        name.to_string()
    } else {
        format!("telemetry_{name}")
    }
}

/// Create an immutable planning snapshot from a default store and any extra stores.
/// Non-default stores are registered as their own tables (`telemetry_<store_name>`).
pub async fn session_from_stores(
    default_store: Arc<Store>,
    default_width_seconds: u64,
    extra_stores: Vec<(String, Arc<Store>, u64)>,
) -> Result<SqlSession> {
    let session = session_from_store(default_store, default_width_seconds).await?;
    for (name, store, width) in extra_stores {
        let table_name = table_name_for_store(&name);
        let catalog = build_sql_catalog(&store, width)?;
        session.register_store_table(&table_name, store.clone(), catalog)?;
    }
    Ok(session)
}
