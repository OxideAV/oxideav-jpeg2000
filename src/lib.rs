//! JPEG 2000 (ISO/IEC 15444) codec.
//!
//! Scope of this crate today:
//!
//! - A pure-Rust parser for the Part-1 J2K codestream marker chain —
//!   SOC, SIZ, COD, QCD, COC, QCC, RGN, POC, PPM, PPT, PLM, PLT, TLM,
//!   CRG, COM, SOT, SOD, EOC. See [`codestream::parse`]. This recovers
//!   image geometry, per-component bit depth, signedness, sub-sampling,
//!   and the byte ranges of each tile-part's compressed payload.
//! - A registered decoder stub that reads enough of the codestream to
//!   expose those parameters and then refuses the actual decode call
//!   with [`Error::Unsupported`][oxideav_core::Error::Unsupported].
//!
//! What is not here yet:
//!
//! - The two wavelet transforms (5/3 integer reversible, 9/7
//!   irreversible).
//! - The MQ arithmetic coder.
//! - EBCOT tier-1 bit-plane coding and tier-2 packet parsing.
//! - The JP2 ISOBMFF box wrapper (`.jp2` with the
//!   `00 00 00 0C 6A 50 20 20 0D 0A 87 0A` signature box + JP2 Colour
//!   Specification / Metadata boxes). `.j2k` raw codestreams are what
//!   the parser accepts.
//! - Any encoder.
//!
//! The parser is exposed publicly so container crates or higher-level
//! tooling can probe a file for image geometry without needing to wait
//! for the sample decoder to land.

pub mod codestream;

use oxideav_codec::{CodecInfo, CodecRegistry, Decoder, Encoder};
use oxideav_core::{CodecCapabilities, CodecId, CodecParameters, Error, Frame, Packet, Result};

pub use codestream::{Codestream, ComponentInfo, Marker, Siz, TilePart};

/// Public codec id string. Matches the Cargo features `jpeg2000` / `jp2`
/// in the aggregator crate.
pub const CODEC_ID_STR: &str = "jpeg2000";

/// Register the JPEG 2000 decoder + encoder factories.
///
/// The decoder factory constructs a parser-only decoder: calling
/// `send_packet` runs [`codestream::parse`] on the payload and stashes
/// the resulting [`Codestream`] for inspection, but `receive_frame`
/// always errors with [`Error::Unsupported`] — sample decoding (DWT, MQ
/// coder, EBCOT) is not implemented yet.
pub fn register(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("jpeg2000_stub")
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
    Ok(Box::new(J2kParseOnlyDecoder::new(params.codec_id.clone())))
}

fn make_encoder(_params: &CodecParameters) -> Result<Box<dyn Encoder>> {
    Err(Error::unsupported("jpeg2000: encoder not yet implemented"))
}

/// Decoder adapter that parses J2K marker chains but cannot produce
/// pixels yet. `send_packet` runs the full Part-1 marker parse on the
/// packet payload; `receive_frame` always returns
/// [`Error::Unsupported`].
pub struct J2kParseOnlyDecoder {
    codec_id: CodecId,
    last: Option<Codestream>,
}

impl J2kParseOnlyDecoder {
    pub fn new(codec_id: CodecId) -> Self {
        Self {
            codec_id,
            last: None,
        }
    }

    /// Last successfully parsed codestream, if any.
    pub fn last_parsed(&self) -> Option<&Codestream> {
        self.last.as_ref()
    }
}

impl Decoder for J2kParseOnlyDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        let cs = codestream::parse(&packet.data)?;
        self.last = Some(cs);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        Err(Error::unsupported("jpeg2000 decode not yet implemented"))
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.last = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_exposes_geometry_then_refuses_decode() {
        let buf = build_tiny_j2k();
        let mut reg = CodecRegistry::new();
        register(&mut reg);
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        let mut dec = reg.make_decoder(&params).expect("factory must succeed");
        let pkt = Packet::new(0, oxideav_core::TimeBase::new(1, 1), buf);
        dec.send_packet(&pkt).expect("parse must succeed");
        match dec.receive_frame() {
            Err(Error::Unsupported(msg)) => {
                assert!(msg.contains("jpeg2000"), "unexpected message: {msg}");
            }
            Err(other) => panic!("expected Unsupported, got {other:?}"),
            Ok(_) => panic!("expected Unsupported, got a frame"),
        }
    }

    #[test]
    fn direct_decoder_reports_geometry() {
        let buf = build_tiny_j2k();
        let mut dec = J2kParseOnlyDecoder::new(CodecId::new(CODEC_ID_STR));
        let pkt = Packet::new(0, oxideav_core::TimeBase::new(1, 1), buf);
        dec.send_packet(&pkt).unwrap();
        let cs = dec.last_parsed().expect("parsed codestream");
        assert_eq!(cs.siz.image_width(), 4);
        assert_eq!(cs.siz.image_height(), 3);
        assert_eq!(cs.siz.num_components(), 1);
        assert_eq!(cs.tile_parts.len(), 1);
    }

    #[test]
    fn encoder_factory_still_unsupported() {
        let mut reg = CodecRegistry::new();
        register(&mut reg);
        let params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        match reg.make_encoder(&params) {
            Err(Error::Unsupported(_)) => {}
            Err(other) => panic!("expected Unsupported, got error {other:?}"),
            Ok(_) => panic!("expected Unsupported, got a live encoder"),
        }
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
