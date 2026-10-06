//! Ingest pipeline for the Lume Telemetry Index (`ti-ingest`).
//!
//! Enforces:
//! - Signal K delta decoding, source precedence and timestamp parsing (spec 06, signalk-formats)
//! - Object flattening, H3 cell stub, and sticky type classification (bsi vs set)
//! - Accumulators and aggregate profiles (default, slow, opt-in)
//! - Watermark closing and late data rewriting (D16, D21)
//! - NDJSON delta recording and deterministic replay
//! - InfluxDB backfill mapper

pub mod bucket;
pub mod classify;
pub mod decode;
pub mod derived;
pub mod docs;
pub mod influx;
pub mod mapped_parquet;
pub mod normalize;
pub mod notifications;
pub mod parquet;
mod window_journal;
pub mod recorder;
pub mod service;
pub mod watermark;
pub mod websocket;

pub use bucket::{resolve_aggs_for_path, BucketWindow};
pub use classify::Classifier;
pub use decode::{decode_delta, RawDataPoint, SignalKDelta, SignalKSource, SignalKUpdate};
pub use derived::{DerivedEvent, DerivedTracker};
pub use influx::{map_influx_point, InfluxPoint, InfluxValue};
pub use normalize::{normalize_point, NormalizedPoint, NormalizedValue};
pub use parquet::{
    backfill_directory, backfill_directory_stores, backfill_parquet_file,
    backfill_parquet_file_stores, compute_file_hash, hash_to_hex, read_parquet_points,
    BackfillStatus,
};
pub use recorder::{DeltaRecorder, DeltaReplay};
pub use service::{
    normalize_signalk_url, resolve_token, ClockFn, IngestService, IngestServiceOptions,
};
pub use watermark::{ClosedBucketObserver, MultiStoreBucketer, WatermarkBucketer};
pub use websocket::{
    build_subscription_messages, connect_signalk, process_message, process_message_multi,
    run_stream_loop, run_stream_loop_multi, subscribe_signalk,
};

pub fn init() {}
