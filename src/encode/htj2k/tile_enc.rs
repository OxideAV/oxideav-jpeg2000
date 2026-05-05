//! HTJ2K codestream + tile-body encoder (round 2).
//!
//! Wraps [`super::cleanup_enc::encode_cleanup`] in the marker chain
//! that ISO/IEC 15444-15 requires: SOC + SIZ (with Rsiz bit 14 set
//! per §A.2) + CAP (Pcap15 + Ccap15) + COD (with SPcod cblk_style
//! bit 6 set per Table A.3) + QCD + SOT + SOD + EOC.
//!
//! Round-2 scope:
//!
//! * Single-tile, single-component (luma `Gray8`), 8-bit unsigned.
//! * Forward 5/3 reversible DWT for `NL ∈ [0, 5]` decomposition
//!   levels via [`crate::encode::dwt::fdwt_53`].
//! * Codeblock partition is the `2^cblk_log2 × 2^cblk_log2` grid of the
//!   PPx=PPy=15 default precincts. Each band's codeblocks are encoded
//!   independently via the cleanup encoder.
//! * Multi-significance per quad is fully supported (round-2 enabled
//!   the EMB table search in [`super::cxt_vlc_enc::pick_emb_for_uoff1`]).
//! * Single quality layer, LRCP progression.
//! * Reversible 5/3 transform byte signalled in COD.

use super::cleanup_enc::{encode_cleanup, SampleHt};
use crate::decode::tile::build_subbands;
use crate::encode::dwt::fdwt_53;
use crate::error::{Jpeg2000Error as Error, Result};
use crate::image::{Jpeg2000Image, Jpeg2000PixelFormat as PixelFormat};

/// Knobs for the HTJ2K encoder.
#[derive(Debug, Clone)]
pub struct EncodeOptionsHt {
    /// Code-block width log2. Default 5 (= 32). Round-2 uses the same
    /// value for both dimensions.
    pub cblk_log2: u8,
    /// Number of decomposition levels (NL). Round 2 supports
    /// `0..=5`.
    pub num_decomp: u8,
}

impl Default for EncodeOptionsHt {
    fn default() -> Self {
        EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 0,
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

#[derive(Default)]
struct ResEnc {
    bands: Vec<BandEnc>,
}

/// Encode a [`Jpeg2000Image`] as a 5/3 reversible HTJ2K codestream.
///
/// Round 2 supports `num_decomp ∈ [0, 5]` for single-component Gray8
/// streams. The image dimensions are arbitrary (need not be a power of
/// two).
pub fn encode_image_htj2k(image: &Jpeg2000Image, opts: &EncodeOptionsHt) -> Result<Vec<u8>> {
    let w = image.width;
    let h = image.height;
    if w == 0 || h == 0 {
        return Err(Error::invalid("HTJ2K encode: zero-dimension image"));
    }
    if image.pixel_format != PixelFormat::Gray8 {
        return Err(Error::unsupported(
            "HTJ2K encode (round 2): only Gray8 supported (multi-component round 3+)",
        ));
    }
    if opts.num_decomp > 5 {
        return Err(Error::unsupported(format!(
            "HTJ2K encode (round 2): num_decomp = {} > 5",
            opts.num_decomp
        )));
    }
    if image.planes.len() != 1 {
        return Err(Error::invalid(
            "HTJ2K encode: Gray8 frame must have 1 plane",
        ));
    }
    let plane = &image.planes[0];
    let precision: u32 = 8;
    let dc_shift = 1i32 << (precision - 1);
    let nl = opts.num_decomp;
    let cblk_log2 = opts.cblk_log2;

    // Build the canvas: signed integer samples after DC level shift.
    let comp_w = w as usize;
    let comp_h = h as usize;
    let mut canvas: Vec<i32> = Vec::with_capacity(comp_w * comp_h);
    for y in 0..comp_h {
        for x in 0..comp_w {
            canvas.push(plane.data[y * plane.stride + x] as i32 - dc_shift);
        }
    }

    // Apply NL levels of forward 5/3 DWT, level-by-level. Each level
    // operates on the LL quadrant of the previous level (top-left
    // `ceil(w_r / 2) × ceil(h_r / 2)` slice). Mirrors the decoder's
    // pyramid synthesis in reverse — `fdwt_53` writes the deinterleaved
    // quadrant layout (LL top-left, HL top-right, LH bottom-left, HH
    // bottom-right).
    let mut cur_w = comp_w;
    let mut cur_h = comp_h;
    for _level in 0..nl as usize {
        if cur_w < 2 || cur_h < 2 {
            break;
        }
        fdwt_53(&mut canvas, cur_w, cur_h, comp_w);
        cur_w = cur_w.div_ceil(2);
        cur_h = cur_h.div_ceil(2);
    }

    // Build per-subband layouts via the same `build_subbands` helper
    // the decoder uses. Single-tile means tile-component bounds equal
    // the component extent.
    let subbands = build_subbands(0, 0, w, h, nl);

    // Quantisation: 5/3 reversible has eps_b = precision + log2_gain_b.
    // log2_gain depends on band_kind: LL=0, HL/LH=1, HH=2.
    let band_eps = |band_kind: u8| -> u32 {
        match band_kind {
            0 => precision,         // LL
            1 | 2 => precision + 1, // HL, LH
            3 => precision + 2,     // HH
            _ => precision,
        }
    };
    let band_numbps = |band_kind: u8| -> u32 { band_eps(band_kind).saturating_sub(1) };

    // For each subband, extract the rectangle from the canvas in its
    // pyramid-quadrant position and encode every codeblock.
    let cblk_dim = 1u32 << cblk_log2;

    let num_res = (nl as usize) + 1;
    let mut per_res: Vec<ResEnc> = (0..num_res).map(|_| ResEnc::default()).collect();

    for sb in &subbands {
        let bw = sb.x1 - sb.x0;
        let bh = sb.y1 - sb.y0;
        if bw == 0 || bh == 0 {
            per_res[sb.resno as usize].bands.push(BandEnc::default());
            continue;
        }
        // Compute canvas-space offset of this band. For LL_0 (resno=0):
        // top-left (0, 0). For other bands at resolution r: at level
        // L = NL - r (forward DWT level number), the canvas extent
        // shrunk to scale_w = ceil(W / 2^L), scale_h = ceil(H / 2^L).
        // Within that scale rectangle, the band occupies a quadrant.
        let level_from_top = (nl as usize) - sb.resno as usize;
        let mut scale_w = comp_w;
        let mut scale_h = comp_h;
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
                let cw = (bx1 - bx0) as usize;
                let ch = (by1 - by0) as usize;

                let mut samples: Vec<SampleHt> = Vec::with_capacity(cw * ch);
                let mut max_mag: u32 = 0;
                for ly in 0..ch {
                    for lx in 0..cw {
                        let cx_canvas = band_cx0 + (bx0 as usize) + lx;
                        let cy_canvas = band_cy0 + (by0 as usize) + ly;
                        let raw = canvas[cy_canvas * comp_w + cx_canvas];
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
                let dcup = encode_cleanup(cw as u32, ch as u32, &samples)?;
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

    // -- Build tier-2 packet body: LRCP, single layer, single tile,
    // single component, default precincts. One packet per resolution
    // covering all bands at that resolution.
    let mut body: Vec<u8> = Vec::new();
    for res in per_res.iter().take(num_res) {
        emit_packet_htj2k(&mut body, res)?;
    }

    // -- Assemble the full codestream --
    let mut cs = Vec::<u8>::new();
    cs.extend_from_slice(&[0xFF, 0x4F]); // SOC
    write_siz_ht(&mut cs, w, h, precision)?;
    write_cap_ht(&mut cs);
    write_cod_ht(&mut cs, cblk_log2, nl);
    write_qcd_reversible(&mut cs, precision as u8, nl);
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

/// Emit one tier-2 packet for a single resolution.
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
///
/// We mirror the spec's per-node lock state across leaves so already-
/// emitted ancestor bits are skipped on the next leaf walk.
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
                // Emit zero bits while the decoder would still loop
                // (low < threshold AND low < value). Mirror exactly so
                // the decoder reads the same number of bits.
                while lows[k][idx] < threshold && lows[k][idx] < node_value {
                    bw.write_bit(0);
                    lows[k][idx] += 1;
                }
                // Lock with a `1` bit IFF the loop exited because
                // `low == value` (the decoder will then read the bit
                // and set value). When `low == threshold` and the
                // loop also exited there, no extra bit is read by the
                // decoder.
                if lows[k][idx] == node_value && lows[k][idx] < threshold {
                    bw.write_bit(1);
                    locked[k][idx] = true;
                } else if lows[k][idx] == node_value {
                    // Edge case: low == threshold == value. Decoder
                    // reads one more bit only if the previous loop
                    // condition was already false; the loop bound
                    // `low < threshold` is exclusive, so when
                    // low == threshold == value the loop never
                    // entered and no bit is read. Encoder skips too.
                    // Actually `lows == value` here AND lows ==
                    // threshold: we don't emit anything because the
                    // decoder won't read a bit (loop exits at
                    // low < threshold check before checking value).
                    // No-op.
                }
            }
        }
    }
}

fn write_siz_ht(out: &mut Vec<u8>, w: u32, h: u32, precision: u32) -> Result<()> {
    out.extend_from_slice(&[0xFF, 0x51]);
    out.extend_from_slice(&41u16.to_be_bytes());
    out.extend_from_slice(&0x4000u16.to_be_bytes());
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.push((precision - 1) as u8);
    out.push(1);
    out.push(1);
    Ok(())
}

fn write_cap_ht(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0xFF, 0x50]);
    out.extend_from_slice(&8u16.to_be_bytes());
    out.extend_from_slice(&0x0002_0000u32.to_be_bytes());
    out.extend_from_slice(&0x0000u16.to_be_bytes());
}

fn write_cod_ht(out: &mut Vec<u8>, cblk_log2: u8, nl: u8) {
    out.extend_from_slice(&[0xFF, 0x52]);
    out.extend_from_slice(&12u16.to_be_bytes());
    out.extend_from_slice(&[0u8, 0, 0x00, 0x01, 0]);
    let cw = cblk_log2 - 2;
    let ch = cblk_log2 - 2;
    out.extend_from_slice(&[nl, cw, ch, 0x40, 1]);
}

/// QCD: reversible 5/3 with `1 + 3 * NL` bands. eps_b = precision +
/// log2_gain_b: LL=0, HL/LH=1, HH=2.
fn write_qcd_reversible(out: &mut Vec<u8>, precision: u8, nl: u8) {
    let num_bands = 1usize + 3 * nl as usize;
    out.extend_from_slice(&[0xFF, 0x5C]);
    // Lqcd = 3 + num_bands (1 byte Sqcd + 1 byte SPqcd per band).
    out.extend_from_slice(&((3 + num_bands) as u16).to_be_bytes());
    out.push(0);
    // Bands: LL_0, HL_1, LH_1, HH_1, HL_2, LH_2, HH_2, ...
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
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }

    /// Round 1 sparse fixture (still passes the original sparse
    /// contract).
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
        };
        let cs = encode_image_htj2k(&img, &opts).expect("encode");
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, data);
    }
}
