//! The standalone image types — the shapes every `oxideav-<format>`
//! image crate shares (`IMAGE_CRATE_API`), specialised for JPEG 2000.
//!
//! * [`Jpeg2000Image`] (alias [`J2kImage`]) — the native-layout image
//!   [`crate::decode()`] returns and [`crate::encode()`] consumes:
//!   dimensions, a [`PixelFormat`] tag, one packed [`Plane`] or one
//!   plane per component for the YCbCr layouts, [`ColorInfo`],
//!   [`Metadata`], an optional [`Palette`] (JP2 `pclr`) and the
//!   component bit depth.
//! * [`RgbImage`] / [`RgbaImage`] — the tightly packed 8-bit raw paths
//!   ([`crate::decode_rgb8`] / [`crate::decode_rgba8`],
//!   [`Jpeg2000Image::to_rgb8`] / [`Jpeg2000Image::to_rgba8`]).
//! * [`ImageInfo`] — what [`crate::info`] reads from the `SIZ` / `COD`
//!   main-header segments and the JP2 Header box without decoding.
//!
//! ## Layout from the component set
//!
//! A JPEG 2000 codestream is a list of components (`SIZ`, T.800 Table
//! A.9), each with its own precision and reference-grid sub-sampling.
//! The contract layout is derived from that list (JP2 channel mapping
//! applied first — palette expansion, `cdef` ordering):
//!
//! | components | sub-sampling | precision | layout |
//! |---|---|---|---|
//! | 1 | any uniform | ≤ 8 / 10 / 12 / 9–16 | `Gray8` / `Gray10Le` / `Gray12Le` / `Gray16Le` |
//! | 2 | uniform | ≤ 8 / 9–16 | `Ya8` / `Ya16Le` |
//! | 3 (RGB signalling) | uniform | ≤ 8 / 9–16 | `Rgb24` / `Rgb48Le` |
//! | 4 (RGB signalling) | uniform | ≤ 8 / 9–16 | `Rgba` / `Rgba64Le` |
//! | 3 (YCC signalling) | uniform | 8 / 10 / 12 / 16 | `Yuv444P` family |
//! | 3 (+ full-res alpha) | chroma 2×2 | 8 / 10 / 12 / 16 | `Yuv420P` / `Yuva420P` families |
//! | 3 (+ full-res alpha) | chroma 2×1 | 8 / 10 / 12 / 16 | `Yuv422P` / `Yuva422P` families |
//! | 3 | chroma 1×2 | 8 / 10 / 12 / 16 | `Yuv440P` family |
//! | 3 | chroma 4×1 | 8 | `Yuv411P` |
//! | 1 index + JP2 palette (≤ 256 entries, 8-bit columns) | — | ≤ 8 | `Pal8` |
//!
//! "Uniform" means every component shares one `(XRsiz, YRsiz)` pair;
//! the image is then the component grid (`width` / `height` are
//! component 0's sample extents). "YCC signalling" is a JP2 `colr`
//! enumerated sYCC (`EnumCS = 18`) or a T.814 parameterized
//! `MatrixCoefficients ≠ 0`; a raw codestream with 1:1 components is
//! RGB. Chroma-sub-sampled files are YCbCr by structure (T.800 J.14);
//! their [`ColorInfo`] is unspecified unless the JP2 header says
//! otherwise, and [`Jpeg2000Image::to_rgb8`] then applies the sYCC
//! (BT.601) matrix at full range, documented in the README.
//!
//! Every other component set — signed samples, precision above 16
//! bits, mixed precisions, five or more components, chroma geometry
//! the core labels do not describe — has no contract layout:
//! [`crate::info`] and [`crate::decode()`] return
//! [`Jpeg2000Error::Unsupported`] naming the set, and the depth API
//! ([`crate::decode_j2k`], [`crate::jp2::decode_jp2`]) reaches the
//! planes as `i32` samples.
//!
//! ## Precision
//!
//! Samples are stored LSB-aligned at their exact decoded value: a 4-bit
//! component rides `Gray8` with `bit_depth = 4`, a 14-bit one rides
//! `Gray16Le` with `bit_depth = 14`; `Gray10Le` / `Gray12Le` and the
//! 10 / 12-bit YUV labels are used when the precision matches exactly.
//! [`Jpeg2000Image::bit_depth`] is the significant-bits count every
//! `to_rgb8` / `to_rgba8` scaling and every encode uses as `Ssiz`.

use crate::error::{Error, Jpeg2000Error};

// ---------------------------------------------------------------------------
// PixelFormat
// ---------------------------------------------------------------------------

/// Pixel layouts the standalone `oxideav-jpeg2000` API produces and
/// accepts. Variant names mirror `oxideav_core::PixelFormat` exactly,
/// so the [`crate::registry`] conversion is a 1:1 match.
///
/// Packed layouts have one plane; the `Yuv*` / `Yuva*` layouts have one
/// plane per component (Y, Cb, Cr[, A]). Deep (`*16Le`, `*10Le`,
/// `*12Le`, `Rgb48Le`, `Rgba64Le`, `Ya16Le`) layouts store each sample
/// as a little-endian 16-bit word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Jpeg2000PixelFormat {
    /// 8-bit grayscale, 1 byte per pixel.
    Gray8,
    /// 10-bit grayscale in little-endian 16-bit words.
    Gray10Le,
    /// 12-bit grayscale in little-endian 16-bit words.
    Gray12Le,
    /// 9–16-bit grayscale in little-endian 16-bit words.
    Gray16Le,
    /// 8-bit grayscale + alpha, 2 bytes per pixel.
    Ya8,
    /// 9–16-bit grayscale + alpha, two little-endian words per pixel.
    Ya16Le,
    /// 8-bit RGB, 3 bytes per pixel.
    Rgb24,
    /// 9–16-bit RGB, three little-endian words per pixel.
    Rgb48Le,
    /// 8-bit RGBA, 4 bytes per pixel.
    Rgba,
    /// 9–16-bit RGBA, four little-endian words per pixel.
    Rgba64Le,
    /// 8-bit palette index; the colour table is
    /// [`Jpeg2000Image::palette`] (JP2 `pclr` + `cmap`).
    Pal8,
    /// 8-bit planar YCbCr, full-resolution chroma.
    Yuv444P,
    /// 10-bit planar YCbCr 4:4:4.
    Yuv444P10Le,
    /// 12-bit planar YCbCr 4:4:4.
    Yuv444P12Le,
    /// 16-bit planar YCbCr 4:4:4 (9–16 significant bits).
    Yuv444P16Le,
    /// 8-bit planar YCbCr, chroma half width.
    Yuv422P,
    /// 10-bit planar YCbCr 4:2:2.
    Yuv422P10Le,
    /// 12-bit planar YCbCr 4:2:2.
    Yuv422P12Le,
    /// 16-bit planar YCbCr 4:2:2.
    Yuv422P16Le,
    /// 8-bit planar YCbCr, chroma half width and half height.
    Yuv420P,
    /// 10-bit planar YCbCr 4:2:0.
    Yuv420P10Le,
    /// 12-bit planar YCbCr 4:2:0.
    Yuv420P12Le,
    /// 16-bit planar YCbCr 4:2:0.
    Yuv420P16Le,
    /// 8-bit planar YCbCr, chroma half height.
    Yuv440P,
    /// 10-bit planar YCbCr 4:4:0.
    Yuv440P10Le,
    /// 12-bit planar YCbCr 4:4:0.
    Yuv440P12Le,
    /// 16-bit planar YCbCr 4:4:0.
    Yuv440P16Le,
    /// 8-bit planar YCbCr, chroma quarter width.
    Yuv411P,
    /// 8-bit planar YCbCr 4:4:4 + full-resolution alpha.
    Yuva444P,
    /// 10-bit planar YCbCr 4:4:4 + alpha.
    Yuva444P10Le,
    /// 12-bit planar YCbCr 4:4:4 + alpha.
    Yuva444P12Le,
    /// 16-bit planar YCbCr 4:4:4 + alpha.
    Yuva444P16Le,
    /// 8-bit planar YCbCr 4:2:2 + full-resolution alpha.
    Yuva422P,
    /// 10-bit planar YCbCr 4:2:2 + alpha.
    Yuva422P10Le,
    /// 12-bit planar YCbCr 4:2:2 + alpha.
    Yuva422P12Le,
    /// 16-bit planar YCbCr 4:2:2 + alpha.
    Yuva422P16Le,
    /// 8-bit planar YCbCr 4:2:0 + full-resolution alpha.
    Yuva420P,
    /// 10-bit planar YCbCr 4:2:0 + alpha.
    Yuva420P10Le,
    /// 12-bit planar YCbCr 4:2:0 + alpha.
    Yuva420P12Le,
    /// 16-bit planar YCbCr 4:2:0 + alpha.
    Yuva420P16Le,
}

/// The contract name for [`Jpeg2000PixelFormat`].
pub type PixelFormat = Jpeg2000PixelFormat;
/// Short alias for [`Jpeg2000PixelFormat`].
pub type J2kPixelFormat = Jpeg2000PixelFormat;

impl Jpeg2000PixelFormat {
    /// Every variant, in declaration order.
    pub const ALL: [Self; 40] = [
        Self::Gray8,
        Self::Gray10Le,
        Self::Gray12Le,
        Self::Gray16Le,
        Self::Ya8,
        Self::Ya16Le,
        Self::Rgb24,
        Self::Rgb48Le,
        Self::Rgba,
        Self::Rgba64Le,
        Self::Pal8,
        Self::Yuv444P,
        Self::Yuv444P10Le,
        Self::Yuv444P12Le,
        Self::Yuv444P16Le,
        Self::Yuv422P,
        Self::Yuv422P10Le,
        Self::Yuv422P12Le,
        Self::Yuv422P16Le,
        Self::Yuv420P,
        Self::Yuv420P10Le,
        Self::Yuv420P12Le,
        Self::Yuv420P16Le,
        Self::Yuv440P,
        Self::Yuv440P10Le,
        Self::Yuv440P12Le,
        Self::Yuv440P16Le,
        Self::Yuv411P,
        Self::Yuva444P,
        Self::Yuva444P10Le,
        Self::Yuva444P12Le,
        Self::Yuva444P16Le,
        Self::Yuva422P,
        Self::Yuva422P10Le,
        Self::Yuva422P12Le,
        Self::Yuva422P16Le,
        Self::Yuva420P,
        Self::Yuva420P10Le,
        Self::Yuva420P12Le,
        Self::Yuva420P16Le,
    ];

    /// Number of components (samples per pixel): 1 for gray / `Pal8`,
    /// 2 for gray + alpha, 3 for RGB / YCbCr, 4 with alpha.
    pub fn components(self) -> usize {
        match self {
            Self::Gray8 | Self::Gray10Le | Self::Gray12Le | Self::Gray16Le | Self::Pal8 => 1,
            Self::Ya8 | Self::Ya16Le => 2,
            Self::Rgb24 | Self::Rgb48Le => 3,
            Self::Rgba | Self::Rgba64Le => 4,
            Self::Yuv444P
            | Self::Yuv444P10Le
            | Self::Yuv444P12Le
            | Self::Yuv444P16Le
            | Self::Yuv422P
            | Self::Yuv422P10Le
            | Self::Yuv422P12Le
            | Self::Yuv422P16Le
            | Self::Yuv420P
            | Self::Yuv420P10Le
            | Self::Yuv420P12Le
            | Self::Yuv420P16Le
            | Self::Yuv440P
            | Self::Yuv440P10Le
            | Self::Yuv440P12Le
            | Self::Yuv440P16Le
            | Self::Yuv411P => 3,
            Self::Yuva444P
            | Self::Yuva444P10Le
            | Self::Yuva444P12Le
            | Self::Yuva444P16Le
            | Self::Yuva422P
            | Self::Yuva422P10Le
            | Self::Yuva422P12Le
            | Self::Yuva422P16Le
            | Self::Yuva420P
            | Self::Yuva420P10Le
            | Self::Yuva420P12Le
            | Self::Yuva420P16Le => 4,
        }
    }

    /// `true` for the planar YCbCr layouts (one plane per component).
    pub fn is_yuv(self) -> bool {
        !matches!(
            self,
            Self::Gray8
                | Self::Gray10Le
                | Self::Gray12Le
                | Self::Gray16Le
                | Self::Ya8
                | Self::Ya16Le
                | Self::Rgb24
                | Self::Rgb48Le
                | Self::Rgba
                | Self::Rgba64Le
                | Self::Pal8
        )
    }

    /// Number of planes: 1 for packed layouts, [`Self::components`] for
    /// the planar YCbCr ones.
    pub fn plane_count(self) -> usize {
        if self.is_yuv() {
            self.components()
        } else {
            1
        }
    }

    /// Storage width of one sample: 8 or 16 bits.
    pub fn storage_bits(self) -> u8 {
        match self {
            Self::Gray8
            | Self::Ya8
            | Self::Rgb24
            | Self::Rgba
            | Self::Pal8
            | Self::Yuv444P
            | Self::Yuv422P
            | Self::Yuv420P
            | Self::Yuv440P
            | Self::Yuv411P
            | Self::Yuva444P
            | Self::Yuva422P
            | Self::Yuva420P => 8,
            _ => 16,
        }
    }

    /// Bytes per sample (1 or 2).
    pub fn bytes_per_sample(self) -> usize {
        usize::from(self.storage_bits() / 8)
    }

    /// Bytes per pixel of a packed layout's single plane; for the
    /// planar YCbCr layouts, bytes per luma sample.
    pub fn bytes_per_pixel(self) -> usize {
        if self.is_yuv() {
            self.bytes_per_sample()
        } else {
            self.components() * self.bytes_per_sample()
        }
    }

    /// The significant-bits count the label pins, when it pins one:
    /// `Some(10)` / `Some(12)` for the `*10Le` / `*12Le` labels, `None`
    /// otherwise (8-bit labels carry 1–8 bits, 16-bit labels 9–16).
    pub fn fixed_bit_depth(self) -> Option<u8> {
        match self {
            Self::Gray10Le
            | Self::Yuv444P10Le
            | Self::Yuv422P10Le
            | Self::Yuv420P10Le
            | Self::Yuv440P10Le
            | Self::Yuva444P10Le
            | Self::Yuva422P10Le
            | Self::Yuva420P10Le => Some(10),
            Self::Gray12Le
            | Self::Yuv444P12Le
            | Self::Yuv422P12Le
            | Self::Yuv420P12Le
            | Self::Yuv440P12Le
            | Self::Yuva444P12Le
            | Self::Yuva422P12Le
            | Self::Yuva420P12Le => Some(12),
            _ => None,
        }
    }

    /// `true` when the layout carries an alpha channel.
    pub fn has_alpha(self) -> bool {
        matches!(
            self,
            Self::Ya8
                | Self::Ya16Le
                | Self::Rgba
                | Self::Rgba64Le
                | Self::Yuva444P
                | Self::Yuva444P10Le
                | Self::Yuva444P12Le
                | Self::Yuva444P16Le
                | Self::Yuva422P
                | Self::Yuva422P10Le
                | Self::Yuva422P12Le
                | Self::Yuva422P16Le
                | Self::Yuva420P
                | Self::Yuva420P10Le
                | Self::Yuva420P12Le
                | Self::Yuva420P16Le
        )
    }

    /// Chroma sub-sampling factors `(horizontal, vertical)` of the
    /// planar YCbCr layouts (`(1, 1)` for 4:4:4 and every packed
    /// layout).
    pub fn chroma_subsampling(self) -> (u8, u8) {
        match self {
            Self::Yuv422P
            | Self::Yuv422P10Le
            | Self::Yuv422P12Le
            | Self::Yuv422P16Le
            | Self::Yuva422P
            | Self::Yuva422P10Le
            | Self::Yuva422P12Le
            | Self::Yuva422P16Le => (2, 1),
            Self::Yuv420P
            | Self::Yuv420P10Le
            | Self::Yuv420P12Le
            | Self::Yuv420P16Le
            | Self::Yuva420P
            | Self::Yuva420P10Le
            | Self::Yuva420P12Le
            | Self::Yuva420P16Le => (2, 2),
            Self::Yuv440P | Self::Yuv440P10Le | Self::Yuv440P12Le | Self::Yuv440P16Le => (1, 2),
            Self::Yuv411P => (4, 1),
            _ => (1, 1),
        }
    }

    /// Sample extents `(width, height)` of plane `plane` for a `width ×
    /// height` image: the luma / packed plane is the full size, chroma
    /// planes are ceiling-divided by [`Self::chroma_subsampling`], the
    /// alpha plane is full size.
    pub fn plane_dimensions(self, plane: usize, width: u32, height: u32) -> (u32, u32) {
        if plane == 1 || plane == 2 {
            let (sx, sy) = self.chroma_subsampling();
            (
                width.div_ceil(u32::from(sx)),
                height.div_ceil(u32::from(sy)),
            )
        } else {
            (width, height)
        }
    }

    /// Tight row size in bytes of plane `plane` for an image `width`
    /// pixels wide.
    pub fn plane_row_bytes(self, plane: usize, width: u32) -> usize {
        let (w, _) = self.plane_dimensions(plane, width, 1);
        (w as usize) * self.bytes_per_pixel()
    }

    /// The sub-sampling pattern `(XRsiz, YRsiz)` of every component of
    /// this layout as a JPEG 2000 encoder writes it into `SIZ`.
    pub fn component_subsampling(self) -> Vec<(u8, u8)> {
        let (sx, sy) = self.chroma_subsampling();
        (0..self.components())
            .map(|c| if c == 1 || c == 2 { (sx, sy) } else { (1, 1) })
            .collect()
    }

    /// The layout a component set maps to: `components` samples per
    /// pixel at `bit_depth` significant bits with `chroma` sub-sampling
    /// of components 1–2 (`(1, 1)` for packed), `ycc` selecting the
    /// YCbCr labels for three / four 1:1 components. `None` when core
    /// has no label for the combination.
    pub fn for_layout(
        components: usize,
        bit_depth: u8,
        chroma: (u8, u8),
        ycc: bool,
    ) -> Option<Self> {
        if bit_depth == 0 || bit_depth > 16 {
            return None;
        }
        let deep = bit_depth > 8;
        if chroma == (1, 1) && !(ycc && (components == 3 || components == 4)) {
            return Some(match (components, deep, bit_depth) {
                (1, false, _) => Self::Gray8,
                (1, true, 10) => Self::Gray10Le,
                (1, true, 12) => Self::Gray12Le,
                (1, true, _) => Self::Gray16Le,
                (2, false, _) => Self::Ya8,
                (2, true, _) => Self::Ya16Le,
                (3, false, _) => Self::Rgb24,
                (3, true, _) => Self::Rgb48Le,
                (4, false, _) => Self::Rgba,
                (4, true, _) => Self::Rgba64Le,
                _ => return None,
            });
        }
        let alpha = match components {
            3 => false,
            4 => true,
            _ => return None,
        };
        // Depth class: 8-bit labels carry 1–8 bits; the 10 / 12 labels
        // are exact; everything else from 9 to 16 rides the 16 label.
        let class = match bit_depth {
            1..=8 => 0,
            10 => 1,
            12 => 2,
            _ => 3,
        };
        let table: [Self; 4] = match (chroma, alpha) {
            ((1, 1), false) => [
                Self::Yuv444P,
                Self::Yuv444P10Le,
                Self::Yuv444P12Le,
                Self::Yuv444P16Le,
            ],
            ((1, 1), true) => [
                Self::Yuva444P,
                Self::Yuva444P10Le,
                Self::Yuva444P12Le,
                Self::Yuva444P16Le,
            ],
            ((2, 1), false) => [
                Self::Yuv422P,
                Self::Yuv422P10Le,
                Self::Yuv422P12Le,
                Self::Yuv422P16Le,
            ],
            ((2, 1), true) => [
                Self::Yuva422P,
                Self::Yuva422P10Le,
                Self::Yuva422P12Le,
                Self::Yuva422P16Le,
            ],
            ((2, 2), false) => [
                Self::Yuv420P,
                Self::Yuv420P10Le,
                Self::Yuv420P12Le,
                Self::Yuv420P16Le,
            ],
            ((2, 2), true) => [
                Self::Yuva420P,
                Self::Yuva420P10Le,
                Self::Yuva420P12Le,
                Self::Yuva420P16Le,
            ],
            ((1, 2), false) => [
                Self::Yuv440P,
                Self::Yuv440P10Le,
                Self::Yuv440P12Le,
                Self::Yuv440P16Le,
            ],
            ((4, 1), false) if class == 0 => return Some(Self::Yuv411P),
            _ => return None,
        };
        Some(table[class])
    }
}

// ---------------------------------------------------------------------------
// Plane / ColorRange / ColorInfo / Metadata / Palette
// ---------------------------------------------------------------------------

/// One pixel plane: `stride` bytes per row, `data` holding at least
/// `stride × (rows − 1) + row_bytes` bytes (rows may carry padding
/// past the visible width).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Plane {
    /// Bytes per row.
    pub stride: usize,
    /// Row-major bytes.
    pub data: Vec<u8>,
}

impl Plane {
    /// Wrap a plane buffer with its row stride.
    pub fn new(stride: usize, data: Vec<u8>) -> Self {
        Self { stride, data }
    }
}

/// Nominal sample range (H.273 `VideoFullRangeFlag`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorRange {
    /// No range was signalled.
    #[default]
    Unspecified,
    /// Limited (video / studio) range: `VideoFullRangeFlag == 0`.
    Limited,
    /// Full (PC) range: `VideoFullRangeFlag == 1`.
    Full,
}

/// Colour signalling of an image: the sample range plus the H.273
/// `ColourPrimaries` / `TransferCharacteristics` /
/// `MatrixCoefficients` code points (`2` = unspecified).
///
/// For JPEG 2000, [`crate::decode()`] fills it from the JP2 Colour
/// Specification box (T.800 §I.5.3.3): enumerated sRGB (`16`) →
/// primaries 1, transfer 13, matrix 0, full range; greyscale (`17`) →
/// primaries 1, transfer 13, matrix 0, full range; sYCC (`18`) →
/// primaries 1, transfer 13, matrix 5, full range (IEC 61966-2-1 Amd 1
/// — the T.871 sYCC code points); a T.814 parameterized box (`METH =
/// 5`) copies its four fields. An ICC-only box leaves the code points
/// unspecified (the profile is on [`Metadata::icc`]). A raw codestream
/// carries no colour information: [`ColorInfo::unspecified`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ColorInfo {
    /// Sample range.
    pub range: ColorRange,
    /// H.273 `ColourPrimaries` code point (`1` = BT.709 / sRGB, `2` =
    /// unspecified).
    pub primaries: u8,
    /// H.273 `TransferCharacteristics` code point (`13` = sRGB, `2` =
    /// unspecified).
    pub transfer: u8,
    /// H.273 `MatrixCoefficients` code point (`0` = identity / RGB,
    /// `5` = BT.601 / sYCC, `2` = unspecified).
    pub matrix: u8,
}

impl ColorInfo {
    /// H.273 "unspecified" code point.
    pub const UNSPECIFIED: u8 = 2;
    /// H.273 `MatrixCoefficients` identity (RGB / GBR) code point.
    pub const MATRIX_IDENTITY: u8 = 0;
    /// H.273 `MatrixCoefficients` BT.601 / sYCC code point.
    pub const MATRIX_BT601: u8 = 5;
    /// H.273 `ColourPrimaries` BT.709 / sRGB code point.
    pub const PRIMARIES_BT709: u8 = 1;
    /// H.273 `TransferCharacteristics` IEC 61966-2-1 sRGB code point.
    pub const TRANSFER_SRGB: u8 = 13;

    /// Build a description from its four parts.
    pub const fn new(range: ColorRange, primaries: u8, transfer: u8, matrix: u8) -> Self {
        Self {
            range,
            primaries,
            transfer,
            matrix,
        }
    }

    /// Every field unspecified — what a raw codestream (no JP2 header)
    /// carries.
    pub const fn unspecified() -> Self {
        Self::new(
            ColorRange::Unspecified,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
        )
    }

    /// sRGB (`EnumCS = 16`, IEC 61966-2-1): BT.709 primaries, sRGB
    /// transfer, identity matrix, full range.
    pub const fn srgb() -> Self {
        Self::new(
            ColorRange::Full,
            Self::PRIMARIES_BT709,
            Self::TRANSFER_SRGB,
            Self::MATRIX_IDENTITY,
        )
    }

    /// sRGB greyscale (`EnumCS = 17`): the sRGB non-linearity on a
    /// single luminance channel — the same code points as
    /// [`Self::srgb`].
    pub const fn srgb_gray() -> Self {
        Self::srgb()
    }

    /// sYCC (`EnumCS = 18`, IEC 61966-2-1 Amd 1): BT.709 primaries,
    /// sRGB transfer, BT.601 matrix, full range.
    pub const fn sycc() -> Self {
        Self::new(
            ColorRange::Full,
            Self::PRIMARIES_BT709,
            Self::TRANSFER_SRGB,
            Self::MATRIX_BT601,
        )
    }

    /// Set the range.
    pub fn with_range(mut self, range: ColorRange) -> Self {
        self.range = range;
        self
    }

    /// Set the primaries code point.
    pub fn with_primaries(mut self, primaries: u8) -> Self {
        self.primaries = primaries;
        self
    }

    /// Set the transfer code point.
    pub fn with_transfer(mut self, transfer: u8) -> Self {
        self.transfer = transfer;
        self
    }

    /// Set the matrix code point.
    pub fn with_matrix(mut self, matrix: u8) -> Self {
        self.matrix = matrix;
        self
    }

    /// `true` when both primaries and transfer are specified (`!= 2`).
    pub fn is_specified(&self) -> bool {
        self.primaries != Self::UNSPECIFIED && self.transfer != Self::UNSPECIFIED
    }

    /// `true` when any field carries information (a non-`2` code point
    /// or a known range).
    pub fn is_signalled(&self) -> bool {
        self.range != ColorRange::Unspecified
            || self.primaries != Self::UNSPECIFIED
            || self.transfer != Self::UNSPECIFIED
            || self.matrix != Self::UNSPECIFIED
    }
}

impl Default for ColorInfo {
    /// [`ColorInfo::unspecified`].
    fn default() -> Self {
        Self::unspecified()
    }
}

/// The metadata blobs every image crate surfaces: an ICC profile, an
/// Exif payload, an XMP packet and a file gamma.
///
/// JPEG 2000 sources `icc` from a JP2 Colour Specification box with
/// `METH = 2` (restricted ICC) or the T.814 `METH = 3` (any ICC).
/// `exif` / `xmp` ride vendor `uuid` boxes (T.800 §I.7.2) whose
/// identifiers are defined outside the staged specifications, so this
/// crate leaves them `None`; `gamma` is `None` (the format carries no
/// single gamma exponent).
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Metadata {
    /// ICC profile bytes (JP2 `colr` `PROFILE` field).
    pub icc: Option<Vec<u8>>,
    /// Exif payload — never filled by this crate (type docs).
    pub exif: Option<Vec<u8>>,
    /// XMP packet — never filled by this crate (type docs).
    pub xmp: Option<Vec<u8>>,
    /// Encoding gamma exponent — never filled by this crate.
    pub gamma: Option<f32>,
}

impl Metadata {
    /// Empty metadata.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or clear) the ICC profile.
    pub fn with_icc(mut self, icc: impl Into<Option<Vec<u8>>>) -> Self {
        self.icc = icc.into();
        self
    }

    /// Set (or clear) the Exif payload.
    pub fn with_exif(mut self, exif: impl Into<Option<Vec<u8>>>) -> Self {
        self.exif = exif.into();
        self
    }

    /// Set (or clear) the XMP packet.
    pub fn with_xmp(mut self, xmp: impl Into<Option<Vec<u8>>>) -> Self {
        self.xmp = xmp.into();
        self
    }

    /// Set (or clear) the gamma.
    pub fn with_gamma(mut self, gamma: impl Into<Option<f32>>) -> Self {
        self.gamma = gamma.into();
        self
    }

    /// `true` when no field is set.
    pub fn is_empty(&self) -> bool {
        self.icc.is_none() && self.exif.is_none() && self.xmp.is_none() && self.gamma.is_none()
    }
}

/// Colour table of an indexed (`Pal8`) image: RGBA entries, index `i`
/// at `entries[i]`. JPEG 2000 builds it from a JP2 Palette box
/// (T.800 §I.5.3.4) with one, three or four 8-bit-or-less unsigned
/// columns (gray replicated into R, G, B; alpha 255 when the box has
/// no fourth column) and at most 256 entries; the encoder writes
/// `pclr` + `cmap` boxes from it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Palette {
    /// `[r, g, b, a]` per entry, at most 256 entries.
    pub entries: Vec<[u8; 4]>,
}

impl Palette {
    /// Wrap a list of RGBA entries.
    pub fn new(entries: Vec<[u8; 4]>) -> Self {
        Self { entries }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when the palette has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entry `index`, if present.
    pub fn get(&self, index: u8) -> Option<[u8; 4]> {
        self.entries.get(usize::from(index)).copied()
    }

    /// `true` when any entry is not fully opaque.
    pub fn has_alpha(&self) -> bool {
        self.entries.iter().any(|e| e[3] != 255)
    }
}

// ---------------------------------------------------------------------------
// RgbImage / RgbaImage
// ---------------------------------------------------------------------------

/// Tightly packed 8-bit RGB image: `width × height × 3` bytes,
/// row-major, channel order `R, G, B`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RgbImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// `width × height × 3` bytes.
    pub data: Vec<u8>,
}

impl RgbImage {
    /// Wrap a tightly packed `width × height × 3` RGB buffer.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data,
        }
    }

    /// The pixel bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume the image and return the pixel bytes.
    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }

    /// Stride (bytes per row) — always `width × 3`.
    pub fn stride(&self) -> usize {
        self.width as usize * 3
    }
}

/// Tightly packed 8-bit RGBA image: `width × height × 4` bytes,
/// row-major, channel order `R, G, B, A` (`A = 255` when the source
/// has no alpha).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RgbaImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// `width × height × 4` bytes.
    pub data: Vec<u8>,
}

impl RgbaImage {
    /// Wrap a tightly packed `width × height × 4` RGBA buffer.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data,
        }
    }

    /// The pixel bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume the image and return the pixel bytes.
    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }

    /// Stride (bytes per row) — always `width × 4`.
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }
}

// ---------------------------------------------------------------------------
// ImageInfo
// ---------------------------------------------------------------------------

/// Header-only description of a JPEG 2000 codestream or JP2 / JPH file
/// ([`crate::info`]): the contract fields plus the codestream shape
/// read from `SIZ` / `COD` and the JP2 Header box.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImageInfo {
    /// Image width in pixels (component 0's sample grid).
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// The layout [`crate::decode()`] would return.
    pub format: PixelFormat,
    /// Always `1` — a JP2 file describes one codestream.
    pub frames: u32,
    /// `true` when the layout carries alpha or the palette has
    /// non-opaque entries.
    pub has_alpha: bool,
    /// Colour signalling from the JP2 Colour Specification box
    /// ([`ColorInfo`] docs).
    pub color: ColorInfo,
    /// `true` when a JP2 `colr` box carries an ICC profile.
    pub has_icc: bool,
    /// Always `false` (see [`Metadata`]).
    pub has_exif: bool,
    /// Always `false` (see [`Metadata`]).
    pub has_xmp: bool,
    /// Significant bits per sample (`Ssiz` precision, 1–16).
    pub bit_depth: u8,
    /// Number of codestream components (`Csiz`), before JP2 palette
    /// expansion.
    pub components: u16,
    /// `true` for a JP2 / JPH file, `false` for a raw codestream.
    pub jp2: bool,
    /// `true` when the codestream signals the T.814 HT block coder
    /// (`Rsiz` bit 14 / `CAP` marker).
    pub high_throughput: bool,
    /// `true` when `COD` selects the reversible 5-3 kernel.
    pub reversible: bool,
    /// Wavelet decomposition levels (`COD` `NL`).
    pub decomposition_levels: u8,
    /// Quality layers (`COD` `SGcod`).
    pub layers: u16,
    /// Number of tiles in the tile grid (T.800 §B.3).
    pub tiles: u32,
    /// The `COD` progression order.
    pub progression: crate::ProgressionOrder,
}

impl ImageInfo {
    /// Assemble a description from its parts; `has_alpha` is derived
    /// from `format` (callers add palette alpha via the field).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        width: u32,
        height: u32,
        format: PixelFormat,
        color: ColorInfo,
        has_icc: bool,
        bit_depth: u8,
        components: u16,
        jp2: bool,
    ) -> Self {
        Self {
            width,
            height,
            format,
            frames: 1,
            has_alpha: format.has_alpha(),
            color,
            has_icc,
            has_exif: false,
            has_xmp: false,
            bit_depth,
            components,
            jp2,
            high_throughput: false,
            reversible: true,
            decomposition_levels: 0,
            layers: 1,
            tiles: 1,
            progression: crate::ProgressionOrder::Lrcp,
        }
    }
}

// ---------------------------------------------------------------------------
// Jpeg2000Image
// ---------------------------------------------------------------------------

/// Decoded JPEG 2000 image in its native layout, as returned by
/// [`crate::decode()`] and consumed by [`crate::encode()`].
///
/// `planes` holds one packed plane for the packed layouts and one
/// plane per component for the `Yuv*` / `Yuva*` layouts (module docs
/// give the derivation). `color` / `metadata` come from the JP2 Header
/// box; `palette` is `Some` for `Pal8`; `bit_depth` is the number of
/// significant bits in every sample (module docs, "Precision").
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Jpeg2000Image {
    /// Image width in pixels (≥ 1).
    pub width: u32,
    /// Image height in pixels (≥ 1).
    pub height: u32,
    /// Native pixel layout.
    pub format: PixelFormat,
    /// Pixel planes — one for packed layouts, one per component for
    /// the planar YCbCr layouts.
    pub planes: Vec<Plane>,
    /// Colour signalling (range + H.273 code points).
    pub color: ColorInfo,
    /// ICC / Exif / XMP / gamma.
    pub metadata: Metadata,
    /// Colour table for `Pal8`.
    pub palette: Option<Palette>,
    /// Significant bits per sample (1–8 for the 8-bit layouts, 9–16
    /// for the 16-bit ones, exactly 10 / 12 for the `*10Le` / `*12Le`
    /// labels). Samples are LSB-aligned at their exact value.
    pub bit_depth: u8,
}

/// Short alias for [`Jpeg2000Image`].
pub type J2kImage = Jpeg2000Image;

impl Jpeg2000Image {
    /// Assemble an image from its geometry, layout and planes. Colour
    /// is [`ColorInfo::unspecified`], metadata empty, no palette, and
    /// `bit_depth` the layout's storage depth (8, 10, 12 or 16); the
    /// `with_*` builders fill those in.
    ///
    /// Returns [`Jpeg2000Error::InvalidData`] when `width` or `height`
    /// is `0`, when the plane count differs from
    /// [`PixelFormat::plane_count`], when a plane's `stride` is below
    /// its row size, or when its `data` is shorter than `stride × (rows
    /// − 1) + row_bytes`; [`Jpeg2000Error::Unsupported`] when the
    /// geometry overflows `usize`.
    pub fn new(
        width: u32,
        height: u32,
        format: PixelFormat,
        planes: Vec<Plane>,
    ) -> Result<Self, Error> {
        Self::check_geometry(width, height, format, &planes)?;
        Ok(Self {
            width,
            height,
            format,
            planes,
            color: ColorInfo::unspecified(),
            metadata: Metadata::default(),
            palette: None,
            bit_depth: format.fixed_bit_depth().unwrap_or(format.storage_bits()),
        })
    }

    /// The [`Self::new`] geometry rules on an existing image (its fields
    /// are public): plane count, strides, plane lengths, plus
    /// `bit_depth` within the layout's range and a palette present for
    /// `Pal8`.
    pub fn validate(&self) -> Result<(), Error> {
        Self::check_geometry(self.width, self.height, self.format, &self.planes)?;
        let bits = self.bit_depth;
        let ok = match self.format.fixed_bit_depth() {
            Some(fixed) => bits == fixed,
            None if self.format.storage_bits() == 8 => (1..=8).contains(&bits),
            None => (9..=16).contains(&bits),
        };
        if !ok {
            return Err(Jpeg2000Error::invalid(format!(
                "bit depth {bits} does not fit {:?}",
                self.format
            )));
        }
        if self.format == PixelFormat::Pal8 && self.palette.is_none() {
            return Err(Jpeg2000Error::invalid("Pal8 image without a palette"));
        }
        Ok(())
    }

    fn check_geometry(
        width: u32,
        height: u32,
        format: PixelFormat,
        planes: &[Plane],
    ) -> Result<(), Error> {
        if width == 0 || height == 0 {
            return Err(Jpeg2000Error::invalid(format!(
                "{width}x{height} image (both dimensions must be > 0)"
            )));
        }
        if planes.len() != format.plane_count() {
            return Err(Jpeg2000Error::invalid(format!(
                "{format:?} needs {} plane(s), got {}",
                format.plane_count(),
                planes.len()
            )));
        }
        for (i, plane) in planes.iter().enumerate() {
            let (pw, ph) = format.plane_dimensions(i, width, height);
            let row_bytes = (pw as usize)
                .checked_mul(format.bytes_per_pixel())
                .ok_or_else(|| Jpeg2000Error::unsupported("row size overflows usize"))?;
            if plane.stride < row_bytes {
                return Err(Jpeg2000Error::invalid(format!(
                    "plane {i}: stride {} below row size {row_bytes}",
                    plane.stride
                )));
            }
            let needed = plane
                .stride
                .checked_mul(ph as usize - 1)
                .and_then(|n| n.checked_add(row_bytes))
                .ok_or_else(|| Jpeg2000Error::unsupported("plane size overflows usize"))?;
            if plane.data.len() < needed {
                return Err(Jpeg2000Error::invalid(format!(
                    "plane {i}: holds {} bytes, geometry needs {needed}",
                    plane.data.len()
                )));
            }
        }
        Ok(())
    }

    /// One tightly packed plane of a packed layout (`stride = width ×
    /// bytes_per_pixel`). Same validation as [`Self::new`]; the planar
    /// YCbCr layouts are rejected with `InvalidData`.
    pub fn packed(
        width: u32,
        height: u32,
        format: PixelFormat,
        data: Vec<u8>,
    ) -> Result<Self, Error> {
        if format.is_yuv() {
            return Err(Jpeg2000Error::invalid(format!(
                "{format:?} is planar; build it with Jpeg2000Image::new"
            )));
        }
        let stride = (width as usize)
            .checked_mul(format.bytes_per_pixel())
            .ok_or_else(|| Jpeg2000Error::unsupported("row size overflows usize"))?;
        Self::new(width, height, format, vec![Plane::new(stride, data)])
    }

    /// Packed `Rgb24` from a tightly packed `width × height × 3`
    /// buffer.
    pub fn from_rgb8(width: u32, height: u32, data: Vec<u8>) -> Result<Self, Error> {
        Self::packed(width, height, PixelFormat::Rgb24, data)
    }

    /// Packed `Rgba` from a tightly packed `width × height × 4` buffer.
    pub fn from_rgba8(width: u32, height: u32, data: Vec<u8>) -> Result<Self, Error> {
        Self::packed(width, height, PixelFormat::Rgba, data)
    }

    /// Set the colour signalling.
    pub fn with_color(mut self, color: ColorInfo) -> Self {
        self.color = color;
        self
    }

    /// Set the metadata.
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Set (or clear) the palette.
    pub fn with_palette(mut self, palette: impl Into<Option<Palette>>) -> Self {
        self.palette = palette.into();
        self
    }

    /// Set the significant-bits count. Returns `InvalidData` when
    /// `bits` is outside the layout's range (1–8 for 8-bit storage,
    /// 9–16 for 16-bit storage, exactly the label's depth for `*10Le`
    /// / `*12Le`).
    pub fn with_bit_depth(mut self, bits: u8) -> Result<Self, Error> {
        let ok = match self.format.fixed_bit_depth() {
            Some(fixed) => bits == fixed,
            None if self.format.storage_bits() == 8 => (1..=8).contains(&bits),
            None => (9..=16).contains(&bits),
        };
        if !ok {
            return Err(Jpeg2000Error::invalid(format!(
                "bit depth {bits} does not fit {:?}",
                self.format
            )));
        }
        self.bit_depth = bits;
        Ok(self)
    }

    /// Image width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Image height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Native pixel layout.
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Number of components (samples per pixel).
    pub fn components(&self) -> usize {
        self.format.components()
    }

    /// Row stride of plane 0 in bytes.
    pub fn stride(&self) -> usize {
        self.planes.first().map_or(0, |p| p.stride)
    }

    /// `true` when the layout or the palette carries alpha.
    pub fn has_alpha(&self) -> bool {
        self.format.has_alpha() || self.palette.as_ref().is_some_and(Palette::has_alpha)
    }

    /// The single plane's bytes for packed layouts (`None` for the
    /// planar YCbCr layouts — use [`Self::into_raw`] or `planes`).
    pub fn as_bytes(&self) -> Option<&[u8]> {
        if self.format.is_yuv() {
            None
        } else {
            self.planes.first().map(|p| p.data.as_slice())
        }
    }

    /// Consume the image: packed layouts return the plane, planar ones
    /// the planes concatenated in order (strides as reported).
    pub fn into_raw(self) -> Vec<u8> {
        let mut planes = self.planes.into_iter();
        let mut out = planes.next().map(|p| p.data).unwrap_or_default();
        for p in planes {
            out.extend_from_slice(&p.data);
        }
        out
    }

    /// Sample `(x, y)` of plane `plane` as its exact LSB-aligned value
    /// (`0..2^bit_depth`), reading one or two bytes per the layout.
    #[inline]
    pub(crate) fn sample(&self, plane: usize, x: usize, y: usize) -> u32 {
        let p = &self.planes[plane];
        let bps = self.format.bytes_per_sample();
        let per_px = if self.format.is_yuv() {
            1
        } else {
            self.format.components()
        };
        let at = y * p.stride + x * bps * per_px;
        if bps == 1 {
            u32::from(p.data[at])
        } else {
            u32::from(u16::from_le_bytes([p.data[at], p.data[at + 1]]))
        }
    }

    /// Scale an exact `bit_depth`-bit value to 8 bits with rounding:
    /// `round(v × 255 / (2^bits − 1))`, the identity at 8 bits.
    #[inline]
    pub(crate) fn to_8bit(v: u32, bits: u8) -> u8 {
        if bits == 8 {
            return v.min(255) as u8;
        }
        let max = (1u32 << bits) - 1;
        let v = v.min(max);
        ((u64::from(v) * 255 + u64::from(max / 2)) / u64::from(max)) as u8
    }

    /// Scaled 8-bit value of component `c` at pixel `(x, y)`. For the
    /// packed layouts this indexes the interleaved plane; for the
    /// planar YCbCr layouts it reads plane `c` at the chroma-reduced
    /// position (nearest / co-sited replication).
    #[inline]
    fn sample8(&self, c: usize, x: usize, y: usize) -> u8 {
        let bits = self.bit_depth;
        if self.format.is_yuv() {
            let (sx, sy) = if c == 1 || c == 2 {
                self.format.chroma_subsampling()
            } else {
                (1, 1)
            };
            Self::to_8bit(
                self.sample(c, x / usize::from(sx), y / usize::from(sy)),
                bits,
            )
        } else {
            let p = &self.planes[0];
            let bps = self.format.bytes_per_sample();
            let at = y * p.stride + (x * self.format.components() + c) * bps;
            let v = if bps == 1 {
                u32::from(p.data[at])
            } else {
                u32::from(u16::from_le_bytes([p.data[at], p.data[at + 1]]))
            };
            Self::to_8bit(v, bits)
        }
    }

    /// Convert one YCbCr triple (8-bit scaled) to RGB with the H.273
    /// matrix on [`Self::color`] (BT.709 = 1, BT.2020 NCL = 9,
    /// otherwise BT.601 — the sYCC / JFIF coefficients) and its range
    /// (limited only when signalled). Fixed-point, 16 fractional bits,
    /// rounded and clamped.
    #[inline]
    fn ycc_to_rgb(&self, y: u8, cb: u8, cr: u8) -> [u8; 3] {
        // (Cr→R, Cb→G, Cr→G, Cb→B) × 65536 from H.273 §8.3 Kr / Kb:
        // BT.601 (0.299, 0.114), BT.709 (0.2126, 0.0722), BT.2020
        // (0.2627, 0.0593).
        let (cr_r, cb_g, cr_g, cb_b): (i64, i64, i64, i64) = match self.color.matrix {
            1 => (103_206, 12_276, 30_679, 121_609),
            9 | 10 => (96_639, 10_784, 37_444, 123_299),
            _ => (91_881, 22_553, 46_802, 116_130),
        };
        let (yy, cb, cr) = if self.color.range == ColorRange::Limited {
            // Y: (Y − 16) × 255 / 219, C: (C − 128) × 255 / 224, kept
            // in the same 16-bit fixed point.
            (
                (i64::from(y) - 16) * 255 * 65_536 / 219,
                (i64::from(cb) - 128) * 255 * 65_536 / 224,
                (i64::from(cr) - 128) * 255 * 65_536 / 224,
            )
        } else {
            (
                i64::from(y) * 65_536,
                (i64::from(cb) - 128) * 65_536,
                (i64::from(cr) - 128) * 65_536,
            )
        };
        let clamp = |v: i64| ((v + 32_768) >> 16).clamp(0, 255) as u8;
        [
            clamp(yy + ((cr * cr_r) >> 16)),
            clamp(yy - ((cb * cb_g) >> 16) - ((cr * cr_g) >> 16)),
            clamp(yy + ((cb * cb_b) >> 16)),
        ]
    }

    /// Write the image as tightly packed 8-bit RGB (`3 × width` bytes
    /// per row). Exact for every native layout: palette entries
    /// expanded, gray replicated, deep samples scaled by
    /// `round(v × 255 / (2^bit_depth − 1))`, YCbCr converted with the
    /// signalled matrix (BT.601 when unspecified) and nearest-sample
    /// chroma replication. Alpha is dropped.
    pub fn to_rgb8(&self) -> Vec<u8> {
        self.convert8(3)
    }

    /// Write the image as tightly packed 8-bit RGBA (`4 × width` bytes
    /// per row); alpha is `255` when the layout has none, the palette
    /// entry's alpha for `Pal8`.
    pub fn to_rgba8(&self) -> Vec<u8> {
        self.convert8(4)
    }

    fn convert8(&self, out_channels: usize) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = Vec::with_capacity(w * h * out_channels);
        let ncomp = self.format.components();
        let fallback_palette = Palette::new(Vec::new());
        let palette = self.palette.as_ref().unwrap_or(&fallback_palette);
        for y in 0..h {
            for x in 0..w {
                let (rgb, a) = match self.format {
                    PixelFormat::Pal8 => {
                        let idx = self.planes[0].data[y * self.planes[0].stride + x];
                        let e = palette.get(idx).unwrap_or([0, 0, 0, 255]);
                        ([e[0], e[1], e[2]], e[3])
                    }
                    f if f.is_yuv() => {
                        let yy = self.sample8(0, x, y);
                        let cb = self.sample8(1, x, y);
                        let cr = self.sample8(2, x, y);
                        let a = if ncomp == 4 {
                            self.sample8(3, x, y)
                        } else {
                            255
                        };
                        (self.ycc_to_rgb(yy, cb, cr), a)
                    }
                    _ => match ncomp {
                        1 => {
                            let g = self.sample8(0, x, y);
                            ([g, g, g], 255)
                        }
                        2 => {
                            let g = self.sample8(0, x, y);
                            ([g, g, g], self.sample8(1, x, y))
                        }
                        3 => (
                            [
                                self.sample8(0, x, y),
                                self.sample8(1, x, y),
                                self.sample8(2, x, y),
                            ],
                            255,
                        ),
                        _ => (
                            [
                                self.sample8(0, x, y),
                                self.sample8(1, x, y),
                                self.sample8(2, x, y),
                            ],
                            self.sample8(3, x, y),
                        ),
                    },
                };
                out.extend_from_slice(&rgb);
                if out_channels == 4 {
                    out.push(a);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_table_matches_module_docs() {
        use Jpeg2000PixelFormat as F;
        assert_eq!(F::for_layout(1, 8, (1, 1), false), Some(F::Gray8));
        assert_eq!(F::for_layout(1, 4, (1, 1), false), Some(F::Gray8));
        assert_eq!(F::for_layout(1, 10, (1, 1), false), Some(F::Gray10Le));
        assert_eq!(F::for_layout(1, 12, (1, 1), false), Some(F::Gray12Le));
        assert_eq!(F::for_layout(1, 14, (1, 1), false), Some(F::Gray16Le));
        assert_eq!(F::for_layout(2, 8, (1, 1), false), Some(F::Ya8));
        assert_eq!(F::for_layout(2, 12, (1, 1), false), Some(F::Ya16Le));
        assert_eq!(F::for_layout(3, 8, (1, 1), false), Some(F::Rgb24));
        assert_eq!(F::for_layout(3, 12, (1, 1), false), Some(F::Rgb48Le));
        assert_eq!(F::for_layout(4, 8, (1, 1), false), Some(F::Rgba));
        assert_eq!(F::for_layout(4, 16, (1, 1), false), Some(F::Rgba64Le));
        assert_eq!(F::for_layout(3, 8, (1, 1), true), Some(F::Yuv444P));
        assert_eq!(F::for_layout(4, 10, (1, 1), true), Some(F::Yuva444P10Le));
        assert_eq!(F::for_layout(3, 8, (2, 2), false), Some(F::Yuv420P));
        assert_eq!(F::for_layout(3, 12, (2, 1), true), Some(F::Yuv422P12Le));
        assert_eq!(F::for_layout(3, 9, (1, 2), false), Some(F::Yuv440P16Le));
        assert_eq!(F::for_layout(3, 8, (4, 1), false), Some(F::Yuv411P));
        assert_eq!(F::for_layout(4, 8, (2, 2), false), Some(F::Yuva420P));
        assert_eq!(F::for_layout(3, 10, (4, 1), false), None);
        assert_eq!(F::for_layout(5, 8, (1, 1), false), None);
        assert_eq!(F::for_layout(1, 17, (1, 1), false), None);
        assert_eq!(F::for_layout(3, 8, (3, 1), false), None);
        assert_eq!(F::for_layout(2, 8, (2, 2), false), None);
        for f in F::ALL.into_iter().filter(|&f| f != F::Pal8) {
            let comps = f.components();
            let ycc = f.is_yuv();
            let depth = f.fixed_bit_depth().unwrap_or(f.storage_bits());
            let chroma = f.chroma_subsampling();
            assert_eq!(F::for_layout(comps, depth, chroma, ycc), Some(f), "{f:?}");
            assert_eq!(f.component_subsampling().len(), comps);
        }
    }

    #[test]
    fn new_validates_geometry() {
        assert!(
            Jpeg2000Image::new(0, 1, PixelFormat::Gray8, vec![Plane::new(1, vec![0])]).is_err()
        );
        assert!(
            Jpeg2000Image::new(2, 1, PixelFormat::Gray8, vec![Plane::new(1, vec![0, 0])]).is_err()
        );
        assert!(
            Jpeg2000Image::new(2, 2, PixelFormat::Gray8, vec![Plane::new(2, vec![0; 3])]).is_err()
        );
        assert!(
            Jpeg2000Image::new(2, 2, PixelFormat::Gray8, vec![Plane::new(3, vec![0; 5])]).is_ok()
        );
        assert!(
            Jpeg2000Image::new(2, 2, PixelFormat::Yuv420P, vec![Plane::new(2, vec![0; 4])])
                .is_err()
        );
        let yuv = Jpeg2000Image::new(
            3,
            3,
            PixelFormat::Yuv420P,
            vec![
                Plane::new(3, vec![0; 9]),
                Plane::new(2, vec![0; 4]),
                Plane::new(2, vec![0; 4]),
            ],
        )
        .expect("4:2:0 ceil geometry");
        assert_eq!(yuv.as_bytes(), None);
        assert_eq!(yuv.into_raw().len(), 17);
        assert!(Jpeg2000Image::packed(2, 2, PixelFormat::Yuv444P, vec![0; 12]).is_err());
        assert!(Jpeg2000Image::from_rgb8(2, 2, vec![0; 11]).is_err());
        let img = Jpeg2000Image::from_rgba8(2, 2, vec![7; 16]).expect("rgba");
        assert_eq!(img.bit_depth, 8);
        assert!(img.clone().with_bit_depth(9).is_err());
        assert!(img.clone().with_bit_depth(5).is_ok());
        let deep = Jpeg2000Image::packed(1, 1, PixelFormat::Gray10Le, vec![0, 0]).expect("g10");
        assert!(deep.clone().with_bit_depth(12).is_err());
        assert!(deep.with_bit_depth(10).is_ok());
    }

    #[test]
    fn to_rgb8_scales_depths_and_expands_palette() {
        // 4-bit gray rides Gray8: 15 → 255, 7 → 119.
        let g4 = Jpeg2000Image::packed(2, 1, PixelFormat::Gray8, vec![15, 7])
            .unwrap()
            .with_bit_depth(4)
            .unwrap();
        assert_eq!(g4.to_rgb8(), vec![255, 255, 255, 119, 119, 119]);
        assert_eq!(g4.to_rgba8(), vec![255, 255, 255, 255, 119, 119, 119, 255]);
        // 12-bit gray: 4095 → 255, 2048 → 128.
        let g12 = Jpeg2000Image::packed(
            2,
            1,
            PixelFormat::Gray12Le,
            [4095u16, 2048]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        )
        .unwrap();
        assert_eq!(g12.to_rgb8(), vec![255, 255, 255, 128, 128, 128]);
        // 16-bit RGBA.
        let rgba16 = Jpeg2000Image::packed(
            1,
            1,
            PixelFormat::Rgba64Le,
            [65535u16, 0, 32768, 65535]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        )
        .unwrap();
        assert_eq!(rgba16.to_rgba8(), vec![255, 0, 128, 255]);
        assert_eq!(rgba16.to_rgb8(), vec![255, 0, 128]);
        // Ya8.
        let ya = Jpeg2000Image::packed(1, 1, PixelFormat::Ya8, vec![10, 20]).unwrap();
        assert_eq!(ya.to_rgba8(), vec![10, 10, 10, 20]);
        // Pal8 with alpha.
        let pal = Jpeg2000Image::packed(3, 1, PixelFormat::Pal8, vec![0, 1, 9])
            .unwrap()
            .with_palette(Palette::new(vec![[1, 2, 3, 255], [4, 5, 6, 7]]));
        assert!(pal.has_alpha());
        assert_eq!(pal.to_rgba8(), vec![1, 2, 3, 255, 4, 5, 6, 7, 0, 0, 0, 255]);
    }

    #[test]
    fn ycc_conversion_matches_sycc_kernel() {
        // Full-range BT.601: Y=128, Cb=Cr=128 → gray 128; pure red
        // (255, 0, 0) is Y=76, Cb=85, Cr=255 in sYCC.
        let img = Jpeg2000Image::new(
            2,
            2,
            PixelFormat::Yuv420P,
            vec![
                Plane::new(2, vec![128, 76, 128, 76]),
                Plane::new(1, vec![128]),
                Plane::new(1, vec![128]),
            ],
        )
        .unwrap();
        let rgb = img.to_rgb8();
        assert_eq!(&rgb[0..3], &[128, 128, 128]);
        let red = Jpeg2000Image::new(
            1,
            1,
            PixelFormat::Yuv444P,
            vec![
                Plane::new(1, vec![76]),
                Plane::new(1, vec![85]),
                Plane::new(1, vec![255]),
            ],
        )
        .unwrap();
        let [r, g, b] = <[u8; 3]>::try_from(red.to_rgb8()).unwrap();
        assert!(r >= 253 && g <= 2 && b <= 2, "{r} {g} {b}");
        // Limited-range BT.709 white: Y=235, C=128 → 255.
        let white = Jpeg2000Image::new(
            1,
            1,
            PixelFormat::Yuv444P,
            vec![
                Plane::new(1, vec![235]),
                Plane::new(1, vec![128]),
                Plane::new(1, vec![128]),
            ],
        )
        .unwrap()
        .with_color(ColorInfo::new(ColorRange::Limited, 1, 1, 1));
        assert_eq!(white.to_rgb8(), vec![255, 255, 255]);
        // Alpha plane of a Yuva layout.
        let yuva = Jpeg2000Image::new(
            1,
            1,
            PixelFormat::Yuva444P,
            vec![
                Plane::new(1, vec![128]),
                Plane::new(1, vec![128]),
                Plane::new(1, vec![128]),
                Plane::new(1, vec![9]),
            ],
        )
        .unwrap();
        assert_eq!(yuva.to_rgba8(), vec![128, 128, 128, 9]);
    }
}
