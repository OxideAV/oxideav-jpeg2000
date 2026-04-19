//! Top-level encoder: frame → `.j2k` codestream bytes.
//!
//! Writes the marker chain SOC → SIZ → COD → QCD → SOT → SOD → EOC,
//! with the tile-part body produced by [`super::tile::encode_tile`].

use super::tile::encode_tile;
use oxideav_core::{Error, Frame, PixelFormat, Result};

/// Encoder knobs for the 5/3 lossless baseline.
#[derive(Debug, Clone)]
pub struct EncodeOptions {
    /// Number of DWT decomposition levels. Default 5 (six resolutions).
    pub num_decomp: u8,
    /// `log2` of the code-block width. Default 6 (= 64 px).
    pub cblk_w_log2: u8,
    /// `log2` of the code-block height. Default 6.
    pub cblk_h_log2: u8,
    /// QCD guard bits. Default 2.
    pub guard_bits: u8,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        EncodeOptions {
            num_decomp: 5,
            cblk_w_log2: 6,
            cblk_h_log2: 6,
            guard_bits: 2,
        }
    }
}

/// Encode a single `Frame::Video` as a `.j2k` codestream.
///
/// Only 5/3 lossless encode is supported. Unsupported formats yield
/// `Error::Unsupported`.
pub fn encode_frame(frame: &Frame, opts: &EncodeOptions) -> Result<Vec<u8>> {
    let vf = match frame {
        Frame::Video(v) => v,
        _ => return Err(Error::unsupported("jpeg2000: only video frames supported")),
    };

    let (num_comps, precision, planes_i32) = match vf.format {
        PixelFormat::Gray8 => {
            if vf.planes.len() != 1 {
                return Err(Error::invalid("jpeg2000: Gray8 frame must have one plane"));
            }
            let p = &vf.planes[0];
            let mut planes = Vec::with_capacity(1);
            let mut plane_i32 = Vec::with_capacity(vf.width as usize * vf.height as usize);
            let shift = 128i32;
            for y in 0..vf.height as usize {
                for x in 0..vf.width as usize {
                    plane_i32.push(p.data[y * p.stride + x] as i32 - shift);
                }
            }
            planes.push(plane_i32);
            (1usize, 8u32, planes)
        }
        _ => {
            return Err(Error::unsupported(format!(
                "jpeg2000: encoder: unsupported pixel format {:?}",
                vf.format
            )));
        }
    };

    let comp_sizes: Vec<(u32, u32, u32, u32)> = (0..num_comps)
        .map(|_| (0, 0, vf.width, vf.height))
        .collect();

    let tile_bytes = encode_tile(
        &planes_i32,
        &comp_sizes,
        opts.num_decomp,
        opts.cblk_w_log2,
        opts.cblk_h_log2,
        opts.guard_bits,
        precision,
    )?;

    // Assemble the codestream.
    let mut out: Vec<u8> = Vec::new();
    // SOC
    out.extend_from_slice(&[0xFF, 0x4F]);
    // SIZ
    write_siz(&mut out, vf.width, vf.height, num_comps, precision)?;
    // COD
    write_cod(&mut out, opts)?;
    // QCD
    write_qcd_reversible(&mut out, opts.guard_bits, opts.num_decomp)?;
    // SOT — fill Psot after body length is known.
    let sot_off = out.len();
    out.extend_from_slice(&[0xFF, 0x90]);
    out.extend_from_slice(&10u16.to_be_bytes()); // Lsot
    out.extend_from_slice(&0u16.to_be_bytes()); // Isot = 0
    let psot_off = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // placeholder Psot
    out.extend_from_slice(&[0, 1]); // TPsot = 0, TNsot = 1
                                    // SOD
    out.extend_from_slice(&[0xFF, 0x93]);
    let sod_off = out.len();
    out.extend_from_slice(&tile_bytes.body);
    // EOC
    let tile_part_end = out.len();
    // Psot counts from the SOT marker to the last byte of compressed
    // data (inclusive). Body = SOT(2) + Lsot(2) + SOT payload(8) +
    // SOD(2) + tile body bytes = 14 + tile body.
    let psot = (tile_part_end - sot_off) as u32;
    out[psot_off..psot_off + 4].copy_from_slice(&psot.to_be_bytes());
    out.extend_from_slice(&[0xFF, 0xD9]);
    let _ = sod_off;
    Ok(out)
}

fn write_siz(out: &mut Vec<u8>, w: u32, h: u32, num_comps: usize, precision: u32) -> Result<()> {
    if !(1..=16383).contains(&num_comps) {
        return Err(Error::invalid(
            "jpeg2000: SIZ: number of components out of range",
        ));
    }
    let lsiz = 38 + 3 * num_comps;
    if lsiz > u16::MAX as usize {
        return Err(Error::invalid("jpeg2000: SIZ segment too long"));
    }
    out.extend_from_slice(&[0xFF, 0x51]);
    out.extend_from_slice(&(lsiz as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // Rsiz
    out.extend_from_slice(&w.to_be_bytes()); // Xsiz
    out.extend_from_slice(&h.to_be_bytes()); // Ysiz
    out.extend_from_slice(&0u32.to_be_bytes()); // XOsiz
    out.extend_from_slice(&0u32.to_be_bytes()); // YOsiz
    out.extend_from_slice(&w.to_be_bytes()); // XTsiz
    out.extend_from_slice(&h.to_be_bytes()); // YTsiz
    out.extend_from_slice(&0u32.to_be_bytes()); // XTOsiz
    out.extend_from_slice(&0u32.to_be_bytes()); // YTOsiz
    out.extend_from_slice(&(num_comps as u16).to_be_bytes());
    for _ in 0..num_comps {
        let ssiz = (precision - 1) as u8; // unsigned
        out.push(ssiz);
        out.push(1); // XRsiz
        out.push(1); // YRsiz
    }
    Ok(())
}

fn write_cod(out: &mut Vec<u8>, opts: &EncodeOptions) -> Result<()> {
    // Lcod = 12 (default precincts, no partitioning).
    out.extend_from_slice(&[0xFF, 0x52]);
    out.extend_from_slice(&12u16.to_be_bytes());
    out.push(0); // Scod — no SOP, no EPH, default precincts
    out.push(0); // SGcod progression order = LRCP
    out.extend_from_slice(&1u16.to_be_bytes()); // num layers
    out.push(0); // MCT = 0
    out.push(opts.num_decomp);
    out.push(opts.cblk_w_log2 - 2);
    out.push(opts.cblk_h_log2 - 2);
    out.push(0); // Cblksty
    out.push(1); // transform = 5/3 reversible
    Ok(())
}

fn write_qcd_reversible(out: &mut Vec<u8>, guard_bits: u8, num_decomp: u8) -> Result<()> {
    let num_bands = 3 * (num_decomp as usize) + 1;
    // Lqcd = length field (2) + Sqcd (1) + SPqcd (num_bands)
    let lqcd = 3 + num_bands;
    if lqcd > u16::MAX as usize {
        return Err(Error::invalid("jpeg2000: QCD segment too long"));
    }
    out.extend_from_slice(&[0xFF, 0x5C]);
    out.extend_from_slice(&(lqcd as u16).to_be_bytes());
    // Sqcd: qntsty=0 (reversible no-quantization), guard bits in upper
    // 3 bits.
    out.push((guard_bits << 5) & 0xE0);
    // SPqcd per band: exponent `eps` stored in bits 3..=7 (5 bits).
    // For reversible transform the encoder picks
    //   eps_b = component_precision + log2_gain_b.
    //
    // We write one epsilon per band in the canonical order LL, then
    // per-resolution HL, LH, HH.
    //
    // Hard-coded 8-bit precision (all baseline images this encoder
    // emits are gray 8-bit).
    let prec = 8u8;
    // LL
    let ll_eps = prec;
    out.push((ll_eps << 3) & 0xF8);
    for _r in 1..=num_decomp {
        let hl_eps = prec + 1;
        let lh_eps = prec + 1;
        let hh_eps = prec + 2;
        out.push((hl_eps << 3) & 0xF8);
        out.push((lh_eps << 3) & 0xF8);
        out.push((hh_eps << 3) & 0xF8);
    }
    Ok(())
}
