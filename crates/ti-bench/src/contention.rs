//! HTTP query load alongside passive Signal K arrival sampling.
use crate::harness::{GoldenCorpus, PI_QUERY_IDS};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{client::IntoClientRequest, Message};

const CORPUS: &str = include_str!("../../../tests/golden/corpus.json");
const MAX_SAMPLES: usize = 1_000_000;
const MAX_SERIES: usize = 4096;
const MAX_REPLY: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Options {
    pub ti_url: String,
    pub signalk: String,
    pub duration: u64,
    pub baseline_secs: u64,
    pub concurrency: usize,
    pub queries: String,
    pub vessel: Option<String>,
    pub window: String,
    pub allow_empty: bool,
    pub token_file: Option<PathBuf>,
    pub out: PathBuf,
}
impl Options {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self {
            ti_url: "http://127.0.0.1:5863".into(),
            signalk: "ws://127.0.0.1:3000/signalk/v1/stream?subscribe=self".into(),
            duration: 600,
            baseline_secs: 120,
            concurrency: 1,
            queries: "Q4,Q8".into(),
            vessel: None,
            window: "last:7d".into(),
            allow_empty: false,
            token_file: None,
            out: PathBuf::new(),
        };
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--allow-empty" {
                options.allow_empty = true;
                i += 1;
                continue;
            }
            let pair = &args[i..];
            let value = pair
                .get(1)
                .ok_or_else(|| format!("{} requires a value", pair[0]))?;
            match pair[0].as_str() {
                "--ti-url" => options.ti_url = value.clone(),
                "--signalk" => options.signalk = value.clone(),
                "--duration" => {
                    options.duration = value.parse().map_err(|_| "invalid --duration")?
                }
                "--baseline-secs" => {
                    options.baseline_secs = value.parse().map_err(|_| "invalid --baseline-secs")?
                }
                "--concurrency" => {
                    options.concurrency = value.parse().map_err(|_| "invalid --concurrency")?
                }
                "--queries" => options.queries = value.clone(),
                "--vessel" => options.vessel = Some(value.clone()),
                "--window" => options.window = value.clone(),
                "--token-file" => options.token_file = Some(value.into()),
                "--out" => options.out = value.into(),
                other => return Err(format!("unknown contention option {other}")),
            }
            i += 2;
        }
        if options.duration == 0
            || options.duration > 86400
            || options.baseline_secs > 86400
            || options.concurrency == 0
            || options.concurrency > 64
            || options.out.as_os_str().is_empty()
        {
            return Err(
                "--out required; duration 1..86400, baseline 0..86400, concurrency 1..64".into(),
            );
        }
        Endpoint::parse(&options.ti_url, "http")?;
        Endpoint::parse(&options.signalk, "ws")?;
        select_queries(&options.queries)?;
        Ok(options)
    }
}
#[derive(Clone, Debug)]
pub struct Query {
    pub id: String,
    pub class: String,
    pub sql: String,
    pub original_sql: String,
}
pub fn select_queries(names: &str) -> Result<Vec<Query>, String> {
    let corpus: GoldenCorpus = serde_json::from_str(CORPUS).map_err(|e| e.to_string())?;
    let names: BTreeSet<_> = names
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        return Err("--queries must name a class or committed query id".into());
    }
    let queries: Vec<_> = corpus
        .entries
        .into_iter()
        .filter(|e| e.exclude.is_none() && PI_QUERY_IDS.contains(&e.id.as_str()))
        .filter(|e| names.contains(e.id.as_str()) || names.contains(e.qclass.as_str()))
        .map(|e| Query {
            id: e.id,
            class: e.qclass,
            original_sql: e.ti_sql.clone(),
            sql: e.ti_sql,
        })
        .collect();
    for name in names {
        if !queries.iter().any(|q| q.id == name || q.class == name) {
            return Err(format!("no committed benchmark queries match {name}"));
        }
    }
    Ok(queries)
}

#[derive(Clone)]
struct Endpoint {
    host: String,
    port: u16,
    target: String,
}
impl Endpoint {
    fn parse(url: &str, scheme: &str) -> Result<Self, String> {
        let uri: tungstenite::http::Uri = url.parse().map_err(|_| "invalid endpoint URL")?;
        if uri.scheme_str() != Some(scheme)
            || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        {
            return Err(format!(
                "expected {scheme} URL without embedded credentials"
            ));
        }
        Ok(Self {
            host: uri
                .host()
                .ok_or("endpoint needs a host")?
                .trim_matches(['[', ']'])
                .into(),
            port: uri.port_u16().unwrap_or(80),
            target: uri.path_and_query().map_or("/", |p| p.as_str()).into(),
        })
    }
    fn connect(&self, timeout: Duration) -> Result<TcpStream, String> {
        let addr = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|_| "endpoint address resolution failed")?
            .next()
            .ok_or("endpoint has no address")?;
        let stream =
            TcpStream::connect_timeout(&addr, timeout).map_err(|_| "endpoint connect failed")?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| e.to_string())?;
        Ok(stream)
    }
}
fn request_http(
    endpoint: &Endpoint,
    route: &str,
    body: Option<&str>,
    token: Option<&str>,
    timeout: Duration,
) -> Result<Value, String> {
    let mut stream = endpoint.connect(timeout)?;
    let method = if body.is_some() { "POST" } else { "GET" };
    let body = body.unwrap_or("");
    let authority = if endpoint.host.contains(':') {
        format!("[{}]:{}", endpoint.host, endpoint.port)
    } else {
        format!("{}:{}", endpoint.host, endpoint.port)
    };
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    write!(stream, "{method} {}{route} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nAccept: application/json\r\nConnection: close\r\n{auth}Content-Length: {}\r\n\r\n{body}",
        endpoint.target.trim_end_matches('/'), body.len()).map_err(|_| "HTTP write failed")?;
    let mut bytes = Vec::new();
    stream
        .take(MAX_REPLY as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "HTTP read failed or timed out")?;
    if bytes.len() > MAX_REPLY {
        return Err("HTTP reply exceeds 1 MiB".into());
    }
    let boundary = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("invalid HTTP reply")?;
    let headers = std::str::from_utf8(&bytes[..boundary]).map_err(|_| "invalid HTTP headers")?;
    let status = headers
        .split_whitespace()
        .nth(1)
        .ok_or("missing HTTP status")?;
    if status != "200" {
        return Err(format!("HTTP status {status}"));
    }
    serde_json::from_slice(&bytes[boundary + 4..]).map_err(|_| "invalid HTTP JSON reply".into())
}
fn query_http(
    endpoint: &Endpoint,
    query: &Query,
    token: Option<&str>,
    timeout: Duration,
) -> Result<Value, String> {
    let body = json!({"sql":query.sql, "max_rows":500}).to_string();
    let value = request_http(endpoint, "/ti/query", Some(&body), token, timeout)?;
    if value.get("rows").and_then(Value::as_array).is_none() || value.get("error").is_some() {
        return Err("query reply lacks rows or reports an error".into());
    }
    Ok(value)
}
fn quote(value: &str) -> String {
    value.replace('\'', "''")
}
fn parse_time(value: &str) -> Result<DateTime<Utc>, String> {
    if let Ok(time) = DateTime::parse_from_rfc3339(value) {
        return Ok(time.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(time) = NaiveDateTime::parse_from_str(value, format) {
            return Ok(time.and_utc());
        }
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc())
        .ok_or_else(|| format!("invalid UTC window timestamp {value}"))
}
fn window_bounds(
    window: &str,
    latest: DateTime<Utc>,
    width: u64,
) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
    let (start, end) = if let Some(last) = window.strip_prefix("last:") {
        let number = last
            .get(..last.len().saturating_sub(1))
            .ok_or("invalid last window")?;
        let factor = match last.as_bytes().last() {
            Some(b's') => 1,
            Some(b'm') => 60,
            Some(b'h') => 3600,
            Some(b'd') => 86400,
            _ => return Err("last window expects positive duration with s/m/h/d".into()),
        };
        let seconds = number
            .parse::<i64>()
            .ok()
            .and_then(|n| n.checked_mul(factor))
            .filter(|n| *n > 0 && *n <= 315360000)
            .ok_or("last window must be positive and at most ten years")?;
        let end = latest
            .checked_add_signed(chrono::Duration::seconds(width as i64))
            .ok_or("window overflow")?;
        (
            end.checked_sub_signed(chrono::Duration::seconds(seconds))
                .ok_or("window overflow")?,
            end,
        )
    } else {
        let (start, end) = window
            .split_once('/')
            .ok_or("--window expects start/end or last:7d")?;
        (parse_time(start)?, parse_time(end)?)
    };
    if start >= end {
        return Err("window start must precede end".into());
    }
    Ok((start, end))
}
fn stamp(time: DateTime<Utc>) -> String {
    time.format("%Y-%m-%d %H:%M:%S").to_string()
}
/// Rewrites only the committed query shapes. Adds upper bounds even where the corpus
/// has an open-ended predicate; aggregate/grouping expressions stay unchanged.
pub fn rewrite_query(
    query: &Query,
    vessel: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    width: u64,
) -> Result<Query, String> {
    let start_text = stamp(start);
    let end_text = stamp(end);
    let point_text = stamp(end - chrono::Duration::seconds(width as i64));
    let mut sql = query
        .original_sql
        .replace("vessels.urn:mrn:imo:mmsi:367000000", &quote(vessel));
    let mut offset = 0;
    while let Some(relative) = sql[offset..].find("TIMESTAMP '") {
        let marker = offset + relative;
        let begin = marker + "TIMESTAMP '".len();
        let finish = begin
            + sql[begin..]
                .find('\'')
                .ok_or("unterminated timestamp literal")?;
        let before = sql[..marker].trim_end();
        let replacement = if before.ends_with('<') || before.ends_with("<=") {
            &end_text
        } else if before.ends_with('=') && !before.ends_with(">=") {
            &point_text
        } else {
            &start_text
        };
        sql.replace_range(begin..finish, replacement);
        offset = begin + replacement.len() + 1;
    }
    if query.class == "Q5" {
        let begin = sql.find("intervals(").ok_or("missing intervals call")? + "intervals(".len();
        let begin = begin
            + sql[begin..]
                .find('\'')
                .ok_or("missing interval predicate")?
            + 1;
        let bytes = sql.as_bytes();
        let mut finish = begin;
        while finish < bytes.len() {
            if bytes[finish] == b'\'' {
                if bytes.get(finish + 1) == Some(&b'\'') {
                    finish += 2;
                    continue;
                }
                break;
            }
            finish += 1;
        }
        if finish == bytes.len() {
            return Err("unterminated interval predicate".into());
        }
        let predicate = format!(
            " AND vessel = '{}' AND ts >= TIMESTAMP '{}' AND ts < TIMESTAMP '{}'",
            quote(vessel),
            start_text,
            end_text
        );
        sql.insert_str(finish, &quote(&predicate));
        // The only old outer condition in this committed class is q5-001's start.
        if let Some(outer) = sql.find("\nWHERE start") {
            sql.truncate(outer);
        }
        sql.push_str(&format!(
            "\nWHERE vessel = '{}' AND start >= TIMESTAMP '{}' AND start < TIMESTAMP '{}'",
            quote(vessel),
            start_text,
            end_text
        ));
    } else {
        let time_column = if query.id == "q6-004" { "t.ts" } else { "ts" };
        let boundary = ["\nGROUP BY", "\nORDER BY"]
            .iter()
            .filter_map(|word| sql.find(word))
            .min()
            .unwrap_or(sql.len());
        sql.insert_str(boundary,&format!(" AND {time_column} >= TIMESTAMP '{start_text}' AND {time_column} < TIMESTAMP '{end_text}'\n"));
    }
    let mut rewritten = query.clone();
    rewritten.sql = sql;
    Ok(rewritten)
}
fn probe(endpoint: &Endpoint, sql: String, token: Option<&str>) -> Result<Value, String> {
    query_http(
        endpoint,
        &Query {
            id: "probe".into(),
            class: "probe".into(),
            original_sql: sql.clone(),
            sql,
        },
        token,
        Duration::from_secs(30),
    )
}
fn prepare(
    options: &Options,
    queries: &[Query],
    endpoint: &Endpoint,
    token: Option<&str>,
) -> Result<(Vec<Query>, Value), String> {
    let vessel = if let Some(vessel) = &options.vessel {
        vessel.clone()
    } else {
        let rows = probe(endpoint,"SELECT vessel, count(*) AS n FROM telemetry GROUP BY vessel ORDER BY n DESC, vessel LIMIT 1".into(),token)?;
        rows["rows"][0]["vessel"]
            .as_str()
            .ok_or_else(|| {
                format!("no vessel in telemetry; cannot resolve contention workload: {rows}")
            })?
            .to_string()
    };
    let latest = probe(
        endpoint,
        format!(
            "SELECT max(ts) AS newest FROM telemetry WHERE vessel = '{}'",
            quote(&vessel)
        ),
        token,
    )?;
    let latest = latest["rows"][0]["newest"]
        .as_str()
        .ok_or("selected vessel has no timestamp")?;
    let status = request_http(endpoint, "/ti/status", None, token, Duration::from_secs(30))?;
    let width = status["width_seconds"]
        .as_u64()
        .filter(|w| *w > 0 && *w <= 86400)
        .ok_or("invalid store bucket width")?;
    let (start, end) = window_bounds(&options.window, parse_time(latest)?, width)?;
    let mut rewritten = Vec::new();
    let mut counts = BTreeMap::new();
    for query in queries {
        let query = rewrite_query(query, &vessel, start, end, width)?;
        let reply = query_http(endpoint, &query, token, Duration::from_secs(30))
            .map_err(|e| format!("preflight {}: {e}", query.id))?;
        let rows = reply["rows"].as_array().expect("validated rows").len();
        if rows == 0 && !options.allow_empty {
            return Err(format!("EMPTY_RESULT: {} returned 0 rows for vessel {vessel}, window {start}/{end}; verify paths/predicates or use --allow-empty",query.id));
        }
        counts.insert(
            query.id.clone(),
            json!({"rows":rows,"truncated":reply["truncated"]}),
        );
        rewritten.push(query);
    }
    Ok((
        rewritten,
        json!({"vessel":vessel,"window":{"requested":options.window,"start":start.to_rfc3339(),"end":end.to_rfc3339(),
        "bucket_width_seconds":width},"preflight":counts,"allow_empty":options.allow_empty}),
    ))
}

fn quantile(sorted: &[f64], fraction: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let index = (sorted.len() as f64 * fraction).ceil() as usize;
    Some(sorted[index.saturating_sub(1).min(sorted.len() - 1)])
}
pub fn distribution(values: &[f64]) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({"n":sorted.len(), "p50":quantile(&sorted, 0.5), "p95":quantile(&sorted, 0.95),
        "p99":quantile(&sorted, 0.99), "max":sorted.last()})
}
#[derive(Default)]
struct PathSamples {
    context: String,
    source: String,
    path: String,
    last: Option<f64>,
    gaps: Vec<f64>,
    latencies: Vec<f64>,
    samples: usize,
    invalid_timestamps: usize,
}
impl PathSamples {
    fn observe(&mut self, receipt: f64, latency: Option<f64>) {
        if let Some(last) = self.last {
            self.gaps.push((receipt - last).max(0.0) * 1000.0);
        }
        self.last = Some(receipt);
        self.samples += 1;
        if let Some(latency) = latency {
            self.latencies.push(latency);
        }
    }
    fn report(&self, end: f64) -> Value {
        let gaps = distribution(&self.gaps);
        let median = gaps["p50"].as_f64();
        let drops = median.map(|m| self.gaps.iter().filter(|g| **g > 2.0 * m).count());
        json!({"context":self.context, "source":self.source, "path":self.path, "samples":self.samples,
            "gaps_ms":gaps, "drops":drops, "drop_threshold_ms":median.map(|m| 2.0*m),
            "timestamp_latency_ms":distribution(&self.latencies), "invalid_timestamps":self.invalid_timestamps,
            "negative_timestamp_latencies":self.latencies.iter().filter(|v| **v < 0.0).count(),
            "silence_at_end_ms":self.last.map(|last| (end-last).max(0.0)*1000.0)})
    }
}
#[derive(Default)]
struct PhaseSamples {
    paths: BTreeMap<String, PathSamples>,
    values: usize,
    malformed: usize,
}
impl PhaseSamples {
    fn delta(&mut self, value: &Value, receipt: f64, receipt_unix_ms: f64) -> Result<(), String> {
        let context = value["context"].as_str().unwrap_or("unknown");
        for update in value["updates"].as_array().into_iter().flatten() {
            let source = update["$source"]
                .as_str()
                .or_else(|| update["source"]["label"].as_str())
                .unwrap_or("unknown");
            let stamp = update["timestamp"].as_str();
            let latency = stamp
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|t| receipt_unix_ms - t.timestamp_micros() as f64 / 1000.0);
            let mut observed = BTreeSet::new();
            for v in update["values"].as_array().into_iter().flatten() {
                let Some(path) = v["path"].as_str() else {
                    continue;
                };
                if !observed.insert(path) {
                    continue;
                }
                if self.values >= MAX_SAMPLES
                    || (self.paths.len() >= MAX_SERIES
                        && !self
                            .paths
                            .contains_key(&format!("{context}|{source}|{path}")))
                {
                    return Err(
                        "Signal K sample capacity exceeded (1M values / 4096 series); run invalid"
                            .into(),
                    );
                }
                let entry = self
                    .paths
                    .entry(format!("{context}|{source}|{path}"))
                    .or_insert_with(|| PathSamples {
                        context: context.into(),
                        source: source.into(),
                        path: path.into(),
                        ..Default::default()
                    });
                entry.observe(receipt, latency);
                entry.invalid_timestamps += usize::from(stamp.is_some() && latency.is_none());
                self.values += 1;
            }
        }
        Ok(())
    }
    fn report(&self, end: f64) -> Value {
        json!({"values":self.values,"malformed_messages":self.malformed,
            "paths":self.paths.iter().map(|(key,p)| (key.clone(),p.report(end))).collect::<BTreeMap<_,_>>()})
    }
}
#[derive(Default)]
struct QuerySamples {
    durations: Vec<f64>,
    rows_total: usize,
    empty_results: usize,
    errors: BTreeMap<String, usize>,
}
fn compare(baseline: &Value, load: &Value) -> Value {
    let mut paths = BTreeMap::new();
    if let Some(base) = baseline["paths"].as_object() {
        for (key, b) in base {
            let l = &load["paths"][key];
            let change = |field: &str| {
                b[field]["p95"]
                    .as_f64()
                    .zip(l[field]["p95"].as_f64())
                    .filter(|(before, _)| *before > 0.0)
                    .map(|(before, after)| (after / before - 1.0) * 100.0)
            };
            paths.insert(
                key.clone(),
                json!({"missing_under_load":l.is_null(),
                "gap_p95_change_percent":change("gaps_ms"),
                "timestamp_latency_p95_change_percent":change("timestamp_latency_ms"),
                "baseline_drops":b["drops"], "load_drops":l["drops"]}),
            );
        }
    }
    json!(paths)
}
fn token(path: Option<&PathBuf>) -> Result<Option<String>, String> {
    path.map(|p| {
        let t = std::fs::read_to_string(p).map_err(|_| "cannot read bearer token file")?;
        let t = t.trim();
        if t.is_empty() || t.contains(['\r', '\n']) {
            return Err("token must be a nonempty single line".into());
        }
        Ok(t.to_string())
    })
    .transpose()
}
/// Run passive baseline then concurrent query load. The caller supplies only committed queries.
pub fn measure(options: &Options, queries: &[Query]) -> Result<Value, String> {
    if queries.is_empty() {
        return Err("no queries to measure".into());
    }
    let bearer = token(options.token_file.as_ref())?;
    let endpoint = Endpoint::parse(&options.ti_url, "http")?;
    let (queries, resolved) = prepare(options, queries, &endpoint, bearer.as_deref())?;
    let ws_endpoint = Endpoint::parse(&options.signalk, "ws")?;
    let mut request = options
        .signalk
        .as_str()
        .into_client_request()
        .map_err(|_| "invalid WebSocket URL")?;
    if let Some(t) = &bearer {
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {t}")
                .parse()
                .map_err(|_| "invalid bearer token header")?,
        );
    }
    let stream = ws_endpoint.connect(Duration::from_secs(5))?;
    let (mut ws, _) =
        tungstenite::client(request, stream).map_err(|_| "Signal K WebSocket handshake failed")?;
    ws.get_ref()
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let baseline_end = start + Duration::from_secs(options.baseline_secs);
    let load_end = baseline_end + Duration::from_secs(options.duration);
    eprintln!(
        "Contention baseline: {} s, no query load; then {} s load, concurrency {}",
        options.baseline_secs, options.duration, options.concurrency
    );
    let cancelled = Arc::new(AtomicBool::new(false));
    let observer_cancelled = cancelled.clone();
    let observer = thread::spawn(move || -> Result<(PhaseSamples, PhaseSamples), String> {
        let result = (|| -> Result<(PhaseSamples, PhaseSamples), String> {
            let mut baseline = PhaseSamples::default();
            let mut load = PhaseSamples::default();
            while Instant::now() < load_end && !observer_cancelled.load(Ordering::Relaxed) {
                match ws.read() {
                    Ok(Message::Text(text)) => {
                        let now = Instant::now();
                        if now >= load_end {
                            break;
                        }
                        let phase = if now < baseline_end {
                            &mut baseline
                        } else {
                            &mut load
                        };
                        match serde_json::from_str::<Value>(&text) {
                            Ok(value) => phase.delta(
                                &value,
                                now.duration_since(start).as_secs_f64(),
                                Utc::now().timestamp_micros() as f64 / 1000.0,
                            )?,
                            Err(_) => phase.malformed += 1,
                        }
                    }
                    Ok(Message::Ping(_)) => ws.flush().map_err(|_| "Signal K pong failed")?,
                    Ok(Message::Close(_)) => {
                        return Err("Signal K WebSocket closed before the run ended".into())
                    }
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e))
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(_) => return Err("Signal K WebSocket failed before the run ended".into()),
                }
            }
            let _ = ws.close(None);
            Ok((baseline, load))
        })();
        if result.is_err() {
            observer_cancelled.store(true, Ordering::Relaxed);
        }
        result
    });
    while Instant::now() < baseline_end && !cancelled.load(Ordering::Relaxed) {
        thread::sleep(
            baseline_end
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(100)),
        );
    }
    eprintln!("Contention load starting; pan and zoom OpenCPN now");
    let next = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for _ in 0..options.concurrency {
        let queries = queries.to_vec();
        let endpoint = endpoint.clone();
        let bearer = bearer.clone();
        let next = next.clone();
        let cancelled = cancelled.clone();
        workers.push(thread::spawn(move || {
            let mut samples: BTreeMap<String, QuerySamples> = BTreeMap::new();
            let mut attempts = 0usize;
            while Instant::now() < load_end && !cancelled.load(Ordering::Relaxed) {
                if attempts >= MAX_SAMPLES {
                    cancelled.store(true, Ordering::Relaxed);
                    return Err(
                        "query sample capacity exceeded (1M attempts per worker); run invalid"
                            .to_string(),
                    );
                }
                attempts += 1;
                let q = &queries[next.fetch_add(1, Ordering::Relaxed) % queries.len()];
                let started = Instant::now();
                let timeout = load_end
                    .saturating_duration_since(started)
                    .min(Duration::from_secs(30));
                if timeout.is_zero() {
                    break;
                }
                let result = query_http(&endpoint, q, bearer.as_deref(), timeout);
                let entry = samples.entry(q.id.clone()).or_default();
                entry
                    .durations
                    .push(started.elapsed().as_secs_f64() * 1000.0);
                match result {
                    Ok(reply) => {
                        let rows = reply["rows"].as_array().expect("validated rows").len();
                        entry.rows_total += rows;
                        entry.empty_results += usize::from(rows == 0);
                    }
                    Err(error) => {
                        *entry.errors.entry(error).or_default() += 1;
                    }
                }
            }
            Ok(samples)
        }));
    }
    let mut collected: BTreeMap<String, QuerySamples> = BTreeMap::new();
    let mut worker_error = None;
    for worker in workers {
        let samples = match worker.join() {
            Ok(Ok(samples)) => samples,
            failure => {
                cancelled.store(true, Ordering::Relaxed);
                worker_error = Some(match failure {
                    Ok(Err(error)) => error,
                    _ => "query worker panicked".into(),
                });
                continue;
            }
        };
        for (key, samples) in samples {
            let entry = collected.entry(key).or_default();
            entry.durations.extend(samples.durations);
            entry.rows_total += samples.rows_total;
            entry.empty_results += samples.empty_results;
            for (error, count) in samples.errors {
                *entry.errors.entry(error).or_default() += count;
            }
        }
    }
    let (baseline, load) = observer
        .join()
        .map_err(|_| "Signal K observer panicked")??;
    if let Some(error) = worker_error {
        return Err(error);
    }
    let base_json = baseline.report(options.baseline_secs as f64);
    let load_json = load.report((options.baseline_secs + options.duration) as f64);
    let query_json: Vec<_> = queries.iter().map(|q| {
        let empty = QuerySamples::default();
        let s = collected.get(&q.id).unwrap_or(&empty);
        json!({"id":q.id,"class":q.class,"original_sql":q.original_sql,"sql":q.sql,
            "rows_total":s.rows_total,"empty_results":s.empty_results,"latency_ms":distribution(&s.durations),
            "attempts":s.durations.len(),"errors":s.errors.values().sum::<usize>(),"error_counts":s.errors})
    }).collect();
    Ok(json!({
        "version":1, "started_at":(Utc::now()-chrono::Duration::milliseconds(start.elapsed().as_millis() as i64)).to_rfc3339(),
        "baseline_seconds":options.baseline_secs, "load_seconds":options.duration,"elapsed_seconds":start.elapsed().as_secs_f64(),
        "concurrency":options.concurrency,"resolved":resolved, "query_set":"committed 26-query PI_QUERY_IDS", "queries":query_json,
        "baseline":base_json, "with_load":load_json, "comparison":compare(&base_json,&load_json),
        "notes":[
            "Gaps use a monotonic receipt clock per context/source/path; phase-boundary gaps are excluded.",
            "drops counts observed inter-arrival gaps strictly greater than twice that phase's median period; it is not proof of lost packets.",
            "Timestamp latency is signed receipt wall time minus exporter timestamp; clock skew can bias it or make it negative. Synchronize clocks; compare only the same source.",
            "Query percentiles include errors and HTTP connection/response transfer; the served query row cap applies. Preflight and vessel/window probes run before the unloaded baseline.",
            "No OpenCPN stutter conclusion is automatic: record the user's pan/zoom observation separately.",
            "Null percentiles/drops mean insufficient observations, not a passing measurement."
        ]
    }))
}
pub fn run(args: &[String]) -> Result<(), String> {
    if args == ["--help"] || args == ["-h"] {
        println!("ti-query-bench contention --ti-url http://127.0.0.1:5863 --signalk ws://127.0.0.1:3000/signalk/v1/stream?subscribe=self --duration 600 --queries Q4,Q8 [--vessel URN] --window last:7d [--allow-empty] --baseline-secs 120 --concurrency 1 [--token-file P] --out FILE.json");
        return Ok(());
    }
    let options = Options::parse(args)?;
    let queries = select_queries(&options.queries)?;
    let report = measure(&options, &queries)?;
    if let Some(parent) = options.out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        &options.out,
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("Contention report: {}", options.out.display());
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synthetic_gaps_drops_and_signed_latency() {
        let mut path = PathSamples::default();
        for (t, l) in [
            (0.0, 10.0),
            (1.0, 20.0),
            (2.0, -5.0),
            (5.0, 30.0),
            (6.0, 15.0),
        ] {
            path.observe(t, Some(l));
        }
        let r = path.report(7.0);
        assert_eq!(r["gaps_ms"]["p50"], 1000.0);
        assert_eq!(r["gaps_ms"]["p95"], 3000.0);
        assert_eq!(r["gaps_ms"]["max"], 3000.0);
        assert_eq!(r["drops"], 1);
        assert_eq!(r["negative_timestamp_latencies"], 1);
        assert_eq!(r["silence_at_end_ms"], 1000.0);
        assert!(PathSamples::default().report(0.0)["drops"].is_null());
        assert_eq!(distribution(&[1.0, 2.0, 3.0, 4.0])["p50"], 2.0);
    }
    #[test]
    fn selection_and_options_reject_unknown_queries_and_zero_concurrency() {
        assert_eq!(select_queries("Q4,Q8").unwrap().len(), 6);
        assert!(select_queries("Q99").is_err());
        let args = ["--out", "out.json", "--concurrency", "0"].map(str::to_string);
        assert!(Options::parse(&args).is_err());
        assert!(Endpoint::parse("http://user:secret@127.0.0.1", "http").is_err());
        assert!(Endpoint::parse("https://127.0.0.1", "http").is_err());
    }
    #[test]
    fn all_committed_queries_rewrite_live_vessel_and_october_window() {
        let start = parse_time("2026-10-01").unwrap();
        let end = parse_time("2026-10-08").unwrap();
        let queries = select_queries("Q1,Q2,Q3,Q4,Q5,Q6,Q7,Q8").unwrap();
        assert_eq!(queries.len(), 26);
        for query in queries {
            let rewritten = rewrite_query(&query, "agent.urn:test", start, end, 10).unwrap();
            use ti_sql::datafusion::sql::sqlparser::{dialect::PostgreSqlDialect, parser::Parser};
            Parser::parse_sql(&PostgreSqlDialect {}, &rewritten.sql)
                .unwrap_or_else(|error| panic!("{}: {error}\n{}", query.id, rewritten.sql));
            assert_eq!(rewritten.original_sql, query.sql);
            assert!(!rewritten.sql.contains("2026-05"), "{}", rewritten.sql);
            assert!(!rewritten.sql.contains("367000000"), "{}", rewritten.sql);
            assert!(rewritten.sql.contains("2026-10-01"), "{}", rewritten.sql);
            assert!(rewritten.sql.contains("2026-10-08"), "{}", rewritten.sql);
        }
        let (start, end) =
            window_bounds("last:7d", parse_time("2026-10-08T00:00:00Z").unwrap(), 10).unwrap();
        assert_eq!((end - start).num_seconds(), 604800);
        assert_eq!(
            end.timestamp(),
            parse_time("2026-10-08T00:00:10Z").unwrap().timestamp()
        );
        assert!(window_bounds("last:0d", end, 10).is_err());
    }
}
