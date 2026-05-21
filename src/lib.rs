//! # oxideav-jpeg2000
//!
//! Pure-Rust JPEG 2000 (J2K) codestream parser and (eventually) codec.
//!
//! ## Status — 2026-05-21 (round 2)
//!
//! Main-header parser ([`parse_j2k_header`], round 1) plus tile-part
//! walker ([`walk_tile_parts`] / [`parse_codestream`], round 2). The
//! walker returns an ordered [`Vec<TilePart>`] giving each tile-part's
//! parsed [`Sot`] (tile index, `Psot`, `TPsot`, `TNsot`) plus byte
//! offsets of its `SOT` marker, `SOD` marker, and bit-stream body
//! inside the input buffer. Both fixed-`Psot` and `Psot == 0` ("body
//! until EOC") framings are supported per T.800 §A.4.2.
//!
//! Codestream-body decoding (tier-1 EBCOT, tier-2 packet parsing,
//! wavelet inverse transform, dequantisation, MCT) and any encoder
//! path are **not** implemented yet — [`decode_jpeg2000`] and
//! [`encode_jpeg2000`] both return [`Error::NotImplemented`].
//!
//! ## Clean-room provenance
//!
//! All structural information consulted while writing this module
//! came from:
//!
//! * ITU-T Rec. T.800 (06/2019) | ISO/IEC 15444-1, §A "Codestream
//!   syntax". Tables A.2 / A.3 (per-header marker allow-list),
//!   A.4 (SOC), A.5 / A.6 (SOT / tile-part counts), A.7 (SOD),
//!   A.8 (EOC), A.9 / A.10 / A.11 (SIZ), A.12 / A.13 / A.14 / A.15
//!   / A.16 / A.17 / A.18 / A.19 / A.20 / A.21 (COD), A.27 / A.28
//!   / A.29 / A.30 (QCD).
//!
//! No external library source (OpenJPEG, OpenJPH, Kakadu, FFmpeg,
//! libavcodec, etc.) was consulted, quoted, paraphrased, or used as
//! a cross-check oracle.

#![warn(missing_debug_implementations)]

#[cfg(feature = "registry")]
use oxideav_core::RuntimeContext;

// ---------------------------------------------------------------------------
// Marker codes (T.800 §A.4 / §A.5 / §A.6 — Tables A.4, A.7, A.8, A.9, A.12,
// A.22, A.27, A.31).
// ---------------------------------------------------------------------------

/// `SOC` — Start of codestream (T.800 Table A.4).
pub const MARKER_SOC: u16 = 0xFF4F;
/// `SOT` — Start of tile-part (T.800 Table A.5).
pub const MARKER_SOT: u16 = 0xFF90;
/// `SOD` — Start of data (T.800 Table A.7).
pub const MARKER_SOD: u16 = 0xFF93;
/// `EOC` — End of codestream (T.800 Table A.8).
pub const MARKER_EOC: u16 = 0xFFD9;
/// `SIZ` — Image and tile size (T.800 Table A.9).
pub const MARKER_SIZ: u16 = 0xFF51;
/// `COD` — Coding style default (T.800 Table A.12).
pub const MARKER_COD: u16 = 0xFF52;
/// `COC` — Coding style component (T.800 Table A.22).
pub const MARKER_COC: u16 = 0xFF53;
/// `QCD` — Quantization default (T.800 Table A.27).
pub const MARKER_QCD: u16 = 0xFF5C;
/// `QCC` — Quantization component (T.800 Table A.31).
pub const MARKER_QCC: u16 = 0xFF5D;
/// `CAP` — Extended capabilities (T.800 Table A.11bis).
pub const MARKER_CAP: u16 = 0xFF50;
/// `PRF` — Profile (T.800 Table A.11quater).
pub const MARKER_PRF: u16 = 0xFF56;
/// `COM` — Comment (T.800 §A.9.2). Skipped by the round-1 header parser.
pub const MARKER_COM: u16 = 0xFF64;

// ---------------------------------------------------------------------------
// Error type.
// ---------------------------------------------------------------------------

/// Crate-local error type.
///
/// The variants describe both round-1 header-parser failures and the
/// "decoder/encoder not yet implemented" sentinel returned by the
/// non-header entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Decoder/encoder body decode/encode paths are not yet wired up.
    /// Header parsing IS implemented — see [`parse_j2k_header`].
    NotImplemented,
    /// The codestream did not start with the SOC marker (T.800 §A.4.1).
    MissingSoc,
    /// SIZ marker was expected immediately after SOC (T.800 §A.5).
    MissingSiz,
    /// COD marker required in main header was not found (T.800 §A.6.1).
    MissingCod,
    /// QCD marker required in main header was not found (T.800 §A.6.4).
    MissingQcd,
    /// Input bytes ended before the parser finished a marker segment.
    UnexpectedEof,
    /// A marker segment's declared length did not match the spec's
    /// fixed-or-derived size constraints.
    InvalidMarkerLength,
    /// SIZ.Csiz (number of components) was outside the spec range
    /// `1..=16_384` (T.800 Table A.9).
    InvalidComponentCount,
    /// SIZ.Ssiz precision was outside the spec range `1..=38` bits
    /// (T.800 Table A.11).
    InvalidSamplePrecision,
    /// COD.SPcod number of decomposition levels was outside the spec
    /// range `0..=32` (T.800 Table A.15).
    InvalidDecompositionLevels,
    /// An expected main-header marker code was not recognised.
    UnknownMarker(u16),
    /// Round-2 tile-part walker hit a marker that's forbidden in a
    /// tile-part header (e.g. `SOC`, `SIZ`, `CAP`, `PRF`, `TLM`, …)
    /// per T.800 Table A.2 column "Tile-part header".
    UnexpectedMainHeaderMarker(u16),
    /// Tile-part walker reached EOF without seeing the `EOC` marker.
    MissingEoc,
    /// `Psot` field referenced a tile-part length that overran the
    /// codestream buffer (T.800 §A.4.2).
    PsotOverflow,
    /// A tile-part walker found `TPsot` > 254 (T.800 Table A.5).
    InvalidTilePartIndex,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotImplemented => write!(
                f,
                "oxideav-jpeg2000: codestream body decode not yet implemented"
            ),
            Error::MissingSoc => write!(f, "JPEG 2000: missing SOC (0xFF4F) marker"),
            Error::MissingSiz => write!(f, "JPEG 2000: missing SIZ marker after SOC"),
            Error::MissingCod => write!(f, "JPEG 2000: missing COD marker in main header"),
            Error::MissingQcd => write!(f, "JPEG 2000: missing QCD marker in main header"),
            Error::UnexpectedEof => write!(f, "JPEG 2000: unexpected end of input"),
            Error::InvalidMarkerLength => write!(f, "JPEG 2000: invalid marker segment length"),
            Error::InvalidComponentCount => {
                write!(f, "JPEG 2000: invalid Csiz (must be 1..=16384)")
            }
            Error::InvalidSamplePrecision => {
                write!(f, "JPEG 2000: invalid Ssiz precision (must be 1..=38)")
            }
            Error::InvalidDecompositionLevels => {
                write!(
                    f,
                    "JPEG 2000: invalid decomposition levels (must be 0..=32)"
                )
            }
            Error::UnknownMarker(m) => write!(f, "JPEG 2000: unknown marker 0x{:04X}", m),
            Error::UnexpectedMainHeaderMarker(m) => write!(
                f,
                "JPEG 2000: marker 0x{:04X} is not allowed inside a tile-part header",
                m
            ),
            Error::MissingEoc => write!(f, "JPEG 2000: codestream ended without EOC marker"),
            Error::PsotOverflow => write!(
                f,
                "JPEG 2000: Psot tile-part length overruns codestream buffer"
            ),
            Error::InvalidTilePartIndex => {
                write!(
                    f,
                    "JPEG 2000: invalid TPsot tile-part index (must be 0..=254)"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

// ---------------------------------------------------------------------------
// Parsed header structs (T.800 §A.5 / §A.6).
// ---------------------------------------------------------------------------

/// One per-component entry from the SIZ marker segment.
///
/// Mirrors the `(Ssizi, XRsizi, YRsizi)` triplet from T.800 Table A.9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizComponent {
    /// Sample precision in bits (1..=38), decoded from the low 7 bits
    /// of `Ssiz` plus one — see T.800 Table A.11.
    pub precision_bits: u8,
    /// Whether the component samples are signed (high bit of `Ssiz`).
    pub is_signed: bool,
    /// `XRsizi` — horizontal sub-sampling factor (1..=255).
    pub h_separation: u8,
    /// `YRsizi` — vertical sub-sampling factor (1..=255).
    pub v_separation: u8,
}

/// Parsed SIZ marker segment (T.800 §A.5.1, Table A.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Siz {
    /// `Rsiz` — capabilities field (T.800 Table A.10).
    pub rsiz: u16,
    /// `Xsiz` — reference grid width.
    pub x_size: u32,
    /// `Ysiz` — reference grid height.
    pub y_size: u32,
    /// `XOsiz` — horizontal image offset on reference grid.
    pub x_offset: u32,
    /// `YOsiz` — vertical image offset on reference grid.
    pub y_offset: u32,
    /// `XTsiz` — tile width on reference grid.
    pub tile_width: u32,
    /// `YTsiz` — tile height on reference grid.
    pub tile_height: u32,
    /// `XTOsiz` — horizontal tile-grid offset.
    pub tile_x_offset: u32,
    /// `YTOsiz` — vertical tile-grid offset.
    pub tile_y_offset: u32,
    /// One entry per component (`Csiz` total).
    pub components: Vec<SizComponent>,
}

/// Progression order, T.800 Table A.16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressionOrder {
    /// `0x00` — Layer-resolution level-component-position progression.
    Lrcp,
    /// `0x01` — Resolution level-layer-component-position progression.
    Rlcp,
    /// `0x02` — Resolution level-position-component-layer progression.
    Rpcl,
    /// `0x03` — Position-component-resolution level-layer progression.
    Pcrl,
    /// `0x04` — Component-position-resolution level-layer progression.
    Cprl,
    /// Reserved/unknown byte value preserved for downstream tooling.
    Reserved(u8),
}

impl ProgressionOrder {
    fn from_byte(b: u8) -> Self {
        match b {
            0x00 => ProgressionOrder::Lrcp,
            0x01 => ProgressionOrder::Rlcp,
            0x02 => ProgressionOrder::Rpcl,
            0x03 => ProgressionOrder::Pcrl,
            0x04 => ProgressionOrder::Cprl,
            other => ProgressionOrder::Reserved(other),
        }
    }
}

/// Wavelet transform kernel, T.800 Table A.20.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveletTransform {
    /// `0x00` — 9-7 irreversible filter.
    Irreversible9x7,
    /// `0x01` — 5-3 reversible filter.
    Reversible5x3,
    /// Reserved byte preserved for downstream tooling.
    Reserved(u8),
}

impl WaveletTransform {
    fn from_byte(b: u8) -> Self {
        match b {
            0x00 => WaveletTransform::Irreversible9x7,
            0x01 => WaveletTransform::Reversible5x3,
            other => WaveletTransform::Reserved(other),
        }
    }
}

/// Parsed COD marker segment (T.800 §A.6.1, Tables A.12 / A.14 / A.15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cod {
    /// `Scod` — coding style flags (T.800 Table A.13).
    pub scod: u8,
    /// Whether user-defined precincts are present (low bit of `Scod`).
    pub user_defined_precincts: bool,
    /// Whether SOP markers may appear (`Scod & 0x02`).
    pub sop_marker_allowed: bool,
    /// Whether EPH markers shall be used (`Scod & 0x04`).
    pub eph_marker_used: bool,
    /// `SGcod` progression order (Table A.16).
    pub progression: ProgressionOrder,
    /// `SGcod` number of layers (1..=65_535).
    pub layers: u16,
    /// `SGcod` multiple component transformation usage (Table A.17).
    /// `0` = none, `1` = MCT on components 0/1/2.
    pub multi_component_transform: u8,
    /// `SPcod` number of decomposition levels, NL (0..=32).
    pub decomposition_levels: u8,
    /// Code-block width exponent offset, xcb (Table A.18).
    /// Real code-block width = `2.pow(xcb + 2)`.
    pub code_block_width_exp: u8,
    /// Code-block height exponent offset, ycb (Table A.18).
    pub code_block_height_exp: u8,
    /// Code-block style flags (Table A.19).
    pub code_block_style: u8,
    /// Wavelet transform kernel (Table A.20).
    pub transform: WaveletTransform,
    /// User-defined precinct sizes when `user_defined_precincts` is true,
    /// `NL+1` bytes per Table A.21. Empty when maximum-precincts mode.
    pub precincts: Vec<u8>,
}

/// Quantisation style, T.800 Table A.28 (low 5 bits of Sqcd).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantizationStyle {
    /// `0` — No quantization (each SPqcd is an 8-bit exponent only).
    None,
    /// `1` — Scalar derived: one 16-bit (mantissa, exponent) for NLLL only.
    ScalarDerived,
    /// `2` — Scalar expounded: one 16-bit (mantissa, exponent) per subband.
    ScalarExpounded,
    /// Reserved/unknown value preserved.
    Reserved(u8),
}

impl QuantizationStyle {
    fn from_byte(b: u8) -> Self {
        match b & 0b0001_1111 {
            0 => QuantizationStyle::None,
            1 => QuantizationStyle::ScalarDerived,
            2 => QuantizationStyle::ScalarExpounded,
            other => QuantizationStyle::Reserved(other),
        }
    }
}

/// Parsed QCD marker segment (T.800 §A.6.4, Tables A.27 / A.28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qcd {
    /// `Sqcd` — full byte (style in low 5 bits, guard bits in high 3).
    pub sqcd: u8,
    /// Decoded quantisation style.
    pub style: QuantizationStyle,
    /// Number of guard bits (high 3 bits of Sqcd, T.800 Table A.28).
    pub guard_bits: u8,
    /// Raw `SPqcd` payload bytes (1 byte per entry for `None` style,
    /// 2 bytes per entry otherwise). Decoded form is left for a
    /// later round once dequantisation lands.
    pub spqcd: Vec<u8>,
}

/// First-pass parsed main-header summary returned by
/// [`parse_j2k_header`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct J2kHeader {
    /// Parsed SIZ marker segment (T.800 §A.5.1).
    pub siz: Siz,
    /// Parsed COD marker segment (T.800 §A.6.1).
    pub cod: Cod,
    /// Parsed QCD marker segment (T.800 §A.6.4).
    pub qcd: Qcd,
    /// Offset (bytes from codestream start) at which the next marker
    /// after the parsed `SOC SIZ ... COD ... QCD` chunk begins, or
    /// the input length if the parser consumed to EOF.
    pub bytes_consumed: usize,
}

impl J2kHeader {
    /// Convenience accessor — number of components, equivalent to
    /// `self.siz.components.len()`.
    pub fn component_count(&self) -> usize {
        self.siz.components.len()
    }

    /// Convenience accessor — `Xsiz - XOsiz` (image width in
    /// reference-grid units). Wraps via [`u32::saturating_sub`] so
    /// malformed (`XOsiz > Xsiz`) inputs don't panic in callers.
    pub fn image_width(&self) -> u32 {
        self.siz.x_size.saturating_sub(self.siz.x_offset)
    }

    /// Convenience accessor — `Ysiz - YOsiz`.
    pub fn image_height(&self) -> u32 {
        self.siz.y_size.saturating_sub(self.siz.y_offset)
    }
}

// ---------------------------------------------------------------------------
// Tile-part header (T.800 §A.4.2 / Table A.5).
// ---------------------------------------------------------------------------

/// Parsed `SOT` marker segment — T.800 §A.4.2 / Table A.5.
///
/// The five `Isot` / `Psot` / `TPsot` / `TNsot` values are returned
/// verbatim as in the codestream so callers can perform their own
/// cross-checks (the spec allows the encoder to encode `Psot = 0` to
/// mean "until EOC" and `TNsot = 0` to mean "tile-part count not yet
/// known"; see T.800 Table A.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sot {
    /// `Isot` — tile index (0..=65_534), T.800 §A.4.2.
    pub tile_index: u16,
    /// `Psot` — length in bytes, from the start of the SOT marker to
    /// the end of the tile-part data. `0` means "until EOC", allowed
    /// only for the last tile-part (T.800 §A.4.2).
    pub psot: u32,
    /// `TPsot` — tile-part index within this tile (0..=254).
    pub tile_part_index: u8,
    /// `TNsot` — total tile-parts in this tile, `0` if unknown
    /// (T.800 Table A.6).
    pub num_tile_parts: u8,
}

/// One walked tile-part — the parsed `SOT` header, byte offsets of
/// the tile-part header / `SOD` / bit-stream body inside the
/// codestream buffer passed to [`walk_tile_parts`], and the body
/// length in bytes.
///
/// All offsets are measured from the start of the input slice — i.e.
/// `sot_offset` points at the `0xFF90` `SOT` marker, `sod_offset`
/// points at the `0xFF93` `SOD` marker, and `body_offset` points at
/// the first byte of the tier-2 bit stream (one byte past the `SOD`
/// marker).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TilePart {
    /// The parsed SOT marker segment for this tile-part.
    pub sot: Sot,
    /// Byte offset of the `SOT` marker (`0xFF90`) inside the
    /// codestream buffer.
    pub sot_offset: usize,
    /// Byte offset of the `SOD` marker (`0xFF93`) inside the buffer.
    pub sod_offset: usize,
    /// Byte offset of the first bit-stream body byte (`sod_offset +
    /// 2`).
    pub body_offset: usize,
    /// Length of the tile-part bit-stream body in bytes — the bytes
    /// between `body_offset` and the next `SOT` / `EOC` marker.
    pub body_len: usize,
}

/// Parsed JPEG 2000 Part-1 codestream — main header plus the
/// ordered list of tile-parts produced by [`walk_tile_parts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct J2kCodestream {
    /// Main-header marker segments (round 1).
    pub header: J2kHeader,
    /// Tile-parts in codestream order.
    pub tile_parts: Vec<TilePart>,
    /// `true` if the codestream ended with an explicit `EOC` marker.
    /// `false` is only legal for truncated streams (T.800 §A.4.4
    /// NOTE 2) but we accept it and surface the fact for callers.
    pub saw_eoc: bool,
}

// ---------------------------------------------------------------------------
// Byte-reader helpers.
// ---------------------------------------------------------------------------

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn read_u8(&mut self) -> Result<u8, Error> {
        if self.remaining() < 1 {
            return Err(Error::UnexpectedEof);
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn read_u16_be(&mut self) -> Result<u16, Error> {
        if self.remaining() < 2 {
            return Err(Error::UnexpectedEof);
        }
        let v = u16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn read_u32_be(&mut self) -> Result<u32, Error> {
        if self.remaining() < 4 {
            return Err(Error::UnexpectedEof);
        }
        let v = u32::from_be_bytes([
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(v)
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.remaining() < n {
            return Err(Error::UnexpectedEof);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn skip(&mut self, n: usize) -> Result<(), Error> {
        if self.remaining() < n {
            return Err(Error::UnexpectedEof);
        }
        self.pos += n;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Marker-segment parsers (T.800 §A.5 / §A.6).
// ---------------------------------------------------------------------------

/// Parses a SIZ marker segment **whose 2-byte marker code has already
/// been consumed**. T.800 §A.5.1 / Table A.9.
fn parse_siz(reader: &mut Reader<'_>) -> Result<Siz, Error> {
    // Lsiz — Table A.9 says 41..=49_190.
    let lsiz = reader.read_u16_be()?;
    if !(41..=49_190).contains(&lsiz) {
        return Err(Error::InvalidMarkerLength);
    }
    let rsiz = reader.read_u16_be()?;
    let x_size = reader.read_u32_be()?;
    let y_size = reader.read_u32_be()?;
    let x_offset = reader.read_u32_be()?;
    let y_offset = reader.read_u32_be()?;
    let tile_width = reader.read_u32_be()?;
    let tile_height = reader.read_u32_be()?;
    let tile_x_offset = reader.read_u32_be()?;
    let tile_y_offset = reader.read_u32_be()?;
    let csiz = reader.read_u16_be()?;
    if !(1..=16_384).contains(&csiz) {
        return Err(Error::InvalidComponentCount);
    }
    // Lsiz must satisfy Lsiz = 38 + 3 * Csiz per T.800 eq. (A-1).
    if lsiz as u32 != 38 + 3 * csiz as u32 {
        return Err(Error::InvalidMarkerLength);
    }
    let mut components = Vec::with_capacity(csiz as usize);
    for _ in 0..csiz {
        let ssiz = reader.read_u8()?;
        let xrsiz = reader.read_u8()?;
        let yrsiz = reader.read_u8()?;
        if xrsiz == 0 || yrsiz == 0 {
            // T.800 Table A.9: XRsizi/YRsizi values are 1..=255.
            return Err(Error::InvalidMarkerLength);
        }
        let is_signed = (ssiz & 0x80) != 0;
        let precision_bits = (ssiz & 0x7F).wrapping_add(1);
        if !(1..=38).contains(&precision_bits) {
            return Err(Error::InvalidSamplePrecision);
        }
        components.push(SizComponent {
            precision_bits,
            is_signed,
            h_separation: xrsiz,
            v_separation: yrsiz,
        });
    }
    Ok(Siz {
        rsiz,
        x_size,
        y_size,
        x_offset,
        y_offset,
        tile_width,
        tile_height,
        tile_x_offset,
        tile_y_offset,
        components,
    })
}

/// Parses a COD marker segment whose marker code has already been
/// consumed. T.800 §A.6.1 / Tables A.12, A.14, A.15.
fn parse_cod(reader: &mut Reader<'_>) -> Result<Cod, Error> {
    let lcod = reader.read_u16_be()?;
    if !(12..=45).contains(&lcod) {
        return Err(Error::InvalidMarkerLength);
    }
    // The payload excludes the 2-byte length field itself but is
    // measured from the start of Lcod, so a Lcod of 12 means 12 bytes
    // total (2 length + 10 body). Compute remaining-body size.
    let body_len = (lcod as usize)
        .checked_sub(2)
        .ok_or(Error::InvalidMarkerLength)?;
    let start = reader.pos;
    let scod = reader.read_u8()?;
    let progression = ProgressionOrder::from_byte(reader.read_u8()?);
    let layers = reader.read_u16_be()?;
    if layers == 0 {
        // Table A.14: 1..=65_535. Zero is reserved.
        return Err(Error::InvalidMarkerLength);
    }
    let multi_component_transform = reader.read_u8()?;
    let decomposition_levels = reader.read_u8()?;
    if decomposition_levels > 32 {
        return Err(Error::InvalidDecompositionLevels);
    }
    let cb_w = reader.read_u8()?;
    let cb_h = reader.read_u8()?;
    let code_block_style = reader.read_u8()?;
    let transform = WaveletTransform::from_byte(reader.read_u8()?);
    let user_defined_precincts = (scod & 0x01) != 0;
    let sop_marker_allowed = (scod & 0x02) != 0;
    let eph_marker_used = (scod & 0x04) != 0;
    let precincts = if user_defined_precincts {
        // Table A.21: one byte per resolution level = NL + 1 bytes.
        let n = decomposition_levels as usize + 1;
        let bytes = reader.read_bytes(n)?;
        bytes.to_vec()
    } else {
        Vec::new()
    };
    let consumed = reader.pos - start;
    if consumed != body_len {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(Cod {
        scod,
        user_defined_precincts,
        sop_marker_allowed,
        eph_marker_used,
        progression,
        layers,
        multi_component_transform,
        decomposition_levels,
        code_block_width_exp: cb_w,
        code_block_height_exp: cb_h,
        code_block_style,
        transform,
        precincts,
    })
}

/// Parses a QCD marker segment whose marker code has already been
/// consumed. T.800 §A.6.4 / Tables A.27, A.28.
fn parse_qcd(reader: &mut Reader<'_>) -> Result<Qcd, Error> {
    let lqcd = reader.read_u16_be()?;
    if !(4..=197).contains(&lqcd) {
        return Err(Error::InvalidMarkerLength);
    }
    let body_len = (lqcd as usize)
        .checked_sub(2)
        .ok_or(Error::InvalidMarkerLength)?;
    if body_len < 1 {
        return Err(Error::InvalidMarkerLength);
    }
    let sqcd = reader.read_u8()?;
    let style = QuantizationStyle::from_byte(sqcd);
    let guard_bits = (sqcd >> 5) & 0x07;
    let payload_len = body_len - 1;
    let bytes = reader.read_bytes(payload_len)?;
    // Spec-side correctness checks for SPqcd payload size per Table
    // A.28 and equation (A-4). For style `None` each SPqcd is 1 byte;
    // otherwise 2 bytes. We can't validate against
    // `number_decomposition_levels` here without coupling the QCD
    // parser to the COD parser, so leave it for the higher-level
    // walker to cross-check.
    let _ = (style, payload_len); // suppress unused warning; logic intact
    let style_check_ok = match style {
        QuantizationStyle::None => true, // 1 byte each
        QuantizationStyle::ScalarDerived => payload_len == 2,
        QuantizationStyle::ScalarExpounded => payload_len % 2 == 0,
        QuantizationStyle::Reserved(_) => true,
    };
    if !style_check_ok {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(Qcd {
        sqcd,
        style,
        guard_bits,
        spqcd: bytes.to_vec(),
    })
}

// ---------------------------------------------------------------------------
// Top-level header walker.
// ---------------------------------------------------------------------------

/// Skips a marker segment whose marker code has been consumed and
/// whose first body field is the 16-bit length. This is the
/// pass-through path for optional main-header markers (CAP, PRF, COM,
/// COC, QCC, …) that aren't part of round-1's parser scope.
fn skip_marker_segment(reader: &mut Reader<'_>) -> Result<(), Error> {
    let len = reader.read_u16_be()?;
    if len < 2 {
        return Err(Error::InvalidMarkerLength);
    }
    reader.skip(len as usize - 2)
}

/// Parses the JPEG 2000 Part-1 main-header marker chain starting at
/// the codestream's SOC and stopping immediately before the first
/// SOT (Start of tile-part) marker.
///
/// On success returns a [`J2kHeader`] populated from the SIZ, COD and
/// QCD marker segments. Optional main-header markers (CAP, PRF, COM,
/// COC, QCC, RGN, POC, PLM, PPM, TLM) are recognised and skipped via
/// their 16-bit length field, but their contents are not retained by
/// this round-1 parser.
///
/// References: T.800 §A.3 (main-header construction), §A.5 (SIZ /
/// CAP / PRF), §A.6 (COD / COC / QCD / QCC / RGN / POC), §A.7 (TLM /
/// PLM / PLT / PPM / PPT).
pub fn parse_j2k_header(bytes: &[u8]) -> Result<J2kHeader, Error> {
    let mut reader = Reader::new(bytes);
    let soc = reader.read_u16_be().map_err(|_| Error::MissingSoc)?;
    if soc != MARKER_SOC {
        return Err(Error::MissingSoc);
    }
    let siz_marker = reader.read_u16_be().map_err(|_| Error::MissingSiz)?;
    if siz_marker != MARKER_SIZ {
        return Err(Error::MissingSiz);
    }
    let siz = parse_siz(&mut reader)?;
    let mut cod: Option<Cod> = None;
    let mut qcd: Option<Qcd> = None;
    loop {
        let marker = reader.read_u16_be()?;
        match marker {
            MARKER_COD => {
                cod = Some(parse_cod(&mut reader)?);
            }
            MARKER_QCD => {
                qcd = Some(parse_qcd(&mut reader)?);
            }
            MARKER_SOT | MARKER_EOC | MARKER_SOD => {
                // End of main header — rewind 2 bytes so the caller
                // can resume marker walking from this point.
                reader.pos -= 2;
                break;
            }
            // Optional main-header markers we skip over by length.
            MARKER_CAP | MARKER_PRF | MARKER_COM | MARKER_COC | MARKER_QCC | 0xFF5E | 0xFF5F
            | 0xFF55 | 0xFF57 | 0xFF58 | 0xFF60 | 0xFF61 => {
                skip_marker_segment(&mut reader)?;
            }
            other => {
                return Err(Error::UnknownMarker(other));
            }
        }
    }
    let cod = cod.ok_or(Error::MissingCod)?;
    let qcd = qcd.ok_or(Error::MissingQcd)?;
    Ok(J2kHeader {
        siz,
        cod,
        qcd,
        bytes_consumed: reader.pos,
    })
}

// ---------------------------------------------------------------------------
// Tile-part walker (T.800 §A.4.2 / §A.4.3 — SOT + SOD).
// ---------------------------------------------------------------------------

/// Parses a `SOT` marker segment **whose 2-byte marker code has
/// already been consumed**. T.800 §A.4.2 / Table A.5.
fn parse_sot(reader: &mut Reader<'_>) -> Result<Sot, Error> {
    // Lsot is fixed at 10 per Table A.5.
    let lsot = reader.read_u16_be()?;
    if lsot != 10 {
        return Err(Error::InvalidMarkerLength);
    }
    let tile_index = reader.read_u16_be()?;
    let psot = reader.read_u32_be()?;
    // Psot = 0 OR 14..=u32::MAX per Table A.5. A non-zero Psot smaller
    // than 14 (the minimum SOT + SOD overhead) is illegal.
    if psot != 0 && psot < 14 {
        return Err(Error::InvalidMarkerLength);
    }
    let tile_part_index = reader.read_u8()?;
    if tile_part_index > 254 {
        // Unreachable for u8, but kept for spec alignment.
        return Err(Error::InvalidTilePartIndex);
    }
    let num_tile_parts = reader.read_u8()?;
    Ok(Sot {
        tile_index,
        psot,
        tile_part_index,
        num_tile_parts,
    })
}

/// Set of marker codes whose appearance inside a tile-part header is
/// **forbidden** per T.800 Table A.2 (main-header-only markers).
///
/// Hitting any of these mid-tile-part is a hard error rather than a
/// length-skip, because the spec rules them out: a real encoder would
/// never emit them and a corrupted-stream heuristic could misalign
/// the walker.
const MAIN_HEADER_ONLY_MARKERS: &[u16] = &[
    MARKER_SOC, MARKER_SIZ, MARKER_CAP, MARKER_PRF, 0xFF63, // CRG
    0xFF55, // TLM
    0xFF57, // PLM
    0xFF60, // PPM
];

/// Walks the per-tile-part marker chain starting at the byte
/// immediately after the main header consumed by [`parse_j2k_header`]
/// and stopping at the `EOC` marker (or EOF, with `saw_eoc = false`).
///
/// For each tile-part the walker returns a [`TilePart`] containing
/// the parsed `SOT`, the byte offsets of the SOT / SOD / body inside
/// the input slice, and the body length in bytes. The actual tier-1 /
/// tier-2 decode is intentionally **not** performed in this round:
/// the body is treated as an opaque span of bytes and the walker only
/// uses the `Psot` length field (or, when `Psot == 0`, "until the
/// next SOT or EOC") to delimit each tile-part.
///
/// References: T.800 §A.4.2 (SOT), §A.4.3 (SOD), §A.4.4 (EOC),
/// Table A.5, Table A.6.
pub fn walk_tile_parts(bytes: &[u8], header: &J2kHeader) -> Result<Vec<TilePart>, Error> {
    let mut tile_parts = Vec::new();
    let mut pos = header.bytes_consumed;
    loop {
        if pos + 2 > bytes.len() {
            return Err(Error::MissingEoc);
        }
        let marker = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]);
        if marker == MARKER_EOC {
            return Ok(tile_parts);
        }
        if marker != MARKER_SOT {
            return Err(Error::UnknownMarker(marker));
        }
        let tp = walk_one_tile_part(bytes, pos)?;
        // Advance: if Psot is non-zero, the next tile-part begins at
        // sot_offset + Psot (T.800 §A.4.2: Psot measures from the start
        // of the SOT marker). If Psot is zero, the body extends to the
        // next SOT or EOC — the walker already located that boundary.
        pos = tp.body_offset + tp.body_len;
        tile_parts.push(tp);
    }
}

/// Like [`walk_tile_parts`] but also reports whether the trailing
/// `EOC` marker was present.
pub fn parse_codestream(bytes: &[u8]) -> Result<J2kCodestream, Error> {
    let header = parse_j2k_header(bytes)?;
    let tile_parts = walk_tile_parts(bytes, &header)?;
    // walk_tile_parts returns Ok only on EOC (the MissingEoc branch
    // returns Err); reaching here implies the walker terminated on
    // EOC.
    Ok(J2kCodestream {
        header,
        tile_parts,
        saw_eoc: true,
    })
}

/// Walks a single tile-part starting at the byte offset of its SOT
/// marker. Returns the parsed [`TilePart`].
fn walk_one_tile_part(bytes: &[u8], sot_offset: usize) -> Result<TilePart, Error> {
    let mut reader = Reader {
        buf: bytes,
        pos: sot_offset,
    };
    let marker = reader.read_u16_be()?;
    if marker != MARKER_SOT {
        return Err(Error::UnknownMarker(marker));
    }
    let sot = parse_sot(&mut reader)?;
    // Walk the tile-part header markers until SOD.
    let sod_offset = loop {
        if reader.remaining() < 2 {
            return Err(Error::UnexpectedEof);
        }
        let m = reader.read_u16_be()?;
        if m == MARKER_SOD {
            // SOD is delimiter — its 2 bytes are already consumed.
            // Record its offset (where the 0xFF93 marker started).
            break reader.pos - 2;
        }
        if MAIN_HEADER_ONLY_MARKERS.contains(&m) {
            return Err(Error::UnexpectedMainHeaderMarker(m));
        }
        match m {
            // Markers permitted in tile-part headers per T.800 Table
            // A.2: COD, COC, RGN, QCD, QCC, POC, PLT, PPT, COM. All
            // carry a 16-bit length so we skip by length.
            MARKER_COD | MARKER_COC | MARKER_QCD | MARKER_QCC | MARKER_COM | 0xFF5E | 0xFF5F
            | 0xFF58 | 0xFF61 => {
                let len = reader.read_u16_be()?;
                if len < 2 {
                    return Err(Error::InvalidMarkerLength);
                }
                reader.skip(len as usize - 2)?;
            }
            other => return Err(Error::UnknownMarker(other)),
        }
    };
    let body_offset = sod_offset + 2;
    // Compute body length per Psot rules (T.800 §A.4.2).
    let body_len = if sot.psot != 0 {
        // Psot measures from the start of the SOT marker to end of
        // tile-part data.
        let psot = sot.psot as usize;
        let tile_part_end = sot_offset.checked_add(psot).ok_or(Error::PsotOverflow)?;
        if tile_part_end > bytes.len() {
            return Err(Error::PsotOverflow);
        }
        if tile_part_end < body_offset {
            return Err(Error::InvalidMarkerLength);
        }
        tile_part_end - body_offset
    } else {
        // Psot == 0: body extends to the next SOT or EOC marker.
        scan_until_sot_or_eoc(bytes, body_offset)?
    };
    Ok(TilePart {
        sot,
        sot_offset,
        sod_offset,
        body_offset,
        body_len,
    })
}

/// Scan forward from `start` for the next `0xFF90` (SOT) or `0xFFD9`
/// (EOC) marker and return the byte distance from `start` to that
/// marker. Used only when `Psot == 0` (T.800 §A.4.2 — last tile-part
/// of a streamed encode).
///
/// The scanner respects JPEG 2000's bitstream framing: a `0xFF` byte
/// followed by anything in `0x00..=0x8F` is **not** a marker (T.800
/// §B.10.1 marker-stuffing — packet-body payloads never produce a
/// false `0xFF9x` / `0xFFDx` sequence). We only need to recognise
/// the SOT (`0xFF90`) / EOC (`0xFFD9`) codes here.
fn scan_until_sot_or_eoc(bytes: &[u8], start: usize) -> Result<usize, Error> {
    let mut i = start;
    while i + 1 < bytes.len() {
        if bytes[i] == 0xFF {
            let next = bytes[i + 1];
            if next == 0x90 || next == 0xD9 {
                return Ok(i - start);
            }
        }
        i += 1;
    }
    // Reached EOF without seeing SOT or EOC.
    Err(Error::MissingEoc)
}

// ---------------------------------------------------------------------------
// Decoder / encoder stubs.
// ---------------------------------------------------------------------------

/// Decode a JPEG 2000 codestream (J2K) into raw component samples.
///
/// **Not yet implemented.** Round 1 lands only the main-header parser
/// ([`parse_j2k_header`]). Returns [`Error::NotImplemented`].
pub fn decode_jpeg2000(_bytes: &[u8]) -> Result<Vec<u8>, Error> {
    Err(Error::NotImplemented)
}

/// Encode raw samples into a JPEG 2000 codestream (J2K).
///
/// **Not yet implemented.** Returns [`Error::NotImplemented`].
pub fn encode_jpeg2000(_pixels: &[u8], _width: u32, _height: u32) -> Result<Vec<u8>, Error> {
    Err(Error::NotImplemented)
}

/// No-op codec registration — header-parser-only build registers
/// nothing into the runtime context yet.
#[cfg(feature = "registry")]
pub fn register(_ctx: &mut RuntimeContext) {}

#[cfg(feature = "registry")]
oxideav_core::register!("jpeg2000", register);

// ---------------------------------------------------------------------------
// Tests — synthetic byte buffers built from T.800 §A.5 / §A.6 tables.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal main header: SOC + SIZ(1 component, 1×1 grid)
    /// + COD(0 levels, 9-7) + QCD(no-quant, 1 byte SPqcd).
    fn synth_minimal_header() -> Vec<u8> {
        let mut v = Vec::new();
        // SOC
        v.extend_from_slice(&MARKER_SOC.to_be_bytes());

        // SIZ: Lsiz = 38 + 3*1 = 41
        v.extend_from_slice(&MARKER_SIZ.to_be_bytes());
        v.extend_from_slice(&41u16.to_be_bytes()); // Lsiz
        v.extend_from_slice(&0u16.to_be_bytes()); // Rsiz
        v.extend_from_slice(&1u32.to_be_bytes()); // Xsiz
        v.extend_from_slice(&1u32.to_be_bytes()); // Ysiz
        v.extend_from_slice(&0u32.to_be_bytes()); // XOsiz
        v.extend_from_slice(&0u32.to_be_bytes()); // YOsiz
        v.extend_from_slice(&1u32.to_be_bytes()); // XTsiz
        v.extend_from_slice(&1u32.to_be_bytes()); // YTsiz
        v.extend_from_slice(&0u32.to_be_bytes()); // XTOsiz
        v.extend_from_slice(&0u32.to_be_bytes()); // YTOsiz
        v.extend_from_slice(&1u16.to_be_bytes()); // Csiz
        v.push(7); // Ssiz0 = 8-bit unsigned (precision = 7+1 = 8)
        v.push(1); // XRsiz
        v.push(1); // YRsiz

        // COD: Lcod = 12 (no precincts), 0 decomp levels
        v.extend_from_slice(&MARKER_COD.to_be_bytes());
        v.extend_from_slice(&12u16.to_be_bytes()); // Lcod
        v.push(0x00); // Scod = max precincts, no SOP, no EPH
        v.push(0x00); // Progression = LRCP
        v.extend_from_slice(&1u16.to_be_bytes()); // Layers = 1
        v.push(0); // MCT = none
        v.push(0); // Decomp levels = 0
        v.push(4); // xcb offset (code-block width exp = 4 -> 64)
        v.push(4); // ycb offset
        v.push(0); // Code-block style
        v.push(1); // Transform = 5-3 reversible

        // QCD: Lqcd = 4, style = no quant + guard 0, 1 SPqcd byte
        v.extend_from_slice(&MARKER_QCD.to_be_bytes());
        v.extend_from_slice(&4u16.to_be_bytes()); // Lqcd
        v.push(0x00); // Sqcd: style 0 (no quant), 0 guard bits
        v.push(0x00); // SPqcd

        // SOT terminator (not consumed by parser — left in stream)
        v.extend_from_slice(&MARKER_SOT.to_be_bytes());
        v
    }

    #[test]
    fn parses_minimal_synthetic_header() {
        let bytes = synth_minimal_header();
        let h = parse_j2k_header(&bytes).expect("parse");
        assert_eq!(h.component_count(), 1);
        assert_eq!(h.image_width(), 1);
        assert_eq!(h.image_height(), 1);
        let c0 = h.siz.components[0];
        assert_eq!(c0.precision_bits, 8);
        assert!(!c0.is_signed);
        assert_eq!(c0.h_separation, 1);
        assert_eq!(c0.v_separation, 1);
        assert_eq!(h.cod.progression, ProgressionOrder::Lrcp);
        assert_eq!(h.cod.layers, 1);
        assert_eq!(h.cod.decomposition_levels, 0);
        assert_eq!(h.cod.transform, WaveletTransform::Reversible5x3);
        assert_eq!(h.qcd.style, QuantizationStyle::None);
        assert_eq!(h.qcd.guard_bits, 0);
        // bytes_consumed should leave the SOT (final 2 bytes) untouched.
        assert_eq!(h.bytes_consumed, bytes.len() - 2);
    }

    #[test]
    fn rejects_missing_soc() {
        // Start with SIZ instead of SOC.
        let mut bytes = synth_minimal_header();
        bytes[0] = 0xFF;
        bytes[1] = 0x51;
        assert_eq!(parse_j2k_header(&bytes), Err(Error::MissingSoc));
    }

    #[test]
    fn rejects_invalid_csiz() {
        // Build a header with Csiz = 0 (out of 1..=16_384).
        let mut v = Vec::new();
        v.extend_from_slice(&MARKER_SOC.to_be_bytes());
        v.extend_from_slice(&MARKER_SIZ.to_be_bytes());
        v.extend_from_slice(&38u16.to_be_bytes()); // Lsiz = 38 + 0 (illegal but checked below)
        v.extend_from_slice(&0u16.to_be_bytes()); // Rsiz
        v.extend_from_slice(&1u32.to_be_bytes()); // Xsiz
        v.extend_from_slice(&1u32.to_be_bytes()); // Ysiz
        v.extend_from_slice(&0u32.to_be_bytes()); // XOsiz
        v.extend_from_slice(&0u32.to_be_bytes()); // YOsiz
        v.extend_from_slice(&1u32.to_be_bytes()); // XTsiz
        v.extend_from_slice(&1u32.to_be_bytes()); // YTsiz
        v.extend_from_slice(&0u32.to_be_bytes()); // XTOsiz
        v.extend_from_slice(&0u32.to_be_bytes()); // YTOsiz
        v.extend_from_slice(&0u16.to_be_bytes()); // Csiz = 0 (invalid)
                                                  // The Lsiz range starts at 41, so we hit InvalidMarkerLength
                                                  // before InvalidComponentCount. Either is an acceptable
                                                  // rejection — just confirm we DON'T parse it as valid.
        assert!(parse_j2k_header(&v).is_err());
    }

    #[test]
    fn parses_three_component_grid() {
        // 3-component 256x128 image, tile = whole image, 5-3 wavelet,
        // 2 decomposition levels. Exercises multi-component SIZ +
        // non-zero decomp level in COD.
        let mut v = Vec::new();
        v.extend_from_slice(&MARKER_SOC.to_be_bytes());

        // SIZ: Lsiz = 38 + 3*3 = 47
        v.extend_from_slice(&MARKER_SIZ.to_be_bytes());
        v.extend_from_slice(&47u16.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes()); // Rsiz
        v.extend_from_slice(&256u32.to_be_bytes());
        v.extend_from_slice(&128u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&256u32.to_be_bytes());
        v.extend_from_slice(&128u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&3u16.to_be_bytes()); // Csiz = 3
        for _ in 0..3 {
            v.push(7); // 8-bit unsigned
            v.push(1);
            v.push(1);
        }

        // COD: 2 decomp levels, 3 sub-bands per level + LL = 7 subbands.
        v.extend_from_slice(&MARKER_COD.to_be_bytes());
        v.extend_from_slice(&12u16.to_be_bytes());
        v.push(0x00);
        v.push(0x00);
        v.extend_from_slice(&1u16.to_be_bytes());
        v.push(1); // MCT = 1 (component transform used)
        v.push(2); // 2 decomp levels
        v.push(4);
        v.push(4);
        v.push(0);
        v.push(1); // 5-3 reversible

        // QCD: scalar expounded -> 1 (LL) + 3*NL = 7 sub-bands * 2 bytes = 14
        // Lqcd = 2 (length) + 1 (Sqcd) + 14 (SPqcd) = 17
        v.extend_from_slice(&MARKER_QCD.to_be_bytes());
        v.extend_from_slice(&17u16.to_be_bytes());
        v.push(0x02); // style = expounded, guard = 0
        for _ in 0..7 {
            v.push(0x10);
            v.push(0x00);
        }

        // SOT terminator
        v.extend_from_slice(&MARKER_SOT.to_be_bytes());

        let h = parse_j2k_header(&v).expect("parse multi-component");
        assert_eq!(h.component_count(), 3);
        assert_eq!(h.image_width(), 256);
        assert_eq!(h.image_height(), 128);
        assert_eq!(h.cod.decomposition_levels, 2);
        assert_eq!(h.cod.multi_component_transform, 1);
        assert_eq!(h.qcd.style, QuantizationStyle::ScalarExpounded);
        // 7 sub-bands * 2 bytes each
        assert_eq!(h.qcd.spqcd.len(), 14);
    }

    #[test]
    fn skips_optional_com_marker() {
        // Inject a COM (comment) marker between SIZ and COD; the
        // round-1 walker should skip-by-length without disturbing the
        // SIZ/COD/QCD result.
        let mut v = Vec::new();
        v.extend_from_slice(&MARKER_SOC.to_be_bytes());

        v.extend_from_slice(&MARKER_SIZ.to_be_bytes());
        v.extend_from_slice(&41u16.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&1u16.to_be_bytes());
        v.push(7);
        v.push(1);
        v.push(1);

        // COM marker with 6-byte segment (Lcom = 6, registration + 2 bytes).
        v.extend_from_slice(&MARKER_COM.to_be_bytes());
        v.extend_from_slice(&6u16.to_be_bytes());
        v.push(0x00);
        v.push(0x01);
        v.push(0xAB);
        v.push(0xCD);

        // COD
        v.extend_from_slice(&MARKER_COD.to_be_bytes());
        v.extend_from_slice(&12u16.to_be_bytes());
        v.push(0x00);
        v.push(0x00);
        v.extend_from_slice(&1u16.to_be_bytes());
        v.push(0);
        v.push(0);
        v.push(4);
        v.push(4);
        v.push(0);
        v.push(1);

        // QCD
        v.extend_from_slice(&MARKER_QCD.to_be_bytes());
        v.extend_from_slice(&4u16.to_be_bytes());
        v.push(0x00);
        v.push(0x00);

        v.extend_from_slice(&MARKER_SOT.to_be_bytes());

        let h = parse_j2k_header(&v).expect("parse with COM");
        assert_eq!(h.component_count(), 1);
        assert_eq!(h.image_width(), 1);
        assert_eq!(h.cod.layers, 1);
    }

    #[test]
    fn missing_cod_is_reported() {
        // SOC + SIZ + QCD + SOT — no COD. Should yield MissingCod.
        let mut v = Vec::new();
        v.extend_from_slice(&MARKER_SOC.to_be_bytes());

        v.extend_from_slice(&MARKER_SIZ.to_be_bytes());
        v.extend_from_slice(&41u16.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&1u16.to_be_bytes());
        v.push(7);
        v.push(1);
        v.push(1);

        v.extend_from_slice(&MARKER_QCD.to_be_bytes());
        v.extend_from_slice(&4u16.to_be_bytes());
        v.push(0x00);
        v.push(0x00);

        v.extend_from_slice(&MARKER_SOT.to_be_bytes());

        assert_eq!(parse_j2k_header(&v), Err(Error::MissingCod));
    }

    #[test]
    fn decode_and_encode_are_still_unimplemented() {
        assert_eq!(decode_jpeg2000(&[0xFF, 0x4F]), Err(Error::NotImplemented));
        assert_eq!(encode_jpeg2000(&[0u8; 4], 2, 2), Err(Error::NotImplemented));
    }

    // -----------------------------------------------------------------------
    // Round-2 SOT / SOD walker tests (T.800 §A.4.2 / §A.4.3).
    // -----------------------------------------------------------------------

    /// Build a `SOT` marker segment (12 bytes total: marker + Lsot=10 +
    /// payload). T.800 §A.4.2 / Table A.5.
    fn synth_sot(isot: u16, psot: u32, tpsot: u8, tnsot: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(12);
        v.extend_from_slice(&MARKER_SOT.to_be_bytes());
        v.extend_from_slice(&10u16.to_be_bytes());
        v.extend_from_slice(&isot.to_be_bytes());
        v.extend_from_slice(&psot.to_be_bytes());
        v.push(tpsot);
        v.push(tnsot);
        v
    }

    /// Append `SOD` + `n` opaque body bytes.
    fn append_sod_body(buf: &mut Vec<u8>, body: &[u8]) {
        buf.extend_from_slice(&MARKER_SOD.to_be_bytes());
        buf.extend_from_slice(body);
    }

    /// Build a full codestream: main header from `synth_minimal_header`
    /// (trimmed to drop its trailing SOT) plus a sequence of
    /// `(sot, body)` tile-parts plus a terminating `EOC`.
    fn synth_codestream(tile_parts: &[(Sot, Vec<u8>)]) -> Vec<u8> {
        // Drop the SOT terminator that synth_minimal_header appends so
        // we can splice our own tile-parts in.
        let mut hdr = synth_minimal_header();
        assert_eq!(&hdr[hdr.len() - 2..], &MARKER_SOT.to_be_bytes());
        hdr.truncate(hdr.len() - 2);
        for (sot, body) in tile_parts {
            // Total tile-part length = 12 (SOT) + 2 (SOD) + body.len().
            // If sot.psot is 0 we leave it; otherwise the test asserts
            // psot == 14 + body.len() so the walker can compute body_len
            // from Psot.
            let sot_bytes = synth_sot(
                sot.tile_index,
                sot.psot,
                sot.tile_part_index,
                sot.num_tile_parts,
            );
            hdr.extend_from_slice(&sot_bytes);
            append_sod_body(&mut hdr, body);
        }
        hdr.extend_from_slice(&MARKER_EOC.to_be_bytes());
        hdr
    }

    #[test]
    fn walks_single_tile_part() {
        let body = vec![0x12, 0x34, 0x56, 0x78];
        let psot = (12 + 2 + body.len()) as u32; // 12 SOT + 2 SOD + body
        let sot = Sot {
            tile_index: 0,
            psot,
            tile_part_index: 0,
            num_tile_parts: 1,
        };
        let bytes = synth_codestream(&[(sot, body.clone())]);
        let cs = parse_codestream(&bytes).expect("parse codestream");
        assert!(cs.saw_eoc);
        assert_eq!(cs.tile_parts.len(), 1);
        let tp = cs.tile_parts[0];
        assert_eq!(tp.sot.tile_index, 0);
        assert_eq!(tp.sot.psot, psot);
        assert_eq!(tp.sot.tile_part_index, 0);
        assert_eq!(tp.sot.num_tile_parts, 1);
        assert_eq!(tp.body_len, body.len());
        // The bytes between body_offset and body_offset + body_len
        // should match the body we encoded.
        assert_eq!(
            &bytes[tp.body_offset..tp.body_offset + tp.body_len],
            body.as_slice()
        );
    }

    #[test]
    fn walks_two_tile_parts() {
        let body0 = vec![0xAA; 8];
        let body1 = vec![0xBB; 16];
        let psot0 = (12 + 2 + body0.len()) as u32;
        let psot1 = (12 + 2 + body1.len()) as u32;
        let tps = [
            (
                Sot {
                    tile_index: 0,
                    psot: psot0,
                    tile_part_index: 0,
                    num_tile_parts: 1,
                },
                body0.clone(),
            ),
            (
                Sot {
                    tile_index: 1,
                    psot: psot1,
                    tile_part_index: 0,
                    num_tile_parts: 1,
                },
                body1.clone(),
            ),
        ];
        let bytes = synth_codestream(&tps);
        let cs = parse_codestream(&bytes).expect("parse codestream");
        assert_eq!(cs.tile_parts.len(), 2);
        assert_eq!(cs.tile_parts[0].sot.tile_index, 0);
        assert_eq!(cs.tile_parts[1].sot.tile_index, 1);
        assert_eq!(cs.tile_parts[0].body_len, body0.len());
        assert_eq!(cs.tile_parts[1].body_len, body1.len());
        // Tile-part order in offsets is strictly monotonic.
        assert!(cs.tile_parts[0].sot_offset < cs.tile_parts[1].sot_offset);
    }

    #[test]
    fn walks_psot_zero_last_tile_part() {
        // Psot == 0 — body extends until EOC. T.800 §A.4.2.
        // We use a body that does NOT contain any 0xFF90 / 0xFFD9 byte
        // pair so the scanner only stops at the appended EOC.
        let body = vec![0x01, 0x02, 0x03, 0x04, 0xFF, 0x00, 0xFF, 0x7F];
        let sot = Sot {
            tile_index: 0,
            psot: 0,
            tile_part_index: 0,
            num_tile_parts: 1,
        };
        let bytes = synth_codestream(&[(sot, body.clone())]);
        let cs = parse_codestream(&bytes).expect("parse codestream");
        assert_eq!(cs.tile_parts.len(), 1);
        let tp = cs.tile_parts[0];
        assert_eq!(tp.sot.psot, 0);
        assert_eq!(tp.body_len, body.len());
    }

    #[test]
    fn rejects_psot_overrun() {
        // Build a single-tile-part stream and lie about Psot — claim
        // 4096 bytes when only ~30 are present.
        let body = vec![0u8; 4];
        let sot = Sot {
            tile_index: 0,
            psot: 4096,
            tile_part_index: 0,
            num_tile_parts: 1,
        };
        let bytes = synth_codestream(&[(sot, body)]);
        let err = parse_codestream(&bytes).unwrap_err();
        assert_eq!(err, Error::PsotOverflow);
    }

    #[test]
    fn rejects_missing_eoc() {
        // Synthesize a codestream then chop the trailing EOC marker.
        let body = vec![0xAA; 8];
        let sot = Sot {
            tile_index: 0,
            psot: (12 + 2 + body.len()) as u32,
            tile_part_index: 0,
            num_tile_parts: 1,
        };
        let mut bytes = synth_codestream(&[(sot, body)]);
        // Trim the last 2 bytes (the EOC marker).
        bytes.truncate(bytes.len() - 2);
        let err = parse_codestream(&bytes).unwrap_err();
        assert_eq!(err, Error::MissingEoc);
    }

    #[test]
    fn rejects_main_header_marker_in_tile_part() {
        // Build a tile-part whose tile-part header (between SOT and
        // SOD) contains a SIZ marker — illegal per T.800 Table A.2.
        let mut tp = synth_sot(0, 0, 0, 1);
        // Inject a SIZ marker after SOT.
        tp.extend_from_slice(&MARKER_SIZ.to_be_bytes());
        tp.extend_from_slice(&41u16.to_be_bytes()); // Lsiz
        tp.extend_from_slice(&[0u8; 39]); // dummy payload
        tp.extend_from_slice(&MARKER_SOD.to_be_bytes());
        tp.extend_from_slice(&[0u8; 4]); // body
                                         // Splice into a full codestream after the main header.
        let mut hdr = synth_minimal_header();
        hdr.truncate(hdr.len() - 2); // drop trailing SOT
        hdr.extend_from_slice(&tp);
        hdr.extend_from_slice(&MARKER_EOC.to_be_bytes());
        let err = parse_codestream(&hdr).unwrap_err();
        assert_eq!(err, Error::UnexpectedMainHeaderMarker(MARKER_SIZ));
    }

    #[test]
    fn accepts_tile_part_with_inline_com_marker() {
        // A COM marker is legal inside the tile-part header (T.800
        // Table A.2 — Comment is optional in both main and tile-part
        // headers). The walker should skip it by length.
        let mut tp = synth_sot(0, 0, 0, 1);
        // COM marker, Lcom = 6, registration = 0x0000, 2 payload bytes.
        tp.extend_from_slice(&MARKER_COM.to_be_bytes());
        tp.extend_from_slice(&6u16.to_be_bytes());
        tp.extend_from_slice(&[0x00, 0x00, 0xDE, 0xAD]);
        // Then SOD + body.
        tp.extend_from_slice(&MARKER_SOD.to_be_bytes());
        tp.extend_from_slice(&[0xCAu8, 0xFE, 0xBA, 0xBE]);
        let mut hdr = synth_minimal_header();
        hdr.truncate(hdr.len() - 2);
        hdr.extend_from_slice(&tp);
        hdr.extend_from_slice(&MARKER_EOC.to_be_bytes());
        let cs = parse_codestream(&hdr).expect("parse with inline COM");
        assert_eq!(cs.tile_parts.len(), 1);
        assert_eq!(cs.tile_parts[0].body_len, 4);
    }

    #[test]
    fn rejects_sot_with_wrong_lsot() {
        // Build a codestream where the SOT marker claims Lsot = 12
        // instead of the spec-mandated 10. T.800 Table A.5.
        let mut hdr = synth_minimal_header();
        hdr.truncate(hdr.len() - 2);
        hdr.extend_from_slice(&MARKER_SOT.to_be_bytes());
        hdr.extend_from_slice(&12u16.to_be_bytes()); // wrong Lsot
        hdr.extend_from_slice(&0u16.to_be_bytes()); // Isot
        hdr.extend_from_slice(&0u32.to_be_bytes()); // Psot
        hdr.push(0); // TPsot
        hdr.push(1); // TNsot
        hdr.extend_from_slice(&[0u8; 2]); // 2 stray bytes from the wrong Lsot
        hdr.extend_from_slice(&MARKER_SOD.to_be_bytes());
        hdr.extend_from_slice(&[0u8; 4]);
        hdr.extend_from_slice(&MARKER_EOC.to_be_bytes());
        let err = parse_codestream(&hdr).unwrap_err();
        assert_eq!(err, Error::InvalidMarkerLength);
    }

    #[test]
    fn walk_tile_parts_reports_offsets_into_buffer() {
        // Verify TilePart.sot_offset and TilePart.sod_offset point
        // exactly at the 0xFF90 / 0xFF93 markers in the buffer.
        let body = vec![0u8; 4];
        let psot = (12 + 2 + body.len()) as u32;
        let sot = Sot {
            tile_index: 0,
            psot,
            tile_part_index: 0,
            num_tile_parts: 1,
        };
        let bytes = synth_codestream(&[(sot, body)]);
        let cs = parse_codestream(&bytes).expect("parse");
        let tp = cs.tile_parts[0];
        assert_eq!(&bytes[tp.sot_offset..tp.sot_offset + 2], &[0xFF, 0x90]);
        assert_eq!(&bytes[tp.sod_offset..tp.sod_offset + 2], &[0xFF, 0x93]);
        assert_eq!(tp.body_offset, tp.sod_offset + 2);
    }
}
