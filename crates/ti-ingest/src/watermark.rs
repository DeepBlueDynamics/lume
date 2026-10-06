//! Watermark tracking, bucket close, late data, and ShardSink dispatch.
//!
//! Enforces:
//! - Watermark at max_event_time - 30 s lateness
//! - Clean bucket closing when watermark passes bucket end
//! - Late data arriving after bucket close emits with `rewrite: true`
//! - Complete coordination with Catalog and ShardSink

use std::collections::{BTreeMap, BTreeSet};
use ti_contracts::{
    bucket_of, BucketIx, Catalog, Result, ShardSink, TiConfig, VesselOrd, VesselSpec, EPOCH,
};

use crate::bucket::BucketWindow;
use crate::classify::Classifier;
use crate::derived::{DerivedEvent, DerivedTracker};
use crate::normalize::NormalizedValue;

pub struct WatermarkBucketer {
    max_event_time: i64,
    open_buckets: BTreeMap<(VesselOrd, BucketIx), BucketWindow>,
    closed_buckets: BTreeSet<(VesselOrd, BucketIx)>,
    classifier: Classifier,
    derived: DerivedTracker,
}

impl WatermarkBucketer {
    pub fn new(config: &TiConfig) -> Self {
        Self {
            max_event_time: EPOCH,
            open_buckets: BTreeMap::new(),
            closed_buckets: BTreeSet::new(),
            classifier: Classifier::new(config),
            derived: DerivedTracker::new(&config.derived),
        }
    }

    pub fn max_event_time(&self) -> i64 {
        self.max_event_time
    }

    pub fn watermark(&self) -> i64 {
        self.max_event_time - 30
    }

    pub fn classifier_mut(&mut self) -> &mut Classifier {
        &mut self.classifier
    }

    pub fn open_bucket_count(&self) -> usize {
        self.open_buckets.len()
    }

    pub fn is_bucket_closed(&self, vessel: VesselOrd, bucket: BucketIx) -> bool {
        self.closed_buckets.contains(&(vessel, bucket))
    }

    /// Ingest a normalized data point.
    #[allow(clippy::too_many_arguments)]
    pub fn ingest_point(
        &mut self,
        context: &str,
        path: &str,
        source: &str,
        ts: i64,
        value: NormalizedValue,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<()> {
        let canonical_urn = if context.starts_with("vessels.urn:") {
            context.to_string()
        } else if context.starts_with("urn:") {
            format!("vessels.{context}")
        } else {
            context.to_string()
        };

        let vessel = catalog.register_vessel(&VesselSpec {
            urn: canonical_urn,
            name: None,
            mmsi: None,
        })?;

        let bucket_ix = bucket_of(ts, config.width_seconds)?;
        if ts > self.max_event_time {
            self.max_event_time = ts;
        }

        // Check sticky classification
        let (eff_path, kind) = match self.classifier.classify(context, path, &value) {
            Some(res) => res,
            None => return Ok(()),
        };

        // Check derived rules
        let mut derived_events = Vec::new();
        match &value {
            NormalizedValue::String(s) => {
                derived_events.extend(self.derived.on_state_value(context, path, s));
            }
            NormalizedValue::Double(d) => {
                derived_events.extend(self.derived.on_numeric_value(context, path, *d));
            }
            NormalizedValue::Bool(b) => {
                let num = if *b { 1.0 } else { 0.0 };
                derived_events.extend(self.derived.on_numeric_value(context, path, num));
                let s = if *b { "true" } else { "false" };
                derived_events.extend(self.derived.on_state_value(context, path, s));
            }
            _ => {}
        }

        let is_closed = self.closed_buckets.contains(&(vessel, bucket_ix));

        if is_closed {
            // Late arrival after bucket close -> emit immediately with rewrite: true
            let mut late_window = BucketWindow::default();
            populate_window(
                &mut late_window,
                &eff_path,
                &value,
                source,
                ts,
                &kind,
                config,
            );
            for event in derived_events {
                populate_derived_event(&mut late_window, event, source);
            }
            let records = late_window.emit_records(vessel, bucket_ix, true, config, catalog)?;
            if !records.is_empty() {
                sink.apply(&records)?;
            }
        } else {
            // Open bucket
            let window = self.open_buckets.entry((vessel, bucket_ix)).or_default();
            populate_window(window, &eff_path, &value, source, ts, &kind, config);
            for event in derived_events {
                populate_derived_event(window, event, source);
            }

            self.close_ready_buckets(config, catalog, sink)?;
        }

        Ok(())
    }

    /// Close any open buckets whose end time <= watermark (max_event_time - 30 s).
    pub fn close_ready_buckets(
        &mut self,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<usize> {
        let watermark = self.watermark();
        let mut closed_count = 0;

        let to_close: Vec<(VesselOrd, BucketIx)> = self
            .open_buckets
            .keys()
            .filter(|(_, b)| {
                let bucket_end = EPOCH + (((*b as i64) + 1) * (config.width_seconds as i64));
                bucket_end <= watermark
            })
            .cloned()
            .collect();

        for key in to_close {
            if let Some(window) = self.open_buckets.remove(&key) {
                let (vessel, bucket) = key;
                let records = window.emit_records(vessel, bucket, false, config, catalog)?;
                if !records.is_empty() {
                    sink.apply(&records)?;
                }
                self.closed_buckets.insert(key);
                closed_count += 1;
            }
        }

        Ok(closed_count)
    }

    /// Explicit flush: close and emit all remaining open buckets.
    pub fn flush_all(
        &mut self,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<usize> {
        let mut count = 0;
        let keys: Vec<(VesselOrd, BucketIx)> = self.open_buckets.keys().cloned().collect();
        for key in keys {
            if let Some(window) = self.open_buckets.remove(&key) {
                let (vessel, bucket) = key;
                let records = window.emit_records(vessel, bucket, false, config, catalog)?;
                if !records.is_empty() {
                    sink.apply(&records)?;
                }
                self.closed_buckets.insert(key);
                count += 1;
            }
        }
        sink.flush()?;
        Ok(count)
    }
}

fn source_priority(path: &str, source: &str, config: &TiConfig) -> usize {
    if let Some(list) = config.source_priorities.get(path) {
        if let Some(pos) = list.iter().position(|s| s == source) {
            return pos;
        }
        return list.len() + 10;
    }
    0
}

fn populate_window(
    window: &mut BucketWindow,
    path: &str,
    value: &NormalizedValue,
    source: &str,
    ts: i64,
    kind: &ti_contracts::FieldKind,
    config: &TiConfig,
) {
    match value {
        NormalizedValue::Double(d) => {
            let scale = match kind {
                ti_contracts::FieldKind::Bsi { scale } => *scale,
                _ => 3,
            };
            window.add_numeric(path, *d, scale, source, ts);
        }
        NormalizedValue::String(s) => {
            let prio = source_priority(path, source, config);
            window.add_set(path, s, prio, source, ts);
        }
        NormalizedValue::Bool(b) => {
            let s = if *b { "true" } else { "false" };
            let prio = source_priority(path, source, config);
            window.add_set(path, s, prio, source, ts);
        }
        NormalizedValue::Geo { lat: _, lon: _ } => {
            // Ingest cell 0 stub until W6
            window.add_geo_cell(path, 0, source);
        }
        NormalizedValue::Null => {}
    }
}

fn populate_derived_event(window: &mut BucketWindow, event: DerivedEvent, source: &str) {
    match event {
        DerivedEvent::Transition { output }
        | DerivedEvent::RisingEdge { output }
        | DerivedEvent::NotificationRaise { output } => {
            window.add_count(&output, 1, source);
        }
    }
}
