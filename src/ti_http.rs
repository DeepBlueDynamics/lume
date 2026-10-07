//! Read-only HTTP surfaces sharing one startup snapshot and runtime.
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
use ti_contracts::Catalog;

pub(crate) struct RequestHeaders<'a>(&'a str);
impl<'a> RequestHeaders<'a> {
    pub(crate) fn get(&self, name: &str) -> Option<&'a str> {
        for line in self.0.lines() {
            if let Some((k, v)) = line.split_once(':') {
                if k.trim().eq_ignore_ascii_case(name) {
                    return Some(v.trim());
                }
            }
        }
        None
    }
    pub(crate) fn bearer_token(&self) -> Option<&'a str> {
        self.get("authorization").and_then(|val| {
            val.strip_prefix("Bearer ")
                .or_else(|| val.strip_prefix("bearer "))
                .map(str::trim)
        })
    }
}

pub struct TiServer {
    pub(crate) engine: RwLock<Arc<ti_sql::TiEngine>>,
    pub(crate) runtime: ti_sql::SurfaceRuntime,
    root: PathBuf,
    pub(crate) gate: Mutex<()>,
    resolver: RwLock<Arc<crate::ti_resolve::PathsResolver>>,
    width: Option<u64>,
    last_reload: Mutex<Option<Instant>>,
    pg_auth_users: Option<Vec<ti_contracts::ScramUser>>,
    store: Arc<Mutex<ti_store::Store>>,
    receiver: Arc<ti_sync::ShoreReceiver>,
    sync_token: Option<String>,
    docs_index: Option<Mutex<crate::ti_docs_index::DocsIndex>>,
    docs_reload: Mutex<()>,
    query_limits: RwLock<ti_contracts::QueryLimits>,
    pg_batches_yielded: Arc<std::sync::atomic::AtomicUsize>,
    pg_query_completed: Arc<std::sync::atomic::AtomicBool>,
}

impl TiServer {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn open(root: &Path) -> Result<Self, String> {
        Self::open_with_width(root, None)
    }

    pub fn open_with_width(root: &Path, width: Option<u64>) -> Result<Self, String> {
        let runtime = ti_sql::surface_runtime().map_err(|e| e.to_string())?;
        let factory = |root: &Path, store: &ti_store::Store, width: u64| {
            Ok(Arc::new(crate::ti_text::LumeText::open(
                root,
                store.catalog().clone(),
                width,
            )?) as Arc<dyn ti_contracts::DocumentIndex>)
        };
        let engine = runtime
            .block_on(ti_sql::TiEngine::open(root, width, Some(&factory)))
            .map_err(|e| e.to_string())?;
        runtime
            .block_on(ti_sql::postgres::register(&engine))
            .map_err(|e| e.to_string())?;
        let resolver = crate::ti_resolve::PathsResolver::new(&engine.session.catalog);
        let canonical_root = if root.exists() {
            root.canonicalize().map_err(|e| e.to_string())?
        } else {
            root.to_path_buf()
        };
        let store_width = width.unwrap_or(engine.session.catalog.width_seconds);
        let store = Arc::new(Mutex::new(
            ti_store::Store::open_or_create(root, store_width).map_err(|e| e.to_string())?,
        ));
        let receiver = Arc::new(ti_sync::ShoreReceiver::new(store.clone()));

        let ti_toml = root.join("ti.toml");
        let (sync_token, query_limits) = if ti_toml.exists() {
            let content = std::fs::read_to_string(&ti_toml)
                .map_err(|e| format!("failed to read ti.toml: {e}"))?;
            let cfg = ti_contracts::TiConfig::from_toml(&content)
                .map_err(|e| format!("invalid ti.toml: {e}"))?;
            if cfg.sync.token_file.is_none() && cfg.sync.token.is_some() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let meta = std::fs::metadata(&ti_toml)
                        .map_err(|e| format!("cannot stat ti.toml: {e}"))?;
                    if meta.permissions().mode() & 0o077 != 0 {
                        return Err("ti.toml containing inline sync token must not be group- or world-accessible (use chmod 600, or prefer token_file)".into());
                    }
                }
            }
            (cfg.sync.resolved_token()?, cfg.query)
        } else {
            (None, ti_contracts::QueryLimits::default())
        };

        Ok(Self {
            resolver: RwLock::new(Arc::new(resolver)),
            engine: RwLock::new(Arc::new(engine)),
            runtime,
            root: canonical_root,
            gate: Mutex::new(()),
            width,
            last_reload: Mutex::new(None),
            pg_auth_users: None,
            store,
            receiver,
            sync_token,
            docs_index: None,
            docs_reload: Mutex::new(()),
            query_limits: RwLock::new(query_limits),
            pg_batches_yielded: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            pg_query_completed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// Register an ordinary index; an absent index starts with an empty sections table.
    pub fn with_docs_index(mut self, root: &Path) -> Result<Self, String> {
        self.docs_index = Some(Mutex::new(crate::ti_docs_index::DocsIndex::new(root)));
        self.force_reload_engine()?;
        Ok(self)
    }

    pub(crate) fn refresh_docs_index(&self) {
        let Some(index) = &self.docs_index else {
            return;
        };
        let Ok(_reload) = self.docs_reload.try_lock() else {
            return;
        };
        let Ok(mut index) = index.try_lock() else {
            return;
        };
        if !index.ready(Instant::now()) {
            return;
        }
        // Release the watcher lock before opening the engine, which also reads this path.
        drop(index);
        match self.force_reload_engine() {
            Ok(()) => {
                if let Ok(mut index) = self.docs_index.as_ref().unwrap().lock() {
                    index.acknowledge();
                }
            }
            Err(error) => {
                eprintln!("Library index reload failed; retaining previous snapshot: {error}");
                if let Ok(mut index) = self.docs_index.as_ref().unwrap().lock() {
                    // Retry when a subsequent publication changes the marker.
                    index.acknowledge();
                }
            }
        }
    }

    /// External auth replaces store auth entirely; ingest/query configuration is untouched.
    pub fn with_pg_auth_config(mut self, path: &Path) -> Result<Self, String> {
        self.pg_auth_users = Some(crate::ti_pg_auth::load_users(path)?);
        Ok(self)
    }

    pub fn with_sync_token(mut self, token: impl Into<String>) -> Self {
        self.sync_token = Some(token.into());
        self
    }

    pub fn query_limits(&self) -> ti_contracts::QueryLimits {
        self.query_limits.read().unwrap().clone()
    }

    pub fn with_query_limits(self, limits: ti_contracts::QueryLimits) -> Self {
        *self.query_limits.write().unwrap() = limits;
        self
    }

    pub fn pg_batches_yielded(&self) -> usize {
        self.pg_batches_yielded
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn pg_query_completed(&self) -> bool {
        self.pg_query_completed
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn pg_query_started(&self) {
        self.pg_query_completed
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.pg_batches_yielded
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn pg_batch_produced(&self) {
        self.pg_batches_yielded
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn pg_query_finished(&self) {
        self.pg_query_completed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn reload_engine(&self) -> Result<(), String> {
        let now = Instant::now();
        {
            let mut last = self.last_reload.lock().map_err(|e| e.to_string())?;
            if let Some(prev) = *last {
                if now.duration_since(prev) < Duration::from_secs(5) {
                    return Ok(());
                }
            }
            *last = Some(now);
        }
        self.force_reload_engine()
    }

    pub fn force_reload_engine(&self) -> Result<(), String> {
        let factory = |root: &Path, store: &ti_store::Store, width: u64| {
            Ok(Arc::new(crate::ti_text::LumeText::open(
                root,
                store.catalog().clone(),
                width,
            )?) as Arc<dyn ti_contracts::DocumentIndex>)
        };
        let engine = self
            .runtime
            .block_on(ti_sql::TiEngine::open(
                &self.root,
                self.width,
                Some(&factory),
            ))
            .map_err(|e| e.to_string())?;
        if let Some(index) = &self.docs_index {
            let root = index.lock().map_err(|e| e.to_string())?.root.clone();
            crate::ti_docs_index::register(&engine.session, &root)?;
        }
        self.runtime
            .block_on(ti_sql::postgres::register(&engine))
            .map_err(|e| e.to_string())?;
        let resolver = crate::ti_resolve::PathsResolver::new(&engine.session.catalog);
        let _guard = self.gate.lock().map_err(|e| e.to_string())?;
        let mut engine_guard = self.engine.write().map_err(|e| e.to_string())?;
        *engine_guard = Arc::new(engine);
        let mut resolver_guard = self.resolver.write().map_err(|e| e.to_string())?;
        *resolver_guard = Arc::new(resolver);
        let path = self.root.join("ti.toml");
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(cfg) = ti_contracts::TiConfig::from_toml(&content) {
                    if let Ok(mut ql) = self.query_limits.write() {
                        *ql = cfg.query;
                    }
                }
            }
        }
        Ok(())
    }
    pub(crate) fn pg_describe(&self, sql: &str, hints: &[String]) -> Result<Value, String> {
        self.refresh_docs_index();
        let engine = self.engine.read().map_err(|e| e.to_string())?.clone();
        let _guard = self.gate.lock().map_err(|e| e.to_string())?;
        engine
            .session
            .reset_diagnostics()
            .map_err(|e| e.to_string())?;
        self.runtime
            .block_on(ti_sql::postgres::describe(&engine, sql, hints))
            .map_err(|e| e.to_string())
    }
    #[allow(dead_code)]
    pub(crate) fn pg_query(
        &self,
        sql: &str,
        parameters: Vec<ti_sql::postgres::Parameter>,
    ) -> Result<Value, String> {
        self.refresh_docs_index();
        let engine = self.engine.read().map_err(|e| e.to_string())?.clone();
        let _guard = self.gate.lock().map_err(|e| e.to_string())?;
        engine
            .session
            .reset_diagnostics()
            .map_err(|e| e.to_string())?;
        self.runtime
            .block_on(ti_sql::postgres::query(&engine, sql, parameters))
            .map_err(|e| e.to_string())
    }
    /// Fail before starting ingestion when pg credentials or listener policy are invalid.
    pub fn validate_pg_auth(&self, bind: &str) -> Result<(), String> {
        let ip: std::net::IpAddr = bind.parse().map_err(|_| "Invalid pg bind address")?;
        crate::ti_pg_auth::AuthConfig::new(self.pg_users()?, !ip.is_loopback(), false)?;
        Ok(())
    }
    pub(crate) fn pg_users(&self) -> Result<Vec<ti_contracts::ScramUser>, String> {
        if let Some(users) = &self.pg_auth_users {
            return Ok(users.clone());
        }
        let path = self.root.join("ti.toml");
        if !path.exists() {
            return Ok(vec![]);
        }
        Ok(ti_contracts::TiConfig::from_toml(
            &std::fs::read_to_string(path).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .auth
        .scram_users)
    }
    pub(crate) fn mcp_width(&self) -> Option<u64> {
        self.engine
            .read()
            .ok()
            .map(|engine| engine.session.catalog.width_seconds)
    }
    pub fn mcp(&self, name: &str, args: &Value) -> Result<String, String> {
        if !args.is_object() {
            return Err("arguments must be an object".into());
        }
        if let Some(root) = args.get("store") {
            let root = root.as_str().ok_or("store must be a string")?;
            // Models often send "" for an optional string; that means the served store.
            if !root.trim().is_empty()
                && Path::new(root).canonicalize().map_err(|e| e.to_string())? != self.root
            {
                return Err("store must match the server's --ti-store".into());
            }
        }
        self.refresh_docs_index();
        let engine = self.engine.read().map_err(|e| e.to_string())?.clone();
        if let Some(width) = args.get("width_seconds") {
            if width.as_u64() != Some(engine.session.catalog.width_seconds) {
                return Err(format!(
                    "store bucket width mismatch: this store\'s width is {} s; omit width_seconds",
                    engine.session.catalog.width_seconds
                ));
            }
        }
        let _guard = self.gate.lock().map_err(|e| e.to_string())?;
        engine
            .session
            .reset_diagnostics()
            .map_err(|e| e.to_string())?;
        let mut reply = self.dispatch(name, args, &engine)?;
        crate::ti_mcp::add_argument_notes(&mut reply, name, args);
        serde_json::to_string(&reply).map_err(|e| e.to_string())
    }
    fn dispatch(
        &self,
        name: &str,
        args: &Value,
        engine: &ti_sql::TiEngine,
    ) -> Result<Value, String> {
        if name == "ti_resolve" {
            let resolver = self.resolver.read().map_err(|e| e.to_string())?.clone();
            self.runtime.block_on(resolver.resolve(engine, args))
        } else {
            self.runtime
                .block_on(crate::ti_mcp::dispatch(engine, name, args))
        }
    }

    fn response(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        headers_raw: &str,
    ) -> Result<Reply, String> {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let req_headers = RequestHeaders(headers_raw);

        let is_sync_endpoint = path == "/ti/manifest" || path.starts_with("/ti/shards/");
        if is_sync_endpoint {
            let Some(server_token) = &self.sync_token else {
                return Ok(Reply::error(
                    401,
                    "Unauthorized: sync bearer token is not configured in ti.toml [sync].",
                ));
            };
            let Some(client_token) = req_headers.bearer_token() else {
                return Ok(Reply::error(
                    401,
                    "Unauthorized: Authorization: Bearer <token> required for sync endpoints.",
                ));
            };
            if !ti_sync::constant_time_bearer_eq(client_token, server_token) {
                return Ok(Reply::error(
                    401,
                    "Unauthorized: invalid bearer token for sync endpoints.",
                ));
            }

            if path == "/ti/manifest" {
                if method != "GET" {
                    return Ok(Reply::error(405, "Method Not Allowed"));
                }
                let (entries, vessels) = {
                    let store = self.store.lock().map_err(|e| e.to_string())?;
                    let entries = store.manifest().entries();
                    let mut vessels = BTreeMap::new();
                    let mut ord = 0u32;
                    while let Ok(urn) = store.catalog().vessel_urn(ord) {
                        vessels.insert(ord.to_string(), urn);
                        ord += 1;
                    }
                    (entries, vessels)
                };
                return Ok(Reply::json(
                    200,
                    json!({ "entries": entries, "vessels": vessels }),
                ));
            }

            if let Some(rest) = path.strip_prefix("/ti/shards/") {
                let parts: Vec<&str> = rest.split('/').collect();
                if parts.len() < 4 {
                    return Ok(Reply::error(400, "Invalid /ti/shards/ path"));
                }
                let vessel_urn = ti_sync::percent_decode(parts[0])
                    .map_err(|e| format!("Invalid vessel URN in path: {e}"))?;
                if vessel_urn.is_empty()
                    || vessel_urn.len() > 512
                    || ti_contracts::validate_entity_urn(&vessel_urn).is_err()
                {
                    return Ok(Reply::error(
                        400,
                        "Invalid vessel URN: must be a canonical entity URN (e.g. vessels.urn:...) under 512 bytes",
                    ));
                }
                let shard: u32 = parts[1]
                    .parse()
                    .map_err(|e| format!("Invalid shard number in path: {e}"))?;
                let version: u64 = parts[2]
                    .parse()
                    .map_err(|e| format!("Invalid version in path: {e}"))?;
                let action = parts[3];

                if action == "status" {
                    if method != "GET" {
                        return Ok(Reply::error(405, "Method Not Allowed"));
                    }
                    let hash = req_headers
                        .get("x-shard-hash")
                        .and_then(|h| ti_sync::hex_decode_32(h).ok())
                        .unwrap_or([0u8; 32]);
                    let transfer = ti_contracts::TransferIdentity {
                        vessel_urn,
                        shard,
                        version,
                        width_seconds: 0,
                        from: 0,
                        to: 0,
                        hash,
                        catalog_hash: [0u8; 32],
                    };
                    let status = match self.receiver.upload_status(&transfer) {
                        Ok(s) => s,
                        Err(ti_contracts::Error::InvalidInput(msg)) => {
                            return Ok(Reply::error(400, &msg));
                        }
                        Err(e) => return Err(e.to_string()),
                    };
                    return Ok(Reply::json(
                        200,
                        serde_json::to_value(&status).map_err(|e| e.to_string())?,
                    ));
                } else if action == "chunks" {
                    if method != "POST" {
                        return Ok(Reply::error(405, "Method Not Allowed"));
                    }
                    if parts.len() != 5 {
                        return Ok(Reply::error(
                            400,
                            "Missing chunk index in /ti/shards/.../chunks/{n}",
                        ));
                    }
                    let chunk_index: u32 = parts[4]
                        .parse()
                        .map_err(|e| format!("Invalid chunk index: {e}"))?;
                    let total_chunks: u32 = req_headers
                        .get("x-total-chunks")
                        .and_then(|v| v.parse().ok())
                        .ok_or("Missing or invalid X-Total-Chunks")?;
                    let offset: u64 = req_headers
                        .get("x-offset")
                        .and_then(|v| v.parse().ok())
                        .ok_or("Missing or invalid X-Offset")?;
                    let total_bytes: u64 = req_headers
                        .get("x-total-bytes")
                        .and_then(|v| v.parse().ok())
                        .ok_or("Missing or invalid X-Total-Bytes")?;
                    let shard_hash = req_headers
                        .get("x-shard-hash")
                        .and_then(|h| ti_sync::hex_decode_32(h).ok())
                        .unwrap_or([0u8; 32]);
                    let catalog_hash = req_headers
                        .get("x-catalog-hash")
                        .and_then(|h| ti_sync::hex_decode_32(h).ok())
                        .unwrap_or([0u8; 32]);
                    let from = req_headers
                        .get("x-from")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let to = req_headers
                        .get("x-to")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let width_seconds = req_headers
                        .get("x-width-seconds")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(10);

                    let chunk = ti_sync::UploadChunk::new(
                        ti_contracts::TransferIdentity {
                            vessel_urn,
                            shard,
                            version,
                            width_seconds,
                            from,
                            to,
                            hash: shard_hash,
                            catalog_hash,
                        },
                        chunk_index,
                        total_chunks,
                        offset,
                        total_bytes,
                        body.to_vec(),
                    );
                    if let Some(h) = req_headers
                        .get("x-blake3")
                        .or_else(|| req_headers.get("content-digest"))
                    {
                        let expected = ti_sync::hex_decode_32(h)
                            .map_err(|e| format!("Invalid X-BLAKE3 header: {e}"))?;
                        if chunk.chunk_hash != expected {
                            return Ok(Reply::error(400, "X-BLAKE3 digest mismatch"));
                        }
                    }
                    let ack = match self.receiver.receive_chunk(&chunk) {
                        Ok(a) => a,
                        Err(ti_contracts::Error::InvalidInput(msg)) => {
                            return Ok(Reply::error(400, &msg));
                        }
                        Err(e) => return Err(e.to_string()),
                    };
                    return Ok(Reply::json(
                        200,
                        serde_json::to_value(&ack).map_err(|e| e.to_string())?,
                    ));
                } else if action == "commit" {
                    if method != "POST" {
                        return Ok(Reply::error(405, "Method Not Allowed"));
                    }
                    let transfer: ti_contracts::TransferIdentity = if !body.is_empty() {
                        let t: ti_contracts::TransferIdentity = serde_json::from_slice(body)
                            .map_err(|e| format!("Invalid commit JSON: {e}"))?;
                        if t.vessel_urn.is_empty()
                            || t.vessel_urn.len() > 512
                            || ti_contracts::validate_entity_urn(&t.vessel_urn).is_err()
                        {
                            return Ok(Reply::error(
                                400,
                                "Invalid vessel URN: must be a canonical entity URN (e.g. vessels.urn:...) under 512 bytes",
                            ));
                        }
                        t
                    } else {
                        let shard_hash = req_headers
                            .get("x-shard-hash")
                            .and_then(|h| ti_sync::hex_decode_32(h).ok())
                            .ok_or("Missing X-Shard-Hash header")?;
                        let catalog_hash = req_headers
                            .get("x-catalog-hash")
                            .and_then(|h| ti_sync::hex_decode_32(h).ok())
                            .unwrap_or([0u8; 32]);
                        let from = req_headers
                            .get("x-from")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        let to = req_headers
                            .get("x-to")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        let width_seconds = req_headers
                            .get("x-width-seconds")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(10);
                        ti_contracts::TransferIdentity {
                            vessel_urn,
                            shard,
                            version,
                            width_seconds,
                            from,
                            to,
                            hash: shard_hash,
                            catalog_hash,
                        }
                    };
                    let entry = match self.receiver.commit_upload(&transfer) {
                        Ok(e) => e,
                        Err(ti_contracts::Error::InvalidInput(msg)) => {
                            return Ok(Reply::error(400, &msg));
                        }
                        Err(e) => return Err(e.to_string()),
                    };
                    let _ = self.reload_engine();
                    return Ok(Reply::json(
                        200,
                        serde_json::to_value(&entry).map_err(|e| e.to_string())?,
                    ));
                } else {
                    return Ok(Reply::error(404, "Unknown shard action"));
                }
            }
        }

        let expected = match path {
            "/ti/query" => {
                if method == "GET" || method == "POST" {
                    method
                } else {
                    "POST"
                }
            }
            "/ti/explain" => "POST",
            "/ti/schema" | "/ti/status" | "/ti/resolve" => "GET",
            _ => return Ok(Reply::error(404, "Unknown TI endpoint")),
        };
        if method != expected {
            return Ok(Reply::error(405, "Method not allowed"));
        }
        let args: Value = if path == "/ti/resolve" {
            resolve_args(query)?
        } else if path == "/ti/query" && method == "GET" {
            query_args(query)?
        } else if method == "POST" {
            serde_json::from_slice(body).map_err(|e| e.to_string())?
        } else {
            json!({})
        };
        if !args.is_object() {
            return Err("body must be a JSON object".into());
        }
        self.refresh_docs_index();
        let engine = self.engine.read().map_err(|e| e.to_string())?.clone();
        let _guard = self.gate.lock().map_err(|e| e.to_string())?;
        engine
            .session
            .reset_diagnostics()
            .map_err(|e| e.to_string())?;
        let accept = req_headers.get("accept").unwrap_or("");
        if path == "/ti/query"
            && !accept
                .split(',')
                .any(|v| v.trim().split(';').next() == Some("application/json"))
        {
            let sql = args
                .get("sql")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("sql is required")?;
            let limit = args
                .get("max_rows")
                .map(|v| {
                    v.as_u64()
                        .filter(|n| *n > 0)
                        .ok_or("max_rows must be a positive integer")
                })
                .transpose()?
                .unwrap_or(500)
                .min(500) as usize;
            let (body, count, truncated) = self
                .runtime
                .block_on(engine.query_arrow(sql, limit))
                .map_err(|e| e.to_string())?;
            return Ok(Reply {
                status: 200,
                kind: "application/vnd.apache.arrow.stream",
                body,
                extra: format!(
                    "X-TI-Row-Count: {count}\r\nX-TI-Truncated: {truncated}\r\n{}",
                    if truncated {
                        "X-TI-Hint: Aggregate results or narrow the time range.\r\n"
                    } else {
                        ""
                    }
                ),
            });
        }
        let name = match path {
            "/ti/query" => "ti_query",
            "/ti/explain" => "ti_explain",
            "/ti/schema" => "ti_schema",
            "/ti/resolve" => "ti_resolve",
            _ => "ti_status",
        };
        let mut args = args;
        if path == "/ti/query" {
            args["format"] = json!("json");
        }
        let reply = self.dispatch(name, &args, &engine)?;
        Ok(Reply::json(200, reply))
    }
}
fn query_param_decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let pair = bytes.get(i + 1..i + 3).ok_or("Invalid percent encoding")?;
                let hex = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
                out.push(u8::from_str_radix(hex, 16).map_err(|e| e.to_string())?);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}

fn resolve_args(query: &str) -> Result<Value, String> {
    let mut args = json!({});
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or("Expected query parameter=value")?;
        let key = query_param_decode(key)?;
        let value = query_param_decode(value)?;
        let target = match key.as_str() {
            "q" => "phrase",
            "vessel" => "vessel",
            "limit" => "limit",
            _ => return Err(format!("Unknown resolve parameter: {key}")),
        };
        if args.get(target).is_some() {
            return Err(format!("Duplicate resolve parameter: {key}"));
        }
        args[target] = if target == "limit" {
            json!(value.parse::<u64>().map_err(|e| e.to_string())?)
        } else {
            json!(value)
        };
    }
    Ok(args)
}

fn query_args(query: &str) -> Result<Value, String> {
    let mut args = json!({});
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or("Expected query parameter=value")?;
        let key = query_param_decode(key)?;
        let value = query_param_decode(value)?;
        match key.as_str() {
            "sql" => args["sql"] = json!(value),
            "max_rows" => {
                let n: u64 = value
                    .parse()
                    .map_err(|e| format!("invalid max_rows: {e}"))?;
                args["max_rows"] = json!(n);
            }
            "format" => args["format"] = json!(value),
            _ => args[key] = json!(value),
        }
    }
    Ok(args)
}
struct Reply {
    status: u16,
    kind: &'static str,
    body: Vec<u8>,
    extra: String,
}
impl Reply {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            kind: "application/json",
            body: value.to_string().into_bytes(),
            extra: String::new(),
        }
    }
    fn error(status: u16, error: &str) -> Self {
        Self::json(status, json!({"error":error}))
    }
    fn write(self, stream: &mut TcpStream) -> std::io::Result<()> {
        let reason = match self.status {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            404 => "Not Found",
            405 => "Method Not Allowed",
            413 => "Payload Too Large",
            _ => "Service Unavailable",
        };
        write!(stream,"HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",self.status,reason,self.kind,self.body.len(),self.extra)?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}
pub fn handle(
    stream: &mut TcpStream,
    server: Option<&TiServer>,
    method: &str,
    path: &str,
    headers: &str,
    initial: &[u8],
) -> std::io::Result<()> {
    if method == "OPTIONS" {
        return Reply {
            status: 204,
            kind: "application/json",
            body: vec![],
            extra: String::new(),
        }
        .write(stream);
    }
    let Some(server) = server else {
        return Reply::error(503, "TI is disabled; start with --ti-store <root>").write(stream);
    };
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    let mut length = None;
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                if length.is_some() {
                    return Reply::error(400, "Duplicate Content-Length").write(stream);
                }
                length = Some(match value.trim().parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => return Reply::error(400, "Invalid Content-Length").write(stream),
                });
            }
            if name.eq_ignore_ascii_case("transfer-encoding") {
                return Reply::error(400, "Transfer-Encoding is unsupported").write(stream);
            }
        }
    }
    let length = length.unwrap_or(0);
    let max_allowed = if path.starts_with("/ti/shards/") {
        16 * 1024 * 1024
    } else {
        ti_sql::MAX_BYTES
    };
    if length > max_allowed {
        return Reply::error(413, "Request body exceeds maximum size").write(stream);
    }
    let mut body = initial[..initial.len().min(length)].to_vec();
    while body.len() < length {
        let mut chunk = [0; 8192];
        let needed = (length - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..needed]) {
            Ok(0) => return Reply::error(400, "Incomplete body").write(stream),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => return Reply::error(400, "Incomplete body").write(stream),
        }
    }
    server
        .response(method, path, &body, headers)
        .unwrap_or_else(|e| Reply::error(400, &e))
        .write(stream)
}
