//! Watermark tracking, bucket close, late data, and ShardSink dispatch.
//!
//! Enforces:
//! - Watermark at max_event_time - 30 s lateness
//! - Clean bucket closing when watermark passes bucket end
//! - Late data arriving after bucket close emits with `rewrite: true`
//! - Complete coordination with Catalog and ShardSink

use crate::counters::SampleCounters;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
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

struct RetryState {
    failures: u32,
    next_attempt: Instant,
}

pub struct WatermarkBucketer {
    width_seconds: u64,
    store_name: String,
    store_aggs: BTreeMap<String, Vec<String>>,
    paths: Option<Vec<String>>,
    max_event_time: i64,
    open_buckets: BTreeMap<(VesselOrd, BucketIx), BucketWindow>,
    closed_buckets: BTreeSet<(VesselOrd, BucketIx)>,
    closed_event_windows: BTreeMap<(VesselOrd, BucketIx), BucketWindow>,
    closed_event_buckets: BTreeSet<(VesselOrd, BucketIx)>,
    classifier: Classifier,
    derived: DerivedTracker,
    closed_observer: Option<Box<dyn ClosedBucketObserver>>,
    pending_closes: BTreeSet<(VesselOrd, BucketIx)>,
    retries: BTreeMap<(VesselOrd, BucketIx), RetryState>,
    logged_failures: BTreeSet<(VesselOrd, BucketIx)>,
    counters: SampleCounters,
    window_bytes: BTreeMap<(VesselOrd, BucketIx), usize>,
    retained_bytes: usize,
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
            closed_event_windows: BTreeMap::new(),
            closed_event_buckets: BTreeSet::new(),
            classifier: Classifier::new(config),
            derived: DerivedTracker::new(&config.derived),
            closed_observer: None,
            pending_closes: BTreeSet::new(),
            retries: BTreeMap::new(),
            logged_failures: BTreeSet::new(),
            counters: SampleCounters::default(),
            window_bytes: BTreeMap::new(),
            retained_bytes: 0,
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
            closed_event_windows: BTreeMap::new(),
            closed_event_buckets: BTreeSet::new(),
            classifier: Classifier::new(config),
            derived: DerivedTracker::new(&config.derived),
            closed_observer: None,
            pending_closes: BTreeSet::new(),
            retries: BTreeMap::new(),
            logged_failures: BTreeSet::new(),
            counters: SampleCounters::default(),
            window_bytes: BTreeMap::new(),
            retained_bytes: 0,
        })
    }

    /// Install or detach the rule evaluator without introducing a SQL dependency.
    pub fn set_closed_bucket_observer(&mut self, observer: Option<Box<dyn ClosedBucketObserver>>) {
        self.closed_observer = observer;
    }

    // Conservative reservation charges every contribution, including repeated
    // samples, rather than undercounting allocator/tree/string overhead.
    fn reserve_sample(
        &mut self,
        key: (VesselOrd, BucketIx),
        path: &str,
        source: &str,
        value: &NormalizedValue,
    ) -> Result<()> {
        const MAX_WINDOWS: usize = 64;
        const MAX_BYTES: usize = 64 * 1024 * 1024;
        let text = match value {
            NormalizedValue::String(s) => s.len(),
            _ => 0,
        };
        let bytes = 1024usize
            .saturating_add(path.len().saturating_mul(8))
            .saturating_add(source.len().saturating_mul(8))
            .saturating_add(text.saturating_mul(4));
        let full = (!self.open_buckets.contains_key(&key)
            && self.open_buckets.len() >= MAX_WINDOWS)
            || self.retained_bytes.saturating_add(bytes) > MAX_BYTES;
        if self.counters.ingest_blocked || full {
            if !self.counters.ingest_blocked {
                eprintln!("INGEST BLOCKED: store={} retained_windows={} reserved_bytes={}; sink recovery required",
                    self.store_name, self.open_buckets.len(), self.retained_bytes);
            }
            self.counters.ingest_blocked = true;
            self.counters.samples_rejected_blocked =
                self.counters.samples_rejected_blocked.saturating_add(1);
            return Err(ti_contracts::Error::InvalidInput(format!(
                "ingest blocked for store {}: retained window limit reached",
                self.store_name
            )));
        }
        self.retained_bytes += bytes;
        *self.window_bytes.entry(key).or_default() += bytes;
        Ok(())
    }

    pub fn counters(&self) -> SampleCounters {
        self.counters
    }
    pub fn pending_retry_count(&self) -> usize {
        self.retries.len()
    }

    fn publish_closes(&mut self, sink: &mut dyn ShardSink, force_flush: bool) -> Result<()> {
        if force_flush || (self.closed_observer.is_some() && !self.pending_closes.is_empty()) {
            sink.flush()?;
        }
        if let Some(observer) = &mut self.closed_observer {
            let keys: Vec<_> = self.pending_closes.iter().copied().collect();
            for (vessel, bucket) in keys {
                observer.on_closed(vessel, bucket, bucket)?;
                self.pending_closes.remove(&(vessel, bucket));
            }
        }
        Ok(())
    }

    fn close_window(
        &mut self,
        key: (VesselOrd, BucketIx),
        now: Instant,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<bool> {
        if self
            .retries
            .get(&key)
            .is_some_and(|retry| now < retry.next_attempt)
        {
            return Ok(false);
        }
        let Some(window) = self.open_buckets.get(&key) else {
            return Ok(false);
        };
        let retry = self.retries.contains_key(&key);
        if retry {
            self.counters.apply_retries = self.counters.apply_retries.saturating_add(1);
        }
        let was_closed = self.closed_buckets.contains(&key);
        // A failed transaction may have published a prefix before returning an error.
        // Reinstall the complete accumulated snapshot, including counts, on retry.
        let result = window
            .emit_records_with_aggs(
                key.0,
                key.1,
                was_closed || retry,
                if self.store_aggs.is_empty() {
                    None
                } else {
                    Some(&self.store_aggs)
                },
                config,
                catalog,
            )
            .and_then(|records| {
                if records.is_empty() {
                    Ok(())
                } else {
                    sink.apply(&records)
                }
            });
        if let Err(error) = result {
            self.counters.apply_failures = self.counters.apply_failures.saturating_add(1);
            let state = self.retries.entry(key).or_insert(RetryState {
                failures: 0,
                next_attempt: now,
            });
            state.failures = state.failures.saturating_add(1);
            state.next_attempt =
                now + Duration::from_secs(1u64 << state.failures.saturating_sub(1).min(5));
            // Cap at 30 seconds; the first cause is logged exactly once for this window.
            state.next_attempt = state.next_attempt.min(now + Duration::from_secs(30));
            if self.logged_failures.insert(key) {
                eprintln!("Ingest window apply/emit failed: store={} vessel={} bucket={}; retained for retry: {}",
                    self.store_name, key.0, key.1, error);
            }
            return Err(error);
        }
        // Only the sink acknowledgement permits removal.
        let window = self.open_buckets.remove(&key).expect("acknowledged window");
        self.retained_bytes = self
            .retained_bytes
            .saturating_sub(self.window_bytes.remove(&key).unwrap_or(0));
        if !window.event_counts.is_empty() {
            Self::remember_event_window(
                &mut self.closed_event_windows,
                &mut self.closed_event_buckets,
                key,
                window,
            );
        }
        self.retries.remove(&key);
        self.closed_buckets.insert(key);
        if !was_closed && self.closed_observer.is_some() {
            self.pending_closes.insert(key);
        }
        Ok(true)
    }

    /// Retry failed windows whose exponential (1..30 second) delay has elapsed.
    /// An explicit monotonic time makes the maintenance schedule deterministic in tests.
    pub fn retry_pending(
        &mut self,
        now: Instant,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<usize> {
        let keys: Vec<_> = self
            .open_buckets
            .keys()
            .filter(|key| self.retries.contains_key(key) || self.counters.ingest_blocked)
            .copied()
            .collect();
        let mut count = 0;
        for key in keys {
            if !self.close_window(key, now, config, catalog, sink)? {
                break;
            }
            count += 1;
        }
        self.publish_closes(sink, false)?;
        if self.counters.ingest_blocked && self.open_buckets.is_empty() {
            self.counters.ingest_blocked = false;
            eprintln!(
                "Ingest resumed: store={} retained windows drained",
                self.store_name
            );
        }
        Ok(count)
    }

    fn remember_event_window(
        windows: &mut BTreeMap<(VesselOrd, BucketIx), BucketWindow>,
        buckets: &mut BTreeSet<(VesselOrd, BucketIx)>,
        key: (VesselOrd, BucketIx),
        window: BucketWindow,
    ) {
        buckets.insert(key);
        windows.insert(key, window);
        // Bound retained late-event state; old history must be repaired by backfill.
        if windows.len() > 128 {
            let oldest = *windows.keys().min_by_key(|(_, bucket)| bucket).unwrap();
            windows.remove(&oldest);
        }
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
        if self.counters.ingest_blocked {
            self.counters.samples_rejected_blocked =
                self.counters.samples_rejected_blocked.saturating_add(1);
            return Err(ti_contracts::Error::InvalidInput(format!(
                "ingest blocked for store {}: retained window limit reached",
                self.store_name
            )));
        }
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

        if crate::counters::nonfinite(&value) {
            self.counters.samples_dropped_nonfinite =
                self.counters.samples_dropped_nonfinite.saturating_add(1);
            return Ok(());
        }
        let bucket_ix = match bucket_of(ts, self.width_seconds) {
            Ok(bucket) => bucket,
            Err(error) => {
                self.counters.samples_dropped_late =
                    self.counters.samples_dropped_late.saturating_add(1);
                return Err(error);
            }
        };
        if ts > self.max_event_time {
            self.max_event_time = ts;
        }

        let policy = self
            .open_buckets
            .get(&(vessel, bucket_ix))
            .or_else(|| self.closed_event_windows.get(&(vessel, bucket_ix)))
            .and_then(|window| window.count_paths.clone())
            .unwrap_or_else(|| {
                if self.closed_buckets.contains(&(vessel, bucket_ix)) {
                    BTreeSet::new()
                } else {
                    config.ingest.count_paths.iter().cloned().collect()
                }
            });
        self.classifier.set_count_paths(&policy);

        // Check sticky classification
        let (eff_path, kind) = match self.classifier.classify(context, path, &value) {
            Some(res) => res,
            None => return Ok(()),
        };

        if self.closed_event_buckets.contains(&(vessel, bucket_ix))
            && !self.closed_event_windows.contains_key(&(vessel, bucket_ix))
            && !self.open_buckets.contains_key(&(vessel, bucket_ix))
        {
            self.counters.samples_dropped_late =
                self.counters.samples_dropped_late.saturating_add(1);
            return Err(ti_contracts::Error::InvalidInput("late event bucket is outside the 128-window repair cache; repair with historical backfill".into()));
        }
        self.reserve_sample((vessel, bucket_ix), &eff_path, source, &value)?;

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
            let mut late_window = self
                .open_buckets
                .get(&(vessel, bucket_ix))
                .cloned()
                .or_else(|| self.closed_event_windows.get(&(vessel, bucket_ix)).cloned())
                .unwrap_or_else(|| BucketWindow {
                    count_paths: Some(BTreeSet::new()),
                    ..BucketWindow::default()
                });
            self.counters.add(populate_window(
                &mut late_window,
                &eff_path,
                &value,
                source,
                ts,
                &kind,
                config,
            )?);
            for event in derived_events {
                populate_derived_event(&mut late_window, event, source);
            }
            // Retain late repairs too: an apply failure must not lose this new sample.
            self.open_buckets.insert((vessel, bucket_ix), late_window);
            self.close_window((vessel, bucket_ix), Instant::now(), config, catalog, sink)?;
        } else {
            let window = self.open_buckets.entry((vessel, bucket_ix)).or_default();
            self.counters.add(populate_window(
                window, &eff_path, &value, source, ts, &kind, config,
            )?);
            for event in derived_events {
                populate_derived_event(window, event, source);
            }
            self.close_ready_buckets(config, catalog, sink)?;
        }

        Ok(())
    }

    /// Close aged buckets. Failed windows remain intact until an acknowledged retry.
    pub fn advance_watermark(
        &mut self,
        watermark: i64,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<usize> {
        let keys: Vec<_> = self
            .open_buckets
            .keys()
            .filter(|(_, bucket)| {
                EPOCH + (i64::from(*bucket) + 1) * self.width_seconds as i64 <= watermark
            })
            .copied()
            .collect();
        let now = Instant::now();
        let mut count = 0;
        for key in keys {
            if !self.close_window(key, now, config, catalog, sink)? {
                break;
            }
            count += 1;
        }
        self.publish_closes(sink, false)?;
        Ok(count)
    }

    pub fn close_ready_buckets(
        &mut self,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<usize> {
        self.advance_watermark(self.watermark(), config, catalog, sink)
    }

    /// Explicit flush respects pending backoff; callers check pending_retry_count.
    pub fn flush_all(
        &mut self,
        config: &TiConfig,
        catalog: &dyn Catalog,
        sink: &mut dyn ShardSink,
    ) -> Result<usize> {
        let keys: Vec<_> = self.open_buckets.keys().copied().collect();
        let now = Instant::now();
        let mut count = 0;
        for key in keys {
            if !self.close_window(key, now, config, catalog, sink)? {
                break;
            }
            count += 1;
        }
        self.publish_closes(sink, true)?;
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

pub(crate) fn populate_window(
    window: &mut BucketWindow,
    path: &str,
    value: &NormalizedValue,
    source: &str,
    ts: i64,
    kind: &ti_contracts::FieldKind,
    config: &TiConfig,
) -> Result<SampleCounters> {
    let mut counters = SampleCounters::default();
    if crate::counters::nonfinite(value) {
        counters.samples_dropped_nonfinite = 1;
        return Ok(counters);
    }
    let count_paths = window
        .count_paths
        .get_or_insert_with(|| config.ingest.count_paths.iter().cloned().collect());
    let count_path = count_paths.contains(path);
    if count_path {
        if !matches!(value, NormalizedValue::Double(v) if v.is_finite()) {
            return Ok(counters);
        }
        let priority = config
            .source_priorities
            .get(path)
            .and_then(|sources| sources.iter().position(|s| s == source))
            .unwrap_or(usize::MAX);
        let before = window
            .event_samples
            .get(path)
            .map_or(0, |acc| acc.count)
            .saturating_sub(window.event_counts.get(path).map_or(0, |acc| acc.count));
        window.add_event_sample(path, source, priority)?;
        let after = window
            .event_samples
            .get(path)
            .map_or(0, |acc| acc.count)
            .saturating_sub(window.event_counts.get(path).map_or(0, |acc| acc.count));
        counters.samples_rejected_source = after.saturating_sub(before);
    }
    match value {
        NormalizedValue::Double(d) => {
            let scale = match kind {
                ti_contracts::FieldKind::Bsi { scale } => *scale,
                _ => 3,
            };
            if ti_contracts::to_fixed(*d, scale).is_err() {
                counters.samples_skipped_magnitude = 1;
                let skipped = window
                    .skipped_magnitudes
                    .entry(path.to_string())
                    .or_default();
                *skipped = skipped
                    .checked_add(1)
                    .filter(|v| *v <= i64::MAX as u64)
                    .ok_or(ti_contracts::Error::Overflow("skipped magnitudes"))?;
            } else {
                window.add_numeric(path, *d, scale, source, ts);
            }
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
    Ok(counters)
}

fn populate_derived_event(window: &mut BucketWindow, event: DerivedEvent, source: &str) {
    match event {
        DerivedEvent::Transition { output }
        | DerivedEvent::RisingEdge { output }
        | DerivedEvent::NotificationRaise { output } => {
            if !window
                .count_paths
                .as_ref()
                .is_some_and(|paths| paths.contains(&output))
            {
                window.add_count(&output, 1, source);
            }
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

    /// Counts store-local sample contributions; fanout may count a sample in several stores.
    pub fn counters(&self) -> SampleCounters {
        let mut counters = SampleCounters::default();
        for bucketer in self.bucketers.values() {
            counters.add(bucketer.counters());
        }
        counters
    }
    pub fn pending_retry_count(&self) -> usize {
        self.bucketers
            .values()
            .map(WatermarkBucketer::pending_retry_count)
            .sum()
    }
    pub fn retry_pending(
        &mut self,
        now: Instant,
        config: &TiConfig,
        catalogs: &BTreeMap<String, &dyn Catalog>,
        sinks: &mut BTreeMap<String, &mut dyn ShardSink>,
    ) -> Result<usize> {
        let mut total = 0;
        let mut first_error = None;
        for (name, bucketer) in &mut self.bucketers {
            if let (Some(catalog), Some(sink)) = (catalogs.get(name), sinks.get_mut(name)) {
                match bucketer.retry_pending(now, config, *catalog, *sink) {
                    Ok(count) => total += count,
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
        }
        first_error.map_or(Ok(total), Err)
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
        let mut first_error = None;
        for (name, bucketer) in &mut self.bucketers {
            if bucketer.is_path_allowed(path, config) {
                if let (Some(catalog), Some(sink)) = (catalogs.get(name), sinks.get_mut(name)) {
                    let result = bucketer.ingest_point(
                        context,
                        path,
                        source,
                        ts,
                        value.clone(),
                        config,
                        *catalog,
                        *sink,
                    );
                    match result {
                        Ok(()) => matched += 1,
                        Err(error) => {
                            if first_error.is_none() {
                                first_error = Some(error);
                            }
                        }
                    }
                }
            }
        }
        first_error.map_or(Ok(matched), Err)
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
        let mut first_error = None;
        for (name, bucketer) in &mut self.bucketers {
            if let (Some(catalog), Some(sink)) = (catalogs.get(name), sinks.get_mut(name)) {
                match bucketer.flush_all(config, *catalog, *sink) {
                    Ok(count) => total += count,
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
        }
        first_error.map_or(Ok(total), Err)
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
        let mut first_error = None;
        for (name, bucketer) in &mut self.bucketers {
            if let (Some(catalog), Some(sink)) = (catalogs.get(name), sinks.get_mut(name)) {
                match bucketer.advance_watermark(watermark, config, *catalog, *sink) {
                    Ok(count) => total += count,
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
        }
        first_error.map_or(Ok(total), Err)
    }

    /// Close ready buckets across all stores whose bucket_end <= that store's watermark.
    pub fn close_ready_buckets(
        &mut self,
        config: &TiConfig,
        catalogs: &BTreeMap<String, &dyn Catalog>,
        sinks: &mut BTreeMap<String, &mut dyn ShardSink>,
    ) -> Result<usize> {
        let mut total = 0;
        let mut first_error = None;
        for (name, bucketer) in &mut self.bucketers {
            if let (Some(catalog), Some(sink)) = (catalogs.get(name), sinks.get_mut(name)) {
                match bucketer.close_ready_buckets(config, *catalog, *sink) {
                    Ok(count) => total += count,
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
        }
        first_error.map_or(Ok(total), Err)
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
                    let norm_scope = crate::profile::Scope::new(3);
                    let norm_points = crate::normalize::normalize_point_ref(
                        raw,
                        &config.allow_paths,
                        &config.deny_paths,
                    );
                    drop(norm_scope);
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

                        let classified = {
                            let _scope = crate::profile::Scope::new(4);
                            bucketer
                                .classifier_mut()
                                .classify(&p.context, &p.path, &p.value)
                        };
                        let (eff_path, kind) = match classified {
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
