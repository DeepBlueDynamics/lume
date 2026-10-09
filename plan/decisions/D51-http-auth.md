# D51 — HTTP, MCP and SSE authentication (APPROVED; A17)

The user approved requiring authentication off loopback. A17 implements nuts.services
authentication on plain/TI `lume serve` and `lume ti ingest --serve`, alongside
A11's static HTTP bearer. No design checkpoint was required.

## Policy and CLI

- `--nuts-auth [URL]`: defaults to `https://auth.nuts.services`.
- `--nuts-allow <emails or user_ids, comma list or @file>`: mandatory and nonempty
  with nuts auth. Files accept comma/newline-separated entries; maximum 64 KiB.
  Exact email/user_id matching, with numeric IDs converted to their decimal string.
- Every route requires a bearer except GET `/health` when nuts auth is enabled.
  Authentication precedes request-body parsing. Read scope covers TI queries,
  MCP transport and SSE; write scope covers OTLP and sync mutations. MCP
  `lume_index` also requires write before tool dispatch. The current MCP transport
  requires read, so indexing through MCP requires both scopes.
- A11 `--http-token-file` remains a constant-time, full-access credential. If both
  flags are supplied, either credential is accepted. Explicit sync/OTLP tokens
  override both on their routes; they need not match each other or global auth.
- Normal HTTP refuses a non-loopback bind without nuts auth or a static HTTP
  token, before binding; integrated ingest validates this before telemetry starts.
  Standalone OTLP retains its existing separately authenticated, OTLP-only listener.
- All serve paths default to `127.0.0.1`, including plain serve (A18).
  The open question was resolved by the user on 2026-10-08: loopback default.
  See [Network exposure](../../docs/OPERATIONS.md#1-network-exposure).
  Unauthenticated loopback access remains unchanged when neither flag is supplied.
  PostgreSQL SCRAM/TLS is separate.

## Verification and exchange

Use `jsonwebtoken =9.3.1` (MIT), default features disabled, on the existing
ring 0.17 backend. Direct ring SHA-256 hashes AHP cache keys. The existing url
crate validates auth URLs. Cargo.lock adds only jsonwebtoken as a new package;
PEM/ASN1, rust_crypto and aws-lc are not enabled. Auth works in the default build.

Verify RS256 signatures offline from the trusted JWKS. Restrict algorithms to
RS256; a supplied kid must match. When kid is absent, try the usable RS256 keys,
because python-jose in nuts-auth does not add kid by default. Require typed
sub, user_id, scopes, exp and iat; check expiration with 60-second leeway and
reject iat more than 60 seconds in the future. Verify signature before allowlist
and scope checks. Nuts JWTs have no iss/aud, so the allowlist is essential.

An `ahp_` bearer is form-POSTed as `token` to `/auth`. Success is
`{access_token, token_type: "Bearer", expires_in}`. The returned JWT receives the
same signature/claims/allowlist verification. Cache only the JWT and its exp,
in memory, keyed by SHA-256 of the AHP token; never persist AHP tokens or JWTs.
Cache reuse stops at exp (without leeway), with expired entries purged and a
1,024-entry cap. Concurrent exchanges serialize independently of offline JWT
verification to coalesce duplicate requests. Restart clears this cache.

## JWKS lifecycle and bounds

Fetch `/.well-known/jwks.json` at startup and every 12 hours, using a background
worker. Save public metadata at `<store>/auth/jwks.json`; plain serve uses
`.lume-index/auth/jwks.json`. Bind cached metadata to the configured auth URL.
Fetch failures use a valid same-service cache; absence/corruption refuses startup.
Refresh failures retain the last usable keys. Atomic publication uses a private
temporary file and sync; only public keys are persisted (Windows replacement
has a remove/rename gap, which fails closed on offline startup).

HTTPS is required for auth service URLs except loopback mocks. Reject URL
credentials, queries and fragments. Disable redirects so AHP tokens cannot be
forwarded to another origin. Bound network requests to 5 seconds, JWKS to 1 MiB/
32 keys, exchange JSON to 32 KiB and bearer length to the existing 8 KiB header
limit. Errors to clients are empty-body 401s. Logs contain only fixed auth
messages; never credentials, upstream error bodies or untrusted URLs.

Use an HTTPS reverse proxy for remote clients; bearer auth does not encrypt
the HTTP listener. Local plugin access remains on loopback unless configured
otherwise. This introduces no online dependency for direct JWT verification
after keys are available; AHP exchange needs the auth service after restart.

## Evidence and validation

The lead verified nonsecret facts from `nuts-auth/core/lib/jwt.py`,
`web/routes/jwt.py` and the live public JWKS: RS256, 30-minute default expiry,
email sub, user_id, read/write scopes, absent kid supported, and the above
exchange envelope. Operational key files and client_secret files were never opened.

`tests/ti_nuts_auth.rs` uses loopback fake JWKS/exchange servers and a generated,
explicitly public test RSA key. It covers signature/expiry/allowlist/scopes,
kid and no-kid JWTs, AHP caching and untrusted exchange replies, offline startup,
no-cache refusal, service-bound cache, plain and ingest serving, route token
precedence, write authorization for indexing, /health and token-free logs.
Validation on rustc 1.96.0: `cargo test --locked --features ti -- --skip concurrent_readers_never_observe_a_partial_index` exited 0 (approved environment-specific atomic-index skip); all five nuts-auth and five static-auth integration tests passed. Whole-package TI and eight-TI-crate `clippy --all-targets -- -D warnings`, crate-scoped fmt and the CI root-file rustfmt check exited 0. `cargo build --locked` without TI also exited 0. Plugin npm tests passed 63/63 on Node 24.15.0; Python bench tests passed 71/71 and Grafana smoke harness tests 7/7. Real credential exchange against nuts.services was not run; exchange behavior was verified with mocks against the lead-confirmed envelope.
