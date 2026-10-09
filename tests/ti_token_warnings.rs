#![cfg(feature = "ti")]

#[test]
fn raw_tokens_warn_without_echoing_secrets() {
    for command in ["ingest", "sync"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_lume"))
            .args(["ti", command, "--token", "private-test-token"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("Deprecated: raw"), "{stderr}");
        assert!(stderr.contains("token-file"), "{stderr}");
        assert!(!stderr.contains("private-test-token"));
        assert!(!String::from_utf8(output.stdout)
            .unwrap()
            .contains("private-test-token"));
    }
}

#[test]
fn ingest_token_file_does_not_warn() {
    let file = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("token-warning-{}", std::process::id()));
    std::fs::write(&file, "private-file-token").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lume"))
        .args(["ti", "ingest", "--token"])
        .arg(&file)
        .output()
        .unwrap();
    std::fs::remove_file(file).unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("Deprecated: raw"));
    assert!(!stderr.contains("private-file-token"));
}
