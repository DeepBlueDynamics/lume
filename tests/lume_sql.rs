#![cfg(feature = "ti")]
use lume::search::{search, LoadedIndex};
use serde_json::{json, Value};
use std::{path::PathBuf, process::Command, sync::Arc};
use ti_contracts::{
    BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, ShardSink, VesselSpec,
};
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "lume-sql-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn index(&self) -> PathBuf {
        let index = self.root.join("index");
        let output = Command::new(env!("CARGO_BIN_EXE_lume"))
            .args([
                "index",
                concat!(env!("CARGO_MANIFEST_DIR"), "/docs/monte_cristo"),
                "--db",
            ])
            .arg(&index)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        index
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[test]
fn monte_cristo_ten_queries_equal_lexical_search_hits_and_order() {
    let fixture = Fixture::new();
    let root = fixture.index();
    let index = LoadedIndex::open(&root).unwrap();
    let runtime = ti_sql::surface_runtime().unwrap();
    let engine = runtime.block_on(lume::sql::open(&root)).unwrap();
    let golden: Value = serde_json::from_str(include_str!("golden/lume_sql.json")).unwrap();
    for query in golden["queries"].as_array().unwrap() {
        let query = query.as_str().unwrap();
        let hits = search(
            &index,
            query,
            &lume::sql::lexical_options(index.bm25.sections.len()),
        )
        .unwrap()
        .hits;
        assert!(!hits.is_empty(), "empty golden query {query}");
        let sql = format!(
            "SELECT count(*) AS n FROM sections WHERE match(body, '{}')",
            query.replace('\'', "''")
        );
        let reply = runtime.block_on(engine.query(&sql, 500)).unwrap();
        assert_eq!(
            reply["rows"][0]["n"].as_u64().unwrap(),
            hits.len() as u64,
            "{query}"
        );
        let sql = format!("SELECT id, score FROM sections WHERE match(body, '{}') ORDER BY score DESC, id LIMIT 10", query.replace('\'', "''"));
        let reply = runtime.block_on(engine.query(&sql, 500)).unwrap();
        let mut expected: Vec<_> = hits.iter().collect();
        expected.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.section_index.cmp(&b.section_index))
        });
        assert_eq!(
            reply["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["id"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .take(10)
                .map(|h| h.section_index as u64)
                .collect::<Vec<_>>(),
            "{query}"
        );
        for (row, hit) in reply["rows"].as_array().unwrap().iter().zip(expected) {
            assert!((row["score"].as_f64().unwrap() - hit.score).abs() < 1e-9);
        }
    }
    let reply = runtime
        .block_on(engine.query("SELECT count(*) AS n FROM sections", 500))
        .unwrap();
    assert_eq!(
        reply["rows"][0]["n"].as_u64().unwrap(),
        index.bm25.sections.len() as u64
    );
    assert!(runtime
        .block_on(engine.query("SELECT score FROM sections LIMIT 1", 500))
        .unwrap()["rows"][0]["score"]
        .is_null());
    for sql in [
        "DELETE FROM sections",
        "CREATE TABLE bad(x INT)",
        "INSERT INTO sections VALUES (1)",
        "DROP TABLE sections",
        "SELECT 1; SELECT 2",
    ] {
        assert!(runtime.block_on(engine.query(sql, 500)).is_err(), "{sql}");
    }
    let reply: Value = serde_json::from_str(
        &lume::sql::call(
            json!({"sql":"SELECT count(*) AS n FROM sections","db":root}),
            "unused",
        )
        .unwrap(),
    )
    .unwrap();
    for key in [
        "rows",
        "columns",
        "row_count",
        "truncated",
        "elapsed_ms",
        "pushdown",
        "units",
        "hint",
    ] {
        assert!(reply.get(key).is_some(), "{key}");
    }
    assert!(lume::sql::call(json!({"sql":"SELECT 1","max_rows":0}), "unused").is_err());
    for sql in [
        "SELECT * FROM generate_series(1, 700)",
        "SELECT repeat('é', 40000) AS big",
    ] {
        let reply: Value = serde_json::from_str(
            &lume::sql::call(json!({"sql":sql,"db":root,"max_rows":999}), "unused").unwrap(),
        )
        .unwrap();
        assert_eq!(reply["truncated"], true);
        assert!(reply["row_count"].as_u64().unwrap() <= 500);
        assert!(serde_json::to_vec(&reply).unwrap().len() <= 65536);
    }
}
#[test]
fn indexed_manual_cross_joins_small_durable_telemetry_store_and_cli() {
    let fixture = Fixture::new();
    let index_root = fixture.root.join("manual");
    std::fs::create_dir_all(&index_root).unwrap();
    let sections = vec![
        lume::bm25::Section {
            title: "Engine manual".into(),
            body: "engine cooling alarm procedure".into(),
            filename: Some("manual.md".into()),
            line_number: 1,
            entities: vec![],
        },
        lume::bm25::Section {
            title: "Sails".into(),
            body: "sail trim instructions".into(),
            filename: Some("manual.md".into()),
            line_number: 10,
            entities: vec![],
        },
    ];
    let index = LoadedIndex::from_parts(lume::bm25::Bm25Index::build(sections, None));
    lume::search::save_json(&index_root.join("bm25.json"), &index.bm25).unwrap();
    let state = lume::search::IndexState {
        target_dir: "manual".into(),
        db_dir: index_root.display().to_string(),
        semantic_enabled: false,
        ollama_entities: false,
        ollama_model: String::new(),
        ollama_url: String::new(),
        tag_dict_path: None,
        semantic_session_id: None,
        cached_files: Default::default(),
    };
    lume::search::save_json(&index_root.join("state.json"), &state).unwrap();
    let store_root = fixture.root.join("store");
    let mut store = ti_store::Store::open_or_create(&store_root, 10).unwrap();
    let vessel = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:test:manual".into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    let field = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "engine.temperature".into(),
            agg: None,
            kind: FieldKind::Bsi { scale: 0 },
            units: Some("K".into()),
        })
        .unwrap();
    store
        .apply(&[1, 2].map(|bucket| BucketRecord {
            vessel,
            bucket,
            field,
            value: FieldValue::Int(300),
            rewrite: false,
        }))
        .unwrap();
    store
        .seal(ti_contracts::ShardKey { vessel, shard: 0 })
        .unwrap();
    store.shutdown().unwrap();
    drop(store);
    let runtime = ti_sql::surface_runtime().unwrap();
    let engine = runtime
        .block_on(ti_sql::TiEngine::open(&store_root, None, None))
        .unwrap();
    lume::sql::register_index(&engine.session, Arc::new(index)).unwrap();
    let sql = "SELECT s.title, count(*) AS n FROM telemetry t CROSS JOIN sections s WHERE match(s.body, 'engine') GROUP BY s.title";
    let reply = runtime.block_on(engine.query(sql, 500)).unwrap();
    assert_eq!(reply["rows"], json!([{"title":"Engine manual","n":2}]));
    let output = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args(["ti", "query", sql, "--store"])
        .arg(&store_root)
        .arg("--docs-index")
        .arg(&index_root)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual["rows"], reply["rows"]);
    let intersection = runtime.block_on(engine.query("SELECT count(*) AS n FROM sections WHERE match(body, 'engine') AND match(body, 'cooling')",500)).unwrap();
    assert_eq!(intersection["rows"][0]["n"], 1);
    assert!(runtime
        .block_on(engine.query(
            "SELECT id FROM sections WHERE match(body, 'engine') OR id = 1",
            500
        ))
        .is_err());
    let absent = runtime.block_on(engine.query("SELECT * FROM entities", 500));
    assert!(absent.is_err());
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args(["sql", "repl", "--db"])
        .arg(&index_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"SELECT count(*)\n AS n FROM sections;\nDELETE FROM sections;\nSELECT 42 AS answer;\n.quit\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("42"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("error:"));
    for format in ["table", "csv", "json"] {
        let output = Command::new(env!("CARGO_BIN_EXE_lume"))
            .args(["sql", "--db"])
            .arg(&index_root)
            .args(["SELECT count(*) AS n FROM sections", "--format", format])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains('2'));
    }
}
#[test]
fn entity_graph_tables_and_argument_shapes() {
    let runtime = ti_sql::surface_runtime().unwrap();
    let session = runtime
        .block_on(ti_sql::SqlSession::new(
            Arc::new(ti_core::MemorySource::new()),
            ti_sql::SqlCatalog::new(10, vec![], vec![], Default::default()).unwrap(),
        ))
        .unwrap();
    let mut index = LoadedIndex::from_parts(lume::bm25::Bm25Index::build(vec![], None));
    index.entity_graph = Some(lume::semantic_mesh::EntityGraph {
        nodes: vec![lume::semantic_mesh::EntityNode {
            id: "engine".into(),
            label: "Engine".into(),
            kind: "equipment".into(),
            frequency: 3,
        }],
        edges: vec![lume::semantic_mesh::EntityEdge {
            source: "engine".into(),
            target: "cooling".into(),
            similarity: 0.5,
            relatedness: 0.8,
            intersection: 1,
            union_size: 2,
        }],
    });
    lume::sql::register_index(&session, Arc::new(index)).unwrap();
    let engine = ti_sql::TiEngine::from_session(session, PathBuf::new());
    assert_eq!(
        runtime
            .block_on(engine.query("SELECT * FROM entities", 500))
            .unwrap()["rows"],
        json!([{"entity":"engine","doc_count":3}])
    );
    assert_eq!(
        runtime
            .block_on(engine.query("SELECT * FROM entity_edges", 500))
            .unwrap()["rows"],
        json!([{"a":"engine","b":"cooling","jaccard":0.5,"relatedness":0.8}])
    );
    let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(lume::sql::parse(&args(&["--db", "index", "SELECT 1"])).is_ok());
    assert!(lume::sql::parse(&args(&["repl", "--db", "index"]))
        .unwrap()
        .sql
        .is_none());
    for a in [
        vec![],
        vec!["--db", "index"],
        vec!["repl", "SELECT 1", "--db", "index"],
        vec!["--db", "index", "SELECT 1", "--format", "bad"],
    ] {
        assert!(lume::sql::parse(&args(&a)).is_err());
    }
    assert_eq!(
        lume::sql::definition()["inputSchema"]["required"],
        json!(["sql"])
    );
    let ti = ti_sql::cli::parse(&args(&[
        "query",
        "SELECT 1",
        "--store",
        "ti",
        "--docs-index",
        "index",
    ]))
    .unwrap();
    assert_eq!(ti.docs_index, Some(PathBuf::from("index")));
}
