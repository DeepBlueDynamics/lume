//! Same-process D52 append+fsync versus the former JSON snapshot strategy.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};
use ti_contracts::Document;
use ti_store::DocStore;
#[derive(Serialize, Deserialize)]
struct Stored {
    id: String,
    vessel: String,
    kind: String,
    ts_start: i64,
    ts_end: Option<i64>,
    title: String,
    body: String,
}
impl From<&Document> for Stored {
    fn from(d: &Document) -> Self {
        Self {
            id: d.id.clone(),
            vessel: d.vessel.clone(),
            kind: d.kind.clone(),
            ts_start: d.ts_start,
            ts_end: d.ts_end,
            title: d.title.clone(),
            body: d.body.clone(),
        }
    }
}
impl From<Stored> for Document {
    fn from(d: Stored) -> Self {
        Self {
            id: d.id,
            vessel: d.vessel,
            kind: d.kind,
            ts_start: d.ts_start,
            ts_end: d.ts_end,
            title: d.title,
            body: d.body,
        }
    }
}
fn document(id: usize, revision: usize) -> Document {
    Document {
        id: format!("doc-{id:06}"),
        vessel: "agent.urn:benchmark".into(),
        kind: "logbook".into(),
        ts_start: 1_780_000_000,
        ts_end: None,
        title: "Benchmark".into(),
        body: format!("{revision:06} {}", "x".repeat(128)),
    }
}
fn write_snapshot(path: &Path, docs: &BTreeMap<(String, String), Document>) -> u64 {
    let stored = docs.values().map(Stored::from).collect::<Vec<_>>();
    let bytes = serde_json::to_vec_pretty(&stored).unwrap();
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary).unwrap();
    file.write_all(&bytes).unwrap();
    file.flush().unwrap();
    file.sync_all().unwrap();
    drop(file);
    fs::rename(temporary, path).unwrap();
    #[cfg(unix)]
    File::open(path.parent().unwrap())
        .unwrap()
        .sync_all()
        .unwrap();
    bytes.len() as u64
}
fn old_upsert(path: &Path, document: Document) -> u64 {
    // Former commit invalidated its stamp, forcing a full reopen/validation on
    // the next write, followed by a sorted whole-set JSON publication.
    let stored: Vec<Stored> = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let mut docs = BTreeMap::new();
    for document in stored.into_iter().map(Document::from) {
        document.validate().unwrap();
        docs.insert((document.vessel.clone(), document.id.clone()), document);
    }
    document.validate().unwrap();
    docs.insert((document.vessel.clone(), document.id.clone()), document);
    write_snapshot(path, &docs)
}
fn quantile(values: &[f64], fraction: f64) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * fraction).ceil() as usize]
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let value = |flag| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|p| args.get(p + 1))
    };
    let iterations = value("--iterations")
        .map(|v| v.parse::<usize>().unwrap())
        .unwrap_or(20);
    assert!(iterations > 0);
    let base = value("--dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".test-tmp"));
    let root = base.join(format!("docstore-bench-{}", std::process::id()));
    fs::create_dir_all(&base).unwrap();
    fs::create_dir(&root).unwrap();
    let mut results = Vec::new();
    println!("| Live docs | JSON p50 ms | JSON p95 ms | Append p50 ms | Append p95 ms | JSON bytes/update | Append bytes/update |");
    println!("|---:|---:|---:|---:|---:|---:|---:|");
    for count in [1_000, 10_000, 50_000] {
        let old_root = root.join(format!("old-{count}"));
        let new_root = root.join(format!("new-{count}"));
        fs::create_dir_all(&old_root).unwrap();
        let path = old_root.join("documents.json");
        let initial = (0..count).map(|id| document(id, 0)).collect::<Vec<_>>();
        let map = initial
            .iter()
            .map(|d| ((d.vessel.clone(), d.id.clone()), d.clone()))
            .collect();
        write_snapshot(&path, &map);
        let mut store = DocStore::open(&new_root).unwrap();
        store.upsert_all(initial).unwrap();
        // Warm both paths without reusing unchanged documents.
        old_upsert(&path, document(0, 1));
        store.upsert_all([document(0, 1)]).unwrap();
        let mut old = Vec::new();
        let mut new = Vec::new();
        let mut old_bytes = 0;
        let mut new_bytes = 0;
        // Isolate the timing blocks: a preceding multi-MiB legacy rewrite
        // must not contaminate the next tiny append's filesystem latency.
        for revision in 2..iterations + 2 {
            let before = fs::metadata(new_root.join("docs/documents.log"))
                .unwrap()
                .len();
            let start = Instant::now();
            store.upsert_all([document(0, revision)]).unwrap();
            new.push(start.elapsed().as_secs_f64() * 1000.);
            let after = fs::metadata(new_root.join("docs/documents.log"))
                .unwrap()
                .len();
            new_bytes += after - before;
        }
        for revision in 2..iterations + 2 {
            let start = Instant::now();
            old_bytes += old_upsert(&path, document(0, revision));
            old.push(start.elapsed().as_secs_f64() * 1000.);
        }
        let (old50, old95, new50, new95) = (
            quantile(&old, 0.5),
            quantile(&old, 0.95),
            quantile(&new, 0.5),
            quantile(&new, 0.95),
        );
        println!(
            "| {count} | {old50:.3} | {old95:.3} | {new50:.3} | {new95:.3} | {} | {} |",
            old_bytes / iterations as u64,
            new_bytes / iterations as u64
        );
        results.push(
            serde_json::json!({"documents":count,"iterations":iterations,
            "old_p50_ms":old50,"old_p95_ms":old95,"append_p50_ms":new50,"append_p95_ms":new95,
            "old_bytes_per_update":old_bytes/iterations as u64,
            "append_bytes_per_update":new_bytes/iterations as u64}),
        );
    }
    // Amortized workload: enough overwrites to trigger repeated compactions.
    let compact_root = root.join("compaction");
    let mut compact = DocStore::open(&compact_root).unwrap();
    compact
        .upsert_all((0..1000).map(|id| document(id, 0)))
        .unwrap();
    let mut times = Vec::new();
    let mut events = 0;
    for revision in 1..=2100 {
        let before = fs::metadata(compact_root.join("docs/documents.log"))
            .unwrap()
            .len();
        let start = Instant::now();
        compact
            .upsert_all([document(revision % 1000, revision)])
            .unwrap();
        times.push(start.elapsed().as_secs_f64() * 1000.);
        if fs::metadata(compact_root.join("docs/documents.log"))
            .unwrap()
            .len()
            < before
        {
            events += 1;
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
        "profile":if cfg!(debug_assertions) { "debug" } else { "release" },
        "body_bytes":135,"results":results,"compaction":{
            "live_docs":1000,"updates":2100,"events":events,
            "p50_ms":quantile(&times,0.5),"p95_ms":quantile(&times,0.95),
            "amortized_mean_ms":times.iter().sum::<f64>()/times.len() as f64,
            "maximum_ms":times.iter().copied().fold(0.,f64::max)}}))
        .unwrap()
    );
    drop(compact);
    fs::remove_dir_all(root).unwrap();
}
