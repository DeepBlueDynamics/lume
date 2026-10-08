use std::collections::BTreeMap;
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use tempfile::tempdir;
use ti_contracts::{
    validate_ordinary_set_rows, Agg, BucketRecord, Catalog, CmpOp, FieldKind, FieldSpec,
    FieldValue, Predicate, RoaringBitmap, ShardKey, ShardSink, ShardSource, VesselSpec,
};
use ti_store::row::FieldData;
use ti_store::Store;

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0xdeadbeefcafe } else { seed })
    }
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn gen_range(&mut self, low: usize, high: usize) -> usize {
        assert!(low < high);
        low + (self.next_u64() as usize % (high - low))
    }
}

fn generate_step_records(
    step: u32,
    v0: u32,
    f_speed: u32,
    f_state: u32,
    row_idle: u32,
    row_running: u32,
    row_stopping: u32,
) -> Vec<BucketRecord> {
    let mut recs = Vec::new();
    let bucket = step;

    // 1. Normal insert for new bucket
    let speed_val = (bucket as i64) * 10;
    let state_val = match bucket % 3 {
        0 => row_idle,
        1 => row_running,
        _ => row_stopping,
    };
    recs.push(BucketRecord {
        vessel: v0,
        bucket,
        field: f_speed,
        value: FieldValue::Int(speed_val),
        rewrite: false,
    });
    recs.push(BucketRecord {
        vessel: v0,
        bucket,
        field: f_state,
        value: FieldValue::SetValue(state_val),
        rewrite: false,
    });

    // 2. Mixed workload: rewrite or clear an earlier bucket
    if step > 2 && step.is_multiple_of(11) {
        // Clear an earlier bucket
        let target = bucket - 2;
        recs.push(BucketRecord {
            vessel: v0,
            bucket: target,
            field: f_speed,
            value: FieldValue::Clear,
            rewrite: true,
        });
        recs.push(BucketRecord {
            vessel: v0,
            bucket: target,
            field: f_state,
            value: FieldValue::Clear,
            rewrite: true,
        });
    } else if step > 2 && step.is_multiple_of(5) {
        // Rewrite an earlier bucket with modified value
        let target = bucket - 2;
        let new_speed = (target as i64) * 10 + 77;
        let new_state = match (bucket + 1) % 3 {
            0 => row_idle,
            1 => row_running,
            _ => row_stopping,
        };
        recs.push(BucketRecord {
            vessel: v0,
            bucket: target,
            field: f_speed,
            value: FieldValue::Int(new_speed),
            rewrite: true,
        });
        recs.push(BucketRecord {
            vessel: v0,
            bucket: target,
            field: f_state,
            value: FieldValue::SetValue(new_state),
            rewrite: true,
        });
    }

    recs
}

#[derive(Clone, Debug, PartialEq)]
struct BucketExpectation {
    speed: Option<i64>,
    state: Option<u32>,
}

type Model = BTreeMap<u32, BucketExpectation>;

fn build_reference_model(
    max_step: u32,
    v0: u32,
    f_speed: u32,
    f_state: u32,
    row_idle: u32,
    row_running: u32,
    row_stopping: u32,
) -> Model {
    let mut model = Model::new();
    for s in 0..=max_step {
        let recs =
            generate_step_records(s, v0, f_speed, f_state, row_idle, row_running, row_stopping);
        for rec in recs {
            let entry = model.entry(rec.bucket).or_insert(BucketExpectation {
                speed: None,
                state: None,
            });
            if rec.field == f_speed {
                match rec.value {
                    FieldValue::Int(v) => entry.speed = Some(v),
                    FieldValue::Clear => entry.speed = None,
                    _ => {}
                }
            } else if rec.field == f_state {
                match rec.value {
                    FieldValue::SetValue(r) => entry.state = Some(r),
                    FieldValue::Clear => entry.state = None,
                    _ => {}
                }
            }
        }
    }
    model
}

fn check_store_matches_model(
    store: &Store,
    model: &Model,
    shard_key: ShardKey,
    f_speed: u32,
    f_state: u32,
) -> std::result::Result<(), String> {
    // 1. Check speed presence
    let actual_speed_presence = store
        .eval(shard_key, &Predicate::Present(f_speed))
        .map_err(|e| format!("eval f_speed presence error: {e}"))?;

    let expected_speed_buckets: Vec<u32> = model
        .iter()
        .filter_map(|(&b, exp)| if exp.speed.is_some() { Some(b) } else { None })
        .collect();

    if actual_speed_presence.len() != expected_speed_buckets.len() as u64 {
        return Err(format!(
            "speed presence count mismatch: actual {}, expected {}",
            actual_speed_presence.len(),
            expected_speed_buckets.len()
        ));
    }

    for &b in &expected_speed_buckets {
        if !actual_speed_presence.contains(b) {
            return Err(format!("expected speed bucket {b} missing from presence"));
        }
    }

    // 2. Check state presence
    let actual_state_presence = store
        .eval(shard_key, &Predicate::Present(f_state))
        .map_err(|e| format!("eval f_state presence error: {e}"))?;

    let expected_state_buckets: Vec<u32> = model
        .iter()
        .filter_map(|(&b, exp)| if exp.state.is_some() { Some(b) } else { None })
        .collect();

    if actual_state_presence.len() != expected_state_buckets.len() as u64 {
        return Err(format!(
            "state presence count mismatch: actual {}, expected {}",
            actual_state_presence.len(),
            expected_state_buckets.len()
        ));
    }

    for &b in &expected_state_buckets {
        if !actual_state_presence.contains(b) {
            return Err(format!("expected state bucket {b} missing from presence"));
        }
    }

    // 3. Verify exact values for each bucket in model
    for (&b, exp) in model {
        if let Some(expected_speed) = exp.speed {
            let hit = store
                .eval(
                    shard_key,
                    &Predicate::BsiCmp {
                        field: f_speed,
                        op: CmpOp::Eq,
                        lo: expected_speed,
                        hi: None,
                    },
                )
                .map_err(|e| format!("eval speed eq failed: {e}"))?;
            if !hit.contains(b) {
                return Err(format!(
                    "bucket {b} speed value mismatch: expected {expected_speed}"
                ));
            }
        }

        if let Some(expected_state) = exp.state {
            let hit = store
                .eval(
                    shard_key,
                    &Predicate::SetEq {
                        field: f_state,
                        rows: vec![expected_state],
                        negate: false,
                    },
                )
                .map_err(|e| format!("eval state eq failed: {e}"))?;
            if !hit.contains(b) {
                return Err(format!(
                    "bucket {b} state value mismatch: expected row {expected_state}"
                ));
            }
        }
    }

    Ok(())
}

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

    let f_speed = store
        .catalog()
        .register_field(&FieldSpec {
            id: 0,
            path: "navigation.speed".into(),
            agg: Some(Agg::Mean),
            kind: FieldKind::Bsi { scale: 3 },
            units: Some("m/s".into()),
        })
        .unwrap();

    let f_state = store
        .catalog()
        .register_field(&FieldSpec {
            id: 1,
            path: "navigation.state".into(),
            agg: None,
            kind: FieldKind::Set,
            units: None,
        })
        .unwrap();

    let row_idle = store.catalog().register_set_value(f_state, "idle").unwrap();
    let row_running = store
        .catalog()
        .register_set_value(f_state, "running")
        .unwrap();
    let row_stopping = store
        .catalog()
        .register_set_value(f_state, "stopping")
        .unwrap();

    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "READY");
    let _ = stdout.flush();

    for step in 0..10_000u32 {
        let recs = generate_step_records(
            step,
            v0,
            f_speed,
            f_state,
            row_idle,
            row_running,
            row_stopping,
        );

        let _ = writeln!(stdout, "APPLYING {step}");
        let _ = stdout.flush();

        store.apply(&recs).unwrap();

        let _ = writeln!(stdout, "ACK {step}");
        let _ = stdout.flush();

        if step.is_multiple_of(7) {
            let _ = writeln!(stdout, "START_FLUSH {step}");
            let _ = stdout.flush();

            store.flush_shards().unwrap();

            let _ = writeln!(stdout, "SHARDS_FLUSHED {step}");
            let _ = stdout.flush();

            store.truncate_wals().unwrap();

            let _ = writeln!(stdout, "WALS_TRUNCATED {step}");
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

    // Test-only replay control; the default 1,000-run schedule is unchanged.
    let seed_override = env::var("CRASH_SEED")
        .ok()
        .map(|value| value.parse::<u64>().expect("CRASH_SEED must be a u64"));
    let total_runs: usize = if seed_override.is_some() { 1 } else { 1_000 };
    let exe = env::current_exe().unwrap();

    println!("Starting {total_runs} kill -9 crash recovery iterations...");
    let test_start = Instant::now();

    let mut count_mid_apply = 0usize;
    let mut count_mid_flush = 0usize;
    let mut count_between_flush_truncate = 0usize;

    for run_idx in 0..total_runs {
        let seed = seed_override.unwrap_or_else(|| {
            0x9e3779b97f4a7c15u64.wrapping_mul((run_idx + 1) as u64) ^ 0x517cc1b727220a95
        });
        let mut rng = Rng::new(seed);

        let kill_mode = rng.gen_range(0, 3);
        let kill_mode_name = match kill_mode {
            0 => "mid-apply",
            1 => "mid-flush",
            _ => "between-flush-and-truncate",
        };

        let target_step = rng.gen_range(1, 30) as u32;
        let target_flush = rng.gen_range(1, 5);
        if seed_override.is_some() {
            println!("Replay seed {seed}, mode {kill_mode_name}, target_step {target_step}, target_flush {target_flush}");
        }

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
        let mut flushes_seen = 0;

        while let Ok(n) = reader.read_line(&mut line) {
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("ACK ") {
                if let Ok(step) = rest.parse::<u32>() {
                    last_acked = Some(step);
                }
            }

            match kill_mode {
                0 => {
                    // mid-apply: kill when we see APPLYING for target_step
                    if let Some(rest) = trimmed.strip_prefix("APPLYING ") {
                        if let Ok(step) = rest.parse::<u32>() {
                            if step >= target_step {
                                break;
                            }
                        }
                    }
                }
                1 => {
                    // mid-flush: kill during START_FLUSH
                    if trimmed.starts_with("START_FLUSH") {
                        flushes_seen += 1;
                        if flushes_seen >= target_flush {
                            break;
                        }
                    }
                }
                _ => {
                    // between flush and truncate: kill after SHARDS_FLUSHED
                    if trimmed.starts_with("SHARDS_FLUSHED") {
                        flushes_seen += 1;
                        if flushes_seen >= target_flush {
                            break;
                        }
                    }
                }
            }
            line.clear();
        }

        // Kill with SIGKILL (kill -9)
        let _ = child.kill();
        let _ = child.wait();

        // Drain any remaining output buffered from child before death
        line.clear();
        while let Ok(n) = reader.read_line(&mut line) {
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("ACK ") {
                if let Ok(step) = rest.parse::<u32>() {
                    last_acked = Some(step);
                }
            }
            line.clear();
        }

        match kill_mode {
            0 => count_mid_apply += 1,
            1 => count_mid_flush += 1,
            _ => count_between_flush_truncate += 1,
        }

        // Re-open store from the same directory to test crash recovery
        let store = match Store::open_or_create(dir.path(), 10) {
            Ok(s) => s,
            Err(e) => {
                let evidence = dir.keep();
                panic!(
                    "Run {run_idx} (seed {seed}, mode {kill_mode_name}) failed to reopen store: {e}; retained evidence at {}", evidence.display()
                );
            }
        };

        let shard_key = ShardKey {
            vessel: 0,
            shard: 0,
        };
        let f_speed = 0u32;
        let f_state = 1u32;
        let row_idle = 0u32;
        let row_running = 1u32;
        let row_stopping = 2u32;

        // D21 Invariant: Enforce validate_ordinary_set_rows after recovery
        if let Some(open) = store.open_shard(&shard_key) {
            if let Some(FieldData::Set(set_field)) = open.data.fields.get(&f_state) {
                let rows: Vec<RoaringBitmap> = set_field.rows().values().cloned().collect();
                if let Err(e) = validate_ordinary_set_rows(set_field.presence(), &rows) {
                    panic!(
                        "Run {run_idx} (seed {seed}, mode {kill_mode_name}) D21 ordinary set invariant failed: {e}"
                    );
                }
            }
        }

        // Reference model verification:
        // Must match either model_acked (all acked records durable),
        // or model_extended (unacked record also durable).
        let (model_acked, model_extended) = match last_acked {
            Some(k) => (
                build_reference_model(k, 0, f_speed, f_state, row_idle, row_running, row_stopping),
                build_reference_model(
                    k + 1,
                    0,
                    f_speed,
                    f_state,
                    row_idle,
                    row_running,
                    row_stopping,
                ),
            ),
            None => (
                BTreeMap::new(),
                build_reference_model(0, 0, f_speed, f_state, row_idle, row_running, row_stopping),
            ),
        };

        let res_acked =
            check_store_matches_model(&store, &model_acked, shard_key, f_speed, f_state);
        let res_extended =
            check_store_matches_model(&store, &model_extended, shard_key, f_speed, f_state);

        if res_acked.is_err() && res_extended.is_err() {
            panic!(
                "Run {run_idx} (seed {seed}, mode {kill_mode_name}) failed crash recovery!\nAcked model error: {:?}\nExtended model error: {:?}",
                res_acked, res_extended
            );
        }

        // Materialize RecordBatch via read() to verify Arrow conversion integrity
        let speed_presence = store.eval(shard_key, &Predicate::Present(f_speed)).unwrap();
        let state_presence = store.eval(shard_key, &Predicate::Present(f_state)).unwrap();
        let all_present = &speed_presence | &state_presence;
        if !all_present.is_empty() {
            let batch = store
                .read(shard_key, &all_present, &[f_speed, f_state])
                .unwrap();
            assert_eq!(batch.num_rows(), all_present.len() as usize);
        }

        if (run_idx + 1).is_multiple_of(200) {
            println!("Completed {}/{total_runs} crash iterations...", run_idx + 1);
        }
    }

    let elapsed = test_start.elapsed();
    println!("--------------------------------------------------");
    println!("Crash recovery test completed successfully!");
    println!("Total runtime: {:?}", elapsed);
    println!("Kill point distribution (total {}):", total_runs);
    println!("  mid-apply:                  {}", count_mid_apply);
    println!("  mid-flush:                  {}", count_mid_flush);
    println!(
        "  between flush and truncate: {}",
        count_between_flush_truncate
    );
    println!("--------------------------------------------------");
}
