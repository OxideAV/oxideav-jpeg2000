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
use oxideav_core::{Error, Result};

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
fn build_resolutions(
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
    let params = DecodeParams {
        comp_precisions: &precisions,
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
    walk_packets(&mut cursor, cod, &mut layouts, num_res, num_comps)?;

    // Per-component IDWT using per-subband buffers.
    let mut out = Vec::with_capacity(num_comps);
    for (comp_idx, &(cx0, cy0, cx1, cy1)) in comp_sizes.iter().enumerate() {
        let comp_w = (cx1 - cx0) as usize;
        let comp_h = (cy1 - cy0) as usize;
        if comp_w == 0 || comp_h == 0 {
            out.push(Vec::new());
            continue;
        }
        if cod.transform == 1 {
            out.push(synth_component_53(
                &layouts[comp_idx],
                num_res,
                comp_w,
                comp_h,
                cod,
                qcd,
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
                    let band_numbps = qcd.guard_bits as i32 + eps as i32 - 1;
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
                            let v = decoded.data[ly * w + lx];
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
                    let band_numbps = qcd.guard_bits as i32 + eps as i32 - 1;
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
                            let v = decoded.data[ly * w + lx];
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
fn parse_precinct_packet(
    cur: &mut Cursor<'_>,
    layer: u32,
    res: &mut ResolutionLayout,
    prec_idx: usize,
    cod: &CodParams,
) -> Result<()> {
    if cod.sop_marker && cur.remaining().starts_with(&[0xFF, 0x91]) {
        if cur.remaining().len() < 6 {
            return Err(Error::invalid("jpeg2000: truncated SOP"));
        }
        cur.consume(6)?;
    }

    let header_start = cur.remaining();
    let mut bio = Bio::new(header_start);
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
    cur.consume(header_bytes_used)?;
    if cod.eph_marker && cur.remaining().starts_with(&[0xFF, 0x92]) {
        cur.consume(2)?;
    }
    for (sb_idx, g_idx, length) in pending {
        let bytes = cur.consume(length as usize)?.to_vec();
        res.cblk_states[sb_idx][g_idx]
            .data
            .extend_from_slice(&bytes);
    }
    Ok(())
}

/// Walk the tier-2 packet stream in the order signalled by `cod.progression_order`.
/// All five Part-1 progression orders are handled (T.800 §B.12.1).
#[allow(clippy::needless_range_loop)]
fn walk_packets(
    cur: &mut Cursor<'_>,
    cod: &CodParams,
    layouts: &mut [Vec<ResolutionLayout>],
    num_res: usize,
    num_comps: usize,
) -> Result<()> {
    let num_layers = cod.num_layers as u32;
    match cod.progression_order {
        // LRCP — §B.12.1.1.
        0 => {
            for layer in 0..num_layers {
                for resno in 0..num_res {
                    for comp in 0..num_comps {
                        let nprec = layouts[comp][resno].precincts.len();
                        for prec in 0..nprec {
                            parse_precinct_packet(
                                cur,
                                layer,
                                &mut layouts[comp][resno],
                                prec,
                                cod,
                            )?;
                        }
                    }
                }
            }
        }
        // RLCP — §B.12.1.2.
        1 => {
            for resno in 0..num_res {
                for layer in 0..num_layers {
                    for comp in 0..num_comps {
                        let nprec = layouts[comp][resno].precincts.len();
                        for prec in 0..nprec {
                            parse_precinct_packet(
                                cur,
                                layer,
                                &mut layouts[comp][resno],
                                prec,
                                cod,
                            )?;
                        }
                    }
                }
            }
        }
        // RPCL — §B.12.1.3. Per-resolution (y, x) walk over the
        // reference grid; the spec note (§B.12.1.3 "NOTE") says we may
        // iterate precincts directly. Group by resolution; sort
        // (component, precinct) pairs by (ref_y, ref_x, comp) to
        // emulate the (y, x) walk efficiently.
        2 => {
            for resno in 0..num_res {
                let mut order: Vec<(u32, u32, usize, usize)> = Vec::new();
                for comp in 0..num_comps {
                    let layout = &layouts[comp][resno];
                    for (prec_idx, prec) in layout.precincts.iter().enumerate() {
                        order.push((prec.ref_y, prec.ref_x, comp, prec_idx));
                    }
                }
                order.sort();
                for (_, _, comp, prec_idx) in order {
                    for layer in 0..num_layers {
                        parse_precinct_packet(
                            cur,
                            layer,
                            &mut layouts[comp][resno],
                            prec_idx,
                            cod,
                        )?;
                    }
                }
            }
        }
        // PCRL — §B.12.1.4. Outer (y, x) over the ref grid; inner
        // component, resolution, layer.
        3 => {
            let mut order: Vec<(u32, u32, usize, usize, usize)> = Vec::new();
            for comp in 0..num_comps {
                for resno in 0..num_res {
                    let layout = &layouts[comp][resno];
                    for (prec_idx, prec) in layout.precincts.iter().enumerate() {
                        order.push((prec.ref_y, prec.ref_x, comp, resno, prec_idx));
                    }
                }
            }
            // Spec order: (y, x, comp, r). We sort by that tuple.
            order.sort();
            for (_, _, comp, resno, prec_idx) in order {
                for layer in 0..num_layers {
                    parse_precinct_packet(cur, layer, &mut layouts[comp][resno], prec_idx, cod)?;
                }
            }
        }
        // CPRL — §B.12.1.5. Outer component, then (y, x), then r, then layer.
        4 => {
            for comp in 0..num_comps {
                let mut order: Vec<(u32, u32, usize, usize)> = Vec::new();
                for resno in 0..num_res {
                    let layout = &layouts[comp][resno];
                    for (prec_idx, prec) in layout.precincts.iter().enumerate() {
                        order.push((prec.ref_y, prec.ref_x, resno, prec_idx));
                    }
                }
                order.sort();
                for (_, _, resno, prec_idx) in order {
                    for layer in 0..num_layers {
                        parse_precinct_packet(
                            cur,
                            layer,
                            &mut layouts[comp][resno],
                            prec_idx,
                            cod,
                        )?;
                    }
                }
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn read_num_passes(bio: &mut Bio<'_>) -> u32 {
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

fn ilog2(n: u32) -> u32 {
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
    walk_packets(&mut cursor, &cod, &mut layouts, num_res, num_comps)?;
    Ok((cod, qcd, layouts))
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
