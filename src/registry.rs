//! `oxideav-core` integration — the `Decoder` / `Encoder` adapters,
//! the [`register`] entry point and the [`Jpeg2000Image`] ⇄
//! [`VideoFrame`] bridge.
//!
//! Gated behind the default-on `registry` Cargo feature so consumers
//! that only want the standalone T.800 surface can depend on
//! `oxideav-jpeg2000` with `default-features = false` and skip the
//! `oxideav-core` dependency.
//!
//! The adapters are thin: [`Jpeg2000Decoder`] calls
//! [`crate::decode_with`] on each packet (a complete raw codestream
//! **or** a whole JP2 / JPH file — the framing is sniffed) and emits
//! the image's native layout as a [`VideoFrame`] with the palette /
//! colour-signal / significant-bits side-channels;
//! [`Jpeg2000Encoder`] rebuilds a [`Jpeg2000Image`] from each frame
//! ([`Jpeg2000Image::from_video_frame`]) and calls [`crate::encode()`].
//! One implementation, two entry doors.

use oxideav_core::{
    frame::VideoPlane, CodecCapabilities, CodecId, CodecInfo, CodecOptionsStruct, CodecParameters,
    CodecRegistry, ColorPrimaries, ColorSignal, ContainerRegistry, Decoder, Encoder,
    Error as CoreError, Frame, MatrixCoefficients, MediaType, OptionField, OptionKind, OptionValue,
    Packet, PixelFormat, RuntimeContext, TimeBase, TransferCharacteristics, VideoFrame,
};

use crate::encode::{Container, EncodeKernel, EncodeOptions};
use crate::image::{ColorInfo, ColorRange, Jpeg2000Image, Jpeg2000PixelFormat, Palette, Plane};
use crate::options::DecodeOptions;
use crate::{Jpeg2000Error, ProgressionOrder};

/// Stable identifier this crate registers under in the codec registry.
pub const CODEC_ID_STR: &str = "jpeg2000";

impl From<Jpeg2000Error> for CoreError {
    fn from(e: Jpeg2000Error) -> Self {
        match e {
            Jpeg2000Error::InvalidData(s) => CoreError::invalid(format!("oxideav-jpeg2000: {s}")),
            Jpeg2000Error::Unsupported(s) => {
                CoreError::unsupported(format!("oxideav-jpeg2000: {s}"))
            }
            Jpeg2000Error::LimitExceeded(s) => CoreError::invalid(format!("oxideav-jpeg2000: {s}")),
            Jpeg2000Error::Io(e) => CoreError::Io(e),
            Jpeg2000Error::NotImplemented => {
                CoreError::unsupported(format!("oxideav-jpeg2000: {e}"))
            }
            other => CoreError::invalid(format!("oxideav-jpeg2000: {other}")),
        }
    }
}

// ---- Pixel-format and colour bridges ----------------------------------------

macro_rules! pixel_format_bridge {
    ($($v:ident),* $(,)?) => {
        impl From<Jpeg2000PixelFormat> for PixelFormat {
            fn from(p: Jpeg2000PixelFormat) -> Self {
                match p {
                    $(Jpeg2000PixelFormat::$v => PixelFormat::$v,)*
                }
            }
        }

        impl TryFrom<PixelFormat> for Jpeg2000PixelFormat {
            type Error = Jpeg2000Error;
            fn try_from(p: PixelFormat) -> Result<Self, Jpeg2000Error> {
                Ok(match p {
                    $(PixelFormat::$v => Jpeg2000PixelFormat::$v,)*
                    other => {
                        return Err(Jpeg2000Error::unsupported(format!(
                            "pixel format {other:?} has no JPEG 2000 layout"
                        )))
                    }
                })
            }
        }
    };
}

pixel_format_bridge!(
    Gray8,
    Gray10Le,
    Gray12Le,
    Gray16Le,
    Ya8,
    Ya16Le,
    Rgb24,
    Rgb48Le,
    Rgba,
    Rgba64Le,
    Pal8,
    Yuv444P,
    Yuv444P10Le,
    Yuv444P12Le,
    Yuv444P16Le,
    Yuv422P,
    Yuv422P10Le,
    Yuv422P12Le,
    Yuv422P16Le,
    Yuv420P,
    Yuv420P10Le,
    Yuv420P12Le,
    Yuv420P16Le,
    Yuv440P,
    Yuv440P10Le,
    Yuv440P12Le,
    Yuv440P16Le,
    Yuv411P,
    Yuva444P,
    Yuva444P10Le,
    Yuva444P12Le,
    Yuva444P16Le,
    Yuva422P,
    Yuva422P10Le,
    Yuva422P12Le,
    Yuva422P16Le,
    Yuva420P,
    Yuva420P10Le,
    Yuva420P12Le,
    Yuva420P16Le,
);

/// [`ColorInfo`] as the framework's [`ColorSignal`] (code points map
/// 1:1; `Unspecified` range stays unspecified).
pub fn to_color_signal(c: &ColorInfo) -> ColorSignal {
    let range = match c.range {
        ColorRange::Unspecified => oxideav_core::ColorRange::Unspecified,
        ColorRange::Limited => oxideav_core::ColorRange::Limited,
        ColorRange::Full => oxideav_core::ColorRange::Full,
    };
    ColorSignal::new(
        range,
        ColorPrimaries(c.primaries),
        TransferCharacteristics(c.transfer),
        MatrixCoefficients(c.matrix),
    )
}

/// The inverse of [`to_color_signal`].
pub fn from_color_signal(s: &ColorSignal) -> ColorInfo {
    let range = match s.range {
        oxideav_core::ColorRange::Limited => ColorRange::Limited,
        oxideav_core::ColorRange::Full => ColorRange::Full,
        _ => ColorRange::Unspecified,
    };
    ColorInfo::new(range, s.primaries.0, s.transfer.0, s.matrix.0)
}

// ---- Jpeg2000Image ⇄ VideoFrame -------------------------------------------------

/// [`From<Jpeg2000Image>`] with an explicit `pts`, moving the planes.
/// Side-channels: the palette for `Pal8`; the colour signal when the
/// file carried one (JP2 `colr`); the per-plane significant-bits count
/// when `bit_depth` differs from the layout's own depth (8 / 10 / 12 /
/// 16).
pub(crate) fn image_into_video_frame(image: Jpeg2000Image, pts: Option<i64>) -> VideoFrame {
    let storage = image
        .format
        .fixed_bit_depth()
        .unwrap_or(image.format.storage_bits());
    let bits = image.bit_depth;
    let nplanes = image.planes.len();
    let mut frame = VideoFrame {
        pts,
        planes: image
            .planes
            .into_iter()
            .map(|p| VideoPlane {
                stride: p.stride,
                data: p.data,
            })
            .collect(),
    };
    if let (Jpeg2000PixelFormat::Pal8, Some(p)) = (image.format, &image.palette) {
        frame.set_palette(p.entries.iter().flat_map(|e| [e[0], e[1], e[2]]).collect());
    }
    if image.color.is_signalled() {
        frame.set_color_signal(to_color_signal(&image.color));
    }
    if bits != storage {
        frame.set_significant_bits(vec![bits; nplanes]);
    }
    frame
}

impl From<Jpeg2000Image> for VideoFrame {
    /// The planes (`pts` `None`) plus the side-channels: palette for
    /// `Pal8`, colour signal when the file carried one, significant bits
    /// when `bit_depth` differs from the label's depth.
    fn from(image: Jpeg2000Image) -> Self {
        image_into_video_frame(image, None)
    }
}

impl From<&Jpeg2000Image> for VideoFrame {
    fn from(image: &Jpeg2000Image) -> Self {
        image_into_video_frame(image.clone(), None)
    }
}

impl From<Jpeg2000Image> for Frame {
    fn from(img: Jpeg2000Image) -> Self {
        Frame::Video(img.into())
    }
}

impl Jpeg2000Image {
    /// Rebuild an image from a framework frame and the stream parameters
    /// that describe it (`width`, `height` and `pixel_format` are
    /// required). `Bgr24` / `Bgra` frames are re-ordered into `Rgb24` /
    /// `Rgba`; the palette side-channel (or `extradata` RGB triples)
    /// becomes [`Jpeg2000Image::palette`] for `Pal8`; the colour-signal
    /// side-channel becomes [`Jpeg2000Image::color`]; the significant-bits
    /// side-channel (plane 0) becomes [`Jpeg2000Image::bit_depth`].
    pub fn from_video_frame(
        frame: &VideoFrame,
        params: &CodecParameters,
    ) -> Result<Self, Jpeg2000Error> {
        let width = params
            .width
            .ok_or_else(|| Jpeg2000Error::invalid("missing width"))?;
        let height = params
            .height
            .ok_or_else(|| Jpeg2000Error::invalid("missing height"))?;
        let core_pix = params
            .pixel_format
            .ok_or_else(|| Jpeg2000Error::invalid("missing pixel_format"))?;
        let (pix, swizzle) = match core_pix {
            PixelFormat::Bgr24 => (Jpeg2000PixelFormat::Rgb24, true),
            PixelFormat::Bgra => (Jpeg2000PixelFormat::Rgba, true),
            other => (Jpeg2000PixelFormat::try_from(other)?, false),
        };
        let src = frame.image_planes();
        if src.len() != pix.plane_count() {
            return Err(Jpeg2000Error::invalid(format!(
                "{pix:?} needs {} plane(s), frame has {}",
                pix.plane_count(),
                src.len()
            )));
        }
        let planes: Vec<Plane> = src
            .iter()
            .map(|p| {
                let mut data = p.data.clone();
                if swizzle {
                    let n = pix.components();
                    for px in data.chunks_exact_mut(n) {
                        px.swap(0, 2);
                    }
                }
                Plane::new(p.stride, data)
            })
            .collect();
        let mut img = Jpeg2000Image::new(width, height, pix, planes)?;
        if pix == Jpeg2000PixelFormat::Pal8 {
            let rgb: Option<&[u8]> = frame
                .palette()
                .or((!params.extradata.is_empty()).then_some(params.extradata.as_slice()));
            img.palette = rgb.map(|rgb| {
                Palette::new(
                    rgb.chunks_exact(3)
                        .map(|c| [c[0], c[1], c[2], 255])
                        .collect(),
                )
            });
            if img.palette.is_none() {
                return Err(Jpeg2000Error::invalid(
                    "Pal8 frame without a palette side-channel or extradata",
                ));
            }
        }
        if let Some(sig) = frame.color_signal() {
            img.color = from_color_signal(&sig);
        }
        if let Some(bits) = frame.plane_significant_bits(0) {
            img = img.with_bit_depth(bits)?;
        }
        Ok(img)
    }
}

impl TryFrom<(&VideoFrame, &CodecParameters)> for Jpeg2000Image {
    type Error = Jpeg2000Error;
    fn try_from((frame, params): (&VideoFrame, &CodecParameters)) -> Result<Self, Jpeg2000Error> {
        Jpeg2000Image::from_video_frame(frame, params)
    }
}

// ---- CodecOptionsStruct (registry-only schema for EncodeOptions) ------------

/// The registry's view of [`EncodeOptions`]: the same knobs under the
/// `CodecOptions` string keys, with the registry default container
/// `j2k` (a bare codestream — the framework carries colour on the
/// frame, not in the file).
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub struct RegistryEncodeOptions {
    /// The options the keys build (kernel resolved by
    /// [`Self::into_options`]).
    pub inner: EncodeOptions,
    lossless: bool,
    fine_bits: u8,
}

impl Default for RegistryEncodeOptions {
    fn default() -> Self {
        Self {
            inner: EncodeOptions::default().with_container(Container::J2k),
            lossless: true,
            fine_bits: 6,
        }
    }
}

impl RegistryEncodeOptions {
    /// The resolved [`EncodeOptions`]: `lossless` picks the kernel,
    /// `fine_bits` its 9-7 step.
    pub fn into_options(self) -> EncodeOptions {
        let mut o = self.inner;
        o.kernel = if self.lossless {
            EncodeKernel::Lossless5x3
        } else {
            EncodeKernel::Lossy9x7 {
                fine_bits: self.fine_bits,
            }
        };
        o
    }
}

impl CodecOptionsStruct for RegistryEncodeOptions {
    const SCHEMA: &'static [OptionField] = &[
        OptionField {
            name: "lossless",
            kind: OptionKind::Bool,
            default: OptionValue::Bool(true),
            help: "Reversible 5-3 kernel (true, default) or irreversible 9-7 (false).",
        },
        OptionField {
            name: "fine_bits",
            kind: OptionKind::U32,
            default: OptionValue::U32(6),
            help: "9-7 quantisation step 2^-fine_bits (0..=8; only with lossless = false).",
        },
        OptionField {
            name: "psnr",
            kind: OptionKind::F32,
            default: OptionValue::F32(0.0),
            help: "PCRD PSNR floor in dB (0 = none).",
        },
        OptionField {
            name: "target_bytes",
            kind: OptionKind::U32,
            default: OptionValue::U32(0),
            help: "PCRD byte budget per codestream (0 = none; bit_rate / frame_rate otherwise).",
        },
        OptionField {
            name: "levels",
            kind: OptionKind::U32,
            default: OptionValue::U32(3),
            help: "Wavelet decomposition levels NL (0..=32).",
        },
        OptionField {
            name: "layers",
            kind: OptionKind::U32,
            default: OptionValue::U32(1),
            help: "Quality layers (1..=65535).",
        },
        OptionField {
            name: "progression",
            kind: OptionKind::Enum(&["lrcp", "rlcp", "rpcl", "pcrl", "cprl"]),
            default: OptionValue::String(String::new()),
            help: "Packet progression order (default lrcp).",
        },
        OptionField {
            name: "tile",
            kind: OptionKind::String,
            default: OptionValue::String(String::new()),
            help: "Tile size as WxH on the reference grid (empty = one tile).",
        },
        OptionField {
            name: "ht",
            kind: OptionKind::Bool,
            default: OptionValue::Bool(false),
            help: "Use the T.814 HT block coder (JPH brand under container = jp2).",
        },
        OptionField {
            name: "plt",
            kind: OptionKind::Bool,
            default: OptionValue::Bool(false),
            help: "Emit PLT packet-length markers.",
        },
        OptionField {
            name: "tlm",
            kind: OptionKind::Bool,
            default: OptionValue::Bool(false),
            help: "Emit a TLM tile-part-length marker.",
        },
        OptionField {
            name: "sop",
            kind: OptionKind::Bool,
            default: OptionValue::Bool(false),
            help: "Emit SOP marker segments.",
        },
        OptionField {
            name: "eph",
            kind: OptionKind::Bool,
            default: OptionValue::Bool(false),
            help: "Emit EPH markers.",
        },
        OptionField {
            name: "comment",
            kind: OptionKind::String,
            default: OptionValue::String(String::new()),
            help: "COM marker text.",
        },
        OptionField {
            name: "container",
            kind: OptionKind::Enum(&["j2k", "j2c", "jp2", "jph"]),
            default: OptionValue::String(String::new()),
            help: "Bare codestream (j2k / j2c, default) or JP2 / JPH file (jp2 / jph).",
        },
    ];

    fn apply(&mut self, key: &str, value: &OptionValue) -> oxideav_core::Result<()> {
        let o = &mut self.inner;
        match key {
            "lossless" => self.lossless = value.as_bool()?,
            "fine_bits" => {
                let f = value.as_u32()?;
                if f > 8 {
                    return Err(CoreError::invalid(format!(
                        "oxideav-jpeg2000 encoder: fine_bits {f} out of 0..=8"
                    )));
                }
                self.fine_bits = f as u8;
            }
            "psnr" => {
                let p = value.as_f32()?;
                o.target_psnr = (p > 0.0).then_some(f64::from(p));
            }
            "target_bytes" => {
                let b = value.as_u32()?;
                o.target_bytes = (b > 0).then_some(b as usize);
            }
            "levels" => {
                let l = value.as_u32()?;
                o.decomposition_levels =
                    u8::try_from(l).ok().filter(|l| *l <= 32).ok_or_else(|| {
                        CoreError::invalid(format!(
                            "oxideav-jpeg2000 encoder: levels {l} out of 0..=32"
                        ))
                    })?;
            }
            "layers" => {
                let l = value.as_u32()?;
                o.layers = u16::try_from(l).ok().filter(|l| *l >= 1).ok_or_else(|| {
                    CoreError::invalid(format!(
                        "oxideav-jpeg2000 encoder: layers {l} out of 1..=65535"
                    ))
                })?;
            }
            "progression" => {
                o.progression = match value.as_str()? {
                    "lrcp" => ProgressionOrder::Lrcp,
                    "rlcp" => ProgressionOrder::Rlcp,
                    "rpcl" => ProgressionOrder::Rpcl,
                    "pcrl" => ProgressionOrder::Pcrl,
                    "cprl" => ProgressionOrder::Cprl,
                    _ => unreachable!("guarded by SCHEMA"),
                };
            }
            "tile" => {
                let v = value.as_str()?;
                if v.is_empty() {
                    o.tile_size = None;
                } else {
                    let bad = || {
                        CoreError::invalid(format!(
                            "oxideav-jpeg2000 encoder: option tile={v:?} is invalid"
                        ))
                    };
                    let (w, h) = v.split_once('x').ok_or_else(bad)?;
                    o.tile_size =
                        Some((w.parse().map_err(|_| bad())?, h.parse().map_err(|_| bad())?));
                }
            }
            "ht" => o.high_throughput = value.as_bool()?,
            "plt" => o.plt = value.as_bool()?,
            "tlm" => o.tlm = value.as_bool()?,
            "sop" => o.sop = value.as_bool()?,
            "eph" => o.eph = value.as_bool()?,
            "comment" => {
                let c = value.as_str()?;
                o.comment = (!c.is_empty()).then(|| c.to_owned());
            }
            "container" => {
                o.container = match value.as_str()? {
                    "j2k" | "j2c" => Container::J2k,
                    "jp2" | "jph" => Container::Jp2,
                    _ => unreachable!("guarded by SCHEMA"),
                };
            }
            _ => unreachable!("guarded by SCHEMA"),
        }
        Ok(())
    }
}

// ---- Registration -------------------------------------------------------------

/// Register the JPEG 2000 decoder + encoder factories into a
/// [`CodecRegistry`].
pub fn register_codecs(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("jpeg2000_sw")
        .with_intra_only(true)
        .with_lossless(true)
        .with_pixel_formats(
            Jpeg2000PixelFormat::ALL
                .iter()
                .map(|&f| PixelFormat::from(f))
                .collect(),
        );
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .encoder(make_encoder)
            .encoder_options::<RegistryEncodeOptions>(),
    );
}

/// Register the file extensions (`.j2k` / `.j2c` raw codestreams,
/// `.jp2` / `.jph` files) so a [`RuntimeContext`] can map a filename
/// hint back to the codec id.
pub fn register_containers(reg: &mut ContainerRegistry) {
    reg.register_extension("j2k", CODEC_ID_STR);
    reg.register_extension("j2c", CODEC_ID_STR);
    reg.register_extension("jp2", CODEC_ID_STR);
    reg.register_extension("jph", CODEC_ID_STR);
}

/// Unified registration entry point: install the codec factories and
/// the extension hints into the supplied [`RuntimeContext`].
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
    register_containers(&mut ctx.containers);
}

// ---- Decoder --------------------------------------------------------------------

/// Factory registered with the codec registry. The framework's
/// [`oxideav_core::DecoderLimits`] tighten the standalone
/// [`DecodeOptions`] (never loosen them); the `CodecOptions` keys
/// `reduce` (resolution levels to discard), `layers` (quality layers to
/// decode) and `strict` are honoured.
pub fn make_decoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Decoder>> {
    let limits = params.limits();
    let mut opts = DecodeOptions::default();
    opts.max_pixels = Some(opts.max_pixels.map_or(limits.max_pixels_per_frame, |m| {
        m.min(limits.max_pixels_per_frame)
    }));
    opts.max_bytes = Some(
        opts.max_bytes
            .map_or(limits.max_alloc_bytes_per_frame, |m| {
                m.min(limits.max_alloc_bytes_per_frame)
            }),
    );
    let bad = |k: &str, v: &str| {
        CoreError::invalid(format!(
            "oxideav-jpeg2000 decoder: option {k}={v:?} is invalid"
        ))
    };
    if let Some(v) = params.options.get("reduce") {
        opts.reduce = v.parse().map_err(|_| bad("reduce", v))?;
    }
    if let Some(v) = params.options.get("layers") {
        let l: u16 = v.parse().map_err(|_| bad("layers", v))?;
        if l == 0 {
            return Err(bad("layers", v));
        }
        opts.layers = Some(l);
    }
    if let Some(v) = params.options.get("strict") {
        opts.strict = match v {
            "true" | "1" | "yes" | "on" => true,
            "false" | "0" | "no" | "off" => false,
            _ => return Err(bad("strict", v)),
        };
    }
    Ok(Box::new(Jpeg2000Decoder::with_options(
        params.clone(),
        opts,
    )))
}

/// JPEG 2000 [`Decoder`] trait impl.
///
/// One-packet-in / one-frame-out: each `send_packet` carries one
/// complete raw codestream or JP2 / JPH file; the matching
/// `receive_frame` returns the picture in its native layout
/// ([`crate::image`] docs) with the palette / colour-signal /
/// significant-bits side-channels, and the decoder's
/// [`CodecParameters`] take the decoded geometry and pixel format.
#[derive(Debug)]
pub struct Jpeg2000Decoder {
    params: CodecParameters,
    opts: DecodeOptions,
    pending: Option<Packet>,
    eof: bool,
}

impl Jpeg2000Decoder {
    /// Build a decoder whose output [`CodecParameters`] start from
    /// `params` (default [`DecodeOptions`]); geometry and pixel format
    /// are re-derived from each successfully decoded frame.
    pub fn new(params: CodecParameters) -> Self {
        Self::with_options(params, DecodeOptions::default())
    }

    /// [`Self::new`] with explicit decode options.
    pub fn with_options(params: CodecParameters, opts: DecodeOptions) -> Self {
        let mut p = params;
        p.media_type = MediaType::Video;
        p.codec_id = CodecId::new(CODEC_ID_STR);
        Self {
            params: p,
            opts,
            pending: None,
            eof: false,
        }
    }

    /// The decoder's current [`CodecParameters`] — authoritative after
    /// the first successful `receive_frame`.
    pub fn params(&self) -> &CodecParameters {
        &self.params
    }
}

impl Decoder for Jpeg2000Decoder {
    fn codec_id(&self) -> &CodecId {
        &self.params.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> oxideav_core::Result<()> {
        if self.pending.is_some() {
            return Err(CoreError::other(
                "oxideav-jpeg2000 decoder: receive_frame must be called before sending another packet",
            ));
        }
        self.pending = Some(packet.clone());
        Ok(())
    }

    fn receive_frame(&mut self) -> oxideav_core::Result<Frame> {
        let Some(pkt) = self.pending.take() else {
            return if self.eof {
                Err(CoreError::Eof)
            } else {
                Err(CoreError::NeedMore)
            };
        };
        let image = crate::decode_with(&pkt.data, &self.opts)?;
        self.params.width = Some(image.width);
        self.params.height = Some(image.height);
        self.params.pixel_format = Some(image.format.into());
        if image.color.is_signalled() {
            self.params.color_signal = to_color_signal(&image.color);
        }
        Ok(Frame::Video(image_into_video_frame(image, pkt.pts)))
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        Ok(())
    }
}

// ---- Encoder --------------------------------------------------------------------

/// Factory for the [`Encoder`] trait impl — installed in the codec
/// registry by [`register`]. Options are validated when the first
/// frame is encoded (the factory only captures the parameter set).
pub fn make_encoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Encoder>> {
    Ok(Box::new(Jpeg2000Encoder::new(params.clone())))
}

/// JPEG 2000 [`Encoder`] trait impl.
///
/// Takes one frame in any [`Jpeg2000PixelFormat`] (plus `Bgr24` /
/// `Bgra`, re-ordered) — from [`CodecParameters::pixel_format`], else
/// inferred from the stride as 8-bit Gray / RGB / RGBA — and emits one
/// intra packet per frame through [`crate::encode()`]. The coding shape
/// comes from the parameters: `bit_rate` (bits per second with
/// `frame_rate`, else bits per frame) becomes a PCRD byte budget, and
/// the `CodecOptions` keys are those of [`RegistryEncodeOptions`]
/// (`lossless`, `fine_bits`, `psnr`, `target_bytes`, `levels`,
/// `layers`, `progression`, `tile`, `ht`, `plt`, `tlm`, `sop`, `eph`,
/// `comment`, `container` — default `j2k`).
#[derive(Debug)]
pub struct Jpeg2000Encoder {
    params: CodecParameters,
    pending: Option<Packet>,
    eof: bool,
}

impl Jpeg2000Encoder {
    /// Build an encoder. `params.width` / `params.height` must be set
    /// before the first frame.
    pub fn new(params: CodecParameters) -> Self {
        let mut p = params;
        p.media_type = MediaType::Video;
        p.codec_id = CodecId::new(CODEC_ID_STR);
        Self {
            params: p,
            pending: None,
            eof: false,
        }
    }

    /// The [`EncodeOptions`] the parameters select.
    fn encode_options(&self) -> oxideav_core::Result<EncodeOptions> {
        let mut o = oxideav_core::parse_options::<RegistryEncodeOptions>(&self.params.options)?
            .into_options();
        if o.target_bytes.is_none() {
            if let Some(bit_rate) = self.params.bit_rate {
                // Bits per second over the frame rate, or bits per frame.
                let bits_per_frame = match self.params.frame_rate {
                    Some(r) if r.num > 0 && r.den > 0 => {
                        (bit_rate as u128 * r.den as u128 / r.num as u128) as u64
                    }
                    _ => bit_rate,
                };
                o.target_bytes = Some(usize::try_from(bits_per_frame / 8).unwrap_or(usize::MAX));
            }
        }
        Ok(o)
    }
}

impl Encoder for Jpeg2000Encoder {
    fn codec_id(&self) -> &CodecId {
        &self.params.codec_id
    }

    fn output_params(&self) -> &CodecParameters {
        &self.params
    }

    fn send_frame(&mut self, frame: &Frame) -> oxideav_core::Result<()> {
        if self.pending.is_some() {
            return Err(CoreError::other(
                "oxideav-jpeg2000 encoder: receive_packet must be called before sending another frame",
            ));
        }
        let Frame::Video(v) = frame else {
            return Err(CoreError::unsupported(
                "oxideav-jpeg2000 encoder: only video frames are supported",
            ));
        };
        let (width, height) = match (self.params.width, self.params.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
            _ => {
                return Err(CoreError::invalid(
                    "oxideav-jpeg2000 encoder: CodecParameters width/height required",
                ))
            }
        };
        let mut params = self.params.clone();
        if params.pixel_format.is_none() {
            let plane = v.planes.first().ok_or_else(|| {
                CoreError::invalid("oxideav-jpeg2000 encoder: video frame has no planes")
            })?;
            params.pixel_format = Some(match (v.planes.len(), plane.stride / width as usize) {
                (1, 1) => PixelFormat::Gray8,
                (1, 3) => PixelFormat::Rgb24,
                (1, 4) => PixelFormat::Rgba,
                _ => return Err(CoreError::unsupported(
                    "oxideav-jpeg2000 encoder: cannot infer a packed pixel format from the stride",
                )),
            });
        }
        let opts = self.encode_options()?;
        let image = Jpeg2000Image::from_video_frame(v, &params)?;
        let bytes_out = crate::encode(&image, &opts)?;
        self.params.width = Some(width);
        self.params.height = Some(height);
        self.params.pixel_format = Some(image.format.into());
        let mut pkt = Packet::new(0, TimeBase::new(1, 1), bytes_out);
        pkt.pts = v.pts;
        pkt.dts = v.pts;
        pkt.flags.keyframe = true; // intra-only
        self.pending = Some(pkt);
        Ok(())
    }

    fn receive_packet(&mut self) -> oxideav_core::Result<Packet> {
        match self.pending.take() {
            Some(pkt) => Ok(pkt),
            None if self.eof => Err(CoreError::Eof),
            None => Err(CoreError::NeedMore),
        }
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_installs_decoder_factory_and_extensions() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let id = CodecId::new(CODEC_ID_STR);
        assert!(
            ctx.codecs.has_decoder(&id),
            "jpeg2000 decoder factory not installed via RuntimeContext"
        );
        assert!(
            ctx.codecs.has_encoder(&id),
            "jpeg2000 encoder factory not installed via RuntimeContext"
        );
        for ext in ["j2k", "j2c", "jp2", "jph"] {
            assert_eq!(
                ctx.containers.container_for_extension(ext),
                Some(CODEC_ID_STR),
                "{ext}"
            );
        }
    }

    #[test]
    fn encoder_round_trips_through_decoder() {
        // Drive the Encoder trait impl with a packed Rgb24 frame, then
        // feed the produced packet to the Decoder trait impl and assert
        // the pixels round-trip bit-exactly (the lossless 5-3 path).
        let (w, h) = (10u32, 7u32);
        let ncomp = 3usize;
        let data: Vec<u8> = (0..(w * h) as usize * ncomp)
            .map(|i| (i * 37 % 256) as u8)
            .collect();
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(w);
        params.height = Some(h);
        let mut enc = make_encoder(&params).expect("encoder factory");
        let frame = Frame::Video(VideoFrame {
            pts: Some(42),
            planes: vec![VideoPlane {
                stride: w as usize * ncomp,
                data: data.clone(),
            }],
        });
        enc.send_frame(&frame).expect("send_frame");
        let pkt = enc.receive_packet().expect("receive_packet");
        assert!(pkt.flags.keyframe);
        assert_eq!(pkt.pts, Some(42));
        assert_eq!(
            &pkt.data[..2],
            &[0xFF, 0x4F],
            "registry default is a bare codestream"
        );

        let mut dec =
            make_decoder(&CodecParameters::video(CodecId::new(CODEC_ID_STR))).expect("factory");
        dec.send_packet(&pkt).expect("send_packet");
        let Frame::Video(out) = dec.receive_frame().expect("receive_frame") else {
            panic!("expected a video frame");
        };
        assert_eq!(out.planes.len(), 1);
        assert_eq!(out.planes[0].data, data, "registry round-trip pixels");
        assert_eq!(
            out.color_signal(),
            None,
            "a bare codestream signals no colour"
        );
    }

    fn drive(params: &CodecParameters, stride: usize, data: Vec<u8>) -> (Packet, VideoFrame) {
        let mut enc = make_encoder(params).expect("encoder factory");
        enc.send_frame(&Frame::Video(VideoFrame {
            pts: Some(1),
            planes: vec![VideoPlane { stride, data }],
        }))
        .expect("send_frame");
        let pkt = enc.receive_packet().expect("receive_packet");
        let mut dec =
            make_decoder(&CodecParameters::video(CodecId::new(CODEC_ID_STR))).expect("factory");
        dec.send_packet(&pkt).expect("send_packet");
        let Frame::Video(out) = dec.receive_frame().expect("receive_frame") else {
            panic!("expected a video frame");
        };
        (pkt, out)
    }

    #[test]
    fn encoder_honours_packed_pixel_formats_both_depths() {
        let (w, h) = (9u32, 6u32);
        let n = (w * h) as usize;
        // 8-bit layouts: BGR / BGRA inputs come back as RGB / RGBA.
        for (format, ncomp, swap) in [
            (PixelFormat::Gray8, 1usize, false),
            (PixelFormat::Rgb24, 3, false),
            (PixelFormat::Bgr24, 3, true),
            (PixelFormat::Rgba, 4, false),
            (PixelFormat::Bgra, 4, true),
            (PixelFormat::Ya8, 2, false),
        ] {
            let data: Vec<u8> = (0..n * ncomp).map(|i| (i * 53 % 256) as u8).collect();
            let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
            params.width = Some(w);
            params.height = Some(h);
            params.pixel_format = Some(format);
            let (_, out) = drive(&params, w as usize * ncomp, data.clone());
            let mut want = data.clone();
            if swap {
                for px in want.chunks_exact_mut(ncomp) {
                    px.swap(0, 2);
                }
            }
            assert_eq!(out.planes[0].data, want, "{format:?}");
        }
        // 16-bit layouts round-trip through the u16 path and come back
        // as the same little-endian format.
        for (format, ncomp, depth) in [
            (PixelFormat::Gray16Le, 1usize, 16u32),
            (PixelFormat::Gray12Le, 1, 12),
            (PixelFormat::Gray10Le, 1, 10),
            (PixelFormat::Rgb48Le, 3, 16),
            (PixelFormat::Rgba64Le, 4, 16),
            (PixelFormat::Ya16Le, 2, 16),
        ] {
            let max = (1u32 << depth) - 1;
            let data: Vec<u8> = (0..n * ncomp)
                .map(|i| ((i as u32 * 2_749) % (max + 1)) as u16)
                .flat_map(u16::to_le_bytes)
                .collect();
            let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
            params.width = Some(w);
            params.height = Some(h);
            params.pixel_format = Some(format);
            let (pkt, out) = drive(&params, w as usize * ncomp * 2, data.clone());
            assert_eq!(out.planes[0].data, data, "{format:?}");
            let hdr = crate::parse_j2k_header(&pkt.data).expect("header");
            assert_eq!(u32::from(hdr.siz.components[0].precision_bits), depth);
            assert_eq!(
                out.significant_bits(),
                None,
                "{format:?}: label-exact depth"
            );
        }
    }

    #[test]
    fn planar_yuv_and_deep_gray_ride_side_channels() {
        // A 4:2:0 frame encodes with SIZ sub-sampling and decodes back
        // planar, byte-exact, with no MCT.
        let (w, h) = (6u32, 4u32);
        let y: Vec<u8> = (0..(w * h) as usize)
            .map(|i| (i * 11 % 256) as u8)
            .collect();
        let c: Vec<u8> = (0..6).map(|i| (100 + i * 7) as u8).collect();
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(w);
        params.height = Some(h);
        params.pixel_format = Some(PixelFormat::Yuv420P);
        params.options = oxideav_core::CodecOptions::new().set("container", "jp2");
        let mut enc = make_encoder(&params).expect("factory");
        enc.send_frame(&Frame::Video(VideoFrame {
            pts: None,
            planes: vec![
                VideoPlane {
                    stride: 6,
                    data: y.clone(),
                },
                VideoPlane {
                    stride: 3,
                    data: c.clone(),
                },
                VideoPlane {
                    stride: 3,
                    data: c.clone(),
                },
            ],
        }))
        .expect("send_frame");
        let pkt = enc.receive_packet().expect("packet");
        let hdr = crate::parse_j2k_header(
            &crate::jp2::parse_jp2(&pkt.data)
                .map(|c| {
                    pkt.data[c.codestream_offset..c.codestream_offset + c.codestream_len].to_vec()
                })
                .expect("jp2"),
        )
        .expect("header");
        assert_eq!(hdr.cod.multi_component_transform, 0);
        assert_eq!(hdr.siz.components[1].h_separation, 2);
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(CODEC_ID_STR))).unwrap();
        dec.send_packet(&pkt).unwrap();
        let Frame::Video(out) = dec.receive_frame().unwrap() else {
            panic!()
        };
        assert_eq!(out.image_planes().len(), 3);
        assert_eq!(out.planes[0].data, y);
        assert_eq!(out.planes[1].data, c);
        assert_eq!(
            out.color_signal().map(|s| s.matrix.0),
            Some(5),
            "JP2 sYCC colr → BT.601 matrix on the frame"
        );

        // A 14-bit gray frame (Gray16Le + significant bits 14) keeps its
        // depth through SIZ and comes back with the side-channel set.
        let g: Vec<u8> = (0..(w * h) as usize)
            .map(|i| (i as u32 * 911 % 16384) as u16)
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(w);
        params.height = Some(h);
        params.pixel_format = Some(PixelFormat::Gray16Le);
        let mut enc = make_encoder(&params).expect("factory");
        enc.send_frame(&Frame::Video(
            VideoFrame {
                pts: None,
                planes: vec![VideoPlane {
                    stride: 12,
                    data: g.clone(),
                }],
            }
            .with_significant_bits(vec![14]),
        ))
        .expect("send_frame");
        let pkt = enc.receive_packet().expect("packet");
        assert_eq!(
            crate::parse_j2k_header(&pkt.data).unwrap().siz.components[0].precision_bits,
            14
        );
        let mut dec = make_decoder(&CodecParameters::video(CodecId::new(CODEC_ID_STR))).unwrap();
        dec.send_packet(&pkt).unwrap();
        let Frame::Video(out) = dec.receive_frame().unwrap() else {
            panic!()
        };
        assert_eq!(out.planes[0].data, g);
        assert_eq!(out.significant_bits(), Some(&[14u8][..]));
    }

    #[test]
    fn encoder_options_select_kernel_budget_container_and_markers() {
        let (w, h) = (32u32, 24u32);
        let n = (w * h) as usize;
        let data: Vec<u8> = (0..n * 3).map(|i| (i * 131 % 251) as u8).collect();
        let base = |opts: oxideav_core::CodecOptions| {
            let mut p = CodecParameters::video(CodecId::new(CODEC_ID_STR));
            p.width = Some(w);
            p.height = Some(h);
            p.pixel_format = Some(PixelFormat::Rgb24);
            p.options = opts;
            p
        };
        // Lossless default: bit-exact, RCT signalled.
        let (pkt, out) = drive(
            &base(oxideav_core::CodecOptions::new()),
            w as usize * 3,
            data.clone(),
        );
        assert_eq!(out.planes[0].data, data);
        let hdr = crate::parse_j2k_header(&pkt.data).expect("header");
        assert_eq!(hdr.cod.multi_component_transform, 1);
        // Lossy 9-7 with a PSNR floor, layers, progression, tiles, PLT.
        let opts = oxideav_core::CodecOptions::new()
            .set("lossless", "false")
            .set("fine_bits", "3")
            .set("psnr", "34")
            .set("layers", "2")
            .set("progression", "rpcl")
            .set("tile", "16x16")
            .set("plt", "true")
            .set("comment", "registry");
        let (pkt, out) = drive(&base(opts), w as usize * 3, data.clone());
        let hdr = crate::parse_j2k_header(&pkt.data).expect("header");
        assert_eq!(hdr.cod.progression, crate::ProgressionOrder::Rpcl);
        assert_eq!(hdr.cod.layers, 2);
        assert_eq!(hdr.siz.tile_width, 16);
        assert!(pkt.data.windows(2).any(|x| x == [0xFF, 0x58]), "PLT");
        assert!(pkt.data.windows(2).any(|x| x == [0xFF, 0x64]), "COM");
        let sse: f64 = out.planes[0]
            .data
            .iter()
            .zip(&data)
            .map(|(&g, &wv)| (f64::from(g) - f64::from(wv)).powi(2))
            .sum();
        let psnr = 10.0 * (255.0f64 * 255.0 / (sse / (n * 3) as f64)).log10();
        assert!(psnr >= 34.0, "{psnr}");
        // A bit_rate with a frame rate is a per-frame budget.
        let mut p = base(oxideav_core::CodecOptions::new());
        p.bit_rate = Some(8 * 600 * 25);
        p.frame_rate = Some(oxideav_core::Rational::new(25, 1));
        let (pkt, _) = drive(&p, w as usize * 3, data.clone());
        assert!(pkt.data.len() <= 600, "{}", pkt.data.len());
        // JP2 container + HT block coder.
        let opts = oxideav_core::CodecOptions::new()
            .set("container", "jp2")
            .set("ht", "true");
        let (pkt, out) = drive(&base(opts), w as usize * 3, data.clone());
        assert!(crate::info(&pkt.data).expect("info").jp2);
        assert_eq!(out.planes[0].data, data);
        let c = crate::jp2::parse_jp2(&pkt.data).expect("parse");
        assert!(c.ftyp.is_jph_compatible());
        assert_eq!(
            out.color_signal().map(|s| s.primaries.0),
            Some(1),
            "sRGB colr"
        );
        // Malformed options surface a clean error at encode time.
        for (k, v) in [
            ("levels", "many"),
            ("container", "tiff"),
            ("fine_bits", "9"),
        ] {
            let opts = oxideav_core::CodecOptions::new().set(k, v);
            let mut enc = make_encoder(&base(opts)).expect("factory defers validation");
            assert!(
                enc.send_frame(&Frame::Video(VideoFrame {
                    pts: None,
                    planes: vec![VideoPlane {
                        stride: w as usize * 3,
                        data: data.clone(),
                    }],
                }))
                .is_err(),
                "{k}={v}"
            );
        }
    }

    #[test]
    fn frame_bridge_round_trips_palette_and_colour() {
        let img = Jpeg2000Image::packed(2, 2, Jpeg2000PixelFormat::Pal8, vec![0, 1, 1, 0])
            .unwrap()
            .with_palette(Palette::new(vec![[1, 2, 3, 255], [4, 5, 6, 255]]))
            .with_color(ColorInfo::srgb());
        let frame: VideoFrame = img.clone().into();
        assert_eq!(frame.palette(), Some(&[1u8, 2, 3, 4, 5, 6][..]));
        assert!(frame.color_signal().is_some());
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(2);
        params.height = Some(2);
        params.pixel_format = Some(PixelFormat::Pal8);
        let back = Jpeg2000Image::try_from((&frame, &params)).expect("bridge");
        assert_eq!(back, img);
        // Missing parameters are an error, not a panic.
        assert!(Jpeg2000Image::from_video_frame(
            &frame,
            &CodecParameters::video(CodecId::new(CODEC_ID_STR))
        )
        .is_err());
    }

    #[test]
    fn decoder_options_reduce_and_strict() {
        let (w, h) = (16u32, 12u32);
        let data: Vec<u8> = (0..(w * h) as usize).map(|i| (i % 251) as u8).collect();
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(w);
        params.height = Some(h);
        params.pixel_format = Some(PixelFormat::Gray8);
        let mut enc = make_encoder(&params).unwrap();
        enc.send_frame(&Frame::Video(VideoFrame {
            pts: None,
            planes: vec![VideoPlane {
                stride: w as usize,
                data,
            }],
        }))
        .unwrap();
        let pkt = enc.receive_packet().unwrap();
        let mut dp = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        dp.options = oxideav_core::CodecOptions::new().set("reduce", "1");
        let mut dec = make_decoder(&dp).unwrap();
        dec.send_packet(&pkt).unwrap();
        let Frame::Video(out) = dec.receive_frame().unwrap() else {
            panic!()
        };
        assert_eq!(out.planes[0].stride, 8);
        assert_eq!(out.planes[0].data.len(), 8 * 6);
        let mut bad = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        bad.options = oxideav_core::CodecOptions::new().set("layers", "0");
        assert!(make_decoder(&bad).is_err());
    }
}
