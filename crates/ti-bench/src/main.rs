//! `ti-bench gen` — synthetic signalk-parquet generator for the Lume TI golden corpus.

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: ti-bench gen --root <dir> [--seed <n>] [--perf]");
        std::process::exit(2);
    }
    match args[1].as_str() {
        "gen" => gen(&args),
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
    let perf = args.iter().any(|a| a == "--perf");

    let (n_vessels, start, end) = if perf {
        // Performance set: 50 vessels x 365 days.
        (
            ti_bench::PERFORMANCE_VESSELS,
            1_743_724_800i64,
            1_775_260_800i64,
        )
    } else {
        (
            ti_bench::CORRECTNESS_VESSELS,
            ti_bench::gen::START_SECS,
            ti_bench::gen::END_SECS,
        )
    };

    let (raw, docs, cats) = ti_bench::write::write_all_stream(&root, seed, n_vessels, start, end);
    eprintln!(
        "wrote {} raw files, {} doc files, {} catalog files to {}",
        raw, docs, cats, root
    );

    // Determinism self-check: regenerate and verify identical file hashes.
    if std::env::var("TI_BENCH_VERIFY_DETERMINISM").is_ok() {
        let tmp = format!("{}.regen", root);
        let _ = std::fs::remove_dir_all(&tmp);
        ti_bench::write::write_all_stream(&tmp, seed, n_vessels, start, end);
        let same = dirs_identical(Path::new(&root), Path::new(&tmp));
        std::fs::remove_dir_all(&tmp).ok();
        if !same {
            eprintln!("DETERMINISM FAILED: regeneration produced different files");
            std::process::exit(1);
        }
        eprintln!("determinism check passed (identical file hashes)");
    }
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
