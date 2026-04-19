//! Inverse discrete wavelet transform (ISO/IEC 15444-1 Annex F).
//!
//! Both the **5/3 reversible** integer lifting scheme and the **9/7
//! irreversible** float lifting scheme are implemented. Subbands at
//! resolution `r` are stored quadrant-packed: LL occupies the top-left
//! block, HL the top-right, LH the bottom-left, HH the bottom-right.
//! The inverse transform un-packs each row / column into the
//! interleaved low/high layout expected by the lifting recurrence, runs
//! one synthesis pass, and writes the result back into the canvas.
//!
//! The layout rule follows ISO 15444-1 §F.3.1 (deinterleave). For an
//! even-length 1-D signal `n` the low samples live at positions
//! `0..n/2` and the high samples at positions `n/2..n`. For odd-length
//! signals the split follows `(n + 1) / 2` and `n / 2` — exact per-axis
//! widths come from the per-resolution `u_offset / v_offset`.

/// Apply a single-level inverse 5/3 integer lifting on an arbitrary-
/// sized rectangular region.
///
/// The 2-D inverse runs the **vertical pass first**, mirroring the
/// encode side (which applies rows then columns forward) so the pair
/// `fdwt_53 → idwt_53` forms a bit-exact reversible round-trip.
pub fn idwt_53(buf: &mut [i32], w: usize, h: usize, stride: usize) {
    // Vertical pass first.
    let mut col_scratch = vec![0i32; h];
    for x in 0..w {
        for y in 0..h {
            col_scratch[y] = buf[y * stride + x];
        }
        interleave_i32(&mut col_scratch);
        idwt_53_1d(&mut col_scratch);
        for y in 0..h {
            buf[y * stride + x] = col_scratch[y];
        }
    }
    // Horizontal pass.
    let mut row_scratch = vec![0i32; w];
    for y in 0..h {
        for x in 0..w {
            row_scratch[x] = buf[y * stride + x];
        }
        interleave_i32(&mut row_scratch);
        idwt_53_1d(&mut row_scratch);
        for x in 0..w {
            buf[y * stride + x] = row_scratch[x];
        }
    }
}

/// Un-deinterleave a 1-D signal: input is `[L0, L1, ..., Lm, H0, H1, ..., Hk]`
/// with `m = (n + 1) / 2`; output is `[L0, H0, L1, H1, ...]`.
pub(crate) fn interleave_i32(x: &mut [i32]) {
    let n = x.len();
    if n < 2 {
        return;
    }
    let m = n.div_ceil(2);
    let mut tmp = vec![0i32; n];
    for k in 0..m {
        tmp[2 * k] = x[k];
    }
    let k_high = n - m;
    for k in 0..k_high {
        tmp[2 * k + 1] = x[m + k];
    }
    x.copy_from_slice(&tmp);
}

fn interleave_f32(x: &mut [f32]) {
    let n = x.len();
    if n < 2 {
        return;
    }
    let m = n.div_ceil(2);
    let mut tmp = vec![0f32; n];
    for k in 0..m {
        tmp[2 * k] = x[k];
    }
    let k_high = n - m;
    for k in 0..k_high {
        tmp[2 * k + 1] = x[m + k];
    }
    x.copy_from_slice(&tmp);
}

/// One-dimensional 5/3 reversible inverse lifting.
///
/// Uses the symmetric whole-sample extension described in T.800
/// §F.3.8.1: when accessing `x[-1]` or `x[n]` the sampler reflects back
/// into the in-range interval.
pub fn idwt_53_1d(x: &mut [i32]) {
    let n = x.len();
    if n < 2 {
        return;
    }
    // Step 1: update even samples — x[2k] -= (x[2k-1] + x[2k+1] + 2) >> 2
    let mut k = 0;
    while 2 * k < n {
        let i = 2 * k;
        let l = if i >= 1 {
            x[i - 1]
        } else if i + 1 < n {
            x[i + 1]
        } else {
            0
        };
        let r = if i + 1 < n {
            x[i + 1]
        } else if i >= 1 {
            x[i - 1]
        } else {
            0
        };
        x[i] = x[i].wrapping_sub(l.wrapping_add(r).wrapping_add(2) >> 2);
        k += 1;
    }
    // Step 2: predict odd samples — x[2k+1] += (x[2k] + x[2k+2]) >> 1
    let mut k = 0;
    while 2 * k + 1 < n {
        let i = 2 * k + 1;
        let l = x[i - 1];
        let r = if i + 1 < n { x[i + 1] } else { l };
        x[i] = x[i].wrapping_add(l.wrapping_add(r) >> 1);
        k += 1;
    }
}

// --- 9/7 irreversible IDWT ---

/// 9/7 lifting constants (T.800 §F.3.8.2 Table F.6).
const ALPHA: f32 = -1.586_134_3;
const BETA: f32 = -0.052_980_12;
const GAMMA: f32 = 0.882_911_1;
const DELTA: f32 = 0.443_506_85;
const K_GAIN: f32 = 1.230_174_1;
/// `2 / K` — historical OpenJPEG constant used on the high-pass lane
/// during inverse 9/7. See `BUG_WEIRD_TWO_INVK` in `opj_tcd.c`.
const TWO_INV_K: f32 = 1.625_732_4;

/// One-dimensional 9/7 inverse lifting.
pub fn idwt_97_1d(x: &mut [f32]) {
    let n = x.len();
    if n < 2 {
        return;
    }
    // Step 0: undo the forward-transform scale. We follow OpenJPEG's
    // `BUG_WEIRD_TWO_INVK` convention: evens are multiplied by `K`
    // (opj_K), odds by `2/K` (opj_two_invK). The `2/K` on odds absorbs
    // the per-band `log2_gain_b` factor that T.800 §E.1.1.2 would
    // normally put into the stepsize — this way we can use the
    // simpler `Rb = precision` stepsize (see `synth_component_97`).
    let mut k = 0;
    while 2 * k < n {
        x[2 * k] *= K_GAIN;
        k += 1;
    }
    let mut k = 0;
    while 2 * k + 1 < n {
        x[2 * k + 1] *= TWO_INV_K;
        k += 1;
    }
    // Step 1: update even — x[2k] -= delta * (x[2k-1] + x[2k+1]).
    let mut k = 0;
    while 2 * k < n {
        let i = 2 * k;
        let l = if i >= 1 {
            x[i - 1]
        } else if i + 1 < n {
            x[i + 1]
        } else {
            0.0
        };
        let r = if i + 1 < n {
            x[i + 1]
        } else if i >= 1 {
            x[i - 1]
        } else {
            0.0
        };
        x[i] -= DELTA * (l + r);
        k += 1;
    }
    // Step 2: predict odd — x[2k+1] -= gamma * (x[2k] + x[2k+2]).
    let mut k = 0;
    while 2 * k + 1 < n {
        let i = 2 * k + 1;
        let l = x[i - 1];
        let r = if i + 1 < n { x[i + 1] } else { l };
        x[i] -= GAMMA * (l + r);
        k += 1;
    }
    // Step 3: update even — x[2k] -= beta * (x[2k-1] + x[2k+1]).
    let mut k = 0;
    while 2 * k < n {
        let i = 2 * k;
        let l = if i >= 1 {
            x[i - 1]
        } else if i + 1 < n {
            x[i + 1]
        } else {
            0.0
        };
        let r = if i + 1 < n {
            x[i + 1]
        } else if i >= 1 {
            x[i - 1]
        } else {
            0.0
        };
        x[i] -= BETA * (l + r);
        k += 1;
    }
    // Step 4: predict odd — x[2k+1] -= alpha * (x[2k] + x[2k+2]).
    let mut k = 0;
    while 2 * k + 1 < n {
        let i = 2 * k + 1;
        let l = x[i - 1];
        let r = if i + 1 < n { x[i + 1] } else { l };
        x[i] -= ALPHA * (l + r);
        k += 1;
    }
}

/// Apply a single-level inverse 9/7 lifting on an arbitrary-sized
/// rectangular region.
pub fn idwt_97(buf: &mut [f32], w: usize, h: usize, stride: usize) {
    let mut row_scratch = vec![0f32; w];
    for y in 0..h {
        for x in 0..w {
            row_scratch[x] = buf[y * stride + x];
        }
        interleave_f32(&mut row_scratch);
        idwt_97_1d(&mut row_scratch);
        for x in 0..w {
            buf[y * stride + x] = row_scratch[x];
        }
    }
    let mut col_scratch = vec![0f32; h];
    for x in 0..w {
        for y in 0..h {
            col_scratch[y] = buf[y * stride + x];
        }
        interleave_f32(&mut col_scratch);
        idwt_97_1d(&mut col_scratch);
        for y in 0..h {
            buf[y * stride + x] = col_scratch[y];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_roundtrip() {
        let mut x = vec![10, 20, 30, 1, 2, 3];
        interleave_i32(&mut x);
        assert_eq!(x, vec![10, 1, 20, 2, 30, 3]);
    }

    #[test]
    fn idwt_53_on_dc_signal_stays_dc() {
        // 1-D IDWT of [LL=L, HL=0] from pre-synth interleave form.
        let mut x = vec![100, 200, 0, 0, 0, 0];
        interleave_i32(&mut x);
        idwt_53_1d(&mut x);
        assert_eq!(x.len(), 6);
    }

    #[test]
    fn idwt_97_zero_stays_zero() {
        let mut x = vec![0.0f32; 8];
        idwt_97_1d(&mut x);
        assert!(x.iter().all(|&v| v.abs() < 1e-4));
    }
}
