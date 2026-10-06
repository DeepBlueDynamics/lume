use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use arrow_schema::{Field, Schema};
use parquet::arrow::ArrowWriter;
use std::{path::PathBuf, sync::Arc};
use ti_contracts::{EntityMapping, TimeUnit, EPOCH};
use ti_ingest::mapped_docs::{self, DocumentsMapping};
#[test]
fn mapped_document_times_ids_null_accounting_and_idempotent_persistence() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mapped-docs-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let columns: Vec<(&str, ArrayRef)> = vec![
        (
            "entity",
            Arc::new(StringArray::from(vec![
                Some("robots.urn:one"),
                None,
                Some("robots.urn:one"),
            ])),
        ),
        (
            "start",
            Arc::new(Int64Array::from(vec![
                Some(EPOCH * 1000),
                Some(EPOCH * 1000),
                None,
            ])),
        ),
        (
            "end",
            Arc::new(Int64Array::from(vec![Some((EPOCH + 10) * 1000); 3])),
        ),
        ("id", Arc::new(StringArray::from(vec!["incident"; 3]))),
        ("title", Arc::new(StringArray::from(vec!["Battery"; 3]))),
        (
            "body",
            Arc::new(StringArray::from(vec!["Inspect battery"; 3])),
        ),
    ];
    let schema = Arc::new(Schema::new(
        columns
            .iter()
            .map(|(n, a)| Field::new(*n, a.data_type().clone(), true))
            .collect::<Vec<_>>(),
    ));
    let batch = RecordBatch::try_new(
        schema.clone(),
        columns.into_iter().map(|(_, a)| a).collect(),
    )
    .unwrap();
    let path = root.join("incidents.parquet");
    let mut writer =
        ArrowWriter::try_new(std::fs::File::create(&path).unwrap(), schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
    let mapping = DocumentsMapping {
        files: path.to_string_lossy().into_owned(),
        entity: EntityMapping::Column("entity".into()),
        time: "start".into(),
        time_end: Some("end".into()),
        time_unit: TimeUnit::Milliseconds,
        timezone: None,
        id: Some("id".into()),
        kind: None,
        kind_constant: "notes".into(),
        title: "title".into(),
        body: "body".into(),
    };
    let mut store = ti_store::DocStore::open(&root).unwrap();
    let first = mapped_docs::read(&mapping, |docs| store.upsert_all(docs)).unwrap();
    assert_eq!(
        (
            first.rows_read,
            first.points_read,
            first.null_entity,
            first.null_time
        ),
        (3, 1, 1, 1)
    );
    let version = store.version();
    mapped_docs::read(&mapping, |docs| store.upsert_all(docs)).unwrap();
    assert_eq!(store.len(), 1);
    assert_eq!(store.version(), version);
    let docs = store.iter().collect::<Vec<_>>();
    assert_eq!(docs[0].vessel, "robots.urn:one");
    assert_eq!(docs[0].ts_end, Some(EPOCH + 10));
    std::fs::remove_dir_all(root).unwrap();
}
