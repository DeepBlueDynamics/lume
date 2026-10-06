//! Opt-in backfill diagnostics; disabled builds compile timers away.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
pub const NAMES: [&str; 8] = ["hash", "decode", "extract", "normalize", "classify", "bucketer", "apply", "flush"];
static TOTAL: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
pub struct Scope { stage: usize, start: Option<Instant> }
impl Scope {
    #[inline(always)]
    pub fn new(stage: usize) -> Self {
        Self { stage, start: cfg!(feature = "backfill-profile").then(Instant::now) }
    }
}
impl Drop for Scope {
    #[inline(always)]
    fn drop(&mut self) {
        if let Some(start) = self.start {
            TOTAL[self.stage].fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
    }
}
pub fn reset() { for total in &TOTAL { total.store(0, Ordering::Relaxed); } }
pub fn seconds() -> [f64; 8] { std::array::from_fn(|i| TOTAL[i].load(Ordering::Relaxed) as f64 / 1e9) }
