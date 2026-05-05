//! HTJ2K codestream + tile-body encoder (round 4).
//!
//! Wraps [`super::cleanup_enc::encode_cleanup`] in the marker chain
//! that ISO/IEC 15444-15 requires: SOC + SIZ (with Rsiz bit 14 set
//! per §A.2) + CAP (Pcap15 + Ccap15) + COD (with SPcod cblk_style
//! bit 6 set per Table A.3) + QCD + (one or more) SOT + SOD + EOC.
//!
//! Round-4 scope (delta over round 3):
//!
//! * **9/7 irreversible transform** path. New
//!   [`EncodeOptionsHt::transform`] selector picks 5/3 reversible
//!   (default, lossless) or 9/7 irreversible (lossy). The 9/7 path runs
//!   the existing forward 9/7 lifting from [`crate::encode::dwt::fdwt_97`]
//!   and applies a per-band scalar quantiser (T.800 Eq E-6) to map
//!   floats onto the M_b grid the cleanup encoder expects. The QCD is
//!   emitted in expounded form (qntsty = 2) with mu = 0.
//! * **Multi-tile** codestream output. New [`EncodeOptionsHt::tile_size`]
//!   knob; when set, the encoder writes the SOC + main-header markers
//!   once and then emits one SOT/SOD pair per tile in raster order.
//!   Per-tile DWT + tier-1 + tier-2 are completely independent, so
//!   tile boundaries land in the wavelet-and-quantised domain (the
//!   classic JPEG 2000 behaviour). The matching HT decoder change
//!   (multi-tile dispatch) lives in
//!   [`crate::decode::htj2k::tier2::decode_frame_htj2k`].
//! * **Sub-sampled chroma input** (`Yuv420P` / `Yuv422P`). Per-component
//!   `(XRsiz, YRsiz)` are written into SIZ; per-component dimensions are
//!   derived as `ceil(W / XRsiz) × ceil(H / YRsiz)` on the reference
//!   grid. The cleanup pass + tier-2 walker iterate over each component
//!   at its own extent. Forward MCT is rejected for sub-sampled input
//!   (the RCT requires same-sized R/G/B and is meaningless when chroma
//!   is at half resolution); the encoder errors out with
//!   `Error::Unsupported` if `use_color_transform = true` is requested
//!   on `Yuv420P` / `Yuv422P` input.
//! * **PPM / PPT packet header placement**. New
//!   [`EncodeOptionsHt::packet_header_placement`] knob mirrors the
//!   classic encoder: `Inline` (default) is what round-3 produced;
//!   `PackedMainHeader` re-routes per-tile-part packet headers into a
//!   single PPM segment in the main header; `PackedPerTilePart` emits
//!   one PPT segment per tile-part. We reuse
//!   [`crate::decode::tile::split_packet_headers`] to extract headers
//!   from the inline body, exactly as the classic encoder does.
//!
//! Carried over from round 3:
//!
//! * Multi-component encode for `Gray8`, `Rgb24`, `Yuv444P` input.
//! * Optional forward 5/3 reversible component transform (RCT, T.800
//!   §G.1) for `Rgb24` input via
//!   [`EncodeOptionsHt::use_color_transform`]; signalled in COD by
//!   setting the `MCT` byte to 1.
//! * HT cleanup pass encoder ([`super::cleanup_enc::encode_cleanup`])
//!   with full multi-significance per quad and the §7.3.6 Eq-4
//!   first-line-pair both-`u_off=1` special case.
//! * Single quality layer.
//!
//! Out of scope (round 5+):
//!
//! * SigProp / MagRef refinement passes (Z_blk ∈ {2, 3}). Currently
//!   the encoder emits cleanup-only (Z_blk = 1).
//! * Multi-layer (single quality layer per code-block).
//! * Constrained sets (T.814 §8) and multi-set HT (T.814 Annex B).

use super::cleanup_enc::{encode_cleanup, SampleHt};
use crate::decode::tile::{
    build_subbands, parse_cod, parse_qcd, split_packet_headers, CodParams as DecodedCodParams,
    QcdParams as DecodedQcdParams,
};
use crate::encode::dwt::{fdwt_53, fdwt_97};
use crate::error::{Jpeg2000Error as Error, Result};
use crate::image::{Jpeg2000Image, Jpeg2000PixelFormat as PixelFormat};

/// Forward wavelet selector for the HTJ2K encoder. Mirrors
/// [`crate::encode::TransformMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtTransform {
    /// 5/3 integer reversible (lossless).
    Reversible53,
    /// 9/7 float irreversible (lossy). Per-band scalar quantiser with
    /// `eps_b = precision`, `mu = 0` so `stepsize_b = 1` on every band.
    Irreversible97,
}

/// Choice of where the per-tile packet-header bytes live in the emitted
/// codestream (T.800 §A.7.4 / §A.7.5). Matches the classic encoder's
/// [`crate::encode::PacketHeaderPlacement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HtPacketHeaderPlacement {
    /// Inline — packet header bytes precede each packet body in the
    /// SOD payload (the historical layout the round-3 encoder produced).
    #[default]
    Inline,
    /// Packed in a `PPM` segment in the main header. Each tile-part SOD
    /// body then carries packet bodies only.
    PackedMainHeader,
    /// Packed per tile-part in a `PPT` segment that precedes SOD in
    /// each tile-part header.
    PackedPerTilePart,
}

/// Knobs for the HTJ2K encoder.
#[derive(Debug, Clone)]
pub struct EncodeOptionsHt {
    /// Code-block width log2. Default 5 (= 32). Round-4 uses the same
    /// value for both dimensions.
    pub cblk_log2: u8,
    /// Number of decomposition levels (NL). Round-4 supports `0..=5`.
    pub num_decomp: u8,
    /// When `true` and the input is `Rgb24`, apply the forward 5/3
    /// reversible component transform (RCT, T.800 §G.1) and signal
    /// `MCT = 1` in COD. Ignored for non-RGB input. Defaults to true.
    pub use_color_transform: bool,
    /// Forward wavelet kernel. Default `Reversible53` (lossless).
    pub transform: HtTransform,
    /// When `Some((tw, th))`, emit a multi-tile codestream with the
    /// reference-grid tile size `(XTsiz, YTsiz) = (tw, th)`. Tile origin
    /// is fixed at `(0, 0)`. The encoder produces one SOT/SOD per tile
    /// in raster order (Isot increments left-to-right then top-to-bottom).
    /// Defaults to `None` (single-tile, `XTsiz = W`, `YTsiz = H`).
    pub tile_size: Option<(u32, u32)>,
    /// Where the per-tile packet header bytes live in the emitted
    /// codestream. Default `Inline`.
    pub packet_header_placement: HtPacketHeaderPlacement,
}

impl Default for EncodeOptionsHt {
    fn default() -> Self {
        EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 0,
            use_color_transform: true,
            transform: HtTransform::Reversible53,
            tile_size: None,
            packet_header_placement: HtPacketHeaderPlacement::Inline,
        }
    }
}

#[derive(Clone, Default)]
struct CodedCblk {
    data: Vec<u8>,
    missing_msb: u32,
    included: bool,
}

#[derive(Clone, Default)]
struct BandEnc {
    cblks_w: u32,
    cblks_h: u32,
    cblks: Vec<CodedCblk>,
}

#[derive(Default, Clone)]
struct ResEnc {
    bands: Vec<BandEnc>,
}

/// Per-component coded subband pyramid. Indexed `per_res[resno]`.
type CompCoded = Vec<ResEnc>;

/// Per-component dimensions on the reference grid (after sub-sampling).
#[derive(Clone, Copy)]
struct CompDims {
    /// Sub-sampling factors signalled in SIZ.
    xrsiz: u8,
    yrsiz: u8,
}

/// Encode a [`Jpeg2000Image`] as an HTJ2K codestream.
///
/// Round-4 supports `num_decomp ∈ [0, 5]`. Pixel formats accepted:
///
/// * `Gray8` — 1 component, 8-bit unsigned.
/// * `Rgb24` — 3 components, 8-bit unsigned. With
///   `use_color_transform = true` the encoder applies the forward RCT
///   (`Y = (R + 2G + B) >> 2`, `Cb = B - G`, `Cr = R - G`) and signals
///   `MCT = 1` in COD; without it the channels are encoded
///   independently and the decoder reads them back as planar YCbCr 4:4:4.
/// * `Yuv444P` — 3 components at full resolution, 8-bit unsigned. No
///   MCT (the channels are already in the YCbCr basis).
/// * `Yuv422P` — 3 components, chroma at half horizontal resolution
///   (`XRsiz_chroma = 2`, `YRsiz_chroma = 1`). MCT must be off.
/// * `Yuv420P` — 3 components, chroma at half H + V (`XRsiz_chroma =
///   YRsiz_chroma = 2`). MCT must be off.
pub fn encode_image_htj2k(image: &Jpeg2000Image, opts: &EncodeOptionsHt) -> Result<Vec<u8>> {
    let w = image.width;
    let h = image.height;
    if w == 0 || h == 0 {
        return Err(Error::invalid("HTJ2K encode: zero-dimension image"));
    }
    if opts.num_decomp > 5 {
        return Err(Error::unsupported(format!(
            "HTJ2K encode: num_decomp = {} > 5",
            opts.num_decomp
        )));
    }
    let precision: u32 = 8;
    let nl = opts.num_decomp;
    let cblk_log2 = opts.cblk_log2;

    // -- Tile grid --
    let (tw, th) = opts.tile_size.unwrap_or((w, h));
    if tw == 0 || th == 0 {
        return Err(Error::invalid("HTJ2K encode: zero-dimension tile_size"));
    }
    let num_tiles_x = w.div_ceil(tw);
    let num_tiles_y = h.div_ceil(th);
    let num_tiles = num_tiles_x as u64 * num_tiles_y as u64;
    if num_tiles > u16::MAX as u64 + 1 {
        return Err(Error::invalid("HTJ2K encode: too many tiles"));
    }

    // -- 1. Decide pixel-format-specific component layout (sub-sampling
    //       factors, MCT eligibility) and build full-image i32 planes
    //       per-component. The DWT runs per-tile so we slice these
    //       planes inside the per-tile loop. --
    let (full_planes, comp_dims, apply_mct) =
        extract_components_full_i32(image, opts.use_color_transform)?;

    // -- 2. Iterate tiles. Per tile: compute the per-component tile
    //       rectangle on the component grid, slice the full-image plane,
    //       run forward DWT (in i32 for 5/3, in f32 for 9/7), tier-1
    //       encode every codeblock, build the LRCP packet body. Also
    //       accumulate the per-tile body for the optional PPM split. --
    let dc_shift = 1i32 << (precision - 1);
    let mut per_tile_bodies: Vec<Vec<u8>> = Vec::with_capacity(num_tiles as usize);
    for ty_idx in 0..num_tiles_y {
        for tx_idx in 0..num_tiles_x {
            let tx0 = tx_idx * tw;
            let ty0 = ty_idx * th;
            let tx1 = (tx0 + tw).min(w);
            let ty1 = (ty0 + th).min(h);
            let body = encode_one_tile(
                &full_planes,
                &comp_dims,
                w,
                h,
                tx0,
                ty0,
                tx1,
                ty1,
                opts.transform,
                cblk_log2,
                nl,
                apply_mct,
                precision,
                dc_shift,
            )?;
            per_tile_bodies.push(body);
        }
    }

    // -- 3. Decide PPM/PPT split. The splitter expects parsed COD/QCD
    //       structures matching the markers we're about to emit; we
    //       re-parse our own marker payloads to avoid drift. The HT COD
    //       differs from the classic Part-1 COD only in the cblk_style
    //       byte (bit 6 set), which the splitter's tier-2 walker
    //       ignores. --
    let prec_for_qcd = if apply_mct { precision + 1 } else { precision };
    let need_split = !matches!(
        opts.packet_header_placement,
        HtPacketHeaderPlacement::Inline
    );
    let mut split_bodies: Vec<Vec<u8>> = Vec::with_capacity(num_tiles as usize);
    let mut split_headers: Vec<Vec<u8>> = Vec::with_capacity(num_tiles as usize);
    if need_split {
        let cod_payload = build_cod_payload(cblk_log2, nl, apply_mct, opts.transform);
        let cod_parsed: DecodedCodParams = parse_cod(&cod_payload)?;
        let qcd_payload = match opts.transform {
            HtTransform::Reversible53 => build_qcd_reversible_payload(prec_for_qcd as u8, nl),
            HtTransform::Irreversible97 => build_qcd_irreversible_payload(prec_for_qcd as u8, nl),
        };
        let qcd_parsed: DecodedQcdParams = parse_qcd(&qcd_payload, nl)?;

        for (tile_idx, body) in per_tile_bodies.iter().enumerate() {
            let p = (tile_idx as u32) % num_tiles_x;
            let q = (tile_idx as u32) / num_tiles_x;
            let tx0 = p * tw;
            let ty0 = q * th;
            let tx1 = (tx0 + tw).min(w);
            let ty1 = (ty0 + th).min(h);
            let comp_sizes_rel: Vec<(u32, u32, u32, u32)> = comp_dims
                .iter()
                .map(|d| {
                    let xr = d.xrsiz as u32;
                    let yr = d.yrsiz as u32;
                    let cw = tx1.div_ceil(xr) - tx0.div_ceil(xr);
                    let ch = ty1.div_ceil(yr) - ty0.div_ceil(yr);
                    (0u32, 0u32, cw, ch)
                })
                .collect();
            let (hdr, body_only) =
                split_packet_headers(body, &comp_sizes_rel, &cod_parsed, &qcd_parsed, None)?;
            split_headers.push(hdr);
            split_bodies.push(body_only);
        }
    }

    // -- 4. Assemble the codestream. --
    let mut cs = Vec::<u8>::new();
    cs.extend_from_slice(&[0xFF, 0x4F]); // SOC
    write_siz_ht(&mut cs, w, h, tw, th, precision, &comp_dims)?;
    write_cap_ht(&mut cs);
    let cod_payload = build_cod_payload(cblk_log2, nl, apply_mct, opts.transform);
    cs.extend_from_slice(&[0xFF, 0x52]);
    cs.extend_from_slice(&((cod_payload.len() + 2) as u16).to_be_bytes());
    cs.extend_from_slice(&cod_payload);
    let qcd_payload = match opts.transform {
        HtTransform::Reversible53 => build_qcd_reversible_payload(prec_for_qcd as u8, nl),
        HtTransform::Irreversible97 => build_qcd_irreversible_payload(prec_for_qcd as u8, nl),
    };
    cs.extend_from_slice(&[0xFF, 0x5C]);
    cs.extend_from_slice(&((qcd_payload.len() + 2) as u16).to_be_bytes());
    cs.extend_from_slice(&qcd_payload);
    if matches!(
        opts.packet_header_placement,
        HtPacketHeaderPlacement::PackedMainHeader
    ) {
        let payload = build_ppm_payload(&split_headers);
        let lppm = (payload.len() + 2) as u16;
        cs.extend_from_slice(&[0xFF, 0x60]);
        cs.extend_from_slice(&lppm.to_be_bytes());
        cs.extend_from_slice(&payload);
    }

    for tile_idx in 0..num_tiles as usize {
        let sot_off = cs.len();
        cs.extend_from_slice(&[0xFF, 0x90]);
        cs.extend_from_slice(&10u16.to_be_bytes());
        cs.extend_from_slice(&(tile_idx as u16).to_be_bytes());
        let psot_pos = cs.len();
        cs.extend_from_slice(&0u32.to_be_bytes());
        cs.extend_from_slice(&[0, 1]); // TPsot=0, TNsot=1
        if matches!(
            opts.packet_header_placement,
            HtPacketHeaderPlacement::PackedPerTilePart
        ) {
            write_ppt_marker(&mut cs, &split_headers[tile_idx])?;
        }
        cs.extend_from_slice(&[0xFF, 0x93]); // SOD
        let body_to_emit: &[u8] = if need_split {
            &split_bodies[tile_idx]
        } else {
            &per_tile_bodies[tile_idx]
        };
        cs.extend_from_slice(body_to_emit);
        let tile_part_end = cs.len();
        let psot = (tile_part_end - sot_off) as u32;
        cs[psot_pos..psot_pos + 4].copy_from_slice(&psot.to_be_bytes());
    }
    cs.extend_from_slice(&[0xFF, 0xD9]); // EOC
    Ok(cs)
}

/// Encode one tile end-to-end into a tier-2 packet body. The body is
/// inline-headers (the optional PPM/PPT split happens in the caller via
/// [`split_packet_headers`]).
#[allow(clippy::too_many_arguments)]
fn encode_one_tile(
    full_planes: &[Vec<i32>],
    comp_dims: &[CompDims],
    img_w: u32,
    img_h: u32,
    tx0: u32,
    ty0: u32,
    tx1: u32,
    ty1: u32,
    transform: HtTransform,
    cblk_log2: u8,
    nl: u8,
    apply_mct: bool,
    precision: u32,
    dc_shift: i32,
) -> Result<Vec<u8>> {
    let _ = apply_mct;
    let num_comps = full_planes.len();
    let cblk_dim = 1u32 << cblk_log2;
    let num_res = (nl as usize) + 1;

    // Per-component tile rectangle on the component grid.
    let mut comp_planes: Vec<Vec<i32>> = Vec::with_capacity(num_comps);
    let mut comp_w_h: Vec<(usize, usize)> = Vec::with_capacity(num_comps);
    for (ci, plane) in full_planes.iter().enumerate() {
        let xr = comp_dims[ci].xrsiz as u32;
        let yr = comp_dims[ci].yrsiz as u32;
        let full_w = img_w.div_ceil(xr) as usize;
        let cx0 = tx0.div_ceil(xr) as usize;
        let cy0 = ty0.div_ceil(yr) as usize;
        let cx1 = tx1.div_ceil(xr) as usize;
        let cy1 = ty1.div_ceil(yr) as usize;
        let _ = img_h;
        let cw = cx1 - cx0;
        let ch = cy1 - cy0;
        // Slice the plane into a tile-local row-major buffer.
        let mut tile_buf = Vec::with_capacity(cw * ch);
        for ly in 0..ch {
            let row = (cy0 + ly) * full_w + cx0;
            tile_buf.extend_from_slice(&plane[row..row + cw]);
        }
        comp_planes.push(tile_buf);
        comp_w_h.push((cw, ch));
    }

    // Forward DWT level-by-level, in the right numeric domain.
    match transform {
        HtTransform::Reversible53 => {
            for (ci, plane) in comp_planes.iter_mut().enumerate() {
                let (cw, ch) = comp_w_h[ci];
                let mut cur_w = cw;
                let mut cur_h = ch;
                for _ in 0..nl as usize {
                    if cur_w < 2 || cur_h < 2 {
                        break;
                    }
                    fdwt_53(plane, cur_w, cur_h, cw);
                    cur_w = cur_w.div_ceil(2);
                    cur_h = cur_h.div_ceil(2);
                }
            }
        }
        HtTransform::Irreversible97 => {
            // 9/7 path: sample data is already DC-level shifted (signed
            // float in the encode path). Convert to f32, lift, quantise
            // back to i32 at the M_b grid where the cleanup encoder
            // operates. The quantiser's stepsize for `eps_b = precision`,
            // `mu = 0` is `1.0` on every band, so quantisation reduces
            // to integer truncation (q = floor(c) for c >= 0, -floor(-c)
            // otherwise). We delay the per-band sub-band slicing into
            // the band-encode loop below.
            for (ci, plane) in comp_planes.iter_mut().enumerate() {
                let (cw, ch) = comp_w_h[ci];
                let mut f: Vec<f32> = plane.iter().map(|&v| v as f32).collect();
                let mut cur_w = cw;
                let mut cur_h = ch;
                for _ in 0..nl as usize {
                    if cur_w < 2 || cur_h < 2 {
                        break;
                    }
                    fdwt_97(&mut f, cur_w, cur_h, cw);
                    cur_w = cur_w.div_ceil(2);
                    cur_h = cur_h.div_ceil(2);
                }
                // Quantise back to i32 with stepsize = 1.0 on every band.
                // Use round-half-to-even (`.round()`) so the float→int
                // mapping symmetrises around zero — matters for chroma
                // where dynamic range straddles zero.
                for (slot, &fv) in plane.iter_mut().zip(f.iter()) {
                    *slot = fv.round() as i32;
                }
            }
        }
    }

    // Per-component subband layout + per-codeblock encode.
    let prec_for_qcd = if apply_mct { precision + 1 } else { precision };
    let band_eps = |band_kind: u8| -> u32 {
        match band_kind {
            0 => prec_for_qcd,         // LL
            1 | 2 => prec_for_qcd + 1, // HL, LH
            3 => prec_for_qcd + 2,     // HH
            _ => prec_for_qcd,
        }
    };
    let band_numbps = |band_kind: u8| -> u32 { band_eps(band_kind).saturating_sub(1) };

    let mut per_comp: Vec<CompCoded> = Vec::with_capacity(num_comps);
    for ci in 0..num_comps {
        let (cw, ch) = comp_w_h[ci];
        let comp_w = cw as u32;
        let comp_h = ch as u32;
        let plane = &comp_planes[ci];
        let subbands = build_subbands(0, 0, comp_w, comp_h, nl);
        let mut per_res: Vec<ResEnc> = (0..num_res).map(|_| ResEnc::default()).collect();
        for sb in &subbands {
            let bw = sb.x1 - sb.x0;
            let bh = sb.y1 - sb.y0;
            if bw == 0 || bh == 0 {
                per_res[sb.resno as usize].bands.push(BandEnc::default());
                continue;
            }
            let level_from_top = (nl as usize) - sb.resno as usize;
            let mut scale_w = cw;
            let mut scale_h = ch;
            for _ in 0..level_from_top {
                scale_w = scale_w.div_ceil(2);
                scale_h = scale_h.div_ceil(2);
            }
            let (band_cx0, band_cy0) = match sb.band_kind {
                0 => (0usize, 0usize),
                1 => (scale_w.div_ceil(2), 0),
                2 => (0, scale_h.div_ceil(2)),
                3 => (scale_w.div_ceil(2), scale_h.div_ceil(2)),
                _ => (0, 0),
            };

            let cblks_w = bw.div_ceil(cblk_dim);
            let cblks_h = bh.div_ceil(cblk_dim);
            let mut cblks: Vec<CodedCblk> = Vec::with_capacity((cblks_w * cblks_h) as usize);
            let nbps = band_numbps(sb.band_kind);

            for cy in 0..cblks_h {
                for cx in 0..cblks_w {
                    let bx0 = cx * cblk_dim;
                    let by0 = cy * cblk_dim;
                    let bx1 = (bx0 + cblk_dim).min(bw);
                    let by1 = (by0 + cblk_dim).min(bh);
                    let cbw = (bx1 - bx0) as usize;
                    let cbh = (by1 - by0) as usize;

                    let mut samples: Vec<SampleHt> = Vec::with_capacity(cbw * cbh);
                    let mut max_mag: u32 = 0;
                    for ly in 0..cbh {
                        for lx in 0..cbw {
                            let cx_canvas = band_cx0 + (bx0 as usize) + lx;
                            let cy_canvas = band_cy0 + (by0 as usize) + ly;
                            let raw = plane[cy_canvas * cw + cx_canvas];
                            let mag = raw.unsigned_abs();
                            max_mag = max_mag.max(mag);
                            let sign: u8 = if raw < 0 { 1 } else { 0 };
                            samples.push(SampleHt { mag, sign });
                        }
                    }

                    if max_mag == 0 {
                        cblks.push(CodedCblk {
                            data: Vec::new(),
                            missing_msb: nbps + 1,
                            included: false,
                        });
                        continue;
                    }

                    let missing_msb = nbps;
                    let dcup = encode_cleanup(cbw as u32, cbh as u32, &samples)?;
                    cblks.push(CodedCblk {
                        data: dcup,
                        missing_msb,
                        included: true,
                    });
                }
            }

            per_res[sb.resno as usize].bands.push(BandEnc {
                cblks_w,
                cblks_h,
                cblks,
            });
        }
        per_comp.push(per_res);
    }

    // Build tier-2 packet body: LRCP, single layer, default precincts.
    // One packet per (resolution, component); within a packet, all
    // bands at that resolution.
    let mut body: Vec<u8> = Vec::new();
    #[allow(clippy::needless_range_loop)]
    for resno in 0..num_res {
        for ci in 0..num_comps {
            emit_packet_htj2k(&mut body, &per_comp[ci][resno])?;
        }
    }
    let _ = dc_shift;
    Ok(body)
}

/// Pull per-component i32 sample planes out of the input image at
/// **full extent on the reference grid** (i.e. the encoder's tile loop
/// later slices these into per-tile rectangles). Returns
/// `(planes, dims, apply_mct)` where each plane is row-major at its
/// component's `(W/XRsiz, H/YRsiz)` extent and DC-level-shifted.
///
/// For `Rgb24` with `use_color_transform = true` the forward 5/3
/// reversible RCT is applied at the pixel domain and the resulting Y/Cb/
/// Cr planes have full image extent (4:4:4-style — RCT requires same-
/// resolution R, G, B).
fn extract_components_full_i32(
    image: &Jpeg2000Image,
    use_color_transform: bool,
) -> Result<(Vec<Vec<i32>>, Vec<CompDims>, bool)> {
    let w = image.width;
    let h = image.height;
    let n = (w as usize) * (h as usize);
    let dc_shift = 128i32;
    match image.pixel_format {
        PixelFormat::Gray8 => {
            if image.planes.len() != 1 {
                return Err(Error::invalid(
                    "HTJ2K encode: Gray8 frame must have 1 plane",
                ));
            }
            let p = &image.planes[0];
            let mut g = Vec::with_capacity(n);
            for y in 0..h as usize {
                for x in 0..w as usize {
                    g.push(p.data[y * p.stride + x] as i32 - dc_shift);
                }
            }
            let dims = vec![CompDims { xrsiz: 1, yrsiz: 1 }];
            Ok((vec![g], dims, false))
        }
        PixelFormat::Rgb24 => {
            if image.planes.len() != 1 {
                return Err(Error::invalid(
                    "HTJ2K encode: Rgb24 frame must have 1 plane",
                ));
            }
            let p = &image.planes[0];
            let mut r = Vec::with_capacity(n);
            let mut g = Vec::with_capacity(n);
            let mut b = Vec::with_capacity(n);
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let off = y * p.stride + 3 * x;
                    r.push(p.data[off] as i32);
                    g.push(p.data[off + 1] as i32);
                    b.push(p.data[off + 2] as i32);
                }
            }
            let dims = vec![CompDims { xrsiz: 1, yrsiz: 1 }; 3];
            if use_color_transform {
                let mut y = Vec::with_capacity(n);
                let mut cb = Vec::with_capacity(n);
                let mut cr = Vec::with_capacity(n);
                for i in 0..n {
                    let rv = r[i];
                    let gv = g[i];
                    let bv = b[i];
                    y.push(((rv + 2 * gv + bv) >> 2) - dc_shift);
                    cb.push(bv - gv);
                    cr.push(rv - gv);
                }
                Ok((vec![y, cb, cr], dims, true))
            } else {
                let r = r.into_iter().map(|v| v - dc_shift).collect();
                let g = g.into_iter().map(|v| v - dc_shift).collect();
                let b = b.into_iter().map(|v| v - dc_shift).collect();
                Ok((vec![r, g, b], dims, false))
            }
        }
        PixelFormat::Yuv444P => {
            if image.planes.len() != 3 {
                return Err(Error::invalid(
                    "HTJ2K encode: Yuv444P frame must have 3 planes",
                ));
            }
            let mut planes_i32: Vec<Vec<i32>> = Vec::with_capacity(3);
            for p in &image.planes {
                let mut buf = Vec::with_capacity(n);
                for y in 0..h as usize {
                    for x in 0..w as usize {
                        buf.push(p.data[y * p.stride + x] as i32 - dc_shift);
                    }
                }
                planes_i32.push(buf);
            }
            let dims = vec![CompDims { xrsiz: 1, yrsiz: 1 }; 3];
            Ok((planes_i32, dims, false))
        }
        PixelFormat::Yuv422P => {
            if image.planes.len() != 3 {
                return Err(Error::invalid(
                    "HTJ2K encode: Yuv422P frame must have 3 planes",
                ));
            }
            if use_color_transform {
                return Err(Error::unsupported(
                    "HTJ2K encode: forward MCT is not defined for sub-sampled chroma input",
                ));
            }
            let cw = (w as usize).div_ceil(2);
            let ch = h as usize;
            let luma = extract_plane_dc_shifted(&image.planes[0], w as usize, h as usize, dc_shift);
            let cb = extract_plane_dc_shifted(&image.planes[1], cw, ch, dc_shift);
            let cr = extract_plane_dc_shifted(&image.planes[2], cw, ch, dc_shift);
            let dims = vec![
                CompDims { xrsiz: 1, yrsiz: 1 },
                CompDims { xrsiz: 2, yrsiz: 1 },
                CompDims { xrsiz: 2, yrsiz: 1 },
            ];
            Ok((vec![luma, cb, cr], dims, false))
        }
        PixelFormat::Yuv420P => {
            if image.planes.len() != 3 {
                return Err(Error::invalid(
                    "HTJ2K encode: Yuv420P frame must have 3 planes",
                ));
            }
            if use_color_transform {
                return Err(Error::unsupported(
                    "HTJ2K encode: forward MCT is not defined for sub-sampled chroma input",
                ));
            }
            let cw = (w as usize).div_ceil(2);
            let ch = (h as usize).div_ceil(2);
            let luma = extract_plane_dc_shifted(&image.planes[0], w as usize, h as usize, dc_shift);
            let cb = extract_plane_dc_shifted(&image.planes[1], cw, ch, dc_shift);
            let cr = extract_plane_dc_shifted(&image.planes[2], cw, ch, dc_shift);
            let dims = vec![
                CompDims { xrsiz: 1, yrsiz: 1 },
                CompDims { xrsiz: 2, yrsiz: 2 },
                CompDims { xrsiz: 2, yrsiz: 2 },
            ];
            Ok((vec![luma, cb, cr], dims, false))
        }
    }
}

/// Read `w * h` bytes out of `plane` (which may have a stride larger
/// than `w`) and return a packed `Vec<i32>` with each value DC-level
/// shifted by `dc_shift`.
fn extract_plane_dc_shifted(
    plane: &crate::image::Jpeg2000Plane,
    w: usize,
    h: usize,
    dc_shift: i32,
) -> Vec<i32> {
    let mut buf = Vec::with_capacity(w * h);
    for y in 0..h {
        let row = y * plane.stride;
        for x in 0..w {
            buf.push(plane.data[row + x] as i32 - dc_shift);
        }
    }
    buf
}

/// Emit one tier-2 packet for a single (resolution, component) tuple.
fn emit_packet_htj2k(out: &mut Vec<u8>, res: &ResEnc) -> Result<()> {
    let mut bw = BioWriterMsbFirst::new();
    let any_included = res.bands.iter().any(|b| b.cblks.iter().any(|c| c.included));
    if !any_included {
        bw.write_bit(0);
        bw.flush_aligned(out);
        return Ok(());
    }
    bw.write_bit(1);

    // Per band, per cblk header.
    for band in &res.bands {
        let cblks_w = band.cblks_w as usize;
        let cblks_h = band.cblks_h as usize;
        if cblks_w == 0 || cblks_h == 0 {
            continue;
        }
        let n = cblks_w * cblks_h;
        let mut incl_leaves = vec![1u32; n];
        for (i, c) in band.cblks.iter().enumerate() {
            if c.included {
                incl_leaves[i] = 0;
            }
        }
        encode_tagtree_threshold1(&mut bw, cblks_w, cblks_h, &incl_leaves);
        for c in &band.cblks {
            if !c.included {
                continue;
            }
            // Zero-bitplane tag tree: leaf value = missing_msb - 1
            // owing to the decoder's threshold-loop off-by-one
            // convention.
            let zb_leaf = c.missing_msb.saturating_sub(1);
            for _ in 0..zb_leaf {
                bw.write_bit(0);
            }
            bw.write_bit(1);
            // num_passes = 1.
            bw.write_bit(0);
            // Lblock growth from default 3.
            let lcup = c.data.len() as u32;
            let mut lblock = 3u32;
            while (1u32 << lblock) <= lcup {
                bw.write_bit(1);
                lblock += 1;
            }
            bw.write_bit(0);
            for k in (0..lblock).rev() {
                bw.write_bit(((lcup >> k) & 1) as u8);
            }
        }
    }
    bw.flush_aligned(out);
    // Packet body: per-band, per-cblk concatenation.
    for band in &res.bands {
        for c in &band.cblks {
            if !c.included {
                continue;
            }
            out.extend_from_slice(&c.data);
        }
    }
    Ok(())
}

/// Tag-tree threshold=1 sweep encoder (carried over from round 3
/// verbatim). See the round-3 module-level doc for the algorithm.
fn encode_tagtree_threshold1(bw: &mut BioWriterMsbFirst, w: usize, h: usize, leaves: &[u32]) {
    let mut levels: Vec<(usize, usize, Vec<u32>)> = Vec::new();
    levels.push((w, h, leaves.to_vec()));
    while levels.last().unwrap().0 > 1 || levels.last().unwrap().1 > 1 {
        let last = levels.last().unwrap();
        let lw = last.0;
        let lh = last.1;
        let lvals = last.2.clone();
        let pw = lw.div_ceil(2);
        let ph = lh.div_ceil(2);
        let mut pvals = vec![u32::MAX; pw * ph];
        for y in 0..lh {
            for x in 0..lw {
                let pi = (y / 2) * pw + (x / 2);
                let v = lvals[y * lw + x];
                pvals[pi] = pvals[pi].min(v);
            }
        }
        levels.push((pw, ph, pvals));
    }
    let n_levels = levels.len();
    let mut lows: Vec<Vec<u32>> = levels
        .iter()
        .map(|(lw, lh, _)| vec![0u32; lw * lh])
        .collect();
    let mut locked: Vec<Vec<bool>> = levels
        .iter()
        .map(|(lw, lh, _)| vec![false; lw * lh])
        .collect();

    let threshold = 1u32;
    for ly in 0..h {
        for lx in 0..w {
            for k in (0..n_levels).rev() {
                let nx = lx >> k;
                let ny = ly >> k;
                let nw = levels[k].0;
                let idx = ny * nw + nx;
                if locked[k][idx] {
                    continue;
                }
                let node_value = levels[k].2[idx];
                while lows[k][idx] < threshold && lows[k][idx] < node_value {
                    bw.write_bit(0);
                    lows[k][idx] += 1;
                }
                if lows[k][idx] == node_value && lows[k][idx] < threshold {
                    bw.write_bit(1);
                    locked[k][idx] = true;
                } else if lows[k][idx] == node_value {
                    // No-op (matches decoder's loop exit conditions).
                }
            }
        }
    }
}

/// Write the SIZ marker. `Lsiz = 38 + 3 * Csiz`. Each component carries
/// `(Ssiz, XRsiz, YRsiz)`. Rsiz bit 14 is set (HTJ2K signal, T.814 §A.2).
fn write_siz_ht(
    out: &mut Vec<u8>,
    w: u32,
    h: u32,
    tw: u32,
    th: u32,
    precision: u32,
    dims: &[CompDims],
) -> Result<()> {
    let csiz = dims.len();
    if !(1..=16383).contains(&csiz) {
        return Err(Error::invalid(
            "HTJ2K encode: SIZ component count out of range",
        ));
    }
    let lsiz = 38 + 3 * csiz;
    if lsiz > u16::MAX as usize {
        return Err(Error::invalid("HTJ2K encode: SIZ segment too long"));
    }
    out.extend_from_slice(&[0xFF, 0x51]);
    out.extend_from_slice(&(lsiz as u16).to_be_bytes());
    out.extend_from_slice(&0x4000u16.to_be_bytes()); // Rsiz: HTJ2K bit 14
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&tw.to_be_bytes());
    out.extend_from_slice(&th.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(csiz as u16).to_be_bytes());
    for d in dims {
        out.push((precision - 1) as u8);
        out.push(d.xrsiz);
        out.push(d.yrsiz);
    }
    Ok(())
}

fn write_cap_ht(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0xFF, 0x50]);
    out.extend_from_slice(&8u16.to_be_bytes());
    out.extend_from_slice(&0x0002_0000u32.to_be_bytes());
    out.extend_from_slice(&0x0000u16.to_be_bytes());
}

/// Build the COD payload (the bytes after marker + Lcod). The classic
/// encoder writes the same layout (T.800 §A.6.1) plus the cblk_style HT
/// bit (bit 6) and the transform byte (5/3 = 1, 9/7 = 0).
fn build_cod_payload(cblk_log2: u8, nl: u8, apply_mct: bool, transform: HtTransform) -> Vec<u8> {
    let mut p = Vec::with_capacity(12);
    p.push(0); // Scod
    p.push(0); // progression: LRCP
    p.extend_from_slice(&1u16.to_be_bytes()); // num_layers
    p.push(if apply_mct { 1 } else { 0 }); // MCT
    let cw = cblk_log2 - 2;
    let ch = cblk_log2 - 2;
    let transform_byte = match transform {
        HtTransform::Reversible53 => 1u8,
        HtTransform::Irreversible97 => 0u8,
    };
    p.extend_from_slice(&[nl, cw, ch, 0x40, transform_byte]);
    p
}

/// Build the QCD payload for the 5/3 reversible path. `1 + 3 * NL` bands.
fn build_qcd_reversible_payload(precision: u8, nl: u8) -> Vec<u8> {
    let num_bands = 1usize + 3 * nl as usize;
    let mut p = Vec::with_capacity(1 + num_bands);
    p.push(0); // Sqcd: qntsty=0 (no quantization), guard_bits=0
    p.push(precision << 3);
    for _r in 1..=nl {
        p.push((precision + 1) << 3);
        p.push((precision + 1) << 3);
        p.push((precision + 2) << 3);
    }
    p
}

/// Build the QCD payload for the 9/7 irreversible path in expounded
/// form (qntsty = 2). One `(eps, mu)` 16-bit pair per band, with
/// `mu = 0` so `stepsize_b = 2^(precision - eps_b)`.
fn build_qcd_irreversible_payload(precision: u8, nl: u8) -> Vec<u8> {
    let num_bands = 1usize + 3 * nl as usize;
    let mut p = Vec::with_capacity(1 + 2 * num_bands);
    p.push(0x02); // Sqcd: qntsty=2 (expounded), guard_bits=0
    let push_band = |p: &mut Vec<u8>, eps: u8| {
        let v: u16 = (eps as u16 & 0x1F) << 11;
        p.extend_from_slice(&v.to_be_bytes());
    };
    push_band(&mut p, precision);
    for _r in 1..=nl {
        push_band(&mut p, precision + 1);
        push_band(&mut p, precision + 1);
        push_band(&mut p, precision + 2);
    }
    p
}

/// Serialise a PPM marker payload (`Zppm` (1) + per-tile-part `(Nppm,
/// Ippm)` records). One PPM segment with `Zppm = 0` covering every
/// tile-part's header byte run in order.
fn build_ppm_payload(per_tile_headers: &[Vec<u8>]) -> Vec<u8> {
    let mut payload =
        Vec::with_capacity(1 + per_tile_headers.iter().map(|h| 4 + h.len()).sum::<usize>());
    payload.push(0u8); // Zppm = 0
    for h in per_tile_headers {
        payload.extend_from_slice(&(h.len() as u32).to_be_bytes());
        payload.extend_from_slice(h);
    }
    payload
}

/// Write a PPT marker segment (`FF 61` + Lppt + Zppt + Ippt bytes).
fn write_ppt_marker(out: &mut Vec<u8>, headers: &[u8]) -> Result<()> {
    let lppt = (1 + headers.len() + 2) as u16;
    out.extend_from_slice(&[0xFF, 0x61]);
    out.extend_from_slice(&lppt.to_be_bytes());
    out.push(0u8); // Zppt = 0
    out.extend_from_slice(headers);
    Ok(())
}

/// MSB-first bit writer with the FF-stuffing rule mirroring the decoder
/// `Bio` reader.
struct BioWriterMsbFirst {
    buf: Vec<u8>,
    cur: u8,
    nbits: u8,
    pending_ff: bool,
}

impl BioWriterMsbFirst {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            cur: 0,
            nbits: 0,
            pending_ff: false,
        }
    }

    fn write_bit(&mut self, bit: u8) {
        let cap: u8 = if self.pending_ff { 7 } else { 8 };
        if self.nbits == cap {
            self.flush_one();
        }
        let cap_now: u8 = if self.pending_ff { 7 } else { 8 };
        let pos = (cap_now - 1) - self.nbits;
        self.cur |= (bit & 1) << pos;
        self.nbits += 1;
    }

    fn flush_one(&mut self) {
        let b = self.cur;
        self.buf.push(b);
        self.pending_ff = b == 0xFF;
        self.cur = 0;
        self.nbits = 0;
    }

    fn flush_aligned(mut self, out: &mut Vec<u8>) {
        if self.nbits > 0 {
            self.flush_one();
        }
        out.extend_from_slice(&self.buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Jpeg2000Plane;
    use crate::{decode_jpeg2000, probe, J2kFlavour};

    fn build_gray_solid(w: u32, h: u32, value: u8) -> Jpeg2000Image {
        Jpeg2000Image {
            width: w,
            height: h,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: w as usize,
                data: vec![value; (w * h) as usize],
            }],
            pts: None,
        }
    }

    /// 32×32 solid 0x80 image, NL=0, single codeblock.
    #[test]
    fn roundtrip_solid_dc_32x32_nl0() {
        let img = build_gray_solid(32, 32, 0x80);
        let cs = encode_image_htj2k(&img, &EncodeOptionsHt::default()).expect("encode");
        let p = probe(&cs).expect("probe");
        assert_eq!(p.flavour, J2kFlavour::HighThroughput);
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, img.planes[0].data);
    }

    /// 32×32 solid 0x80, NL=1.
    #[test]
    fn roundtrip_solid_dc_32x32_nl1() {
        let img = build_gray_solid(32, 32, 0x80);
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, img.planes[0].data);
    }

    /// Sparse pattern at NL=1.
    #[test]
    fn roundtrip_sparse_32x32_nl1() {
        let mut data = vec![0x80u8; 32 * 32];
        data[0] = 0x81;
        data[5 * 32 + 5] = 0x7F;
        data[10 * 32 + 10] = 0x82;
        let img = Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: 32,
                data: data.clone(),
            }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }

    /// Round 1 sparse fixture (still passes the original sparse contract).
    #[test]
    fn roundtrip_sparse_one_per_quad_32x32() {
        let mut data = vec![0x80u8; 32 * 32];
        data[0] = 0x81;
        data[4 * 32 + 4] = 0x7F;
        let img = Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: 32,
                data: data.clone(),
            }],
            pts: None,
        };
        let cs = encode_image_htj2k(&img, &EncodeOptionsHt::default()).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }

    /// 64x64 image at NL=2 with a centred bright square.
    #[test]
    fn roundtrip_64x64_nl2_square() {
        let mut data = vec![0x40u8; 64 * 64];
        for y in 24..40 {
            for x in 24..40 {
                data[y * 64 + x] = 0xC0;
            }
        }
        let img = Jpeg2000Image {
            width: 64,
            height: 64,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: 64,
                data: data.clone(),
            }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 2,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }

    /// 64x64 image at NL=3 — every band has 1 codeblock; tests deeper
    /// pyramid + multiple resolution-level packets.
    #[test]
    fn roundtrip_64x64_nl3_gradient() {
        let mut data = Vec::with_capacity(64 * 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = ((x + y) * 4).min(255) as u8;
                data.push(v);
            }
        }
        let img = Jpeg2000Image {
            width: 64,
            height: 64,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: 64,
                data: data.clone(),
            }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 3,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }

    /// 32x32 noise pattern at NL=1 — exercises multi-significance in
    /// every band.
    #[test]
    fn roundtrip_32x32_nl1_noise() {
        let mut data = Vec::with_capacity(32 * 32);
        for i in 0..(32 * 32) {
            data.push(((i * 17) % 251) as u8);
        }
        let img = Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: 32,
                data: data.clone(),
            }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }

    /// 32x32 RGB solid colour, no MCT.
    #[test]
    fn roundtrip_32x32_rgb_no_mct() {
        let mut data = Vec::with_capacity(32 * 32 * 3);
        for _ in 0..(32 * 32) {
            data.push(0xC0);
            data.push(0x40);
            data.push(0x80);
        }
        let img = Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Rgb24,
            planes: vec![Jpeg2000Plane { stride: 96, data }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            use_color_transform: false,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.pixel_format, PixelFormat::Yuv444P);
        assert_eq!(decoded.planes.len(), 3);
        assert!(decoded.planes[0].data.iter().all(|&v| v == 0xC0));
        assert!(decoded.planes[1].data.iter().all(|&v| v == 0x40));
        assert!(decoded.planes[2].data.iter().all(|&v| v == 0x80));
    }

    /// 32x32 RGB solid with MCT (forward RCT).
    #[test]
    fn roundtrip_32x32_rgb_with_mct() {
        let mut data = Vec::with_capacity(32 * 32 * 3);
        for _ in 0..(32 * 32) {
            data.push(0xC0);
            data.push(0x40);
            data.push(0x80);
        }
        let img = Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Rgb24,
            planes: vec![Jpeg2000Plane { stride: 96, data }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let p = probe(&cs).expect("probe");
        assert_eq!(p.num_components, 3);
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes.len(), 3);
        assert!(decoded.planes[0].data.iter().all(|&v| v == 0xC0));
        assert!(decoded.planes[1].data.iter().all(|&v| v == 0x40));
        assert!(decoded.planes[2].data.iter().all(|&v| v == 0x80));
    }

    /// 32x32 RGB gradient with MCT at NL=1.
    #[test]
    fn roundtrip_32x32_rgb_gradient_mct_nl1() {
        let mut data = Vec::with_capacity(32 * 32 * 3);
        for y in 0..32u32 {
            for x in 0..32u32 {
                let r = ((x * 8) & 0xFF) as u8;
                let g = ((y * 8) & 0xFF) as u8;
                let b = (((x + y) * 4) & 0xFF) as u8;
                data.push(r);
                data.push(g);
                data.push(b);
            }
        }
        let stride = 32 * 3;
        let img = Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Rgb24,
            planes: vec![Jpeg2000Plane {
                stride,
                data: data.clone(),
            }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        for y in 0..32usize {
            for x in 0..32usize {
                let off = y * stride + 3 * x;
                assert_eq!(decoded.planes[0].data[y * 32 + x], data[off]);
                assert_eq!(decoded.planes[1].data[y * 32 + x], data[off + 1]);
                assert_eq!(decoded.planes[2].data[y * 32 + x], data[off + 2]);
            }
        }
    }

    /// 32x32 Yuv444P sparse — 3-component planar input, no MCT.
    #[test]
    fn roundtrip_32x32_yuv444_planar() {
        let mut y = vec![0x80u8; 32 * 32];
        let mut cb = vec![0x40u8; 32 * 32];
        let mut cr = vec![0xC0u8; 32 * 32];
        y[0] = 0x81;
        cb[5 * 32 + 5] = 0x42;
        cr[10 * 32 + 10] = 0xBE;
        let img = Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Yuv444P,
            planes: vec![
                Jpeg2000Plane {
                    stride: 32,
                    data: y.clone(),
                },
                Jpeg2000Plane {
                    stride: 32,
                    data: cb.clone(),
                },
                Jpeg2000Plane {
                    stride: 32,
                    data: cr.clone(),
                },
            ],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.pixel_format, PixelFormat::Yuv444P);
        assert_eq!(decoded.planes[0].data, y);
        assert_eq!(decoded.planes[1].data, cb);
        assert_eq!(decoded.planes[2].data, cr);
    }

    /// Round-4: 32×32 Yuv420P planar input — chroma at 16×16.
    #[test]
    fn roundtrip_32x32_yuv420p() {
        let y = vec![0x80u8; 32 * 32];
        let cb = vec![0x40u8; 16 * 16];
        let cr = vec![0xC0u8; 16 * 16];
        let img = Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Yuv420P,
            planes: vec![
                Jpeg2000Plane {
                    stride: 32,
                    data: y.clone(),
                },
                Jpeg2000Plane {
                    stride: 16,
                    data: cb.clone(),
                },
                Jpeg2000Plane {
                    stride: 16,
                    data: cr.clone(),
                },
            ],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            use_color_transform: false,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let p = probe(&cs).expect("probe");
        assert_eq!(p.num_components, 3);
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.pixel_format, PixelFormat::Yuv420P);
        assert_eq!(decoded.planes[0].data, y);
        assert_eq!(decoded.planes[1].data, cb);
        assert_eq!(decoded.planes[2].data, cr);
    }

    /// Round-4: 32×32 Yuv422P planar input — chroma at 16×32.
    #[test]
    fn roundtrip_32x32_yuv422p() {
        let y = vec![0x80u8; 32 * 32];
        let cb = vec![0x40u8; 16 * 32];
        let cr = vec![0xC0u8; 16 * 32];
        let img = Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Yuv422P,
            planes: vec![
                Jpeg2000Plane {
                    stride: 32,
                    data: y.clone(),
                },
                Jpeg2000Plane {
                    stride: 16,
                    data: cb.clone(),
                },
                Jpeg2000Plane {
                    stride: 16,
                    data: cr.clone(),
                },
            ],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            use_color_transform: false,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.pixel_format, PixelFormat::Yuv422P);
        assert_eq!(decoded.planes[0].data, y);
        assert_eq!(decoded.planes[1].data, cb);
        assert_eq!(decoded.planes[2].data, cr);
    }

    /// Round-4: 9/7 irreversible round-trip on a solid-DC fixture.
    /// Even with quantisation, a constant image should round-trip very
    /// close to the original (DC coefficient sits in the LL band; the
    /// stepsize is 1 so the only loss is rounding noise).
    #[test]
    fn roundtrip_9_7_solid_dc_32x32() {
        let img = build_gray_solid(32, 32, 0x80);
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            transform: HtTransform::Irreversible97,
            use_color_transform: false,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        // The 9/7 path is float — allow ±2 LSB on the solid fixture.
        for (i, (&d, &o)) in decoded.planes[0]
            .data
            .iter()
            .zip(img.planes[0].data.iter())
            .enumerate()
        {
            let diff = d as i32 - o as i32;
            assert!(
                diff.abs() <= 2,
                "9/7 solid roundtrip drift at {i}: decoded={d} orig={o}"
            );
        }
    }

    /// Round-4: 9/7 irreversible on a 64×64 gradient.
    #[test]
    fn roundtrip_9_7_gradient_64x64_nl2() {
        let mut data = Vec::with_capacity(64 * 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = ((x + y) * 4).min(255) as u8;
                data.push(v);
            }
        }
        let img = Jpeg2000Image {
            width: 64,
            height: 64,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: 64,
                data: data.clone(),
            }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 2,
            transform: HtTransform::Irreversible97,
            use_color_transform: false,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        // 9/7 is lossy — measure mean absolute deviation. Stepsize = 1,
        // so the worst-case loss is ~1 LSB per sample but accumulated
        // float error in the lifting can drift up to a few LSB on a
        // textured 64×64 fixture.
        let mut sum_abs = 0u64;
        let mut max_dev = 0i32;
        for (&d, &o) in decoded.planes[0].data.iter().zip(data.iter()) {
            let diff = (d as i32 - o as i32).abs();
            sum_abs += diff as u64;
            max_dev = max_dev.max(diff);
        }
        let mad = sum_abs as f64 / data.len() as f64;
        assert!(
            mad <= 4.0,
            "9/7 gradient MAD {mad:.2} > 4.0 (max {max_dev})"
        );
    }

    /// Round-4: multi-tile 64×64 image with `XTsiz=YTsiz=32` — 4 tiles,
    /// each one 32×32. Self round-trip must be bit-exact (5/3 path).
    #[test]
    fn roundtrip_multitile_64x64_2x2() {
        let mut data = Vec::with_capacity(64 * 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = ((x + 3 * y) & 0xFF) as u8;
                data.push(v);
            }
        }
        let img = Jpeg2000Image {
            width: 64,
            height: 64,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: 64,
                data: data.clone(),
            }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            tile_size: Some((32, 32)),
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }

    /// Round-4: multi-tile with non-aligned tile sizes (image not a
    /// multiple of XTsiz/YTsiz).
    #[test]
    fn roundtrip_multitile_48x48_nonaligned() {
        let mut data = Vec::with_capacity(48 * 48);
        for y in 0..48 {
            for x in 0..48 {
                let v = ((x ^ y) * 4).min(255) as u8;
                data.push(v);
            }
        }
        let img = Jpeg2000Image {
            width: 48,
            height: 48,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: 48,
                data: data.clone(),
            }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            tile_size: Some((32, 32)),
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }

    /// Round-4: multi-tile + RGB+MCT.
    #[test]
    fn roundtrip_multitile_64x64_rgb_mct() {
        let mut data = Vec::with_capacity(64 * 64 * 3);
        for y in 0..64u32 {
            for x in 0..64u32 {
                data.push(((x * 4) & 0xFF) as u8);
                data.push(((y * 4) & 0xFF) as u8);
                data.push((((x + y) * 2) & 0xFF) as u8);
            }
        }
        let stride = 64 * 3;
        let img = Jpeg2000Image {
            width: 64,
            height: 64,
            pixel_format: PixelFormat::Rgb24,
            planes: vec![Jpeg2000Plane {
                stride,
                data: data.clone(),
            }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 1,
            tile_size: Some((32, 32)),
            use_color_transform: true,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        for y in 0..64usize {
            for x in 0..64usize {
                let off = y * stride + 3 * x;
                assert_eq!(decoded.planes[0].data[y * 64 + x], data[off]);
                assert_eq!(decoded.planes[1].data[y * 64 + x], data[off + 1]);
                assert_eq!(decoded.planes[2].data[y * 64 + x], data[off + 2]);
            }
        }
    }

    /// Round-4: PPM-packed packet headers self round-trip.
    #[test]
    fn roundtrip_ppm_packed_headers_64x64() {
        let mut data = Vec::with_capacity(64 * 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = ((x + y) * 4).min(255) as u8;
                data.push(v);
            }
        }
        let img = Jpeg2000Image {
            width: 64,
            height: 64,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: 64,
                data: data.clone(),
            }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 2,
            packet_header_placement: HtPacketHeaderPlacement::PackedMainHeader,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }

    /// Round-4: PPT-packed packet headers self round-trip.
    #[test]
    fn roundtrip_ppt_packed_headers_64x64() {
        let mut data = Vec::with_capacity(64 * 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = ((x + y) * 4).min(255) as u8;
                data.push(v);
            }
        }
        let img = Jpeg2000Image {
            width: 64,
            height: 64,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: 64,
                data: data.clone(),
            }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            num_decomp: 2,
            packet_header_placement: HtPacketHeaderPlacement::PackedPerTilePart,
            ..Default::default()
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }
}
