//! Lifetime counters for live ingestion (not persisted shard metadata).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct SampleCounters {
    pub samples_dropped_late: u64,
    pub samples_dropped_nonfinite: u64,
    pub samples_skipped_magnitude: u64,
    pub samples_rejected_source: u64,
    pub apply_failures: u64,
    pub apply_retries: u64,
    pub samples_rejected_blocked: u64,
    pub ingest_blocked: bool,
}
impl SampleCounters {
    pub fn add(&mut self, other: Self) {
        self.samples_dropped_late = self.samples_dropped_late.saturating_add(other.samples_dropped_late);
        self.samples_dropped_nonfinite = self.samples_dropped_nonfinite.saturating_add(other.samples_dropped_nonfinite);
        self.samples_skipped_magnitude = self.samples_skipped_magnitude.saturating_add(other.samples_skipped_magnitude);
        self.samples_rejected_source = self.samples_rejected_source.saturating_add(other.samples_rejected_source);
        self.apply_failures = self.apply_failures.saturating_add(other.apply_failures);
        self.apply_retries = self.apply_retries.saturating_add(other.apply_retries);
        self.samples_rejected_blocked = self.samples_rejected_blocked.saturating_add(other.samples_rejected_blocked);
        self.ingest_blocked |= other.ingest_blocked;
    }
}
pub(crate) fn nonfinite(value: &crate::normalize::NormalizedValue) -> bool {
    match value {
        crate::normalize::NormalizedValue::Double(v) => !v.is_finite(),
        crate::normalize::NormalizedValue::Geo { lat, lon } => !lat.is_finite() || !lon.is_finite(),
        _ => false,
    }
}
