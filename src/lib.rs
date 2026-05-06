//! JPEG 2000 (ISO/IEC 15444) codec.
//!
//! Scope of this crate today:
//!
//! - A pure-Rust parser for the Part-1 J2K codestream marker chain —
//!   SOC, SIZ, COD, QCD, COC, QCC, RGN, POC, PPM, PPT, PLM, PLT, TLM,
//!   CRG, COM, SOT, SOD, EOC. See [`codestream::parse`]. This recovers
//!   image geometry, per-component bit depth, signedness, sub-sampling,
//!   and the byte ranges of each tile-part's compressed payload.
//! - A Part-1 **sample decoder** that reconstructs pixels from a
//!   codestream. The decoder covers:
//!   - MQ arithmetic coder (47-state, ported from OpenJPEG).
//!   - EBCOT tier-1 passes: significance propagation, magnitude
//!     refinement, cleanup.
//!   - Tier-2 packet header parsing + inclusion / zero-bitplane tag
//!     trees.
//!   - Inverse 5/3 integer reversible lifting (Part-1 lossless) and
//!     9/7 irreversible float lifting (end-to-end; RGB → YCbCr inverse
//!     ICT for 3-component streams).
//!   - DC level-shift, clipping, reversible component transform (RCT)
//!     for 3-component 5/3 streams and irreversible component
//!     transform (ICT) for 3-component 9/7 streams.
//!   - All five Part-1 progression orders — LRCP, RLCP, RPCL, PCRL,
//!     CPRL (§B.12.1.1–B.12.1.5). User-defined precinct partitions
//!     (§A.6.1 / §B.6) honoured for every order. Multiple quality
//!     layers (per T.800 §B.10 — accumulated coding-pass contributions
//!     across packets).
//!   - **POC marker** (§A.6.6 / §B.12.2 / §B.12.3): mid-stream
//!     progression-order changes. Each progression-order volume
//!     specifies `(RSpoc, CSpoc, LYEpoc, REpoc, CEpoc, Ppoc)` and is
//!     processed in order; per-(component, resolution, precinct) layer
//!     counters advance across volumes per the spec rule "the layer
//!     always starts with the next one for a given tile-component,
//!     resolution level and precinct". POC may appear in the main
//!     header (applies to all tiles) or per-tile-part-header (override).
//!   - **Packed packet headers** — PPM (§A.7.4, main header) and PPT
//!     (§A.7.5, tile-part header). When present, the tier-2 walker
//!     reads packet headers from the packed buffer instead of from the
//!     compressed body. PPM segments are sorted by Zppm and the
//!     resulting concatenated stream is split into per-tile-part
//!     header chunks (`Nppm`-prefixed). PPT segments are sorted by
//!     Zppt within each tile.
//!   - Multi-tile decode (§B.3): the frame-level driver walks the
//!     tile grid, groups tile-parts by `Isot`, decodes each tile in
//!     isolation (per-tile RCT / ICT per §G.1 / §G.2), and pastes
//!     the result into the assembled image.
//! - A Part-1 **sample encoder** that writes `.j2k` codestreams (or
//!   `.jp2` containers) for 8-bit Gray / RGB input. Supports both
//!   transforms:
//!     - 5/3 integer reversible (bit-exact lossless) with optional
//!       forward RCT (§G.1) for 3-channel input.
//!     - 9/7 irreversible float with per-band scalar quantisation and
//!       optional forward ICT (§G.2).
//!
//!   Pipeline mirrors the decoder: forward DWT, MQ encoder, EBCOT
//!   tier-1 passes, tier-2 packet construction (inclusion +
//!   zero-bitplane tag trees, adaptive Lblock, comma-coded pass
//!   count), and the SOC / SIZ / COD / QCD / SOT / SOD / EOC marker
//!   chain. The encoder honours
//!   [`encode::EncodeOptions::progression`] for all five Part-1
//!   progression orders, optionally schedules them via
//!   [`encode::EncodeOptions::poc`] (POC marker, T.800 §A.6.6), and
//!   can pack packet headers into a main-header `PPM` segment or a
//!   per-tile-part `PPT` segment via
//!   [`encode::EncodeOptions::packet_header_placement`]
//!   (T.800 §A.7.4 / §A.7.5). Setting `EncodeOptions::jp2_wrapper`
//!   additionally emits the ISOBMFF `jP  ` + `ftyp` + `jp2h` + `jp2c`
//!   boxes from ISO/IEC 15444-1 Annex I; the decoder auto-detects and
//!   strips the wrapper on input.
//!
//! HTJ2K (ISO/IEC 15444-15) — opt-in via the `htj2k` Cargo feature:
//!
//! - **CAP / CPF / PRF marker parsing** in [`codestream::parse`]:
//!   `Pcap15` (mask `0x0002_0000`) discriminates HT codestreams from
//!   classic Part-1 ones, and the `Ccap15` sub-profile bits + `CPFnum`
//!   are surfaced via [`Probe`] / [`Cap`] / [`Cpf`].
//! - **FBCOT entropy decoder** in
//!   [`decode::htj2k::decode_codeblock`] — the three HT passes
//!   (cleanup + SigProp + MagRef) per Annex B of 15444-15, with both
//!   CxtVLC tables of Annex C transcribed verbatim.
//! - **Tier-2 packet walker** in [`decode::htj2k::decode_frame_htj2k`]
//!   that reuses the Part-1 packet header syntax (T.800 §B.10) and
//!   routes each codeblock's bytes through the FBCOT decoder. Handles
//!   single-tile, single-layer, LRCP-only HT codestreams. Round 4
//!   adds multi-pass codeblock dispatch (Z_blk in {2, 3} → Lcup/Lref
//!   split) and routes the existing 9/7 irreversible IDWT through
//!   the FBCOT path.
//!
//! What is not here yet:
//!
//! - Encoder input pixel formats beyond `Gray8` / `Rgb24` 8-bit, and
//!   the encoder still emits a single quality layer with default
//!   precincts (one packet per (component, resolution)).
//! - HTJ2K encoder. Round 3 only ships the decoder side.
//!
//! ## Standalone vs registry-integrated
//!
//! The crate's default `registry` Cargo feature pulls in `oxideav-core`
//! and exposes the `Decoder`/`Encoder` trait surface plus a
//! [`registry::register`] entry point. Disable the feature
//! (`default-features = false`) for an oxideav-core-free build that
//! still exposes the standalone [`decode_jpeg2000`] / [`encode_jpeg2000`]
//! API plus the underlying `codestream` / `decode` / `encode` modules.

pub mod codestream;
pub mod decode;
pub mod encode;
pub mod error;
pub mod image;

#[cfg(feature = "registry")]
pub mod registry;

#[cfg(feature = "registry")]
pub use registry::{
    __oxideav_entry, register, register_codecs, register_containers, J2kDecoder, J2kEncoder,
};

pub use codestream::{Cap, Codestream, ComponentInfo, Cpf, Marker, Siz, TilePart};
pub use error::{Jpeg2000Error, Result};
pub use image::{Jpeg2000Image, Jpeg2000PixelFormat, Jpeg2000Plane};

/// Public codec id string. Matches the Cargo features `jpeg2000` / `jp2`
/// in the aggregator crate.
///
/// Note: HTJ2K (ISO/IEC 15444-15) reuses this codec id — the
/// distinction is signalled inside the codestream via the `CAP` marker
/// (Pcap bit 15). See [`probe`] for run-time discrimination.
pub const CODEC_ID_STR: &str = "jpeg2000";

/// Result of [`probe`]: a quick classification of a JPEG 2000 buffer
/// (raw `.j2k` codestream or JP2 ISOBMFF wrapper) without running the
/// full sample decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// Image canvas width in pixels (`Xsiz - XOsiz`).
    pub width: u32,
    /// Image canvas height in pixels (`Ysiz - YOsiz`).
    pub height: u32,
    /// Number of image components.
    pub num_components: usize,
    /// Block-coding flavour signalled by the codestream.
    pub flavour: J2kFlavour,
    /// `Pcap` value from the `CAP` marker, if present. `None` for
    /// classic Part-1 streams that omit `CAP` entirely.
    pub pcap: Option<u32>,
    /// `Ccap15` (HTJ2K sub-profile bits) when `flavour == HighThroughput`.
    pub ccap15: Option<u16>,
    /// `CPFnum` value from the `CPF` marker, if present
    /// (HTJ2K only, ISO/IEC 15444-15 §A.6).
    pub cpfnum: Option<u128>,
}

/// JPEG 2000 block-coding flavour reported by [`probe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum J2kFlavour {
    /// Classic Part-1 EBCOT block coder (ISO/IEC 15444-1, T.800).
    /// Either no `CAP` marker is present, or `CAP` is present but
    /// `Pcap15` is 0.
    ClassicPart1,
    /// High-Throughput JPEG 2000 (ISO/IEC 15444-15, T.814) — `CAP`
    /// marker is present and `Pcap15` is 1, indicating the FBCOT
    /// block coder is in use for at least some code-blocks.
    HighThroughput,
}

/// Quickly classify a JPEG 2000 buffer.
///
/// The buffer may be a raw `.j2k` codestream (starting with `FF 4F`
/// SOC) or a JP2 ISOBMFF container (starting with the 12-byte JP2
/// signature box). Returns the image geometry, component count, and
/// the block-coding flavour (classic Part-1 vs HTJ2K). HTJ2K
/// detection follows ISO/IEC 15444-15 §A.3.1: the `CAP` marker
/// segment must be present and `Pcap15` (mask `0x0002_0000` —
/// the 15th most-significant bit of the 32-bit `Pcap`) must be 1.
///
/// This is decoder-side only — it parses the marker chain but does
/// not touch the compressed sample data, and is therefore safe and
/// fast to call on untrusted input.
pub fn probe(buf: &[u8]) -> Result<Probe> {
    let jp2_signature = [
        0x00, 0x00, 0x00, 0x0C, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A,
    ];
    let data: Vec<u8> = if buf.len() >= 12 && buf[..12] == jp2_signature {
        encode::codestream::extract_jp2_codestream(buf)?
    } else {
        buf.to_vec()
    };
    let cs = codestream::parse(&data)?;
    let flavour = if cs.is_htj2k() {
        J2kFlavour::HighThroughput
    } else {
        J2kFlavour::ClassicPart1
    };
    let pcap = cs.cap.as_ref().map(|c| c.pcap);
    let ccap15 = cs.cap.as_ref().and_then(|c| c.ccap15());
    let cpfnum = cs.cpf.as_ref().map(|c| c.cpfnum);
    Ok(Probe {
        width: cs.siz.image_width(),
        height: cs.siz.image_height(),
        num_components: cs.siz.num_components(),
        flavour,
        pcap,
        ccap15,
        cpfnum,
    })
}

/// Standalone decode entry point.
///
/// Accepts either a raw `.j2k` codestream (starting with `FF 4F` SOC)
/// or a JP2 ISOBMFF container (starting with the 12-byte JP2 signature
/// box) and returns the decoded image. With the default `registry`
/// feature on, [`registry::J2kDecoder`] wraps this for the
/// `oxideav-core` `Decoder` trait surface.
pub fn decode_jpeg2000(buf: &[u8]) -> Result<Jpeg2000Image> {
    let jp2_signature = [
        0x00, 0x00, 0x00, 0x0C, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A,
    ];
    let data: Vec<u8> = if buf.len() >= 12 && buf[..12] == jp2_signature {
        encode::codestream::extract_jp2_codestream(buf)?
    } else {
        buf.to_vec()
    };
    let cs = codestream::parse(&data)?;
    #[cfg(feature = "htj2k")]
    if cs.is_htj2k() {
        return decode::htj2k::decode_frame_htj2k(&cs, &data);
    }
    decode::frame::decode_frame(&cs, &data)
}

/// Standalone encode entry point.
///
/// Encodes the image into a `.j2k` codestream (or `.jp2` container if
/// `opts.jp2_wrapper` is true). With the default `registry` feature on,
/// [`registry::J2kEncoder`] wraps this for the `oxideav-core` `Encoder`
/// trait surface.
pub fn encode_jpeg2000(image: &Jpeg2000Image, opts: &encode::EncodeOptions) -> Result<Vec<u8>> {
    encode::codestream::encode_image(image, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_probe_reports_geometry_on_tiny() {
        let buf = build_tiny_j2k();
        // The hand-crafted stream has valid markers but no useful
        // payload, so probe (which is marker-only) should succeed.
        let p = probe(&buf).expect("probe");
        assert_eq!(p.width, 4);
        assert_eq!(p.height, 3);
        assert_eq!(p.num_components, 1);
        assert_eq!(p.flavour, J2kFlavour::ClassicPart1);
    }

    fn build_tiny_j2k() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0xFF, 0x4F]);
        v.extend_from_slice(&[0xFF, 0x51]);
        v.extend_from_slice(&41u16.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&4u32.to_be_bytes());
        v.extend_from_slice(&3u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&4u32.to_be_bytes());
        v.extend_from_slice(&3u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&1u16.to_be_bytes());
        v.extend_from_slice(&[7, 1, 1]);
        v.extend_from_slice(&[0xFF, 0x52]);
        v.extend_from_slice(&12u16.to_be_bytes());
        v.extend_from_slice(&[0, 0, 0, 0, 0, 5, 4, 4, 0, 0]);
        v.extend_from_slice(&[0xFF, 0x5C]);
        v.extend_from_slice(&5u16.to_be_bytes());
        v.extend_from_slice(&[0x00, 0x00, 0x00]);
        let sot_marker_off = v.len();
        v.extend_from_slice(&[0xFF, 0x90]);
        v.extend_from_slice(&10u16.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes());
        let psot_pos = v.len();
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&[0, 1]);
        v.extend_from_slice(&[0xFF, 0x93]);
        v.extend_from_slice(&[0x00, 0x00]);
        let tile_part_end = v.len();
        let psot = (tile_part_end - sot_marker_off) as u32;
        v[psot_pos..psot_pos + 4].copy_from_slice(&psot.to_be_bytes());
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }
}
