//! Session capture and deterministic NDJSON replay.
//!
//! Enforces:
//! - Line-delimited JSON (NDJSON) capture of Signal K deltas
//! - Deterministic replay into the ingest pipeline for testing and M2 verification

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use ti_contracts::{Catalog, Error, Result, ShardSink, TiConfig};

use crate::decode::{decode_delta, SignalKDelta};
use crate::normalize::normalize_point;
use crate::watermark::WatermarkBucketer;

pub struct DeltaRecorder {
    file: File,
}

impl DeltaRecorder {
    pub fn create(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Ok(Self { file })
    }

    pub fn record_delta(&mut self, delta: &SignalKDelta) -> Result<()> {
        let json = serde_json::to_string(delta)
            .map_err(|e| Error::InvalidInput(format!("json encode error: {e}")))?;
        writeln!(self.file, "{json}")?;
        self.file.flush()?;
        Ok(())
    }
}

pub struct DeltaReplay {
    reader: BufReader<File>,
}

impl DeltaReplay {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        Ok(Self {
            reader: BufReader::new(file),
        })
    }

    /// Replay all recorded deltas through the watermark bucketer into a sink.
    pub fn replay_all(
        mut self,
        self_urn: &str,
        bucketer: &mut WatermarkBucketer,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<usize> {
        let mut line = String::new();
        let mut count = 0;

        while self.reader.read_line(&mut line)? > 0 {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                let delta: SignalKDelta = serde_json::from_str(trimmed)
                    .map_err(|e| Error::Corrupt(format!("invalid delta JSON: {e}")))?;

                let (raw_points, meta) = decode_delta(&delta, self_urn, bucketer.max_event_time());
                for (p, v) in meta {
                    if let Some(units) = v.get("units").and_then(|u| u.as_str()) {
                        bucketer.classifier_mut().register_meta_units(&p, units);
                    }
                }

                for raw in raw_points {
                    let norm_points = normalize_point(raw, &config.allow_paths, &config.deny_paths);
                    for p in norm_points {
                        bucketer.ingest_point(
                            &p.context,
                            &p.path,
                            &p.source,
                            p.timestamp,
                            p.value,
                            config,
                            catalog,
                            sink,
                        )?;
                        count += 1;
                    }
                }
            }
            line.clear();
        }

        // Flush all remaining open buckets at end of replay
        bucketer.flush_all(config, catalog, sink)?;
        Ok(count)
    }
}
