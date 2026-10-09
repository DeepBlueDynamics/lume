//! Reproducible, non-gating single-shard comparison baseline.
use std::{hint::black_box, time::Instant};
use ti_core::{BsiField, CmpOp, RoaringBitmap};
fn main() {
    let mut field = BsiField::new(0).unwrap();
    let columns: RoaringBitmap = (0..65536).collect();
    // Permutation spreads each bit across the shard rather than only long runs.
    for col in 0u32..65536 {
        field
            .set(col, ((col.wrapping_mul(40503)) & 65535) as i64)
            .unwrap();
    }
    assert_eq!(field.depth(), 16);
    assert_eq!(
        field
            .compare(CmpOp::Lt, 32768, None, &columns)
            .unwrap()
            .len(),
        32768
    );
    for _ in 0..100 {
        black_box(
            field
                .compare(CmpOp::Lt, black_box(32768), None, black_box(&columns))
                .unwrap(),
        );
    }
    let iterations = 2000;
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(
            field
                .compare(CmpOp::Lt, black_box(32768), None, black_box(&columns))
                .unwrap(),
        );
    }
    let elapsed = start.elapsed();
    println!("depth=16 columns=65536 comparison=Lt threshold=32768 iterations={iterations} elapsed_ms={:.3} mean_us={:.3} debug_assertions={}",elapsed.as_secs_f64()*1000.,elapsed.as_secs_f64()*1e6/iterations as f64,cfg!(debug_assertions));
}
