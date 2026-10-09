use super::*;
const VERIFIER: &str = "SCRAM-SHA-256$4096:W22ZaJ0SNY7soEsUEjb6gQ==$WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=";
fn connection_config(server: &Server) -> tokio_postgres::Config {
    let mut config = tokio_postgres::Config::new();
    config
        .host(server.pg.rsplit_once(':').unwrap().0)
        .port(server.pg.rsplit(':').next().unwrap().parse().unwrap())
        .user("lume")
        .dbname("ti");
    config
}
#[test]
fn extended_typed_rows_parameters_grafana_and_recovery() {
    let server = Server::start_with(None, true);
    ti_sql::surface_runtime().unwrap().block_on(async {
        let (client, connection) = connection_config(&server).connect(tokio_postgres::NoTls).await.unwrap();
        let task = tokio::spawn(connection);
        let rows = client.query("SELECT ts, \"navigation.speedOverGround\" AS speed, CAST(42 AS BIGINT) AS n, 'é' AS label, true AS flag FROM telemetry ORDER BY ts", &[]).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<_, chrono::DateTime<chrono::Utc>>(0).timestamp(), 1577836810);
        assert_eq!(rows[0].get::<_, f64>(1), 2.0);
        assert_eq!(rows[0].get::<_, i64>(2), 42);
        assert_eq!(rows[0].get::<_, String>(3), "é");
        assert!(rows[0].get::<_, bool>(4));
        let statement = client.prepare("SELECT $1::BIGINT AS number, $2::TEXT AS label").await.unwrap();
        assert_eq!(statement.params(), &[tokio_postgres::types::Type::INT8, tokio_postgres::types::Type::TEXT]);
        let row = client.query_one(&statement, &[&42_i64, &"O'Brien; DELETE FROM telemetry"]).await.unwrap();
        assert_eq!(row.get::<_, i64>(0), 42);
        assert_eq!(row.get::<_, String>(1), "O'Brien; DELETE FROM telemetry");
        let row = client.query_one("SELECT $1::INT AS n", &[&7_i32]).await.unwrap();
        assert_eq!(row.get::<_, i64>(0), 7);
        let at = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:10Z").unwrap().with_timezone(&chrono::Utc);
        let row = client.query_one("SELECT $1::TIMESTAMP WITH TIME ZONE AS at", &[&at]).await.unwrap();
        assert_eq!(row.get::<_, chrono::DateTime<chrono::Utc>>(0), at);
        let row = client.query_one("SELECT CAST(NULL AS BIGINT) AS absent", &[]).await.unwrap();
        assert_eq!(row.get::<_, Option<i64>>(0), None);
        let fixture: Value = serde_json::from_str(include_str!("../golden/grafana-pg.json")).unwrap();
        for query in fixture["queries"].as_array().unwrap() {
            let sql = query["sql"].as_str().unwrap();
            let messages = client.simple_query(sql).await.unwrap_or_else(|e| panic!("{}: {e}", query["id"]));
            assert_eq!(messages.iter().filter(|m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(_))).count(), query["rows"].as_u64().unwrap() as usize, "{}", query["id"]);
        }
        assert!(client.prepare("DELETE FROM telemetry").await.is_err());
        assert_eq!(client.query_one("SELECT count(*) AS n FROM telemetry", &[]).await.unwrap().get::<_, i64>(0), 2);
        drop(client);
        task.await.unwrap().unwrap();
    });
}
#[test]
fn scram_accept_reject_unknown_user_on_explicit_loopback() {
    for bind in [None, Some("127.0.0.2")] {
        let server = Server::start_with_auth(bind, true, Some(VERIFIER));
        ti_sql::surface_runtime().unwrap().block_on(async {
            for (user, password) in [("lume", "wrong"), ("missing", "pencil")] {
                let mut config = connection_config(&server);
                config.user(user).password(password);
                match config.connect(tokio_postgres::NoTls).await {
                    Err(e) => assert_eq!(e.code().unwrap().code(), "28P01"),
                    Ok(_) => panic!("accepted invalid credentials"),
                }
            }
            let mut config = connection_config(&server);
            config.password("pencil");
            let (client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
            let task = tokio::spawn(connection);
            assert_eq!(
                client
                    .query_one("SELECT 42::BIGINT AS n", &[])
                    .await
                    .unwrap()
                    .get::<_, i64>(0),
                42
            );
            drop(client);
            task.await.unwrap().unwrap();
        });
    }
}

#[test]
fn node_verifier_external_auth_and_independent_pg_bind() {
    let output = Command::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/signalk-lume-ti"))
        .args(["-e", "require('./lib/pg').deriveVerifier('pencil',Buffer.from('W22ZaJ0SNY7soEsUEjb6gQ==','base64')).then(v=>process.stdout.write(v))"])
        .output().expect("Node is required for verifier interoperability");
    assert!(output.status.success(), "Node verifier derivation failed");
    let verifier = String::from_utf8(output.stdout).unwrap();
    assert_eq!(verifier, VERIFIER);
    let server =
        Server::start_config_pg(None, true, Some(&verifier), false, Some("127.0.0.2"), true);
    assert!(server.url.starts_with("http://127.0.0.1:"));
    assert!(server.pg.starts_with("127.0.0.2:"));
    assert!(server.get("/ti/status").is_object());
    let store_config = std::fs::read_to_string(server.root.join("store/ti.toml")).unwrap();
    let config = ti_contracts::TiConfig::from_toml(&store_config).unwrap();
    assert_eq!(config.width_seconds, 10);
    assert_eq!(config.signal_k.url, "ws://127.0.0.1:29999");
    assert_eq!(config.auth.scram_users[0].username, "store-user");
    ti_sql::surface_runtime().unwrap().block_on(async {
        for user in ["store-user", "missing"] {
            let mut config = connection_config(&server);
            config.user(user).password("pencil");
            assert!(
                config.connect(tokio_postgres::NoTls).await.is_err(),
                "store auth merged"
            );
        }
        let mut config = connection_config(&server);
        config.password("pencil");
        let (client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        let task = tokio::spawn(connection);
        let rows = client
            .query(
                "SELECT ts, \"navigation.speedOverGround\" AS speed FROM telemetry ORDER BY ts",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(
            rows[0]
                .get::<_, chrono::DateTime<chrono::Utc>>(0)
                .timestamp(),
            1577836810
        );
        assert_eq!(rows[0].get::<_, f64>(1), 2.0);
        let smoke: Value =
            serde_json::from_str(include_str!("../golden/grafana-smoke.json")).unwrap();
        assert_eq!(smoke["queries"].as_array().unwrap().len(), 20);
        for query in smoke["queries"].as_array().unwrap() {
            client
                .simple_query(query["sql"].as_str().unwrap())
                .await
                .unwrap_or_else(|e| panic!("{}: {e}", query["id"]));
        }
        drop(client);
        task.await.unwrap().unwrap();
    });
    assert_eq!(
        std::fs::read_to_string(server.root.join("store/ti.toml")).unwrap(),
        store_config
    );
}
#[cfg(unix)]
#[test]
fn external_auth_permissions_fail_closed() {
    use std::os::unix::fs::PermissionsExt;
    let server = Server::start();
    let auth_path = server.root.join("pg-auth.toml");
    std::fs::write(
        &auth_path,
        format!("[[auth.scram_users]]\nusername='lume'\nverifier='{VERIFIER}'\n"),
    )
    .unwrap();
    for mode in [0o644, 0o640, 0o604] {
        std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(mode)).unwrap();
        for args in [
            vec!["serve", "--port", "0", "--pg", "0", "--ti-store"],
            vec![
                "ti",
                "ingest",
                "--serve",
                "--port",
                "0",
                "--pg",
                "0",
                "--signalk",
                "ws://127.0.0.1:1",
                "--store",
            ],
        ] {
            let output = Command::new(env!("CARGO_BIN_EXE_lume"))
                .args(args)
                .arg(server.root.join("store"))
                .arg("--pg-auth-config")
                .arg(&auth_path)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("group- or world-accessible"));
        }
    }
}
