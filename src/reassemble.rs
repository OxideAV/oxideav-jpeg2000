//! Code-block → sub-band reassembly — T.800 §B.7 / §B.9 + Annex E.
//!
//! This module is the bridge between three other tier-2 submodules:
//!
//! * **[`crate::t1`]** has decoded a rectangular [`t1::CodeBlock`] per
//!   §D.3, leaving every coefficient with a positionally-weighted
//!   magnitude and a sign.
//! * **[`crate::geometry`]** has enumerated the precincts of one
//!   resolution level into [`PrecinctCodeBlocks`] (one
//!   [`PrecinctSubBand`] per sub-band, each carrying clipped sample
//!   corners `(x0, y0, x1, y1)` on the sub-band domain per §B.7 / §B.9).
//! * **[`crate::dequant`]** turns one tier-1 [`t1::Coefficient`] back into
//!   a reconstructed transform coefficient (Equation E-1 → E-6 / E-7 /
//!   E-8).
//!
//! This module composes those into a per-sub-band coefficient array
//! sized exactly `(tbx1 - tbx0) × (tby1 - tby0)` — the input shape the
//! [`crate::dwt::sr_2d_5x3`] / [`crate::dwt::sr_2d_9x7`] inverse 2D
//! sub-band reconstruction consumes.
//!
//! ## What the §B.7 NOTE costs us
//!
//! The §B.7 "code-block partition is anchored at `(0, 0)` and may
//! extend past the sub-band edge — only the coefficients inside the
//! sub-band are coded" clause is already absorbed in
//! [`PrecinctCodeBlock`]'s clipped `(x0, y0, x1, y1)`: the caller's
//! [`t1::CodeBlock`] has exactly `(x1 - x0) × (y1 - y0)` real
//! coefficients. Scatter is therefore a direct copy into
//! `[x0 - tbx0 .. x1 - tbx0, y0 - tby0 .. y1 - tby0)`.
//!
//! ## What the §B.10.5 zero-bit-plane lift costs us
//!
//! The number of decoded magnitude bits per coefficient, `Nb(u, v)`, is
//! **not** uniform inside a code-block under arbitrary
//! truncation. In the simple "fully-decoded code-block" case `Nb = Mb -
//! P` where `P` is the zero-bit-plane count from §B.10.5 (the packet-
//! header tag tree per code-block); every coefficient in the code-block
//! shares that `Nb`. The richer per-pass / per-coefficient `Nb` (e.g.
//! a tier-2 packet that includes only the SP pass of a given bit-plane
//! and not the MR / cleanup passes) is reachable from the
//! `Pass` order driven by [`crate::t1::BitPlaneSequencer`], but for the
//! reassembly bridge we accept the uniform-`Nb` case as the input
//! contract (the caller has tracked the actual decoded-bit count for
//! every coefficient as part of running the passes); a future round
//! can lift `Nb` to per-coefficient.
//!
//! ## What this module does NOT cover
//!
//! * Multi-component transformation (Annex G).
//! * ROI scaling-shift de-application (§J.10).
//! * The HTJ2K (Part 15) block-coder path.
//! * Selection of `r` per the §E.1.1.2 NOTE — the caller picks (a
//!   common choice is `r = 0.5`).
//!
//! ## Clean-room provenance
//!
//! Implemented solely from
//! `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`:
//!
//! * §B.7 (code-block partition anchored at `(0, 0)` on the sub-band
//!   domain; §B.7 NOTE — only the part of a partition cell inside the
//!   sub-band is coded).
//! * §B.9 (the code-blocks of every sub-band confined to one precinct;
//!   raster order is the §B.10.8 packet-header walk order).
//! * §B.10.5 (zero-bit-plane information tag tree — establishes the
//!   per-code-block `Mb − 1 − P` "first non-empty" bit-plane).
//! * Annex E (inverse quantisation) — Equation E-1 (signed `qb` from
//!   tier-1 magnitude / sign), Equation E-6 (irreversible Rqb),
//!   Equation E-7 (reversible Rqb at `Nb = Mb`), Equation E-8
//!   (reversible Rqb at `Nb < Mb`).
//!
//! No external library source (OpenJPEG, OpenJPH, Kakadu, FFmpeg,
//! libavcodec, etc.) was consulted, quoted, paraphrased, or used as a
//! cross-check oracle. No WebSearch / WebFetch was used for any
//! reason.

use crate::dequant::{
    irreversible_step_size, nominal_dynamic_range, qb_signed, reconstruct_irreversible,
    reconstruct_reversible, subband_gain_log2, StepSize,
};
use crate::geometry::{PrecinctCodeBlock, ResolutionLevel, SubBand, SubBandOrientation};
use crate::t1::CodeBlock;
use crate::Error;

/// One decoded code-block ready to be scattered into a sub-band array.
///
/// `placement` carries the code-block's clipped sample corners on the
/// sub-band domain — exactly as produced by
/// [`crate::geometry::derive_precinct_code_blocks`]. `coefficients`
/// must be the tier-1-output [`CodeBlock`] for that placement: its
/// `width()` / `height()` must equal `placement.x1 - placement.x0` /
/// `placement.y1 - placement.y0` (the clipped extent per §B.7 NOTE).
///
/// `nb` is the per-code-block "number of decoded magnitude bits"
/// — the uniform `Nb(u, v)` shared by every coefficient inside the
/// block under non-truncated decoding (`Nb = Mb − P` where `P` is the
/// §B.10.5 zero-bit-plane count). The caller computes it from `Mb`
/// (Equation E-2) minus `P` minus any unfinished bit-planes the
/// packet-header pass count left undecoded for this block.
///
/// Borrowing `coefficients` keeps the bridge zero-copy: the same
/// [`CodeBlock`] can be inspected by the test suite while the
/// reassembly pass reads each coefficient through
/// [`CodeBlock::coefficient`].
#[derive(Debug, Clone, Copy)]
pub struct CodedCodeBlock<'a> {
    /// Clipped placement of this code-block on the sub-band domain.
    pub placement: PrecinctCodeBlock,
    /// Decoded tier-1 code-block (`width * height` coefficients).
    pub coefficients: &'a CodeBlock,
    /// Uniform per-coefficient `Nb(u, v)` (number of decoded magnitude
    /// bits). The §B.10.5 / §E.1.1.2 truncation model.
    pub nb: u32,
}

/// One sub-band's `(εb, µb)` quantisation parameters paired with its
/// resolved `Mb` (Equation E-2) and `Rb` (Equation E-4).
///
/// Carried as a single struct so the caller can resolve §A.6.4 / E
/// once (per sub-band, per component) and pass it through to every
/// reassembly invocation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubBandQuantization {
    /// `(εb, µb)` for this sub-band.
    pub step: StepSize,
    /// `Mb` per Equation E-2 (`G + εb − 1`).
    pub mb: u32,
    /// `Rb` per Equation E-4 (`RI + log₂(gainb)`).
    pub rb: u32,
}

impl SubBandQuantization {
    /// Resolve `(Mb, Rb)` from sample precision, guard bits, sub-band
    /// orientation and `(εb, µb)`.
    ///
    /// `precision` is `RI` — the SIZ component sample precision in
    /// bits (`Ssizi & 0x7F + 1`). `guard_bits` is the QCD / QCC
    /// high-3-bit `G` count.
    pub fn resolve(
        precision: u32,
        guard_bits: u8,
        orientation: SubBandOrientation,
        step: StepSize,
    ) -> Result<Self, Error> {
        let mb = crate::dequant::mb(guard_bits, step.epsilon)?;
        let rb = nominal_dynamic_range(precision, orientation);
        // Cross-check sub-band gain wasn't out of whack (defensive;
        // current `subband_gain_log2` returns 0..=2 so it can't
        // mismatch, but resolved here for completeness in case future
        // rounds extend `Rb` per a Part-2 extension).
        let _ = subband_gain_log2(orientation);
        Ok(Self { step, mb, rb })
    }
}

// =====================================================================
// One-sub-band reassembly.
// =====================================================================

/// Scatter every coded code-block of one sub-band into a single
/// `(width, height)` `i32` array (the **reversible 5-3** path).
///
/// `band` is the sub-band's [`SubBand`] (its `tbx0` / `tby0` are the
/// per-array origin). `blocks` is the list of decoded code-blocks
/// belonging to this sub-band — across **all** precincts of the
/// owning resolution level. `mb` is the per-sub-band Equation E-2
/// integer-representation bit-count (`G + εb − 1`); the reversible
/// path needs no `Δb` because §E.1.2.1 fixes it at `1`. `r` is the
/// §E.1.1.2 reconstruction parameter (the spec allows `0 ≤ r < 1`;
/// pass `0.5` for the conventional midpoint).
///
/// Reversible reconstruction follows Equations E-7 / E-8: `Δb = 1`,
/// so the reconstruction is exact integer recovery when `Nb = Mb` and
/// a `r · 2^(Mb − Nb)` midpoint lift otherwise. The result is rounded
/// toward zero into `i32`; callers running on `Mb ≤ 31` (the standard
/// 5-3 bit budget under `G = 1..7`, `precision ≤ 23`) never see
/// saturation.
///
/// # Errors
///
/// Returns [`Error::InvalidMarkerLength`] if any code-block's
/// placement extends outside the sub-band rectangle, if a
/// [`CodeBlock`]'s width / height does not match its placement's
/// clipped extent, or if two code-blocks claim the same coefficient.
pub fn reassemble_subband_5x3(
    band: &SubBand,
    blocks: &[CodedCodeBlock<'_>],
    mb: u32,
    r: f64,
) -> Result<Vec<i32>, Error> {
    let width = band.width() as usize;
    let height = band.height() as usize;
    if width == 0 || height == 0 {
        return Ok(Vec::new());
    }
    let mut out = vec![0_i32; width * height];
    let mut written = vec![false; width * height];

    for cb in blocks {
        check_placement(band, &cb.placement, cb.coefficients)?;
        let dx = (cb.placement.x0 - band.tbx0) as usize;
        let dy = (cb.placement.y0 - band.tby0) as usize;
        let block_w = cb.coefficients.width();
        let block_h = cb.coefficients.height();
        for v in 0..block_h {
            for u in 0..block_w {
                let coef = cb.coefficients.coefficient(u, v);
                let qb = qb_signed(coef);
                let r_qb = reconstruct_reversible(qb, mb, cb.nb, r);
                let target = (dy + v) * width + (dx + u);
                if written[target] {
                    return Err(Error::InvalidMarkerLength);
                }
                written[target] = true;
                // Round toward zero — Equation E-7 returns an exact
                // integer when `Nb = Mb`, and Equation E-8's `r ·
                // 2^(Mb − Nb)` midpoint lift is conventionally
                // truncated toward zero by the rounding into the
                // wavelet integer domain.
                out[target] = r_qb_to_i32(r_qb);
            }
        }
    }

    Ok(out)
}

/// Scatter every coded code-block of one sub-band into a single
/// `(width, height)` `f64` array (the **irreversible 9-7** path).
///
/// `band` and `blocks` carry the same content as for
/// [`reassemble_subband_5x3`]. `quant` resolves the per-sub-band
/// `(Δb, Mb)` pair; `r` is the §E.1.1.2 reconstruction parameter
/// (conventionally `0.5`).
///
/// Irreversible reconstruction follows Equation E-6: `Δb = 2^(Rb −
/// εb) · (1 + µb / 2^11)`; `Rqb = (qb + sign(qb) · r · 2^(Mb − Nb))
/// · Δb`. The result stays in `f64` until the inverse 9-7 DWT pulls
/// it back to the sample domain (Equation F-7's STEP1 / STEP2 scale
/// by `K` / `1/K` are the only floating-point dependence the spec
/// puts on the path).
///
/// # Errors
///
/// As [`reassemble_subband_5x3`].
pub fn reassemble_subband_9x7(
    band: &SubBand,
    blocks: &[CodedCodeBlock<'_>],
    quant: SubBandQuantization,
    r: f64,
) -> Result<Vec<f64>, Error> {
    let width = band.width() as usize;
    let height = band.height() as usize;
    if width == 0 || height == 0 {
        return Ok(Vec::new());
    }
    let mut out = vec![0.0_f64; width * height];
    let mut written = vec![false; width * height];
    let step_size = irreversible_step_size(quant.rb, quant.step);

    for cb in blocks {
        check_placement(band, &cb.placement, cb.coefficients)?;
        let dx = (cb.placement.x0 - band.tbx0) as usize;
        let dy = (cb.placement.y0 - band.tby0) as usize;
        let block_w = cb.coefficients.width();
        let block_h = cb.coefficients.height();
        for v in 0..block_h {
            for u in 0..block_w {
                let coef = cb.coefficients.coefficient(u, v);
                let qb = qb_signed(coef);
                let r_qb = reconstruct_irreversible(qb, quant.mb, cb.nb, step_size, r);
                let target = (dy + v) * width + (dx + u);
                if written[target] {
                    return Err(Error::InvalidMarkerLength);
                }
                written[target] = true;
                out[target] = r_qb;
            }
        }
    }

    Ok(out)
}

/// Cast a reversible reconstruction `Rqb` to `i32` with saturation.
///
/// Equation E-7 returns an exact integer (the `qb` it received). The
/// `Nb < Mb` truncation path of Equation E-8 produces a `qb + r ·
/// 2^(Mb − Nb)` value that may not land on an integer — we truncate
/// toward zero, matching the §F.4 "the encoder rounds toward the
/// nearest integer; the decoder takes whatever the wavelet produces"
/// reading. Out-of-`i32`-range values saturate to `i32::MIN` /
/// `i32::MAX` rather than wrap.
#[inline]
fn r_qb_to_i32(value: f64) -> i32 {
    if value.is_nan() {
        0
    } else if value >= i32::MAX as f64 {
        i32::MAX
    } else if value <= i32::MIN as f64 {
        i32::MIN
    } else {
        // `as i32` truncates toward zero for in-range f64s, which is
        // the conventional reading of Equation E-8's midpoint lift.
        value as i32
    }
}

/// Validate that a [`PrecinctCodeBlock`] placement fits the sub-band
/// and that its dimensions match a [`CodeBlock`].
fn check_placement(
    band: &SubBand,
    placement: &PrecinctCodeBlock,
    block: &CodeBlock,
) -> Result<(), Error> {
    if placement.x0 < band.tbx0
        || placement.y0 < band.tby0
        || placement.x1 > band.tbx1
        || placement.y1 > band.tby1
    {
        return Err(Error::InvalidMarkerLength);
    }
    let placement_w = placement.x1.saturating_sub(placement.x0) as usize;
    let placement_h = placement.y1.saturating_sub(placement.y0) as usize;
    if block.width() != placement_w || block.height() != placement_h {
        return Err(Error::InvalidMarkerLength);
    }
    if block.orientation() != band.orientation {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(())
}

// =====================================================================
// One-resolution-level reassembly.
// =====================================================================

/// All four sub-band coefficient arrays of one resolution level, ready
/// to feed [`crate::dwt::sr_2d_5x3`].
///
/// At `r = 0` the resolution carries one `LL` band, so `hl` / `lh` /
/// `hh` are empty `Vec`s with `dims = (0, 0)`. At `r ≥ 1` `ll` is the
/// already-reconstructed `(r − 1)` band the caller carries forward;
/// `hl` / `lh` / `hh` are the §B.5 high-pass bands of this resolution
/// level.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolutionArrays5x3 {
    /// `LL` band coefficients (raster-major).
    pub ll: Vec<i32>,
    /// `LL` band dimensions `(w, h)`.
    pub ll_dims: (usize, usize),
    /// `HL` band coefficients (raster-major).
    pub hl: Vec<i32>,
    /// `HL` band dimensions.
    pub hl_dims: (usize, usize),
    /// `LH` band coefficients.
    pub lh: Vec<i32>,
    /// `LH` band dimensions.
    pub lh_dims: (usize, usize),
    /// `HH` band coefficients.
    pub hh: Vec<i32>,
    /// `HH` band dimensions.
    pub hh_dims: (usize, usize),
}

/// All four sub-band coefficient arrays of one resolution level, ready
/// to feed [`crate::dwt::sr_2d_9x7`].
#[derive(Debug, Clone, PartialEq)]
pub struct ResolutionArrays9x7 {
    /// `LL` band coefficients (raster-major).
    pub ll: Vec<f64>,
    /// `LL` band dimensions.
    pub ll_dims: (usize, usize),
    /// `HL` band coefficients.
    pub hl: Vec<f64>,
    /// `HL` band dimensions.
    pub hl_dims: (usize, usize),
    /// `LH` band coefficients.
    pub lh: Vec<f64>,
    /// `LH` band dimensions.
    pub lh_dims: (usize, usize),
    /// `HH` band coefficients.
    pub hh: Vec<f64>,
    /// `HH` band dimensions.
    pub hh_dims: (usize, usize),
}

/// A caller-provided dispatch: for one sub-band, give me the list of
/// decoded code-blocks (across every precinct).
///
/// Used by [`reassemble_resolution_5x3`] / [`reassemble_resolution_9x7`]
/// so the caller controls how the per-(precinct, sub-band, code-block)
/// triples are collected — most callers will walk a §B.12 progression
/// order and accumulate into a `Vec<CodedCodeBlock>` per sub-band, but
/// other layouts (e.g. layer-by-layer streaming) are supported as long
/// as the caller hands one slice per sub-band when this trait fires.
pub trait BlockSource<'a> {
    /// Return the decoded code-blocks belonging to `band` of this
    /// resolution level. Each returned block must already have its
    /// tier-1 passes run; the caller controls how / when those passes
    /// fire.
    fn blocks_for(&self, band: &SubBand) -> &[CodedCodeBlock<'a>];
}

impl<'a> BlockSource<'a> for &[&[CodedCodeBlock<'a>]] {
    fn blocks_for(&self, band: &SubBand) -> &[CodedCodeBlock<'a>] {
        // For the trivial slice-of-slices source, the caller must
        // pre-arrange the slices in the §B.9 packet order matching
        // `level.sub_bands` — `LL` at `r = 0`, then `[HL, LH, HH]` at
        // `r ≥ 1`. We pick the slice whose orientation matches the
        // sub-band (raster scan; falls back to empty when missing).
        for (i, group) in self.iter().enumerate() {
            if let Some(first) = group.first() {
                if first.coefficients.orientation() == band.orientation {
                    return self[i];
                }
            }
        }
        &[]
    }
}

/// Assemble the four sub-band arrays of one resolution level under the
/// **reversible 5-3** path.
///
/// `level` enumerates the sub-bands at this resolution; `mb_per_band`
/// supplies one `Mb` per `level.sub_bands` entry (Equation E-2 input).
/// `r` is the §E.1.1.2 reconstruction parameter. At `r = 0` only the
/// LL band is populated; `hl` / `lh` / `hh` are empty.
///
/// The caller-provided `ll_carry` is the `LL` band the previous-step
/// (`r − 1`) inverse 2D_SR produced — it is **not** computed here. At
/// `r = 0` `ll_carry` should be the `LL` band's own coefficients
/// (reassembled from this level's `LL` sub-band).
pub fn reassemble_resolution_5x3<'a, S: BlockSource<'a>>(
    level: &ResolutionLevel,
    source: &S,
    mb_per_band: &[u32],
    r: f64,
) -> Result<ResolutionArrays5x3, Error> {
    if mb_per_band.len() != level.sub_bands.len() {
        return Err(Error::InvalidMarkerLength);
    }
    let mut out = ResolutionArrays5x3 {
        ll: Vec::new(),
        ll_dims: (0, 0),
        hl: Vec::new(),
        hl_dims: (0, 0),
        lh: Vec::new(),
        lh_dims: (0, 0),
        hh: Vec::new(),
        hh_dims: (0, 0),
    };
    for (band, &mb) in level.sub_bands.iter().zip(mb_per_band.iter()) {
        let blocks = source.blocks_for(band);
        let array = reassemble_subband_5x3(band, blocks, mb, r)?;
        let dims = (band.width() as usize, band.height() as usize);
        match band.orientation {
            SubBandOrientation::LL => {
                out.ll = array;
                out.ll_dims = dims;
            }
            SubBandOrientation::HL => {
                out.hl = array;
                out.hl_dims = dims;
            }
            SubBandOrientation::LH => {
                out.lh = array;
                out.lh_dims = dims;
            }
            SubBandOrientation::HH => {
                out.hh = array;
                out.hh_dims = dims;
            }
        }
    }
    Ok(out)
}

/// Assemble the four sub-band arrays of one resolution level under the
/// **irreversible 9-7** path.
pub fn reassemble_resolution_9x7<'a, S: BlockSource<'a>>(
    level: &ResolutionLevel,
    source: &S,
    quant_per_band: &[SubBandQuantization],
    r: f64,
) -> Result<ResolutionArrays9x7, Error> {
    if quant_per_band.len() != level.sub_bands.len() {
        return Err(Error::InvalidMarkerLength);
    }
    let mut out = ResolutionArrays9x7 {
        ll: Vec::new(),
        ll_dims: (0, 0),
        hl: Vec::new(),
        hl_dims: (0, 0),
        lh: Vec::new(),
        lh_dims: (0, 0),
        hh: Vec::new(),
        hh_dims: (0, 0),
    };
    for (band, &quant) in level.sub_bands.iter().zip(quant_per_band.iter()) {
        let blocks = source.blocks_for(band);
        let array = reassemble_subband_9x7(band, blocks, quant, r)?;
        let dims = (band.width() as usize, band.height() as usize);
        match band.orientation {
            SubBandOrientation::LL => {
                out.ll = array;
                out.ll_dims = dims;
            }
            SubBandOrientation::HL => {
                out.hl = array;
                out.hl_dims = dims;
            }
            SubBandOrientation::LH => {
                out.lh = array;
                out.lh_dims = dims;
            }
            SubBandOrientation::HH => {
                out.hh = array;
                out.hh_dims = dims;
            }
        }
    }
    Ok(out)
}

// =====================================================================
// Tests.
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{PrecinctCodeBlock, SubBand, SubBandOrientation};
    use crate::t1::{CodeBlock, Coefficient};

    /// Helper: build a CodeBlock pre-populated with given (mag, sign)
    /// pairs in raster order.
    fn make_block(
        orientation: SubBandOrientation,
        width: usize,
        height: usize,
        entries: &[(u32, bool)],
    ) -> CodeBlock {
        assert_eq!(entries.len(), width * height);
        let coefficients: Vec<Coefficient> = entries
            .iter()
            .map(|&(mag, sign)| Coefficient {
                magnitude: mag,
                sigma: mag != 0,
                sign,
                already_refined: false,
            })
            .collect();
        CodeBlock::from_coefficients(orientation, width, height, coefficients)
    }

    // ---------------------------------------------------------------
    // SubBandQuantization::resolve.
    // ---------------------------------------------------------------

    #[test]
    fn quantization_resolve_ll_at_8bit_no_guard_overflow() {
        // RI = 8, G = 1, εb = 8, µb = 0 → Mb = 1 + 8 - 1 = 8, Rb = 8 + 0 = 8.
        let step = StepSize {
            epsilon: 8,
            mantissa: 0,
        };
        let q = SubBandQuantization::resolve(8, 1, SubBandOrientation::LL, step).unwrap();
        assert_eq!(q.mb, 8);
        assert_eq!(q.rb, 8);
    }

    #[test]
    fn quantization_resolve_hh_lifts_rb_by_two() {
        // RI = 8, HH gain = 4 → Rb = 8 + 2 = 10.
        let step = StepSize {
            epsilon: 8,
            mantissa: 0,
        };
        let q = SubBandQuantization::resolve(8, 1, SubBandOrientation::HH, step).unwrap();
        assert_eq!(q.rb, 10);
    }

    // ---------------------------------------------------------------
    // Single-sub-band scatter, reversible.
    // ---------------------------------------------------------------

    #[test]
    fn scatter_one_block_into_single_sub_band() {
        // 4x2 LL sub-band, one 4x2 code-block covering it.
        let band = SubBand {
            orientation: SubBandOrientation::LL,
            nb: 1,
            tbx0: 0,
            tby0: 0,
            tbx1: 4,
            tby1: 2,
        };
        let entries = [
            (1, false),
            (0, false),
            (3, false),
            (2, true),
            (0, false),
            (5, false),
            (7, true),
            (1, false),
        ];
        let block = make_block(SubBandOrientation::LL, 4, 2, &entries);
        let placement = PrecinctCodeBlock {
            cbx: 0,
            cby: 0,
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 2,
        };
        let blocks = [CodedCodeBlock {
            placement,
            coefficients: &block,
            nb: 8,
        }];
        // Reversible, Mb = 8, Nb = 8 → Equation E-7 exact integer
        // recovery. Result should be raw qb_signed for each
        // coefficient.
        let out = reassemble_subband_5x3(&band, &blocks, 8, 0.5).unwrap();
        assert_eq!(out, vec![1, 0, 3, -2, 0, 5, -7, 1]);
    }

    #[test]
    fn scatter_two_blocks_side_by_side_no_overlap() {
        // 4x2 sub-band split into two 2x2 code-blocks (left and right).
        let band = SubBand {
            orientation: SubBandOrientation::LL,
            nb: 1,
            tbx0: 0,
            tby0: 0,
            tbx1: 4,
            tby1: 2,
        };
        let left = make_block(
            SubBandOrientation::LL,
            2,
            2,
            &[(10, false), (11, false), (12, false), (13, false)],
        );
        let right = make_block(
            SubBandOrientation::LL,
            2,
            2,
            &[(20, true), (21, false), (22, false), (23, true)],
        );
        let blocks = [
            CodedCodeBlock {
                placement: PrecinctCodeBlock {
                    cbx: 0,
                    cby: 0,
                    x0: 0,
                    y0: 0,
                    x1: 2,
                    y1: 2,
                },
                coefficients: &left,
                nb: 8,
            },
            CodedCodeBlock {
                placement: PrecinctCodeBlock {
                    cbx: 1,
                    cby: 0,
                    x0: 2,
                    y0: 0,
                    x1: 4,
                    y1: 2,
                },
                coefficients: &right,
                nb: 8,
            },
        ];
        let out = reassemble_subband_5x3(&band, &blocks, 8, 0.5).unwrap();
        // Row 0: left[0,0]=10, left[1,0]=11, right[0,0]=-20, right[1,0]=21.
        // Row 1: left[0,1]=12, left[1,1]=13, right[0,1]=22, right[1,1]=-23.
        assert_eq!(out, vec![10, 11, -20, 21, 12, 13, 22, -23]);
    }

    #[test]
    fn scatter_with_non_zero_band_origin() {
        // Sub-band whose tbx0 / tby0 are non-zero — placement must
        // subtract them so the block lands at the right offset.
        let band = SubBand {
            orientation: SubBandOrientation::HL,
            nb: 1,
            tbx0: 5,
            tby0: 3,
            tbx1: 8,
            tby1: 5,
        };
        let block = make_block(
            SubBandOrientation::HL,
            3,
            2,
            &[
                (1, false),
                (2, false),
                (3, false),
                (4, false),
                (5, false),
                (6, false),
            ],
        );
        let blocks = [CodedCodeBlock {
            placement: PrecinctCodeBlock {
                cbx: 0,
                cby: 0,
                x0: 5,
                y0: 3,
                x1: 8,
                y1: 5,
            },
            coefficients: &block,
            nb: 8,
        }];
        let out = reassemble_subband_5x3(&band, &blocks, 8, 0.5).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn scatter_truncated_block_midpoint_lifts_magnitudes() {
        // Reversible Equation E-8: Nb < Mb → Rqb = qb ± r · 2^(Mb − Nb).
        // Mb = 5, Nb = 3, r = 0.5 → offset = 0.5 · 4 = 2.
        let band = SubBand {
            orientation: SubBandOrientation::LL,
            nb: 1,
            tbx0: 0,
            tby0: 0,
            tbx1: 1,
            tby1: 2,
        };
        // qb = +4 → Rqb = 4 + 2 = 6 → i32 6.
        // qb = -4 → Rqb = -4 - 2 = -6 → i32 -6.
        let block = make_block(SubBandOrientation::LL, 1, 2, &[(4, false), (4, true)]);
        let blocks = [CodedCodeBlock {
            placement: PrecinctCodeBlock {
                cbx: 0,
                cby: 0,
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 2,
            },
            coefficients: &block,
            nb: 3,
        }];
        let out = reassemble_subband_5x3(&band, &blocks, 5, 0.5).unwrap();
        assert_eq!(out, vec![6, -6]);
    }

    #[test]
    fn scatter_block_outside_band_rejected() {
        let band = SubBand {
            orientation: SubBandOrientation::LL,
            nb: 1,
            tbx0: 0,
            tby0: 0,
            tbx1: 4,
            tby1: 2,
        };
        let block = make_block(
            SubBandOrientation::LL,
            2,
            2,
            &[(1, false), (1, false), (1, false), (1, false)],
        );
        let blocks = [CodedCodeBlock {
            placement: PrecinctCodeBlock {
                cbx: 0,
                cby: 0,
                x0: 3,
                y0: 0,
                x1: 5,
                y1: 2,
            },
            coefficients: &block,
            nb: 8,
        }];
        let res = reassemble_subband_5x3(&band, &blocks, 8, 0.5);
        assert_eq!(res, Err(Error::InvalidMarkerLength));
    }

    #[test]
    fn scatter_orientation_mismatch_rejected() {
        let band = SubBand {
            orientation: SubBandOrientation::HL,
            nb: 1,
            tbx0: 0,
            tby0: 0,
            tbx1: 2,
            tby1: 2,
        };
        let block = make_block(
            SubBandOrientation::LL,
            2,
            2,
            &[(1, false), (1, false), (1, false), (1, false)],
        );
        let blocks = [CodedCodeBlock {
            placement: PrecinctCodeBlock {
                cbx: 0,
                cby: 0,
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 2,
            },
            coefficients: &block,
            nb: 8,
        }];
        let res = reassemble_subband_5x3(&band, &blocks, 8, 0.5);
        assert_eq!(res, Err(Error::InvalidMarkerLength));
    }

    #[test]
    fn scatter_block_dim_mismatch_rejected() {
        let band = SubBand {
            orientation: SubBandOrientation::LL,
            nb: 1,
            tbx0: 0,
            tby0: 0,
            tbx1: 4,
            tby1: 4,
        };
        // Placement says 2x2 but block is 1x1.
        let block = make_block(SubBandOrientation::LL, 1, 1, &[(1, false)]);
        let blocks = [CodedCodeBlock {
            placement: PrecinctCodeBlock {
                cbx: 0,
                cby: 0,
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 2,
            },
            coefficients: &block,
            nb: 8,
        }];
        let res = reassemble_subband_5x3(&band, &blocks, 8, 0.5);
        assert_eq!(res, Err(Error::InvalidMarkerLength));
    }

    #[test]
    fn scatter_overlapping_blocks_rejected() {
        let band = SubBand {
            orientation: SubBandOrientation::LL,
            nb: 1,
            tbx0: 0,
            tby0: 0,
            tbx1: 4,
            tby1: 2,
        };
        let block = make_block(SubBandOrientation::LL, 3, 2, &[(1, false); 6]);
        // Two blocks both claiming (1..3, 0..2).
        let blocks = [
            CodedCodeBlock {
                placement: PrecinctCodeBlock {
                    cbx: 0,
                    cby: 0,
                    x0: 0,
                    y0: 0,
                    x1: 3,
                    y1: 2,
                },
                coefficients: &block,
                nb: 8,
            },
            CodedCodeBlock {
                placement: PrecinctCodeBlock {
                    cbx: 1,
                    cby: 0,
                    x0: 1,
                    y0: 0,
                    x1: 4,
                    y1: 2,
                },
                coefficients: &block,
                nb: 8,
            },
        ];
        let res = reassemble_subband_5x3(&band, &blocks, 8, 0.5);
        assert_eq!(res, Err(Error::InvalidMarkerLength));
    }

    #[test]
    fn scatter_empty_band_returns_empty_vec() {
        let band = SubBand {
            orientation: SubBandOrientation::LL,
            nb: 1,
            tbx0: 5,
            tby0: 5,
            tbx1: 5,
            tby1: 5,
        };
        let out = reassemble_subband_5x3(&band, &[], 8, 0.5).unwrap();
        assert!(out.is_empty());
    }

    // ---------------------------------------------------------------
    // Single-sub-band scatter, irreversible.
    // ---------------------------------------------------------------

    #[test]
    fn scatter_irreversible_applies_step_size() {
        // Equation E-3: Δb = 2^(Rb − εb) · (1 + µb / 2^11).
        // Rb = 8, εb = 8, µb = 0 → Δb = 1.0.
        // qb = +5, Mb = 8, Nb = 8, r = 0.5: Equation E-6 always applies
        // the midpoint lift, so offset = 0.5 · 2^0 = 0.5 → Rqb =
        // (5 + 0.5) · 1.0 = 5.5.
        let band = SubBand {
            orientation: SubBandOrientation::LL,
            nb: 1,
            tbx0: 0,
            tby0: 0,
            tbx1: 1,
            tby1: 1,
        };
        let block = make_block(SubBandOrientation::LL, 1, 1, &[(5, false)]);
        let quant = SubBandQuantization::resolve(
            8,
            1,
            SubBandOrientation::LL,
            StepSize {
                epsilon: 8,
                mantissa: 0,
            },
        )
        .unwrap();
        let blocks = [CodedCodeBlock {
            placement: PrecinctCodeBlock {
                cbx: 0,
                cby: 0,
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1,
            },
            coefficients: &block,
            nb: 8,
        }];
        let out = reassemble_subband_9x7(&band, &blocks, quant, 0.5).unwrap();
        assert_eq!(out.len(), 1);
        assert!((out[0] - 5.5).abs() < 1e-12);
    }

    #[test]
    fn scatter_irreversible_zero_r_is_exact_integer_recovery() {
        // r = 0 disables the §E.1.1.2 midpoint lift entirely, so a
        // qb = 5, Δb = 1.0 coefficient reconstructs to exactly 5.0.
        let band = SubBand {
            orientation: SubBandOrientation::LL,
            nb: 1,
            tbx0: 0,
            tby0: 0,
            tbx1: 1,
            tby1: 1,
        };
        let block = make_block(SubBandOrientation::LL, 1, 1, &[(5, false)]);
        let quant = SubBandQuantization::resolve(
            8,
            1,
            SubBandOrientation::LL,
            StepSize {
                epsilon: 8,
                mantissa: 0,
            },
        )
        .unwrap();
        let blocks = [CodedCodeBlock {
            placement: PrecinctCodeBlock {
                cbx: 0,
                cby: 0,
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1,
            },
            coefficients: &block,
            nb: 8,
        }];
        let out = reassemble_subband_9x7(&band, &blocks, quant, 0.0).unwrap();
        assert!((out[0] - 5.0).abs() < 1e-12);
    }

    #[test]
    fn scatter_irreversible_step_size_two() {
        // For HL band, Rb = 8 + 1 = 9. To get Δb = 2 with Rb = 9, we
        // need 2^(Rb - εb) = 2^(9 - 8) = 2, mantissa 0 → Δb = 2.
        // Mb = G + εb - 1 = 1 + 8 - 1 = 8.
        // qb = +5, Nb = 8 = Mb, r = 0 (skip midpoint) → Rqb = 5 · 2 = 10.0.
        let band = SubBand {
            orientation: SubBandOrientation::HL,
            nb: 1,
            tbx0: 0,
            tby0: 0,
            tbx1: 1,
            tby1: 1,
        };
        let block = make_block(SubBandOrientation::HL, 1, 1, &[(5, false)]);
        let quant = SubBandQuantization::resolve(
            8,
            1,
            SubBandOrientation::HL,
            StepSize {
                epsilon: 8,
                mantissa: 0,
            },
        )
        .unwrap();
        assert_eq!(quant.mb, 8);
        assert_eq!(quant.rb, 9);
        let blocks = [CodedCodeBlock {
            placement: PrecinctCodeBlock {
                cbx: 0,
                cby: 0,
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1,
            },
            coefficients: &block,
            nb: 8,
        }];
        let out = reassemble_subband_9x7(&band, &blocks, quant, 0.0).unwrap();
        assert!((out[0] - 10.0).abs() < 1e-12);
    }

    #[test]
    fn scatter_irreversible_zero_coefficient_no_offset() {
        // Equation E-6's special case: qb == 0 → Rqb = 0 regardless
        // of r / Δb / Mb / Nb.
        let band = SubBand {
            orientation: SubBandOrientation::LL,
            nb: 1,
            tbx0: 0,
            tby0: 0,
            tbx1: 1,
            tby1: 1,
        };
        let block = make_block(SubBandOrientation::LL, 1, 1, &[(0, false)]);
        let quant = SubBandQuantization::resolve(
            8,
            1,
            SubBandOrientation::LL,
            StepSize {
                epsilon: 8,
                mantissa: 0,
            },
        )
        .unwrap();
        let blocks = [CodedCodeBlock {
            placement: PrecinctCodeBlock {
                cbx: 0,
                cby: 0,
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1,
            },
            coefficients: &block,
            nb: 8,
        }];
        let out = reassemble_subband_9x7(&band, &blocks, quant, 0.5).unwrap();
        assert_eq!(out[0], 0.0);
    }

    // ---------------------------------------------------------------
    // Resolution-level assembly + round-trip through inverse DWT.
    // ---------------------------------------------------------------

    #[test]
    fn resolution_level_assembly_round_trips_5x3_constant_signal() {
        // 4x4 tile-component, NL = 1.
        // Resolution level r = 1 carries HL/LH/HH bands at 2x2 each;
        // resolution level r = 0 carries LL band at 2x2.
        // For a constant signal x[i,j] = c, the forward 5-3 DWT
        // produces LL = c, HL = LH = HH = 0 (no detail). Run the
        // reassembly then the inverse 2D_SR; check it reconstructs to
        // a constant signal.
        let level = ResolutionLevel {
            r: 1,
            n_l: 1,
            trx0: 0,
            try0: 0,
            trx1: 4,
            try1: 4,
            sub_bands: vec![
                SubBand {
                    orientation: SubBandOrientation::HL,
                    nb: 1,
                    tbx0: 0,
                    tby0: 0,
                    tbx1: 2,
                    tby1: 2,
                },
                SubBand {
                    orientation: SubBandOrientation::LH,
                    nb: 1,
                    tbx0: 0,
                    tby0: 0,
                    tbx1: 2,
                    tby1: 2,
                },
                SubBand {
                    orientation: SubBandOrientation::HH,
                    nb: 1,
                    tbx0: 0,
                    tby0: 0,
                    tbx1: 2,
                    tby1: 2,
                },
            ],
        };
        let zero_block_hl = make_block(SubBandOrientation::HL, 2, 2, &[(0, false); 4]);
        let zero_block_lh = make_block(SubBandOrientation::LH, 2, 2, &[(0, false); 4]);
        let zero_block_hh = make_block(SubBandOrientation::HH, 2, 2, &[(0, false); 4]);
        let placement = PrecinctCodeBlock {
            cbx: 0,
            cby: 0,
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 2,
        };

        let hl_blocks: &[CodedCodeBlock<'_>] = &[CodedCodeBlock {
            placement,
            coefficients: &zero_block_hl,
            nb: 8,
        }];
        let lh_blocks: &[CodedCodeBlock<'_>] = &[CodedCodeBlock {
            placement,
            coefficients: &zero_block_lh,
            nb: 8,
        }];
        let hh_blocks: &[CodedCodeBlock<'_>] = &[CodedCodeBlock {
            placement,
            coefficients: &zero_block_hh,
            nb: 8,
        }];
        let groups: Vec<&[CodedCodeBlock<'_>]> = vec![hl_blocks, lh_blocks, hh_blocks];
        let source = groups.as_slice();

        let arrays = reassemble_resolution_5x3(&level, &source, &[8, 8, 8], 0.5).unwrap();
        assert_eq!(arrays.hl, vec![0, 0, 0, 0]);
        assert_eq!(arrays.hl_dims, (2, 2));
        assert_eq!(arrays.lh_dims, (2, 2));
        assert_eq!(arrays.hh_dims, (2, 2));
        assert!(arrays.ll.is_empty()); // LL not in this resolution level.

        // Feed LL = constant 5 plus zero high-pass into the inverse 5-3.
        let ll = vec![5_i32; 4];
        let result = crate::dwt::sr_2d_5x3(
            &ll,
            (2, 2),
            &arrays.hl,
            arrays.hl_dims,
            &arrays.lh,
            arrays.lh_dims,
            &arrays.hh,
            arrays.hh_dims,
            0,
            0,
        )
        .unwrap();
        for px in &result.data {
            assert_eq!(*px, 5);
        }
    }

    #[test]
    fn resolution_level_5x3_picks_correct_band_per_orientation() {
        // Sub-bands listed in [HL, LH, HH] order; the BlockSource trait
        // must direct each sub-band's call to the matching slice
        // regardless of insertion order in the source array.
        let level = ResolutionLevel {
            r: 1,
            n_l: 1,
            trx0: 0,
            try0: 0,
            trx1: 2,
            try1: 2,
            sub_bands: vec![
                SubBand {
                    orientation: SubBandOrientation::HL,
                    nb: 1,
                    tbx0: 0,
                    tby0: 0,
                    tbx1: 1,
                    tby1: 1,
                },
                SubBand {
                    orientation: SubBandOrientation::LH,
                    nb: 1,
                    tbx0: 0,
                    tby0: 0,
                    tbx1: 1,
                    tby1: 1,
                },
                SubBand {
                    orientation: SubBandOrientation::HH,
                    nb: 1,
                    tbx0: 0,
                    tby0: 0,
                    tbx1: 1,
                    tby1: 1,
                },
            ],
        };
        let block_hl = make_block(SubBandOrientation::HL, 1, 1, &[(11, false)]);
        let block_lh = make_block(SubBandOrientation::LH, 1, 1, &[(22, false)]);
        let block_hh = make_block(SubBandOrientation::HH, 1, 1, &[(33, false)]);
        let placement = PrecinctCodeBlock {
            cbx: 0,
            cby: 0,
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };

        let hl_blocks: &[CodedCodeBlock<'_>] = &[CodedCodeBlock {
            placement,
            coefficients: &block_hl,
            nb: 8,
        }];
        let lh_blocks: &[CodedCodeBlock<'_>] = &[CodedCodeBlock {
            placement,
            coefficients: &block_lh,
            nb: 8,
        }];
        let hh_blocks: &[CodedCodeBlock<'_>] = &[CodedCodeBlock {
            placement,
            coefficients: &block_hh,
            nb: 8,
        }];
        // Pass in HH-first order to confirm BlockSource matches on
        // orientation, not list position.
        let groups: Vec<&[CodedCodeBlock<'_>]> = vec![hh_blocks, hl_blocks, lh_blocks];
        let source = groups.as_slice();
        let arrays = reassemble_resolution_5x3(&level, &source, &[8, 8, 8], 0.5).unwrap();
        assert_eq!(arrays.hl, vec![11]);
        assert_eq!(arrays.lh, vec![22]);
        assert_eq!(arrays.hh, vec![33]);
    }

    #[test]
    fn resolution_level_5x3_mb_per_band_length_check() {
        let level = ResolutionLevel {
            r: 0,
            n_l: 1,
            trx0: 0,
            try0: 0,
            trx1: 1,
            try1: 1,
            sub_bands: vec![SubBand {
                orientation: SubBandOrientation::LL,
                nb: 1,
                tbx0: 0,
                tby0: 0,
                tbx1: 1,
                tby1: 1,
            }],
        };
        let groups: Vec<&[CodedCodeBlock<'_>]> = Vec::new();
        let source = groups.as_slice();
        let res = reassemble_resolution_5x3(&level, &source, &[8, 8], 0.5);
        assert_eq!(res, Err(Error::InvalidMarkerLength));
    }

    // ---------------------------------------------------------------
    // r_qb_to_i32 saturation.
    // ---------------------------------------------------------------

    #[test]
    fn r_qb_to_i32_saturates_above_i32_max() {
        assert_eq!(r_qb_to_i32(1e20), i32::MAX);
    }

    #[test]
    fn r_qb_to_i32_saturates_below_i32_min() {
        assert_eq!(r_qb_to_i32(-1e20), i32::MIN);
    }

    #[test]
    fn r_qb_to_i32_handles_nan() {
        assert_eq!(r_qb_to_i32(f64::NAN), 0);
    }

    #[test]
    fn r_qb_to_i32_truncates_toward_zero() {
        // E-8 lift may produce non-integer values; truncate toward zero.
        assert_eq!(r_qb_to_i32(3.7), 3);
        assert_eq!(r_qb_to_i32(-3.7), -3);
    }
}
