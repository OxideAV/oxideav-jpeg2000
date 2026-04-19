//! Tile decoder: orchestrates tier-2 packet parsing, tier-1 bitplane
//! decode, dequantisation, inverse DWT and level shift for a single
//! JPEG 2000 tile.
//!
//! Scope
//! -----
//!
//! - **5/3 integer reversible** wavelet (Part-1 lossless default).
//! - **LRCP** and **RLCP** progression orders only.
//! - One layer only (baseline lossless output).
//! - One precinct per resolution (PPx = PPy = 15 in the COD).
//! - Single tile per frame.
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

pub struct PrecinctState {
    pub inclusion: TagTree,
    pub zero_bitplanes: TagTree,
    pub cblks: Vec<CblkState>,
    pub cblks_w: usize,
    pub cblks_h: usize,
}

impl PrecinctState {
    fn new(cblks_w: usize, cblks_h: usize) -> Self {
        let w = cblks_w.max(1);
        let h = cblks_h.max(1);
        PrecinctState {
            inclusion: TagTree::new(w, h),
            zero_bitplanes: TagTree::new(w, h),
            cblks: vec![
                CblkState {
                    lblock: 3,
                    ..Default::default()
                };
                w * h
            ],
            cblks_w,
            cblks_h,
        }
    }
}

pub struct ResolutionLayout {
    pub resno: u8,
    pub subbands: Vec<SubbandInfo>,
    pub prec_states: Vec<PrecinctState>,
    pub cblk_rects: Vec<Vec<(u32, u32, u32, u32)>>,
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

fn build_resolutions(
    subbands: Vec<SubbandInfo>,
    num_decomp: u8,
    cblk_w_log2: u8,
    cblk_h_log2: u8,
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
            let mut prec_states = Vec::with_capacity(subs.len());
            let mut cblk_rects = Vec::with_capacity(subs.len());
            for sb in &subs {
                let cw = 1u32 << cblk_w_log2;
                let ch = 1u32 << cblk_h_log2;
                let band_w = sb.x1.saturating_sub(sb.x0);
                let band_h = sb.y1.saturating_sub(sb.y0);
                let cblks_w = div_ceil(band_w, cw) as usize;
                let cblks_h = div_ceil(band_h, ch) as usize;
                prec_states.push(PrecinctState::new(cblks_w, cblks_h));
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
            }
            ResolutionLayout {
                resno: resno as u8,
                subbands: subs,
                prec_states,
                cblk_rects,
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
    if cod.num_layers != 1 {
        return Err(Error::unsupported(
            "jpeg2000: only one quality layer is currently supported",
        ));
    }
    if cod.transform > 1 {
        return Err(Error::unsupported(
            "jpeg2000: unknown transform id (must be 0 or 1)",
        ));
    }
    if !(cod.progression_order == 0 || cod.progression_order == 1) {
        return Err(Error::unsupported(
            "jpeg2000: only LRCP / RLCP progression orders are supported",
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
        ));
    }

    let mut cursor = Cursor::new(body);
    match cod.progression_order {
        0 => {
            for layer in 0..cod.num_layers as u32 {
                for resno in 0..num_res {
                    for comp in 0..num_comps {
                        parse_precinct_packet(&mut cursor, layer, &mut layouts[comp][resno], cod)?;
                    }
                }
            }
        }
        1 => {
            for resno in 0..num_res {
                for layer in 0..cod.num_layers as u32 {
                    for comp in 0..num_comps {
                        parse_precinct_packet(&mut cursor, layer, &mut layouts[comp][resno], cod)?;
                    }
                }
            }
        }
        _ => unreachable!(),
    }

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
            let prec = &layout.prec_states[sb_idx];
            for cy in 0..prec.cblks_h {
                for cx in 0..prec.cblks_w {
                    let idx = cy * prec.cblks_w + cx;
                    let st = &prec.cblks[idx];
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
            let prec_state = &layout.prec_states[sb_idx];
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
            for cy in 0..prec_state.cblks_h {
                for cx in 0..prec_state.cblks_w {
                    let idx = cy * prec_state.cblks_w + cx;
                    let st = &prec_state.cblks[idx];
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

fn parse_precinct_packet(
    cur: &mut Cursor<'_>,
    layer: u32,
    res: &mut ResolutionLayout,
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
    let mut pending: Vec<(usize, usize, u32)> = Vec::new();

    if bio.read_bit() == 0 {
        bio.inalign();
    } else {
        for sb_idx in 0..res.subbands.len() {
            let prec = &mut res.prec_states[sb_idx];
            let cblks_w = prec.cblks_w;
            let cblks_h = prec.cblks_h;
            for cy in 0..cblks_h {
                for cx in 0..cblks_w {
                    let cblk_idx = cy * cblks_w + cx;
                    let included_now;
                    let missing_msb;
                    if !prec.cblks[cblk_idx].included {
                        included_now = prec.inclusion.decode(cx, cy, layer + 1, &mut bio);
                        if !included_now {
                            continue;
                        }
                        // Missing-MSB (zero bitplanes) tag tree.
                        // OpenJPEG starts at i=0 and iterates `while
                        // !decode(i)` until the decoder reports the
                        // leaf value is below the threshold. On break,
                        // cblk->numbps = band->numbps + 1 - i. Our
                        // `missing_msb` stores the zero-bitplane count
                        // in the same scale as OpenJPEG's `i`.
                        let mut i = 0u32;
                        loop {
                            if prec.zero_bitplanes.decode(cx, cy, i, &mut bio) {
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
                        missing_msb = prec.cblks[cblk_idx].missing_msb;
                    }
                    let num_passes = read_num_passes(&mut bio);
                    while bio.read_bit() == 1 {
                        prec.cblks[cblk_idx].lblock += 1;
                    }
                    let len_bits = prec.cblks[cblk_idx].lblock + ilog2(num_passes);
                    let length = bio.read(len_bits);
                    prec.cblks[cblk_idx].included = true;
                    prec.cblks[cblk_idx].total_passes += num_passes;
                    prec.cblks[cblk_idx].missing_msb = missing_msb;
                    pending.push((sb_idx, cblk_idx, length));
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
    for (sb_idx, cblk_idx, length) in pending {
        let bytes = cur.consume(length as usize)?.to_vec();
        res.prec_states[sb_idx].cblks[cblk_idx]
            .data
            .extend_from_slice(&bytes);
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
