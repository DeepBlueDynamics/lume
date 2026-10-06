use arrow_array::{types::Int8Type, *};
use arrow_schema::{Field, Schema};
use parquet::arrow::ArrowWriter;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use ti_contracts::{
    Catalog, EntityMapping, ParquetFormat, ParquetMapping, ShardSink, ShardSource, TiConfig,
    TimeUnit,
};
use ti_ingest::mapped_parquet::{self, MappingReport};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "mapped-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn write(path: &Path, columns: Vec<(&str, ArrayRef)>) {
    let schema = Arc::new(Schema::new(
        columns
            .iter()
            .map(|(name, values)| Field::new(*name, values.data_type().clone(), true))
            .collect::<Vec<_>>(),
    ));
    let batch = RecordBatch::try_new(
        schema.clone(),
        columns.into_iter().map(|(_, a)| a).collect(),
    )
    .unwrap();
    let mut writer =
        ArrowWriter::try_new(std::fs::File::create(path).unwrap(), schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}
fn mapping(files: String, format: ParquetFormat) -> ParquetMapping {
    ParquetMapping {
        files,
        entity: EntityMapping::Column("id".into()),
        time: "time".into(),
        time_unit: TimeUnit::Milliseconds,
        timezone: Some("UTC".into()),
        format,
        metric: (format == ParquetFormat::Long).then(|| "metric".into()),
        value: (format == ParquetFormat::Long).then(|| "value".into()),
        source: None,
        prefix: "robot.".into(),
        exclude: vec!["ignored".into()],
    }
}
const URN: &str = "vessels.urn:robot-fixture";
#[test]
fn long_and_wide_streaming_projection_skip_counts_and_replay() {
    let scratch = Scratch::new();
    let wide = scratch.0.join("wide.parquet");
    let count = 8200; // Forces the same bucket across the 8192-row Arrow batch boundary.
    let dictionary = DictionaryArray::<Int8Type>::from_iter((0..count).map(|_| Some("moving")));
    write(
        &wide,
        vec![
            (
                "id",
                Arc::new(StringArray::from_iter_values((0..count).map(|_| URN))),
            ),
            (
                "time",
                Arc::new(Int64Array::from(vec![
                    ti_contracts::EPOCH * 1000 + 10001;
                    count
                ])),
            ),
            (
                "current",
                Arc::new(Float64Array::from(
                    (0..count).map(|i| i as f64).collect::<Vec<_>>(),
                )),
            ),
            ("enabled", Arc::new(BooleanArray::from(vec![true; count]))),
            ("mode", Arc::new(dictionary)),
            ("ignored", Arc::new(Int64Array::from(vec![1; count]))),
        ],
    );
    let mapping = mapping(wide.to_string_lossy().into_owned(), ParquetFormat::Wide);
    let mut config = TiConfig {
        store_root: scratch.0.join("store").to_string_lossy().into_owned(),
        units: BTreeMap::from([(
            "*.current".into(),
            ti_contracts::MetricUnit {
                unit: "A".into(),
                scale: 2,
            },
        )]),
        ..Default::default()
    };
    config.profiles.opt_in.push("count".into());
    let mut store = ti_store::Store::open_or_create(Path::new(&config.store_root), 10).unwrap();
    let catalog = store.catalog().clone();
    let catalogs = BTreeMap::from([("default".into(), catalog.as_ref() as &dyn Catalog)]);
    let import = |store: &mut ti_store::Store| {
        let mut sinks = BTreeMap::from([("default".into(), store as &mut dyn ShardSink)]);
        mapped_parquet::backfill(
            std::slice::from_ref(&mapping),
            &config,
            &catalogs,
            &mut sinks,
        )
        .unwrap()
    };
    let report = import(&mut store);
    assert_eq!((report.rows_read, report.points_read), (8200, 24600));
    assert!(report.unit_scale_misses.is_empty());
    let fields = catalog.fields().unwrap();
    let count_field = fields
        .iter()
        .find(|f| f.path == "robot.current" && f.agg == Some(ti_contracts::Agg::Count))
        .unwrap()
        .id;
    let mean_field = fields
        .iter()
        .find(|f| f.path == "robot.current" && f.agg == Some(ti_contracts::Agg::Mean))
        .unwrap();
    assert_eq!(mean_field.units.as_deref(), Some("A"));
    assert_eq!(mean_field.kind, ti_contracts::FieldKind::Bsi { scale: 2 });
    assert!(!fields.iter().any(|f| f.path.contains("ignored")));
    let key = store.shards(None, 0, u32::MAX)[0];
    let mask = store.eval(key, &ti_contracts::Predicate::All).unwrap();
    let batch = store
        .read(key, &mask, &[count_field, mean_field.id])
        .unwrap();
    assert_eq!(
        batch
            .column_by_name("robot.current@count")
            .unwrap()
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap()
            .value(0),
        8200
    );
    assert_eq!(
        batch
            .column_by_name("robot.current@mean")
            .unwrap()
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap()
            .value(0),
        4099.5
    );
    let before = store.seal(key).unwrap().hash;
    import(&mut store);
    assert_eq!(store.seal(key).unwrap().hash, before);

    let long = scratch.0.join("long.parquet");
    write(
        &long,
        vec![
            (
                "id",
                Arc::new(StringArray::from(vec![
                    Some(URN),
                    None,
                    Some(URN),
                    Some(URN),
                ])),
            ),
            (
                "time",
                Arc::new(Int64Array::from(vec![
                    Some(ti_contracts::EPOCH * 1000),
                    Some(0),
                    None,
                    Some(ti_contracts::EPOCH * 1000),
                ])),
            ),
            ("metric", Arc::new(StringArray::from(vec!["current"; 4]))),
            (
                "value",
                Arc::new(Float64Array::from(vec![
                    Some(1.25),
                    Some(1.),
                    Some(1.),
                    None,
                ])),
            ),
        ],
    );
    let mut report = MappingReport::default();
    let mut points = Vec::new();
    mapped_parquet::read_file(&long, &mapping_for_long(&long), &mut report, |batch| {
        points.extend(batch);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        (
            report.rows_read,
            report.null_entity,
            report.null_time,
            report.null_value
        ),
        (4, 1, 1, 1)
    );
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].path, "robot.current");
    let mut sinks = BTreeMap::from([("default".into(), &mut store as &mut dyn ShardSink)]);
    mapped_parquet::backfill(&[mapping_for_long(&long)], &config, &catalogs, &mut sinks).unwrap();
    drop(sinks);
    assert_eq!(
        store
            .eval(key, &ti_contracts::Predicate::All)
            .unwrap()
            .len(),
        2
    );
}
fn mapping_for_long(path: &Path) -> ParquetMapping {
    mapping(path.to_string_lossy().into_owned(), ParquetFormat::Long)
}

#[test]
fn timestamp_date_integer_and_naive_text_are_explicit() {
    use mapped_parquet::time_seconds;
    let integer = Int64Array::from(vec![-1]);
    assert_eq!(
        time_seconds(&integer, 0, TimeUnit::Milliseconds, None).unwrap(),
        Some(-1)
    );
    let dictionary = DictionaryArray::<Int8Type>::try_new(
        Int8Array::from(vec![0]),
        Arc::new(StringArray::from(vec![None::<&str>])),
    )
    .unwrap();
    assert_eq!(
        time_seconds(&dictionary, 0, TimeUnit::Rfc3339, None).unwrap(),
        None
    );
    let date = Date32Array::from(vec![1]);
    assert_eq!(
        time_seconds(&date, 0, TimeUnit::Seconds, Some("+02:00")).unwrap(),
        Some(86400 - 7200)
    );
    let time = TimestampMicrosecondArray::from(vec![1_000_001]).with_timezone("UTC");
    assert_eq!(
        time_seconds(&time, 0, TimeUnit::Seconds, None).unwrap(),
        Some(1)
    );
    let naive = StringArray::from(vec!["2026-01-01 00:00:00"]);
    assert!(time_seconds(&naive, 0, TimeUnit::Rfc3339, None).is_err());
    assert_eq!(
        time_seconds(&naive, 0, TimeUnit::Rfc3339, Some("+02:00")).unwrap(),
        Some(1767218400)
    );
    let bad = StringArray::from(vec!["banana"]);
    assert!(time_seconds(&bad, 0, TimeUnit::Rfc3339, Some("UTC")).is_err());
}

#[test]
fn missing_columns_and_invalid_mappings_fail_before_ingest() {
    let scratch = Scratch::new();
    let path = scratch.0.join("bad.parquet");
    write(&path, vec![("id", Arc::new(StringArray::from(vec![URN])))]);
    assert!(mapped_parquet::read_file(
        &path,
        &mapping_for_long(&path),
        &mut MappingReport::default(),
        |_| Ok(())
    )
    .is_err());
    assert!(
        mapped_parquet::expand_files(&scratch.0.join("missing*.parquet").to_string_lossy())
            .is_err()
    );
    let paths =
        mapped_parquet::expand_files(&scratch.0.join("**/*.parquet").to_string_lossy()).unwrap();
    assert_eq!(paths, vec![path]);
}

#[test]
#[ignore = "requires LUME_TI_BIN pointing at a root binary built with --features ti"]
fn long_and_wide_cli_backfill_and_status() {
    let binary = std::env::var("LUME_TI_BIN").expect("LUME_TI_BIN required");
    let scratch = Scratch::new();
    let long = scratch.0.join("long.parquet");
    let wide = scratch.0.join("wide.parquet");
    let times = vec![
        ti_contracts::EPOCH * 1000 + 10000,
        ti_contracts::EPOCH * 1000 + 20000,
    ];
    write(
        &long,
        vec![
            ("id", Arc::new(StringArray::from(vec![URN; 2]))),
            ("time", Arc::new(Int64Array::from(times.clone()))),
            ("metric", Arc::new(StringArray::from(vec!["current"; 2]))),
            ("value", Arc::new(Float64Array::from(vec![1.25, 2.75]))),
        ],
    );
    write(
        &wide,
        vec![
            ("id", Arc::new(StringArray::from(vec![URN; 2]))),
            ("time", Arc::new(Int64Array::from(times))),
            ("current", Arc::new(Float64Array::from(vec![1.25, 2.75]))),
            ("mode", Arc::new(StringArray::from(vec!["idle", "moving"]))),
        ],
    );
    let units = scratch.0.join("units.toml");
    std::fs::write(&units, "[units]\n'*.current' = { unit = 'A', scale = 2 }\n").unwrap();
    for (name, file, flags) in [
        (
            "long",
            &long,
            vec!["--metric", "metric", "--value", "value"],
        ),
        ("wide", &wide, vec!["--wide"]),
    ] {
        let store = scratch.0.join(name);
        let mut command = std::process::Command::new(&binary);
        command
            .args(["ti", "backfill", "--parquet"])
            .arg(file)
            .args([
                "--entity",
                "id",
                "--time",
                "time",
                "--time-unit",
                "ms",
                "--prefix",
                "robot.",
                "--units",
            ])
            .arg(&units)
            .arg("--store")
            .arg(&store)
            .args(flags);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["rows_read"], 2);
        let query = std::process::Command::new(&binary)
            .args([
                "ti",
                "query",
                "SELECT count(*) AS n, sum(\"robot.current@mean\") AS current FROM telemetry",
                "--json",
                "--store",
            ])
            .arg(&store)
            .output()
            .unwrap();
        assert!(
            query.status.success(),
            "{}",
            String::from_utf8_lossy(&query.stderr)
        );
        let response: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
        assert_eq!(response["rows"][0]["n"], 2);
        assert_eq!(response["rows"][0]["current"], 4.0);
        let status = std::process::Command::new(&binary)
            .args(["ti", "status", "--store"])
            .arg(&store)
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status["parquet_import"]["rows_read"], 2);
        assert!(status["parquet_import"]["unit_scale_misses"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}
