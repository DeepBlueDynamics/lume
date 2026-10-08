#![cfg(feature = "ti")]
//! Same-process document range A/B on an owned fixture; native release is the timing gate.
//! Opening legacy document persistence can migrate it, so never use a shared fixture.
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};
use ti_sql::datafusion::{
    self,
    physical_plan::{collect, displayable},
};

fn canonical(batches: &[datafusion::arrow::record_batch::RecordBatch]) -> Vec<String> {
    let mut rows = ti_sql::rows_json(batches)
        .unwrap()
        .into_iter()
        .map(|row| serde_json::to_string(&row).unwrap())
        .collect::<Vec<_>>();
    rows.sort();
    rows
}
fn equivalent_rows(a: &[String], b: &[String], id: &str) -> bool {
    if a == b {
        return true;
    }
    if id != "qx-012" {
        return false;
    }
    fn rounded(value: Value) -> Value {
        match value {
            Value::Number(n) if n.is_f64() => {
                let value = format!("{:.11e}", n.as_f64().unwrap())
                    .parse::<f64>()
                    .unwrap();
                json!(value)
            }
            Value::Array(items) => Value::Array(items.into_iter().map(rounded).collect()),
            Value::Object(items) => {
                Value::Object(items.into_iter().map(|(k, v)| (k, rounded(v))).collect())
            }
            other => other,
        }
    }
    let normalize = |rows: &[String]| {
        let mut rows = rows
            .iter()
            .map(|row| serde_json::to_string(&rounded(serde_json::from_str(row).unwrap())).unwrap())
            .collect::<Vec<_>>();
        rows.sort();
        rows
    };
    normalize(a) == normalize(b)
}
async fn measure(session: &ti_sql::SqlSession, sql: &str) -> (Value, Vec<String>) {
    session.reset_diagnostics().unwrap();
    let start = Instant::now();
    let frame = session.prepare(sql).await.unwrap();
    let logical_ms = start.elapsed().as_secs_f64() * 1000.0;
    let phase = Instant::now();
    let plan = frame.create_physical_plan().await.unwrap();
    let physical_ms = phase.elapsed().as_secs_f64() * 1000.0;
    let phase = Instant::now();
    let batches = collect(
        plan.clone(),
        Arc::new(datafusion::execution::TaskContext::default()),
    )
    .await
    .unwrap();
    let execute_ms = phase.elapsed().as_secs_f64() * 1000.0;
    let total_ms = start.elapsed().as_secs_f64() * 1000.0;
    let reports = session.reports().unwrap();
    (
        json!({"logical_ms":logical_ms,"physical_ms":physical_ms,"execute_ms":execute_ms,"total_ms":total_ms,
        "materialized_rows":reports.iter().map(|s|s.materialized_rows).sum::<u64>(),
        "plan":displayable(plan.as_ref()).indent(true).to_string()}),
        canonical(&batches),
    )
}
fn percentile(samples: &[Value], fraction: f64) -> f64 {
    let mut values = samples
        .iter()
        .map(|s| s["total_ms"].as_f64().unwrap())
        .collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    values[((values.len() as f64 * fraction).ceil() as usize).saturating_sub(1)]
}
#[test]
#[ignore = "requires an owned copy of the Pi benchmark store"]
fn q6_document_range_join_profile() {
    let root = PathBuf::from(std::env::var_os("TI_Q6_STORE").expect("TI_Q6_STORE"));
    let output = PathBuf::from(std::env::var_os("TI_Q6_OUTPUT").expect("TI_Q6_OUTPUT"));
    assert!(!output.exists(), "use a fresh output filename");
    let runs = std::env::var("TI_Q6_RUNS")
        .unwrap_or_else(|_| "7".into())
        .parse::<usize>()
        .unwrap();
    assert!((1..=100).contains(&runs));
    let all = std::env::var("TI_Q6_ALL").as_deref() == Ok("1");
    let selected = std::env::var("TI_Q6_IDS").ok().map(|ids| {
        ids.split(',')
            .map(str::trim)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    let corpus: Value = serde_json::from_str(include_str!("golden/corpus.json")).unwrap();
    ti_sql::surface_runtime().unwrap().block_on(async {
        let saved=Arc::new(Mutex::new(None));
        let captured=saved.clone();
        let documents=move |path:&std::path::Path,store:&ti_store::Store,width:u64| {
            let index=Arc::new(lume::ti_text::LumeText::open(path,store.catalog().clone(),width)?)
                as Arc<dyn ti_contracts::DocumentIndex>;
            *captured.lock().unwrap()=Some(index.clone());
            Ok(index)
        };
        let engine=ti_sql::TiEngine::open(&root,None,Some(&documents)).await.unwrap();
        let baseline=ti_sql::SqlSession::new_with_document_range_pruning(
            engine.session.source.clone(),engine.session.catalog.clone(),false).await.unwrap();
        baseline.register_documents(saved.lock().unwrap().take().unwrap()).unwrap();
        let cache=engine.query_cache_control().unwrap();
        cache.set_budget(256*1024*1024).unwrap();
        let mut results=vec![];
        for query in corpus["entries"].as_array().unwrap() {
            let id=query["id"].as_str().unwrap();
            if let Some(ids) = &selected {
                if !ids.iter().any(|selected| selected == id) { continue; }
            } else if if all { !ti_bench::harness::PI_QUERY_IDS.contains(&id) } else {id!="q6-004"} {continue;}
            let sql=query["ti_sql"].as_str().unwrap();
            let mut expected=None;
            let mut exact_rows_match=true;
            let mut variants=vec![];
            for (label,session) in [("before",&baseline),("after",&engine.session)] {
                cache.clear().unwrap();
                let (cold,answer)=measure(session,sql).await;
                if let Some(ref prior)=expected { exact_rows_match &= &answer == prior; assert!(equivalent_rows(&answer,prior,id),"{id} A/B mismatch"); }
                else {expected=Some(answer.clone());}
                let mut samples=vec![];
                for _ in 0..runs {
                    let (sample,rows)=measure(session,sql).await;
                    exact_rows_match &= rows == answer;
                    assert!(equivalent_rows(&rows,&answer,id),"{id} warm mismatch");
                    samples.push(sample);
                }
                let p50=percentile(&samples,0.5);
                let p95=percentile(&samples,0.95);
                println!("{id} {label}: p50 {p50:.3} ms, p95 {p95:.3} ms, {} rows, materialized {}",answer.len(),samples[0]["materialized_rows"]);
                variants.push(json!({"label":label,"cold":cold,"samples":samples,"p50_ms":p50,"p95_ms":p95,
                    "rows":answer.len(),"cache":cache.stats().unwrap()}));
            }
            results.push(json!({"id":id,"sql":sql,"values_match":true,"exact_rows_match":exact_rows_match,"comparison":if id=="qx-012" {"12 significant digits for avg() float last-bit noise"} else {"exact canonical rows"},"answer":expected,"variants":variants}));
        }
        assert_eq!(results.len(),selected.as_ref().map(Vec::len).unwrap_or(if all {26} else {1}));
        let report=json!({"store":root,"runs":runs,"queries":results,
            "cache_bytes":256*1024*1024,"cold_definition":"decoded cache cleared; OS cache uncontrolled",
            "profile":if cfg!(debug_assertions) {"debug"} else {"release"}});
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::write(output,serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    });
}
