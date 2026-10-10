//! SIMD kernels and scalar references for MiniRoaring container operations.
//!
//! Provides accelerated bitwise AND, OR, ANDNOT, popcount, and array intersection
//! for MiniRoaring containers. Unsafe vector intrinsics are quarantined in this module.

// ─── Scalar References ───────────────────────────────────────────────────────

/// Bitwise AND of two 1024-word bitmap containers into `dst`.
/// Returns `true` if `dst` contains any non-zero word (i.e. is non-empty).
pub fn scalar_bitmap_and(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) -> bool {
    let mut any = 0u64;
    for i in 0..1024 {
        let v = a[i] & b[i];
        dst[i] = v;
        any |= v;
    }
    any != 0
}

/// Bitwise OR of two 1024-word bitmap containers into `dst`.
pub fn scalar_bitmap_or(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) {
    for i in 0..1024 {
        dst[i] = a[i] | b[i];
    }
}

/// Bitwise ANDNOT (a \ b, i.e. `a & !b`) of two 1024-word bitmap containers into `dst`.
/// Returns `true` if `dst` contains any non-zero word (i.e. is non-empty).
pub fn scalar_bitmap_andnot(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) -> bool {
    let mut any = 0u64;
    for i in 0..1024 {
        let v = a[i] & !b[i];
        dst[i] = v;
        any |= v;
    }
    any != 0
}

/// Popcount across all 1024 words (65,536 bits) of a bitmap container.
pub fn scalar_bitmap_popcount(a: &[u64; 1024]) -> usize {
    let mut count = 0usize;
    for &w in a {
        count += w.count_ones() as usize;
    }
    count
}

/// Counts the set bits in the intersection `a & b` without materializing an output buffer.
pub fn scalar_bitmap_and_popcount(a: &[u64; 1024], b: &[u64; 1024]) -> usize {
    let mut count = 0usize;
    for i in 0..1024 {
        count += (a[i] & b[i]).count_ones() as usize;
    }
    count
}

/// Standard 2-pointer intersection of two sorted u16 slices.
pub fn scalar_array_intersect(dst: &mut Vec<u16>, a: &[u16], b: &[u16]) {
    dst.clear();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            dst.push(a[i]);
            i += 1;
            j += 1;
        } else if a[i] < b[j] {
            i += 1;
        } else {
            j += 1;
        }
    }
}

/// Counts the intersection of two sorted u16 slices without allocating.
pub fn scalar_array_intersect_count(a: &[u16], b: &[u16]) -> usize {
    let (mut i, mut j, mut count) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            count += 1;
            i += 1;
            j += 1;
        } else if a[i] < b[j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    count
}

/// Galloping / exponential search intersection for unbalanced array sizes (ratio > 8x).
pub fn galloping_array_intersect(dst: &mut Vec<u16>, small: &[u16], large: &[u16]) {
    dst.clear();
    let mut base = 0;
    for &target in small {
        if base >= large.len() {
            break;
        }
        if large[base] > target {
            continue;
        }
        let mut step = 1;
        let mut high = base;
        while high + step < large.len() && large[high + step] <= target {
            high += step;
            step *= 2;
        }
        let end = (high + step + 1).min(large.len());
        match large[high..end].binary_search(&target) {
            Ok(found) => {
                dst.push(target);
                base = high + found + 1;
            }
            Err(ins) => {
                base = high + ins;
            }
        }
    }
}

/// Galloping / exponential search intersection count for unbalanced array sizes (ratio > 8x).
pub fn galloping_array_intersect_count(small: &[u16], large: &[u16]) -> usize {
    let mut count = 0;
    let mut base = 0;
    for &target in small {
        if base >= large.len() {
            break;
        }
        if large[base] > target {
            continue;
        }
        let mut step = 1;
        let mut high = base;
        while high + step < large.len() && large[high + step] <= target {
            high += step;
            step *= 2;
        }
        let end = (high + step + 1).min(large.len());
        match large[high..end].binary_search(&target) {
            Ok(found) => {
                count += 1;
                base = high + found + 1;
            }
            Err(ins) => {
                base = high + ins;
            }
        }
    }
    count
}

// ─── x86_64 AVX2 Kernels ─────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    pub unsafe fn bitmap_and(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) -> bool {
        let mut any = _mm256_setzero_si256();
        let d_ptr = dst.as_mut_ptr() as *mut __m256i;
        let a_ptr = a.as_ptr() as *const __m256i;
        let b_ptr = b.as_ptr() as *const __m256i;

        for i in 0..256 {
            let va = _mm256_loadu_si256(a_ptr.add(i));
            let vb = _mm256_loadu_si256(b_ptr.add(i));
            let v = _mm256_and_si256(va, vb);
            _mm256_storeu_si256(d_ptr.add(i), v);
            any = _mm256_or_si256(any, v);
        }
        _mm256_testz_si256(any, any) == 0
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn bitmap_or(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) {
        let d_ptr = dst.as_mut_ptr() as *mut __m256i;
        let a_ptr = a.as_ptr() as *const __m256i;
        let b_ptr = b.as_ptr() as *const __m256i;

        for i in 0..256 {
            let va = _mm256_loadu_si256(a_ptr.add(i));
            let vb = _mm256_loadu_si256(b_ptr.add(i));
            let v = _mm256_or_si256(va, vb);
            _mm256_storeu_si256(d_ptr.add(i), v);
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn bitmap_andnot(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) -> bool {
        let mut any = _mm256_setzero_si256();
        let d_ptr = dst.as_mut_ptr() as *mut __m256i;
        let a_ptr = a.as_ptr() as *const __m256i;
        let b_ptr = b.as_ptr() as *const __m256i;

        for i in 0..256 {
            let va = _mm256_loadu_si256(a_ptr.add(i));
            let vb = _mm256_loadu_si256(b_ptr.add(i));
            // _mm256_andnot_si256(m, x) computes (!m) & x.
            // We want a & !b == (!vb) & va.
            let v = _mm256_andnot_si256(vb, va);
            _mm256_storeu_si256(d_ptr.add(i), v);
            any = _mm256_or_si256(any, v);
        }
        _mm256_testz_si256(any, any) == 0
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn bitmap_popcount(a: &[u64; 1024]) -> usize {
        let mut total = 0usize;
        let mut i = 0;
        while i < 1024 {
            total += a[i].count_ones() as usize
                + a[i + 1].count_ones() as usize
                + a[i + 2].count_ones() as usize
                + a[i + 3].count_ones() as usize;
            i += 4;
        }
        total
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn bitmap_and_popcount(a: &[u64; 1024], b: &[u64; 1024]) -> usize {
        let mut total = 0usize;
        let mut i = 0;
        while i < 1024 {
            total += (a[i] & b[i]).count_ones() as usize
                + (a[i + 1] & b[i + 1]).count_ones() as usize
                + (a[i + 2] & b[i + 2]).count_ones() as usize
                + (a[i + 3] & b[i + 3]).count_ones() as usize;
            i += 4;
        }
        total
    }
}

// ─── aarch64 NEON Kernels ────────────────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
#[allow(dead_code)]
mod neon {
    use core::arch::aarch64::*;

    pub unsafe fn bitmap_and(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) -> bool {
        let mut any = vdupq_n_u64(0);
        let d_ptr = dst.as_mut_ptr();
        let a_ptr = a.as_ptr();
        let b_ptr = b.as_ptr();

        for i in (0..1024).step_by(2) {
            let va = vld1q_u64(a_ptr.add(i));
            let vb = vld1q_u64(b_ptr.add(i));
            let v = vandq_u64(va, vb);
            vst1q_u64(d_ptr.add(i), v);
            any = vorrq_u64(any, v);
        }
        vmaxvq_u32(vreinterpretq_u32_u64(any)) != 0
    }

    pub unsafe fn bitmap_or(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) {
        let d_ptr = dst.as_mut_ptr();
        let a_ptr = a.as_ptr();
        let b_ptr = b.as_ptr();

        for i in (0..1024).step_by(2) {
            let va = vld1q_u64(a_ptr.add(i));
            let vb = vld1q_u64(b_ptr.add(i));
            let v = vorrq_u64(va, vb);
            vst1q_u64(d_ptr.add(i), v);
        }
    }

    pub unsafe fn bitmap_andnot(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) -> bool {
        let mut any = vdupq_n_u64(0);
        let d_ptr = dst.as_mut_ptr();
        let a_ptr = a.as_ptr();
        let b_ptr = b.as_ptr();

        for i in (0..1024).step_by(2) {
            let va = vld1q_u64(a_ptr.add(i));
            let vb = vld1q_u64(b_ptr.add(i));
            // vbicq_u64(a, b) computes a & !b
            let v = vbicq_u64(va, vb);
            vst1q_u64(d_ptr.add(i), v);
            any = vorrq_u64(any, v);
        }
        vmaxvq_u32(vreinterpretq_u32_u64(any)) != 0
    }

    pub unsafe fn bitmap_popcount(a: &[u64; 1024]) -> usize {
        let ptr = a.as_ptr() as *const u8;
        let mut acc = vdupq_n_u32(0);

        for i in 0..512 {
            let v = vld1q_u8(ptr.add(i * 16));
            let cnt = vcntq_u8(v);
            let p16 = vpaddlq_u8(cnt);
            let p32 = vpaddlq_u16(p16);
            acc = vaddq_u32(acc, p32);
        }
        vaddvq_u32(acc) as usize
    }

    pub unsafe fn bitmap_and_popcount(a: &[u64; 1024], b: &[u64; 1024]) -> usize {
        let a_ptr = a.as_ptr() as *const u8;
        let b_ptr = b.as_ptr() as *const u8;
        let mut acc = vdupq_n_u32(0);

        for i in 0..512 {
            let va = vld1q_u8(a_ptr.add(i * 16));
            let vb = vld1q_u8(b_ptr.add(i * 16));
            let v = vandq_u8(va, vb);
            let cnt = vcntq_u8(v);
            let p16 = vpaddlq_u8(cnt);
            let p32 = vpaddlq_u16(p16);
            acc = vaddq_u32(acc, p32);
        }
        vaddvq_u32(acc) as usize
    }
}

// ─── Dispatch Wrappers with Runtime Feature Detection ────────────────────────

/// Accelerated bitwise AND of two 1024-word bitmap containers into `dst`.
pub fn bitmap_and(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { avx2::bitmap_and(dst, a, b) };
        }
    }
    // On aarch64 (ARM Cortex-A76), LLVM auto-vectorizes the scalar single-pass
    // loop with accumulated OR faster than manual NEON store + reduction (3.33 vs 4.45 ms, 0.75x).
    scalar_bitmap_and(dst, a, b)
}

/// Accelerated bitwise OR of two 1024-word bitmap containers into `dst`.
pub fn bitmap_or(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe { avx2::bitmap_or(dst, a, b) };
            return;
        }
    }
    // On aarch64, route to scalar_bitmap_or.
    scalar_bitmap_or(dst, a, b);
}

/// Accelerated bitwise ANDNOT (`a & !b`) of two 1024-word bitmap containers into `dst`.
pub fn bitmap_andnot(dst: &mut [u64; 1024], a: &[u64; 1024], b: &[u64; 1024]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { avx2::bitmap_andnot(dst, a, b) };
        }
    }
    // On aarch64, route to scalar_bitmap_andnot.
    scalar_bitmap_andnot(dst, a, b)
}

/// Accelerated popcount over all 1024 words of a bitmap container.
pub fn bitmap_popcount(a: &[u64; 1024]) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { avx2::bitmap_popcount(a) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return unsafe { neon::bitmap_popcount(a) };
    }
    #[allow(unreachable_code)]
    scalar_bitmap_popcount(a)
}

/// Accelerated intersection popcount (`popcount(a & b)`) without allocating.
pub fn bitmap_and_popcount(a: &[u64; 1024], b: &[u64; 1024]) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { avx2::bitmap_and_popcount(a, b) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return unsafe { neon::bitmap_and_popcount(a, b) };
    }
    #[allow(unreachable_code)]
    scalar_bitmap_and_popcount(a, b)
}

/// Intersects two sorted u16 slices into `dst`, dispatching to galloping search
/// if size ratio exceeds 8x.
pub fn array_intersect(dst: &mut Vec<u16>, a: &[u16], b: &[u16]) {
    if a.len() * 8 < b.len() {
        galloping_array_intersect(dst, a, b);
    } else if b.len() * 8 < a.len() {
        galloping_array_intersect(dst, b, a);
    } else {
        scalar_array_intersect(dst, a, b);
    }
}

/// Counts intersection of two sorted u16 slices, dispatching to galloping search
/// if size ratio exceeds 8x.
pub fn array_intersect_count(a: &[u16], b: &[u16]) -> usize {
    if a.len() * 8 < b.len() {
        galloping_array_intersect_count(a, b)
    } else if b.len() * 8 < a.len() {
        galloping_array_intersect_count(b, a)
    } else {
        scalar_array_intersect_count(a, b)
    }
}

// ─── Tests & Verification ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn generate_test_bitmap(cardinality_target: usize, seed: u64) -> [u64; 1024] {
        let mut bm = [0u64; 1024];
        if cardinality_target == 0 {
            return bm;
        }
        if cardinality_target >= 65536 {
            return [!0u64; 1024];
        }

        let mut rng = seed;
        let mut lcg = || -> u64 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            rng
        };

        if cardinality_target < 200 {
            // Sparse singletons
            for _ in 0..cardinality_target {
                let bit = (lcg() % 65536) as usize;
                bm[bit / 64] |= 1u64 << (bit % 64);
            }
        } else {
            // Dense / clustered runs
            let mut remaining = cardinality_target;
            while remaining > 0 {
                let start = (lcg() % 65536) as usize;
                let run_len = ((lcg() % 256) as usize + 1).min(remaining);
                for bit in start..(start + run_len).min(65536) {
                    bm[bit / 64] |= 1u64 << (bit % 64);
                }
                remaining = remaining.saturating_sub(run_len);
            }
        }
        bm
    }

    #[test]
    fn test_differential_edge_sizes_and_patterns() {
        let edge_sizes = [
            0, 1, 63, 64, 127, 128, 1023, 1024, 1025, 4096, 32768, 65535, 65536,
        ];

        for (seed_idx, &size_a) in edge_sizes.iter().enumerate() {
            for &size_b in &edge_sizes {
                let a = generate_test_bitmap(size_a, (seed_idx as u64 + 1) * 31);
                let b = generate_test_bitmap(size_b, (seed_idx as u64 + 1) * 97);

                // 1. Bitwise AND
                let mut d_scalar = [0u64; 1024];
                let mut d_simd = [0u64; 1024];
                let any_scalar = scalar_bitmap_and(&mut d_scalar, &a, &b);
                let any_simd = bitmap_and(&mut d_simd, &a, &b);
                assert_eq!(
                    d_scalar, d_simd,
                    "bitmap_and mismatch at sizes ({size_a}, {size_b})"
                );
                assert_eq!(
                    any_scalar, any_simd,
                    "bitmap_and non-empty mismatch at ({size_a}, {size_b})"
                );

                // 2. Bitwise OR
                let mut d_scalar_or = [0u64; 1024];
                let mut d_simd_or = [0u64; 1024];
                scalar_bitmap_or(&mut d_scalar_or, &a, &b);
                bitmap_or(&mut d_simd_or, &a, &b);
                assert_eq!(
                    d_scalar_or, d_simd_or,
                    "bitmap_or mismatch at sizes ({size_a}, {size_b})"
                );

                // 3. Bitwise ANDNOT
                let mut d_scalar_diff = [0u64; 1024];
                let mut d_simd_diff = [0u64; 1024];
                let any_scalar_diff = scalar_bitmap_andnot(&mut d_scalar_diff, &a, &b);
                let any_simd_diff = bitmap_andnot(&mut d_simd_diff, &a, &b);
                assert_eq!(
                    d_scalar_diff, d_simd_diff,
                    "bitmap_andnot mismatch at sizes ({size_a}, {size_b})"
                );
                assert_eq!(
                    any_scalar_diff, any_simd_diff,
                    "bitmap_andnot non-empty mismatch at ({size_a}, {size_b})"
                );

                // 4. Popcount
                let cnt_scalar = scalar_bitmap_popcount(&a);
                let cnt_simd = bitmap_popcount(&a);
                assert_eq!(
                    cnt_scalar, cnt_simd,
                    "bitmap_popcount mismatch at size {size_a}"
                );

                // 5. Intersection popcount
                let and_cnt_scalar = scalar_bitmap_and_popcount(&a, &b);
                let and_cnt_simd = bitmap_and_popcount(&a, &b);
                assert_eq!(
                    and_cnt_scalar, and_cnt_simd,
                    "bitmap_and_popcount mismatch at ({size_a}, {size_b})"
                );
            }
        }
    }

    #[test]
    fn test_alternating_words_and_boundary_bits() {
        let mut a = [0u64; 1024];
        let mut b = [0u64; 1024];
        for i in 0..1024 {
            a[i] = if i % 2 == 0 {
                0x5555_5555_5555_5555
            } else {
                0xAAAA_AAAA_AAAA_AAAA
            };
            b[i] = if i % 2 == 0 {
                0x3333_3333_3333_3333
            } else {
                0xCCCC_CCCC_CCCC_CCCC
            };
        }

        let mut d_scalar = [0u64; 1024];
        let mut d_simd = [0u64; 1024];
        assert_eq!(
            scalar_bitmap_and(&mut d_scalar, &a, &b),
            bitmap_and(&mut d_simd, &a, &b)
        );
        assert_eq!(d_scalar, d_simd);

        let mut d_or_s = [0u64; 1024];
        let mut d_or_simd = [0u64; 1024];
        scalar_bitmap_or(&mut d_or_s, &a, &b);
        bitmap_or(&mut d_or_simd, &a, &b);
        assert_eq!(d_or_s, d_or_simd);

        let mut d_diff_s = [0u64; 1024];
        let mut d_diff_simd = [0u64; 1024];
        assert_eq!(
            scalar_bitmap_andnot(&mut d_diff_s, &a, &b),
            bitmap_andnot(&mut d_diff_simd, &a, &b)
        );
        assert_eq!(d_diff_s, d_diff_simd);

        assert_eq!(scalar_bitmap_popcount(&a), bitmap_popcount(&a));
        assert_eq!(
            scalar_bitmap_and_popcount(&a, &b),
            bitmap_and_popcount(&a, &b)
        );
    }

    #[test]
    fn test_array_intersection_galloping_vs_scalar() {
        let small = vec![5, 100, 500, 2000, 45000];
        let mut large = Vec::new();
        for i in 0..5000 {
            large.push((i * 10) as u16);
        }

        let mut dst_scalar = Vec::new();
        let mut dst_gallop = Vec::new();
        let mut dst_auto = Vec::new();

        scalar_array_intersect(&mut dst_scalar, &small, &large);
        galloping_array_intersect(&mut dst_gallop, &small, &large);
        array_intersect(&mut dst_auto, &small, &large);

        assert_eq!(dst_scalar, dst_gallop);
        assert_eq!(dst_scalar, dst_auto);
        assert_eq!(
            scalar_array_intersect_count(&small, &large),
            dst_scalar.len()
        );
        assert_eq!(
            galloping_array_intersect_count(&small, &large),
            dst_scalar.len()
        );
        assert_eq!(array_intersect_count(&small, &large), dst_scalar.len());
        assert_eq!(array_intersect_count(&large, &small), dst_scalar.len());
    }

    #[test]
    #[ignore]
    fn test_microbench_simd_vs_scalar() {
        use std::time::Instant;

        let a = generate_test_bitmap(16384, 12345);
        let b = generate_test_bitmap(16384, 67890);
        let iters = 10_000;

        let mut d = [0u64; 1024];

        // 1. Bitwise AND
        let t0 = Instant::now();
        for _ in 0..iters {
            scalar_bitmap_and(&mut d, &a, &b);
            std::hint::black_box(&d);
        }
        let elapsed_scalar_and = t0.elapsed();

        let t0 = Instant::now();
        for _ in 0..iters {
            bitmap_and(&mut d, &a, &b);
            std::hint::black_box(&d);
        }
        let elapsed_simd_and = t0.elapsed();

        // 2. Popcount
        let mut total_s = 0;
        let t0 = Instant::now();
        for _ in 0..iters {
            total_s += scalar_bitmap_popcount(&a);
        }
        let elapsed_scalar_pop = t0.elapsed();
        std::hint::black_box(total_s);

        let mut total_simd = 0;
        let t0 = Instant::now();
        for _ in 0..iters {
            total_simd += bitmap_popcount(&a);
        }
        let elapsed_simd_pop = t0.elapsed();
        std::hint::black_box(total_simd);

        // 3. AND-Popcount
        let mut total_and_s = 0;
        let t0 = Instant::now();
        for _ in 0..iters {
            total_and_s += scalar_bitmap_and_popcount(&a, &b);
        }
        let elapsed_scalar_and_pop = t0.elapsed();
        std::hint::black_box(total_and_s);

        let mut total_and_simd = 0;
        let t0 = Instant::now();
        for _ in 0..iters {
            total_and_simd += bitmap_and_popcount(&a, &b);
        }
        let elapsed_simd_and_pop = t0.elapsed();
        std::hint::black_box(total_and_simd);

        println!("\n=== MiniRoaring SIMD Microbenchmark (10,000 iterations x 8 KiB) ===");
        println!(
            "bitmap_and:     Scalar {:>8.2?} | SIMD {:>8.2?} | Speedup: {:.2}x",
            elapsed_scalar_and,
            elapsed_simd_and,
            elapsed_scalar_and.as_nanos() as f64 / elapsed_simd_and.as_nanos().max(1) as f64
        );
        println!(
            "bitmap_popcount: Scalar {:>8.2?} | SIMD {:>8.2?} | Speedup: {:.2}x",
            elapsed_scalar_pop,
            elapsed_simd_pop,
            elapsed_scalar_pop.as_nanos() as f64 / elapsed_simd_pop.as_nanos().max(1) as f64
        );
        println!(
            "and_popcount:   Scalar {:>8.2?} | SIMD {:>8.2?} | Speedup: {:.2}x",
            elapsed_scalar_and_pop,
            elapsed_simd_and_pop,
            elapsed_scalar_and_pop.as_nanos() as f64
                / elapsed_simd_and_pop.as_nanos().max(1) as f64
        );
    }
}
