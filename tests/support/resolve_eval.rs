#![cfg(feature = "ti")]
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
};
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
/// Opt-in real-store gate. Never writes the store; artifacts go to the caller's output.
#[test]
#[ignore = "requires a boat corpus store and Python; run explicitly for M5 evaluation"]
fn boat_phrases_through_live_mcp() {
    let store = std::env::var_os("TI_RESOLVE_STORE").expect("TI_RESOLVE_STORE");
    let out = PathBuf::from(std::env::var_os("TI_RESOLVE_OUTPUT").expect("TI_RESOLVE_OUTPUT"));
    let label = std::env::var("TI_RESOLVE_LABEL").expect("TI_RESOLVE_LABEL");
    let split = std::env::var("TI_RESOLVE_SPLIT").unwrap_or_else(|_| "all".into());
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_lume"))
            .args(["serve", "--bind", "127.0.0.1", "--port", "0", "--ti-store"])
            .arg(store)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut reader = BufReader::new(server.0.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let url = line.split_whitespace().last().expect("server startup URL");
    assert!(url.starts_with("http://127.0.0.1:"), "{line}");
    let python = std::env::var("PYTHON").unwrap_or_else(|_| "python3".into());
    let mut command = Command::new(python);
    command
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bench/resolve_eval.py"))
        .args([
            "--mcp-url",
            &format!("{url}/mcp"),
            "--label",
            &label,
            "--split",
            &split,
            "--output",
        ])
        .arg(&out);
    if std::env::var("TI_RESOLVE_SHOW_HOLDOUT").as_deref() == Ok("1") {
        command.arg("--show-holdout");
    }
    let status = command.status().unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("results.json")).unwrap()).unwrap();
    let expected = match split.as_str() {
        "all" => 100,
        "development" => 70,
        "holdout" => 30,
        "independent" => 0,
        _ => panic!("invalid split"),
    };
    assert_eq!(report["metrics"]["all"]["total"], expected);
    assert_eq!(report["metrics"]["all"]["errors"], 0);
    if split == "all" || split == "independent" {
        assert_eq!(report["metrics"]["independent"]["total"], 20);
        assert_eq!(report["metrics"]["independent"]["errors"], 0);
    }
    if std::env::var("TI_RESOLVE_REQUIRE_PASS").as_deref() == Ok("1") {
        assert!(
            status.success(),
            "M5 top-3 gate failed: {}",
            report["metrics"]
        );
    } else {
        assert!(status.success() || status.code() == Some(1), "{status}");
    }
}
