use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

/// Serializable ti.toml schema. Unknown keys are errors; omitted keys use defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TiConfig {
    /// Fixed store bucket width; changing it requires rebuilding the store.
    pub width_seconds: u64,
    /// Local store root.
    pub store_root: String,
    /// Edge retention in years.
    pub retention_years: u32,
    /// Maximum registered fields per vessel.
    pub field_cap: u32,
    /// Signal K connection.
    pub signal_k: SignalKConfig,
    /// Explicit finite numeric event paths counted per bucket.
    pub ingest: IngestConfig,
    /// Allow/deny glob patterns, deny takes precedence.
    pub allow_paths: Vec<String>,
    /// Default deny patterns.
    pub deny_paths: Vec<String>,
    /// Named aggregate profiles.
    pub profiles: AggregateProfiles,
    /// Unit to decimal scale registry.
    pub unit_scales: BTreeMap<String, u8>,
    /// Exact-path scale overrides.
    pub path_scales: BTreeMap<String, u8>,
    /// Exact-path preferred sources, most preferred first.
    pub source_priorities: BTreeMap<String, Vec<String>>,
    /// Derived transition/edge/notification rules.
    pub derived: Vec<DerivedRule>,
    /// SQL alert rules evaluated once per newly closed default-store bucket (W10).
    pub rules: Vec<AlertRule>,
    /// Explicit generic Parquet inputs (D38), empty for Signal K installations.
    pub sources: crate::SourcesConfig,
    /// Metric glob to physical units and fixed-point scale (D38).
    pub units: crate::MetricUnits,
    /// Listener configuration.
    pub bind: BindConfig,
    /// Optional bearer/NUTS/SCRAM configuration.
    pub auth: AuthConfig,
    /// Query resource limits.
    pub query: QueryLimits,
    /// Multi-store configurations (D30). If empty/omitted, single-store mode uses top-level fields.
    #[serde(default)]
    pub stores: BTreeMap<String, StoreConfig>,
    /// Fleet synchronization configuration (W8).
    #[serde(default)]
    pub sync: SyncConfig,
}

/// Ingest-time event-count policy; changes apply to newly opened buckets only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct IngestConfig {
    /// Exact numeric leaf paths; bare columns count preferred-source samples.
    pub count_paths: Vec<String>,
}

/// Device token comes from a one-time Signal K access request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SignalKConfig {
    /// WebSocket stream URL.
    pub url: String,
    /// Device token; never a Signal K user password.
    pub token: Option<String>,
    /// Server-provided access-request polling href, preserved verbatim.
    pub access_request_href: Option<String>,
    /// True when the access-request endpoint returned 404 with security disabled.
    pub no_auth_required: bool,
}

/// Profile names and aggregate columns enabled for numeric paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AggregateProfiles {
    /// Normal profile.
    pub default: Vec<String>,
    /// Paths with median sample interval >= W.
    pub slow: Vec<String>,
    /// Extra aggregates explicitly enabled by the operator.
    pub opt_in: Vec<String>,
}

/// Listener inside the deployment namespace; restrict host publication to LAN on boats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BindConfig {
    /// IPv4/IPv6 address, 0.0.0.0 inside containers.
    pub address: String,
    /// HTTP/MCP port.
    pub http_port: u16,
    /// Optional PostgreSQL port.
    pub pg_port: Option<u16>,
}

/// Shore NUTS integration and optional boat credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// Enable the existing shore NUTS auth adapter.
    pub nuts: bool,
    /// Optional HTTP bearer token.
    pub bearer_token: Option<String>,
    /// PostgreSQL SCRAM users.
    pub scram_users: Vec<ScramUser>,
}

/// PostgreSQL SCRAM verifier; no plaintext password is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScramUser {
    /// Unique username.
    pub username: String,
    /// SCRAM-SHA-256 verifier string; cryptographic validation belongs to pgwire.
    pub verifier: String,
}

/// Query resource and admission limits in `ti.toml [query]`.
///
/// Streaming execution keeps pgwire memory usage bounded to one record batch plus the
/// encoder buffer, so the defaults (100,000 rows / 16 MiB) are safe on aarch64 SBCs
/// (e.g. Raspberry Pi 4 / 5) with <= 8 GB RAM. For heavily constrained systems (e.g.
/// 2 GB RAM or dense multi-client dashboards), `pg_max_rows` (e.g. 10000) and `pg_max_bytes`
/// (e.g. 4194304 for 4 MiB) can be lowered in `[query]`.
/// HTTP `/ti/query` and MCP tools keep fixed agent-facing limits of 500 rows / 64 KiB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueryLimits {
    /// Per-query timeout.
    pub timeout_seconds: u64,
    /// Per-query memory pool bytes.
    pub memory_bytes: u64,
    /// Whole-unit memory limit, including ingest.
    pub unit_memory_bytes: u64,
    /// Parallel query partitions.
    pub target_partitions: usize,
    /// Heavy-query concurrency.
    pub heavy_queries: usize,
    /// Pause background work above this temperature.
    pub thermal_celsius: u16,
    /// Maximum rows returned by a pgwire query (default: 100,000 for Grafana).
    pub pg_max_rows: usize,
    /// Maximum result bytes returned by a pgwire query (default: 16 MiB for Grafana).
    pub pg_max_bytes: usize,
}

/// Fleet synchronization configuration (W8).
///
/// Prefer `token_file` over inline `token`. An inline token in `ti.toml` is a plaintext
/// secret and on Unix requires `ti.toml` to have mode 0600 (not group- or world-accessible).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SyncConfig {
    /// Path to a shared bearer token file for sync endpoints.
    pub token_file: Option<String>,
    /// Inline bearer token for sync (alternative to token_file).
    pub token: Option<String>,
    /// Link budget in bytes per day, if any.
    pub link_budget_bytes: Option<u64>,
    /// Idle priority flag (pause under load).
    pub idle_priority: bool,
}

impl SyncConfig {
    /// Resolve the bearer token from token_file if present, or inline token.
    ///
    /// If `token_file` is set but cannot be read or is empty, returns an error
    /// and refuses to fall back to an inline token.
    pub fn resolved_token(&self) -> std::result::Result<Option<String>, String> {
        if let Some(ref path) = self.token_file {
            let content = std::fs::read_to_string(path)
                .map_err(|e| format!("failed to read sync token_file '{path}': {e}"))?;
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                return Err(format!("sync token_file '{path}' is empty"));
            }
            return Ok(Some(trimmed));
        }
        let inline = self
            .token
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(inline)
    }
}

/// Configuration for a named store (D30).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreConfig {
    /// Bucket width string with suffix (e.g. "10s", "1s").
    pub width: String,
    /// Local edge retention duration string (e.g. "730d", "90d", "forever").
    pub retention: String,
    /// Optional shore retention duration string (e.g. "90d", "forever").
    pub shore_retention: Option<String>,
    /// Optional custom store root directory on disk. Defaults to `<store_root>/stores/<name>`.
    pub root: Option<String>,
    /// Path allow-list glob patterns. None/omitted means all paths are allowed.
    pub paths: Option<Vec<String>>,
    /// Path pattern -> aggregate names mapping, e.g. "navigation.*" = ["last"].
    pub aggs: BTreeMap<String, Vec<String>>,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            width: "10s".into(),
            retention: "730d".into(),
            shore_retention: None,
            root: None,
            paths: None,
            aggs: BTreeMap::new(),
        }
    }
}

impl StoreConfig {
    /// Parsed width in seconds.
    pub fn width_seconds(&self) -> Result<u64> {
        parse_duration_seconds(&self.width, "width")
    }

    /// Parsed retention in seconds, or None if "forever".
    pub fn retention_seconds(&self) -> Result<Option<u64>> {
        parse_retention(&self.retention, "retention")
    }

    /// Parsed shore retention in seconds, or None if "forever" or unset.
    pub fn shore_retention_seconds(&self) -> Result<Option<u64>> {
        match &self.shore_retention {
            Some(s) => parse_retention(s, "shore_retention"),
            None => Ok(None),
        }
    }

    /// Resolve effective directory for this store.
    pub fn resolved_root(&self, parent_store_root: &str, store_name: &str) -> String {
        if let Some(r) = &self.root {
            r.clone()
        } else {
            format!("{parent_store_root}/stores/{store_name}")
        }
    }
}

/// Parse duration string with suffix s, m, h, d into seconds.
/// Suffix multipliers:
/// - 's': 1
/// - 'm': 60
/// - 'h': 3600
/// - 'd': 86400
///
/// Rejects 0 and unknown suffixes.
pub fn parse_duration_seconds(s: &str, key: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        return Err(invalid(key, "duration must not be empty"));
    }
    let (num_str, unit) = if let Some(stripped) = s.strip_suffix('s') {
        (stripped, 1u64)
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, 60u64)
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, 3600u64)
    } else if let Some(stripped) = s.strip_suffix('d') {
        (stripped, 86400u64)
    } else {
        return Err(invalid(
            key,
            "unknown duration suffix (expected 's', 'm', 'h', or 'd')",
        ));
    };

    let count: u64 = num_str
        .trim()
        .parse()
        .map_err(|_| invalid(key, "invalid duration integer"))?;
    if count == 0 {
        return Err(invalid(key, "duration must be positive"));
    }
    count
        .checked_mul(unit)
        .ok_or_else(|| invalid(key, "duration overflow"))
}

/// Retention can be a duration or the literal "forever" (represented as None).
pub fn parse_retention(s: &str, key: &str) -> Result<Option<u64>> {
    let s = s.trim();
    if s == "forever" {
        Ok(None)
    } else {
        parse_duration_seconds(s, key).map(Some)
    }
}

/// One operator-approved SQL predicate that writes alert documents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertRule {
    pub name: String,
    pub severity: String,
    pub when: String,
    #[serde(rename = "for", default)]
    pub hold: Option<String>,
    #[serde(default)]
    pub vessel: Option<String>,
    pub message: String,
    #[serde(default = "default_alert_cap")]
    pub max_per_hour: u32,
}
fn default_alert_cap() -> u32 {
    60
}

impl AlertRule {
    /// Exact nanosecond hold duration, shared with intervals().
    pub fn hold_nanoseconds(&self) -> Result<i128> {
        self.hold
            .as_deref()
            .map(parse_interval_duration_nanoseconds)
            .transpose()
            .map(|v| v.unwrap_or(0))
    }
}

/// Parse the duration syntax used by intervals() and alert holds without rounding.
pub fn parse_interval_duration_nanoseconds(s: &str) -> Result<i128> {
    let fail = |message: &str| invalid("interval duration", message);
    let s = s.trim();
    let n = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let (digits, unit) = s.split_at(n);
    let factor: i128 = match unit.trim() {
        "ns" => 1,
        "us" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60_000_000_000,
        "h" => 3_600_000_000_000,
        "d" => 86_400_000_000_000,
        "w" => 604_800_000_000_000,
        _ => return Err(fail("requires ns/us/ms/s/m/h/d/w")),
    };
    let mut parts = digits.split('.');
    let whole = parts
        .next()
        .unwrap_or("")
        .parse::<i128>()
        .map_err(|_| fail("invalid nonnegative duration"))?;
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some() || fraction.len() > 9 || !fraction.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(fail("invalid fractional precision"));
    }
    let divisor = 10i128.pow(fraction.len() as u32);
    let fractional = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i128>()
            .map_err(|_| fail("invalid fraction"))?
    };
    let scaled = fractional
        .checked_mul(factor)
        .ok_or_else(|| fail("overflow"))?;
    if scaled % divisor != 0 {
        return Err(fail("below nanosecond precision"));
    }
    whole
        .checked_mul(factor)
        .and_then(|v| v.checked_add(scaled / divisor))
        .ok_or_else(|| fail("overflow"))
}

/// Derived event rule kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedKind {
    /// Count transitions into a configured state.
    Transition,
    /// Count rising edges.
    RisingEdge,
    /// Notification state and raise count.
    Notification,
}

/// One operator rule applied at bucket close.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedRule {
    /// Source path/glob.
    pub path: String,
    /// Destination field/path.
    pub output: String,
    /// Rule operation.
    pub kind: DerivedKind,
    /// Required for transition, e.g. started; otherwise absent.
    pub state: Option<String>,
}

impl Default for SignalKConfig {
    fn default() -> Self {
        Self {
            url: "ws://localhost:3000/signalk/v1/stream?subscribe=none".into(),
            token: None,
            access_request_href: None,
            no_auth_required: false,
        }
    }
}
impl Default for AggregateProfiles {
    fn default() -> Self {
        Self {
            default: vec!["mean".into(), "min".into(), "max".into()],
            slow: vec!["last".into()],
            opt_in: vec![],
        }
    }
}
impl Default for BindConfig {
    fn default() -> Self {
        Self {
            address: "0.0.0.0".into(),
            http_port: 8080,
            pg_port: Some(5432),
        }
    }
}
impl Default for QueryLimits {
    fn default() -> Self {
        Self {
            timeout_seconds: 30,
            memory_bytes: 1 << 30,
            unit_memory_bytes: 1536 << 20,
            target_partitions: 2,
            heavy_queries: 1,
            thermal_celsius: 75,
            pg_max_rows: 100_000,
            pg_max_bytes: 16 * 1024 * 1024,
        }
    }
}
impl Default for TiConfig {
    fn default() -> Self {
        Self {
            width_seconds: 10,
            store_root: "./ti".into(),
            retention_years: 2,
            field_cap: 2000,
            signal_k: SignalKConfig::default(),
            ingest: IngestConfig::default(),
            allow_paths: vec![],
            deny_paths: vec!["*.ais.*".into(), "design.*".into()],
            profiles: AggregateProfiles::default(),
            unit_scales: [
                ("rad", 4),
                ("m/s", 3),
                ("K", 2),
                ("V", 3),
                ("A", 2),
                ("W", 1),
                ("Pa", 0),
                ("ratio", 4),
                ("m", 2),
                ("Hz", 1),
                ("s", 0),
                ("J", 0),
                ("C", 0),
                ("lat/lon", 7),
                ("deg", 7),
                ("unknown", 3),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect(),
            path_scales: BTreeMap::new(),
            source_priorities: BTreeMap::new(),
            derived: vec![],
            rules: vec![],
            sources: crate::SourcesConfig::default(),
            units: BTreeMap::new(),
            bind: BindConfig::default(),
            auth: AuthConfig::default(),
            query: QueryLimits::default(),
            stores: BTreeMap::new(),
            sync: SyncConfig::default(),
        }
    }
}

fn invalid(key: &str, message: &str) -> Error {
    Error::InvalidInput(format!("{key}: {message}"))
}

impl AuthConfig {
    /// Parse only [auth]; other sections are intentionally ignored, never merged.
    pub fn from_toml(input: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct AuthOnly {
            #[serde(default)]
            auth: AuthConfig,
        }
        let auth: AuthOnly =
            toml::from_str(input).map_err(|e| invalid("pg auth config", &e.to_string()))?;
        let config = TiConfig {
            auth: auth.auth,
            ..TiConfig::default()
        };
        config.validate()?;
        Ok(config.auth)
    }
}

impl TiConfig {
    /// Parse TOML, then validate with key-qualified diagnostics.
    pub fn from_toml(input: &str) -> Result<Self> {
        let mut config: Self =
            toml::from_str(input).map_err(|e| invalid("ti.toml", &e.to_string()))?;
        for (unit, scale) in Self::default().unit_scales {
            config.unit_scales.entry(unit).or_insert(scale);
        }
        config.validate()?;
        Ok(config)
    }

    /// Reject incompatible width when opening an existing store.
    pub fn validate_store_width(&self, existing_width: u64) -> Result<()> {
        if self.width_seconds != existing_width {
            return Err(invalid(
                "width_seconds",
                "changing store width requires a rebuild",
            ));
        }
        self.validate()
    }

    /// Validate all configured limits, scale tables and names.
    pub fn validate(&self) -> Result<()> {
        if self.width_seconds == 0 {
            return Err(invalid("width_seconds", "must be positive"));
        }
        if self.store_root.trim().is_empty() {
            return Err(invalid("store_root", "must not be empty"));
        }
        if self.retention_years == 0 {
            return Err(invalid("retention_years", "must be positive"));
        }
        if self.field_cap == 0 {
            return Err(invalid("field_cap", "must be positive"));
        }
        let authority = self
            .signal_k
            .url
            .strip_prefix("ws://")
            .or_else(|| self.signal_k.url.strip_prefix("wss://"))
            .and_then(|rest| rest.split(['/', '?', '#']).next());
        if authority.is_none_or(str::is_empty) || self.signal_k.url.chars().any(char::is_whitespace)
        {
            return Err(invalid(
                "signal_k.url",
                "requires ws:// or wss:// and a nonempty authority without whitespace",
            ));
        }
        if self.signal_k.token.as_ref().is_some_and(|x| x.is_empty()) {
            return Err(invalid("signal_k.token", "must not be empty"));
        }
        if self
            .signal_k
            .access_request_href
            .as_ref()
            .is_some_and(|x| x.trim().is_empty())
        {
            return Err(invalid("signal_k.access_request_href", "must not be empty"));
        }
        let mut count_paths = BTreeSet::new();
        for path in &self.ingest.count_paths {
            if path.is_empty()
                || path
                    .chars()
                    .any(|c| c.is_whitespace() || "*@#$[]".contains(c))
                || path.split('.').any(str::is_empty)
                || !count_paths.insert(path)
            {
                return Err(invalid(
                    "ingest.count_paths",
                    "requires unique exact leaf paths without globs or aggregate suffixes",
                ));
            }
        }
        for (key, values) in [
            ("allow_paths", &self.allow_paths),
            ("deny_paths", &self.deny_paths),
        ] {
            if values.iter().any(|x| x.trim().is_empty()) {
                return Err(invalid(key, "empty pattern"));
            }
        }
        for (key, values) in [
            ("profiles.default", &self.profiles.default),
            ("profiles.slow", &self.profiles.slow),
            ("profiles.opt_in", &self.profiles.opt_in),
        ] {
            let mut seen = BTreeSet::new();
            for agg in values {
                if !["mean", "min", "max", "last", "count"].contains(&agg.as_str())
                    || !seen.insert(agg)
                {
                    return Err(invalid(key, "unknown or duplicate aggregate"));
                }
            }
            if key != "profiles.opt_in" && values.is_empty() {
                return Err(invalid(key, "must not be empty"));
            }
        }
        for (root, scales) in [
            ("unit_scales", &self.unit_scales),
            ("path_scales", &self.path_scales),
        ] {
            for (path, scale) in scales {
                if path.trim().is_empty() || *scale > 18 {
                    return Err(invalid(
                        &format!("{root}.{path}"),
                        "requires nonempty key and scale 0..=18",
                    ));
                }
            }
        }
        for (path, sources) in &self.source_priorities {
            let mut seen = BTreeSet::new();
            if path.is_empty()
                || sources.is_empty()
                || sources.iter().any(|s| s.is_empty() || !seen.insert(s))
            {
                return Err(invalid(
                    &format!("source_priorities.{path}"),
                    "requires unique nonempty sources",
                ));
            }
        }
        for (i, rule) in self.derived.iter().enumerate() {
            if rule.path.is_empty() || rule.output.is_empty() {
                return Err(invalid(
                    &format!("derived[{i}].path/output"),
                    "must not be empty",
                ));
            }
            if (rule.kind == DerivedKind::Transition
                && rule.state.as_ref().is_none_or(|s| s.is_empty()))
                || (rule.kind != DerivedKind::Transition && rule.state.is_some())
            {
                return Err(invalid(
                    &format!("derived[{i}].state"),
                    "only transitions require a nonempty state",
                ));
            }
        }
        for mapping in &self.sources.parquet {
            mapping.validate()?;
        }
        let backfill = &self.sources.backfill;
        if backfill.max_active_bytes == 0
            || backfill.max_index_entries == 0
            || backfill.max_journal_bytes == 0
        {
            return Err(Error::InvalidInput(
                "sources.backfill limits must be positive".into(),
            ));
        }
        for (pattern, entry) in &self.units {
            if pattern.is_empty() || entry.unit.is_empty() || entry.scale > 18 {
                return Err(invalid(
                    "units",
                    "requires nonempty patterns/units and scale <= 18",
                ));
            }
        }
        let mut rule_names = BTreeSet::new();
        for (i, rule) in self.rules.iter().enumerate() {
            let key = format!("rules[{i}]");
            if rule.name.is_empty()
                || !rule
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-.".contains(&c))
                || !rule_names.insert(&rule.name)
            {
                return Err(invalid(
                    &key,
                    "name must be unique and use letters, digits, _, - or .",
                ));
            }
            if !["alert", "warn", "alarm", "emergency"].contains(&rule.severity.as_str())
                || rule.when.trim().is_empty()
                || rule.message.trim().is_empty()
                || rule.max_per_hour == 0
                || rule.vessel.as_ref().is_some_and(|v| v.trim().is_empty())
            {
                return Err(invalid(
                    &key,
                    "requires severity, predicate, message and a positive hourly cap",
                ));
            }
            rule.hold_nanoseconds()
                .map_err(|e| invalid(&key, &e.to_string()))?;
        }
        if self.bind.address.parse::<IpAddr>().is_err() {
            return Err(invalid("bind.address", "must be an IPv4/IPv6 address"));
        }
        if self.bind.http_port == 0 {
            return Err(invalid("bind.http_port", "must be nonzero"));
        }
        if self.bind.pg_port == Some(0) || self.bind.pg_port == Some(self.bind.http_port) {
            return Err(invalid(
                "bind.pg_port",
                "must be nonzero and distinct from HTTP",
            ));
        }
        if self
            .auth
            .bearer_token
            .as_ref()
            .is_some_and(|x| x.is_empty())
        {
            return Err(invalid("auth.bearer_token", "must not be empty"));
        }
        let mut users = BTreeSet::new();
        for (i, user) in self.auth.scram_users.iter().enumerate() {
            if user.username.is_empty() || !users.insert(&user.username) {
                return Err(invalid(
                    &format!("auth.scram_users[{i}].username"),
                    "must be nonempty and unique",
                ));
            }
            if !user.verifier.starts_with("SCRAM-SHA-256$") {
                return Err(invalid(
                    &format!("auth.scram_users[{i}].verifier"),
                    "requires a SCRAM-SHA-256 verifier",
                ));
            }
        }
        for (key, value) in [
            ("query.timeout_seconds", self.query.timeout_seconds),
            ("query.memory_bytes", self.query.memory_bytes),
            ("query.unit_memory_bytes", self.query.unit_memory_bytes),
            (
                "query.target_partitions",
                self.query.target_partitions as u64,
            ),
            ("query.heavy_queries", self.query.heavy_queries as u64),
            ("query.thermal_celsius", self.query.thermal_celsius as u64),
        ] {
            if value == 0 {
                return Err(invalid(key, "must be positive"));
            }
        }
        if self.query.memory_bytes > self.query.unit_memory_bytes {
            return Err(invalid(
                "query.memory_bytes",
                "exceeds whole-unit memory cap",
            ));
        }

        // Multi-store validation (D30)
        if !self.stores.is_empty() {
            if !self.stores.contains_key("default") {
                return Err(invalid(
                    "stores",
                    "multi-store configuration requires a 'default' store",
                ));
            }

            let mut seen_roots = BTreeSet::new();
            for (name, store) in &self.stores {
                let prefix = format!("stores.{name}");

                let width_sec = parse_duration_seconds(&store.width, &format!("{prefix}.width"))?;
                if 3600 % width_sec != 0 {
                    return Err(invalid(
                        &format!("{prefix}.width"),
                        &format!("width ({width_sec}s) must divide 3600 evenly"),
                    ));
                }

                parse_retention(&store.retention, &format!("{prefix}.retention"))?;

                if let Some(shore) = &store.shore_retention {
                    parse_retention(shore, &format!("{prefix}.shore_retention"))?;
                }

                if let Some(r) = &store.root {
                    if r.trim().is_empty() {
                        return Err(invalid(&format!("{prefix}.root"), "must not be empty"));
                    }
                }

                let resolved_r = store.resolved_root(&self.store_root, name);
                if !seen_roots.insert(resolved_r.clone()) {
                    return Err(invalid(
                        &format!("{prefix}.root"),
                        &format!("duplicate store root '{resolved_r}'"),
                    ));
                }

                if let Some(paths) = &store.paths {
                    if paths.is_empty() {
                        return Err(invalid(
                            &format!("{prefix}.paths"),
                            "must not be empty if specified",
                        ));
                    }
                    for (p_idx, p) in paths.iter().enumerate() {
                        if p.trim().is_empty() {
                            return Err(invalid(
                                &format!("{prefix}.paths[{p_idx}]"),
                                "empty pattern",
                            ));
                        }
                    }
                }

                for (pattern, aggs) in &store.aggs {
                    if pattern.trim().is_empty() {
                        return Err(invalid(
                            &format!("{prefix}.aggs"),
                            "pattern must not be empty",
                        ));
                    }
                    if aggs.is_empty() {
                        return Err(invalid(
                            &format!("{prefix}.aggs.{pattern}"),
                            "aggregate list must not be empty",
                        ));
                    }
                    let mut seen = BTreeSet::new();
                    for agg in aggs {
                        if !["mean", "min", "max", "last", "count"].contains(&agg.as_str())
                            || !seen.insert(agg)
                        {
                            return Err(invalid(
                                &format!("{prefix}.aggs.{pattern}"),
                                "unknown or duplicate aggregate",
                            ));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Returns the resolved multi-store configuration map (D30).
    /// If `stores` is empty, synthesizes a single "default" store from top-level fields.
    pub fn resolved_stores(&self) -> BTreeMap<String, StoreConfig> {
        if !self.stores.is_empty() {
            let mut resolved = self.stores.clone();
            for (name, store) in &mut resolved {
                if store.root.is_none() {
                    store.root = Some(store.resolved_root(&self.store_root, name));
                }
            }
            resolved
        } else {
            let mut m = BTreeMap::new();
            m.insert(
                "default".to_string(),
                StoreConfig {
                    width: format!("{}s", self.width_seconds),
                    retention: format!("{}d", self.retention_years as u64 * 365),
                    shore_retention: None,
                    root: Some(self.store_root.clone()),
                    paths: if self.allow_paths.is_empty() {
                        None
                    } else {
                        Some(self.allow_paths.clone())
                    },
                    aggs: BTreeMap::new(),
                },
            );
            m
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn alert_rules_accept_literal_paths_and_interval_holds() {
        let config = TiConfig::from_toml(
            r#"
store_root = 'C:\boats\ti'
[[rules]]
name = 'battery'
severity = 'warn'
when = '"electrical.batteries.house.voltage@min" < 24.6'
for = '5m'
message = 'house battery {electrical.batteries.house.voltage@min} V'
"#,
        )
        .unwrap();
        assert_eq!(config.rules[0].max_per_hour, 60);
        assert_eq!(config.rules[0].hold_nanoseconds().unwrap(), 300_000_000_000);
        assert_eq!(
            parse_interval_duration_nanoseconds("0.001s").unwrap(),
            1_000_000
        );
        assert!(parse_interval_duration_nanoseconds("0.1ns").is_err());
        assert!(TiConfig::from_toml(
            r#"[[rules]]
name='battery'
severity='warn'
when='true'
message='battery'
max_per_hour=0"#
        )
        .is_err());
    }

    #[test]
    fn defaults_and_store_width_are_valid() {
        let c = TiConfig::from_toml("").unwrap();
        assert_eq!(c.width_seconds, 10);
        assert_eq!(c.retention_years, 2);
        assert_eq!(c.unit_scales["Hz"], 1);
        assert_eq!(c.unit_scales["lat/lon"], 7);
        assert!(c.validate_store_width(10).is_ok());
        assert!(c
            .validate_store_width(1)
            .unwrap_err()
            .to_string()
            .contains("width_seconds"));
    }
    #[test]
    fn partial_unit_registry_preserves_defaults_and_checks_connection() {
        let c = TiConfig::from_toml("[unit_scales]\nHz=2\ncustom=6").unwrap();
        assert_eq!(c.unit_scales["Hz"], 2);
        assert_eq!(c.unit_scales["rad"], 4);
        assert_eq!(c.unit_scales["custom"], 6);
        for url in ["ws://", "wss:///stream", "ws://boat name/stream"] {
            let input = format!("[signal_k]\nurl={url:?}");
            assert!(TiConfig::from_toml(&input)
                .unwrap_err()
                .to_string()
                .contains("signal_k.url"));
        }
    }
    #[test]
    fn access_href_is_preserved_and_security_off_is_explicit() {
        let c=TiConfig::from_toml("[signal_k]\naccess_request_href='/signalk/v1/requests/returned-id'\nno_auth_required=true").unwrap();
        assert_eq!(
            c.signal_k.access_request_href.as_deref(),
            Some("/signalk/v1/requests/returned-id")
        );
        assert!(c.signal_k.no_auth_required);
        assert!(TiConfig::from_toml("[signal_k]\naccess_request_href=''")
            .unwrap_err()
            .to_string()
            .contains("signal_k.access_request_href"));
    }
    #[test]
    fn connection_and_profiles_override() {
        let c=TiConfig::from_toml("[signal_k]\nurl='wss://boat/stream'\ntoken='device'\n[profiles]\nopt_in=['last','count']").unwrap();
        assert_eq!(c.signal_k.token.as_deref(), Some("device"));
        assert_eq!(c.profiles.opt_in.len(), 2);
        assert!(TiConfig::from_toml("[profiles]\nslow=['bogus']")
            .unwrap_err()
            .to_string()
            .contains("profiles.slow"));
    }
    #[test]
    fn bind_auth_scram_and_limits() {
        let c=TiConfig::from_toml("[bind]\naddress='::'\nhttp_port=8081\n[auth]\nbearer_token='token'\n[[auth.scram_users]]\nusername='ti'\nverifier='SCRAM-SHA-256$fixture'\n[query]\nmemory_bytes=536870912").unwrap();
        assert_eq!(c.bind.address, "::");
        assert_eq!(c.auth.scram_users[0].username, "ti");
        assert_eq!(c.query.memory_bytes, 512 << 20);
        assert!(TiConfig::from_toml("[query]\ntarget_partitions=0")
            .unwrap_err()
            .to_string()
            .contains("query.target_partitions"));
        assert!(TiConfig::from_toml("[bind]\naddress='localhost'")
            .unwrap_err()
            .to_string()
            .contains("bind.address"));
    }
    #[test]
    fn derived_scales_sources_and_strict_keys() {
        let c=TiConfig::from_toml("[path_scales]\n'propulsion.port.revolutions'=2\n[source_priorities]\n'navigation.speedOverGround'=['n2k.115']\n[[derived]]\npath='propulsion.*.state'\noutput='propulsion.*.state@starts'\nkind='transition'\nstate='started'").unwrap();
        assert_eq!(c.derived[0].kind, DerivedKind::Transition);
        assert_eq!(c.path_scales["propulsion.port.revolutions"], 2);
        for (input, key) in [
            ("width_seconds=0", "width_seconds"),
            ("[path_scales]\nx=19", "path_scales.x"),
            ("bad_key=1", "bad_key"),
            (
                "[[derived]]\npath='x'\noutput='y'\nkind='transition'",
                "derived[0].state",
            ),
        ] {
            assert!(TiConfig::from_toml(input)
                .unwrap_err()
                .to_string()
                .contains(key));
        }
    }

    #[test]
    fn d30_multi_store_config_roundtrip_and_validation() {
        let toml_str = r#"
[stores.default]
width = "10s"
retention = "730d"

[stores.hr]
width = "1s"
retention = "90d"
shore_retention = "90d"
paths = ["navigation.position", "navigation.speedOverGround"]
[stores.hr.aggs]
"navigation.*" = ["last"]
"environment.wind.*" = ["mean", "max"]
"environment.depth.*" = ["min"]
"#;
        let c = TiConfig::from_toml(toml_str).unwrap();
        assert_eq!(c.stores.len(), 2);
        let default_store = &c.stores["default"];
        assert_eq!(default_store.width_seconds().unwrap(), 10);
        assert_eq!(
            default_store.retention_seconds().unwrap(),
            Some(730 * 86400)
        );
        assert_eq!(
            default_store.resolved_root("/var/ti", "default"),
            "/var/ti/stores/default"
        );

        let hr_store = &c.stores["hr"];
        assert_eq!(hr_store.width_seconds().unwrap(), 1);
        assert_eq!(hr_store.retention_seconds().unwrap(), Some(90 * 86400));
        assert_eq!(
            hr_store.shore_retention_seconds().unwrap(),
            Some(90 * 86400)
        );
        assert_eq!(hr_store.paths.as_ref().unwrap().len(), 2);
        assert_eq!(hr_store.aggs["navigation.*"], vec!["last"]);
        assert_eq!(hr_store.aggs["environment.wind.*"], vec!["mean", "max"]);

        // Empty stores backward compatibility
        let empty_stores_cfg =
            TiConfig::from_toml("width_seconds = 10\nstore_root = '/var/ti'\nretention_years = 2")
                .unwrap();
        assert!(empty_stores_cfg.stores.is_empty());
        let resolved = empty_stores_cfg.resolved_stores();
        assert_eq!(resolved.len(), 1);
        assert!(resolved.contains_key("default"));
        assert_eq!(resolved["default"].width_seconds().unwrap(), 10);
        assert_eq!(resolved["default"].root.as_deref(), Some("/var/ti"));

        // Validation errors
        for (input, expected_err) in [
            (
                "[stores.hr]\nwidth = '1s'\nretention = '90d'",
                "stores: multi-store configuration requires a 'default' store",
            ),
            (
                "[stores.default]\nwidth = '7s'\nretention = '730d'",
                "stores.default.width: width (7s) must divide 3600 evenly",
            ),
            (
                "[stores.default]\nwidth = '10x'\nretention = '730d'",
                "stores.default.width: unknown duration suffix",
            ),
            (
                "[stores.default]\nwidth = '0s'\nretention = '730d'",
                "stores.default.width: duration must be positive",
            ),
            (
                "[stores.default]\nwidth = 'forever'\nretention = '730d'",
                "stores.default.width: unknown duration suffix",
            ),
            (
                "[stores.default]\nwidth = '10s'\nretention = '10foo'",
                "stores.default.retention: unknown duration suffix",
            ),
            (
                "[stores.default]\nwidth = '10s'\nretention = '730d'\n[stores.default.aggs]\n'x' = ['bogus']",
                "stores.default.aggs.x: unknown or duplicate aggregate",
            ),
            (
                "[stores.default]\nwidth = '10s'\nretention = '730d'\npaths = []",
                "stores.default.paths: must not be empty if specified",
            ),
            (
                "[stores.default]\nwidth = '10s'\nretention = '730d'\npaths = ['']",
                "stores.default.paths[0]: empty pattern",
            ),
            (
                "[stores.default]\nwidth = '10s'\nretention = '730d'\nroot = '/same'\n[stores.hr]\nwidth = '1s'\nretention = '90d'\nroot = '/same'",
                "stores.hr.root: duplicate store root '/same'",
            ),
        ] {
            let err = TiConfig::from_toml(input).unwrap_err().to_string();
            assert!(
                err.contains(expected_err),
                "Expected error containing '{expected_err}', got '{err}'"
            );
        }

        // Retention "forever" is allowed
        let forever_cfg =
            TiConfig::from_toml("[stores.default]\nwidth = '10s'\nretention = 'forever'").unwrap();
        assert_eq!(
            forever_cfg.stores["default"].retention_seconds().unwrap(),
            None
        );
    }
}
