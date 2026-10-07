# D46 — pgwire TLS, certificate sources, and sslmode policy

**Status:** Approved (Option 2)  
**Base commit:** `plan/lume-ti 2df24b5`  
**Branch:** `ti/pg-tls` in `/workspace/lume/.lanes/w3`  

## 1. Context and Objective

Lume exposes a read-only PostgreSQL wire protocol endpoint (`ti_pg.rs`) powered by `pgwire = "=0.41.0"`. Previously, this listener operated exclusively in unencrypted plaintext (`None` passed to `pgwire::tokio::process_socket`).

Before Grafana or external `psql` clients can connect across boat LANs or external networks, Lume requires TLS support. This decision establishes:
1. The TLS engine integration (`pgwire`'s built-in `server-api-ring` vs. direct `tokio-rustls`).
2. Certificate provisioning (`--pg-tls-cert`/`--pg-tls-key` PEM files, private key `0600` permissions check, and automatic self-signed generation on first run via `rcgen`).
3. Network connection security policy (mandatory TLS off loopback/docker0, unless explicit `--pg-allow-plaintext` is specified).
4. Dependency licensing audit and release binary size impact.

---

## 2. Decision: Approved as Option 2 (Custom Handshake with Permissive Licences Only)

**Decision**: Approved as **Option 2** (custom `SSLRequest`/TLS loop with permissive licences only).  
**Rationale**: Lume is BSD-3 licensed and has no copyleft dependencies yet. Adding MPL-2.0 (`x509-certificate` via `pgwire`'s `server-api-ring` feature) is a project licence decision for the user. Therefore:
- **Option 2** is the active implementation: it uses `tokio-rustls 0.26` and `rustls-pemfile 2.2` directly, maintaining strictly permissive licences (`MIT / Apache-2.0 / ISC`).
- **Option 1** (`pgwire` built-in `server-api-ring`) is recorded as available if the user accepts MPL-2.0 in the future.

### Postgres Wire Protocol Negotiation
PostgreSQL wire protocol does not use raw direct TLS by default; it uses an in-band negotiation protocol:
1. Client connects via plain TCP and sends an 8-byte packet:
   - `SSLRequest` packet: length `8` (as i32), code `80877103` (`0x04D2_162F`).
   - `GSSENCRequest` packet: length `8` (as i32), code `80877104` (`0x04D2_1630`).
2. Server responds with a single ASCII byte:
   - For `SSLRequest`: `'S'` (SSL supported) or `'N'` (SSL refused/unsupported).
   - For `GSSENCRequest`: `'N'` (GSS encryption unsupported).
3. If `'S'`, client immediately begins the TLS handshake (`ClientHello`) on that established TCP connection.
4. After TLS establishes, the client sends its normal `StartupMessage`. If `'N'`, the client either sends `StartupMessage` in plaintext (if `sslmode=prefer` or `allow`) or disconnects (if `sslmode=require`).

### Option 2 Implementation Mechanics
Because `pgwire::tokio::process_socket` requires `Option<pgwire::tokio::TlsAcceptor>` (which is an uninhabited placeholder enum without `server-api-ring`), Lume implements connection handling in `src/ti_pg.rs`:
1. `handle_handshake(socket, tls_acceptor)` inspects the initial 8-byte header:
   - If `SSLRequest` and TLS is configured (`tls_acceptor.is_some()`): writes byte `b'S'`, flushes, and initiates `tls_acceptor.accept(socket).await` to produce a `tokio_rustls::server::TlsStream<TcpStream>`. Returns `(PgStream::Tls(stream), true)`.
   - If `SSLRequest` and TLS is not configured: writes byte `b'N'`, flushes, and returns `(PgStream::Plain(socket), false)`.
   - If `GSSENCRequest`: writes byte `b'N'`, flushes, and recurses to read the subsequent `SSLRequest` or `StartupMessage`.
   - If any other message (e.g. standard plaintext `StartupMessage`): prepends the 8 peeked bytes back into the stream using `tokio_util::codec::Framed` and returns `(PgStream::Plain(prefixed_socket), false)`.
2. A custom enum `PgStream` implements both `AsyncRead` and `AsyncWrite`, delegating to either `Plain(PrefixStream<TcpStream>)` or `Tls(TlsStream<TcpStream>)`.
3. The framed stream is processed using `pgwire::tokio::server::process_message(stream, &client_info, &authenticator, &processor)`.
4. Client secure state (`client_info.is_secure()`) is marked `true` when TLS is active.

---

## 3. Certificate Sources & Key Permissions

### CLI & Configuration Interface
- **CLI Flags**:
  - `--pg-tls-cert <PATH>`: Path to PEM-encoded certificate or certificate chain.
  - `--pg-tls-key <PATH>`: Path to PEM-encoded private key (PKCS#8, PKCS#1 RSA, or SEC1 EC).
  - `--pg-allow-plaintext`: Explicitly permit unencrypted connections on non-loopback binds.
- **`ti.toml [bind]` Section**:
  ```toml
  [bind]
  address = "0.0.0.0"
  pg_port = 5433
  pg_tls_cert = "/var/lib/lume/pg_cert.pem"
  pg_tls_key = "/var/lib/lume/pg_key.pem"
  pg_allow_plaintext = false
  ```
  CLI flags take precedence over `ti.toml`. Specifying only `--pg-tls-cert` without `--pg-tls-key` (or vice versa) returns a CLI validation error.

### Private Key `0600` Permissions Check
To prevent compromised private keys, Lume enforces mode `0600` on Unix systems, reusing the exact safe descriptor-check pattern from [src/ti_pg_auth.rs](file:///workspace/lume/.lanes/w3/src/ti_pg_auth.rs):
```rust
pub(crate) fn open_private_file(path: &Path, label: &str) -> Result<File, String> {
    let file = File::open(path).map_err(|e| format!("Cannot open {label}: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file
            .metadata()
            .map_err(|e| format!("Cannot stat {label}: {e}"))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!("{label} must not be group- or world-accessible (use chmod 600)"));
        }
    }
    Ok(file)
}
```
Opening the descriptor before checking metadata eliminates TOCTOU symlink/path replacement races. On Windows, permissions checking is a no-op.

### Automatic Self-Signed Generation on First Run
If neither `--pg-tls-cert`/`--pg-tls-key` nor config values are provided:
1. Lume checks for existing auto-generated certs at `<store_root>/pg_cert.pem` and `<store_root>/pg_key.pem`.
2. If absent, Lume generates a fresh self-signed X.509 certificate using `rcgen` (`rcgen = { version = "0.14", default-features = false, features = ["pem", "ring"] }`).
3. **Key creation security**: On Unix, `<store_root>/pg_key.pem` is created with mode `0600` using `std::os::unix::fs::OpenOptionsExt::mode(0o600)`.
4. **Certificate parameters**:
   - Subject Common Name: `Lume Postgres Server`
   - Validity: 10 years (3,650 days), matching typical marine / edge appliance service intervals.
   - Subject Alternative Names (SANs):
     - `localhost`
     - `127.0.0.1`
     - `::1`
     - `halos.local`
     - `172.17.0.1` (Docker host bridge default)
5. Existing generated certificates are reused across daemon restarts to avoid certificate churn and invalidating client trust stores.

### Manual OpenSSL Generation (Alternative)
For operators preferring external certificate provisioning:
```bash
openssl req -x509 -newkey rsa:4096 -keyout pg_key.pem -out pg_cert.pem -days 3650 -nodes \
  -subj "/CN=Lume Postgres Server" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1,DNS:halos.local,IP:172.17.0.1"
chmod 600 pg_key.pem
```

---

## 4. sslmode Behaviour and Bind Policy

### Network Classification
- **Loopback**: `127.0.0.0/8` (IPv4) or `::1` (IPv6), checked via `ip.is_loopback()`.
- **docker0 / Container Bridge**: `172.17.0.0/16` or host gateway `172.17.0.1`.
- **Non-loopback / External**: `0.0.0.0`, `::`, or specific LAN/WAN interface IPs.

### Connection Policy Matrix
| Bind Address | Client TLS (`sslmode`) | `--pg-allow-plaintext` | Result |
|---|---|---|---|
| Loopback (`127.0.0.1`, `127.0.0.2`, `::1`) | TLS (`require`) | Any | **Allowed** |
| Loopback (`127.0.0.1`, `127.0.0.2`, `::1`) | Plaintext (`disable`) | Any | **Allowed** (default local IPC) |
| docker0 (`172.17.0.0/16`) | TLS (`require`) | Any | **Allowed** |
| docker0 (`172.17.0.0/16`) | Plaintext (`disable`) | Any | **Allowed** (container bridge) |
| Non-loopback (`0.0.0.0`, LAN) | TLS (`require`) | Any | **Allowed** |
| Non-loopback (`0.0.0.0`, LAN) | Plaintext (`disable`) | `false` (default) | **REJECTED** (SQLSTATE `28000`) |
| Non-loopback (`0.0.0.0`, LAN) | Plaintext (`disable`) | `true` | **Allowed** (with warning log) |

### Rejection Implementation
When `--pg-bind` is not loopback or docker0 and `--pg-allow-plaintext` is false, `AuthConfig::new` sets `require_tls = true`. In `StartupHandler::on_startup`:
```rust
if self.config.require_tls && !client.is_secure() {
    return Err(super::ti_pg::error(
        "28000",
        "TLS connection is required for non-loopback connections. Connect using sslmode=require, or start the server with --pg-allow-plaintext",
    ));
}
```
Standard PostgreSQL SQLSTATE `28000` (`invalid_authorization_specification`) cleanly instructs `psql` or Grafana to connect using `sslmode=require`.

### SCRAM-SHA-256 over TLS
SCRAM-SHA-256 password authentication functions seamlessly over TLS. PostgreSQL channel binding (`SCRAM-SHA-256-PLUS` / `tls-server-end-point`) is not required and is omitted, avoiding the need for `x509-certificate` or ASN.1 cert parsing at runtime during authentication.

---

## 5. Dependency Licensing Audit & cargo deny Evidence

Lume is distributed under BSD-3-Clause / MIT / Apache-2.0. All new dependencies and their transitive sub-dependencies have been verified 100% permissive.

### Cargo Deny Licence Evidence
```
# Direct runtime dependencies
tokio-rustls 0.26.2       MIT OR Apache-2.0        (permissive)
rustls-pemfile 2.2.0      Apache-2.0 OR ISC OR MIT (permissive)
rcgen 0.14.10             MIT OR Apache-2.0        (permissive)
tokio-util 0.7.18         MIT                      (permissive)

# Transitive runtime dependencies pulled by rcgen
pem 3.0.5                 MIT OR Apache-2.0        (permissive)
yasna 0.5.2               MIT OR Apache-2.0        (permissive)
x509-parser 0.17.0        MIT OR Apache-2.0        (permissive)
asn1-rs 0.7.1             MIT OR Apache-2.0        (permissive)
oid-registry 0.8.1        MIT OR Apache-2.0        (permissive)
data-encoding 2.9.0       MIT                      (permissive)
time 0.3.47               MIT OR Apache-2.0        (permissive)
rustls-pki-types 1.14.1   MIT OR Apache-2.0        (permissive)
ring 0.17.14              MIT / Apache-2.0 / ISC   (permissive)

# Dev-only dependency (for integration tests)
tokio-postgres-rustls 0.13.0  MIT OR Apache-2.0    (permissive)
```

**Copyleft Count**: **0** (No GPL, LGPL, AGPL, or MPL-2.0 licenses in Option 2).

---

## 6. Binary-Size Delta Measurement

Empirical release builds on Linux `x86_64` (`cargo build --release --features ti`):

| Artifact | File Size (Bytes) | Size (MB) | Delta vs. Baseline | Delta (%) |
|---|---|---|---|---|
| **Baseline unstripped** | `109,844,808` | 104.76 MB | — | — |
| **Baseline stripped** | `92,754,384` | 88.46 MB | — | — |
| **Option 2 unstripped** | `109,960,112` | 104.87 MB | +115,304 B | +0.10% |
| **Option 2 stripped** | `92,868,432` | 88.57 MB | **+114,048 B** | **+0.12%** |

The binary size increase is only ~114 KiB (+0.12%) because `ring`, `rustls`, and Tokio are already compiled into Lume.

---

## 7. Verification & Test Suite

The implementation is verified by integration tests in `tests/pg_tls.rs` (7/7 passing):
1. `test_tls_sslmode_require_succeeds`: Validates connecting with `tokio-postgres` and `tokio-postgres-rustls` under `sslmode=require` against auto-generated self-signed certificate.
2. `test_non_loopback_refuses_plaintext`: Confirms unencrypted connections (`sslmode=disable`) to `0.0.0.0` are rejected with SQLSTATE `28000`.
3. `test_plaintext_loopback_unchanged`: Confirms local connections to `127.0.0.1` continue working unencrypted with zero config.
4. `test_loopback_127_0_0_2`: Confirms alias loopback addresses work without TLS.
5. `test_non_loopback_allows_plaintext_when_configured`: Confirms `--pg-allow-plaintext` permits unencrypted connections on `0.0.0.0`.
6. `test_wrong_key_mode_rejected`: Confirms private keys with permissive permissions (e.g. `0644`) fail with descriptor permission error on Unix.
7. `test_scram_over_tls`: Confirms full SCRAM-SHA-256 authentication handshake completes successfully over TLS.
