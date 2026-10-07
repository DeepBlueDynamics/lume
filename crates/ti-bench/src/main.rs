//! `ti-bench` — synthetic signalk-parquet generator and golden-corpus benchmark runner.

use std::path::Path;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: ti-bench <gen|bench> [args...]");
        std::process::exit(2);
    }
    match args[1].as_str() {
        "gen" => gen(&args),
        "bench" => {
            if let Err(e) = bench(&args).await {
                eprintln!("benchmark error: {e}");
                std::process::exit(1);
            }
        }
        other => {
            eprintln!("unknown command: {other}");
            std::process::exit(2);
        }
    }
}

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn gen(args: &[String]) {
    let root = arg(args, "--root").unwrap_or_else(|| "ti-bench-out".to_string());
    let seed = arg(args, "--seed")
        .map(|s| s.parse::<u64>().unwrap_or(ti_bench::DEFAULT_SEED))
        .unwrap_or(ti_bench::DEFAULT_SEED);
    if let Some(profile) = arg(args, "--profile") {
        match profile.as_str() {
            "robots" => {
                let (raw, docs) = ti_bench::robots::generate(Path::new(&root), seed);
                eprintln!("wrote {raw} robot telemetry files and {docs} document files to {root}");
                return;
            }
            "signalk" => {}
            _ => {
                eprintln!("unknown profile: {profile}");
                std::process::exit(2);
            }
        }
    }
    let perf = args.iter().any(|a| a == "--perf");

    let start = arg(args, "--start")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(if perf {
            1_743_724_800i64
        } else {
            ti_bench::gen::START_SECS
        });
    let end = arg(args, "--end")
        .and_then(|s| s.parse::<i64>().ok())
        .or_else(|| {
            arg(args, "--days")
                .and_then(|s| s.parse::<i64>().ok())
                .map(|d| start + d * 86_400)
        })
        .unwrap_or(if perf {
            1_775_260_800i64
        } else {
            ti_bench::gen::END_SECS
        });
    let n_vessels = arg(args, "--vessels")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(if perf {
            ti_bench::PERFORMANCE_VESSELS
        } else {
            ti_bench::CORRECTNESS_VESSELS
        });

    let hz = arg(args, "--hz").and_then(|s| s.parse::<f64>().ok());
    let per_path = args
        .iter()
        .any(|a| a == "--per-path" || a == "--per-path-override");
    let gen_config = if per_path {
        ti_bench::gen::GenConfig {
            hz: hz.unwrap_or(1.0),
            per_path_override: true,
        }
    } else if let Some(rate) = hz {
        ti_bench::gen::GenConfig {
            hz: rate,
            per_path_override: false,
        }
    } else {
        ti_bench::gen::GenConfig::default()
    };

    let (raw, docs, cats) = ti_bench::write::write_all_stream_with_config(
        &root,
        seed,
        n_vessels,
        start,
        end,
        &gen_config,
    );
    eprintln!(
        "wrote {} raw files, {} doc files, {} catalog files to {}",
        raw, docs, cats, root
    );

    // Determinism self-check: regenerate and verify identical file hashes.
    if std::env::var("TI_BENCH_VERIFY_DETERMINISM").is_ok() {
        let tmp = format!("{}.regen", root);
        let _ = std::fs::remove_dir_all(&tmp);
        ti_bench::write::write_all_stream_with_config(
            &tmp,
            seed,
            n_vessels,
            start,
            end,
            &gen_config,
        );
        let same = dirs_identical(Path::new(&root), Path::new(&tmp));
        std::fs::remove_dir_all(&tmp).ok();
        if !same {
            eprintln!("DETERMINISM FAILED: regeneration produced different files");
            std::process::exit(1);
        }
        eprintln!("determinism check passed (identical file hashes)");
    }
}

async fn bench(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let store = arg(args, "--store")
        .unwrap_or_else(|| "/workspace/lume/.lanes/data/store-full".to_string());
    let parquet = arg(args, "--parquet")
        .unwrap_or_else(|| "/workspace/lume/.lanes/data/correctness".to_string());
    let corpus = arg(args, "--corpus").unwrap_or_else(|| "tests/golden/corpus.json".to_string());
    let out_dir = arg(args, "--out-dir").unwrap_or_else(|| "bench/results".to_string());
    let iterations = arg(args, "--iterations")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(5);
    let class_filter = arg(args, "--class");

    let cache_bytes = arg(args, "--cache-bytes")
        .map(|s| s.parse::<u64>())
        .transpose()?;
    ti_bench::harness::run_benchmark_with_cache(
        &store,
        &parquet,
        &corpus,
        &out_dir,
        iterations,
        class_filter.as_deref(),
        cache_bytes,
    )
    .await?;

    Ok(())
}

/// Compare two directory trees for byte-identical contents (file names + hashes).
fn dirs_identical(a: &Path, b: &Path) -> bool {
    use std::collections::BTreeMap;

    fn snapshot(root: &Path) -> BTreeMap<String, u64> {
        let mut m = BTreeMap::new();
        fn walk(dir: &Path, base: &Path, m: &mut BTreeMap<String, u64>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap())
                .collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, m);
                } else {
                    let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
                    let bytes = std::fs::read(&p).unwrap();
                    // FNV-1a hash of contents
                    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                    for &b in &bytes {
                        h ^= b as u64;
                        h = h.wrapping_mul(0x1000_0000_01b3);
                    }
                    m.insert(rel, h);
                }
            }
        }
        walk(root, root, &mut m);
        m
    }

    snapshot(a) == snapshot(b)
}
