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
pub mod influx;
pub mod normalize;
pub mod parquet;
pub mod recorder;
pub mod watermark;
pub mod websocket;

pub use bucket::BucketWindow;
pub use classify::Classifier;
pub use decode::{decode_delta, SignalKDelta, SignalKSource, SignalKUpdate};
pub use derived::{DerivedEvent, DerivedTracker};
pub use influx::{map_influx_point, InfluxPoint, InfluxValue};
pub use normalize::{normalize_point, NormalizedPoint, NormalizedValue};
pub use parquet::{
    backfill_parquet_file, compute_file_hash, hash_to_hex, read_parquet_points, BackfillStatus,
};
pub use recorder::{DeltaRecorder, DeltaReplay};
pub use watermark::WatermarkBucketer;
pub use websocket::{
    build_subscription_messages, connect_signalk, process_message, run_stream_loop,
    subscribe_signalk,
};

pub fn init() {}
