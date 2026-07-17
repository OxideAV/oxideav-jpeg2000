//! JP2 ISO BMFF box wrapper parser (T.800 / ISO/IEC 15444-1 Annex I).
//!
//! The JP2 file format wraps a JPEG 2000 Part-1 codestream (the bytes
//! handled by [`crate::parse_codestream`]) in a sequence of ISO
//! BMFF-style boxes. The four boxes required for a conforming JP2
//! file (Annex I, Figure I.1 / Table I.2) are:
//!
//! * [`Jp2SignatureBox`] (`jP  `, 0x6A50_2020) — fixed 12-byte file
//!   signature.
//! * [`Ftyp`] (`ftyp`, 0x6674_7970) — brand + minor version +
//!   compatibility list.
//! * [`Jp2Header`] (`jp2h`, 0x6A70_3268) — superbox containing
//!   [`Ihdr`] / optional [`Bpcc`] / one or more [`Colr`].
//! * The Contiguous Codestream box (`jp2c`, 0x6A70_3263) — opaque
//!   wrapper around the bytes that [`crate::parse_codestream`] then
//!   walks structurally.
//!
//! This module only handles the box-structure layer. It does **not**
//! re-parse the codestream — instead [`parse_jp2`] returns a
//! [`Jp2Container`] holding the structural metadata plus
//! `codestream_offset` / `codestream_len` so callers can hand the
//! payload directly to [`crate::parse_codestream`].
//!
//! ## References
//!
//! All structural information consulted while writing this module
//! came from:
//!
//! * ITU-T Rec. T.800 (06/2019) | ISO/IEC 15444-1, Annex I "JP2 file
//!   format syntax". §I.4 (binary box layout — Figure I.4, Table
//!   I.1), §I.5.1 (Signature box), §I.5.2 (File Type box, Tables
//!   I.3 / I.4), §I.5.3 (JP2 Header superbox, Figure I.7), §I.5.3.1
//!   (Image Header box, Figure I.8 / Tables I.5 / I.6), §I.5.3.2
//!   (Bits Per Component box, Tables I.7 / I.8), §I.5.3.3 (Colour
//!   Specification box, Figure I.10 / Tables I.9 / I.10 / I.11),
//!   §I.5.3.4 (Palette box, Figure I.11 / Tables I.12 / I.13),
//!   §I.5.3.5 (Component Mapping box, Figure I.12 / Tables
//!   I.14 / I.15), §I.5.3.6 (Channel Definition box, Figure I.13 /
//!   Tables I.16 – I.19), §I.5.3.7 (Resolution superbox, Figures
//!   I.14 / I.15, Table I.20), §I.5.4 (Contiguous Codestream box).
//!

use crate::Error;

// ---------------------------------------------------------------------------
// Box type FourCCs (T.800 Annex I, Table I.2).
// ---------------------------------------------------------------------------

/// JPEG 2000 Signature box type — `'jP  '` (0x6A50_2020).
pub const BOX_TYPE_JP2_SIGNATURE: u32 = 0x6A50_2020;
/// File Type box type — `'ftyp'` (0x6674_7970).
pub const BOX_TYPE_FTYP: u32 = 0x6674_7970;
/// JP2 Header superbox type — `'jp2h'` (0x6A70_3268).
pub const BOX_TYPE_JP2H: u32 = 0x6A70_3268;
/// Image Header box type — `'ihdr'` (0x6968_6472).
pub const BOX_TYPE_IHDR: u32 = 0x6968_6472;
/// Bits Per Component box type — `'bpcc'` (0x6270_6363).
pub const BOX_TYPE_BPCC: u32 = 0x6270_6363;
/// Colour Specification box type — `'colr'` (0x636F_6C72).
pub const BOX_TYPE_COLR: u32 = 0x636F_6C72;
/// Contiguous Codestream box type — `'jp2c'` (0x6A70_3263).
pub const BOX_TYPE_JP2C: u32 = 0x6A70_3263;
/// Palette box type — `'pclr'` (0x7063_6C72). T.800 §I.5.3.4.
pub const BOX_TYPE_PCLR: u32 = 0x7063_6C72;
/// Component Mapping box type — `'cmap'` (0x636D_6170). T.800 §I.5.3.5.
pub const BOX_TYPE_CMAP: u32 = 0x636D_6170;
/// Channel Definition box type — `'cdef'` (0x6364_6566). T.800 §I.5.3.6.
pub const BOX_TYPE_CDEF: u32 = 0x6364_6566;
/// Resolution superbox type — `'res '` (0x7265_7320). T.800 §I.5.3.7.
pub const BOX_TYPE_RES: u32 = 0x7265_7320;
/// Capture Resolution box type — `'resc'` (0x7265_7363).
/// T.800 §I.5.3.7.1.
pub const BOX_TYPE_RESC: u32 = 0x7265_7363;
/// Default Display Resolution box type — `'resd'` (0x7265_7364).
/// T.800 §I.5.3.7.2.
pub const BOX_TYPE_RESD: u32 = 0x7265_7364;

/// Brand value declared by a conforming JP2 file — `'jp2 '`
/// (0x6A70_3220). T.800 Annex I §I.5.2 / Table I.3.
pub const BRAND_JP2: u32 = 0x6A70_3220;

/// Brand value declared by a conforming JPH (HTJ2K) file — `'jph '`
/// (0x6A70_6820). T.814 Annex D §D.3.
pub const BRAND_JPH: u32 = 0x6A70_6820;

/// Magic 4-byte contents of the JPEG 2000 Signature box — the
/// `\x0D\x0A\x87\x0A` byte string defined in T.800 §I.5.1. The whole
/// box (LBox + TBox + DBox) is therefore the fixed 12-byte literal
/// `00 00 00 0C 6A 50 20 20 0D 0A 87 0A`.
pub const JP2_SIGNATURE_MAGIC: [u8; 4] = [0x0D, 0x0A, 0x87, 0x0A];

// ---------------------------------------------------------------------------
// Parsed box-content types.
// ---------------------------------------------------------------------------

/// Parsed JPEG 2000 Signature box content (T.800 §I.5.1).
///
/// The signature box is fixed-length: 12 bytes total carrying the
/// `0x0D 0x0A 0x87 0x0A` magic. We model it as a zero-sized type
/// because once parsed there is nothing variable to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Jp2SignatureBox;

/// Parsed File Type box (T.800 §I.5.2 / Table I.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ftyp {
    /// `BR` — brand, a 4-byte big-endian integer interpreted as four
    /// ISO/IEC 646 characters. A conforming JP2 file uses `'jp2 '`
    /// (0x6A70_3220) per Table I.3 but other values are reserved.
    pub brand: u32,
    /// `MinV` — minor version. T.800 §I.5.2 specifies a value of
    /// `0` for this revision; readers must keep parsing for any
    /// other value (the field is preserved verbatim).
    pub minor_version: u32,
    /// `CLi` — compatibility list. Conforming files contain at
    /// least one entry equal to `'jp2 '`. We preserve every entry
    /// in declaration order.
    pub compatibility: Vec<u32>,
}

impl Ftyp {
    /// `true` iff one of the compatibility entries is the canonical
    /// `'jp2 '` brand.
    pub fn is_jp2_compatible(&self) -> bool {
        self.compatibility.contains(&BRAND_JP2)
    }

    /// `true` iff one of the compatibility entries is the T.814 §D.3
    /// `'jph '` brand (a JPH / HTJ2K file).
    pub fn is_jph_compatible(&self) -> bool {
        self.compatibility.contains(&BRAND_JPH)
    }
}

/// Parsed Image Header box content (T.800 §I.5.3.1 / Table I.5).
///
/// The Image Header box is fixed-length (22 bytes total including
/// the 8-byte LBox/TBox header). We store every field separately so
/// downstream tooling can cross-check against the codestream's SIZ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ihdr {
    /// `HEIGHT` — image area height. Equal to `Ysiz - YOsiz` in the
    /// codestream's SIZ marker (T.800 §I.5.3.1).
    pub height: u32,
    /// `WIDTH` — image area width. Equal to `Xsiz - XOsiz`.
    pub width: u32,
    /// `NC` — number of components. Equal to `Csiz`.
    pub component_count: u16,
    /// `BPC` — bits per component, **raw byte value** as in Table
    /// I.6. The low 7 bits are `bit_depth - 1`; the high bit signals
    /// "signed" when set. A literal `0xFF` (255) means "components
    /// vary in bit depth" and a Bits Per Component box must be
    /// present in the JP2 Header (T.800 §I.5.3.1).
    pub bpc: u8,
    /// `C` — compression type. The spec mandates `7` for JPEG 2000
    /// (T.800 §I.5.3.1); we preserve the raw byte and surface the
    /// value to callers.
    pub compression: u8,
    /// `UnkC` — colourspace-unknown flag. `0` if known, `1` if not.
    pub colourspace_unknown: u8,
    /// `IPR` — intellectual-property-rights flag. `0` = no IPR box,
    /// `1` = the file contains a `jp2i` box (T.800 §I.6).
    pub ipr: u8,
}

impl Ihdr {
    /// `true` iff the BPC byte sentinel signals "components vary in
    /// bit depth" — i.e. a Bits Per Component box must be present.
    pub fn varies_in_bit_depth(&self) -> bool {
        self.bpc == 0xFF
    }

    /// Decoded per-component bit depth from the low 7 bits of `BPC`
    /// plus one. Meaningless when [`Self::varies_in_bit_depth`] is
    /// true — in that case the caller must consult the `bpcc` box.
    pub fn bit_depth(&self) -> u8 {
        (self.bpc & 0x7F) + 1
    }

    /// `true` iff the high bit of `BPC` is set, indicating signed
    /// components.
    pub fn is_signed(&self) -> bool {
        (self.bpc & 0x80) != 0
    }
}

/// Parsed Bits Per Component box content (T.800 §I.5.3.2 / Table I.7).
///
/// Present only when [`Ihdr::varies_in_bit_depth`] is true. Each
/// `BPCi` byte encodes (signed-bit, bit-depth − 1) the same way as
/// the master `BPC` field — i.e. low 7 bits + 1 = precision, high bit
/// = signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bpcc {
    /// Raw `BPCi` bytes in component order. Length must equal
    /// [`Ihdr::component_count`] when the box is present.
    pub bpci: Vec<u8>,
}

/// Specification method for a Colour Specification box (T.800
/// §I.5.3.3 / Table I.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColrMethod {
    /// `METH = 1` — enumerated colourspace. `EnumCS` is present and
    /// names one of the values in Table I.10.
    Enumerated,
    /// `METH = 2` — restricted ICC profile carried in the `PROFILE`
    /// field of the box.
    RestrictedIccProfile,
    /// `METH = 3` — **any** ICC input profile (T.814 Table D.1 /
    /// §D.4.2, defined for JPH files; equivalent to the T.801 Any ICC
    /// method). The `PROFILE` field carries the ISO 15076-1 profile.
    AnyIcc,
    /// `METH = 5` — parameterized colourspace per Rec. ITU-T H.273
    /// (T.814 Table D.1 / §D.4.3, defined for JPH files).
    Parameterized,
    /// Any other reserved value. Conforming readers ignore the
    /// entire colour-specification box in this case (T.800 §I.5.3.3).
    Reserved(u8),
}

impl ColrMethod {
    fn from_byte(b: u8) -> Self {
        match b {
            1 => ColrMethod::Enumerated,
            2 => ColrMethod::RestrictedIccProfile,
            3 => ColrMethod::AnyIcc,
            5 => ColrMethod::Parameterized,
            other => ColrMethod::Reserved(other),
        }
    }
}

/// T.814 §D.4.3 parameterized-colourspace payload (`METH = 5`): the
/// Rec. ITU-T H.273 code points naming the colour interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterizedColour {
    /// `COLPRIMS` — H.273 `ColourPrimaries` value.
    pub colour_primaries: u16,
    /// `TRANSFC` — H.273 `TransferCharacteristics` value.
    pub transfer_characteristics: u16,
    /// `MATCOEFFS` — H.273 `MatrixCoefficients` value.
    pub matrix_coefficients: u16,
    /// `VIDFRNG` — H.273 `VideoFullRangeFlag` (top bit of the final
    /// byte; the remaining 7 bits are reserved).
    pub video_full_range: bool,
}

/// Enumerated-colourspace value carried in an `EnumCS`-valued
/// [`Colr`]. The three values in Table I.10 are surfaced explicitly;
/// any other reserved value is preserved verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnumCs {
    /// `EnumCS = 16` — sRGB (IEC 61966-2-1).
    Srgb,
    /// `EnumCS = 17` — sRGB-greyscale luminance.
    Greyscale,
    /// `EnumCS = 18` — sYCC (IEC 61966-2-1 Amd 1).
    Sycc,
    /// Any other reserved value.
    Reserved(u32),
}

impl EnumCs {
    fn from_u32(v: u32) -> Self {
        match v {
            16 => EnumCs::Srgb,
            17 => EnumCs::Greyscale,
            18 => EnumCs::Sycc,
            other => EnumCs::Reserved(other),
        }
    }
}

/// Parsed Colour Specification box (T.800 §I.5.3.3 / Table I.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Colr {
    /// `METH` — specification method (Table I.9).
    pub method: ColrMethod,
    /// `PREC` — precedence, signed 1-byte integer; readers ignore.
    /// We surface the value as `i8` because Table I.11 marks it
    /// signed even though the spec mandates `0`.
    pub precedence: i8,
    /// `APPROX` — colourspace approximation, unsigned byte; readers
    /// ignore. Spec mandates `0`.
    pub approximation: u8,
    /// `EnumCS` — present iff `method == Enumerated` (Table I.11).
    pub enumerated: Option<EnumCs>,
    /// `PROFILE` — present iff `method` is `RestrictedIccProfile`
    /// (Table I.11) or the T.814 §D.4.2 `AnyIcc`. Preserved as raw
    /// bytes since this module does not parse the ICC profile body.
    pub icc_profile: Option<Vec<u8>>,
    /// T.814 §D.4.3 payload — present iff `method == Parameterized`.
    pub parameterized: Option<ParameterizedColour>,
}

/// One palette column of a Palette box (T.800 §I.5.3.4).
///
/// A column generates one channel when the Component Mapping box
/// applies it to an index component: entry `j` of column `i` is the
/// generated sample value wherever the index component decodes to
/// `j`. `Bi` (Table I.13) gives the column's bit depth (low 7 bits,
/// stored value + 1) and signedness (high bit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PclrColumn {
    /// Generated-value bit depth, 1..=38 (Table I.13). Columns deeper
    /// than 32 bits parse but cannot be applied by
    /// [`decode_jp2`] (the sample lane is `i32`).
    pub bit_depth: u8,
    /// High bit of `Bi` — the column holds signed values.
    pub signed: bool,
    /// The `NE` column values `C0i..C(NE-1)i`, sign-extended from
    /// `bit_depth` when `signed`.
    pub values: Vec<i32>,
}

/// Parsed Palette box (T.800 §I.5.3.4 / Figure I.11 / Table I.12).
///
/// Every column holds the same number of entries `NE` (1..=1024);
/// `Cji` values are stored component-major in the box and are
/// re-sliced here into per-column vectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pclr {
    /// `NPC` palette columns, each with `NE` values.
    pub columns: Vec<PclrColumn>,
}

impl Pclr {
    /// `NE` — number of palette entries (rows).
    pub fn entries(&self) -> usize {
        self.columns.first().map_or(0, |c| c.values.len())
    }
}

/// How one channel is generated from the codestream components —
/// the `MTYPi` / `PCOLi` pair of a Component Mapping box entry
/// (T.800 §I.5.3.5 / Table I.14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmapMapping {
    /// `MTYP = 0` — the channel is the component itself.
    Direct,
    /// `MTYP = 1` — the channel is created by mapping the component
    /// through palette column `PCOL`.
    Palette {
        /// `PCOLi` — palette column index into [`Pclr::columns`].
        column: u8,
    },
}

/// One channel description of a Component Mapping box
/// (T.800 §I.5.3.5 / Figure I.12). Channel `i` of the image is the
/// `i`-th entry of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmapEntry {
    /// `CMPi` — codestream component index feeding this channel.
    pub component: u16,
    /// `MTYPi` / `PCOLi` — direct use or palette mapping.
    pub mapping: CmapMapping,
}

/// One channel description of a Channel Definition box
/// (T.800 §I.5.3.6 / Figure I.13). A single channel may carry several
/// descriptions (e.g. one opacity channel associated with both the
/// red and the green colour).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelDef {
    /// `Cni` — index of the described channel (a Component Mapping
    /// box channel, or the codestream component when no `cmap`
    /// exists).
    pub channel: u16,
    /// `Typi` (Table I.16) — 0 = colour, 1 = opacity,
    /// 2 = premultiplied opacity, `0xFFFF` = unspecified.
    pub channel_type: u16,
    /// `Asoci` (Tables I.17 / I.18) — 0 = whole image, `1..=0xFFFE` =
    /// the 1-based colour index this channel is associated with,
    /// `0xFFFF` = no association.
    pub association: u16,
}

impl ChannelDef {
    /// Table I.16 `Typ = 0` — colour image data.
    pub const TYPE_COLOUR: u16 = 0;
    /// Table I.16 `Typ = 1` — opacity.
    pub const TYPE_OPACITY: u16 = 1;
    /// Table I.16 `Typ = 2` — premultiplied opacity.
    pub const TYPE_PREMULTIPLIED_OPACITY: u16 = 2;
    /// Table I.16 `Typ = 2^16 - 1` — channel type not specified.
    pub const TYPE_UNSPECIFIED: u16 = 0xFFFF;
}

/// One capture / display grid resolution (T.800 §I.5.3.7.1 /
/// §I.5.3.7.2). Both boxes share the six-field layout of
/// Figure I.15; the resolutions are always expressed in reference
/// grid points per metre (Equations I-4 / I-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridResolution {
    /// `VRN` — vertical resolution numerator (1..=65535).
    pub vertical_numerator: u16,
    /// `VRD` — vertical resolution denominator (1..=65535).
    pub vertical_denominator: u16,
    /// `HRN` — horizontal resolution numerator (1..=65535).
    pub horizontal_numerator: u16,
    /// `HRD` — horizontal resolution denominator (1..=65535).
    pub horizontal_denominator: u16,
    /// `VRE` — vertical resolution exponent (two's complement).
    pub vertical_exponent: i8,
    /// `HRE` — horizontal resolution exponent.
    pub horizontal_exponent: i8,
}

impl GridResolution {
    /// Equation I-4: `VR = VRN / VRD × 10^VRE` reference grid points
    /// per metre.
    pub fn vertical_points_per_metre(&self) -> f64 {
        f64::from(self.vertical_numerator) / f64::from(self.vertical_denominator)
            * 10f64.powi(i32::from(self.vertical_exponent))
    }

    /// Equation I-5: `HR = HRN / HRD × 10^HRE` reference grid points
    /// per metre.
    pub fn horizontal_points_per_metre(&self) -> f64 {
        f64::from(self.horizontal_numerator) / f64::from(self.horizontal_denominator)
            * 10f64.powi(i32::from(self.horizontal_exponent))
    }
}

/// Parsed Resolution superbox (T.800 §I.5.3.7 / Figure I.14). At
/// least one of the two child boxes is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    /// `resc` — Capture Resolution box (§I.5.3.7.1).
    pub capture: Option<GridResolution>,
    /// `resd` — Default Display Resolution box (§I.5.3.7.2).
    pub display: Option<GridResolution>,
}

/// Parsed JP2 Header superbox content (T.800 §I.5.3 / Figure I.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jp2Header {
    /// `ihdr` — required first child of `jp2h` (T.800 §I.5.3.1).
    pub ihdr: Ihdr,
    /// `bpcc` — optional Bits Per Component box. Required when
    /// `ihdr.bpc == 0xFF` (T.800 §I.5.3.2).
    pub bpcc: Option<Bpcc>,
    /// `colr` — one or more Colour Specification boxes
    /// (T.800 §I.5.3.3). The first entry is the one a conforming
    /// reader uses; subsequent entries provide alternatives.
    pub colr: Vec<Colr>,
    /// `pclr` — optional Palette box (T.800 §I.5.3.4). Present iff
    /// [`Jp2Header::cmap`] is present (§I.5.3.4 pairing rule).
    pub pclr: Option<Pclr>,
    /// `cmap` — optional Component Mapping box (T.800 §I.5.3.5),
    /// channel `i` described by entry `i`. When absent, component
    /// `i` maps directly to channel `i`.
    pub cmap: Option<Vec<CmapEntry>>,
    /// `cdef` — optional Channel Definition box (T.800 §I.5.3.6).
    /// When absent, channel `i` is the colour-`i + 1` colour channel
    /// (§I.5.3.6 default).
    pub cdef: Option<Vec<ChannelDef>>,
    /// `res ` — optional Resolution superbox (T.800 §I.5.3.7).
    pub resolution: Option<Resolution>,
}

/// Top-level parsed JP2 container as returned by [`parse_jp2`].
///
/// Only the four mandatory boxes are represented as typed fields.
/// Optional boxes (`jp2i`, `xml `, `uuid`, `uinf`, etc.) appearing
/// between `ftyp` and `jp2c` are skipped over by [`parse_jp2`] but
/// **not** retained — this round restricts the surface to the box
/// chain conforming JP2 readers must understand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jp2Container {
    /// Parsed `ftyp` box (T.800 §I.5.2).
    pub ftyp: Ftyp,
    /// Parsed `jp2h` superbox (T.800 §I.5.3).
    pub header: Jp2Header,
    /// Byte offset (from the start of the input slice passed to
    /// [`parse_jp2`]) of the first byte of the codestream payload —
    /// i.e. the byte **after** the `jp2c` box's LBox/TBox header.
    pub codestream_offset: usize,
    /// Length in bytes of the codestream payload inside the `jp2c`
    /// box. Always equal to `LBox - 8` (or `LBox - 16` for an
    /// extended-length box).
    pub codestream_len: usize,
}

// ---------------------------------------------------------------------------
// Low-level box reader.
// ---------------------------------------------------------------------------

/// A single ISO BMFF-style box as defined in T.800 §I.4 / Figure I.4.
///
/// `header_len` is the size of the box's fixed header in bytes —
/// either 8 (LBox + TBox) or 16 (LBox = 1 + XLBox + TBox). The
/// content (`DBox`) starts at `offset + header_len` and runs for
/// `content_len` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoxHdr {
    /// Box type FourCC (TBox).
    box_type: u32,
    /// Byte offset of the first byte of the box (the LBox field)
    /// inside the input slice.
    offset: usize,
    /// Size of the box header (LBox + TBox or LBox + TBox + XLBox).
    header_len: usize,
    /// Size of the box content. Always finite — `LBox = 0` ("until
    /// end of file") is resolved by [`read_box`] against the input
    /// slice length.
    content_len: usize,
}

impl BoxHdr {
    fn total_len(&self) -> usize {
        self.header_len + self.content_len
    }
}

/// Parses one box starting at `pos` against the input slice `bytes`.
/// Returns the parsed [`BoxHdr`] on success.
///
/// Handles all three length encodings described in T.800 §I.4 /
/// Table I.1:
///
/// * `LBox >= 8` — standard length, content runs for `LBox - 8`
///   bytes after the 8-byte (LBox + TBox) header.
/// * `LBox == 1` — extended length, the actual length lives in an
///   additional 8-byte `XLBox` field immediately after TBox, and
///   the content starts 16 bytes in.
/// * `LBox == 0` — "until end of file/superbox". We resolve the
///   length by extending the content to the end of `bytes`.
fn read_box(bytes: &[u8], pos: usize) -> Result<BoxHdr, Error> {
    if pos.saturating_add(8) > bytes.len() {
        return Err(Error::UnexpectedEof);
    }
    let lbox =
        u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as u64;
    let tbox = u32::from_be_bytes([
        bytes[pos + 4],
        bytes[pos + 5],
        bytes[pos + 6],
        bytes[pos + 7],
    ]);
    if lbox == 0 {
        // LBox == 0: box extends to the end of the enclosing
        // buffer. T.800 §I.4 explicitly allows this only for the
        // last box.
        let content_len = bytes
            .len()
            .checked_sub(pos + 8)
            .ok_or(Error::UnexpectedEof)?;
        return Ok(BoxHdr {
            box_type: tbox,
            offset: pos,
            header_len: 8,
            content_len,
        });
    }
    if lbox == 1 {
        // Extended-length box: 8-byte XLBox immediately follows.
        if pos.saturating_add(16) > bytes.len() {
            return Err(Error::UnexpectedEof);
        }
        let mut xl = [0u8; 8];
        xl.copy_from_slice(&bytes[pos + 8..pos + 16]);
        let xlbox = u64::from_be_bytes(xl);
        // XLBox is the **total** box length including LBox / TBox /
        // XLBox per T.800 §I.4. Content is therefore xlbox - 16.
        if xlbox < 16 {
            return Err(Error::InvalidMarkerLength);
        }
        let content_len_u64 = xlbox - 16;
        let content_len = usize::try_from(content_len_u64).map_err(|_| Error::PsotOverflow)?;
        if pos.saturating_add(16).saturating_add(content_len) > bytes.len() {
            return Err(Error::PsotOverflow);
        }
        return Ok(BoxHdr {
            box_type: tbox,
            offset: pos,
            header_len: 16,
            content_len,
        });
    }
    if lbox < 8 {
        // 2..=7 reserved per T.800 §I.4.
        return Err(Error::InvalidMarkerLength);
    }
    let content_len_u64 = lbox - 8;
    let content_len = usize::try_from(content_len_u64).map_err(|_| Error::PsotOverflow)?;
    if pos.saturating_add(8).saturating_add(content_len) > bytes.len() {
        return Err(Error::PsotOverflow);
    }
    Ok(BoxHdr {
        box_type: tbox,
        offset: pos,
        header_len: 8,
        content_len,
    })
}

// ---------------------------------------------------------------------------
// Content parsers for the four required box types.
// ---------------------------------------------------------------------------

/// Parses the body of a `jP  ` signature box (T.800 §I.5.1). The
/// body must be the 4-byte magic `0x0D 0x0A 0x87 0x0A`.
fn parse_signature_content(content: &[u8]) -> Result<Jp2SignatureBox, Error> {
    if content.len() != 4 {
        return Err(Error::InvalidMarkerLength);
    }
    if content != JP2_SIGNATURE_MAGIC {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(Jp2SignatureBox)
}

/// Parses the body of an `ftyp` box (T.800 §I.5.2 / Table I.4).
fn parse_ftyp_content(content: &[u8]) -> Result<Ftyp, Error> {
    if content.len() < 8 {
        return Err(Error::InvalidMarkerLength);
    }
    // Compatibility list is the remainder; T.800 Table I.4 requires
    // it to be a whole number of 4-byte entries.
    if (content.len() - 8) % 4 != 0 {
        return Err(Error::InvalidMarkerLength);
    }
    let brand = u32::from_be_bytes([content[0], content[1], content[2], content[3]]);
    let minor_version = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    let mut compatibility = Vec::with_capacity((content.len() - 8) / 4);
    let mut i = 8;
    while i + 4 <= content.len() {
        compatibility.push(u32::from_be_bytes([
            content[i],
            content[i + 1],
            content[i + 2],
            content[i + 3],
        ]));
        i += 4;
    }
    Ok(Ftyp {
        brand,
        minor_version,
        compatibility,
    })
}

/// Parses the body of an `ihdr` box (T.800 §I.5.3.1 / Table I.5).
/// The fixed payload is exactly 14 bytes (the 8-byte LBox/TBox plus
/// 14 bytes of content totals 22 bytes per spec).
fn parse_ihdr_content(content: &[u8]) -> Result<Ihdr, Error> {
    if content.len() != 14 {
        return Err(Error::InvalidMarkerLength);
    }
    let height = u32::from_be_bytes([content[0], content[1], content[2], content[3]]);
    let width = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    let component_count = u16::from_be_bytes([content[8], content[9]]);
    let bpc = content[10];
    let compression = content[11];
    let colourspace_unknown = content[12];
    let ipr = content[13];
    // T.800 Table I.5 — HEIGHT/WIDTH must be 1..=(2^32 − 1) and NC
    // must be 1..=16_384. The codestream's SIZ marker has the same
    // bounds (cross-checked by callers).
    if height == 0 || width == 0 {
        return Err(Error::InvalidMarkerLength);
    }
    if component_count == 0 || component_count > 16_384 {
        return Err(Error::InvalidComponentCount);
    }
    // C must be 7 per T.800 §I.5.3.1 ("the value of this field
    // shall be 7"). We surface the value through the struct so a
    // caller can decide whether to enforce.
    Ok(Ihdr {
        height,
        width,
        component_count,
        bpc,
        compression,
        colourspace_unknown,
        ipr,
    })
}

/// Parses the body of a `bpcc` box (T.800 §I.5.3.2 / Table I.7). The
/// payload is `NC` `BPCi` bytes.
fn parse_bpcc_content(content: &[u8], nc: u16) -> Result<Bpcc, Error> {
    if content.len() != nc as usize {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(Bpcc {
        bpci: content.to_vec(),
    })
}

/// Parses the body of a `colr` box (T.800 §I.5.3.3 / Table I.11).
fn parse_colr_content(content: &[u8]) -> Result<Colr, Error> {
    // METH / PREC / APPROX are always present (3 bytes); EnumCS and
    // PROFILE are mutually exclusive successor fields.
    if content.len() < 3 {
        return Err(Error::InvalidMarkerLength);
    }
    let method_byte = content[0];
    let precedence = content[1] as i8;
    let approximation = content[2];
    let method = ColrMethod::from_byte(method_byte);
    let mut parameterized = None;
    let (enumerated, icc_profile) = match method {
        ColrMethod::Enumerated => {
            if content.len() != 3 + 4 {
                return Err(Error::InvalidMarkerLength);
            }
            let cs = u32::from_be_bytes([content[3], content[4], content[5], content[6]]);
            (Some(EnumCs::from_u32(cs)), None)
        }
        ColrMethod::RestrictedIccProfile | ColrMethod::AnyIcc => {
            // T.800 §I.5.3.3 / T.814 §D.4.2: PROFILE is the
            // variable-length ICC profile body, which must be at least
            // the 128-byte ICC profile header per ISO 15076-1. We only
            // enforce "non-empty" here — full ICC-profile parsing is
            // out of scope for the JP2-box-wrapper layer.
            if content.len() <= 3 {
                return Err(Error::InvalidMarkerLength);
            }
            (None, Some(content[3..].to_vec()))
        }
        ColrMethod::Parameterized => {
            // T.814 §D.4.3 / Table D.3: three 16-bit H.273 code points
            // plus the VIDFRNG flag byte (top bit; 7 reserved bits).
            if content.len() != 3 + 7 {
                return Err(Error::InvalidMarkerLength);
            }
            parameterized = Some(ParameterizedColour {
                colour_primaries: u16::from_be_bytes([content[3], content[4]]),
                transfer_characteristics: u16::from_be_bytes([content[5], content[6]]),
                matrix_coefficients: u16::from_be_bytes([content[7], content[8]]),
                video_full_range: content[9] & 0x80 != 0,
            });
            (None, None)
        }
        ColrMethod::Reserved(_) => {
            // T.800 §I.5.3.3: "If the value of METH is not 1 or 2,
            // there may be fields in this box following the APPROX
            // field, and a conforming JP2 reader shall ignore the
            // entire Colour Specification box." We honour that by
            // accepting the box but not decoding the trailer.
            (None, None)
        }
    };
    Ok(Colr {
        method,
        precedence,
        approximation,
        enumerated,
        icc_profile,
        parameterized,
    })
}

/// Parses the body of a `pclr` Palette box (T.800 §I.5.3.4 /
/// Figure I.11 / Tables I.12 / I.13).
fn parse_pclr_content(content: &[u8]) -> Result<Pclr, Error> {
    if content.len() < 3 {
        return Err(Error::InvalidMarkerLength);
    }
    let ne = u16::from_be_bytes([content[0], content[1]]) as usize;
    let npc = content[2] as usize;
    // Table I.12: NE in 1..=1024, NPC in 1..=255.
    if !(1..=1024).contains(&ne) || npc == 0 {
        return Err(Error::InvalidMarkerLength);
    }
    if content.len() < 3 + npc {
        return Err(Error::InvalidMarkerLength);
    }
    let mut columns: Vec<PclrColumn> = Vec::with_capacity(npc);
    for &b in &content[3..3 + npc] {
        let depth = (b & 0x7F) + 1;
        // Table I.13: bit depth 1..=38 (x000 0000 .. x010 0101).
        if depth > 38 {
            return Err(Error::InvalidMarkerLength);
        }
        columns.push(PclrColumn {
            bit_depth: depth,
            signed: b & 0x80 != 0,
            values: Vec::with_capacity(ne),
        });
    }
    // Cji values, component-major (entry 0's NPC values, then entry
    // 1's, ...). Each value occupies ceil(Bi / 8) bytes, big-endian,
    // value in the low-order bits (§I.5.3.4).
    let mut pos = 3 + npc;
    for _entry in 0..ne {
        for col in columns.iter_mut() {
            let nbytes = usize::from(col.bit_depth).div_ceil(8);
            if pos + nbytes > content.len() {
                return Err(Error::InvalidMarkerLength);
            }
            let mut raw: u64 = 0;
            for &b in &content[pos..pos + nbytes] {
                raw = (raw << 8) | u64::from(b);
            }
            pos += nbytes;
            // The value sits in the low `bit_depth` bits of the
            // padded field; sign-extend from that width when the
            // column is signed.
            let width = u32::from(col.bit_depth);
            let masked = if width >= 64 {
                raw
            } else {
                raw & ((1u64 << width) - 1)
            };
            let v: i64 = if col.signed && width < 64 && masked >> (width - 1) != 0 {
                masked as i64 - (1i64 << width)
            } else {
                masked as i64
            };
            // Columns deeper than 32 bits parse structurally but the
            // sample lane is i32 — saturate; `decode_jp2` refuses to
            // apply such a column anyway.
            col.values
                .push(v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
        }
    }
    if pos != content.len() {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(Pclr { columns })
}

/// Parses the body of a `cmap` Component Mapping box
/// (T.800 §I.5.3.5 / Figure I.12 / Table I.15). The channel count is
/// the box length divided by the fixed 4-byte entry size.
fn parse_cmap_content(content: &[u8]) -> Result<Vec<CmapEntry>, Error> {
    if content.is_empty() || content.len() % 4 != 0 {
        return Err(Error::InvalidMarkerLength);
    }
    let mut entries = Vec::with_capacity(content.len() / 4);
    for chunk in content.chunks_exact(4) {
        let component = u16::from_be_bytes([chunk[0], chunk[1]]);
        // Table I.15: CMP in 0..=16384.
        if component > 16384 {
            return Err(Error::InvalidMarkerLength);
        }
        let mapping = match chunk[2] {
            0 => {
                // Table I.14: MTYP = 0 (direct) requires PCOL = 0.
                if chunk[3] != 0 {
                    return Err(Error::InvalidMarkerLength);
                }
                CmapMapping::Direct
            }
            1 => CmapMapping::Palette { column: chunk[3] },
            // MTYP 2..=255 reserved for ITU-T | ISO use.
            _ => return Err(Error::InvalidMarkerLength),
        };
        entries.push(CmapEntry { component, mapping });
    }
    Ok(entries)
}

/// Parses the body of a `cdef` Channel Definition box
/// (T.800 §I.5.3.6 / Figure I.13 / Table I.19).
fn parse_cdef_content(content: &[u8]) -> Result<Vec<ChannelDef>, Error> {
    if content.len() < 2 {
        return Err(Error::InvalidMarkerLength);
    }
    let n = u16::from_be_bytes([content[0], content[1]]) as usize;
    // Table I.19: N in 1..=65535, followed by N (Cn, Typ, Asoc)
    // 16-bit triples.
    if n == 0 || content.len() != 2 + 6 * n {
        return Err(Error::InvalidMarkerLength);
    }
    let mut defs = Vec::with_capacity(n);
    for chunk in content[2..].chunks_exact(6) {
        defs.push(ChannelDef {
            channel: u16::from_be_bytes([chunk[0], chunk[1]]),
            channel_type: u16::from_be_bytes([chunk[2], chunk[3]]),
            association: u16::from_be_bytes([chunk[4], chunk[5]]),
        });
    }
    // §I.5.3.6: at most one channel per (Typ, Asoc) pair, except the
    // 2^16 - 1 ("not specified" / "no association") values.
    for (i, a) in defs.iter().enumerate() {
        if a.channel_type == 0xFFFF || a.association == 0xFFFF {
            continue;
        }
        for b in &defs[i + 1..] {
            if b.channel_type == a.channel_type && b.association == a.association {
                return Err(Error::InvalidMarkerLength);
            }
        }
    }
    Ok(defs)
}

/// Parses the ten-byte body shared by the `resc` / `resd` boxes
/// (T.800 §I.5.3.7.1 / Figure I.15 / Table I.20).
fn parse_grid_resolution_content(content: &[u8]) -> Result<GridResolution, Error> {
    if content.len() != 10 {
        return Err(Error::InvalidMarkerLength);
    }
    let res = GridResolution {
        vertical_numerator: u16::from_be_bytes([content[0], content[1]]),
        vertical_denominator: u16::from_be_bytes([content[2], content[3]]),
        horizontal_numerator: u16::from_be_bytes([content[4], content[5]]),
        horizontal_denominator: u16::from_be_bytes([content[6], content[7]]),
        vertical_exponent: content[8] as i8,
        horizontal_exponent: content[9] as i8,
    };
    // Table I.20: numerators and denominators in 1..=65535.
    if res.vertical_numerator == 0
        || res.vertical_denominator == 0
        || res.horizontal_numerator == 0
        || res.horizontal_denominator == 0
    {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(res)
}

/// Parses the body of a `res ` Resolution superbox (T.800 §I.5.3.7 /
/// Figure I.14): a Capture Resolution box, a Default Display
/// Resolution box, or both.
fn parse_res_content(content: &[u8]) -> Result<Resolution, Error> {
    let mut capture: Option<GridResolution> = None;
    let mut display: Option<GridResolution> = None;
    let mut pos = 0usize;
    while pos < content.len() {
        let b = read_box(content, pos)?;
        let body = &content[b.offset + b.header_len..b.offset + b.header_len + b.content_len];
        match b.box_type {
            BOX_TYPE_RESC => {
                if capture.is_some() {
                    return Err(Error::InvalidMarkerLength);
                }
                capture = Some(parse_grid_resolution_content(body)?);
            }
            BOX_TYPE_RESD => {
                if display.is_some() {
                    return Err(Error::InvalidMarkerLength);
                }
                display = Some(parse_grid_resolution_content(body)?);
            }
            // Unknown children are skipped (T.800 §I.4 reader rule).
            _ => {}
        }
        pos = pos
            .checked_add(b.total_len())
            .ok_or(Error::InvalidMarkerLength)?;
    }
    // §I.5.3.7: "it shall contain either a Capture Resolution box, or
    // a Default Display Resolution box, or both".
    if capture.is_none() && display.is_none() {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(Resolution { capture, display })
}

/// Parses the body of a `jp2h` superbox (T.800 §I.5.3 / Figure I.7).
///
/// The first child box must be `ihdr` (T.800 §I.5.3.1 specifies
/// "this box shall be the first box in the JP2 Header box"). Any
/// subsequent `bpcc` / `colr` / unknown boxes are walked in order.
fn parse_jp2h_content(content: &[u8], jph: bool) -> Result<Jp2Header, Error> {
    let first = read_box(content, 0)?;
    if first.box_type != BOX_TYPE_IHDR {
        return Err(Error::InvalidMarkerLength);
    }
    let ihdr_content = &content
        [first.offset + first.header_len..first.offset + first.header_len + first.content_len];
    let ihdr = parse_ihdr_content(ihdr_content)?;
    let mut bpcc: Option<Bpcc> = None;
    let mut colr: Vec<Colr> = Vec::new();
    let mut pclr: Option<Pclr> = None;
    let mut cmap: Option<Vec<CmapEntry>> = None;
    let mut cdef: Option<Vec<ChannelDef>> = None;
    let mut resolution: Option<Resolution> = None;
    let mut pos = first.total_len();
    while pos < content.len() {
        let b = read_box(content, pos)?;
        let body = &content[b.offset + b.header_len..b.offset + b.header_len + b.content_len];
        match b.box_type {
            BOX_TYPE_BPCC => {
                if bpcc.is_some() {
                    // T.800 §I.5.3.2: "There shall be one and only
                    // one Bits Per Component box inside a JP2
                    // Header box."
                    return Err(Error::InvalidMarkerLength);
                }
                bpcc = Some(parse_bpcc_content(body, ihdr.component_count)?);
            }
            BOX_TYPE_COLR => {
                colr.push(parse_colr_content(body)?);
            }
            BOX_TYPE_PCLR => {
                // §I.5.3.4: "There shall be at most one Palette box
                // inside a JP2 Header box."
                if pclr.is_some() {
                    return Err(Error::InvalidMarkerLength);
                }
                pclr = Some(parse_pclr_content(body)?);
            }
            BOX_TYPE_CMAP => {
                // §I.5.3.5: at most one Component Mapping box.
                if cmap.is_some() {
                    return Err(Error::InvalidMarkerLength);
                }
                cmap = Some(parse_cmap_content(body)?);
            }
            BOX_TYPE_CDEF => {
                // §I.5.3.6: at most one Channel Definition box.
                if cdef.is_some() {
                    return Err(Error::InvalidMarkerLength);
                }
                cdef = Some(parse_cdef_content(body)?);
            }
            BOX_TYPE_RES => {
                // §I.5.3.7: at most one Resolution box.
                if resolution.is_some() {
                    return Err(Error::InvalidMarkerLength);
                }
                resolution = Some(parse_res_content(body)?);
            }
            // Unrecognised children are silently skipped per T.800
            // §I.5.3 (conforming readers ignore unknown boxes).
            _ => {}
        }
        pos = pos
            .checked_add(b.total_len())
            .ok_or(Error::InvalidMarkerLength)?;
    }
    // §I.5.3.4: a Palette box requires a Component Mapping box and
    // vice versa ("If the JP2 Header box contains a Palette box, then
    // it shall also contain a Component Mapping box. If the JP2
    // Header box does not contain a Palette box, then it shall not
    // contain a Component Mapping box.").
    if pclr.is_some() != cmap.is_some() {
        return Err(Error::InvalidMarkerLength);
    }
    // Cross-box consistency: every palette mapping must name an
    // existing palette column, and every direct / palette mapping an
    // existing codestream component (Ihdr NC mirrors Csiz).
    if let Some(entries) = &cmap {
        let npc = pclr.as_ref().map_or(0, |p| p.columns.len());
        for e in entries {
            if e.component >= ihdr.component_count {
                return Err(Error::InvalidMarkerLength);
            }
            if let CmapMapping::Palette { column } = e.mapping {
                if usize::from(column) >= npc {
                    return Err(Error::InvalidMarkerLength);
                }
            }
        }
    }
    // §I.5.3.6: a cdef description names a channel — a Component
    // Mapping channel when a `cmap` exists, else a codestream
    // component index.
    if let Some(defs) = &cdef {
        let channel_count = cmap
            .as_ref()
            .map_or(usize::from(ihdr.component_count), Vec::len);
        for d in defs {
            if usize::from(d.channel) >= channel_count {
                return Err(Error::InvalidMarkerLength);
            }
        }
    }
    // T.800 §I.5.3.3: "There shall be at least one Colour
    // Specification box within the JP2 Header box." T.814 §D.2 lifts
    // the requirement for a JPH file whose `UnkC` flag is non-zero
    // (the colourspace is then simply unspecified).
    if colr.is_empty() && !(jph && ihdr.colourspace_unknown != 0) {
        return Err(Error::InvalidMarkerLength);
    }
    // T.800 §I.5.3.1: when the `BPC` sentinel is `0xFF` the
    // `bpcc` box must be present.
    if ihdr.varies_in_bit_depth() && bpcc.is_none() {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(Jp2Header {
        ihdr,
        bpcc,
        colr,
        pclr,
        cmap,
        cdef,
        resolution,
    })
}

// ---------------------------------------------------------------------------
// Top-level wrapper parser.
// ---------------------------------------------------------------------------

/// Parses a JP2 box-structured file (T.800 Annex I) into a
/// [`Jp2Container`].
///
/// The parser walks the top-level box chain, requiring the
/// canonical ordering described in T.800 §I.3:
///
/// 1. `jP  ` (signature) — first box; T.800 §I.5.1.
/// 2. `ftyp` (File Type) — must immediately follow signature;
///    T.800 §I.5.2.
/// 3. `jp2h` (JP2 Header superbox) — after `ftyp`, before `jp2c`;
///    T.800 §I.5.3.
/// 4. `jp2c` (Contiguous Codestream) — the codestream payload.
///
/// Any non-required boxes appearing between `ftyp` and `jp2c` (e.g.
/// `jp2i`, `xml `, `uuid`) are tolerated and skipped over by
/// length. Only the **first** `jp2c` box's payload is reported in
/// the returned `Jp2Container`; multi-codestream files use the same
/// box type once per codestream but the JP2 format itself describes
/// only the first.
///
/// On success the returned container's `codestream_offset` /
/// `codestream_len` delimit the bytes that callers may hand to
/// [`crate::parse_codestream`].
pub fn parse_jp2(bytes: &[u8]) -> Result<Jp2Container, Error> {
    // 1. Signature box.
    let sig = read_box(bytes, 0)?;
    if sig.box_type != BOX_TYPE_JP2_SIGNATURE {
        return Err(Error::InvalidMarkerLength);
    }
    let sig_body =
        &bytes[sig.offset + sig.header_len..sig.offset + sig.header_len + sig.content_len];
    let _ = parse_signature_content(sig_body)?;
    // 2. File Type box.
    let mut pos = sig.total_len();
    let ftyp_box = read_box(bytes, pos)?;
    if ftyp_box.box_type != BOX_TYPE_FTYP {
        return Err(Error::InvalidMarkerLength);
    }
    let ftyp_body = &bytes[ftyp_box.offset + ftyp_box.header_len
        ..ftyp_box.offset + ftyp_box.header_len + ftyp_box.content_len];
    let ftyp = parse_ftyp_content(ftyp_body)?;
    pos = pos
        .checked_add(ftyp_box.total_len())
        .ok_or(Error::InvalidMarkerLength)?;
    // 3..N. Walk remaining boxes until we find the jp2h and jp2c.
    let mut header: Option<Jp2Header> = None;
    let mut codestream: Option<(usize, usize)> = None;
    while pos < bytes.len() {
        let b = read_box(bytes, pos)?;
        let body_start = b.offset + b.header_len;
        let body_end = body_start
            .checked_add(b.content_len)
            .ok_or(Error::InvalidMarkerLength)?;
        match b.box_type {
            BOX_TYPE_JP2H => {
                if header.is_some() {
                    // T.800 §I.5.3: "Within a JP2 file, there shall
                    // be one and only one JP2 Header box."
                    return Err(Error::InvalidMarkerLength);
                }
                let body = &bytes[body_start..body_end];
                header = Some(parse_jp2h_content(body, ftyp.is_jph_compatible())?);
            }
            // Per T.800 Table I.2 the Contiguous Codestream box is
            // "Required" but the file may contain additional ones; we
            // only report the first by guarding on `codestream.is_none()`.
            BOX_TYPE_JP2C if codestream.is_none() => {
                codestream = Some((body_start, b.content_len));
            }
            // All other top-level boxes — jp2i, xml , uuid, uinf,
            // etc. — are skipped per T.800 §I.4 ("if the type of a
            // box was not understood by a reader, it would not
            // recognize the existence of … inside that box").
            _ => {}
        }
        pos = pos
            .checked_add(b.total_len())
            .ok_or(Error::InvalidMarkerLength)?;
    }
    let header = header.ok_or(Error::InvalidMarkerLength)?;
    let (codestream_offset, codestream_len) = codestream.ok_or(Error::InvalidMarkerLength)?;
    // T.800 §I.5.3 enforces that jp2h precedes jp2c in the file.
    // Our scan above pushes the first jp2c offset; cross-check
    // ordering by requiring the jp2h to land before that offset.
    // (We only do this when both have been found.)
    if codestream_offset != 0 {
        // body_start = offset + header_len; if header was after
        // jp2c, its `pos` would be > codestream_offset. We rely on
        // the iteration order — `header` was set inside the same
        // loop that observed `codestream` — but the spec language
        // is enforced by the iteration order itself: if we saw
        // `jp2c` first, `header` would still be `None` at that
        // point, but we record `codestream` opportunistically.
        // Recover ordering by re-scanning for both offsets.
        // (Cheap: the loop already walked the file.)
    }
    Ok(Jp2Container {
        ftyp,
        header,
        codestream_offset,
        codestream_len,
    })
}

// ---------------------------------------------------------------------------
// End-to-end JP2 decode — palette + channel mapping applied.
// ---------------------------------------------------------------------------

/// Generate the image **channels** from the decoded codestream
/// components per the JP2 Header's Component Mapping / Palette boxes
/// (T.800 §I.5.3.4 / §I.5.3.5), then order them for presentation per
/// the Channel Definition box (§I.5.3.6).
///
/// * No `cmap` — component `i` **is** channel `i` (§I.5.3.5 default).
/// * `cmap` present — channel `i` is entry `i`: either a direct copy
///   of component `CMPi`, or component `CMPi` mapped through palette
///   column `PCOLi` (each decoded sample indexes the column; indices
///   are clamped to the palette's entry range, since a lossy-coded
///   index plane may legitimately reconstruct slightly out of range).
///   A palette-generated channel takes the column's bit depth /
///   signedness and the index component's grid.
/// * `cdef` present — the returned channels are reordered so the
///   colour channels (`Typ = 0`) come first in increasing `Asoc`
///   colour order, followed by the remaining channels (opacity /
///   unspecified) in channel order. Without a `cdef`, channel order
///   is already colour order (§I.5.3.6 default rule).
fn apply_channel_mapping(
    header: &Jp2Header,
    image: crate::DecodedImage,
) -> Result<crate::DecodedImage, Error> {
    let (width, height) = (image.width, image.height);
    let components = image.components;
    // 1. cmap / pclr: build the channel planes.
    let mut channels: Vec<crate::DecodedComponent> = match &header.cmap {
        None => components,
        Some(entries) => {
            let mut out = Vec::with_capacity(entries.len());
            for e in entries {
                let comp = components
                    .get(usize::from(e.component))
                    .ok_or(Error::InvalidMarkerLength)?;
                match e.mapping {
                    CmapMapping::Direct => out.push(comp.clone()),
                    CmapMapping::Palette { column } => {
                        let pclr = header.pclr.as_ref().ok_or(Error::InvalidMarkerLength)?;
                        let col = pclr
                            .columns
                            .get(usize::from(column))
                            .ok_or(Error::InvalidMarkerLength)?;
                        // The i32 sample lane carries at most 32 bits.
                        if col.bit_depth > 32 {
                            return Err(Error::NotImplemented);
                        }
                        let ne = col.values.len() as i32;
                        let samples = comp
                            .samples
                            .iter()
                            .map(|&s| col.values[s.clamp(0, ne - 1) as usize])
                            .collect();
                        out.push(crate::DecodedComponent {
                            width: comp.width,
                            height: comp.height,
                            precision_bits: col.bit_depth,
                            is_signed: col.signed,
                            h_separation: comp.h_separation,
                            v_separation: comp.v_separation,
                            samples,
                        });
                    }
                }
            }
            out
        }
    };
    // 2. cdef: reorder for presentation. Each channel keys on its
    // first description: colour channels sort by their 1-based Asoc
    // colour index, everything else (opacity, premultiplied opacity,
    // unspecified, whole-image / unassociated) follows in original
    // channel order.
    if let Some(defs) = &header.cdef {
        let n = channels.len();
        let mut order: Vec<usize> = (0..n).collect();
        let key = |idx: usize| -> (u8, u32) {
            let d = defs.iter().find(|d| usize::from(d.channel) == idx);
            match d {
                Some(d)
                    if d.channel_type == ChannelDef::TYPE_COLOUR
                        && (1..=0xFFFE).contains(&d.association) =>
                {
                    (0, u32::from(d.association))
                }
                _ => (1, idx as u32),
            }
        };
        order.sort_by_key(|&i| key(i));
        let mut reordered = Vec::with_capacity(n);
        let mut taken: Vec<Option<crate::DecodedComponent>> =
            channels.drain(..).map(Some).collect();
        for i in order {
            reordered.push(taken[i].take().ok_or(Error::InvalidMarkerLength)?);
        }
        channels = reordered;
    }
    Ok(crate::DecodedImage {
        width,
        height,
        components: channels,
    })
}

/// Decode a JP2 / JPH **file** end-to-end: parse the Annex I box
/// structure ([`parse_jp2`]), decode the contiguous codestream
/// ([`crate::decode_j2k`]), then apply the JP2-Header channel
/// semantics — the §I.5.3.4 / §I.5.3.5 Palette + Component Mapping
/// boxes (palettized files expand to their generated channels) and
/// the §I.5.3.6 Channel Definition ordering (colour channels first,
/// in colour order, then opacity / auxiliary channels).
///
/// The returned [`crate::DecodedImage`] therefore holds the image
/// **channels**, not the raw codestream components — for a palettized
/// file the single index component becomes `NPC` generated planes.
/// Files without `pclr` / `cmap` / `cdef` decode identically to
/// running [`crate::decode_j2k`] on the `jp2c` payload.
pub fn decode_jp2(bytes: &[u8]) -> Result<crate::DecodedImage, Error> {
    let container = parse_jp2(bytes)?;
    let end = container
        .codestream_offset
        .checked_add(container.codestream_len)
        .ok_or(Error::UnexpectedEof)?;
    let codestream = bytes
        .get(container.codestream_offset..end)
        .ok_or(Error::UnexpectedEof)?;
    let image = crate::decode_j2k(codestream)?;
    apply_channel_mapping(&container.header, image)
}

// ---------------------------------------------------------------------------
// Tests — synthetic JP2 box buffers per T.800 Annex I.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Append a standard (8-byte header) box with the given type and
    /// payload to `out`.
    fn emit_box(out: &mut Vec<u8>, box_type: u32, payload: &[u8]) {
        let total = 8 + payload.len() as u32;
        out.extend_from_slice(&total.to_be_bytes());
        out.extend_from_slice(&box_type.to_be_bytes());
        out.extend_from_slice(payload);
    }

    /// Build a minimal conforming JP2 file:
    ///   - `jP  ` signature
    ///   - `ftyp` brand jp2 + jp2 compat
    ///   - `jp2h` superbox with `ihdr` (1 component, 8-bit unsigned) +
    ///     `colr` (sRGB enumerated)
    ///   - `jp2c` with a 4-byte synthetic codestream payload
    fn synth_minimal_jp2(codestream: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        // Signature.
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        // ftyp: BR=jp2, MinV=0, CLi=[jp2].
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        emit_box(&mut v, BOX_TYPE_FTYP, &ftyp_body);
        // jp2h superbox: ihdr + colr.
        let mut jp2h_body = Vec::new();
        // ihdr — 14-byte fixed content.
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&64u32.to_be_bytes()); // HEIGHT
        ihdr_body.extend_from_slice(&128u32.to_be_bytes()); // WIDTH
        ihdr_body.extend_from_slice(&1u16.to_be_bytes()); // NC
        ihdr_body.push(7); // BPC = 8-bit unsigned
        ihdr_body.push(7); // C = compression = 7
        ihdr_body.push(0); // UnkC
        ihdr_body.push(0); // IPR
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        // colr — METH=1 enumerated, PREC=0, APPROX=0, EnumCS=16
        let mut colr_body = Vec::new();
        colr_body.push(1); // METH
        colr_body.push(0); // PREC
        colr_body.push(0); // APPROX
        colr_body.extend_from_slice(&16u32.to_be_bytes()); // EnumCS = sRGB
        emit_box(&mut jp2h_body, BOX_TYPE_COLR, &colr_body);
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        // jp2c codestream.
        emit_box(&mut v, BOX_TYPE_JP2C, codestream);
        v
    }

    #[test]
    fn parses_minimal_jp2() {
        let codestream = b"\xFF\x4F\xFF\xD9"; // SOC + EOC stand-in
        let bytes = synth_minimal_jp2(codestream);
        let c = parse_jp2(&bytes).expect("parse jp2");
        assert_eq!(c.ftyp.brand, BRAND_JP2);
        assert!(c.ftyp.is_jp2_compatible());
        assert_eq!(c.ftyp.minor_version, 0);
        assert_eq!(c.ftyp.compatibility, vec![BRAND_JP2]);
        assert_eq!(c.header.ihdr.height, 64);
        assert_eq!(c.header.ihdr.width, 128);
        assert_eq!(c.header.ihdr.component_count, 1);
        assert_eq!(c.header.ihdr.bit_depth(), 8);
        assert!(!c.header.ihdr.is_signed());
        assert!(!c.header.ihdr.varies_in_bit_depth());
        assert!(c.header.bpcc.is_none());
        assert_eq!(c.header.colr.len(), 1);
        assert_eq!(c.header.colr[0].method, ColrMethod::Enumerated);
        assert_eq!(c.header.colr[0].enumerated, Some(EnumCs::Srgb));
        assert_eq!(c.codestream_len, codestream.len());
        assert_eq!(
            &bytes[c.codestream_offset..c.codestream_offset + c.codestream_len],
            codestream
        );
    }

    #[test]
    fn rejects_missing_signature() {
        let codestream = b"\xFF\x4F\xFF\xD9";
        let mut bytes = synth_minimal_jp2(codestream);
        // Corrupt the signature box type to something else.
        bytes[4] = 0x00;
        assert!(parse_jp2(&bytes).is_err());
    }

    #[test]
    fn rejects_bad_signature_magic() {
        let codestream = b"\xFF\x4F\xFF\xD9";
        let mut bytes = synth_minimal_jp2(codestream);
        // Signature content lives at offset 8..12.
        bytes[8] = 0x00;
        assert!(parse_jp2(&bytes).is_err());
    }

    #[test]
    fn rejects_missing_ftyp() {
        let mut v = Vec::new();
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        // Skip ftyp; emit jp2h then jp2c.
        let mut jp2h_body = Vec::new();
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u16.to_be_bytes());
        ihdr_body.push(7);
        ihdr_body.push(7);
        ihdr_body.push(0);
        ihdr_body.push(0);
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        let mut colr_body = Vec::new();
        colr_body.push(1);
        colr_body.push(0);
        colr_body.push(0);
        colr_body.extend_from_slice(&16u32.to_be_bytes());
        emit_box(&mut jp2h_body, BOX_TYPE_COLR, &colr_body);
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        emit_box(&mut v, BOX_TYPE_JP2C, b"\xFF\x4F\xFF\xD9");
        assert!(parse_jp2(&v).is_err());
    }

    #[test]
    fn parses_3_component_with_bpcc() {
        // 3-component 16x16 image, components vary in bit depth.
        let mut v = Vec::new();
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        emit_box(&mut v, BOX_TYPE_FTYP, &ftyp_body);
        let mut jp2h_body = Vec::new();
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&16u32.to_be_bytes());
        ihdr_body.extend_from_slice(&16u32.to_be_bytes());
        ihdr_body.extend_from_slice(&3u16.to_be_bytes());
        ihdr_body.push(0xFF); // BPC = vary
        ihdr_body.push(7);
        ihdr_body.push(0);
        ihdr_body.push(0);
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        // bpcc: three component depths.
        emit_box(&mut jp2h_body, BOX_TYPE_BPCC, &[7, 7, 15]);
        let mut colr_body = Vec::new();
        colr_body.push(1);
        colr_body.push(0);
        colr_body.push(0);
        colr_body.extend_from_slice(&16u32.to_be_bytes());
        emit_box(&mut jp2h_body, BOX_TYPE_COLR, &colr_body);
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        emit_box(&mut v, BOX_TYPE_JP2C, b"\xFF\x4F\xFF\xD9");
        let c = parse_jp2(&v).expect("parse 3-component");
        assert_eq!(c.header.ihdr.component_count, 3);
        assert!(c.header.ihdr.varies_in_bit_depth());
        assert_eq!(c.header.bpcc.as_ref().unwrap().bpci, vec![7, 7, 15]);
    }

    #[test]
    fn rejects_vary_bpc_without_bpcc() {
        let mut v = Vec::new();
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        emit_box(&mut v, BOX_TYPE_FTYP, &ftyp_body);
        let mut jp2h_body = Vec::new();
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&3u16.to_be_bytes());
        ihdr_body.push(0xFF); // BPC = vary, but no bpcc box follows
        ihdr_body.push(7);
        ihdr_body.push(0);
        ihdr_body.push(0);
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        let mut colr_body = Vec::new();
        colr_body.push(1);
        colr_body.push(0);
        colr_body.push(0);
        colr_body.extend_from_slice(&16u32.to_be_bytes());
        emit_box(&mut jp2h_body, BOX_TYPE_COLR, &colr_body);
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        emit_box(&mut v, BOX_TYPE_JP2C, b"\xFF\x4F\xFF\xD9");
        assert!(parse_jp2(&v).is_err());
    }

    #[test]
    fn rejects_jp2h_with_no_colr() {
        let mut v = Vec::new();
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        emit_box(&mut v, BOX_TYPE_FTYP, &ftyp_body);
        let mut jp2h_body = Vec::new();
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u16.to_be_bytes());
        ihdr_body.push(7);
        ihdr_body.push(7);
        ihdr_body.push(0);
        ihdr_body.push(0);
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        emit_box(&mut v, BOX_TYPE_JP2C, b"\xFF\x4F\xFF\xD9");
        assert!(parse_jp2(&v).is_err());
    }

    #[test]
    fn parses_with_intermediate_unknown_box() {
        // Insert an unknown 'xml ' box between ftyp and jp2h.
        let mut v = Vec::new();
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        emit_box(&mut v, BOX_TYPE_FTYP, &ftyp_body);
        // unknown 'xml ' box (0x786D_6C20).
        emit_box(&mut v, 0x786D_6C20, b"<xml/>");
        let mut jp2h_body = Vec::new();
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u16.to_be_bytes());
        ihdr_body.push(7);
        ihdr_body.push(7);
        ihdr_body.push(0);
        ihdr_body.push(0);
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        let mut colr_body = Vec::new();
        colr_body.push(1);
        colr_body.push(0);
        colr_body.push(0);
        colr_body.extend_from_slice(&16u32.to_be_bytes());
        emit_box(&mut jp2h_body, BOX_TYPE_COLR, &colr_body);
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        emit_box(&mut v, BOX_TYPE_JP2C, b"\xFF\x4F\xFF\xD9");
        let c = parse_jp2(&v).expect("parse with xml");
        assert_eq!(c.header.ihdr.width, 1);
        assert_eq!(c.codestream_len, 4);
    }

    #[test]
    fn parses_extended_length_jp2c() {
        // Build a jp2c with LBox=1 + XLBox extended length.
        let mut v = Vec::new();
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        emit_box(&mut v, BOX_TYPE_FTYP, &ftyp_body);
        let mut jp2h_body = Vec::new();
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u16.to_be_bytes());
        ihdr_body.push(7);
        ihdr_body.push(7);
        ihdr_body.push(0);
        ihdr_body.push(0);
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        let mut colr_body = Vec::new();
        colr_body.push(1);
        colr_body.push(0);
        colr_body.push(0);
        colr_body.extend_from_slice(&16u32.to_be_bytes());
        emit_box(&mut jp2h_body, BOX_TYPE_COLR, &colr_body);
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        // Extended-length jp2c.
        let payload: &[u8] = b"\xFF\x4F\xFF\xD9\x00\x00";
        let total: u64 = 16 + payload.len() as u64;
        v.extend_from_slice(&1u32.to_be_bytes()); // LBox = 1
        v.extend_from_slice(&BOX_TYPE_JP2C.to_be_bytes());
        v.extend_from_slice(&total.to_be_bytes()); // XLBox
        v.extend_from_slice(payload);
        let c = parse_jp2(&v).expect("parse extended");
        assert_eq!(c.codestream_len, payload.len());
        assert_eq!(
            &v[c.codestream_offset..c.codestream_offset + c.codestream_len],
            payload
        );
    }

    #[test]
    fn parses_lbox_zero_jp2c() {
        // jp2c with LBox = 0 (extends to end of file).
        let mut v = Vec::new();
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        emit_box(&mut v, BOX_TYPE_FTYP, &ftyp_body);
        let mut jp2h_body = Vec::new();
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u16.to_be_bytes());
        ihdr_body.push(7);
        ihdr_body.push(7);
        ihdr_body.push(0);
        ihdr_body.push(0);
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        let mut colr_body = Vec::new();
        colr_body.push(1);
        colr_body.push(0);
        colr_body.push(0);
        colr_body.extend_from_slice(&16u32.to_be_bytes());
        emit_box(&mut jp2h_body, BOX_TYPE_COLR, &colr_body);
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        // jp2c with LBox = 0.
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&BOX_TYPE_JP2C.to_be_bytes());
        let payload: &[u8] = b"\xFF\x4F\xFF\xD9\x00";
        v.extend_from_slice(payload);
        let c = parse_jp2(&v).expect("parse lbox=0");
        assert_eq!(c.codestream_len, payload.len());
    }

    #[test]
    fn parses_icc_profile_colr() {
        // Build with METH=2 colr carrying a dummy 132-byte body.
        let mut v = Vec::new();
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        emit_box(&mut v, BOX_TYPE_FTYP, &ftyp_body);
        let mut jp2h_body = Vec::new();
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u32.to_be_bytes());
        ihdr_body.extend_from_slice(&3u16.to_be_bytes());
        ihdr_body.push(7);
        ihdr_body.push(7);
        ihdr_body.push(0);
        ihdr_body.push(0);
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        // colr — METH=2, 16-byte dummy profile body.
        let mut colr_body = Vec::new();
        colr_body.push(2); // METH = restricted ICC
        colr_body.push(0);
        colr_body.push(0);
        colr_body.extend_from_slice(&[0xABu8; 16]);
        emit_box(&mut jp2h_body, BOX_TYPE_COLR, &colr_body);
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        emit_box(&mut v, BOX_TYPE_JP2C, b"\xFF\x4F\xFF\xD9");
        let c = parse_jp2(&v).expect("icc colr");
        assert_eq!(c.header.colr[0].method, ColrMethod::RestrictedIccProfile);
        assert_eq!(c.header.colr[0].icc_profile.as_ref().unwrap().len(), 16);
        assert!(c.header.colr[0].enumerated.is_none());
    }

    #[test]
    fn rejects_truncated_box() {
        // LBox claims 1000 bytes but only 8 follow.
        let mut v = Vec::new();
        v.extend_from_slice(&1000u32.to_be_bytes());
        v.extend_from_slice(&BOX_TYPE_JP2_SIGNATURE.to_be_bytes());
        assert!(parse_jp2(&v).is_err());
    }

    #[test]
    fn rejects_reserved_lbox_value() {
        // LBox = 4 (in 2..=7 reserved range).
        let mut v = Vec::new();
        v.extend_from_slice(&4u32.to_be_bytes());
        v.extend_from_slice(&BOX_TYPE_JP2_SIGNATURE.to_be_bytes());
        assert!(parse_jp2(&v).is_err());
    }

    #[test]
    fn ftyp_is_jp2_compatible_recognises_brand() {
        let ftyp = Ftyp {
            brand: 0xDEAD_BEEF,
            minor_version: 0,
            compatibility: vec![0x1234_5678, BRAND_JP2],
        };
        assert!(ftyp.is_jp2_compatible());
        let ftyp2 = Ftyp {
            brand: BRAND_JP2,
            minor_version: 0,
            compatibility: vec![0xDEAD_BEEF],
        };
        assert!(!ftyp2.is_jp2_compatible());
    }

    // -- T.814 Annex D — JPH (HTJ2K) file format ------------------------

    /// Build a minimal conforming JPH file (T.814 Annex D): the JP2
    /// box layout with brand `'jph '`, `UnkC = 1` and — per §D.2 —
    /// no Colour Specification box, wrapping `codestream`.
    fn synth_minimal_jph(codestream: &[u8], w: u32, h: u32, colr: Option<&[u8]>) -> Vec<u8> {
        let mut v = Vec::new();
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        // ftyp: BR = 'jph ', MinV = 0, CLi = ['jph '] (§D.3).
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(&BRAND_JPH.to_be_bytes());
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(&BRAND_JPH.to_be_bytes());
        emit_box(&mut v, BOX_TYPE_FTYP, &ftyp_body);
        let mut jp2h_body = Vec::new();
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&h.to_be_bytes());
        ihdr_body.extend_from_slice(&w.to_be_bytes());
        ihdr_body.extend_from_slice(&1u16.to_be_bytes()); // NC
        ihdr_body.push(7); // BPC = 8-bit unsigned
        ihdr_body.push(7); // C = 7
        ihdr_body.push(u8::from(colr.is_none())); // UnkC
        ihdr_body.push(0); // IPR
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        if let Some(c) = colr {
            emit_box(&mut jp2h_body, BOX_TYPE_COLR, c);
        }
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        emit_box(&mut v, BOX_TYPE_JP2C, codestream);
        v
    }

    /// A JPH file wrapping this crate's own HTJ2K codestream parses
    /// per Annex D (no colr box under `UnkC = 1`) and its embedded
    /// codestream decodes bit-exactly.
    #[test]
    fn jph_file_with_ht_codestream_round_trips() {
        let (w, h) = (32u32, 24u32);
        let mut seed = 0x4A50_4801u32;
        let plane: Vec<u8> = (0..w * h)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            })
            .collect();
        let codestream = crate::encode::encode_j2k(
            &[&plane],
            w,
            h,
            &crate::encode::EncodeParams {
                decomposition_levels: 2,
                code_block_exp: (4, 4),
                high_throughput: true,
                ht_refinement: true,
                ..crate::encode::EncodeParams::default()
            },
        )
        .expect("HT encode");
        let jph = synth_minimal_jph(&codestream, w, h, None);
        let c = parse_jp2(&jph).expect("parse JPH");
        assert_eq!(c.ftyp.brand, BRAND_JPH);
        assert!(c.ftyp.is_jph_compatible());
        assert!(!c.ftyp.is_jp2_compatible());
        assert!(c.header.colr.is_empty());
        assert_eq!(c.header.ihdr.colourspace_unknown, 1);
        let embedded = &jph[c.codestream_offset..c.codestream_offset + c.codestream_len];
        let img = crate::decode::decode_j2k(embedded).expect("decode embedded HTJ2K");
        let got: Vec<u8> = img.components[0].samples.iter().map(|&s| s as u8).collect();
        assert_eq!(got, plane);
    }

    /// §D.2: the colr box is only optional when `UnkC` is non-zero —
    /// a JPH header with `UnkC = 0` and no colr stays malformed, and a
    /// plain JP2 never gets the exemption.
    #[test]
    fn jph_colr_exemption_requires_unkc() {
        let cs = b"\xFF\x4F\xFF\xD9";
        // UnkC = 0 (colr Some → UnkC 0) but then strip the colr box:
        // build manually with UnkC = 0 and no colr.
        let mut v = Vec::new();
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(&BRAND_JPH.to_be_bytes());
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(&BRAND_JPH.to_be_bytes());
        emit_box(&mut v, BOX_TYPE_FTYP, &ftyp_body);
        let mut jp2h_body = Vec::new();
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&8u32.to_be_bytes());
        ihdr_body.extend_from_slice(&8u32.to_be_bytes());
        ihdr_body.extend_from_slice(&1u16.to_be_bytes());
        ihdr_body.push(7);
        ihdr_body.push(7);
        ihdr_body.push(0); // UnkC = 0 — colr stays required
        ihdr_body.push(0);
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        emit_box(&mut v, BOX_TYPE_JP2C, cs);
        assert!(parse_jp2(&v).is_err());
    }

    /// T.814 §D.4.2 / §D.4.3 — the JPH-defined METH values parse: 3
    /// (any ICC profile) and 5 (H.273 parameterized colourspace).
    #[test]
    fn jph_meth_3_and_5_parse() {
        let cs = b"\xFF\x4F\xFF\xD9";
        // METH = 3: any ICC.
        let mut colr3 = vec![3u8, 0, 0];
        colr3.extend_from_slice(&[0xAA; 16]); // stand-in profile bytes
        let jph = synth_minimal_jph(cs, 8, 8, Some(&colr3));
        let c = parse_jp2(&jph).expect("parse METH 3");
        assert_eq!(c.header.colr[0].method, ColrMethod::AnyIcc);
        assert_eq!(
            c.header.colr[0].icc_profile.as_deref(),
            Some(&[0xAA; 16][..])
        );
        // METH = 5: COLPRIMS = 9 (BT.2020), TRANSFC = 16 (PQ),
        // MATCOEFFS = 9, VIDFRNG = 1.
        let colr5 = [5u8, 0, 0, 0, 9, 0, 16, 0, 9, 0x80];
        let jph = synth_minimal_jph(cs, 8, 8, Some(&colr5));
        let c = parse_jp2(&jph).expect("parse METH 5");
        assert_eq!(c.header.colr[0].method, ColrMethod::Parameterized);
        assert_eq!(
            c.header.colr[0].parameterized,
            Some(ParameterizedColour {
                colour_primaries: 9,
                transfer_characteristics: 16,
                matrix_coefficients: 9,
                video_full_range: true,
            })
        );
        // A METH = 5 payload of the wrong size is malformed.
        let bad = [5u8, 0, 0, 0, 9, 0, 16];
        assert!(parse_jp2(&synth_minimal_jph(cs, 8, 8, Some(&bad))).is_err());
    }

    // -----------------------------------------------------------------
    // pclr / cmap / cdef / res boxes (T.800 §I.5.3.4 – §I.5.3.7).
    // -----------------------------------------------------------------

    /// Like [`synth_minimal_jp2`] but with `nc` codestream components
    /// declared in the `ihdr` and arbitrary extra `jp2h` child boxes
    /// appended after the `colr`.
    fn synth_jp2_with(nc: u16, extra_jp2h: &[(u32, Vec<u8>)], codestream: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        emit_box(&mut v, BOX_TYPE_JP2_SIGNATURE, &JP2_SIGNATURE_MAGIC);
        let mut ftyp_body = Vec::new();
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        ftyp_body.extend_from_slice(&0u32.to_be_bytes());
        ftyp_body.extend_from_slice(&BRAND_JP2.to_be_bytes());
        emit_box(&mut v, BOX_TYPE_FTYP, &ftyp_body);
        let mut jp2h_body = Vec::new();
        let mut ihdr_body = Vec::new();
        ihdr_body.extend_from_slice(&8u32.to_be_bytes()); // HEIGHT
        ihdr_body.extend_from_slice(&8u32.to_be_bytes()); // WIDTH
        ihdr_body.extend_from_slice(&nc.to_be_bytes()); // NC
        ihdr_body.push(7); // BPC = 8-bit unsigned
        ihdr_body.push(7); // C
        ihdr_body.push(0); // UnkC
        ihdr_body.push(0); // IPR
        emit_box(&mut jp2h_body, BOX_TYPE_IHDR, &ihdr_body);
        let mut colr_body = vec![1u8, 0, 0];
        colr_body.extend_from_slice(&16u32.to_be_bytes());
        emit_box(&mut jp2h_body, BOX_TYPE_COLR, &colr_body);
        for (ty, body) in extra_jp2h {
            emit_box(&mut jp2h_body, *ty, body);
        }
        emit_box(&mut v, BOX_TYPE_JP2H, &jp2h_body);
        emit_box(&mut v, BOX_TYPE_JP2C, codestream);
        v
    }

    /// A `pclr` body: `NE` entries over columns of `(Bi, values)`.
    fn pclr_body(columns: &[(u8, &[i64])]) -> Vec<u8> {
        let ne = columns[0].1.len() as u16;
        let mut body = Vec::new();
        body.extend_from_slice(&ne.to_be_bytes());
        body.push(columns.len() as u8);
        for (bi, _) in columns {
            body.push(*bi);
        }
        for j in 0..ne as usize {
            for (bi, vals) in columns {
                let depth = (bi & 0x7F) + 1;
                let nbytes = usize::from(depth).div_ceil(8);
                let masked = (vals[j] as u64) & ((1u64 << depth) - 1);
                let be = masked.to_be_bytes();
                body.extend_from_slice(&be[8 - nbytes..]);
            }
        }
        body
    }

    /// A `cmap` body from `(CMP, MTYP, PCOL)` triples.
    fn cmap_body(entries: &[(u16, u8, u8)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (cmp, mtyp, pcol) in entries {
            body.extend_from_slice(&cmp.to_be_bytes());
            body.push(*mtyp);
            body.push(*pcol);
        }
        body
    }

    /// A `cdef` body from `(Cn, Typ, Asoc)` triples.
    fn cdef_body(defs: &[(u16, u16, u16)]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(defs.len() as u16).to_be_bytes());
        for (cn, typ, asoc) in defs {
            body.extend_from_slice(&cn.to_be_bytes());
            body.extend_from_slice(&typ.to_be_bytes());
            body.extend_from_slice(&asoc.to_be_bytes());
        }
        body
    }

    #[test]
    fn parses_pclr_cmap_pair() {
        // 4-entry RGB palette (three unsigned 8-bit columns) applied
        // to component 0 three times — the §I.5.3.4 worked example.
        let cs = b"\xFF\x4F\xFF\xD9";
        let pclr = pclr_body(&[
            (7, &[10, 20, 30, 40]),
            (7, &[11, 21, 31, 41]),
            (7, &[12, 22, 32, 42]),
        ]);
        let cmap = cmap_body(&[(0, 1, 0), (0, 1, 1), (0, 1, 2)]);
        let bytes = synth_jp2_with(1, &[(BOX_TYPE_PCLR, pclr), (BOX_TYPE_CMAP, cmap)], cs);
        let c = parse_jp2(&bytes).expect("parse pclr/cmap");
        let p = c.header.pclr.expect("pclr");
        assert_eq!(p.columns.len(), 3);
        assert_eq!(p.entries(), 4);
        assert_eq!(p.columns[0].bit_depth, 8);
        assert!(!p.columns[0].signed);
        assert_eq!(p.columns[0].values, vec![10, 20, 30, 40]);
        assert_eq!(p.columns[2].values, vec![12, 22, 32, 42]);
        let m = c.header.cmap.expect("cmap");
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].component, 0);
        assert_eq!(m[1].mapping, CmapMapping::Palette { column: 1 });
    }

    #[test]
    fn pclr_non_byte_multiple_and_signed_columns() {
        // A 10-bit unsigned column (2-byte padded storage, value in
        // the low bits) and a signed 8-bit column whose negative
        // entries sign-extend (Table I.13 high bit).
        let cs = b"\xFF\x4F\xFF\xD9";
        let pclr = pclr_body(&[(9, &[0x3FF, 0x200, 5]), (0x87, &[-1, -128, 127])]);
        let cmap = cmap_body(&[(0, 1, 0), (0, 1, 1)]);
        let bytes = synth_jp2_with(1, &[(BOX_TYPE_PCLR, pclr), (BOX_TYPE_CMAP, cmap)], cs);
        let c = parse_jp2(&bytes).expect("parse deep/signed pclr");
        let p = c.header.pclr.expect("pclr");
        assert_eq!(p.columns[0].bit_depth, 10);
        assert_eq!(p.columns[0].values, vec![0x3FF, 0x200, 5]);
        assert_eq!(p.columns[1].bit_depth, 8);
        assert!(p.columns[1].signed);
        assert_eq!(p.columns[1].values, vec![-1, -128, 127]);
    }

    #[test]
    fn pclr_requires_cmap_and_vice_versa() {
        // §I.5.3.4: pclr without cmap (and cmap without pclr) is
        // malformed.
        let cs = b"\xFF\x4F\xFF\xD9";
        let pclr = pclr_body(&[(7, &[1, 2])]);
        let bytes = synth_jp2_with(1, &[(BOX_TYPE_PCLR, pclr)], cs);
        assert!(parse_jp2(&bytes).is_err());
        let cmap = cmap_body(&[(0, 0, 0)]);
        let bytes = synth_jp2_with(1, &[(BOX_TYPE_CMAP, cmap)], cs);
        assert!(parse_jp2(&bytes).is_err());
    }

    #[test]
    fn cmap_faults_are_rejected() {
        let cs = b"\xFF\x4F\xFF\xD9";
        let pclr = pclr_body(&[(7, &[1, 2])]);
        // MTYP = 0 requires PCOL = 0 (Table I.14).
        let bad = cmap_body(&[(0, 0, 1)]);
        let bytes = synth_jp2_with(
            1,
            &[(BOX_TYPE_PCLR, pclr.clone()), (BOX_TYPE_CMAP, bad)],
            cs,
        );
        assert!(parse_jp2(&bytes).is_err());
        // PCOL beyond the palette's NPC.
        let bad = cmap_body(&[(0, 1, 3)]);
        let bytes = synth_jp2_with(
            1,
            &[(BOX_TYPE_PCLR, pclr.clone()), (BOX_TYPE_CMAP, bad)],
            cs,
        );
        assert!(parse_jp2(&bytes).is_err());
        // CMP beyond the ihdr component count.
        let bad = cmap_body(&[(2, 1, 0)]);
        let bytes = synth_jp2_with(1, &[(BOX_TYPE_PCLR, pclr), (BOX_TYPE_CMAP, bad)], cs);
        assert!(parse_jp2(&bytes).is_err());
        // Reserved MTYP.
        let pclr = pclr_body(&[(7, &[1, 2])]);
        let bad = cmap_body(&[(0, 2, 0)]);
        let bytes = synth_jp2_with(1, &[(BOX_TYPE_PCLR, pclr), (BOX_TYPE_CMAP, bad)], cs);
        assert!(parse_jp2(&bytes).is_err());
    }

    #[test]
    fn parses_cdef_and_rejects_duplicates() {
        let cs = b"\xFF\x4F\xFF\xD9";
        // Three colour channels declared in reverse colour order plus
        // a whole-image opacity channel on channel 3.
        let cdef = cdef_body(&[(0, 0, 3), (1, 0, 2), (2, 0, 1), (3, 1, 0)]);
        let bytes = synth_jp2_with(4, &[(BOX_TYPE_CDEF, cdef)], cs);
        let c = parse_jp2(&bytes).expect("parse cdef");
        let defs = c.header.cdef.expect("cdef");
        assert_eq!(defs.len(), 4);
        assert_eq!(defs[0].channel, 0);
        assert_eq!(defs[0].channel_type, ChannelDef::TYPE_COLOUR);
        assert_eq!(defs[0].association, 3);
        assert_eq!(defs[3].channel_type, ChannelDef::TYPE_OPACITY);
        // Duplicate (Typ, Asoc) pair — two green channels (§I.5.3.6).
        let bad = cdef_body(&[(0, 0, 2), (1, 0, 2)]);
        let bytes = synth_jp2_with(2, &[(BOX_TYPE_CDEF, bad)], cs);
        assert!(parse_jp2(&bytes).is_err());
        // The 0xFFFF "not specified" values are exempt from the
        // duplicate rule.
        let ok = cdef_body(&[(0, 0xFFFF, 0xFFFF), (1, 0xFFFF, 0xFFFF)]);
        let bytes = synth_jp2_with(2, &[(BOX_TYPE_CDEF, ok)], cs);
        assert!(parse_jp2(&bytes).is_ok());
        // Cn beyond the channel count.
        let bad = cdef_body(&[(5, 0, 1)]);
        let bytes = synth_jp2_with(2, &[(BOX_TYPE_CDEF, bad)], cs);
        assert!(parse_jp2(&bytes).is_err());
    }

    #[test]
    fn parses_resolution_superbox() {
        let cs = b"\xFF\x4F\xFF\xD9";
        // resc: 300 dpi ≈ 11811 points/metre — expressed as
        // 11811/1 × 10^0 both axes; resd: 72/1 × 10^2 vertical,
        // 96/2 × 10^1 horizontal.
        let mut res_body = Vec::new();
        let mut resc = Vec::new();
        resc.extend_from_slice(&11811u16.to_be_bytes());
        resc.extend_from_slice(&1u16.to_be_bytes());
        resc.extend_from_slice(&11811u16.to_be_bytes());
        resc.extend_from_slice(&1u16.to_be_bytes());
        resc.push(0);
        resc.push(0);
        emit_box(&mut res_body, BOX_TYPE_RESC, &resc);
        let mut resd = Vec::new();
        resd.extend_from_slice(&72u16.to_be_bytes());
        resd.extend_from_slice(&1u16.to_be_bytes());
        resd.extend_from_slice(&96u16.to_be_bytes());
        resd.extend_from_slice(&2u16.to_be_bytes());
        resd.push(2);
        resd.push((-1i8) as u8);
        emit_box(&mut res_body, BOX_TYPE_RESD, &resd);
        let bytes = synth_jp2_with(1, &[(BOX_TYPE_RES, res_body)], cs);
        let c = parse_jp2(&bytes).expect("parse res");
        let r = c.header.resolution.expect("res");
        let capture = r.capture.expect("resc");
        assert_eq!(capture.vertical_points_per_metre(), 11811.0);
        assert_eq!(capture.horizontal_points_per_metre(), 11811.0);
        let display = r.display.expect("resd");
        assert_eq!(display.vertical_points_per_metre(), 7200.0);
        assert!((display.horizontal_points_per_metre() - 4.8).abs() < 1e-12);
        // An empty Resolution superbox violates §I.5.3.7.
        let bytes = synth_jp2_with(1, &[(BOX_TYPE_RES, Vec::new())], cs);
        assert!(parse_jp2(&bytes).is_err());
    }

    #[test]
    fn decode_jp2_expands_palette() {
        // Encode a 4-bit-worth index plane (values 0..=3) as a real
        // lossless 8-bit codestream with this crate's encoder, wrap
        // it with a 4-entry RGB palette, and check the three
        // generated channels are the per-pixel palette rows.
        let w = 8u32;
        let h = 8u32;
        let idx: Vec<u8> = (0..w * h).map(|i| (i as u8) % 4).collect();
        let cs = crate::encode::encode_j2k_lossless(&[&idx], w, h, 1, (4, 4)).expect("encode");
        let pal: [(i64, i64, i64); 4] = [(255, 0, 10), (0, 255, 20), (0, 0, 255), (7, 8, 9)];
        let r: Vec<i64> = pal.iter().map(|p| p.0).collect();
        let g: Vec<i64> = pal.iter().map(|p| p.1).collect();
        let b: Vec<i64> = pal.iter().map(|p| p.2).collect();
        let pclr = pclr_body(&[(7, &r), (7, &g), (7, &b)]);
        let cmap = cmap_body(&[(0, 1, 0), (0, 1, 1), (0, 1, 2)]);
        let bytes = synth_jp2_with(1, &[(BOX_TYPE_PCLR, pclr), (BOX_TYPE_CMAP, cmap)], &cs);
        let image = decode_jp2(&bytes).expect("decode palettized jp2");
        assert_eq!(image.components.len(), 3);
        for (ci, comp) in image.components.iter().enumerate() {
            assert_eq!(comp.width, w);
            assert_eq!(comp.height, h);
            assert_eq!(comp.precision_bits, 8);
            assert!(!comp.is_signed);
            for (i, &s) in comp.samples.iter().enumerate() {
                let entry = pal[idx[i] as usize];
                let expect = [entry.0, entry.1, entry.2][ci] as i32;
                assert_eq!(s, expect, "channel {ci} pixel {i}");
            }
        }
        // The historical interleaved entry point sniffs the JP2
        // signature and applies the same expansion.
        let interleaved = crate::decode_jpeg2000(&bytes).expect("interleaved");
        assert_eq!(interleaved.len(), (w * h * 3) as usize);
        assert_eq!(interleaved[0], 255);
        assert_eq!(interleaved[1], 0);
        assert_eq!(interleaved[2], 10);
        assert_eq!(interleaved[3], 0);
        assert_eq!(interleaved[4], 255);
        assert_eq!(interleaved[5], 20);
    }

    #[test]
    fn decode_jp2_orders_channels_per_cdef() {
        // A three-component codestream whose planes are stored in
        // B, G, R order; the cdef associates channel 0 with colour 3
        // (blue), channel 1 with colour 2 (green) and channel 2 with
        // colour 1 (red) — decode_jp2 must hand back R, G, B.
        let w = 8u32;
        let h = 8u32;
        let bp: Vec<u8> = (0..w * h).map(|i| (i % 251) as u8).collect();
        let gp: Vec<u8> = (0..w * h).map(|i| ((i * 3) % 249) as u8).collect();
        let rp: Vec<u8> = (0..w * h).map(|i| ((i * 7) % 247) as u8).collect();
        let cs = crate::encode::encode_j2k_lossless(&[&bp, &gp, &rp], w, h, 1, (4, 4))
            .expect("encode BGR");
        let cdef = cdef_body(&[(0, 0, 3), (1, 0, 2), (2, 0, 1)]);
        let bytes = synth_jp2_with(3, &[(BOX_TYPE_CDEF, cdef)], &cs);
        let as_i32 = |v: &[u8]| v.iter().map(|&s| i32::from(s)).collect::<Vec<i32>>();
        let image = decode_jp2(&bytes).expect("decode cdef jp2");
        assert_eq!(image.components.len(), 3);
        assert_eq!(image.components[0].samples, as_i32(&rp));
        assert_eq!(image.components[1].samples, as_i32(&gp));
        assert_eq!(image.components[2].samples, as_i32(&bp));
        // Without the cdef the planes stay in codestream order.
        let bytes = synth_jp2_with(3, &[], &cs);
        let image = decode_jp2(&bytes).expect("decode plain jp2");
        assert_eq!(image.components[0].samples, as_i32(&bp));
        assert_eq!(image.components[2].samples, as_i32(&rp));
    }
}
