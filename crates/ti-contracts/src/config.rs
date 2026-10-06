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
    /// Listener configuration.
    pub bind: BindConfig,
    /// Optional bearer/NUTS/SCRAM configuration.
    pub auth: AuthConfig,
    /// Query resource limits.
    pub query: QueryLimits,
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

/// Boat resource/admission defaults; lower memory_bytes to 512 MiB on Pi 4.
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
            bind: BindConfig::default(),
            auth: AuthConfig::default(),
            query: QueryLimits::default(),
        }
    }
}

fn invalid(key: &str, message: &str) -> Error {
    Error::InvalidInput(format!("{key}: {message}"))
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
