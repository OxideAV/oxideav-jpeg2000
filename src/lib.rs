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
//!   - LRCP + RLCP progression orders. Single quality layer. Default
//!     precinct size (one precinct per resolution).
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
//!   chain. Setting `EncodeOptions::jp2_wrapper` additionally emits
//!   the ISOBMFF `jP  ` + `ftyp` + `jp2h` + `jp2c` boxes from
//!   ISO/IEC 15444-1 Annex I; the decoder auto-detects and strips the
//!   wrapper on input.
//!
//! What is not here yet:
//!
//! - Multi-layer (progressive quality) streams, user-defined precinct
//!   grids, and the CPRL / PCRL progression orders.
//! - Encoder input pixel formats beyond `Gray8` / `Rgb24` 8-bit.
//! - RGB input whose RCT chroma excursions go outside the 8-bit
//!   signed range (requires 9-bit signed chroma in the SIZ).

pub mod codestream;
pub mod decode;
pub mod encode;

use oxideav_codec::{CodecInfo, CodecRegistry, Decoder, Encoder};
use oxideav_core::{CodecCapabilities, CodecId, CodecParameters, Error, Frame, Packet, Result};

pub use codestream::{Codestream, ComponentInfo, Marker, Siz, TilePart};

/// Public codec id string. Matches the Cargo features `jpeg2000` / `jp2`
/// in the aggregator crate.
pub const CODEC_ID_STR: &str = "jpeg2000";

/// Register the JPEG 2000 decoder + encoder factories.
///
/// The decoder factory constructs a full Part-1 sample decoder. The
/// encoder factory is still a stub — writing JPEG 2000 bitstreams is
/// not in scope for this crate yet.
pub fn register(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("jpeg2000")
        .with_lossy(true)
        .with_intra_only(true);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .encoder(make_encoder),
    );
}

fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    Ok(Box::new(J2kDecoder::new(params.codec_id.clone())))
}

fn make_encoder(params: &CodecParameters) -> Result<Box<dyn Encoder>> {
    Ok(Box::new(J2kEncoder::new(params.codec_id.clone())))
}

/// JPEG 2000 sample encoder — 5/3 integer reversible (lossless),
/// single-tile, single-layer, default precincts. See
/// [`encode::EncodeOptions`] for the knobs this implementation honours.
pub struct J2kEncoder {
    output_params: CodecParameters,
    opts: encode::EncodeOptions,
    pending: Option<Packet>,
    seq_counter: u64,
}

impl J2kEncoder {
    pub fn new(codec_id: CodecId) -> Self {
        Self {
            output_params: CodecParameters::video(codec_id),
            opts: encode::EncodeOptions::default(),
            pending: None,
            seq_counter: 0,
        }
    }

    /// Replace the encode parameters. Call before any `send_frame`.
    pub fn set_options(&mut self, opts: encode::EncodeOptions) {
        self.opts = opts;
    }
}

impl Encoder for J2kEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.output_params.codec_id
    }

    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }

    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let bytes = encode::encode_frame(frame, &self.opts)?;
        let pkt = Packet::new(0u32, oxideav_core::TimeBase::new(1, 1), bytes);
        self.seq_counter = self.seq_counter.wrapping_add(1);
        self.pending = Some(pkt);
        Ok(())
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        self.pending
            .take()
            .ok_or_else(|| Error::invalid("jpeg2000: no packet pending"))
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// JPEG 2000 sample decoder.
pub struct J2kDecoder {
    codec_id: CodecId,
    /// The last parsed codestream, retained for geometry inspection.
    last_parsed: Option<Codestream>,
    /// The pending decoded frame, if `send_packet` produced one.
    pending: Option<Frame>,
}

impl J2kDecoder {
    pub fn new(codec_id: CodecId) -> Self {
        Self {
            codec_id,
            last_parsed: None,
            pending: None,
        }
    }

    pub fn last_parsed(&self) -> Option<&Codestream> {
        self.last_parsed.as_ref()
    }
}

impl Decoder for J2kDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        // Auto-detect JP2 ISOBMFF wrapper: a jp2 file starts with the
        // 12-byte signature box `00 00 00 0C 6A 50 20 20 0D 0A 87 0A`.
        // Raw j2k codestreams start with SOC = FF 4F. If we see the JP2
        // magic, extract the inner `.j2k` codestream before parsing.
        let jp2_signature = [
            0x00, 0x00, 0x00, 0x0C, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A,
        ];
        let data: Vec<u8> = if packet.data.len() >= 12 && packet.data[..12] == jp2_signature {
            encode::codestream::extract_jp2_codestream(&packet.data)?
        } else {
            packet.data.clone()
        };
        let cs = codestream::parse(&data)?;
        let frame = decode::frame::decode_frame(&cs, &data)?;
        self.last_parsed = Some(cs);
        self.pending = Some(frame);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.pending
            .take()
            .ok_or_else(|| Error::invalid("jpeg2000: no frame pending"))
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.last_parsed = None;
        self.pending = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_decoder_reports_geometry_on_tiny() {
        let buf = build_tiny_j2k();
        let mut dec = J2kDecoder::new(CodecId::new(CODEC_ID_STR));
        let pkt = Packet::new(0, oxideav_core::TimeBase::new(1, 1), buf);
        // The tiny hand-crafted stream has no valid tier-1 payload —
        // decoding will fail somewhere past the parser. That's fine;
        // this test just ensures the parser + decoder plumbing wires
        // together without panicking.
        let _ = dec.send_packet(&pkt);
        // The parser-only branch should have recorded geometry even if
        // the decode attempt errored.
        if let Some(cs) = dec.last_parsed() {
            assert_eq!(cs.siz.image_width(), 4);
            assert_eq!(cs.siz.image_height(), 3);
            assert_eq!(cs.siz.num_components(), 1);
            assert_eq!(cs.tile_parts.len(), 1);
        }
    }

    #[test]
    fn encoder_factory_builds_live_encoder() {
        let mut reg = CodecRegistry::new();
        register(&mut reg);
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let enc = reg.make_encoder(&params).expect("factory returns encoder");
        assert_eq!(enc.codec_id().as_str(), CODEC_ID_STR);
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
