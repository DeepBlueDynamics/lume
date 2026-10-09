//! On-disk layout for generated signalk-parquet data.
//!
//! Every column name, hive path segment and file-naming rule lives HERE, in one
//! module, so the layout is cheap to change when the verified real-world schema
//! lands (plan/design/signalk-formats.md §1.3). Do not scatter layout strings
//! across the crate.
//!
//! Layout (real signalk-parquet raw tier):
//!   <root>/tier=raw/context=<ctx>/path=<path>/year=YYYY/day=DDD/<prefix>_<YYYY-MM-DDTHHMM>.parquet
//!   <root>/docs/*.parquet
//!   <root>/catalog/{vessels,paths,shards}/*.parquet
//!
//! Context is sanitized: `.` -> `__`, `:` -> `-`. Path: `.` -> `__`.

pub const TIER: &str = "raw";

/// Sanitize a Signal K context URN into a hive directory segment.
pub fn sanitize_context(context: &str) -> String {
    context.replace('.', "__").replace(':', "-")
}

/// Sanitize a Signal K path into a hive directory segment.
pub fn sanitize_path(path: &str) -> String {
    path.replace('.', "__")
}

/// UTC day-of-year (001-366) of a timestamp, as the `day=DDD` segment.
pub fn day_of_year(year: i32, month: u32, day: u32) -> u32 {
    use chrono::{Datelike, NaiveDate};
    NaiveDate::from_ymd_opt(year, month, day).unwrap().ordinal()
}

/// Raw-tier parquet column names, in the (sorted) order the real writer emits.
/// Every column is optional. `value` carries scalar DOUBLE/BOOLEAN/UTF8; object
/// paths use `value_<key>` columns instead and have no `value`.
pub mod raw_columns {
    // Scalar column set (the real writer sorts these alphabetically).
    pub const CONTEXT: &str = "context";
    pub const META: &str = "meta";
    pub const PATH: &str = "path";
    pub const RECEIVED_TIMESTAMP: &str = "received_timestamp";
    pub const SIGNALK_TIMESTAMP: &str = "signalk_timestamp";
    pub const SOURCE: &str = "source";
    pub const SOURCE_LABEL: &str = "source_label";
    pub const SOURCE_PGN: &str = "source_pgn";
    pub const SOURCE_SRC: &str = "source_src";
    pub const SOURCE_TYPE: &str = "source_type";
    pub const VALUE: &str = "value";
}

/// The `docs` table columns (notes / logbook / alerts), written as parquet.
pub mod docs_columns {
    pub const CONTEXT: &str = "context";
    pub const KIND: &str = "kind";
    pub const TS_START: &str = "ts_start";
    pub const TS_END: &str = "ts_end";
    pub const TITLE: &str = "title";
    pub const BODY: &str = "body";
}

/// The `vessels` catalog columns.
pub mod vessels_columns {
    pub const ORD: &str = "ord";
    pub const URN: &str = "urn";
    pub const NAME: &str = "name";
    pub const MMSI: &str = "mmsi";
    pub const FIRST_SEEN: &str = "first_seen";
    pub const LAST_SEEN: &str = "last_seen";
}

/// The `paths` catalog columns.
pub mod paths_columns {
    pub const PATH: &str = "path";
    pub const FIELD: &str = "field";
    pub const AGG: &str = "agg";
    pub const TYPE: &str = "type";
    pub const UNITS: &str = "units";
    pub const SCALE: &str = "scale";
    pub const DEPTH: &str = "depth";
    pub const DESCRIPTION: &str = "description";
    pub const FIRST_SEEN: &str = "first_seen";
    pub const LAST_SEEN: &str = "last_seen";
}

/// The `shards` catalog columns.
pub mod shards_columns {
    pub const VESSEL: &str = "vessel";
    pub const SHARD_NO: &str = "shard_no";
    pub const TS_FROM: &str = "ts_from";
    pub const TS_TO: &str = "ts_to";
    pub const SEALED: &str = "sealed";
    pub const BYTES: &str = "bytes";
    pub const HASH: &str = "hash";
}

/// Build the hive directory path for a raw-tier file.
pub fn raw_dir(root: &str, context: &str, path: &str, year: i32, doy: u32) -> String {
    format!(
        "{}/tier={}/context={}/path={}/year={}/day={:03}",
        root,
        TIER,
        sanitize_context(context),
        sanitize_path(path),
        year,
        doy
    )
}

/// Build a raw-tier file name (export-time stamp, matches the real writer).
pub fn raw_file_name(prefix: &str, export_iso_compact: &str) -> String {
    format!("{}_{}.parquet", prefix, export_iso_compact)
}
