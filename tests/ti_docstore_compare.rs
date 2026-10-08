#![cfg(feature = "ti")]
use std::{path::PathBuf, process::Command};
use ti_contracts::{Catalog, Document, ShardSink};

#[test]
fn q2_comparison_reads_append_documents_through_real_sql() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("docstore-q2-compare-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let store_root = root.join("store");
    let corpus: serde_json::Value = serde_json::from_slice(
        &std::fs::read(repository.join("tests/golden/corpus.json")).unwrap(),
    )
    .unwrap();
    let entry = corpus["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "q2-001")
        .unwrap();
    let rows: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            repository
                .join("tests/golden")
                .join(entry["expected_path"].as_str().unwrap()),
        )
        .unwrap(),
    )
    .unwrap();
    let expected = rows.as_array().unwrap();
    assert_eq!(expected.len(), 3);
    let vessel_urn = expected[0]["vessel"].as_str().unwrap();
    let mut store = ti_store::Store::open_or_create(&store_root, 10).unwrap();
    let vessel = store
        .catalog()
        .register_vessel(&ti_contracts::VesselSpec {
            urn: vessel_urn.into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
    store
        .seal(ti_contracts::ShardKey { vessel, shard: 0 })
        .unwrap();
    store.shutdown().unwrap();
    std::fs::write(store_root.join("ti.toml"), "width_seconds = 10\n").unwrap();
    let mut documents = ti_store::DocStore::open(&store_root).unwrap();
    documents
        .upsert_all(expected.iter().enumerate().map(|(index, row)| {
            let start = chrono::NaiveDateTime::parse_from_str(
                row["win"].as_str().unwrap(),
                "%Y-%m-%d %H:%M:%S",
            )
            .unwrap()
            .and_utc()
            .timestamp();
            Document {
                id: format!("note-{index}"),
                vessel: vessel_urn.into(),
                kind: "notes".into(),
                ts_start: start,
                ts_end: Some(start + 600),
                title: "Leak note".into(),
                body: "water leak inspection".into(),
            }
        }))
        .unwrap();
    assert!(store_root.join("docs/documents.log").exists());
    assert!(!store_root.join("docs/documents.json").exists());
    let reply = root.join("reply.json");
    std::fs::write(
        &reply,
        serde_json::to_vec(&serde_json::json!({"rows":rows})).unwrap(),
    )
    .unwrap();
    // Windows may only have the Store stub as `python`; probe for an interpreter that runs.
    let Some(python) = [&["python3"][..], &["python"], &["py", "-3"]]
        .into_iter()
        .find(|cmd| {
            Command::new(cmd[0])
                .args(&cmd[1..])
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
    else {
        eprintln!("skipping: no working Python interpreter (python3, python or py -3)");
        std::fs::remove_dir_all(root).unwrap();
        return;
    };
    let output = Command::new(python[0])
        .args(&python[1..])
        .arg(repository.join("tests/golden/count_paths_q2_compare.py"))
        .arg("--lume-bin")
        .arg(env!("CARGO_BIN_EXE_lume"))
        .arg("--store")
        .arg(&store_root)
        .arg("--reply")
        .arg(&reply)
        .current_dir(&repository)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("\"primary_vessel_windows\": 3"), "{text}");
    assert!(text.contains("\"oracle_windows\": 3"), "{text}");
    std::fs::remove_dir_all(root).unwrap();
}
