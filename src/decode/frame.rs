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

use oxideav_core::{Error, Frame, PixelFormat, Result, TimeBase, VideoFrame, VideoPlane};

use super::tile::{decode_tile_with_params, parse_cod, parse_qcd, CodParams, DecodeParams};
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
        return Err(Error::invalid("jpeg2000: tile count exceeds codestream limit"));
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

        let params = DecodeParams {
            comp_precisions: &comp_precisions,
        };
        let planes = decode_tile_with_params(&tile_body, &comp_sizes_rel, &cod, &qcd, &params)?;

        // DC level-shift + clip, then per-tile inverse component transform
        // (RCT / ICT) if MCT=1. Paste into the assembled image planes.
        let mut shifted = dc_shift_and_pack(&planes, &comp_sizes_rel, &cs.siz)?;
        apply_per_tile_mct(&mut shifted, &comp_sizes_rel, &cod)?;

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

    Ok(Frame::Video(VideoFrame {
        format: pixel_format,
        width: img_w,
        height: img_h,
        pts: None,
        time_base: TimeBase::new(1, 1),
        planes,
    }))
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

/// Apply the inverse RCT (§G.1) or ICT (§G.2) per-tile. Requires three
/// components with matching sub-sampling — which is a SIZ-level guarantee
/// when MCT=1 (see §A.6.1). No-op for 1-component streams or when the
/// decoder's COD reports MCT=0.
fn apply_per_tile_mct(
    planes: &mut [Vec<u8>],
    comp_sizes_rel: &[(u32, u32, u32, u32)],
    cod: &CodParams,
) -> Result<()> {
    if planes.len() != 3 || cod.mct == 0 {
        return Ok(());
    }
    // All three sub-bands must share dimensions when MCT=1.
    let (_, _, w0, h0) = comp_sizes_rel[0];
    let (_, _, w1, h1) = comp_sizes_rel[1];
    let (_, _, w2, h2) = comp_sizes_rel[2];
    if (w0, h0) != (w1, h1) || (w0, h0) != (w2, h2) {
        return Err(Error::invalid(
            "jpeg2000: MCT=1 requires matching component dimensions",
        ));
    }
    if cod.transform == 1 {
        apply_rct_inverse(planes, w0 as usize, h0 as usize);
    } else {
        apply_ict_inverse(planes, w0 as usize, h0 as usize);
    }
    Ok(())
}

/// Reverse the JPEG 2000 reversible component transform (RCT), mapping
/// Y/Cb/Cr back into R/G/B. Operates in place on the three 8-bit planes.
fn apply_rct_inverse(planes: &mut [Vec<u8>], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let y_v = planes[0][i] as i32;
            let cb = planes[1][i] as i32 - 128;
            let cr = planes[2][i] as i32 - 128;
            let g = y_v - ((cb + cr) >> 2);
            let r = cr + g;
            let b = cb + g;
            planes[0][i] = r.clamp(0, 255) as u8;
            planes[1][i] = g.clamp(0, 255) as u8;
            planes[2][i] = b.clamp(0, 255) as u8;
        }
    }
}

/// Reverse the JPEG 2000 irreversible component transform (ICT),
/// mapping YCbCr back into RGB per T.800 §G.2 (identical matrix to ITU-R
/// BT.601 JPEG YCbCr):
///
/// ```text
///   R = Y              + 1.402   * (Cr - 128)
///   G = Y - 0.34413 * (Cb - 128) - 0.71414 * (Cr - 128)
///   B = Y + 1.772   * (Cb - 128)
/// ```
fn apply_ict_inverse(planes: &mut [Vec<u8>], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let yf = planes[0][i] as f32;
            let cb = planes[1][i] as f32 - 128.0;
            let cr = planes[2][i] as f32 - 128.0;
            let r = yf + 1.402 * cr;
            let g = yf - 0.344_13 * cb - 0.714_14 * cr;
            let b = yf + 1.772 * cb;
            planes[0][i] = r.round().clamp(0.0, 255.0) as u8;
            planes[1][i] = g.round().clamp(0.0, 255.0) as u8;
            planes[2][i] = b.round().clamp(0.0, 255.0) as u8;
        }
    }
}
