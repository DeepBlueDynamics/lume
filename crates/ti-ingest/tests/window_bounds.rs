use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::Arc};
use arrow_array::{ArrayRef, Float64Array, Int64Array, StringArray, RecordBatch};
use arrow_schema::{Field, Schema};
use parquet::arrow::ArrowWriter;
use ti_contracts::{BucketRecord, Catalog, EntityMapping, Error, FieldValue, ParquetFormat, ParquetMapping, Result, ShardKey, ShardManifestEntry, ShardSink, TiConfig, TimeUnit, EPOCH};
use ti_ingest::mapped_parquet;
struct Scratch(PathBuf);
impl Scratch { fn new(name: &str) -> Self { let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("window-{name}-{}",std::process::id())); std::fs::create_dir_all(&root).unwrap();Self(root) } }
impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
fn write(path: &Path, times: Vec<i64>, values: Vec<f64>) {
    let columns: Vec<(&str, ArrayRef)> = vec![("id",Arc::new(StringArray::from(vec!["robots.urn:bounded"; times.len()]))),("time",Arc::new(Int64Array::from(times))),("current",Arc::new(Float64Array::from(values)))];
    let schema = Arc::new(Schema::new(columns.iter().map(|(n,a)|Field::new(*n,a.data_type().clone(),false)).collect::<Vec<_>>()));
    let batch = RecordBatch::try_new(schema.clone(),columns.into_iter().map(|(_,a)|a).collect()).unwrap();
    let mut writer = ArrowWriter::try_new(std::fs::File::create(path).unwrap(),schema,None).unwrap();writer.write(&batch).unwrap();writer.close().unwrap();
}
fn mapping(files: String) -> ParquetMapping { ParquetMapping { files, entity:EntityMapping::Column("id".into()),time:"time".into(),time_unit:TimeUnit::Seconds,timezone:None,format:ParquetFormat::Wide,metric:None,value:None,source:None,prefix:String::new(),exclude:vec![] } }
#[derive(Default)]
struct Capture { records: BTreeMap<u32, FieldValue>, rss: Vec<u64> }
fn rss() -> u64 {
    #[cfg(target_os="linux")]
    { let status = std::fs::read_to_string("/proc/self/status").unwrap();
      status.lines().find(|l|l.starts_with("VmRSS:")).unwrap().split_whitespace().nth(1).unwrap().parse::<u64>().unwrap()*1024 }
    #[cfg(not(target_os="linux"))]
    { 0 }
}
impl ShardSink for Capture {
    fn apply(&mut self, records: &[BucketRecord]) -> Result<()> { for r in records.iter().filter(|r|r.bucket==0) { self.records.insert(r.field,r.value.clone()); } self.rss.push(rss());Ok(()) }
    fn flush(&mut self) -> Result<()> { self.rss.push(rss());Ok(()) }
    fn seal(&mut self, _: ShardKey) -> Result<ShardManifestEntry> { Err(Error::Unsupported("capture seal".into())) }
}
#[test]
fn multi_file_watermarks_bound_memory_and_late_rows_merge_exactly() {
    let scratch = Scratch::new("rss");
    for file in 0..120 {
        write(&scratch.0.join(format!("{file:03}.parquet")),(0..256).map(|i|EPOCH+(file*256+i)*10).collect(),vec![1.0;256]);
    }
    write(&scratch.0.join("999-late.parquet"),vec![EPOCH],vec![3.0]);
    let mut config = TiConfig { store_root:scratch.0.join("store").to_string_lossy().into_owned(),..Default::default() };
    config.profiles.opt_in.push("count".into());
    let catalog = ti_store::DiskCatalog::open_or_create(Path::new(&config.store_root)).unwrap();
    let catalogs = BTreeMap::from([("default".into(),&catalog as &dyn Catalog)]);
    let mut sink = Capture::default();
    let report = mapped_parquet::backfill(&[mapping(scratch.0.join("*.parquet").to_string_lossy().into_owned())],&config,&catalogs,&mut BTreeMap::from([("default".into(),&mut sink as &mut dyn ShardSink)])).unwrap();
    assert_eq!(report.files,121);
    assert!(report.peak_active_windows <= 620,"{}",report.peak_active_windows);
    assert!(report.peak_active_bytes < 2*1024*1024);
    assert_eq!(report.late_bucket_reloads,1);
    assert!(report.journal_bytes > 0);
    let mean = catalog.fields().unwrap().into_iter().find(|f|f.path=="current"&&f.agg==Some(ti_contracts::Agg::Mean)).unwrap().id;
    assert_eq!(sink.records[&mean],FieldValue::Int(2000));
    let count = catalog.fields().unwrap().into_iter().find(|f|f.path=="current"&&f.agg==Some(ti_contracts::Agg::Count)).unwrap().id;
    assert_eq!(sink.records[&count],FieldValue::Int(2));
    let steady = &sink.rss[sink.rss.len()/3..];
    let range = steady.iter().max().unwrap()-steady.iter().min().unwrap();
    #[cfg(target_os="linux")]
    assert!(range < 32*1024*1024,"RSS grew by {range} bytes across the multi-file run: {:?}",sink.rss);
    println!("peak windows={}, active bytes={}, journal bytes={}, steady RSS growth={range}",report.peak_active_windows,report.peak_active_bytes,report.journal_bytes);
    assert!(!std::fs::read_dir(&config.store_root).unwrap().any(|e|e.unwrap().file_name().to_string_lossy().starts_with(".parquet-windows-")));
}
#[test]
fn active_index_and_scratch_caps_fail_with_clear_errors() {
    let scratch = Scratch::new("caps");
    write(&scratch.0.join("input.parquet"),vec![EPOCH,EPOCH+100_000,EPOCH+200_000],vec![1.0,2.0,3.0]);
    for (limit,message) in [(0,"limits must be positive"),(1,"memory cap"),(2,"scratch cap"),(3,"index cap")] {
        let mut config = TiConfig { store_root:scratch.0.join(format!("store-{limit}")).to_string_lossy().into_owned(),..Default::default() };
        match limit {
            0 | 1 => config.sources.backfill.max_active_bytes=limit,
            2 => config.sources.backfill.max_journal_bytes=1,
            3 => config.sources.backfill.max_index_entries=1,
            _ => unreachable!(),
        }
        let catalog = ti_store::DiskCatalog::open_or_create(Path::new(&config.store_root)).unwrap();
        let mut sink=Capture::default();
        let err=mapped_parquet::backfill(&[mapping(scratch.0.join("input.parquet").to_string_lossy().into_owned())],&config,&BTreeMap::from([("default".into(),&catalog as &dyn Catalog)]),&mut BTreeMap::from([("default".into(),&mut sink as &mut dyn ShardSink)])).unwrap_err();
        assert!(err.to_string().contains(message),"{err}");
    }
}
