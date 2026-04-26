//! Public frame decoder entry point: take a parsed codestream and the
//! raw tile-part body bytes, return a fully decoded `oxideav_core::Frame`.
//!
//! Multi-tile decode
//! -----------------
//!
//! A JPEG 2000 image may be split into a rectangular grid of tiles
//! (T.800 §B.3). Each tile is coded independently — its own tile-parts,
//! its own quantisation / coding style when COC / QCC are present, and
//! its own RCT / ICT component transform (§G.1, §G.2). The decoder here:
//!
//! 1. Walks the tile grid using the SIZ parameters
//!    (`XTsiz`, `YTsiz`, `XTOsiz`, `YTOsiz`, `Xsiz`, `Ysiz`). Tile
//!    `(p, q)` covers the reference-grid rectangle
//!    `tx0 = max(XTOsiz + p*XTsiz, XOsiz)`,
//!    `ty0 = max(YTOsiz + q*YTsiz, YOsiz)`,
//!    `tx1 = min(XTOsiz + (p+1)*XTsiz, Xsiz)`,
//!    `ty1 = min(YTOsiz + (q+1)*YTsiz, Ysiz)`.
//!    The per-component tile rectangle is obtained by
//!    dividing with ceiling by that component's `XRsiz` / `YRsiz`.
//!    See §B.3.
//!
//! 2. Concatenates all tile-parts of the same tile index (ordered by
//!    `TPsot`) into a single body buffer (§A.4).
//!
//! 3. Runs the per-tile decode (tier-2 → tier-1 → IDWT → DC shift →
//!    inverse RCT / ICT) and pastes the resulting component samples into
//!    the pre-allocated image plane at the tile's component-local offset.
//!
//! Tiles that have no tile-parts in the codestream are left untouched
//! (filled with zeros by the initial allocation). A codestream must
//! otherwise contain at least one tile-part for at least one tile —
//! an empty codestream is rejected.

use oxideav_core::{Error, Frame, PixelFormat, Result, VideoFrame, VideoPlane};

use super::tile::{
    decode_tile_with_params, parse_cod, parse_poc, parse_qcd, CodParams, DecodeParams, PocParams,
};
use crate::codestream::{Codestream, Siz};

/// Decode one JPEG 2000 still into an uncompressed video frame.
pub fn decode_frame(cs: &Codestream, buf: &[u8]) -> Result<Frame> {
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
    // Main-header POC (T.800 §A.6.6) — applies to every tile unless the
    // tile-part header carries its own POC override.
    let main_poc: Option<PocParams> = if let Some(b) = &cs.poc {
        Some(parse_poc(b, cs.siz.components.len() as u16)?)
    } else {
        None
    };

    // PPM packed packet headers from the main header (T.800 §A.7.4).
    // When present, every tile's packet headers live in PPM rather
    // than in the tile-part bodies. We unpack the (Zppm-sorted,
    // concatenated) PPM stream into one packet-header buffer per
    // tile-part, indexed in the order tile-parts appear in the
    // codestream.
    let ppm_per_tile_part: Option<Vec<Vec<u8>>> = if !cs.ppm.is_empty() {
        Some(unpack_ppm(&cs.ppm, cs.tile_parts.len())?)
    } else {
        None
    };

    if cs.tile_parts.is_empty() {
        return Err(Error::invalid("jpeg2000: no tile-parts in codestream"));
    }

    let img_w = cs.siz.image_width();
    let img_h = cs.siz.image_height();
    let num_comps = cs.siz.components.len();
    if num_comps == 0 {
        return Err(Error::invalid("jpeg2000: SIZ has zero components"));
    }

    // Per-component full-image dimensions (see §B.2: "Xsiz and Ysiz,
    // divided with ceiling by the component sub-sampling factors").
    let full_comp_dims: Vec<(usize, usize)> = cs
        .siz
        .components
        .iter()
        .map(|c| {
            (
                div_ceil(img_w, c.xrsiz as u32) as usize,
                div_ceil(img_h, c.yrsiz as u32) as usize,
            )
        })
        .collect();

    // Pre-allocate the final (post-RCT / ICT, DC-shifted, 8-bit-packed)
    // component planes at full image size.
    let mut image_planes: Vec<Vec<u8>> = full_comp_dims
        .iter()
        .map(|&(w, h)| vec![0u8; w * h])
        .collect();

    // Tile grid (§B.3).
    let (num_tiles_x, num_tiles_y) = tile_grid_dims(&cs.siz)?;
    let total_tiles = (num_tiles_x as u64) * (num_tiles_y as u64);
    if total_tiles == 0 {
        return Err(Error::invalid("jpeg2000: empty tile grid"));
    }
    if total_tiles > u16::MAX as u64 + 1 {
        return Err(Error::invalid(
            "jpeg2000: tile count exceeds codestream limit",
        ));
    }

    // Group tile-parts by tile index, preserving on-the-wire order —
    // which §A.4 requires matches TPsot ordering.
    let mut by_tile: Vec<Vec<usize>> = vec![Vec::new(); total_tiles as usize];
    for (i, tp) in cs.tile_parts.iter().enumerate() {
        if (tp.tile_index as u64) >= total_tiles {
            return Err(Error::invalid(format!(
                "jpeg2000: SOT Isot={} exceeds tile grid ({} tiles)",
                tp.tile_index, total_tiles
            )));
        }
        by_tile[tp.tile_index as usize].push(i);
    }

    let comp_precisions: Vec<u32> = cs.siz.components.iter().map(|c| c.bit_depth()).collect();

    #[allow(clippy::needless_range_loop)]
    for tile_idx in 0..total_tiles as usize {
        if by_tile[tile_idx].is_empty() {
            // Tile missing from the codestream — leave zeros (this matches
            // the spec requirement that decoders tolerate out-of-order
            // tile-parts, though in practice a fully-coded image has
            // every tile present).
            continue;
        }
        let p = (tile_idx as u32) % num_tiles_x;
        let q = (tile_idx as u32) / num_tiles_x;

        // Reference-grid rectangle for this tile.
        let (tx0, ty0, tx1, ty1) = tile_ref_rect(&cs.siz, p, q);

        // Per-component tile rectangles in component coordinates
        // (§B.3 — equivalent to scaling down the reference-grid tile
        // rect by the component sub-sampling).
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
            // The tile decoder treats the rectangle as tile-local —
            // `(x0, y0)` is the tile's own origin, `(x1, y1)` its extent.
            // We pass absolute-from-zero coordinates equal to the tile
            // size so the sub-band / code-block layout is built correctly
            // for the tile. This preserves the original single-tile
            // behaviour, where the tile equalled the whole image.
            comp_sizes_rel.push((0, 0, cx1 - cx0, cy1 - cy0));
        }

        // Concatenate all tile-parts of this tile (already in on-the-wire
        // order because we push in parser-emit order). §A.4.2 requires
        // tile-parts to appear in TPsot order within the codestream.
        let mut tile_body = Vec::new();
        for &tp_ix in &by_tile[tile_idx] {
            let tp = &cs.tile_parts[tp_ix];
            let start = tp.sod_offset;
            let end = start + tp.sod_length;
            if end > buf.len() {
                return Err(Error::invalid(
                    "jpeg2000: tile-part body extends past codestream",
                ));
            }
            tile_body.extend_from_slice(&buf[start..end]);
        }

        // Per-tile POC override (T.800 §A.6.6). The spec allows POC
        // marker segments to appear in any tile-part header of a tile
        // (the first segment must precede the first packet of the
        // affected progression). We aggregate every POC found in the
        // tile's tile-parts into a single concatenated progression list.
        // Per the precedence rule "Tile-part POC > Main POC > Tile-part
        // COD > Main COD" any tile-part POC fully overrides the main POC.
        let mut tile_poc_bytes: Vec<u8> = Vec::new();
        for &tp_ix in &by_tile[tile_idx] {
            if let Some(b) = &cs.tile_parts[tp_ix].poc {
                tile_poc_bytes.extend_from_slice(b);
            }
        }
        let tile_poc: Option<PocParams> = if !tile_poc_bytes.is_empty() {
            Some(parse_poc(&tile_poc_bytes, cs.siz.components.len() as u16)?)
        } else {
            main_poc.clone()
        };

        // Per-tile packet headers from PPM or PPT (T.800 §A.7.4 / §A.7.5).
        // When PPM is present, concatenate the per-tile-part chunks in
        // the order this tile's tile-parts appear in the codestream.
        // Else if any tile-part carries PPT segments, sort the PPT
        // payloads by Zppt across all tile-parts of this tile and
        // concatenate Ippt bodies. Else, no packed headers — the body
        // contains its own headers (the historical case).
        let tile_packet_headers: Option<Vec<u8>> = if let Some(per_tp) = &ppm_per_tile_part {
            let mut buf = Vec::new();
            for &tp_ix in &by_tile[tile_idx] {
                if let Some(b) = per_tp.get(tp_ix) {
                    buf.extend_from_slice(b);
                }
            }
            Some(buf)
        } else {
            // Aggregate PPT segments. Spec: "The sequence of (Ippti)
            // parameters from this marker segment is concatenated, in
            // the order of increasing Zppt". We aggregate PPTs from all
            // tile-parts of this tile, sorted by Zppt within each
            // tile-part header (the spec doesn't explicitly say PPTs
            // can span tile-parts, but it's safe to walk in tile-part
            // order then by Zppt).
            let mut all_ppt: Vec<(u8, &[u8])> = Vec::new();
            for &tp_ix in &by_tile[tile_idx] {
                for ppt_seg in &cs.tile_parts[tp_ix].ppt {
                    if ppt_seg.is_empty() {
                        continue;
                    }
                    let zppt = ppt_seg[0];
                    all_ppt.push((zppt, &ppt_seg[1..]));
                }
            }
            if all_ppt.is_empty() {
                None
            } else {
                all_ppt.sort_by_key(|&(z, _)| z);
                let mut buf = Vec::new();
                for (_, body) in all_ppt {
                    buf.extend_from_slice(body);
                }
                Some(buf)
            }
        };

        let params = DecodeParams {
            comp_precisions: &comp_precisions,
            poc: tile_poc.as_ref(),
            packet_headers: tile_packet_headers.as_deref(),
        };
        let mut planes = decode_tile_with_params(&tile_body, &comp_sizes_rel, &cod, &qcd, &params)?;

        // Per spec T.800 §G.1 (Figure G.1) the decoder MUST apply the
        // inverse component transform (RCT / ICT) BEFORE the inverse DC
        // level shift — the inverse RCT operates on the signed
        // (un-shifted) wavelet output, then the inverse DC level shift
        // is applied to the unsigned components only. Doing it the
        // other way round (a) clips the chroma excursions of the RCT
        // to ±128 (Y1, Y2 are signed and can reach ±255 for 8-bit
        // input — see G.2.1 NOTE) and (b) applies +2^(Ssiz) to the
        // chroma planes which the spec never asks for.
        apply_per_tile_mct_i32(&mut planes, &comp_sizes_rel, &cod, &cs.siz)?;
        let shifted = dc_shift_and_pack(&planes, &comp_sizes_rel, &cs.siz)?;

        for ci in 0..num_comps {
            let (cx0, cy0, cx1, cy1) = comp_sizes_abs[ci];
            let w = (cx1 - cx0) as usize;
            let h = (cy1 - cy0) as usize;
            let (full_w, _) = full_comp_dims[ci];
            let src = &shifted[ci];
            let dst = &mut image_planes[ci];
            for ly in 0..h {
                let dst_row = (cy0 as usize + ly) * full_w + cx0 as usize;
                let src_row = ly * w;
                dst[dst_row..dst_row + w].copy_from_slice(&src[src_row..src_row + w]);
            }
        }
    }

    // Assemble VideoFrame. Accept 1 (gray) or 3 (YUV / RGB) components.
    let (pixel_format, planes) = match num_comps {
        1 => (
            PixelFormat::Gray8,
            vec![VideoPlane {
                stride: full_comp_dims[0].0,
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
                    stride: full_comp_dims[i].0,
                    data: p,
                })
                .collect();
            (pf, planes)
        }
        n => {
            return Err(Error::unsupported(format!(
                "jpeg2000: {} components — only 1 or 3 supported",
                n
            )))
        }
    };

    let _ = pixel_format;
    let _ = img_w;
    let _ = img_h;
    Ok(Frame::Video(VideoFrame { pts: None, planes }))
}

/// Number of tiles in the grid along X and Y. T.800 §B.3:
///
/// ```text
///   numXtiles = ceil( (Xsiz - XTOsiz) / XTsiz )
///   numYtiles = ceil( (Ysiz - YTOsiz) / YTsiz )
/// ```
fn tile_grid_dims(siz: &Siz) -> Result<(u32, u32)> {
    if siz.xtsiz == 0 || siz.ytsiz == 0 {
        return Err(Error::invalid("jpeg2000: SIZ XTsiz or YTsiz is zero"));
    }
    if siz.xtosiz > siz.xsiz || siz.ytosiz > siz.ysiz {
        return Err(Error::invalid("jpeg2000: SIZ tile origin past image"));
    }
    let nx = div_ceil(siz.xsiz - siz.xtosiz, siz.xtsiz);
    let ny = div_ceil(siz.ysiz - siz.ytosiz, siz.ytsiz);
    Ok((nx.max(1), ny.max(1)))
}

/// Reference-grid rectangle for tile (p, q). T.800 §B.3:
///
/// ```text
///   tx0 = max(XTOsiz + p*XTsiz, XOsiz)
///   ty0 = max(YTOsiz + q*YTsiz, YOsiz)
///   tx1 = min(XTOsiz + (p+1)*XTsiz, Xsiz)
///   ty1 = min(YTOsiz + (q+1)*YTsiz, Ysiz)
/// ```
fn tile_ref_rect(siz: &Siz, p: u32, q: u32) -> (u32, u32, u32, u32) {
    let tx0 = (siz.xtosiz + p * siz.xtsiz).max(siz.xosiz);
    let ty0 = (siz.ytosiz + q * siz.ytsiz).max(siz.yosiz);
    let tx1 = (siz.xtosiz + (p + 1) * siz.xtsiz).min(siz.xsiz);
    let ty1 = (siz.ytosiz + (q + 1) * siz.ytsiz).min(siz.ysiz);
    (tx0, ty0, tx1, ty1)
}

fn div_ceil(a: u32, b: u32) -> u32 {
    if b == 0 {
        return 0;
    }
    a.div_ceil(b)
}

/// Unpack the main-header PPM segments (T.800 §A.7.4) into a
/// per-tile-part packet-header buffer.
///
/// PPM payload layout (across all PPM segments, sorted by Zppm and
/// concatenated):
///
/// ```text
///   [Nppm_0 (4 bytes)] [packet headers for tile-part 0 — Nppm_0 bytes]
///   [Nppm_1 (4 bytes)] [packet headers for tile-part 1 — Nppm_1 bytes]
///   ...
/// ```
///
/// The `Nppm` boundaries are not aligned with PPM segment boundaries —
/// a single packet-header block may span PPM segments. The spec's
/// ordering rule ("kth entry in the resulting list contains the number
/// of bytes and packet headers for the kth tile-part appearing in the
/// codestream") tells us tile-part indices in the unpacked output
/// correspond directly to the order tile-parts appear in the SOT chain.
fn unpack_ppm(ppm_segments: &[Vec<u8>], num_tile_parts: usize) -> Result<Vec<Vec<u8>>> {
    // Sort by Zppm (first byte of each segment payload).
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

    // Concatenate the trailing bytes (after Zppm) into one stream,
    // then walk the (Nppm, headers) records.
    let mut stream: Vec<u8> = Vec::new();
    for body in sorted {
        stream.extend_from_slice(body);
    }

    let mut out: Vec<Vec<u8>> = Vec::with_capacity(num_tile_parts);
    let mut i = 0usize;
    while i < stream.len() && out.len() < num_tile_parts {
        if i + 4 > stream.len() {
            return Err(Error::invalid(
                "jpeg2000: PPM stream truncated reading Nppm",
            ));
        }
        let n =
            u32::from_be_bytes([stream[i], stream[i + 1], stream[i + 2], stream[i + 3]]) as usize;
        i += 4;
        if i + n > stream.len() {
            return Err(Error::invalid(format!(
                "jpeg2000: PPM stream truncated: need {n} bytes, have {}",
                stream.len() - i
            )));
        }
        out.push(stream[i..i + n].to_vec());
        i += n;
    }
    while out.len() < num_tile_parts {
        // Fewer Nppm entries than tile-parts — leave the rest empty so
        // the per-tile-part lookup falls back gracefully.
        out.push(Vec::new());
    }
    Ok(out)
}

/// Apply DC level shift + per-component bit-depth clip, then pack to 8-bit.
/// Operates on a single tile's components (sized per `comp_sizes_rel`).
fn dc_shift_and_pack(
    component_planes: &[Vec<i32>],
    comp_sizes_rel: &[(u32, u32, u32, u32)],
    siz: &Siz,
) -> Result<Vec<Vec<u8>>> {
    let mut shifted: Vec<Vec<u8>> = Vec::with_capacity(component_planes.len());
    for (i, plane) in component_planes.iter().enumerate() {
        let depth = siz.components[i].bit_depth();
        let signed = siz.components[i].is_signed();
        let shift = if signed { 0i32 } else { 1i32 << (depth - 1) };
        let max = ((1u32 << depth) - 1) as i32;
        let (_cx0, _cy0, cx1, cy1) = comp_sizes_rel[i];
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

/// Apply the inverse RCT (§G.2.2) or ICT (§G.3.2) per-tile, in place
/// on the signed wavelet-output `i32` planes — that is, BEFORE the
/// inverse DC level shift (Figure G.1). Requires three components with
/// matching sub-sampling, which is a SIZ-level guarantee when MCT=1
/// (see §A.6.1). No-op for 1-component streams or when the COD reports
/// MCT=0.
///
/// The §G.1 ordering matters: the chroma outputs of the forward RCT
/// (Y1 = I2 - I1, Y2 = I0 - I1, see §G.2.1) span one bit more than
/// the original component precision (NOTE under (G-5)). At 8-bit they
/// reach ±255, so any "treat as unsigned-shifted u8" hack that runs
/// the inverse RCT on already-clipped 0..255 chroma loses information.
fn apply_per_tile_mct_i32(
    planes: &mut [Vec<i32>],
    comp_sizes_rel: &[(u32, u32, u32, u32)],
    cod: &CodParams,
    siz: &Siz,
) -> Result<()> {
    if planes.len() != 3 || cod.mct == 0 {
        return Ok(());
    }
    // All three sub-bands must share dimensions when MCT=1 (§G.2 / §G.3
    // both require equal separation on the reference grid).
    let (_, _, w0, h0) = comp_sizes_rel[0];
    let (_, _, w1, h1) = comp_sizes_rel[1];
    let (_, _, w2, h2) = comp_sizes_rel[2];
    if (w0, h0) != (w1, h1) || (w0, h0) != (w2, h2) {
        return Err(Error::invalid(
            "jpeg2000: MCT=1 requires matching component dimensions",
        ));
    }
    let n = (w0 as usize) * (h0 as usize);
    if planes[0].len() != n || planes[1].len() != n || planes[2].len() != n {
        return Err(Error::invalid(
            "jpeg2000: MCT=1 plane size disagrees with comp_sizes_rel",
        ));
    }
    if cod.transform == 1 {
        apply_rct_inverse_i32(planes, n);
    } else {
        // §G.3 ICT requires a common bit-depth for the three input
        // components — same restriction as §G.2 RCT. The float pipeline
        // we operate on here level-shifted Y0 by -2^(Ssiz-1) on the
        // encoder side; mirror that on the decoder by passing the
        // luma's component precision so the inverse can re-add it.
        let depth = siz.components[0].bit_depth();
        apply_ict_inverse_i32(planes, n, depth);
    }
    Ok(())
}

/// Reverse the JPEG 2000 reversible component transform (RCT) per
/// T.800 §G.2.2 equations (G-6) … (G-8):
///
/// ```text
///   I1 = Y0 - floor((Y2 + Y1) / 4)      // G
///   I0 = Y2 + I1                        // R
///   I2 = Y1 + I1                        // B
/// ```
///
/// Operates on the signed wavelet output. The inverse DC level shift
/// (§G.1.2) is applied separately afterwards by [`dc_shift_and_pack`].
///
/// The spec's `floor(... / 4)` matches Rust's `>> 2` on `i32` (which
/// is an arithmetic shift / floor division), provided we keep the
/// values as `i32`. Care: `(-3) / 4 == 0` in Rust (truncation toward
/// zero), but `(-3) >> 2 == -1` (floor) — so we use `>> 2` and not
/// `/ 4` here. (G.2.2 says floor explicitly.)
fn apply_rct_inverse_i32(planes: &mut [Vec<i32>], n: usize) {
    // The loop indexes three sibling planes simultaneously, so the
    // standard `iter_mut().take(n)` rewrite that clippy proposes does
    // not apply — splitting the borrows would obscure the §G.2.2
    // formula and slow the inner loop down.
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

/// Reverse the JPEG 2000 irreversible component transform (ICT) per
/// T.800 §G.3.2 equations (G-12) … (G-14):
///
/// ```text
///   I0 = Y0           + 1.402   * Y2     // R
///   I1 = Y0 - 0.34413 * Y1 - 0.71414 * Y2// G
///   I2 = Y0           + 1.772   * Y1     // B
/// ```
///
/// Operates on the signed wavelet output before the inverse DC level
/// shift (§G.1, Figure G.1). The encoder only level-shifted the luma
/// `Y0` (subtracted `2^(prec-1)` per (G-1)), because the ICT chroma
/// rows in (G-10) and (G-11) sum to zero on the input components and
/// therefore produce zero-centered `Y1`, `Y2` directly — no DC level
/// shift is applied to chroma either side. After the inverse ICT the
/// three outputs are all in the same "centered on `-2^(prec-1)`" frame
/// as the input luma was on the encoder side; the downstream
/// [`dc_shift_and_pack`] adds `2^(prec-1)` to each unsigned component,
/// restoring R/G/B to the unsigned `0..2^prec-1` range.
///
/// `_luma_depth` is unused at present — kept for API symmetry with
/// the RCT path and as documentation that the ICT inverse MUST honour
/// the luma component's bit depth when the encoder ever supports
/// non-8-bit precision.
fn apply_ict_inverse_i32(planes: &mut [Vec<i32>], n: usize, _luma_depth: u32) {
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
