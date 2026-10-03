//! The standalone image API (`IMAGE_CRATE_API`): [`probe`], [`info`],
//! [`decode`] / [`decode_with`] / [`decode_rgb8`] / [`decode_rgba8`] /
//! [`decode_from`], [`encode`] / [`encode_rgb8`] / [`encode_rgba8`] /
//! [`encode_to`].
//!
//! Every function here is a thin composition of the crate's depth
//! surface — [`crate::parse_j2k_header`] / [`crate::parse_codestream`]
//! for the header walk, [`crate::jp2::parse_jp2`] for the Annex I box
//! wrapper, the tile driver behind [`crate::decode_j2k`] for samples,
//! `jp2::apply_channel_mapping` for the JP2 palette / channel
//! semantics, and [`crate::encode::encode_j2k`] /
//! [`crate::jp2::write_jp2`] on the way out — plus the component-set →
//! layout derivation documented on [`crate::image`]. The framework
//! `Decoder` / `Encoder` in [`crate::registry`] call these same
//! functions.

use std::io::{Read, Write};

use crate::encode::{encode_j2k, encode_j2k_u16, Container, EncodeOptions};
use crate::error::{Error, Jpeg2000Error};
use crate::image::{
    ColorInfo, ColorRange, ImageInfo, Jpeg2000Image, Metadata, Palette, PixelFormat, Plane,
    RgbImage, RgbaImage,
};
use crate::jp2::{
    self, ChannelDef, CmapEntry, CmapMapping, Colr, ColrMethod, EnumCs, Jp2Container, Jp2Header,
    Jp2WriteOptions, Pclr, PclrColumn,
};
use crate::options::DecodeOptions;
use crate::{DecodedComponent, DecodedImage, J2kHeader, Siz, MARKER_SIZ, MARKER_SOC};

// ---------------------------------------------------------------------------
// probe
// ---------------------------------------------------------------------------

/// `true` when `bytes` starts like a JPEG 2000 picture: the 12-byte JP2
/// Signature box (T.800 §I.5.1 — JP2 and JPH files) or the `SOC` +
/// `SIZ` marker pair that opens every Annex A codestream (`.j2k` /
/// `.j2c`). Total, allocation-free, `false` on short input.
pub fn probe(bytes: &[u8]) -> bool {
    is_jp2_file(bytes)
        || (bytes.len() >= 4
            && bytes[0..2] == MARKER_SOC.to_be_bytes()
            && bytes[2..4] == MARKER_SIZ.to_be_bytes())
}

/// `true` iff `bytes` starts with the fixed 12-byte JP2 Signature box
/// (T.800 §I.5.1) — a JP2 / JPH **file** rather than a bare codestream.
pub(crate) fn is_jp2_file(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && bytes[0..4] == [0x00, 0x00, 0x00, 0x0C]
        && bytes[4..8] == jp2::BOX_TYPE_JP2_SIGNATURE.to_be_bytes()
        && bytes[8..12] == jp2::JP2_SIGNATURE_MAGIC
}

// ---------------------------------------------------------------------------
// Header walk shared by `info` and `decode_with`
// ---------------------------------------------------------------------------

/// What the header walk learns before any sample is touched.
struct Parsed<'a> {
    /// The Annex A codestream (the whole input, or the `jp2c` payload).
    codestream: &'a [u8],
    header: J2kHeader,
    container: Option<Jp2Container>,
}

fn parse(bytes: &[u8], strict: bool) -> Result<Parsed<'_>, Error> {
    if is_jp2_file(bytes) {
        let container = jp2::parse_jp2(bytes)?;
        let end = container
            .codestream_offset
            .checked_add(container.codestream_len)
            .ok_or(Jpeg2000Error::UnexpectedEof)?;
        let codestream = bytes
            .get(container.codestream_offset..end)
            .ok_or(Jpeg2000Error::UnexpectedEof)?;
        let header = crate::parse_j2k_header(codestream)?;
        if strict {
            // T.800 §I.5.3.1: the Image Header box repeats the
            // codestream's geometry and component count.
            let ihdr = &container.header.ihdr;
            let siz = &header.siz;
            let (w, h) = (
                siz.x_size.saturating_sub(siz.x_offset),
                siz.y_size.saturating_sub(siz.y_offset),
            );
            if ihdr.width != w
                || ihdr.height != h
                || usize::from(ihdr.component_count) != siz.components.len()
            {
                return Err(Jpeg2000Error::invalid(format!(
                    "ihdr {}x{}x{} disagrees with SIZ {w}x{h}x{}",
                    ihdr.width,
                    ihdr.height,
                    ihdr.component_count,
                    siz.components.len()
                )));
            }
        }
        Ok(Parsed {
            codestream,
            header,
            container: Some(container),
        })
    } else {
        let header = crate::parse_j2k_header(bytes)?;
        Ok(Parsed {
            codestream: bytes,
            header,
            container: None,
        })
    }
}

/// One image channel as the header predicts it: precision, signedness
/// and reference-grid separation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Chan {
    precision: u8,
    signed: bool,
    hsep: u8,
    vsep: u8,
}

impl Chan {
    fn of(c: &DecodedComponent) -> Self {
        Self {
            precision: c.precision_bits,
            signed: c.is_signed,
            hsep: c.h_separation,
            vsep: c.v_separation,
        }
    }
}

/// The channel list the JP2 Header produces from the `SIZ` components
/// (the header-only mirror of [`jp2::apply_channel_mapping`]).
fn predicted_channels(siz: &Siz, jp2h: Option<&Jp2Header>) -> Result<Vec<Chan>, Error> {
    let base: Vec<Chan> = siz
        .components
        .iter()
        .map(|c| Chan {
            precision: c.precision_bits,
            signed: c.is_signed,
            hsep: c.h_separation,
            vsep: c.v_separation,
        })
        .collect();
    let Some(h) = jp2h else {
        return Ok(base);
    };
    let mut chans = match &h.cmap {
        None => base,
        Some(entries) => {
            let mut out = Vec::with_capacity(entries.len());
            for e in entries {
                let comp = base
                    .get(usize::from(e.component))
                    .ok_or(Jpeg2000Error::InvalidMarkerLength)?;
                match e.mapping {
                    CmapMapping::Direct => out.push(*comp),
                    CmapMapping::Palette { column } => {
                        let pclr = h.pclr.as_ref().ok_or(Jpeg2000Error::InvalidMarkerLength)?;
                        let col = pclr
                            .columns
                            .get(usize::from(column))
                            .ok_or(Jpeg2000Error::InvalidMarkerLength)?;
                        if col.bit_depth > 32 {
                            return Err(Jpeg2000Error::NotImplemented);
                        }
                        out.push(Chan {
                            precision: col.bit_depth,
                            signed: col.signed,
                            hsep: comp.hsep,
                            vsep: comp.vsep,
                        });
                    }
                }
            }
            out
        }
    };
    if let Some(defs) = &h.cdef {
        let order = jp2::cdef_presentation_order(defs, chans.len());
        chans = order.iter().map(|&i| chans[i]).collect();
    }
    Ok(chans)
}

/// The `Pal8` plan: `Some(palette)` when the JP2 palette can ride the
/// contract's 8-bit RGBA table — one unsigned ≤ 8-bit index component,
/// a `pclr` of at most 256 entries in one, three or four unsigned
/// ≤ 8-bit columns, a `cmap` applying columns `0..n` in order to that
/// component, and no `cdef` reordering. Anything else expands to
/// channels (`None`).
fn palette_plan(siz: &Siz, jp2h: Option<&Jp2Header>) -> Option<Palette> {
    let h = jp2h?;
    let pclr = h.pclr.as_ref()?;
    let cmap = h.cmap.as_ref()?;
    if siz.components.len() != 1 {
        return None;
    }
    let idx = &siz.components[0];
    if idx.is_signed || idx.precision_bits > 8 {
        return None;
    }
    let ncol = pclr.columns.len();
    if !matches!(ncol, 1 | 3 | 4) || pclr.entries() == 0 || pclr.entries() > 256 {
        return None;
    }
    if pclr
        .columns
        .iter()
        .any(|c| c.signed || c.bit_depth == 0 || c.bit_depth > 8)
    {
        return None;
    }
    if cmap.len() != ncol
        || cmap
            .iter()
            .enumerate()
            .any(|(i, e)| e.component != 0 || e.mapping != CmapMapping::Palette { column: i as u8 })
    {
        return None;
    }
    if let Some(defs) = &h.cdef {
        let order = jp2::cdef_presentation_order(defs, ncol);
        if order.iter().enumerate().any(|(k, &i)| k != i) {
            return None;
        }
    }
    let entries = (0..pclr.entries())
        .map(|j| {
            let v = |c: usize| {
                let col = &pclr.columns[c];
                Jpeg2000Image::to_8bit(col.values[j].max(0) as u32, col.bit_depth)
            };
            match ncol {
                1 => {
                    let g = v(0);
                    [g, g, g, 255]
                }
                3 => [v(0), v(1), v(2), 255],
                _ => [v(0), v(1), v(2), v(3)],
            }
        })
        .collect();
    Some(Palette::new(entries))
}

/// The contract layout of a channel list.
#[derive(Clone, Copy, Debug)]
struct Layout {
    format: PixelFormat,
    bit_depth: u8,
}

/// `index_depth` is the index component's precision when the `Pal8`
/// plan applies (the channels are then the palette columns).
fn layout_for(chans: &[Chan], ycc: bool, index_depth: Option<u8>) -> Result<Layout, Error> {
    let first = chans
        .first()
        .ok_or_else(|| Jpeg2000Error::invalid("codestream has no components"))?;
    if let Some(bits) = index_depth {
        return Ok(Layout {
            format: PixelFormat::Pal8,
            bit_depth: bits,
        });
    }
    let n = chans.len();
    if chans.iter().any(|c| c.signed) {
        return Err(Jpeg2000Error::unsupported(
            "signed components have no contract layout (use decode_j2k / jp2::decode_jp2)",
        ));
    }
    if chans.iter().any(|c| c.precision != first.precision) {
        let depths: Vec<u8> = chans.iter().map(|c| c.precision).collect();
        return Err(Jpeg2000Error::unsupported(format!(
            "mixed component precisions {depths:?} have no contract layout (use decode_j2k)"
        )));
    }
    if first.precision > 16 {
        return Err(Jpeg2000Error::unsupported(format!(
            "{}-bit components exceed the 16-bit contract layouts (use decode_j2k)",
            first.precision
        )));
    }
    let base = (first.hsep, first.vsep);
    let uniform = chans.iter().all(|c| (c.hsep, c.vsep) == base);
    let chroma = if uniform {
        (1u8, 1u8)
    } else {
        let bad = || {
            let seps: Vec<(u8, u8)> = chans.iter().map(|c| (c.hsep, c.vsep)).collect();
            Jpeg2000Error::unsupported(format!(
                "component sub-sampling {seps:?} has no contract layout (use decode_j2k)"
            ))
        };
        if !(n == 3 || n == 4) {
            return Err(bad());
        }
        let c1 = (chans[1].hsep, chans[1].vsep);
        let c2 = (chans[2].hsep, chans[2].vsep);
        if c1 != c2 || (n == 4 && (chans[3].hsep, chans[3].vsep) != base) {
            return Err(bad());
        }
        if c1.0 % base.0 != 0 || c1.1 % base.1 != 0 {
            return Err(bad());
        }
        (c1.0 / base.0, c1.1 / base.1)
    };
    let format = PixelFormat::for_layout(n, first.precision, chroma, ycc || chroma != (1, 1))
        .ok_or_else(|| {
            Jpeg2000Error::unsupported(format!(
                "{n} components at {} bits with chroma sub-sampling {chroma:?} have no contract layout (use decode_j2k)",
                first.precision
            ))
        })?;
    Ok(Layout {
        format,
        bit_depth: first.precision,
    })
}

/// Colour signalling, ICC profile and the YCC flag from the JP2 Header's
/// Colour Specification boxes ([`ColorInfo`] docs): the first box sets
/// the code points, any ICC-method box supplies the profile.
fn colour_from_jp2(h: &Jp2Header) -> (ColorInfo, Option<Vec<u8>>, bool) {
    let icc = h.colr.iter().find_map(|c| c.icc_profile.clone());
    let mut color = ColorInfo::unspecified();
    let mut ycc = false;
    if let Some(first) = h.colr.first() {
        match (first.method, first.enumerated, first.parameterized) {
            (ColrMethod::Enumerated, Some(EnumCs::Srgb), _) => color = ColorInfo::srgb(),
            (ColrMethod::Enumerated, Some(EnumCs::Greyscale), _) => color = ColorInfo::srgb_gray(),
            (ColrMethod::Enumerated, Some(EnumCs::Sycc), _) => {
                color = ColorInfo::sycc();
                ycc = true;
            }
            (ColrMethod::Parameterized, _, Some(p)) => {
                let cp = |v: u16| u8::try_from(v).unwrap_or(ColorInfo::UNSPECIFIED);
                color = ColorInfo::new(
                    if p.video_full_range {
                        ColorRange::Full
                    } else {
                        ColorRange::Limited
                    },
                    cp(p.colour_primaries),
                    cp(p.transfer_characteristics),
                    cp(p.matrix_coefficients),
                );
                ycc = !matches!(p.matrix_coefficients, 0 | 2);
            }
            _ => {}
        }
    }
    (color, icc, ycc)
}

/// Ceiling division of `v` by `2^d` (Equation B-14's reduced-grid map).
#[inline]
fn ceil_shift(v: u32, d: u8) -> u32 {
    if d == 0 {
        return v;
    }
    let step = 1u64 << d.min(32);
    ((u64::from(v) + step - 1) >> d.min(32)) as u32
}

/// Sample extents of a component with separation `(xr, yr)` on the
/// image area (Equations B-1 / B-2), reduced by `reduce` levels.
fn component_dims(siz: &Siz, xr: u8, yr: u8, reduce: u8) -> (u32, u32) {
    let (xr, yr) = (u32::from(xr.max(1)), u32::from(yr.max(1)));
    let x0 = ceil_shift(siz.x_offset.div_ceil(xr), reduce);
    let x1 = ceil_shift(siz.x_size.div_ceil(xr), reduce);
    let y0 = ceil_shift(siz.y_offset.div_ceil(yr), reduce);
    let y1 = ceil_shift(siz.y_size.div_ceil(yr), reduce);
    (x1.saturating_sub(x0), y1.saturating_sub(y0))
}

/// Everything the header walk decides about the picture.
struct Plan {
    layout: Layout,
    palette: Option<Palette>,
    color: ColorInfo,
    icc: Option<Vec<u8>>,
    width: u32,
    height: u32,
    /// `i32` working set of the decode (bytes).
    working_set: u64,
}

fn plan(parsed: &Parsed<'_>, reduce: u8) -> Result<Plan, Error> {
    let siz = &parsed.header.siz;
    let jp2h = parsed.container.as_ref().map(|c| &c.header);
    let (color, icc, ycc) = jp2h.map(colour_from_jp2).unwrap_or_default();
    let palette = palette_plan(siz, jp2h);
    let chans = predicted_channels(siz, jp2h)?;
    let index_depth = palette.as_ref().map(|_| siz.components[0].precision_bits);
    let layout = layout_for(&chans, ycc, index_depth)?;
    let first = chans[0];
    let (width, height) = component_dims(siz, first.hsep, first.vsep, reduce);
    if width == 0 || height == 0 {
        return Err(Jpeg2000Error::invalid(format!(
            "image reduces to {width}x{height} at reduce = {reduce}"
        )));
    }
    let mut working_set: u64 = 0;
    for c in &siz.components {
        let (w, h) = component_dims(siz, c.h_separation, c.v_separation, reduce);
        working_set = working_set.saturating_add((u64::from(w) * u64::from(h)).saturating_mul(4));
    }
    if jp2h.is_some_and(|h| h.cmap.is_some()) {
        for c in &chans {
            let (w, h) = component_dims(siz, c.hsep, c.vsep, reduce);
            working_set =
                working_set.saturating_add((u64::from(w) * u64::from(h)).saturating_mul(4));
        }
    }
    Ok(Plan {
        layout,
        palette,
        color,
        icc,
        width,
        height,
        working_set,
    })
}

// ---------------------------------------------------------------------------
// info
// ---------------------------------------------------------------------------

/// Header-only description of a codestream or JP2 / JPH file: `SIZ` /
/// `COD` and the JP2 Header box are parsed, no packet is read and no
/// sample plane allocated. Returns [`Jpeg2000Error::Unsupported`] when
/// the component set has no contract layout ([`crate::image`] docs);
/// such files remain fully inspectable through
/// [`crate::parse_j2k_header`] / [`crate::jp2::parse_jp2`].
pub fn info(bytes: &[u8]) -> Result<ImageInfo, Error> {
    let parsed = parse(bytes, false)?;
    let plan = plan(&parsed, 0)?;
    let siz = &parsed.header.siz;
    let cod = &parsed.header.cod;
    let (tx, ty) = crate::geometry::tile_grid_extent(siz)?;
    let mut info = ImageInfo::new(
        plan.width,
        plan.height,
        plan.layout.format,
        plan.color,
        plan.icc.is_some(),
        plan.layout.bit_depth,
        siz.components.len() as u16,
        parsed.container.is_some(),
    );
    info.has_alpha |= plan.palette.as_ref().is_some_and(Palette::has_alpha);
    info.high_throughput = siz.rsiz & 0x4000 != 0;
    info.reversible = cod.transform == crate::WaveletTransform::Reversible5x3;
    info.decomposition_levels = cod.decomposition_levels;
    info.layers = cod.layers;
    info.tiles = tx.saturating_mul(ty);
    info.progression = cod.progression;
    Ok(info)
}

// ---------------------------------------------------------------------------
// decode
// ---------------------------------------------------------------------------

/// Decode a codestream or JP2 / JPH file into its native layout with
/// [`DecodeOptions::default`].
pub fn decode(bytes: &[u8]) -> Result<Jpeg2000Image, Error> {
    decode_with(bytes, &DecodeOptions::default())
}

/// [`decode`] with limits, strictness, reduced resolution and layer
/// selection ([`DecodeOptions`]). Limits are checked against the `SIZ`
/// geometry before any plane is allocated.
pub fn decode_with(bytes: &[u8], opts: &DecodeOptions) -> Result<Jpeg2000Image, Error> {
    if opts.layers == Some(0) {
        return Err(Jpeg2000Error::invalid(
            "DecodeOptions::layers = Some(0): an image cannot be reconstructed from zero layers",
        ));
    }
    let parsed = parse(bytes, opts.strict)?;
    let plan = plan(&parsed, opts.reduce)?;
    opts.check(plan.width, plan.height, plan.working_set)?;

    let cs = crate::parse_codestream(parsed.codestream)?;
    if opts.strict {
        let eoc = cs
            .tile_parts
            .last()
            .map_or(cs.header.bytes_consumed, |tp| tp.body_offset + tp.body_len);
        let end = eoc + 2;
        if parsed.codestream.len() > end {
            return Err(Jpeg2000Error::invalid(format!(
                "{} trailing bytes after EOC",
                parsed.codestream.len() - end
            )));
        }
    }
    let decoded = crate::decode::decode_codestream_with(
        parsed.codestream,
        &cs,
        opts.reduce,
        opts.layers.unwrap_or(u16::MAX),
    )?;

    let mut image = if let Some(palette) = plan.palette {
        // The index component itself, not the expanded channels.
        let idx = decoded
            .components
            .into_iter()
            .next()
            .ok_or_else(|| Jpeg2000Error::invalid("codestream has no components"))?;
        pal8_image(idx, plan.layout.bit_depth, plan.width, plan.height)?.with_palette(palette)
    } else {
        let channels = match parsed.container.as_ref() {
            Some(c) => jp2::apply_channel_mapping(&c.header, decoded)?.components,
            None => decoded.components,
        };
        build_image(channels, plan.layout, plan.width, plan.height)?
    };
    image.color = plan.color;
    image.metadata = Metadata::new().with_icc(plan.icc);
    Ok(image)
}

fn pal8_image(
    idx: DecodedComponent,
    bits: u8,
    width: u32,
    height: u32,
) -> Result<Jpeg2000Image, Error> {
    if idx.width != width || idx.height != height {
        return Err(Jpeg2000Error::invalid(format!(
            "index component is {}x{}, header promised {width}x{height}",
            idx.width, idx.height
        )));
    }
    let data: Vec<u8> = idx.samples.iter().map(|&s| s.clamp(0, 255) as u8).collect();
    Jpeg2000Image::packed(width, height, PixelFormat::Pal8, data)?.with_bit_depth(bits)
}

/// Assemble the contract image from the decoded channels: packed
/// layouts interleave into one plane, YCbCr layouts keep one plane per
/// channel. Every channel's grid must match the layout's plane geometry.
fn build_image(
    channels: Vec<DecodedComponent>,
    layout: Layout,
    width: u32,
    height: u32,
) -> Result<Jpeg2000Image, Error> {
    let format = layout.format;
    let bits = layout.bit_depth;
    let max = (1i64 << bits) - 1;
    if channels.len() != format.components() {
        return Err(Jpeg2000Error::invalid(format!(
            "{} channels decoded, {:?} needs {}",
            channels.len(),
            format,
            format.components()
        )));
    }
    for (i, c) in channels.iter().enumerate() {
        let (pw, ph) = format.plane_dimensions(if format.is_yuv() { i } else { 0 }, width, height);
        if c.width != pw || c.height != ph {
            return Err(Jpeg2000Error::unsupported(format!(
                "channel {i} is {}x{}, the {format:?} plane geometry needs {pw}x{ph} (use decode_j2k)",
                c.width, c.height
            )));
        }
        if Chan::of(c).precision != bits || c.is_signed {
            return Err(Jpeg2000Error::invalid(format!(
                "channel {i} precision changed between header and decode"
            )));
        }
    }
    let bps = format.bytes_per_sample();
    let planes = if format.is_yuv() {
        channels
            .iter()
            .map(|c| {
                let stride = c.width as usize * bps;
                let mut data = vec![0u8; stride * c.height as usize];
                write_samples(&mut data, &c.samples, 0, 1, bps, max);
                Plane::new(stride, data)
            })
            .collect()
    } else {
        let n = channels.len();
        let stride = width as usize * n * bps;
        let mut data = vec![0u8; stride * height as usize];
        for (ci, c) in channels.iter().enumerate() {
            write_samples(&mut data, &c.samples, ci, n, bps, max);
        }
        vec![Plane::new(stride, data)]
    };
    Jpeg2000Image::new(width, height, format, planes)?.with_bit_depth(bits)
}

/// Write `samples` (clamped to `0..=max`) into `data` at sample slots
/// `k × step + offset`, one or two little-endian bytes each.
fn write_samples(
    data: &mut [u8],
    samples: &[i32],
    offset: usize,
    step: usize,
    bps: usize,
    max: i64,
) {
    for (k, &s) in samples.iter().enumerate() {
        let v = i64::from(s).clamp(0, max);
        let at = (k * step + offset) * bps;
        if bps == 1 {
            data[at] = v as u8;
        } else {
            data[at..at + 2].copy_from_slice(&(v as u16).to_le_bytes());
        }
    }
}

/// [`decode`] then [`Jpeg2000Image::to_rgb8`]: tightly packed 8-bit RGB.
pub fn decode_rgb8(bytes: &[u8]) -> Result<RgbImage, Error> {
    let img = decode(bytes)?;
    Ok(RgbImage::new(img.width, img.height, img.to_rgb8()))
}

/// [`decode`] then [`Jpeg2000Image::to_rgba8`]: tightly packed 8-bit
/// RGBA (`255` alpha when the layout has none).
pub fn decode_rgba8(bytes: &[u8]) -> Result<RgbaImage, Error> {
    let img = decode(bytes)?;
    Ok(RgbaImage::new(img.width, img.height, img.to_rgba8()))
}

/// Read `r` to its end and [`decode`] the bytes (JPEG 2000 codestreams
/// are not decodable incrementally: the tile-part chain and pointer
/// markers span the whole stream).
pub fn decode_from<R: Read>(mut r: R) -> Result<Jpeg2000Image, Error> {
    let mut bytes = Vec::new();
    r.read_to_end(&mut bytes)?;
    decode(&bytes)
}

// ---------------------------------------------------------------------------
// encode
// ---------------------------------------------------------------------------

/// Encode `image` in its native layout per `opts`.
///
/// Every [`PixelFormat`] is written as it is: packed layouts become one
/// `SIZ` component per channel at 1:1 (`bit_depth` as `Ssiz`), the
/// YCbCr layouts write components 1–2 with the layout's `XRsiz` /
/// `YRsiz` and no MCT (T.800 J.14.1), `Pal8` writes the index component
/// plus a JP2 Palette + Component Mapping box. With
/// [`Container::Jp2`] (the default) the JP2 Header carries the
/// [`ColorInfo`] (sRGB / greyscale / sYCC enumerated colourspace, or a
/// T.814 parameterized box for HT codestreams), the ICC profile, the
/// palette and an opacity channel definition; [`Container::J2k`]
/// writes the bare codestream and keeps none of them.
///
/// `opts.mct == None` turns the RCT / ICT on for the RGB layouts and
/// off otherwise. Returns [`Jpeg2000Error::Unsupported`] for `Pal8`
/// into a bare codestream (the palette would be lost) and
/// [`Jpeg2000Error::InvalidData`] for an image whose planes do not
/// match its geometry or a `Pal8` image without a palette.
pub fn encode(image: &Jpeg2000Image, opts: &EncodeOptions) -> Result<Vec<u8>, Error> {
    image.validate()?;
    let format = image.format;
    let bits = image.bit_depth;
    let n = format.components();
    let (w, h) = (image.width, image.height);

    let mut params = opts.clone();
    let rgb_like = matches!(
        format,
        PixelFormat::Rgb24 | PixelFormat::Rgb48Le | PixelFormat::Rgba | PixelFormat::Rgba64Le
    );
    params.mct = Some(match (opts.mct, format.is_yuv()) {
        (_, true) => false,
        (Some(m), false) => m && n >= 3,
        (None, false) => rgb_like,
    });
    params.sub_sampling = if format.is_yuv() {
        format.component_subsampling()
    } else {
        Vec::new()
    };
    if format == PixelFormat::Pal8 {
        if opts.container == Container::J2k {
            return Err(Jpeg2000Error::unsupported(
                "Pal8 needs the JP2 container (a bare codestream cannot carry the palette)",
            ));
        }
        if image.palette.is_none() {
            return Err(Jpeg2000Error::invalid("Pal8 image without a palette"));
        }
    }

    // Split the planes into per-component sample vectors.
    let bps = format.bytes_per_sample();
    let codestream = if bps == 1 {
        let planes: Vec<Vec<u8>> = component_planes(image, |b| b[0]);
        let refs: Vec<&[u8]> = planes.iter().map(Vec::as_slice).collect();
        if bits == 8 {
            encode_j2k(&refs, w, h, &params)?
        } else {
            let wide: Vec<Vec<u16>> = planes
                .iter()
                .map(|p| p.iter().map(|&v| u16::from(v)).collect())
                .collect();
            let refs: Vec<&[u16]> = wide.iter().map(Vec::as_slice).collect();
            encode_j2k_u16(&refs, w, h, bits, &params)?
        }
    } else {
        let planes: Vec<Vec<u16>> = component_planes(image, |b| u16::from_le_bytes([b[0], b[1]]));
        let refs: Vec<&[u16]> = planes.iter().map(Vec::as_slice).collect();
        encode_j2k_u16(&refs, w, h, bits, &params)?
    };

    match opts.container {
        Container::J2k => Ok(codestream),
        Container::Jp2 => {
            let header = crate::parse_j2k_header(&codestream)?;
            let jph = header.siz.rsiz & 0x4000 != 0;
            jp2::write_jp2(&codestream, &jp2_options(image, jph))
        }
    }
}

/// Gather component `c`'s samples from the image planes in row-major
/// order, `read` decoding one sample from its bytes.
fn component_planes<T: Copy>(image: &Jpeg2000Image, read: impl Fn(&[u8]) -> T) -> Vec<Vec<T>> {
    let format = image.format;
    let bps = format.bytes_per_sample();
    let n = format.components();
    (0..n)
        .map(|c| {
            if format.is_yuv() {
                let (pw, ph) = format.plane_dimensions(c, image.width, image.height);
                let p = &image.planes[c];
                let mut out = Vec::with_capacity(pw as usize * ph as usize);
                for y in 0..ph as usize {
                    let row = &p.data[y * p.stride..];
                    for x in 0..pw as usize {
                        out.push(read(&row[x * bps..x * bps + bps]));
                    }
                }
                out
            } else {
                let p = &image.planes[0];
                let (w, h) = (image.width as usize, image.height as usize);
                let mut out = Vec::with_capacity(w * h);
                for y in 0..h {
                    let row = &p.data[y * p.stride..];
                    for x in 0..w {
                        let at = (x * n + c) * bps;
                        out.push(read(&row[at..at + bps]));
                    }
                }
                out
            }
        })
        .collect()
}

/// The JP2 Header description of an image: colour from
/// [`Jpeg2000Image::color`] / ICC, palette boxes for `Pal8`, opacity
/// channel definitions for the alpha layouts.
fn jp2_options(image: &Jpeg2000Image, jph: bool) -> Jp2WriteOptions {
    let format = image.format;
    let n = format.components();
    let color = image.color;
    // Channel count the colour description sees (palette columns for
    // Pal8).
    let channels = match (format, &image.palette) {
        (PixelFormat::Pal8, Some(p)) => {
            if p.has_alpha() {
                4
            } else {
                3
            }
        }
        _ => n,
    };
    let mut opts = Jp2WriteOptions::for_components(channels);
    let enumerated = |cs: EnumCs| Colr {
        method: ColrMethod::Enumerated,
        precedence: 0,
        approximation: 0,
        enumerated: Some(cs),
        icc_profile: None,
        parameterized: None,
    };
    let srgb_points = color.primaries == ColorInfo::PRIMARIES_BT709
        && color.transfer == ColorInfo::TRANSFER_SRGB
        && color.range != ColorRange::Limited;
    let conventional = if format.is_yuv() {
        EnumCs::Sycc
    } else if channels <= 2 {
        EnumCs::Greyscale
    } else {
        EnumCs::Srgb
    };
    let expressible = !color.is_signalled()
        || (srgb_points
            && ((format.is_yuv() && color.matrix == ColorInfo::MATRIX_BT601)
                || (!format.is_yuv() && color.matrix == ColorInfo::MATRIX_IDENTITY)));
    opts.colour = if let Some(icc) = &image.metadata.icc {
        vec![Colr {
            method: ColrMethod::RestrictedIccProfile,
            precedence: 0,
            approximation: 0,
            enumerated: None,
            icc_profile: Some(icc.clone()),
            parameterized: None,
        }]
    } else if expressible {
        vec![enumerated(conventional)]
    } else if jph {
        vec![Colr {
            method: ColrMethod::Parameterized,
            precedence: 0,
            approximation: 0,
            enumerated: None,
            icc_profile: None,
            parameterized: Some(jp2::ParameterizedColour {
                colour_primaries: u16::from(color.primaries),
                transfer_characteristics: u16::from(color.transfer),
                matrix_coefficients: u16::from(color.matrix),
                video_full_range: color.range != ColorRange::Limited,
            }),
        }]
    } else {
        // Part 1 can only enumerate sRGB / greyscale / sYCC: write the
        // conventional one and flag it as an approximation (`UnkC`).
        opts.colourspace_unknown = true;
        vec![enumerated(conventional)]
    };
    if let (PixelFormat::Pal8, Some(p)) = (format, &image.palette) {
        let column = |k: usize| PclrColumn {
            bit_depth: 8,
            signed: false,
            values: p.entries.iter().map(|e| i32::from(e[k])).collect(),
        };
        let columns: Vec<PclrColumn> = (0..channels).map(column).collect();
        opts.palette = Some(Pclr { columns });
        opts.component_mapping = Some(
            (0..channels)
                .map(|k| CmapEntry {
                    component: 0,
                    mapping: CmapMapping::Palette { column: k as u8 },
                })
                .collect(),
        );
        if channels == 4 {
            opts.channel_definitions = Some(
                (0..4u16)
                    .map(|c| ChannelDef {
                        channel: c,
                        channel_type: if c == 3 {
                            ChannelDef::TYPE_OPACITY
                        } else {
                            ChannelDef::TYPE_COLOUR
                        },
                        association: if c == 3 { 0 } else { c + 1 },
                    })
                    .collect(),
            );
        }
    }
    opts
}

/// Encode tightly packed 8-bit RGB as `Rgb24` (three 8-bit components,
/// RCT / ICT on unless `opts.mct` says otherwise).
pub fn encode_rgb8(
    width: u32,
    height: u32,
    rgb: &[u8],
    opts: &EncodeOptions,
) -> Result<Vec<u8>, Error> {
    encode(
        &Jpeg2000Image::from_rgb8(width, height, rgb.to_vec())?,
        opts,
    )
}

/// Encode tightly packed 8-bit RGBA as `Rgba` (four 8-bit components,
/// the fourth declared as opacity in the JP2 header; RCT / ICT across
/// the first three).
pub fn encode_rgba8(
    width: u32,
    height: u32,
    rgba: &[u8],
    opts: &EncodeOptions,
) -> Result<Vec<u8>, Error> {
    encode(
        &Jpeg2000Image::from_rgba8(width, height, rgba.to_vec())?,
        opts,
    )
}

/// [`encode`] into a writer.
pub fn encode_to<W: Write>(
    image: &Jpeg2000Image,
    opts: &EncodeOptions,
    mut w: W,
) -> Result<(), Error> {
    let bytes = encode(image, opts)?;
    w.write_all(&bytes)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Decoded-image bridge used by the deprecated interleaved entry point.
// ---------------------------------------------------------------------------

/// The pre-contract 8-bit interleaving of a [`DecodedImage`] (every
/// channel unsigned, ≤ 8 bits, one geometry).
pub(crate) fn interleave_8bit(image: &DecodedImage) -> Result<Vec<u8>, Error> {
    let ncomp = image.components.len();
    let first = image
        .components
        .first()
        .ok_or(Jpeg2000Error::NotImplemented)?;
    let (w, h) = (first.width, first.height);
    for c in &image.components {
        if c.precision_bits > 8 || c.is_signed || c.width != w || c.height != h {
            return Err(Jpeg2000Error::NotImplemented);
        }
    }
    let len = (w as usize)
        .checked_mul(h as usize)
        .and_then(|v| v.checked_mul(ncomp))
        .ok_or(Jpeg2000Error::InvalidMarkerLength)?;
    let mut out = vec![0u8; len];
    for (ci, comp) in image.components.iter().enumerate() {
        for (i, &s) in comp.samples.iter().enumerate() {
            out[i * ncomp + ci] = s.clamp(0, 255) as u8;
        }
    }
    Ok(out)
}
