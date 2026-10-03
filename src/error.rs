//! Crate-local error type: [`Jpeg2000Error`] and its contract alias
//! [`Error`].
//!
//! The standalone (no `oxideav-core`) API returns this type everywhere;
//! with the `registry` feature, [`crate::registry`] adds the
//! `From<Jpeg2000Error> for oxideav_core::Error` mapping so the
//! framework `Decoder` / `Encoder` stay thin adapters.

/// Contract alias for [`Jpeg2000Error`] (image-crate API contract):
/// `oxideav_jpeg2000::Error`.
pub type Error = Jpeg2000Error;

/// Error type of `oxideav-jpeg2000` (the contract name is the
/// [`Error`] alias).
///
/// The four contract variants come first — [`InvalidData`],
/// [`Unsupported`], [`LimitExceeded`] and [`Io`] — followed by the
/// T.800 / T.814 diagnostic variants the codestream parser, tier-2
/// walker and tier-1 engines raise (each names the clause it enforces).
/// Every diagnostic variant is a *malformed-stream* report except
/// [`NotImplemented`], which is the codestream-tool spelling of
/// "unsupported" (a coding tool or layout this crate does not wire).
///
/// `Io` carries the `std::io::Error`, so the enum derives neither
/// `Clone` nor `PartialEq`; tests match on variants or `Display`.
///
/// [`InvalidData`]: Self::InvalidData
/// [`Unsupported`]: Self::Unsupported
/// [`LimitExceeded`]: Self::LimitExceeded
/// [`Io`]: Self::Io
/// [`NotImplemented`]: Self::NotImplemented
#[derive(Debug)]
#[non_exhaustive]
pub enum Jpeg2000Error {
    /// The input is not a well-formed JPEG 2000 codestream or JP2 /
    /// JPH file, or a caller-assembled [`crate::Jpeg2000Image`] has
    /// inconsistent geometry. The string names what was wrong.
    InvalidData(String),
    /// The file uses a layout or feature the contract surface cannot
    /// express or this crate does not implement — signed or
    /// deeper-than-16-bit components, component sets with no
    /// [`crate::PixelFormat`] (see the README layout table), an
    /// encoder input the format cannot carry. The depth API
    /// ([`crate::decode_j2k`] / [`crate::jp2::decode_jp2`]) still
    /// reaches every component plane of such files.
    Unsupported(String),
    /// A [`crate::DecodeOptions`] limit (`max_width` / `max_height` /
    /// `max_pixels` / `max_bytes`) would be exceeded. Raised from the
    /// `SIZ` geometry before any sample plane is allocated.
    LimitExceeded(String),
    /// A read ([`crate::decode_from`]) or write ([`crate::encode_to`])
    /// on a caller-supplied stream failed.
    Io(std::io::Error),
    /// The codestream needs a coding tool the decode wiring does not
    /// handle yet (e.g. a non-Maxshift / Part-2 `RGN` style, or a `COC`
    /// whose Table A.19 code-block-**style** byte diverges from the
    /// `COD`) — or the encoder was asked for a layout outside its
    /// lossless-5-3 surface (see [`crate::encode::encode_j2k_lossless`]). The
    /// `COC` / `QCC` / `RGN` / `POC` overrides, `PPM` / `PPT` relocated
    /// headers, all five §B.12.1 progression orders, and the §A.6.6
    /// progression order change *are* honoured on the decode side. See
    /// [`mod@crate::decode`] for the supported surface.
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
    /// Round-5 packet-header reader hit an invalid bit-sequence or a
    /// geometry mismatch (T.800 §B.10).
    InvalidPacketHeader,
    /// A `COD` / `COC` user-defined precinct exponent violated the
    /// T.800 §B.6 / Table A.21 constraint that `PPx` / `PPy` "may only
    /// equal zero at the resolution level corresponding to the `NLLL`
    /// band" — i.e. a `PPx = 0` or `PPy = 0` was signalled at `r > 0`.
    /// No conforming encoder can have produced the stream, so it is
    /// rejected rather than decoded against a precinct lattice the
    /// packet sequence cannot match.
    InvalidPrecinctSize,
    /// Round-5 packet-header walker advanced past the end of the
    /// tile-part body before all geometry-required packets were
    /// decoded.
    PacketHeaderOverrun,
    /// The T.800 §D.5 error-resilience segmentation symbol decoded at
    /// the end of a cleanup pass was not the required value `0xA`
    /// (binary `1010`). Indicates that bit errors corrupted this
    /// bit-plane's compressed image data.
    SegmentationSymbolMismatch,
    /// A main-header `TLM` marker segment (T.800 §A.7.1) disagrees
    /// with the actual tile-part chain: entry count != tile-part
    /// count, a `Ttlm` tile index != the corresponding `Isot`, a
    /// `Ptlm` length != the tile-part's real `SOT`-to-body-end span
    /// (== `Psot`), an `ST = 0` layout without one-tile-part-per-tile
    /// in index order, or duplicate / malformed `Ztlm` sequencing.
    /// Signals a lost, reordered or corrupted tile-part.
    TlmMismatch,
    /// A tile-part's `PLT` packet-length list (T.800 §A.7.3) disagrees
    /// with the actual packets: entry count != the number of packets
    /// starting in that tile-part, an `Iplt` != the packet's real byte
    /// span, an incomplete trailing `Iplt`, or duplicate `Zplt`
    /// sequencing. Signals a lost, resized or reordered packet.
    PltMismatch,
    /// An HTJ2K (ITU-T T.814 | ISO/IEC 15444-15) HT segment did not
    /// conform to the §7.1 bit-stream-recovery constraints — e.g. a
    /// stuffing bit was non-zero, a state machine ran past its bound,
    /// or a CxtVLC codeword failed to match any Annex C table entry.
    /// The §7.1.1 `error()` state.
    HtCorruptSegment,
}

impl Jpeg2000Error {
    /// Construct an [`Jpeg2000Error::InvalidData`] from a message.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidData(msg.into())
    }

    /// Construct an [`Jpeg2000Error::Unsupported`] from a message.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    /// Construct an [`Jpeg2000Error::LimitExceeded`] from a message.
    pub fn limit(msg: impl Into<String>) -> Self {
        Self::LimitExceeded(msg.into())
    }
}

impl From<std::io::Error> for Jpeg2000Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl core::fmt::Display for Jpeg2000Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::InvalidData(s) => write!(f, "JPEG 2000: invalid data: {s}"),
            Error::Unsupported(s) => write!(f, "JPEG 2000: unsupported: {s}"),
            Error::LimitExceeded(s) => write!(f, "JPEG 2000: limit exceeded: {s}"),
            Error::Io(e) => write!(f, "JPEG 2000: io error: {e}"),
            Error::NotImplemented => write!(
                f,
                "oxideav-jpeg2000: codestream uses a coding tool not yet wired (or encoder entry point)"
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
            Error::InvalidPacketHeader => {
                write!(f, "JPEG 2000: malformed packet header (T.800 §B.10)")
            }
            Error::InvalidPrecinctSize => {
                write!(
                    f,
                    "JPEG 2000: precinct exponent PPx/PPy = 0 at resolution level r > 0 (T.800 §B.6)"
                )
            }
            Error::PacketHeaderOverrun => {
                write!(
                    f,
                    "JPEG 2000: packet-header walker overran the tile-part body"
                )
            }
            Error::SegmentationSymbolMismatch => write!(
                f,
                "JPEG 2000: §D.5 segmentation symbol decoded != 0xA (bit-plane corruption)"
            ),
            Error::TlmMismatch => write!(
                f,
                "oxideav-jpeg2000: TLM tile-part lengths disagree with the actual tile-part chain (T.800 A.7.1)"
            ),
            Error::PltMismatch => write!(
                f,
                "oxideav-jpeg2000: PLT packet lengths disagree with the actual packets (T.800 A.7.3)"
            ),
            Error::HtCorruptSegment => write!(
                f,
                "JPEG 2000: malformed HTJ2K HT segment (T.814 §7.1 error())"
            ),
        }
    }
}

impl std::error::Error for Jpeg2000Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
