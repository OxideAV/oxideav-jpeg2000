//! Tile decoder: orchestrates tier-2 packet parsing, tier-1 bitplane
//! decode, dequantisation, inverse DWT and level shift for a single
//! JPEG 2000 tile.
//!
//! Scope
//! -----
//!
//! - **5/3 integer reversible** wavelet (Part-1 lossless default) and
//!   **9/7 irreversible** float wavelet (lossy).
//! - **LRCP**, **RLCP**, **RPCL**, **PCRL**, and **CPRL** progression
//!   orders (T.800 §B.12.1). User-specified precinct partitions
//!   (§A.6.1 / §B.6) are honoured: the tier-2 walker iterates one
//!   packet per `(component, resolution, precinct, layer)` tuple in the
//!   spec's order, with each precinct emitting only the code-blocks of
//!   each sub-band that fall inside its rectangular footprint
//!   (§B.6 / §B.7 / §B.9).
//! - **Multiple quality layers** — each layer accumulates extra coding
//!   passes per code-block. Per T.800 Table D.8 default ("termination
//!   only on last pass"), the MQ stream is not broken at intermediate
//!   layer boundaries, so the per-code-block byte segments concatenate
//!   into one codeword segment that the tier-1 decoder runs once.
//!
//! Decodes a single tile. The multi-tile walk lives in
//! [`super::frame::decode_frame`], which groups tile-parts by `Isot`
//! (T.800 §A.4, §B.3) and invokes this entry point once per tile.
//!
//! Layout strategy
//! ---------------
//!
//! We work in the canonical "per-resolution canvas" layout:
//!
//! 1. Each sub-band's samples are held in a standalone `Vec<i32>`
//!    sized exactly to the sub-band dimensions.
//! 2. To synthesise resolution `r` from `r-1`, we build a combined
//!    canvas the size of LL_r, copy LL_{r-1} into the top-left quadrant,
//!    HL_r into the top-right, LH_r into the bottom-left, HH_r into the
//!    bottom-right, and run [`super::dwt::idwt_53`] on the whole block.
//!    The output is LL_r, which feeds the next iteration.

use super::bio::Bio;
use super::dwt;
use super::t1::{self, Orient};
use super::tagtree::TagTree;
use crate::error::{Jpeg2000Error as Error, Result};

#[derive(Debug, Clone)]
pub struct CodParams {
    pub sop_marker: bool,
    pub eph_marker: bool,
    pub progression_order: u8,
    pub num_layers: u16,
    pub mct: u8,
    pub num_decomp: u8,
    pub cblk_w_log2: u8,
    pub cblk_h_log2: u8,
    pub cblk_style: u32,
    /// `0` = 9/7 irreversible; `1` = 5/3 reversible.
    pub transform: u8,
    pub precincts: Vec<(u8, u8)>,
}

pub fn parse_cod(bytes: &[u8]) -> Result<CodParams> {
    if bytes.len() < 10 {
        return Err(Error::invalid("jpeg2000: COD payload too short"));
    }
    let scod = bytes[0];
    let sop_marker = (scod & 0x02) != 0;
    let eph_marker = (scod & 0x04) != 0;
    let user_precincts = (scod & 0x01) != 0;
    let progression_order = bytes[1];
    let num_layers = u16::from_be_bytes([bytes[2], bytes[3]]);
    let mct = bytes[4];
    let num_decomp = bytes[5];
    let cblk_w_log2 = (bytes[6] & 0x0F) + 2;
    let cblk_h_log2 = (bytes[7] & 0x0F) + 2;
    let cblk_style = bytes[8] as u32;
    let transform = bytes[9];
    let num_res = (num_decomp as usize) + 1;
    let precincts = if user_precincts {
        if bytes.len() < 10 + num_res {
            return Err(Error::invalid("jpeg2000: COD precinct bytes short"));
        }
        let mut v = Vec::with_capacity(num_res);
        for i in 0..num_res {
            let b = bytes[10 + i];
            v.push((b & 0x0F, (b >> 4) & 0x0F));
        }
        v
    } else {
        vec![(15, 15); num_res]
    };
    Ok(CodParams {
        sop_marker,
        eph_marker,
        progression_order,
        num_layers,
        mct,
        num_decomp,
        cblk_w_log2,
        cblk_h_log2,
        cblk_style,
        transform,
        precincts,
    })
}

/// One progression-order volume from a POC marker segment
/// (T.800 §A.6.6, §B.12.2). The volume covers all packets in the box
/// `[res_start, res_end) × [comp_start, comp_end) × [0, layer_end)`,
/// emitted in order `progression`.
#[derive(Debug, Clone, Copy)]
pub struct PocProgression {
    /// `RSpoc` — start resolution, inclusive.
    pub res_start: u8,
    /// `CSpoc` — start component, inclusive.
    pub comp_start: u16,
    /// `LYEpoc` — end layer, exclusive (always >= 1).
    pub layer_end: u16,
    /// `REpoc` — end resolution, exclusive (always > `res_start`).
    pub res_end: u8,
    /// `CEpoc` — end component, exclusive (always > `comp_start`).
    /// A wire value of `0` is interpreted as `256` per Table A.32.
    pub comp_end: u16,
    /// `Ppoc` — progression order for this volume (Table A.16).
    pub progression: u8,
}

/// Parsed POC marker payload (T.800 §A.6.6).
#[derive(Debug, Clone)]
pub struct PocParams {
    pub progressions: Vec<PocProgression>,
}

/// Decode a POC marker segment payload (without the leading length).
///
/// The number of progressions is derived from the segment length: each
/// progression occupies 7 bytes when `Csiz < 257` (8-bit component
/// fields) or 9 bytes when `Csiz >= 257` (16-bit component fields).
/// See Equation A-6.
pub fn parse_poc(bytes: &[u8], num_components: u16) -> Result<PocParams> {
    let csiz_wide = num_components >= 257;
    let entry_size = if csiz_wide { 9 } else { 7 };
    if bytes.is_empty() || bytes.len() % entry_size != 0 {
        return Err(Error::invalid(format!(
            "jpeg2000: POC segment length {} not divisible by {}",
            bytes.len(),
            entry_size
        )));
    }
    let n = bytes.len() / entry_size;
    let mut progressions = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * entry_size;
        let res_start = bytes[off];
        let (comp_start, off2) = if csiz_wide {
            (
                u16::from_be_bytes([bytes[off + 1], bytes[off + 2]]),
                off + 3,
            )
        } else {
            (bytes[off + 1] as u16, off + 2)
        };
        let layer_end = u16::from_be_bytes([bytes[off2], bytes[off2 + 1]]);
        let res_end = bytes[off2 + 2];
        let (comp_end_raw, off3) = if csiz_wide {
            (
                u16::from_be_bytes([bytes[off2 + 3], bytes[off2 + 4]]),
                off2 + 5,
            )
        } else {
            (bytes[off2 + 3] as u16, off2 + 4)
        };
        // Per Table A.32: a CEpoc value of 0 means "256" (when Csiz < 257)
        // — the maximum component count in that mode.
        let comp_end = if comp_end_raw == 0 && !csiz_wide {
            256
        } else {
            comp_end_raw
        };
        let progression = bytes[off3];
        if progression > 4 {
            return Err(Error::unsupported(
                "jpeg2000: POC progression order > 4 (Part-1 supports 0..=4)",
            ));
        }
        if res_end <= res_start {
            return Err(Error::invalid(format!(
                "jpeg2000: POC entry {i}: REpoc ({res_end}) must exceed RSpoc ({res_start})"
            )));
        }
        if comp_end <= comp_start {
            return Err(Error::invalid(format!(
                "jpeg2000: POC entry {i}: CEpoc ({comp_end}) must exceed CSpoc ({comp_start})"
            )));
        }
        if layer_end == 0 {
            return Err(Error::invalid(format!(
                "jpeg2000: POC entry {i}: LYEpoc must be >= 1"
            )));
        }
        progressions.push(PocProgression {
            res_start,
            comp_start,
            layer_end,
            res_end,
            comp_end,
            progression,
        });
    }
    Ok(PocParams { progressions })
}

#[derive(Debug, Clone)]
pub struct QcdParams {
    pub guard_bits: u8,
    pub bands: Vec<(u8, u16)>,
    pub is_reversible: bool,
}

pub fn parse_qcd(bytes: &[u8], num_decomp: u8) -> Result<QcdParams> {
    if bytes.is_empty() {
        return Err(Error::invalid("jpeg2000: QCD empty"));
    }
    let sqcd = bytes[0];
    let qntsty = sqcd & 0x1F;
    let guard_bits = sqcd >> 5;
    let num_bands = 3 * (num_decomp as usize) + 1;
    let bands = match qntsty {
        0 => {
            if bytes.len() < 1 + num_bands {
                return Err(Error::invalid("jpeg2000: QCD reversible short"));
            }
            (0..num_bands)
                .map(|i| ((bytes[1 + i] >> 3) & 0x1F, 0u16))
                .collect()
        }
        1 => {
            if bytes.len() < 3 {
                return Err(Error::invalid("jpeg2000: QCD derived short"));
            }
            let v = u16::from_be_bytes([bytes[1], bytes[2]]);
            let exp = (v >> 11) as u8;
            let mant = v & 0x7FF;
            vec![(exp, mant); num_bands]
        }
        2 => {
            if bytes.len() < 1 + 2 * num_bands {
                return Err(Error::invalid("jpeg2000: QCD expounded short"));
            }
            (0..num_bands)
                .map(|i| {
                    let v = u16::from_be_bytes([bytes[1 + 2 * i], bytes[1 + 2 * i + 1]]);
                    ((v >> 11) as u8, v & 0x7FF)
                })
                .collect()
        }
        _ => return Err(Error::invalid("jpeg2000: QCD Sqcd reserved")),
    };
    Ok(QcdParams {
        guard_bits,
        bands,
        is_reversible: qntsty == 0,
    })
}

/// One sub-band descriptor.
#[derive(Clone, Copy, Debug)]
pub struct SubbandInfo {
    pub orient: Orient,
    /// Band identifier inside its resolution: 0=LL, 1=HL, 2=LH, 3=HH.
    pub band_kind: u8,
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub resno: u8,
    /// QCD band index — 0 = LL, then HL/LH/HH per resolution in order.
    pub band_idx: usize,
}

fn div_ceil(a: u32, b: u32) -> u32 {
    if b == 0 {
        return 0;
    }
    a.div_ceil(b)
}

#[derive(Clone, Default)]
pub struct CblkState {
    pub included: bool,
    pub total_passes: u32,
    pub lblock: u32,
    pub data: Vec<u8>,
    pub missing_msb: u32,
    /// HTJ2K-only auxiliary buffer for the HT refinement segment
    /// (`Dref`, length `Lref`) when a packet contains both an HT
    /// cleanup pass *and* SigProp/MagRef passes (Z_blk in {2, 3}).
    /// Per ISO/IEC 15444-15 §B.3 each such packet emits TWO codeword
    /// segments: the cleanup terminating at pass index 0 (which is in
    /// the T set), and the refinement terminating at the last included
    /// pass. The classic Part-1 walker leaves this empty.
    pub data_ref: Vec<u8>,
}

/// Per-(precinct, sub-band) tier-2 decoder state. Holds the inclusion
/// and zero-bit-plane tag trees for the code-blocks of one sub-band
/// that fall inside a given precinct, plus the precinct's local
/// position in the sub-band's global code-block grid.
pub struct PrecinctSubband {
    pub inclusion: TagTree,
    pub zero_bitplanes: TagTree,
    /// Top-left code-block index of this precinct in the sub-band's
    /// global cblk grid (`[0..cblks_w[sb]) × [0..cblks_h[sb]))`).
    pub cx0: usize,
    pub cy0: usize,
    /// Local code-block grid dimensions inside this precinct.
    pub pcw: usize,
    pub pch: usize,
}

impl PrecinctSubband {
    fn new(cx0: usize, cy0: usize, pcw: usize, pch: usize) -> Self {
        // Tag trees must have at least one leaf to keep `decode` honest
        // even for empty precincts (§B.6 — every precinct emits a packet
        // header even when the precinct contains no code-blocks).
        let tw = pcw.max(1);
        let th = pch.max(1);
        PrecinctSubband {
            inclusion: TagTree::new(tw, th),
            zero_bitplanes: TagTree::new(tw, th),
            cx0,
            cy0,
            pcw,
            pch,
        }
    }
}

/// One precinct (T.800 §B.6). Holds per-sub-band tag-tree state and
/// the precinct's reference-grid origin (used by RPCL/PCRL/CPRL).
pub struct Precinct {
    /// One slot per sub-band in this resolution (1 for `r = 0`,
    /// 3 for `r > 0`, in HL/LH/HH order).
    pub sb_states: Vec<PrecinctSubband>,
    /// Reference-grid coordinates of the precinct's notional top-left
    /// (per the spec the partition is anchored at LL_r (0,0); we map
    /// back through the resolution scale and component sub-sampling to
    /// reference-grid coordinates so the position-driven progression
    /// orders can sort precincts in the spec's `(y, x)` walk order).
    pub ref_x: u32,
    pub ref_y: u32,
}

pub struct ResolutionLayout {
    pub resno: u8,
    pub subbands: Vec<SubbandInfo>,
    /// Precincts in raster order (`px + py * nprec_w`).
    pub precincts: Vec<Precinct>,
    pub nprec_w: usize,
    pub nprec_h: usize,
    /// Per-sub-band global code-block grid + persistent decoder state.
    /// All four arrays are indexed by sub-band slot in `subbands`.
    pub cblks_w: Vec<usize>,
    pub cblks_h: Vec<usize>,
    pub cblk_rects: Vec<Vec<(u32, u32, u32, u32)>>,
    pub cblk_states: Vec<Vec<CblkState>>,
    /// Precinct width/height exponents (§A.6.1 Table A.21).
    pub ppx: u8,
    pub ppy: u8,
    /// Effective code-block log2 dimensions after the §B.7 clamp
    /// (`min(xcb, PPx [- 1])` / same for `ycb`).
    pub xcb_eff: u8,
    pub ycb_eff: u8,
}

/// Build sub-band layouts following ISO 15444-1 §F.2 / §B.4.
///
/// For a tile region `(tx0, ty0, tx1, ty1)` in component coordinates
/// and `num_decomp` decomposition levels (resolution count =
/// `num_decomp + 1`):
///
/// - Resolution 0: one LL sub-band, covering
///   `ceil(tx/2^L) × ceil(ty/2^L)`.
/// - Resolution `r` in `1..=L`: three sub-bands HL, LH, HH at level
///   `r`. Each sub-band lives on the downsampled grid at scale
///   `2^(L-r+1)` before the lifting shift. HL takes `x` shifted back
///   by `2^(L-r)`; LH takes `y` shifted back; HH takes both.
pub fn build_subbands(tx0: u32, ty0: u32, tx1: u32, ty1: u32, num_decomp: u8) -> Vec<SubbandInfo> {
    let mut out = Vec::new();
    let ll_div: u32 = 1 << num_decomp;
    let ll_x0 = div_ceil(tx0, ll_div);
    let ll_y0 = div_ceil(ty0, ll_div);
    let ll_x1 = div_ceil(tx1, ll_div);
    let ll_y1 = div_ceil(ty1, ll_div);
    out.push(SubbandInfo {
        orient: Orient::Ll,
        band_kind: 0,
        x0: ll_x0,
        y0: ll_y0,
        x1: ll_x1,
        y1: ll_y1,
        resno: 0,
        band_idx: 0,
    });
    let mut band_idx = 1usize;
    for resno in 1..=num_decomp as u32 {
        // Divisor at this resolution for this level's sub-bands. One
        // lifting step undoes the factor of two — the sub-bands HL/LH/HH
        // are sampled on the 2^(L - r + 1)-subgrid.
        let lvl = num_decomp as u32 - resno + 1;
        let div = 1u32 << lvl;
        let shift = div >> 1;
        // HL
        out.push(SubbandInfo {
            orient: Orient::Hl,
            band_kind: 1,
            x0: div_ceil(tx0.saturating_sub(shift), div),
            y0: div_ceil(ty0, div),
            x1: div_ceil(tx1.saturating_sub(shift), div),
            y1: div_ceil(ty1, div),
            resno: resno as u8,
            band_idx,
        });
        band_idx += 1;
        // LH (uses LL's context table)
        out.push(SubbandInfo {
            orient: Orient::Ll,
            band_kind: 2,
            x0: div_ceil(tx0, div),
            y0: div_ceil(ty0.saturating_sub(shift), div),
            x1: div_ceil(tx1, div),
            y1: div_ceil(ty1.saturating_sub(shift), div),
            resno: resno as u8,
            band_idx,
        });
        band_idx += 1;
        // HH
        out.push(SubbandInfo {
            orient: Orient::Hh,
            band_kind: 3,
            x0: div_ceil(tx0.saturating_sub(shift), div),
            y0: div_ceil(ty0.saturating_sub(shift), div),
            x1: div_ceil(tx1.saturating_sub(shift), div),
            y1: div_ceil(ty1.saturating_sub(shift), div),
            resno: resno as u8,
            band_idx,
        });
        band_idx += 1;
    }
    out
}

/// Build per-resolution layouts for one tile-component, including the
/// precinct partition (§B.6), the code-block partition inside each
/// sub-band (§B.7), and the `(precinct, sub-band) → cblk-range`
/// mapping that the tier-2 walker needs.
///
/// `tile_comp_bounds = (tcx0, tcy0, tcx1, tcy1)` — the tile-component
/// rectangle in component coordinates. We derive LL_r bounds at each
/// resolution as `ceil(tc_/2^(NL - r))` (T.800 §F.4).
///
/// `xrsiz`/`yrsiz` come from SIZ. They are used only to map precinct
/// LL_r coordinates back to reference-grid coordinates for the
/// position-driven progression orders.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_resolutions(
    subbands: Vec<SubbandInfo>,
    num_decomp: u8,
    cblk_w_log2: u8,
    cblk_h_log2: u8,
    precincts_pp: &[(u8, u8)],
    tile_comp_bounds: (u32, u32, u32, u32),
    xrsiz: u32,
    yrsiz: u32,
) -> Vec<ResolutionLayout> {
    let num_res = (num_decomp as usize) + 1;
    let mut per_res: Vec<Vec<SubbandInfo>> = vec![Vec::new(); num_res];
    for sb in subbands {
        per_res[sb.resno as usize].push(sb);
    }
    per_res
        .into_iter()
        .enumerate()
        .map(|(resno, subs)| {
            let (ppx, ppy) = precincts_pp.get(resno).copied().unwrap_or((15, 15));
            // Effective code-block size per §B.7. For r > 0 the cblk
            // extent inside the precinct partition is bounded by
            // `2^(PPx - 1) × 2^(PPy - 1)`; for r = 0 it is the full
            // `2^PPx × 2^PPy`.
            let pp_cblk_x = if resno == 0 {
                ppx
            } else {
                ppx.saturating_sub(1)
            };
            let pp_cblk_y = if resno == 0 {
                ppy
            } else {
                ppy.saturating_sub(1)
            };
            let xcb_eff = cblk_w_log2.min(pp_cblk_x);
            let ycb_eff = cblk_h_log2.min(pp_cblk_y);
            let cw = 1u32 << xcb_eff;
            let ch = 1u32 << ycb_eff;

            // §B.6 precinct count for this resolution. The partition
            // is anchored at (0,0) of the LL_r grid, so the spec's
            // formula collapses to ceil(trx1/2^PPx) - floor(trx0/2^PPx)
            // (Eq B-16). LL_r bounds come from the tile-component
            // bounds scaled by `2^(NL - r)` per §F.4.
            let (tcx0, tcy0, tcx1, tcy1) = tile_comp_bounds;
            let down = 1u32 << (num_decomp as u32 - resno as u32);
            let trx0 = div_ceil(tcx0, down);
            let try0 = div_ceil(tcy0, down);
            let trx1 = div_ceil(tcx1, down);
            let try1 = div_ceil(tcy1, down);
            let pp_cell_x = 1u32 << ppx;
            let pp_cell_y = 1u32 << ppy;
            let px_lo = trx0 / pp_cell_x;
            let px_hi = div_ceil(trx1, pp_cell_x);
            let py_lo = try0 / pp_cell_y;
            let py_hi = div_ceil(try1, pp_cell_y);
            let nprec_w = (px_hi - px_lo) as usize;
            let nprec_h = (py_hi - py_lo) as usize;
            let nprec_w_eff = nprec_w.max(1);
            let nprec_h_eff = nprec_h.max(1);

            // Per-sub-band global code-block grid.
            let mut cblks_w_v: Vec<usize> = Vec::with_capacity(subs.len());
            let mut cblks_h_v: Vec<usize> = Vec::with_capacity(subs.len());
            let mut cblk_rects: Vec<Vec<(u32, u32, u32, u32)>> = Vec::with_capacity(subs.len());
            let mut cblk_states: Vec<Vec<CblkState>> = Vec::with_capacity(subs.len());
            for sb in &subs {
                let band_w = sb.x1.saturating_sub(sb.x0);
                let band_h = sb.y1.saturating_sub(sb.y0);
                let cblks_w = div_ceil(band_w, cw) as usize;
                let cblks_h = div_ceil(band_h, ch) as usize;
                let mut rects = Vec::with_capacity(cblks_w * cblks_h);
                for cy in 0..cblks_h {
                    for cx in 0..cblks_w {
                        let x0 = sb.x0 + cx as u32 * cw;
                        let y0 = sb.y0 + cy as u32 * ch;
                        let x1 = (x0 + cw).min(sb.x1);
                        let y1 = (y0 + ch).min(sb.y1);
                        rects.push((x0, y0, x1, y1));
                    }
                }
                cblk_rects.push(rects);
                cblk_states.push(vec![
                    CblkState {
                        lblock: 3,
                        ..Default::default()
                    };
                    cblks_w * cblks_h
                ]);
                cblks_w_v.push(cblks_w);
                cblks_h_v.push(cblks_h);
            }

            // Build precinct list (raster order). For each precinct,
            // compute the per-sub-band code-block range that falls
            // inside the precinct's footprint (§B.6 / §B.7). For r > 0
            // the sub-band coords run at half the LL_r scale, so the
            // precinct cell is `2^(PPx-1) × 2^(PPy-1)` in sub-band coords.
            let mut precincts: Vec<Precinct> = Vec::with_capacity(nprec_w_eff * nprec_h_eff);
            for py in 0..nprec_h_eff {
                for px in 0..nprec_w_eff {
                    let abs_px = (px_lo as usize + px) as u32;
                    let abs_py = (py_lo as usize + py) as u32;
                    // Precinct extent in LL_r coords.
                    let p_ll_x0 = abs_px * pp_cell_x;
                    let p_ll_y0 = abs_py * pp_cell_y;
                    let p_ll_x1 = p_ll_x0 + pp_cell_x;
                    let p_ll_y1 = p_ll_y0 + pp_cell_y;

                    let mut sb_states = Vec::with_capacity(subs.len());
                    for (sb_idx, sb) in subs.iter().enumerate() {
                        // Map precinct extent into sub-band coords.
                        // For r = 0 (LL only) the sub-band IS the LL_r
                        // grid. For r > 0 we divide by 2.
                        let (sx0_p, sy0_p, sx1_p, sy1_p) = if resno == 0 {
                            (p_ll_x0, p_ll_y0, p_ll_x1, p_ll_y1)
                        } else {
                            (p_ll_x0 / 2, p_ll_y0 / 2, p_ll_x1 / 2, p_ll_y1 / 2)
                        };
                        // Clip to sub-band's own extent.
                        let sx0 = sx0_p.max(sb.x0);
                        let sy0 = sy0_p.max(sb.y0);
                        let sx1 = sx1_p.min(sb.x1);
                        let sy1 = sy1_p.min(sb.y1);
                        // Code-block index range. Cblks are anchored
                        // at sub-band (0, 0), so we floor-divide the
                        // intersection by the cblk size and round up
                        // the upper edge. Then translate by the
                        // sub-band's own origin so the indices are
                        // local to the sub-band's cblk array.
                        let (cx0, cy0, pcw, pch) = if sx1 <= sx0 || sy1 <= sy0 {
                            (0, 0, 0, 0)
                        } else {
                            let cx_lo = (sx0 / cw) as usize;
                            let cx_hi = div_ceil(sx1, cw) as usize;
                            let cy_lo = (sy0 / ch) as usize;
                            let cy_hi = div_ceil(sy1, ch) as usize;
                            // Translate to sub-band-local cblk index.
                            let sb_cx_lo = (sb.x0 / cw) as usize;
                            let sb_cy_lo = (sb.y0 / ch) as usize;
                            (
                                cx_lo.saturating_sub(sb_cx_lo),
                                cy_lo.saturating_sub(sb_cy_lo),
                                cx_hi.saturating_sub(cx_lo),
                                cy_hi.saturating_sub(cy_lo),
                            )
                        };
                        // Clamp to sub-band's global cblk grid bounds.
                        let pcw = pcw.min(cblks_w_v[sb_idx].saturating_sub(cx0));
                        let pch = pch.min(cblks_h_v[sb_idx].saturating_sub(cy0));
                        sb_states.push(PrecinctSubband::new(cx0, cy0, pcw, pch));
                    }

                    // Reference-grid origin of this precinct, used by
                    // the position-driven progression orders. For the
                    // LL_r → reference-grid map (§B.5) we scale by
                    // `2^(NL - r)` and the component sub-sampling.
                    let nl = num_decomp as u32;
                    let r = resno as u32;
                    let scale = 1u32 << (nl - r);
                    let ref_x = p_ll_x0.saturating_mul(scale).saturating_mul(xrsiz);
                    let ref_y = p_ll_y0.saturating_mul(scale).saturating_mul(yrsiz);

                    precincts.push(Precinct {
                        sb_states,
                        ref_x,
                        ref_y,
                    });
                }
            }

            ResolutionLayout {
                resno: resno as u8,
                subbands: subs,
                precincts,
                nprec_w: nprec_w_eff,
                nprec_h: nprec_h_eff,
                cblks_w: cblks_w_v,
                cblks_h: cblks_h_v,
                cblk_rects,
                cblk_states,
                ppx,
                ppy,
                xcb_eff,
                ycb_eff,
            }
        })
        .collect()
}

/// Per-component decoded sample bit-depth hint used to compute the 9/7
/// sub-band step size. `None` means "use the guard-bits-based heuristic"
/// — fine for 8-bit components but deeper ones will skew.
pub struct DecodeParams<'a> {
    pub comp_precisions: &'a [u32],
    /// Optional per-tile progression-order override (T.800 §A.6.6 /
    /// §B.12.3). When `Some`, the tier-2 walker iterates each
    /// progression-order volume in sequence; when `None`, it uses the
    /// single progression order from `cod`.
    pub poc: Option<&'a PocParams>,
    /// Optional packed packet headers for this tile (T.800 §A.7.4 PPM /
    /// §A.7.5 PPT). When `Some`, the tier-2 walker reads packet header
    /// bytes from this slice and packet bodies from `body`. When
    /// `None`, both come from `body` (the historical layout).
    pub packet_headers: Option<&'a [u8]>,
    /// Per-component Maxshift ROI scaling value `s` from any in-scope
    /// `RGN` marker (T.800 §A.6.3 + §H.1). `0` means "no ROI for this
    /// component" — the synthesis runs unchanged. A non-zero entry
    /// causes (a) `band_numbps` to be bumped by `s` so tier-1 decodes
    /// the extra ROI bit-planes, and (b) a post-T1 threshold-shift:
    /// any reconstructed magnitude `>= 2^Mb` is identified as an ROI
    /// coefficient and divided by `2^s`. Slice length should equal
    /// `comp_precisions.len()`; missing entries default to 0.
    pub roi_shifts: &'a [u8],
}

#[allow(clippy::needless_range_loop)]
pub fn decode_tile(
    body: &[u8],
    comp_sizes: &[(u32, u32, u32, u32)],
    cod: &CodParams,
    qcd: &QcdParams,
) -> Result<Vec<Vec<i32>>> {
    // Legacy signature used by tests: assume 8-bit components.
    let precisions: Vec<u32> = vec![8u32; comp_sizes.len()];
    let roi_shifts: Vec<u8> = vec![0u8; comp_sizes.len()];
    let params = DecodeParams {
        comp_precisions: &precisions,
        poc: None,
        packet_headers: None,
        roi_shifts: &roi_shifts,
    };
    decode_tile_with_params(body, comp_sizes, cod, qcd, &params)
}

#[allow(clippy::needless_range_loop)]
pub fn decode_tile_with_params(
    body: &[u8],
    comp_sizes: &[(u32, u32, u32, u32)],
    cod: &CodParams,
    qcd: &QcdParams,
    params: &DecodeParams<'_>,
) -> Result<Vec<Vec<i32>>> {
    if cod.num_layers == 0 {
        return Err(Error::invalid(
            "jpeg2000: COD signals zero quality layers (must be >= 1)",
        ));
    }
    if cod.transform > 1 {
        return Err(Error::unsupported(
            "jpeg2000: unknown transform id (must be 0 or 1)",
        ));
    }
    if !matches!(cod.progression_order, 0..=4) {
        return Err(Error::unsupported(
            "jpeg2000: progression order must be 0..=4 (LRCP/RLCP/RPCL/PCRL/CPRL)",
        ));
    }

    let num_comps = comp_sizes.len();
    let num_res = (cod.num_decomp as usize) + 1;
    let mut layouts: Vec<Vec<ResolutionLayout>> = Vec::with_capacity(num_comps);
    for &(x0, y0, x1, y1) in comp_sizes {
        let subbands = build_subbands(x0, y0, x1, y1, cod.num_decomp);
        layouts.push(build_resolutions(
            subbands,
            cod.num_decomp,
            cod.cblk_w_log2,
            cod.cblk_h_log2,
            &cod.precincts,
            (x0, y0, x1, y1),
            // Component sub-sampling is already folded into `comp_sizes`
            // by the caller, so component coords here are 1:1 with the
            // reference grid for this tile-component. See `decode_frame`.
            1,
            1,
        ));
    }

    let mut cursor = Cursor::new(body);
    walk_packets(
        &mut cursor,
        cod,
        params.poc,
        params.packet_headers,
        &mut layouts,
        num_res,
        num_comps,
    )?;

    // Per-component IDWT using per-subband buffers.
    let mut out = Vec::with_capacity(num_comps);
    for (comp_idx, &(cx0, cy0, cx1, cy1)) in comp_sizes.iter().enumerate() {
        let comp_w = (cx1 - cx0) as usize;
        let comp_h = (cy1 - cy0) as usize;
        if comp_w == 0 || comp_h == 0 {
            out.push(Vec::new());
            continue;
        }
        let roi_shift = params.roi_shifts.get(comp_idx).copied().unwrap_or(0);
        if cod.transform == 1 {
            out.push(synth_component_53(
                &layouts[comp_idx],
                num_res,
                comp_w,
                comp_h,
                cod,
                qcd,
                roi_shift,
            )?);
        } else {
            // 9/7 irreversible float path.
            let prec = params.comp_precisions.get(comp_idx).copied().unwrap_or(8);
            out.push(synth_component_97(
                &layouts[comp_idx],
                num_res,
                comp_w,
                comp_h,
                cod,
                qcd,
                prec,
                roi_shift,
            )?);
        }
    }

    Ok(out)
}

/// Decode + synthesise one component using the 5/3 reversible integer
/// wavelet.
#[allow(clippy::needless_range_loop)]
fn synth_component_53(
    layouts: &[ResolutionLayout],
    num_res: usize,
    comp_w: usize,
    comp_h: usize,
    cod: &CodParams,
    qcd: &QcdParams,
    roi_shift: u8,
) -> Result<Vec<i32>> {
    // Decode every sub-band's code-blocks into its own buffer.
    let mut band_bufs: Vec<Vec<i32>> = Vec::with_capacity(num_res * 3 + 1);
    for resno in 0..num_res {
        let layout = &layouts[resno];
        for (sb_idx, sb) in layout.subbands.iter().enumerate() {
            let bw = (sb.x1 - sb.x0) as usize;
            let bh = (sb.y1 - sb.y0) as usize;
            let mut buf = vec![0i32; bw * bh];
            let cblks = &layout.cblk_states[sb_idx];
            let cblks_w = layout.cblks_w[sb_idx];
            let cblks_h = layout.cblks_h[sb_idx];
            for cy in 0..cblks_h {
                for cx in 0..cblks_w {
                    let idx = cy * cblks_w + cx;
                    let st = &cblks[idx];
                    if !st.included || st.total_passes == 0 {
                        continue;
                    }
                    let (bx0, by0, bx1, by1) = layout.cblk_rects[sb_idx][idx];
                    let w = (bx1 - bx0) as usize;
                    let h = (by1 - by0) as usize;
                    let (eps, _mant) = qcd.bands[sb.band_idx];
                    // Per T.800 §A.6.3 + §H.1 the RGN Maxshift `s`
                    // extends the bit-plane budget by `s` so the
                    // encoder can place upshifted coefficients above
                    // the natural background range. The encoder
                    // signals the per-codeblock effective shift via
                    // `missing_msb`: codeblocks that exercise the
                    // extra `s` planes have `missing_msb < s` and
                    // need a `>> s` correction; codeblocks confined
                    // to the bottom Mb planes (`missing_msb >= s`)
                    // were never upshifted and decode normally.
                    let bg_mb = qcd.guard_bits as i32 + eps as i32 - 1;
                    let (extra_planes, post_shift) =
                        if roi_shift > 0 && st.missing_msb < roi_shift as u32 {
                            (roi_shift as i32, roi_shift as u32)
                        } else {
                            (0, 0)
                        };
                    let band_numbps = bg_mb + extra_planes;
                    let bpno = band_numbps + 1 - st.missing_msb as i32;
                    if bpno < 1 {
                        continue;
                    }
                    let decoded = t1::decode_cblk(
                        st.data.clone(),
                        w,
                        h,
                        bpno,
                        st.total_passes,
                        sb.orient,
                        cod.cblk_style,
                    );
                    let rel_x = (bx0 - sb.x0) as usize;
                    let rel_y = (by0 - sb.y0) as usize;
                    for ly in 0..h {
                        for lx in 0..w {
                            let v0 = decoded.data[ly * w + lx];
                            let v = if post_shift > 0 {
                                let sign = v0.signum();
                                sign * (v0.abs() >> post_shift)
                            } else {
                                v0
                            };
                            buf[(rel_y + ly) * bw + (rel_x + lx)] = v / 2;
                        }
                    }
                }
            }
            band_bufs.push(buf);
        }
    }

    // Synthesise upward: at each resolution r in 1..=num_decomp,
    // combine LL_{r-1} + HL_r + LH_r + HH_r → LL_r via IDWT-53.
    let mut ll = band_bufs[0].clone();
    let layout0 = &layouts[0];
    let (mut ll_w, mut ll_h) = (
        (layout0.subbands[0].x1 - layout0.subbands[0].x0) as usize,
        (layout0.subbands[0].y1 - layout0.subbands[0].y0) as usize,
    );

    for resno in 1..num_res {
        let layout = &layouts[resno];
        let hl = &band_bufs[1 + (resno - 1) * 3];
        let lh = &band_bufs[1 + (resno - 1) * 3 + 1];
        let hh = &band_bufs[1 + (resno - 1) * 3 + 2];
        let hl_sb = &layout.subbands[0];
        let lh_sb = &layout.subbands[1];
        let hh_sb = &layout.subbands[2];
        let hl_w = (hl_sb.x1 - hl_sb.x0) as usize;
        let hl_h = (hl_sb.y1 - hl_sb.y0) as usize;
        let lh_w = (lh_sb.x1 - lh_sb.x0) as usize;
        let lh_h = (lh_sb.y1 - lh_sb.y0) as usize;
        let hh_w = (hh_sb.x1 - hh_sb.x0) as usize;
        let hh_h = (hh_sb.y1 - hh_sb.y0) as usize;
        let canvas_w = ll_w + hl_w;
        let canvas_h = ll_h + lh_h;
        debug_assert_eq!(canvas_w, lh_w + hh_w);
        debug_assert_eq!(canvas_h, hl_h + hh_h);
        let mut canvas = vec![0i32; canvas_w * canvas_h];
        for y in 0..ll_h {
            for x in 0..ll_w {
                canvas[y * canvas_w + x] = ll[y * ll_w + x];
            }
        }
        for y in 0..hl_h {
            for x in 0..hl_w {
                canvas[y * canvas_w + (ll_w + x)] = hl[y * hl_w + x];
            }
        }
        for y in 0..lh_h {
            for x in 0..lh_w {
                canvas[(ll_h + y) * canvas_w + x] = lh[y * lh_w + x];
            }
        }
        for y in 0..hh_h {
            for x in 0..hh_w {
                canvas[(ll_h + y) * canvas_w + (ll_w + x)] = hh[y * hh_w + x];
            }
        }
        dwt::idwt_53(&mut canvas, canvas_w, canvas_h, canvas_w);
        ll = canvas;
        ll_w = canvas_w;
        ll_h = canvas_h;
    }

    debug_assert_eq!(ll.len(), comp_w * comp_h);
    Ok(ll)
}

/// Decode + synthesise one component using the 9/7 irreversible float
/// wavelet. Returns a flat `Vec<i32>` of component samples in the same
/// coordinate system as the 5/3 path, so `frame.rs` can apply DC level
/// shift / clipping uniformly.
///
/// Dequantisation (T.800 §E.1.1.2):
///
/// ```text
///   stepsize_b = (1 + mant/2^11) * 2^(Rb - eps)
///   sample_f   = sample_i * 0.5 * stepsize_b
/// ```
///
/// Following OpenJPEG's `BUG_WEIRD_TWO_INVK` convention: `Rb = prec`
/// (no per-band gain on decode). The gain is baked into the lifting
/// scale factors on the inverse transform.
#[allow(clippy::too_many_arguments, clippy::needless_range_loop)]
fn synth_component_97(
    layouts: &[ResolutionLayout],
    num_res: usize,
    comp_w: usize,
    comp_h: usize,
    cod: &CodParams,
    qcd: &QcdParams,
    precision: u32,
    roi_shift: u8,
) -> Result<Vec<i32>> {
    // Decode every sub-band's code-blocks into a float buffer with
    // dequantised samples.
    let mut band_bufs: Vec<Vec<f32>> = Vec::with_capacity(num_res * 3 + 1);
    for resno in 0..num_res {
        let layout = &layouts[resno];
        for (sb_idx, sb) in layout.subbands.iter().enumerate() {
            let bw = (sb.x1 - sb.x0) as usize;
            let bh = (sb.y1 - sb.y0) as usize;
            let mut buf = vec![0f32; bw * bh];
            let cblks = &layout.cblk_states[sb_idx];
            let cblks_w = layout.cblks_w[sb_idx];
            let cblks_h = layout.cblks_h[sb_idx];
            let (eps, mant) = qcd.bands[sb.band_idx];
            // Stepsize per T.800 Eq E-3. For the 9/7 decoder we match
            // OpenJPEG's `BUG_WEIRD_TWO_INVK` convention (see
            // `opj_tcd_init_tile`): `Rb = precision` (no `log2_gain_b`
            // factor). The `log2_gain_b` bits are recovered instead by
            // the IDWT's `K` / `2/K` scaling — see `idwt_97_1d`.
            let rb = precision as i32;
            let stepsize = (1.0f64 + (mant as f64) / 2048.0) * 2f64.powi(rb - eps as i32);
            // 0.5 factor matches OpenJPEG's `0.5f * band->stepsize`:
            // our tier-1 samples carry the `oneplushalf` magnitude which
            // bakes a factor of 2 into the value. Halving it undoes that.
            let scale = 0.5f64 * stepsize;
            for cy in 0..cblks_h {
                for cx in 0..cblks_w {
                    let idx = cy * cblks_w + cx;
                    let st = &cblks[idx];
                    if !st.included || st.total_passes == 0 {
                        continue;
                    }
                    let (bx0, by0, bx1, by1) = layout.cblk_rects[sb_idx][idx];
                    let w = (bx1 - bx0) as usize;
                    let h = (by1 - by0) as usize;
                    // RGN per-codeblock: if `missing_msb < s` the
                    // encoder reached the extra `s` bit-planes for
                    // this block and we must (a) extend the budget
                    // by `s` and (b) divide the decoded magnitude
                    // by `2^s` post-T1 (T.800 §A.6.3 + §H.1).
                    let bg_mb = qcd.guard_bits as i32 + eps as i32 - 1;
                    let (extra_planes, post_shift) =
                        if roi_shift > 0 && st.missing_msb < roi_shift as u32 {
                            (roi_shift as i32, roi_shift as u32)
                        } else {
                            (0, 0)
                        };
                    let band_numbps = bg_mb + extra_planes;
                    let bpno = band_numbps + 1 - st.missing_msb as i32;
                    if bpno < 1 {
                        continue;
                    }
                    let decoded = t1::decode_cblk(
                        st.data.clone(),
                        w,
                        h,
                        bpno,
                        st.total_passes,
                        sb.orient,
                        cod.cblk_style,
                    );
                    let rel_x = (bx0 - sb.x0) as usize;
                    let rel_y = (by0 - sb.y0) as usize;
                    for ly in 0..h {
                        for lx in 0..w {
                            let v0 = decoded.data[ly * w + lx];
                            let v = if post_shift > 0 {
                                let sign = v0.signum();
                                sign * (v0.abs() >> post_shift)
                            } else {
                                v0
                            };
                            buf[(rel_y + ly) * bw + (rel_x + lx)] = (v as f64 * scale) as f32;
                        }
                    }
                }
            }
            band_bufs.push(buf);
            let _ = bh;
        }
    }

    // Synthesise upward using the 9/7 float IDWT.
    let mut ll = band_bufs[0].clone();
    let layout0 = &layouts[0];
    let (mut ll_w, mut ll_h) = (
        (layout0.subbands[0].x1 - layout0.subbands[0].x0) as usize,
        (layout0.subbands[0].y1 - layout0.subbands[0].y0) as usize,
    );

    for resno in 1..num_res {
        let layout = &layouts[resno];
        let hl = &band_bufs[1 + (resno - 1) * 3];
        let lh = &band_bufs[1 + (resno - 1) * 3 + 1];
        let hh = &band_bufs[1 + (resno - 1) * 3 + 2];
        let hl_sb = &layout.subbands[0];
        let lh_sb = &layout.subbands[1];
        let hh_sb = &layout.subbands[2];
        let hl_w = (hl_sb.x1 - hl_sb.x0) as usize;
        let hl_h = (hl_sb.y1 - hl_sb.y0) as usize;
        let lh_w = (lh_sb.x1 - lh_sb.x0) as usize;
        let lh_h = (lh_sb.y1 - lh_sb.y0) as usize;
        let hh_w = (hh_sb.x1 - hh_sb.x0) as usize;
        let hh_h = (hh_sb.y1 - hh_sb.y0) as usize;
        let canvas_w = ll_w + hl_w;
        let canvas_h = ll_h + lh_h;
        debug_assert_eq!(canvas_w, lh_w + hh_w);
        debug_assert_eq!(canvas_h, hl_h + hh_h);
        let mut canvas = vec![0f32; canvas_w * canvas_h];
        for y in 0..ll_h {
            for x in 0..ll_w {
                canvas[y * canvas_w + x] = ll[y * ll_w + x];
            }
        }
        for y in 0..hl_h {
            for x in 0..hl_w {
                canvas[y * canvas_w + (ll_w + x)] = hl[y * hl_w + x];
            }
        }
        for y in 0..lh_h {
            for x in 0..lh_w {
                canvas[(ll_h + y) * canvas_w + x] = lh[y * lh_w + x];
            }
        }
        for y in 0..hh_h {
            for x in 0..hh_w {
                canvas[(ll_h + y) * canvas_w + (ll_w + x)] = hh[y * hh_w + x];
            }
        }
        dwt::idwt_97(&mut canvas, canvas_w, canvas_h, canvas_w);
        ll = canvas;
        ll_w = canvas_w;
        ll_h = canvas_h;
    }

    debug_assert_eq!(ll.len(), comp_w * comp_h);
    // Round floats to nearest integer for the downstream level-shift
    // pipeline.
    Ok(ll.into_iter().map(|v| v.round() as i32).collect())
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Cursor { buf, pos: 0 }
    }
    fn remaining(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
    fn consume(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(Error::invalid("jpeg2000: packet body past end"));
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }
}

/// Parse one packet header + body for a single (resolution, precinct,
/// layer) tuple. The packet covers all sub-bands of `res` whose
/// code-blocks fall inside the chosen precinct (§B.6 / §B.9 / §B.10).
///
/// `header_cur` is `Some` when the packet header bytes come from a
/// PPM (main header) or PPT (tile-part header) buffer per §A.7.4 /
/// §A.7.5; the packet body is still read from `cur` (the SOD body).
/// When `header_cur` is `None` both header and body come from `cur`.
fn parse_precinct_packet(
    cur: &mut Cursor<'_>,
    header_cur: Option<&mut Cursor<'_>>,
    layer: u32,
    res: &mut ResolutionLayout,
    prec_idx: usize,
    cod: &CodParams,
) -> Result<()> {
    // SOP markers (§A.8.1) are only emitted in the body stream; with
    // PPM/PPT they still appear in the body before the (now empty)
    // packet header, between header bytes and body — so we skip SOP
    // from the body cursor regardless of where headers come from.
    if cod.sop_marker && cur.remaining().starts_with(&[0xFF, 0x91]) {
        if cur.remaining().len() < 6 {
            return Err(Error::invalid("jpeg2000: truncated SOP"));
        }
        cur.consume(6)?;
    }

    // The header source: an external PPM/PPT cursor if supplied, else
    // the body cursor itself. We always operate on a `Bio` reading the
    // raw header byte slice — `numbytes_read` then tells us how many
    // bytes the header consumed so we can advance the underlying
    // source cursor after the bit-aligned read.
    let (header_slice, from_external) = if let Some(c) = &header_cur {
        (c.remaining(), true)
    } else {
        (cur.remaining(), false)
    };
    let mut bio = Bio::new(header_slice);
    // (sb_idx, global_cblk_idx, length).
    let mut pending: Vec<(usize, usize, u32)> = Vec::new();

    if bio.read_bit() == 0 {
        bio.inalign();
    } else {
        for sb_idx in 0..res.subbands.len() {
            let cblks_w_g = res.cblks_w[sb_idx];
            let prec = &mut res.precincts[prec_idx].sb_states[sb_idx];
            let pcw = prec.pcw;
            let pch = prec.pch;
            let base_cx = prec.cx0;
            let base_cy = prec.cy0;
            // Empty (sub-band, precinct) intersection: nothing to emit.
            // Per §B.6 every precinct still emits a packet header; the
            // tag-tree just isn't queried for sub-bands with no cblks.
            if pcw == 0 || pch == 0 {
                continue;
            }
            for lcy in 0..pch {
                for lcx in 0..pcw {
                    let g_cx = base_cx + lcx;
                    let g_cy = base_cy + lcy;
                    let g_idx = g_cy * cblks_w_g + g_cx;
                    let included_now;
                    let missing_msb;
                    if !res.cblk_states[sb_idx][g_idx].included {
                        included_now = prec.inclusion.decode(lcx, lcy, layer + 1, &mut bio);
                        if !included_now {
                            continue;
                        }
                        // Missing-MSB (zero bitplanes) tag tree.
                        let mut i = 0u32;
                        loop {
                            if prec.zero_bitplanes.decode(lcx, lcy, i, &mut bio) {
                                break;
                            }
                            i += 1;
                            if i > 64 {
                                return Err(Error::invalid(
                                    "jpeg2000: missing-MSB tag tree runaway",
                                ));
                            }
                        }
                        missing_msb = i;
                    } else {
                        included_now = bio.read_bit() != 0;
                        if !included_now {
                            continue;
                        }
                        missing_msb = res.cblk_states[sb_idx][g_idx].missing_msb;
                    }
                    let num_passes = read_num_passes(&mut bio);
                    while bio.read_bit() == 1 {
                        res.cblk_states[sb_idx][g_idx].lblock += 1;
                    }
                    let len_bits = res.cblk_states[sb_idx][g_idx].lblock + ilog2(num_passes);
                    let length = bio.read(len_bits);
                    let st = &mut res.cblk_states[sb_idx][g_idx];
                    st.included = true;
                    st.total_passes += num_passes;
                    st.missing_msb = missing_msb;
                    pending.push((sb_idx, g_idx, length));
                }
            }
        }
        bio.inalign();
    }
    let header_bytes_used = bio.numbytes_read();
    if from_external {
        // Advance the PPM/PPT cursor; do not touch the body cursor for
        // header bytes. EPH markers (when emitted) are still part of
        // the PPM/PPT stream per §A.8.2, so we consume them from the
        // header source as well.
        let header_cur = header_cur.expect("from_external implies header_cur is Some");
        header_cur.consume(header_bytes_used)?;
        if cod.eph_marker && header_cur.remaining().starts_with(&[0xFF, 0x92]) {
            header_cur.consume(2)?;
        }
    } else {
        cur.consume(header_bytes_used)?;
        if cod.eph_marker && cur.remaining().starts_with(&[0xFF, 0x92]) {
            cur.consume(2)?;
        }
    }
    for (sb_idx, g_idx, length) in pending {
        let bytes = cur.consume(length as usize)?.to_vec();
        res.cblk_states[sb_idx][g_idx]
            .data
            .extend_from_slice(&bytes);
    }
    Ok(())
}

/// Walk the tier-2 packet stream in the order signalled by `cod.progression_order`,
/// optionally overridden by a POC marker (T.800 §A.6.6 / §B.12.2 / §B.12.3).
///
/// When `poc` is `None` the walker emits one progression covering the
/// full `(comp, res, layer)` cube with `cod.progression_order`. When
/// `poc` is `Some` each progression-order volume is processed in turn,
/// with each `(comp, res, prec)` tuple's per-progression layer counter
/// advancing across volumes (the spec rule "the layer always starts
/// with the next one for a given tile-component, resolution level and
/// precinct"). Packets that have already been emitted are skipped.
///
/// `packet_headers` is `Some` when the tile uses PPM/PPT (T.800 §A.7.4
/// / §A.7.5): the bytes contain every packet header for this tile in
/// progression order (the same order the walker visits them). The
/// packet bodies still come from `cur`.
#[allow(clippy::needless_range_loop)]
fn walk_packets(
    cur: &mut Cursor<'_>,
    cod: &CodParams,
    poc: Option<&PocParams>,
    packet_headers: Option<&[u8]>,
    layouts: &mut [Vec<ResolutionLayout>],
    num_res: usize,
    num_comps: usize,
) -> Result<()> {
    let num_layers = cod.num_layers as u32;
    let progressions: Vec<PocProgression> = if let Some(poc) = poc {
        poc.progressions.clone()
    } else {
        vec![PocProgression {
            res_start: 0,
            comp_start: 0,
            layer_end: cod.num_layers,
            res_end: num_res as u8,
            comp_end: num_comps as u16,
            progression: cod.progression_order,
        }]
    };

    // Local owned cursor over the packed packet headers (PPM/PPT). We
    // build it as a `Box`-owned cursor so we can pass `&mut` references
    // into `parse_precinct_packet` without lifetime gymnastics across
    // the dispatch loop.
    let mut header_cursor: Option<Cursor<'_>> = packet_headers.map(Cursor::new);

    // Helper to invoke `parse_precinct_packet` with the right header
    // source. Inlined as a closure-like macro because Rust closures
    // can't easily borrow disjoint slots from `layouts`.
    macro_rules! emit_packet {
        ($cur:expr, $hdr:expr, $layer:expr, $layout:expr, $prec:expr, $cod:expr) => {{
            let hdr_ref: Option<&mut Cursor<'_>> = $hdr.as_mut();
            parse_precinct_packet($cur, hdr_ref, $layer, $layout, $prec, $cod)?
        }};
    }

    // Per-(comp, res, prec) "next layer to emit" counter (§B.12.2).
    // Indexed `[comp][res][prec_idx]`. Initialised to 0; each emitted
    // packet for tuple `(c, r, k)` bumps the counter by 1.
    let mut next_layer: Vec<Vec<Vec<u32>>> = (0..num_comps)
        .map(|c| {
            (0..num_res)
                .map(|r| vec![0u32; layouts[c][r].precincts.len()])
                .collect()
        })
        .collect();

    for prog in &progressions {
        // Clamp progression bounds to actual tile geometry — POC volumes
        // are allowed to over-specify (e.g. CEpoc of 256 for a 3-comp
        // image; REpoc beyond actual resolutions). We silently clip.
        let comp_lo = (prog.comp_start as usize).min(num_comps);
        let comp_hi = (prog.comp_end as usize).min(num_comps);
        let res_lo = (prog.res_start as usize).min(num_res);
        let res_hi = (prog.res_end as usize).min(num_res);
        let layer_hi = (prog.layer_end as u32).min(num_layers);
        if comp_lo >= comp_hi || res_lo >= res_hi || layer_hi == 0 {
            continue;
        }
        match prog.progression {
            // LRCP — §B.12.1.1.
            0 => {
                for layer in 0..layer_hi {
                    for resno in res_lo..res_hi {
                        for comp in comp_lo..comp_hi {
                            let nprec = layouts[comp][resno].precincts.len();
                            for prec in 0..nprec {
                                if next_layer[comp][resno][prec] != layer {
                                    continue;
                                }
                                emit_packet!(
                                    cur,
                                    header_cursor,
                                    layer,
                                    &mut layouts[comp][resno],
                                    prec,
                                    cod
                                );
                                next_layer[comp][resno][prec] = layer + 1;
                            }
                        }
                    }
                }
            }
            // RLCP — §B.12.1.2.
            1 => {
                for resno in res_lo..res_hi {
                    for layer in 0..layer_hi {
                        for comp in comp_lo..comp_hi {
                            let nprec = layouts[comp][resno].precincts.len();
                            for prec in 0..nprec {
                                if next_layer[comp][resno][prec] != layer {
                                    continue;
                                }
                                emit_packet!(
                                    cur,
                                    header_cursor,
                                    layer,
                                    &mut layouts[comp][resno],
                                    prec,
                                    cod
                                );
                                next_layer[comp][resno][prec] = layer + 1;
                            }
                        }
                    }
                }
            }
            // RPCL — §B.12.1.3. Per-resolution (y, x) walk over the
            // reference grid; the spec note (§B.12.1.3 "NOTE") says we
            // may iterate precincts directly.
            2 => {
                for resno in res_lo..res_hi {
                    let mut order: Vec<(u32, u32, usize, usize)> = Vec::new();
                    for comp in comp_lo..comp_hi {
                        let layout = &layouts[comp][resno];
                        for (prec_idx, prec) in layout.precincts.iter().enumerate() {
                            order.push((prec.ref_y, prec.ref_x, comp, prec_idx));
                        }
                    }
                    order.sort();
                    for (_, _, comp, prec_idx) in order {
                        for layer in 0..layer_hi {
                            if next_layer[comp][resno][prec_idx] != layer {
                                continue;
                            }
                            emit_packet!(
                                cur,
                                header_cursor,
                                layer,
                                &mut layouts[comp][resno],
                                prec_idx,
                                cod
                            );
                            next_layer[comp][resno][prec_idx] = layer + 1;
                        }
                    }
                }
            }
            // PCRL — §B.12.1.4. Outer (y, x) over the ref grid; inner
            // component, resolution, layer.
            3 => {
                let mut order: Vec<(u32, u32, usize, usize, usize)> = Vec::new();
                for comp in comp_lo..comp_hi {
                    for resno in res_lo..res_hi {
                        let layout = &layouts[comp][resno];
                        for (prec_idx, prec) in layout.precincts.iter().enumerate() {
                            order.push((prec.ref_y, prec.ref_x, comp, resno, prec_idx));
                        }
                    }
                }
                order.sort();
                for (_, _, comp, resno, prec_idx) in order {
                    for layer in 0..layer_hi {
                        if next_layer[comp][resno][prec_idx] != layer {
                            continue;
                        }
                        emit_packet!(
                            cur,
                            header_cursor,
                            layer,
                            &mut layouts[comp][resno],
                            prec_idx,
                            cod
                        );
                        next_layer[comp][resno][prec_idx] = layer + 1;
                    }
                }
            }
            // CPRL — §B.12.1.5. Outer component, then (y, x), then r, then layer.
            4 => {
                for comp in comp_lo..comp_hi {
                    let mut order: Vec<(u32, u32, usize, usize)> = Vec::new();
                    for resno in res_lo..res_hi {
                        let layout = &layouts[comp][resno];
                        for (prec_idx, prec) in layout.precincts.iter().enumerate() {
                            order.push((prec.ref_y, prec.ref_x, resno, prec_idx));
                        }
                    }
                    order.sort();
                    for (_, _, resno, prec_idx) in order {
                        for layer in 0..layer_hi {
                            if next_layer[comp][resno][prec_idx] != layer {
                                continue;
                            }
                            emit_packet!(
                                cur,
                                header_cursor,
                                layer,
                                &mut layouts[comp][resno],
                                prec_idx,
                                cod
                            );
                            next_layer[comp][resno][prec_idx] = layer + 1;
                        }
                    }
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}

pub(crate) fn read_num_passes(bio: &mut Bio<'_>) -> u32 {
    if bio.read_bit() == 0 {
        return 1;
    }
    if bio.read_bit() == 0 {
        return 2;
    }
    let v = bio.read(2);
    if v < 3 {
        return 3 + v;
    }
    let v = bio.read(5);
    if v < 31 {
        return 6 + v;
    }
    37 + bio.read(7)
}

pub(crate) fn ilog2(n: u32) -> u32 {
    if n == 0 {
        0
    } else {
        31 - n.leading_zeros()
    }
}

/// Build per-component `Vec<ResolutionLayout>`s for a parsed J2K
/// codestream and run the tier-2 walker. Shared between the
/// round-6 diagnostic helpers below.
fn round6_walk_layouts(j2k: &[u8]) -> Result<(CodParams, QcdParams, Vec<Vec<ResolutionLayout>>)> {
    let cs = crate::codestream::parse(j2k)?;
    let cod = parse_cod(cs.cod.as_ref().ok_or_else(|| Error::invalid("no cod"))?)?;
    let qcd = parse_qcd(
        cs.qcd.as_ref().ok_or_else(|| Error::invalid("no qcd"))?,
        cod.num_decomp,
    )?;
    let (w, h) = (cs.siz.image_width(), cs.siz.image_height());
    let num_comps = cs.siz.num_components();
    let num_res = (cod.num_decomp as usize) + 1;
    let mut layouts: Vec<Vec<ResolutionLayout>> = Vec::with_capacity(num_comps);
    for _ in 0..num_comps {
        let sb = build_subbands(0, 0, w, h, cod.num_decomp);
        layouts.push(build_resolutions(
            sb,
            cod.num_decomp,
            cod.cblk_w_log2,
            cod.cblk_h_log2,
            &cod.precincts,
            (0, 0, w, h),
            1,
            1,
        ));
    }
    let mut body = Vec::new();
    for tp in &cs.tile_parts {
        body.extend_from_slice(&j2k[tp.sod_offset..tp.sod_offset + tp.sod_length]);
    }
    let mut cursor = Cursor::new(&body);
    walk_packets(
        &mut cursor,
        &cod,
        None,
        None,
        &mut layouts,
        num_res,
        num_comps,
    )?;
    Ok((cod, qcd, layouts))
}

/// Split the in-line packet headers out of a single tile's body.
///
/// Walks the same tier-2 progression as [`walk_packets`] over `body`
/// (which is one tile's concatenated tile-part bodies, headers in line —
/// the historical layout) and returns:
///
/// - `headers`: the concatenated per-packet header bytes (each ending
///   in an `EPH` marker when `cod.eph_marker` is set, exactly as they
///   would appear inside a PPM/PPT packed-header stream per
///   T.800 §A.7.4 / §A.7.5).
/// - `body_only`: the concatenated per-packet bodies, with `SOP` markers
///   preserved in their original position (per §A.8.1, SOP belongs to
///   the SOD stream regardless of PPM/PPT).
///
/// Together these support the "header / body splitter" used by the
/// PPM and PPT round-trip tests: the decoder reading
/// `body = body_only` plus `packet_headers = headers` must produce the
/// same image as decoding the original `body` directly. The split is
/// purely a re-arrangement of bytes — no decoding is performed.
///
/// `comp_sizes`, `cod`, `qcd`, `poc` must match the parameters used by
/// the decoder for the same tile; the tier-2 layout is recomputed from
/// scratch so the splitter does not share state with `decode_tile_*`.
#[allow(clippy::needless_range_loop)]
pub fn split_packet_headers(
    body: &[u8],
    comp_sizes: &[(u32, u32, u32, u32)],
    cod: &CodParams,
    qcd: &QcdParams,
    poc: Option<&PocParams>,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let _ = qcd; // tier-2 walker doesn't need QCD; included for API symmetry.
    if cod.num_layers == 0 {
        return Err(Error::invalid(
            "jpeg2000: COD signals zero quality layers (must be >= 1)",
        ));
    }
    if !matches!(cod.progression_order, 0..=4) {
        return Err(Error::unsupported(
            "jpeg2000: progression order must be 0..=4 (LRCP/RLCP/RPCL/PCRL/CPRL)",
        ));
    }

    let num_comps = comp_sizes.len();
    let num_res = (cod.num_decomp as usize) + 1;
    let mut layouts: Vec<Vec<ResolutionLayout>> = Vec::with_capacity(num_comps);
    for &(x0, y0, x1, y1) in comp_sizes {
        let subbands = build_subbands(x0, y0, x1, y1, cod.num_decomp);
        layouts.push(build_resolutions(
            subbands,
            cod.num_decomp,
            cod.cblk_w_log2,
            cod.cblk_h_log2,
            &cod.precincts,
            (x0, y0, x1, y1),
            1,
            1,
        ));
    }

    let mut cursor = Cursor::new(body);
    let mut headers_out: Vec<u8> = Vec::new();
    let mut body_out: Vec<u8> = Vec::new();
    walk_packets_split(
        &mut cursor,
        cod,
        poc,
        &mut headers_out,
        &mut body_out,
        &mut layouts,
        num_res,
        num_comps,
    )?;
    // Any trailing bytes between the last packet and the end of the
    // tile-part body (rare, but legal padding) are appended to the body
    // output verbatim.
    body_out.extend_from_slice(cursor.remaining());
    Ok((headers_out, body_out))
}

/// Mirror of `walk_packets` that extracts header bytes into
/// `headers_out` and body bytes into `body_out` instead of decoding.
#[allow(clippy::needless_range_loop, clippy::too_many_arguments)]
fn walk_packets_split(
    cur: &mut Cursor<'_>,
    cod: &CodParams,
    poc: Option<&PocParams>,
    headers_out: &mut Vec<u8>,
    body_out: &mut Vec<u8>,
    layouts: &mut [Vec<ResolutionLayout>],
    num_res: usize,
    num_comps: usize,
) -> Result<()> {
    let num_layers = cod.num_layers as u32;
    let progressions: Vec<PocProgression> = if let Some(poc) = poc {
        poc.progressions.clone()
    } else {
        vec![PocProgression {
            res_start: 0,
            comp_start: 0,
            layer_end: cod.num_layers,
            res_end: num_res as u8,
            comp_end: num_comps as u16,
            progression: cod.progression_order,
        }]
    };

    let mut next_layer: Vec<Vec<Vec<u32>>> = (0..num_comps)
        .map(|c| {
            (0..num_res)
                .map(|r| vec![0u32; layouts[c][r].precincts.len()])
                .collect()
        })
        .collect();

    macro_rules! emit {
        ($cur:expr, $layer:expr, $layout:expr, $prec:expr, $cod:expr) => {{
            split_one_packet($cur, headers_out, body_out, $layer, $layout, $prec, $cod)?
        }};
    }

    for prog in &progressions {
        let comp_lo = (prog.comp_start as usize).min(num_comps);
        let comp_hi = (prog.comp_end as usize).min(num_comps);
        let res_lo = (prog.res_start as usize).min(num_res);
        let res_hi = (prog.res_end as usize).min(num_res);
        let layer_hi = (prog.layer_end as u32).min(num_layers);
        if comp_lo >= comp_hi || res_lo >= res_hi || layer_hi == 0 {
            continue;
        }
        match prog.progression {
            0 => {
                for layer in 0..layer_hi {
                    for resno in res_lo..res_hi {
                        for comp in comp_lo..comp_hi {
                            let nprec = layouts[comp][resno].precincts.len();
                            for prec in 0..nprec {
                                if next_layer[comp][resno][prec] != layer {
                                    continue;
                                }
                                emit!(cur, layer, &mut layouts[comp][resno], prec, cod);
                                next_layer[comp][resno][prec] = layer + 1;
                            }
                        }
                    }
                }
            }
            1 => {
                for resno in res_lo..res_hi {
                    for layer in 0..layer_hi {
                        for comp in comp_lo..comp_hi {
                            let nprec = layouts[comp][resno].precincts.len();
                            for prec in 0..nprec {
                                if next_layer[comp][resno][prec] != layer {
                                    continue;
                                }
                                emit!(cur, layer, &mut layouts[comp][resno], prec, cod);
                                next_layer[comp][resno][prec] = layer + 1;
                            }
                        }
                    }
                }
            }
            2 => {
                for resno in res_lo..res_hi {
                    let mut order: Vec<(u32, u32, usize, usize)> = Vec::new();
                    for comp in comp_lo..comp_hi {
                        let layout = &layouts[comp][resno];
                        for (prec_idx, prec) in layout.precincts.iter().enumerate() {
                            order.push((prec.ref_y, prec.ref_x, comp, prec_idx));
                        }
                    }
                    order.sort();
                    for (_, _, comp, prec_idx) in order {
                        for layer in 0..layer_hi {
                            if next_layer[comp][resno][prec_idx] != layer {
                                continue;
                            }
                            emit!(cur, layer, &mut layouts[comp][resno], prec_idx, cod);
                            next_layer[comp][resno][prec_idx] = layer + 1;
                        }
                    }
                }
            }
            3 => {
                let mut order: Vec<(u32, u32, usize, usize, usize)> = Vec::new();
                for comp in comp_lo..comp_hi {
                    for resno in res_lo..res_hi {
                        let layout = &layouts[comp][resno];
                        for (prec_idx, prec) in layout.precincts.iter().enumerate() {
                            order.push((prec.ref_y, prec.ref_x, comp, resno, prec_idx));
                        }
                    }
                }
                order.sort();
                for (_, _, comp, resno, prec_idx) in order {
                    for layer in 0..layer_hi {
                        if next_layer[comp][resno][prec_idx] != layer {
                            continue;
                        }
                        emit!(cur, layer, &mut layouts[comp][resno], prec_idx, cod);
                        next_layer[comp][resno][prec_idx] = layer + 1;
                    }
                }
            }
            4 => {
                for comp in comp_lo..comp_hi {
                    let mut order: Vec<(u32, u32, usize, usize)> = Vec::new();
                    for resno in res_lo..res_hi {
                        let layout = &layouts[comp][resno];
                        for (prec_idx, prec) in layout.precincts.iter().enumerate() {
                            order.push((prec.ref_y, prec.ref_x, resno, prec_idx));
                        }
                    }
                    order.sort();
                    for (_, _, resno, prec_idx) in order {
                        for layer in 0..layer_hi {
                            if next_layer[comp][resno][prec_idx] != layer {
                                continue;
                            }
                            emit!(cur, layer, &mut layouts[comp][resno], prec_idx, cod);
                            next_layer[comp][resno][prec_idx] = layer + 1;
                        }
                    }
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}

/// Header/body splitter for a single packet — runs the same tag-tree
/// and Bio bookkeeping as `parse_precinct_packet` (without the body
/// payload extraction) so the exact `(header_bytes, body)` slices can
/// be copied into the splitter outputs.
fn split_one_packet(
    cur: &mut Cursor<'_>,
    headers_out: &mut Vec<u8>,
    body_out: &mut Vec<u8>,
    layer: u32,
    res: &mut ResolutionLayout,
    prec_idx: usize,
    cod: &CodParams,
) -> Result<()> {
    // SOP — copied verbatim into body_out; per §A.8.1 the SOP marker
    // is part of the SOD stream regardless of PPM/PPT.
    if cod.sop_marker && cur.remaining().starts_with(&[0xFF, 0x91]) {
        if cur.remaining().len() < 6 {
            return Err(Error::invalid("jpeg2000: truncated SOP"));
        }
        let sop = cur.consume(6)?;
        body_out.extend_from_slice(sop);
    }

    // Header: parse with Bio just enough to learn `numbytes_read` and
    // to update tag-tree / cblk-state for downstream packets. Body
    // length needed too because we must skip past the body in `cur`.
    let header_start = cur.pos;
    let header_slice = cur.remaining();
    let mut bio = Bio::new(header_slice);
    let mut pending_lengths: Vec<u32> = Vec::new();

    if bio.read_bit() == 0 {
        bio.inalign();
    } else {
        for sb_idx in 0..res.subbands.len() {
            let cblks_w_g = res.cblks_w[sb_idx];
            let prec = &mut res.precincts[prec_idx].sb_states[sb_idx];
            let pcw = prec.pcw;
            let pch = prec.pch;
            let base_cx = prec.cx0;
            let base_cy = prec.cy0;
            if pcw == 0 || pch == 0 {
                continue;
            }
            for lcy in 0..pch {
                for lcx in 0..pcw {
                    let g_cx = base_cx + lcx;
                    let g_cy = base_cy + lcy;
                    let g_idx = g_cy * cblks_w_g + g_cx;
                    let included_now;
                    let missing_msb;
                    if !res.cblk_states[sb_idx][g_idx].included {
                        included_now = prec.inclusion.decode(lcx, lcy, layer + 1, &mut bio);
                        if !included_now {
                            continue;
                        }
                        let mut i = 0u32;
                        loop {
                            if prec.zero_bitplanes.decode(lcx, lcy, i, &mut bio) {
                                break;
                            }
                            i += 1;
                            if i > 64 {
                                return Err(Error::invalid(
                                    "jpeg2000: missing-MSB tag tree runaway",
                                ));
                            }
                        }
                        missing_msb = i;
                    } else {
                        included_now = bio.read_bit() != 0;
                        if !included_now {
                            continue;
                        }
                        missing_msb = res.cblk_states[sb_idx][g_idx].missing_msb;
                    }
                    let num_passes = read_num_passes(&mut bio);
                    while bio.read_bit() == 1 {
                        res.cblk_states[sb_idx][g_idx].lblock += 1;
                    }
                    let len_bits = res.cblk_states[sb_idx][g_idx].lblock + ilog2(num_passes);
                    let length = bio.read(len_bits);
                    let st = &mut res.cblk_states[sb_idx][g_idx];
                    st.included = true;
                    st.total_passes += num_passes;
                    st.missing_msb = missing_msb;
                    pending_lengths.push(length);
                }
            }
        }
        bio.inalign();
    }
    let header_bytes_used = bio.numbytes_read();
    cur.consume(header_bytes_used)?;
    // EPH belongs to the packet header per §A.8.2 — when the splitter
    // emits to a PPM/PPT stream the EPH must accompany the header.
    let mut header_with_eph_end = header_start + header_bytes_used;
    if cod.eph_marker && cur.remaining().starts_with(&[0xFF, 0x92]) {
        cur.consume(2)?;
        header_with_eph_end += 2;
    }
    headers_out.extend_from_slice(&cur.buf[header_start..header_with_eph_end]);

    // Body — concatenate the bytes claimed by every (sb, cblk) length
    // we recorded, in the same order they were parsed.
    let mut total_body: usize = 0;
    for length in &pending_lengths {
        total_body += *length as usize;
    }
    let body_slice = cur.consume(total_body)?;
    body_out.extend_from_slice(body_slice);
    Ok(())
}

/// Round-6 diagnostic helper. Decodes a single-tile `.j2k` codestream
/// and returns the per-sub-band tier-1 output (already `/ 2`) for LL,
/// HL, LH, HH at resolution 1 (for a 1-level 5/3 codestream). Each
/// returned buffer is flat `hw * hh` = quarter of the image. Used to
/// pin which sub-band our decoder disagrees with the OPJ fixture on.
#[allow(clippy::needless_range_loop, clippy::type_complexity)]
pub fn decode_subbands_round6(j2k: &[u8]) -> Result<(Vec<i32>, Vec<i32>, Vec<i32>, Vec<i32>)> {
    let (cod, qcd, layouts) = round6_walk_layouts(j2k)?;
    let num_res = (cod.num_decomp as usize) + 1;
    if num_res != 2 {
        return Err(Error::unsupported(
            "decode_subbands_round6: expects 1 decomposition level",
        ));
    }
    let mut subband_results: Vec<Vec<i32>> = Vec::with_capacity(4);
    for resno in 0..num_res {
        let layout = &layouts[0][resno];
        for (sb_idx, sb) in layout.subbands.iter().enumerate() {
            let bw = (sb.x1 - sb.x0) as usize;
            let bh = (sb.y1 - sb.y0) as usize;
            let mut buf = vec![0i32; bw * bh];
            let cblks = &layout.cblk_states[sb_idx];
            let cblks_w = layout.cblks_w[sb_idx];
            let cblks_h = layout.cblks_h[sb_idx];
            for cy in 0..cblks_h {
                for cx in 0..cblks_w {
                    let idx = cy * cblks_w + cx;
                    let st = &cblks[idx];
                    if !st.included || st.total_passes == 0 {
                        continue;
                    }
                    let (bx0, by0, bx1, by1) = layout.cblk_rects[sb_idx][idx];
                    let cbw = (bx1 - bx0) as usize;
                    let cbh = (by1 - by0) as usize;
                    let (eps, _mant) = qcd.bands[sb.band_idx];
                    let band_numbps = qcd.guard_bits as i32 + eps as i32 - 1;
                    let bpno = band_numbps + 1 - st.missing_msb as i32;
                    if bpno < 1 {
                        continue;
                    }
                    let decoded = t1::decode_cblk(
                        st.data.clone(),
                        cbw,
                        cbh,
                        bpno,
                        st.total_passes,
                        sb.orient,
                        cod.cblk_style,
                    );
                    let rel_x = (bx0 - sb.x0) as usize;
                    let rel_y = (by0 - sb.y0) as usize;
                    for ly in 0..cbh {
                        for lx in 0..cbw {
                            let v = decoded.data[ly * cbw + lx];
                            buf[(rel_y + ly) * bw + (rel_x + lx)] = v / 2;
                        }
                    }
                }
            }
            subband_results.push(buf);
        }
    }
    Ok((
        subband_results[0].clone(),
        subband_results[1].clone(),
        subband_results[2].clone(),
        subband_results[3].clone(),
    ))
}

/// Round-6 diagnostic helper. Same shape as `extract_ll_cblk_round6`
/// but targets one of the resolution-1 sub-band code-blocks. `band_kind`
/// selects HL=1, LH=2, HH=3.
#[allow(clippy::needless_range_loop)]
pub fn extract_sb_cblk_round6(
    j2k: &[u8],
    band_kind: u8,
) -> Result<(usize, usize, i32, i32, u32, Vec<u8>)> {
    let (_cod, qcd, layouts) = round6_walk_layouts(j2k)?;
    // Pick sb_idx in resno=1 by band_kind. subbands at res=1 are stored
    // in order HL, LH, HH (band_kind 1, 2, 3).
    let sb_idx = match band_kind {
        1 => 0,
        2 => 1,
        3 => 2,
        _ => return Err(Error::invalid("band_kind must be 1..=3")),
    };
    let layout = &layouts[0][1];
    let sb = &layout.subbands[sb_idx];
    let bw = (sb.x1 - sb.x0) as usize;
    let bh = (sb.y1 - sb.y0) as usize;
    let (eps, _mant) = qcd.bands[sb.band_idx];
    let band_numbps = qcd.guard_bits as i32 + eps as i32 - 1;
    let st = &layout.cblk_states[sb_idx][0];
    let bpno = band_numbps + 1 - st.missing_msb as i32;
    Ok((bw, bh, band_numbps, bpno, st.total_passes, st.data.clone()))
}

/// Round-6 diagnostic helper. Parses a single-tile `.j2k` codestream,
/// walks tier-2 just far enough to populate the LL-resolution-0 code
/// block, and returns its raw byte stream along with the decoder
/// parameters `decode_cblk` expects. Public-but-unstable: used only by
/// the `opj_t1_mqtrace` diagnostic test.
#[allow(clippy::needless_range_loop)]
pub fn extract_ll_cblk_round6(j2k: &[u8]) -> Result<(usize, usize, i32, i32, u32, Vec<u8>)> {
    let (_cod, qcd, layouts) = round6_walk_layouts(j2k)?;
    let layout0 = &layouts[0][0];
    let sb = &layout0.subbands[0];
    let bw = (sb.x1 - sb.x0) as usize;
    let bh = (sb.y1 - sb.y0) as usize;
    let (eps, _mant) = qcd.bands[sb.band_idx];
    let band_numbps = qcd.guard_bits as i32 + eps as i32 - 1;
    let st = &layout0.cblk_states[0][0];
    let bpno = band_numbps + 1 - st.missing_msb as i32;
    Ok((bw, bh, band_numbps, bpno, st.total_passes, st.data.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn div_ceil_basic() {
        assert_eq!(div_ceil(10, 1), 10);
        assert_eq!(div_ceil(10, 2), 5);
        assert_eq!(div_ceil(11, 2), 6);
    }

    #[test]
    fn parse_cod_default_values() {
        let seg = [0x00, 0x00, 0x00, 0x01, 0x00, 0x05, 0x04, 0x04, 0x00, 0x01];
        let cod = parse_cod(&seg).expect("cod");
        assert_eq!(cod.num_decomp, 5);
        assert_eq!(cod.transform, 1);
        assert_eq!(cod.num_layers, 1);
        assert_eq!(cod.cblk_w_log2, 6);
        assert_eq!(cod.cblk_h_log2, 6);
    }

    #[test]
    fn parse_qcd_reversible() {
        let mut seg = vec![0x40];
        for i in 0..16 {
            seg.push((i << 3) as u8);
        }
        let qcd = parse_qcd(&seg, 5).expect("qcd");
        assert!(qcd.is_reversible);
        assert_eq!(qcd.guard_bits, 2);
        assert_eq!(qcd.bands.len(), 16);
    }

    #[test]
    fn subband_dims_64x64_five_levels() {
        // 5-level pyramid of a 64x64 canvas: LL0 = 2x2.
        let sbs = build_subbands(0, 0, 64, 64, 5);
        assert_eq!(sbs[0].band_kind, 0); // LL
        assert_eq!(sbs[0].x1 - sbs[0].x0, 2);
        assert_eq!(sbs[0].y1 - sbs[0].y0, 2);
        // Level 5 (finest) HL/LH/HH each 32x32.
        let finest_hl = sbs
            .iter()
            .find(|s| s.resno == 5 && s.band_kind == 1)
            .unwrap();
        assert_eq!(finest_hl.x1 - finest_hl.x0, 32);
        assert_eq!(finest_hl.y1 - finest_hl.y0, 32);
    }
}
