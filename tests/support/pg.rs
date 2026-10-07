use super::*;
#[test]
fn postgres_simple_query_caps_read_only_and_shared_snapshot() {
    // 127.0.0.2: an explicit non-default bind that stays on loopback (no firewall prompt).
    for bind in [None, Some("127.0.0.2")] {
        let server = Server::start_with(bind, true);
        let runtime = ti_sql::surface_runtime().unwrap();
        runtime.block_on(async {
            let (client, connection) = tokio_postgres::connect(
                &format!(
                    "host={} port={} user=lume dbname=ti sslmode=disable",
                    server.pg.split(':').next().unwrap(),
                    server.pg.split(':').nth(1).unwrap()
                ),
                tokio_postgres::NoTls,
            )
            .await
            .unwrap();
            let task = tokio::spawn(connection);
            let rows = client
                .simple_query("SELECT count(*) FROM telemetry")
                .await
                .unwrap();
            let row = rows
                .iter()
                .find_map(|m| match m {
                    tokio_postgres::SimpleQueryMessage::Row(row) => Some(row),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{rows:?}"));
            assert_eq!(row.get(0), Some("2"));
            let catalog = server.root.join("store/catalog");
            let hidden = server.root.join("hidden-catalog");
            std::fs::rename(&catalog, &hidden).unwrap();
            let rows = client
                .simple_query("SELECT \"navigation.speedOverGround\" FROM telemetry ORDER BY ts")
                .await
                .unwrap();
            let values: Vec<_> = rows
                .iter()
                .filter_map(|m| match m {
                    tokio_postgres::SimpleQueryMessage::Row(row) => row.get(0),
                    _ => None,
                })
                .collect();
            assert_eq!(values, vec!["2.0", "4.0"]);
            let rows = client
                .simple_query("SELECT NULL AS absent, 'é' AS utf8, true AS flag")
                .await
                .unwrap();
            let row = rows
                .iter()
                .find_map(|m| match m {
                    tokio_postgres::SimpleQueryMessage::Row(row) => Some(row),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{rows:?}"));
            assert_eq!(row.get(0), None);
            assert_eq!(row.get(1), Some("é"));
            assert_eq!(row.get(2), Some("t"));
            for sql in [
                "DELETE FROM telemetry",
                "CREATE TABLE x (v INT)",
                "INSERT INTO telemetry VALUES (1)",
                "SELECT 1; DELETE FROM telemetry",
            ] {
                assert!(client.simple_query(sql).await.is_err(), "{sql}");
            }
            assert!(client
                .simple_query("SELECT * FROM generate_series(1, 501)")
                .await
                .is_ok());
            for sql in [
                "SELECT * FROM generate_series(1, 100001)",
                "SELECT repeat('é', 9000000) AS huge",
            ] {
                let error = client.simple_query(sql).await.unwrap_err();
                assert_eq!(error.code().unwrap().code(), "54000");
                assert!(error.as_db_error().unwrap().message().contains("aggregate"));
            }
            // A failed query must leave the connection usable.
            let rows = client
                .simple_query("SELECT count(*) AS n FROM telemetry")
                .await
                .unwrap();
            let row = rows
                .iter()
                .find_map(|m| match m {
                    tokio_postgres::SimpleQueryMessage::Row(row) => Some(row),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{rows:?}"));
            assert_eq!(row.get(0), Some("2"));
            assert_eq!(server.get("/ti/status")["width_seconds"], 10);
            std::fs::rename(hidden, catalog).unwrap();
            drop(client);
            task.await.unwrap().unwrap();
        });
    }
}
#[test]
fn pg_flag_requires_store_and_a_valid_port() {
    for args in [
        vec!["serve", "--pg", "0"],
        vec!["serve", "--ti-store", "missing", "--pg"],
        vec!["serve", "--ti-store", "missing", "--pg", "65536"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_lume"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("--pg"));
    }
}
