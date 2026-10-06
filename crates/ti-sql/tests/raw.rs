use datafusion::{
    arrow::{
        array::{ArrayRef, Float64Array, StringArray},
        datatypes::{DataType, Field, Schema},
        record_batch::RecordBatch,
    },
    prelude::SessionContext,
};
use parquet::arrow::ArrowWriter;
use std::sync::Arc;
fn file(root: &std::path::Path, name: &str, path: &str, values: Vec<(&str, ArrayRef)>) {
    let mut fields = vec![];
    let mut arrays = vec![];
    for (name, value) in [
        ("context", "vessels.urn:test:1"),
        ("path", path),
        ("signalk_timestamp", "invalid"),
        ("received_timestamp", "2020-01-01T00:00:01Z"),
        ("source_label", "gps"),
    ] {
        fields.push(Field::new(name, DataType::Utf8, true));
        arrays.push(Arc::new(StringArray::from(vec![value])) as ArrayRef);
    }
    for (name, array) in values {
        fields.push(Field::new(name, array.data_type().clone(), true));
        arrays.push(array);
    }
    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();
    let mut writer = ArrowWriter::try_new(
        std::fs::File::create(root.join(name)).unwrap(),
        schema,
        None,
    )
    .unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}
#[tokio::test]
async fn real_layout_mixed_types_object_keys_and_received_fallback() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-output")
        .join(format!("raw-{}", std::process::id()));
    std::fs::create_dir_all(root.join("tier=raw")).unwrap();
    let raw = root.join("tier=raw");
    file(
        &raw,
        "number.parquet",
        "speed",
        vec![("value", Arc::new(Float64Array::from(vec![1.25])))],
    );
    file(
        &raw,
        "string.parquet",
        "state",
        vec![("value", Arc::new(StringArray::from(vec!["on"])))],
    );
    file(
        &raw,
        "position.parquet",
        "navigation.position",
        vec![
            ("value_latitude", Arc::new(Float64Array::from(vec![47.5]))),
            (
                "value_longitude",
                Arc::new(Float64Array::from(vec![-122.5])),
            ),
        ],
    );
    // A raw view must never accidentally include quarantine files.
    std::fs::create_dir_all(root.join("quarantine")).unwrap();
    file(
        &root.join("quarantine"),
        "bad.parquet",
        "speed",
        vec![("value", Arc::new(Float64Array::from(vec![99.0])))],
    );
    let context = SessionContext::new();
    ti_sql::register_raw(&context, &root).await.unwrap();
    let schema = context.table_provider("raw").await.unwrap().schema();
    let actual = schema
        .fields()
        .iter()
        .map(|f| {
            let ty = match f.data_type() {
                DataType::Utf8 | DataType::Utf8View | DataType::LargeUtf8 => "VARCHAR",
                DataType::Timestamp(datafusion::arrow::datatypes::TimeUnit::Microsecond, None) => {
                    "TIMESTAMP"
                }
                DataType::Float64 => "DOUBLE",
                other => panic!("unexpected raw type {other:?}"),
            };
            (f.name().as_str(), ty)
        })
        .collect::<Vec<_>>();
    // Column-for-column DuckDB read_raw RETURNS TABLE signature.
    assert_eq!(
        actual,
        vec![
            ("context", "VARCHAR"),
            ("ts", "TIMESTAMP"),
            ("path", "VARCHAR"),
            ("value", "DOUBLE"),
            ("value_str", "VARCHAR"),
            ("source", "VARCHAR")
        ]
    );
    let batches = context
        .sql("SELECT path,value,value_str,source,CAST(ts AS VARCHAR) AS ts FROM raw ORDER BY path")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 4);
    let mut found = vec![];
    for row in ti_sql::rows_json(&batches).unwrap() {
        let path = row["path"].as_str().unwrap();
        found.push(path.to_owned());
        assert!(!row["ts"].is_null());
        if path == "state" {
            assert!(row["value"].is_null());
            assert!(!row["value_str"].is_null());
        } else {
            assert!(!row["value"].is_null());
            assert!(row["value_str"].is_null());
        }
    }
    assert_eq!(
        found,
        [
            "navigation.position.latitude",
            "navigation.position.longitude",
            "speed",
            "state"
        ]
    );
    std::fs::remove_dir_all(root).unwrap();
}
