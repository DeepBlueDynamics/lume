//! Parquet writing: emits the real signalk-parquet raw-tier layout, plus docs and
//! catalog tables. All Arrow/parquet column choices live here.

use crate::gen::{self, Attitude, Doc, Generated, Position, Sample};
use crate::layout;
use crate::model;
use arrow::array::{
    ArrayRef, BooleanArray, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray,
    UInt16Array, UInt32Array, UInt8Array,
};
use arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use std::collections::BTreeMap;
use std::sync::Arc;

fn props() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build()
}

fn write_batch(schema: &Schema, batch: &RecordBatch, path: &str) {
    let parent = std::path::Path::new(path).parent().unwrap();
    std::fs::create_dir_all(parent).unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut w = ArrowWriter::try_new(file, Arc::new(schema.clone()), Some(props())).unwrap();
    w.write(batch).unwrap();
    w.close().unwrap();
}

fn str_array(v: &[String]) -> ArrayRef {
    Arc::new(StringArray::from(v.to_vec()))
}
fn f64_array(v: &[f64]) -> ArrayRef {
    Arc::new(Float64Array::from(v.to_vec()))
}

/// Emit the whole raw tier + docs + catalogs under `root`. Returns (raw files, doc files, catalog files).
pub fn write_all(
    root: &str,
    gen: &Generated,
    n_vessels: usize,
    start: i64,
    end: i64,
) -> (u64, u64, u64) {
    let raw = write_raw(root, &gen.samples, &gen.positions, &gen.attitudes);
    let docs = write_docs(root, &gen.docs);
    let cats = write_catalogs(root, n_vessels, start, end);
    (raw, docs, cats)
}

/// Streaming entry point: run [`gen::stream`] and write each (vessel, day) chunk
/// to disk immediately, so the full correctness set never exceeds a day's memory.
/// Returns (raw files, doc files, catalog files).
pub fn write_all_stream(
    root: &str,
    seed: u64,
    n_vessels: usize,
    start: i64,
    end: i64,
) -> (u64, u64, u64) {
    write_all_stream_with_config(
        root,
        seed,
        n_vessels,
        start,
        end,
        &gen::GenConfig::default(),
    )
}

/// Streaming entry point with explicit GenConfig.
pub fn write_all_stream_with_config(
    root: &str,
    seed: u64,
    n_vessels: usize,
    start: i64,
    end: i64,
    config: &gen::GenConfig,
) -> (u64, u64, u64) {
    let mut raw = 0u64;
    let mut docs = 0u64;

    let mut on_day = |context: &str, _day: i64, s: &[Sample], p: &[Position], a: &[Attitude]| {
        raw += write_day(root, context, s, p, a);
    };
    let mut on_docs = |context: &str, d: &[Doc]| {
        docs += write_docs(root, d);
        let _ = context;
    };
    gen::stream_with_config(
        seed,
        n_vessels,
        start,
        end,
        config,
        &mut on_day,
        &mut on_docs,
    );

    let cats = write_catalogs(root, n_vessels, start, end);
    (raw, docs, cats)
}

/// Write one (context, day) chunk: group its samples by path and emit each path's
/// raw-tier file for that day.
fn write_day(
    root: &str,
    context: &str,
    samples: &[Sample],
    positions: &[Position],
    attitudes: &[Attitude],
) -> u64 {
    let mut files = 0u64;

    let mut by_path: BTreeMap<String, Vec<&Sample>> = BTreeMap::new();
    for s in samples {
        by_path.entry(s.path.clone()).or_default().push(s);
    }
    for (path, ss) in by_path {
        let (year, doy) = gen::day_key(ss[0].ts_secs);
        let dir = layout::raw_dir(root, context, &path, year, doy);
        let fname = layout::raw_file_name("signalk_data", &format!("{:04}0101T0400", year));
        let out = format!("{}/{}", dir, fname);
        write_scalar_group(context, &path, &ss, &out);
        files += 1;
    }

    if !positions.is_empty() {
        let (year, doy) = gen::day_key(positions[0].ts_secs);
        let dir = layout::raw_dir(root, context, model::POSITION_PATH, year, doy);
        let fname = layout::raw_file_name("signalk_data", &format!("{:04}0101T0400", year));
        let out = format!("{}/{}", dir, fname);
        let refs: Vec<&Position> = positions.iter().collect();
        write_position_group(context, &refs, &out);
        files += 1;
    }

    if !attitudes.is_empty() {
        let (year, doy) = gen::day_key(attitudes[0].ts_secs);
        let dir = layout::raw_dir(root, context, model::ATTITUDE_PATH, year, doy);
        let fname = layout::raw_file_name("signalk_data", &format!("{:04}0101T0400", year));
        let out = format!("{}/{}", dir, fname);
        let refs: Vec<&Attitude> = attitudes.iter().collect();
        write_attitude_group(context, &refs, &out);
        files += 1;
    }

    files
}

/// Group scalar samples by (context, path, day).
fn group_samples<'a>(
    samples: &'a [Sample],
) -> BTreeMap<(String, String, i32, u32), Vec<&'a Sample>> {
    let mut map: BTreeMap<(String, String, i32, u32), Vec<&'a Sample>> = BTreeMap::new();
    for s in samples {
        let (y, doy) = gen::day_key(s.ts_secs);
        map.entry((s.context.clone(), s.path.clone(), y, doy))
            .or_default()
            .push(s);
    }
    map
}

fn write_raw(
    root: &str,
    samples: &[Sample],
    positions: &[Position],
    attitudes: &[Attitude],
) -> u64 {
    let mut files = 0u64;

    // Scalar paths.
    for ((context, path, year, doy), ss) in group_samples(samples) {
        let dir = layout::raw_dir(root, &context, &path, year, doy);
        let fname = layout::raw_file_name("signalk_data", &format!("{:04}0101T0400", year));
        let out = format!("{}/{}", dir, fname);
        write_scalar_group(&context, &path, &ss, &out);
        files += 1;
    }

    // Position object paths.
    let pos_by_day: BTreeMap<(String, i32, u32), Vec<&Position>> =
        positions.iter().fold(BTreeMap::new(), |mut m, p| {
            let (y, doy) = gen::day_key(p.ts_secs);
            m.entry((p.context.clone(), y, doy)).or_default().push(p);
            m
        });
    for ((context, year, doy), ps) in pos_by_day {
        let dir = layout::raw_dir(root, &context, model::POSITION_PATH, year, doy);
        let fname = layout::raw_file_name("signalk_data", &format!("{:04}0101T0400", year));
        let out = format!("{}/{}", dir, fname);
        write_position_group(&context, &ps, &out);
        files += 1;
    }

    // Attitude object paths.
    let att_by_day: BTreeMap<(String, i32, u32), Vec<&Attitude>> =
        attitudes.iter().fold(BTreeMap::new(), |mut m, a| {
            let (y, doy) = gen::day_key(a.ts_secs);
            m.entry((a.context.clone(), y, doy)).or_default().push(a);
            m
        });
    for ((context, year, doy), as_) in att_by_day {
        let dir = layout::raw_dir(root, &context, model::ATTITUDE_PATH, year, doy);
        let fname = layout::raw_file_name("signalk_data", &format!("{:04}0101T0400", year));
        let out = format!("{}/{}", dir, fname);
        write_attitude_group(&context, &as_, &out);
        files += 1;
    }

    files
}

fn write_scalar_group(context: &str, path: &str, ss: &[&Sample], out: &str) {
    let n = ss.len();
    // Fill timestamps / source from the samples.
    let mut received = vec![String::new(); n];
    let mut signalk = vec![String::new(); n];
    let mut source = vec![String::new(); n];
    let mut source_label = vec![String::new(); n];
    let mut source_type = vec![String::new(); n];
    let mut source_pgn = vec![String::new(); n];
    let mut source_src = vec![String::new(); n];
    let mut meta = vec![String::new(); n];
    let mut numeric = vec![f64::NAN; n];
    let mut sstr = vec![String::new(); n];

    let any_numeric = ss.iter().any(|s| s.value.is_some());
    for (i, s) in ss.iter().enumerate() {
        let iso = gen::iso(s.ts_secs);
        received[i] = iso.clone();
        signalk[i] = iso;
        source[i] = format!(
            "{{\"label\":\"{}\",\"type\":\"{}\"}}",
            s.source_label, s.source_type
        );
        source_label[i] = s.source_label.clone();
        source_type[i] = s.source_type.clone();
        source_pgn[i] = "0".to_string();
        source_src[i] = "017".to_string();
        meta[i] = "{}".to_string();
        if let Some(v) = s.value {
            numeric[i] = v;
        } else if let Some(st) = &s.value_str {
            sstr[i] = st.clone();
        }
    }

    let context_col = vec![context.to_string(); n];
    let path_col = vec![path.to_string(); n];
    let mut fields = vec![
        Field::new(layout::raw_columns::CONTEXT, DataType::Utf8, true),
        Field::new(layout::raw_columns::META, DataType::Utf8, true),
        Field::new(layout::raw_columns::PATH, DataType::Utf8, true),
        Field::new(
            layout::raw_columns::RECEIVED_TIMESTAMP,
            DataType::Utf8,
            true,
        ),
        Field::new(layout::raw_columns::SIGNALK_TIMESTAMP, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_LABEL, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_PGN, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_SRC, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_TYPE, DataType::Utf8, true),
    ];
    let mut cols: Vec<ArrayRef> = vec![
        str_array(&context_col),
        str_array(&meta),
        str_array(&path_col),
        str_array(&received),
        str_array(&signalk),
        str_array(&source),
        str_array(&source_label),
        str_array(&source_pgn),
        str_array(&source_src),
        str_array(&source_type),
    ];
    if any_numeric {
        fields.push(Field::new(
            layout::raw_columns::VALUE,
            DataType::Float64,
            true,
        ));
        cols.push(f64_array(&numeric));
    } else {
        fields.push(Field::new(layout::raw_columns::VALUE, DataType::Utf8, true));
        cols.push(str_array(&sstr));
    }
    let schema = Schema::new(fields);
    let batch = RecordBatch::try_new(Arc::new(schema.clone()), cols).unwrap();
    write_batch(&schema, &batch, out);
}

fn write_position_group(context: &str, ps: &[&Position], out: &str) {
    let n = ps.len();
    let mut received = vec![String::new(); n];
    let mut signalk = vec![String::new(); n];
    let mut source = vec![String::new(); n];
    let mut source_label = vec![String::new(); n];
    let mut source_type = vec![String::new(); n];
    let mut source_pgn = vec![String::new(); n];
    let mut source_src = vec![String::new(); n];
    let mut meta = vec![String::new(); n];
    let mut lat = vec![f64::NAN; n];
    let mut lon = vec![f64::NAN; n];
    for (i, p) in ps.iter().enumerate() {
        let iso = gen::iso(p.ts_secs);
        received[i] = iso.clone();
        signalk[i] = iso;
        source[i] = format!(
            "{{\"label\":\"{}\",\"type\":\"{}\"}}",
            p.source_label, p.source_type
        );
        source_label[i] = p.source_label.clone();
        source_type[i] = p.source_type.clone();
        source_pgn[i] = "0".to_string();
        source_src[i] = "017".to_string();
        meta[i] = "{}".to_string();
        lat[i] = p.latitude;
        lon[i] = p.longitude;
    }
    let context_col = vec![context.to_string(); n];
    let path_col = vec![model::POSITION_PATH.to_string(); n];
    let fields = vec![
        Field::new(layout::raw_columns::CONTEXT, DataType::Utf8, true),
        Field::new(layout::raw_columns::META, DataType::Utf8, true),
        Field::new(layout::raw_columns::PATH, DataType::Utf8, true),
        Field::new(
            layout::raw_columns::RECEIVED_TIMESTAMP,
            DataType::Utf8,
            true,
        ),
        Field::new(layout::raw_columns::SIGNALK_TIMESTAMP, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_LABEL, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_PGN, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_SRC, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_TYPE, DataType::Utf8, true),
        Field::new("value_latitude", DataType::Float64, true),
        Field::new("value_longitude", DataType::Float64, true),
    ];
    let cols: Vec<ArrayRef> = vec![
        str_array(&context_col),
        str_array(&meta),
        str_array(&path_col),
        str_array(&received),
        str_array(&signalk),
        str_array(&source),
        str_array(&source_label),
        str_array(&source_pgn),
        str_array(&source_src),
        str_array(&source_type),
        f64_array(&lat),
        f64_array(&lon),
    ];
    let schema = Schema::new(fields);
    let batch = RecordBatch::try_new(Arc::new(schema.clone()), cols).unwrap();
    write_batch(&schema, &batch, out);
}

fn write_attitude_group(context: &str, as_: &[&Attitude], out: &str) {
    let n = as_.len();
    let mut received = vec![String::new(); n];
    let mut signalk = vec![String::new(); n];
    let mut source = vec![String::new(); n];
    let mut source_label = vec![String::new(); n];
    let mut source_type = vec![String::new(); n];
    let mut source_pgn = vec![String::new(); n];
    let mut source_src = vec![String::new(); n];
    let mut meta = vec![String::new(); n];
    let mut roll = vec![f64::NAN; n];
    let mut pitch = vec![f64::NAN; n];
    for (i, a) in as_.iter().enumerate() {
        let iso = gen::iso(a.ts_secs);
        received[i] = iso.clone();
        signalk[i] = iso;
        source[i] = format!(
            "{{\"label\":\"{}\",\"type\":\"{}\"}}",
            a.source_label, a.source_type
        );
        source_label[i] = a.source_label.clone();
        source_type[i] = a.source_type.clone();
        source_pgn[i] = "0".to_string();
        source_src[i] = "017".to_string();
        meta[i] = "{}".to_string();
        roll[i] = a.roll;
        pitch[i] = a.pitch;
    }
    let context_col = vec![context.to_string(); n];
    let path_col = vec![model::ATTITUDE_PATH.to_string(); n];
    let fields = vec![
        Field::new(layout::raw_columns::CONTEXT, DataType::Utf8, true),
        Field::new(layout::raw_columns::META, DataType::Utf8, true),
        Field::new(layout::raw_columns::PATH, DataType::Utf8, true),
        Field::new(
            layout::raw_columns::RECEIVED_TIMESTAMP,
            DataType::Utf8,
            true,
        ),
        Field::new(layout::raw_columns::SIGNALK_TIMESTAMP, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_LABEL, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_PGN, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_SRC, DataType::Utf8, true),
        Field::new(layout::raw_columns::SOURCE_TYPE, DataType::Utf8, true),
        Field::new("value_roll", DataType::Float64, true),
        Field::new("value_pitch", DataType::Float64, true),
    ];
    let cols: Vec<ArrayRef> = vec![
        str_array(&context_col),
        str_array(&meta),
        str_array(&path_col),
        str_array(&received),
        str_array(&signalk),
        str_array(&source),
        str_array(&source_label),
        str_array(&source_pgn),
        str_array(&source_src),
        str_array(&source_type),
        f64_array(&roll),
        f64_array(&pitch),
    ];
    let schema = Schema::new(fields);
    let batch = RecordBatch::try_new(Arc::new(schema.clone()), cols).unwrap();
    write_batch(&schema, &batch, out);
}

pub fn write_docs(root: &str, docs: &[Doc]) -> u64 {
    if docs.is_empty() {
        return 0;
    }
    let n = docs.len();
    let mut context = vec![String::new(); n];
    let mut kind = vec![String::new(); n];
    let mut ts_start = vec![String::new(); n];
    let mut ts_end = vec![String::new(); n];
    let mut title = vec![String::new(); n];
    let mut body = vec![String::new(); n];
    for (i, d) in docs.iter().enumerate() {
        context[i] = d.context.clone();
        kind[i] = d.kind.clone();
        ts_start[i] = gen::iso(d.ts_start);
        ts_end[i] = gen::iso(d.ts_end);
        title[i] = d.title.clone();
        body[i] = d.body.clone();
    }
    let schema = Schema::new(vec![
        Field::new(layout::docs_columns::CONTEXT, DataType::Utf8, true),
        Field::new(layout::docs_columns::KIND, DataType::Utf8, true),
        Field::new(layout::docs_columns::TS_START, DataType::Utf8, true),
        Field::new(layout::docs_columns::TS_END, DataType::Utf8, true),
        Field::new(layout::docs_columns::TITLE, DataType::Utf8, true),
        Field::new(layout::docs_columns::BODY, DataType::Utf8, true),
    ]);
    let cols: Vec<ArrayRef> = vec![
        str_array(&context),
        str_array(&kind),
        str_array(&ts_start),
        str_array(&ts_end),
        str_array(&title),
        str_array(&body),
    ];
    let batch = RecordBatch::try_new(Arc::new(schema.clone()), cols).unwrap();
    // One docs file per context, named deterministically by sanitized context, so
    // streaming writes per vessel never overwrite each other.
    let ctx = layout::sanitize_context(&context[0]);
    let out = format!("{}/docs/{}.parquet", root, ctx);
    write_batch(&schema, &batch, &out);
    1
}

pub fn write_catalogs(root: &str, n_vessels: usize, start: i64, end: i64) -> u64 {
    let mut files = 0u64;

    {
        let n = n_vessels;
        let mut ord = vec![0u32; n];
        let mut urn = vec![String::new(); n];
        let mut name = vec![String::new(); n];
        let mut mmsi = vec![String::new(); n];
        let mut first_seen = vec![String::new(); n];
        let mut last_seen = vec![String::new(); n];
        for i in 0..n {
            ord[i] = i as u32;
            urn[i] = model::VESSEL_URNS[i].to_string();
            name[i] = model::VESSEL_NAMES[i].to_string();
            mmsi[i] = format!("367{:06}", i);
            first_seen[i] = gen::iso(start);
            last_seen[i] = gen::iso(end);
        }
        let schema = Schema::new(vec![
            Field::new(layout::vessels_columns::ORD, DataType::UInt32, true),
            Field::new(layout::vessels_columns::URN, DataType::Utf8, true),
            Field::new(layout::vessels_columns::NAME, DataType::Utf8, true),
            Field::new(layout::vessels_columns::MMSI, DataType::Utf8, true),
            Field::new(layout::vessels_columns::FIRST_SEEN, DataType::Utf8, true),
            Field::new(layout::vessels_columns::LAST_SEEN, DataType::Utf8, true),
        ]);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(UInt32Array::from(ord)),
            str_array(&urn),
            str_array(&name),
            str_array(&mmsi),
            str_array(&first_seen),
            str_array(&last_seen),
        ];
        let batch = RecordBatch::try_new(Arc::new(schema.clone()), cols).unwrap();
        let out = format!("{}/catalog/vessels/vessels.parquet", root);
        write_batch(&schema, &batch, &out);
        files += 1;
    }

    {
        let mut paths: Vec<(&str, &str)> = Vec::new();
        for p in model::NUMERIC_PATHS {
            paths.push((*p, "bsi"));
        }
        for p in model::COUNT_PATHS {
            paths.push((*p, "count"));
        }
        for p in model::SET_PATHS {
            paths.push((*p, "set"));
        }
        paths.push(("navigation.position.latitude", "bsi"));
        paths.push(("navigation.position.longitude", "bsi"));
        paths.push(("navigation.attitude.roll", "bsi"));

        let n = paths.len();
        let mut path = vec![String::new(); n];
        let mut field = vec![String::new(); n];
        let mut agg = vec![String::new(); n];
        let mut ty = vec![String::new(); n];
        let mut units = vec![String::new(); n];
        let mut scale = vec![None::<u8>; n];
        let depth = vec![None::<u16>; n];
        let mut desc = vec![String::new(); n];
        let mut first_seen = vec![String::new(); n];
        let mut last_seen = vec![String::new(); n];
        for (i, (p, t)) in paths.iter().enumerate() {
            path[i] = p.to_string();
            field[i] = p.to_string();
            agg[i] = "mean".to_string();
            ty[i] = t.to_string();
            units[i] = "".to_string();
            scale[i] = model::scale_for(p);
            desc[i] = format!("Synthetic {}", p);
            first_seen[i] = gen::iso(start);
            last_seen[i] = gen::iso(end);
        }
        let schema = Schema::new(vec![
            Field::new(layout::paths_columns::PATH, DataType::Utf8, true),
            Field::new(layout::paths_columns::FIELD, DataType::Utf8, true),
            Field::new(layout::paths_columns::AGG, DataType::Utf8, true),
            Field::new(layout::paths_columns::TYPE, DataType::Utf8, true),
            Field::new(layout::paths_columns::UNITS, DataType::Utf8, true),
            Field::new(layout::paths_columns::SCALE, DataType::UInt8, true),
            Field::new(layout::paths_columns::DEPTH, DataType::UInt16, true),
            Field::new(layout::paths_columns::DESCRIPTION, DataType::Utf8, true),
            Field::new(layout::paths_columns::FIRST_SEEN, DataType::Utf8, true),
            Field::new(layout::paths_columns::LAST_SEEN, DataType::Utf8, true),
        ]);
        let cols: Vec<ArrayRef> = vec![
            str_array(&path),
            str_array(&field),
            str_array(&agg),
            str_array(&ty),
            str_array(&units),
            Arc::new(UInt8Array::from(scale)),
            Arc::new(UInt16Array::from(depth)),
            str_array(&desc),
            str_array(&first_seen),
            str_array(&last_seen),
        ];
        let batch = RecordBatch::try_new(Arc::new(schema.clone()), cols).unwrap();
        let out = format!("{}/catalog/paths/paths.parquet", root);
        write_batch(&schema, &batch, &out);
        files += 1;
    }

    {
        let schema = Schema::new(vec![
            Field::new(layout::shards_columns::VESSEL, DataType::Utf8, true),
            Field::new(layout::shards_columns::SHARD_NO, DataType::Int32, true),
            Field::new(layout::shards_columns::TS_FROM, DataType::Utf8, true),
            Field::new(layout::shards_columns::TS_TO, DataType::Utf8, true),
            Field::new(layout::shards_columns::SEALED, DataType::Boolean, true),
            Field::new(layout::shards_columns::BYTES, DataType::Int64, true),
            Field::new(layout::shards_columns::HASH, DataType::Utf8, true),
        ]);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(Int32Array::from(Vec::<i32>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
            Arc::new(BooleanArray::from(Vec::<bool>::new())),
            Arc::new(Int64Array::from(Vec::<i64>::new())),
            Arc::new(StringArray::from(Vec::<String>::new())),
        ];
        let batch = RecordBatch::try_new(Arc::new(schema.clone()), cols).unwrap();
        let out = format!("{}/catalog/shards/shards.parquet", root);
        write_batch(&schema, &batch, &out);
        files += 1;
    }

    files
}
