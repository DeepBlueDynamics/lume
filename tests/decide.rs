#![cfg(feature = "ti")]

use lume::decide::{
    append_agent_run_log, build_bounded_state, build_decide_request_payload, cloud_teacher_notice,
    decide_block_from_http_error, evaluate_decide_response, finish_run_decide_and_log,
    select_top_sql_examples, AgentRunRecord, DecideConfig, DecideLogBlock, DEFAULT_DECIDE_PRESET,
    DEFAULT_DECIDE_TIMEOUT_SECS,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use ti_contracts::{
    Agg, BucketRecord, Catalog, Document, FieldKind, FieldSpec, FieldValue, ShardSink, VesselSpec,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TestDir {
    root: PathBuf,
    store_root: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "decide-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let store_root = root.join("store");
        fs::create_dir_all(&store_root).unwrap();

        let mut store = ti_store::Store::open_or_create(&store_root, 10).unwrap();
        let vessel = store
            .catalog()
            .register_vessel(&VesselSpec {
                urn: "vessels.urn:test:decide".into(),
                name: Some("Test Vessel".into()),
                mmsi: None,
            })
            .unwrap();
        let field = store
            .catalog()
            .register_field(&FieldSpec {
                id: 0,
                path: "navigation.speedOverGround".into(),
                agg: Some(Agg::Mean),
                kind: FieldKind::Bsi { scale: 3 },
                units: Some("m/s".into()),
            })
            .unwrap();
        store
            .apply(&[BucketRecord {
                vessel,
                bucket: 1,
                field,
                value: FieldValue::Int(7200),
                rewrite: false,
            }])
            .unwrap();
        store
            .seal(ti_contracts::ShardKey { vessel, shard: 0 })
            .unwrap();
        store.shutdown().unwrap();
        fs::write(store_root.join("ti.toml"), "width_seconds = 10\n").unwrap();

        Self { root, store_root }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Real captured 200 OK response from Addita 0.1.0 (`main` commit `fb1d519`)
/// with live Ollaya 0.7.2 (`laya:en`) + live Ollama (`qwen3.5:4b`).
const REAL_ADDITA_RESPONSE_200: &str = r#"{"model":"laya:en","answers":{"rows_answer_question":{"type":"noul","noul":0.8536},"answer_supported":{"type":"noul","noul":0.7887}},"usage":{"input_tokens":568,"output_tokens":0},"routing":{"router":"laya:latest","model":"laya:en","route":"english","reason":"English Latin text"},"state_truncated":false,"done_reason":"decide","created_at":"2026-10-16T19:17:49.972150515Z","total_duration":2906119325,"load_duration":0,"eval_duration":389952383,"addita":{"id":"81614d3f-2889-4f05-9e44-75b315d19496","ref":"lume-run-fb1d519-001","final":{"rows_answer_question":true,"answer_supported":true},"source":"teacher","teacher":"answered","teacher_model":"qwen3.5:4b","teacher_host":"host.docker.internal:11434","guards":[],"agreement":{"rows_answer_question":true,"answer_supported":true},"teacher_answers":{"rows_answer_question":true,"answer_supported":true},"student_ms":405,"teacher_ms":2466}}"#;

#[test]
fn real_addita_contract_run_log_and_system_prompt_example_injection() {
    let fixture = TestDir::new();
    let cfg = DecideConfig::default();
    assert_eq!(
        cfg.timeout,
        Duration::from_secs(DEFAULT_DECIDE_TIMEOUT_SECS),
        "default decide timeout must be 30s"
    );

    let sql_query =
        "SELECT ts, \"navigation.speedOverGround\" FROM telemetry ORDER BY ts DESC LIMIT 5";
    let state = build_bounded_state(
        "What was our speed over ground?",
        sql_query,
        &[sql_query.to_string()],
        &[json!({"ts": "2026-10-16T18:00:00Z", "navigation.speedOverGround": 7.2})],
        false,
        None,
        "The speed over ground is 7.2 m/s.",
    );

    // 1. Verify Addita vs plain Ollaya request payload & preset flag
    let req_inline = build_decide_request_payload(&cfg, "lume-run-real-001", &state, true);
    assert_eq!(req_inline["model"], "laya");
    assert!(req_inline.get("preset").is_none());
    assert_eq!(
        req_inline["questions"]["rows_answer_question"]["type"],
        "noul"
    );
    assert_eq!(
        req_inline["questions"]["rows_answer_question"]["criteria"]["true"],
        "The SQL and its returned rows directly answer what the user asked."
    );
    assert_eq!(
        req_inline["questions"]["answer_supported"]["criteria"]["true"],
        "Every figure and claim in the answer is grounded in state.rows."
    );
    assert_eq!(req_inline["addita"], json!({"ref": "lume-run-real-001"}));
    assert!(req_inline["addita"].get("allow_cloud_teacher").is_none());

    let req_ollaya = build_decide_request_payload(&cfg, "lume-run-real-001", &state, false);
    assert!(
        req_ollaya.get("addita").is_none(),
        "plain Ollaya request must omit addita block"
    );

    let cfg_preset = DecideConfig {
        decide_preset: Some(DEFAULT_DECIDE_PRESET.to_string()),
        ..Default::default()
    };
    let req_preset = build_decide_request_payload(&cfg_preset, "lume-run-real-002", &state, true);
    assert_eq!(req_preset["preset"], "lume.sql_judge");
    assert!(req_preset.get("questions").is_none());

    // 2. Evaluate real captured Addita 200 OK response and append to <store>/agent_runs/
    let parsed_resp: Value = serde_json::from_str(REAL_ADDITA_RESPONSE_200).unwrap();
    let decide_block = evaluate_decide_response(&parsed_resp, 0.8).unwrap();
    assert_eq!(decide_block.judged, json!(true));
    assert!(decide_block.good);
    assert_eq!(decide_block.routing_model.as_deref(), Some("laya:en"));
    assert_eq!(
        decide_block.addita_id.as_deref(),
        Some("81614d3f-2889-4f05-9e44-75b315d19496")
    );
    assert_eq!(decide_block.teacher.as_deref(), Some("answered"));
    assert_eq!(decide_block.teacher_model.as_deref(), Some("qwen3.5:4b"));
    assert_eq!(
        decide_block.teacher_host.as_deref(),
        Some("host.docker.internal:11434")
    );
    assert_eq!(decide_block.student_ms, Some(405));
    assert_eq!(decide_block.teacher_ms, Some(2466));
    assert_eq!(decide_block.noul.get("rows_answer_question"), Some(&0.8536));
    assert_eq!(decide_block.noul.get("answer_supported"), Some(&0.7887));
    assert_eq!(cloud_teacher_notice(&decide_block), None);

    let record = AgentRunRecord {
        ts: "2026-10-16T19:17:49Z".to_string(),
        run_ref: "lume-run-fb1d519-001".to_string(),
        question: "What was our speed over ground?".to_string(),
        sql: vec![sql_query.to_string()],
        rows_sample: state.rows.clone(),
        answer: state.answer.clone(),
        model: "qwen3.5:4b".to_string(),
        decide: decide_block,
    };
    let log_file = append_agent_run_log(&fixture.store_root, &record).unwrap();
    let log_raw = fs::read_to_string(&log_file).unwrap();
    let logged: Value = serde_json::from_str(log_raw.lines().next().unwrap()).unwrap();
    assert_eq!(logged["decide"]["judged"], true);
    assert_eq!(logged["decide"]["good"], true);
    assert_eq!(logged["decide"]["routing_model"], "laya:en");
    assert_eq!(logged["decide"]["student_ms"], 405);
    assert_eq!(logged["decide"]["teacher_ms"], 2466);
    assert_eq!(logged["decide"]["noul"]["rows_answer_question"], 0.8536);
    assert_eq!(logged["decide"]["noul"]["answer_supported"], 0.7887);
    assert_eq!(
        logged["decide"]["addita_id"],
        "81614d3f-2889-4f05-9e44-75b315d19496"
    );
    assert_eq!(logged["decide"]["teacher_model"], "qwen3.5:4b");
    assert_eq!(
        logged["decide"]["teacher_host"],
        "host.docker.internal:11434"
    );

    // 3. Verify build_system_prompt_with_examples injects Run 1's SQL example, and --no-examples suppresses it
    let store_str = fixture.store_root.to_str().unwrap();
    let sys_prompt = lume::chat_sql::build_system_prompt_with_examples(
        Some(store_str),
        None,
        ".lume-index",
        "What was our speed over ground?",
        false,
    );
    assert!(
        sys_prompt.contains(
            "Past questions on this store answered with working SQL (examples, adapt; don't copy blindly):"
        ),
        "system prompt did not include few-shot header:\n{sys_prompt}"
    );
    assert!(sys_prompt.contains(sql_query));

    let sys_prompt_no_ex = lume::chat_sql::build_system_prompt_with_examples(
        Some(store_str),
        None,
        ".lume-index",
        "What was our speed over ground?",
        true,
    );
    assert!(
        !sys_prompt_no_ex.contains("Past questions on this store answered with working SQL"),
        "--no-examples must suppress example injection"
    );

    // 4. Verify --no-decide and --no-agent-log in finish_run_decide_and_log
    let lines_before = fs::read_to_string(&log_file).unwrap().lines().count();
    let cfg_opt_out = DecideConfig {
        decide_url: Some("http://127.0.0.1:11345".to_string()),
        no_decide: true,
        no_agent_log: true,
        ..Default::default()
    };
    let block_opt_out = finish_run_decide_and_log(
        &cfg_opt_out,
        "What was our speed over ground?",
        &[sql_query.to_string()],
        Some(sql_query),
        &state.rows,
        false,
        None,
        "The speed over ground is 7.2 m/s.",
        "qwen3.5:4b",
        Some(store_str),
        None,
        ".lume-index",
    );
    assert_eq!(block_opt_out, DecideLogBlock::default());
    let lines_after = fs::read_to_string(&log_file).unwrap().lines().count();
    assert_eq!(
        lines_after, lines_before,
        "--no-agent-log must prevent appending to agent_runs"
    );
}

#[test]
fn late_pending_resolution_via_ti_docs_and_cloud_teacher_notice() {
    let fixture = TestDir::new();

    // Real Addita response when teacher is offline ("pending") and student fallback is in addita.final
    let resp_pending = json!({
        "model": "laya:en",
        "answers": {
            "rows_answer_question": { "type": "noul", "noul": 0.91 },
            "answer_supported": { "type": "noul", "noul": 0.88 }
        },
        "done_reason": "decide",
        "addita": {
            "id": "addita-dec-pending-1",
            "ref": "lume-run-pending-001",
            "final": {
                "rows_answer_question": true,
                "answer_supported": true
            },
            "source": "student",
            "teacher": "pending",
            "teacher_model": "glm-5.1:cloud",
            "teacher_host": "ollama.com"
        }
    });
    let pending_block = evaluate_decide_response(&resp_pending, 0.8).unwrap();
    assert_eq!(pending_block.judged, json!("pending"));
    assert!(
        !pending_block.good,
        "pending teacher run must be stored as good = false until resolved"
    );
    assert_eq!(
        cloud_teacher_notice(&pending_block).as_deref(),
        Some("judged by cloud teacher glm-5.1:cloud")
    );

    let sql_query =
        "SELECT ts, \"navigation.speedOverGround\" FROM telemetry ORDER BY ts DESC LIMIT 5";
    let pending_record = AgentRunRecord {
        ts: "2026-10-16T18:50:00Z".to_string(),
        run_ref: "lume-run-pending-001".to_string(),
        question: "What was our speed over ground?".to_string(),
        sql: vec![sql_query.to_string()],
        rows_sample: vec![],
        answer: "7.2 m/s".to_string(),
        model: "qwen3.5:4b".to_string(),
        decide: pending_block,
    };
    append_agent_run_log(&fixture.store_root, &pending_record).unwrap();

    // Before late OTLP decision arrives, pending run is not selected as GOOD
    assert!(
        select_top_sql_examples(&fixture.store_root, "What was our speed over ground?").is_empty()
    );

    // Write late OTLP decision record into TI store docs table with title = "addita.decision.final"
    let mut doc_store = ti_store::DocStore::open(&fixture.store_root).unwrap();
    doc_store
        .upsert_all([Document {
            id: "doc-final-1".into(),
            vessel: "agent.urn:addita".into(),
            kind: "logbook".into(),
            ts_start: ti_contracts::EPOCH + 100,
            ts_end: None,
            title: "addita.decision.final".into(),
            body: serde_json::to_string(&json!({
                "id": "addita-dec-pending-1",
                "ref": "lume-run-pending-001",
                "final": {
                    "rows_answer_question": true,
                    "answer_supported": true
                }
            }))
            .unwrap(),
        }])
        .unwrap();

    // Verify that select_top_sql_examples promotes the pending run to GOOD via docs table lookup
    let examples = select_top_sql_examples(&fixture.store_root, "What was our speed over ground?");
    assert_eq!(examples.len(), 1);
    assert_eq!(examples[0].sql, sql_query);

    // Also verify real non-2xx error body parsing produces judged = false with error_code
    let err_block = decide_block_from_http_error(
        422,
        r#"{"error":"questions: Field required","code":"INVALID_REQUEST","detail":[{"type":"missing","loc":["body","questions"],"msg":"Field required"}]}"#,
    );
    assert_eq!(err_block.judged, json!(false));
    assert!(!err_block.good);
    assert_eq!(err_block.error_code.as_deref(), Some("INVALID_REQUEST"));
}

#[test]
fn cli_help_and_flag_validation() {
    let help = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args(["agent", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let stdout = String::from_utf8_lossy(&help.stdout);
    for flag in [
        "--decide-url",
        "--decide-model",
        "--decide-token-file",
        "--decide-threshold",
        "--decide-preset",
        "--decide-timeout",
        "--no-decide",
        "--no-agent-log",
        "--no-examples",
    ] {
        assert!(stdout.contains(flag), "missing {flag} in lume agent --help");
    }
    assert!(
        !stdout.contains("--decide-allow-cloud-teacher"),
        "--decide-allow-cloud-teacher must be removed from help"
    );

    let bad_threshold = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args(["agent", "--decide-threshold", "1.5", "question"])
        .output()
        .unwrap();
    assert!(!bad_threshold.status.success());
    assert!(String::from_utf8_lossy(&bad_threshold.stderr).contains("Invalid --decide-threshold"));

    let bad_timeout = Command::new(env!("CARGO_BIN_EXE_lume"))
        .args(["agent", "--decide-timeout", "0", "question"])
        .output()
        .unwrap();
    assert!(!bad_timeout.status.success());
    assert!(String::from_utf8_lossy(&bad_timeout.stderr).contains("Invalid --decide-timeout"));
}
