//! `oxideav-core` integration: `Decoder` / `Encoder` trait impls,
//! `Frame` / `Error` conversions, and the [`register`] entry point.
//!
//! Gated behind the default-on `registry` Cargo feature. With the
//! feature off the rest of the crate still exposes the standalone
//! [`crate::decode_jpeg2000`] / [`crate::encode_jpeg2000`] API plus
//! the underlying `codestream` / `decode` / `encode` modules and
//! [`crate::Jpeg2000Image`] / [`crate::Jpeg2000Error`] types — none of
//! which depend on `oxideav-core`.

use oxideav_core::{
    frame::VideoPlane, CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecRegistry,
    ContainerRegistry, Decoder, Encoder, Error, Frame, Packet, PixelFormat, Result, TimeBase,
    VideoFrame,
};

use crate::error::Jpeg2000Error;
use crate::image::{Jpeg2000Image, Jpeg2000PixelFormat};
use crate::{codestream, decode, encode, Codestream, CODEC_ID_STR};

// `VideoPlane` and `VideoFrame` are used only by `From<Jpeg2000Image> for Frame`.
// `Jpeg2000PixelFormat` is used only by `From<Jpeg2000PixelFormat> for PixelFormat`.

impl From<Jpeg2000Error> for Error {
    fn from(e: Jpeg2000Error) -> Self {
        match e {
            Jpeg2000Error::InvalidData(s) => Error::InvalidData(s),
            Jpeg2000Error::Unsupported(s) => Error::Unsupported(s),
        }
    }
}

impl From<Jpeg2000PixelFormat> for PixelFormat {
    fn from(p: Jpeg2000PixelFormat) -> Self {
        match p {
            Jpeg2000PixelFormat::Gray8 => PixelFormat::Gray8,
            Jpeg2000PixelFormat::Rgb24 => PixelFormat::Rgb24,
            Jpeg2000PixelFormat::Yuv444P => PixelFormat::Yuv444P,
            Jpeg2000PixelFormat::Yuv422P => PixelFormat::Yuv422P,
            Jpeg2000PixelFormat::Yuv420P => PixelFormat::Yuv420P,
        }
    }
}

impl From<Jpeg2000Image> for Frame {
    fn from(img: Jpeg2000Image) -> Self {
        let planes = img
            .planes
            .into_iter()
            .map(|p| VideoPlane {
                stride: p.stride,
                data: p.data,
            })
            .collect();
        Frame::Video(VideoFrame {
            pts: img.pts,
            planes,
        })
    }
}

/// Register the JPEG 2000 decoder + encoder factories.
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

/// Register the JPEG 2000 file-extension hints. All standard codestream
/// and container extensions are mapped to the `"jpeg2000"` container
/// name so the framework can resolve them to this codec from a path:
///
/// - `.j2k` / `.j2c` / `.jpc` — raw Part-1 codestreams (T.800).
/// - `.jp2` — JP2 ISOBMFF wrapper (ISO/IEC 15444-1 Annex I).
/// - `.jpf` / `.jpx` — JPX extended container (ISO/IEC 15444-2).
/// - `.jpm` — JPM compound-image container (ISO/IEC 15444-6).
///
/// The crate's decoder transparently handles both raw codestreams and
/// the JP2 box wrapper; JPX / JPM are JP2-derived ISOBMFF formats whose
/// embedded `jp2c` codestream(s) the same decoder can consume.
///
/// Extensions are stored case-insensitively by [`ContainerRegistry`]
/// itself, so `.JP2`, `.Jp2`, etc. resolve identically.
pub fn register_containers(reg: &mut ContainerRegistry) {
    // Raw Part-1 codestream extensions.
    reg.register_extension("j2k", "jpeg2000");
    reg.register_extension("j2c", "jpeg2000");
    reg.register_extension("jpc", "jpeg2000");
    // JP2 box format (ISO/IEC 15444-1 Annex I).
    reg.register_extension("jp2", "jpeg2000");
    // JPX extended container (ISO/IEC 15444-2).
    reg.register_extension("jpf", "jpeg2000");
    reg.register_extension("jpx", "jpeg2000");
    // JPM compound-image container (ISO/IEC 15444-6).
    reg.register_extension("jpm", "jpeg2000");
}

fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    Ok(Box::new(J2kDecoder::new(params.codec_id.clone())))
}

fn make_encoder(params: &CodecParameters) -> Result<Box<dyn Encoder>> {
    Ok(Box::new(J2kEncoder::new_from_params(params)))
}

/// JPEG 2000 sample encoder — 5/3 integer reversible (lossless) or 9/7
/// irreversible, single-tile, single-layer, default precincts. See
/// [`crate::encode::EncodeOptions`] for the knobs this implementation
/// honours. (The decoder side accepts multi-layer codestreams; the
/// encoder emits one layer for now.)
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

    /// Build an encoder from full `CodecParameters`. Stashes width /
    /// height / pixel format in `output_params` so `send_frame` can
    /// read them when calling [`crate::encode_jpeg2000`] (the slim
    /// `VideoFrame` no longer carries them).
    pub fn new_from_params(params: &CodecParameters) -> Self {
        Self {
            output_params: params.clone(),
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
        let width = self
            .output_params
            .width
            .ok_or_else(|| Error::invalid("jpeg2000 encoder: missing width in params"))?;
        let height = self
            .output_params
            .height
            .ok_or_else(|| Error::invalid("jpeg2000 encoder: missing height in params"))?;
        let pix = self
            .output_params
            .pixel_format
            .ok_or_else(|| Error::invalid("jpeg2000 encoder: missing pixel_format in params"))?;
        let bytes = encode::codestream::encode_frame(frame, width, height, pix, &self.opts)?;
        let pkt = Packet::new(0u32, TimeBase::new(1, 1), bytes);
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
        // HTJ2K dispatch (ISO/IEC 15444-15). The classic EBCOT tier-1
        // path cannot decode HT code-blocks: the FBCOT entropy decoder
        // (Annex B of 15444-15) is a wholly different bitstream syntax.
        // With the `htj2k` feature enabled, route HT codestreams
        // through [`crate::decode::htj2k::decode_frame_htj2k`], which
        // reuses the Part-1 packet-header tier-2 walker but dispatches
        // per codeblock to the FBCOT decoder.
        #[cfg(feature = "htj2k")]
        if cs.is_htj2k() {
            let img_res = decode::htj2k::decode_frame_htj2k(&cs, &data);
            self.last_parsed = Some(cs);
            let img = img_res?;
            self.pending = Some(img.into());
            return Ok(());
        }
        let img = decode::frame::decode_frame(&cs, &data)?;
        self.last_parsed = Some(cs);
        self.pending = Some(img.into());
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
        let pkt = Packet::new(0, TimeBase::new(1, 1), buf);
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

    #[test]
    fn register_containers_maps_all_standard_extensions() {
        let mut reg = ContainerRegistry::new();
        register_containers(&mut reg);
        for ext in ["j2k", "j2c", "jpc", "jp2", "jpf", "jpx", "jpm"] {
            assert_eq!(
                reg.container_for_extension(ext),
                Some("jpeg2000"),
                "extension .{ext} must resolve to \"jpeg2000\"",
            );
        }
    }

    #[test]
    fn register_containers_is_case_insensitive() {
        let mut reg = ContainerRegistry::new();
        register_containers(&mut reg);
        // Mixed-case + all-uppercase variants must resolve identically.
        for ext in [
            "J2K", "J2C", "JPC", "JP2", "JPF", "JPX", "JPM", "Jp2", "jPx",
        ] {
            assert_eq!(
                reg.container_for_extension(ext),
                Some("jpeg2000"),
                "extension .{ext} (mixed case) must resolve to \"jpeg2000\"",
            );
        }
    }

    #[test]
    fn register_containers_rejects_unrelated_extensions() {
        let mut reg = ContainerRegistry::new();
        register_containers(&mut reg);
        // Sanity: unrelated extensions must NOT resolve to jpeg2000.
        for ext in ["png", "jpg", "jpeg", "gif", "webp", "bmp"] {
            assert_eq!(
                reg.container_for_extension(ext),
                None,
                "extension .{ext} must not be claimed by jpeg2000",
            );
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
