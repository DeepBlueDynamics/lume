use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{Field, Schema};
use parquet::arrow::ArrowWriter;
use std::{path::PathBuf, sync::Arc};
use ti_contracts::{EntityMapping, ParquetFormat, ParquetMapping, TimeUnit, EPOCH};
use ti_ingest::mapped_parquet::{read_file, MappingReport};
#[test]
fn wide_rows_cannot_expand_into_an_unbounded_point_vector() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("wide-bound-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("wide.parquet");
    let mut columns: Vec<(String, ArrayRef)> = vec![
        (
            "id".into(),
            Arc::new(StringArray::from(vec!["robots.urn:wide"; 200])),
        ),
        ("time".into(), Arc::new(Int64Array::from(vec![EPOCH; 200]))),
    ];
    for i in 0..100 {
        columns.push((
            format!("metric_{i}"),
            Arc::new(Float64Array::from(vec![1.0; 200])),
        ));
    }
    let schema = Arc::new(Schema::new(
        columns
            .iter()
            .map(|(n, a)| Field::new(n, a.data_type().clone(), false))
            .collect::<Vec<_>>(),
    ));
    let batch = RecordBatch::try_new(
        schema.clone(),
        columns.into_iter().map(|(_, a)| a).collect(),
    )
    .unwrap();
    let mut writer =
        ArrowWriter::try_new(std::fs::File::create(&path).unwrap(), schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
    let mapping = ParquetMapping {
        files: path.to_string_lossy().into_owned(),
        entity: EntityMapping::Column("id".into()),
        time: "time".into(),
        time_unit: TimeUnit::Seconds,
        timezone: None,
        format: ParquetFormat::Wide,
        metric: None,
        value: None,
        source: None,
        prefix: String::new(),
        exclude: vec![],
    };
    let mut count = 0;
    let mut calls = 0;
    let mut report = MappingReport::default();
    read_file(&path, &mapping, &mut report, |points| {
        assert!(points.len() <= 8192);
        count += points.len();
        calls += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 20_000);
    assert!(calls >= 3);
    assert_eq!(report.rows_read, 200);
    std::fs::remove_dir_all(root).unwrap();
}
