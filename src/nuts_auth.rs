//! D51: nuts.services RS256 verification and bounded, memory-only AHP exchange.
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const DEFAULT_URL: &str = "https://auth.nuts.services";
const REFRESH: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_JWKS: u64 = 1024 * 1024;
const MAX_EXCHANGE: u64 = 32 * 1024;
const MAX_CACHED_TOKENS: usize = 1024;

pub struct NutsConfig {
    url: String,
    allow: HashSet<String>,
}

impl NutsConfig {
    pub fn from_args(args: &[String]) -> Result<Option<Self>, String> {
        let auth = positions(args, "--nuts-auth")?;
        let allow = positions(args, "--nuts-allow")?;
        let Some(index) = auth else {
            if allow.is_some() {
                return Err("--nuts-allow requires --nuts-auth".into());
            }
            return Ok(None);
        };
        let url = args
            .get(index + 1)
            .filter(|v| !v.starts_with("--"))
            .map(String::as_str)
            .unwrap_or(DEFAULT_URL);
        let allow = allow
            .and_then(|i| args.get(i + 1))
            .filter(|v| !v.starts_with("--"))
            .ok_or("--nuts-auth requires --nuts-allow <emails or user_ids, comma list or @file>")?;
        Self::new(url, allow).map(Some)
    }

    fn new(base: &str, allow: &str) -> Result<Self, String> {
        let parsed = url::Url::parse(base).map_err(|_| "Invalid nuts auth URL")?;
        let local = parsed.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !(parsed.scheme() == "https" || parsed.scheme() == "http" && local)
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("nuts auth URL must be HTTPS (HTTP allowed only on loopback), without credentials, query or fragment".into());
        }
        let text = if let Some(path) = allow.strip_prefix('@') {
            let file = std::fs::File::open(path).map_err(|_| "Cannot read nuts allowlist file")?;
            let mut text = String::new();
            file.take(65537)
                .read_to_string(&mut text)
                .map_err(|_| "Cannot read nuts allowlist file")?;
            text
        } else {
            allow.to_string()
        };
        if text.len() > 65536 {
            return Err("nuts allowlist exceeds 64 KiB".into());
        }
        let allow: HashSet<String> = text
            .split([',', '\n', '\r'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if allow.is_empty() {
            return Err("nuts allowlist must not be empty".into());
        }
        Ok(Self {
            url: parsed.as_str().trim_end_matches('/').into(),
            allow,
        })
    }
}

fn positions(args: &[String], flag: &str) -> Result<Option<usize>, String> {
    let mut matches = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| arg.as_str() == flag);
    let first = matches.next().map(|(i, _)| i);
    if matches.next().is_some() {
        return Err(format!("{flag} must be specified only once"));
    }
    Ok(first)
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    user_id: serde_json::Value,
    scopes: Vec<String>,
    exp: u64,
    iat: u64,
}
#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}
#[derive(Deserialize)]
struct Jwk {
    kty: String,
    n: Option<String>,
    e: Option<String>,
    kid: Option<String>,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
}
struct Key {
    kid: Option<String>,
    key: DecodingKey,
}
struct CachedToken {
    jwt: String,
    exp: u64,
}

/// Contains no plaintext AHP tokens. Deliberately has no Debug implementation.
pub struct NutsAuth {
    config: NutsConfig,
    cache: PathBuf,
    agent: ureq::Agent,
    keys: RwLock<Vec<Key>>,
    tokens: Mutex<HashMap<[u8; 32], CachedToken>>,
}

impl NutsAuth {
    pub fn open(config: NutsConfig, root: &Path) -> Result<Arc<Self>, String> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .redirects(0)
            .build();
        let cache = root.join("auth/jwks.json");
        let fetched = fetch_keys(&agent, &config.url);
        let (keys, document) = match fetched {
            Ok(value) => value,
            Err(_) => {
                let mut text = String::new();
                std::fs::File::open(&cache)
                    .and_then(|file| file.take(MAX_JWKS + 1).read_to_string(&mut text))
                    .map_err(|_| "nuts auth cannot start: JWKS unavailable and no usable cache")?;
                let document: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|_| "nuts auth cannot start: invalid JWKS cache")?;
                if text.len() as u64 > MAX_JWKS
                    || document["lume_auth_url"].as_str() != Some(&config.url)
                {
                    return Err("nuts auth cannot start: JWKS cache does not match auth URL".into());
                }
                let keys = parse_keys(&document)?;
                eprintln!("nuts auth: JWKS fetch unavailable; using cached public keys");
                (keys, document)
            }
        };
        // Cache only public keys, bound to the configured service URL.
        write_cache(&cache, &document)?;
        let auth = Arc::new(Self {
            config,
            cache,
            agent,
            keys: RwLock::new(keys),
            tokens: Mutex::new(HashMap::new()),
        });
        let weak = Arc::downgrade(&auth);
        std::thread::Builder::new()
            .name("nuts-jwks-refresh".into())
            .spawn(move || {
                let mut last = Instant::now();
                loop {
                    std::thread::sleep(Duration::from_secs(60));
                    let Some(auth) = weak.upgrade() else {
                        break;
                    };
                    if last.elapsed() >= REFRESH {
                        auth.refresh();
                        last = Instant::now();
                    }
                }
            })
            .map_err(|_| "Cannot start nuts JWKS refresh worker")?;
        Ok(auth)
    }

    fn refresh(&self) {
        match fetch_keys(&self.agent, &self.config.url) {
            Ok((keys, document)) => {
                if write_cache(&self.cache, &document).is_err() {
                    eprintln!("nuts auth: cannot update public JWKS cache");
                }
                if let Ok(mut guard) = self.keys.write() {
                    *guard = keys;
                }
            }
            Err(_) => {
                eprintln!("nuts auth: JWKS refresh unavailable; retaining cached public keys")
            }
        }
    }

    pub(crate) fn accepts(&self, token: &str, scope: &str) -> bool {
        if token.len() > 8192 {
            return false;
        }
        if token.starts_with("ahp_") {
            self.exchanged(token)
                .and_then(|jwt| self.verify(&jwt))
                .is_some_and(|claims| claims.scopes.iter().any(|s| s == scope))
        } else {
            self.verify(token)
                .is_some_and(|claims| claims.scopes.iter().any(|s| s == scope))
        }
    }

    fn verify(&self, token: &str) -> Option<Claims> {
        let header = decode_header(token).ok()?;
        if header.alg != Algorithm::RS256 {
            return None;
        }
        let mut validation = Validation::new(Algorithm::RS256);
        validation.leeway = 60;
        validation.validate_aud = false; // nuts-auth does not issue iss/aud.
        validation.validate_nbf = true;
        validation.set_required_spec_claims(&["sub", "exp"]);
        let keys = self.keys.read().ok()?;
        for key in keys.iter().filter(|key| {
            header
                .kid
                .as_ref()
                .is_none_or(|kid| key.kid.as_ref() == Some(kid))
        }) {
            if let Ok(decoded) = decode::<Claims>(token, &key.key, &validation) {
                let claims = decoded.claims;
                let uid = match &claims.user_id {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => return None,
                };
                if claims.iat > now().saturating_add(60)
                    || claims.exp < claims.iat
                    || !(self.config.allow.contains(&claims.sub)
                        || self.config.allow.contains(&uid))
                {
                    return None;
                }
                return Some(claims);
            }
        }
        None
    }

    fn exchanged(&self, token: &str) -> Option<String> {
        let digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
        let key: [u8; 32] = digest.as_ref().try_into().ok()?;
        let mut cache = self.tokens.lock().ok()?;
        let time = now();
        cache.retain(|_, value| value.exp > time);
        if let Some(value) = cache.get(&key) {
            return Some(value.jwt.clone());
        }
        // Serialize exchanges so simultaneous requests for one AHP don't duplicate work.
        // This lock is independent of offline JWT verification and JWKS refresh.
        let response = self
            .agent
            .post(&format!("{}/auth", self.config.url))
            .send_form(&[("token", token)])
            .ok()?;
        let document = read_json(response, MAX_EXCHANGE).ok()?;
        let jwt = document["access_token"].as_str()?.to_owned();
        if document["token_type"]
            .as_str()
            .is_none_or(|s| !s.eq_ignore_ascii_case("bearer"))
        {
            return None;
        }
        let claims = self.verify(&jwt)?;
        if cache.len() >= MAX_CACHED_TOKENS {
            let victim = cache
                .iter()
                .min_by_key(|(_, value)| value.exp)
                .map(|(key, _)| *key)?;
            cache.remove(&victim);
        }
        if claims.exp > time {
            cache.insert(
                key,
                CachedToken {
                    jwt: jwt.clone(),
                    exp: claims.exp,
                },
            );
        }
        Some(jwt)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn read_json(response: ureq::Response, cap: u64) -> Result<serde_json::Value, String> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Auth service response could not be read")?;
    if bytes.len() as u64 > cap {
        return Err("Auth service response exceeds limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid auth service JSON".into())
}
fn parse_keys(document: &serde_json::Value) -> Result<Vec<Key>, String> {
    let jwks: Jwks = serde_json::from_value(document.clone()).map_err(|_| "Invalid JWKS")?;
    if jwks.keys.len() > 32 {
        return Err("JWKS has too many keys".into());
    }
    let mut keys = Vec::new();
    for key in jwks.keys {
        if key.kty != "RSA"
            || key.alg.as_deref().is_some_and(|v| v != "RS256")
            || key.usage.as_deref().is_some_and(|v| v != "sig")
        {
            continue;
        }
        let (Some(n), Some(e)) = (key.n, key.e) else {
            continue;
        };
        if !(342..=1366).contains(&n.len()) || e.len() > 16 {
            continue;
        }
        let decoding =
            DecodingKey::from_rsa_components(&n, &e).map_err(|_| "Invalid RSA public key")?;
        keys.push(Key {
            kid: key.kid,
            key: decoding,
        });
    }
    if keys.is_empty() {
        return Err("JWKS has no usable RS256 keys".into());
    }
    Ok(keys)
}
fn fetch_keys(agent: &ureq::Agent, base: &str) -> Result<(Vec<Key>, serde_json::Value), String> {
    let response = agent
        .get(&format!("{base}/.well-known/jwks.json"))
        .call()
        .map_err(|_| "JWKS fetch failed")?;
    let mut document = read_json(response, MAX_JWKS)?;
    let keys = parse_keys(&document)?;
    document["lume_auth_url"] = serde_json::json!(base);
    Ok((keys, document))
}
fn write_cache(path: &Path, document: &serde_json::Value) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("Invalid JWKS cache path")?;
    std::fs::create_dir_all(parent).map_err(|_| "Cannot create JWKS cache directory")?;
    let temporary = parent.join(format!(".jwks-{}.tmp", crate::uuid_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| "Cannot write JWKS cache")?;
        file.write_all(document.to_string().as_bytes())
            .map_err(|_| "Cannot write JWKS cache")?;
        file.sync_all().map_err(|_| "Cannot sync JWKS cache")?;
        // Windows rename cannot replace an existing file. Removing public metadata
        // first is safe: a failed replacement refuses offline startup rather than trusting old bytes.
        #[cfg(windows)]
        if path.exists() {
            std::fs::remove_file(path).map_err(|_| "Cannot replace JWKS cache")?;
        }
        std::fs::rename(&temporary, path).map_err(|_| "Cannot publish JWKS cache")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
