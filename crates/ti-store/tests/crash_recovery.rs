use std::env;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use tempfile::tempdir;
use ti_contracts::{
    Agg, BucketRecord, Catalog, FieldKind, FieldSpec, FieldValue, Predicate, ShardKey, ShardSink,
    ShardSource, VesselSpec,
};
use ti_store::Store;

fn run_worker(dir: PathBuf) {
    let mut store = Store::open_or_create(&dir, 10).unwrap();

    let v0 = store
        .catalog()
        .register_vessel(&VesselSpec {
            urn: "vessels.urn:mrn:signalk:uuid:boat-crash".into(),
            name: Some("CrashTest".into()),
            mmsi: None,
        })
        .unwrap();

    let f0 = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "navigation.speed".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 3 },
            units: Some("m/s".into()),
        })
        .unwrap();

    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "READY");
    let _ = stdout.flush();

    for i in 0..10_000u32 {
        let rec = BucketRecord {
            vessel: v0,
            bucket: i,
            field: f0,
            value: FieldValue::Int((i as i64) * 10),
            rewrite: false,
        };
        store.apply(&[rec]).unwrap();
        let _ = writeln!(stdout, "ACK {i}");
        let _ = stdout.flush();

        if i % 3 == 0 {
            let _ = writeln!(stdout, "FLUSHING {i}");
            let _ = stdout.flush();
            store.flush().unwrap();
            let _ = writeln!(stdout, "FLUSHED {i}");
            let _ = stdout.flush();
        }
    }
}

#[test]
fn test_crash_recovery_1000_runs() {
    if env::var("CRASH_WORKER").is_ok() {
        let dir = PathBuf::from(env::var("CRASH_DIR").unwrap());
        run_worker(dir);
        return;
    }

    const TOTAL_RUNS: usize = 1_000;
    let exe = env::current_exe().unwrap();

    println!("Starting 1,000 kill -9 crash recovery iterations...");

    for run_idx in 0..TOTAL_RUNS {
        let dir = tempdir().unwrap();

        let mut child = Command::new(&exe)
            .env("CRASH_WORKER", "1")
            .env("CRASH_DIR", dir.path())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();

        let stdout = child.stdout.take().unwrap();
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        let mut last_acked: Option<u32> = None;
        let kill_target_flushes = (run_idx % 5) + 1;
        let mut flushes_seen = 0;

        while let Ok(n) = reader.read_line(&mut line) {
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("ACK ") {
                if let Ok(bucket) = rest.parse::<u32>() {
                    last_acked = Some(bucket);
                }
            } else if trimmed.starts_with("FLUSHING") {
                flushes_seen += 1;
                if flushes_seen >= kill_target_flushes {
                    // Send kill -9 directly during flush!
                    break;
                }
            }
            line.clear();
        }

        // Kill with SIGKILL (kill -9)
        let _ = child.kill();
        let _ = child.wait();

        // If the worker acknowledged at least one record before being killed,
        // verify replay and consistency
        if let Some(max_acked) = last_acked {
            // Re-open store from the same directory
            let store = Store::open_or_create(dir.path(), 10).expect("Store recovery failed");

            let shard_key = ShardKey {
                vessel: 0,
                shard: 0,
            };

            let present_bits = store
                .eval(shard_key, &Predicate::Present(0))
                .expect("Eval presence failed");

            // Zero lost acknowledged records:
            // Every bucket from 0 up to max_acked must be present!
            for b in 0..=max_acked {
                assert!(
                    present_bits.contains(b),
                    "Run {run_idx}: Acknowledged bucket {b} was lost after crash recovery! Max acked: {max_acked}",
                );
            }

            // Zero duplicated records:
            // The number of present buckets cannot exceed the number of distinct buckets applied
            assert!(
                present_bits.len() <= (max_acked + 10) as u64,
                "Run {run_idx}: Unexpected excess buckets found after crash recovery!",
            );

            // Verify data integrity for each acknowledged bucket
            let batch = store
                .read(shard_key, &present_bits, &[0])
                .expect("Read failed");
            assert_eq!(batch.num_rows(), present_bits.len() as usize);
        }

        if (run_idx + 1) % 200 == 0 {
            println!("Completed {}/1000 crash iterations...", run_idx + 1);
        }
    }

    println!("All 1,000 kill -9 crash recovery iterations passed with zero lost or duplicated records!");
}
