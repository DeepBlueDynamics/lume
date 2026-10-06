use std::path::Path;
use ti_sql::{diff_rows, open_fixture, verify, Tolerance};
#[tokio::test]
async fn stored_expected_fixture_and_explicit_m4_exclusions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture");
    let session = open_fixture(&root.join("snapshot.json")).await.unwrap();
    let report = verify(&session, &root, None, None).await.unwrap();
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!(report.passed, 6);
    assert_eq!(report.excluded, 4);
    assert!(report
        .entries
        .iter()
        .filter(|e| e.status == "excluded")
        .all(|e| e.reason.is_some()));
}
#[test]
fn comparator_requires_exact_keys_and_half_unit_tolerance() {
    let tolerance: Tolerance =
        serde_json::from_str(r#"{"sort":["ts"],"exact":["ts","n"],"bsi":{"x":2}}"#).unwrap();
    let actual =
        serde_json::from_str(r#"[{"ts":"2020-01-01T00:00:00Z","n":1,"x":1.254}]"#).unwrap();
    let expected =
        serde_json::from_str(r#"[{"ts":"2020-01-01 00:00:00","n":1,"x":1.25}]"#).unwrap();
    diff_rows(actual, expected, &tolerance).unwrap();
    let actual =
        serde_json::from_str(r#"[{"ts":"2020-01-01T00:00:00Z","n":1,"x":1.256}]"#).unwrap();
    let expected =
        serde_json::from_str(r#"[{"ts":"2020-01-01 00:00:00","n":1,"x":1.25}]"#).unwrap();
    assert!(diff_rows(actual, expected, &tolerance).is_err());
}
