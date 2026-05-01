//! HTJ2K tier-2 packet walker + frame driver.
//!
//! Bridges the existing Part-1 tier-2 packet header syntax (T.800
//! §B.10) to the round-2 FBCOT entropy decoder defined in
//! [`super::decode_codeblock`]. Per ISO/IEC 15444-15 §A.4 + Annex B:
//!
//! - The packet header structure (inclusion + zero-bit-plane tag trees,
//!   `num_passes` comma-coded value, adaptive `Lblock`, per-codeblock
//!   length field) is **identical** to Part-1. Only the payload bytes
//!   are interpreted differently — they form HT cleanup / refinement
//!   segments (§7.1) instead of an MQ-coded bit-plane stream.
//! - For a single-layer codestream where every code-block emits a
//!   single first packet (the common case), the per-codeblock byte
//!   range yielded by the tier-2 walker is exactly `Dcup` of length
//!   `Lcup` when `Z_blk = 1` (cleanup-only). Z_blk values >= 2 require
//!   a refinement segment too; for round 3 we only handle `Z_blk = 1`
//!   (the "cleanup only" case the reference tests exercise) and
//!   surface anything richer as `Error::Unsupported`.
//!
//! Scope of this driver (round 3):
//!
//! 1. Single-tile, single-layer, single-component or 3-component
//!    HTJ2K codestreams.
//! 2. Any number of decomposition levels — the existing 5/3 / 9/7
//!    inverse DWT is reused. The fixture in `tests/htj2k_pixels.rs`
//!    uses `NL = 0` (identity transform) for end-to-end byte
//!    correctness.
//! 3. Per-codeblock dispatch: each included codeblock has its bytes
//!    routed through `decode_codeblock`; the decoded `(mag, sign)`
//!    arrays are reassembled into sub-band raster order, then
//!    de-quantised + IDWT-synthesised + DC-level-shifted exactly as in
//!    the classic-EBCOT path.

use super::{decode_codeblock, ZBlk};
use crate::codestream::{Codestream, Siz};
use crate::decode::bio::Bio;
use crate::decode::dwt;
use crate::decode::tile::{
    build_resolutions, build_subbands, ilog2, parse_cod, parse_qcd, read_num_passes, CodParams,
    QcdParams, ResolutionLayout,
};
use oxideav_core::{Error, Frame, PixelFormat, Result, VideoFrame, VideoPlane};

/// Decode an HTJ2K codestream end-to-end into a `Frame`.
///
/// Mirrors the public [`crate::decode::frame::decode_frame`] entry
/// point but routes per-codeblock bytes through the FBCOT decoder
/// instead of the classic EBCOT MQ tier-1.
pub fn decode_frame_htj2k(cs: &Codestream, buf: &[u8]) -> Result<Frame> {
    let cod_bytes = cs
        .cod
        .as_ref()
        .ok_or_else(|| Error::invalid("jpeg2000: missing COD segment"))?;
    let qcd_bytes = cs
        .qcd
        .as_ref()
        .ok_or_else(|| Error::invalid("jpeg2000: missing QCD segment"))?;
    let cod = parse_cod(cod_bytes)?;
    let qcd = parse_qcd(qcd_bytes, cod.num_decomp)?;

    // §A.4: HTJ2K SPcod must signal "all blocks HT" (bit 6 = 1, bit 7 = 0).
    // We accept any cblk_style for round 3 — the bytes in the body are
    // what they are; we trust the CAP-marker dispatch in `lib.rs` to gate
    // calls into this driver.
    if cs.tile_parts.is_empty() {
        return Err(Error::invalid("jpeg2000: no tile-parts in codestream"));
    }
    if cs.tile_parts.len() > 1 {
        return Err(Error::unsupported(
            "HTJ2K: multi-tile-part codestreams (round 4+)",
        ));
    }
    if !cs.ppm.is_empty() {
        return Err(Error::unsupported(
            "HTJ2K: PPM-packed packet headers (round 4+)",
        ));
    }
    if !cs.tile_parts[0].ppt.is_empty() {
        return Err(Error::unsupported(
            "HTJ2K: PPT-packed packet headers (round 4+)",
        ));
    }
    if cs.poc.is_some() || cs.tile_parts[0].poc.is_some() {
        return Err(Error::unsupported("HTJ2K: POC progressions (round 4+)"));
    }
    if cod.num_layers != 1 {
        return Err(Error::unsupported(
            "HTJ2K: multi-layer codestreams (round 4+)",
        ));
    }
    if cod.progression_order != 0 {
        return Err(Error::unsupported(
            "HTJ2K: only LRCP progression supported in round 3",
        ));
    }

    let img_w = cs.siz.image_width();
    let img_h = cs.siz.image_height();
    let num_comps = cs.siz.components.len();
    if num_comps == 0 {
        return Err(Error::invalid("jpeg2000: SIZ has zero components"));
    }
    if num_comps != 1 && num_comps != 3 {
        return Err(Error::unsupported(format!(
            "HTJ2K: {num_comps} components — only 1 or 3 supported in round 3"
        )));
    }

    // Single-tile precondition: the only tile-part covers the full image.
    let tp = &cs.tile_parts[0];
    if tp.tile_index != 0 {
        return Err(Error::unsupported(
            "HTJ2K: only the single-tile case is supported in round 3",
        ));
    }
    let body_start = tp.sod_offset;
    let body_end = body_start + tp.sod_length;
    if body_end > buf.len() {
        return Err(Error::invalid(
            "jpeg2000: tile-part body extends past codestream",
        ));
    }
    let body = &buf[body_start..body_end];

    // Build per-component subband + cblk layout. Single-tile means the
    // tile-component rectangle equals the image extent divided by the
    // component sub-sampling.
    let mut all_planes_i32: Vec<Vec<i32>> = Vec::with_capacity(num_comps);
    let mut comp_sizes: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(num_comps);
    for c in &cs.siz.components {
        let xr = c.xrsiz as u32;
        let yr = c.yrsiz as u32;
        let cx1 = img_w.div_ceil(xr);
        let cy1 = img_h.div_ceil(yr);
        comp_sizes.push((0, 0, cx1, cy1));
    }

    let mut layouts: Vec<Vec<ResolutionLayout>> = Vec::with_capacity(num_comps);
    for &(x0, y0, x1, y1) in &comp_sizes {
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

    let num_res = (cod.num_decomp as usize) + 1;
    walk_packets_htj2k(body, &cod, &mut layouts, num_res, num_comps)?;

    // Reconstruct each component: HT-decode every included code-block,
    // place magnitudes into per-sub-band raster buffers, dequantise,
    // IDWT, level-shift, pack to u8.
    let comp_precisions: Vec<u32> = cs.siz.components.iter().map(|c| c.bit_depth()).collect();
    for (ci, &(_, _, cw, ch)) in comp_sizes.iter().enumerate() {
        let prec = comp_precisions[ci];
        let plane = synth_component_htj2k(
            &layouts[ci],
            num_res,
            cw as usize,
            ch as usize,
            &cod,
            &qcd,
            prec,
        )?;
        all_planes_i32.push(plane);
    }

    // Inverse RCT/ICT for 3-component streams when MCT is set.
    if num_comps == 3 && cod.mct == 1 {
        // Require matching dims (single-tile → enforced by SIZ sub-sampling).
        let n = all_planes_i32[0].len();
        if all_planes_i32[1].len() != n || all_planes_i32[2].len() != n {
            return Err(Error::invalid(
                "HTJ2K: MCT=1 requires matching component dimensions",
            ));
        }
        if cod.transform == 1 {
            apply_rct_inverse(&mut all_planes_i32, n);
        } else {
            let depth = cs.siz.components[0].bit_depth();
            apply_ict_inverse(&mut all_planes_i32, n, depth);
        }
    }

    let shifted = dc_shift_and_pack(&all_planes_i32, &comp_sizes, &cs.siz)?;

    let (pixel_format, planes) = match num_comps {
        1 => (
            PixelFormat::Gray8,
            vec![VideoPlane {
                stride: comp_sizes[0].2 as usize,
                data: shifted.into_iter().next().unwrap(),
            }],
        ),
        3 => {
            let c0 = &cs.siz.components[0];
            let c1 = &cs.siz.components[1];
            let c2 = &cs.siz.components[2];
            let same = c1.xrsiz == c2.xrsiz && c1.yrsiz == c2.yrsiz;
            let pf = if !same {
                PixelFormat::Yuv444P
            } else if c0.xrsiz == 1 && c1.xrsiz == 2 && c1.yrsiz == 2 {
                PixelFormat::Yuv420P
            } else if c0.xrsiz == 1 && c1.xrsiz == 2 && c1.yrsiz == 1 {
                PixelFormat::Yuv422P
            } else {
                PixelFormat::Yuv444P
            };
            let _ = pf;
            let planes = shifted
                .into_iter()
                .enumerate()
                .map(|(i, p)| VideoPlane {
                    stride: comp_sizes[i].2 as usize,
                    data: p,
                })
                .collect();
            (pf, planes)
        }
        _ => unreachable!(),
    };
    let _ = pixel_format;
    Ok(Frame::Video(VideoFrame { pts: None, planes }))
}

/// Walk the LRCP single-layer tier-2 stream, capturing each
/// codeblock's (length, byte-range) into the `CblkState` table the
/// classic decoder uses. This is byte-for-byte the same syntax as
/// classic Part-1; only the downstream interpretation of those bytes
/// changes.
#[allow(clippy::needless_range_loop)]
fn walk_packets_htj2k(
    body: &[u8],
    cod: &CodParams,
    layouts: &mut [Vec<ResolutionLayout>],
    num_res: usize,
    num_comps: usize,
) -> Result<()> {
    let mut cur = Cursor::new(body);
    // LRCP: outer layer (always 0 for single-layer), then resolution,
    // component, precinct. The loop reads / mutates `layouts[comp][resno]`
    // in spec order and isn't easily expressible as a chained iterator.
    for resno in 0..num_res {
        for comp in 0..num_comps {
            let nprec = layouts[comp][resno].precincts.len();
            for prec_idx in 0..nprec {
                parse_packet(&mut cur, 0, &mut layouts[comp][resno], prec_idx, cod)?;
            }
        }
    }
    Ok(())
}

fn parse_packet(
    cur: &mut Cursor<'_>,
    layer: u32,
    res: &mut ResolutionLayout,
    prec_idx: usize,
    cod: &CodParams,
) -> Result<()> {
    if cod.sop_marker && cur.remaining().starts_with(&[0xFF, 0x91]) {
        if cur.remaining().len() < 6 {
            return Err(Error::invalid("HTJ2K: truncated SOP"));
        }
        cur.consume(6)?;
    }
    let header_slice = cur.remaining();
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
                                return Err(Error::invalid("HTJ2K: missing-MSB tag tree runaway"));
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

/// Inflate one HTJ2K-coded component into a flat `Vec<i32>` of decoded
/// samples (still in the wavelet domain, pre-DC-shift).
#[allow(clippy::too_many_arguments)]
fn synth_component_htj2k(
    layouts: &[ResolutionLayout],
    num_res: usize,
    comp_w: usize,
    comp_h: usize,
    cod: &CodParams,
    qcd: &QcdParams,
    precision: u32,
) -> Result<Vec<i32>> {
    if cod.transform == 1 {
        synth_component_htj2k_53(layouts, num_res, comp_w, comp_h, qcd)
    } else {
        synth_component_htj2k_97(layouts, num_res, comp_w, comp_h, qcd, precision)
    }
}

#[allow(clippy::needless_range_loop)]
fn synth_component_htj2k_53(
    layouts: &[ResolutionLayout],
    num_res: usize,
    comp_w: usize,
    comp_h: usize,
    qcd: &QcdParams,
) -> Result<Vec<i32>> {
    let mut band_bufs: Vec<Vec<i32>> = Vec::with_capacity(num_res * 3 + 1);
    for resno in 0..num_res {
        let layout = &layouts[resno];
        for (sb_idx, sb) in layout.subbands.iter().enumerate() {
            let bw = (sb.x1 - sb.x0) as usize;
            let bh = (sb.y1 - sb.y0) as usize;
            let mut buf = vec![0i32; bw * bh];
            decode_subband_htj2k(layout, sb_idx, sb.band_idx, qcd, &mut buf, bw, bh, false)?;
            band_bufs.push(buf);
        }
    }
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

fn synth_component_htj2k_97(
    layouts: &[ResolutionLayout],
    num_res: usize,
    comp_w: usize,
    comp_h: usize,
    qcd: &QcdParams,
    precision: u32,
) -> Result<Vec<i32>> {
    // Round 3 only ships the reversible transform end-to-end. The 9/7
    // path needs the same per-band stepsize machinery as the classic
    // `synth_component_97` — left as a future-round task.
    let _ = (layouts, num_res, comp_w, comp_h, qcd, precision);
    Err(Error::unsupported(
        "HTJ2K: 9/7 irreversible transform not yet wired (round 4+)",
    ))
}

/// Decode every code-block of one sub-band via FBCOT and write the
/// signed sample magnitudes into `buf` (raster order, width `bw`).
///
/// `is_lossy` selects whether the magnitudes get an extra `>> 1` shift
/// (mirrors the classic 5/3 path's `v / 2`). For HTJ2K reversible
/// (5/3) the convention is the same: the cleanup pass output `μ` is
/// the "M_b-bit" magnitude and we keep parity with the encoder by
/// dividing by 2 — this matches `synth_component_53` in the classic
/// driver.
#[allow(clippy::too_many_arguments)]
fn decode_subband_htj2k(
    layout: &ResolutionLayout,
    sb_idx: usize,
    band_idx: usize,
    qcd: &QcdParams,
    buf: &mut [i32],
    bw: usize,
    _bh: usize,
    is_lossy: bool,
) -> Result<()> {
    let cblks = &layout.cblk_states[sb_idx];
    let cblks_w = layout.cblks_w[sb_idx];
    let cblks_h = layout.cblks_h[sb_idx];
    let sb = &layout.subbands[sb_idx];
    for cy in 0..cblks_h {
        for cx in 0..cblks_w {
            let idx = cy * cblks_w + cx;
            let st = &cblks[idx];
            if !st.included || st.total_passes == 0 || st.data.is_empty() {
                continue;
            }
            let (bx0, by0, bx1, by1) = layout.cblk_rects[sb_idx][idx];
            let w = (bx1 - bx0) as usize;
            let h = (by1 - by0) as usize;
            if w == 0 || h == 0 {
                continue;
            }
            let zblk = match st.total_passes {
                1 => ZBlk::One,
                2 => ZBlk::Two,
                3 => ZBlk::Three,
                _ => {
                    return Err(Error::unsupported(
                        "HTJ2K: more than 3 coding passes per code-block (round 4+)",
                    ));
                }
            };
            // For Z_blk == 1 the entire codeblock byte segment is the
            // cleanup segment; no refinement bytes. For Z_blk in
            // {2, 3} the segment also carries an HT refinement segment
            // appended after Lcup. Splitting Lcup vs Lref requires the
            // tier-2 termination model (one length per terminated
            // segment), which the encoder side hasn't produced for HTJ2K
            // yet — for now we surface multi-pass codeblocks as
            // Unsupported. The round-3 fixture exercises Z_blk = 1.
            let (dcup, dref): (&[u8], &[u8]) = match zblk {
                ZBlk::Zero => (&[][..], &[][..]),
                ZBlk::One => (&st.data[..], &[][..]),
                _ => {
                    return Err(Error::unsupported(
                        "HTJ2K: SigProp/MagRef refinement segments need tier-2 termination split (round 4+)",
                    ));
                }
            };
            let out = decode_codeblock(w as u32, h as u32, zblk, dcup, dref)?;

            // Compute the band's bit-depth M_b for sign reconstruction.
            // For HT cleanup, sample value = sign ? -mag : mag, then
            // shifted right by `M_b - bpno - 1` to land at the correct
            // bit-plane. For lossless 5/3 with `Z_blk = 1` the cleanup
            // covers all bit-planes so no extra shift is needed past
            // the missing-MSB accounting baked into the encoder.
            let (eps, _mant) = qcd.bands[band_idx];
            let band_numbps = qcd.guard_bits as i32 + eps as i32 - 1;
            let bpno = band_numbps + 1 - st.missing_msb as i32;
            if bpno < 1 {
                continue;
            }

            // Map quad-scan output → raster.
            let qw = (w as u32).div_ceil(2);
            let rel_x = (bx0 - sb.x0) as usize;
            let rel_y = (by0 - sb.y0) as usize;
            for ly in 0..h {
                for lx in 0..w {
                    let qx = (lx as u32) / 2;
                    let qy = (ly as u32) / 2;
                    let dx = (lx as u32) & 1;
                    let dy = (ly as u32) & 1;
                    let j = match (dx, dy) {
                        (0, 0) => 0,
                        (0, 1) => 1,
                        (1, 0) => 2,
                        (1, 1) => 3,
                        _ => unreachable!(),
                    };
                    let q = (qy as usize) * (qw as usize) + qx as usize;
                    let n = 4 * q + j;
                    let mag = out.mag[n] as i32;
                    let sign = out.sign[n];
                    let mut v = if sign != 0 { -mag } else { mag };
                    // The cleanup pass returns `μ_n` which is the
                    // unsigned bit-pattern at the band's number of bits
                    // M_b (counting the MSB as the implicit "1"). For
                    // 5/3 reversible the convention here matches the
                    // classic decoder which divides by 2 to undo the
                    // `oneplushalf` factor.
                    if is_lossy {
                        // 9/7 not yet wired; placeholder.
                    } else {
                        v >>= 1;
                    }
                    buf[(rel_y + ly) * bw + (rel_x + lx)] = v;
                }
            }
            let _ = (bpno, sb);
        }
    }
    Ok(())
}

/// 5/3 inverse RCT (T.800 §G.2.2). Same algorithm as the classic
/// frame driver — duplicated here so the HTJ2K module is self-contained.
fn apply_rct_inverse(planes: &mut [Vec<i32>], n: usize) {
    #[allow(clippy::needless_range_loop)]
    for i in 0..n {
        let y_v = planes[0][i];
        let y1 = planes[1][i];
        let y2 = planes[2][i];
        let g = y_v - ((y2 + y1) >> 2);
        let r = y2 + g;
        let b = y1 + g;
        planes[0][i] = r;
        planes[1][i] = g;
        planes[2][i] = b;
    }
}

/// 9/7 inverse ICT (T.800 §G.3.2). Same as the classic driver.
fn apply_ict_inverse(planes: &mut [Vec<i32>], n: usize, _luma_depth: u32) {
    #[allow(clippy::needless_range_loop)]
    for i in 0..n {
        let yf = planes[0][i] as f32;
        let y1 = planes[1][i] as f32;
        let y2 = planes[2][i] as f32;
        let r = yf + 1.402 * y2;
        let g = yf - 0.344_13 * y1 - 0.714_14 * y2;
        let b = yf + 1.772 * y1;
        planes[0][i] = r.round() as i32;
        planes[1][i] = g.round() as i32;
        planes[2][i] = b.round() as i32;
    }
}

/// Apply DC level shift + per-component bit-depth clip + pack to u8.
/// Mirrors the classic-decoder helper of the same name.
fn dc_shift_and_pack(
    component_planes: &[Vec<i32>],
    comp_sizes: &[(u32, u32, u32, u32)],
    siz: &Siz,
) -> Result<Vec<Vec<u8>>> {
    let mut shifted: Vec<Vec<u8>> = Vec::with_capacity(component_planes.len());
    for (i, plane) in component_planes.iter().enumerate() {
        let depth = siz.components[i].bit_depth();
        let signed = siz.components[i].is_signed();
        let shift = if signed { 0i32 } else { 1i32 << (depth - 1) };
        let max = ((1u32 << depth) - 1) as i32;
        let (_cx0, _cy0, cx1, cy1) = comp_sizes[i];
        let w = cx1 as usize;
        let h = cy1 as usize;
        let mut bytes = Vec::with_capacity(w * h);
        for &v in plane {
            let lv = v.saturating_add(shift).clamp(0, max);
            let scaled = if depth > 8 {
                (lv >> (depth - 8)) as u8
            } else if depth < 8 {
                ((lv << (8 - depth)) & 0xFF) as u8
            } else {
                lv as u8
            };
            bytes.push(scaled);
        }
        bytes.resize(w * h, 0);
        shifted.push(bytes);
    }
    Ok(shifted)
}

/// Internal byte-cursor copied from the classic tile decoder. Kept
/// private so the HTJ2K and classic drivers can evolve independently.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn remaining(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
    fn consume(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(Error::invalid(format!(
                "HTJ2K: tried to consume {n} bytes, only {} remain",
                self.buf.len() - self.pos
            )));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
}
