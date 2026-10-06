//! Benchmark harness for Lume TI golden corpus queries (Q1–Q8).
//!
//! Measures cold and warm latency (p50, p95, p99), compares against spec/13 targets,
//! and runs DuckDB baseline on the same Parquet tier if DuckDB is available.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use ti_sql::TiEngine;

#[derive(Debug, Deserialize)]
pub struct GoldenCorpus {
    pub version: u32,
    pub bucket_width_seconds: u64,
    pub entries: Vec<CorpusEntry>,
}

#[derive(Debug, Deserialize)]
pub struct CorpusEntry {
    pub id: String,
    #[serde(rename = "class")]
    pub qclass: String,
    pub description: Option<String>,
    pub ti_sql: String,
    pub oracle_sql: Option<String>,
    pub exclude: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryMetric {
    pub id: String,
    pub class: String,
    pub description: String,
    pub rows: usize,
    pub cold_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub mean_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassMetric {
    pub class: String,
    pub target_p95_ms: f64,
    pub target_description: String,
    pub cold_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub meets_target: bool,
    pub duckdb_p95_ms: Option<f64>,
    pub speedup_vs_duckdb: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullReport {
    pub date: String,
    pub git_sha: String,
    pub store_path: String,
    pub parquet_path: String,
    pub iterations: usize,
    pub total_queries_run: usize,
    pub classes: BTreeMap<String, ClassMetric>,
    pub queries: Vec<QueryMetric>,
    pub duckdb_baseline: Option<serde_json::Value>,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (sorted.len() as f64 * p).floor() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn class_target(class: &str) -> (f64, &'static str) {
    match class {
        "Q1" => (20.0, "≤ 20 ms (point lookup)"),
        "Q2" => (
            100.0,
            "≤ 100 ms (shore) / ≤ 150 ms (edge) (selective multi-pred)",
        ),
        "Q3" => (
            60.0,
            "≤ 60 ms (shore) / ≤ 50 ms (edge) (count with filters)",
        ),
        "Q4" => (
            300.0,
            "≤ 300 ms (shore) / ≤ 400 ms (edge) (windowed aggregate)",
        ),
        "Q5" => (150.0, "≤ 150 ms (intervals)"),
        "Q6" => (
            150.0,
            "≤ 150 ms (shore) / ≤ 200 ms (edge) (text + telemetry)",
        ),
        "Q7" => (
            250.0,
            "≤ 250 ms (shore) / ≤ 300 ms (edge) (geo + telemetry)",
        ),
        "Q8" => (500.0, "DuckDB parity ±50 % (broad scan)"),
        _ => (1000.0, "target not defined"),
    }
}

pub async fn run_benchmark(
    store_dir: &str,
    parquet_dir: &str,
    corpus_file: &str,
    out_dir: &str,
    iterations: usize,
    class_filter: Option<&str>,
) -> Result<FullReport, Box<dyn std::error::Error>> {
    eprintln!("=== Lume TI Benchmark Runner ===");
    eprintln!("Store path   : {}", store_dir);
    eprintln!("Parquet path : {}", parquet_dir);
    eprintln!("Corpus path  : {}", corpus_file);
    eprintln!("Iterations   : {}", iterations);

    let corpus_bytes = std::fs::read(corpus_file)?;
    let corpus: GoldenCorpus = serde_json::from_slice(&corpus_bytes)?;

    let git_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();

    eprintln!("Initializing TiEngine on store...");
    let engine = TiEngine::open(Path::new(store_dir), None, None).await?;

    let mut query_metrics = Vec::new();
    let mut class_warm_latencies: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut class_cold_latencies: BTreeMap<String, Vec<f64>> = BTreeMap::new();

    for entry in &corpus.entries {
        if let Some(reason) = &entry.exclude {
            eprintln!(
                "Skipping excluded query {} ({}): {}",
                entry.id, entry.qclass, reason
            );
            continue;
        }
        if let Some(cf) = class_filter {
            if entry.qclass != cf {
                continue;
            }
        }

        eprint!("Running {} ({}) ... ", entry.id, entry.qclass);

        // Cold run
        let t0 = Instant::now();
        let batches = engine.session.query(&entry.ti_sql).await?;
        let cold_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let rows: usize = batches.iter().map(|b| b.num_rows()).sum();

        // Warm runs
        let mut warm_durations = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let t1 = Instant::now();
            let _ = engine.session.query(&entry.ti_sql).await?;
            warm_durations.push(t1.elapsed().as_secs_f64() * 1000.0);
        }

        let mut sorted_warm = warm_durations.clone();
        sorted_warm.sort_by(|a, b| a.total_cmp(b));

        let p50 = percentile(&sorted_warm, 0.50);
        let p95 = percentile(&sorted_warm, 0.95);
        let p99 = percentile(&sorted_warm, 0.99);
        let min = sorted_warm.first().copied().unwrap_or(0.0);
        let max = sorted_warm.last().copied().unwrap_or(0.0);
        let mean = sorted_warm.iter().sum::<f64>() / sorted_warm.len().max(1) as f64;

        eprintln!(
            "cold={:.2}ms, p50={:.2}ms, p95={:.2}ms, rows={}",
            cold_ms, p50, p95, rows
        );

        query_metrics.push(QueryMetric {
            id: entry.id.clone(),
            class: entry.qclass.clone(),
            description: entry.description.clone().unwrap_or_default(),
            rows,
            cold_ms,
            p50_ms: p50,
            p95_ms: p95,
            p99_ms: p99,
            min_ms: min,
            max_ms: max,
            mean_ms: mean,
        });

        class_warm_latencies
            .entry(entry.qclass.clone())
            .or_default()
            .extend(&warm_durations);
        class_cold_latencies
            .entry(entry.qclass.clone())
            .or_default()
            .push(cold_ms);
    }

    // Attempt DuckDB baseline
    let duckdb_report = run_duckdb_baseline(parquet_dir, corpus_file, iterations);

    // Compute class metrics
    let mut class_metrics = BTreeMap::new();
    for (class_name, mut warm_lats) in class_warm_latencies {
        warm_lats.sort_by(|a, b| a.total_cmp(b));
        let cold_lats = class_cold_latencies
            .get(&class_name)
            .cloned()
            .unwrap_or_default();
        let cold_p50 = {
            let mut sc = cold_lats.clone();
            sc.sort_by(|a, b| a.total_cmp(b));
            percentile(&sc, 0.50)
        };
        let p50 = percentile(&warm_lats, 0.50);
        let p95 = percentile(&warm_lats, 0.95);
        let p99 = percentile(&warm_lats, 0.99);

        let (target_ms, target_desc) = class_target(&class_name);
        let meets_target = p95 <= target_ms;

        let duckdb_p95 = duckdb_report
            .as_ref()
            .and_then(|d| d.get("classes"))
            .and_then(|c| c.get(&class_name))
            .and_then(|m| m.get("p95_ms"))
            .and_then(|v| v.as_f64());

        let speedup = duckdb_p95.map(|dd_p95| dd_p95 / p95.max(1e-6));

        class_metrics.insert(
            class_name.clone(),
            ClassMetric {
                class: class_name,
                target_p95_ms: target_ms,
                target_description: target_desc.to_string(),
                cold_ms: cold_p50,
                p50_ms: p50,
                p95_ms: p95,
                p99_ms: p99,
                meets_target,
                duckdb_p95_ms: duckdb_p95,
                speedup_vs_duckdb: speedup,
            },
        );
    }

    let report = FullReport {
        date: date.clone(),
        git_sha: git_sha.clone(),
        store_path: store_dir.to_string(),
        parquet_path: parquet_dir.to_string(),
        iterations,
        total_queries_run: query_metrics.len(),
        classes: class_metrics,
        queries: query_metrics,
        duckdb_baseline: duckdb_report,
    };

    // Output JSON and Markdown
    let out_path = Path::new(out_dir);
    std::fs::create_dir_all(out_path)?;

    let json_filename = format!("{}-{}.json", date, git_sha);
    let md_filename = format!("{}-{}.md", date, git_sha);

    let json_file = out_path.join(&json_filename);
    let md_file = out_path.join(&md_filename);

    std::fs::write(&json_file, serde_json::to_string_pretty(&report)?)?;
    std::fs::write(&md_file, generate_markdown_summary(&report))?;

    eprintln!("\n=== Benchmark Completed ===");
    eprintln!("JSON results written to : {}", json_file.display());
    eprintln!("Markdown summary        : {}", md_file.display());
    println!("{}", generate_markdown_summary(&report));

    Ok(report)
}

fn run_duckdb_baseline(
    parquet_dir: &str,
    corpus_file: &str,
    iterations: usize,
) -> Option<serde_json::Value> {
    let tmp_out = format!("target/duckdb_baseline_tmp_{}.json", std::process::id());
    let script_candidates = [
        "bench/duckdb_baseline.py",
        "crates/ti-bench/bench/duckdb_baseline.py",
        "../../bench/duckdb_baseline.py",
    ];
    let script = script_candidates
        .iter()
        .find(|s| Path::new(s).exists())
        .copied()?;

    // Try runners: py -3 (Windows), python3, python
    let runners = ["py", "python3", "python"];
    for &runner in &runners {
        let mut cmd = Command::new(runner);
        if runner == "py" {
            cmd.arg("-3");
        }
        cmd.args([
            script,
            "--data-dir",
            parquet_dir,
            "--corpus",
            corpus_file,
            "--iterations",
            &iterations.to_string(),
            "--output-json",
            &tmp_out,
        ]);

        if let Ok(status) = cmd.status() {
            if status.success() {
                if let Ok(bytes) = std::fs::read(&tmp_out) {
                    let _ = std::fs::remove_file(&tmp_out);
                    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        eprintln!("DuckDB baseline collected successfully via {}", runner);
                        return Some(val);
                    }
                }
            }
        }
    }

    eprintln!("Note: DuckDB baseline unavailable (DuckDB not installed in environment, run via py -3 on host)");
    None
}

fn generate_markdown_summary(report: &FullReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Lume TI Benchmark Report — {} (`{}`)\n\n",
        report.date, report.git_sha
    ));
    out.push_str(&format!("- **Store Path**: `{}`\n", report.store_path));
    out.push_str(&format!("- **Parquet Path**: `{}`\n", report.parquet_path));
    out.push_str(&format!("- **Warm Iterations**: {}\n", report.iterations));
    out.push_str(&format!(
        "- **Queries Run**: {}\n\n",
        report.total_queries_run
    ));

    out.push_str("## Class Summary vs Spec/13 Targets\n\n");
    out.push_str("| Class | Spec Target | TI Cold p50 | TI Warm p50 | TI Warm p95 | TI Warm p99 | DuckDB p95 | Status |\n");
    out.push_str("|-------|-------------|-------------|-------------|-------------|-------------|------------|--------|\n");

    for (class_name, stats) in &report.classes {
        let dd_str = stats
            .duckdb_p95_ms
            .map(|ms| format!("{:.2} ms", ms))
            .unwrap_or_else(|| "n/a".to_string());
        let status = if stats.meets_target {
            "✅ Pass"
        } else {
            "⚠️ Over"
        };
        out.push_str(&format!(
            "| **{}** | {} | {:.2} ms | {:.2} ms | {:.2} ms | {:.2} ms | {} | {} |\n",
            class_name,
            stats.target_description,
            stats.cold_ms,
            stats.p50_ms,
            stats.p95_ms,
            stats.p99_ms,
            dd_str,
            status
        ));
    }

    out.push_str("\n## Detailed Query Latencies\n\n");
    out.push_str("| Query ID | Class | Rows | Cold | p50 | p95 | p99 | Description |\n");
    out.push_str("|----------|-------|------|------|-----|-----|-----|-------------|\n");

    for q in &report.queries {
        out.push_str(&format!(
            "| `{}` | {} | {} | {:.2} ms | {:.2} ms | {:.2} ms | {:.2} ms | {} |\n",
            q.id, q.class, q.rows, q.cold_ms, q.p50_ms, q.p95_ms, q.p99_ms, q.description
        ));
    }

    out
}
