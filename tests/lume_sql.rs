#![cfg(feature = "ti")]
use lume::search::{search, LoadedIndex};
use serde_json::{json, Value};
use std::{path::PathBuf, process::Command, sync::Arc};
use ti_contracts::{
    BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, ShardSink, VesselSpec,
};
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RESIDENT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
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
    let _guard = RESIDENT_TEST_LOCK.lock().unwrap();
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
        format_version: lume::search::FORMAT_VERSION_STEMMED,
        target_dir: "manual".into(),
        db_dir: index_root.display().to_string(),
        semantic_enabled: false,
        ollama_entities: false,
        ollama_model: String::new(),
        ollama_url: String::new(),
        tag_dict_path: None,
        semantic_session_id: None,
        cached_files: Default::default(),
        stemmed: true,
        keep_hyphens: false,
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

#[test]
fn monte_cristo_match_and_not_match() {
    let _guard = RESIDENT_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    let root = fixture.index();
    let runtime = ti_sql::surface_runtime().unwrap();
    let engine = runtime.block_on(lume::sql::open(&root)).unwrap();

    let sql_pos =
        "SELECT id, score FROM sections WHERE match(body, 'dantes') ORDER BY score DESC, id";
    let reply_pos = runtime.block_on(engine.query(sql_pos, 500)).unwrap();
    let rows_pos = reply_pos["rows"].as_array().unwrap();
    let count_pos = rows_pos.len();
    assert!(count_pos > 0);

    let pos_scores: std::collections::BTreeMap<u64, f64> = rows_pos
        .iter()
        .map(|r| (r["id"].as_u64().unwrap(), r["score"].as_f64().unwrap()))
        .collect();

    let sql_not = "SELECT id, score FROM sections WHERE match(body, 'dantes') AND NOT match(body, 'prison') ORDER BY score DESC, id";
    let reply_not = runtime.block_on(engine.query(sql_not, 500)).unwrap();
    let rows_not = reply_not["rows"].as_array().unwrap();
    let count_not = rows_not.len();

    assert!(
        count_not < count_pos,
        "NOT match() should exclude sections containing negated term"
    );
    assert!(count_not > 0, "should still have hits for positive term");

    // Edge case 3: scores of remaining rows must equal scores from positive match alone
    for r in rows_not {
        let id = r["id"].as_u64().unwrap();
        let score = r["score"].as_f64().unwrap();
        let original_score = pos_scores
            .get(&id)
            .expect("row must exist in positive hits");
        assert!(
            (score - original_score).abs() < 1e-9,
            "score for id {id} changed: {score} vs {original_score}"
        );
    }

    // Edge case 1: WHERE NOT match(body, 'x') with no positive match
    // Returns every section except matches, with score IS NULL
    let sql_prison = "SELECT count(*) AS n FROM sections WHERE match(body, 'prison')";
    let reply_prison = runtime.block_on(engine.query(sql_prison, 500)).unwrap();
    let count_prison = reply_prison["rows"][0]["n"].as_u64().unwrap();

    let sql_total = "SELECT count(*) AS n FROM sections";
    let reply_total = runtime.block_on(engine.query(sql_total, 500)).unwrap();
    let count_total = reply_total["rows"][0]["n"].as_u64().unwrap();

    let sql_only_not = "SELECT count(*) AS n FROM sections WHERE NOT match(body, 'prison')";
    let reply_only_not = runtime.block_on(engine.query(sql_only_not, 500)).unwrap();
    let count_only_not = reply_only_not["rows"][0]["n"].as_u64().unwrap();
    assert_eq!(
        count_only_not,
        count_total - count_prison,
        "NOT match() without positive match should return all non-matching sections"
    );

    let sql_only_not_score = "SELECT score FROM sections WHERE NOT match(body, 'prison') LIMIT 1";
    let reply_only_not_score = runtime
        .block_on(engine.query(sql_only_not_score, 500))
        .unwrap();
    assert!(
        reply_only_not_score["rows"][0]["score"].is_null(),
        "score must be NULL when there is no positive match"
    );

    // Edge case 2: match() under OR is not a top-level conjunct and must error clearly
    for bad_sql in [
        "SELECT id FROM sections WHERE match(body, 'dantes') OR NOT match(body, 'prison')",
        "SELECT id FROM sections WHERE match(body, 'dantes') OR match(body, 'prison')",
    ] {
        let err = runtime.block_on(engine.query(bad_sql, 500)).unwrap_err();
        assert!(
            err.to_string()
                .contains("match() is only supported as a top-level AND filter"),
            "expected clear error message for OR query '{bad_sql}', got: {err}"
        );
    }
}

#[test]
fn monte_cristo_lume_sql_reuses_resident_index() {
    let _guard = RESIDENT_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    let root = fixture.index();
    let runtime = ti_sql::surface_runtime().unwrap();

    let engine1 = runtime.block_on(lume::sql::open(&root)).unwrap();
    let cached1 = lume::resident_index::open(&root).unwrap();

    let reply1 = runtime
        .block_on(engine1.query("SELECT count(*) AS n FROM sections", 500))
        .unwrap();
    assert!(reply1["rows"][0]["n"].as_u64().unwrap() > 0);

    let engine2 = runtime.block_on(lume::sql::open(&root)).unwrap();
    let cached2 = lume::resident_index::open(&root).unwrap();
    assert!(
        Arc::ptr_eq(&cached1, &cached2),
        "engine open must reuse the in-memory resident index"
    );

    let reply2 = runtime
        .block_on(engine2.query("SELECT count(*) AS n FROM sections", 500))
        .unwrap();
    assert_eq!(reply2["rows"], reply1["rows"]);

    // MCP call interface
    let reply_mcp1: Value = serde_json::from_str(
        &lume::sql::call(
            json!({"sql": "SELECT count(*) AS n FROM sections", "db": root}),
            "unused",
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(reply_mcp1["rows"], reply1["rows"]);

    let cached3 = lume::resident_index::open(&root).unwrap();
    assert!(
        Arc::ptr_eq(&cached1, &cached3),
        "MCP call must reuse the in-memory resident index"
    );

    let reply_mcp2: Value = serde_json::from_str(
        &lume::sql::call(
            json!({"sql": "SELECT count(*) AS n FROM sections", "db": root}),
            "unused",
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(reply_mcp2["rows"], reply1["rows"]);

    let cached4 = lume::resident_index::open(&root).unwrap();
    assert!(
        Arc::ptr_eq(&cached1, &cached4),
        "Subsequent MCP call must reuse the in-memory resident index"
    );
}

#[test]
fn metadata_columns_pushdown_and_facets_equivalence() {
    let fixture = Fixture::new();
    let index_root = fixture.root.join("meta_sql");
    std::fs::create_dir_all(&index_root).unwrap();

    let sec0 = lume::bm25::Section {
        title: "Paper 0".into(),
        body: "cancer immunotherapy dna genetics".into(),
        filename: Some("d0.md".into()),
        line_number: 1,
        entities: vec![],
    };
    let sec1 = lume::bm25::Section {
        title: "Paper 1".into(),
        body: "cancer therapy dna clinical trials".into(),
        filename: Some("d1.md".into()),
        line_number: 1,
        entities: vec![],
    };
    let sec2 = lume::bm25::Section {
        title: "Paper 2".into(),
        body: "astronomy stars galaxies telescope".into(),
        filename: Some("d2.md".into()),
        line_number: 1,
        entities: vec![],
    };
    let sec3 = lume::bm25::Section {
        title: "Paper 3".into(),
        body: "cancer overview epidemiology".into(),
        filename: Some("d3.md".into()),
        line_number: 1,
        entities: vec![],
    };
    let bm25 = lume::bm25::Bm25Index::build(vec![sec0, sec1, sec2, sec3], None);

    let mut schema = std::collections::HashMap::new();
    schema.insert("category".into(), lume::meta::FieldType::Keyword);
    schema.insert("tags".into(), lume::meta::FieldType::KeywordList);
    schema.insert("year".into(), lume::meta::FieldType::Integer);

    let mut columns = std::collections::HashMap::new();
    columns.insert(
        "category".into(),
        lume::meta::Column::Keyword {
            dict: vec!["biology".into(), "medicine".into()],
            ords: vec![Some(0), Some(1), None, None],
            bitmaps: vec![
                lume::fast_retrieval::MiniRoaring::from_sorted(&[0]),
                lume::fast_retrieval::MiniRoaring::from_sorted(&[1]),
            ],
        },
    );
    columns.insert(
        "tags".into(),
        lume::meta::Column::KeywordList {
            dict: vec!["clinical".into(), "dna".into(), "general".into()],
            offsets: vec![0, 1, 3, 3, 4],
            ords: vec![1, 0, 1, 2], // sec0: [dna], sec1: [clinical, dna], sec2: [], sec3: [general]
            bitmaps: vec![
                lume::fast_retrieval::MiniRoaring::from_sorted(&[1]),
                lume::fast_retrieval::MiniRoaring::from_sorted(&[0, 1]),
                lume::fast_retrieval::MiniRoaring::from_sorted(&[3]),
            ],
        },
    );
    columns.insert(
        "year".into(),
        lume::meta::Column::Integer {
            present_runs: vec![[0, 4]],
            values: vec![Some(2015), Some(2025), Some(2022), Some(2018)],
        },
    );

    let meta = lume::meta::MetaIndex {
        meta_version: 1,
        num_sections: 4,
        generation: "test-gen".into(),
        schema,
        files: std::collections::HashMap::new(),
        columns,
    };

    let index = LoadedIndex {
        state: None,
        bm25,
        spelling: None,
        entity_graph: None,
        tagger: None,
        cache_dir: None,
        meta: Some(meta),
    };

    let index_arc = Arc::new(index);
    let runtime = ti_sql::surface_runtime().unwrap();
    let session = runtime
        .block_on(ti_sql::SqlSession::new(
            Arc::new(ti_core::MemorySource::new()),
            ti_sql::SqlCatalog::new(10, vec![], vec![], Default::default()).unwrap(),
        ))
        .unwrap();
    lume::sql::register_index(&session, Arc::clone(&index_arc)).unwrap();
    let engine = ti_sql::TiEngine::from_session(session, index_root);

    // 1. Native facet vs SQL GROUP BY category
    let opts = lume::search::SearchOptions {
        limit: 10,
        mode: lume::search::SearchMode::LexicalOnly,
        graph_beta: 0.0,
        facets: vec![lume::meta::FacetRequest::Field("category".into())],
        ..Default::default()
    };
    let native_res = lume::search::search(&index_arc, "cancer", &opts).unwrap();
    let native_cat_facet = native_res.facets.as_ref().unwrap().get("category").unwrap();

    let sql_cat = "SELECT category, count(*) AS n FROM sections WHERE match(body, 'cancer') AND category IS NOT NULL GROUP BY category ORDER BY category";
    let reply_cat = runtime.block_on(engine.query(sql_cat, 500)).unwrap();
    let rows_cat = reply_cat["rows"].as_array().unwrap();

    if let lume::meta::FacetResult::Field { buckets, missing } = native_cat_facet {
        assert_eq!(*missing, 1);
        assert_eq!(rows_cat.len(), buckets.len());
        let sql_map: std::collections::BTreeMap<String, u64> = rows_cat
            .iter()
            .map(|r| {
                (
                    r["category"].as_str().unwrap().to_string(),
                    r["n"].as_u64().unwrap(),
                )
            })
            .collect();
        let native_map: std::collections::BTreeMap<String, u64> = buckets
            .iter()
            .map(|b| (b.val.clone(), b.count as u64))
            .collect();
        assert_eq!(sql_map, native_map);

        for window in buckets.windows(2) {
            let (b1, b2) = (&window[0], &window[1]);
            assert!(
                b1.count > b2.count || (b1.count == b2.count && b1.val <= b2.val),
                "native buckets must be sorted by count desc, then value asc; got {:?} before {:?}",
                b1,
                b2
            );
        }
    } else {
        panic!("expected field facet for category");
    }

    // 2. year >= 2020 is reported Exact in EXPLAIN
    let table = lume::sql::SectionsTable {
        index: Arc::clone(&index_arc),
    };
    use ti_sql::datafusion::catalog::TableProvider;
    use ti_sql::datafusion::logical_expr::{col, lit, TableProviderFilterPushDown};
    let year_filter = col("year").gt_eq(lit(2020i64));
    let pushdown = table.supports_filters_pushdown(&[&year_filter]).unwrap();
    assert_eq!(
        pushdown,
        vec![TableProviderFilterPushDown::Exact],
        "year >= 2020 must be reported Exact"
    );

    let plan_str = runtime
        .block_on(
            engine
                .session
                .explain("SELECT id FROM sections WHERE year >= 2020"),
        )
        .unwrap();
    assert!(
        !plan_str.contains("FilterExec"),
        "FilterExec should not be present when year >= 2020 is pushed down as Exact: {plan_str}"
    );

    // 3. unnest(tags) equals the tags facet
    let opts_tags = lume::search::SearchOptions {
        limit: 10,
        mode: lume::search::SearchMode::LexicalOnly,
        graph_beta: 0.0,
        facets: vec![lume::meta::FacetRequest::Field("tags".into())],
        ..Default::default()
    };
    let native_res_tags = lume::search::search(&index_arc, "cancer", &opts_tags).unwrap();
    let native_tags_facet = native_res_tags
        .facets
        .as_ref()
        .unwrap()
        .get("tags")
        .unwrap();

    let sql_tags = "SELECT tag, count(*) AS n FROM (SELECT unnest(tags) AS tag FROM sections WHERE match(body, 'cancer')) GROUP BY tag ORDER BY tag";
    let reply_tags = runtime.block_on(engine.query(sql_tags, 500)).unwrap();
    let rows_tags = reply_tags["rows"].as_array().unwrap();

    if let lume::meta::FacetResult::Field { buckets, missing } = native_tags_facet {
        assert_eq!(*missing, 0);
        assert_eq!(rows_tags.len(), buckets.len());
        let sql_map: std::collections::BTreeMap<String, u64> = rows_tags
            .iter()
            .map(|r| {
                (
                    r["tag"].as_str().unwrap().to_string(),
                    r["n"].as_u64().unwrap(),
                )
            })
            .collect();
        let native_map: std::collections::BTreeMap<String, u64> = buckets
            .iter()
            .map(|b| (b.val.clone(), b.count as u64))
            .collect();
        assert_eq!(sql_map, native_map);

        for window in buckets.windows(2) {
            let (b1, b2) = (&window[0], &window[1]);
            assert!(
                b1.count > b2.count || (b1.count == b2.count && b1.val <= b2.val),
                "native buckets must be sorted by count desc, then value asc; got {:?} before {:?}",
                b1,
                b2
            );
        }
    } else {
        panic!("expected field facet for tags");
    }

    // 4. the no-match() path is restricted by the filter
    let sql_no_match = "SELECT count(*) AS n FROM sections WHERE year >= 2020";
    let reply_no_match = runtime.block_on(engine.query(sql_no_match, 500)).unwrap();
    assert_eq!(reply_no_match["rows"][0]["n"].as_u64().unwrap(), 2); // sec1 (2025) and sec2 (2022)

    let sql_no_match_rows = "SELECT id, score, year FROM sections WHERE year >= 2020 ORDER BY id";
    let reply_rows = runtime
        .block_on(engine.query(sql_no_match_rows, 500))
        .unwrap();
    let rows = reply_rows["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"].as_u64().unwrap(), 1);
    assert!(rows[0]["score"].is_null());
    assert_eq!(rows[0]["year"].as_i64().unwrap(), 2025);
    assert_eq!(rows[1]["id"].as_u64().unwrap(), 2);
    assert!(rows[1]["score"].is_null());
    assert_eq!(rows[1]["year"].as_i64().unwrap(), 2022);

    // array_has(tags, 'dna') on no-match path
    let sql_array_has = "SELECT count(*) AS n FROM sections WHERE array_has(tags, 'dna')";
    let reply_array_has = runtime.block_on(engine.query(sql_array_has, 500)).unwrap();
    assert_eq!(reply_array_has["rows"][0]["n"].as_u64().unwrap(), 2); // sec0 and sec1
}

#[test]
fn test_plain_index_writes_format_version_2_and_loads() {
    let _lock = RESIDENT_TEST_LOCK.lock().unwrap();
    let fixture = Fixture::new();
    let index_dir = fixture.index();
    let state_raw = std::fs::read_to_string(index_dir.join("state.json")).unwrap();
    let state: Value = serde_json::from_str(&state_raw).unwrap();
    assert_eq!(
        state["format_version"], 2,
        "plain index must have format_version 2"
    );
    assert!(
        !index_dir.join("meta.json").exists(),
        "meta.json must not exist for plain index"
    );
    let loaded = LoadedIndex::open(&index_dir).unwrap();
    assert!(loaded.meta.is_none());
}
