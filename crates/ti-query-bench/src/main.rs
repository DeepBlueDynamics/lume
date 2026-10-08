//! Native query-class benchmark with the same lexical document index as lume serve.
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("benchmark error: {error}");
        std::process::exit(if error.to_string().starts_with("EMPTY_RESULT:") {
            2
        } else {
            1
        });
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::{path::Path, sync::Arc};
    use ti_bench::harness::{run_benchmark_with_documents, BenchmarkOptions};

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("contention") {
        return ti_bench::contention::run(&args[2..]).map_err(Into::into);
    }
    let warm_only = args.get(1).map(String::as_str) == Some("warm");
    if !warm_only && args.get(1).map(String::as_str) != Some("bench") {
        return Err("usage: ti_query_bench bench --store ROOT [--corpus FILE --out-dir DIR --iterations N --cache-bytes N --class Q6 --sha SHA --pi]".into());
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
    let sha = value("--sha");
    let pi = args.iter().any(|arg| arg == "--pi");
    if pi && class.is_some() {
        return Err("--pi runs all 26 queries; do not pass --class".into());
    }
    let documents = |root: &Path, store: &ti_store::Store, width: u64| {
        Ok(Arc::new(lume::ti_text::LumeText::open(
            root,
            store.catalog().clone(),
            width,
        )?) as Arc<dyn ti_contracts::DocumentIndex>)
    };
    if warm_only {
        let engine = ti_sql::TiEngine::open(Path::new(&store), None, Some(&documents)).await?;
        let control = engine.query_cache_control()?;
        if let Some(bytes) = cache {
            control.set_budget(bytes)?;
        }
        control.clear()?;
        println!("WARM_BEGIN");
        std::io::Write::flush(&mut std::io::stdout())?;
        let report = control.warm_configured(Path::new(&store))?;
        println!("WARM_REPORT {}", serde_json::to_string(&report)?);
        std::io::Write::flush(&mut std::io::stdout())?;
        if args.iter().any(|arg| arg == "--hold-for-rss") {
            let mut release = String::new();
            std::io::stdin().read_line(&mut release)?;
        }
        return Ok(());
    }
    run_benchmark_with_documents(
        BenchmarkOptions {
            store_dir: &store,
            parquet_dir: &parquet,
            corpus_file: &corpus,
            out_dir: &output,
            iterations,
            class_filter: class.as_deref(),
            cache_budget_bytes: cache,
            warm_before_cold: args.iter().any(|arg| arg == "--warm-before-cold"),
            result_label: if pi { Some("pi") } else { None },
            sha_override: sha.as_deref(),
            query_allow: if pi {
                Some(ti_bench::harness::PI_QUERY_IDS)
            } else {
                None
            },
        },
        Some(&documents),
    )
    .await?;
    Ok(())
}
