#![cfg(feature = "ti")]

use lume::ti_http::TiServer;
use lume::ti_pg::{self, PgOptions};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use ti_contracts::{Catalog, ShardSink};
use tokio_postgres_rustls::MakeRustlsConnect;

const VERIFIER: &str = "SCRAM-SHA-256$4096:W22ZaJ0SNY7soEsUEjb6gQ==$WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=";

struct TestDir {
    root: PathBuf,
    store_root: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "pg-tls-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let store_root = root.join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let mut store = ti_store::Store::open_or_create(&store_root, 10).unwrap();
        let vessel = store
            .catalog()
            .register_vessel(&ti_contracts::VesselSpec {
                urn: "vessels.urn:test:pgtls".into(),
                name: None,
                mmsi: None,
            })
            .unwrap();
        store
            .seal(ti_contracts::ShardKey { vessel, shard: 0 })
            .unwrap();
        store.shutdown().unwrap();
        std::fs::write(store_root.join("ti.toml"), "width_seconds = 10\n").unwrap();
        Self { root, store_root }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn authenticated_idle_connection_expires_and_admission_recovers() {
    use std::time::Duration;
    let test_dir = TestDir::new();
    std::fs::write(test_dir.store_root.join("ti.toml"),
        format!("width_seconds = 10\n[[auth.scram_users]]\nusername = 'lume'\nverifier = '{VERIFIER}'\n")).unwrap();
    let server = Arc::new(TiServer::open(&test_dir.store_root).unwrap());
    let options = PgOptions {
        idle_timeout: Duration::from_millis(100),
        ..Default::default()
    };
    let listener =
        ti_pg::start_with_options(server, "127.0.0.1:0".parse().unwrap(), &options).unwrap();
    ti_sql::surface_runtime().unwrap().block_on(async {
        let config = {
            let mut c = tokio_postgres::Config::new();
            c.host("127.0.0.1")
                .port(listener.address.port())
                .user("lume")
                .password("pencil")
                .dbname("ti");
            c
        };
        for _ in 0..33 {
            let (client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
            let connection = tokio::spawn(connection);
            assert_eq!(client.query("SELECT 1", &[]).await.unwrap().len(), 1);
            let _closed = tokio::time::timeout(Duration::from_secs(2), connection)
                .await
                .unwrap()
                .unwrap();
            assert!(client.is_closed());
        }
        let (client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        let connection = tokio::spawn(connection);
        assert_eq!(client.query("SELECT 1", &[]).await.unwrap().len(), 1);
        drop(client);
        let _ = connection.await;
    });
}

fn make_tls_connector(cert_path: &std::path::Path) -> MakeRustlsConnect {
    let mut file = std::io::BufReader::new(std::fs::File::open(cert_path).expect("open cert"));
    let certs = rustls_pemfile::certs(&mut file)
        .collect::<Result<Vec<_>, _>>()
        .expect("read certs");
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    for cert in certs {
        roots.add(cert).expect("add cert to store");
    }
    let config = tokio_rustls::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    MakeRustlsConnect::new(config)
}

#[test]
fn test_tls_sslmode_require_succeeds() {
    let test_dir = TestDir::new();
    let server = Arc::new(TiServer::open(&test_dir.store_root).unwrap());
    let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = ti_pg::start(server, bind_addr).unwrap();
    let port = listener.address.port();
    let cert_path = test_dir.store_root.join("pg_cert.pem");
    assert!(cert_path.exists(), "pg_cert.pem should be auto-generated");

    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        let tls = make_tls_connector(&cert_path);
        let (client, connection) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(port)
            .user("lume")
            .dbname("ti")
            .ssl_mode(tokio_postgres::config::SslMode::Require)
            .connect(tls)
            .await
            .unwrap();
        let conn_task = tokio::spawn(connection);

        let rows = client.query("SELECT 1", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);
        drop(client);
        let _ = conn_task.await;
    });
}

#[test]
fn test_plaintext_loopback_unchanged() {
    let test_dir = TestDir::new();
    let server = Arc::new(TiServer::open(&test_dir.store_root).unwrap());
    let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = ti_pg::start(server, bind_addr).unwrap();
    let port = listener.address.port();

    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        let (client, connection) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(port)
            .user("lume")
            .dbname("ti")
            .connect(tokio_postgres::NoTls)
            .await
            .unwrap();
        let conn_task = tokio::spawn(connection);

        let rows = client.query("SELECT 1", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);
        drop(client);
        let _ = conn_task.await;
    });
}

#[test]
fn test_loopback_127_0_0_2() {
    let test_dir = TestDir::new();
    let server = Arc::new(TiServer::open(&test_dir.store_root).unwrap());
    let bind_addr: SocketAddr = "127.0.0.2:0".parse().unwrap();
    let listener = ti_pg::start(server, bind_addr).unwrap();
    let port = listener.address.port();

    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        let (client, connection) = tokio_postgres::Config::new()
            .host("127.0.0.2")
            .port(port)
            .user("lume")
            .dbname("ti")
            .connect(tokio_postgres::NoTls)
            .await
            .unwrap();
        let conn_task = tokio::spawn(connection);

        let rows = client.query("SELECT 1", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);
        drop(client);
        let _ = conn_task.await;
    });
}

#[test]
fn test_require_tls_policy() {
    use std::net::IpAddr;
    let default_options = PgOptions::default();
    let allow_plaintext_options = PgOptions {
        allow_plaintext: true,
        ..Default::default()
    };
    let require_tls_true_options = PgOptions {
        require_tls: Some(true),
        ..Default::default()
    };
    let require_tls_false_options = PgOptions {
        require_tls: Some(false),
        ..Default::default()
    };

    let non_loopback_cases: &[&str] = &["0.0.0.0", "192.0.2.1", "::", "172.17.5.5", "172.18.0.1"];
    let loopback_or_docker0_cases: &[&str] = &["127.0.0.1", "127.0.0.2", "::1", "172.17.0.1"];

    for &addr_str in non_loopback_cases {
        let ip: IpAddr = addr_str.parse().unwrap();
        assert!(
            ti_pg::require_tls(ip, &default_options),
            "Address {addr_str} should require TLS by default"
        );
        assert!(
            !ti_pg::require_tls(ip, &allow_plaintext_options),
            "Address {addr_str} should NOT require TLS when allow_plaintext is true"
        );
        assert!(
            !ti_pg::require_tls(ip, &require_tls_false_options),
            "Address {addr_str} should NOT require TLS when require_tls is Some(false)"
        );
        assert!(
            ti_pg::require_tls(ip, &require_tls_true_options),
            "Address {addr_str} should require TLS when require_tls is Some(true)"
        );
    }

    for &addr_str in loopback_or_docker0_cases {
        let ip: IpAddr = addr_str.parse().unwrap();
        assert!(
            !ti_pg::require_tls(ip, &default_options),
            "Address {addr_str} should NOT require TLS by default"
        );
        assert!(
            !ti_pg::require_tls(ip, &allow_plaintext_options),
            "Address {addr_str} should NOT require TLS when allow_plaintext is true"
        );
        assert!(
            !ti_pg::require_tls(ip, &require_tls_false_options),
            "Address {addr_str} should NOT require TLS when require_tls is Some(false)"
        );
        assert!(
            ti_pg::require_tls(ip, &require_tls_true_options),
            "Address {addr_str} should require TLS when require_tls is Some(true)"
        );
    }
}

#[test]
fn test_non_loopback_refuses_plaintext() {
    let test_dir = TestDir::new();
    std::fs::write(
        test_dir.store_root.join("ti.toml"),
        format!("width_seconds = 10\n[[auth.scram_users]]\nusername = 'lume'\nverifier = '{VERIFIER}'\n"),
    )
    .unwrap();
    let server = Arc::new(TiServer::open(&test_dir.store_root).unwrap());
    let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let options = PgOptions {
        require_tls: Some(true),
        ..Default::default()
    };
    let listener = ti_pg::start_with_options(server, bind_addr, &options).unwrap();
    let port = listener.address.port();
    let cert_path = test_dir.store_root.join("pg_cert.pem");
    assert!(cert_path.exists());

    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        // Plaintext connection to non-loopback bind must be refused with SQLSTATE 28000
        let res = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(port)
            .user("lume")
            .password("pencil")
            .dbname("ti")
            .connect(tokio_postgres::NoTls)
            .await;
        assert!(
            res.is_err(),
            "Expected plaintext connection to non-loopback bind to be refused"
        );
        let err = res.err().unwrap();
        let db_err = err.as_db_error().expect("expected Postgres DbError");
        assert_eq!(db_err.code().code(), "28000");
        assert!(db_err.message().contains("TLS"));

        // TLS connection to the same server must succeed
        let tls = make_tls_connector(&cert_path);
        let (client, connection) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(port)
            .user("lume")
            .password("pencil")
            .dbname("ti")
            .ssl_mode(tokio_postgres::config::SslMode::Require)
            .connect(tls)
            .await
            .unwrap();
        let conn_task = tokio::spawn(connection);

        let rows = client.query("SELECT 1", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);
        drop(client);
        let _ = conn_task.await;
    });
}

#[test]
fn test_non_loopback_allows_plaintext_when_configured() {
    let test_dir = TestDir::new();
    std::fs::write(
        test_dir.store_root.join("ti.toml"),
        format!("width_seconds = 10\n[[auth.scram_users]]\nusername = 'lume'\nverifier = '{VERIFIER}'\n"),
    )
    .unwrap();
    let server = Arc::new(TiServer::open(&test_dir.store_root).unwrap());
    let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let options = PgOptions {
        tls_cert: None,
        tls_key: None,
        allow_plaintext: true,
        require_tls: Some(true),
        ..Default::default()
    };
    let listener = ti_pg::start_with_options(server, bind_addr, &options).unwrap();
    let port = listener.address.port();

    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        let (client, connection) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(port)
            .user("lume")
            .password("pencil")
            .dbname("ti")
            .connect(tokio_postgres::NoTls)
            .await
            .unwrap();
        let conn_task = tokio::spawn(connection);

        let rows = client.query("SELECT 1", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);
        drop(client);
        let _ = conn_task.await;
    });
}

#[cfg(unix)]
#[test]
fn test_wrong_key_mode_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let test_dir = TestDir::new();
    let server = Arc::new(TiServer::open(&test_dir.store_root).unwrap());

    let (cert_path, key_path) = (
        test_dir.store_root.join("custom_cert.pem"),
        test_dir.store_root.join("custom_key.pem"),
    );
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, signing_key.serialize_pem()).unwrap();

    // Set 0644 mode (world-readable)
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let options = PgOptions {
        tls_cert: Some(cert_path),
        tls_key: Some(key_path),
        allow_plaintext: false,
        require_tls: None,
        ..Default::default()
    };

    let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let result = ti_pg::start_with_options(server, bind_addr, &options);
    assert!(
        result.is_err(),
        "Expected starting with mode 0644 key to fail"
    );
    let err = result.err().unwrap();
    assert!(
        err.contains("pg TLS private key must not be group- or world-accessible (use chmod 600)"),
        "Unexpected error: {err}"
    );
}

#[test]
fn test_scram_over_tls() {
    let test_dir = TestDir::new();
    std::fs::write(
        test_dir.store_root.join("ti.toml"),
        format!("width_seconds = 10\n[[auth.scram_users]]\nusername = 'lume'\nverifier = '{VERIFIER}'\n"),
    )
    .unwrap();
    let server = Arc::new(TiServer::open(&test_dir.store_root).unwrap());
    let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = ti_pg::start(server, bind_addr).unwrap();
    let port = listener.address.port();
    let cert_path = test_dir.store_root.join("pg_cert.pem");

    let runtime = ti_sql::surface_runtime().unwrap();
    runtime.block_on(async {
        // SCRAM success over TLS
        let tls = make_tls_connector(&cert_path);
        let (client, connection) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(port)
            .user("lume")
            .password("pencil")
            .dbname("ti")
            .ssl_mode(tokio_postgres::config::SslMode::Require)
            .connect(tls)
            .await
            .unwrap();
        let conn_task = tokio::spawn(connection);

        let rows = client.query("SELECT 1", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);
        drop(client);
        let _ = conn_task.await;

        // SCRAM failure over TLS with bad password
        let tls_bad = make_tls_connector(&cert_path);
        let bad_res = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(port)
            .user("lume")
            .password("wrong_password")
            .dbname("ti")
            .ssl_mode(tokio_postgres::config::SslMode::Require)
            .connect(tls_bad)
            .await;
        assert!(bad_res.is_err(), "Expected wrong password to fail");
    });
}
