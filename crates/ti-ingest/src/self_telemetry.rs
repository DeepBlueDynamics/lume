//! Lume's own operational metrics, ingested into the store it writes.
//!
//! Every interval the live ingest service records its counters, lag, flush cost and
//! process memory/CPU as the entity `lume.urn:host:<hostname>`, so the same SQL that
//! answers fleet questions answers "how is Lume doing?":
//!
//! ```sql
//! SELECT ts, "lume.ingest.valuesPerSecond@mean", "lume.process.rssBytes@max"
//! FROM telemetry WHERE vessel LIKE 'lume.urn:%' ORDER BY ts DESC LIMIT 10
//! ```
//!
//! Set `LUME_TI_SELF_TELEMETRY=0` (or `off`) to disable, or to a number of seconds to
//! change the default 10 s interval.

use std::time::{Duration, Instant};

use crate::counters::SampleCounters;

/// Source label on every self-telemetry sample.
pub const SOURCE: &str = "lume.self";

/// Self samples carry wall-clock time and share the event-time watermark, so they are
/// only emitted while the incoming data is this close to the wall clock. Replaying old
/// data must not have its buckets closed early by a "now" sample.
pub const LIVE_WINDOW_SECONDS: i64 = 300;

/// Counters the ingest service hands over for one sample.
#[derive(Debug, Clone, Copy, Default)]
pub struct SelfStats {
    pub records_ingested: u64,
    pub reconnects: u64,
    pub lag_seconds: Option<f64>,
    pub last_flush: Duration,
    pub counters: SampleCounters,
}

pub struct SelfTelemetry {
    context: String,
    interval: Duration,
    last: Option<(Instant, u64, Option<u64>)>,
}

impl SelfTelemetry {
    /// Enabled unless `LUME_TI_SELF_TELEMETRY` is `0`, `off` or `false`.
    pub fn from_env() -> Option<Self> {
        let setting = std::env::var("LUME_TI_SELF_TELEMETRY").unwrap_or_default();
        let setting = setting.trim().to_ascii_lowercase();
        if matches!(setting.as_str(), "0" | "off" | "false" | "no") {
            return None;
        }
        let seconds = setting.parse::<u64>().ok().filter(|s| *s > 0).unwrap_or(10);
        Some(Self::new(&hostname(), Duration::from_secs(seconds)))
    }

    pub fn new(host: &str, interval: Duration) -> Self {
        Self {
            context: format!("lume.urn:host:{}", sanitize(host)),
            interval,
            last: None,
        }
    }

    /// The entity URN the samples are recorded under.
    pub fn context(&self) -> &str {
        &self.context
    }

    pub fn due(&self, now: Instant) -> bool {
        self.last
            .is_none_or(|(at, _, _)| now.duration_since(at) >= self.interval)
    }

    /// Paths and values for one sample. Rates need a previous sample, so the first call
    /// reports counters only.
    pub fn sample(&mut self, now: Instant, stats: &SelfStats) -> Vec<(&'static str, f64)> {
        let cpu_ticks = process_cpu_ticks();
        let mut out = vec![
            ("lume.ingest.recordsIngested", stats.records_ingested as f64),
            ("lume.ingest.reconnects", stats.reconnects as f64),
            (
                "lume.ingest.lastFlushSeconds",
                stats.last_flush.as_secs_f64(),
            ),
            (
                "lume.ingest.samplesRejectedBlocked",
                stats.counters.samples_rejected_blocked as f64,
            ),
            (
                "lume.ingest.samplesDroppedLate",
                stats.counters.samples_dropped_late as f64,
            ),
            (
                "lume.ingest.applyFailures",
                stats.counters.apply_failures as f64,
            ),
            (
                "lume.ingest.blocked",
                if stats.counters.ingest_blocked {
                    1.0
                } else {
                    0.0
                },
            ),
        ];
        if let Some(lag) = stats.lag_seconds {
            out.push(("lume.ingest.lagSeconds", lag));
        }
        if let Some(rss) = process_rss_bytes() {
            out.push(("lume.process.rssBytes", rss as f64));
        }
        if let Some((at, records, ticks)) = self.last {
            let elapsed = now.duration_since(at).as_secs_f64();
            if elapsed > 0.0 {
                let delta = stats.records_ingested.saturating_sub(records) as f64;
                out.push(("lume.ingest.valuesPerSecond", delta / elapsed));
                if let (Some(before), Some(after)) = (ticks, cpu_ticks) {
                    // Linux reports CPU time in USER_HZ ticks, 100 per second on every arch.
                    let busy = after.saturating_sub(before) as f64 / 100.0;
                    out.push(("lume.process.cpuPercent", 100.0 * busy / elapsed));
                }
            }
        }
        self.last = Some((now, stats.records_ingested, cpu_ticks));
        out
    }
}

/// True when the newest event time is within [`LIVE_WINDOW_SECONDS`] of `now_unix`.
pub fn is_live(max_event_time: i64, now_unix: i64) -> bool {
    max_event_time > ti_contracts::EPOCH && (now_unix - max_event_time).abs() <= LIVE_WINDOW_SECONDS
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

fn sanitize(host: &str) -> String {
    let name: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    if name.trim_matches('-').is_empty() {
        "localhost".into()
    } else {
        name
    }
}

#[cfg(target_os = "linux")]
fn process_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(not(target_os = "linux"))]
fn process_rss_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn process_cpu_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the parenthesised command name; utime and stime are fields 14 and 15.
    let rest = &stat[stat.rfind(')')? + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

#[cfg(not(target_os = "linux"))]
fn process_cpu_ticks() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_is_a_lume_entity_not_a_vessel() {
        let telemetry = SelfTelemetry::new("HaLOS.local", Duration::from_secs(10));
        assert_eq!(telemetry.context(), "lume.urn:host:halos-local");
        assert!(ti_contracts::validate_entity_urn(telemetry.context()).is_ok());
        assert_eq!(
            SelfTelemetry::new("", Duration::from_secs(10)).context(),
            "lume.urn:host:localhost"
        );
    }

    #[test]
    fn first_sample_has_counters_then_rates_follow() {
        let mut telemetry = SelfTelemetry::new("box", Duration::from_secs(10));
        let start = Instant::now();
        assert!(telemetry.due(start));
        let first = telemetry.sample(
            start,
            &SelfStats {
                records_ingested: 1_000,
                ..Default::default()
            },
        );
        assert!(first
            .iter()
            .any(|(p, v)| *p == "lume.ingest.recordsIngested" && *v == 1_000.0));
        assert!(!first
            .iter()
            .any(|(p, _)| *p == "lume.ingest.valuesPerSecond"));
        assert!(!telemetry.due(start + Duration::from_secs(5)));

        let later = start + Duration::from_secs(10);
        assert!(telemetry.due(later));
        let second = telemetry.sample(
            later,
            &SelfStats {
                records_ingested: 201_000,
                ..Default::default()
            },
        );
        let rate = second
            .iter()
            .find(|(p, _)| *p == "lume.ingest.valuesPerSecond")
            .map(|(_, v)| *v)
            .unwrap();
        assert!((rate - 20_000.0).abs() < 1.0, "{rate}");
    }

    #[test]
    fn only_live_data_admits_wall_clock_samples() {
        let now = 1_791_380_000;
        assert!(is_live(now - 2, now));
        assert!(!is_live(now - 3_600, now), "replaying an hour-old stream");
        assert!(!is_live(ti_contracts::EPOCH, now), "no data yet");
    }
}
