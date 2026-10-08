//! Native query-class benchmark with the same lexical document index as lume serve.
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("benchmark error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::{path::Path, sync::Arc};
    use ti_bench::harness::{run_benchmark_with_documents, BenchmarkOptions};

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("bench") {
        return Err("usage: ti_query_bench bench --store ROOT [--corpus FILE --out-dir DIR --iterations N --cache-bytes N --class Q6]".into());
    }
    let value = |flag: &str| {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let store = value("--store").ok_or("--store is required")?;
    let parquet = value("--parquet").unwrap_or_else(|| store.clone());
    let corpus = value("--corpus").unwrap_or_else(|| "tests/golden/corpus.json".into());
    let output = value("--out-dir").unwrap_or_else(|| "bench/results".into());
    let iterations = value("--iterations")
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(7);
    let cache = value("--cache-bytes")
        .map(|value| value.parse::<u64>())
        .transpose()?;
    let class = value("--class");
    let documents = |root: &Path, store: &ti_store::Store, width: u64| {
        Ok(Arc::new(lume::ti_text::LumeText::open(
            root,
            store.catalog().clone(),
            width,
        )?) as Arc<dyn ti_contracts::DocumentIndex>)
    };
    run_benchmark_with_documents(
        BenchmarkOptions {
            store_dir: &store,
            parquet_dir: &parquet,
            corpus_file: &corpus,
            out_dir: &output,
            iterations,
            class_filter: class.as_deref(),
            cache_budget_bytes: cache,
        },
        Some(&documents),
    )
    .await?;
    Ok(())
}
