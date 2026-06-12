//! `oxideav-core` integration — `Decoder` trait impl and the
//! [`register`] entry point.
//!
//! Gated behind the default-on `registry` Cargo feature so consumers
//! that only want the standalone T.800 surface can depend on
//! `oxideav-jpeg2000` with `default-features = false` and skip the
//! `oxideav-core` dependency.
//!
//! The registered decoder accepts one complete raw J2K codestream per
//! packet (`.j2k` / `.j2c` — the bare T.800 Annex A codestream, not
//! the JP2 box wrapper) and emits a [`Frame::Video`]:
//!
//! * 1 component → [`PixelFormat::Gray8`],
//! * 3 components → [`PixelFormat::Rgb24`],
//! * 4 components → [`PixelFormat::Rgba`].
//!
//! Components must be unsigned, at most 8-bit, and `1:1` sub-sampled
//! for the packed conversion; anything else surfaces as a clean
//! `unsupported` error (the planar [`crate::decode_j2k`] entry point
//! has no such restriction).

use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecRegistry, ContainerRegistry,
    Decoder, Error as CoreError, Frame, MediaType, Packet, PixelFormat, RuntimeContext, VideoFrame,
    VideoPlane,
};

use crate::{decode_j2k, DecodedImage, Error};

/// Stable identifier this crate registers under in the codec registry.
pub const CODEC_ID_STR: &str = "jpeg2000";

impl From<Error> for CoreError {
    fn from(e: Error) -> Self {
        match e {
            Error::NotImplemented => CoreError::unsupported(format!("oxideav-jpeg2000: {e}")),
            other => CoreError::invalid(format!("oxideav-jpeg2000: {other}")),
        }
    }
}

/// Pack a [`DecodedImage`] into one interleaved 8-bit [`VideoFrame`].
///
/// Returns the frame plus the `(width, height, pixel_format)` triple
/// for the decoder to surface on its [`CodecParameters`].
fn image_to_frame(
    image: &DecodedImage,
    pts: Option<i64>,
) -> oxideav_core::Result<(VideoFrame, u32, u32, PixelFormat)> {
    let ncomp = image.components.len();
    let format = match ncomp {
        1 => PixelFormat::Gray8,
        3 => PixelFormat::Rgb24,
        4 => PixelFormat::Rgba,
        other => {
            return Err(CoreError::unsupported(format!(
                "oxideav-jpeg2000: no packed pixel format for {other} components"
            )))
        }
    };
    let first = &image.components[0];
    let (w, h) = (first.width, first.height);
    for c in &image.components {
        if c.precision_bits > 8 || c.is_signed || c.width != w || c.height != h {
            return Err(CoreError::unsupported(
                "oxideav-jpeg2000: packed output needs unsigned <=8-bit 1:1-sampled components",
            ));
        }
    }
    let stride = (w as usize).saturating_mul(ncomp);
    let mut data = vec![0u8; stride.saturating_mul(h as usize)];
    for (ci, comp) in image.components.iter().enumerate() {
        for (i, &s) in comp.samples.iter().enumerate() {
            data[i * ncomp + ci] = s.clamp(0, 255) as u8;
        }
    }
    Ok((
        VideoFrame {
            pts,
            planes: vec![VideoPlane { stride, data }],
        },
        w,
        h,
        format,
    ))
}

/// Factory for the [`Decoder`] trait impl — installed in the codec
/// registry and called by the framework when a `jpeg2000` packet
/// stream needs decoding.
pub fn make_decoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Decoder>> {
    Ok(Box::new(Jpeg2000Decoder::new(params.clone())))
}

/// JPEG 2000 [`Decoder`] trait impl.
///
/// One-packet-in / one-frame-out: each `send_packet` carries one
/// complete raw J2K codestream; the matching `receive_frame` returns
/// the decoded picture as packed 8-bit Gray8 / Rgb24 / Rgba.
#[derive(Debug)]
pub struct Jpeg2000Decoder {
    params: CodecParameters,
    pending: Option<Packet>,
    eof: bool,
}

impl Jpeg2000Decoder {
    /// Build a decoder whose output [`CodecParameters`] start from
    /// `params`; geometry and pixel format are re-derived from each
    /// successfully decoded frame.
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
        let image = decode_j2k(&pkt.data)?;
        let (frame, w, h, format) = image_to_frame(&image, pkt.pts)?;
        self.params.width = Some(w);
        self.params.height = Some(h);
        self.params.pixel_format = Some(format);
        Ok(Frame::Video(frame))
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        Ok(())
    }
}

/// Register the JPEG 2000 decoder factory into a [`CodecRegistry`].
pub fn register_codecs(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("jpeg2000_sw")
        .with_intra_only(true)
        .with_lossless(true)
        .with_pixel_formats(vec![
            PixelFormat::Gray8,
            PixelFormat::Rgb24,
            PixelFormat::Rgba,
        ]);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder),
    );
}

/// Register the raw-codestream file extensions (`.j2k` / `.j2c`) so a
/// [`RuntimeContext`] can map a filename hint back to the codec id.
pub fn register_containers(reg: &mut ContainerRegistry) {
    reg.register_extension("j2k", CODEC_ID_STR);
    reg.register_extension("j2c", CODEC_ID_STR);
}

/// Unified registration entry point: install both the decoder factory
/// and the extension hints into the supplied [`RuntimeContext`].
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
    register_containers(&mut ctx.containers);
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
        assert!(!ctx.codecs.has_encoder(&id));
        assert_eq!(
            ctx.containers.container_for_extension("j2k"),
            Some(CODEC_ID_STR)
        );
        assert_eq!(
            ctx.containers.container_for_extension("j2c"),
            Some(CODEC_ID_STR)
        );
    }
}
