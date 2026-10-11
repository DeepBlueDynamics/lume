use super::*;

pub(super) fn rebuild_manual(root: &std::path::Path) {
    rebuild_manual_format(root, "json");
}
pub(super) fn rebuild_manual_format(root: &std::path::Path, format: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_lume"))
        .arg("index")
        .arg(root.join("manual"))
        .arg("--db")
        .arg(root.join("index"))
        .arg("--force")
        .env("LUME_INDEX_FORMAT", format)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root
        .join(if format == "4" {
            "index/index.json"
        } else {
            "index/manifest.json"
        })
        .is_file());
}

#[test]
fn shared_docs_index_reloads_http_and_pg_without_restarting() {
    let server = Server::start_with_docs(None, true, None, false, None, false, true);
    let query = |sql: &str| -> Value {
        server
            .post("/ti/query", json!({"sql":sql}), true)
            .unwrap()
            .into_json()
            .unwrap()
    };
    let sql = "SELECT s.title, s.body, count(*) AS n FROM sections s CROSS JOIN telemetry t WHERE match(s.body,'bilge pump') GROUP BY s.title,s.body";
    let initial = query(sql);
    assert_eq!(initial["row_count"], 1);
    assert_eq!(initial["rows"][0]["n"], 2);
    assert!(initial["rows"][0]["body"]
        .as_str()
        .unwrap()
        .contains("original"));
    assert_eq!(query("SELECT count(*) AS n FROM entities")["row_count"], 1);

    let alerts = query("SELECT s.title,d.id FROM docs d CROSS JOIN sections s WHERE d.kind='alerts' AND match(s.body,'bilge pump')");
    assert_eq!(alerts["rows"][0]["id"], "alert:bilge");
    assert_eq!(alerts["row_count"], 1);
    let runtime = ti_sql::surface_runtime().unwrap();
    let (client, connection) = runtime
        .block_on(tokio_postgres::connect(
            &format!(
                "host=127.0.0.1 port={} user=lume dbname=lume",
                server.pg.rsplit(':').next().unwrap()
            ),
            tokio_postgres::NoTls,
        ))
        .unwrap();
    runtime.spawn(async move {
        connection.await.unwrap();
    });
    assert_eq!(
        runtime
            .block_on(client.query_one("SELECT count(*) AS n FROM sections", &[]))
            .unwrap()
            .get::<_, i64>(0),
        1
    );

    std::fs::write(server.root.join("manual/manual.md"), "## Rebuilt manual\n\nBilge pump replacement procedure. Disconnect power, fit the replacement pump, check the discharge hose and test the float switch before departure.\n").unwrap();
    rebuild_manual(&server.root);
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let reply = query(sql);
        if reply["rows"][0]["body"]
            .as_str()
            .unwrap()
            .contains("replacement")
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{reply}");
        std::thread::sleep(Duration::from_millis(200));
    }
    let body: String = runtime
        .block_on(client.query_one(
            "SELECT body FROM sections WHERE match(body,'bilge pump') LIMIT 1",
            &[],
        ))
        .unwrap()
        .get(0);
    assert!(body.contains("replacement"));
    assert!(!body.contains("original"));

    // "invalid JSON" is the payload serde reports as "expected value at line 1
    // column 1". It must not replace the working snapshot. Put the previous
    // files back before the reload debounce so this check does not log a
    // failed reload; torn writes are covered by the concurrent save_json test.
    let bm25_path = server.root.join("index/bm25.json");
    let manifest_path = server.root.join("index/manifest.json");
    let good_bm25 = std::fs::read(&bm25_path).unwrap();
    let good_manifest = std::fs::read(&manifest_path).unwrap();
    std::fs::write(&bm25_path, "invalid JSON").unwrap();
    std::fs::write(&manifest_path, "failed-generation").unwrap();
    assert!(query(sql)["rows"][0]["body"]
        .as_str()
        .unwrap()
        .contains("replacement"));
    std::fs::write(&bm25_path, good_bm25).unwrap();
    std::fs::write(&manifest_path, good_manifest).unwrap();
    rebuild_manual(&server.root);
    query(sql);
    std::thread::sleep(Duration::from_millis(2100));
    assert_eq!(query(sql)["row_count"], 1);
    std::fs::remove_file(server.root.join("manual/manual.md")).unwrap();
    rebuild_manual(&server.root);
    query("SELECT count(*) AS n FROM sections");
    std::thread::sleep(Duration::from_millis(2100));
    assert_eq!(
        query("SELECT count(*) AS n FROM sections")["rows"][0]["n"],
        0
    );
    assert_eq!(
        runtime
            .block_on(client.query_one("SELECT count(*) AS n FROM sections", &[]))
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[test]
fn explicit_v4_docs_index_cli_http_pg_and_overlay_reload() {
    let server = Server::start_with_docs_format(
        Some("127.0.0.1"),
        true,
        None,
        false,
        None,
        false,
        true,
        "4",
    );
    let db = server.root.join("index");
    assert!(db.join("index.json").is_file());
    assert!(!db.join("bm25.json").exists());
    let query = |sql: &str| -> Value {
        server
            .post("/ti/query", json!({"sql":sql}), true)
            .unwrap()
            .into_json()
            .unwrap()
    };
    let sql = "SELECT s.title, s.body, count(*) AS n FROM sections s CROSS JOIN telemetry t WHERE match(s.body,'bilge pump') GROUP BY s.title,s.body";
    let initial = query(sql);
    assert_eq!(initial["rows"][0]["n"], 2);
    assert!(initial["rows"][0]["body"]
        .as_str()
        .unwrap()
        .contains("original"));
    let cli = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args(["ti", "query", sql, "--store"])
        .arg(server.root.join("store"))
        .arg("--docs-index")
        .arg(&db)
        .arg("--json")
        .env_remove("LUME_EMBED_MODEL")
        .env_remove("LUME_EMBED_DIMENSIONS")
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cli: Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(cli["rows"], initial["rows"]);
    let runtime = ti_sql::surface_runtime().unwrap();
    let (client, connection) = runtime
        .block_on(tokio_postgres::connect(
            &format!(
                "host=127.0.0.1 port={} user=lume dbname=lume",
                server.pg.rsplit(':').next().unwrap()
            ),
            tokio_postgres::NoTls,
        ))
        .unwrap();
    runtime.spawn(async move {
        connection.await.unwrap();
    });
    assert_eq!(
        runtime
            .block_on(client.query_one("SELECT count(*) FROM sections", &[]))
            .unwrap()
            .get::<_, i64>(0),
        1
    );

    std::fs::write(server.root.join("manual/manual.md"), "## Rebuilt manual\n\nBilge pump replacement procedure. Disconnect power, fit the replacement pump, check the discharge hose and test the float switch before departure.\n").unwrap();
    rebuild_manual_format(&server.root, "4");
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let reply = query(sql);
        if reply["rows"][0]["body"]
            .as_str()
            .unwrap()
            .contains("replacement")
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{reply}");
        std::thread::sleep(Duration::from_millis(200));
    }
    let index = lume::search::LoadedIndex::open(&db).unwrap();
    lume::index_binary::overlays::publish(
        &db,
        vec![lume::index_binary::overlays::Replacement {
            section: 0,
            source_hash: lume::index_binary::overlays::source_hash(&index.bm25.sections[0]),
            entities: vec!["Lagoon".into(), "Bilge Pump".into()],
        }],
        |_| Ok(()),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let reply = query("SELECT entity FROM entities WHERE entity='lagoon'");
        if reply["row_count"] == 1 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{reply}");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(
        runtime
            .block_on(client.query_one("SELECT entity FROM entities WHERE entity='lagoon'", &[]))
            .unwrap()
            .get::<_, String>(0),
        "lagoon"
    );
    assert_eq!(query(sql)["rows"][0]["n"], 2);
    assert_eq!(query("SELECT d.id FROM docs d CROSS JOIN sections s WHERE d.kind='alerts' AND match(s.body,'bilge pump')")["rows"][0]["id"], "alert:bilge");
}
