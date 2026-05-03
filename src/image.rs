//! Crate-local uncompressed image representation.
//!
//! Defined here (rather than reusing `oxideav_core::VideoFrame`) so the
//! crate can be built with the default `registry` feature off — i.e.
//! without depending on `oxideav-core` at all. When the `registry`
//! feature is on the [`crate::registry`] module provides
//! `From<Jpeg2000Image> for oxideav_core::Frame` (and the reverse
//! conversion needed by the encoder side) so the `Decoder` / `Encoder`
//! trait surface still interoperates cleanly.

/// Pixel layout used by [`Jpeg2000Image`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jpeg2000PixelFormat {
    /// 8-bit single-channel grayscale, one plane.
    Gray8,
    /// 8-bit packed RGB, one plane (3 bytes per pixel).
    Rgb24,
    /// 8-bit planar 4:4:4 YCbCr, three planes at full resolution.
    Yuv444P,
    /// 8-bit planar 4:2:2 YCbCr, three planes (chroma at half H).
    Yuv422P,
    /// 8-bit planar 4:2:0 YCbCr, three planes (chroma at half H/V).
    Yuv420P,
}

/// One image plane: row-major bytes plus the row stride in bytes.
#[derive(Debug, Clone)]
pub struct Jpeg2000Plane {
    /// Bytes per row in `data` (may be larger than the logical row width).
    pub stride: usize,
    /// Raw plane bytes, packed `stride` × number of rows.
    pub data: Vec<u8>,
}

/// One decoded JPEG 2000 frame.
///
/// All-`std`, no `oxideav-core` types — the crate's standalone path
/// hands these out directly. The gated [`crate::registry`] module
/// provides a `From<Jpeg2000Image> for oxideav_core::Frame` conversion.
#[derive(Debug, Clone)]
pub struct Jpeg2000Image {
    /// Picture width in pixels.
    pub width: u32,
    /// Picture height in pixels.
    pub height: u32,
    /// Pixel layout. Determines how many planes are expected and how
    /// to interpret each plane's bytes.
    pub pixel_format: Jpeg2000PixelFormat,
    /// One entry per plane (1 for `Gray8` / `Rgb24`, 3 for the planar
    /// YCbCr formats).
    pub planes: Vec<Jpeg2000Plane>,
    /// Optional presentation timestamp. The standalone decode path
    /// always leaves this `None`; the registry-backed `Decoder` impl
    /// fills it in from the `Packet` it consumed.
    pub pts: Option<i64>,
}
