//! OTLP receiver soak: Claude Code-shaped `/v1/metrics` and `/v1/logs` posts.
//!
//! Token sums are temporality 1 (delta). The receiver stores the running total, so
//! `sum("claude_code.token.usage")` is not the sum of increments. The check compares
//! the sum of per-vessel `max("claude_code.token.usage")` to the deltas in 2xx posts.
//! Log doc ids hash entity, time, title, and body, so each log gets a unique time.

use crate::rng::SplitMix64;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const USAGE: &str = "\
Usage: ti-bench otlp-soak [--url http://127.0.0.1:PORT | --spawn] [OPTIONS]

Options:
  --url <URL>            Receiver base URL (http://host:port). Required without --spawn.
  --spawn                Start `lume ti otlp` on 127.0.0.1 with an ephemeral port.
  --lume-bin <PATH>      lume binary for --spawn (else $LUME_BIN or target/debug/lume).
  --token-file <PATH>    Bearer token file. Ignored with --spawn (loopback has no token).
  --agents <N>           Simulated agents (required).
  --rate <R>             Log posts per second per agent (required).
  --duration <SEC>       Wall-clock seconds (required).
  --seed <U64>           Deterministic variation seed (default 42).
  --out <PATH>           JSON results path (required).

Each agent posts /v1/metrics every 10s and /v1/logs at --rate. Times are phase-offset
by agent/agents of each interval. Metric model/tool/file variation is on the log
records; the token series stays unattributed so the column is claude_code.token.usage.";

const METRIC_INTERVAL_NS: u64 = 10_000_000_000;
const BASE_UNIX_NS: u128 = 1_577_836_811_000_000_000;
const SALT_METRIC: u64 = 0x6D65_7472_6963;
const SALT_LOG: u64 = 0x6C6F_6773;
const MODELS: &[&str] = &["claude-sonnet", "claude-haiku", "claude-opus"];
const TOOLS: &[&str] = &["nuts_edit", "nuts_replace", "shell"];
const FILES: &[&str] = &[
    "src/service.rs",
    "src/main.rs",
    "crates/ti-bench/src/otlp_soak.rs",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Metric,
    Log,
}

#[derive(Clone, Debug)]
struct Event {
    kind: Kind,
    seq: u64,
    offset_ns: u64,
}

#[derive(Debug)]
pub struct SoakConfig {
    pub url: Option<String>,
    pub token_file: Option<PathBuf>,
    pub agents: u32,
    pub rate: f64,
    pub duration_s: u64,
    pub seed: u64,
    pub out: PathBuf,
    pub spawn: bool,
    pub lume_bin: Option<PathBuf>,
}

#[derive(Clone, Copy)]
struct Sample {
    kind: Kind,
    status: Option<u16>,
    latency_ms: f64,
    tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub sha: String,
    pub seed: u64,
    pub agents: u32,
    pub rate: f64,
    pub duration_s: u64,
    pub load_wall_s: f64,
    pub url: String,
    pub spawned: bool,
    pub schedule: Schedule,
    pub sent: Sent,
    pub latency_ms: BTreeMap<String, Latency>,
    pub status: BTreeMap<String, BTreeMap<String, u64>>,
    pub rss_bytes: Rss,
    pub sql: SqlCheck,
}

#[derive(Debug, Serialize)]
pub struct Schedule {
    pub metrics_every_s: u64,
    pub log_interval_ns: u64,
    pub phase: &'static str,
    pub metric_temporality: u32,
    pub token_column: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Sent {
    pub offered_metrics: u64,
    pub offered_logs: u64,
    pub metrics_posts: u64,
    pub log_posts: u64,
    pub metrics_2xx: u64,
    pub log_2xx: u64,
    pub tokens: u64,
    pub logs: u64,
}

#[derive(Debug, Serialize)]
pub struct Latency {
    pub n: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p99: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Rss {
    Bytes { start: u64, end: u64, peak: u64 },
    Unavailable(String),
}

#[derive(Debug, Serialize)]
pub struct SqlCheck {
    pub token_totals: f64,
    pub tokens_sent: u64,
    pub docs: u64,
    pub logs_sent: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct Spawned {
    child: Child,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    store: PathBuf,
}

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(thread) = self.stdout_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_dir_all(&self.store);
    }
}

struct RssStats {
    start: Option<u64>,
    end: Option<u64>,
    peak: Option<u64>,
}

pub fn run(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let cfg = parse_args(args)?;
    let report = execute(&cfg)?;
    println!(
        "otlp-soak: out={} logs_2xx={} metrics_2xx={} tokens={} docs={} sql_ok={} p95_logs_ms={} p95_metrics_ms={} rss_peak={}",
        cfg.out.display(),
        report.sent.log_2xx,
        report.sent.metrics_2xx,
        report.sql.tokens_sent,
        report.sql.docs,
        report.sql.ok,
        report
            .latency_ms
            .get("/v1/logs")
            .and_then(|l| l.p95)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        report
            .latency_ms
            .get("/v1/metrics")
            .and_then(|l| l.p95)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        match &report.rss_bytes {
            Rss::Bytes { peak, .. } => peak.to_string(),
            Rss::Unavailable(v) => v.clone(),
        }
    );
    Ok(())
}

pub fn parse_args(args: &[String]) -> Result<SoakConfig, String> {
    let mut url = None;
    let mut token_file = None;
    let mut agents = None;
    let mut rate = None;
    let mut duration_s = None;
    let mut seed = 42u64;
    let mut out = None;
    let mut spawn = false;
    let mut lume_bin = None;
    let mut i = 2;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--spawn" {
            spawn = true;
            i += 1;
            continue;
        }
        let value = args
            .get(i + 1)
            .filter(|s| !s.starts_with("--"))
            .ok_or_else(|| format!("{flag} requires a value\n{USAGE}"))?;
        match flag {
            "--url" => url = Some(value.clone()),
            "--token-file" => token_file = Some(PathBuf::from(value)),
            "--agents" => {
                agents = Some(
                    value
                        .parse::<u32>()
                        .map_err(|e| format!("invalid --agents: {e}"))?,
                );
            }
            "--rate" => {
                rate = Some(
                    value
                        .parse::<f64>()
                        .map_err(|e| format!("invalid --rate: {e}"))?,
                );
            }
            "--duration" => {
                duration_s = Some(
                    value
                        .parse::<u64>()
                        .map_err(|e| format!("invalid --duration: {e}"))?,
                );
            }
            "--seed" => {
                seed = value
                    .parse::<u64>()
                    .map_err(|e| format!("invalid --seed: {e}"))?;
            }
            "--out" => out = Some(PathBuf::from(value)),
            "--lume-bin" => lume_bin = Some(PathBuf::from(value)),
            other => return Err(format!("unknown option: {other}\n{USAGE}")),
        }
        i += 2;
    }
    let agents = agents.ok_or_else(|| format!("--agents is required\n{USAGE}"))?;
    let rate = rate.ok_or_else(|| format!("--rate is required\n{USAGE}"))?;
    let duration_s = duration_s.ok_or_else(|| format!("--duration is required\n{USAGE}"))?;
    let out = out.ok_or_else(|| format!("--out is required\n{USAGE}"))?;
    if agents == 0 {
        return Err("--agents must be >= 1".into());
    }
    if !(rate.is_finite() && rate > 0.0) {
        return Err("--rate must be a positive finite number".into());
    }
    if duration_s == 0 {
        return Err("--duration must be >= 1".into());
    }
    if !spawn && url.is_none() {
        return Err(format!("pass --url or --spawn\n{USAGE}"));
    }
    Ok(SoakConfig {
        url,
        token_file,
        agents,
        rate,
        duration_s,
        seed,
        out,
        spawn,
        lume_bin,
    })
}

pub fn execute(cfg: &SoakConfig) -> Result<Report, String> {
    let interval_ns = log_interval_ns(cfg.rate)?;
    let mut by_agent = Vec::with_capacity(cfg.agents as usize);
    for agent in 0..cfg.agents {
        by_agent.push(agent_events(
            agent,
            cfg.agents,
            interval_ns,
            cfg.duration_s,
        )?);
    }
    let (spawned, url, token) = if cfg.spawn {
        let bin = resolve_lume(cfg.lume_bin.as_deref())?;
        let store = std::env::temp_dir().join(format!(
            "otlp-soak-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let (child, listen) = spawn_lume(&bin, &store)?;
        let addr = socket_from_url(&listen)?;
        wait_ready(addr)?;
        (Some(child), listen, None)
    } else {
        let url = cfg.url.clone().ok_or("--url is required without --spawn")?;
        let token = match &cfg.token_file {
            Some(path) => Some(read_token(path)?),
            None => None,
        };
        (None, url, token)
    };
    let addr = socket_from_url(&url)?;
    let rss_stop = Arc::new(AtomicBool::new(false));
    let rss_thread = spawned.as_ref().map(|server| {
        let pid = server.child.id();
        let stop = Arc::clone(&rss_stop);
        thread::Builder::new()
            .name("otlp-soak-rss".into())
            .spawn(move || sample_rss(pid, &stop))
            .ok()
    });
    let mut offered_metrics = 0u64;
    let mut offered_logs = 0u64;
    for events in &by_agent {
        for event in events {
            match event.kind {
                Kind::Metric => offered_metrics += 1,
                Kind::Log => offered_logs += 1,
            }
        }
    }
    let started = Instant::now();
    let done = Arc::new(AtomicUsize::new(0));
    let total: usize = by_agent.iter().map(Vec::len).sum();
    let progress_stop = Arc::new(AtomicBool::new(false));
    let progress = {
        let done = Arc::clone(&done);
        let stop = Arc::clone(&progress_stop);
        thread::spawn(move || {
            let mut next = Instant::now() + Duration::from_secs(30);
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(200));
                if Instant::now() >= next {
                    eprintln!("otlp-soak: {}/{total} posts", done.load(Ordering::Relaxed));
                    next = Instant::now() + Duration::from_secs(30);
                }
            }
        })
    };
    eprintln!(
        "otlp-soak: agents={} rate={} duration={}s url={} posts={total}",
        cfg.agents, cfg.rate, cfg.duration_s, url
    );
    let mut handles = Vec::with_capacity(by_agent.len());
    for (agent, events) in by_agent.into_iter().enumerate() {
        let token = token.clone();
        let done = Arc::clone(&done);
        let agent = u32::try_from(agent).unwrap_or(0);
        let ctx = AgentCtx {
            agent,
            seed: cfg.seed,
            addr,
            token,
            started,
            duration_s: cfg.duration_s,
        };
        handles.push(
            thread::Builder::new()
                .name(format!("otlp-soak-{agent}"))
                .spawn(move || run_agent(ctx, events, &done))
                .map_err(|e| format!("spawn agent thread: {e}"))?,
        );
    }
    let mut samples = Vec::new();
    for handle in handles {
        samples.extend(
            handle
                .join()
                .map_err(|_| "agent thread panicked".to_string())?,
        );
    }
    progress_stop.store(true, Ordering::Relaxed);
    let _ = progress.join();
    let load_wall_s = started.elapsed().as_secs_f64();
    let sql = sql_check(addr, token.as_deref(), &samples);
    rss_stop.store(true, Ordering::Relaxed);
    let rss = match rss_thread {
        Some(Some(thread)) => thread.join().unwrap_or(RssStats {
            start: None,
            end: None,
            peak: None,
        }),
        _ => RssStats {
            start: None,
            end: None,
            peak: None,
        },
    };
    let report = build_report(
        cfg,
        &url,
        interval_ns,
        LoadMeta {
            offered_metrics,
            offered_logs,
            load_wall_s,
        },
        &samples,
        &sql,
        &rss,
    );
    if let Some(parent) = cfg.out.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
    }
    let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    let mut file = fs::File::create(&cfg.out).map_err(|e| format!("create results: {e}"))?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    if !sql.ok {
        let tail = spawned
            .as_ref()
            .map(|server| tail_file(&server.store.join("receiver.err"), 20))
            .unwrap_or_default();
        return Err(format!(
            "sql check failed: tokens sent {} stored {} logs sent {} docs {}{}{tail}",
            sql.tokens_sent,
            sql.token_totals,
            sql.logs_sent,
            sql.docs,
            sql.error
                .as_deref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        ));
    }
    Ok(report)
}

struct AgentCtx {
    agent: u32,
    seed: u64,
    addr: SocketAddr,
    token: Option<String>,
    started: Instant,
    duration_s: u64,
}

struct LoadMeta {
    offered_metrics: u64,
    offered_logs: u64,
    load_wall_s: f64,
}

fn run_agent(ctx: AgentCtx, events: Vec<Event>, done: &AtomicUsize) -> Vec<Sample> {
    let window = Duration::from_secs(ctx.duration_s);
    let AgentCtx {
        agent,
        seed,
        addr,
        token,
        started,
        ..
    } = &ctx;
    let mut out = Vec::with_capacity(events.len());
    for event in events {
        if started.elapsed() >= window {
            break;
        }
        let deadline = *started + Duration::from_nanos(event.offset_ns);
        let now = Instant::now();
        if deadline > now {
            let until_window = window.saturating_sub(started.elapsed());
            thread::sleep(deadline.saturating_duration_since(now).min(until_window));
            if started.elapsed() >= window {
                break;
            }
        }
        let prepared = match event.kind {
            Kind::Metric => metric_body(*seed, *agent, event.seq, event.offset_ns),
            Kind::Log => log_body(*seed, *agent, event.seq, event.offset_ns).map(|body| (body, 0)),
        };
        let sample = match prepared {
            Ok((body, tokens)) => {
                let path = match event.kind {
                    Kind::Metric => "/v1/metrics",
                    Kind::Log => "/v1/logs",
                };
                let posted = post(*addr, path, &body, token.as_deref());
                let success = matches!(posted.status, Some(status) if (200..300).contains(&status));
                Sample {
                    kind: event.kind,
                    status: posted.status,
                    latency_ms: posted.latency_ms,
                    tokens: if success { tokens } else { 0 },
                }
            }
            Err(_) => Sample {
                kind: event.kind,
                status: None,
                latency_ms: 0.0,
                tokens: 0,
            },
        };
        out.push(sample);
        done.fetch_add(1, Ordering::Relaxed);
    }
    out
}

fn build_report(
    cfg: &SoakConfig,
    url: &str,
    interval_ns: u64,
    meta: LoadMeta,
    samples: &[Sample],
    sql: &SqlCheck,
    rss: &RssStats,
) -> Report {
    let mut status: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut latencies: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut sent = Sent {
        offered_metrics: meta.offered_metrics,
        offered_logs: meta.offered_logs,
        metrics_posts: 0,
        log_posts: 0,
        metrics_2xx: 0,
        log_2xx: 0,
        tokens: 0,
        logs: 0,
    };
    for sample in samples {
        let path = match sample.kind {
            Kind::Metric => "/v1/metrics",
            Kind::Log => "/v1/logs",
        };
        *status
            .entry(path.to_string())
            .or_default()
            .entry(status_key(sample.status).to_string())
            .or_default() += 1;
        latencies.entry(path).or_default().push(sample.latency_ms);
        let success = matches!(sample.status, Some(code) if (200..300).contains(&code));
        match sample.kind {
            Kind::Metric => {
                sent.metrics_posts += 1;
                if success {
                    sent.metrics_2xx += 1;
                    sent.tokens += sample.tokens;
                }
            }
            Kind::Log => {
                sent.log_posts += 1;
                if success {
                    sent.log_2xx += 1;
                    sent.logs += 1;
                }
            }
        }
    }
    for counts in status.values_mut() {
        for key in ["200", "400", "413", "503", "other", "error"] {
            counts.entry(key.to_string()).or_default();
        }
    }
    let mut latency_ms = BTreeMap::new();
    for (path, mut values) in latencies {
        values.sort_by(|a, b| a.total_cmp(b));
        latency_ms.insert(
            path.to_string(),
            Latency {
                n: u64::try_from(values.len()).unwrap_or(0),
                p50: percentile(&values, 50.0).map(round_ms),
                p95: percentile(&values, 95.0).map(round_ms),
                p99: percentile(&values, 99.0).map(round_ms),
            },
        );
    }
    Report {
        sha: git_sha(),
        seed: cfg.seed,
        agents: cfg.agents,
        rate: cfg.rate,
        duration_s: cfg.duration_s,
        load_wall_s: round_ms(meta.load_wall_s),
        url: url.to_string(),
        spawned: cfg.spawn,
        schedule: Schedule {
            metrics_every_s: 10,
            log_interval_ns: interval_ns,
            phase: "agent * interval / agents",
            metric_temporality: 1,
            token_column: "claude_code.token.usage",
        },
        sent,
        latency_ms,
        status,
        rss_bytes: match (rss.start, rss.end, rss.peak) {
            (Some(start), Some(end), Some(peak)) => Rss::Bytes { start, end, peak },
            _ => Rss::Unavailable("n/a".to_string()),
        },
        sql: sql.clone_report(),
    }
}

impl SqlCheck {
    fn clone_report(&self) -> Self {
        Self {
            token_totals: self.token_totals,
            tokens_sent: self.tokens_sent,
            docs: self.docs,
            logs_sent: self.logs_sent,
            ok: self.ok,
            error: self.error.clone(),
        }
    }
}

fn sql_check(addr: SocketAddr, token: Option<&str>, samples: &[Sample]) -> SqlCheck {
    let tokens_sent = samples
        .iter()
        .filter(|s| s.kind == Kind::Metric)
        .map(|s| s.tokens)
        .sum();
    let logs_sent = u64::try_from(
        samples
            .iter()
            .filter(|s| {
                s.kind == Kind::Log && matches!(s.status, Some(code) if (200..300).contains(&code))
            })
            .count(),
    )
    .unwrap_or(0);
    match sql_numbers(addr, token, tokens_sent, logs_sent) {
        Ok((token_totals, docs)) => {
            let tokens_ok = (token_totals - tokens_sent as f64).abs() < 1e-3;
            let docs_ok = docs == logs_sent;
            SqlCheck {
                token_totals,
                tokens_sent,
                docs,
                logs_sent,
                ok: tokens_ok && docs_ok,
                error: None,
            }
        }
        Err(error) => SqlCheck {
            token_totals: 0.0,
            tokens_sent,
            docs: 0,
            logs_sent,
            ok: false,
            error: Some(error),
        },
    }
}

fn sql_numbers(
    addr: SocketAddr,
    token: Option<&str>,
    tokens_sent: u64,
    logs_sent: u64,
) -> Result<(f64, u64), String> {
    let agent_rows = match query(addr, token, "SELECT count(*) AS n FROM telemetry_agents") {
        Ok(agents) => json_u64(&agents["rows"][0]["n"]).unwrap_or(0),
        Err(_) if tokens_sent == 0 => 0,
        Err(error) => return Err(error),
    };
    let token_totals = if agent_rows == 0 {
        if tokens_sent != 0 {
            return Err(format!(
                "telemetry_agents is empty but {tokens_sent} token deltas were accepted"
            ));
        }
        0.0
    } else {
        let rows = query(
            addr,
            token,
            "SELECT vessel, max(\"claude_code.token.usage\") AS tokens FROM telemetry_agents GROUP BY vessel",
        )?;
        let list = rows["rows"]
            .as_array()
            .ok_or("token query returned no rows array")?;
        if list.len() >= 500 {
            return Err("token query hit the 500-row cap".into());
        }
        let mut sum = 0.0;
        for row in list {
            sum += json_f64(&row["tokens"]).unwrap_or(0.0);
        }
        sum
    };
    let docs = match query(addr, token, "SELECT count(*) AS n FROM docs") {
        Ok(reply) => json_u64(&reply["rows"][0]["n"]).ok_or("docs count was not a number")?,
        Err(_) if logs_sent == 0 => 0,
        Err(error) => return Err(error),
    };
    if docs == 0 && logs_sent != 0 {
        return Err(format!(
            "docs is empty but {logs_sent} log posts were accepted"
        ));
    }
    Ok((token_totals, docs))
}

fn query(addr: SocketAddr, token: Option<&str>, sql: &str) -> Result<Value, String> {
    let body =
        serde_json::to_vec(&json!({"sql": sql, "max_rows": 500})).map_err(|e| e.to_string())?;
    let posted = post(addr, "/ti/query", &body, token);
    let status = posted
        .status
        .ok_or_else(|| "sql query transport error".to_string())?;
    if status != 200 {
        return Err(format!(
            "sql status {status}: {}",
            String::from_utf8_lossy(&posted.body)
        ));
    }
    serde_json::from_slice(&posted.body).map_err(|e| format!("sql json: {e}"))
}

fn json_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_i64().map(|n| n as f64))
        .or_else(|| v.as_u64().map(|n| n as f64))
}

fn json_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    if let Some(n) = v.as_i64() {
        return u64::try_from(n).ok();
    }
    let n = v.as_f64()?;
    if n >= 0.0 && n < (1u64 << 53) as f64 && (n - n.round()).abs() < 1e-6 {
        return Some(n.round() as u64);
    }
    None
}

fn status_key(status: Option<u16>) -> &'static str {
    match status {
        Some(200) => "200",
        Some(400) => "400",
        Some(413) => "413",
        Some(503) => "503",
        Some(_) => "other",
        None => "error",
    }
}

fn percentile(sorted: &[f64], pct: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (pct / 100.0 * sorted.len() as f64).ceil();
    if !rank.is_finite() || rank < 1.0 {
        return sorted.first().copied();
    }
    let idx = usize::try_from(rank as u64)
        .unwrap_or(1)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    Some(sorted[idx])
}

fn round_ms(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn log_interval_ns(rate: f64) -> Result<u64, String> {
    if !(rate.is_finite() && rate > 0.0) {
        return Err("rate must be a positive finite number".into());
    }
    let ns = (1_000_000_000.0 / rate).round();
    if !(ns.is_finite() && ns >= 1.0 && ns < (1u64 << 53) as f64) {
        return Err("rate is out of range".into());
    }
    Ok(ns as u64)
}

fn duration_ns(duration_s: u64) -> Result<u64, String> {
    duration_s
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "duration is too large".to_string())
}

fn phase_ns(agent: u32, agents: u32, interval_ns: u64) -> u64 {
    if agents == 0 {
        return 0;
    }
    u64::try_from(u128::from(agent) * u128::from(interval_ns) / u128::from(agents)).unwrap_or(0)
}

fn agent_events(
    agent: u32,
    agents: u32,
    log_interval_ns: u64,
    duration_s: u64,
) -> Result<Vec<Event>, String> {
    let duration = duration_ns(duration_s)?;
    let mut events = Vec::new();
    let metric_phase = phase_ns(agent, agents, METRIC_INTERVAL_NS);
    let mut seq = 0u64;
    loop {
        let offset = metric_phase + seq.saturating_mul(METRIC_INTERVAL_NS);
        if offset >= duration || (seq > 0 && offset < metric_phase) {
            break;
        }
        events.push(Event {
            kind: Kind::Metric,
            seq,
            offset_ns: offset,
        });
        seq += 1;
    }
    let log_phase = phase_ns(agent, agents, log_interval_ns);
    let mut seq = 0u64;
    loop {
        let offset = log_phase + seq.saturating_mul(log_interval_ns);
        if offset >= duration || (seq > 0 && offset < log_phase) {
            break;
        }
        events.push(Event {
            kind: Kind::Log,
            seq,
            offset_ns: offset,
        });
        seq += 1;
    }
    events.sort_by(|a, b| {
        a.offset_ns
            .cmp(&b.offset_ns)
            .then_with(|| match (a.kind, b.kind) {
                (Kind::Metric, Kind::Log) => std::cmp::Ordering::Less,
                (Kind::Log, Kind::Metric) => std::cmp::Ordering::Greater,
                _ => a.seq.cmp(&b.seq),
            })
    });
    Ok(events)
}

fn mix(seed: u64, agent: u32, seq: u64, salt: u64) -> SplitMix64 {
    let mixed = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(u64::from(agent))
        .wrapping_mul(0xBF58_476D_1CE4_E5B9)
        .wrapping_add(seq)
        .wrapping_mul(0x94D0_49BB_1331_11EB)
        .wrapping_add(salt);
    SplitMix64::new(mixed)
}

fn token_delta(seed: u64, agent: u32, seq: u64) -> u64 {
    1 + mix(seed, agent, seq, SALT_METRIC).below(16)
}

fn pick<'a>(rng: &mut SplitMix64, items: &[&'a str]) -> &'a str {
    let n = u64::try_from(items.len()).unwrap_or(1).max(1);
    let idx = usize::try_from(rng.below(n)).unwrap_or(0);
    items.get(idx).copied().unwrap_or(items[0])
}

fn unix_nano(offset_ns: u64) -> String {
    (BASE_UNIX_NS + u128::from(offset_ns)).to_string()
}

fn metric_body(seed: u64, agent: u32, seq: u64, offset_ns: u64) -> Result<(Vec<u8>, u64), String> {
    let tokens = token_delta(seed, agent, seq);
    let time = unix_nano(offset_ns);
    let start = unix_nano(offset_ns.saturating_sub(METRIC_INTERVAL_NS));
    let value = json!({
        "resourceMetrics": [{
            "resource": {"attributes": [
                {"key": "service.name", "value": {"stringValue": "claude-code"}},
                {"key": "service.instance.id", "value": {"stringValue": format!("soak-{agent}")}}
            ]},
            "scopeMetrics": [{
                "scope": {"name": "claude-code"},
                "metrics": [
                    {
                        "name": "claude_code.token.usage",
                        "unit": "{token}",
                        "sum": {
                            "aggregationTemporality": 1,
                            "isMonotonic": true,
                            "dataPoints": [{
                                "timeUnixNano": time,
                                "startTimeUnixNano": start,
                                "asInt": tokens.to_string()
                            }]
                        }
                    },
                    {
                        "name": "claude_code.active_time",
                        "unit": "s",
                        "gauge": {"dataPoints": [{
                            "timeUnixNano": time,
                            "asDouble": 1.5
                        }]}
                    },
                    {
                        "name": "claude_code.tool.duration",
                        "unit": "s",
                        "histogram": {"dataPoints": [{
                            "timeUnixNano": time,
                            "sum": 4.5,
                            "count": "1",
                            "bucketCounts": ["1"],
                            "explicitBounds": [1]
                        }]}
                    }
                ]
            }]
        }]
    });
    let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    Ok((bytes, tokens))
}

fn log_body(seed: u64, agent: u32, seq: u64, offset_ns: u64) -> Result<Vec<u8>, String> {
    let mut rng = mix(seed, agent, seq, SALT_LOG);
    let model = pick(&mut rng, MODELS);
    let tool = pick(&mut rng, TOOLS);
    let file = pick(&mut rng, FILES);
    let added = 1 + rng.below(8);
    let removed = rng.below(5);
    let value = json!({
        "resourceLogs": [{
            "resource": {"attributes": [
                {"key": "pane", "value": {"stringValue": format!("soak-{agent}")}},
                {"key": "service.name", "value": {"stringValue": "claude-code"}}
            ]},
            "scopeLogs": [{
                "logRecords": [{
                    "timeUnixNano": unix_nano(offset_ns),
                    "eventName": "file.edit",
                    "body": {"stringValue": format!("edited {file} agent={agent} seq={seq} model={model}")},
                    "attributes": [
                        {"key": "file.path", "value": {"stringValue": file}},
                        {"key": "op", "value": {"stringValue": "edit"}},
                        {"key": "tool", "value": {"stringValue": tool}},
                        {"key": "model", "value": {"stringValue": model}},
                        {"key": "lines.added", "value": {"intValue": added.to_string()}},
                        {"key": "lines.removed", "value": {"intValue": removed.to_string()}}
                    ]
                }]
            }]
        }]
    });
    serde_json::to_vec(&value).map_err(|e| e.to_string())
}

#[cfg(test)]
fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/otlp")
}

#[cfg(test)]
fn load_golden(name: &str) -> Result<Value, String> {
    let path = golden_dir().join(name);
    let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

struct PostResult {
    status: Option<u16>,
    body: Vec<u8>,
    latency_ms: f64,
}

fn post(addr: SocketAddr, path: &str, body: &[u8], token: Option<&str>) -> PostResult {
    let started = Instant::now();
    let fail = |started: Instant| PostResult {
        status: None,
        body: Vec::new(),
        latency_ms: elapsed_ms(started),
    };
    let mut stream = match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
        Ok(stream) => stream,
        Err(_) => return fail(started),
    };
    let _ = stream.set_nodelay(true);
    // Accepted posts can sit behind the receiver lock. A short read timeout
    // drops the socket after the server has stored the batch, so the SQL
    // check then sees more rows than the client counted.
    if stream
        .set_read_timeout(Some(Duration::from_secs(180)))
        .is_err()
        || stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .is_err()
    {
        return fail(started);
    }
    let req = encode_request(&addr.to_string(), path, body, token);
    let wrote = stream.write_all(&req).and_then(|_| stream.flush());
    if wrote.is_err() {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    }
    match read_response(&mut stream) {
        Ok((status, body)) => PostResult {
            status: Some(status),
            body,
            latency_ms: elapsed_ms(started),
        },
        Err(_) => fail(started),
    }
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn encode_request(host: &str, path: &str, body: &[u8], token: Option<&str>) -> Vec<u8> {
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn read_response(stream: &mut TcpStream) -> Result<(u16, Vec<u8>), String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        if buf.len() > 2 * 1024 * 1024 {
            return Err("response exceeds 2 MiB".into());
        }
        if let Some((status, body)) = split_http(&buf) {
            return Ok((status, body));
        }
        match stream.read(&mut tmp) {
            Ok(0) => {
                return split_http(&buf).ok_or_else(|| "empty http response".to_string());
            }
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                return split_http(&buf).ok_or_else(|| e.to_string());
            }
            Err(e) => return split_http(&buf).ok_or_else(|| e.to_string()),
        }
    }
}

fn split_http(buf: &[u8]) -> Option<(u16, Vec<u8>)> {
    let sep = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(buf.get(..sep)?).ok()?;
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let mut content_length = None;
    for line in head.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().ok();
            }
        }
    }
    let body_at = sep + 4;
    match content_length {
        Some(len) if buf.len() >= body_at + len => {
            Some((status, buf[body_at..body_at + len].to_vec()))
        }
        Some(_) => None,
        None => Some((status, buf.get(body_at..)?.to_vec())),
    }
}

fn socket_from_url(url: &str) -> Result<SocketAddr, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("only http:// URLs are supported: {url}"))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    authority
        .parse()
        .map_err(|e| format!("invalid receiver url {url}: {e}"))
}

fn read_token(path: &Path) -> Result<String, String> {
    let token =
        fs::read_to_string(path).map_err(|e| format!("read token {}: {e}", path.display()))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("token file is empty".into());
    }
    if token.bytes().any(|b| b == b'\r' || b == b'\n') {
        return Err("token contains a line break".into());
    }
    Ok(token)
}

fn resolve_lume(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(format!("lume binary not found: {}", path.display()));
    }
    if let Some(path) = std::env::var_os("LUME_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }
    for rel in ["../../target/debug/lume", "../../target/release/lume"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err("lume binary not found; set LUME_BIN or build --features ti --bin lume".into())
}

fn spawn_lume(bin: &Path, store: &Path) -> Result<(Spawned, String), String> {
    fs::create_dir_all(store).map_err(|e| format!("create store: {e}"))?;
    let mut child = Command::new(bin)
        .args(["ti", "otlp", "--store"])
        .arg(store)
        .args(["--bind", "127.0.0.1", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", bin.display()))?;
    let stdout = child.stdout.take().ok_or("lume stdout was not piped")?;
    let stderr = child.stderr.take().ok_or("lume stderr was not piped")?;
    let err_log = store.join("receiver.err");
    let stderr_thread = thread::spawn(move || drain(stderr, err_log));
    let mut spawned = Spawned {
        child,
        stdout_thread: None,
        stderr_thread: Some(stderr_thread),
        store: store.to_path_buf(),
    };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let result = reader.read_line(&mut line).map(|_| line);
        let _ = tx.send((result, reader));
    });
    let (line, reader) = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok((Ok(line), reader)) => (line, reader),
        Ok((Err(e), _)) => return Err(format!("reading listen line: {e}")),
        Err(_) => return Err("timed out waiting for the OTLP listen line".into()),
    };
    let url = line
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim()
        .to_string();
    if !url.starts_with("http://127.0.0.1:") && !url.starts_with("http://[::1]:") {
        return Err(format!("receiver listen line was not loopback: {line}"));
    }
    let out_log = store.join("receiver.out");
    spawned.stdout_thread = Some(thread::spawn(move || drain(reader, out_log)));
    Ok((spawned, url))
}

fn wait_ready(addr: SocketAddr) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("receiver {addr} did not accept connections"));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn drain(mut reader: impl Read, path: PathBuf) {
    let mut file = fs::File::create(path).ok();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if let Some(file) = file.as_mut() {
                    let _ = file.write_all(&buf[..n]);
                }
            }
        }
    }
}

fn tail_file(path: &Path, lines: usize) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return String::new();
    };
    let kept: Vec<&str> = text.lines().rev().take(lines).collect();
    if kept.is_empty() {
        return String::new();
    }
    let mut out = String::from("; receiver stderr:");
    for line in kept.into_iter().rev() {
        out.push(' ');
        out.push_str(line);
    }
    out
}

fn git_sha() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(target_os = "linux")]
fn read_rss(pid: u32) -> Option<(u64, u64)> {
    let text = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let mut rss = None;
    let mut hwm = None;
    for line in text.lines() {
        let kib = |prefix: &str| -> Option<u64> {
            let rest = line.strip_prefix(prefix)?;
            rest.split_whitespace().next()?.parse::<u64>().ok()
        };
        if line.starts_with("VmRSS:") {
            rss = kib("VmRSS:");
        } else if line.starts_with("VmHWM:") {
            hwm = kib("VmHWM:");
        }
    }
    let rss = rss? * 1024;
    let hwm = hwm.unwrap_or(rss / 1024) * 1024;
    Some((rss, hwm))
}

#[cfg(not(target_os = "linux"))]
fn read_rss(_pid: u32) -> Option<(u64, u64)> {
    None
}

fn sample_rss(pid: u32, stop: &AtomicBool) -> RssStats {
    let mut start = None;
    let mut end = None;
    let mut peak = None;
    loop {
        if let Some((rss, hwm)) = read_rss(pid) {
            if start.is_none() {
                start = Some(rss);
            }
            end = Some(rss);
            let sample = rss.max(hwm);
            peak = Some(peak.map(|prev: u64| prev.max(sample)).unwrap_or(sample));
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    if let Some((rss, hwm)) = read_rss(pid) {
        if start.is_none() {
            start = Some(rss);
        }
        end = Some(rss);
        let sample = rss.max(hwm);
        peak = Some(peak.map(|prev: u64| prev.max(sample)).unwrap_or(sample));
    }
    RssStats { start, end, peak }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric_names(value: &Value) -> Vec<String> {
        let mut names = Vec::new();
        let Some(resources) = value.get("resourceMetrics").and_then(Value::as_array) else {
            return names;
        };
        for resource in resources {
            let Some(scopes) = resource.get("scopeMetrics").and_then(Value::as_array) else {
                continue;
            };
            for scope in scopes {
                let Some(metrics) = scope.get("metrics").and_then(Value::as_array) else {
                    continue;
                };
                for metric in metrics {
                    if let Some(name) = metric.get("name").and_then(Value::as_str) {
                        names.push(name.to_string());
                    }
                }
            }
        }
        names
    }

    #[test]
    fn generator_is_deterministic_and_varies_log_fields() {
        let a = log_body(42, 0, 3, 1_000).unwrap();
        let b = log_body(42, 0, 3, 1_000).unwrap();
        assert_eq!(a, b);
        let (ma, ta) = metric_body(42, 1, 4, 2_000).unwrap();
        let (mb, tb) = metric_body(42, 1, 4, 2_000).unwrap();
        assert_eq!(ma, mb);
        assert_eq!(ta, tb);
        assert!((1..=16).contains(&ta));

        let mut triples = BTreeMap::new();
        for agent in 0..8 {
            let body: Value = serde_json::from_slice(&log_body(42, agent, 0, 0).unwrap()).unwrap();
            let attrs = &body["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["attributes"];
            let find = |key: &str| {
                attrs
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|a| a["key"] == key)
                    .unwrap()["value"]["stringValue"]
                    .as_str()
                    .unwrap()
                    .to_string()
            };
            let model = find("model");
            let tool = find("tool");
            let file = find("file.path");
            assert!(MODELS.contains(&model.as_str()));
            assert!(TOOLS.contains(&tool.as_str()));
            assert!(FILES.contains(&file.as_str()));
            triples.insert(agent, (model, tool, file));
        }
        let distinct: std::collections::BTreeSet<_> = triples.values().cloned().collect();
        assert!(
            distinct.len() > 1,
            "expected seed variation across agents: {triples:?}"
        );
    }

    #[test]
    fn metric_shape_matches_golden_names_and_delta_tokens_sum() {
        let golden = load_golden("metrics.json").unwrap();
        let (body, tokens) = metric_body(42, 7, 0, 0).unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(metric_names(&golden), metric_names(&value));
        let point = &value["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0];
        assert_eq!(point["name"], "claude_code.token.usage");
        assert_eq!(point["sum"]["aggregationTemporality"], 1);
        assert!(point["sum"]["dataPoints"][0].get("attributes").is_none());
        assert_eq!(
            value["resourceMetrics"][0]["resource"]["attributes"][1]["value"]["stringValue"],
            "soak-7"
        );
        assert_eq!(point["sum"]["dataPoints"][0]["asInt"], tokens.to_string());

        let mut sum = 0u64;
        for agent in 0..2 {
            for seq in 0..30 {
                let delta = token_delta(42, agent, seq);
                assert!((1..=16).contains(&delta));
                sum += delta;
            }
        }
        let again = (0..2)
            .map(|agent| (0..30).map(|seq| token_delta(42, agent, seq)).sum::<u64>())
            .sum::<u64>();
        assert_eq!(sum, again);

        let logs = load_golden("logs.json").unwrap();
        let log: Value = serde_json::from_slice(&log_body(42, 3, 1, 50).unwrap()).unwrap();
        assert_eq!(
            logs["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["eventName"],
            log["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["eventName"]
        );
        assert_eq!(
            log["resourceLogs"][0]["resource"]["attributes"][0]["value"]["stringValue"],
            "soak-3"
        );
    }

    #[test]
    fn schedule_is_unique_and_counts_match_rate() {
        let events = agent_events(3, 10, 1_000_000_000, 30).unwrap();
        let logs: Vec<u64> = events
            .iter()
            .filter(|e| e.kind == Kind::Log)
            .map(|e| e.offset_ns)
            .collect();
        let metrics: Vec<u64> = events
            .iter()
            .filter(|e| e.kind == Kind::Metric)
            .map(|e| e.offset_ns)
            .collect();
        assert_eq!(logs.len(), 30);
        assert_eq!(metrics, vec![3_000_000_000, 13_000_000_000, 23_000_000_000]);
        let mut times = logs.clone();
        times.sort_unstable();
        times.dedup();
        assert_eq!(times.len(), logs.len());
        let agent0 = agent_events(0, 2, 1_000_000_000, 10).unwrap();
        assert_eq!(agent0.iter().filter(|e| e.kind == Kind::Metric).count(), 1);
        assert_eq!(agent0.iter().filter(|e| e.kind == Kind::Log).count(), 10);
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let values: Vec<f64> = (1..=100).map(|n| n as f64).collect();
        assert_eq!(percentile(&values, 50.0), Some(50.0));
        assert_eq!(percentile(&values, 95.0), Some(95.0));
        assert_eq!(percentile(&values, 99.0), Some(99.0));
        assert_eq!(percentile(&[7.0], 99.0), Some(7.0));
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn parse_requires_load_arguments() {
        let err = parse_args(&[
            "ti-bench".into(),
            "otlp-soak".into(),
            "--agents".into(),
            "2".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--rate"), "{err}");
        let err = parse_args(&[
            "ti-bench".into(),
            "otlp-soak".into(),
            "--agents".into(),
            "2".into(),
            "--rate".into(),
            "1".into(),
            "--duration".into(),
            "10".into(),
            "--out".into(),
            "out.json".into(),
        ])
        .unwrap_err();
        assert!(err.contains("--url") || err.contains("--spawn"), "{err}");
    }

    #[test]
    fn spawn_smoke_two_agents_ten_seconds() {
        let Ok(bin) = resolve_lume(None) else {
            eprintln!(
                "otlp-soak smoke skipped: lume binary not found (set LUME_BIN or build --features ti --bin lume)"
            );
            return;
        };
        let out = std::env::temp_dir().join(format!("otlp-soak-smoke-{}.json", std::process::id()));
        let cfg = SoakConfig {
            url: None,
            token_file: None,
            agents: 2,
            rate: 1.0,
            duration_s: 10,
            seed: 42,
            out: out.clone(),
            spawn: true,
            lume_bin: Some(bin),
        };
        let report = execute(&cfg).unwrap_or_else(|e| panic!("{e}"));
        let _ = fs::remove_file(&out);
        assert!(report.sql.ok, "{report:?}");
        assert_eq!(report.sent.log_posts, 20);
        assert_eq!(report.sent.metrics_posts, 2);
        assert_eq!(report.sent.log_2xx, 20, "{report:?}");
        assert_eq!(report.sent.metrics_2xx, 2, "{report:?}");
        assert_eq!(report.sql.docs, 20);
        assert_eq!(report.sql.logs_sent, 20);
        assert!((report.sql.token_totals - report.sql.tokens_sent as f64).abs() < 1e-3);
    }
}
