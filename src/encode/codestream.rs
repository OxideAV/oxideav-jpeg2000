//! Top-level encoder: frame → `.j2k` / `.jp2` codestream bytes.
//!
//! Writes the marker chain SOC → SIZ → COD → QCD → SOT → SOD → EOC,
//! with the tile-part body produced by [`super::tile::encode_tile`] or
//! [`super::tile::encode_tile_97`]. When `EncodeOptions::jp2_wrapper`
//! is enabled, the raw J2K codestream is additionally wrapped in the
//! ISOBMFF box structure that ISO/IEC 15444-1 Annex I specifies for
//! `.jp2` files.

use super::tile::{encode_tile, encode_tile_97};
use oxideav_core::{Error, Frame, PixelFormat, Result};

/// Wavelet transform selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformMode {
    /// 5/3 reversible integer (bit-exact lossless).
    Reversible53,
    /// 9/7 irreversible float with scalar quantisation (lossy).
    Irreversible97,
}

/// Encoder knobs.
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
    /// Wavelet transform kind.
    pub transform: TransformMode,
    /// If true, wrap the raw J2K codestream in the ISO/IEC 15444-1
    /// Annex I JP2 box structure (`.jp2`). If false, return the raw
    /// `.j2k` codestream only.
    pub jp2_wrapper: bool,
    /// If true (default when encoding 3-channel input), apply the
    /// RCT / ICT component transform from RGB to YCbCr and signal it
    /// via `MCT = 1` in the COD. Ignored for single-component input.
    pub use_color_transform: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        EncodeOptions {
            num_decomp: 5,
            cblk_w_log2: 6,
            cblk_h_log2: 6,
            guard_bits: 2,
            transform: TransformMode::Reversible53,
            jp2_wrapper: false,
            use_color_transform: true,
        }
    }
}

/// Forward reversible component transform (RCT) per T.800 §G.1.
///
/// Maps RGB → Y/Cb/Cr with integer, invertible arithmetic:
///
/// ```text
///   Y  = floor((R + 2G + B) / 4)
///   Cb = B - G
///   Cr = R - G
/// ```
///
/// Inputs and outputs are `u8` with the standard DC-level-shift
/// applied downstream (caller subtracts 128 for the luma, keeps the
/// chroma centered on 0).
pub(crate) fn forward_rct_u8(
    r: &[u8],
    g: &[u8],
    b: &[u8],
    w: usize,
    h: usize,
) -> (Vec<i32>, Vec<i32>, Vec<i32>) {
    let n = w * h;
    let mut y = Vec::with_capacity(n);
    let mut cb = Vec::with_capacity(n);
    let mut cr = Vec::with_capacity(n);
    for i in 0..n {
        let ri = r[i] as i32;
        let gi = g[i] as i32;
        let bi = b[i] as i32;
        let yv = (ri + 2 * gi + bi) >> 2;
        let cbv = bi - gi;
        let crv = ri - gi;
        y.push(yv);
        cb.push(cbv);
        cr.push(crv);
    }
    let _ = h;
    (y, cb, cr)
}

/// Forward irreversible component transform (ICT) per T.800 §G.2.
///
/// Maps RGB → Y/Cb/Cr with the ITU-R BT.601 JPEG YCbCr matrix. Inputs
/// are `u8` RGB; outputs are `f32` already DC-level-shifted (luma has
/// 128 subtracted so the whole signal is centered on zero, which is
/// what the forward 9/7 expects).
pub(crate) fn forward_ict_u8(
    r: &[u8],
    g: &[u8],
    b: &[u8],
    w: usize,
    h: usize,
) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let n = w * h;
    let mut y = Vec::with_capacity(n);
    let mut cb = Vec::with_capacity(n);
    let mut cr = Vec::with_capacity(n);
    for i in 0..n {
        let rf = r[i] as f32;
        let gf = g[i] as f32;
        let bf = b[i] as f32;
        let yv = 0.299 * rf + 0.587 * gf + 0.114 * bf;
        let cbv = -0.168_736 * rf - 0.331_264 * gf + 0.5 * bf;
        let crv = 0.5 * rf - 0.418_688 * gf - 0.081_312 * bf;
        // Level shift luma to signed-centered; chroma is already ±.
        y.push(yv - 128.0);
        cb.push(cbv);
        cr.push(crv);
    }
    let _ = h;
    (y, cb, cr)
}

/// Encode a single `Frame::Video` as a `.j2k` (or `.jp2`) codestream.
///
/// Supported pixel formats:
/// - `Gray8` → single component, 8-bit, DC-level-shifted.
/// - `Rgb24` → three components, 8-bit, optionally passed through the
///   forward RCT (for 5/3) or ICT (for 9/7) component transform.
pub fn encode_frame(frame: &Frame, opts: &EncodeOptions) -> Result<Vec<u8>> {
    let vf = match frame {
        Frame::Video(v) => v,
        _ => return Err(Error::unsupported("jpeg2000: only video frames supported")),
    };

    let w = vf.width;
    let h = vf.height;
    let num_pixels = w as usize * h as usize;
    let precision = 8u32; // Ssiz ≤ 8 — scope restricted to 8-bit samples.

    // Extract per-channel u8 planes for the supported pixel formats.
    let (channels_u8, num_comps, is_color) = match vf.format {
        PixelFormat::Gray8 => {
            if vf.planes.len() != 1 {
                return Err(Error::invalid("jpeg2000: Gray8 frame must have one plane"));
            }
            let p = &vf.planes[0];
            let mut gray = Vec::with_capacity(num_pixels);
            for y in 0..h as usize {
                for x in 0..w as usize {
                    gray.push(p.data[y * p.stride + x]);
                }
            }
            (vec![gray], 1usize, false)
        }
        PixelFormat::Rgb24 => {
            if vf.planes.len() != 1 {
                return Err(Error::invalid("jpeg2000: Rgb24 frame must have one plane"));
            }
            let p = &vf.planes[0];
            let mut r = Vec::with_capacity(num_pixels);
            let mut g = Vec::with_capacity(num_pixels);
            let mut b = Vec::with_capacity(num_pixels);
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let off = y * p.stride + 3 * x;
                    r.push(p.data[off]);
                    g.push(p.data[off + 1]);
                    b.push(p.data[off + 2]);
                }
            }
            (vec![r, g, b], 3usize, true)
        }
        _ => {
            return Err(Error::unsupported(format!(
                "jpeg2000: encoder: unsupported pixel format {:?}",
                vf.format
            )));
        }
    };

    let apply_mct = is_color && opts.use_color_transform;
    let comp_sizes: Vec<(u32, u32, u32, u32)> = (0..num_comps).map(|_| (0, 0, w, h)).collect();

    // Tile body.
    let tile_bytes = match opts.transform {
        TransformMode::Reversible53 => {
            let planes_i32: Vec<Vec<i32>> = if apply_mct {
                // Forward RCT.
                let (y, cb, cr) = forward_rct_u8(
                    &channels_u8[0],
                    &channels_u8[1],
                    &channels_u8[2],
                    w as usize,
                    h as usize,
                );
                // DC level shift the luma (Y) to signed-centered.
                // Chroma (Cb/Cr) from RCT is already centered on 0.
                let shift = 1i32 << (precision - 1);
                let y: Vec<i32> = y.into_iter().map(|v| v - shift).collect();
                vec![y, cb, cr]
            } else {
                // No MCT: just DC level shift each plane.
                let shift = 1i32 << (precision - 1);
                channels_u8
                    .iter()
                    .map(|p| p.iter().map(|&v| v as i32 - shift).collect::<Vec<i32>>())
                    .collect()
            };
            encode_tile(
                &planes_i32,
                &comp_sizes,
                opts.num_decomp,
                opts.cblk_w_log2,
                opts.cblk_h_log2,
                opts.guard_bits,
                precision,
            )?
        }
        TransformMode::Irreversible97 => {
            let planes_f32: Vec<Vec<f32>> = if apply_mct {
                let (y, cb, cr) = forward_ict_u8(
                    &channels_u8[0],
                    &channels_u8[1],
                    &channels_u8[2],
                    w as usize,
                    h as usize,
                );
                vec![y, cb, cr]
            } else {
                let shift = (1i32 << (precision - 1)) as f32;
                channels_u8
                    .iter()
                    .map(|p| p.iter().map(|&v| v as f32 - shift).collect::<Vec<f32>>())
                    .collect()
            };
            let (stepsizes, band_eps) = build_97_band_params(opts.num_decomp, precision as u8);
            encode_tile_97(
                &planes_f32,
                &comp_sizes,
                opts.num_decomp,
                opts.cblk_w_log2,
                opts.cblk_h_log2,
                opts.guard_bits,
                &stepsizes,
                &band_eps,
            )?
        }
    };

    // Assemble the codestream.
    let mut cs: Vec<u8> = Vec::new();
    // SOC
    cs.extend_from_slice(&[0xFF, 0x4F]);
    // SIZ
    write_siz(&mut cs, w, h, num_comps, precision)?;
    // COD
    write_cod(&mut cs, opts, apply_mct)?;
    // QCD
    match opts.transform {
        TransformMode::Reversible53 => {
            write_qcd_reversible(&mut cs, opts.guard_bits, opts.num_decomp)?;
        }
        TransformMode::Irreversible97 => {
            let (_, band_eps) = build_97_band_params(opts.num_decomp, precision as u8);
            write_qcd_irreversible(&mut cs, opts.guard_bits, opts.num_decomp, &band_eps)?;
        }
    }
    // SOT — fill Psot after body length is known.
    let sot_off = cs.len();
    cs.extend_from_slice(&[0xFF, 0x90]);
    cs.extend_from_slice(&10u16.to_be_bytes()); // Lsot
    cs.extend_from_slice(&0u16.to_be_bytes()); // Isot = 0
    let psot_off = cs.len();
    cs.extend_from_slice(&0u32.to_be_bytes()); // placeholder Psot
    cs.extend_from_slice(&[0, 1]); // TPsot = 0, TNsot = 1
                                   // SOD
    cs.extend_from_slice(&[0xFF, 0x93]);
    cs.extend_from_slice(&tile_bytes.body);
    let tile_part_end = cs.len();
    let psot = (tile_part_end - sot_off) as u32;
    cs[psot_off..psot_off + 4].copy_from_slice(&psot.to_be_bytes());
    // EOC
    cs.extend_from_slice(&[0xFF, 0xD9]);

    if opts.jp2_wrapper {
        Ok(wrap_jp2(&cs, w, h, num_comps, precision, is_color))
    } else {
        Ok(cs)
    }
}

/// Pick per-band `(stepsize, eps)` for the 9/7 path. We use the same
/// scheme the decoder assumes (`Rb = precision`, no log2_gain_b). For
/// lossy compression we set `eps_b = precision` so `stepsize_b = 1` on
/// every band; this matches OpenJPEG's `opj_dwt_encode_stepsize`
/// default for the `USE_DERIVED_STEPSIZE` quality target.
fn build_97_band_params(num_decomp: u8, precision: u8) -> (Vec<f32>, Vec<u8>) {
    let num_bands = 3 * (num_decomp as usize) + 1;
    let mut stepsizes = Vec::with_capacity(num_bands);
    let mut band_eps = Vec::with_capacity(num_bands);
    // Band 0 = LL of resolution 0.
    let eps = precision; // stepsize = 2^(precision - eps) = 1
    let step = 1.0f32;
    stepsizes.push(step);
    band_eps.push(eps);
    for _r in 1..=num_decomp {
        for _ in 0..3 {
            stepsizes.push(step);
            band_eps.push(eps);
        }
    }
    (stepsizes, band_eps)
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

fn write_cod(out: &mut Vec<u8>, opts: &EncodeOptions, apply_mct: bool) -> Result<()> {
    // Lcod = 12 (default precincts, no partitioning).
    out.extend_from_slice(&[0xFF, 0x52]);
    out.extend_from_slice(&12u16.to_be_bytes());
    out.push(0); // Scod — no SOP, no EPH, default precincts
    out.push(0); // SGcod progression order = LRCP
    out.extend_from_slice(&1u16.to_be_bytes()); // num layers
    out.push(if apply_mct { 1 } else { 0 }); // MCT flag
    out.push(opts.num_decomp);
    out.push(opts.cblk_w_log2 - 2);
    out.push(opts.cblk_h_log2 - 2);
    out.push(0); // Cblksty
    let transform_byte = match opts.transform {
        TransformMode::Reversible53 => 1u8,
        TransformMode::Irreversible97 => 0u8,
    };
    out.push(transform_byte);
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
    // per-resolution HL, LH, HH. Hard-coded 8-bit precision (all
    // baseline images this encoder emits are 8-bit).
    let prec = 8u8;
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

/// Emit QCD for the 9/7 irreversible transform in "expounded" form
/// (qntsty = 2): one `(eps, mu)` pair per sub-band, packed MSB-first
/// with `eps` in the top 5 bits and `mu` in the bottom 11. We always
/// emit `mu = 0` (stepsize mantissa = 1) to keep the stepsize exactly
/// `2^(precision - eps)`.
fn write_qcd_irreversible(
    out: &mut Vec<u8>,
    guard_bits: u8,
    num_decomp: u8,
    band_eps: &[u8],
) -> Result<()> {
    let num_bands = 3 * (num_decomp as usize) + 1;
    if band_eps.len() != num_bands {
        return Err(Error::invalid("jpeg2000: QCD eps length mismatch"));
    }
    // Lqcd = length(2) + Sqcd(1) + SPqcd(2 * num_bands)
    let lqcd = 3 + 2 * num_bands;
    if lqcd > u16::MAX as usize {
        return Err(Error::invalid("jpeg2000: QCD segment too long"));
    }
    out.extend_from_slice(&[0xFF, 0x5C]);
    out.extend_from_slice(&(lqcd as u16).to_be_bytes());
    // Sqcd: qntsty = 2 (expounded), guard bits in upper 3 bits.
    out.push(((guard_bits << 5) & 0xE0) | 0x02);
    for &eps in band_eps {
        // mu = 0 → stepsize = 2^(Rb - eps) with Rb = precision.
        let v: u16 = (eps as u16 & 0x1F) << 11;
        out.extend_from_slice(&v.to_be_bytes());
    }
    Ok(())
}

// --- JP2 ISOBMFF wrapper (ISO/IEC 15444-1 Annex I) ---

/// Assemble an ISOBMFF-style box: 4 bytes big-endian length + 4 bytes
/// type code + payload. Box length includes the 8-byte header.
fn box_push(out: &mut Vec<u8>, ty: &[u8; 4], payload: &[u8]) {
    let size = (payload.len() + 8) as u32;
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(ty);
    out.extend_from_slice(payload);
}

/// Wrap a raw J2K codestream with the minimal JP2 box structure
/// required by Annex I: `jP  ` signature, `ftyp`, `jp2h` (containing
/// `ihdr` + `colr`), then `jp2c` with the codestream payload.
pub(crate) fn wrap_jp2(
    cs: &[u8],
    w: u32,
    h: u32,
    num_comps: usize,
    precision: u32,
    is_color: bool,
) -> Vec<u8> {
    let mut out = Vec::new();

    // Signature box: "jP  " with payload 0x0D 0x0A 0x87 0x0A.
    box_push(&mut out, b"jP  ", &[0x0D, 0x0A, 0x87, 0x0A]);

    // File-type box: major_brand=jp2, minor_version=0, compat=[jp2].
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"jp2 ");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"jp2 ");
    box_push(&mut out, b"ftyp", &ftyp);

    // jp2 header super-box: ihdr + colr.
    let mut jp2h = Vec::new();
    // ihdr: height(4) width(4) nc(2) bpc(1) C(1) UnkC(1) IPR(1).
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&(num_comps as u16).to_be_bytes());
    ihdr.push((precision - 1) as u8 & 0x7F); // unsigned samples
    ihdr.push(7); // Compression type: 7 = JPEG 2000
    ihdr.push(0); // UnkC
    ihdr.push(0); // IPR
    box_push(&mut jp2h, b"ihdr", &ihdr);
    // colr: method=1 (enumerated colourspace), precedence=0, approx=0,
    // EnumCS = 16 (sRGB) for RGB or 17 (greyscale) for gray.
    let mut colr = Vec::new();
    colr.push(1); // Method
    colr.push(0); // Precedence
    colr.push(0); // Approx
    let enum_cs: u32 = if is_color { 16 } else { 17 };
    colr.extend_from_slice(&enum_cs.to_be_bytes());
    box_push(&mut jp2h, b"colr", &colr);
    box_push(&mut out, b"jp2h", &jp2h);

    // Contiguous codestream box: jp2c + raw j2k.
    box_push(&mut out, b"jp2c", cs);
    out
}

/// Extract the inner J2K codestream from a JP2 ISOBMFF container. Used
/// by decoder tests; the production decoder path continues to expect
/// raw `.j2k` input for now.
pub fn extract_jp2_codestream(buf: &[u8]) -> Result<Vec<u8>> {
    let mut i = 0usize;
    while i + 8 <= buf.len() {
        let size = u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
        let ty = [buf[i + 4], buf[i + 5], buf[i + 6], buf[i + 7]];
        if size < 8 {
            return Err(Error::invalid("jpeg2000: jp2 box size < 8"));
        }
        let end = i
            .checked_add(size as usize)
            .ok_or_else(|| Error::invalid("jpeg2000: jp2 box size overflow"))?;
        if end > buf.len() {
            return Err(Error::invalid("jpeg2000: jp2 box past end"));
        }
        if &ty == b"jp2c" {
            return Ok(buf[i + 8..end].to_vec());
        }
        i = end;
    }
    Err(Error::invalid("jpeg2000: no jp2c box found"))
}
