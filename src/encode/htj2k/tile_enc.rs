//! HTJ2K codestream + tile-body encoder (round 1).
//!
//! Wraps [`super::cleanup_enc::encode_cleanup`] in the marker chain
//! that ISO/IEC 15444-15 requires: SOC + SIZ (with Rsiz bit 14 set
//! per §A.2) + CAP (Pcap15 + Ccap15) + COD (with SPcod cblk_style
//! bit 6 set per Table A.3) + QCD + SOT + SOD + EOC.
//!
//! Round-1 restrictions:
//!
//! * Single-tile, single-component (luma `Gray8`), 8-bit unsigned.
//! * 32×32 code-blocks (cblk_log2 = 5), single 32×32 image so the
//!   whole frame is one code-block.
//! * NL = 0 (identity DWT) — no forward 5/3 needed for the cleanup
//!   round-trip; the input samples (after DC level shift) are coded
//!   directly. Multi-decomp + forward DWT are deferred to round 2.
//! * Single quality layer, LRCP progression, default precincts.
//! * Reversible 5/3 transform byte signalled in COD.

use super::cleanup_enc::{encode_cleanup, SampleHt};
use crate::error::{Jpeg2000Error as Error, Result};
use crate::image::{Jpeg2000Image, Jpeg2000PixelFormat as PixelFormat};

/// Knobs for the round-1 HTJ2K encoder.
#[derive(Debug, Clone)]
pub struct EncodeOptionsHt {
    /// Code-block width log2. Default 5 (= 32). Round-1 restricts to
    /// the same value for both dimensions and to a single code-block
    /// covering the entire image.
    pub cblk_log2: u8,
    /// Number of decomposition levels (NL). Round 1 only supports 0
    /// (identity DWT); round 2 will wire the forward 5/3 lifting.
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

/// Encode a [`Jpeg2000Image`] as a 5/3 reversible HTJ2K codestream
/// (round 1: 32×32 single-component identity-DWT).
pub fn encode_image_htj2k(image: &Jpeg2000Image, opts: &EncodeOptionsHt) -> Result<Vec<u8>> {
    let w = image.width;
    let h = image.height;
    if w == 0 || h == 0 {
        return Err(Error::invalid("HTJ2K encode: zero-dimension image"));
    }
    if image.pixel_format != PixelFormat::Gray8 {
        return Err(Error::unsupported(
            "HTJ2K encode (round 1): only Gray8 supported",
        ));
    }
    if opts.num_decomp != 0 {
        return Err(Error::unsupported(
            "HTJ2K encode (round 1): NL must be 0; multi-level forward DWT in round 2",
        ));
    }
    if image.planes.len() != 1 {
        return Err(Error::invalid(
            "HTJ2K encode: Gray8 frame must have 1 plane",
        ));
    }
    let cblk_dim: u32 = 1u32 << opts.cblk_log2;
    if w != cblk_dim || h != cblk_dim {
        return Err(Error::unsupported(format!(
            "HTJ2K encode (round 1): image must be exactly {}x{} (cblk_log2={})",
            cblk_dim, cblk_dim, opts.cblk_log2
        )));
    }

    let plane = &image.planes[0];
    let precision: u32 = 8;
    let dc_shift = 1i32 << (precision - 1);
    let n_pixels = (w as usize) * (h as usize);
    // Pull samples row-major and DC-level-shift to signed-centered.
    let mut samples: Vec<SampleHt> = Vec::with_capacity(n_pixels);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let raw = plane.data[y * plane.stride + x] as i32 - dc_shift;
            let mag = raw.unsigned_abs();
            let sign: u8 = if raw < 0 { 1 } else { 0 };
            samples.push(SampleHt { mag, sign });
        }
    }

    // Pick `missing_msb` so the dequant left-shift `pblk = band_numbps
    // − missing_msb` evaluates to 0. This matches the decoder's
    // expectation that cleanup-pass magnitudes already sit at the
    // M_b grid for lossless coding (T.800 Eq E-1 with N_b = S_blk + 1
    // and S_blk = missing_msb − 1). band_numbps = guard_bits(=0) +
    // eps(=precision=8) − 1 = 7. So `missing_msb = 7`.
    //
    // The zero-bitplane tag-tree leaf carries `missing_msb − 1` (the
    // decoder's threshold-loop counts from 0 and adds 1 on success).
    let band_numbps: u32 = precision - 1;
    let missing_msb: u32 = band_numbps;
    let zb_leaf: u32 = missing_msb.saturating_sub(1);

    // -- HT cleanup segment for the single 32×32 codeblock --
    let dcup = encode_cleanup(w, h, &samples)?;

    // -- Tier-2 packet header for one packet, one (component, res, prec)
    // with one included codeblock. We hand-build the bit-stream the
    // existing decoder's `parse_packet` walker expects.
    //
    // Packet header bits (MSB-first per §B.10):
    //   1. packet-non-empty flag                     : 1
    //   2. inclusion tag-tree (1×1) at threshold 1   : 1 (terminator)
    //   3. zero-bitplane tag-tree (1×1) at threshold 1: 1 (terminator)
    //      → missing_msb = 0
    //   4. num_passes = 1                            : 0
    //   5. Lblock growth (none)                      : 0
    //   6. length field width = lblock(=3) + ilog2(1)=0 → 3 bits.
    //      Length must equal Lcup. Lcup ranges up to 32+ kB so 3 bits
    //      is way too narrow; we grow Lblock by `n` bits via leading 1
    //      bits in step 5 to fit Lcup.
    //
    // Adaptive Lblock growth: starts at 3. Total length-field width is
    // `lblock + 0` bits. We must have `lblock` such that
    // `Lcup < 2^lblock`. So increment until `2^lblock > Lcup`.

    let lcup = dcup.len() as u32;
    let mut lblock = 3u32;
    while (1u32 << lblock) <= lcup {
        lblock += 1;
    }
    let lblock_growth_bits = lblock - 3;

    // Build the packet header bit-stream MSB-first.
    //
    // Zero-bitplane tag-tree (1×1 leaf, leaf value = missing_msb): we
    // emit `missing_msb` zero bits (advancing `low` past the leaf
    // value's predecessors) followed by a single `1` terminator. The
    // decoder's loop walks `low` from 0 upward, importing bits until
    // either `low == leaf_value` (signal: `1` terminator) or
    // `low == threshold` (no terminator needed). The threshold-sweep
    // loop in the cleanup walker calls `decode` for thresholds
    // 1..=missing_msb+1; only the LAST call (threshold = missing_msb +
    // 1) imports the terminator after `missing_msb` zero advances.
    let mut header = Vec::<u8>::new();
    let mut bw = BioWriterMsbFirst::new();
    bw.write_bit(1); // packet non-empty
    bw.write_bit(1); // inclusion tag-tree terminator: leaf < threshold(1)
    for _ in 0..zb_leaf {
        bw.write_bit(0); // advance `low` toward leaf_value = zb_leaf
    }
    bw.write_bit(1); // zero-bitplane terminator: low == leaf_value
    bw.write_bit(0); // num_passes = 1
    for _ in 0..lblock_growth_bits {
        bw.write_bit(1); // each '1' bumps lblock by 1
    }
    bw.write_bit(0); // terminator for lblock-growth loop
                     // Length field: `lblock` bits MSB-first carrying Lcup.
    for i in (0..lblock).rev() {
        bw.write_bit(((lcup >> i) & 1) as u8);
    }
    bw.flush_aligned(&mut header);

    // -- Assemble the full codestream --
    let mut cs = Vec::<u8>::new();
    cs.extend_from_slice(&[0xFF, 0x4F]); // SOC
    write_siz_ht(&mut cs, w, h, precision)?;
    write_cap_ht(&mut cs);
    write_cod_ht(&mut cs, opts.cblk_log2);
    write_qcd_reversible_nl0(&mut cs, precision as u8);
    // SOT / SOD / EOC
    let sot_off = cs.len();
    cs.extend_from_slice(&[0xFF, 0x90]);
    cs.extend_from_slice(&10u16.to_be_bytes()); // Lsot = 10
    cs.extend_from_slice(&0u16.to_be_bytes()); // Isot
    let psot_pos = cs.len();
    cs.extend_from_slice(&0u32.to_be_bytes()); // Psot — patched below
    cs.extend_from_slice(&[0, 1]); // TPsot=0, TNsot=1
    cs.extend_from_slice(&[0xFF, 0x93]); // SOD
    cs.extend_from_slice(&header);
    cs.extend_from_slice(&dcup);
    let tile_part_end = cs.len();
    let psot = (tile_part_end - sot_off) as u32;
    cs[psot_pos..psot_pos + 4].copy_from_slice(&psot.to_be_bytes());
    cs.extend_from_slice(&[0xFF, 0xD9]); // EOC
    Ok(cs)
}

fn write_siz_ht(out: &mut Vec<u8>, w: u32, h: u32, precision: u32) -> Result<()> {
    out.extend_from_slice(&[0xFF, 0x51]);
    out.extend_from_slice(&41u16.to_be_bytes()); // Lsiz fixed for 1 component
                                                 // Per ISO/IEC 15444-15 §A.2: Rsiz bit 14 (mask 0x4000) must be set
                                                 // to indicate the codestream uses HTJ2K extensions.
    out.extend_from_slice(&0x4000u16.to_be_bytes());
    out.extend_from_slice(&w.to_be_bytes()); // Xsiz
    out.extend_from_slice(&h.to_be_bytes()); // Ysiz
    out.extend_from_slice(&0u32.to_be_bytes()); // XOsiz
    out.extend_from_slice(&0u32.to_be_bytes()); // YOsiz
    out.extend_from_slice(&w.to_be_bytes()); // XTsiz = full image (single tile)
    out.extend_from_slice(&h.to_be_bytes()); // YTsiz
    out.extend_from_slice(&0u32.to_be_bytes()); // XTOsiz
    out.extend_from_slice(&0u32.to_be_bytes()); // YTOsiz
    out.extend_from_slice(&1u16.to_be_bytes()); // Csiz = 1 component
    out.push((precision - 1) as u8); // Ssiz: 8-bit unsigned ⇒ 7
    out.push(1); // XRsiz
    out.push(1); // YRsiz
    Ok(())
}

fn write_cap_ht(out: &mut Vec<u8>) {
    // CAP marker (§A.5.2 + ISO/IEC 15444-15 §A.3.1):
    //   Lcap = 6 + 2*N where N = popcount(Pcap).
    //   We set only Pcap15 (mask 0x0002_0000) so N = 1 ⇒ Lcap = 8.
    //   Ccap15 = 0x0000 — HTONLY profile, single HT set, no RGN,
    //   homogeneous, reversible, magnitude bound = 8.
    out.extend_from_slice(&[0xFF, 0x50]);
    out.extend_from_slice(&8u16.to_be_bytes()); // Lcap
    out.extend_from_slice(&0x0002_0000u32.to_be_bytes()); // Pcap with Pcap15 set
    out.extend_from_slice(&0x0000u16.to_be_bytes()); // Ccap15
}

fn write_cod_ht(out: &mut Vec<u8>, cblk_log2: u8) {
    // COD with HT cleanup-only flag in SPcod cblk_style (Table A.3):
    //   bit 6 = 1 → "all blocks HT"; bit 7 = 0 → cleanup pass only.
    out.extend_from_slice(&[0xFF, 0x52]);
    out.extend_from_slice(&12u16.to_be_bytes()); // Lcod = 12
                                                 // SGcod (5 bytes): Scod=0 (no SOP/EPH, default precincts),
                                                 //                  prog=0 (LRCP), layers=1 (BE u16), MCT=0.
    out.extend_from_slice(&[0u8, 0, 0x00, 0x01, 0]);
    // SPcod (5 bytes): NL, cblk_w-2, cblk_h-2, cblk_style, transform.
    let cw = cblk_log2 - 2;
    let ch = cblk_log2 - 2;
    out.extend_from_slice(&[0, cw, ch, 0x40, 1]);
    // 0x40 = bit 6 set ("HT codeblocks"); transform = 1 (5/3 reversible).
}

fn write_qcd_reversible_nl0(out: &mut Vec<u8>, precision: u8) {
    out.extend_from_slice(&[0xFF, 0x5C]);
    out.extend_from_slice(&4u16.to_be_bytes()); // Lqcd = 4
    out.push(0); // Sqcd: qntsty=0 (reversible), guard_bits=0
                 // Single LL band exponent = precision (matches band's M_b).
    out.push(precision << 3);
}

/// Tiny MSB-first bit writer mirroring the decoder's `Bio` reader.
/// Buffers bytes, with the FF-stuffing rule (any byte that reaches
/// `0xFF` causes the next byte to carry only 7 payload bits).
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
        // MSB-first inside the cap-wide byte. The 0xFF stuffing rule
        // forces bit-7 to 0 (handled by the cap=7 case naturally —
        // we start filling at position cap-1 = 6).
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

    fn build_gray32_solid(value: u8) -> Jpeg2000Image {
        Jpeg2000Image {
            width: 32,
            height: 32,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: 32,
                data: vec![value; 32 * 32],
            }],
            pts: None,
        }
    }

    /// Encode a 32×32 solid 0x80 image; verify the codestream is
    /// detected as HTJ2K and decodes back to the original bytes.
    #[test]
    fn roundtrip_solid_dc_32x32() {
        let img = build_gray32_solid(0x80);
        let cs = encode_image_htj2k(&img, &EncodeOptionsHt::default()).expect("encode");
        let p = probe(&cs).expect("probe");
        assert_eq!(p.flavour, J2kFlavour::HighThroughput);
        assert_eq!(p.width, 32);
        assert_eq!(p.height, 32);
        let decoded = decode_jpeg2000(&cs).expect("decode");
        assert_eq!(decoded.planes[0].data, img.planes[0].data);
    }

    /// Encode a 32×32 image whose pixels deviate from the DC midpoint
    /// by ±1 in a sparse pattern — exercises the cleanup encoder's
    /// non-zero ρ path while keeping at most one significant sample
    /// per quad.
    #[test]
    fn roundtrip_sparse_one_per_quad_32x32() {
        let mut data = vec![0x80u8; 32 * 32];
        // Place a +1 sample at (0, 0) and a -1 sample at (4, 4).
        // After DC shift, those become +1 (sign 0) and -1 (sign 1).
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
}
