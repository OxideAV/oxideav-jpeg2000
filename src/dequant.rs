//! Tier-2 inverse quantisation — T.800 Annex E.
//!
//! This module implements the per-sub-band **dequantisation** step that
//! turns the tier-1 magnitude / sign of one wavelet coefficient back into
//! a reconstructed transform coefficient. It does **not** perform any
//! tier-1 decoding (that is [`crate::t1`]) and it does **not** apply the
//! inverse wavelet transform (Annex F, a later round). It is the bridge
//! between those two steps.
//!
//! ## What this module covers
//!
//! * **§E.1 — Inverse quantization procedure** as a whole — the
//!   `qb(u, v)` integer recovery of Equation E-1 from a tier-1
//!   [`crate::t1::Coefficient`] (its already-positionally-weighted
//!   `magnitude` plus its `sign`).
//! * **§E.1.1 — Irreversible transformation:**
//!   * §E.1.1.1 + Equation E-3 — quantisation step size `Δb` from the
//!     (`Rb`, `εb`, `µb`) triple.
//!   * §E.1.1.1 + Equation E-4 — nominal dynamic range `Rb = RI +
//!     log₂(gainb)` with the Table E.1 sub-band-gain exponents.
//!   * §E.1.1.1 + Equation E-5 — the `(εb, µb) = (ε₀ - NL + nb, µ₀)`
//!     derivation that expands a single NLLL-sub-band `(ε₀, µ₀)`
//!     (`ScalarDerived`) to every sub-band.
//!   * §E.1.1.2 + Equation E-6 — reconstruction `Rqb(u, v)` with the
//!     `r` reconstruction parameter (default `r = ½`, conventional
//!     midpoint placement).
//! * **§E.1.2 — Reversible transformation:**
//!   * §E.1.2.1 — `Δb = 1`.
//!   * §E.1.2.2 + Equations E-7 / E-8 — reconstruction either as
//!     `qb(u, v)` directly (when `Nb = Mb`) or with a `Δb = 1`
//!     reconstruction offset (when `Nb < Mb`).
//! * **§A.6.4 / Tables A.28 / A.29 / A.30** — parsing of the `SPqcd`
//!   payload bytes into typed `(εb, µb)` pairs for the three
//!   [`crate::QuantizationStyle`] variants the QCD / QCC parser
//!   already returns. Reversible / `None`-style entries are 8 bits
//!   wide with `εb` in the high 5 bits and the low 3 bits reserved.
//!   Irreversible 16-bit entries are big-endian with `εb` in the high
//!   5 bits and `µb` in the low 11 bits.
//! * **§E.2 — Encoder-side quantisation (informative).** Equation E-9
//!   (`qb = sign(ab) · ⌊|ab| / Δb⌋`) so the round-trip
//!   `encode → dequantise` lines up under midpoint reconstruction.
//!   The decoder never invokes this; it lives here so the test suite
//!   can validate the irreversible-path round-trip without any
//!   external reference.
//!
//! ## What this module does NOT cover
//!
//! * The encoder for reversible transformation (`b = RI + log₂(gainb)`,
//!   no division). That falls out of the §E.1.2 reversible-step-size
//!   `Δb = 1`; the encoder simply emits the tier-1 magnitudes of the
//!   integer wavelet coefficients verbatim.
//! * MCT bit growth (`c` in Equation E-10). The MCT (Annex G) is a
//!   separate later round; until it lands, every reversible step here
//!   uses `c = 0`.
//! * Annex E's Equation E-1 says `MSBi(b, u, v)` is the **decoded**
//!   bit, but it does **not** say how the partially-decoded magnitude
//!   is placed in storage. We follow the t1-module convention: each
//!   coding pass OR-accumulates `1 << bitplane` into `Coefficient::
//!   magnitude` at the bit-plane weight, so the stored value is
//!   `sum_{i=l}^{Nb} MSBi · 2^(Mb - i)` directly. This is the same
//!   value Equation E-1 produces before the `(1 - 2*sb)` sign
//!   multiplication; the helpers below treat it that way.
//!
//! ## Clean-room provenance
//!
//! Implemented solely from
//! `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf` Annex E (§E.1
//! prologue + Equations E-1 / E-2; §E.1.1.1 + Equations E-3 / E-4 /
//! E-5 + Table E.1; §E.1.1.2 + Equation E-6; §E.1.2.1; §E.1.2.2 +
//! Equations E-7 / E-8; §E.2 + Equation E-9) and §A.6.4 + Tables A.28
//! / A.29 / A.30 (the SPqcd byte / 16-bit-word layouts the parser at
//! `crate::lib.rs` already consumes raw). No external library source
//! — OpenJPEG, OpenJPH, Kakadu, FFmpeg, libavcodec, jpeg2000-rs, etc.
//! — was consulted, quoted, paraphrased, or used as a cross-check
//! oracle. No WebSearch / WebFetch was used for any reason.

use crate::geometry::SubBandOrientation;
use crate::t1::Coefficient;
use crate::Error;

/// One parsed `(εb, µb)` pair from a single `SPqcd` / `SPqcc` entry.
///
/// `epsilon` is the **exponent** (Table A.29 reversible 5-bit field;
/// Table A.30 irreversible 5-bit field — same range, `0..=31`).
/// `mantissa` is the **mantissa** (Table A.30 irreversible 11-bit
/// field, `0..=2047`). Reversible / `None`-style entries carry only
/// an exponent and report `mantissa = 0` — `mantissa` is ignored by
/// Equation E-3 in those cases because Δb is held at 1 per §E.1.2.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepSize {
    /// `εb`, the quantisation step-size exponent. Range `0..=31`.
    pub epsilon: u8,
    /// `µb`, the quantisation step-size mantissa. Range `0..=2047`
    /// for irreversible; held at `0` for reversible.
    pub mantissa: u16,
}

impl StepSize {
    /// Decode one reversible / `None`-style 8-bit `SPqcd` byte per
    /// Table A.29: `εb` is the high 5 bits, low 3 bits reserved.
    /// The reserved bits are accepted (the spec says "all other
    /// values reserved" of the **whole** byte range, but the
    /// `0000 0xxx` to `1111 1xxx` pattern is exhaustive on `εb`
    /// already; the low 3 bits of any one row are simply ignored).
    pub fn from_reversible_byte(byte: u8) -> Self {
        Self {
            epsilon: byte >> 3,
            mantissa: 0,
        }
    }

    /// Decode one irreversible 16-bit `SPqcd` word per Table A.30:
    /// `εb` is the high 5 bits of the big-endian word, `µb` is the
    /// low 11 bits.
    pub fn from_irreversible_word(word: u16) -> Self {
        Self {
            epsilon: ((word >> 11) & 0x1F) as u8,
            mantissa: word & 0x07FF,
        }
    }

    /// Decode one irreversible 16-bit `SPqcd` entry from a 2-byte
    /// big-endian slice. Returns [`Error::InvalidMarkerLength`] if
    /// the slice is shorter than 2 bytes.
    pub fn from_irreversible_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < 2 {
            return Err(Error::InvalidMarkerLength);
        }
        Ok(Self::from_irreversible_word(u16::from_be_bytes([
            bytes[0], bytes[1],
        ])))
    }

    /// Parse the full `SPqcd` payload from `style = None` or
    /// `style = Reversible` (Table A.29): one byte per sub-band, each
    /// holding only `εb`.
    pub fn parse_reversible_payload(payload: &[u8]) -> Vec<Self> {
        payload
            .iter()
            .copied()
            .map(Self::from_reversible_byte)
            .collect()
    }

    /// Parse the full `SPqcd` payload from `style = ScalarExpounded`
    /// (Table A.30): one 16-bit big-endian word per sub-band, each
    /// holding `(εb, µb)`.
    pub fn parse_irreversible_payload(payload: &[u8]) -> Result<Vec<Self>, Error> {
        if payload.len() % 2 != 0 {
            return Err(Error::InvalidMarkerLength);
        }
        let mut out = Vec::with_capacity(payload.len() / 2);
        for chunk in payload.chunks_exact(2) {
            out.push(Self::from_irreversible_word(u16::from_be_bytes([
                chunk[0], chunk[1],
            ])));
        }
        Ok(out)
    }

    /// Parse the single `(ε₀, µ₀)` NLLL entry from `style =
    /// ScalarDerived` (Table A.30 again, but only one entry). Use
    /// [`derive_from_nlll`] to expand to every sub-band.
    pub fn parse_derived_payload(payload: &[u8]) -> Result<Self, Error> {
        Self::from_irreversible_bytes(payload)
    }
}

// ---------------------------------------------------------------------------
// Table E.1 — sub-band gains.
// ---------------------------------------------------------------------------

/// Base-2 logarithm of the sub-band gain `gainb` per T.800 Table E.1.
///
/// `LL → 0`, `HL → 1`, `LH → 1`, `HH → 2`. These are the contribution
/// of each sub-band's high-pass filters to the nominal dynamic range
/// per Equation E-4.
pub fn subband_gain_log2(orientation: SubBandOrientation) -> u32 {
    match orientation {
        SubBandOrientation::LL => 0,
        SubBandOrientation::HL => 1,
        SubBandOrientation::LH => 1,
        SubBandOrientation::HH => 2,
    }
}

/// Nominal dynamic range `Rb = RI + log₂(gainb)` per T.800 Equation
/// E-4. `precision` is the per-component sample precision `RI` from
/// the SIZ marker (Table A.11, `Ssiz + 1`, in `1..=38`).
pub fn nominal_dynamic_range(precision: u32, orientation: SubBandOrientation) -> u32 {
    precision + subband_gain_log2(orientation)
}

// ---------------------------------------------------------------------------
// Equation E-5 — derived `(εb, µb)` expansion from the NLLL entry.
// ---------------------------------------------------------------------------

/// Expand the single NLLL `(ε₀, µ₀)` pair to a per-sub-band `(εb,
/// µb)` pair under `ScalarDerived` quantisation, per T.800 Equation
/// E-5: `(εb, µb) = (ε₀ - NL + nb, µ₀)`.
///
/// `nl` is the number of decomposition levels (the `COD` / `COC`
/// `SPcod`-side `NL`, `0..=32`). `nb` is the **decomposition level**
/// of the sub-band, **not** the resolution-level index `r`:
/// `nb = NL - r + 1` at `r ≥ 1`, `nb = NL` at `r = 0` (the NLLL band).
/// This matches the convention used by [`crate::geometry::SubBand`].
///
/// Returns [`Error::InvalidDecompositionLevels`] when `nb > nl` (out
/// of `0..=NL` range) and [`Error::InvalidMarkerLength`] when the
/// derivation would underflow `ε₀ - NL + nb` to a negative value
/// (a corrupt codestream).
pub fn derive_from_nlll(nlll: StepSize, nl: u8, nb: u8) -> Result<StepSize, Error> {
    if nb > nl {
        return Err(Error::InvalidDecompositionLevels);
    }
    let signed = i32::from(nlll.epsilon) - i32::from(nl) + i32::from(nb);
    if signed < 0 {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(StepSize {
        epsilon: signed as u8,
        mantissa: nlll.mantissa,
    })
}

// ---------------------------------------------------------------------------
// Equation E-2 — `Mb`.
// ---------------------------------------------------------------------------

/// Compute `Mb = G + εb - 1` per T.800 Equation E-2.
///
/// `G` is the guard-bit count from the high 3 bits of `Sqcd` (`0..=7`,
/// Table A.28). The result is the bit count of the unsigned magnitude
/// `qb(u, v)` integer representation of Equation E-1.
///
/// Returns [`Error::InvalidMarkerLength`] when `εb == 0 && G == 0`
/// (the spec's prose around §E.1 implicitly requires `Mb ≥ 1` —
/// `Nb ≤ Mb` and `Nb` is the bit-plane index from `B.10.5`).
pub fn mb(guard_bits: u8, epsilon: u8) -> Result<u32, Error> {
    let sum = u32::from(guard_bits) + u32::from(epsilon);
    if sum < 1 {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(sum - 1)
}

// ---------------------------------------------------------------------------
// Equation E-3 — irreversible quantisation step size `Δb`.
// ---------------------------------------------------------------------------

/// Compute `Δb = 2^(Rb - εb) · (1 + µb / 2^11)` per T.800 Equation
/// E-3.
///
/// The denominator `2^11` is the 11 bits allocated to `µb` in the
/// codestream (Table A.30). Returned as `f64` to retain the
/// intermediate sub-bit precision; callers in the integer arithmetic
/// path should convert just before applying Equation E-6.
///
/// Note that `Rb - εb` may be negative; we cast to a signed integer
/// before exponentiating so `2^(-3)` evaluates as `1/8`.
pub fn irreversible_step_size(rb: u32, step: StepSize) -> f64 {
    let exponent = (rb as i32) - i32::from(step.epsilon);
    let two_pow = (exponent as f64).exp2(); // 2^exponent, no rounding
    let mantissa_factor = 1.0 + (f64::from(step.mantissa) / 2048.0);
    two_pow * mantissa_factor
}

// ---------------------------------------------------------------------------
// Equations E-1 / E-6 / E-7 / E-8 — reconstructed transform coefficient.
// ---------------------------------------------------------------------------

/// Recover the signed `qb(u, v)` integer of Equation E-1 from the
/// magnitude / sign carried in a tier-1 [`Coefficient`].
///
/// The tier-1 coding passes OR-accumulate `1 << bitplane` into
/// `Coefficient::magnitude` at the bit-plane weight, so the stored
/// value is the magnitude term `sum_{i=l}^{Nb} MSBi · 2^(Mb - i)`
/// directly. We multiply by `(1 - 2·sb)` to get the signed `qb`.
/// The result is `i64` to leave room for `Mb = 38 + 7 = 45` bits of
/// magnitude on a maximum-precision irreversible code-stream.
pub fn qb_signed(coeff: Coefficient) -> i64 {
    let magnitude = i64::from(coeff.magnitude);
    if coeff.sign {
        -magnitude
    } else {
        magnitude
    }
}

/// Reconstruct the transform coefficient under **irreversible**
/// quantisation per T.800 Equation E-6:
///
/// ```text
///                 (qb + r · 2^(Mb - Nb)) · Δb   for qb > 0
/// Rqb(u, v)  =    (qb - r · 2^(Mb - Nb)) · Δb   for qb < 0
///                  0                            for qb == 0
/// ```
///
/// `nb` is `Nb(u, v)` — the number of decoded magnitude bits of this
/// coefficient (the bit-plane index counter from §B.10.5). `mb` is
/// the per-sub-band integer-representation bit count from
/// [`mb`]. `step_size` is `Δb` from [`irreversible_step_size`]. `r`
/// is the §E.1.1.2 reconstruction parameter (the spec's "may be
/// chosen for example to produce the best visual or objective
/// quality", typically `0.5`).
///
/// When `nb == mb` the coefficient is fully decoded; the offset term
/// `2^(mb - nb) = 1`. When `nb < mb` (a truncated bit-plane), the
/// offset shifts the reconstruction toward the midpoint of the
/// truncation bin per the §E.1.1.2 NOTE that "values for `r` fall in
/// the range of `0 ≤ r < 1`, and a common value is `r = 1/2`".
pub fn reconstruct_irreversible(qb: i64, mb_bits: u32, nb: u32, step_size: f64, r: f64) -> f64 {
    if qb == 0 {
        return 0.0;
    }
    // mb >= nb in any conforming code-stream; we surface a saturating
    // value if not so an upstream bug shows up as a wildly large
    // reconstruction rather than a panic.
    let shift = mb_bits.saturating_sub(nb);
    let offset = r * (shift as f64).exp2();
    let signed_offset = if qb > 0 { offset } else { -offset };
    (qb as f64 + signed_offset) * step_size
}

/// Reconstruct the transform coefficient under **reversible**
/// quantisation per T.800 Equation E-7 (`Nb = Mb`) or Equation E-8
/// (`Nb < Mb`).
///
/// `Δb = 1` per §E.1.2.1, so the equations reduce to:
///
/// ```text
/// if Nb == Mb:                                    Rqb = qb
/// if Nb <  Mb and qb > 0:    Rqb = (qb + r · 2^(Mb - Nb)) · 1
/// if Nb <  Mb and qb < 0:    Rqb = (qb - r · 2^(Mb - Nb)) · 1
/// if Nb <  Mb and qb == 0:   Rqb = 0
/// ```
///
/// The `Nb = Mb` path is exact integer recovery — no `r` involved —
/// because every magnitude bit was decoded and there is nothing to
/// reconstruct beyond the integer. The `Nb < Mb` path uses the same
/// `r`-weighted midpoint as the irreversible reconstruction in
/// [`reconstruct_irreversible`] but with `Δb = 1`.
///
/// Returned as `i64` when `nb == mb_bits` (exact integer recovery)
/// and as `f64` otherwise. To keep a single signature for callers
/// that aggregate both paths into the same wavelet array, this
/// function always returns `f64`; integer recovery is exact in `f64`
/// for `Mb ≤ 53`, well within the spec's `Mb ≤ 38 + 7 = 45`.
pub fn reconstruct_reversible(qb: i64, mb_bits: u32, nb: u32, r: f64) -> f64 {
    if nb >= mb_bits {
        return qb as f64;
    }
    if qb == 0 {
        return 0.0;
    }
    let shift = mb_bits - nb;
    let offset = r * (shift as f64).exp2();
    let signed_offset = if qb > 0 { offset } else { -offset };
    qb as f64 + signed_offset
}

// ---------------------------------------------------------------------------
// Equation E-9 — encoder-side quantisation (informative).
// ---------------------------------------------------------------------------

/// Encoder-side scalar quantisation per T.800 Equation E-9 (§E.2,
/// informative): `qb = sign(ab) · ⌊|ab| / Δb⌋`.
///
/// Returned as `i64`; the caller decides how many bit-planes of the
/// magnitude to actually carry in the codestream (`Nb` is a per-
/// coefficient decision distinct from `Mb`).
///
/// Used by the test suite to validate the round-trip
/// `encode → decode → reconstruct` under midpoint reconstruction
/// without an external reference. The decoder never calls this.
pub fn quantise_irreversible(ab: f64, step_size: f64) -> i64 {
    if step_size == 0.0 {
        return 0;
    }
    let sign = if ab < 0.0 { -1i64 } else { 1i64 };
    let abs_q = (ab.abs() / step_size).floor() as i64;
    sign * abs_q
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::SubBandOrientation;

    // -----------------------------------------------------------------
    // SPqcd byte / word decoding (Table A.29 / A.30).
    // -----------------------------------------------------------------

    #[test]
    fn reversible_byte_extracts_epsilon_from_high_5_bits() {
        // 0b10101_000 → epsilon = 0b10101 = 21
        let step = StepSize::from_reversible_byte(0b1010_1000);
        assert_eq!(step.epsilon, 21);
        assert_eq!(step.mantissa, 0);
    }

    #[test]
    fn reversible_byte_ignores_low_3_bits() {
        // The low 3 bits are reserved; we accept any value.
        let a = StepSize::from_reversible_byte(0b0001_0000);
        let b = StepSize::from_reversible_byte(0b0001_0111);
        assert_eq!(a.epsilon, b.epsilon);
        assert_eq!(a.epsilon, 2);
    }

    #[test]
    fn reversible_byte_boundary_zero_and_max() {
        assert_eq!(StepSize::from_reversible_byte(0x00).epsilon, 0);
        assert_eq!(StepSize::from_reversible_byte(0xFF).epsilon, 31);
    }

    #[test]
    fn irreversible_word_splits_5_and_11_bits() {
        // bits 15..11 = epsilon, bits 10..0 = mantissa
        // 0b11011_111_1111_1111 → epsilon = 27, mantissa = 2047
        let step = StepSize::from_irreversible_word(0xDFFF);
        assert_eq!(step.epsilon, 27);
        assert_eq!(step.mantissa, 2047);
    }

    #[test]
    fn irreversible_word_zero() {
        let step = StepSize::from_irreversible_word(0);
        assert_eq!(step.epsilon, 0);
        assert_eq!(step.mantissa, 0);
    }

    #[test]
    fn irreversible_word_pure_exponent_no_mantissa() {
        // epsilon = 5, mantissa = 0 → 0b00101_000_0000_0000 = 0x2800
        let step = StepSize::from_irreversible_word(0x2800);
        assert_eq!(step.epsilon, 5);
        assert_eq!(step.mantissa, 0);
    }

    #[test]
    fn irreversible_word_pure_mantissa_no_exponent() {
        // epsilon = 0, mantissa = 1024 → 0b00000_100_0000_0000 = 0x0400
        let step = StepSize::from_irreversible_word(0x0400);
        assert_eq!(step.epsilon, 0);
        assert_eq!(step.mantissa, 1024);
    }

    #[test]
    fn irreversible_bytes_decodes_big_endian() {
        // 0x2A 0x55 → 0x2A55 → epsilon = 5, mantissa = 0b01010_0101_0101 = 0x255
        let step = StepSize::from_irreversible_bytes(&[0x2A, 0x55]).unwrap();
        assert_eq!(step.epsilon, 5);
        assert_eq!(step.mantissa, 0x255);
    }

    #[test]
    fn irreversible_bytes_too_short_errors() {
        assert_eq!(
            StepSize::from_irreversible_bytes(&[0x2A]),
            Err(Error::InvalidMarkerLength)
        );
        assert_eq!(
            StepSize::from_irreversible_bytes(&[]),
            Err(Error::InvalidMarkerLength)
        );
    }

    #[test]
    fn parse_reversible_payload_one_byte_per_subband() {
        // 3 sub-bands: epsilons 1, 2, 31
        let payload = [0b0000_1000, 0b0001_0000, 0b1111_1111];
        let steps = StepSize::parse_reversible_payload(&payload);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].epsilon, 1);
        assert_eq!(steps[1].epsilon, 2);
        assert_eq!(steps[2].epsilon, 31);
        for s in &steps {
            assert_eq!(s.mantissa, 0);
        }
    }

    #[test]
    fn parse_irreversible_payload_two_bytes_per_subband() {
        let payload = [0x28, 0x00, 0x2A, 0x55];
        let steps = StepSize::parse_irreversible_payload(&payload).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(
            steps[0],
            StepSize {
                epsilon: 5,
                mantissa: 0
            }
        );
        assert_eq!(
            steps[1],
            StepSize {
                epsilon: 5,
                mantissa: 0x255
            }
        );
    }

    #[test]
    fn parse_irreversible_payload_odd_length_errors() {
        let payload = [0x28, 0x00, 0x2A];
        assert_eq!(
            StepSize::parse_irreversible_payload(&payload),
            Err(Error::InvalidMarkerLength)
        );
    }

    #[test]
    fn parse_derived_payload_one_pair() {
        let payload = [0x28, 0x00];
        let step = StepSize::parse_derived_payload(&payload).unwrap();
        assert_eq!(
            step,
            StepSize {
                epsilon: 5,
                mantissa: 0
            }
        );
    }

    // -----------------------------------------------------------------
    // Table E.1 sub-band gains.
    // -----------------------------------------------------------------

    #[test]
    fn subband_gain_table_e1() {
        assert_eq!(subband_gain_log2(SubBandOrientation::LL), 0);
        assert_eq!(subband_gain_log2(SubBandOrientation::HL), 1);
        assert_eq!(subband_gain_log2(SubBandOrientation::LH), 1);
        assert_eq!(subband_gain_log2(SubBandOrientation::HH), 2);
    }

    // -----------------------------------------------------------------
    // Equation E-4.
    // -----------------------------------------------------------------

    #[test]
    fn nominal_dynamic_range_equation_e4() {
        // RI = 8 bits, LL gain 1 → Rb = 8 + 0 = 8.
        assert_eq!(nominal_dynamic_range(8, SubBandOrientation::LL), 8);
        // RI = 8 bits, HL gain 2 → Rb = 8 + 1 = 9.
        assert_eq!(nominal_dynamic_range(8, SubBandOrientation::HL), 9);
        // RI = 8 bits, HH gain 4 → Rb = 8 + 2 = 10.
        assert_eq!(nominal_dynamic_range(8, SubBandOrientation::HH), 10);
        // RI = 16 bits (deep frame), LH gain 2 → Rb = 17.
        assert_eq!(nominal_dynamic_range(16, SubBandOrientation::LH), 17);
    }

    // -----------------------------------------------------------------
    // Equation E-5.
    // -----------------------------------------------------------------

    #[test]
    fn derive_from_nlll_at_nb_eq_nl_is_identity() {
        // εb = ε₀ - NL + nb. At nb = NL: εb = ε₀.
        let nlll = StepSize {
            epsilon: 10,
            mantissa: 1234,
        };
        let derived = derive_from_nlll(nlll, 5, 5).unwrap();
        assert_eq!(derived, nlll);
    }

    #[test]
    fn derive_from_nlll_high_pass_decrements_epsilon() {
        // NL = 3, nb = 1 (resolution level r = 3): εb = ε₀ - 3 + 1 = ε₀ - 2.
        let nlll = StepSize {
            epsilon: 10,
            mantissa: 1234,
        };
        let derived = derive_from_nlll(nlll, 3, 1).unwrap();
        assert_eq!(derived.epsilon, 8);
        assert_eq!(derived.mantissa, 1234);
    }

    #[test]
    fn derive_from_nlll_mantissa_unchanged_per_e5() {
        // Equation E-5 mutates only εb; µb is held constant.
        let nlll = StepSize {
            epsilon: 7,
            mantissa: 999,
        };
        for nb in 0..=5u8 {
            let derived = derive_from_nlll(nlll, 5, nb).unwrap();
            assert_eq!(derived.mantissa, 999);
        }
    }

    #[test]
    fn derive_from_nlll_rejects_nb_above_nl() {
        let nlll = StepSize {
            epsilon: 5,
            mantissa: 0,
        };
        assert_eq!(
            derive_from_nlll(nlll, 3, 4),
            Err(Error::InvalidDecompositionLevels)
        );
    }

    #[test]
    fn derive_from_nlll_rejects_negative_underflow() {
        // ε₀ = 1, NL = 5, nb = 0 → εb = 1 - 5 + 0 = -4: invalid.
        let nlll = StepSize {
            epsilon: 1,
            mantissa: 0,
        };
        assert_eq!(
            derive_from_nlll(nlll, 5, 0),
            Err(Error::InvalidMarkerLength)
        );
    }

    // -----------------------------------------------------------------
    // Equation E-2.
    // -----------------------------------------------------------------

    #[test]
    fn mb_equation_e2() {
        // Mb = G + εb - 1
        assert_eq!(mb(1, 8).unwrap(), 8); // 1 + 8 - 1 = 8
        assert_eq!(mb(2, 10).unwrap(), 11);
        assert_eq!(mb(7, 31).unwrap(), 37); // worst case 7 + 31 - 1
        assert_eq!(mb(1, 0).unwrap(), 0); // 1 + 0 - 1 = 0 (just OK)
    }

    #[test]
    fn mb_rejects_zero_sum() {
        // G = 0, εb = 0: sum = 0, Mb would be -1.
        assert_eq!(mb(0, 0), Err(Error::InvalidMarkerLength));
    }

    // -----------------------------------------------------------------
    // Equation E-3.
    // -----------------------------------------------------------------

    #[test]
    fn irreversible_step_size_zero_mantissa_is_pure_power_of_two() {
        // Δb = 2^(Rb - εb) · 1 when µb = 0.
        // Rb = 8, εb = 4 → Δb = 16.0
        let step = StepSize {
            epsilon: 4,
            mantissa: 0,
        };
        assert!((irreversible_step_size(8, step) - 16.0).abs() < 1e-12);
        // Rb = 8, εb = 8 → Δb = 1.0
        let step = StepSize {
            epsilon: 8,
            mantissa: 0,
        };
        assert!((irreversible_step_size(8, step) - 1.0).abs() < 1e-12);
        // Rb = 8, εb = 10 → Δb = 0.25 (negative exponent)
        let step = StepSize {
            epsilon: 10,
            mantissa: 0,
        };
        assert!((irreversible_step_size(8, step) - 0.25).abs() < 1e-12);
    }

    #[test]
    fn irreversible_step_size_mantissa_factor_is_one_plus_m_over_2048() {
        // µb = 1024 → factor = 1 + 0.5 = 1.5. Rb = εb → 2^0 = 1.
        let step = StepSize {
            epsilon: 8,
            mantissa: 1024,
        };
        assert!((irreversible_step_size(8, step) - 1.5).abs() < 1e-12);
        // µb = 2047 → factor = 1 + 2047/2048
        let step = StepSize {
            epsilon: 8,
            mantissa: 2047,
        };
        let expected = 1.0 + 2047.0 / 2048.0;
        assert!((irreversible_step_size(8, step) - expected).abs() < 1e-12);
    }

    // -----------------------------------------------------------------
    // qb_signed — Equation E-1 sign multiplication.
    // -----------------------------------------------------------------

    #[test]
    fn qb_signed_positive_coeff() {
        let c = Coefficient {
            magnitude: 42,
            sigma: true,
            sign: false,
            already_refined: false,
        };
        assert_eq!(qb_signed(c), 42);
    }

    #[test]
    fn qb_signed_negative_coeff() {
        let c = Coefficient {
            magnitude: 42,
            sigma: true,
            sign: true,
            already_refined: false,
        };
        assert_eq!(qb_signed(c), -42);
    }

    #[test]
    fn qb_signed_zero_is_zero_regardless_of_sign_bit() {
        // sign is a don't-care when magnitude is zero (the coefficient is
        // insignificant); §E.1 / Equation E-1 makes qb = 0 fall through
        // to the qb = 0 branch of Equation E-6.
        let pos = Coefficient {
            magnitude: 0,
            sigma: false,
            sign: false,
            already_refined: false,
        };
        let neg = Coefficient {
            magnitude: 0,
            sigma: false,
            sign: true,
            already_refined: false,
        };
        assert_eq!(qb_signed(pos), 0);
        assert_eq!(qb_signed(neg), 0);
    }

    // -----------------------------------------------------------------
    // Equation E-6 — irreversible reconstruction.
    // -----------------------------------------------------------------

    #[test]
    fn reconstruct_irreversible_zero_qb_yields_zero() {
        assert_eq!(reconstruct_irreversible(0, 8, 8, 1.0, 0.5), 0.0);
    }

    #[test]
    fn reconstruct_irreversible_full_decode_no_midpoint_lift() {
        // nb = mb → shift = 0 → 2^0 = 1; with r = 0 the result is qb * Δb.
        let r = 0.0;
        let step = 0.25;
        let rqb = reconstruct_irreversible(8, 10, 10, step, r);
        assert!((rqb - 2.0).abs() < 1e-12);
    }

    #[test]
    fn reconstruct_irreversible_positive_qb_adds_midpoint() {
        // qb = +4, mb = 10, nb = 8 (truncated 2 bit-planes early),
        // Δb = 1, r = 0.5: Rqb = (4 + 0.5 · 2^2) · 1 = 4 + 2 = 6.
        let rqb = reconstruct_irreversible(4, 10, 8, 1.0, 0.5);
        assert!((rqb - 6.0).abs() < 1e-12);
    }

    #[test]
    fn reconstruct_irreversible_negative_qb_subtracts_midpoint() {
        // qb = -4, mb = 10, nb = 8, Δb = 1, r = 0.5: Rqb = (-4 - 0.5 · 4) · 1 = -6.
        let rqb = reconstruct_irreversible(-4, 10, 8, 1.0, 0.5);
        assert!((rqb + 6.0).abs() < 1e-12);
    }

    #[test]
    fn reconstruct_irreversible_applies_step_size() {
        // qb = +1, mb = 8, nb = 8, Δb = 0.5, r = 0.5: Rqb = (1 + 0.5 · 1) · 0.5 = 0.75.
        let rqb = reconstruct_irreversible(1, 8, 8, 0.5, 0.5);
        assert!((rqb - 0.75).abs() < 1e-12);
    }

    // -----------------------------------------------------------------
    // Equations E-7 / E-8 — reversible reconstruction.
    // -----------------------------------------------------------------

    #[test]
    fn reconstruct_reversible_full_decode_is_exact_integer() {
        // E-7: Nb = Mb → Rqb = qb. No r-weighted midpoint.
        for q in [-100, -1, 0, 1, 42, 12345] {
            let rqb = reconstruct_reversible(q, 8, 8, 0.5);
            assert!((rqb - q as f64).abs() < 1e-12, "q = {}", q);
        }
    }

    #[test]
    fn reconstruct_reversible_truncated_positive_lifts() {
        // E-8 positive: qb = +1, Mb = 8, Nb = 4, r = 0.5
        // → Rqb = (1 + 0.5 · 2^4) = 1 + 8 = 9
        let rqb = reconstruct_reversible(1, 8, 4, 0.5);
        assert!((rqb - 9.0).abs() < 1e-12);
    }

    #[test]
    fn reconstruct_reversible_truncated_negative_lifts() {
        // E-8 negative: qb = -1, Mb = 8, Nb = 4, r = 0.5
        // → Rqb = (-1 - 0.5 · 2^4) = -9
        let rqb = reconstruct_reversible(-1, 8, 4, 0.5);
        assert!((rqb + 9.0).abs() < 1e-12);
    }

    #[test]
    fn reconstruct_reversible_truncated_zero_stays_zero() {
        // E-8 qb == 0 branch: Rqb = 0.
        let rqb = reconstruct_reversible(0, 8, 4, 0.5);
        assert_eq!(rqb, 0.0);
    }

    // -----------------------------------------------------------------
    // Equation E-9 round-trip — encoder + decoder line up under r = 0.5.
    // -----------------------------------------------------------------

    #[test]
    fn quantise_then_reconstruct_round_trip_midpoint() {
        // JPEG 2000's Equation E-9 quantiser is a dead-zone uniform
        // mid-tread with **floor** (`⌊|ab|/Δb⌋`), giving a centre bin
        // of width `2·Δb` around zero. Equation E-6 reconstructs
        // qb = 0 to 0 (no midpoint lift on the dead-zone bin) and
        // every other bin to its midpoint when r = 0.5. So the
        // worst-case round-trip error |Rqb - ab| is bounded by
        // **Δb** — driven by values just inside the dead zone — and
        // by Δb/2 for every other bin.
        let step = StepSize {
            epsilon: 8,
            mantissa: 0,
        };
        let dx = irreversible_step_size(8, step); // Δb = 1.0
        let mb_bits = mb(1, step.epsilon).unwrap();
        let r = 0.5;
        for ab in [-7.3_f64, -1.0, 0.0, 0.4, 0.6, 1.2, 5.5, 42.0] {
            let q = quantise_irreversible(ab, dx);
            let rqb = reconstruct_irreversible(q, mb_bits, mb_bits, dx, r);
            let err = (rqb - ab).abs();
            // Dead-zone bin if |ab| < Δb; other bins are mid-tread.
            let bound = if ab.abs() < dx { dx } else { dx * 0.5 };
            assert!(
                err <= bound + 1e-12,
                "ab = {} rqb = {} err = {}",
                ab,
                rqb,
                err
            );
        }
    }

    #[test]
    fn quantise_then_reconstruct_round_trip_with_fractional_step() {
        // Δb = 0.5 (εb > Rb): step size halves; dead-zone bound is Δb.
        let step = StepSize {
            epsilon: 9,
            mantissa: 0,
        };
        let dx = irreversible_step_size(8, step);
        assert!((dx - 0.5).abs() < 1e-12);
        let mb_bits = mb(1, step.epsilon).unwrap();
        let r = 0.5;
        for ab in [-2.4_f64, -0.7, 0.1, 0.6, 1.3, 3.9] {
            let q = quantise_irreversible(ab, dx);
            let rqb = reconstruct_irreversible(q, mb_bits, mb_bits, dx, r);
            let err = (rqb - ab).abs();
            let bound = if ab.abs() < dx { dx } else { dx * 0.5 };
            assert!(
                err <= bound + 1e-12,
                "ab = {} rqb = {} err = {}",
                ab,
                rqb,
                err
            );
        }
    }

    #[test]
    fn quantise_then_reconstruct_outside_dead_zone_bounded_by_half_step() {
        // Tighter test: for any |ab| ≥ Δb, the round-trip error stays
        // within Δb/2 (the classic mid-tread guarantee for non-dead-zone bins).
        let step = StepSize {
            epsilon: 8,
            mantissa: 0,
        };
        let dx = irreversible_step_size(8, step); // 1.0
        let mb_bits = mb(1, step.epsilon).unwrap();
        let r = 0.5;
        for ab in [-7.3_f64, -1.2, -1.0, 1.0, 1.2, 5.5, 42.0] {
            let q = quantise_irreversible(ab, dx);
            let rqb = reconstruct_irreversible(q, mb_bits, mb_bits, dx, r);
            let err = (rqb - ab).abs();
            assert!(
                err <= dx * 0.5 + 1e-12,
                "ab = {} rqb = {} err = {}",
                ab,
                rqb,
                err
            );
        }
    }

    #[test]
    fn quantise_zero_step_size_guarded() {
        // Defensive: Δb = 0 should not panic with divide-by-zero.
        assert_eq!(quantise_irreversible(42.0, 0.0), 0);
    }

    // -----------------------------------------------------------------
    // Worked example: 8-bit grayscale, NL = 1, ScalarDerived NLLL.
    // -----------------------------------------------------------------

    #[test]
    fn worked_example_8bit_grayscale_nl1_derived() {
        // RI = 8, NL = 1, G = 1.
        // NLLL signalled: (ε₀, µ₀) = (8, 0) → Δ_NLLL = 2^(Rb_LL - 8) · 1 = 2^(8 - 8) = 1.
        let nlll = StepSize {
            epsilon: 8,
            mantissa: 0,
        };
        let g = 1u8;

        // LL band (r = 0, nb = NL = 1): εb = ε₀ - NL + nb = 8 - 1 + 1 = 8.
        let ll = derive_from_nlll(nlll, 1, 1).unwrap();
        assert_eq!(
            ll,
            StepSize {
                epsilon: 8,
                mantissa: 0
            }
        );
        let rb_ll = nominal_dynamic_range(8, SubBandOrientation::LL);
        assert!((irreversible_step_size(rb_ll, ll) - 1.0).abs() < 1e-12);
        assert_eq!(mb(g, ll.epsilon).unwrap(), 8);

        // High-pass bands (r = 1, nb = NL - r + 1 = 1): same ε, but
        // different Rb because gainb differs.
        let hp = derive_from_nlll(nlll, 1, 1).unwrap();
        let rb_hl = nominal_dynamic_range(8, SubBandOrientation::HL);
        let dx_hl = irreversible_step_size(rb_hl, hp);
        // Rb_HL = 9, εb = 8 → 2^1 = 2.0.
        assert!((dx_hl - 2.0).abs() < 1e-12);
        let rb_hh = nominal_dynamic_range(8, SubBandOrientation::HH);
        let dx_hh = irreversible_step_size(rb_hh, hp);
        // Rb_HH = 10, εb = 8 → 2^2 = 4.0.
        assert!((dx_hh - 4.0).abs() < 1e-12);
    }

    #[test]
    fn worked_example_reversible_pass_through() {
        // For reversible, Δb = 1; the tier-1 coefficient comes out unchanged when fully decoded.
        let coeff = Coefficient {
            magnitude: 137,
            sigma: true,
            sign: true,
            already_refined: false,
        };
        let qb = qb_signed(coeff);
        assert_eq!(qb, -137);
        let mb_bits = 8;
        let rqb = reconstruct_reversible(qb, mb_bits, mb_bits, 0.5);
        assert_eq!(rqb, -137.0);
    }
}
