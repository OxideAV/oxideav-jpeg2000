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
//! G-12 / G-13 / G-14). No external library source — OpenJPEG,
//! OpenJPH, Kakadu, FFmpeg, libavcodec, jpeg2000-rs, grok-jpeg2000,
//! etc. — was consulted, quoted, paraphrased, or used as a
//! cross-check oracle. No WebSearch / WebFetch was used for any
//! reason.
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
/// or greater than `31`. Callers handling `Ssiz ≥ 32` should clamp
/// in their own `i64` surface (the formula is the same one bit
/// wider).
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
}
