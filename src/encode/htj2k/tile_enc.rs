//! HTJ2K codestream + tile-body encoder (round 3).
//!
//! Wraps [`super::cleanup_enc::encode_cleanup`] in the marker chain
//! that ISO/IEC 15444-15 requires: SOC + SIZ (with Rsiz bit 14 set
//! per §A.2) + CAP (Pcap15 + Ccap15) + COD (with SPcod cblk_style
//! bit 6 set per Table A.3) + QCD + SOT + SOD + EOC.
//!
//! Round-3 scope (delta over round 2):
//!
//! * Multi-component encode for `Gray8`, `Rgb24`, and `Yuv444P` input
//!   pixel formats. SIZ writes `Csiz = N` with the matching per-
//!   component `XRsiz`/`YRsiz`; the tier-2 packet emit loop writes one
//!   packet per `(resolution, component)` in LRCP order.
//! * Optional forward 5/3 reversible component transform (RCT) for RGB
//!   input via [`EncodeOptionsHt::use_color_transform`]; signalled in
//!   COD by setting the `MCT` byte to 1 (T.800 §A.6.1 / §G.1). The
//!   crate's HTJ2K decoder already inverts the RCT when `MCT == 1` and
//!   the COD transform byte selects 5/3 (commit `a2df342`).
//!
//! Carried over from round 2:
//!
//! * Forward 5/3 reversible DWT for `NL ∈ [0, 5]` decomposition
//!   levels via [`crate::encode::dwt::fdwt_53`].
//! * HT cleanup pass encoder ([`super::cleanup_enc::encode_cleanup`])
//!   with full multi-significance per quad and the §7.3.6 Eq-4
//!   first-line-pair both-`u_off=1` special case.
//! * Single tile, single quality layer.
//!
//! Out of scope (round 4+):
//!
//! * SigProp / MagRef refinement passes (Z_blk ∈ {2, 3}).
//! * Multi-tile (the HTJ2K decoder rejects multi-tile-part codestreams
//!   today; this encoder matches that limit).
//! * Sub-sampled chroma (4:2:2 / 4:2:0). Both sides need per-component
//!   sub-band layouts at the sub-sampled extent.
//! * 9/7 irreversible transform path (encoder still 5/3 only; decoder
//!   has a 9/7 HTJ2K synthesis path already).
//! * PPM/PPT packet headers (§A.7.4 / §A.7.5).
//! * Constrained sets (T.814 §8) and multi-set HT (T.814 Annex B).

use super::cleanup_enc::{encode_cleanup, SampleHt};
use crate::decode::tile::build_subbands;
use crate::encode::dwt::fdwt_53;
use crate::error::{Jpeg2000Error as Error, Result};
use crate::image::{Jpeg2000Image, Jpeg2000PixelFormat as PixelFormat};

/// Knobs for the HTJ2K encoder.
#[derive(Debug, Clone)]
pub struct EncodeOptionsHt {
    /// Code-block width log2. Default 5 (= 32). Round-3 uses the same
    /// value for both dimensions.
    pub cblk_log2: u8,
    /// Number of decomposition levels (NL). Round-3 supports `0..=5`.
    pub num_decomp: u8,
    /// When `true` and the input is `Rgb24`, apply the forward 5/3
    /// reversible component transform (RCT, T.800 §G.1) and signal
    /// `MCT = 1` in COD. Ignored for non-RGB input. Defaults to true.
    pub use_color_transform: bool,
}

impl Default for EncodeOptionsHt {
    fn default() -> Self {
        EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 0,
            use_color_transform: true,
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
    /// Width / height in samples on the component grid (`ceil(W / XRsiz)`
    /// / `ceil(H / YRsiz)`).
    w: u32,
    h: u32,
    /// Sub-sampling factors signalled in SIZ.
    xrsiz: u8,
    yrsiz: u8,
}

/// Encode a [`Jpeg2000Image`] as a 5/3 reversible HTJ2K codestream.
///
/// Round-3 supports `num_decomp ∈ [0, 5]`. Pixel formats accepted:
///
/// * `Gray8` — 1 component, 8-bit unsigned.
/// * `Rgb24` — 3 components, 8-bit unsigned. With
///   `use_color_transform = true` the encoder applies the forward RCT
///   (`Y = (R + 2G + B) >> 2`, `Cb = B - G`, `Cr = R - G`) and signals
///   `MCT = 1` in COD; without it the channels are encoded
///   independently and the decoder reads them back as planar YCbCr 4:4:4.
/// * `Yuv444P` — 3 components at full resolution, 8-bit unsigned. No
///   MCT (the channels are already in the YCbCr basis).
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
    let dc_shift = 1i32 << (precision - 1);
    let nl = opts.num_decomp;
    let cblk_log2 = opts.cblk_log2;

    // -- 1. Extract per-component i32 sample planes (DC-level shifted
    // for unsigned 8-bit input) and decide whether MCT is in play.
    let (mut comp_planes, comp_dims, apply_mct) =
        extract_components_i32(image, opts.use_color_transform, dc_shift)?;
    let num_comps = comp_planes.len();

    // -- 2. Forward 5/3 DWT per component, level-by-level. Each level
    // operates on the LL quadrant of the previous level (top-left
    // `ceil(w_r / 2) × ceil(h_r / 2)` slice).
    for (ci, plane) in comp_planes.iter_mut().enumerate() {
        let cw = comp_dims[ci].w as usize;
        let ch = comp_dims[ci].h as usize;
        let mut cur_w = cw;
        let mut cur_h = ch;
        for _level in 0..nl as usize {
            if cur_w < 2 || cur_h < 2 {
                break;
            }
            fdwt_53(plane, cur_w, cur_h, cw);
            cur_w = cur_w.div_ceil(2);
            cur_h = cur_h.div_ceil(2);
        }
    }

    // -- 3. Per-component subband layout + per-codeblock encode.
    let cblk_dim = 1u32 << cblk_log2;
    // When MCT (forward RCT) is active, the chroma channels carry one
    // extra bit of dynamic range (Cb = B - G, Cr = R - G can reach
    // ±(2^precision - 1) → 9 bits for 8-bit RGB input). QCD is shared
    // across all components in our encoder (no QCC), so we bump every
    // band's epsilon by 1 in that case to give the chroma headroom.
    // For luma this is just spare capacity; the cleanup pass still
    // round-trips bit-exactly.
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

    let num_res = (nl as usize) + 1;
    let mut per_comp: Vec<CompCoded> = Vec::with_capacity(num_comps);
    for ci in 0..num_comps {
        let cw = comp_dims[ci].w as usize;
        let ch = comp_dims[ci].h as usize;
        let comp_w = comp_dims[ci].w;
        let comp_h = comp_dims[ci].h;
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

    // -- 4. Build tier-2 packet body: LRCP, single layer, single tile,
    // default precincts. One packet per (resolution, component); within
    // a packet, all bands at that resolution. Indexed loop because
    // `per_comp[ci][resno]` reads two axes in spec order.
    let mut body: Vec<u8> = Vec::new();
    #[allow(clippy::needless_range_loop)]
    for resno in 0..num_res {
        for ci in 0..num_comps {
            emit_packet_htj2k(&mut body, &per_comp[ci][resno])?;
        }
    }

    // -- 5. Assemble the full codestream --
    let mut cs = Vec::<u8>::new();
    cs.extend_from_slice(&[0xFF, 0x4F]); // SOC
    write_siz_ht(&mut cs, w, h, precision, &comp_dims)?;
    write_cap_ht(&mut cs);
    write_cod_ht(&mut cs, cblk_log2, nl, apply_mct);
    write_qcd_reversible(&mut cs, prec_for_qcd as u8, nl);
    let sot_off = cs.len();
    cs.extend_from_slice(&[0xFF, 0x90]);
    cs.extend_from_slice(&10u16.to_be_bytes());
    cs.extend_from_slice(&0u16.to_be_bytes());
    let psot_pos = cs.len();
    cs.extend_from_slice(&0u32.to_be_bytes());
    cs.extend_from_slice(&[0, 1]);
    cs.extend_from_slice(&[0xFF, 0x93]); // SOD
    cs.extend_from_slice(&body);
    let tile_part_end = cs.len();
    let psot = (tile_part_end - sot_off) as u32;
    cs[psot_pos..psot_pos + 4].copy_from_slice(&psot.to_be_bytes());
    cs.extend_from_slice(&[0xFF, 0xD9]); // EOC
    Ok(cs)
}

/// Pull per-component i32 sample planes out of the input image. Returns
/// `(planes, dims, apply_mct)` where each plane is row-major at its
/// component's `(w, h)` extent and DC-level-shifted (i.e. unsigned 8-bit
/// → signed-centered i32 by subtracting `1 << (precision - 1)`). For
/// `Rgb24` with `use_color_transform = true` the forward 5/3 reversible
/// RCT is applied: Y is DC-level shifted; Cb/Cr are already centered.
fn extract_components_i32(
    image: &Jpeg2000Image,
    use_color_transform: bool,
    dc_shift: i32,
) -> Result<(Vec<Vec<i32>>, Vec<CompDims>, bool)> {
    let w = image.width;
    let h = image.height;
    let n = (w as usize) * (h as usize);
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
            let dims = vec![CompDims {
                w,
                h,
                xrsiz: 1,
                yrsiz: 1,
            }];
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
            let dims = vec![
                CompDims {
                    w,
                    h,
                    xrsiz: 1,
                    yrsiz: 1,
                };
                3
            ];
            if use_color_transform {
                // Forward 5/3 reversible RCT (T.800 §G.1):
                //   Y  = floor((R + 2G + B) / 4)
                //   Cb = B - G
                //   Cr = R - G
                // Y is in [0, 255] before DC shift; chroma is signed and
                // already centered.
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
            let dims = vec![
                CompDims {
                    w,
                    h,
                    xrsiz: 1,
                    yrsiz: 1,
                };
                3
            ];
            // YUV444P is already in luma/chroma basis — never apply MCT.
            Ok((planes_i32, dims, false))
        }
        _ => Err(Error::unsupported(format!(
            "HTJ2K encode: pixel format {:?} not yet supported (Gray8 / Rgb24 / Yuv444P only)",
            image.pixel_format
        ))),
    }
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
            // convention (leaf_value = missing_msb - 1, threshold sweep
            // 0..=missing_msb finds break at threshold = missing_msb).
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

/// Tag-tree threshold=1 sweep encoder.
///
/// The decoder walks each leaf top-down through the tag tree, updating
/// a running lower-bound `low` per node by importing `0` bits, until
/// the bound reaches either `threshold` (give up) or the node value
/// (lock with `1` bit). At threshold=1 the only outcomes per leaf are:
///   * leaf == 0 (included): emit one `1` bit at the leaf (and zero
///     bits at any unlocked ancestor below the leaf's value).
///   * leaf >= 1 (not included): emit one `0` bit at the leaf — `low`
///     reaches threshold without locking.
fn encode_tagtree_threshold1(bw: &mut BioWriterMsbFirst, w: usize, h: usize, leaves: &[u32]) {
    // Build the tree: levels[0] = leaves, levels[1] = parents of pairs
    // in levels[0], etc. Each parent's value = min of its 2x2 children.
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
            // Walk root-to-leaf, emitting bits per node.
            for k in (0..n_levels).rev() {
                // At level k (0 = leaf), the node index is
                // (lx >> k, ly >> k).
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
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
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

fn write_cod_ht(out: &mut Vec<u8>, cblk_log2: u8, nl: u8, apply_mct: bool) {
    out.extend_from_slice(&[0xFF, 0x52]);
    out.extend_from_slice(&12u16.to_be_bytes());
    // Scod=0, SGcod (4 bytes): progression=0 (LRCP), num layers BE u16=1, mct=apply_mct.
    out.push(0); // Scod
    out.push(0); // progression: LRCP
    out.extend_from_slice(&1u16.to_be_bytes()); // num_layers
    out.push(if apply_mct { 1 } else { 0 }); // MCT
    let cw = cblk_log2 - 2;
    let ch = cblk_log2 - 2;
    // SPcod: NL, xcb, ycb, cbsty=0x40 (HT), transform=1 (5/3).
    out.extend_from_slice(&[nl, cw, ch, 0x40, 1]);
}

/// QCD: reversible 5/3 with `1 + 3 * NL` bands. eps_b = precision +
/// log2_gain_b: LL=0, HL/LH=1, HH=2.
fn write_qcd_reversible(out: &mut Vec<u8>, precision: u8, nl: u8) {
    let num_bands = 1usize + 3 * nl as usize;
    out.extend_from_slice(&[0xFF, 0x5C]);
    out.extend_from_slice(&((3 + num_bands) as u16).to_be_bytes());
    out.push(0);
    out.push(precision << 3);
    for _r in 1..=nl {
        out.push((precision + 1) << 3);
        out.push((precision + 1) << 3);
        out.push((precision + 2) << 3);
    }
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
            cblk_log2: 5,
            num_decomp: 1,
            use_color_transform: true,
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
            cblk_log2: 5,
            num_decomp: 1,
            use_color_transform: true,
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
            cblk_log2: 5,
            num_decomp: 2,
            use_color_transform: true,
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
            cblk_log2: 5,
            num_decomp: 3,
            use_color_transform: true,
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
            cblk_log2: 5,
            num_decomp: 1,
            use_color_transform: true,
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }

    /// 32x32 RGB solid colour, no MCT — three independent components,
    /// decoder reads back as planar Yuv444P (since the channels stay in
    /// their raw RGB basis but the pixel-format mapper picks the planar
    /// 4:4:4 layout).
    #[test]
    fn roundtrip_32x32_rgb_no_mct() {
        let mut data = Vec::with_capacity(32 * 32 * 3);
        for _ in 0..(32 * 32) {
            data.push(0xC0); // R
            data.push(0x40); // G
            data.push(0x80); // B
        }
        let img = Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Rgb24,
            planes: vec![Jpeg2000Plane { stride: 96, data }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 1,
            use_color_transform: false,
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.pixel_format, PixelFormat::Yuv444P);
        // Three planes, each constant at the RGB component value.
        assert_eq!(decoded.planes.len(), 3);
        assert!(decoded.planes[0].data.iter().all(|&v| v == 0xC0));
        assert!(decoded.planes[1].data.iter().all(|&v| v == 0x40));
        assert!(decoded.planes[2].data.iter().all(|&v| v == 0x80));
    }

    /// 32x32 RGB solid with MCT (forward RCT). The decoder inverts the
    /// RCT and exposes the RGB triple again as planar Yuv444P planes
    /// (R, G, B in plane order — the decoder restores the original
    /// pre-RCT planes in slots 0/1/2).
    #[test]
    fn roundtrip_32x32_rgb_with_mct() {
        let mut data = Vec::with_capacity(32 * 32 * 3);
        for _ in 0..(32 * 32) {
            data.push(0xC0); // R
            data.push(0x40); // G
            data.push(0x80); // B
        }
        let img = Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Rgb24,
            planes: vec![Jpeg2000Plane { stride: 96, data }],
            pts: None,
        };
        let opts = EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 1,
            use_color_transform: true,
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let p = probe(&cs).expect("probe");
        assert_eq!(p.num_components, 3);
        let decoded = decode_jpeg2000(&cs).expect("decode");
        // After inverse RCT the decoder restores the R/G/B planes.
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
            cblk_log2: 5,
            num_decomp: 1,
            use_color_transform: true,
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        // Bit-exact roundtrip on per-channel u8 data.
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
            cblk_log2: 5,
            num_decomp: 1,
            use_color_transform: true, // ignored for YUV input
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.pixel_format, PixelFormat::Yuv444P);
        assert_eq!(decoded.planes[0].data, y);
        assert_eq!(decoded.planes[1].data, cb);
        assert_eq!(decoded.planes[2].data, cr);
    }
}
