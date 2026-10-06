//! Watermark tracking, bucket close, late data, and ShardSink dispatch.
//!
//! Enforces:
//! - Watermark at max_event_time - 30 s lateness
//! - Clean bucket closing when watermark passes bucket end
//! - Late data arriving after bucket close emits with `rewrite: true`
//! - Complete coordination with Catalog and ShardSink

use std::collections::{BTreeMap, BTreeSet};
use ti_contracts::{
    bucket_of, BucketIx, Catalog, Result, ShardSink, StoreConfig, TiConfig, VesselOrd, VesselSpec,
    EPOCH,
};

use crate::bucket::BucketWindow;
use crate::classify::Classifier;
use crate::derived::{DerivedEvent, DerivedTracker};
use crate::normalize::NormalizedValue;

/// Receives newly closed telemetry buckets after the sink has published its batch.
/// Bounds are inclusive. Observers run in vessel/bucket order, once for each new
/// close; late rewrites do not retrigger them. Errors propagate to the ingest caller.
/// Historical rules run after complete backfill, rather than partial per-path files.
pub trait ClosedBucketObserver: Send {
    fn on_closed(
        &mut self,
        vessel: VesselOrd,
        from_bucket: BucketIx,
        to_bucket: BucketIx,
    ) -> Result<()>;
}

pub struct WatermarkBucketer {
    width_seconds: u64,
    store_name: String,
    store_aggs: BTreeMap<String, Vec<String>>,
    paths: Option<Vec<String>>,
    max_event_time: i64,
    open_buckets: BTreeMap<(VesselOrd, BucketIx), BucketWindow>,
    closed_buckets: BTreeSet<(VesselOrd, BucketIx)>,
    classifier: Classifier,
    derived: DerivedTracker,
    closed_observer: Option<Box<dyn ClosedBucketObserver>>,
}

impl WatermarkBucketer {
    pub fn new(config: &TiConfig) -> Self {
        Self {
            width_seconds: config.width_seconds,
            store_name: "default".into(),
            store_aggs: BTreeMap::new(),
            paths: None,
            max_event_time: EPOCH,
            open_buckets: BTreeMap::new(),
            closed_buckets: BTreeSet::new(),
            classifier: Classifier::new(config),
            derived: DerivedTracker::new(&config.derived),
            closed_observer: None,
        }
    }

    pub fn new_for_store(name: &str, store: &StoreConfig, config: &TiConfig) -> Result<Self> {
        let width_seconds = store.width_seconds()?;
        Ok(Self {
            width_seconds,
            store_name: name.to_string(),
            store_aggs: store.aggs.clone(),
            paths: store.paths.clone(),
            max_event_time: EPOCH,
            open_buckets: BTreeMap::new(),
            closed_buckets: BTreeSet::new(),
            classifier: Classifier::new(config),
            derived: DerivedTracker::new(&config.derived),
            closed_observer: None,
        })
    }

    /// Install or detach the rule evaluator without introducing a SQL dependency.
    pub fn set_closed_bucket_observer(&mut self, observer: Option<Box<dyn ClosedBucketObserver>>) {
        self.closed_observer = observer;
    }

    fn notify_closed(&mut self, keys: &[(VesselOrd, BucketIx)]) -> Result<()> {
        if let Some(observer) = &mut self.closed_observer {
            for &(vessel, bucket) in keys {
                if let Err(e) = observer.on_closed(vessel, bucket, bucket) {
                    eprintln!("ClosedBucketObserver error: {e}");
                }
            }
        }
        Ok(())
    }

    pub fn width_seconds(&self) -> u64 {
        self.width_seconds
    }

    pub fn store_name(&self) -> &str {
        &self.store_name
    }

    pub fn store_aggs(&self) -> &BTreeMap<String, Vec<String>> {
        &self.store_aggs
    }

    pub fn paths(&self) -> Option<&[String]> {
        self.paths.as_deref()
    }

    pub fn is_path_allowed(&self, path: &str, config: &TiConfig) -> bool {
        if let Some(ref allow_list) = self.paths {
            crate::normalize::is_path_allowed(path, allow_list, &config.deny_paths)
        } else {
            crate::normalize::is_path_allowed(path, &config.allow_paths, &config.deny_paths)
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

        let bucket_ix = bucket_of(ts, self.width_seconds)?;
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
        let store_aggs_opt = if self.store_aggs.is_empty() {
            None
        } else {
            Some(&self.store_aggs)
        };

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
            )?;
            for event in derived_events {
                populate_derived_event(&mut late_window, event, source);
            }
            let records = late_window.emit_records_with_aggs(
                vessel,
                bucket_ix,
                true,
                store_aggs_opt,
                config,
                catalog,
            )?;
            if !records.is_empty() {
                sink.apply(&records)?;
            }
        } else {
            // Open bucket
            let window = self.open_buckets.entry((vessel, bucket_ix)).or_default();
            populate_window(window, &eff_path, &value, source, ts, &kind, config)?;
            for event in derived_events {
                populate_derived_event(window, event, source);
            }

            self.close_ready_buckets(config, catalog, sink)?;
        }

        Ok(())
    }

    /// Close any open buckets whose end time <= watermark.
    pub fn advance_watermark(
        &mut self,
        watermark: i64,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<usize> {
        let mut closed_count = 0;
        let mut newly_closed = Vec::new();

        let to_close: Vec<(VesselOrd, BucketIx)> = self
            .open_buckets
            .keys()
            .filter(|(_, b)| {
                let bucket_end = EPOCH + (((*b as i64) + 1) * (self.width_seconds as i64));
                bucket_end <= watermark
            })
            .cloned()
            .collect();

        let store_aggs_opt = if self.store_aggs.is_empty() {
            None
        } else {
            Some(&self.store_aggs)
        };

        for key in to_close {
            if let Some(window) = self.open_buckets.remove(&key) {
                let (vessel, bucket) = key;
                let records = window.emit_records_with_aggs(
                    vessel,
                    bucket,
                    false,
                    store_aggs_opt,
                    config,
                    catalog,
                )?;
                if !records.is_empty() {
                    sink.apply(&records)?;
                }
                self.closed_buckets.insert(key);
                closed_count += 1;
                newly_closed.push(key);
            }
        }

        if self.closed_observer.is_some() && !newly_closed.is_empty() {
            sink.flush()?;
        }
        self.notify_closed(&newly_closed)?;
        Ok(closed_count)
    }

    /// Close any open buckets whose end time <= watermark (max_event_time - 30 s).
    pub fn close_ready_buckets(
        &mut self,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<usize> {
        let watermark = self.watermark();
        self.advance_watermark(watermark, config, catalog, sink)
    }

    /// Explicit flush: close and emit all remaining open buckets.
    pub fn flush_all(
        &mut self,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<usize> {
        let mut count = 0;
        let mut newly_closed = Vec::new();
        let keys: Vec<(VesselOrd, BucketIx)> = self.open_buckets.keys().cloned().collect();

        let store_aggs_opt = if self.store_aggs.is_empty() {
            None
        } else {
            Some(&self.store_aggs)
        };

        for key in keys {
            if let Some(window) = self.open_buckets.remove(&key) {
                let (vessel, bucket) = key;
                let records = window.emit_records_with_aggs(
                    vessel,
                    bucket,
                    false,
                    store_aggs_opt,
                    config,
                    catalog,
                )?;
                if !records.is_empty() {
                    sink.apply(&records)?;
                }
                self.closed_buckets.insert(key);
                count += 1;
                newly_closed.push(key);
            }
        }
        sink.flush()?;
        self.notify_closed(&newly_closed)?;
        Ok(count)
    }
}

fn source_priority(path: &str, source: &str, config: &TiConfig) -> usize {
    config
        .source_priorities
        .get(path)
        .and_then(|l| l.iter().position(|s| s == source))
        .unwrap_or(0)
}

fn populate_window(
    window: &mut BucketWindow,
    path: &str,
    value: &NormalizedValue,
    source: &str,
    ts: i64,
    kind: &ti_contracts::FieldKind,
    config: &TiConfig,
) -> Result<()> {
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
        NormalizedValue::Geo { lat, lon } => {
            for cell in ti_geo::cells_for(*lat, *lon)? {
                window.add_geo_cell(path, cell, source);
            }
        }
        NormalizedValue::Null => {}
    }
    Ok(())
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

/// Multi-store routing and bucketing coordinator (D30).
/// Routes incoming points to every configured store whose `paths` allow-list matches,
/// bucketing each stream independently at that store's width with its aggregates.
pub struct MultiStoreBucketer {
    bucketers: BTreeMap<String, WatermarkBucketer>,
}

impl MultiStoreBucketer {
    /// Initialize a bucketer for each store configured in `config.resolved_stores()`.
    pub fn new(config: &TiConfig) -> Result<Self> {
        let mut bucketers = BTreeMap::new();
        for (name, store_cfg) in config.resolved_stores() {
            let b = WatermarkBucketer::new_for_store(&name, &store_cfg, config)?;
            bucketers.insert(name, b);
        }
        Ok(Self { bucketers })
    }

    /// Register an observer for one named store; alert rules use "default".
    pub fn set_closed_bucket_observer(
        &mut self,
        store_name: &str,
        observer: Option<Box<dyn ClosedBucketObserver>>,
    ) -> Result<()> {
        self.bucketers
            .get_mut(store_name)
            .ok_or_else(|| {
                ti_contracts::Error::InvalidInput(format!("unknown store {store_name:?}"))
            })?
            .set_closed_bucket_observer(observer);
        Ok(())
    }

    pub fn bucketer(&self, store_name: &str) -> Option<&WatermarkBucketer> {
        self.bucketers.get(store_name)
    }

    pub fn bucketer_mut(&mut self, store_name: &str) -> Option<&mut WatermarkBucketer> {
        self.bucketers.get_mut(store_name)
    }

    pub fn stores(&self) -> impl Iterator<Item = &String> {
        self.bucketers.keys()
    }

    pub fn max_event_time(&self) -> i64 {
        self.bucketers
            .values()
            .map(|b| b.max_event_time())
            .max()
            .unwrap_or(EPOCH)
    }

    pub fn register_meta_units(&mut self, path: &str, units: &str) {
        for b in self.bucketers.values_mut() {
            b.classifier_mut().register_meta_units(path, units);
        }
    }

    /// Ingest a normalized data point, fanning it out to every store whose allow-list matches.
    /// Returns the number of stores that ingested the point.
    #[allow(clippy::too_many_arguments)]
    pub fn ingest_point(
        &mut self,
        context: &str,
        path: &str,
        source: &str,
        ts: i64,
        value: &NormalizedValue,
        config: &TiConfig,
        catalogs: &BTreeMap<String, &dyn Catalog>,
        sinks: &mut BTreeMap<String, &mut dyn ShardSink>,
    ) -> Result<usize> {
        let mut matched = 0;
        for (name, bucketer) in &mut self.bucketers {
            if bucketer.is_path_allowed(path, config) {
                if let (Some(catalog), Some(sink)) = (catalogs.get(name), sinks.get_mut(name)) {
                    bucketer.ingest_point(
                        context,
                        path,
                        source,
                        ts,
                        value.clone(),
                        config,
                        *catalog,
                        *sink,
                    )?;
                    matched += 1;
                }
            }
        }
        Ok(matched)
    }

    /// Ingest a raw Signal K data point, normalizing it and fanning it out to every matching store.
    pub fn ingest_raw(
        &mut self,
        raw: crate::decode::RawDataPoint,
        config: &TiConfig,
        catalogs: &BTreeMap<String, &dyn Catalog>,
        sinks: &mut BTreeMap<String, &mut dyn ShardSink>,
    ) -> Result<usize> {
        let norm_pts =
            crate::normalize::normalize_point(raw, &config.allow_paths, &config.deny_paths);
        let mut total = 0;
        for p in norm_pts {
            total += self.ingest_point(
                &p.context,
                &p.path,
                &p.source,
                p.timestamp,
                &p.value,
                config,
                catalogs,
                sinks,
            )?;
        }
        Ok(total)
    }

    /// Close and emit all open buckets across all stores.
    pub fn flush_all(
        &mut self,
        config: &TiConfig,
        catalogs: &BTreeMap<String, &dyn Catalog>,
        sinks: &mut BTreeMap<String, &mut dyn ShardSink>,
    ) -> Result<usize> {
        let mut total = 0;
        for (name, bucketer) in &mut self.bucketers {
            if let (Some(catalog), Some(sink)) = (catalogs.get(name), sinks.get_mut(name)) {
                total += bucketer.flush_all(config, *catalog, *sink)?;
            }
        }
        Ok(total)
    }

    /// Advance watermark across all stores, closing buckets that have aged out.
    pub fn advance_watermark(
        &mut self,
        watermark: i64,
        config: &TiConfig,
        catalogs: &BTreeMap<String, &dyn Catalog>,
        sinks: &mut BTreeMap<String, &mut dyn ShardSink>,
    ) -> Result<usize> {
        let mut total = 0;
        for (name, bucketer) in &mut self.bucketers {
            if let (Some(catalog), Some(sink)) = (catalogs.get(name), sinks.get_mut(name)) {
                total += bucketer.advance_watermark(watermark, config, *catalog, *sink)?;
            }
        }
        Ok(total)
    }

    /// Close ready buckets across all stores whose bucket_end <= that store's watermark.
    pub fn close_ready_buckets(
        &mut self,
        config: &TiConfig,
        catalogs: &BTreeMap<String, &dyn Catalog>,
        sinks: &mut BTreeMap<String, &mut dyn ShardSink>,
    ) -> Result<usize> {
        let mut total = 0;
        for (name, bucketer) in &mut self.bucketers {
            if let (Some(catalog), Some(sink)) = (catalogs.get(name), sinks.get_mut(name)) {
                total += bucketer.close_ready_buckets(config, *catalog, *sink)?;
            }
        }
        Ok(total)
    }

    /// Ingest a batch of raw data points in backfill mode.
    /// For each store whose sink and catalog are provided, points are normalized,
    /// filtered by that store's allow-list, classified, accumulated into that store's
    /// bucket windows, and emitted with `rewrite: true`.
    /// Records are applied to each store's sink in chunks of at most `chunk_size` records.
    /// Returns the number of buckets emitted per store.
    pub fn backfill_raw_points(
        &mut self,
        raw_points: &[crate::decode::RawDataPoint],
        config: &TiConfig,
        catalogs: &BTreeMap<String, &dyn Catalog>,
        sinks: &mut BTreeMap<String, &mut dyn ShardSink>,
        chunk_size: usize,
    ) -> Result<BTreeMap<String, usize>> {
        let mut emitted_counts = BTreeMap::new();

        for (name, bucketer) in &mut self.bucketers {
            if let (Some(catalog), Some(sink)) = (catalogs.get(name), sinks.get_mut(name)) {
                let mut windows: BTreeMap<(VesselOrd, u32), BucketWindow> = BTreeMap::new();
                let width_seconds = bucketer.width_seconds();
                let store_aggs = bucketer.store_aggs().clone();
                let store_aggs_opt = if store_aggs.is_empty() {
                    None
                } else {
                    Some(&store_aggs)
                };

                for raw in raw_points {
                    let norm_points = crate::normalize::normalize_point(
                        raw.clone(),
                        &config.allow_paths,
                        &config.deny_paths,
                    );
                    for p in norm_points {
                        if !bucketer.is_path_allowed(&p.path, config) {
                            continue;
                        }

                        let canonical_urn = if p.context.starts_with("vessels.urn:") {
                            p.context.clone()
                        } else if p.context.starts_with("urn:") {
                            format!("vessels.{}", p.context)
                        } else {
                            p.context.clone()
                        };

                        let vessel = catalog.register_vessel(&VesselSpec {
                            urn: canonical_urn,
                            name: None,
                            mmsi: None,
                        })?;
                        let bucket_ix = bucket_of(p.timestamp, width_seconds)?;

                        let (eff_path, kind) = match bucketer
                            .classifier_mut()
                            .classify(&p.context, &p.path, &p.value)
                        {
                            Some(res) => res,
                            None => continue,
                        };

                        let window = windows.entry((vessel, bucket_ix)).or_default();
                        populate_window(
                            window,
                            &eff_path,
                            &p.value,
                            &p.source,
                            p.timestamp,
                            &kind,
                            config,
                        )?;
                    }
                }

                let mut buckets_emitted = 0;
                let mut pending = Vec::new();
                for ((vessel, bucket_ix), window) in windows {
                    let records = window.emit_records_with_aggs(
                        vessel,
                        bucket_ix,
                        true,
                        store_aggs_opt,
                        config,
                        *catalog,
                    )?;
                    if !records.is_empty() {
                        pending.extend(records);
                        buckets_emitted += 1;
                        if pending.len() >= chunk_size {
                            sink.apply(&pending)?;
                            pending.clear();
                        }
                    }
                }
                if !pending.is_empty() {
                    sink.apply(&pending)?;
                }
                emitted_counts.insert(name.clone(), buckets_emitted);
            }
        }

        Ok(emitted_counts)
    }
}
