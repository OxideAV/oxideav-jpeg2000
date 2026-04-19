//! Public frame decoder entry point: take a parsed codestream and the
//! raw tile-part body bytes, return a fully decoded `oxideav_core::Frame`.

use oxideav_core::{Error, Frame, PixelFormat, Result, TimeBase, VideoFrame, VideoPlane};

use super::tile::{decode_tile, parse_cod, parse_qcd};
use crate::codestream::Codestream;

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
    if cs.tile_parts.iter().any(|tp| tp.tile_index != 0) {
        return Err(Error::unsupported(
            "jpeg2000: multi-tile codestreams are not yet supported",
        ));
    }

    // Concatenate all tile-parts of tile 0 into a single body buffer.
    let mut tile_body = Vec::new();
    for tp in &cs.tile_parts {
        let start = tp.sod_offset;
        let end = start + tp.sod_length;
        if end > buf.len() {
            return Err(Error::invalid(
                "jpeg2000: tile-part body extends past codestream",
            ));
        }
        tile_body.extend_from_slice(&buf[start..end]);
    }

    // Build per-component size rectangles. The tile covers the full
    // image (single-tile case); per-component subsampling is read from
    // SIZ.
    let img_w = cs.siz.image_width();
    let img_h = cs.siz.image_height();
    let comp_sizes: Vec<(u32, u32, u32, u32)> = cs
        .siz
        .components
        .iter()
        .map(|c| {
            let w = div_ceil(img_w, c.xrsiz as u32);
            let h = div_ceil(img_h, c.yrsiz as u32);
            (0u32, 0u32, w, h)
        })
        .collect();

    let component_planes = decode_tile(&tile_body, &comp_sizes, &cod, &qcd)?;

    // DC level-shift + clip to per-component dynamic range.
    let mut shifted: Vec<Vec<u8>> = Vec::with_capacity(component_planes.len());
    for (i, plane) in component_planes.iter().enumerate() {
        let depth = cs.siz.components[i].bit_depth();
        let signed = cs.siz.components[i].is_signed();
        let shift = if signed { 0i32 } else { 1i32 << (depth - 1) };
        let max = ((1u32 << depth) - 1) as i32;
        let (_cx0, _cy0, cx1, cy1) = comp_sizes[i];
        let w = cx1 as usize;
        let h = cy1 as usize;
        let mut bytes = Vec::with_capacity(w * h);
        for &v in plane {
            let lv = v.saturating_add(shift).clamp(0, max);
            // Scale to 8-bit if the component is deeper — baseline
            // fixture is 8-bit so this is usually a no-op.
            let scaled = if depth > 8 {
                (lv >> (depth - 8)) as u8
            } else if depth < 8 {
                ((lv << (8 - depth)) & 0xFF) as u8
            } else {
                lv as u8
            };
            bytes.push(scaled);
        }
        // Trim / pad if the plane ended up shorter than w*h (shouldn't).
        bytes.resize(w * h, 0);
        shifted.push(bytes);
    }

    // Assemble VideoFrame. Accept 1 (gray), 3 (YUV4:4:4/YUV4:2:0 depending
    // on subsampling), or any other count falling back to gray-of-first.
    let (pixel_format, planes) = match cs.siz.components.len() {
        1 => (
            PixelFormat::Gray8,
            vec![VideoPlane {
                stride: img_w as usize,
                data: shifted.remove(0),
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
            // Apply reversible component transform (RCT) if MCT=1 in the
            // COD. This round-trips RGB <-> YUV using the integer
            // transform in T.800 G.2.
            let mut planes = shifted;
            if cod.mct == 1 {
                apply_rct_inverse(
                    &mut planes,
                    comp_sizes[0].2 as usize,
                    comp_sizes[0].3 as usize,
                );
            }
            let planes = planes
                .into_iter()
                .enumerate()
                .map(|(i, p)| VideoPlane {
                    stride: comp_sizes[i].2 as usize,
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

fn div_ceil(a: u32, b: u32) -> u32 {
    if b == 0 {
        return 0;
    }
    a.div_ceil(b)
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
