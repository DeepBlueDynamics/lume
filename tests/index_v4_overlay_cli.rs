use lume::bm25::{Bm25Index, Section};
use lume::search::{LoadedIndex, OpenEnvChecks, SearchMode, SearchOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Stub {
    url: String,
    calls: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Stub {
    fn new(pause: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(!pause));
        let stop = Arc::new(AtomicBool::new(false));
        let (c, r, s) = (calls.clone(), release.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut bytes = Vec::new();
                let (offset, length) = loop {
                    let mut chunk = [0u8; 4096];
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(offset) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = std::str::from_utf8(&bytes[..offset]).unwrap();
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= offset + 4 + length {
                            break (offset + 4, length);
                        }
                    }
                };
                let request: serde_json::Value =
                    serde_json::from_slice(&bytes[offset..offset + length]).unwrap();
                let body = request["messages"][1]["content"].as_str().unwrap();
                let entities: Vec<String> = if body.contains("empty result") {
                    Vec::new()
                } else {
                    vec!["Bilge Pump".into(), "Lagoon".into()]
                };
                let call = c.fetch_add(1, Ordering::SeqCst) + 1;
                while call == 17 && !r.load(Ordering::SeqCst) && !s.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let reply = serde_json::json!({"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"extract_entities","arguments":{"entities":entities}}}]}}).to_string();
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply);
            }
        });
        Self {
            url,
            calls,
            release,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.release.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct Run(Child);
impl Drop for Run {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn cmd(source: &std::path::Path, db: &std::path::Path, stub: &Stub) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lume"));
    cmd.args(["index", "-s", "-o"])
        .arg(source)
        .arg("--db")
        .arg(db)
        .arg("--ollama-url")
        .arg(&stub.url)
        .env("LUME_INDEX_FORMAT", "4")
        .env("LUME_EXTRACT_WORKERS", "1")
        .env("LUME_STEM", "1")
        .env_remove("NUTS_SERVICES_TOKEN")
        .env_remove("LUME_EMBED_MODEL")
        .env_remove("LUME_EMBED_DIMENSIONS")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}
fn wait(mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "acceptance timeout"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn cli_overlays_are_searchable_mid_run_resume_and_compact_exactly() {
    let root = std::env::temp_dir().join(format!("lume-overlay-cli-{}", lume::uuid_v4()));
    let source = root.join("docs");
    let db = root.join("index");
    std::fs::create_dir_all(&source).unwrap();
    for i in 0..32 {
        let text = if i % 7 == 0 {
            "empty result water"
        } else {
            "Bilge pump drains Lagoon water"
        };
        std::fs::write(source.join(format!("{i:02}.txt")), format!("{text}\n")).unwrap();
    }
    let stub = Stub::new(true);
    let mut first = Run(cmd(&source, &db, &stub).spawn().unwrap());
    wait(|| stub.calls.load(Ordering::SeqCst) == 17);
    wait(|| {
        lume::index_binary::generation::read_manifest(&db)
            .ok()
            .and_then(|m| m.entity_overlay)
            .is_some_and(|h| h.sequence == 1)
    });
    let mid = LoadedIndex::open_with_checks(&db, OpenEnvChecks::default()).unwrap();
    assert_eq!(
        mid.bm25
            .sections
            .iter()
            .filter(|s| !s.entities.is_empty())
            .count(),
        16
    );
    assert_eq!(mid.bm25.entity_posting_lists["bilge pump"].len(), 13);
    assert!(!lume::search::search(
        &mid,
        "bilge pump",
        &SearchOptions {
            mode: SearchMode::LexicalOnly,
            ..Default::default()
        }
    )
    .unwrap()
    .is_empty());
    println!("mid-run search: 16 processed sections, 13 entity matches");
    first.0.kill().unwrap();
    first.0.wait().unwrap();
    let sealed = lume::index_binary::generation::read_manifest(&db).unwrap();
    assert!(LoadedIndex::open_with_checks(&db, OpenEnvChecks::default()).is_ok());
    stub.release.store(true, Ordering::SeqCst);
    let status = cmd(&source, &db, &stub).status().unwrap();
    assert!(status.success());
    assert_eq!(
        stub.calls.load(Ordering::SeqCst),
        33,
        "resume re-extracted a sealed result"
    );
    let final_index = LoadedIndex::open_with_checks(&db, OpenEnvChecks::default()).unwrap();
    let final_manifest = lume::index_binary::generation::read_manifest(&db).unwrap();
    assert_ne!(sealed.generation, final_manifest.generation);
    assert!(final_manifest.entity_overlay.is_none());
    assert!(final_index
        .bm25
        .sections
        .iter()
        .all(|s| !s.entities.is_empty()));
    assert!(final_index
        .bm25
        .sections
        .iter()
        .all(|s| s.entities.len() <= 2));
    let mut expected: Vec<Section> = mid.bm25.sections.clone();
    for section in &mut expected {
        section.entities = if section.body.contains("empty result") {
            vec!["__LUME_PROCESSED__".into()]
        } else {
            vec!["Bilge Pump".into(), "Lagoon".into()]
        };
    }
    let bm25 = Bm25Index::build(expected, None);
    let graph = lume::semantic_mesh::EntityGraph::build(
        &bm25.entity_posting_lists,
        &bm25.entity_kinds,
        &bm25.entity_labels,
        0.1,
        bm25.sections.len(),
    );
    let mut one_shot = LoadedIndex::from_parts(bm25);
    one_shot.entity_graph = Some(graph);
    for query in ["bilge pump", "water", "Lagoon"] {
        let options = SearchOptions {
            mode: SearchMode::LexicalOnly,
            ..Default::default()
        };
        let actual = lume::search::search(&final_index, query, &options).unwrap();
        let expected = lume::search::search(&one_shot, query, &options).unwrap();
        assert_eq!(
            actual
                .hits
                .iter()
                .map(|hit| (
                    hit.section_index,
                    hit.score.to_bits(),
                    hit.bm25_score.to_bits()
                ))
                .collect::<Vec<_>>(),
            expected
                .hits
                .iter()
                .map(|hit| (
                    hit.section_index,
                    hit.score.to_bits(),
                    hit.bm25_score.to_bits()
                ))
                .collect::<Vec<_>>(),
            "exact score bits"
        );
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap(),
            "final score/ranking parity for {query}"
        );
    }
    println!("kill/resume: exactly 16 remaining requests; final one-shot score parity");
    std::fs::remove_dir_all(root).unwrap();
}
