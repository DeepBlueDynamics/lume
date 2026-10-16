//! Lume decide client for Addita / Ollaya (`POST /api/decide`), run logging
//! (`<store>/agent_runs/YYYY-MM-DD.jsonl`), and BM25 few-shot SQL example selection.

use crate::bm25::{Bm25Index, Bm25Params, SearchVariant, Section};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEFAULT_DECIDE_MODEL: &str = "laya";
pub const DEFAULT_DECIDE_THRESHOLD: f64 = 0.8;
pub const DEFAULT_DECIDE_PRESET: &str = "lume.sql_judge";
pub const DEFAULT_DECIDE_TIMEOUT_SECS: u64 = 30;
pub const MAX_STATE_ROWS: usize = 20;
pub const MAX_STATE_BYTES: usize = 16 * 1024;
pub const RETENTION_DAYS: u64 = 30;
pub const EXAMPLES_MAX_DAYS: u64 = 90;
pub const EXAMPLES_MAX_LINES: usize = 2000;
pub const EXAMPLES_TOP_K: usize = 3;

/// Configuration for Addita / Ollaya SQL judging, run logging, and example injection.
#[derive(Debug, Clone)]
pub struct DecideConfig {
    pub decide_url: Option<String>,
    pub decide_model: String,
    pub decide_token_file: Option<PathBuf>,
    pub decide_threshold: f64,
    pub decide_preset: Option<String>,
    pub no_decide: bool,
    pub no_agent_log: bool,
    pub no_examples: bool,
    pub discovery_file: Option<PathBuf>,
    pub timeout: Duration,
}

impl Default for DecideConfig {
    fn default() -> Self {
        let timeout_secs = std::env::var("LUME_DECIDE_TIMEOUT")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_DECIDE_TIMEOUT_SECS);
        Self {
            decide_url: std::env::var("LUME_DECIDE_URL")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            decide_model: DEFAULT_DECIDE_MODEL.to_string(),
            decide_token_file: std::env::var("LUME_DECIDE_TOKEN_FILE")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            decide_threshold: DEFAULT_DECIDE_THRESHOLD,
            decide_preset: None,
            no_decide: false,
            no_agent_log: false,
            no_examples: false,
            discovery_file: std::env::var("LUME_ADDITA_DISCOVERY_FILE")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

static CLI_DECIDE_CONFIG: OnceLock<Mutex<Option<DecideConfig>>> = OnceLock::new();

fn cli_decide_cell() -> &'static Mutex<Option<DecideConfig>> {
    CLI_DECIDE_CONFIG.get_or_init(|| Mutex::new(None))
}

/// Sets the process-wide [`DecideConfig`] parsed by `lume agent` / `lume chat`.
pub fn set_cli_decide_config(config: DecideConfig) {
    *cli_decide_cell().lock().unwrap() = Some(config);
}

/// Returns the active [`DecideConfig`] set by the CLI, or [`DecideConfig::default()`].
pub fn current_decide_config() -> DecideConfig {
    cli_decide_cell()
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default()
}

/// Reader shape for `~/.addita/serve.json`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AdditaServeDiscovery {
    pub url: Option<String>,
    pub token_file: Option<String>,
}

pub fn default_addita_discovery_path() -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os("LUME_ADDITA_DISCOVERY_FILE") {
        if !custom.is_empty() {
            return Some(PathBuf::from(custom));
        }
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".addita").join("serve.json"))
}

pub fn read_addita_discovery(path: &Path) -> Option<AdditaServeDiscovery> {
    let raw = fs::read_to_string(path).ok()?;
    let parsed: AdditaServeDiscovery = serde_json::from_str(&raw).ok()?;
    let url = parsed.url.as_deref()?.trim();
    if url.is_empty() {
        return None;
    }
    Some(parsed)
}

impl DecideConfig {
    /// Resolves `(base_url, token_file)` from explicit CLI/env settings or `~/.addita/serve.json`.
    pub fn resolve_endpoint_and_token_file(&self) -> Option<(String, Option<PathBuf>)> {
        if self.no_decide {
            return None;
        }
        if let Some(raw) = self
            .decide_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some((normalize_base_url(raw), self.decide_token_file.clone()));
        }
        let disc_path = self
            .discovery_file
            .clone()
            .or_else(default_addita_discovery_path)?;
        let disc = read_addita_discovery(&disc_path)?;
        let url = normalize_base_url(disc.url.as_deref()?);
        let token_file = self.decide_token_file.clone().or_else(|| {
            disc.token_file
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        });
        Some((url, token_file))
    }

    /// Returns the normalized decide endpoint URL if judging is enabled.
    pub fn effective_decide_url(&self) -> Option<String> {
        self.resolve_endpoint_and_token_file().map(|(url, _)| url)
    }
}

pub fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

/// Summary of the decide judgment stored in each `agent_runs/YYYY-MM-DD.jsonl` record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecideLogBlock {
    /// `true`, `false`, or `"pending"`.
    pub judged: Value,
    pub good: bool,
    #[serde(default)]
    pub noul: BTreeMap<String, f64>,
    #[serde(rename = "final", default)]
    pub final_decisions: BTreeMap<String, bool>,
    #[serde(default)]
    pub routing_model: Option<String>,
    #[serde(default)]
    pub addita_id: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub teacher: Option<String>,
    #[serde(default)]
    pub teacher_model: Option<String>,
    #[serde(default)]
    pub teacher_host: Option<String>,
    #[serde(default)]
    pub student_ms: Option<u64>,
    #[serde(default)]
    pub teacher_ms: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
}

impl Default for DecideLogBlock {
    fn default() -> Self {
        Self {
            judged: json!(false),
            good: false,
            noul: BTreeMap::new(),
            final_decisions: BTreeMap::new(),
            routing_model: None,
            addita_id: None,
            source: None,
            teacher: None,
            teacher_model: None,
            teacher_host: None,
            student_ms: None,
            teacher_ms: None,
            error: None,
            error_code: None,
        }
    }
}

/// One line in `<store>/agent_runs/YYYY-MM-DD.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunRecord {
    pub ts: String,
    #[serde(rename = "ref")]
    pub run_ref: String,
    pub question: String,
    pub sql: Vec<String>,
    pub rows_sample: Vec<Value>,
    pub answer: String,
    pub model: String,
    pub decide: DecideLogBlock,
}

/// State object sent to `POST /api/decide` (matches `lume.sql_judge` preset).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecideState {
    pub question: String,
    pub sql: String,
    pub sql_all: Vec<String>,
    pub rows: Vec<Value>,
    pub rows_truncated: bool,
    pub error: Option<String>,
    pub answer: String,
}

/// Builds a bounded `state` object (`<= 20` rows, total serialized state `<= 16 KB`).
/// Sets `rows_truncated = true` when rows or text fields had to be truncated.
pub fn build_bounded_state(
    question: &str,
    sql: &str,
    sql_all: &[String],
    rows: &[Value],
    already_truncated: bool,
    error: Option<&str>,
    answer: &str,
) -> DecideState {
    let mut rows_truncated = already_truncated || rows.len() > MAX_STATE_ROWS;
    let mut bounded_rows: Vec<Value> = rows.iter().take(MAX_STATE_ROWS).cloned().collect();
    let mut state = DecideState {
        question: question.to_string(),
        sql: sql.to_string(),
        sql_all: sql_all.to_vec(),
        rows: bounded_rows.clone(),
        rows_truncated,
        error: error.map(str::to_string),
        answer: answer.to_string(),
    };

    while serde_json::to_vec(&state)
        .map(|b| b.len() > MAX_STATE_BYTES)
        .unwrap_or(false)
    {
        state.rows_truncated = true;
        rows_truncated = true;
        if bounded_rows.len() > 1 {
            bounded_rows.pop();
            state.rows = bounded_rows.clone();
        } else if bounded_rows.len() == 1 {
            if truncate_strings_in_value(&mut bounded_rows[0], 1024) {
                state.rows = bounded_rows.clone();
            } else {
                bounded_rows.clear();
                state.rows.clear();
            }
        } else if state.sql_all.len() > 1 {
            state.sql_all.pop();
        } else if state.answer.len() > 1024 {
            state.answer = truncate_utf8(&state.answer, state.answer.len() / 2);
        } else if state.sql.len() > 1024 {
            state.sql = truncate_utf8(&state.sql, state.sql.len() / 2);
        } else if let Some(ref err) = state.error {
            if err.len() > 512 {
                state.error = Some(truncate_utf8(err, err.len() / 2));
            } else {
                break;
            }
        } else {
            break;
        }
    }
    state.rows_truncated = rows_truncated;
    state
}

fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    let suffix = "...[truncated]";
    let budget = max_bytes.saturating_sub(suffix.len());
    let mut end = budget.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &s[..end])
}

fn truncate_strings_in_value(val: &mut Value, max_bytes: usize) -> bool {
    let mut changed = false;
    match val {
        Value::String(s) => {
            if s.len() > max_bytes {
                *s = truncate_utf8(s, max_bytes);
                changed = true;
            }
        }
        Value::Array(arr) => {
            for item in arr {
                if truncate_strings_in_value(item, max_bytes) {
                    changed = true;
                }
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                if truncate_strings_in_value(v, max_bytes) {
                    changed = true;
                }
            }
        }
        _ => {}
    }
    changed
}

/// Builds the inline `questions` definition matching `presets/lume.sql_judge.json`.
pub fn inline_questions() -> Value {
    json!({
        "rows_answer_question": {
            "type": "noul",
            "instructions": "Given state.question, state.sql, and state.rows, do the returned rows actually answer the user's question (right metric, right filter, right time window, non-empty when data is expected)?",
            "criteria": {
                "true": "The SQL and its returned rows directly answer what the user asked.",
                "false": "The rows are empty when they shouldn't be, answer a different question, or miss a requested filter or column."
            }
        },
        "answer_supported": {
            "type": "noul",
            "instructions": "If state.answer is present, is every number and claim in state.answer supported by state.rows without contradiction or invention? (If state.answer is null or absent, judge whether state.rows alone are sufficient to state an answer.)",
            "criteria": {
                "true": "Every figure and claim in the answer is grounded in state.rows.",
                "false": "The answer invents numbers, contradicts state.rows, or claims something the rows do not show."
            }
        }
    })
}

/// Builds the JSON request body for `POST /api/decide`.
pub fn build_decide_request_payload(
    config: &DecideConfig,
    run_ref: &str,
    state: &DecideState,
    is_addita: bool,
) -> Value {
    let mut payload = serde_json::Map::new();
    payload.insert("model".to_string(), json!(config.decide_model));
    if let Some(ref preset) = config.decide_preset {
        payload.insert("preset".to_string(), json!(preset));
    } else {
        payload.insert("questions".to_string(), inline_questions());
    }
    payload.insert(
        "state".to_string(),
        serde_json::to_value(state).unwrap_or_else(|_| json!({})),
    );
    if is_addita {
        payload.insert("addita".to_string(), json!({ "ref": run_ref }));
    }
    Value::Object(payload)
}

/// Probes `GET {base_url}/health` or `GET {base_url}/api/version` to check whether the server
/// identifies as `{"service":"addita"}`.
pub fn discover_is_addita(base_url: &str, token: Option<&str>, timeout: Duration) -> bool {
    let probe_timeout = timeout.min(Duration::from_secs(2));
    for suffix in ["/health", "/api/version"] {
        let url = format!("{base_url}{suffix}");
        let mut req = ureq::get(&url).timeout(probe_timeout);
        if let Some(tok) = token {
            if !tok.is_empty() {
                req = req.set("Authorization", &format!("Bearer {tok}"));
            }
        }
        let Ok(resp) = req.call() else {
            continue;
        };
        if resp.status() != 200 {
            continue;
        }
        let Ok(body) = resp.into_json::<Value>() else {
            continue;
        };
        if body.get("service").and_then(|v| v.as_str()) == Some("addita") {
            return true;
        }
    }
    false
}

/// Returns `true` if `raw_host` is loopback or private/local LAN (not a cloud endpoint).
pub fn is_local_or_lan_host(raw_host: &str) -> bool {
    let trimmed = raw_host.trim();
    if trimmed.is_empty() {
        return true;
    }
    let without_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or(bracketed)
    } else if authority.matches(':').count() == 1 {
        authority.split(':').next().unwrap_or(authority)
    } else {
        authority
    };
    let lower = host.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return true;
    }
    if matches!(
        lower.as_str(),
        "localhost" | "host.docker.internal" | "local" | "::1"
    ) || lower.ends_with(".localhost")
        || lower.ends_with(".local")
        || lower.ends_with(".lan")
        || lower.ends_with(".home.arpa")
    {
        return true;
    }
    if let Ok(ipv4) = lower.parse::<Ipv4Addr>() {
        let octets = ipv4.octets();
        return ipv4.is_loopback()
            || ipv4.is_private()
            || ipv4.is_link_local()
            || (octets[0] == 100 && (64..=127).contains(&octets[1]));
    }
    if let Ok(ipv6) = lower.parse::<Ipv6Addr>() {
        let seg0 = ipv6.segments()[0];
        return ipv6.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80;
    }
    false
}

fn load_bearer_token(path: &Path) -> Result<String, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("failed to read decide token file '{}': {e}", path.display()))?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(format!("decide token file '{}' is empty", path.display()));
    }
    Ok(trimmed)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawDecideResponse {
    model: Option<String>,
    routing: Option<RawRoutingBlock>,
    answers: BTreeMap<String, Value>,
    addita: Option<RawAdditaBlock>,
    error: Option<String>,
    code: Option<String>,
    detail: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawRoutingBlock {
    model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawAdditaBlock {
    id: Option<String>,
    #[serde(rename = "final")]
    final_decisions: BTreeMap<String, Value>,
    source: Option<String>,
    teacher: Option<String>,
    teacher_model: Option<String>,
    teacher_host: Option<String>,
    student_ms: Option<u64>,
    teacher_ms: Option<u64>,
}

fn parse_ollaya_error_body(raw: &str, status_code: Option<u16>) -> (String, Option<String>) {
    if let Ok(val) = serde_json::from_str::<Value>(raw) {
        if let Some(obj) = val.as_object() {
            let err_str = obj
                .get("error")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let code_str = obj.get("code").and_then(|v| v.as_str()).map(str::to_string);
            let detail_str = obj.get("detail").and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| (!v.is_null()).then(|| v.to_string()))
            });
            if err_str.is_some() || code_str.is_some() {
                let mut msg = err_str.unwrap_or_else(|| {
                    status_code
                        .map(|c| format!("decide HTTP {c}"))
                        .unwrap_or_else(|| "decide error".to_string())
                });
                if let Some(det) = detail_str.filter(|d| !d.is_empty()) {
                    msg = format!("{msg} ({det})");
                }
                return (truncate_utf8(&msg, 512), code_str);
            }
        }
    }
    let trimmed = raw.trim();
    let msg = match (status_code, trimmed.is_empty()) {
        (Some(code), true) => format!("decide HTTP {code}"),
        (Some(code), false) => format!("decide HTTP {code}: {}", truncate_utf8(trimmed, 256)),
        (None, true) => "decide error".to_string(),
        (None, false) => truncate_utf8(trimmed, 256),
    };
    (msg, None)
}

fn parse_noul_value(v: &Value) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    if let Some(b) = v.as_bool() {
        return Some(if b { 1.0 } else { 0.0 });
    }
    if let Some(obj) = v.as_object() {
        if let Some(n) = obj.get("noul").and_then(|x| x.as_f64()) {
            return Some(n);
        }
        if let Some(b) = obj.get("noul").and_then(|x| x.as_bool()) {
            return Some(if b { 1.0 } else { 0.0 });
        }
        if let Some(b) = obj.get("value").and_then(|x| x.as_bool()) {
            return Some(if b { 1.0 } else { 0.0 });
        }
    }
    None
}

fn parse_bool_value(v: &Value) -> Option<bool> {
    if let Some(b) = v.as_bool() {
        return Some(b);
    }
    if let Some(obj) = v.as_object() {
        if let Some(b) = obj.get("value").and_then(|x| x.as_bool()) {
            return Some(b);
        }
    }
    None
}

/// Converts a non-2xx HTTP response body from Addita / Ollaya into a [`DecideLogBlock`] with `judged = false`.
pub fn decide_block_from_http_error(status_code: u16, raw_body: &str) -> DecideLogBlock {
    let (msg, err_code) = parse_ollaya_error_body(raw_body, Some(status_code));
    DecideLogBlock {
        judged: json!(false),
        good: false,
        error: Some(msg),
        error_code: err_code,
        ..Default::default()
    }
}

/// Returns a one-line stderr notice (`"judged by cloud teacher <model>"`) when `teacher_host` is non-loopback/non-LAN.
pub fn cloud_teacher_notice(block: &DecideLogBlock) -> Option<String> {
    let host = block.teacher_host.as_deref()?;
    if is_local_or_lan_host(host) {
        return None;
    }
    let model = block
        .teacher_model
        .as_deref()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or("unknown");
    Some(format!("judged by cloud teacher {model}"))
}

/// Evaluates a parsed JSON response from `POST /api/decide` into a [`DecideLogBlock`].
pub fn evaluate_decide_response(body: &Value, threshold: f64) -> Result<DecideLogBlock, String> {
    let obj = body
        .as_object()
        .ok_or_else(|| "decide response must be a JSON object".to_string())?;
    if obj.get("error").and_then(|v| v.as_str()).is_some() {
        let (msg, code) = parse_ollaya_error_body(&body.to_string(), None);
        return Ok(DecideLogBlock {
            judged: json!(false),
            good: false,
            error: Some(msg),
            error_code: code,
            ..Default::default()
        });
    }
    if !obj.contains_key("answers") && !obj.contains_key("addita") {
        return Err("decide response missing both 'answers' and 'addita' fields".to_string());
    }
    let parsed: RawDecideResponse = serde_json::from_value(body.clone())
        .map_err(|e| format!("invalid decide response shape: {e}"))?;

    let mut noul = BTreeMap::new();
    for (k, v) in &parsed.answers {
        if let Some(score) = parse_noul_value(v) {
            noul.insert(k.clone(), score);
        }
    }

    let routing_model = parsed
        .routing
        .and_then(|r| r.model)
        .or(parsed.model)
        .filter(|m| !m.trim().is_empty());

    let mut final_decisions = BTreeMap::new();
    let mut addita_id = None;
    let mut source = None;
    let mut teacher = None;
    let mut teacher_model = None;
    let mut teacher_host = None;
    let mut student_ms = None;
    let mut teacher_ms = None;

    if let Some(addita) = parsed.addita {
        addita_id = addita.id;
        source = addita.source;
        teacher = addita.teacher;
        teacher_model = addita.teacher_model;
        teacher_host = addita.teacher_host;
        student_ms = addita.student_ms;
        teacher_ms = addita.teacher_ms;
        for (k, v) in &addita.final_decisions {
            if let Some(b) = parse_bool_value(v) {
                final_decisions.insert(k.clone(), b);
            }
        }
    }

    // TODO(addita): When teacher == "pending", a late final decision may arrive via OTLP log
    // "addita.decision.final" {id, ref, final} in Lume docs or via GET /api/addita/decision/{id}.
    if teacher.as_deref() == Some("pending") {
        return Ok(DecideLogBlock {
            judged: json!("pending"),
            good: false,
            noul,
            final_decisions,
            routing_model,
            addita_id,
            source,
            teacher,
            teacher_model,
            teacher_host,
            student_ms,
            teacher_ms,
            error: None,
            error_code: None,
        });
    }

    let good = if !final_decisions.is_empty() {
        final_decisions
            .get("rows_answer_question")
            .copied()
            .unwrap_or(false)
            && final_decisions
                .get("answer_supported")
                .copied()
                .unwrap_or(false)
    } else if noul.contains_key("rows_answer_question") && noul.contains_key("answer_supported") {
        noul["rows_answer_question"] >= threshold && noul["answer_supported"] >= threshold
    } else {
        false
    };

    Ok(DecideLogBlock {
        judged: json!(true),
        good,
        noul,
        final_decisions,
        routing_model,
        addita_id,
        source,
        teacher,
        teacher_model,
        teacher_host,
        student_ms,
        teacher_ms,
        error: None,
        error_code: None,
    })
}

/// Performs discovery (`GET /health` or `/api/version`) and a single `POST /api/decide` call (no retry).
pub fn judge_run(config: &DecideConfig, run_ref: &str, state: &DecideState) -> DecideLogBlock {
    let Some((base_url, token_path)) = config.resolve_endpoint_and_token_file() else {
        return DecideLogBlock::default();
    };

    let token = match token_path.as_deref() {
        Some(path) => match load_bearer_token(path) {
            Ok(t) => Some(t),
            Err(err) => {
                return DecideLogBlock {
                    judged: json!(false),
                    good: false,
                    error: Some(err),
                    ..Default::default()
                };
            }
        },
        None => None,
    };

    let is_addita = discover_is_addita(&base_url, token.as_deref(), config.timeout);
    let payload = build_decide_request_payload(config, run_ref, state, is_addita);

    let decide_endpoint = format!("{base_url}/api/decide");
    let mut req = ureq::post(&decide_endpoint)
        .set("Content-Type", "application/json")
        .timeout(config.timeout);
    if let Some(ref tok) = token {
        req = req.set("Authorization", &format!("Bearer {tok}"));
    }

    let response = match req.send_json(payload) {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            return decide_block_from_http_error(code, &body);
        }
        Err(ureq::Error::Transport(t)) => {
            return DecideLogBlock {
                judged: json!(false),
                good: false,
                error: Some(format!("decide request failed: {t}")),
                ..Default::default()
            };
        }
    };

    let body: Value = match response.into_json() {
        Ok(v) => v,
        Err(e) => {
            return DecideLogBlock {
                judged: json!(false),
                good: false,
                error: Some(format!("invalid decide JSON: {e}")),
                ..Default::default()
            };
        }
    };

    let block = match evaluate_decide_response(&body, config.decide_threshold) {
        Ok(b) => b,
        Err(err) => DecideLogBlock {
            judged: json!(false),
            good: false,
            error: Some(err),
            ..Default::default()
        },
    };

    if let Some(note) = cloud_teacher_notice(&block) {
        eprintln!("{note}");
    }

    block
}

/// Resolves the store directory for `agent_runs/` (`ti_store`, else `docs_index`, else `db_dir` if it exists).
pub fn resolve_run_log_store_dir(
    ti_store: Option<&str>,
    docs_index: Option<&str>,
    db_dir: &str,
) -> Option<PathBuf> {
    if let Some(ti) = ti_store.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(ti));
    }
    if let Some(docs) = docs_index.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(docs));
    }
    let db_path = Path::new(db_dir);
    if db_path.exists() {
        return Some(db_path.to_path_buf());
    }
    None
}

/// Simple UTC calendar date (`YYYY-MM-DD`) represented as days since 1970-01-01.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UtcDate {
    pub epoch_days: i64,
}

impl UtcDate {
    pub fn today() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            epoch_days: (secs / 86_400) as i64,
        }
    }

    pub fn from_ymd(year: i32, month: u32, day: u32) -> Option<Self> {
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        let y = year as i64 - if month <= 2 { 1 } else { 0 };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = (y - era * 400) as u64;
        let m = month as u64;
        let d = day as u64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe as i64 - 719_468;
        let candidate = Self { epoch_days: days };
        if candidate.to_ymd() == (year, month, day) {
            Some(candidate)
        } else {
            None
        }
    }

    pub fn parse_ymd(s: &str) -> Option<Self> {
        let raw = s.get(..10)?;
        let mut parts = raw.split('-');
        let y: i32 = parts.next()?.parse().ok()?;
        let m: u32 = parts.next()?.parse().ok()?;
        let d: u32 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Self::from_ymd(y, m, d)
    }

    pub fn to_ymd(self) -> (i32, u32, u32) {
        let z = self.epoch_days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = (yoe as i64) + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = if m <= 2 { y + 1 } else { y };
        (year as i32, m as u32, d as u32)
    }

    pub fn format_ymd(self) -> String {
        let (y, m, d) = self.to_ymd();
        format!("{y:04}-{m:02}-{d:02}")
    }

    pub fn sub_days(self, days: u64) -> Self {
        Self {
            epoch_days: self.epoch_days.saturating_sub(days as i64),
        }
    }
}

pub fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let date = UtcDate {
        epoch_days: (secs / 86_400) as i64,
    };
    let rem = secs % 86_400;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    format!("{}T{hour:02}:{minute:02}:{second:02}Z", date.format_ymd())
}

/// Appends one `AgentRunRecord` to `<store_root>/agent_runs/YYYY-MM-DD.jsonl` (`0700` dir, `0600` file)
/// and prunes files older than `RETENTION_DAYS` (30 days).
pub fn append_agent_run_log(store_root: &Path, record: &AgentRunRecord) -> Result<PathBuf, String> {
    let today = UtcDate::parse_ymd(&record.ts).unwrap_or_else(UtcDate::today);
    append_agent_run_log_on_date(store_root, record, today)
}

pub fn append_agent_run_log_on_date(
    store_root: &Path,
    record: &AgentRunRecord,
    today: UtcDate,
) -> Result<PathBuf, String> {
    let runs_dir = store_root.join("agent_runs");
    create_private_dir(&runs_dir)?;
    prune_old_run_logs(&runs_dir, today, RETENTION_DAYS);

    let file_path = runs_dir.join(format!("{}.jsonl", today.format_ymd()));
    let mut line =
        serde_json::to_string(record).map_err(|e| format!("serialize agent run: {e}"))?;
    line.push('\n');

    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(&file_path)
        .map_err(|e| format!("open '{}': {e}", file_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600));
    }
    file.write_all(line.as_bytes())
        .map_err(|e| format!("write '{}': {e}", file_path.display()))?;
    file.flush()
        .map_err(|e| format!("flush '{}': {e}", file_path.display()))?;
    Ok(file_path)
}

fn create_private_dir(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(dir)
            .map_err(|e| format!("create dir '{}': {e}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("chmod dir '{}': {e}", dir.display()))?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir).map_err(|e| format!("create dir '{}': {e}", dir.display()))?;
    }
    Ok(())
}

pub fn prune_old_run_logs(runs_dir: &Path, today: UtcDate, retention_days: u64) {
    let cutoff = today.sub_days(retention_days);
    let Ok(entries) = fs::read_dir(runs_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(file_date) = UtcDate::parse_ymd(stem) {
            if file_date < cutoff {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlExample {
    pub question: String,
    pub sql: String,
}

/// Queries the TI `docs` table (single SQL query) for any `addita.decision.final` rows
/// whose `final` object has both `rows_answer_question: true` and `answer_supported: true`.
#[cfg(feature = "ti")]
fn load_late_good_refs_from_ti_docs(store_root: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    if !store_root.join("ti.toml").exists() && !store_root.join("catalog.json").exists() {
        return out;
    }
    let Some(store_str) = store_root.to_str() else {
        return out;
    };
    let Ok(raw) = crate::ti_mcp::call(
        "ti_query",
        json!({
            "store": store_str,
            "sql": "SELECT body FROM docs WHERE title = 'addita.decision.final' ORDER BY ts_start DESC LIMIT 200"
        }),
    ) else {
        return out;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return out;
    };
    let Some(rows) = parsed.get("rows").and_then(|r| r.as_array()) else {
        return out;
    };
    for row in rows {
        let Some(body_str) = row.get("body").and_then(|b| b.as_str()) else {
            continue;
        };
        let Ok(body_json) = serde_json::from_str::<Value>(body_str) else {
            continue;
        };
        let run_ref = body_json.get("ref").and_then(|v| v.as_str()).or_else(|| {
            body_json
                .get("attributes")
                .and_then(|a| a.get("ref"))
                .and_then(|v| v.as_str())
        });
        let Some(run_ref) = run_ref.filter(|r| !r.is_empty()) else {
            continue;
        };
        let final_val = body_json
            .get("final")
            .or_else(|| body_json.get("attributes").and_then(|a| a.get("final")));
        let final_obj = match final_val {
            Some(Value::Object(map)) => Some(map.clone()),
            Some(Value::String(s)) => serde_json::from_str::<Value>(s)
                .ok()
                .and_then(|v| v.as_object().cloned()),
            _ => None,
        };
        if let Some(map) = final_obj {
            let rows_ok = map
                .get("rows_answer_question")
                .and_then(parse_bool_value)
                .unwrap_or(false);
            let ans_ok = map
                .get("answer_supported")
                .and_then(parse_bool_value)
                .unwrap_or(false);
            if rows_ok && ans_ok {
                out.insert(run_ref.to_string());
            }
        }
    }
    out
}

#[cfg(not(feature = "ti"))]
fn load_late_good_refs_from_ti_docs(_store_root: &Path) -> HashSet<String> {
    HashSet::new()
}

/// Loads recent GOOD runs (`<= 90` days, `<= 2000` lines) from `<store_root>/agent_runs/`,
/// ranks them against `question` using Lume's BM25 engine, and returns the top 3 distinct SQL examples.
pub fn select_top_sql_examples(store_root: &Path, question: &str) -> Vec<SqlExample> {
    select_top_sql_examples_on_date(store_root, question, UtcDate::today())
}

pub fn select_top_sql_examples_on_date(
    store_root: &Path,
    question: &str,
    today: UtcDate,
) -> Vec<SqlExample> {
    let runs_dir = store_root.join("agent_runs");
    if !runs_dir.is_dir() {
        return Vec::new();
    }
    let cutoff = today.sub_days(EXAMPLES_MAX_DAYS);
    let Ok(entries) = fs::read_dir(&runs_dir) else {
        return Vec::new();
    };

    let mut dated_files: Vec<(UtcDate, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?;
            let date = UtcDate::parse_ymd(stem)?;
            if date < cutoff {
                return None;
            }
            Some((date, path))
        })
        .collect();

    // Newest files first so the 2000-line cap retains the most recent runs.
    dated_files.sort_by_key(|b| std::cmp::Reverse(b.0));

    let mut total_lines = 0usize;
    let mut parsed_records: Vec<AgentRunRecord> = Vec::new();
    let mut has_pending = false;

    'files: for (_, path) in dated_files {
        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        let reader = BufReader::new(file);
        let mut lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
        // Within each file, read newest lines first.
        lines.reverse();
        for line in lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            total_lines += 1;
            if let Ok(rec) = serde_json::from_str::<AgentRunRecord>(trimmed) {
                if rec.decide.judged == json!("pending") {
                    has_pending = true;
                }
                parsed_records.push(rec);
            }
            if total_lines >= EXAMPLES_MAX_LINES {
                break 'files;
            }
        }
    }

    let late_good_refs = if has_pending {
        load_late_good_refs_from_ti_docs(store_root)
    } else {
        HashSet::new()
    };

    let mut good_runs: Vec<SqlExample> = Vec::new();
    for rec in parsed_records {
        let is_good = (rec.decide.good && rec.decide.judged == json!(true))
            || late_good_refs.contains(&rec.run_ref);
        if is_good {
            if let Some(winning_sql) = rec.sql.last().map(|s| s.trim()).filter(|s| !s.is_empty()) {
                let q = rec.question.trim();
                if !q.is_empty() {
                    good_runs.push(SqlExample {
                        question: q.to_string(),
                        sql: winning_sql.to_string(),
                    });
                }
            }
        }
    }

    if good_runs.is_empty() {
        return Vec::new();
    }

    let sections: Vec<Section> = good_runs
        .iter()
        .enumerate()
        .map(|(idx, ex)| Section {
            title: ex.question.clone(),
            body: format!("{} {}", ex.question, ex.sql),
            line_number: idx + 1,
            filename: None,
            entities: Vec::new(),
        })
        .collect();

    let bm25 = Bm25Index::build(sections, None);
    let hits = bm25.search_quiet(
        question,
        SearchVariant::Classic,
        &Bm25Params::default(),
        None,
    );

    let mut selected: Vec<SqlExample> = Vec::with_capacity(EXAMPLES_TOP_K);
    let mut seen_sql: HashSet<String> = HashSet::new();

    for hit in hits {
        if let Some(ex) = good_runs.get(hit.section_index) {
            let norm_sql = normalize_sql_for_dedup(&ex.sql);
            if seen_sql.insert(norm_sql) {
                selected.push(ex.clone());
                if selected.len() >= EXAMPLES_TOP_K {
                    return selected;
                }
            }
        }
    }

    // If BM25 found no lexical token overlap, fall back to the most recent GOOD runs on this store.
    if selected.is_empty() {
        for ex in good_runs {
            let norm_sql = normalize_sql_for_dedup(&ex.sql);
            if seen_sql.insert(norm_sql) {
                selected.push(ex);
                if selected.len() >= EXAMPLES_TOP_K {
                    break;
                }
            }
        }
    }

    selected
}

fn normalize_sql_for_dedup(sql: &str) -> String {
    sql.trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Formats selected SQL examples for injection into the system prompt.
pub fn format_examples_for_prompt(examples: &[SqlExample]) -> Option<String> {
    if examples.is_empty() {
        return None;
    }
    let mut out = String::from(
        "\nPast questions on this store answered with working SQL (examples, adapt; don't copy blindly):",
    );
    for ex in examples {
        out.push_str(&format!("\nQ: {} SQL: {}", ex.question, ex.sql));
    }
    Some(out)
}

/// Runs post-answer judging and appends the run record to `<store>/agent_runs/YYYY-MM-DD.jsonl`.
#[allow(clippy::too_many_arguments)]
pub fn finish_run_decide_and_log(
    config: &DecideConfig,
    question: &str,
    executed_sql: &[String],
    last_successful_sql: Option<&str>,
    last_successful_rows: &[Value],
    last_successful_truncated: bool,
    last_sql_error: Option<&str>,
    answer: &str,
    model: &str,
    ti_store: Option<&str>,
    docs_index: Option<&str>,
    db_dir: &str,
) -> DecideLogBlock {
    let run_ref = format!("lume-run-{}", crate::uuid_v4());
    let mut sql_list: Vec<String> = executed_sql.to_vec();
    if let Some(win) = last_successful_sql {
        let win_trimmed = win.trim().to_string();
        if !win_trimmed.is_empty() && sql_list.last() != Some(&win_trimmed) {
            sql_list.retain(|s| s != &win_trimmed);
            sql_list.push(win_trimmed);
        }
    }
    let chosen_sql = last_successful_sql
        .or_else(|| sql_list.last().map(String::as_str))
        .unwrap_or("");
    let state_error = if last_successful_sql.is_some() {
        None
    } else {
        last_sql_error
    };
    let state = build_bounded_state(
        question,
        chosen_sql,
        &sql_list,
        last_successful_rows,
        last_successful_truncated,
        state_error,
        answer,
    );

    let decide_block = if config.effective_decide_url().is_some() {
        judge_run(config, &run_ref, &state)
    } else {
        DecideLogBlock::default()
    };

    if !config.no_agent_log {
        if let Some(store_root) = resolve_run_log_store_dir(ti_store, docs_index, db_dir) {
            let record = AgentRunRecord {
                ts: rfc3339_now(),
                run_ref,
                question: question.to_string(),
                sql: sql_list,
                rows_sample: state.rows,
                answer: answer.to_string(),
                model: model.to_string(),
                decide: decide_block.clone(),
            };
            let _ = append_agent_run_log(&store_root, &record);
        }
    }

    decide_block
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "lume-decide-{prefix}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Real captured 200 OK response from Addita 0.1.0 (`main` commit `fb1d519`)
    /// with live Ollaya 0.7.2 (`laya:en`) + live Ollama (`qwen3.5:4b`).
    const REAL_ADDITA_LIVE_FIXTURE_200: &str = r#"{"model":"laya:en","answers":{"rows_answer_question":{"type":"noul","noul":0.8536},"answer_supported":{"type":"noul","noul":0.7887}},"usage":{"input_tokens":568,"output_tokens":0},"routing":{"router":"laya:latest","model":"laya:en","route":"english","reason":"English Latin text"},"state_truncated":false,"done_reason":"decide","created_at":"2026-10-16T19:17:49.972150515Z","total_duration":2906119325,"load_duration":0,"eval_duration":389952383,"addita":{"id":"81614d3f-2889-4f05-9e44-75b315d19496","ref":"lume-run-fb1d519-001","final":{"rows_answer_question":true,"answer_supported":true},"source":"teacher","teacher":"answered","teacher_model":"qwen3.5:4b","teacher_host":"host.docker.internal:11434","guards":[],"agreement":{"rows_answer_question":true,"answer_supported":true},"teacher_answers":{"rows_answer_question":true,"answer_supported":true},"student_ms":405,"teacher_ms":2466}}"#;

    /// Real captured error responses from Addita 0.1.0 (`main` commit `fb1d519`).
    const REAL_ADDITA_ERR_422_MISSING_QUESTIONS: &str = r#"{"error":"questions: Field required","code":"INVALID_REQUEST","detail":[{"type":"missing","loc":["body","questions"],"msg":"Field required"}]}"#;
    const REAL_ADDITA_ERR_404_NOT_FOUND: &str =
        r#"{"error":"unknown endpoint GET /does-not-exist","code":"NOT_FOUND"}"#;
    const REAL_ADDITA_ERR_405_METHOD_NOT_ALLOWED: &str = r#"{"error":"method GET is not allowed on /api/decide; allowed: POST","code":"METHOD_NOT_ALLOWED"}"#;
    const REAL_ADDITA_ERR_404_UNKNOWN_PRESET: &str =
        r#"{"error":"preset 'lume.nonexistent' not found","code":"NOT_FOUND"}"#;
    const REAL_ADDITA_ERR_422_STREAMING: &str = r#"{"error":"stream: /api/decide does not stream; omit \"stream\" or set it to false","code":"INVALID_REQUEST","detail":[{"type":"stream_unsupported","loc":["body","stream"],"msg":"/api/decide does not stream; omit \"stream\" or set it to false"}]}"#;

    #[test]
    fn real_captured_addita_and_ollaya_responses_and_pending_teacher() {
        // 1. Live fixture from Addita main (fb1d519, real student laya:en + local teacher qwen3.5:4b):
        // note that answer_supported noul is 0.7887 (< 0.80), but addita.final is true/true -> GOOD!
        let body_fixture: Value = serde_json::from_str(REAL_ADDITA_LIVE_FIXTURE_200).unwrap();
        let block_fixture = evaluate_decide_response(&body_fixture, 0.8).unwrap();
        assert_eq!(block_fixture.judged, json!(true));
        assert!(block_fixture.good);
        assert_eq!(block_fixture.routing_model.as_deref(), Some("laya:en"));
        assert_eq!(
            block_fixture.addita_id.as_deref(),
            Some("81614d3f-2889-4f05-9e44-75b315d19496")
        );
        assert_eq!(block_fixture.source.as_deref(), Some("teacher"));
        assert_eq!(block_fixture.teacher.as_deref(), Some("answered"));
        assert_eq!(
            block_fixture.teacher_host.as_deref(),
            Some("host.docker.internal:11434")
        );
        assert_eq!(block_fixture.teacher_model.as_deref(), Some("qwen3.5:4b"));
        assert_eq!(block_fixture.student_ms, Some(405));
        assert_eq!(block_fixture.teacher_ms, Some(2466));
        assert_eq!(
            block_fixture.noul.get("rows_answer_question"),
            Some(&0.8536)
        );
        assert_eq!(block_fixture.noul.get("answer_supported"), Some(&0.7887));
        assert_eq!(cloud_teacher_notice(&block_fixture), None);

        // 2. addita.final has false -> NOT GOOD even if noul >= 0.8
        let resp_final_bad = json!({
            "answers": {
                "rows_answer_question": { "type": "noul", "noul": 0.99 },
                "answer_supported": { "type": "noul", "noul": 0.99 }
            },
            "addita": {
                "id": "dec-final-bad",
                "final": {
                    "rows_answer_question": true,
                    "answer_supported": false
                },
                "source": "teacher",
                "teacher": "answered",
                "student_ms": 380,
                "teacher_ms": 2100
            }
        });
        let block_bad = evaluate_decide_response(&resp_final_bad, 0.8).unwrap();
        assert_eq!(block_bad.judged, json!(true));
        assert!(!block_bad.good);

        // 3. Plain Ollaya response (no addita block) -> noul >= threshold on both
        let resp_noul = json!({
            "model": "laya:en",
            "answers": {
                "rows_answer_question": { "type": "noul", "noul": 0.85 },
                "answer_supported": { "type": "noul", "noul": 0.80 }
            },
            "done_reason": "decide"
        });
        let block_good = evaluate_decide_response(&resp_noul, 0.8).unwrap();
        assert_eq!(block_good.judged, json!(true));
        assert!(block_good.good);
        let block_strict = evaluate_decide_response(&resp_noul, 0.9).unwrap();
        assert_eq!(block_strict.judged, json!(true));
        assert!(!block_strict.good);

        // 4. teacher == "pending" -> judged = "pending", good = false unconditionally
        // (even when Addita populates addita.final with student fallback values)
        let resp_pending = json!({
            "answers": {
                "rows_answer_question": { "type": "noul", "noul": 0.95 },
                "answer_supported": { "type": "noul", "noul": 0.95 }
            },
            "addita": {
                "id": "dec-pending",
                "final": {
                    "rows_answer_question": true,
                    "answer_supported": true
                },
                "source": "student",
                "teacher": "pending",
                "teacher_model": "glm-5.1:cloud",
                "teacher_host": "ollama.com",
                "student_ms": 390,
                "teacher_ms": 15
            }
        });
        let block_pending = evaluate_decide_response(&resp_pending, 0.8).unwrap();
        assert_eq!(block_pending.judged, json!("pending"));
        assert!(!block_pending.good);
        assert_eq!(block_pending.teacher.as_deref(), Some("pending"));
        assert_eq!(block_pending.student_ms, Some(390));
        assert_eq!(block_pending.teacher_ms, Some(15));
        assert_eq!(
            cloud_teacher_notice(&block_pending).as_deref(),
            Some("judged by cloud teacher glm-5.1:cloud")
        );
    }

    #[test]
    fn real_captured_addita_error_bodies_and_request_payload_shape() {
        for (status, raw, expected_code, expected_substr) in [
            (
                422,
                REAL_ADDITA_ERR_422_MISSING_QUESTIONS,
                Some("INVALID_REQUEST"),
                "questions: Field required",
            ),
            (
                404,
                REAL_ADDITA_ERR_404_NOT_FOUND,
                Some("NOT_FOUND"),
                "unknown endpoint GET /does-not-exist",
            ),
            (
                405,
                REAL_ADDITA_ERR_405_METHOD_NOT_ALLOWED,
                Some("METHOD_NOT_ALLOWED"),
                "method GET is not allowed on /api/decide",
            ),
            (
                404,
                REAL_ADDITA_ERR_404_UNKNOWN_PRESET,
                Some("NOT_FOUND"),
                "preset 'lume.nonexistent' not found",
            ),
            (
                422,
                REAL_ADDITA_ERR_422_STREAMING,
                Some("INVALID_REQUEST"),
                "/api/decide does not stream",
            ),
            (500, "internal error", None, "decide HTTP 500"),
        ] {
            let block = decide_block_from_http_error(status, raw);
            assert_eq!(block.judged, json!(false));
            assert!(!block.good);
            assert_eq!(block.error_code.as_deref(), expected_code);
            assert!(
                block
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .contains(expected_substr),
                "unexpected error message: {:?}",
                block.error
            );
        }

        // Verify request payload shape for Addita vs plain Ollaya, inline questions vs preset
        let state = build_bounded_state(
            "What was our speed?",
            "SELECT 1",
            &["SELECT 1".to_string()],
            &[json!({"sog": 7.2})],
            false,
            None,
            "7.2 knots",
        );
        let cfg_inline = DecideConfig::default();
        let req_addita = build_decide_request_payload(&cfg_inline, "lume-run-123", &state, true);
        assert_eq!(req_addita["model"], "laya");
        assert!(req_addita.get("preset").is_none());
        assert_eq!(
            req_addita["questions"]["rows_answer_question"]["type"],
            "noul"
        );
        assert!(req_addita["questions"]["rows_answer_question"]["instructions"].is_string());
        assert!(req_addita["questions"]["answer_supported"]["instructions"].is_string());
        assert_eq!(req_addita["addita"], json!({ "ref": "lume-run-123" }));
        assert!(req_addita["addita"].get("allow_cloud_teacher").is_none());

        let req_ollaya = build_decide_request_payload(&cfg_inline, "lume-run-123", &state, false);
        assert!(req_ollaya.get("addita").is_none());

        let cfg_preset = DecideConfig {
            decide_preset: Some(DEFAULT_DECIDE_PRESET.to_string()),
            ..Default::default()
        };
        let req_preset = build_decide_request_payload(&cfg_preset, "lume-run-456", &state, true);
        assert_eq!(req_preset["preset"], "lume.sql_judge");
        assert!(req_preset.get("questions").is_none());
    }

    #[test]
    fn local_and_lan_vs_cloud_teacher_host_classification() {
        assert!(is_local_or_lan_host("127.0.0.1"));
        assert!(is_local_or_lan_host("http://127.0.0.1:11434"));
        assert!(is_local_or_lan_host("localhost:11434"));
        assert!(is_local_or_lan_host("host.docker.internal:11434"));
        assert!(is_local_or_lan_host("[::1]:11434"));
        assert!(is_local_or_lan_host("192.168.1.42:11434"));
        assert!(is_local_or_lan_host("10.0.0.5"));
        assert!(is_local_or_lan_host("172.20.0.3"));
        assert!(is_local_or_lan_host("navpi.local"));
        assert!(!is_local_or_lan_host("ollama.com"));
        assert!(!is_local_or_lan_host("https://api.ollama.com/v1"));
        assert!(!is_local_or_lan_host("8.8.8.8"));
    }

    #[test]
    fn state_row_and_byte_truncation_marks_rows_truncated() {
        let many_rows: Vec<Value> = (0..25).map(|i| json!({ "idx": i, "v": "ok" })).collect();
        let sql_all = vec!["SELECT 0".to_string(), "SELECT 1".to_string()];
        let st = build_bounded_state("q", "SELECT 1", &sql_all, &many_rows, false, None, "ans");
        assert_eq!(st.rows.len(), MAX_STATE_ROWS);
        assert!(st.rows_truncated);
        assert_eq!(st.sql_all, sql_all);
        assert!(serde_json::to_vec(&st).unwrap().len() <= MAX_STATE_BYTES);

        let huge_rows: Vec<Value> = (0..15)
            .map(|i| json!({ "idx": i, "payload": "x".repeat(2000) }))
            .collect();
        let st_huge =
            build_bounded_state("q", "SELECT 1", &sql_all, &huge_rows, false, None, "ans");
        assert!(st_huge.rows.len() < 15);
        assert!(st_huge.rows_truncated);
        assert!(serde_json::to_vec(&st_huge).unwrap().len() <= MAX_STATE_BYTES);
    }

    #[test]
    fn addita_serve_json_discovery_fallback_and_token_precedence() {
        let dir = TempDir::new("serve-disc");
        let disc_path = dir.path.join("serve.json");
        let disc_token = dir.path.join("disc.token");
        let explicit_token = dir.path.join("explicit.token");
        fs::write(&disc_token, "disc-secret\n").unwrap();
        fs::write(&explicit_token, "explicit-secret\n").unwrap();
        fs::write(
            &disc_path,
            serde_json::to_string(&json!({
                "service": "addita",
                "url": "http://127.0.0.1:11345",
                "port": 11345,
                "pid": 216771,
                "version": "0.1.0",
                "token_file": disc_token.to_str().unwrap(),
                "started": "2026-10-16T18:45:21.877Z"
            }))
            .unwrap(),
        )
        .unwrap();

        let cfg_from_disc = DecideConfig {
            decide_url: None,
            decide_token_file: None,
            discovery_file: Some(disc_path.clone()),
            ..Default::default()
        };
        let (url, tok) = cfg_from_disc.resolve_endpoint_and_token_file().unwrap();
        assert_eq!(url, "http://127.0.0.1:11345");
        assert_eq!(tok.as_deref(), Some(disc_token.as_path()));

        // Explicit --decide-token-file overrides serve.json token_file
        let cfg_override_tok = DecideConfig {
            decide_url: None,
            decide_token_file: Some(explicit_token.clone()),
            discovery_file: Some(disc_path),
            ..Default::default()
        };
        let (_, tok2) = cfg_override_tok.resolve_endpoint_and_token_file().unwrap();
        assert_eq!(tok2.as_deref(), Some(explicit_token.as_path()));
    }

    #[test]
    fn perms_retention_prune_and_examples_selection() {
        let store_a = TempDir::new("store-a");
        let store_b = TempDir::new("store-b");
        let runs_dir_a = store_a.path.join("agent_runs");
        fs::create_dir_all(&runs_dir_a).unwrap();

        let today = UtcDate::from_ymd(2026, 10, 16).unwrap();
        let stale_file = runs_dir_a.join("2026-09-01.jsonl"); // 45 days old (> 30 days)
        let recent_file = runs_dir_a.join("2026-10-01.jsonl"); // 15 days old (<= 30 days)
        fs::write(&stale_file, "{}\n").unwrap();

        let make_rec = |q: &str, sql: &str, judged: Value, good: bool| AgentRunRecord {
            ts: "2026-10-16T12:00:00Z".to_string(),
            run_ref: "lume-run-test".to_string(),
            question: q.to_string(),
            sql: vec![sql.to_string()],
            rows_sample: vec![],
            answer: "ok".to_string(),
            model: "qwen2.5:7b".to_string(),
            decide: DecideLogBlock {
                judged,
                good,
                ..Default::default()
            },
        };

        // Write a mix of GOOD, BAD, PENDING, and duplicate SQL into recent_file
        let mut recent_lines = String::new();
        for rec in [
            make_rec(
                "What was the temperature in New Braunfels weather broadcast?",
                "SELECT body FROM docs WHERE title = 'radio.weather' ORDER BY ts_start DESC LIMIT 1",
                json!(true),
                true,
            ),
            // Duplicate SQL (should be deduped)
            make_rec(
                "Latest New Braunfels weather broadcast temperature?",
                "SELECT body FROM docs WHERE title = 'radio.weather' ORDER BY ts_start DESC LIMIT 1;",
                json!(true),
                true,
            ),
            make_rec(
                "What was the wind speed on the weather broadcast?",
                "SELECT json_num(body, 'wind_kts') FROM docs WHERE title = 'radio.weather'",
                json!(true),
                true,
            ),
            make_rec(
                "What was the barometric pressure on the weather broadcast?",
                "SELECT json_num(body, 'pressure_mb') FROM docs WHERE title = 'radio.weather'",
                json!(true),
                true,
            ),
            make_rec(
                "What was the humidity on the weather broadcast?",
                "SELECT json_num(body, 'humidity_pct') FROM docs WHERE title = 'radio.weather'",
                json!(true),
                true,
            ),
            // Bad run (must be ignored)
            make_rec(
                "What was the temperature in New Braunfels?",
                "SELECT wrong_col FROM docs",
                json!(true),
                false,
            ),
            // Pending teacher run (must be ignored unless late OTLP final arrives)
            make_rec(
                "What was the temperature in New Braunfels?",
                "SELECT pending_col FROM docs",
                json!("pending"),
                false,
            ),
        ] {
            recent_lines.push_str(&serde_json::to_string(&rec).unwrap());
            recent_lines.push('\n');
        }
        fs::write(&recent_file, recent_lines).unwrap();

        // Append today's run and verify stale file is pruned and permissions are 0700 / 0600
        let today_rec = make_rec("Other store question", "SELECT 999", json!(false), false);
        let written_path = append_agent_run_log_on_date(&store_a.path, &today_rec, today).unwrap();
        assert!(
            !stale_file.exists(),
            "file older than 30 days must be pruned"
        );
        assert!(
            recent_file.exists(),
            "recent file within 30 days must be kept"
        );
        assert!(written_path.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = fs::metadata(&runs_dir_a).unwrap().permissions().mode() & 0o777;
            let file_mode = fs::metadata(&written_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(file_mode, 0o600);
        }

        // Write a GOOD run to store_b and verify store_a never sees it and empty store has none
        let empty_store = TempDir::new("store-empty");
        assert!(
            select_top_sql_examples_on_date(&empty_store.path, "temperature", today).is_empty()
        );

        let store_b_rec = make_rec(
            "What was the temperature in New Braunfels?",
            "SELECT secret_store_b FROM docs",
            json!(true),
            true,
        );
        append_agent_run_log_on_date(&store_b.path, &store_b_rec, today).unwrap();

        let examples = select_top_sql_examples_on_date(
            &store_a.path,
            "What was the temperature in New Braunfels on the weather broadcast?",
            today,
        );
        assert_eq!(examples.len(), 3, "top 3 distinct SQL examples");
        assert_eq!(
            examples[0].sql.trim_end_matches(';'),
            "SELECT body FROM docs WHERE title = 'radio.weather' ORDER BY ts_start DESC LIMIT 1"
        );
        for ex in &examples {
            assert!(!ex.sql.contains("wrong_col"));
            assert!(!ex.sql.contains("pending_col"));
            assert!(!ex.sql.contains("secret_store_b"));
        }
    }
}
