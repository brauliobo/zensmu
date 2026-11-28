use super::BenchWorkload;
use std::hint::black_box;

/// 64-byte cache line = 8 x `u64`. 4096 cache lines = 256 KiB (exceeds Zen L1D
/// while heavily exercising L1D eviction/refill and L2 cache lines).
const CACHE_LINES: usize = 4096;
const WORDS_PER_LINE: usize = 8;
pub(super) const CACHE_BUFFER_WORDS: usize = CACHE_LINES * WORDS_PER_LINE;

#[inline]
fn feistel_round(x: u64, key: u64) -> u64 {
    let mut v = x.wrapping_add(key);
    v = (v ^ (v >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    v ^ (v >> 31) ^ v.rotate_left(13)
}

/// Multi-lane 64-bit integer ALU/IMUL/bit-mix stress using a bijective Feistel
/// network: runs 1024 forward rounds across two independent 128-bit pairs and
/// unwinds 1024 inverse rounds, verifying bit-exact recovery of the initial state.
fn run_integer_batch(batch_idx: u64) -> std::result::Result<u64, String> {
    const ROUNDS: u64 = 1024;
    let seed = batch_idx.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xA5A5_5A5A_F00F_0FF0;

    let init_l0 = black_box(seed ^ 0x0123_4567_89AB_CDEF);
    let init_r0 = black_box(seed.rotate_left(29) ^ 0xFEDC_BA98_7654_3210);
    let init_l1 = black_box(seed.wrapping_add(0x517C_C1B7_2722_0A95));
    let init_r1 = black_box(seed.rotate_right(19) ^ 0x6C62_272E_07BB_0142);

    let (mut l0, mut r0) = (init_l0, init_r0);
    let (mut l1, mut r1) = (init_l1, init_r1);

    for round in 0..ROUNDS {
        let k0 = seed.wrapping_add(round.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let k1 = k0 ^ 0xD1B5_4A32_D192_ED03;

        let next_r0 = l0 ^ feistel_round(r0, k0);
        l0 = r0;
        r0 = next_r0;

        let next_r1 = l1 ^ feistel_round(r1, k1);
        l1 = r1;
        r1 = next_r1;
    }

    // Optimization barrier preventing the compiler from cancelling forward+inverse rounds.
    l0 = black_box(l0);
    r0 = black_box(r0);
    l1 = black_box(l1);
    r1 = black_box(r1);
    let digest = l0 ^ r0.rotate_left(17) ^ l1.rotate_left(31) ^ r1.rotate_left(47);

    for round in (0..ROUNDS).rev() {
        let k0 = seed.wrapping_add(round.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let k1 = k0 ^ 0xD1B5_4A32_D192_ED03;

        let prev_l0 = r0 ^ feistel_round(l0, k0);
        r0 = l0;
        l0 = prev_l0;

        let prev_l1 = r1 ^ feistel_round(l1, k1);
        r1 = l1;
        l1 = prev_l1;
    }

    if l0 != init_l0 || r0 != init_r0 || l1 != init_l1 || r1 != init_r1 {
        return Err(format!(
            "integer ALU self-check mismatch on batch {batch_idx}: \
             expected ({init_l0:#018x}, {init_r0:#018x}, {init_l1:#018x}, {init_r1:#018x}), \
             got ({l0:#018x}, {r0:#018x}, {l1:#018x}, {r1:#018x})"
        ));
    }

    Ok(black_box(digest))
}

fn fp_fma_pass(seed_idx: u64) -> [f64; 8] {
    const STEPS: usize = 512;
    let base = ((seed_idx & 0xFF) as f64 + 1.0) * 0.003_906_25;
    let mut acc: [f64; 8] = [
        black_box(1.000_000_1 + base),
        black_box(0.999_999_7 - base * 0.5),
        black_box(1.141_592_6 + base * 0.25),
        black_box(0.718_281_8 + base * 0.125),
        black_box(std::f64::consts::SQRT_2 - base * 0.0625),
        black_box(0.618_033_9 + base * 0.375),
        black_box(1.732_050_8 - base * 0.1875),
        black_box(0.866_025_4 + base * 0.3125),
    ];
    let mul: [f64; 8] = [
        0.999_877_929_687_5,
        0.999_755_859_375,
        0.999_633_789_062_5,
        0.999_511_718_75,
        0.999_389_648_437_5,
        0.999_267_578_125,
        0.999_145_507_812_5,
        0.999_023_437_5,
    ];
    let add: [f64; 8] = [
        0.000_317_382_812_5,
        0.000_421_142_578_125,
        0.000_518_798_828_125,
        0.000_610_351_562_5,
        0.000_701_904_296_875,
        0.000_793_457_031_25,
        0.000_885_009_765_625,
        0.000_976_562_5,
    ];

    for step in 0..STEPS {
        let bias = ((step & 7) as f64) * 0.000_122_070_312_5;
        for i in 0..8 {
            let next = acc[i].mul_add(mul[i], add[i] + bias);
            let neighbor = acc[(i + 1) & 7];
            acc[i] = next.mul_add(0.999_5, neighbor * 0.000_5);
        }
    }

    black_box(acc)
}

/// Multi-lane IEEE-754 FP64 FMA stress with dual-execution bit-exact verification
/// plus an exact closed-form dyadic FMA series check.
fn run_fp_batch(batch_idx: u64) -> std::result::Result<u64, String> {
    let pass_a = fp_fma_pass(black_box(batch_idx));
    let pass_b = fp_fma_pass(black_box(batch_idx));

    let mut digest = 0u64;
    for (lane, (&a, &b)) in pass_a.iter().zip(pass_b.iter()).enumerate() {
        if !a.is_finite() || !b.is_finite() || a.to_bits() != b.to_bits() {
            return Err(format!(
                "FP64/FMA self-check mismatch on batch {batch_idx} lane {lane}: \
                 pass_a={a:?} ({:#018x}) vs pass_b={b:?} ({:#018x})",
                a.to_bits(),
                b.to_bits()
            ));
        }
        digest ^= a.to_bits().rotate_left((lane as u32) * 7);
    }

    // Exact dyadic FMA series: sum_{k=0..1024} (k * slope + 0.5)
    // = slope * (1023 * 1024 / 2) + 512.0 = slope * 523_776.0 + 512.0.
    // Every intermediate term is an exact half-integer well below 2^52.
    const EXACT_N: usize = 1024;
    let slope = black_box(((batch_idx & 7) + 1) as f64);
    let mut exact_sum = black_box(0.0_f64);
    for k in 0..EXACT_N {
        exact_sum = (k as f64).mul_add(slope, exact_sum + 0.5);
    }
    let expected_exact = slope * 523_776.0 + 512.0;
    if exact_sum.to_bits() != expected_exact.to_bits() {
        return Err(format!(
            "FP64 exact dyadic FMA self-check mismatch on batch {batch_idx}: \
             expected {expected_exact} ({:#018x}), got {exact_sum} ({:#018x})",
            expected_exact.to_bits(),
            exact_sum.to_bits()
        ));
    }

    Ok(black_box(digest ^ exact_sum.to_bits()))
}

#[inline]
fn cache_word_pattern(idx: usize, seed: u64) -> u64 {
    let x = (idx as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(seed);
    let y = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    y ^ (y >> 27)
}

/// Prime-strided 64-byte cache-line L1/L2 write -> read-modify-write -> verify pass.
fn run_cache_batch(buf: &mut [u64], batch_idx: u64) -> std::result::Result<u64, String> {
    if buf.len() != CACHE_BUFFER_WORDS {
        return Err(format!(
            "internal cache buffer size mismatch: expected {CACHE_BUFFER_WORDS}, got {}",
            buf.len()
        ));
    }

    const WRITE_STRIDE: usize = 131;
    const RMW_STRIDE: usize = 257;
    const VERIFY_STRIDE: usize = 521;
    const LINE_MASK: usize = CACHE_LINES - 1;

    let seed = batch_idx
        .wrapping_mul(0xD1B5_4A32_D192_ED03)
        .wrapping_add(0x6A09_E667_F3BC_C908);
    let xor_mask = seed.rotate_left(23) ^ 0xA55A_5AA5_C33C_3CC3;

    // Pass 1: prime-strided cache-line writes
    for step in 0..CACHE_LINES {
        let line = step.wrapping_mul(WRITE_STRIDE) & LINE_MASK;
        let base = line * WORDS_PER_LINE;
        for w in 0..WORDS_PER_LINE {
            let idx = base + w;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = cache_word_pattern(idx, seed);
            }
        }
    }
    black_box(buf.as_mut_ptr());

    // Pass 2: prime-strided read-modify-write in a different permutation
    for step in 0..CACHE_LINES {
        let line = step.wrapping_mul(RMW_STRIDE) & LINE_MASK;
        let base = line * WORDS_PER_LINE;
        for w in 0..WORDS_PER_LINE {
            let idx = base + w;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = slot.rotate_left(19) ^ xor_mask;
            }
        }
    }
    black_box(buf.as_ptr());

    // Pass 3: prime-strided full readback and bit-exact verification
    let mut digest = 0u64;
    for step in 0..CACHE_LINES {
        let line = step.wrapping_mul(VERIFY_STRIDE) & LINE_MASK;
        let base = line * WORDS_PER_LINE;
        for w in 0..WORDS_PER_LINE {
            let idx = base + w;
            let expected = cache_word_pattern(idx, seed).rotate_left(19) ^ xor_mask;
            let actual = buf.get(idx).copied().unwrap_or(0);
            if actual != expected {
                return Err(format!(
                    "L1/L2 cache self-check mismatch on batch {batch_idx} at word {idx}: \
                     expected {expected:#018x}, got {actual:#018x}"
                ));
            }
            digest ^= actual.rotate_left((idx & 63) as u32);
        }
    }

    Ok(black_box(digest))
}

pub(super) fn run_workload_batch(
    workload: BenchWorkload,
    cache_buf: &mut [u64],
    batch_idx: u64,
) -> std::result::Result<u64, String> {
    match workload {
        BenchWorkload::Integer => run_integer_batch(batch_idx),
        BenchWorkload::FloatingPoint => run_fp_batch(batch_idx),
        BenchWorkload::Cache => run_cache_batch(cache_buf, batch_idx),
        BenchWorkload::Mixed => {
            let d_int = run_integer_batch(batch_idx)?;
            let d_fp = run_fp_batch(batch_idx)?;
            let d_cache = run_cache_batch(cache_buf, batch_idx)?;
            Ok(black_box(
                d_int ^ d_fp.rotate_left(11) ^ d_cache.rotate_left(29),
            ))
        }
    }
}
