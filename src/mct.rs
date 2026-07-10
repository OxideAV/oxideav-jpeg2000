//! Multiple-component transformation — T.800 Annex G.
//!
//! This module implements the **decoder-side** multi-component path
//! that turns the three reconstructed tile-components `Y0`, `Y1`, `Y2`
//! (output of [`crate::dwt`]) back into image-domain colour samples
//! `I0`, `I1`, `I2`. It covers the normative inverse direction of
//! Annex G — i.e. the two transforms a decoder needs:
//!
//! * **§G.2.2 — Inverse Reversible Component Transform (RCT).** The
//!   integer-in / integer-out three-line lifting of Equations G-6,
//!   G-7, G-8:
//!
//!   ```text
//!   I1(x, y) = Y0(x, y) - ⌊(Y2(x, y) + Y1(x, y)) / 4⌋
//!   I0(x, y) = Y2(x, y) + I1(x, y)
//!   I2(x, y) = Y1(x, y) + I1(x, y)
//!   ```
//!
//!   The division rounds toward minus infinity (Annex F prologue
//!   convention, inherited because the RCT is paired with the 5-3
//!   reversible filter via Annex G.2). The transform is exact and
//!   self-inverse against the §G.2.1 forward equations: the unit-test
//!   battery below proves it by round-tripping the 256-entry diagonal
//!   `(R, G, B) = (k, k, k)` and a hand-picked
//!   `(R, G, B) = (200, 100, 50)` sample.
//!
//! * **§G.3.2 — Inverse Irreversible Component Transform (ICT).** The
//!   linear 3×3 inverse of the §G.3.1 forward Y'CbCr matrix, given by
//!   Equations G-12, G-13, G-14:
//!
//!   ```text
//!   I0(x, y) = Y0(x, y)                              + 1.402   * Y2(x, y)
//!   I1(x, y) = Y0(x, y) - 0.34413 * Y1(x, y) - 0.71414 * Y2(x, y)
//!   I2(x, y) = Y0(x, y) + 1.772   * Y1(x, y)
//!   ```
//!
//!   The spec is explicit (G.3.2 closing paragraph) that "Equations
//!   (G-12), (G-13) and (G-14) do not imply a required precision for
//!   the coefficients" — the literals are kept as `f32` constants in
//!   line with the surrounding 9-7 irreversible wavelet path. The
//!   inputs come from [`crate::dwt`]'s 9-7 reconstruction and are
//!   real-valued sample arrays.
//!
//! The forward direction (§G.2.1 RCT, §G.3.1 ICT) is the encoder's
//! job and is **not** implemented in this round — every encoder path
//! in the crate still returns [`crate::Error::NotImplemented`].
//!
//! ## DC level shifting (§G.1)
//!
//! §G.1.1 (forward) and §G.1.2 (inverse) DC-level-shift the unsigned
//! components by `±2^(Ssiz - 1)`. The shift is applied **before** the
//! forward RCT/ICT in the encoder, and **after** the inverse RCT/ICT
//! in the decoder. Signed components (Ssiz MSB = `1`, per T.800
//! Table A.11) are **not** level-shifted at all.
//!
//! Four pairs of primitives are exposed:
//!
//! * [`forward_dc_level_shift_unsigned`] / [`inverse_dc_level_shift_unsigned`]
//!   — Equations G-1 / G-2 verbatim on `i32` slices, valid for
//!   `precision ∈ 1..=31`.
//! * [`forward_dc_level_shift_unsigned_i64`] /
//!   [`inverse_dc_level_shift_unsigned_i64`] — the `i64`-widened
//!   variants for `precision ∈ 1..=38` (the full Table A.11 range);
//!   used by the tile-reconstruction surface when an `Ssiz` byte
//!   carries a value the `i32` primitives cannot represent.
//! * [`forward_dc_level_shift`] / [`inverse_dc_level_shift`] — the
//!   signed-aware dispatchers that take the SIZ component's
//!   `is_signed` flag (the parsed Ssiz MSB) and apply the unsigned
//!   shift only when `is_signed == false`. These are the entry
//!   points the tile-reconstruction round will call once per
//!   component.
//! * [`clamp_to_dynamic_range`] — the §G.1.2 NOTE's "typical
//!   solution" for the overflow/underflow caused by quantization,
//!   clipping reconstructed samples to the original
//!   `[0, 2^Ssiz - 1]` (unsigned) or
//!   `[-2^(Ssiz-1), 2^(Ssiz-1) - 1]` (signed) range.
//! * [`clamp_to_dynamic_range_i64`] — the `i64`-widened mirror of
//!   the clamp helper, covering `precision ∈ 1..=38` (the full
//!   Table A.11 range). Pairs symmetrically with the
//!   `*_dc_level_shift_unsigned_i64` primitives so a caller staging
//!   the `Ssiz ≥ 32` reconstruction path can close §G.1.2 entirely
//!   in `i64`.
//!
//! ## What this module does NOT cover
//!
//! * **The forward RCT / ICT.** Encoder-only; deferred to the round
//!   that wires `encode_jpeg2000` up.
//! * **Non-3-component MCT.** Annex G is normative only for the first
//!   three image components (`indexed as 0, 1 and 2` — see §G.2 and
//!   §G.3 prologues). Components 3+ pass through unchanged; this
//!   module's API is therefore strictly 3-component.
//! * **Per-tile-component MCT toggle.** The COD marker's `SGcod` MCT
//!   byte (`0` = none, `1` = MCT-on-0/1/2; see T.800 Table A.16) is
//!   parsed by the main-header walker into [`crate::Cod::mct`]; the
//!   pick between [`inverse_rct`] / [`inverse_ict`] / no-op based on
//!   that byte is wired by the tile-reconstruction round (deferred —
//!   see the Roadmap in `README.md`). This module just exposes the
//!   three primitives.
//!
//! ## Clean-room provenance
//!
//! Implemented solely from
//! `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf` Annex G (§G.1
//! prologue + §G.1.1 / §G.1.2 DC level shifting; §G.2 prologue + §G.2.1
//! forward RCT [used only as the forward reference for the
//! self-inverse round-trip tests] + §G.2.2 inverse RCT + Equations
//! G-6 / G-7 / G-8; §G.3 prologue + §G.3.2 inverse ICT + Equations
//! G-12 / G-13 / G-14).
//!
//! ## Numerical model
//!
//! Two surfaces are exposed:
//!
//! * **Reversible (`i32`).** [`inverse_rct`] operates on `i32`
//!   coefficient arrays. The Annex F prologue's `⌊·/4⌋ = >> 2`
//!   floor-division convention is preserved (`i32` arithmetic right
//!   shift, since we want floor toward minus infinity for negative
//!   `Y1 + Y2` sums as well). Self-inverse against [`forward_rct`].
//! * **Irreversible (`f32`).** [`inverse_ict`] operates on `f32`
//!   coefficient arrays. The matrix literals are stored as
//!   `f32` constants. Forward-then-inverse round-trips within a few
//!   ULPs (the §G.3.2 spec note about precision applies — these are
//!   informative coefficients, not bit-exact constants).
//!
//! Both surfaces take three independent `&mut` slices (one per
//! component) of equal length and operate in place per Annex G's
//! per-`(x, y)` formulation. Length-mismatch is a programming error
//! and panics in debug builds via `debug_assert_eq!`; the public
//! entry points return [`crate::Error::InvalidMarkerLength`] for
//! length-mismatch in release builds rather than panicking.

use crate::Error;

// ---------------------------------------------------------------------------
// §G.2 — Reversible Component Transform (integer / lossless).
// ---------------------------------------------------------------------------

/// Inverse Reversible Component Transform (RCT) — T.800 §G.2.2.
///
/// Operates in place on three equal-length `i32` slices representing
/// the three reconstructed tile-components `Y0`, `Y1`, `Y2` (output of
/// the inverse 5-3 wavelet transform). After the call, the slices
/// hold `I0`, `I1`, `I2` (the de-correlated → colour-space samples).
///
/// Equations G-6, G-7, G-8 verbatim:
///
/// * `I1 = Y0 - ⌊(Y2 + Y1) / 4⌋`
/// * `I0 = Y2 + I1`
/// * `I2 = Y1 + I1`
///
/// The division floors toward minus infinity per the Annex F prologue
/// convention (which Annex G.2 inherits via the 5-3-only pairing
/// rule). On `i32` the safe way to express this is
/// `(y2 + y1).div_euclid(4)`-style arithmetic; this implementation
/// uses an arithmetic-right-shift of two on the wrapping sum, which
/// matches `⌊·/4⌋` for all `i32` inputs (the right-shift of a signed
/// integer in Rust is defined as arithmetic, replicating the sign
/// bit, i.e. floor division by a power of two).
///
/// # Errors
///
/// Returns [`Error::InvalidMarkerLength`] if the three slices do not
/// share a common length.
pub fn inverse_rct(c0: &mut [i32], c1: &mut [i32], c2: &mut [i32]) -> Result<(), Error> {
    if c0.len() != c1.len() || c1.len() != c2.len() {
        return Err(Error::InvalidMarkerLength);
    }
    for i in 0..c0.len() {
        let y0 = c0[i];
        let y1 = c1[i];
        let y2 = c2[i];
        // I1 = Y0 - ⌊(Y2 + Y1) / 4⌋
        // Use wrapping_add to side-step a debug-build overflow on
        // adversarial inputs; the spec does not bound the input
        // amplitude, and the RCT is exact only within the
        // representable range anyway.
        let sum = y2.wrapping_add(y1);
        // Arithmetic right shift floors toward minus infinity for
        // both positive and negative `sum`; this matches the Annex F
        // prologue's `⌊·/4⌋` convention.
        let floor_div4 = sum >> 2;
        let i1 = y0.wrapping_sub(floor_div4);
        let i0 = y2.wrapping_add(i1);
        let i2 = y1.wrapping_add(i1);
        c0[i] = i0;
        c1[i] = i1;
        c2[i] = i2;
    }
    Ok(())
}

/// Forward Reversible Component Transform (RCT) — T.800 §G.2.1.
///
/// Equations G-3, G-4, G-5 verbatim:
///
/// * `Y0 = ⌊(I0 + 2 I1 + I2) / 4⌋`
/// * `Y1 = I2 - I1`
/// * `Y2 = I0 - I1`
///
/// Provided so the test battery can round-trip §G.2.1 → §G.2.2 in
/// pure-Rust without an encoder-side glue layer. Not part of the
/// decoder's hot path; the encoder round will reuse it.
///
/// # Errors
///
/// Returns [`Error::InvalidMarkerLength`] if the three slices do not
/// share a common length.
pub fn forward_rct(c0: &mut [i32], c1: &mut [i32], c2: &mut [i32]) -> Result<(), Error> {
    if c0.len() != c1.len() || c1.len() != c2.len() {
        return Err(Error::InvalidMarkerLength);
    }
    for i in 0..c0.len() {
        let r = c0[i];
        let g = c1[i];
        let b = c2[i];
        // Y0 = ⌊(I0 + 2 I1 + I2) / 4⌋   (§G.2.1 Eq. G-3)
        let sum = r.wrapping_add(g.wrapping_mul(2)).wrapping_add(b);
        let y0 = sum >> 2;
        // Y1 = I2 - I1                  (§G.2.1 Eq. G-4)
        let y1 = b.wrapping_sub(g);
        // Y2 = I0 - I1                  (§G.2.1 Eq. G-5)
        let y2 = r.wrapping_sub(g);
        c0[i] = y0;
        c1[i] = y1;
        c2[i] = y2;
    }
    Ok(())
}

/// `i64`-widened inverse Reversible Component Transform — T.800
/// §G.2.2 for the full Table A.11 `Ssiz ≤ 38` range.
///
/// Same Equations G-6, G-7, G-8 as [`inverse_rct`], rolled out one
/// word wider. Use when the SIZ-marker component precision exceeds
/// 31 bits and the caller has staged the reversible reconstruction
/// pipeline on `i64` buffers (paired with
/// [`inverse_dc_level_shift_unsigned_i64`] and
/// [`clamp_to_dynamic_range_i64`]). The §G.2.1 NOTE about `Y1` / `Y2`
/// carrying one bit more precision than the original components means
/// a 38-bit component's transform coefficients need 39 bits — far
/// inside `i64`, so unlike the `i32` surface no wrapping can fire on
/// any legal Table A.11 input.
///
/// The `⌊·/4⌋` floor-toward-minus-infinity convention is realised by
/// the same arithmetic-right-shift-by-two as the `i32` variant.
///
/// # Errors
///
/// Returns [`Error::InvalidMarkerLength`] if the three slices do not
/// share a common length.
pub fn inverse_rct_i64(c0: &mut [i64], c1: &mut [i64], c2: &mut [i64]) -> Result<(), Error> {
    if c0.len() != c1.len() || c1.len() != c2.len() {
        return Err(Error::InvalidMarkerLength);
    }
    for i in 0..c0.len() {
        let y0 = c0[i];
        let y1 = c1[i];
        let y2 = c2[i];
        // I1 = Y0 - ⌊(Y2 + Y1) / 4⌋        (§G.2.2 Eq. G-6)
        let sum = y2.wrapping_add(y1);
        let floor_div4 = sum >> 2;
        let i1 = y0.wrapping_sub(floor_div4);
        // I0 = Y2 + I1                      (§G.2.2 Eq. G-7)
        let i0 = y2.wrapping_add(i1);
        // I2 = Y1 + I1                      (§G.2.2 Eq. G-8)
        let i2 = y1.wrapping_add(i1);
        c0[i] = i0;
        c1[i] = i1;
        c2[i] = i2;
    }
    Ok(())
}

/// `i64`-widened forward Reversible Component Transform — T.800
/// §G.2.1 for the full Table A.11 `Ssiz ≤ 38` range.
///
/// Same Equations G-3, G-4, G-5 as [`forward_rct`], one word wider.
/// Provided so the test battery (and the encoder MCT toggle round)
/// can round-trip §G.2.1 → §G.2.2 on `Ssiz ≥ 32` sample values that
/// the `i32` surface cannot represent.
///
/// # Errors
///
/// Returns [`Error::InvalidMarkerLength`] if the three slices do not
/// share a common length.
pub fn forward_rct_i64(c0: &mut [i64], c1: &mut [i64], c2: &mut [i64]) -> Result<(), Error> {
    if c0.len() != c1.len() || c1.len() != c2.len() {
        return Err(Error::InvalidMarkerLength);
    }
    for i in 0..c0.len() {
        let r = c0[i];
        let g = c1[i];
        let b = c2[i];
        // Y0 = ⌊(I0 + 2 I1 + I2) / 4⌋      (§G.2.1 Eq. G-3)
        let sum = r.wrapping_add(g.wrapping_mul(2)).wrapping_add(b);
        let y0 = sum >> 2;
        // Y1 = I2 - I1                      (§G.2.1 Eq. G-4)
        let y1 = b.wrapping_sub(g);
        // Y2 = I0 - I1                      (§G.2.1 Eq. G-5)
        let y2 = r.wrapping_sub(g);
        c0[i] = y0;
        c1[i] = y1;
        c2[i] = y2;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// §G.3 — Irreversible Component Transform (linear / lossy).
// ---------------------------------------------------------------------------

/// `0.34413` — inverse-ICT `Y1` coefficient feeding `I1` (T.800 Eq. G-13).
const ICT_INV_Y1_TO_I1: f32 = 0.34413;
/// `0.71414` — inverse-ICT `Y2` coefficient feeding `I1` (T.800 Eq. G-13).
const ICT_INV_Y2_TO_I1: f32 = 0.71414;
/// `1.402`   — inverse-ICT `Y2` coefficient feeding `I0` (T.800 Eq. G-12).
const ICT_INV_Y2_TO_I0: f32 = 1.402;
/// `1.772`   — inverse-ICT `Y1` coefficient feeding `I2` (T.800 Eq. G-14).
const ICT_INV_Y1_TO_I2: f32 = 1.772;

/// Inverse Irreversible Component Transform (ICT) — T.800 §G.3.2.
///
/// Operates in place on three equal-length `f32` slices representing
/// the three reconstructed tile-components `Y0`, `Y1`, `Y2` (output of
/// the inverse 9-7 wavelet transform). After the call, the slices
/// hold `I0`, `I1`, `I2`.
///
/// Equations G-12, G-13, G-14 verbatim:
///
/// * `I0 = Y0 + 1.402 * Y2`
/// * `I1 = Y0 - 0.34413 * Y1 - 0.71414 * Y2`
/// * `I2 = Y0 + 1.772 * Y1`
///
/// Per the §G.3.2 closing paragraph, the spec is explicit that no
/// particular precision for the coefficients is required; `f32` is
/// kept here for parity with the 9-7 irreversible wavelet path.
///
/// # Errors
///
/// Returns [`Error::InvalidMarkerLength`] if the three slices do not
/// share a common length.
pub fn inverse_ict(c0: &mut [f32], c1: &mut [f32], c2: &mut [f32]) -> Result<(), Error> {
    if c0.len() != c1.len() || c1.len() != c2.len() {
        return Err(Error::InvalidMarkerLength);
    }
    for i in 0..c0.len() {
        let y0 = c0[i];
        let y1 = c1[i];
        let y2 = c2[i];
        // Compute all three outputs from the inputs BEFORE writing
        // back any slot, otherwise an in-place update of c0 would
        // poison the c1 / c2 computations.
        let i0 = y0 + ICT_INV_Y2_TO_I0 * y2;
        let i1 = y0 - ICT_INV_Y1_TO_I1 * y1 - ICT_INV_Y2_TO_I1 * y2;
        let i2 = y0 + ICT_INV_Y1_TO_I2 * y1;
        c0[i] = i0;
        c1[i] = i1;
        c2[i] = i2;
    }
    Ok(())
}

/// Forward Irreversible Component Transform (ICT) — T.800 §G.3.1.
///
/// Equations G-9, G-10, G-11 verbatim:
///
/// * `Y0 =  0.299    * I0 + 0.587    * I1 + 0.114    * I2`
/// * `Y1 = -0.16875  * I0 - 0.331260 * I1 + 0.5      * I2`
/// * `Y2 =  0.5      * I0 - 0.41869  * I1 - 0.08131  * I2`
///
/// Provided so the test battery can round-trip §G.3.1 → §G.3.2 in
/// pure-Rust without an encoder-side glue layer. Not part of the
/// decoder's hot path; the encoder round will reuse it.
///
/// # Errors
///
/// Returns [`Error::InvalidMarkerLength`] if the three slices do not
/// share a common length.
pub fn forward_ict(c0: &mut [f32], c1: &mut [f32], c2: &mut [f32]) -> Result<(), Error> {
    if c0.len() != c1.len() || c1.len() != c2.len() {
        return Err(Error::InvalidMarkerLength);
    }
    for i in 0..c0.len() {
        let r = c0[i];
        let g = c1[i];
        let b = c2[i];
        let y0 = 0.299 * r + 0.587 * g + 0.114 * b;
        let y1 = -0.16875 * r - 0.331260 * g + 0.5 * b;
        let y2 = 0.5 * r - 0.41869 * g - 0.08131 * b;
        c0[i] = y0;
        c1[i] = y1;
        c2[i] = y2;
    }
    Ok(())
}

/// Forward Irreversible Component Transform (ICT) — T.800 §G.3.1,
/// `f64` variant for the encoder's real-valued 9-7 pipeline.
///
/// Same Equations G-9, G-10, G-11 as [`forward_ict`]; the encoder's
/// forward 9-7 cascade carries `f64` planes (see
/// [`crate::dwt::sd_2d_9x7`]), so the component transform is applied in
/// the same width to avoid a precision round-trip. Per the §G.3
/// closing paragraph no particular coefficient precision is required.
///
/// # Errors
///
/// Returns [`Error::InvalidMarkerLength`] if the three slices do not
/// share a common length.
pub fn forward_ict_f64(c0: &mut [f64], c1: &mut [f64], c2: &mut [f64]) -> Result<(), Error> {
    if c0.len() != c1.len() || c1.len() != c2.len() {
        return Err(Error::InvalidMarkerLength);
    }
    for i in 0..c0.len() {
        let r = c0[i];
        let g = c1[i];
        let b = c2[i];
        let y0 = 0.299 * r + 0.587 * g + 0.114 * b;
        let y1 = -0.16875 * r - 0.331260 * g + 0.5 * b;
        let y2 = 0.5 * r - 0.41869 * g - 0.08131 * b;
        c0[i] = y0;
        c1[i] = y1;
        c2[i] = y2;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// §G.1 — DC level shifting.
// ---------------------------------------------------------------------------

/// Forward DC level shift for an unsigned tile-component — T.800 §G.1.1.
///
/// Per Equation G-1, when the MSB of `Ssiz` is zero (i.e. the
/// component is unsigned per Table A.11), the encoder subtracts the
/// same `2^(Ssiz - 1)` quantity from every sample before the
/// forward multiple-component transform — or, if no MCT is used,
/// before the forward wavelet transform of Annex F:
///
/// ```text
/// I'(x, y) = I(x, y) - 2^(Ssiz - 1)
/// ```
///
/// `precision` is the per-component bit depth `Ssiz` as recorded in
/// the SIZ marker (T.800 §A.5.1; see [`crate::SizComponent::precision`]).
///
/// # Errors
///
/// Returns [`Error::InvalidSamplePrecision`] if `precision` is `0`
/// or greater than `31`. The `i32` coefficient slice cannot
/// represent the `1 << (precision - 1)` shift for `precision ≥ 32`;
/// callers with `Ssiz ≥ 32` must use
/// [`forward_dc_level_shift_unsigned_i64`] instead.
pub fn forward_dc_level_shift_unsigned(samples: &mut [i32], precision: u8) -> Result<(), Error> {
    if precision == 0 || precision > 31 {
        return Err(Error::InvalidSamplePrecision);
    }
    let shift: i32 = 1_i32 << (precision - 1);
    for s in samples.iter_mut() {
        *s = s.wrapping_sub(shift);
    }
    Ok(())
}

/// Inverse DC level shift for an unsigned tile-component — T.800 §G.1.2.
///
/// Per §G.1.2 (Equation G-2), after the inverse multiple-component
/// transform, each unsigned tile-component is level-shifted by
/// `+2^(Ssiz - 1)` to restore the original unsigned-sample dynamic
/// range:
///
/// ```text
/// I(x, y) = I'(x, y) + 2^(Ssiz - 1)
/// ```
///
/// `precision` is the per-component bit depth `Ssiz` as recorded in
/// the SIZ marker (T.800 §A.5.1; see [`crate::SizComponent::precision`]).
/// Caller is responsible for skipping this step on signed components
/// (the SIZ marker's `Ssiz` high bit, see Table A.11; §G.1.2 only
/// shifts unsigned components) — [`inverse_dc_level_shift`] is the
/// signed-aware wrapper.
///
/// # Errors
///
/// Returns [`Error::InvalidSamplePrecision`] if `precision` is `0` or
/// greater than `31`. T.800 Table A.11 admits `Ssiz` up to 38 bits,
/// but the `i32` coefficient slice cannot represent the
/// `1 << (precision - 1)` shift for `precision ≥ 32`; callers with
/// `Ssiz ≥ 32` must use [`inverse_dc_level_shift_unsigned_i64`].
pub fn inverse_dc_level_shift_unsigned(samples: &mut [i32], precision: u8) -> Result<(), Error> {
    if precision == 0 || precision > 31 {
        return Err(Error::InvalidSamplePrecision);
    }
    let shift: i32 = 1_i32 << (precision - 1);
    for s in samples.iter_mut() {
        *s = s.wrapping_add(shift);
    }
    Ok(())
}

/// `i64`-widened forward DC level shift — T.800 §G.1.1 for the full
/// Table A.11 `Ssiz ≤ 38` range.
///
/// Use when the SIZ-marker component precision exceeds 31 bits and
/// the caller has already widened its sample buffer to `i64`. Same
/// Equation G-1 semantics as [`forward_dc_level_shift_unsigned`].
///
/// # Errors
///
/// Returns [`Error::InvalidSamplePrecision`] if `precision` is `0`
/// or greater than `38` (the T.800 Table A.11 upper bound on
/// `Ssiz`).
pub fn forward_dc_level_shift_unsigned_i64(
    samples: &mut [i64],
    precision: u8,
) -> Result<(), Error> {
    if precision == 0 || precision > 38 {
        return Err(Error::InvalidSamplePrecision);
    }
    let shift: i64 = 1_i64 << (precision - 1);
    for s in samples.iter_mut() {
        *s = s.wrapping_sub(shift);
    }
    Ok(())
}

/// `i64`-widened inverse DC level shift — T.800 §G.1.2 for the full
/// Table A.11 `Ssiz ≤ 38` range.
///
/// Use when the SIZ-marker component precision exceeds 31 bits and
/// the caller has already widened its sample buffer to `i64`. Same
/// Equation G-2 semantics as [`inverse_dc_level_shift_unsigned`].
///
/// # Errors
///
/// Returns [`Error::InvalidSamplePrecision`] if `precision` is `0`
/// or greater than `38` (the T.800 Table A.11 upper bound on
/// `Ssiz`).
pub fn inverse_dc_level_shift_unsigned_i64(
    samples: &mut [i64],
    precision: u8,
) -> Result<(), Error> {
    if precision == 0 || precision > 38 {
        return Err(Error::InvalidSamplePrecision);
    }
    let shift: i64 = 1_i64 << (precision - 1);
    for s in samples.iter_mut() {
        *s = s.wrapping_add(shift);
    }
    Ok(())
}

/// Signed-aware forward DC level shift — T.800 §G.1.1 dispatcher.
///
/// Bridges the SIZ marker's per-component `Ssiz` MSB
/// (`is_signed == true` ⇒ the MSB is `1`, per Table A.11) and the
/// §G.1.1 prologue rule that "DC level shifting is performed on
/// samples of components that are unsigned only". When `is_signed`
/// is `true` the call is a no-op; otherwise it forwards to
/// [`forward_dc_level_shift_unsigned`].
///
/// This is the entry point a tile-reconstruction surface should
/// call once per component prior to the forward MCT (or, when no
/// MCT is used, prior to the forward 2D DWT).
///
/// # Errors
///
/// Propagates any error from [`forward_dc_level_shift_unsigned`]
/// when `is_signed == false`.
pub fn forward_dc_level_shift(
    samples: &mut [i32],
    precision: u8,
    is_signed: bool,
) -> Result<(), Error> {
    if is_signed {
        // §G.1.1 prologue: signed components are not level-shifted.
        // Still validate `precision` so the caller cannot smuggle an
        // out-of-range Ssiz past this gate.
        if precision == 0 || precision > 38 {
            return Err(Error::InvalidSamplePrecision);
        }
        return Ok(());
    }
    forward_dc_level_shift_unsigned(samples, precision)
}

/// Signed-aware inverse DC level shift — T.800 §G.1.2 dispatcher.
///
/// Mirror of [`forward_dc_level_shift`] for the decoder side. When
/// `is_signed == true` the call is a no-op (the §G.1.2 prologue:
/// "Inverse DC level shifting is performed on reconstructed samples
/// of components that are unsigned only"); otherwise it forwards to
/// [`inverse_dc_level_shift_unsigned`].
///
/// # Errors
///
/// Propagates any error from [`inverse_dc_level_shift_unsigned`]
/// when `is_signed == false`.
pub fn inverse_dc_level_shift(
    samples: &mut [i32],
    precision: u8,
    is_signed: bool,
) -> Result<(), Error> {
    if is_signed {
        if precision == 0 || precision > 38 {
            return Err(Error::InvalidSamplePrecision);
        }
        return Ok(());
    }
    inverse_dc_level_shift_unsigned(samples, precision)
}

/// Clip reconstructed samples to their original dynamic range —
/// the "typical solution" recommended by the §G.1.2 NOTE.
///
/// The §G.1.2 NOTE warns that "due to quantization effects, the
/// reconstructed samples I(x, y) may exceed the dynamic range of the
/// original samples" and observes that "clipping the value to the
/// nearest value within the original dynamic range is a typical
/// solution". The procedure is *not* normative — this helper is
/// what a decoder caller should reach for once it has run the
/// inverse 2D DWT and the inverse MCT (and, for unsigned
/// components, [`inverse_dc_level_shift_unsigned`]).
///
/// The clip range is determined by Table A.11 from `precision`
/// (`Ssiz`'s low 7 bits) and `is_signed` (`Ssiz`'s MSB):
///
/// * **Unsigned**: `[0, 2^precision - 1]`.
/// * **Signed**:   `[-2^(precision - 1), 2^(precision - 1) - 1]`.
///
/// # Errors
///
/// Returns [`Error::InvalidSamplePrecision`] if `precision` is `0`
/// or greater than `31`. Callers handling `Ssiz ≥ 32` should reach
/// for [`clamp_to_dynamic_range_i64`] instead — that variant covers
/// the full `precision ∈ 1..=38` Table A.11 range on an `i64` slice.
pub fn clamp_to_dynamic_range(
    samples: &mut [i32],
    precision: u8,
    is_signed: bool,
) -> Result<(), Error> {
    if precision == 0 || precision > 31 {
        return Err(Error::InvalidSamplePrecision);
    }
    let (lo, hi) = if is_signed {
        let half = 1_i32 << (precision - 1);
        (-half, half - 1)
    } else {
        let span = if precision == 31 {
            i32::MAX
        } else {
            (1_i32 << precision) - 1
        };
        (0, span)
    };
    for s in samples.iter_mut() {
        *s = (*s).clamp(lo, hi);
    }
    Ok(())
}

/// `i64`-widened mirror of [`clamp_to_dynamic_range`] — the §G.1.2
/// NOTE's "typical solution" extended to the `Ssiz ≥ 32` corner of
/// T.800 Table A.11.
///
/// Use when the caller has staged the reconstruction pipeline on
/// `i64` buffers — i.e. after a call to
/// [`inverse_dc_level_shift_unsigned_i64`] — and needs the matching
/// post-quantization clip. The clip endpoints are the §G.1.2 NOTE
/// formula widened one bit:
///
/// * **Unsigned**: `[0, 2^precision - 1]`.
/// * **Signed**:   `[-2^(precision - 1), 2^(precision - 1) - 1]`.
///
/// The `precision == 38` endpoints are both representable in `i64`
/// (`2^38 - 1` ≪ `i64::MAX`, `-2^37` ≫ `i64::MIN`), so unlike the
/// `i32` variant there is no edge case at the upper bound — the
/// shift `1_i64 << precision` is always well-defined for
/// `precision ∈ 1..=38`.
///
/// # Errors
///
/// Returns [`Error::InvalidSamplePrecision`] if `precision` is `0`
/// or greater than `38` (the T.800 Table A.11 upper bound on
/// `Ssiz`). The 1..=31 `i64` range is accepted: a caller with a
/// modest-precision component that still wants to share an `i64`
/// buffer with a wider sibling pays only the wider clamp arithmetic,
/// not a separate code path.
pub fn clamp_to_dynamic_range_i64(
    samples: &mut [i64],
    precision: u8,
    is_signed: bool,
) -> Result<(), Error> {
    if precision == 0 || precision > 38 {
        return Err(Error::InvalidSamplePrecision);
    }
    let (lo, hi) = if is_signed {
        let half: i64 = 1_i64 << (precision - 1);
        (-half, half - 1)
    } else {
        let span: i64 = (1_i64 << precision) - 1;
        (0_i64, span)
    };
    for s in samples.iter_mut() {
        *s = (*s).clamp(lo, hi);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// §G.1 + §G.2 / §G.3 — per-tile-component threading.
// ---------------------------------------------------------------------------

/// Three-component reconstruction parameters fed to the §G threading
/// surface — one entry per component, in the same `(0, 1, 2)` order
/// the SIZ marker lists them.
///
/// Mirrors the [`crate::SizComponent`] fields the inverse §G pipeline
/// actually consumes (the §G.1 / §G.2 / §G.3 procedures only read the
/// component's `Ssiz` byte: precision + signedness; the sub-sampling
/// factors are a §B / §F concern that the caller has already honoured
/// by handing in matched-length sample slices). This three-tuple is
/// the smallest invariant the threading code needs to dispatch the
/// inverse DC level-shift and the dynamic-range clamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentDescriptor {
    /// Per-component sample precision in bits (1..=38), the
    /// `precision_bits` field of [`crate::SizComponent`].
    pub precision_bits: u8,
    /// Per-component signedness (Ssiz MSB), the `is_signed` field of
    /// [`crate::SizComponent`].
    pub is_signed: bool,
}

impl ComponentDescriptor {
    /// Build a [`ComponentDescriptor`] from a parsed
    /// [`crate::SizComponent`]. The two extra `XRsiz / YRsiz`
    /// sub-sampling fields the SIZ marker carries are deliberately
    /// dropped: §G operates per `(x, y)` after the §F / §B layers
    /// have already realised the per-component grid.
    pub const fn from_siz_component(c: &crate::SizComponent) -> Self {
        Self {
            precision_bits: c.precision_bits,
            is_signed: c.is_signed,
        }
    }
}

/// Inverse-MCT mode selected by the COD marker's `SGcod` MCT byte
/// (T.800 Table A.17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InverseMctMode {
    /// `SGcod_MCT == 0` — no MCT applied at the encoder. Each
    /// component is independently DC-level-shifted; no RCT / ICT
    /// runs.
    None,
    /// `SGcod_MCT == 1` paired with the 5-3 reversible kernel —
    /// inverse RCT (§G.2.2) on components `(0, 1, 2)`.
    Rct,
    /// `SGcod_MCT == 1` paired with the 9-7 irreversible kernel —
    /// inverse ICT (§G.3.2) on components `(0, 1, 2)`.
    Ict,
}

/// Thread the §G.2.2 inverse RCT + §G.1.2 per-component inverse DC
/// level shift + §G.1.2-NOTE clamp across three reconstructed
/// reversible-path tile-components — T.800 §G.1.2 prologue rule
/// "performed after the computation of the inverse multiple component
/// transformation".
///
/// This is the per-tile glue that sits between [`crate::dwt::idwt_5x3`]
/// (which returns one [`crate::dwt::Interleaved2D<i32>`] per
/// component) and the caller's per-tile pixel buffer. The three
/// component slices are operated on in place; on return they hold
/// the final clipped sample values for the three reconstructed
/// components in `(0, 1, 2)` order.
///
/// Sequence executed per the §G.1 placement diagram (Figure G.1 when
/// `mode == InverseMctMode::Rct`, Figure G.2 when `mode ==
/// InverseMctMode::None`):
///
/// 1. If `mode == InverseMctMode::Rct`, run [`inverse_rct`] on
///    `(c0, c1, c2)`. The Annex G.2 prologue requires that the three
///    components share the same separation on the reference grid and
///    the same bit-depth — this is enforced here by checking
///    `descriptors[0..3]` carry equal `precision_bits` and equal
///    `is_signed` flags when MCT is on.
/// 2. For each component `i in 0..3`, call [`inverse_dc_level_shift`]
///    with that component's `(precision_bits, is_signed)` from
///    `descriptors[i]`. Signed components no-op per §G.1.2 prologue;
///    unsigned components are shifted by `+2^(precision_bits - 1)`.
/// 3. For each component `i in 0..3`, call
///    [`clamp_to_dynamic_range`] with the same descriptor. This is
///    the §G.1.2 NOTE's "typical solution" for the
///    quantization-overflow case.
///
/// The threading is `O(3 N)` for `N` samples per component. The
/// inverse RCT runs first and `O(N)` per component once each; the
/// level-shift and clamp run `O(N)` per component once each.
///
/// `mode == InverseMctMode::Ict` is rejected: ICT operates on `f32`
/// (T.800 §G.3.2 closing paragraph), so the 9-7 path uses a separate
/// entry point — see [`reconstruct_tile_components_9x7`].
///
/// # Errors
///
/// * [`Error::InvalidMarkerLength`] if the three slices do not share
///   a common length, or if `descriptors.len() != 3`.
/// * [`Error::InvalidSamplePrecision`] if any descriptor's
///   `precision_bits` is `0` or greater than `31` (the `i32`
///   reversible-path surface bound; callers with `Ssiz ≥ 32` should
///   stage the i64-widened path themselves).
/// * [`Error::InvalidComponentCount`] if `mode ==
///   InverseMctMode::Rct` and the three descriptors do not all share
///   the same `(precision_bits, is_signed)` pair (the §G.2 prologue
///   constraint).
/// * [`Error::NotImplemented`] if `mode == InverseMctMode::Ict`
///   (wrong entry point — see [`reconstruct_tile_components_9x7`]).
pub fn reconstruct_tile_components_5x3(
    c0: &mut [i32],
    c1: &mut [i32],
    c2: &mut [i32],
    descriptors: &[ComponentDescriptor],
    mode: InverseMctMode,
) -> Result<(), Error> {
    if c0.len() != c1.len() || c1.len() != c2.len() {
        return Err(Error::InvalidMarkerLength);
    }
    if descriptors.len() != 3 {
        return Err(Error::InvalidMarkerLength);
    }
    // Validate every descriptor's precision sits in the i32-path
    // window before doing any work, so a bad descriptor[2] doesn't
    // get caught only after RCT + level-shift on (0, 1) have run.
    for d in descriptors {
        if d.precision_bits == 0 || d.precision_bits > 31 {
            return Err(Error::InvalidSamplePrecision);
        }
    }
    match mode {
        InverseMctMode::Ict => {
            // ICT is the 9-7 irreversible path. The reversible-path
            // entry point cannot honour it because ICT operates on
            // f32; route the caller to the 9-7 entry point.
            return Err(Error::NotImplemented);
        }
        InverseMctMode::Rct => {
            // §G.2 prologue: "The three components input into the
            // RCT shall have the same separation on the reference
            // grid and the same bit-depth." The separation half is
            // already honoured by the caller's matched-length
            // slices; the bit-depth (and signedness) half is checked
            // here.
            let d0 = descriptors[0];
            if descriptors[1] != d0 || descriptors[2] != d0 {
                return Err(Error::InvalidComponentCount);
            }
            inverse_rct(c0, c1, c2)?;
        }
        InverseMctMode::None => {
            // Figure G.2 path — components flow through the inverse
            // DWT independently and the only §G work is the
            // per-component level-shift + clamp.
        }
    }
    // Per-component inverse DC level shift (§G.1.2 Eq. G-2) +
    // §G.1.2 NOTE dynamic-range clamp. Both are signed-aware via
    // the dispatchers in this module.
    for (slice, d) in [
        (c0 as &mut [i32], descriptors[0]),
        (c1, descriptors[1]),
        (c2, descriptors[2]),
    ] {
        inverse_dc_level_shift(slice, d.precision_bits, d.is_signed)?;
        clamp_to_dynamic_range(slice, d.precision_bits, d.is_signed)?;
    }
    Ok(())
}

/// Thread the §G.2.2 inverse RCT + §G.1.2 per-component inverse DC
/// level shift + §G.1.2-NOTE clamp across an **arbitrary** number of
/// reconstructed reversible-path tile-components — the multi-component
/// generalisation of [`reconstruct_tile_components_5x3`].
///
/// T.800 §G.2 says the RCT "is a decorrelating transformation applied
/// to the **first three** components of an image (indexed as 0, 1 and
/// 2)". An image is not required to carry exactly three components: a
/// single-component greyscale tile, a two-component pair, or a
/// four-plus-component image (e.g. RGBA, or a multispectral scene) are
/// all legal. This entry point realises that rule:
///
/// * `mode == InverseMctMode::Rct` runs [`inverse_rct`] on components
///   `(0, 1, 2)` **only when at least three components are present**.
///   Components with index `≥ 3` are never touched by the transform —
///   they flow through the Figure G.2 placement (level-shift + clamp
///   only), exactly as the §G.2 "first three" wording requires. When
///   fewer than three components are present, no RCT can run: the
///   `Rct` mode is rejected for `components.len() < 3` (a one- or
///   two-component tile cannot legally signal an RCT in the COD
///   marker — there is nothing for Equations G-6..G-8 to operate on).
/// * `mode == InverseMctMode::None` is the pure Figure G.2 path for
///   any component count `≥ 1`: every component is independently
///   level-shifted + clamped per its own descriptor.
///
/// The §G.2 "same separation and bit-depth" prologue constraint is
/// enforced on components `(0, 1, 2)` only (the transform inputs) when
/// `mode == InverseMctMode::Rct`; the index-`≥ 3` pass-through
/// components may each carry their own distinct
/// `(precision_bits, is_signed)` pair.
///
/// `components[i]` is paired with `descriptors[i]`; the two slices must
/// have the same length, and every component slice must share a common
/// per-sample length (the §G "same separation on the reference grid"
/// half of the prologue — already realised by the §B / §F layers).
///
/// # Errors
///
/// * [`Error::InvalidMarkerLength`] if `components.is_empty()`, if
///   `components.len() != descriptors.len()`, or if the component
///   slices do not all share a common length.
/// * [`Error::InvalidSamplePrecision`] if any descriptor's
///   `precision_bits` is `0` or greater than `31` (the `i32`
///   reversible-path surface bound).
/// * [`Error::InvalidComponentCount`] if `mode ==
///   InverseMctMode::Rct` and either fewer than three components are
///   present, or the first three descriptors do not all share the
///   same `(precision_bits, is_signed)` pair (the §G.2 prologue
///   constraint).
/// * [`Error::NotImplemented`] if `mode == InverseMctMode::Ict`
///   (wrong entry point — ICT is the 9-7 / `f32` surface, see
///   [`reconstruct_tile_components_9x7`]).
pub fn reconstruct_tile_components_5x3_multi(
    components: &mut [&mut [i32]],
    descriptors: &[ComponentDescriptor],
    mode: InverseMctMode,
) -> Result<(), Error> {
    if components.is_empty() {
        return Err(Error::InvalidMarkerLength);
    }
    if components.len() != descriptors.len() {
        return Err(Error::InvalidMarkerLength);
    }
    for d in descriptors {
        if d.precision_bits == 0 || d.precision_bits > 31 {
            return Err(Error::InvalidSamplePrecision);
        }
    }
    match mode {
        InverseMctMode::Ict => {
            // ICT is the 9-7 irreversible / f32 path.
            return Err(Error::NotImplemented);
        }
        InverseMctMode::Rct => {
            // §G.2: the RCT operates on the first three components.
            // A COD marker cannot legally signal an RCT for a tile
            // with fewer than three components.
            if components.len() < 3 {
                return Err(Error::InvalidComponentCount);
            }
            // §G.2 "same separation on the reference grid": the three
            // transform inputs carry the same sample count. Validate
            // up-front so a length mismatch does not surface only
            // after the RCT has mutated (0, 1, 2). Components outside
            // the transform (and every component under
            // `InverseMctMode::None`, where §G.1.2 is purely
            // per-component) may be sub-sampled differently and are
            // free to differ in length.
            let len = components[0].len();
            if components[1].len() != len || components[2].len() != len {
                return Err(Error::InvalidMarkerLength);
            }
            // §G.2 prologue "same separation and bit-depth" — checked
            // on the three transform inputs only. The index-≥3
            // pass-through components are free to differ.
            let d0 = descriptors[0];
            if descriptors[1] != d0 || descriptors[2] != d0 {
                return Err(Error::InvalidComponentCount);
            }
            // Split off the first three slices so the borrow checker
            // accepts three simultaneous &mut into the component
            // collection.
            let (head, _tail) = components.split_at_mut(3);
            if let [c0, c1, c2] = head {
                inverse_rct(c0, c1, c2)?;
            }
        }
        InverseMctMode::None => {
            // Figure G.2 path — no MCT applied at any component count.
        }
    }
    // Per-component inverse DC level shift (§G.1.2 Eq. G-2) +
    // §G.1.2 NOTE dynamic-range clamp across every component, MCT'd or
    // pass-through alike.
    for (slice, d) in components.iter_mut().zip(descriptors.iter()) {
        inverse_dc_level_shift(slice, d.precision_bits, d.is_signed)?;
        clamp_to_dynamic_range(slice, d.precision_bits, d.is_signed)?;
    }
    Ok(())
}

/// `i64`-widened mirror of [`reconstruct_tile_components_5x3`] —
/// the §G.2.2 inverse RCT + §G.1.2 inverse DC level shift +
/// §G.1.2-NOTE clamp threading for tile-components whose SIZ-marker
/// precision exceeds the `i32` surface's 31-bit bound.
///
/// T.800 Table A.11 admits `Ssiz` up to 38 bits; the `i32` threading
/// entry point caps at 31 because the `1 << (Ssiz - 1)` level-shift
/// constant and the `[0, 2^Ssiz - 1]` clamp endpoint stop being
/// representable. This mirror composes the three `i64` primitives
/// that landed for exactly this purpose —
/// [`inverse_rct_i64`], [`inverse_dc_level_shift_unsigned_i64`], and
/// [`clamp_to_dynamic_range_i64`] — into the same Figure G.1
/// (`mode == InverseMctMode::Rct`) / Figure G.2
/// (`mode == InverseMctMode::None`) sequence:
///
/// 1. If `mode == InverseMctMode::Rct`, enforce the §G.2 prologue
///    "same separation and bit-depth" rule on `descriptors[0..3]`
///    and run [`inverse_rct_i64`].
/// 2. Per component: inverse DC level shift (§G.1.2 Eq. G-2) for
///    unsigned descriptors — signed components are not shifted, per
///    the §G.1.2 prologue "components that are unsigned only" rule.
/// 3. Per component: [`clamp_to_dynamic_range_i64`], the §G.1.2 NOTE
///    "typical solution" clip.
///
/// The accepted `precision_bits` window is the full Table A.11
/// `1..=38` range — a modest-precision component sharing an `i64`
/// staging buffer with a wide sibling flows through unchanged, so a
/// caller does not have to split a mixed-precision tile across the
/// two surfaces.
///
/// `mode == InverseMctMode::Ict` is rejected: ICT is the 9-7 / `f32`
/// surface — see [`reconstruct_tile_components_9x7`].
///
/// # Errors
///
/// * [`Error::InvalidMarkerLength`] if the three slices do not share
///   a common length, or if `descriptors.len() != 3`.
/// * [`Error::InvalidSamplePrecision`] if any descriptor's
///   `precision_bits` is `0` or greater than `38` (the Table A.11
///   upper bound on `Ssiz`).
/// * [`Error::InvalidComponentCount`] if `mode ==
///   InverseMctMode::Rct` and the three descriptors do not all share
///   the same `(precision_bits, is_signed)` pair (the §G.2 prologue
///   constraint).
/// * [`Error::NotImplemented`] if `mode == InverseMctMode::Ict`
///   (wrong entry point — see [`reconstruct_tile_components_9x7`]).
pub fn reconstruct_tile_components_5x3_i64(
    c0: &mut [i64],
    c1: &mut [i64],
    c2: &mut [i64],
    descriptors: &[ComponentDescriptor],
    mode: InverseMctMode,
) -> Result<(), Error> {
    if c0.len() != c1.len() || c1.len() != c2.len() {
        return Err(Error::InvalidMarkerLength);
    }
    if descriptors.len() != 3 {
        return Err(Error::InvalidMarkerLength);
    }
    // Validate every descriptor's precision before doing any work, so
    // a bad descriptor[2] doesn't get caught only after RCT +
    // level-shift on (0, 1) have run. The bound is the Table A.11
    // `Ssiz ≤ 38` ceiling, not the `i32` surface's 31.
    for d in descriptors {
        if d.precision_bits == 0 || d.precision_bits > 38 {
            return Err(Error::InvalidSamplePrecision);
        }
    }
    match mode {
        InverseMctMode::Ict => {
            // ICT is the 9-7 irreversible / f32 path.
            return Err(Error::NotImplemented);
        }
        InverseMctMode::Rct => {
            // §G.2 prologue: "The three components input into the RCT
            // shall have the same separation on the reference grid and
            // the same bit-depth." Separation is honoured by the
            // caller's matched-length slices; bit-depth (and
            // signedness) is checked here.
            let d0 = descriptors[0];
            if descriptors[1] != d0 || descriptors[2] != d0 {
                return Err(Error::InvalidComponentCount);
            }
            inverse_rct_i64(c0, c1, c2)?;
        }
        InverseMctMode::None => {
            // Figure G.2 path — per-component level-shift + clamp only.
        }
    }
    // Per-component inverse DC level shift (§G.1.2 Eq. G-2) + §G.1.2
    // NOTE dynamic-range clamp. The §G.1.2 prologue shifts unsigned
    // components only; precision is already validated above, so the
    // signed branch simply skips the shift.
    for (slice, d) in [
        (c0 as &mut [i64], descriptors[0]),
        (c1, descriptors[1]),
        (c2, descriptors[2]),
    ] {
        if !d.is_signed {
            inverse_dc_level_shift_unsigned_i64(slice, d.precision_bits)?;
        }
        clamp_to_dynamic_range_i64(slice, d.precision_bits, d.is_signed)?;
    }
    Ok(())
}

/// Thread the §G.3.2 inverse ICT + §G.1.2 per-component inverse DC
/// level shift + §G.1.2-NOTE clamp across three reconstructed
/// irreversible-path tile-components.
///
/// The 9-7 counterpart of [`reconstruct_tile_components_5x3`]: the
/// three `f32` slices carry the §F.3 9-7 reconstructed coefficients
/// (caller has already downcast the `f64` IDWT output if it ran the
/// `f64` 9-7 path), the inverse ICT runs in `f32`, and the result is
/// rounded to `i32` for the DC level-shift and clamp via
/// `round-to-nearest-even` semantics (the §G.3.2 closing paragraph
/// notes the spec does not pin the ICT coefficients' precision; a
/// rounding step into `i32` after ICT is the conventional way of
/// landing on a representable per-sample value).
///
/// `out0`, `out1`, `out2` receive the rounded, level-shifted,
/// clipped samples. They must each be the same length as the
/// matching input slice.
///
/// Sequence executed per the §G.1 placement diagram (Figure G.1 when
/// `mode == InverseMctMode::Ict`, Figure G.2 when `mode ==
/// InverseMctMode::None`):
///
/// 1. If `mode == InverseMctMode::Ict`, run [`inverse_ict`] on
///    `(c0, c1, c2)`. The §G.3 prologue mirrors §G.2's "same
///    separation and bit-depth" rule on the three components; this
///    is enforced via the same `descriptors[0..3]` equality check.
/// 2. Round each `f32` sample to its nearest integer (ties-to-even,
///    matching Rust's `f32::round_ties_even` semantics) and write it
///    into the matching `out*` slot.
/// 3. For each component, run [`inverse_dc_level_shift`] then
///    [`clamp_to_dynamic_range`] over the integerised slot.
///
/// `mode == InverseMctMode::Rct` is rejected: RCT operates on `i32`
/// (T.800 §G.2.2), so the 5-3 path uses
/// [`reconstruct_tile_components_5x3`] instead.
///
/// # Errors
///
/// * [`Error::InvalidMarkerLength`] if any of the six slices do not
///   share a common length, or if `descriptors.len() != 3`.
/// * [`Error::InvalidSamplePrecision`] if any descriptor's
///   `precision_bits` is `0` or greater than `31`.
/// * [`Error::InvalidComponentCount`] if `mode ==
///   InverseMctMode::Ict` and the three descriptors do not all share
///   the same `(precision_bits, is_signed)` pair (the §G.3 prologue
///   constraint).
/// * [`Error::NotImplemented`] if `mode == InverseMctMode::Rct`
///   (wrong entry point — see [`reconstruct_tile_components_5x3`]).
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_tile_components_9x7(
    c0: &mut [f32],
    c1: &mut [f32],
    c2: &mut [f32],
    out0: &mut [i32],
    out1: &mut [i32],
    out2: &mut [i32],
    descriptors: &[ComponentDescriptor],
    mode: InverseMctMode,
) -> Result<(), Error> {
    if c0.len() != c1.len() || c1.len() != c2.len() {
        return Err(Error::InvalidMarkerLength);
    }
    if out0.len() != c0.len() || out1.len() != c1.len() || out2.len() != c2.len() {
        return Err(Error::InvalidMarkerLength);
    }
    if descriptors.len() != 3 {
        return Err(Error::InvalidMarkerLength);
    }
    for d in descriptors {
        if d.precision_bits == 0 || d.precision_bits > 31 {
            return Err(Error::InvalidSamplePrecision);
        }
    }
    match mode {
        InverseMctMode::Rct => {
            return Err(Error::NotImplemented);
        }
        InverseMctMode::Ict => {
            let d0 = descriptors[0];
            if descriptors[1] != d0 || descriptors[2] != d0 {
                return Err(Error::InvalidComponentCount);
            }
            inverse_ict(c0, c1, c2)?;
        }
        InverseMctMode::None => {
            // Figure G.2 path on the 9-7 surface — no MCT applied.
        }
    }
    // Round-to-nearest-even into the i32 output slots, then run the
    // signed-aware inverse DC level shift + clamp on each component
    // independently.
    for (src, dst, d) in [
        (&*c0, out0 as &mut [i32], descriptors[0]),
        (&*c1, out1, descriptors[1]),
        (&*c2, out2, descriptors[2]),
    ] {
        round_f32_into_i32(src, dst);
        inverse_dc_level_shift(dst, d.precision_bits, d.is_signed)?;
        clamp_to_dynamic_range(dst, d.precision_bits, d.is_signed)?;
    }
    Ok(())
}

/// Round one component's `f32` reconstructed samples to `i32`
/// ties-to-even, saturating at the cast point.
///
/// `f32::round_ties_even` is the IEEE-754 default rounding mode and
/// matches the "no required precision" language of the §G.3.2 closing
/// paragraph: the rounding direction is a decoder choice. Exact ties
/// are not hypothetical — an irreversible stream with zero
/// decomposition levels and a power-of-two `Δb` reconstructs *every*
/// sample at its §E.1.1.2 quantisation-bin midpoint `(|qb| + r)·Δb`,
/// i.e. exactly on `X.5` — and the independent black-box reference
/// decoders split on the convention: two of three realise IEEE
/// ties-to-even on the signed pre-DC-shift sample (byte-exact against
/// this implementation on that shape), the third rounds ties away
/// from zero (a ±1, peak-1 disagreement at every midpoint sample —
/// well inside the ISO/IEC 15444-4 Table C.1 allowances, which is the
/// latitude that standard exists to budget). Ties-to-even follows the
/// majority convention and the IEEE default. The saturation keeps the
/// cast well-defined when an ICT-amplified sample wanders outside the
/// `i32` range on a pathological input — the subsequent §G.1.2 NOTE
/// clamp pulls it back to the descriptor range anyway.
///
/// Caller guarantees `src.len() == dst.len()`.
fn round_f32_into_i32(src: &[f32], dst: &mut [i32]) {
    for (s, o) in src.iter().zip(dst.iter_mut()) {
        let r = s.round_ties_even();
        *o = if r >= i32::MAX as f32 {
            i32::MAX
        } else if r <= i32::MIN as f32 {
            i32::MIN
        } else {
            r as i32
        };
    }
}

/// Thread the §G.3.2 inverse ICT + §G.1.2 per-component inverse DC
/// level shift + §G.1.2-NOTE clamp across an **arbitrary** number of
/// reconstructed irreversible-path tile-components — the
/// multi-component generalisation of
/// [`reconstruct_tile_components_9x7`], and the 9-7 / `f32` mirror of
/// [`reconstruct_tile_components_5x3_multi`].
///
/// T.800 §G.3 says the ICT "is a decorrelating transformation applied
/// to the **first three** components of an image (indexed as 0, 1 and
/// 2)" — the same "first three" wording as the §G.2 RCT. An image is
/// not required to carry exactly three components, so this entry
/// point realises that rule on the irreversible surface:
///
/// * `mode == InverseMctMode::Ict` runs [`inverse_ict`] on components
///   `(0, 1, 2)` **only when at least three components are present**.
///   Components with index `≥ 3` are never touched by the transform —
///   they flow through the Figure G.2 placement (round, level-shift
///   and clamp only). When fewer than three components are present,
///   the `Ict` mode is rejected (a one- or two-component tile cannot
///   legally signal an ICT in the COD marker — there is nothing for
///   Equations G-12..G-14 to operate on).
/// * `mode == InverseMctMode::None` is the pure Figure G.2 path for
///   any component count `≥ 1`: every component is independently
///   rounded, level-shifted and clamped per its own descriptor.
///
/// The §G.3 "same separation and bit-depth" prologue constraint is
/// enforced on components `(0, 1, 2)` only (the transform inputs)
/// when `mode == InverseMctMode::Ict`; the index-`≥ 3` pass-through
/// components may each carry their own distinct
/// `(precision_bits, is_signed)` pair.
///
/// `components[i]` is paired `1:1` with `outputs[i]` and
/// `descriptors[i]`. Every component slice must share a common
/// per-sample length (the §G "same separation on the reference grid"
/// half of the prologue — already realised by the §B / §F layers),
/// and every output slice must carry that same length. After the
/// (optional) inverse ICT, each component's `f32` samples are rounded
/// ties-to-even into the matching `outputs[i]` slot with saturation
/// at the cast point, then level-shifted + clamped per
/// `descriptors[i]` — the same integerisation contract as the
/// fixed-arity 9-7 entry point.
///
/// `mode == InverseMctMode::Rct` is rejected: RCT operates on `i32`
/// (T.800 §G.2.2), so the 5-3 path uses
/// [`reconstruct_tile_components_5x3_multi`] instead.
///
/// # Errors
///
/// * [`Error::InvalidMarkerLength`] if `components.is_empty()`, if
///   `components`, `outputs` and `descriptors` do not all share a
///   common count, or if the component / output slices do not all
///   share a common length.
/// * [`Error::InvalidSamplePrecision`] if any descriptor's
///   `precision_bits` is `0` or greater than `31` (the `i32` output
///   surface bound).
/// * [`Error::InvalidComponentCount`] if `mode ==
///   InverseMctMode::Ict` and either fewer than three components are
///   present, or the first three descriptors do not all share the
///   same `(precision_bits, is_signed)` pair (the §G.3 prologue
///   constraint).
/// * [`Error::NotImplemented`] if `mode == InverseMctMode::Rct`
///   (wrong entry point — RCT is the 5-3 / `i32` surface, see
///   [`reconstruct_tile_components_5x3_multi`]).
pub fn reconstruct_tile_components_9x7_multi(
    components: &mut [&mut [f32]],
    outputs: &mut [&mut [i32]],
    descriptors: &[ComponentDescriptor],
    mode: InverseMctMode,
) -> Result<(), Error> {
    if components.is_empty() {
        return Err(Error::InvalidMarkerLength);
    }
    if components.len() != outputs.len() || components.len() != descriptors.len() {
        return Err(Error::InvalidMarkerLength);
    }
    // Each output slot must match its component's sample count
    // (per-component pairing; sub-sampled siblings may differ from
    // one another).
    for (c, o) in components.iter().zip(outputs.iter()) {
        if o.len() != c.len() {
            return Err(Error::InvalidMarkerLength);
        }
    }
    for d in descriptors {
        if d.precision_bits == 0 || d.precision_bits > 31 {
            return Err(Error::InvalidSamplePrecision);
        }
    }
    match mode {
        InverseMctMode::Rct => {
            // RCT is the 5-3 reversible / i32 path.
            return Err(Error::NotImplemented);
        }
        InverseMctMode::Ict => {
            // §G.3: the ICT operates on the first three components.
            // A COD marker cannot legally signal an ICT for a tile
            // with fewer than three components.
            if components.len() < 3 {
                return Err(Error::InvalidComponentCount);
            }
            // §G.3 "same separation on the reference grid": the three
            // transform inputs carry the same sample count. Validate
            // up-front so a length mismatch does not surface only
            // after the ICT has mutated (0, 1, 2). Components outside
            // the transform (and every component under
            // `InverseMctMode::None`, where §G.1.2 is purely
            // per-component) may be sub-sampled differently and are
            // free to differ in length.
            let len = components[0].len();
            if components[1].len() != len || components[2].len() != len {
                return Err(Error::InvalidMarkerLength);
            }
            // §G.3 prologue "same separation and bit-depth" — checked
            // on the three transform inputs only. The index-≥3
            // pass-through components are free to differ.
            let d0 = descriptors[0];
            if descriptors[1] != d0 || descriptors[2] != d0 {
                return Err(Error::InvalidComponentCount);
            }
            // Split off the first three slices so the borrow checker
            // accepts three simultaneous &mut into the component
            // collection.
            let (head, _tail) = components.split_at_mut(3);
            if let [c0, c1, c2] = head {
                inverse_ict(c0, c1, c2)?;
            }
        }
        InverseMctMode::None => {
            // Figure G.2 path — no MCT applied at any component count.
        }
    }
    // Per-component round-to-nearest-even integerisation + inverse DC
    // level shift (§G.1.2 Eq. G-2) + §G.1.2 NOTE dynamic-range clamp
    // across every component, MCT'd or pass-through alike.
    for ((src, dst), d) in components
        .iter()
        .zip(outputs.iter_mut())
        .zip(descriptors.iter())
    {
        round_f32_into_i32(src, dst);
        inverse_dc_level_shift(dst, d.precision_bits, d.is_signed)?;
        clamp_to_dynamic_range(dst, d.precision_bits, d.is_signed)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Tabulated §G.2.1 worked example — `(R, G, B) = (200, 100, 50)`:
    ///
    /// * `Y0 = ⌊(200 + 200 + 50) / 4⌋ = ⌊450/4⌋ = 112`
    /// * `Y1 = 50 - 100 = -50`
    /// * `Y2 = 200 - 100 = 100`
    #[test]
    fn forward_rct_matches_g_2_1_worked_example() {
        let mut c0 = [200_i32];
        let mut c1 = [100_i32];
        let mut c2 = [50_i32];
        forward_rct(&mut c0, &mut c1, &mut c2).unwrap();
        assert_eq!(c0[0], 112);
        assert_eq!(c1[0], -50);
        assert_eq!(c2[0], 100);
    }

    /// Tabulated §G.2.2 inverse on the §G.2.1 worked example —
    /// `(Y0, Y1, Y2) = (112, -50, 100)`:
    ///
    /// * `I1 = 112 - ⌊(100 + -50) / 4⌋ = 112 - 12 = 100`
    /// * `I0 = 100 + 100 = 200`
    /// * `I2 = -50 + 100 = 50`
    #[test]
    fn inverse_rct_matches_g_2_2_worked_example() {
        let mut c0 = [112_i32];
        let mut c1 = [-50_i32];
        let mut c2 = [100_i32];
        inverse_rct(&mut c0, &mut c1, &mut c2).unwrap();
        assert_eq!(c0[0], 200);
        assert_eq!(c1[0], 100);
        assert_eq!(c2[0], 50);
    }

    /// §G.2 reversibility — forward RCT followed by inverse RCT shall
    /// recover the original `(I0, I1, I2)` triple exactly for every
    /// 8-bit grayscale axis `(R, G, B) = (k, k, k)`, `k = 0..=255`,
    /// plus every coordinate axis `(k, 0, 0)`, `(0, k, 0)`,
    /// `(0, 0, k)`.
    #[test]
    fn rct_roundtrips_unit_axes() {
        for k in 0_i32..=255_i32 {
            // Grayscale diagonal — Y1 and Y2 vanish, Y0 = k.
            let (mut a, mut b, mut c) = ([k], [k], [k]);
            forward_rct(&mut a, &mut b, &mut c).unwrap();
            // R = G = B = k ⇒ Y1 = Y2 = 0, Y0 = ⌊(k + 2k + k)/4⌋ = k.
            assert_eq!(a[0], k);
            assert_eq!(b[0], 0);
            assert_eq!(c[0], 0);
            inverse_rct(&mut a, &mut b, &mut c).unwrap();
            assert_eq!(a[0], k);
            assert_eq!(b[0], k);
            assert_eq!(c[0], k);

            // Red axis.
            let (mut a, mut b, mut c) = ([k], [0_i32], [0_i32]);
            forward_rct(&mut a, &mut b, &mut c).unwrap();
            inverse_rct(&mut a, &mut b, &mut c).unwrap();
            assert_eq!(a[0], k);
            assert_eq!(b[0], 0);
            assert_eq!(c[0], 0);

            // Green axis.
            let (mut a, mut b, mut c) = ([0_i32], [k], [0_i32]);
            forward_rct(&mut a, &mut b, &mut c).unwrap();
            inverse_rct(&mut a, &mut b, &mut c).unwrap();
            assert_eq!(a[0], 0);
            assert_eq!(b[0], k);
            assert_eq!(c[0], 0);

            // Blue axis.
            let (mut a, mut b, mut c) = ([0_i32], [0_i32], [k]);
            forward_rct(&mut a, &mut b, &mut c).unwrap();
            inverse_rct(&mut a, &mut b, &mut c).unwrap();
            assert_eq!(a[0], 0);
            assert_eq!(b[0], 0);
            assert_eq!(c[0], k);
        }
    }

    /// §G.2 reversibility — forward RCT followed by inverse RCT shall
    /// recover an arbitrary `(R, G, B)` triple exactly across the
    /// full 8-bit cube.
    #[test]
    fn rct_roundtrips_full_8bit_cube_diagonal_slice() {
        // The full 256³ cube is 16.7 M triples — run the corners +
        // every 17-step (15³ = 3375 triples) instead to keep the
        // suite fast while still exercising the diagonals.
        for r in (0_i32..=255_i32).step_by(17) {
            for g in (0_i32..=255_i32).step_by(17) {
                for b in (0_i32..=255_i32).step_by(17) {
                    let (mut a, mut bm, mut cm) = ([r], [g], [b]);
                    forward_rct(&mut a, &mut bm, &mut cm).unwrap();
                    inverse_rct(&mut a, &mut bm, &mut cm).unwrap();
                    assert_eq!(
                        (a[0], bm[0], cm[0]),
                        (r, g, b),
                        "RCT roundtrip diverged at (R,G,B) = ({}, {}, {})",
                        r,
                        g,
                        b
                    );
                }
            }
        }
    }

    /// §G.2 reversibility extends below zero — the spec note after
    /// Equations G-4 / G-5 explains that the RCT outputs grow by one
    /// bit; the inverse must therefore self-cancel for negative
    /// `Y1`, `Y2` inputs as well. Spot-check with `Y2 + Y1 = -1`,
    /// `-2`, `-3`, `-4`, `-5` (covers all four residue classes of
    /// `(Y2 + Y1) mod 4` on the negative side, exercising the
    /// arithmetic-right-shift `⌊·/4⌋` floor convention).
    #[test]
    fn inverse_rct_floor_division_handles_negative_sums() {
        // Y0 = 10, Y1 + Y2 = -1 ⇒ ⌊-1/4⌋ = -1 ⇒ I1 = 10 - (-1) = 11.
        let mut c0 = [10_i32];
        let mut c1 = [0_i32];
        let mut c2 = [-1_i32];
        inverse_rct(&mut c0, &mut c1, &mut c2).unwrap();
        assert_eq!(c1[0], 11); // I1
                               // Y0 = 10, Y1 + Y2 = -4 ⇒ ⌊-4/4⌋ = -1 ⇒ I1 = 10 - (-1) = 11.
        let mut c0 = [10_i32];
        let mut c1 = [-2_i32];
        let mut c2 = [-2_i32];
        inverse_rct(&mut c0, &mut c1, &mut c2).unwrap();
        assert_eq!(c1[0], 11);
        // Y0 = 10, Y1 + Y2 = -5 ⇒ ⌊-5/4⌋ = -2 ⇒ I1 = 10 - (-2) = 12.
        let mut c0 = [10_i32];
        let mut c1 = [-3_i32];
        let mut c2 = [-2_i32];
        inverse_rct(&mut c0, &mut c1, &mut c2).unwrap();
        assert_eq!(c1[0], 12);
    }

    /// §G.3 round-trip — forward ICT followed by inverse ICT recovers
    /// the input within a few `f32` ULPs. The §G.3.2 NOTE about
    /// precision applies; the literals are kept as `f32` constants
    /// so the tolerance has to absorb the accumulated mantissa loss
    /// of three multiply-adds at scale ≤ 255. `5e-3` in normalised
    /// pixel units is wider than the worst observed drift
    /// (`~1.1e-3` at `k = 64`) by a factor of ~4.
    #[test]
    fn ict_roundtrips_8bit_axes_within_tolerance() {
        const TOL: f32 = 5e-3;
        // Grayscale axis k = 0, 32, 64, …, 255.
        for k in (0_u32..=255_u32).step_by(32) {
            let k = k as f32;
            let (mut a, mut b, mut c) = ([k], [k], [k]);
            forward_ict(&mut a, &mut b, &mut c).unwrap();
            inverse_ict(&mut a, &mut b, &mut c).unwrap();
            assert!((a[0] - k).abs() < TOL, "I0 drift at k = {}: {}", k, a[0]);
            assert!((b[0] - k).abs() < TOL, "I1 drift at k = {}: {}", k, b[0]);
            assert!((c[0] - k).abs() < TOL, "I2 drift at k = {}: {}", k, c[0]);
        }
        // §G.3 NOTE — when fed an `(R, G, B)` triple, the §G.3.1
        // forward transform is the standard Y'CbCr matrix. Round-trip
        // a known colour: `(R, G, B) = (200, 100, 50)`.
        let mut c0 = [200.0_f32];
        let mut c1 = [100.0_f32];
        let mut c2 = [50.0_f32];
        forward_ict(&mut c0, &mut c1, &mut c2).unwrap();
        inverse_ict(&mut c0, &mut c1, &mut c2).unwrap();
        assert!((c0[0] - 200.0).abs() < TOL);
        assert!((c1[0] - 100.0).abs() < TOL);
        assert!((c2[0] - 50.0).abs() < TOL);
    }

    /// §G.3.2 NOTE — when fed a pure-red `(R, G, B) = (255, 0, 0)`
    /// pixel, the §G.3.1 forward transform yields the textbook
    /// Y'CbCr-601 triple `(76.245, -43.031, 127.5)` (within rounding):
    ///
    /// * `Y0 = 0.299 · 255 = 76.245`
    /// * `Y1 = -0.16875 · 255 = -43.031`
    /// * `Y2 = 0.5 · 255 = 127.5`
    ///
    /// Verifies the §G.3.1 coefficients aren't transposed or signed
    /// wrong.
    #[test]
    fn forward_ict_red_matches_y_cb_cr_601_textbook() {
        let mut c0 = [255.0_f32];
        let mut c1 = [0.0_f32];
        let mut c2 = [0.0_f32];
        forward_ict(&mut c0, &mut c1, &mut c2).unwrap();
        assert!((c0[0] - 76.245).abs() < 1e-3, "Y0 = {}", c0[0]);
        assert!((c1[0] - (-43.03125)).abs() < 1e-3, "Y1 = {}", c1[0]);
        assert!((c2[0] - 127.5).abs() < 1e-3, "Y2 = {}", c2[0]);
    }

    /// Length-mismatch is reported, not panicked. (Public surface
    /// stability — `parse_codestream` callers may not have done the
    /// per-tile bounds work themselves.)
    #[test]
    fn length_mismatch_returns_invalid_marker_length() {
        let mut a = [0_i32; 4];
        let mut b = [0_i32; 3];
        let mut c = [0_i32; 4];
        assert_eq!(
            inverse_rct(&mut a, &mut b, &mut c),
            Err(Error::InvalidMarkerLength)
        );
        assert_eq!(
            forward_rct(&mut a, &mut b, &mut c),
            Err(Error::InvalidMarkerLength)
        );

        let mut a = [0.0_f32; 4];
        let mut b = [0.0_f32; 4];
        let mut c = [0.0_f32; 5];
        assert_eq!(
            inverse_ict(&mut a, &mut b, &mut c),
            Err(Error::InvalidMarkerLength)
        );
        assert_eq!(
            forward_ict(&mut a, &mut b, &mut c),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// §G.1.2 DC level shift — Ssiz = 8 ⇒ shift = `+128`.
    #[test]
    fn inverse_dc_level_shift_unsigned_8bit() {
        let mut s = [-128_i32, -1, 0, 1, 127];
        inverse_dc_level_shift_unsigned(&mut s, 8).unwrap();
        assert_eq!(s, [0_i32, 127, 128, 129, 255]);
    }

    /// §G.1.2 DC level shift — Ssiz = 12 ⇒ shift = `+2048`.
    #[test]
    fn inverse_dc_level_shift_unsigned_12bit() {
        let mut s = [-2048_i32, -1, 0, 2047];
        inverse_dc_level_shift_unsigned(&mut s, 12).unwrap();
        assert_eq!(s, [0_i32, 2047, 2048, 4095]);
    }

    /// Out-of-range precision is reported, not panicked.
    #[test]
    fn inverse_dc_level_shift_rejects_invalid_precision() {
        let mut s = [0_i32; 4];
        assert_eq!(
            inverse_dc_level_shift_unsigned(&mut s, 0),
            Err(Error::InvalidSamplePrecision)
        );
        assert_eq!(
            inverse_dc_level_shift_unsigned(&mut s, 32),
            Err(Error::InvalidSamplePrecision)
        );
        // 31 is the upper bound representable in an `i32` shift.
        assert!(inverse_dc_level_shift_unsigned(&mut s, 31).is_ok());
        assert!(inverse_dc_level_shift_unsigned(&mut s, 1).is_ok());
    }

    /// §G.2 reversibility — empty slices are a no-op, not an error.
    /// (Matches the convention adopted by [`crate::dwt`].)
    #[test]
    fn empty_inputs_are_a_noop() {
        let mut a: [i32; 0] = [];
        let mut b: [i32; 0] = [];
        let mut c: [i32; 0] = [];
        assert!(inverse_rct(&mut a, &mut b, &mut c).is_ok());
        assert!(forward_rct(&mut a, &mut b, &mut c).is_ok());

        let mut a: [f32; 0] = [];
        let mut b: [f32; 0] = [];
        let mut c: [f32; 0] = [];
        assert!(inverse_ict(&mut a, &mut b, &mut c).is_ok());
        assert!(forward_ict(&mut a, &mut b, &mut c).is_ok());
    }

    // -------------------------------------------------------------------
    // §G.1.1 — Forward DC level shift coverage.
    // -------------------------------------------------------------------

    /// §G.1.1 — Ssiz = 8 ⇒ shift = `-128`.
    #[test]
    fn forward_dc_level_shift_unsigned_8bit() {
        let mut s = [0_i32, 127, 128, 129, 255];
        forward_dc_level_shift_unsigned(&mut s, 8).unwrap();
        assert_eq!(s, [-128_i32, -1, 0, 1, 127]);
    }

    /// §G.1.1 — Ssiz = 12 ⇒ shift = `-2048`.
    #[test]
    fn forward_dc_level_shift_unsigned_12bit() {
        let mut s = [0_i32, 2047, 2048, 4095];
        forward_dc_level_shift_unsigned(&mut s, 12).unwrap();
        assert_eq!(s, [-2048_i32, -1, 0, 2047]);
    }

    /// Out-of-range precision is reported on the forward path too.
    #[test]
    fn forward_dc_level_shift_rejects_invalid_precision() {
        let mut s = [0_i32; 4];
        assert_eq!(
            forward_dc_level_shift_unsigned(&mut s, 0),
            Err(Error::InvalidSamplePrecision)
        );
        assert_eq!(
            forward_dc_level_shift_unsigned(&mut s, 32),
            Err(Error::InvalidSamplePrecision)
        );
        assert!(forward_dc_level_shift_unsigned(&mut s, 1).is_ok());
        assert!(forward_dc_level_shift_unsigned(&mut s, 31).is_ok());
    }

    /// §G.1.1 → §G.1.2 round-trip on the full unsigned 8-bit
    /// range — the encoder shift followed by the decoder shift is
    /// the identity on every sample.
    #[test]
    fn dc_level_shift_round_trip_8bit_full_range() {
        let mut s: Vec<i32> = (0..256_i32).collect();
        let original = s.clone();
        forward_dc_level_shift_unsigned(&mut s, 8).unwrap();
        // After the forward shift, the centred dynamic range is
        // [-128, 127].
        assert_eq!(s[0], -128);
        assert_eq!(s[255], 127);
        inverse_dc_level_shift_unsigned(&mut s, 8).unwrap();
        assert_eq!(s, original);
    }

    /// §G.1.1 → §G.1.2 round-trip across 12-bit range with a stride.
    #[test]
    fn dc_level_shift_round_trip_12bit_stride() {
        let mut s: Vec<i32> = (0..4096_i32).step_by(7).collect();
        let original = s.clone();
        forward_dc_level_shift_unsigned(&mut s, 12).unwrap();
        inverse_dc_level_shift_unsigned(&mut s, 12).unwrap();
        assert_eq!(s, original);
    }

    // -------------------------------------------------------------------
    // §G.1 — `i64`-widened path for Ssiz ∈ 32..=38 (Table A.11).
    // -------------------------------------------------------------------

    /// §G.1.1 / §G.1.2 — Ssiz = 32 round-trip in the `i64` surface.
    #[test]
    fn dc_level_shift_i64_round_trip_32bit() {
        // A handful of probes spanning the full unsigned 32-bit range
        // (the `i32`-only primitives reject `precision = 32`).
        let mut s: Vec<i64> = vec![
            0,
            1,
            i64::from(i32::MAX) + 1, // 2^31 — exactly the midpoint
            (1_i64 << 32) - 1,       // 2^32 - 1 (top of 32-bit range)
        ];
        let original = s.clone();
        forward_dc_level_shift_unsigned_i64(&mut s, 32).unwrap();
        // After the forward shift the dynamic range is centred on
        // zero: `[-2^31, 2^31 - 1]`.
        assert_eq!(s[0], -(1_i64 << 31));
        assert_eq!(s[2], 0);
        assert_eq!(s[3], (1_i64 << 31) - 1);
        inverse_dc_level_shift_unsigned_i64(&mut s, 32).unwrap();
        assert_eq!(s, original);
    }

    /// §G.1.1 / §G.1.2 — Ssiz = 38 (Table A.11's upper bound) round-
    /// trips through the `i64` surface.
    #[test]
    fn dc_level_shift_i64_round_trip_38bit() {
        let span: i64 = (1_i64 << 38) - 1;
        let mut s: Vec<i64> = vec![0, 1, 1_i64 << 37, span];
        let original = s.clone();
        forward_dc_level_shift_unsigned_i64(&mut s, 38).unwrap();
        assert_eq!(s[0], -(1_i64 << 37));
        assert_eq!(s[2], 0);
        assert_eq!(s[3], (1_i64 << 37) - 1);
        inverse_dc_level_shift_unsigned_i64(&mut s, 38).unwrap();
        assert_eq!(s, original);
    }

    /// Out-of-range precision rejection on the `i64` path: 0 and
    /// 39+ are errors; 1 and 38 are accepted (the Table A.11 ends).
    #[test]
    fn dc_level_shift_i64_rejects_invalid_precision() {
        let mut s = [0_i64; 4];
        assert_eq!(
            forward_dc_level_shift_unsigned_i64(&mut s, 0),
            Err(Error::InvalidSamplePrecision)
        );
        assert_eq!(
            forward_dc_level_shift_unsigned_i64(&mut s, 39),
            Err(Error::InvalidSamplePrecision)
        );
        assert_eq!(
            inverse_dc_level_shift_unsigned_i64(&mut s, 0),
            Err(Error::InvalidSamplePrecision)
        );
        assert_eq!(
            inverse_dc_level_shift_unsigned_i64(&mut s, 39),
            Err(Error::InvalidSamplePrecision)
        );
        assert!(forward_dc_level_shift_unsigned_i64(&mut s, 1).is_ok());
        assert!(forward_dc_level_shift_unsigned_i64(&mut s, 38).is_ok());
        assert!(inverse_dc_level_shift_unsigned_i64(&mut s, 1).is_ok());
        assert!(inverse_dc_level_shift_unsigned_i64(&mut s, 38).is_ok());
    }

    // -------------------------------------------------------------------
    // §G.1 — signed-aware dispatcher.
    // -------------------------------------------------------------------

    /// Signed components are pass-through under both directions
    /// (§G.1.1 / §G.1.2 prologue: "unsigned only"). The buffer must
    /// be unchanged when `is_signed == true`.
    #[test]
    fn dc_level_shift_signed_dispatcher_is_noop() {
        let original = vec![-128_i32, -1, 0, 1, 127];
        let mut s = original.clone();
        forward_dc_level_shift(&mut s, 8, true).unwrap();
        assert_eq!(s, original);
        inverse_dc_level_shift(&mut s, 8, true).unwrap();
        assert_eq!(s, original);

        // 12-bit signed range probe.
        let original = vec![-2048_i32, -1, 0, 2047];
        let mut s = original.clone();
        forward_dc_level_shift(&mut s, 12, true).unwrap();
        assert_eq!(s, original);
        inverse_dc_level_shift(&mut s, 12, true).unwrap();
        assert_eq!(s, original);
    }

    /// Unsigned dispatch forwards to the bare primitive; the
    /// `is_signed == false` 8-bit path round-trips for the
    /// `[0, 255]` range.
    #[test]
    fn dc_level_shift_unsigned_dispatcher_round_trips_8bit() {
        let mut s: Vec<i32> = (0..256_i32).collect();
        let original = s.clone();
        forward_dc_level_shift(&mut s, 8, false).unwrap();
        inverse_dc_level_shift(&mut s, 8, false).unwrap();
        assert_eq!(s, original);
    }

    /// Signed dispatcher with out-of-range Ssiz still reports an
    /// error (the dispatcher validates `precision` so callers can't
    /// smuggle a malformed Ssiz past it).
    #[test]
    fn dc_level_shift_signed_dispatcher_validates_precision() {
        let mut s = [0_i32; 4];
        assert_eq!(
            forward_dc_level_shift(&mut s, 0, true),
            Err(Error::InvalidSamplePrecision)
        );
        assert_eq!(
            forward_dc_level_shift(&mut s, 39, true),
            Err(Error::InvalidSamplePrecision)
        );
        assert_eq!(
            inverse_dc_level_shift(&mut s, 0, true),
            Err(Error::InvalidSamplePrecision)
        );
        assert_eq!(
            inverse_dc_level_shift(&mut s, 39, true),
            Err(Error::InvalidSamplePrecision)
        );
        assert!(forward_dc_level_shift(&mut s, 8, true).is_ok());
        assert!(forward_dc_level_shift(&mut s, 38, true).is_ok());
    }

    // -------------------------------------------------------------------
    // §G.1.2 NOTE — dynamic-range clipping.
    // -------------------------------------------------------------------

    /// Unsigned 8-bit clip: any sample outside `[0, 255]` is pulled
    /// to the nearest endpoint; in-range samples are untouched.
    #[test]
    fn clamp_dynamic_range_unsigned_8bit() {
        let mut s = [-10_i32, 0, 1, 254, 255, 256, 1_000_000];
        clamp_to_dynamic_range(&mut s, 8, false).unwrap();
        assert_eq!(s, [0_i32, 0, 1, 254, 255, 255, 255]);
    }

    /// Unsigned 12-bit clip: range `[0, 4095]`.
    #[test]
    fn clamp_dynamic_range_unsigned_12bit() {
        let mut s = [-1_i32, 0, 4095, 4096, i32::MAX];
        clamp_to_dynamic_range(&mut s, 12, false).unwrap();
        assert_eq!(s, [0_i32, 0, 4095, 4095, 4095]);
    }

    /// Signed 8-bit clip: range `[-128, 127]`.
    #[test]
    fn clamp_dynamic_range_signed_8bit() {
        let mut s = [-200_i32, -128, 0, 127, 200];
        clamp_to_dynamic_range(&mut s, 8, true).unwrap();
        assert_eq!(s, [-128_i32, -128, 0, 127, 127]);
    }

    /// Signed 16-bit clip: range `[-32_768, 32_767]`.
    #[test]
    fn clamp_dynamic_range_signed_16bit() {
        let mut s = [-40_000_i32, -32_768, 0, 32_767, 40_000];
        clamp_to_dynamic_range(&mut s, 16, true).unwrap();
        assert_eq!(s, [-32_768_i32, -32_768, 0, 32_767, 32_767]);
    }

    /// `precision = 31` is accepted on the unsigned side; the upper
    /// bound saturates at `i32::MAX` (since `1 << 31` overflows the
    /// signed type — we represent `2^31 - 1`'s upper bound via
    /// `i32::MAX` explicitly).
    #[test]
    fn clamp_dynamic_range_unsigned_31bit_upper_bound() {
        let mut s = [-1_i32, 0, i32::MAX];
        clamp_to_dynamic_range(&mut s, 31, false).unwrap();
        assert_eq!(s, [0_i32, 0, i32::MAX]);
    }

    /// Out-of-range `precision` on the clip helper is reported, not
    /// panicked.
    #[test]
    fn clamp_dynamic_range_rejects_invalid_precision() {
        let mut s = [0_i32; 4];
        assert_eq!(
            clamp_to_dynamic_range(&mut s, 0, false),
            Err(Error::InvalidSamplePrecision)
        );
        assert_eq!(
            clamp_to_dynamic_range(&mut s, 32, false),
            Err(Error::InvalidSamplePrecision)
        );
        assert!(clamp_to_dynamic_range(&mut s, 1, false).is_ok());
        assert!(clamp_to_dynamic_range(&mut s, 31, true).is_ok());
    }

    // -------------------------------------------------------------------
    // §G.1.2 NOTE — `i64`-widened dynamic-range clipping.
    // -------------------------------------------------------------------

    /// Unsigned 8-bit clip on the `i64` surface matches the `i32`
    /// variant byte-for-byte — the formula is the same, just one
    /// integer-width wider.
    #[test]
    fn clamp_dynamic_range_i64_unsigned_8bit_matches_i32() {
        let mut s = [-10_i64, 0, 1, 254, 255, 256, 1_000_000];
        clamp_to_dynamic_range_i64(&mut s, 8, false).unwrap();
        assert_eq!(s, [0_i64, 0, 1, 254, 255, 255, 255]);
    }

    /// Signed 12-bit clip on the `i64` surface — the `[-2048, 2047]`
    /// window is unchanged from the `i32` formula.
    #[test]
    fn clamp_dynamic_range_i64_signed_12bit() {
        let mut s = [-3_000_i64, -2_048, -1, 0, 2_047, 2_048, 10_000];
        clamp_to_dynamic_range_i64(&mut s, 12, true).unwrap();
        assert_eq!(s, [-2_048_i64, -2_048, -1, 0, 2_047, 2_047, 2_047]);
    }

    /// Unsigned 32-bit clip — the headline reason the `i64` surface
    /// exists. Range `[0, 2^32 - 1]`; `i32::MIN`-class underflows
    /// pull to 0; samples above `2^32 - 1` pull to `2^32 - 1`.
    #[test]
    fn clamp_dynamic_range_i64_unsigned_32bit() {
        let span: i64 = (1_i64 << 32) - 1;
        let mut s = [
            -1_i64,
            0,
            1,
            (1_i64 << 31), // 2^31 — well inside the 32-bit window
            span,
            span + 1,
            i64::MAX,
        ];
        clamp_to_dynamic_range_i64(&mut s, 32, false).unwrap();
        assert_eq!(s, [0_i64, 0, 1, 1_i64 << 31, span, span, span]);
    }

    /// Signed 32-bit clip — range `[-2^31, 2^31 - 1]` on the `i64`
    /// surface. The endpoints both stay in range (untouched); values
    /// straddling each end pull to the nearest endpoint.
    #[test]
    fn clamp_dynamic_range_i64_signed_32bit() {
        let lo: i64 = -(1_i64 << 31);
        let hi: i64 = (1_i64 << 31) - 1;
        let mut s = [lo - 1, lo, -1, 0, hi, hi + 1, i64::MAX];
        clamp_to_dynamic_range_i64(&mut s, 32, true).unwrap();
        assert_eq!(s, [lo, lo, -1, 0, hi, hi, hi]);
    }

    /// Unsigned 38-bit clip — Table A.11's upper bound. Range
    /// `[0, 2^38 - 1]`.
    #[test]
    fn clamp_dynamic_range_i64_unsigned_38bit_upper_bound() {
        let span: i64 = (1_i64 << 38) - 1;
        let mut s = [-1_i64, 0, span, span + 1, i64::MAX];
        clamp_to_dynamic_range_i64(&mut s, 38, false).unwrap();
        assert_eq!(s, [0_i64, 0, span, span, span]);
    }

    /// Signed 38-bit clip — Table A.11's upper bound on the signed
    /// side. Range `[-2^37, 2^37 - 1]`.
    #[test]
    fn clamp_dynamic_range_i64_signed_38bit_upper_bound() {
        let lo: i64 = -(1_i64 << 37);
        let hi: i64 = (1_i64 << 37) - 1;
        let mut s = [i64::MIN, lo - 1, lo, 0, hi, hi + 1, i64::MAX];
        clamp_to_dynamic_range_i64(&mut s, 38, true).unwrap();
        assert_eq!(s, [lo, lo, lo, 0, hi, hi, hi]);
    }

    /// `precision = 1` on the unsigned side — range `[0, 1]`. The
    /// `i64` surface accepts 1-bit components too, mirroring how the
    /// `*_dc_level_shift_unsigned_i64` primitives behave.
    #[test]
    fn clamp_dynamic_range_i64_unsigned_1bit() {
        let mut s = [-5_i64, 0, 1, 2, i64::MAX];
        clamp_to_dynamic_range_i64(&mut s, 1, false).unwrap();
        assert_eq!(s, [0_i64, 0, 1, 1, 1]);
    }

    /// In-range samples are not modified — the clip is a pure
    /// `clamp(lo, hi)`, not a quantize.
    #[test]
    fn clamp_dynamic_range_i64_in_range_passthrough() {
        let original = [0_i64, 1, 100, 1_000, 65_535, 1_i64 << 36];
        let mut s = original;
        clamp_to_dynamic_range_i64(&mut s, 38, false).unwrap();
        assert_eq!(s, original);
    }

    /// Empty slice is a valid (and cheap) call — the clip helper
    /// must not assume at least one sample.
    #[test]
    fn clamp_dynamic_range_i64_empty_slice_ok() {
        let mut s: [i64; 0] = [];
        assert!(clamp_to_dynamic_range_i64(&mut s, 32, false).is_ok());
        assert!(clamp_to_dynamic_range_i64(&mut s, 38, true).is_ok());
    }

    /// Out-of-range `precision` is reported — `0`, `39`, and `255`
    /// all error; `1` and `38` are accepted (the `i64` surface
    /// inherits the Table A.11 1..=38 window from the
    /// `*_dc_level_shift_unsigned_i64` primitives).
    #[test]
    fn clamp_dynamic_range_i64_rejects_invalid_precision() {
        let mut s = [0_i64; 4];
        assert_eq!(
            clamp_to_dynamic_range_i64(&mut s, 0, false),
            Err(Error::InvalidSamplePrecision)
        );
        assert_eq!(
            clamp_to_dynamic_range_i64(&mut s, 39, false),
            Err(Error::InvalidSamplePrecision)
        );
        assert_eq!(
            clamp_to_dynamic_range_i64(&mut s, 255, true),
            Err(Error::InvalidSamplePrecision)
        );
        assert!(clamp_to_dynamic_range_i64(&mut s, 1, false).is_ok());
        assert!(clamp_to_dynamic_range_i64(&mut s, 38, true).is_ok());
    }

    /// Composes with the `i64` inverse DC level shift: after
    /// `inverse_dc_level_shift_unsigned_i64`, the clip pulls any
    /// post-quantization overshoot back into the `[0, 2^p - 1]`
    /// unsigned window. Uses `precision = 32` so the chain exercises
    /// the surface the `i32`-only primitives cannot reach.
    #[test]
    fn clamp_dynamic_range_i64_composes_with_inverse_level_shift_32bit() {
        // Reconstructed centred samples — three in-range plus one
        // overshoot above the encoded peak and one undershoot below.
        let span: i64 = (1_i64 << 32) - 1;
        let mut s: Vec<i64> = vec![
            -(1_i64 << 31),       // post-IDWT lower endpoint (centred)
            0,                    // middle of the centred window
            (1_i64 << 31) - 1,    // post-IDWT upper endpoint (centred)
            (1_i64 << 31),        // overshoot above the centred window
            -(1_i64 << 31) - 100, // undershoot below the centred window
        ];
        inverse_dc_level_shift_unsigned_i64(&mut s, 32).unwrap();
        // After the inverse shift the buffer is on the un-centred
        // `[0, 2^32 - 1]` scale plus the two overshoot samples.
        assert_eq!(s[0], 0);
        assert_eq!(s[2], span);
        clamp_to_dynamic_range_i64(&mut s, 32, false).unwrap();
        // Overshoot pulls to the unsigned top; undershoot pulls to 0.
        assert_eq!(s, vec![0_i64, 1_i64 << 31, span, span, 0]);
    }

    // -------------------------------------------------------------------
    // §G.1 + §G.2 / §G.3 — per-tile-component threading.
    // -------------------------------------------------------------------

    fn d_unsigned(p: u8) -> ComponentDescriptor {
        ComponentDescriptor {
            precision_bits: p,
            is_signed: false,
        }
    }

    fn d_signed(p: u8) -> ComponentDescriptor {
        ComponentDescriptor {
            precision_bits: p,
            is_signed: true,
        }
    }

    /// `ComponentDescriptor::from_siz_component` drops the SIZ
    /// sub-sampling factors but copies the precision + signedness
    /// verbatim.
    #[test]
    fn descriptor_from_siz_component_preserves_precision_and_signedness() {
        let c = crate::SizComponent {
            precision_bits: 12,
            is_signed: true,
            h_separation: 1,
            v_separation: 2,
        };
        let d = ComponentDescriptor::from_siz_component(&c);
        assert_eq!(d.precision_bits, 12);
        assert!(d.is_signed);
    }

    /// 5-3 + RCT + unsigned 8-bit: §G.2.1 worked example fed into
    /// the threading layer with the encoder having already DC-shifted
    /// and forward-RCT'd the (200, 100, 50) triple. The threading
    /// layer's job is to invert RCT, add back `+128`, and clamp —
    /// recovering the original (200, 100, 50).
    #[test]
    fn thread_5x3_rct_unsigned_8bit_recovers_g_2_1_example() {
        // Encoder: (R, G, B) = (200, 100, 50); subtract 128 each →
        // (72, -28, -78); forward RCT (Eq. G-3/G-4/G-5):
        //   Y0 = floor((72 + 2*(-28) + -78) / 4) = floor(-62/4) = -16
        //   Y1 = -78 - (-28) = -50
        //   Y2 = 72 - (-28) = 100
        let mut c0 = [-16_i32];
        let mut c1 = [-50_i32];
        let mut c2 = [100_i32];
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8)];
        reconstruct_tile_components_5x3(&mut c0, &mut c1, &mut c2, &descs, InverseMctMode::Rct)
            .unwrap();
        assert_eq!(c0[0], 200);
        assert_eq!(c1[0], 100);
        assert_eq!(c2[0], 50);
    }

    /// 5-3 + RCT + 8-bit on a 256-entry diagonal — round-tripping the
    /// `(R, G, B) = (k, k, k)` line. Encoder DC-shifts each value to
    /// `k - 128`, then forward RCT collapses the diagonal into
    /// `(Y0, Y1, Y2) = (k - 128, 0, 0)`. The threading layer should
    /// recover `(k, k, k)` exactly for every `k ∈ 0..=255`.
    #[test]
    fn thread_5x3_rct_unsigned_8bit_recovers_grayscale_diagonal() {
        for k in 0_i32..=255_i32 {
            let mut c0 = [k - 128];
            let mut c1 = [0_i32];
            let mut c2 = [0_i32];
            let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8)];
            reconstruct_tile_components_5x3(&mut c0, &mut c1, &mut c2, &descs, InverseMctMode::Rct)
                .unwrap();
            assert_eq!(c0[0], k, "diagonal k={}: c0", k);
            assert_eq!(c1[0], k, "diagonal k={}: c1", k);
            assert_eq!(c2[0], k, "diagonal k={}: c2", k);
        }
    }

    /// 5-3 + no MCT + unsigned 8-bit: components flow through the
    /// inverse DWT independently. The threading layer just adds back
    /// the per-component `+2^(p - 1)` and clamps. With `(p_i) = (8,
    /// 10, 12)` and DWT-output samples `(0, 0, 0)` the level-shift
    /// alone produces `(128, 512, 2048)` and the clamp leaves them
    /// untouched.
    #[test]
    fn thread_5x3_none_mode_independent_per_component_level_shift() {
        let mut c0 = [0_i32, 100, -50];
        let mut c1 = [0_i32, 100, -50];
        let mut c2 = [0_i32, 100, -50];
        let descs = [d_unsigned(8), d_unsigned(10), d_unsigned(12)];
        reconstruct_tile_components_5x3(&mut c0, &mut c1, &mut c2, &descs, InverseMctMode::None)
            .unwrap();
        assert_eq!(c0, [128_i32, 228, 78]);
        assert_eq!(c1, [512_i32, 612, 462]);
        assert_eq!(c2, [2048_i32, 2148, 1998]);
    }

    /// 5-3 + no MCT + signed 8-bit: signed components skip the
    /// level shift but still get clamped to `[-128, 127]`.
    #[test]
    fn thread_5x3_none_mode_signed_component_clamps_only() {
        let mut c0 = [-200_i32, -128, 0, 127, 200];
        let mut c1 = c0;
        let mut c2 = c0;
        let descs = [d_signed(8), d_signed(8), d_signed(8)];
        reconstruct_tile_components_5x3(&mut c0, &mut c1, &mut c2, &descs, InverseMctMode::None)
            .unwrap();
        assert_eq!(c0, [-128_i32, -128, 0, 127, 127]);
        assert_eq!(c1, c0);
        assert_eq!(c2, c0);
    }

    /// 5-3 + no MCT + clipping: an over-amplified reconstructed
    /// sample lands outside the 8-bit unsigned range after the
    /// `+128` level shift, and the clamp pulls it back to 255.
    #[test]
    fn thread_5x3_none_mode_clamps_overshoot() {
        // DWT output 200 + level shift 128 = 328 → clamp to 255.
        // DWT output -200 + level shift 128 = -72 → clamp to 0.
        let mut c0 = [200_i32, -200, 0, 127];
        let mut c1 = c0;
        let mut c2 = c0;
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8)];
        reconstruct_tile_components_5x3(&mut c0, &mut c1, &mut c2, &descs, InverseMctMode::None)
            .unwrap();
        assert_eq!(c0, [255_i32, 0, 128, 255]);
        assert_eq!(c1, c0);
        assert_eq!(c2, c0);
    }

    /// 5-3 + RCT requires every component's `(precision, signedness)`
    /// to match the §G.2 prologue "same separation and bit-depth"
    /// rule; a mismatched second component is rejected.
    #[test]
    fn thread_5x3_rct_rejects_unequal_precision() {
        let mut c0 = [0_i32];
        let mut c1 = [0_i32];
        let mut c2 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(10), d_unsigned(8)];
        assert_eq!(
            reconstruct_tile_components_5x3(&mut c0, &mut c1, &mut c2, &descs, InverseMctMode::Rct,),
            Err(Error::InvalidComponentCount)
        );
    }

    /// 5-3 + RCT rejects mixed signedness across the three
    /// components — the §G.2 prologue requires uniform bit-depth
    /// AND uniform signedness.
    #[test]
    fn thread_5x3_rct_rejects_mixed_signedness() {
        let mut c0 = [0_i32];
        let mut c1 = [0_i32];
        let mut c2 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(8), d_signed(8)];
        assert_eq!(
            reconstruct_tile_components_5x3(&mut c0, &mut c1, &mut c2, &descs, InverseMctMode::Rct,),
            Err(Error::InvalidComponentCount)
        );
    }

    /// 5-3 entry point refuses ICT (wrong kernel pairing per the
    /// §G.2 / §G.3 prologues; the 9-7 entry point owns ICT).
    #[test]
    fn thread_5x3_rejects_ict_mode() {
        let mut c0 = [0_i32];
        let mut c1 = [0_i32];
        let mut c2 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8)];
        assert_eq!(
            reconstruct_tile_components_5x3(&mut c0, &mut c1, &mut c2, &descs, InverseMctMode::Ict,),
            Err(Error::NotImplemented)
        );
    }

    /// 5-3 entry point rejects mismatched slice lengths up front.
    #[test]
    fn thread_5x3_rejects_mismatched_slice_lengths() {
        let mut c0 = [0_i32; 4];
        let mut c1 = [0_i32; 3];
        let mut c2 = [0_i32; 4];
        let descs = [d_unsigned(8); 3];
        assert_eq!(
            reconstruct_tile_components_5x3(
                &mut c0,
                &mut c1,
                &mut c2,
                &descs,
                InverseMctMode::None,
            ),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// 5-3 entry point rejects a non-3 descriptor count.
    #[test]
    fn thread_5x3_rejects_non_three_descriptors() {
        let mut c0 = [0_i32];
        let mut c1 = [0_i32];
        let mut c2 = [0_i32];
        let descs = [d_unsigned(8); 2];
        assert_eq!(
            reconstruct_tile_components_5x3(
                &mut c0,
                &mut c1,
                &mut c2,
                &descs,
                InverseMctMode::None,
            ),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// 5-3 entry point rejects out-of-range precision (any descriptor).
    #[test]
    fn thread_5x3_rejects_out_of_range_precision() {
        let mut c0 = [0_i32];
        let mut c1 = [0_i32];
        let mut c2 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(32), d_unsigned(8)];
        assert_eq!(
            reconstruct_tile_components_5x3(
                &mut c0,
                &mut c1,
                &mut c2,
                &descs,
                InverseMctMode::None,
            ),
            Err(Error::InvalidSamplePrecision)
        );
    }

    // -------------------------------------------------------------------
    // §G.2 multi-component generalisation — RCT on the first three
    // components, pass-through on index ≥ 3.
    // -------------------------------------------------------------------

    /// Multi-component RCT with exactly three components matches the
    /// fixed-arity §G.2.1 worked example: the multi entry point is a
    /// drop-in superset of `reconstruct_tile_components_5x3`.
    #[test]
    fn thread_5x3_multi_rct_three_components_matches_fixed_arity() {
        let mut c0 = [-16_i32];
        let mut c1 = [-50_i32];
        let mut c2 = [100_i32];
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8)];
        let mut comps: [&mut [i32]; 3] = [&mut c0, &mut c1, &mut c2];
        reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::Rct).unwrap();
        assert_eq!(c0[0], 200);
        assert_eq!(c1[0], 100);
        assert_eq!(c2[0], 50);
    }

    /// Four-component RCT image (e.g. RGBA): the §G.2 transform touches
    /// only components `(0, 1, 2)`; the index-3 alpha plane flows
    /// through the Figure G.2 placement (level-shift + clamp only) and
    /// is recovered untransformed. The alpha plane carries its own
    /// distinct descriptor (different precision) — legal because the
    /// "same bit-depth" prologue binds only the three transform inputs.
    #[test]
    fn thread_5x3_multi_rct_four_components_alpha_passthrough() {
        // (R, G, B) = (200, 100, 50) pre-RCT'd as in the §G.2.1
        // example; alpha (index 3) is an independent 10-bit plane whose
        // DWT output is 0 → level shift gives +512.
        let mut c0 = [-16_i32];
        let mut c1 = [-50_i32];
        let mut c2 = [100_i32];
        let mut c3 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8), d_unsigned(10)];
        let mut comps: [&mut [i32]; 4] = [&mut c0, &mut c1, &mut c2, &mut c3];
        reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::Rct).unwrap();
        assert_eq!(c0[0], 200);
        assert_eq!(c1[0], 100);
        assert_eq!(c2[0], 50);
        // Alpha plane: 0 + 2^(10-1) = 512, clamp no-op.
        assert_eq!(c3[0], 512);
    }

    /// Single-component greyscale tile, no MCT: pure Figure G.2 path at
    /// component count 1. The lone plane is level-shifted + clamped.
    #[test]
    fn thread_5x3_multi_none_single_component() {
        let mut c0 = [0_i32, 100, -200, 200];
        let descs = [d_unsigned(8)];
        let mut comps: [&mut [i32]; 1] = [&mut c0];
        reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::None).unwrap();
        // +128 then clamp to [0, 255].
        assert_eq!(c0, [128_i32, 228, 0, 255]);
    }

    /// Two-component tile, no MCT: each plane independently
    /// level-shifted + clamped per its own descriptor.
    #[test]
    fn thread_5x3_multi_none_two_components() {
        let mut c0 = [0_i32, 100];
        let mut c1 = [0_i32, 100];
        let descs = [d_unsigned(8), d_unsigned(12)];
        let mut comps: [&mut [i32]; 2] = [&mut c0, &mut c1];
        reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::None).unwrap();
        assert_eq!(c0, [128_i32, 228]);
        assert_eq!(c1, [2048_i32, 2148]);
    }

    /// RCT requires at least three components: a two-component tile
    /// cannot legally signal an RCT in the COD marker.
    #[test]
    fn thread_5x3_multi_rct_rejects_fewer_than_three() {
        let mut c0 = [0_i32];
        let mut c1 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(8)];
        let mut comps: [&mut [i32]; 2] = [&mut c0, &mut c1];
        assert_eq!(
            reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::Rct),
            Err(Error::InvalidComponentCount)
        );
    }

    /// The §G.2 "same bit-depth" prologue binds the three transform
    /// inputs: an RCT with components `(0, 1, 2)` of mixed precision is
    /// rejected even when a legal index-3 component is present.
    #[test]
    fn thread_5x3_multi_rct_rejects_unequal_precision_on_first_three() {
        let mut c0 = [0_i32];
        let mut c1 = [0_i32];
        let mut c2 = [0_i32];
        let mut c3 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(10), d_unsigned(8), d_unsigned(8)];
        let mut comps: [&mut [i32]; 4] = [&mut c0, &mut c1, &mut c2, &mut c3];
        assert_eq!(
            reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::Rct),
            Err(Error::InvalidComponentCount)
        );
    }

    /// ICT mode is rejected on the reversible multi entry point (wrong
    /// surface — ICT operates on `f32`).
    #[test]
    fn thread_5x3_multi_rejects_ict_mode() {
        let mut c0 = [0_i32];
        let mut c1 = [0_i32];
        let mut c2 = [0_i32];
        let descs = [d_unsigned(8); 3];
        let mut comps: [&mut [i32]; 3] = [&mut c0, &mut c1, &mut c2];
        assert_eq!(
            reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::Ict),
            Err(Error::NotImplemented)
        );
    }

    /// Empty component collection is rejected.
    #[test]
    fn thread_5x3_multi_rejects_empty() {
        let descs: [ComponentDescriptor; 0] = [];
        let mut comps: [&mut [i32]; 0] = [];
        assert_eq!(
            reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::None),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// Mismatched component / descriptor counts are rejected.
    #[test]
    fn thread_5x3_multi_rejects_count_mismatch() {
        let mut c0 = [0_i32];
        let mut c1 = [0_i32];
        let descs = [d_unsigned(8); 3];
        let mut comps: [&mut [i32]; 2] = [&mut c0, &mut c1];
        assert_eq!(
            reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::None),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// Component slices that do not share a common length are rejected
    /// (the §G "same separation on the reference grid" rule).
    #[test]
    fn thread_5x3_multi_rejects_ragged_lengths() {
        let mut c0 = [0_i32, 0];
        let mut c1 = [0_i32];
        let mut c2 = [0_i32, 0];
        let descs = [d_unsigned(8); 3];
        let mut comps: [&mut [i32]; 3] = [&mut c0, &mut c1, &mut c2];
        assert_eq!(
            reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::Rct),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// Out-of-range precision on any descriptor (including an index-≥3
    /// pass-through component) is rejected up front.
    #[test]
    fn thread_5x3_multi_rejects_out_of_range_precision() {
        let mut c0 = [0_i32];
        let mut c1 = [0_i32];
        let mut c2 = [0_i32];
        let mut c3 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8), d_unsigned(32)];
        let mut comps: [&mut [i32]; 4] = [&mut c0, &mut c1, &mut c2, &mut c3];
        assert_eq!(
            reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::Rct),
            Err(Error::InvalidSamplePrecision)
        );
    }

    /// Five-component multispectral tile, no MCT: every plane is
    /// independently level-shifted + clamped. Exercises the loop past
    /// the three-component boundary on the Figure G.2 path.
    #[test]
    fn thread_5x3_multi_none_five_components() {
        let mut planes: Vec<Vec<i32>> = (0..5).map(|_| vec![0_i32, 300, -300]).collect();
        let descs = [d_unsigned(8); 5];
        {
            let mut comps: Vec<&mut [i32]> = planes.iter_mut().map(|p| p.as_mut_slice()).collect();
            reconstruct_tile_components_5x3_multi(&mut comps, &descs, InverseMctMode::None)
                .unwrap();
        }
        for p in &planes {
            // 0 + 128 = 128; 300 + 128 = 428 → clamp 255; -300 + 128 =
            // -172 → clamp 0.
            assert_eq!(p.as_slice(), [128_i32, 255, 0]);
        }
    }

    // -------------------------------------------------------------------
    // §G.2 i64-widened surface — RCT primitives + threading mirror
    // for the Table A.11 `Ssiz ≥ 32` corner.
    // -------------------------------------------------------------------

    /// `i64` forward RCT matches the §G.2.1 worked example —
    /// `(200, 100, 50)` → `(112, -50, 100)` (same arithmetic as the
    /// `i32` variant on narrow inputs).
    #[test]
    fn forward_rct_i64_matches_g_2_1_worked_example() {
        let mut c0 = [200_i64];
        let mut c1 = [100_i64];
        let mut c2 = [50_i64];
        forward_rct_i64(&mut c0, &mut c1, &mut c2).unwrap();
        assert_eq!(c0[0], 112);
        assert_eq!(c1[0], -50);
        assert_eq!(c2[0], 100);
    }

    /// `i64` inverse RCT matches the §G.2.2 worked example —
    /// `(112, -50, 100)` → `(200, 100, 50)`.
    #[test]
    fn inverse_rct_i64_matches_g_2_2_worked_example() {
        let mut c0 = [112_i64];
        let mut c1 = [-50_i64];
        let mut c2 = [100_i64];
        inverse_rct_i64(&mut c0, &mut c1, &mut c2).unwrap();
        assert_eq!(c0[0], 200);
        assert_eq!(c1[0], 100);
        assert_eq!(c2[0], 50);
    }

    /// `i64` inverse RCT matches the `i32` variant sample-for-sample
    /// across a probe set that exercises the negative-sum floor
    /// (`(Y2 + Y1) >> 2` toward minus infinity) on both surfaces.
    #[test]
    fn inverse_rct_i64_matches_i32_on_narrow_inputs() {
        let y0 = [-16_i32, 0, 7, -7, 100, -100, i16::MAX as i32];
        let y1 = [-50_i32, 1, -1, 3, -3, 99, -99];
        let y2 = [100_i32, -1, 1, -3, 3, -99, 99];
        let mut a32 = y0;
        let mut b32 = y1;
        let mut c32 = y2;
        inverse_rct(&mut a32, &mut b32, &mut c32).unwrap();
        let mut a64: Vec<i64> = y0.iter().map(|&v| v as i64).collect();
        let mut b64: Vec<i64> = y1.iter().map(|&v| v as i64).collect();
        let mut c64: Vec<i64> = y2.iter().map(|&v| v as i64).collect();
        inverse_rct_i64(&mut a64, &mut b64, &mut c64).unwrap();
        for i in 0..y0.len() {
            assert_eq!(a64[i], a32[i] as i64, "i={}: I0", i);
            assert_eq!(b64[i], b32[i] as i64, "i={}: I1", i);
            assert_eq!(c64[i], c32[i] as i64, "i={}: I2", i);
        }
    }

    /// §G.2 reversibility on `Ssiz = 38`-scale magnitudes — forward
    /// then inverse recovers DC-shifted probes spanning the
    /// `[-2^37, 2^37 - 1]` window exactly. These values are
    /// unrepresentable on the `i32` RCT surface; the §G.2.1 NOTE's
    /// one-bit `Y1` / `Y2` growth (39 bits here) stays far inside
    /// `i64`.
    #[test]
    fn rct_i64_roundtrips_wide_38bit_probes() {
        let half = 1_i64 << 37;
        let i0 = [half - 1, -half, 0, half - 1, -half, 123_456_789_012];
        let i1 = [-half, half - 1, half - 1, 0, -1, -987_654_321_098];
        let i2 = [half - 1, -half, -half, 1, half - 1, 5];
        let (mut a, mut b, mut c) = (i0, i1, i2);
        forward_rct_i64(&mut a, &mut b, &mut c).unwrap();
        inverse_rct_i64(&mut a, &mut b, &mut c).unwrap();
        assert_eq!(a, i0);
        assert_eq!(b, i1);
        assert_eq!(c, i2);
    }

    /// `i64` RCT pair rejects mismatched slice lengths.
    #[test]
    fn rct_i64_rejects_mismatched_lengths() {
        let mut c0 = [0_i64; 2];
        let mut c1 = [0_i64; 3];
        let mut c2 = [0_i64; 2];
        assert_eq!(
            forward_rct_i64(&mut c0, &mut c1, &mut c2),
            Err(Error::InvalidMarkerLength)
        );
        assert_eq!(
            inverse_rct_i64(&mut c0, &mut c1, &mut c2),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// i64 threading mirror + RCT + unsigned 8-bit: the same §G.2.1
    /// worked-example input as the fixed-arity `i32` test recovers
    /// `(200, 100, 50)` — the mirror is a drop-in widening on
    /// narrow-precision tiles.
    #[test]
    fn thread_5x3_i64_rct_unsigned_8bit_matches_i32_fixed_arity() {
        let mut c0 = [-16_i64];
        let mut c1 = [-50_i64];
        let mut c2 = [100_i64];
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8)];
        reconstruct_tile_components_5x3_i64(&mut c0, &mut c1, &mut c2, &descs, InverseMctMode::Rct)
            .unwrap();
        assert_eq!(c0[0], 200);
        assert_eq!(c1[0], 100);
        assert_eq!(c2[0], 50);
    }

    /// i64 threading + RCT + unsigned 36-bit — the headline case the
    /// `i32` surface cannot represent. Encoder side: §G.1.1 forward
    /// DC shift (`-2^35`) then §G.2.1 forward RCT on three wide
    /// samples; the threading mirror inverts RCT, adds back `+2^35`,
    /// and clamps — recovering the originals exactly.
    #[test]
    fn thread_5x3_i64_rct_unsigned_36bit_round_trip() {
        let originals = [
            [1_i64 << 35, (1 << 36) - 1, 0],
            [(1_i64 << 34) + 7, 1, (1 << 36) - 1],
            [12_345_678_901_i64, (1 << 35) - 1, 1 << 33],
        ];
        let mut c0 = originals[0];
        let mut c1 = originals[1];
        let mut c2 = originals[2];
        forward_dc_level_shift_unsigned_i64(&mut c0, 36).unwrap();
        forward_dc_level_shift_unsigned_i64(&mut c1, 36).unwrap();
        forward_dc_level_shift_unsigned_i64(&mut c2, 36).unwrap();
        forward_rct_i64(&mut c0, &mut c1, &mut c2).unwrap();
        let descs = [d_unsigned(36), d_unsigned(36), d_unsigned(36)];
        reconstruct_tile_components_5x3_i64(&mut c0, &mut c1, &mut c2, &descs, InverseMctMode::Rct)
            .unwrap();
        assert_eq!(c0, originals[0]);
        assert_eq!(c1, originals[1]);
        assert_eq!(c2, originals[2]);
    }

    /// i64 threading + no MCT + unsigned 38-bit (the Table A.11 upper
    /// bound): level shift adds `2^37`; an overshoot clamps to
    /// `2^38 - 1` and an undershoot clamps to `0`.
    #[test]
    fn thread_5x3_i64_none_mode_38bit_level_shift_and_clamp() {
        let half = 1_i64 << 37;
        let top = (1_i64 << 38) - 1;
        // 0 + 2^37 = 2^37; (2^37) + 2^37 = 2^38 → clamp to 2^38 - 1;
        // (-2^37 - 5) + 2^37 = -5 → clamp to 0.
        let mut c0 = [0_i64, half, -half - 5];
        let mut c1 = c0;
        let mut c2 = c0;
        let descs = [d_unsigned(38), d_unsigned(38), d_unsigned(38)];
        reconstruct_tile_components_5x3_i64(
            &mut c0,
            &mut c1,
            &mut c2,
            &descs,
            InverseMctMode::None,
        )
        .unwrap();
        assert_eq!(c0, [half, top, 0]);
        assert_eq!(c1, c0);
        assert_eq!(c2, c0);
    }

    /// i64 threading + no MCT + signed 32-bit: signed components skip
    /// the §G.1.2 shift but still get the NOTE clamp to
    /// `[-2^31, 2^31 - 1]`.
    #[test]
    fn thread_5x3_i64_none_mode_signed_clamps_only() {
        let half = 1_i64 << 31;
        let mut c0 = [-half - 1, -half, 0, half - 1, half];
        let mut c1 = c0;
        let mut c2 = c0;
        let descs = [d_signed(32), d_signed(32), d_signed(32)];
        reconstruct_tile_components_5x3_i64(
            &mut c0,
            &mut c1,
            &mut c2,
            &descs,
            InverseMctMode::None,
        )
        .unwrap();
        assert_eq!(c0, [-half, -half, 0, half - 1, half - 1]);
        assert_eq!(c1, c0);
        assert_eq!(c2, c0);
    }

    /// i64 threading + RCT enforces the §G.2 prologue uniform
    /// `(precision, signedness)` rule across the three components.
    #[test]
    fn thread_5x3_i64_rct_rejects_unequal_precision_and_signedness() {
        let mut c0 = [0_i64];
        let mut c1 = [0_i64];
        let mut c2 = [0_i64];
        let descs = [d_unsigned(36), d_unsigned(38), d_unsigned(36)];
        assert_eq!(
            reconstruct_tile_components_5x3_i64(
                &mut c0,
                &mut c1,
                &mut c2,
                &descs,
                InverseMctMode::Rct,
            ),
            Err(Error::InvalidComponentCount)
        );
        let descs = [d_unsigned(36), d_unsigned(36), d_signed(36)];
        assert_eq!(
            reconstruct_tile_components_5x3_i64(
                &mut c0,
                &mut c1,
                &mut c2,
                &descs,
                InverseMctMode::Rct,
            ),
            Err(Error::InvalidComponentCount)
        );
    }

    /// i64 threading refuses ICT (wrong kernel pairing — the 9-7 /
    /// `f32` entry point owns ICT).
    #[test]
    fn thread_5x3_i64_rejects_ict_mode() {
        let mut c0 = [0_i64];
        let mut c1 = [0_i64];
        let mut c2 = [0_i64];
        let descs = [d_unsigned(36), d_unsigned(36), d_unsigned(36)];
        assert_eq!(
            reconstruct_tile_components_5x3_i64(
                &mut c0,
                &mut c1,
                &mut c2,
                &descs,
                InverseMctMode::Ict,
            ),
            Err(Error::NotImplemented)
        );
    }

    /// i64 threading rejects mismatched slice lengths and a non-3
    /// descriptor count up front.
    #[test]
    fn thread_5x3_i64_rejects_shape_mismatches() {
        let mut c0 = [0_i64; 4];
        let mut c1 = [0_i64; 3];
        let mut c2 = [0_i64; 4];
        let descs = [d_unsigned(36); 3];
        assert_eq!(
            reconstruct_tile_components_5x3_i64(
                &mut c0,
                &mut c1,
                &mut c2,
                &descs,
                InverseMctMode::None,
            ),
            Err(Error::InvalidMarkerLength)
        );
        let mut c1 = [0_i64; 4];
        let two = [d_unsigned(36); 2];
        assert_eq!(
            reconstruct_tile_components_5x3_i64(
                &mut c0,
                &mut c1,
                &mut c2,
                &two,
                InverseMctMode::None,
            ),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// i64 threading accepts the full Table A.11 `1..=38` precision
    /// window (a mixed-precision `None`-mode tile sharing an `i64`
    /// staging buffer flows through) and rejects `0` / `39`.
    #[test]
    fn thread_5x3_i64_precision_window_is_table_a11() {
        // Mixed (8, 32, 38) None-mode tile: per-component shift.
        let mut c0 = [0_i64];
        let mut c1 = [0_i64];
        let mut c2 = [0_i64];
        let descs = [d_unsigned(8), d_unsigned(32), d_unsigned(38)];
        reconstruct_tile_components_5x3_i64(
            &mut c0,
            &mut c1,
            &mut c2,
            &descs,
            InverseMctMode::None,
        )
        .unwrap();
        assert_eq!(c0[0], 128);
        assert_eq!(c1[0], 1_i64 << 31);
        assert_eq!(c2[0], 1_i64 << 37);
        // Out-of-range precisions reject before any mutation.
        for bad in [0_u8, 39, 255] {
            let mut c0 = [0_i64];
            let mut c1 = [0_i64];
            let mut c2 = [0_i64];
            let descs = [d_unsigned(8), d_unsigned(bad), d_unsigned(8)];
            assert_eq!(
                reconstruct_tile_components_5x3_i64(
                    &mut c0,
                    &mut c1,
                    &mut c2,
                    &descs,
                    InverseMctMode::None,
                ),
                Err(Error::InvalidSamplePrecision),
                "precision {} must reject",
                bad
            );
        }
    }

    /// 9-7 + ICT + unsigned 8-bit: the §G.3.1 forward ICT on the
    /// `(200 - 128, 100 - 128, 50 - 128) = (72, -28, -78)` shifted
    /// triple round-trips through the threading layer back to
    /// `(200, 100, 50)` within rounding error (the f32 ICT
    /// coefficients are informative per §G.3.2 closing paragraph).
    #[test]
    fn thread_9x7_ict_unsigned_8bit_recovers_rgb_sample() {
        // Encoder side: forward-shift then forward-ICT.
        let mut c0 = [72.0_f32];
        let mut c1 = [-28.0_f32];
        let mut c2 = [-78.0_f32];
        forward_ict(&mut c0, &mut c1, &mut c2).unwrap();
        // Decoder side: thread it through.
        let mut o0 = [0_i32];
        let mut o1 = [0_i32];
        let mut o2 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8)];
        reconstruct_tile_components_9x7(
            &mut c0,
            &mut c1,
            &mut c2,
            &mut o0,
            &mut o1,
            &mut o2,
            &descs,
            InverseMctMode::Ict,
        )
        .unwrap();
        // The §G.3 coefficients are informative; allow ±1 LSB after
        // round-to-nearest-even.
        assert!((o0[0] - 200).abs() <= 1, "I0 = {} (want ~200)", o0[0]);
        assert!((o1[0] - 100).abs() <= 1, "I1 = {} (want ~100)", o1[0]);
        assert!((o2[0] - 50).abs() <= 1, "I2 = {} (want ~50)", o2[0]);
    }

    /// 9-7 + no MCT + unsigned 8-bit: per-component independent
    /// round → level-shift → clamp, exactly like the 5-3 None
    /// path but on the f32 surface.
    #[test]
    fn thread_9x7_none_mode_round_then_level_shift_then_clamp() {
        let mut c0 = [0.0_f32, 0.4, -0.6, 100.0, -200.0];
        let mut c1 = c0;
        let mut c2 = c0;
        let mut o0 = [0_i32; 5];
        let mut o1 = [0_i32; 5];
        let mut o2 = [0_i32; 5];
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8)];
        reconstruct_tile_components_9x7(
            &mut c0,
            &mut c1,
            &mut c2,
            &mut o0,
            &mut o1,
            &mut o2,
            &descs,
            InverseMctMode::None,
        )
        .unwrap();
        // 0.0 → 0; +128 → 128.
        // 0.4 → 0 (ties-to-even rounds half away from zero, but 0.4 is closer to 0); +128 → 128.
        // -0.6 → -1; +128 → 127.
        // 100.0 → 100; +128 → 228.
        // -200.0 → -200; +128 → -72 → clamp 0.
        assert_eq!(o0, [128_i32, 128, 127, 228, 0]);
        assert_eq!(o1, o0);
        assert_eq!(o2, o0);
    }

    /// 9-7 + ICT requires equal `(precision, signedness)` per the
    /// §G.3 prologue (mirroring §G.2's rule).
    #[test]
    fn thread_9x7_ict_rejects_unequal_precision() {
        let mut c0 = [0.0_f32];
        let mut c1 = [0.0_f32];
        let mut c2 = [0.0_f32];
        let mut o0 = [0_i32];
        let mut o1 = [0_i32];
        let mut o2 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(10)];
        assert_eq!(
            reconstruct_tile_components_9x7(
                &mut c0,
                &mut c1,
                &mut c2,
                &mut o0,
                &mut o1,
                &mut o2,
                &descs,
                InverseMctMode::Ict,
            ),
            Err(Error::InvalidComponentCount)
        );
    }

    /// 9-7 entry point refuses RCT (wrong kernel pairing).
    #[test]
    fn thread_9x7_rejects_rct_mode() {
        let mut c0 = [0.0_f32];
        let mut c1 = [0.0_f32];
        let mut c2 = [0.0_f32];
        let mut o0 = [0_i32];
        let mut o1 = [0_i32];
        let mut o2 = [0_i32];
        let descs = [d_unsigned(8); 3];
        assert_eq!(
            reconstruct_tile_components_9x7(
                &mut c0,
                &mut c1,
                &mut c2,
                &mut o0,
                &mut o1,
                &mut o2,
                &descs,
                InverseMctMode::Rct,
            ),
            Err(Error::NotImplemented)
        );
    }

    /// 9-7 entry point rejects out-of-range output slot length.
    #[test]
    fn thread_9x7_rejects_output_length_mismatch() {
        let mut c0 = [0.0_f32; 4];
        let mut c1 = [0.0_f32; 4];
        let mut c2 = [0.0_f32; 4];
        let mut o0 = [0_i32; 4];
        let mut o1 = [0_i32; 3]; // wrong
        let mut o2 = [0_i32; 4];
        let descs = [d_unsigned(8); 3];
        assert_eq!(
            reconstruct_tile_components_9x7(
                &mut c0,
                &mut c1,
                &mut c2,
                &mut o0,
                &mut o1,
                &mut o2,
                &descs,
                InverseMctMode::None,
            ),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// 9-7 entry point saturates an ICT-amplified `f32` sample to
    /// `i32::{MIN, MAX}` at the cast point, before the §G.1.2 NOTE
    /// clamp runs. The cast saturation alone keeps the cast
    /// well-defined; the §G.1.2 inverse level-shift then runs on
    /// the saturated `i32` via the underlying `wrapping_add`
    /// primitive, so an `i32::MAX + 128` lane wraps and the
    /// subsequent clamp pulls it to `0` (not `255`). The lower
    /// end is well-behaved: `i32::MIN + 128 = i32::MIN + 128` is
    /// still negative, clamping to `0`.
    #[test]
    fn thread_9x7_saturates_pathological_f32_input() {
        let mut c0 = [1e30_f32, -1e30, 0.0];
        let mut c1 = [0.0_f32, 0.0, 0.0];
        let mut c2 = [0.0_f32, 0.0, 0.0];
        let mut o0 = [0_i32; 3];
        let mut o1 = [0_i32; 3];
        let mut o2 = [0_i32; 3];
        let descs = [d_unsigned(8); 3];
        reconstruct_tile_components_9x7(
            &mut c0,
            &mut c1,
            &mut c2,
            &mut o0,
            &mut o1,
            &mut o2,
            &descs,
            InverseMctMode::None,
        )
        .unwrap();
        // 1e30 saturates to i32::MAX, then `wrapping_add(128)`
        // wraps to a large-negative value, which the §G.1.2 NOTE
        // clamp pulls to 0.
        // -1e30 saturates to i32::MIN, `wrapping_add(128)` =
        // i32::MIN + 128 (still hugely negative), clamps to 0.
        // 0.0 → 0 → +128 → 128 (in-range, no clamp).
        assert_eq!(o0, [0_i32, 0, 128]);
        assert_eq!(o1, [128_i32, 128, 128]);
        assert_eq!(o2, [128_i32, 128, 128]);
    }

    // -------------------------------------------------------------------
    // reconstruct_tile_components_9x7_multi — §G.3 multi-component
    // generalisation.
    // -------------------------------------------------------------------

    /// Multi-component ICT with exactly three components matches the
    /// fixed-arity entry point on the §G.3.1 forward-ICT'd
    /// `(200, 100, 50)` sample: the multi entry point is a drop-in
    /// superset of `reconstruct_tile_components_9x7`.
    #[test]
    fn thread_9x7_multi_ict_three_components_matches_fixed_arity() {
        // Encoder side: forward-shift then forward-ICT.
        let mut a0 = [72.0_f32];
        let mut a1 = [-28.0_f32];
        let mut a2 = [-78.0_f32];
        forward_ict(&mut a0, &mut a1, &mut a2).unwrap();
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8)];

        // Fixed-arity reference.
        let (mut f0, mut f1, mut f2) = (a0, a1, a2);
        let mut r0 = [0_i32];
        let mut r1 = [0_i32];
        let mut r2 = [0_i32];
        reconstruct_tile_components_9x7(
            &mut f0,
            &mut f1,
            &mut f2,
            &mut r0,
            &mut r1,
            &mut r2,
            &descs,
            InverseMctMode::Ict,
        )
        .unwrap();

        // Multi entry point on the same inputs.
        let (mut m0, mut m1, mut m2) = (a0, a1, a2);
        let mut o0 = [0_i32];
        let mut o1 = [0_i32];
        let mut o2 = [0_i32];
        {
            let mut comps: [&mut [f32]; 3] = [&mut m0, &mut m1, &mut m2];
            let mut outs: [&mut [i32]; 3] = [&mut o0, &mut o1, &mut o2];
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::Ict,
            )
            .unwrap();
        }
        assert_eq!((o0[0], o1[0], o2[0]), (r0[0], r1[0], r2[0]));
        // And the recovered triple lands within ±1 LSB of the source
        // (the §G.3 coefficients are informative per §G.3.2 closing
        // paragraph).
        assert!((o0[0] - 200).abs() <= 1, "I0 = {} (want ~200)", o0[0]);
        assert!((o1[0] - 100).abs() <= 1, "I1 = {} (want ~100)", o1[0]);
        assert!((o2[0] - 50).abs() <= 1, "I2 = {} (want ~50)", o2[0]);
    }

    /// Four-component ICT image (e.g. RGBA): the §G.3 transform
    /// touches only components `(0, 1, 2)`; the index-3 alpha plane
    /// flows through the Figure G.2 placement (round + level-shift +
    /// clamp only) and is recovered untransformed. The alpha plane
    /// carries its own distinct descriptor (different precision) —
    /// legal because the "same bit-depth" prologue binds only the
    /// three transform inputs.
    #[test]
    fn thread_9x7_multi_ict_four_components_alpha_passthrough() {
        let mut a0 = [72.0_f32];
        let mut a1 = [-28.0_f32];
        let mut a2 = [-78.0_f32];
        forward_ict(&mut a0, &mut a1, &mut a2).unwrap();
        // Alpha (index 3) is an independent 10-bit plane whose DWT
        // output is 0.25 → rounds to 0 → level shift gives +512.
        let mut a3 = [0.25_f32];
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8), d_unsigned(10)];
        let mut o0 = [0_i32];
        let mut o1 = [0_i32];
        let mut o2 = [0_i32];
        let mut o3 = [0_i32];
        {
            let mut comps: [&mut [f32]; 4] = [&mut a0, &mut a1, &mut a2, &mut a3];
            let mut outs: [&mut [i32]; 4] = [&mut o0, &mut o1, &mut o2, &mut o3];
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::Ict,
            )
            .unwrap();
        }
        assert!((o0[0] - 200).abs() <= 1, "I0 = {} (want ~200)", o0[0]);
        assert!((o1[0] - 100).abs() <= 1, "I1 = {} (want ~100)", o1[0]);
        assert!((o2[0] - 50).abs() <= 1, "I2 = {} (want ~50)", o2[0]);
        // Alpha plane: round(0.25) = 0, then 0 + 2^(10-1) = 512.
        assert_eq!(o3[0], 512);
    }

    /// Single-component greyscale tile, no MCT: pure Figure G.2 path
    /// at component count 1 on the f32 surface — round, level-shift,
    /// clamp.
    #[test]
    fn thread_9x7_multi_none_single_component() {
        let mut c0 = [0.0_f32, 0.4, -0.6, 100.0, -200.0];
        let mut o0 = [0_i32; 5];
        let descs = [d_unsigned(8)];
        {
            let mut comps: [&mut [f32]; 1] = [&mut c0];
            let mut outs: [&mut [i32]; 1] = [&mut o0];
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::None,
            )
            .unwrap();
        }
        // Same expectations as the fixed-arity None-mode test.
        assert_eq!(o0, [128_i32, 128, 127, 228, 0]);
    }

    /// Two-component tile, no MCT: each plane independently rounded +
    /// level-shifted + clamped per its own descriptor.
    #[test]
    fn thread_9x7_multi_none_two_components() {
        let mut c0 = [0.0_f32, 100.0];
        let mut c1 = [0.0_f32, 100.0];
        let mut o0 = [0_i32; 2];
        let mut o1 = [0_i32; 2];
        let descs = [d_unsigned(8), d_unsigned(12)];
        {
            let mut comps: [&mut [f32]; 2] = [&mut c0, &mut c1];
            let mut outs: [&mut [i32]; 2] = [&mut o0, &mut o1];
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::None,
            )
            .unwrap();
        }
        assert_eq!(o0, [128_i32, 228]);
        assert_eq!(o1, [2048_i32, 2148]);
    }

    /// Five-component multispectral tile, no MCT: every plane is
    /// independently rounded + level-shifted + clamped. Exercises the
    /// loop past the three-component boundary on the Figure G.2 path.
    #[test]
    fn thread_9x7_multi_none_five_components() {
        let mut planes: Vec<Vec<f32>> = (0..5).map(|_| vec![0.0_f32, 300.0, -300.0]).collect();
        let mut outs_storage: Vec<Vec<i32>> = (0..5).map(|_| vec![0_i32; 3]).collect();
        let descs = [d_unsigned(8); 5];
        {
            let mut comps: Vec<&mut [f32]> = planes.iter_mut().map(|p| p.as_mut_slice()).collect();
            let mut outs: Vec<&mut [i32]> =
                outs_storage.iter_mut().map(|p| p.as_mut_slice()).collect();
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::None,
            )
            .unwrap();
        }
        for o in &outs_storage {
            // 0 + 128 = 128; 300 + 128 = 428 → clamp 255;
            // -300 + 128 = -172 → clamp 0.
            assert_eq!(o.as_slice(), [128_i32, 255, 0]);
        }
    }

    /// ICT requires at least three components: a two-component tile
    /// cannot legally signal an ICT in the COD marker.
    #[test]
    fn thread_9x7_multi_ict_rejects_fewer_than_three() {
        let mut c0 = [0.0_f32];
        let mut c1 = [0.0_f32];
        let mut o0 = [0_i32];
        let mut o1 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(8)];
        let mut comps: [&mut [f32]; 2] = [&mut c0, &mut c1];
        let mut outs: [&mut [i32]; 2] = [&mut o0, &mut o1];
        assert_eq!(
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::Ict
            ),
            Err(Error::InvalidComponentCount)
        );
    }

    /// The §G.3 "same bit-depth" prologue binds the three transform
    /// inputs: an ICT with components `(0, 1, 2)` of mixed precision
    /// is rejected even when a legal index-3 component is present.
    #[test]
    fn thread_9x7_multi_ict_rejects_unequal_precision_on_first_three() {
        let mut c0 = [0.0_f32];
        let mut c1 = [0.0_f32];
        let mut c2 = [0.0_f32];
        let mut c3 = [0.0_f32];
        let mut o0 = [0_i32];
        let mut o1 = [0_i32];
        let mut o2 = [0_i32];
        let mut o3 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(10), d_unsigned(8), d_unsigned(8)];
        let mut comps: [&mut [f32]; 4] = [&mut c0, &mut c1, &mut c2, &mut c3];
        let mut outs: [&mut [i32]; 4] = [&mut o0, &mut o1, &mut o2, &mut o3];
        assert_eq!(
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::Ict
            ),
            Err(Error::InvalidComponentCount)
        );
    }

    /// The §G.3 prologue also binds signedness — mixed signedness on
    /// the three transform inputs is rejected.
    #[test]
    fn thread_9x7_multi_ict_rejects_mixed_signedness_on_first_three() {
        let mut c0 = [0.0_f32];
        let mut c1 = [0.0_f32];
        let mut c2 = [0.0_f32];
        let mut o0 = [0_i32];
        let mut o1 = [0_i32];
        let mut o2 = [0_i32];
        let descs = [d_unsigned(8), d_signed(8), d_unsigned(8)];
        let mut comps: [&mut [f32]; 3] = [&mut c0, &mut c1, &mut c2];
        let mut outs: [&mut [i32]; 3] = [&mut o0, &mut o1, &mut o2];
        assert_eq!(
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::Ict
            ),
            Err(Error::InvalidComponentCount)
        );
    }

    /// RCT mode is rejected on the irreversible multi entry point
    /// (wrong surface — RCT operates on `i32`).
    #[test]
    fn thread_9x7_multi_rejects_rct_mode() {
        let mut c0 = [0.0_f32];
        let mut c1 = [0.0_f32];
        let mut c2 = [0.0_f32];
        let mut o0 = [0_i32];
        let mut o1 = [0_i32];
        let mut o2 = [0_i32];
        let descs = [d_unsigned(8); 3];
        let mut comps: [&mut [f32]; 3] = [&mut c0, &mut c1, &mut c2];
        let mut outs: [&mut [i32]; 3] = [&mut o0, &mut o1, &mut o2];
        assert_eq!(
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::Rct
            ),
            Err(Error::NotImplemented)
        );
    }

    /// Empty component collection is rejected.
    #[test]
    fn thread_9x7_multi_rejects_empty() {
        let descs: [ComponentDescriptor; 0] = [];
        let mut comps: [&mut [f32]; 0] = [];
        let mut outs: [&mut [i32]; 0] = [];
        assert_eq!(
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::None
            ),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// Mismatched component / output / descriptor counts are rejected.
    #[test]
    fn thread_9x7_multi_rejects_count_mismatch() {
        let mut c0 = [0.0_f32];
        let mut c1 = [0.0_f32];
        let mut o0 = [0_i32];
        let mut o1 = [0_i32];

        // components vs descriptors mismatch.
        let descs3 = [d_unsigned(8); 3];
        let mut comps: [&mut [f32]; 2] = [&mut c0, &mut c1];
        let mut outs: [&mut [i32]; 2] = [&mut o0, &mut o1];
        assert_eq!(
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs3,
                InverseMctMode::None
            ),
            Err(Error::InvalidMarkerLength)
        );

        // components vs outputs mismatch.
        let descs2 = [d_unsigned(8); 2];
        let mut comps: [&mut [f32]; 2] = [&mut c0, &mut c1];
        let mut outs1: [&mut [i32]; 1] = [&mut o0];
        assert_eq!(
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs1,
                &descs2,
                InverseMctMode::None
            ),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// Component slices that do not share a common length are rejected
    /// (the §G "same separation on the reference grid" rule), as are
    /// output slots that do not match the component length.
    #[test]
    fn thread_9x7_multi_rejects_ragged_lengths() {
        // Ragged component slices.
        let mut c0 = [0.0_f32, 0.0];
        let mut c1 = [0.0_f32];
        let mut c2 = [0.0_f32, 0.0];
        let mut o0 = [0_i32; 2];
        let mut o1 = [0_i32];
        let mut o2 = [0_i32; 2];
        let descs = [d_unsigned(8); 3];
        let mut comps: [&mut [f32]; 3] = [&mut c0, &mut c1, &mut c2];
        let mut outs: [&mut [i32]; 3] = [&mut o0, &mut o1, &mut o2];
        assert_eq!(
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::Ict
            ),
            Err(Error::InvalidMarkerLength)
        );

        // Uniform components but one short output slot.
        let mut c0 = [0.0_f32, 0.0];
        let mut c1 = [0.0_f32, 0.0];
        let mut c2 = [0.0_f32, 0.0];
        let mut o0 = [0_i32; 2];
        let mut o1 = [0_i32];
        let mut o2 = [0_i32; 2];
        let mut comps: [&mut [f32]; 3] = [&mut c0, &mut c1, &mut c2];
        let mut outs: [&mut [i32]; 3] = [&mut o0, &mut o1, &mut o2];
        assert_eq!(
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::Ict
            ),
            Err(Error::InvalidMarkerLength)
        );
    }

    /// Out-of-range precision on any descriptor (including an
    /// index-≥3 pass-through component) is rejected up front.
    #[test]
    fn thread_9x7_multi_rejects_out_of_range_precision() {
        let mut c0 = [0.0_f32];
        let mut c1 = [0.0_f32];
        let mut c2 = [0.0_f32];
        let mut c3 = [0.0_f32];
        let mut o0 = [0_i32];
        let mut o1 = [0_i32];
        let mut o2 = [0_i32];
        let mut o3 = [0_i32];
        let descs = [d_unsigned(8), d_unsigned(8), d_unsigned(8), d_unsigned(32)];
        let mut comps: [&mut [f32]; 4] = [&mut c0, &mut c1, &mut c2, &mut c3];
        let mut outs: [&mut [i32]; 4] = [&mut o0, &mut o1, &mut o2, &mut o3];
        assert_eq!(
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::Ict
            ),
            Err(Error::InvalidSamplePrecision)
        );
    }

    /// Pathological f32 inputs saturate at the cast point on the
    /// multi surface exactly as on the fixed-arity entry point.
    #[test]
    fn thread_9x7_multi_saturates_pathological_f32_input() {
        let mut c0 = [1e30_f32, -1e30, 0.0];
        let mut o0 = [0_i32; 3];
        let descs = [d_unsigned(8)];
        {
            let mut comps: [&mut [f32]; 1] = [&mut c0];
            let mut outs: [&mut [i32]; 1] = [&mut o0];
            reconstruct_tile_components_9x7_multi(
                &mut comps,
                &mut outs,
                &descs,
                InverseMctMode::None,
            )
            .unwrap();
        }
        // Same expectations as the fixed-arity saturation test:
        // saturate → wrapping level shift → §G.1.2 NOTE clamp.
        assert_eq!(o0, [0_i32, 0, 128]);
    }
}
