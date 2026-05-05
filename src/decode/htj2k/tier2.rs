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
use crate::error::{Jpeg2000Error as Error, Result};
use crate::image::{
    Jpeg2000Image, Jpeg2000PixelFormat as PixelFormat, Jpeg2000Plane as VideoPlane,
};

/// Compute `(num_tiles_x, num_tiles_y)` from SIZ. `XTsiz` / `YTsiz` are
/// the nominal tile dimensions on the reference grid; the picture spans
/// `[XOsiz, Xsiz)` × `[YOsiz, Ysiz)`.
fn tile_grid_dims_ht(siz: &Siz) -> Result<(u32, u32)> {
    if siz.xtsiz == 0 || siz.ytsiz == 0 {
        return Err(Error::invalid("HTJ2K: SIZ XTsiz or YTsiz is zero"));
    }
    let nx = (siz.xsiz - siz.xtosiz).div_ceil(siz.xtsiz);
    let ny = (siz.ysiz - siz.ytosiz).div_ceil(siz.ytsiz);
    Ok((nx, ny))
}

/// Reference-grid rectangle for tile `(p, q)` per T.800 §B.3.
fn tile_ref_rect_ht(siz: &Siz, p: u32, q: u32) -> (u32, u32, u32, u32) {
    let tx0 = (siz.xtosiz + p * siz.xtsiz).max(siz.xosiz);
    let ty0 = (siz.ytosiz + q * siz.ytsiz).max(siz.yosiz);
    let tx1 = (siz.xtosiz + (p + 1) * siz.xtsiz).min(siz.xsiz);
    let ty1 = (siz.ytosiz + (q + 1) * siz.ytsiz).min(siz.ysiz);
    (tx0, ty0, tx1, ty1)
}

#[inline]
fn div_ceil(a: u32, b: u32) -> u32 {
    if b == 0 {
        0
    } else {
        a.div_ceil(b)
    }
}

/// Decode an HTJ2K codestream end-to-end into a [`Jpeg2000Image`].
///
/// Mirrors the public [`crate::decode::frame::decode_frame`] entry
/// point but routes per-codeblock bytes through the FBCOT decoder
/// instead of the classic EBCOT MQ tier-1.
pub fn decode_frame_htj2k(cs: &Codestream, buf: &[u8]) -> Result<Jpeg2000Image> {
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
    // We accept any cblk_style for round 4 — the bytes in the body are
    // what they are; we trust the CAP-marker dispatch in `lib.rs` to gate
    // calls into this driver.
    if cs.tile_parts.is_empty() {
        return Err(Error::invalid("jpeg2000: no tile-parts in codestream"));
    }
    if cs.poc.is_some() || cs.tile_parts.iter().any(|tp| tp.poc.is_some()) {
        return Err(Error::unsupported("HTJ2K: POC progressions (round 5+)"));
    }
    if cod.num_layers != 1 {
        return Err(Error::unsupported(
            "HTJ2K: multi-layer codestreams (round 5+)",
        ));
    }
    if cod.progression_order != 0 {
        return Err(Error::unsupported(
            "HTJ2K: only LRCP progression supported through round 4",
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
            "HTJ2K: {num_comps} components — only 1 or 3 supported through round 4"
        )));
    }

    // Tile grid (§B.3). Round-4 supports multi-tile codestreams.
    let (num_tiles_x, num_tiles_y) = tile_grid_dims_ht(&cs.siz)?;
    let total_tiles = (num_tiles_x as u64) * (num_tiles_y as u64);
    if total_tiles == 0 {
        return Err(Error::invalid("HTJ2K: empty tile grid"));
    }
    if total_tiles > u16::MAX as u64 + 1 {
        return Err(Error::invalid("HTJ2K: tile count exceeds codestream limit"));
    }

    // Group tile-parts by tile index, preserving on-the-wire order.
    let mut by_tile: Vec<Vec<usize>> = vec![Vec::new(); total_tiles as usize];
    for (i, tp) in cs.tile_parts.iter().enumerate() {
        if (tp.tile_index as u64) >= total_tiles {
            return Err(Error::invalid(format!(
                "HTJ2K: SOT Isot={} exceeds tile grid ({} tiles)",
                tp.tile_index, total_tiles
            )));
        }
        by_tile[tp.tile_index as usize].push(i);
    }

    // PPM packed packet headers (T.800 §A.7.4): the main-header carries
    // a sequence of `(Nppm, Ippm)` records, one per tile-part of the
    // codestream in order. Pre-split into per-tile-part header byte runs.
    let ppm_per_tile_part: Option<Vec<Vec<u8>>> = if !cs.ppm.is_empty() {
        Some(parse_ppm_per_tile_part(&cs.ppm, cs.tile_parts.len())?)
    } else {
        None
    };

    let comp_precisions: Vec<u32> = cs.siz.components.iter().map(|c| c.bit_depth()).collect();

    // Pre-allocate each component's full-image plane.
    let comp_full_dims: Vec<(usize, usize)> = cs
        .siz
        .components
        .iter()
        .map(|c| {
            let xr = c.xrsiz as u32;
            let yr = c.yrsiz as u32;
            (img_w.div_ceil(xr) as usize, img_h.div_ceil(yr) as usize)
        })
        .collect();
    let mut image_planes: Vec<Vec<u8>> = comp_full_dims
        .iter()
        .map(|&(w, h)| vec![0u8; w * h])
        .collect();

    #[allow(clippy::needless_range_loop)]
    for tile_idx in 0..total_tiles as usize {
        if by_tile[tile_idx].is_empty() {
            // Tile missing from the codestream — leave zeros.
            continue;
        }
        let p = (tile_idx as u32) % num_tiles_x;
        let q = (tile_idx as u32) / num_tiles_x;
        let (tx0, ty0, tx1, ty1) = tile_ref_rect_ht(&cs.siz, p, q);

        // Per-component tile rectangle (component-grid).
        let mut comp_sizes_abs: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(num_comps);
        let mut comp_sizes_rel: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(num_comps);
        for c in &cs.siz.components {
            let xr = c.xrsiz as u32;
            let yr = c.yrsiz as u32;
            let cx0 = div_ceil(tx0, xr);
            let cy0 = div_ceil(ty0, yr);
            let cx1 = div_ceil(tx1, xr);
            let cy1 = div_ceil(ty1, yr);
            comp_sizes_abs.push((cx0, cy0, cx1, cy1));
            comp_sizes_rel.push((0, 0, cx1 - cx0, cy1 - cy0));
        }

        // Concatenate all tile-parts of this tile.
        let mut tile_body = Vec::new();
        for &tp_ix in &by_tile[tile_idx] {
            let tp = &cs.tile_parts[tp_ix];
            let start = tp.sod_offset;
            let end = start + tp.sod_length;
            if end > buf.len() {
                return Err(Error::invalid(
                    "HTJ2K: tile-part body extends past codestream",
                ));
            }
            tile_body.extend_from_slice(&buf[start..end]);
        }

        // Build the per-tile packet-header byte run when PPM or PPT is in
        // use. The body cursor still walks `tile_body` (which contains
        // packet bodies only, since the encoder routed headers into PPM/PPT).
        let tile_packet_headers: Option<Vec<u8>> = if let Some(per_tp) = &ppm_per_tile_part {
            let mut out = Vec::new();
            for &tp_ix in &by_tile[tile_idx] {
                if let Some(b) = per_tp.get(tp_ix) {
                    out.extend_from_slice(b);
                }
            }
            Some(out)
        } else {
            // PPT: aggregate all PPT segments (sorted by Zppt) of this
            // tile's tile-parts.
            let mut all_ppt: Vec<(u8, &[u8])> = Vec::new();
            for &tp_ix in &by_tile[tile_idx] {
                for ppt_seg in &cs.tile_parts[tp_ix].ppt {
                    if ppt_seg.is_empty() {
                        continue;
                    }
                    all_ppt.push((ppt_seg[0], &ppt_seg[1..]));
                }
            }
            if all_ppt.is_empty() {
                None
            } else {
                all_ppt.sort_by_key(|&(z, _)| z);
                let mut out = Vec::new();
                for (_, payload) in all_ppt {
                    out.extend_from_slice(payload);
                }
                Some(out)
            }
        };

        // Build per-component subband + cblk layout.
        let mut layouts: Vec<Vec<ResolutionLayout>> = Vec::with_capacity(num_comps);
        for &(x0, y0, x1, y1) in &comp_sizes_rel {
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
        walk_packets_htj2k(
            &tile_body,
            tile_packet_headers.as_deref(),
            &cod,
            &mut layouts,
            num_res,
            num_comps,
        )?;

        // Reconstruct each component for this tile.
        let mut tile_planes: Vec<Vec<i32>> = Vec::with_capacity(num_comps);
        for (ci, &(_, _, cw, ch)) in comp_sizes_rel.iter().enumerate() {
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
            tile_planes.push(plane);
        }

        // Inverse MCT (RCT/ICT) for 3-component streams when MCT is set.
        if num_comps == 3 && cod.mct == 1 {
            let n = tile_planes[0].len();
            if tile_planes[1].len() != n || tile_planes[2].len() != n {
                return Err(Error::invalid(
                    "HTJ2K: MCT=1 requires matching component dimensions",
                ));
            }
            if cod.transform == 1 {
                apply_rct_inverse(&mut tile_planes, n);
            } else {
                let depth = cs.siz.components[0].bit_depth();
                apply_ict_inverse(&mut tile_planes, n, depth);
            }
        }

        let shifted = dc_shift_and_pack(&tile_planes, &comp_sizes_rel, &cs.siz)?;

        // Stitch the tile's planes into the full image planes.
        for ci in 0..num_comps {
            let (cx0, cy0, cx1, cy1) = comp_sizes_abs[ci];
            let w = (cx1 - cx0) as usize;
            let h = (cy1 - cy0) as usize;
            let (full_w, _) = comp_full_dims[ci];
            let src = &shifted[ci];
            let dst = &mut image_planes[ci];
            for ly in 0..h {
                let dst_row = (cy0 as usize + ly) * full_w + cx0 as usize;
                let src_row = ly * w;
                dst[dst_row..dst_row + w].copy_from_slice(&src[src_row..src_row + w]);
            }
        }
    }

    let (pixel_format, planes) = match num_comps {
        1 => (
            PixelFormat::Gray8,
            vec![VideoPlane {
                stride: comp_full_dims[0].0,
                data: image_planes.remove(0),
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
            let planes = image_planes
                .into_iter()
                .enumerate()
                .map(|(i, p)| VideoPlane {
                    stride: comp_full_dims[i].0,
                    data: p,
                })
                .collect();
            (pf, planes)
        }
        _ => unreachable!(),
    };
    Ok(Jpeg2000Image {
        width: img_w,
        height: img_h,
        pixel_format,
        planes,
        pts: None,
    })
}

/// Split a PPM main-header payload run into per-tile-part header byte
/// runs. The PPM payload format per T.800 §A.7.4 is a sequence of `Zppm`
/// (1 byte) + concatenated `(Nppm: u32_BE, Ippm: Nppm bytes)` records,
/// one record per tile-part of the codestream in order.
///
/// `ppm_segments` are the raw per-segment payloads as surfaced by the
/// codestream parser (each still carrying its leading `Zppm` byte). We
/// sort by `Zppm`, strip it, concatenate the trailing bytes, then walk
/// `(Nppm, Ippm)` records. Mirrors `crate::decode::frame::unpack_ppm`.
fn parse_ppm_per_tile_part(
    ppm_segments: &[Vec<u8>],
    num_tile_parts: usize,
) -> Result<Vec<Vec<u8>>> {
    let mut sorted: Vec<&[u8]> = Vec::with_capacity(ppm_segments.len());
    {
        let mut tmp: Vec<(u8, &[u8])> = ppm_segments
            .iter()
            .map(|s| {
                if s.is_empty() {
                    (0u8, s.as_slice())
                } else {
                    (s[0], &s[1..])
                }
            })
            .collect();
        tmp.sort_by_key(|&(z, _)| z);
        for (_, body) in tmp {
            sorted.push(body);
        }
    }
    let mut payload: Vec<u8> = Vec::new();
    for body in sorted {
        payload.extend_from_slice(body);
    }
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(num_tile_parts);
    let mut cur = 0usize;
    while cur < payload.len() && out.len() < num_tile_parts {
        if cur + 4 > payload.len() {
            return Err(Error::invalid("HTJ2K: PPM truncated Nppm"));
        }
        let n = u32::from_be_bytes([
            payload[cur],
            payload[cur + 1],
            payload[cur + 2],
            payload[cur + 3],
        ]) as usize;
        cur += 4;
        if cur + n > payload.len() {
            return Err(Error::invalid("HTJ2K: PPM truncated Ippm"));
        }
        out.push(payload[cur..cur + n].to_vec());
        cur += n;
    }
    // Pad with empty entries if the codestream listed fewer headers than
    // tile-parts (legal — those tile-parts contribute no packets).
    while out.len() < num_tile_parts {
        out.push(Vec::new());
    }
    Ok(out)
}

/// Walk the LRCP single-layer tier-2 stream, capturing each
/// codeblock's (length, byte-range) into the `CblkState` table the
/// classic decoder uses. This is byte-for-byte the same syntax as
/// classic Part-1; only the downstream interpretation of those bytes
/// changes.
///
/// When `packet_headers` is `Some`, the bytes contain every packet
/// header for this tile in progression order; the body cursor `body`
/// then carries packet bodies only (PPM/PPT setup, T.800 §A.7.4 /
/// §A.7.5).
#[allow(clippy::needless_range_loop)]
fn walk_packets_htj2k(
    body: &[u8],
    packet_headers: Option<&[u8]>,
    cod: &CodParams,
    layouts: &mut [Vec<ResolutionLayout>],
    num_res: usize,
    num_comps: usize,
) -> Result<()> {
    let mut cur = Cursor::new(body);
    let mut header_cursor: Option<Cursor<'_>> = packet_headers.map(Cursor::new);
    // LRCP: outer layer (always 0 for single-layer), then resolution,
    // component, precinct. The loop reads / mutates `layouts[comp][resno]`
    // in spec order and isn't easily expressible as a chained iterator.
    for resno in 0..num_res {
        for comp in 0..num_comps {
            let nprec = layouts[comp][resno].precincts.len();
            for prec_idx in 0..nprec {
                parse_packet(
                    &mut cur,
                    header_cursor.as_mut(),
                    0,
                    &mut layouts[comp][resno],
                    prec_idx,
                    cod,
                )?;
            }
        }
    }
    Ok(())
}

fn parse_packet(
    cur: &mut Cursor<'_>,
    header_cursor: Option<&mut Cursor<'_>>,
    layer: u32,
    res: &mut ResolutionLayout,
    prec_idx: usize,
    cod: &CodParams,
) -> Result<()> {
    // SOP marker is part of the body stream (NOT the packed-headers
    // stream), so consume it from `cur` regardless of where headers live.
    if cod.sop_marker && cur.remaining().starts_with(&[0xFF, 0x91]) {
        if cur.remaining().len() < 6 {
            return Err(Error::invalid("HTJ2K: truncated SOP"));
        }
        cur.consume(6)?;
    }
    // Pick the bit-stream source for the packet header. When PPM/PPT is
    // in use the header bytes live in a separate stream (hdr_cur);
    // otherwise the body cursor itself supplies the header bits.
    let use_separate_hdr = header_cursor.is_some();
    let header_slice: &[u8] = if let Some(ref hc) = header_cursor {
        hc.remaining()
    } else {
        cur.remaining()
    };
    let mut bio = Bio::new(header_slice);
    // Per HTJ2K §B.3 a packet contribution to one code-block consists of
    // either 1 codeword segment (Z_blk = 1: cleanup only) or 2 codeword
    // segments (Z_blk in {2, 3}: cleanup terminates at pass index 0 ∈ T,
    // and the SigProp+MagRef refinement segment terminates at the last
    // included pass). We capture per-codeblock pending state as
    // `(sb_idx, g_idx, lcup, lref)`; `lref == 0` covers Z_blk = 1.
    let mut pending: Vec<(usize, usize, u32, u32)> = Vec::new();

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
                    // Per T.800 §B.10.7.2 "multiple codeword segments" +
                    // ISO/IEC 15444-15 §B.3, an HTJ2K packet contribution
                    // produces 1 codeword segment when num_passes == 1
                    // (cleanup only) and 2 codeword segments when
                    // num_passes ∈ {2, 3} — the cleanup terminates at
                    // pass index 0 ∈ T, the refinement segment at the
                    // last included pass. Each length field uses
                    // `Lblock + ⌊log2(passes_added_in_segment)⌋` bits.
                    //
                    //   N=1: K=1, lengths = [log2(1)] bits  → [Lblock]
                    //   N=2: K=2, lengths = [log2(1), log2(1)]
                    //   N=3: K=2, lengths = [log2(1), log2(2)]
                    let lblock = res.cblk_states[sb_idx][g_idx].lblock;
                    let (lcup, lref) = if num_passes <= 1 {
                        (bio.read(lblock + ilog2(num_passes)), 0u32)
                    } else {
                        // First segment: 1 pass added (the cleanup pass).
                        let l1 = bio.read(lblock + ilog2(1));
                        // Second segment: (num_passes - 1) passes added.
                        let l2 = bio.read(lblock + ilog2(num_passes - 1));
                        (l1, l2)
                    };
                    let st = &mut res.cblk_states[sb_idx][g_idx];
                    st.included = true;
                    st.total_passes += num_passes;
                    st.missing_msb = missing_msb;
                    pending.push((sb_idx, g_idx, lcup, lref));
                }
            }
        }
        bio.inalign();
    }
    let header_bytes_used = bio.numbytes_read();
    if let Some(hc) = header_cursor {
        hc.consume(header_bytes_used)?;
        if cod.eph_marker && hc.remaining().starts_with(&[0xFF, 0x92]) {
            hc.consume(2)?;
        }
    } else {
        cur.consume(header_bytes_used)?;
        if cod.eph_marker && cur.remaining().starts_with(&[0xFF, 0x92]) {
            cur.consume(2)?;
        }
    }
    let _ = use_separate_hdr;
    for (sb_idx, g_idx, lcup, lref) in pending {
        let cleanup_bytes = cur.consume(lcup as usize)?.to_vec();
        res.cblk_states[sb_idx][g_idx]
            .data
            .extend_from_slice(&cleanup_bytes);
        if lref > 0 {
            let ref_bytes = cur.consume(lref as usize)?.to_vec();
            res.cblk_states[sb_idx][g_idx]
                .data_ref
                .extend_from_slice(&ref_bytes);
        }
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

/// 9/7 irreversible HTJ2K synthesis.
///
/// Routes per-codeblock byte segments through the FBCOT decoder. The
/// cleanup / SigProp / MagRef passes produce a sample tuple
/// (μ_n, s_n, z_n, r_n) per T.814 §7.6, which `decode_subband_htj2k_97`
/// then converts to a float at the band's M_b grid (see Eq E-1) and
/// multiplies by the T.800 §E.1.1.2 stepsize
/// `(1 + mant/2048) * 2^(Rb - eps)` with `Rb = precision`, leaving the
/// per-band `log2_gain_b` to be recovered by the K / 2/K gain in the
/// 9/7 lifting (`dwt::idwt_97_1d`). NB: HTJ2K μ_n is a plain integer
/// at the M_b grid — there is no oneplushalf bit baked in, so the
/// scale is `stepsize`, not `0.5 * stepsize` like the classic Part-1
/// MQ synth path.
#[allow(clippy::needless_range_loop)]
fn synth_component_htj2k_97(
    layouts: &[ResolutionLayout],
    num_res: usize,
    comp_w: usize,
    comp_h: usize,
    qcd: &QcdParams,
    precision: u32,
) -> Result<Vec<i32>> {
    let mut band_bufs: Vec<Vec<f32>> = Vec::with_capacity(num_res * 3 + 1);
    for resno in 0..num_res {
        let layout = &layouts[resno];
        for (sb_idx, sb) in layout.subbands.iter().enumerate() {
            let bw = (sb.x1 - sb.x0) as usize;
            let bh = (sb.y1 - sb.y0) as usize;
            let mut buf = vec![0f32; bw * bh];
            decode_subband_htj2k_97(
                layout,
                sb_idx,
                sb.band_idx,
                qcd,
                &mut buf,
                bw,
                bh,
                precision,
            )?;
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
    Ok(ll.into_iter().map(|v| v.round() as i32).collect())
}

/// Decode every code-block of one sub-band via FBCOT and write the
/// signed integer sample magnitudes into `buf` (raster order, width
/// `bw`). Used by the 5/3 reversible synthesis path.
///
/// Per ISO/IEC 15444-15 §7.6 the *signed integer* sample value at the
/// band's M_b precision is reconstructed from the per-sample tuple
/// `(μ_n, s_n, z_n, r_n)` according to T.800 Eq E-1 with
/// `N_b = S_blk + 1 + z_n` and `MSB_i = bit(S_blk+1-i, μ_n)`:
///
/// ```text
///     q_b = (-1)^s · μ_extended · 2^(M_b - N_b)
/// ```
///
/// where `μ_extended = (μ_n << 1) | r_n` when `z_n = 1` (MagRef or
/// SigProp adds one extra LSB) and `μ_extended = μ_n` otherwise.
///
/// The per-block bit-plane shift `pblk = M_b - S_blk - 1` is exactly
/// what the round-6 cleanup decoder did NOT thread into the
/// reconstruction. The packet header's "missing-MSB" tag-tree value
/// is stored in `CblkState::missing_msb` after the off-by-one
/// threshold-loop convention shared with classic Part-1
/// (`leaf_value = field - 1`), so `S_blk = missing_msb - 1` and
/// `pblk = band_numbps - missing_msb` (since `M_b = band_numbps`).
///
/// For 5/3 reversible there is no Annex E reconstruction-r adjustment
/// (we honour Eq E-7 / E-8 with `r = 0`): `Rq_b = q_b`.
#[allow(clippy::too_many_arguments)]
fn decode_subband_htj2k(
    layout: &ResolutionLayout,
    sb_idx: usize,
    _band_idx: usize,
    qcd: &QcdParams,
    buf: &mut [i32],
    bw: usize,
    _bh: usize,
    _is_lossy: bool,
) -> Result<()> {
    let cblks = &layout.cblk_states[sb_idx];
    let cblks_w = layout.cblks_w[sb_idx];
    let cblks_h = layout.cblks_h[sb_idx];
    let sb = &layout.subbands[sb_idx];
    let (eps, _mant) = qcd.bands[sb.band_idx];
    let band_numbps = qcd.guard_bits as i32 + eps as i32 - 1; // M_b
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
                0 => ZBlk::Zero,
                1 => ZBlk::One,
                2 => ZBlk::Two,
                3 => ZBlk::Three,
                _ => {
                    return Err(Error::unsupported(
                        "HTJ2K: more than 3 coding passes per code-block (round 5+)",
                    ));
                }
            };
            let dcup: &[u8] = &st.data[..];
            let dref: &[u8] = &st.data_ref[..];
            let out = decode_codeblock(w as u32, h as u32, zblk, dcup, dref)?;

            // pblk = M_b - S_blk - 1, where S_blk = missing_msb - 1
            // owing to the threshold-loop off-by-one in
            // `walk_packets_htj2k` (shared with classic Part-1).
            let pblk = band_numbps - st.missing_msb as i32;
            let qw = (w as u32).div_ceil(2);
            let rel_x = (bx0 - sb.x0) as usize;
            let rel_y = (by0 - sb.y0) as usize;
            for ly in 0..h {
                for lx in 0..w {
                    let n = quad_scan_index(qw, lx as u32, ly as u32);
                    let mu = out.mag[n] as i64;
                    let sign = out.sign[n];
                    let z = out.z[n];
                    let r = out.refinement[n];
                    // μ_extended * 2^pblk_eff, with pblk_eff = pblk when
                    // z = 0 and pblk - 1 when z = 1 (the extra MagRef /
                    // SigProp LSB lives one plane below the cleanup
                    // bit-plane). For 5/3 reversible the encoder picks
                    // missing_msb so that pblk >= 0 in the cleanup-only
                    // case; pblk - 1 may equal -1 for z != 0, which
                    // means a half-step refinement that the integer
                    // 5/3 path just truncates (Eq E-7, r = 0).
                    let unsigned = if mu != 0 {
                        if z != 0 {
                            let mext = (mu << 1) | r as i64;
                            if pblk >= 1 {
                                mext << (pblk - 1)
                            } else {
                                mext >> (1 - pblk).max(0)
                            }
                        } else if pblk >= 0 {
                            mu << pblk
                        } else {
                            mu >> (-pblk)
                        }
                    } else if z != 0 {
                        let v = r as i64;
                        if pblk >= 1 {
                            v << (pblk - 1)
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let v = if sign != 0 { -unsigned } else { unsigned };
                    buf[(rel_y + ly) * bw + (rel_x + lx)] = v as i32;
                }
            }
            let _ = sb;
        }
    }
    Ok(())
}

/// 9/7 irreversible variant of [`decode_subband_htj2k`]. Same FBCOT +
/// μ/r/z reconstruction, but emits dequantised float samples per
/// T.800 §E.1.1.2.
///
/// The cleanup pass emits magnitudes `μ_n` whose LSB sits at the
/// significant bit-plane `S_blk = missing_msb − 1` (per T.814 §7.3.4
/// Figure 4 + the threshold-loop convention shared with classic
/// Part-1). To map them onto the band's M_b grid where the dequant
/// stepsize `Δ_b` is meaningful, every sample must be left-shifted
/// by `pblk = M_b − S_blk − 1 = band_numbps − missing_msb` (T.800
/// Eq E-1 with `N_b = S_blk + 1 + z_n`). When MagRef / SigProp
/// contribute an extra LSB (`z_n = 1`), `μ_extended = (μ_n << 1) | r_n`
/// already carries a factor of 2, so the effective shift drops to
/// `pblk − 1`. Float arithmetic preserves the half-step refinement
/// (`pblk − 1 = −1` ⇒ multiplicative 0.5) that the integer 5/3 path
/// has to truncate (Eq E-7, r = 0).
#[allow(clippy::too_many_arguments)]
fn decode_subband_htj2k_97(
    layout: &ResolutionLayout,
    sb_idx: usize,
    band_idx: usize,
    qcd: &QcdParams,
    buf: &mut [f32],
    bw: usize,
    _bh: usize,
    precision: u32,
) -> Result<()> {
    let cblks = &layout.cblk_states[sb_idx];
    let cblks_w = layout.cblks_w[sb_idx];
    let cblks_h = layout.cblks_h[sb_idx];
    let sb = &layout.subbands[sb_idx];
    let (eps, mant) = qcd.bands[band_idx];
    // Stepsize per T.800 Eq E-3, with `Rb = precision` so the per-band
    // log2_gain factor is recovered by the IDWT lifting scale (matches
    // the classic Part-1 9/7 synth path). The dequantised float for an
    // integer `q_b` at the M_b grid is then `q_b * stepsize`.
    //
    // NOTE: the classic Part-1 MQ-coded magnitude carries an implicit
    // half-step (oneplushalf, +0.5) bit, so its synth path multiplies
    // by `0.5 * stepsize`. HTJ2K's μ_n (T.814 §7.6) is a plain integer
    // at the M_b grid — no half-step is folded in — so the multiplier
    // here is `stepsize`, not `0.5 * stepsize`.
    let rb = precision as i32;
    let stepsize = (1.0f64 + (mant as f64) / 2048.0) * 2f64.powi(rb - eps as i32);
    let scale = stepsize;
    let band_numbps = qcd.guard_bits as i32 + eps as i32 - 1; // M_b

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
                0 => ZBlk::Zero,
                1 => ZBlk::One,
                2 => ZBlk::Two,
                3 => ZBlk::Three,
                _ => {
                    return Err(Error::unsupported(
                        "HTJ2K: more than 3 coding passes per code-block (round 5+)",
                    ));
                }
            };
            let dcup: &[u8] = &st.data[..];
            let dref: &[u8] = &st.data_ref[..];
            let out = decode_codeblock(w as u32, h as u32, zblk, dcup, dref)?;

            // pblk = M_b − S_blk − 1 = band_numbps − missing_msb (same
            // identity as the 5/3 path; see `decode_subband_htj2k`).
            let pblk = band_numbps - st.missing_msb as i32;
            let qw = (w as u32).div_ceil(2);
            let rel_x = (bx0 - sb.x0) as usize;
            let rel_y = (by0 - sb.y0) as usize;
            for ly in 0..h {
                for lx in 0..w {
                    let n = quad_scan_index(qw, lx as u32, ly as u32);
                    let mu = out.mag[n];
                    let sign = out.sign[n];
                    let z = out.z[n];
                    let r = out.refinement[n];
                    let signed_mb = mb_grid_value_97(mu, sign, z, r, pblk);
                    let dequant = signed_mb * scale;
                    buf[(rel_y + ly) * bw + (rel_x + lx)] = dequant as f32;
                }
            }
        }
    }
    Ok(())
}

/// Reconstruct the signed value of one HTJ2K sample at the band's
/// **M_b grid** (as a float) per T.800 Eq E-1 with the FBCOT
/// (`μ_n`, `s_n`, `z_n`, `r_n`) tuple.
///
/// `pblk = M_b − S_blk − 1 = band_numbps − missing_msb` is the
/// per-codeblock left-shift required to align the cleanup magnitude
/// (whose LSB sits at S_blk) onto the M_b grid where the dequant
/// stepsize Δ_b is meaningful (Eq E-3). When MagRef / SigProp
/// contribute an extra LSB (`z_n = 1`), the extended magnitude
/// `μ_extended = (μ_n << 1) | r_n` already carries a factor of 2, so
/// the effective shift drops to `pblk − 1`. Float arithmetic preserves
/// the half-step refinement (`pblk − 1 = −1` ⇒ multiplicative 0.5)
/// that the integer 5/3 path has to truncate (Eq E-7, r = 0).
///
/// pblk can be negative when the encoder picks `missing_msb > M_b`
/// — the cleanup magnitude is then truncated by `−pblk` LSBs, which
/// is a bit-rate / quality trade the encoder owns. We honour the
/// sign of `pblk` symmetrically on both branches.
#[inline]
pub(crate) fn mb_grid_value_97(mu: u64, sign: u8, z: u8, r: u8, pblk: i32) -> f64 {
    let unsigned_mb: f64 = if mu != 0 {
        if z != 0 {
            let mext = ((mu << 1) | r as u64) as f64;
            mext * 2.0f64.powi(pblk - 1)
        } else {
            (mu as f64) * 2.0f64.powi(pblk)
        }
    } else if z != 0 {
        (r as f64) * 2.0f64.powi(pblk - 1)
    } else {
        0.0
    };
    if sign != 0 {
        -unsigned_mb
    } else {
        unsigned_mb
    }
}

/// Map (lx, ly) inside a code-block into the FBCOT quad-scan sample
/// index `n = 4q + j`.
#[inline]
fn quad_scan_index(qw: u32, lx: u32, ly: u32) -> usize {
    let qx = lx / 2;
    let qy = ly / 2;
    let dx = lx & 1;
    let dy = ly & 1;
    let j = match (dx, dy) {
        (0, 0) => 0,
        (0, 1) => 1,
        (1, 0) => 2,
        (1, 1) => 3,
        _ => unreachable!(),
    };
    let q = (qy as usize) * (qw as usize) + qx as usize;
    4 * q + j
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

#[cfg(test)]
mod tests {
    //! Direct exercises of the per-codeblock M_b-grid reconstruction
    //! formula on the 9/7 float path (`mb_grid_value_97`).
    //!
    //! The fixture-driven `htj2k_lossy97_*` interop tests only ever
    //! hit `pblk == 0` because OpenJPH (the binary used to author the
    //! fixtures) emits one cleanup pass per codeblock and sets
    //! `missing_msb = M_b` — making `pblk = M_b − missing_msb = 0`.
    //! The pblk > 0 / pblk < 0 / z = 1 cases are therefore checked
    //! here as direct calls into the helper, anchored on T.800 Eq E-1
    //! and T.814 §7.6 / Figure 4.
    use super::mb_grid_value_97;

    /// pblk = 0, z = 0, sign = 0: the cleanup magnitude is the M_b
    /// value verbatim (no shift).
    #[test]
    fn pblk0_z0_is_identity() {
        let v = mb_grid_value_97(7, 0, 0, 0, 0);
        assert_eq!(v, 7.0);
    }

    /// pblk = 0, z = 0, sign = 1: M_b value is negated.
    #[test]
    fn pblk0_z0_sign_flips() {
        let v = mb_grid_value_97(7, 1, 0, 0, 0);
        assert_eq!(v, -7.0);
    }

    /// pblk > 0, z = 0: cleanup magnitude left-shifted by `pblk`.
    #[test]
    fn pblk_positive_left_shifts_cleanup() {
        // M_b = 13, missing_msb = 11 ⇒ pblk = 2, μ = 5.
        // Expected: 5 * 2^2 = 20.
        let v = mb_grid_value_97(5, 0, 0, 0, 2);
        assert_eq!(v, 20.0);
    }

    /// pblk > 0, z = 1: extended magnitude `(μ << 1) | r` left-shifted
    /// by `pblk − 1`. Verifies that the SigProp/MagRef LSB lives one
    /// plane below the cleanup plane (T.800 Eq E-1 with N_b = S_blk +
    /// 1 + z_n).
    #[test]
    fn pblk_positive_z1_uses_extended_mag() {
        // μ = 5, r = 1 ⇒ μ_ext = (5 << 1) | 1 = 11.
        // pblk = 2 ⇒ shift = 1 ⇒ 11 * 2 = 22.
        let v = mb_grid_value_97(5, 0, 1, 1, 2);
        assert_eq!(v, 22.0);
    }

    /// pblk = 0, z = 1: half-step refinement survives in float as 0.5
    /// multiplier (the integer 5/3 path truncates this case to 0).
    #[test]
    fn pblk0_z1_preserves_half_step_in_float() {
        // μ = 3, r = 1 ⇒ μ_ext = 7. shift = -1 ⇒ 7 * 0.5 = 3.5.
        let v = mb_grid_value_97(3, 0, 1, 1, 0);
        assert_eq!(v, 3.5);
    }

    /// μ = 0, z = 1: r contributes a half-step LSB even when the
    /// cleanup magnitude was zero (newly-significant via SigProp).
    #[test]
    fn mu_zero_z1_emits_half_step_lsb() {
        let v = mb_grid_value_97(0, 0, 1, 1, 1);
        // r=1 -> 1 * 2^0 = 1.0
        assert_eq!(v, 1.0);
        // pblk = 0, half-step:
        let v = mb_grid_value_97(0, 0, 1, 1, 0);
        assert_eq!(v, 0.5);
        // pblk = 0, r = 0: zero.
        let v = mb_grid_value_97(0, 0, 1, 0, 0);
        assert_eq!(v, 0.0);
    }

    /// pblk < 0 (encoder picks missing_msb > M_b): negative shift —
    /// i.e. multiply by 2^pblk = a fraction. The float path keeps
    /// the precise value where the integer path would truncate.
    #[test]
    fn pblk_negative_shrinks_magnitude() {
        // μ = 8, pblk = −1 ⇒ 8 * 0.5 = 4.0
        let v = mb_grid_value_97(8, 0, 0, 0, -1);
        assert_eq!(v, 4.0);
    }

    /// Multi-band sweep: a representative span of (μ, z, r, sign,
    /// pblk) tuples that the 9/7 path encounters when decoding a
    /// real multi-decomposition codestream. Each tuple is checked
    /// against the closed-form Eq E-1 expectation.
    #[test]
    fn multi_band_sweep_matches_eq_e1() {
        // (mu, sign, z, r, pblk, expected)
        let cases: &[(u64, u8, u8, u8, i32, f64)] = &[
            (1, 0, 0, 0, 0, 1.0),
            (1, 1, 0, 0, 0, -1.0),
            (1, 0, 0, 0, 3, 8.0),  // 1 << 3
            (3, 0, 0, 0, 4, 48.0), // 3 << 4
            (3, 1, 0, 0, 4, -48.0),
            (3, 0, 1, 0, 4, 48.0), // (6 << 3) = 48
            (3, 0, 1, 1, 4, 56.0), // (7 << 3) = 56
            (0, 0, 1, 1, 4, 8.0),  // 1 << 3
            (0, 0, 0, 0, 4, 0.0),
            (15, 0, 0, 0, 0, 15.0),
            (15, 0, 1, 1, 0, 15.5), // (31) * 0.5
        ];
        for &(mu, sign, z, r, pblk, expected) in cases {
            let v = mb_grid_value_97(mu, sign, z, r, pblk);
            assert!(
                (v - expected).abs() < 1e-9,
                "tuple (μ={mu}, s={sign}, z={z}, r={r}, pblk={pblk}) → {v}, expected {expected}",
            );
        }
    }
}
