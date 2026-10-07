//! Synthetic Signal K delta WebSocket server for load benchmarking.
//!
//! Serves `ws://<bind>:<port>/signalk/v1/stream` with hello and batched deltas
//! at a configurable rate, supporting > 20,000 values/s sustained on a Raspberry Pi.

use crate::rng::SplitMix64;
use chrono::Utc;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    f64::consts::TAU,
    io::ErrorKind,
    net::{SocketAddr, TcpListener},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{
    handshake::server::{Request, Response},
    Message,
};

/// Specification of a realistic Signal K path.
pub struct PathDef {
    pub path: &'static str,
    pub min: f64,
    pub max: f64,
    pub is_int: bool,
}

pub const ALL_PATHS: &[PathDef] = &[
    // Navigation
    PathDef {
        path: "navigation.speedOverGround",
        min: 0.0,
        max: 15.0,
        is_int: false,
    },
    PathDef {
        path: "navigation.speedThroughWater",
        min: 0.0,
        max: 15.0,
        is_int: false,
    },
    PathDef {
        path: "navigation.courseOverGroundTrue",
        min: 0.0,
        max: TAU,
        is_int: false,
    },
    PathDef {
        path: "navigation.courseOverGroundMagnetic",
        min: 0.0,
        max: TAU,
        is_int: false,
    },
    PathDef {
        path: "navigation.headingTrue",
        min: 0.0,
        max: TAU,
        is_int: false,
    },
    PathDef {
        path: "navigation.headingMagnetic",
        min: 0.0,
        max: TAU,
        is_int: false,
    },
    PathDef {
        path: "navigation.rateOfTurn",
        min: -0.15,
        max: 0.15,
        is_int: false,
    },
    PathDef {
        path: "navigation.attitude.roll",
        min: -0.3,
        max: 0.3,
        is_int: false,
    },
    PathDef {
        path: "navigation.attitude.pitch",
        min: -0.15,
        max: 0.15,
        is_int: false,
    },
    PathDef {
        path: "navigation.attitude.yaw",
        min: 0.0,
        max: TAU,
        is_int: false,
    },
    PathDef {
        path: "navigation.magneticVariation",
        min: -0.1,
        max: 0.1,
        is_int: false,
    },
    PathDef {
        path: "navigation.gnss.satellites",
        min: 8.0,
        max: 18.0,
        is_int: true,
    },
    PathDef {
        path: "navigation.gnss.horizontalDilution",
        min: 0.7,
        max: 1.8,
        is_int: false,
    },
    PathDef {
        path: "navigation.trip.log",
        min: 1000.0,
        max: 500000.0,
        is_int: false,
    },
    PathDef {
        path: "navigation.position.latitude",
        min: 37.7,
        max: 37.9,
        is_int: false,
    },
    PathDef {
        path: "navigation.position.longitude",
        min: -122.5,
        max: -122.3,
        is_int: false,
    },
    // Environment
    PathDef {
        path: "environment.wind.speedTrue",
        min: 0.0,
        max: 25.0,
        is_int: false,
    },
    PathDef {
        path: "environment.wind.speedApparent",
        min: 0.0,
        max: 30.0,
        is_int: false,
    },
    PathDef {
        path: "environment.wind.angleTrueWater",
        min: 0.0,
        max: TAU,
        is_int: false,
    },
    PathDef {
        path: "environment.wind.angleApparent",
        min: 0.0,
        max: TAU,
        is_int: false,
    },
    PathDef {
        path: "environment.depth.belowTransducer",
        min: 2.0,
        max: 45.0,
        is_int: false,
    },
    PathDef {
        path: "environment.depth.belowSurface",
        min: 3.0,
        max: 46.0,
        is_int: false,
    },
    PathDef {
        path: "environment.depth.surfaceToTransducer",
        min: 1.0,
        max: 1.0,
        is_int: false,
    },
    PathDef {
        path: "environment.water.temperature",
        min: 283.15,
        max: 298.15,
        is_int: false,
    },
    PathDef {
        path: "environment.outside.temperature",
        min: 280.15,
        max: 303.15,
        is_int: false,
    },
    PathDef {
        path: "environment.outside.pressure",
        min: 99500.0,
        max: 103000.0,
        is_int: false,
    },
    PathDef {
        path: "environment.outside.relativeHumidity",
        min: 0.35,
        max: 0.95,
        is_int: false,
    },
    // Propulsion
    PathDef {
        path: "propulsion.port.revolutions",
        min: 10.0,
        max: 40.0,
        is_int: false,
    },
    PathDef {
        path: "propulsion.port.motorPower",
        min: 200.0,
        max: 6000.0,
        is_int: false,
    },
    PathDef {
        path: "propulsion.port.temperature",
        min: 310.15,
        max: 355.15,
        is_int: false,
    },
    PathDef {
        path: "propulsion.port.coolantTemperature",
        min: 320.15,
        max: 365.15,
        is_int: false,
    },
    PathDef {
        path: "propulsion.port.oilPressure",
        min: 200000.0,
        max: 450000.0,
        is_int: false,
    },
    PathDef {
        path: "propulsion.port.oilTemperature",
        min: 320.15,
        max: 370.15,
        is_int: false,
    },
    PathDef {
        path: "propulsion.starboard.revolutions",
        min: 10.0,
        max: 40.0,
        is_int: false,
    },
    PathDef {
        path: "propulsion.starboard.motorPower",
        min: 200.0,
        max: 6000.0,
        is_int: false,
    },
    PathDef {
        path: "propulsion.starboard.temperature",
        min: 310.15,
        max: 355.15,
        is_int: false,
    },
    PathDef {
        path: "propulsion.starboard.coolantTemperature",
        min: 320.15,
        max: 365.15,
        is_int: false,
    },
    PathDef {
        path: "propulsion.starboard.oilPressure",
        min: 200000.0,
        max: 450000.0,
        is_int: false,
    },
    PathDef {
        path: "propulsion.starboard.oilTemperature",
        min: 320.15,
        max: 370.15,
        is_int: false,
    },
    // Electrical
    PathDef {
        path: "electrical.batteries.house.voltage",
        min: 12.2,
        max: 14.4,
        is_int: false,
    },
    PathDef {
        path: "electrical.batteries.house.current",
        min: -30.0,
        max: 45.0,
        is_int: false,
    },
    PathDef {
        path: "electrical.batteries.house.temperature",
        min: 288.15,
        max: 308.15,
        is_int: false,
    },
    PathDef {
        path: "electrical.batteries.house.stateOfCharge",
        min: 0.65,
        max: 1.0,
        is_int: false,
    },
    PathDef {
        path: "electrical.batteries.starter.voltage",
        min: 12.4,
        max: 14.6,
        is_int: false,
    },
    PathDef {
        path: "electrical.batteries.starter.current",
        min: -5.0,
        max: 10.0,
        is_int: false,
    },
    PathDef {
        path: "electrical.solar.house.panelPower",
        min: 0.0,
        max: 400.0,
        is_int: false,
    },
    PathDef {
        path: "electrical.solar.house.current",
        min: 0.0,
        max: 28.0,
        is_int: false,
    },
    PathDef {
        path: "electrical.inverters.main.ac.voltage",
        min: 220.0,
        max: 240.0,
        is_int: false,
    },
    PathDef {
        path: "electrical.inverters.main.ac.current",
        min: 0.5,
        max: 12.0,
        is_int: false,
    },
    PathDef {
        path: "electrical.inverters.main.ac.frequency",
        min: 49.8,
        max: 50.2,
        is_int: false,
    },
    // Tanks
    PathDef {
        path: "tanks.freshWater.port.currentLevel",
        min: 0.15,
        max: 0.95,
        is_int: false,
    },
    PathDef {
        path: "tanks.freshWater.starboard.currentLevel",
        min: 0.15,
        max: 0.95,
        is_int: false,
    },
    PathDef {
        path: "tanks.fuel.main.currentLevel",
        min: 0.2,
        max: 0.9,
        is_int: false,
    },
    PathDef {
        path: "tanks.wasteWater.black.currentLevel",
        min: 0.05,
        max: 0.8,
        is_int: false,
    },
    // Steering
    PathDef {
        path: "steering.rudderAngle",
        min: -0.6,
        max: 0.6,
        is_int: false,
    },
    PathDef {
        path: "steering.rudderAngleTarget",
        min: -0.6,
        max: 0.6,
        is_int: false,
    },
];

#[derive(Debug, Clone)]
pub struct FeedConfig {
    pub bind: String,
    pub port: u16,
    pub values_per_sec: u64,
    pub vessels: usize,
    pub batch_size: usize,
    pub seed: u64,
    pub duration_sec: Option<u64>,
    pub max_values: Option<u64>,
    pub ramp: Option<Vec<(u64, u64)>>, // (values_per_sec, duration_seconds)
    pub self_urn: String,
}

impl Default for FeedConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".into(),
            port: 3000,
            values_per_sec: 20_000,
            vessels: 21,
            batch_size: 100,
            seed: 42,
            duration_sec: None,
            max_values: None,
            ramp: None,
            self_urn: "urn:mrn:imo:mmsi:367000000".into(),
        }
    }
}

pub fn parse_ramp_spec(s: &str) -> Result<Vec<(u64, u64)>, String> {
    let mut stages = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (rate_str, dur_str) = part.split_once(':').ok_or_else(|| {
            format!("Invalid ramp stage '{part}', expected <rate>:<duration_sec>")
        })?;
        let rate: u64 = rate_str
            .trim()
            .parse()
            .map_err(|e| format!("Invalid ramp rate '{rate_str}': {e}"))?;
        let dur: u64 = dur_str
            .trim()
            .parse()
            .map_err(|e| format!("Invalid ramp duration '{dur_str}': {e}"))?;
        stages.push((rate, dur));
    }
    if stages.is_empty() {
        return Err("Ramp specification must contain at least one stage".into());
    }
    Ok(stages)
}

pub fn parse_args(args: &[String]) -> Result<FeedConfig, String> {
    let mut config = FeedConfig::default();
    let mut i = 2; // skip program name and "sk-feed"
    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                config.bind = args.get(i).ok_or("--bind requires an IP address")?.clone();
            }
            "--port" => {
                i += 1;
                config.port = args
                    .get(i)
                    .ok_or("--port requires a port number")?
                    .parse()
                    .map_err(|e| format!("Invalid --port: {e}"))?;
            }
            "--values-per-sec" | "--rate" => {
                i += 1;
                config.values_per_sec = args
                    .get(i)
                    .ok_or("--values-per-sec requires a number")?
                    .parse()
                    .map_err(|e| format!("Invalid --values-per-sec: {e}"))?;
            }
            "--vessels" => {
                i += 1;
                config.vessels = args
                    .get(i)
                    .ok_or("--vessels requires a number")?
                    .parse()
                    .map_err(|e| format!("Invalid --vessels: {e}"))?;
                if config.vessels == 0 {
                    config.vessels = 1;
                }
            }
            "--batch-size" => {
                i += 1;
                config.batch_size = args
                    .get(i)
                    .ok_or("--batch-size requires a number")?
                    .parse()
                    .map_err(|e| format!("Invalid --batch-size: {e}"))?;
                if config.batch_size == 0 {
                    config.batch_size = 1;
                }
            }
            "--seed" => {
                i += 1;
                config.seed = args
                    .get(i)
                    .ok_or("--seed requires a number")?
                    .parse()
                    .map_err(|e| format!("Invalid --seed: {e}"))?;
            }
            "--duration" => {
                i += 1;
                config.duration_sec = Some(
                    args.get(i)
                        .ok_or("--duration requires seconds")?
                        .parse()
                        .map_err(|e| format!("Invalid --duration: {e}"))?,
                );
            }
            "--max-values" => {
                i += 1;
                config.max_values = Some(
                    args.get(i)
                        .ok_or("--max-values requires a number")?
                        .parse()
                        .map_err(|e| format!("Invalid --max-values: {e}"))?,
                );
            }
            "--ramp" => {
                i += 1;
                let spec = args
                    .get(i)
                    .ok_or("--ramp requires a specification string")?;
                config.ramp = Some(parse_ramp_spec(spec)?);
            }
            "--self-urn" => {
                i += 1;
                config.self_urn = args
                    .get(i)
                    .ok_or("--self-urn requires a URN string")?
                    .clone();
            }
            "--help" | "-h" => {
                println!(
                    "Usage: ti-bench sk-feed [OPTIONS]\n\n\
                     Options:\n\
                       --bind <IP>             Bind IP address (default: 127.0.0.1)\n\
                       --port <PORT>           Port to listen on (default: 3000, 0 for random)\n\
                       --values-per-sec <N>    Target values/second throughput (default: 20000)\n\
                       --vessels <N>           Number of vessels (default: 21; 1 self + 20 AIS)\n\
                       --batch-size <N>        Number of values per delta batch (default: 100)\n\
                       --seed <U64>            Deterministic PRNG seed (default: 42)\n\
                       --duration <SECS>       Benchmark duration in seconds (default: infinite)\n\
                       --max-values <N>        Stop after sending N values\n\
                       --ramp <SPEC>           Ramp schedule: <rate>:<dur>,<rate>:<dur>...\n\
                       --self-urn <URN>        Self vessel URN (default: urn:mrn:imo:mmsi:367000000)\n"
                );
                std::process::exit(0);
            }
            other => return Err(format!("Unknown option: {other}")),
        }
        i += 1;
    }
    Ok(config)
}

pub fn run(args: &[String]) -> Result<(), String> {
    let config = parse_args(args)?;
    let stop = Arc::new(AtomicBool::new(false));
    serve(config, stop)?;
    Ok(())
}

pub struct TestFeedHandle {
    pub addr: SocketAddr,
    pub stop: Arc<AtomicBool>,
    pub handle: thread::JoinHandle<Result<(), String>>,
}

impl TestFeedHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn join(self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        self.handle.join().map_err(|e| format!("{e:?}"))?
    }
}

/// Start feed in a background thread for tests, returning bound local address and stop handle.
pub fn start_test_feed(config: FeedConfig) -> Result<TestFeedHandle, String> {
    let bind_addr = format!("{}:{}", config.bind, config.port);
    let listener =
        TcpListener::bind(&bind_addr).map_err(|e| format!("Failed to bind to {bind_addr}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let local_addr = listener.local_addr().map_err(|e| e.to_string())?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();

    let handle = thread::Builder::new()
        .name("sk-feed-test".into())
        .spawn(move || serve_listener(listener, config, stop_thread))
        .map_err(|e| e.to_string())?;

    Ok(TestFeedHandle {
        addr: local_addr,
        stop,
        handle,
    })
}

pub fn serve(config: FeedConfig, stop: Arc<AtomicBool>) -> Result<(), String> {
    let bind_addr = format!("{}:{}", config.bind, config.port);
    let listener =
        TcpListener::bind(&bind_addr).map_err(|e| format!("Failed to bind to {bind_addr}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let local_addr = listener.local_addr().map_err(|e| e.to_string())?;
    println!(
        "Signal K synthetic feed listening on ws://{}:{}/signalk/v1/stream",
        local_addr.ip(),
        local_addr.port()
    );
    serve_listener(listener, config, stop)
}

#[allow(clippy::result_large_err)]
fn serve_listener(
    listener: TcpListener,
    config: FeedConfig,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let overall_start = Instant::now();

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if let Some(dur) = config.duration_sec {
            if overall_start.elapsed().as_secs() >= dur {
                break;
            }
        }

        match listener.accept() {
            Ok((stream, peer_addr)) => {
                let _ = stream.set_nodelay(true);
                stream.set_nonblocking(false).map_err(|e| e.to_string())?;

                let mut subscribe_none = false;
                let ws_res = tungstenite::accept_hdr(stream, |req: &Request, res: Response| {
                    if let Some(query) = req.uri().query() {
                        if query.contains("subscribe=none") {
                            subscribe_none = true;
                        }
                    }
                    Ok(res)
                });

                let mut ws = match ws_res {
                    Ok(w) => w,
                    Err(e) => {
                        eprintln!("[sk-feed] WebSocket handshake error from {peer_addr}: {e}");
                        continue;
                    }
                };

                // Send hello message
                let hello = json!({
                    "name": "signalk-synthetic-feed",
                    "version": "1.0.0",
                    "self": config.self_urn,
                    "roles": ["master", "main"],
                    "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                });
                if let Err(e) = ws.send(Message::Text(hello.to_string())) {
                    eprintln!("[sk-feed] Failed to send hello to {peer_addr}: {e}");
                    continue;
                }

                // Switch to non-blocking for delta stream loop
                let _ = ws.get_ref().set_nonblocking(true);

                handle_client(&mut ws, &config, subscribe_none, &stop, overall_start);

                if stop.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                return Err(format!("Listener accept error: {e}"));
            }
        }
    }

    Ok(())
}

fn handle_client<S: std::io::Read + std::io::Write>(
    ws: &mut tungstenite::WebSocket<S>,
    config: &FeedConfig,
    subscribe_none: bool,
    stop: &Arc<AtomicBool>,
    overall_start: Instant,
) {
    let mut rng = SplitMix64::new(config.seed);
    let mut subscribed = !subscribe_none;
    let mut subscribed_paths: HashSet<String> = HashSet::new();
    let mut subscribe_all_paths = true;
    let mut star_subscribed = false;
    let mut target_vessel_context: Option<String> = None;

    // Ramp state
    let stages = config.ramp.clone().unwrap_or_else(|| {
        vec![(
            config.values_per_sec,
            config.duration_sec.unwrap_or(u64::MAX),
        )]
    });
    let mut current_stage_idx = 0;
    let mut stage_start = Instant::now();
    let mut stage_values_sent: u64 = 0;
    let mut total_values_sent: u64 = 0;

    let mut vessel_round_robin = 0usize;
    let mut path_cursor = 0usize;

    // Pre-create vessel URN strings to avoid allocations during hot loop
    let ais_contexts: Vec<String> = (1..config.vessels)
        .map(|idx| format!("vessels.urn:mrn:imo:mmsi:367{:06}", idx))
        .collect();

    while current_stage_idx < stages.len() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if let Some(dur) = config.duration_sec {
            if overall_start.elapsed().as_secs() >= dur {
                break;
            }
        }
        if let Some(max_v) = config.max_values {
            if total_values_sent >= max_v {
                break;
            }
        }

        // Check for incoming client messages (e.g. subscribe)
        match ws.read() {
            Ok(msg) => match msg {
                Message::Text(text) => {
                    if let Ok(val) = serde_json::from_str::<Value>(&text) {
                        if let Some(sub_list) = val.get("subscribe").and_then(|s| s.as_array()) {
                            subscribed = true;

                            if let Some(ctx) = val.get("context").and_then(|c| c.as_str()) {
                                if ctx != "vessels.self" && ctx != "vessels.*" && ctx != "*" {
                                    target_vessel_context = Some(ctx.to_string());
                                }
                            }

                            // Signal K subscriptions are additive: a later
                            // `notifications.*` must not cancel an earlier `*`.
                            for item in sub_list {
                                if let Some(path_str) = item.get("path").and_then(|p| p.as_str()) {
                                    if path_str == "*" {
                                        star_subscribed = true;
                                    } else {
                                        subscribed_paths.insert(path_str.to_string());
                                    }
                                }
                            }
                            subscribe_all_paths = star_subscribed || subscribed_paths.is_empty();
                        }
                    }
                }
                Message::Ping(data) => {
                    let _ = ws.send(Message::Pong(data));
                }
                Message::Close(_) => break,
                _ => {}
            },
            Err(tungstenite::Error::Io(ref e)) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => {
                eprintln!("[sk-feed] Client connection closed or error: {e}");
                break;
            }
        }

        if !subscribed {
            thread::sleep(Duration::from_millis(5));
            continue;
        }

        let (stage_rate, stage_duration) = stages[current_stage_idx];

        // Check stage progression
        if stage_start.elapsed().as_secs() >= stage_duration {
            current_stage_idx += 1;
            if current_stage_idx >= stages.len() {
                break;
            }
            let (next_rate, next_dur) = stages[current_stage_idx];
            println!(
                "[sk-feed] Ramp stage transition: rate={next_rate} values/s, duration={next_dur}s"
            );
            stage_start = Instant::now();
            stage_values_sent = 0;
            continue;
        }

        // Available paths filtered by subscription
        let active_paths: Vec<&PathDef> = if subscribe_all_paths {
            ALL_PATHS.iter().collect()
        } else {
            ALL_PATHS
                .iter()
                .filter(|p| {
                    subscribed_paths.contains(p.path)
                        || subscribed_paths.iter().any(|sub| {
                            if let Some(prefix) = sub.strip_suffix(".*") {
                                p.path.starts_with(prefix)
                            } else {
                                false
                            }
                        })
                })
                .collect()
        };

        if active_paths.is_empty() {
            thread::sleep(Duration::from_millis(5));
            continue;
        }

        // Determine batch size: don't exceed remaining target or available paths
        let batch_size = config.batch_size.min(stage_rate as usize).max(1);

        // Select vessel context
        let (context, source) = if let Some(ref ctx) = target_vessel_context {
            (ctx.as_str(), "ais.0")
        } else if config.vessels <= 1 || vessel_round_robin.is_multiple_of(config.vessels) {
            ("vessels.self", "n2k.160")
        } else {
            let ais_idx = (vessel_round_robin % config.vessels) - 1;
            (ais_contexts[ais_idx].as_str(), "ais.0")
        };
        vessel_round_robin = vessel_round_robin.wrapping_add(1);

        let now_iso = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        // Generate values
        let mut values_array = Vec::with_capacity(batch_size);
        for _ in 0..batch_size {
            let pdef = active_paths[path_cursor % active_paths.len()];
            path_cursor = path_cursor.wrapping_add(1);

            let val: Value = if pdef.is_int {
                let v = rng.range(pdef.min, pdef.max).round() as i64;
                json!(v)
            } else {
                let v = rng.range(pdef.min, pdef.max);
                let rounded = (v * 10000.0).round() / 10000.0;
                json!(rounded)
            };

            values_array.push(json!({
                "path": pdef.path,
                "value": val
            }));
        }

        let delta = json!({
            "context": context,
            "updates": [{
                "$source": source,
                "timestamp": now_iso,
                "values": values_array
            }]
        });

        if let Err(e) = ws.send(Message::Text(delta.to_string())) {
            eprintln!("[sk-feed] Send error: {e}");
            break;
        }

        stage_values_sent += batch_size as u64;
        total_values_sent += batch_size as u64;

        // Rate pacing
        let target_elapsed = Duration::from_secs_f64(stage_values_sent as f64 / stage_rate as f64);
        let actual_elapsed = stage_start.elapsed();
        if target_elapsed > actual_elapsed {
            let sleep_dur = target_elapsed - actual_elapsed;
            if sleep_dur > Duration::from_micros(100) {
                thread::sleep(sleep_dur);
            }
        }
    }
}
