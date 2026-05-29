//! Inverse discrete wavelet transform — T.800 Annex F (§F.3).
//!
//! This module implements the **decoder-side** sub-band reconstruction
//! procedure that lifts the de-quantised wavelet coefficients
//! (`Rqb(u, v)`, the output of [`crate::dequant`]) back into image-
//! domain samples for a tile-component. It covers exactly the
//! normative §F.3 path:
//!
//! * **§F.3.1 — The IDWT procedure.** Iterate `2D_SR` over the
//!   `lev = NL .. 1` decomposition levels, each step folding the four
//!   sub-bands `(levLL, levHL, levLH, levHH)` into the next level's
//!   `(lev - 1)LL` band, until `0LL` (the reconstructed tile
//!   component) is obtained.
//! * **§F.3.2 — The 2D_SR procedure.** `2D_INTERLEAVE` followed by
//!   `HOR_SR` followed by `VER_SR`, producing the reconstructed
//!   `(lev - 1)LL` two-dimensional array.
//! * **§F.3.3 — The 2D_INTERLEAVE procedure.** Place the four sub-
//!   band coefficients on the even/odd sample-grid lattice so that
//!   `a(2u,   2v)   = aLL(u, v)`,
//!   `a(2u+1, 2v)   = aHL(u, v)`,
//!   `a(2u,   2v+1) = aLH(u, v)`,
//!   `a(2u+1, 2v+1) = aHH(u, v)`.
//! * **§F.3.4 + §F.3.5 — The HOR_SR / VER_SR procedures.** Apply the
//!   1D sub-band reconstruction along every row / every column of
//!   the interleaved array.
//! * **§F.3.6 — The 1D_SR procedure.** Length-one short-circuit
//!   (`X(i0) = Y(i0)` if `i0` is even, `X(i0) = Y(i0)/2` if odd) plus
//!   the length-≥-2 `1D_EXTR` → `1D_FILTR` pipeline.
//! * **§F.3.7 — The 1D_EXTR procedure (periodic symmetric
//!   extension).** Equation F-3 `Yext(i) = Y(PSEO(i, i0, il))` with
//!   the closed-form PSEO of Equation F-4, plus the minimum
//!   extension parameters of Tables F.2 and F.3 (`ileft5-3`,
//!   `iright5-3`, `ileft9-7`, `iright9-7` keyed on the parity of
//!   `i0` and `il`).
//! * **§F.3.8.1 — The 1D_FILTR5-3R (reversible) procedure.** The two-
//!   step lifting of Equations F-5 and F-6 with the integer-rounding
//!   `⌊·/4⌋` / `⌊·/2⌋` divisions (`>> 2` / `>> 1` on floored ints
//!   per the §F prologue's "all divisions round toward minus
//!   infinity" convention).
//! * **§F.3.8.2 — The 1D_FILTR9-7I (irreversible) procedure.** The
//!   six-step lifting of Equation F-7 with the parameters
//!   `(α, β, γ, δ, K)` of Table F.4.
//!
//! ## What this module does NOT cover
//!
//! * **The forward (encoder) DWT.** §F.4 specifies the informative
//!   forward procedure; the decoder doesn't need it. A follow-up
//!   round can mirror this surface with an `fdwt_*` family if /
//!   when the encoder path is wired up.
//! * **MCT.** The multi-component transform (Annex G) is a separate
//!   later round.
//! * **Bit-depth de-scaling.** Annex G's reverse DC-level shift
//!   takes the reconstructed coefficients of the LL band at
//!   `lev = 0` and adds `2^(RI - 1)` for unsigned components; that
//!   step is the dequant / MCT round's responsibility, not the
//!   DWT's.
//! * **Codeblock → sub-band reassembly.** The reconstructed
//!   coefficients delivered to this module are assumed to already
//!   be tier-2-assembled into `(u, v)` rectangular sub-band arrays
//!   keyed by [`crate::geometry::SubBandOrientation`]. The bridge
//!   from `t1::CodeBlock` outputs to these `SubBand` arrays is a
//!   separate later round.
//!
//! ## Clean-room provenance
//!
//! Implemented solely from
//! `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf` Annex F (§F.3
//! prologue; §F.3.1 IDWT; §F.3.2 2D_SR; §F.3.3 2D_INTERLEAVE; §F.3.4
//! HOR_SR; §F.3.5 VER_SR; §F.3.6 1D_SR + length-one parity rule;
//! §F.3.7 1D_EXTR + Equations F-3 / F-4 + Tables F.2 / F.3; §F.3.8.1
//! 1D_FILTR5-3R + Equations F-5 / F-6; §F.3.8.2 1D_FILTR9-7I +
//! Equation F-7 + Table F.4 lifting parameters). No external
//! library source — OpenJPEG, OpenJPH, Kakadu, FFmpeg, libavcodec,
//! jpeg2000-rs, etc. — was consulted, quoted, paraphrased, or used
//! as a cross-check oracle. No WebSearch / WebFetch was used for
//! any reason.
//!
//! ## Numerical model
//!
//! The 5-3 reversible path is an integer-in / integer-out lifting
//! filter; this module operates on `i32` arrays directly.
//!
//! The 9-7 irreversible path is a real-valued lifting filter; this
//! module operates on `f64` arrays. The Table F.4 parameters are
//! the **approximate-value** decimal expansions transcribed
//! verbatim from the spec; that's the precision the standard prints
//! and matches the §F prologue's "approximate value" column.

#![allow(clippy::needless_range_loop)]
#![allow(clippy::manual_div_ceil)]

use crate::geometry::SubBandOrientation;
use crate::Error;

// =====================================================================
// §F.3.8.2.1 — Lifting parameters for the 9-7 irreversible filter.
// =====================================================================

/// Table F.4 — `α` for the 9-7 irreversible filter (`-g4 / g3`,
/// approximate value `-1.586 134 342 059 924`).
pub const ALPHA_9X7: f64 = -1.586_134_342_059_924;

/// Table F.4 — `β` for the 9-7 irreversible filter (`g3 / r1`,
/// approximate value `-0.052 980 118 572 961`).
pub const BETA_9X7: f64 = -0.052_980_118_572_961;

/// Table F.4 — `γ` for the 9-7 irreversible filter (`r1 / s0`,
/// approximate value `0.882 911 075 530 934`).
pub const GAMMA_9X7: f64 = 0.882_911_075_530_934;

/// Table F.4 — `δ` for the 9-7 irreversible filter (`s0 / t0`,
/// approximate value `0.443 506 852 043 971`).
pub const DELTA_9X7: f64 = 0.443_506_852_043_971;

/// Table F.4 — `K` scaling parameter for the 9-7 irreversible
/// filter (`1 / t0`, approximate value `1.230 174 104 914 001`).
pub const K_9X7: f64 = 1.230_174_104_914_001;

// =====================================================================
// §F.3.7 — Periodic symmetric extension.
// =====================================================================

/// §F.3.7 Equation F-4 — `PSEO(i, i0, il)`.
///
/// The closed-form periodic-symmetric-extension index of T.800
/// Equation F-4:
///
/// ```text
///   PSEO(i, i0, il) = i0 + min( mod(i - i0, 2*(il - i0 - 1)),
///                               2*(il - i0 - 1) - mod(i - i0,
///                                                     2*(il - i0 - 1)) )
/// ```
///
/// `i0` is the index of the first signal coefficient and `il` is
/// the index immediately following the last (so the signal has
/// `il - i0` coefficients).
///
/// The function returns a valid in-bounds index in `i0..il` for
/// any `i: i32`; this is the §F.3.7 "extension of the signal with
/// the signal coefficients obtained by a reflection" mapping
/// generalised to indices arbitrarily far outside the signal
/// (Annex F notes that this is required when higher decomposition
/// levels are involved).
///
/// # Panics
///
/// Panics if `i0 >= il` (an empty or inverted signal range is a
/// programmer error — the §F.3.6 length-one short-circuit
/// guarantees the filter never sees this).
pub fn pseo(i: i32, i0: i32, il: i32) -> i32 {
    assert!(
        i0 < il,
        "pseo: signal range i0={} il={} must satisfy i0 < il",
        i0,
        il
    );
    let len = il - i0;
    if len == 1 {
        // Degenerate single-coefficient signal; the §F.3.6 1D_SR
        // length-one rule short-circuits before extension is
        // applied, so any sensible answer suffices. Return i0.
        return i0;
    }
    let period = 2 * (len - 1);
    // Euclidean mod into [0, period).
    let raw = (i - i0).rem_euclid(period);
    let folded = raw.min(period - raw);
    i0 + folded
}

/// §F.3.7 Tables F.2 and F.3 — minimum extension parameters for
/// the 5-3 reversible filter, keyed on the parity of `i0` and `il`.
///
/// Returns `(ileft, iright)`.
pub fn extension_amounts_5x3(i0: i32, il: i32) -> (i32, i32) {
    let ileft = if i0.rem_euclid(2) == 0 { 1 } else { 2 };
    let iright = if il.rem_euclid(2) == 1 { 1 } else { 2 };
    (ileft, iright)
}

/// §F.3.7 Tables F.2 and F.3 — minimum extension parameters for
/// the 9-7 irreversible filter, keyed on the parity of `i0` and
/// `il`.
///
/// Returns `(ileft, iright)`.
pub fn extension_amounts_9x7(i0: i32, il: i32) -> (i32, i32) {
    let ileft = if i0.rem_euclid(2) == 0 { 3 } else { 4 };
    let iright = if il.rem_euclid(2) == 1 { 3 } else { 4 };
    (ileft, iright)
}

// =====================================================================
// §F.3.8.1 — The 1D_FILTR5-3R procedure.
// =====================================================================

/// §F.3.6 1D_SR — length-one short-circuit, 5-3 reversible.
///
/// For `i0 == il - 1` (single coefficient): `X(i0) = Y(i0)` if `i0`
/// is even (the lone coefficient is an `LL` sample), and
/// `X(i0) = Y(i0) / 2` if `i0` is odd (the lone coefficient is an
/// `HL` / `LH` / `HH` sample whose lifting partner is absent). The
/// division is floor-rounded per the §F prologue convention.
fn length_one_5x3(y_i0: i32, i0: i32) -> i32 {
    if i0.rem_euclid(2) == 0 {
        y_i0
    } else {
        // Spec: "X(i0) to Y(i0)/2"; we use floor division which
        // matches the §F prologue's round-toward-minus-infinity
        // convention. `i32::div_euclid` for x >= 0 == standard
        // floor; for negatives it still floors.
        y_i0.div_euclid(2)
    }
}

/// §F.3.6 + §F.3.7 + §F.3.8.1 — full 1D_SR for the 5-3 reversible
/// filter.
///
/// Takes the signal `y[i0..il]` (i.e. coefficient index `i0 + k`
/// lives in `y[k]` for `k = 0 .. il - i0`) and writes the inverse-
/// filtered output to `x[i0..il]` (same layout).
///
/// The internal extension uses the closed-form [`pseo`] of
/// Equation F-4 so the lifting steps can fetch arbitrary out-of-
/// range indices without materialising an extended buffer.
///
/// # Errors
///
/// Returns [`Error::InvalidMarkerLength`] if `y.len() != il - i0`
/// or `x.len() != il - i0` (length-mismatch is a caller bug, but
/// surfacing it as a typed Error keeps the §F.3.6 boundary tidy).
pub fn idwt_1d_5x3(y: &[i32], x: &mut [i32], i0: i32, il: i32) -> Result<(), Error> {
    let len = (il - i0) as usize;
    if y.len() != len || x.len() != len || i0 >= il {
        return Err(Error::InvalidMarkerLength);
    }
    // §F.3.6 length-one branch.
    if len == 1 {
        x[0] = length_one_5x3(y[0], i0);
        return Ok(());
    }
    // Index helper: returns the value of Yext at coefficient index `i`.
    let yext = |i: i32| -> i32 { y[(pseo(i, i0, il) - i0) as usize] };
    // §F.3.8.1 Equation F-5 — even-index outputs:
    //     X(2n) = Yext(2n) - ⌊ (Yext(2n - 1) + Yext(2n + 1) + 2) / 4 ⌋
    //                                                     for ⌈i0/2⌉ ≤ n < ⌈il/2⌉
    let n_lo_even = div_floor(i0, 2); // ⌊i0/2⌋
    let n_hi_even = div_floor(il + 1, 2); // ⌈il/2⌉
    let mut n = n_lo_even;
    while n < n_hi_even {
        let two_n = 2 * n;
        let v = yext(two_n) - div_floor(yext(two_n - 1) + yext(two_n + 1) + 2, 4);
        if two_n >= i0 && two_n < il {
            x[(two_n - i0) as usize] = v;
        }
        n += 1;
    }
    // §F.3.8.1 Equation F-6 — odd-index outputs:
    //     X(2n + 1) = Yext(2n + 1) + ⌊ (X(2n) + X(2n + 2)) / 2 ⌋
    //                                                     for ⌊i0/2⌋ ≤ n < ⌊il/2⌋
    let n_lo_odd = div_floor(i0, 2);
    let n_hi_odd = div_floor(il, 2);
    let xext = |x_slice: &[i32], i: i32| -> i32 {
        // Even-indexed X values are valid in the array slot
        // pseo(i, i0, il); odd values haven't been written yet,
        // but the §F.3.8.1 equation only ever references X(2n)
        // and X(2n + 2), both even, so the lookup is safe.
        x_slice[(pseo(i, i0, il) - i0) as usize]
    };
    let mut n = n_lo_odd;
    while n < n_hi_odd {
        let two_n = 2 * n;
        let v = yext(two_n + 1) + div_floor(xext(x, two_n) + xext(x, two_n + 2), 2);
        if two_n + 1 >= i0 && two_n + 1 < il {
            x[(two_n + 1 - i0) as usize] = v;
        }
        n += 1;
    }
    Ok(())
}

// =====================================================================
// §F.3.8.2 — The 1D_FILTR9-7I procedure.
// =====================================================================

/// §F.3.6 length-one short-circuit, 9-7 irreversible (real-valued).
fn length_one_9x7(y_i0: f64, i0: i32) -> f64 {
    if i0.rem_euclid(2) == 0 {
        y_i0
    } else {
        y_i0 / 2.0
    }
}

/// §F.3.6 + §F.3.7 + §F.3.8.2 — full 1D_SR for the 9-7 irreversible
/// filter.
///
/// Takes the signal `y[i0..il]` and writes the inverse-filtered
/// output to `x[i0..il]`.
///
/// The lifting steps of Equation F-7 are applied in the spec-
/// mandated order STEP1 → STEP2 → STEP3 → STEP4 → STEP5 → STEP6,
/// each over its own index range. The implementation materialises
/// a working buffer of length `(il - i0) + ileft + iright`, where
/// `(ileft, iright)` come from Table F.3 (`9-7` row). Index
/// arithmetic on this buffer is done in extended-index space `j =
/// i + ileft - i0`, so `buf[j]` corresponds to coefficient index
/// `i = j + i0 - ileft`. All in-range writes are then copied out
/// to `x`.
///
/// # Errors
///
/// Returns [`Error::InvalidMarkerLength`] if `y.len() != il - i0`
/// or `x.len() != il - i0` or `i0 >= il`.
pub fn idwt_1d_9x7(y: &[f64], x: &mut [f64], i0: i32, il: i32) -> Result<(), Error> {
    let len = (il - i0) as usize;
    if y.len() != len || x.len() != len || i0 >= il {
        return Err(Error::InvalidMarkerLength);
    }
    if len == 1 {
        x[0] = length_one_9x7(y[0], i0);
        return Ok(());
    }
    // Table F.3 minimum extensions guarantee the OUTPUT X(k) for
    // `i0 ≤ k < il` is correct, but the §F.3.8.2 prose's
    // intermediate ranges access indices up to `2*⌈il/2⌉ + 3` on
    // the right and as low as `2*(⌊i0/2⌋ - 2) - 1` on the left.
    // We size the working buffer to those bounds, which always
    // exceed Table F.3's `iright = 3` / `ileft = 3` minimums and
    // pass the §F.3.7 "Values equal to or greater than … will
    // produce the same array X" rider.
    let n_floor_i0 = div_floor(i0, 2);
    let n_ceil_il = div_floor(il + 1, 2);
    let access_lo: i32 = 2 * (n_floor_i0 - 2) - 1;
    let access_hi: i32 = 2 * (n_ceil_il + 1) + 1;
    let ileft = (i0 - access_lo).max(3);
    let iright = (access_hi - (il - 1)).max(3);
    let ext_len = (len as i32 + ileft + iright) as usize;
    let mut buf = vec![0.0_f64; ext_len];
    // Fill buf so that coefficient index `i` lives at buf[i - i0 + ileft].
    for j in 0..ext_len {
        let i = j as i32 + i0 - ileft;
        let folded = pseo(i, i0, il);
        buf[j] = y[(folded - i0) as usize];
    }
    // Convenience: convert a coefficient index `i` to buf-slot.
    let slot = |i: i32| -> usize { (i - i0 + ileft) as usize };
    // Index ranges per §F.3.8.2 prose ("Firstly, step 1 is
    // performed for all values of n such that ⌊i0/2⌋ - 1 ≤ n
    // < ⌈il/2⌉ + 2 …").
    let n_lo = |off: i32| -> i32 { div_floor(i0, 2) + off };
    let n_hi = |off: i32| -> i32 { div_floor(il + 1, 2) + off };
    // STEP1: X(2n) = K * Yext(2n),     ⌊i0/2⌋ - 1 ≤ n < ⌈il/2⌉ + 2.
    for n in n_lo(-1)..n_hi(2) {
        let i = 2 * n;
        buf[slot(i)] = K_9X7 * buf[slot(i)];
    }
    // STEP2: X(2n + 1) = (1/K) * Yext(2n + 1),
    //                                  ⌊i0/2⌋ - 2 ≤ n < ⌈il/2⌉ + 2.
    let n_lo_b = div_floor(i0, 2);
    let n_hi_b = div_floor(il + 1, 2);
    for n in (n_lo_b - 2)..(n_hi_b + 2) {
        let i = 2 * n + 1;
        buf[slot(i)] = (1.0 / K_9X7) * buf[slot(i)];
    }
    // STEP3: X(2n) -= δ * (X(2n - 1) + X(2n + 1)),
    //                                  ⌊i0/2⌋ - 1 ≤ n < ⌈il/2⌉ + 2.
    for n in n_lo(-1)..n_hi(2) {
        let i = 2 * n;
        buf[slot(i)] += -DELTA_9X7 * (buf[slot(i - 1)] + buf[slot(i + 1)]);
    }
    // STEP4: X(2n + 1) -= γ * (X(2n) + X(2n + 2)),
    //                                  ⌊i0/2⌋ - 1 ≤ n < ⌈il/2⌉ + 1.
    for n in n_lo(-1)..n_hi(1) {
        let i = 2 * n + 1;
        buf[slot(i)] += -GAMMA_9X7 * (buf[slot(i - 1)] + buf[slot(i + 1)]);
    }
    // STEP5: X(2n) -= β * (X(2n - 1) + X(2n + 1)),
    //                                  ⌊i0/2⌋ ≤ n < ⌈il/2⌉ + 1.
    for n in n_lo(0)..n_hi(1) {
        let i = 2 * n;
        buf[slot(i)] += -BETA_9X7 * (buf[slot(i - 1)] + buf[slot(i + 1)]);
    }
    // STEP6: X(2n + 1) -= α * (X(2n) + X(2n + 2)),
    //                                  ⌊i0/2⌋ ≤ n < ⌈il/2⌉.
    for n in n_lo(0)..n_hi(0) {
        let i = 2 * n + 1;
        buf[slot(i)] += -ALPHA_9X7 * (buf[slot(i - 1)] + buf[slot(i + 1)]);
    }
    // Copy in-range outputs into x.
    for k in 0..len {
        let i = i0 + k as i32;
        x[k] = buf[slot(i)];
    }
    Ok(())
}

// =====================================================================
// §F.3.3 — The 2D_INTERLEAVE procedure.
// =====================================================================

/// §F.3.3 — interleave the four sub-bands of a single resolution-
/// level decomposition into a single 2D array on the even/odd
/// sample-grid lattice.
///
/// The input sub-bands are passed in scan order with `(width,
/// height)` dimensions; the output is a 2D `i32` (or `f64`) array
/// of dimensions `(ll.0 + hl.0, ll.1 + lh.1)` whose `(2u, 2v)`
/// position carries `aLL(u, v)`, `(2u+1, 2v)` carries `aHL(u, v)`,
/// `(2u, 2v+1)` carries `aLH(u, v)`, and `(2u+1, 2v+1)` carries
/// `aHH(u, v)`. The `(out_w, out_h)` total dimensions follow §B.5
/// (sum of low-pass and high-pass widths / heights).
///
/// # Errors
///
/// Returns [`Error::InvalidMarkerLength`] if any sub-band slice's
/// length disagrees with its declared `(w, h)` or if the sub-band
/// widths / heights are not consistent with §F.3.3:
/// `ll_w + hl_w == lh_w + hh_w == out_w`, similarly for heights.
#[allow(clippy::too_many_arguments)]
pub fn interleave_2d_i32(
    ll: &[i32],
    ll_dims: (usize, usize),
    hl: &[i32],
    hl_dims: (usize, usize),
    lh: &[i32],
    lh_dims: (usize, usize),
    hh: &[i32],
    hh_dims: (usize, usize),
) -> Result<Interleaved2D<i32>, Error> {
    let out_w = ll_dims.0 + hl_dims.0;
    let out_h = ll_dims.1 + lh_dims.1;
    validate_subband_grid(ll_dims, hl_dims, lh_dims, hh_dims)?;
    if ll.len() != ll_dims.0 * ll_dims.1
        || hl.len() != hl_dims.0 * hl_dims.1
        || lh.len() != lh_dims.0 * lh_dims.1
        || hh.len() != hh_dims.0 * hh_dims.1
    {
        return Err(Error::InvalidMarkerLength);
    }
    let mut data = vec![0_i32; out_w * out_h];
    for v in 0..ll_dims.1 {
        for u in 0..ll_dims.0 {
            data[2 * v * out_w + 2 * u] = ll[v * ll_dims.0 + u];
        }
    }
    for v in 0..hl_dims.1 {
        for u in 0..hl_dims.0 {
            data[2 * v * out_w + 2 * u + 1] = hl[v * hl_dims.0 + u];
        }
    }
    for v in 0..lh_dims.1 {
        for u in 0..lh_dims.0 {
            data[(2 * v + 1) * out_w + 2 * u] = lh[v * lh_dims.0 + u];
        }
    }
    for v in 0..hh_dims.1 {
        for u in 0..hh_dims.0 {
            data[(2 * v + 1) * out_w + 2 * u + 1] = hh[v * hh_dims.0 + u];
        }
    }
    Ok(Interleaved2D {
        data,
        width: out_w,
        height: out_h,
    })
}

/// §F.3.3 — `f64` variant of [`interleave_2d_i32`] for the 9-7
/// irreversible path.
#[allow(clippy::too_many_arguments)]
pub fn interleave_2d_f64(
    ll: &[f64],
    ll_dims: (usize, usize),
    hl: &[f64],
    hl_dims: (usize, usize),
    lh: &[f64],
    lh_dims: (usize, usize),
    hh: &[f64],
    hh_dims: (usize, usize),
) -> Result<Interleaved2D<f64>, Error> {
    let out_w = ll_dims.0 + hl_dims.0;
    let out_h = ll_dims.1 + lh_dims.1;
    validate_subband_grid(ll_dims, hl_dims, lh_dims, hh_dims)?;
    if ll.len() != ll_dims.0 * ll_dims.1
        || hl.len() != hl_dims.0 * hl_dims.1
        || lh.len() != lh_dims.0 * lh_dims.1
        || hh.len() != hh_dims.0 * hh_dims.1
    {
        return Err(Error::InvalidMarkerLength);
    }
    let mut data = vec![0.0_f64; out_w * out_h];
    for v in 0..ll_dims.1 {
        for u in 0..ll_dims.0 {
            data[2 * v * out_w + 2 * u] = ll[v * ll_dims.0 + u];
        }
    }
    for v in 0..hl_dims.1 {
        for u in 0..hl_dims.0 {
            data[2 * v * out_w + 2 * u + 1] = hl[v * hl_dims.0 + u];
        }
    }
    for v in 0..lh_dims.1 {
        for u in 0..lh_dims.0 {
            data[(2 * v + 1) * out_w + 2 * u] = lh[v * lh_dims.0 + u];
        }
    }
    for v in 0..hh_dims.1 {
        for u in 0..hh_dims.0 {
            data[(2 * v + 1) * out_w + 2 * u + 1] = hh[v * hh_dims.0 + u];
        }
    }
    Ok(Interleaved2D {
        data,
        width: out_w,
        height: out_h,
    })
}

fn validate_subband_grid(
    ll_dims: (usize, usize),
    hl_dims: (usize, usize),
    lh_dims: (usize, usize),
    hh_dims: (usize, usize),
) -> Result<(), Error> {
    // §F.3.3 sample-grid lattice requires:
    //   LL.w == LH.w, HL.w == HH.w
    //   LL.h == HL.h, LH.h == HH.h
    if ll_dims.0 != lh_dims.0
        || hl_dims.0 != hh_dims.0
        || ll_dims.1 != hl_dims.1
        || lh_dims.1 != hh_dims.1
    {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(())
}

/// Output of [`interleave_2d_i32`] / [`interleave_2d_f64`] — the
/// pre-filtering interleaved 2D array described by §F.3.3.
#[derive(Debug, Clone, PartialEq)]
pub struct Interleaved2D<T> {
    /// Row-major storage of the `width * height` interleaved
    /// coefficient grid.
    pub data: Vec<T>,
    /// Horizontal extent (sum of LL.w and HL.w per §F.3.3).
    pub width: usize,
    /// Vertical extent (sum of LL.h and LH.h per §F.3.3).
    pub height: usize,
}

// =====================================================================
// §F.3.4 + §F.3.5 — HOR_SR / VER_SR.
// =====================================================================

/// §F.3.4 — apply the 1-D inverse sub-band reconstruction to every
/// row of `a` (in-place), 5-3 reversible.
///
/// The signal index range for each row is `i0 .. i0 + width`. The
/// `i0` argument is the §F.3.6 origin of the row (i.e. the
/// absolute coefficient index of the row's first sample on the
/// next-coarser LL band's coordinate system). It controls the
/// even / odd parity of the leftmost sample.
pub fn hor_sr_5x3(a: &mut Interleaved2D<i32>, i0: i32) -> Result<(), Error> {
    let il = i0 + a.width as i32;
    let mut tmp = vec![0_i32; a.width];
    for v in 0..a.height {
        let row = &a.data[v * a.width..(v + 1) * a.width];
        let row_vec = row.to_vec();
        idwt_1d_5x3(&row_vec, &mut tmp, i0, il)?;
        a.data[v * a.width..(v + 1) * a.width].copy_from_slice(&tmp);
    }
    Ok(())
}

/// §F.3.5 — apply the 1-D inverse sub-band reconstruction to every
/// column of `a` (in-place), 5-3 reversible. `j0` plays the same
/// role for column origin that `i0` plays for rows in
/// [`hor_sr_5x3`].
pub fn ver_sr_5x3(a: &mut Interleaved2D<i32>, j0: i32) -> Result<(), Error> {
    let jl = j0 + a.height as i32;
    let mut col = vec![0_i32; a.height];
    let mut out = vec![0_i32; a.height];
    for u in 0..a.width {
        for v in 0..a.height {
            col[v] = a.data[v * a.width + u];
        }
        idwt_1d_5x3(&col, &mut out, j0, jl)?;
        for v in 0..a.height {
            a.data[v * a.width + u] = out[v];
        }
    }
    Ok(())
}

/// §F.3.4 — `f64` 9-7 irreversible row-wise inverse filter.
pub fn hor_sr_9x7(a: &mut Interleaved2D<f64>, i0: i32) -> Result<(), Error> {
    let il = i0 + a.width as i32;
    let mut tmp = vec![0.0_f64; a.width];
    for v in 0..a.height {
        let row = &a.data[v * a.width..(v + 1) * a.width];
        let row_vec = row.to_vec();
        idwt_1d_9x7(&row_vec, &mut tmp, i0, il)?;
        a.data[v * a.width..(v + 1) * a.width].copy_from_slice(&tmp);
    }
    Ok(())
}

/// §F.3.5 — `f64` 9-7 irreversible column-wise inverse filter.
pub fn ver_sr_9x7(a: &mut Interleaved2D<f64>, j0: i32) -> Result<(), Error> {
    let jl = j0 + a.height as i32;
    let mut col = vec![0.0_f64; a.height];
    let mut out = vec![0.0_f64; a.height];
    for u in 0..a.width {
        for v in 0..a.height {
            col[v] = a.data[v * a.width + u];
        }
        idwt_1d_9x7(&col, &mut out, j0, jl)?;
        for v in 0..a.height {
            a.data[v * a.width + u] = out[v];
        }
    }
    Ok(())
}

// =====================================================================
// §F.3.2 — The 2D_SR procedure (single level).
// =====================================================================

/// §F.3.2 — apply one level of 2D sub-band reconstruction:
/// `2D_INTERLEAVE` then `HOR_SR` then `VER_SR`.
///
/// All sub-bands are 5-3 reversible (integer lifting).
///
/// `(i0, j0)` is the §B.5 origin of the output `(lev - 1) LL` band
/// — i.e. the per-resolution-level coordinate of the top-left
/// reconstructed coefficient. It propagates the parity of the
/// leftmost / topmost sample into the §F.3.6 length-one and
/// extension-parity rules.
#[allow(clippy::too_many_arguments)]
pub fn sr_2d_5x3(
    ll: &[i32],
    ll_dims: (usize, usize),
    hl: &[i32],
    hl_dims: (usize, usize),
    lh: &[i32],
    lh_dims: (usize, usize),
    hh: &[i32],
    hh_dims: (usize, usize),
    i0: i32,
    j0: i32,
) -> Result<Interleaved2D<i32>, Error> {
    let mut a = interleave_2d_i32(ll, ll_dims, hl, hl_dims, lh, lh_dims, hh, hh_dims)?;
    hor_sr_5x3(&mut a, i0)?;
    ver_sr_5x3(&mut a, j0)?;
    Ok(a)
}

/// §F.3.2 — `f64` 9-7 variant of [`sr_2d_5x3`].
#[allow(clippy::too_many_arguments)]
pub fn sr_2d_9x7(
    ll: &[f64],
    ll_dims: (usize, usize),
    hl: &[f64],
    hl_dims: (usize, usize),
    lh: &[f64],
    lh_dims: (usize, usize),
    hh: &[f64],
    hh_dims: (usize, usize),
    i0: i32,
    j0: i32,
) -> Result<Interleaved2D<f64>, Error> {
    let mut a = interleave_2d_f64(ll, ll_dims, hl, hl_dims, lh, lh_dims, hh, hh_dims)?;
    hor_sr_9x7(&mut a, i0)?;
    ver_sr_9x7(&mut a, j0)?;
    Ok(a)
}

// =====================================================================
// Helpers.
// =====================================================================

/// Floor-division for `i32` (round toward minus infinity).
///
/// Used wherever the §F prologue's "all divisions round toward
/// minus infinity" convention applies. `i32::div_euclid` is
/// floor-division for positive divisors, which is exactly what we
/// need (`div = 2` and `div = 4` are the only divisors in §F.3.8.1).
fn div_floor(n: i32, d: i32) -> i32 {
    debug_assert!(d > 0, "div_floor only handles positive divisors");
    n.div_euclid(d)
}

/// Inspector: which kernel a [`crate::WaveletTransform`] selects.
///
/// Mirrors the COD / COC `SPcod / SPcoc` "transformation" byte
/// (T.800 Table A.20). Returns `None` for reserved transform
/// values that the codestream is allowed to declare but that
/// this module cannot decode.
pub fn kernel_for(transform: crate::WaveletTransform) -> Option<KernelKind> {
    match transform {
        crate::WaveletTransform::Reversible5x3 => Some(KernelKind::Reversible5x3),
        crate::WaveletTransform::Irreversible9x7 => Some(KernelKind::Irreversible9x7),
        crate::WaveletTransform::Reserved(_) => None,
    }
}

/// Which lifting filter to invoke (selected by `SPcod / SPcoc`
/// "transformation" byte, T.800 Table A.20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelKind {
    /// 5-3 reversible integer-in / integer-out lifting (§F.3.8.1).
    Reversible5x3,
    /// 9-7 irreversible real-valued lifting (§F.3.8.2).
    Irreversible9x7,
}

/// Round-trip helper: given a §F.3.3 `(SubBandOrientation, u, v)`
/// position, compute the corresponding `(2u + d_u, 2v + d_v)`
/// interleaved-array position.
///
/// Used by callers that want to round-trip an interleaved
/// coefficient back to its sub-band-of-origin without rebuilding
/// the four sub-band arrays.
pub fn interleave_position(orientation: SubBandOrientation, u: usize, v: usize) -> (usize, usize) {
    let du = match orientation {
        SubBandOrientation::LL | SubBandOrientation::LH => 0,
        SubBandOrientation::HL | SubBandOrientation::HH => 1,
    };
    let dv = match orientation {
        SubBandOrientation::LL | SubBandOrientation::HL => 0,
        SubBandOrientation::LH | SubBandOrientation::HH => 1,
    };
    (2 * u + du, 2 * v + dv)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------
    // §F.3.7 PSEO — Equation F-4.
    // -------------------------------------------------------------

    #[test]
    fn pseo_in_range_returns_input() {
        let i0 = 0;
        let il = 8;
        for i in i0..il {
            assert_eq!(pseo(i, i0, il), i, "pseo({}, 0, 8)", i);
        }
    }

    #[test]
    fn pseo_reflects_about_i0() {
        // First-coefficient mirror: pseo(i0 - k) = pseo(i0 + k).
        let i0 = 3;
        let il = 11;
        for k in 1..=4 {
            assert_eq!(pseo(i0 - k, i0, il), pseo(i0 + k, i0, il));
        }
    }

    #[test]
    fn pseo_reflects_about_last_coefficient() {
        // Last-coefficient mirror: pseo(il - 1 + k) = pseo(il - 1 - k).
        let i0 = 3;
        let il = 11;
        for k in 1..=4 {
            assert_eq!(pseo(il - 1 + k, i0, il), pseo(il - 1 - k, i0, il));
        }
    }

    #[test]
    fn pseo_period_is_2_times_len_minus_1() {
        let i0 = 0;
        let il = 5; // period = 2 * 4 = 8.
        for i in -16..16 {
            assert_eq!(pseo(i + 8, i0, il), pseo(i, i0, il), "i={}", i);
        }
    }

    #[test]
    fn pseo_length_one_returns_i0() {
        // Degenerate single-coefficient branch.
        assert_eq!(pseo(0, 5, 6), 5);
        assert_eq!(pseo(100, 5, 6), 5);
        assert_eq!(pseo(-50, 5, 6), 5);
    }

    // -------------------------------------------------------------
    // §F.3.7 Tables F.2 / F.3 — extension amounts.
    // -------------------------------------------------------------

    #[test]
    fn extension_amounts_5x3_table_f2_f3() {
        // i0 even, il odd → (1, 1).
        assert_eq!(extension_amounts_5x3(0, 7), (1, 1));
        assert_eq!(extension_amounts_5x3(2, 9), (1, 1));
        // i0 odd, il even → (2, 2).
        assert_eq!(extension_amounts_5x3(1, 8), (2, 2));
        assert_eq!(extension_amounts_5x3(3, 10), (2, 2));
        // Mixed parities.
        assert_eq!(extension_amounts_5x3(0, 8), (1, 2));
        assert_eq!(extension_amounts_5x3(1, 7), (2, 1));
    }

    #[test]
    fn extension_amounts_9x7_table_f2_f3() {
        assert_eq!(extension_amounts_9x7(0, 7), (3, 3));
        assert_eq!(extension_amounts_9x7(1, 8), (4, 4));
        assert_eq!(extension_amounts_9x7(0, 8), (3, 4));
        assert_eq!(extension_amounts_9x7(1, 7), (4, 3));
    }

    // -------------------------------------------------------------
    // §F.3.8.1 — 1D_FILTR5-3R length-one.
    // -------------------------------------------------------------

    #[test]
    fn idwt_1d_5x3_length_one_even_origin_passes_through() {
        let y = [7_i32];
        let mut x = [0_i32];
        idwt_1d_5x3(&y, &mut x, 0, 1).unwrap();
        assert_eq!(x, [7]);
    }

    #[test]
    fn idwt_1d_5x3_length_one_odd_origin_halves() {
        let y = [8_i32];
        let mut x = [0_i32];
        idwt_1d_5x3(&y, &mut x, 1, 2).unwrap();
        assert_eq!(x, [4]);
        // Floor rounding for odd value.
        let y = [7_i32];
        let mut x = [0_i32];
        idwt_1d_5x3(&y, &mut x, 1, 2).unwrap();
        assert_eq!(x, [3]);
    }

    // -------------------------------------------------------------
    // §F.3.8.1 — 1D_FILTR5-3R full identity / spike tests.
    // -------------------------------------------------------------

    #[test]
    fn idwt_1d_5x3_zero_signal_returns_zero() {
        // The inverse of the all-zero coefficient signal is the all-
        // zero image signal — both lifting steps are linear in their
        // inputs.
        let y = vec![0_i32; 16];
        let mut x = vec![99_i32; 16];
        idwt_1d_5x3(&y, &mut x, 0, 16).unwrap();
        assert_eq!(x, vec![0; 16]);
    }

    #[test]
    fn idwt_1d_5x3_lossless_round_trip_constant() {
        // Apply forward 5-3 lifting to a constant signal, then
        // inverse 5-3 lifting; we should recover the constant.
        let n: i32 = 16;
        let x_in: Vec<i32> = (0..n).map(|_| 100).collect();
        let y = fdwt_1d_5x3(&x_in, 0, n);
        let mut x_out = vec![0_i32; n as usize];
        idwt_1d_5x3(&y, &mut x_out, 0, n).unwrap();
        assert_eq!(x_in, x_out);
    }

    #[test]
    fn idwt_1d_5x3_lossless_round_trip_ramp() {
        // Linear ramp is the canonical "smooth" signal — important
        // because the §F.3.8.1 even-index lifting subtracts a
        // smoothed estimate.
        let n: i32 = 12;
        let x_in: Vec<i32> = (0..n).collect();
        let y = fdwt_1d_5x3(&x_in, 0, n);
        let mut x_out = vec![0_i32; n as usize];
        idwt_1d_5x3(&y, &mut x_out, 0, n).unwrap();
        assert_eq!(x_in, x_out);
    }

    #[test]
    fn idwt_1d_5x3_lossless_round_trip_sawtooth() {
        let n: i32 = 14;
        let x_in: Vec<i32> = (0..n).map(|k| if k % 3 == 0 { 200 } else { -50 }).collect();
        let y = fdwt_1d_5x3(&x_in, 0, n);
        let mut x_out = vec![0_i32; n as usize];
        idwt_1d_5x3(&y, &mut x_out, 0, n).unwrap();
        assert_eq!(x_in, x_out);
    }

    #[test]
    fn idwt_1d_5x3_lossless_round_trip_odd_length() {
        // Odd lengths exercise the il-odd column of Table F.3.
        let n: i32 = 13;
        let x_in: Vec<i32> = (0..n).map(|k| 5 + k * k - 3 * k).collect();
        let y = fdwt_1d_5x3(&x_in, 0, n);
        let mut x_out = vec![0_i32; n as usize];
        idwt_1d_5x3(&y, &mut x_out, 0, n).unwrap();
        assert_eq!(x_in, x_out);
    }

    #[test]
    fn idwt_1d_5x3_lossless_round_trip_odd_origin() {
        // i0 = 1 exercises the i0-odd parity branch.
        let n: i32 = 12;
        let i0: i32 = 1;
        let il: i32 = i0 + n;
        let x_in: Vec<i32> = (0..n).map(|k| 3 * k + 7).collect();
        let y = fdwt_1d_5x3_at(&x_in, i0, il);
        let mut x_out = vec![0_i32; n as usize];
        idwt_1d_5x3(&y, &mut x_out, i0, il).unwrap();
        assert_eq!(x_in, x_out);
    }

    // -------------------------------------------------------------
    // §F.3.8.2 — 1D_FILTR9-7I round-trip identity.
    // -------------------------------------------------------------

    #[test]
    fn idwt_1d_9x7_length_one_even_origin_passes_through() {
        let y = [3.5_f64];
        let mut x = [0.0_f64];
        idwt_1d_9x7(&y, &mut x, 0, 1).unwrap();
        assert_eq!(x, [3.5]);
    }

    #[test]
    fn idwt_1d_9x7_length_one_odd_origin_halves() {
        let y = [3.0_f64];
        let mut x = [0.0_f64];
        idwt_1d_9x7(&y, &mut x, 1, 2).unwrap();
        assert_eq!(x, [1.5]);
    }

    #[test]
    fn idwt_1d_9x7_dc_coefficient_produces_constant_signal() {
        // Feed the decoder a DC-only coefficient stream:
        // Y = [c, 0, c, 0, c, 0, ...] where every LL coefficient is
        // some constant `c` and every HL/LH/HH coefficient is zero.
        // The inverse DWT should produce a CONSTANT signal X — the
        // exact value depends on K and the lifting parameters, but
        // every output sample must be equal to its neighbours (the
        // signature of a DC reconstruction). This is a structural
        // check on the §F.3.8.2 step order and sign conventions
        // that does NOT depend on the test-only encoder oracle.
        let n: i32 = 20;
        let c = 1.0;
        let y: Vec<f64> = (0..n).map(|i| if i % 2 == 0 { c } else { 0.0 }).collect();
        let mut x = vec![0.0_f64; n as usize];
        idwt_1d_9x7(&y, &mut x, 0, n).unwrap();
        // Inner samples should all be ≈ x[4] (skipping boundary
        // samples where periodic-symmetric extension distorts a few
        // samples — Annex F notes the boundary is exact only after
        // the iright/ileft minimums are met).
        let ref_val = x[8];
        for k in 6..14 {
            assert!(
                (x[k as usize] - ref_val).abs() < 1e-9,
                "x[{}] = {} != x[8] = {}",
                k,
                x[k as usize],
                ref_val
            );
        }
    }

    #[test]
    fn idwt_1d_9x7_zero_signal_returns_zero() {
        let y = vec![0.0_f64; 16];
        let mut x = vec![1.0_f64; 16];
        idwt_1d_9x7(&y, &mut x, 0, 16).unwrap();
        for v in &x {
            assert!(v.abs() < 1e-12, "non-zero output {}", v);
        }
    }

    #[test]
    fn idwt_1d_9x7_dc_coefficient_produces_dc_in_interior() {
        // Verify the DC reconstruction property in the interior at
        // varying signal lengths and origins — the §F.3.8.2
        // lifting must reduce `Y = [c, 0, c, 0, ...]` to a
        // constant in the interior regardless of length parity.
        for &(i0, n) in &[(0_i32, 18_i32), (0, 21), (1, 18), (1, 21)] {
            let il = i0 + n;
            let c = 2.5;
            let y: Vec<f64> = (0..n)
                .map(|k| {
                    let i = i0 + k;
                    if i.rem_euclid(2) == 0 {
                        c
                    } else {
                        0.0
                    }
                })
                .collect();
            let mut x = vec![0.0_f64; n as usize];
            idwt_1d_9x7(&y, &mut x, i0, il).unwrap();
            // Inner samples should all be equal (DC reconstruction).
            let mid = (n / 2) as usize;
            let ref_val = x[mid];
            // Allow ±3 boundary samples on either side as in §F.3
            // the periodic-symmetric extension only converges to
            // exact DC at distance ≥ iright_9x7 from the boundary.
            let inset = 6_usize.min(n as usize / 4);
            for k in inset..(n as usize - inset) {
                assert!(
                    (x[k] - ref_val).abs() < 1e-9,
                    "n={}, i0={}: x[{}]={} != x[mid]={} ({})",
                    n,
                    i0,
                    k,
                    x[k],
                    ref_val,
                    (x[k] - ref_val).abs()
                );
            }
            // DC should be non-zero and have the expected sign.
            assert!(ref_val.abs() > 0.5, "DC reconstruction collapsed to ~0");
            assert!(ref_val > 0.0, "DC reconstruction sign-flipped");
        }
    }

    #[test]
    fn idwt_1d_9x7_is_linear() {
        // The §F.3.8.2 inverse filter is a linear operator: scaling
        // the input by `s` scales the output by `s`. This is a
        // structural property of the lifting steps and does NOT
        // depend on an external encoder reference.
        let n: i32 = 16;
        let y1: Vec<f64> = (0..n).map(|k| ((k * 7) % 5) as f64).collect();
        let s = 3.5;
        let y2: Vec<f64> = y1.iter().map(|v| v * s).collect();
        let mut x1 = vec![0.0_f64; n as usize];
        let mut x2 = vec![0.0_f64; n as usize];
        idwt_1d_9x7(&y1, &mut x1, 0, n).unwrap();
        idwt_1d_9x7(&y2, &mut x2, 0, n).unwrap();
        for k in 0..n as usize {
            assert!(
                (x2[k] - s * x1[k]).abs() < 1e-9,
                "linearity at k={}: {} vs {}",
                k,
                x2[k],
                s * x1[k]
            );
        }
    }

    #[test]
    fn idwt_1d_9x7_is_additive() {
        // Inverse DWT(a + b) == inverse DWT(a) + inverse DWT(b).
        let n: i32 = 14;
        let a: Vec<f64> = (0..n).map(|k| k as f64).collect();
        let b: Vec<f64> = (0..n).map(|k| (k * k) as f64 / 7.0).collect();
        let ab: Vec<f64> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        let mut x_a = vec![0.0_f64; n as usize];
        let mut x_b = vec![0.0_f64; n as usize];
        let mut x_ab = vec![0.0_f64; n as usize];
        idwt_1d_9x7(&a, &mut x_a, 0, n).unwrap();
        idwt_1d_9x7(&b, &mut x_b, 0, n).unwrap();
        idwt_1d_9x7(&ab, &mut x_ab, 0, n).unwrap();
        for k in 0..n as usize {
            assert!(
                (x_ab[k] - (x_a[k] + x_b[k])).abs() < 1e-9,
                "additivity at k={}",
                k
            );
        }
    }

    #[test]
    fn idwt_1d_9x7_impulse_response_decays() {
        // Single-sample impulse in the LL coefficient at the
        // centre of the signal: Y(centre) = 1, all others = 0.
        // The inverse 9-7 lifting must produce a finite-extent
        // impulse response with magnitude decaying away from
        // centre (smoothness of the 9-7 synthesis filter).
        let n: i32 = 24;
        let centre = (n / 2) as usize & !1; // even
        let mut y = vec![0.0_f64; n as usize];
        y[centre] = 1.0;
        let mut x = vec![0.0_f64; n as usize];
        idwt_1d_9x7(&y, &mut x, 0, n).unwrap();
        // Magnitude at centre should dominate.
        assert!(x[centre].abs() > 0.5);
        // Far-from-centre tap magnitudes should be smaller than the
        // central tap.
        let edge = x[0].abs().max(x[n as usize - 1].abs());
        assert!(
            edge < x[centre].abs() * 0.5,
            "impulse failed to decay: centre={}, edge={}",
            x[centre],
            edge
        );
    }

    // -------------------------------------------------------------
    // §F.3.3 — 2D_INTERLEAVE.
    // -------------------------------------------------------------

    #[test]
    fn interleave_2d_places_subbands_on_correct_lattice() {
        // 2x2 LL, 2x2 HL, 2x2 LH, 2x2 HH → 4x4 interleaved.
        let ll = vec![10_i32, 11, 12, 13];
        let hl = vec![20_i32, 21, 22, 23];
        let lh = vec![30_i32, 31, 32, 33];
        let hh = vec![40_i32, 41, 42, 43];
        let out = interleave_2d_i32(&ll, (2, 2), &hl, (2, 2), &lh, (2, 2), &hh, (2, 2)).unwrap();
        assert_eq!(out.width, 4);
        assert_eq!(out.height, 4);
        // Row 0: LL HL LL HL.
        assert_eq!(&out.data[0..4], &[10, 20, 11, 21]);
        // Row 1: LH HH LH HH.
        assert_eq!(&out.data[4..8], &[30, 40, 31, 41]);
        // Row 2: LL HL LL HL.
        assert_eq!(&out.data[8..12], &[12, 22, 13, 23]);
        // Row 3: LH HH LH HH.
        assert_eq!(&out.data[12..16], &[32, 42, 33, 43]);
    }

    #[test]
    fn interleave_2d_rejects_inconsistent_subband_dims() {
        let ll = vec![0_i32; 4];
        let hl = vec![0_i32; 4];
        let lh = vec![0_i32; 6]; // wrong width → rejected.
        let hh = vec![0_i32; 4];
        let err =
            interleave_2d_i32(&ll, (2, 2), &hl, (2, 2), &lh, (3, 2), &hh, (2, 2)).unwrap_err();
        assert!(matches!(err, Error::InvalidMarkerLength));
    }

    #[test]
    fn interleave_position_mirrors_subband_orientation_table() {
        assert_eq!(interleave_position(SubBandOrientation::LL, 3, 4), (6, 8));
        assert_eq!(interleave_position(SubBandOrientation::HL, 3, 4), (7, 8));
        assert_eq!(interleave_position(SubBandOrientation::LH, 3, 4), (6, 9));
        assert_eq!(interleave_position(SubBandOrientation::HH, 3, 4), (7, 9));
    }

    // -------------------------------------------------------------
    // §F.3.2 — 2D_SR round-trip (5-3).
    // -------------------------------------------------------------

    #[test]
    fn sr_2d_5x3_round_trip_8x8_zero() {
        let ll = vec![0_i32; 16]; // 4x4
        let hl = vec![0_i32; 16];
        let lh = vec![0_i32; 16];
        let hh = vec![0_i32; 16];
        let out = sr_2d_5x3(&ll, (4, 4), &hl, (4, 4), &lh, (4, 4), &hh, (4, 4), 0, 0).unwrap();
        assert_eq!(out.width, 8);
        assert_eq!(out.height, 8);
        for v in &out.data {
            assert_eq!(*v, 0);
        }
    }

    #[test]
    fn sr_2d_5x3_round_trip_8x8_ramp() {
        // Forward-decompose a known 8x8 image, then inverse via
        // sr_2d_5x3 and verify we got it back.
        let w = 8_i32;
        let h = 8_i32;
        let mut src = vec![0_i32; (w * h) as usize];
        for v in 0..h {
            for u in 0..w {
                src[(v * w + u) as usize] = u + 3 * v;
            }
        }
        // Forward 2D 5-3 (one level): VER_FDWT then HOR_FDWT on a
        // copy of src, then decompose into the four sub-bands.
        let (ll, hl, lh, hh) = fdwt_2d_5x3_split(&src, w as usize, h as usize, 0, 0);
        let llw = (w as usize + 1) / 2;
        let llh = (h as usize + 1) / 2;
        let hlw = w as usize / 2;
        let lhh = h as usize / 2;
        let out = sr_2d_5x3(
            &ll,
            (llw, llh),
            &hl,
            (hlw, llh),
            &lh,
            (llw, lhh),
            &hh,
            (hlw, lhh),
            0,
            0,
        )
        .unwrap();
        assert_eq!(out.data, src);
    }

    // -------------------------------------------------------------
    // Kernel selector.
    // -------------------------------------------------------------

    #[test]
    fn kernel_for_dispatches_on_wavelet_transform_byte() {
        assert_eq!(
            kernel_for(crate::WaveletTransform::Reversible5x3),
            Some(KernelKind::Reversible5x3)
        );
        assert_eq!(
            kernel_for(crate::WaveletTransform::Irreversible9x7),
            Some(KernelKind::Irreversible9x7)
        );
        assert_eq!(kernel_for(crate::WaveletTransform::Reserved(0x42)), None);
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn lifting_parameters_match_table_f4_signs() {
        // Sanity check: Table F.4's first two parameters are
        // negative and the last three are positive. (Catches a
        // sign-flip on the constants without consulting any
        // external implementation.)
        assert!(ALPHA_9X7 < 0.0);
        assert!(BETA_9X7 < 0.0);
        assert!(GAMMA_9X7 > 0.0);
        assert!(DELTA_9X7 > 0.0);
        assert!(K_9X7 > 1.0);
    }

    // -------------------------------------------------------------
    // Equation-mismatch / boundary failure paths.
    // -------------------------------------------------------------

    #[test]
    fn idwt_1d_5x3_rejects_length_mismatch() {
        let y = [1_i32, 2, 3];
        let mut x = [0_i32; 4];
        let err = idwt_1d_5x3(&y, &mut x, 0, 3).unwrap_err();
        assert!(matches!(err, Error::InvalidMarkerLength));
    }

    #[test]
    fn idwt_1d_9x7_rejects_inverted_range() {
        let y = [1.0_f64; 4];
        let mut x = [0.0_f64; 4];
        let err = idwt_1d_9x7(&y, &mut x, 5, 5).unwrap_err();
        assert!(matches!(err, Error::InvalidMarkerLength));
    }

    // ============================================================
    // Forward DWT used ONLY by the test suite — round-trip oracles
    // so we don't have to import a reference implementation. The
    // decoder never invokes these.
    // ============================================================

    /// Encoder-side §F.4.6 1D_SD for the 5-3 reversible filter.
    /// This is the inverse of [`idwt_1d_5x3`]: given samples
    /// `x[i0..il]`, produce coefficients `y[i0..il]` such that
    /// `idwt_1d_5x3(fdwt_1d_5x3(x))` returns `x` exactly.
    fn fdwt_1d_5x3(x: &[i32], i0: i32, il: i32) -> Vec<i32> {
        fdwt_1d_5x3_at(x, i0, il)
    }

    /// Worker version of [`fdwt_1d_5x3`] that accepts an explicit
    /// (i0, il) range (i.e. for testing odd-origin signals).
    ///
    /// Equation F-7 prologue (encoder, informative §F.4):
    ///   Y(2n + 1) = X(2n + 1) - ⌊ (X(2n) + X(2n + 2)) / 2 ⌋
    ///   Y(2n)     = X(2n)     + ⌊ (Y(2n - 1) + Y(2n + 1) + 2) / 4 ⌋
    fn fdwt_1d_5x3_at(x: &[i32], i0: i32, il: i32) -> Vec<i32> {
        let len = (il - i0) as usize;
        let mut y = x.to_vec();
        if len <= 1 {
            // Length-one passthrough (inverse of length_one_5x3).
            if len == 1 && i0.rem_euclid(2) == 1 {
                y[0] = x[0] * 2;
            }
            return y;
        }
        let lookup = |y_slice: &[i32], i: i32| -> i32 {
            let folded = pseo(i, i0, il);
            y_slice[(folded - i0) as usize]
        };
        // Inverse of Equation F-6 first (encoder order is the reverse
        // of decoder order):
        //     Y(2n + 1) = X(2n + 1) - ⌊ (X(2n) + X(2n + 2)) / 2 ⌋
        let n_lo_odd = div_floor(i0, 2);
        let n_hi_odd = div_floor(il, 2);
        for n in n_lo_odd..n_hi_odd {
            let two_n = 2 * n;
            if two_n + 1 >= i0 && two_n + 1 < il {
                let xtn = lookup(&y, two_n);
                let xtn2 = lookup(&y, two_n + 2);
                let v = x[(two_n + 1 - i0) as usize] - div_floor(xtn + xtn2, 2);
                y[(two_n + 1 - i0) as usize] = v;
            }
        }
        // Then inverse of Equation F-5:
        //     Y(2n) = X(2n) + ⌊ (Y(2n - 1) + Y(2n + 1) + 2) / 4 ⌋
        let n_lo_even = div_floor(i0, 2);
        let n_hi_even = div_floor(il + 1, 2);
        for n in n_lo_even..n_hi_even {
            let two_n = 2 * n;
            if two_n >= i0 && two_n < il {
                let ytm1 = y[(pseo(two_n - 1, i0, il) - i0) as usize];
                let ytp1 = y[(pseo(two_n + 1, i0, il) - i0) as usize];
                let v = x[(two_n - i0) as usize] + div_floor(ytm1 + ytp1 + 2, 4);
                y[(two_n - i0) as usize] = v;
            }
        }
        y
    }

    /// Test-only 2D forward 5-3 split into the four sub-bands at
    /// `(i0, j0) = (0, 0)`. Produces `(LL, HL, LH, HH)` row-major
    /// arrays.
    fn fdwt_2d_5x3_split(
        src: &[i32],
        w: usize,
        h: usize,
        i0: i32,
        j0: i32,
    ) -> (Vec<i32>, Vec<i32>, Vec<i32>, Vec<i32>) {
        // Horizontal pass: each row.
        let mut a = src.to_vec();
        for v in 0..h {
            let row = &a[v * w..(v + 1) * w];
            let row_vec = row.to_vec();
            let y = fdwt_1d_5x3_at(&row_vec, i0, i0 + w as i32);
            a[v * w..(v + 1) * w].copy_from_slice(&y);
        }
        // Vertical pass: each column.
        let mut col = vec![0_i32; h];
        for u in 0..w {
            for v in 0..h {
                col[v] = a[v * w + u];
            }
            let y = fdwt_1d_5x3_at(&col, j0, j0 + h as i32);
            for v in 0..h {
                a[v * w + u] = y[v];
            }
        }
        // De-interleave into four sub-bands.
        let llw = (w + 1) / 2;
        let llh = (h + 1) / 2;
        let hlw = w / 2;
        let lhh = h / 2;
        let mut ll = vec![0_i32; llw * llh];
        let mut hl = vec![0_i32; hlw * llh];
        let mut lh = vec![0_i32; llw * lhh];
        let mut hh = vec![0_i32; hlw * lhh];
        for v in 0..llh {
            for u in 0..llw {
                ll[v * llw + u] = a[2 * v * w + 2 * u];
            }
        }
        for v in 0..llh {
            for u in 0..hlw {
                hl[v * hlw + u] = a[2 * v * w + 2 * u + 1];
            }
        }
        for v in 0..lhh {
            for u in 0..llw {
                lh[v * llw + u] = a[(2 * v + 1) * w + 2 * u];
            }
        }
        for v in 0..lhh {
            for u in 0..hlw {
                hh[v * hlw + u] = a[(2 * v + 1) * w + 2 * u + 1];
            }
        }
        (ll, hl, lh, hh)
    }
}
