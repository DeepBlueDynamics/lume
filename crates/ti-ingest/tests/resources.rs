use serde_json::json;
use ti_contracts::Document;
use ti_ingest::resources::{DocumentPoller, ResourceClient, ResourceDocuments};
use ti_store::DocStore;
mod support {
    pub mod resources;
}
use support::resources::MockSignalK;
const URN: &str = "vessels.urn:mrn:signalk:uuid:resources";
#[test]
fn notes_create_update_delete_restart_and_failure_preserve_unowned_docs() {
    let server = MockSignalK::new();
    let root = tempfile::tempdir().unwrap();
    let client = ResourceClient::new(&server.url, Some("test-token".into())).unwrap();
    let mut docs = ResourceDocuments::open(root.path()).unwrap();
    DocStore::open(root.path())
        .unwrap()
        .upsert_all([Document {
            id: "manual".into(),
            vessel: URN.into(),
            kind: "notes".into(),
            ts_start: 1780000000,
            ts_end: None,
            title: "Imported".into(),
            body: "keep me".into(),
        }])
        .unwrap();
    *server.notes.lock().unwrap() = (
        200,
        json!({"note":{"timestamp":"2026-10-06T12:00:00Z","title":"Reef","description":"reef marker","position":{"latitude":50,"longitude":4}}}),
    );
    docs.apply(client.notes().unwrap().unwrap(), URN, 1780000000)
        .unwrap();
    let first = DocStore::open(root.path()).unwrap();
    let note = first.iter().find(|d| d.id == "note").unwrap();
    assert_eq!(note.kind, "notes");
    assert_eq!(note.ts_end, None);
    assert!(note.body.contains("position"));
    let bytes = std::fs::read(root.path().join("docs/documents.log")).unwrap();
    docs.apply(client.notes().unwrap().unwrap(), URN, 1780000100)
        .unwrap();
    assert_eq!(
        DocStore::open(root.path())
            .unwrap()
            .iter()
            .find(|d| d.id == "note"),
        Some(note)
    );
    assert_eq!(
        std::fs::read(root.path().join("docs/documents.log")).unwrap(),
        bytes
    );
    *server.notes.lock().unwrap() = (
        200,
        json!({"note":{"title":"Updated","text":"anchorage","timeRange":{"start":"2026-10-06T12:00:00Z","end":"2026-10-06T12:02:00Z"}}}),
    );
    docs.apply(client.notes().unwrap().unwrap(), URN, 1780000200)
        .unwrap();
    assert!(DocStore::open(root.path())
        .unwrap()
        .iter()
        .find(|d| d.id == "note")
        .unwrap()
        .body
        .contains("anchorage"));
    *server.notes.lock().unwrap() = (500, json!({}));
    assert!(client.notes().is_err());
    assert_eq!(DocStore::open(root.path()).unwrap().len(), 2);
    *server.notes.lock().unwrap() = (200, json!({"note":{"timestamp":"invalid"}}));
    assert!(docs
        .apply(client.notes().unwrap().unwrap(), URN, 1780000200)
        .is_err());
    assert_eq!(DocStore::open(root.path()).unwrap().len(), 2);
    *server.notes.lock().unwrap() = (200, json!({"ancient":{"timestamp":"2019-12-31T12:00:00Z"}}));
    assert!(docs
        .apply(client.notes().unwrap().unwrap(), URN, 1780000200)
        .is_err());
    assert_eq!(docs.rejected_pre_epoch, 1);
    assert_eq!(DocStore::open(root.path()).unwrap().len(), 2);
    *server.notes.lock().unwrap() = (200, json!({}));
    ResourceDocuments::open(root.path())
        .unwrap()
        .apply(client.notes().unwrap().unwrap(), URN, 1780000300)
        .unwrap();
    assert_eq!(
        DocStore::open(root.path())
            .unwrap()
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>(),
        ["manual"]
    );
    assert!(server.requests.lock().unwrap().iter().all(|r| r
        .to_lowercase()
        .contains("authorization: bearer test-token")));
}
#[test]
fn missing_timestamp_is_stable_and_logbook_is_optional() {
    let server = MockSignalK::new();
    let root = tempfile::tempdir().unwrap();
    let client = ResourceClient::new(&server.url, None).unwrap();
    let mut docs = ResourceDocuments::open(root.path()).unwrap();
    *server.notes.lock().unwrap() = (200, json!({"note":{"text":"untimed"}}));
    docs.apply(client.notes().unwrap().unwrap(), URN, 1780000000)
        .unwrap();
    docs.apply(client.notes().unwrap().unwrap(), URN, 1780001000)
        .unwrap();
    assert_eq!(
        DocStore::open(root.path())
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .ts_start,
        1780000000
    );
    assert!(client.logbook().unwrap().is_none());
    *server.logbook.lock().unwrap() = (
        200,
        json!({"entry":{"datetime":"2026-10-06T12:00:00Z","text":"reef mainsail","telemetry":[{"path":"navigation.position","value":{"latitude":50,"longitude":4}}]}}),
    );
    docs.apply(client.logbook().unwrap().unwrap(), URN, 1780001000)
        .unwrap();
    assert_eq!(
        DocStore::open(root.path())
            .unwrap()
            .iter()
            .find(|d| d.kind == "logbook")
            .unwrap()
            .id,
        "logbook/entry"
    );
    *server.logbook.lock().unwrap() = (200, json!({}));
    docs.apply(client.logbook().unwrap().unwrap(), URN, 1780001000)
        .unwrap();
    assert_eq!(DocStore::open(root.path()).unwrap().len(), 1);
    *server.notes.lock().unwrap() = (
        200,
        json!({"pinned":{"title":"Pin","properties":{
            "group":"lume-ti","x-lume-ti":{"start":"2026-10-06T12:00:00Z","end":"2026-10-06T12:02:00Z"}
        }}}),
    );
    docs.apply(client.notes().unwrap().unwrap(), URN, 1780001000)
        .unwrap();
    assert_eq!(
        DocStore::open(root.path())
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .ts_end,
        Some(1791288120)
    );
    let mut service = ti_ingest::service::IngestService::new(
        ti_contracts::TiConfig::default(),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    service.documents_rejected_pre_epoch = 3;
    service.write_status(root.path(), false);
    let status: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("ingest_status.json")).unwrap())
            .unwrap();
    assert_eq!(status["documents_rejected_pre_epoch"], 3);
    assert!(server
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| !r.to_lowercase().contains("authorization:")));
}
#[test]
fn legacy_logbook_requires_a_complete_snapshot_before_deleting() {
    let server = MockSignalK::new();
    let root = tempfile::tempdir().unwrap();
    let client = ResourceClient::new(&server.url, None).unwrap();
    let mut docs = ResourceDocuments::open(root.path()).unwrap();
    *server.logbook.lock().unwrap() = (
        404,
        json!({"legacy":{
            "/plugins/signalk-logbook/logs":["2026-10-06"],
            "/plugins/signalk-logbook/logs/2026-10-06":[{"datetime":"2026-10-06T12:00:00Z","text":"legacy reef","end":true}]
        }}),
    );
    docs.apply(client.logbook().unwrap().unwrap(), URN, 1780000000)
        .unwrap();
    let stored = DocStore::open(root.path()).unwrap();
    let entry = stored.iter().next().unwrap();
    assert_eq!(entry.id, "logbook/2026-10-06/2026-10-06T12:00:00Z");
    assert_eq!(entry.ts_end, None);
    *server.logbook.lock().unwrap() = (
        404,
        json!({"legacy":{
            "/plugins/signalk-logbook/logs":["2026-10-06","2026-10-07"],
            "/plugins/signalk-logbook/logs/2026-10-06":[]
        }}),
    );
    assert!(client.logbook().is_err());
    assert_eq!(DocStore::open(root.path()).unwrap().len(), 1);
    *server.logbook.lock().unwrap() = (
        404,
        json!({"legacy":{
            "/plugins/signalk-logbook/logs":[]
        }}),
    );
    docs.apply(client.logbook().unwrap().unwrap(), URN, 1780000000)
        .unwrap();
    assert!(DocStore::open(root.path()).unwrap().is_empty());
}
#[test]
fn worker_fetches_without_touching_document_store() {
    let server = MockSignalK::new();
    let poller = DocumentPoller::start(ResourceClient::new(&server.url, None).unwrap()).unwrap();
    let snapshot = poller
        .receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    assert_eq!(snapshot.kind, "notes");
}

#[test]
fn optional_missing_sources_are_throttled_and_working_notes_keep_polling() {
    let server = MockSignalK::new();
    let client = ResourceClient::new(&server.url, None).unwrap();
    for status in [404, 401] {
        *server.logbook.lock().unwrap() = (status, json!({}));
        for _ in 0..3 {
            assert!(client.logbook().unwrap().is_none());
            assert!(client.notes().unwrap().is_some());
        }
    }
    assert_eq!(
        client.take_notices(),
        vec!["Signal K document source not present: logbook"]
    );
    *server.notes.lock().unwrap() = (401, json!({}));
    assert!(client.notes().unwrap().is_none());
    assert_eq!(
        client.take_notices(),
        vec!["Signal K document source not present: notes"]
    );
    let authenticated = ResourceClient::new(&server.url, Some("test-token".into())).unwrap();
    assert!(authenticated.notes().is_err());
    assert!(authenticated.take_notices().is_empty());
    *server.notes.lock().unwrap() = (200, json!({"new":{"text":"still polling"}}));
    assert_eq!(client.notes().unwrap().unwrap().entries.len(), 1);
    assert!(client.take_notices().is_empty());
}
