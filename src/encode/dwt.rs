//! Forward 5/3 integer reversible lifting — ISO/IEC 15444-1 §F.4.8.1.
//!
//! Partner to [`crate::decode::dwt`]. Takes an `i32` canvas and runs
//! the forward discrete wavelet transform in place, leaving the output
//! in the de-interleaved per-band layout (L samples first on each row
//! and each column, then H samples). That is, after one level of
//! analysis on a row of length `n`, the first `ceil(n/2)` entries hold
//! the low-pass samples and the remaining `floor(n/2)` hold the
//! high-pass samples.

/// One-dimensional 5/3 reversible forward lifting.
///
/// Symmetric whole-sample extension matches the decoder; the standard
/// forward pass is:
///
/// ```text
///   d[n] = x[2n+1] - floor((x[2n] + x[2n+2]) / 2)
///   s[n] = x[2n]   + floor((d[n-1] + d[n] + 2) / 4)
/// ```
///
/// where `x[i]` reflects through the boundaries.
pub fn fdwt_53_1d(x: &mut [i32]) {
    let n = x.len();
    if n < 2 {
        return;
    }
    // Step 1: update odd samples (high-pass).
    //   x[2k+1] -= (x[2k] + x[2k+2]) >> 1
    let mut k = 0;
    while 2 * k + 1 < n {
        let i = 2 * k + 1;
        let l = x[i - 1];
        let r = if i + 1 < n { x[i + 1] } else { l };
        x[i] = x[i].wrapping_sub(l.wrapping_add(r) >> 1);
        k += 1;
    }
    // Step 2: predict even samples (low-pass).
    //   x[2k] += (x[2k-1] + x[2k+1] + 2) >> 2
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
        x[i] = x[i].wrapping_add(l.wrapping_add(r).wrapping_add(2) >> 2);
        k += 1;
    }
    // Step 3: deinterleave. Move L samples to [0..m] and H samples to
    // [m..n], where m = ceil(n / 2). This is the mirror of the
    // `interleave_i32` used on decode.
    deinterleave_i32(x);
}

/// Inverse of [`crate::decode::dwt::interleave_i32`]: take an
/// interleaved `[L0, H0, L1, H1, ...]` buffer and rearrange it to
/// `[L0, L1, ..., Lm-1, H0, H1, ...]`.
fn deinterleave_i32(x: &mut [i32]) {
    let n = x.len();
    if n < 2 {
        return;
    }
    let m = n.div_ceil(2);
    let mut tmp = vec![0i32; n];
    for k in 0..m {
        tmp[k] = x[2 * k];
    }
    let k_high = n - m;
    for k in 0..k_high {
        tmp[m + k] = x[2 * k + 1];
    }
    x.copy_from_slice(&tmp);
}

/// Apply a single level of 2-D forward 5/3 lifting to a rectangular
/// region. The output lays low-pass samples in the top-left quadrant,
/// HL in the top-right, LH in the bottom-left, HH in the bottom-right
/// — matching the canvas layout the decoder expects.
///
/// Per T.800 §F.4.2 the 2D_SD procedure applies VER_SD (column pass)
/// first, then HOR_SD (row pass). Matching that axis order is
/// required — the integer 5/3 lifting uses floored divisions that do
/// NOT commute across axes, so a row-first forward DWT produces
/// coefficients that differ from the spec-conformant ordering in the
/// HH sub-band (round 7: HH was the last-mile diff vs OpenJPEG).
///
/// Pairs with [`crate::decode::dwt::idwt_53`] which applies HOR_SR
/// (row) then VER_SR (column) per §F.3.2, so the full round-trip is
/// bit-exact.
pub fn fdwt_53(buf: &mut [i32], w: usize, h: usize, stride: usize) {
    // Column pass first (VER_SD in the spec).
    let mut col = vec![0i32; h];
    for x in 0..w {
        for y in 0..h {
            col[y] = buf[y * stride + x];
        }
        fdwt_53_1d(&mut col);
        for y in 0..h {
            buf[y * stride + x] = col[y];
        }
    }
    // Then row pass (HOR_SD).
    let mut row = vec![0i32; w];
    for y in 0..h {
        for x in 0..w {
            row[x] = buf[y * stride + x];
        }
        fdwt_53_1d(&mut row);
        for x in 0..w {
            buf[y * stride + x] = row[x];
        }
    }
}

// --- 9/7 irreversible forward DWT ---

/// 9/7 lifting constants — must match the decoder's
/// [`crate::decode::dwt`] constants.
const ALPHA: f32 = -1.586_134_3;
const BETA: f32 = -0.052_980_12;
const GAMMA: f32 = 0.882_911_1;
const DELTA: f32 = 0.443_506_85;
const K_GAIN: f32 = 1.230_174_1;
/// `2 / K` — inverse uses this on the odd lane to absorb the per-band
/// gain factor. See `BUG_WEIRD_TWO_INVK` in OpenJPEG's `opj_tcd.c`.
const TWO_INV_K: f32 = 1.625_732_4;

/// One-dimensional 9/7 irreversible forward lifting.
///
/// Inverse of [`crate::decode::dwt::idwt_97_1d`] — applies the five
/// lifting steps in reverse order and finally scales even samples by
/// `1/K` and odd samples by `K/2` (= `1 / (2/K)`). On return the buffer
/// is in the deinterleaved `[L0, L1, ..., Lm-1, H0, H1, ..., Hk-1]`
/// layout.
pub fn fdwt_97_1d(x: &mut [f32]) {
    let n = x.len();
    if n < 2 {
        return;
    }
    // Step 1 (reverse of idwt step 4): predict odd — add ALPHA * even neighbours.
    //   x[2k+1] += alpha * (x[2k] + x[2k+2])
    let mut k = 0;
    while 2 * k + 1 < n {
        let i = 2 * k + 1;
        let l = x[i - 1];
        let r = if i + 1 < n { x[i + 1] } else { l };
        x[i] += ALPHA * (l + r);
        k += 1;
    }
    // Step 2 (reverse of idwt step 3): update even — add BETA * odd neighbours.
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
        x[i] += BETA * (l + r);
        k += 1;
    }
    // Step 3 (reverse of idwt step 2): predict odd — add GAMMA * even neighbours.
    let mut k = 0;
    while 2 * k + 1 < n {
        let i = 2 * k + 1;
        let l = x[i - 1];
        let r = if i + 1 < n { x[i + 1] } else { l };
        x[i] += GAMMA * (l + r);
        k += 1;
    }
    // Step 4 (reverse of idwt step 1): update even — add DELTA * odd neighbours.
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
        x[i] += DELTA * (l + r);
        k += 1;
    }
    // Step 5 (reverse of idwt step 0): scale. Inverse multiplied evens
    // by `K` and odds by `2/K`; we must multiply by the reciprocals.
    let mut k = 0;
    while 2 * k < n {
        x[2 * k] /= K_GAIN;
        k += 1;
    }
    let mut k = 0;
    while 2 * k + 1 < n {
        x[2 * k + 1] /= TWO_INV_K;
        k += 1;
    }
    // Deinterleave: even → low-pass, odd → high-pass.
    deinterleave_f32(x);
}

fn deinterleave_f32(x: &mut [f32]) {
    let n = x.len();
    if n < 2 {
        return;
    }
    let m = n.div_ceil(2);
    let mut tmp = vec![0f32; n];
    for k in 0..m {
        tmp[k] = x[2 * k];
    }
    let k_high = n - m;
    for k in 0..k_high {
        tmp[m + k] = x[2 * k + 1];
    }
    x.copy_from_slice(&tmp);
}

/// Apply one level of 2-D forward 9/7 lifting to a rectangular region.
/// Column pass runs first, mirroring the decoder's row-first inverse —
/// `fdwt_97 → idwt_97` recovers the input (up to float precision).
pub fn fdwt_97(buf: &mut [f32], w: usize, h: usize, stride: usize) {
    // Column pass first (inverse of idwt's final col pass).
    let mut col = vec![0f32; h];
    for x in 0..w {
        for y in 0..h {
            col[y] = buf[y * stride + x];
        }
        fdwt_97_1d(&mut col);
        for y in 0..h {
            buf[y * stride + x] = col[y];
        }
    }
    // Then row pass.
    let mut row = vec![0f32; w];
    for y in 0..h {
        for x in 0..w {
            row[x] = buf[y * stride + x];
        }
        fdwt_97_1d(&mut row);
        for x in 0..w {
            buf[y * stride + x] = row[x];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::dwt::idwt_53_1d;

    #[test]
    fn deinterleave_roundtrip() {
        let mut x = vec![10, 1, 20, 2, 30, 3];
        deinterleave_i32(&mut x);
        assert_eq!(x, vec![10, 20, 30, 1, 2, 3]);
    }

    /// Forward 5/3 followed by inverse 5/3 must reconstruct the
    /// original signal bit-exactly (reversible).
    #[test]
    fn roundtrip_small() {
        let orig = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let mut buf = orig.clone();
        fdwt_53_1d(&mut buf);
        // Interleave back to L/H alternating then run inverse.
        // `idwt_53_1d` expects the interleaved layout; we built the
        // output in deinterleaved layout, so re-interleave first.
        crate::decode::dwt::interleave_i32(&mut buf);
        idwt_53_1d(&mut buf);
        assert_eq!(buf, orig);
    }

    /// 2-D round-trip on a small rectangle.
    #[test]
    fn roundtrip_2d() {
        let w = 8;
        let h = 4;
        let orig: Vec<i32> = (0..(w * h) as i32).collect();
        let mut buf = orig.clone();
        fdwt_53(&mut buf, w, h, w);
        // To invert, we need the single-level inverse. The decoder's
        // 2-D inverse expects the quadrant-packed layout — which is
        // exactly what `fdwt_53` produced — so we can plug it in.
        crate::decode::dwt::idwt_53(&mut buf, w, h, w);
        assert_eq!(buf, orig);
    }

    /// 2-D round-trip at 16x16 on scattered signed integers — covers
    /// the opj16 interop path where our forward DWT was previously
    /// applying the row pass before the column pass.
    #[test]
    fn roundtrip_2d_16x16() {
        let w = 16;
        let h = 16;
        let orig: Vec<i32> = (0..(w * h) as i32).map(|i| (i * 7) % 255 - 127).collect();
        let mut buf = orig.clone();
        fdwt_53(&mut buf, w, h, w);
        crate::decode::dwt::idwt_53(&mut buf, w, h, w);
        assert_eq!(buf, orig);
    }

    /// Forward-then-inverse on the opj16 test PGM must return the
    /// original DC-shifted values bit-exactly. If this diverges, the
    /// fdwt/idwt axis ordering is out of sync with the spec and every
    /// interop fixture mismatches by ±1 LSB in scattered samples.
    #[test]
    fn roundtrip_opj16_pgm() {
        // DC-shifted reference samples for the 16x16 testsrc PGM.
        let pgm = include_bytes!("../../tests/fixtures/opj16.pgm");
        // Skip PGM (P5) header: 3 newlines then payload.
        let mut i = 0;
        let mut nl = 0;
        while i < pgm.len() && nl < 3 {
            if pgm[i] == b'\n' {
                nl += 1;
            }
            i += 1;
        }
        let orig: Vec<i32> = pgm[i..].iter().map(|&b| b as i32 - 128).collect();
        assert_eq!(orig.len(), 16 * 16);
        let mut buf = orig.clone();
        fdwt_53(&mut buf, 16, 16, 16);
        crate::decode::dwt::idwt_53(&mut buf, 16, 16, 16);
        assert_eq!(buf, orig);
    }

    /// Inverse-then-forward on a synthetic sub-band canvas must return
    /// the original values bit-exactly. Pairs with `roundtrip_opj16_pgm`
    /// to guarantee `fdwt_53` is exactly the inverse of `idwt_53` in
    /// both directions — which is what the decoder relies on when
    /// reconstructing OpenJPEG-generated fixtures.
    #[test]
    fn reverse_roundtrip_16x16() {
        let w = 16;
        let h = 16;
        let orig: Vec<i32> = (0..(w * h) as i32).map(|i| (i * 13) % 301 - 151).collect();
        let mut buf = orig.clone();
        crate::decode::dwt::idwt_53(&mut buf, w, h, w);
        fdwt_53(&mut buf, w, h, w);
        assert_eq!(buf, orig);
    }

    /// Multiple sizes: ensure the col-first forward / row-first inverse
    /// pair round-trips for every resolution the multi-level decoder
    /// walks up through (2x2, 4x4, 8x8, 16x16, 32x32, 64x64).
    #[test]
    fn roundtrip_power_of_two_sizes() {
        for n in [2usize, 4, 8, 16, 32, 64] {
            let orig: Vec<i32> = (0..(n * n) as i32).map(|i| (i * 37) % 251 - 127).collect();
            let mut buf = orig.clone();
            fdwt_53(&mut buf, n, n, n);
            crate::decode::dwt::idwt_53(&mut buf, n, n, n);
            assert_eq!(buf, orig, "round-trip failed at size {n}x{n}");
        }
    }

    /// 2-D round-trip of a simple ramp in X direction.
    #[test]
    fn roundtrip_x_ramp_2d() {
        let w = 8;
        let h = 4;
        let mut orig = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                orig.push(x as i32 * 10 + y as i32);
            }
        }
        let mut buf = orig.clone();
        fdwt_53(&mut buf, w, h, w);
        crate::decode::dwt::idwt_53(&mut buf, w, h, w);
        for i in 0..orig.len() {
            assert_eq!(
                buf[i],
                orig[i],
                "ramp mismatch at ({}, {}): orig {}, got {}",
                i % w,
                i / w,
                orig[i],
                buf[i]
            );
        }
    }

    /// 2-D round-trip on a gradient (the round-trip failure case from
    /// the top-level encoder test).
    #[test]
    fn roundtrip_gradient_2d() {
        let w = 8;
        let h = 8;
        let mut orig = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let v = ((x + y) * 255 / (w + h - 2)).min(255) as i32 - 128;
                orig.push(v);
            }
        }
        let mut buf = orig.clone();
        fdwt_53(&mut buf, w, h, w);
        // Apply a second forward (if the encoder does multiple levels,
        // but not relevant here — we only test single level).
        // Then the exact inverse:
        crate::decode::dwt::idwt_53(&mut buf, w, h, w);
        for i in 0..orig.len() {
            assert_eq!(
                buf[i],
                orig[i],
                "mismatch at ({}, {}): orig {}, got {}",
                i % w,
                i / w,
                orig[i],
                buf[i]
            );
        }
    }

    /// 2-D round-trip with explicit HOR-then-VER inverse (matches
    /// `idwt_53` after the round-7 spec-conformant axis swap). `fdwt_53`
    /// applies the column pass first per §F.4.2; the matching inverse
    /// applies HOR_SR then VER_SR per §F.3.2.
    #[test]
    fn roundtrip_gradient_row_first_inv() {
        let w = 8;
        let h = 8;
        let mut orig = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let v = ((x + y) * 255 / (w + h - 2)).min(255) as i32 - 128;
                orig.push(v);
            }
        }
        let mut buf = orig.clone();
        fdwt_53(&mut buf, w, h, w);
        // Row-first inverse (matches `idwt_53`).
        let mut row = vec![0i32; w];
        for y in 0..h {
            for x in 0..w {
                row[x] = buf[y * w + x];
            }
            crate::decode::dwt::interleave_i32(&mut row);
            crate::decode::dwt::idwt_53_1d(&mut row);
            for x in 0..w {
                buf[y * w + x] = row[x];
            }
        }
        let mut col = vec![0i32; h];
        for x in 0..w {
            for y in 0..h {
                col[y] = buf[y * w + x];
            }
            crate::decode::dwt::interleave_i32(&mut col);
            crate::decode::dwt::idwt_53_1d(&mut col);
            for y in 0..h {
                buf[y * w + x] = col[y];
            }
        }
        for i in 0..orig.len() {
            assert_eq!(
                buf[i],
                orig[i],
                "row-first-inv mismatch at ({}, {}): orig {}, got {}",
                i % w,
                i / w,
                orig[i],
                buf[i]
            );
        }
    }

    /// 1-D forward then inverse on a single gradient row should be
    /// bit-exact.
    #[test]
    fn roundtrip_gradient_1d() {
        let orig = vec![-128i32, -110, -92, -74, -56, -37, -19, -1];
        let mut buf = orig.clone();
        fdwt_53_1d(&mut buf);
        // Re-interleave and invert.
        crate::decode::dwt::interleave_i32(&mut buf);
        crate::decode::dwt::idwt_53_1d(&mut buf);
        assert_eq!(buf, orig);
    }

    /// Forward 9/7 followed by inverse 9/7 reconstructs the original
    /// signal to within float precision.
    #[test]
    fn roundtrip_97_1d() {
        let orig: Vec<f32> = (0..16).map(|i| (i as f32) * 3.0 - 10.0).collect();
        let mut buf = orig.clone();
        fdwt_97_1d(&mut buf);
        // Reinterleave for the inverse (inverse expects the interleaved
        // `[L0, H0, L1, H1, ...]` layout).
        let mut interleaved = vec![0f32; buf.len()];
        let m = buf.len().div_ceil(2);
        for k in 0..m {
            interleaved[2 * k] = buf[k];
        }
        let k_high = buf.len() - m;
        for k in 0..k_high {
            interleaved[2 * k + 1] = buf[m + k];
        }
        crate::decode::dwt::idwt_97_1d(&mut interleaved);
        for i in 0..orig.len() {
            assert!(
                (interleaved[i] - orig[i]).abs() < 1e-3,
                "9/7 1D roundtrip mismatch at {}: orig={}, got={}",
                i,
                orig[i],
                interleaved[i]
            );
        }
    }

    /// 2-D 9/7 roundtrip on a small rectangle.
    #[test]
    fn roundtrip_97_2d() {
        let w = 8;
        let h = 8;
        let orig: Vec<f32> = (0..(w * h) as i32).map(|v| v as f32).collect();
        let mut buf = orig.clone();
        fdwt_97(&mut buf, w, h, w);
        crate::decode::dwt::idwt_97(&mut buf, w, h, w);
        for i in 0..orig.len() {
            assert!(
                (buf[i] - orig[i]).abs() < 1e-2,
                "9/7 2D roundtrip mismatch at {}: orig={}, got={}",
                i,
                orig[i],
                buf[i]
            );
        }
    }

    /// Row ∘ Col vs Col ∘ Row should give the same result (separable
    /// DWT commutes).
    #[test]
    fn forward_col_row_vs_row_col() {
        let w = 4;
        let h = 4;
        let orig = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        // Row then col:
        let mut rc = orig.clone();
        let mut r = vec![0i32; w];
        for y in 0..h {
            for x in 0..w {
                r[x] = rc[y * w + x];
            }
            fdwt_53_1d(&mut r);
            for x in 0..w {
                rc[y * w + x] = r[x];
            }
        }
        let mut c = vec![0i32; h];
        for x in 0..w {
            for y in 0..h {
                c[y] = rc[y * w + x];
            }
            fdwt_53_1d(&mut c);
            for y in 0..h {
                rc[y * w + x] = c[y];
            }
        }
        // Col then row:
        let mut cr = orig.clone();
        let mut c2 = vec![0i32; h];
        for x in 0..w {
            for y in 0..h {
                c2[y] = cr[y * w + x];
            }
            fdwt_53_1d(&mut c2);
            for y in 0..h {
                cr[y * w + x] = c2[y];
            }
        }
        let mut r2 = vec![0i32; w];
        for y in 0..h {
            for x in 0..w {
                r2[x] = cr[y * w + x];
            }
            fdwt_53_1d(&mut r2);
            for x in 0..w {
                cr[y * w + x] = r2[x];
            }
        }
        assert_eq!(rc, cr, "separable transforms should commute");
    }
}
