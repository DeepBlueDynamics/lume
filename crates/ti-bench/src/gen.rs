//! Synthetic data generation: models vessel passages, anchoring, dock time, engine
//! on/off, bilge cycles correlated with heel, wind fronts, notifications and notes
//! with planted keywords.

use crate::model;
use crate::rng::SplitMix64;
use chrono::{DateTime, TimeZone, Utc};

pub const EPOCH_SECS: i64 = 1_577_836_800; // 2020-01-01T00:00:00Z
pub const BUCKET_W: i64 = 10;

/// Correctness window: 2026-03-01 .. 2026-06-01 (≈92 days).
pub const START_SECS: i64 = 1_772_323_200; // 2026-03-01T00:00:00Z
pub const END_SECS: i64 = 1_780_272_000; // 2026-06-01T00:00:00Z

fn ts_to_dt(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).unwrap()
}

pub fn iso(secs: i64) -> String {
    ts_to_dt(secs).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// UTC day key (year, ordinal) for a timestamp.
pub fn day_key(secs: i64) -> (i32, u32) {
    use chrono::Datelike;
    let d = ts_to_dt(secs).date_naive();
    (d.year(), d.ordinal())
}

#[derive(Clone, Copy, PartialEq)]
enum VesselState {
    Dock,
    Passage,
    Anchored,
}

struct Segment {
    state: VesselState,
    start: i64,
    end: i64,
}

fn build_schedule(seed: &mut SplitMix64, start: i64, end: i64) -> Vec<Segment> {
    let mut segs = Vec::new();
    let mut t = start;
    while t < end {
        let (state, dur) = match seed.below(3) {
            0 => (VesselState::Passage, 86_400 + seed.below(3) as i64 * 86_400),
            1 => (
                VesselState::Anchored,
                43_200 + seed.below(4) as i64 * 43_200,
            ),
            _ => (VesselState::Dock, 43_200 + seed.below(4) as i64 * 43_200),
        };
        let e = (t + dur).min(end);
        segs.push(Segment {
            state,
            start: t,
            end: e,
        });
        t = e;
    }
    segs
}

fn state_at(segs: &[Segment], t: i64) -> VesselState {
    segs.iter()
        .find(|s| t >= s.start && t < s.end)
        .map(|s| s.state)
        .unwrap_or(VesselState::Dock)
}

/// A scalar sample (bsi / set / count).
#[derive(Clone)]
pub struct Sample {
    pub context: String,
    pub path: String,
    pub ts_secs: i64,
    pub value: Option<f64>,
    pub value_str: Option<String>,
    pub source_label: String,
    pub source_type: String,
}

/// A position object sample (path = navigation.position).
#[derive(Clone)]
pub struct Position {
    pub context: String,
    pub ts_secs: i64,
    pub latitude: f64,
    pub longitude: f64,
    pub source_label: String,
    pub source_type: String,
}

/// An attitude object sample (path = navigation.attitude).
#[derive(Clone)]
pub struct Attitude {
    pub context: String,
    pub ts_secs: i64,
    pub roll: f64,
    pub pitch: f64,
    pub source_label: String,
    pub source_type: String,
}

pub struct Doc {
    pub context: String,
    pub kind: String,
    pub ts_start: i64,
    pub ts_end: i64,
    pub title: String,
    pub body: String,
}

impl Clone for Doc {
    fn clone(&self) -> Self {
        Doc {
            context: self.context.clone(),
            kind: self.kind.clone(),
            ts_start: self.ts_start,
            ts_end: self.ts_end,
            title: self.title.clone(),
            body: self.body.clone(),
        }
    }
}

pub use crate::model::is_hr_path;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GenConfig {
    pub hz: f64,
    pub per_path_override: bool,
}

impl Default for GenConfig {
    fn default() -> Self {
        Self {
            hz: 0.1,
            per_path_override: false,
        }
    }
}

impl GenConfig {
    pub fn is_default(&self) -> bool {
        (self.hz - 0.1).abs() < 1e-6 && !self.per_path_override
    }
}

pub struct Generated {
    pub samples: Vec<Sample>,
    pub positions: Vec<Position>,
    pub attitudes: Vec<Attitude>,
    pub docs: Vec<Doc>,
}

/// Collecting wrapper over [`stream`] for small windows (tests only).
pub fn generate(seed: u64, n_vessels: usize, start: i64, end: i64) -> Generated {
    generate_with_config(seed, n_vessels, start, end, &GenConfig::default())
}

/// Collecting wrapper over [`stream_with_config`] for small windows.
pub fn generate_with_config(
    seed: u64,
    n_vessels: usize,
    start: i64,
    end: i64,
    config: &GenConfig,
) -> Generated {
    let mut samples = Vec::new();
    let mut positions = Vec::new();
    let mut attitudes = Vec::new();
    let mut docs = Vec::new();
    stream_with_config(
        seed,
        n_vessels,
        start,
        end,
        config,
        &mut |_ctx, _day, s, p, a| {
            samples.extend_from_slice(s);
            positions.extend_from_slice(p);
            attitudes.extend_from_slice(a);
        },
        &mut |_ctx, d| docs.extend_from_slice(d),
    );
    Generated {
        samples,
        positions,
        attitudes,
        docs,
    }
}

/// Callback receiving one (context, UTC day) chunk of generated samples.
type DaySink<'a> = dyn FnMut(&str, i64, &[Sample], &[Position], &[Attitude]) + 'a;
/// Callback receiving one vessel's generated docs.
type DocsSink<'a> = dyn FnMut(&str, &[Doc]) + 'a;

/// Stream generation, flushing per (vessel, UTC day) so the correctness set never
/// exceeds a day's memory. The RNG consumption order is identical to a monolithic
/// run, so output is byte-for-byte the same as [`generate`] for the same seed.
pub fn stream(
    seed: u64,
    n_vessels: usize,
    start: i64,
    end: i64,
    on_day: &mut DaySink,
    on_docs: &mut DocsSink,
) {
    stream_with_config(
        seed,
        n_vessels,
        start,
        end,
        &GenConfig::default(),
        on_day,
        on_docs,
    );
}

pub fn stream_with_config(
    seed: u64,
    n_vessels: usize,
    start: i64,
    end: i64,
    config: &GenConfig,
    on_day: &mut DaySink,
    on_docs: &mut DocsSink,
) {
    for v in 0..n_vessels {
        let context = model::VESSEL_URNS[v.min(model::VESSEL_URNS.len() - 1)].to_string();
        let mut vseed = SplitMix64::new(
            seed.wrapping_add(v as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15),
        );
        let segs = build_schedule(&mut vseed, start, end);
        gen_vessel(
            &mut vseed, &context, &segs, v, start, end, config, on_day, on_docs,
        );
    }
}

fn day_start(ts: i64) -> i64 {
    ts - ts.rem_euclid(86_400)
}

#[allow(clippy::too_many_arguments)]
fn gen_vessel(
    rng: &mut SplitMix64,
    context: &str,
    segs: &[Segment],
    vessel_idx: usize,
    start: i64,
    end: i64,
    config: &GenConfig,
    on_day: &mut DaySink,
    on_docs: &mut DocsSink,
) {
    let src = model::SOURCE_LABEL.to_string();
    let stype = "NMEA2000".to_string();

    let mut lat = 36.0 + (vessel_idx as f64) * 0.2;
    let mut lon = -122.0 + (vessel_idx as f64) * 0.3;

    let mut samples: Vec<Sample> = Vec::new();
    let mut positions: Vec<Position> = Vec::new();
    let mut attitudes: Vec<Attitude> = Vec::new();
    let mut cur_day = day_start(start);

    if config.is_default() {
        let mut t = start;
        while t < end {
            let d0 = day_start(t);
            if d0 != cur_day {
                on_day(context, cur_day, &samples, &positions, &attitudes);
                samples.clear();
                positions.clear();
                attitudes.clear();
                cur_day = d0;
            }

            let state = state_at(segs, t);
            let is_nav = state == VesselState::Passage;

            // numeric paths
            for p in model::NUMERIC_PATHS {
                let val = numeric_value(rng, p, state, is_nav, t);
                samples.push(Sample {
                    context: context.to_string(),
                    path: p.to_string(),
                    ts_secs: t,
                    value: Some(val),
                    value_str: None,
                    source_label: src.clone(),
                    source_type: stype.clone(),
                });
            }

            // position object
            if state == VesselState::Passage {
                lat += rng.range(-0.001, 0.001);
                lon += rng.range(-0.001, 0.001);
            }
            positions.push(Position {
                context: context.to_string(),
                ts_secs: t,
                latitude: lat,
                longitude: lon,
                source_label: src.clone(),
                source_type: stype.clone(),
            });

            // attitude object
            let roll = if is_nav {
                rng.range(-0.1, 0.1)
            } else {
                rng.range(-0.35, 0.35)
            };
            let pitch = if is_nav { rng.range(-0.05, 0.05) } else { 0.0 };
            attitudes.push(Attitude {
                context: context.to_string(),
                ts_secs: t,
                roll,
                pitch,
                source_label: src.clone(),
                source_type: stype.clone(),
            });

            // set fields
            let (main_st, port_st, stbd_st, nav_st) = state_strings(state, rng, vessel_idx);
            for (p, v) in [
                ("propulsion.main.state", main_st),
                ("propulsion.port.state", port_st),
                ("propulsion.starboard.state", stbd_st),
                ("navigation.state", nav_st),
            ] {
                samples.push(Sample {
                    context: context.to_string(),
                    path: p.to_string(),
                    ts_secs: t,
                    value: None,
                    value_str: Some(v.to_string()),
                    source_label: src.clone(),
                    source_type: stype.clone(),
                });
            }

            // bilge pump cycles, correlated with heel
            let heel = if is_nav { 0.1 } else { 0.35 };
            let bilge_prob = if rng.next_f64() < heel * 0.2 {
                0.9
            } else {
                0.01
            };
            if rng.next_f64() < bilge_prob {
                let cycles = 1 + rng.below(4) as i64;
                for _ in 0..cycles {
                    samples.push(Sample {
                        context: context.to_string(),
                        path: "electrical.bilge.pumpCycles".to_string(),
                        ts_secs: t,
                        value: Some(1.0),
                        value_str: None,
                        source_label: src.clone(),
                        source_type: stype.clone(),
                    });
                }
            }

            t += BUCKET_W;
        }
    } else {
        let step_secs = if config.per_path_override {
            1i64
        } else {
            (1.0 / config.hz).round().max(1.0) as i64
        };
        let mut t = start;
        while t < end {
            let d0 = day_start(t);
            if d0 != cur_day {
                on_day(context, cur_day, &samples, &positions, &attitudes);
                samples.clear();
                positions.clear();
                attitudes.clear();
                cur_day = d0;
            }

            let state = state_at(segs, t);
            let is_nav = state == VesselState::Passage;
            let is_10s = (t - start) % 10 == 0;

            // numeric paths
            for p in model::NUMERIC_PATHS {
                let is_hr = is_hr_path(p);
                if is_hr || (!config.per_path_override) || is_10s {
                    let val = numeric_value(rng, p, state, is_nav, t);
                    samples.push(Sample {
                        context: context.to_string(),
                        path: p.to_string(),
                        ts_secs: t,
                        value: Some(val),
                        value_str: None,
                        source_label: src.clone(),
                        source_type: stype.clone(),
                    });
                }
            }

            // position object (navigation.position is HR)
            if is_hr_path(model::POSITION_PATH) || (!config.per_path_override) || is_10s {
                if state == VesselState::Passage {
                    lat += rng.range(-0.001, 0.001);
                    lon += rng.range(-0.001, 0.001);
                }
                positions.push(Position {
                    context: context.to_string(),
                    ts_secs: t,
                    latitude: lat,
                    longitude: lon,
                    source_label: src.clone(),
                    source_type: stype.clone(),
                });
            }

            // attitude object (navigation.attitude is HR)
            if is_hr_path(model::ATTITUDE_PATH) || (!config.per_path_override) || is_10s {
                let roll = if is_nav {
                    rng.range(-0.1, 0.1)
                } else {
                    rng.range(-0.35, 0.35)
                };
                let pitch = if is_nav { rng.range(-0.05, 0.05) } else { 0.0 };
                attitudes.push(Attitude {
                    context: context.to_string(),
                    ts_secs: t,
                    roll,
                    pitch,
                    source_label: src.clone(),
                    source_type: stype.clone(),
                });
            }

            // set fields
            let (main_st, port_st, stbd_st, nav_st) = state_strings(state, rng, vessel_idx);
            for (p, v) in [
                ("propulsion.main.state", main_st),
                ("propulsion.port.state", port_st),
                ("propulsion.starboard.state", stbd_st),
                ("navigation.state", nav_st),
            ] {
                let is_hr = is_hr_path(p);
                if is_hr || (!config.per_path_override) || is_10s {
                    samples.push(Sample {
                        context: context.to_string(),
                        path: p.to_string(),
                        ts_secs: t,
                        value: None,
                        value_str: Some(v.to_string()),
                        source_label: src.clone(),
                        source_type: stype.clone(),
                    });
                }
            }

            // bilge pump cycles, correlated with heel (non-HR)
            if (!config.per_path_override) || is_10s {
                let heel = if is_nav { 0.1 } else { 0.35 };
                let bilge_prob = if rng.next_f64() < heel * 0.2 {
                    0.9
                } else {
                    0.01
                };
                if rng.next_f64() < bilge_prob {
                    let cycles = 1 + rng.below(4) as i64;
                    for _ in 0..cycles {
                        samples.push(Sample {
                            context: context.to_string(),
                            path: "electrical.bilge.pumpCycles".to_string(),
                            ts_secs: t,
                            value: Some(1.0),
                            value_str: None,
                            source_label: src.clone(),
                            source_type: stype.clone(),
                        });
                    }
                }
            }

            t += step_secs;
        }
    }
    on_day(context, cur_day, &samples, &positions, &attitudes);

    // docs (one batch per vessel)
    let mut docs = Vec::new();
    gen_docs(rng, &mut docs, context, start, end);
    on_docs(context, &docs);
}

fn numeric_value(
    rng: &mut SplitMix64,
    path: &str,
    state: VesselState,
    is_nav: bool,
    t: i64,
) -> f64 {
    match path {
        "environment.wind.speedTrue" => {
            if state == VesselState::Passage {
                rng.gauss(8.0, 3.0, 0.0, 25.0)
            } else {
                rng.gauss(3.0, 2.0, 0.0, 15.0)
            }
        }
        "environment.wind.speedApparent" => {
            let true_w = if is_nav {
                rng.gauss(8.0, 3.0, 0.0, 25.0)
            } else {
                rng.gauss(3.0, 2.0, 0.0, 15.0)
            };
            true_w + rng.range(-4.0, 4.0)
        }
        "navigation.speedOverGround" => {
            if is_nav {
                rng.gauss(4.0, 1.5, 0.0, 9.0)
            } else {
                rng.gauss(0.1, 0.05, 0.0, 0.5)
            }
        }
        "propulsion.port.motorPower" | "propulsion.starboard.motorPower" => {
            if state == VesselState::Passage {
                rng.gauss(1200.0, 400.0, 0.0, 2500.0)
            } else {
                0.0
            }
        }
        "propulsion.port.revolutions" | "propulsion.starboard.revolutions" => {
            if is_nav {
                rng.gauss(40.0, 8.0, 0.0, 80.0)
            } else {
                0.0
            }
        }
        "propulsion.port.temperature" | "propulsion.starboard.temperature" => {
            if is_nav {
                rng.gauss(85.0, 5.0, 60.0, 110.0)
            } else {
                rng.gauss(40.0, 3.0, 25.0, 60.0)
            }
        }
        "electrical.batteries.house.voltage" => {
            if state == VesselState::Dock {
                rng.gauss(27.0, 0.3, 24.0, 28.8)
            } else {
                rng.gauss(25.5, 0.5, 22.0, 27.0)
            }
        }
        "electrical.batteries.house.current" => {
            if state == VesselState::Passage {
                rng.gauss(-15.0, 8.0, -40.0, 30.0)
            } else {
                rng.gauss(2.0, 1.0, 0.0, 20.0)
            }
        }
        "electrical.batteries.house.stateOfCharge" => {
            if state == VesselState::Dock {
                rng.gauss(0.9, 0.05, 0.2, 1.0)
            } else {
                rng.gauss(0.7, 0.15, 0.1, 1.0)
            }
        }
        "electrical.solar.house.panelPower" => {
            let h = ((t - START_SECS) % 86_400) as f64 / 3600.0;
            let daylight = if (6.0..18.0).contains(&h) {
                ((h - 6.0) * std::f64::consts::PI / 12.0).sin()
            } else {
                0.0
            };
            rng.gauss(1500.0 * daylight, 100.0, 0.0, 3750.0)
        }
        "environment.depth.belowTransducer" => {
            if state == VesselState::Anchored {
                rng.gauss(3.0, 1.0, 1.0, 15.0)
            } else if is_nav {
                rng.gauss(20.0, 8.0, 2.0, 60.0)
            } else {
                rng.gauss(8.0, 3.0, 2.0, 30.0)
            }
        }
        "environment.outside.temperature" => rng.gauss(18.0, 3.0, 5.0, 32.0),
        "environment.outside.pressure" => rng.gauss(101_300.0, 500.0, 98_000.0, 104_000.0),
        "tanks.freshWater.port.currentLevel" | "tanks.freshWater.starboard.currentLevel" => {
            let frac = (t - START_SECS) as f64 / (END_SECS - START_SECS) as f64;
            (1.0 - frac * 0.6).max(0.1) + rng.range(-0.01, 0.01)
        }
        _ => 0.0,
    }
}

fn state_strings(
    state: VesselState,
    rng: &mut SplitMix64,
    vessel_idx: usize,
) -> (&'static str, &'static str, &'static str, &'static str) {
    let twin = vessel_idx == 0;
    match state {
        VesselState::Passage => {
            let main = if rng.next_f64() < 0.9 {
                "started"
            } else {
                "idle"
            };
            let port = if rng.next_f64() < 0.9 {
                "started"
            } else {
                "idle"
            };
            let stbd = if twin {
                if rng.next_f64() < 0.9 {
                    "started"
                } else {
                    "idle"
                }
            } else {
                "off"
            };
            (main, port, stbd, "sailing")
        }
        VesselState::Anchored => ("off", "off", "off", "anchored"),
        VesselState::Dock => ("off", "off", "off", "moored"),
    }
}

fn gen_docs(rng: &mut SplitMix64, docs: &mut Vec<Doc>, context: &str, start: i64, end: i64) {
    let mut t = start;
    let mut i = 0u64;
    while t < end {
        if i.is_multiple_of(2) {
            let kw = model::PLANTED_NOTES[(i / 2) as usize % model::PLANTED_NOTES.len()];
            docs.push(Doc {
                context: context.to_string(),
                kind: "notes".to_string(),
                ts_start: t,
                ts_end: t + 60,
                title: format!("Note {}", i),
                body: format!("Routine log. {} observed near the engine room.", kw),
            });
        }
        if i.is_multiple_of(4) {
            let kw = model::PLANTED_LOGBOOK[0];
            docs.push(Doc {
                context: context.to_string(),
                kind: "logbook".to_string(),
                ts_start: t,
                ts_end: t + 3600,
                title: format!("Log {}", i),
                body: format!("Logbook entry: reached {} for the night.", kw),
            });
        }
        if i.is_multiple_of(8) {
            let kw = model::PLANTED_ALERTS[0];
            docs.push(Doc {
                context: context.to_string(),
                kind: "alerts".to_string(),
                ts_start: t,
                ts_end: t + 120,
                title: format!("Alert {}", i),
                body: format!("{} condition detected.", kw),
            });
        }
        t += 21_600;
        i += 1;
        let _ = rng.next_u64();
    }
}
