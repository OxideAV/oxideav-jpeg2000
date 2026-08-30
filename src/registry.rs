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
    Decoder, Encoder, Error as CoreError, Frame, MediaType, Packet, PixelFormat, RuntimeContext,
    TimeBase, VideoFrame, VideoPlane,
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

/// Pack a [`DecodedImage`] into one interleaved [`VideoFrame`]: 8-bit
/// components as `Gray8` / `Rgb24` / `Rgba`, deeper (9–16-bit) ones as
/// little-endian `Gray16Le` / `Rgb48Le` / `Rgba64Le` (`Gray10Le` /
/// `Gray12Le` at exactly those depths).
///
/// Returns the frame plus the `(width, height, pixel_format)` triple
/// for the decoder to surface on its [`CodecParameters`].
fn image_to_frame(
    image: &DecodedImage,
    pts: Option<i64>,
) -> oxideav_core::Result<(VideoFrame, u32, u32, PixelFormat)> {
    let ncomp = image.components.len();
    let first = image
        .components
        .first()
        .ok_or_else(|| CoreError::invalid("oxideav-jpeg2000: image has no components"))?;
    let depth = first.precision_bits;
    let format = match (ncomp, depth) {
        (1, 0..=8) => PixelFormat::Gray8,
        (3, 0..=8) => PixelFormat::Rgb24,
        (4, 0..=8) => PixelFormat::Rgba,
        (1, 10) => PixelFormat::Gray10Le,
        (1, 12) => PixelFormat::Gray12Le,
        (1, 9..=16) => PixelFormat::Gray16Le,
        (3, 9..=16) => PixelFormat::Rgb48Le,
        (4, 9..=16) => PixelFormat::Rgba64Le,
        _ => {
            return Err(CoreError::unsupported(format!(
                "oxideav-jpeg2000: {ncomp} components at {depth} bits have no packed PixelFormat"
            )))
        }
    };
    let bytes = if depth > 8 { 2usize } else { 1 };
    let (w, h) = (image.width, image.height);
    for c in &image.components {
        if c.precision_bits != depth || c.is_signed || c.width != w || c.height != h {
            return Err(CoreError::unsupported(
                "oxideav-jpeg2000: only uniform-depth unsigned full-resolution components pack into a frame",
            ));
        }
    }
    let stride = (w as usize).saturating_mul(ncomp).saturating_mul(bytes);
    let mut data = vec![0u8; stride.saturating_mul(h as usize)];
    for (ci, c) in image.components.iter().enumerate() {
        for (i, &v) in c.samples.iter().enumerate() {
            let at = (i * ncomp + ci) * bytes;
            if bytes == 1 {
                data[at] = v.clamp(0, 255) as u8;
            } else {
                data[at..at + 2].copy_from_slice(&(v.clamp(0, 65_535) as u16).to_le_bytes());
            }
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
        // A packet may carry either a bare Annex A codestream or a
        // whole JP2 / JPH file — the latter routes through the Annex I
        // channel semantics (palette expansion, channel ordering).
        let image = if crate::looks_like_jp2(&pkt.data) {
            crate::jp2::decode_jp2(&pkt.data)?
        } else {
            decode_j2k(&pkt.data)?
        };
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

/// Factory for the [`Encoder`] trait impl — installed in the codec
/// registry by [`register`].
pub fn make_encoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Encoder>> {
    Ok(Box::new(Jpeg2000Encoder::new(params.clone())))
}

/// JPEG 2000 [`Encoder`] trait impl.
///
/// Takes one packed interleaved plane per frame in any of `Gray8`,
/// `Rgb24`, `Rgba`, `Bgr24`, `Bgra`, `Gray16Le`, `Gray10Le`,
/// `Gray12Le`, `Rgb48Le` or `Rgba64Le` (from
/// [`CodecParameters::pixel_format`], else inferred from the stride as
/// 8-bit Gray / RGB / RGBA) and emits one intra packet per frame. The
/// coding shape comes from the parameters:
///
/// * `bit_rate` (bits per second) with `frame_rate` — or without one,
///   read as bits per frame — becomes a PCRD byte budget per frame;
/// * [`CodecParameters::options`] keys: `lossless` (`true` default:
///   5-3 + RCT; `false`: 9-7 + ICT with `fine_bits`, default 6),
///   `psnr` (dB floor), `target_bytes`, `levels` (`NL`), `layers`,
///   `progression` (`lrcp` / `rlcp` / `rpcl` / `pcrl` / `cprl`),
///   `tile` (`WxH`), `ht` (`true` selects the T.814 HT block coder),
///   `plt` / `tlm` / `sop` / `eph` (`true`), `comment`, and
///   `container` (`j2k` default, or `jp2` for an Annex I / T.814
///   Annex D file with the conventional colour header).
#[derive(Debug)]
pub struct Jpeg2000Encoder {
    params: CodecParameters,
    pending: Option<Packet>,
    eof: bool,
}

impl Jpeg2000Encoder {
    /// Build an encoder. `params.width` / `params.height` must be set
    /// before the first frame.
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

    /// The [`crate::encode::EncodeParams`] the parameters select.
    fn encode_params(&self, ncomp: usize) -> oxideav_core::Result<crate::encode::EncodeParams> {
        use crate::encode::{EncodeKernel, EncodeParams};
        let opts = &self.params.options;
        let bad = |k: &str, v: &str| {
            CoreError::invalid(format!(
                "oxideav-jpeg2000 encoder: option {k}={v:?} is invalid"
            ))
        };
        let flag = |k: &str| -> oxideav_core::Result<Option<bool>> {
            match opts.get(k) {
                None => Ok(None),
                Some("true" | "1" | "yes") => Ok(Some(true)),
                Some("false" | "0" | "no") => Ok(Some(false)),
                Some(v) => Err(bad(k, v)),
            }
        };
        let mut p = EncodeParams::default();
        let lossless = flag("lossless")?.unwrap_or(true);
        if !lossless {
            let fine_bits = match opts.get("fine_bits") {
                None => 6,
                Some(v) => v.parse::<u8>().map_err(|_| bad("fine_bits", v))?,
            };
            p.kernel = EncodeKernel::Lossy9x7 { fine_bits };
        }
        p.mct = ncomp >= 3;
        if let Some(v) = opts.get("levels") {
            p.decomposition_levels = v.parse().map_err(|_| bad("levels", v))?;
        }
        if let Some(v) = opts.get("layers") {
            p.layers = v.parse().map_err(|_| bad("layers", v))?;
        }
        if let Some(v) = opts.get("progression") {
            p.progression = match v.to_ascii_lowercase().as_str() {
                "lrcp" => crate::ProgressionOrder::Lrcp,
                "rlcp" => crate::ProgressionOrder::Rlcp,
                "rpcl" => crate::ProgressionOrder::Rpcl,
                "pcrl" => crate::ProgressionOrder::Pcrl,
                "cprl" => crate::ProgressionOrder::Cprl,
                _ => return Err(bad("progression", v)),
            };
        }
        if let Some(v) = opts.get("tile") {
            let (w, h) = v.split_once('x').ok_or_else(|| bad("tile", v))?;
            p.tile_size = Some((
                w.parse().map_err(|_| bad("tile", v))?,
                h.parse().map_err(|_| bad("tile", v))?,
            ));
        }
        if let Some(v) = opts.get("psnr") {
            p.target_psnr = Some(v.parse().map_err(|_| bad("psnr", v))?);
        }
        if let Some(v) = opts.get("target_bytes") {
            p.target_bytes = Some(v.parse().map_err(|_| bad("target_bytes", v))?);
        } else if let Some(bit_rate) = self.params.bit_rate {
            // Bits per second over the frame rate, or bits per frame.
            let bits_per_frame = match self.params.frame_rate {
                Some(r) if r.num > 0 && r.den > 0 => {
                    (bit_rate as u128 * r.den as u128 / r.num as u128) as u64
                }
                _ => bit_rate,
            };
            p.target_bytes = Some(usize::try_from(bits_per_frame / 8).unwrap_or(usize::MAX));
        }
        p.high_throughput = flag("ht")?.unwrap_or(false);
        p.plt = flag("plt")?.unwrap_or(false);
        p.tlm = flag("tlm")?.unwrap_or(false);
        p.sop = flag("sop")?.unwrap_or(false);
        p.eph = flag("eph")?.unwrap_or(false);
        p.comment = opts.get("comment").map(str::to_owned);
        Ok(p)
    }
}

impl Encoder for Jpeg2000Encoder {
    fn codec_id(&self) -> &CodecId {
        &self.params.codec_id
    }

    fn output_params(&self) -> &CodecParameters {
        &self.params
    }

    fn send_frame(&mut self, frame: &Frame) -> oxideav_core::Result<()> {
        if self.pending.is_some() {
            return Err(CoreError::other(
                "oxideav-jpeg2000 encoder: receive_packet must be called before sending another frame",
            ));
        }
        let Frame::Video(v) = frame else {
            return Err(CoreError::unsupported(
                "oxideav-jpeg2000 encoder: only video frames are supported",
            ));
        };
        let (width, height) = match (self.params.width, self.params.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
            _ => {
                return Err(CoreError::invalid(
                    "oxideav-jpeg2000 encoder: CodecParameters width/height required",
                ))
            }
        };
        let plane = v.planes.first().ok_or_else(|| {
            CoreError::invalid("oxideav-jpeg2000 encoder: video frame has no planes")
        })?;
        if v.planes.len() != 1 {
            return Err(CoreError::unsupported(
                "oxideav-jpeg2000 encoder: expected one packed interleaved plane",
            ));
        }
        // Component count, bytes per sample, depth, and the plane order
        // that maps the packed layout onto components 0..n.
        let format = match self.params.pixel_format {
            Some(f) => f,
            None => match plane.stride / width as usize {
                1 => PixelFormat::Gray8,
                3 => PixelFormat::Rgb24,
                4 => PixelFormat::Rgba,
                _ => return Err(CoreError::unsupported(
                    "oxideav-jpeg2000 encoder: cannot infer a packed pixel format from the stride",
                )),
            },
        };
        let (ncomp, bytes, depth, order): (usize, usize, u8, [usize; 4]) = match format {
            PixelFormat::Gray8 => (1, 1, 8, [0, 0, 0, 0]),
            PixelFormat::Rgb24 => (3, 1, 8, [0, 1, 2, 0]),
            PixelFormat::Bgr24 => (3, 1, 8, [2, 1, 0, 0]),
            PixelFormat::Rgba => (4, 1, 8, [0, 1, 2, 3]),
            PixelFormat::Bgra => (4, 1, 8, [2, 1, 0, 3]),
            PixelFormat::Gray16Le => (1, 2, 16, [0, 0, 0, 0]),
            PixelFormat::Gray10Le => (1, 2, 10, [0, 0, 0, 0]),
            PixelFormat::Gray12Le => (1, 2, 12, [0, 0, 0, 0]),
            PixelFormat::Rgb48Le => (3, 2, 16, [0, 1, 2, 0]),
            PixelFormat::Rgba64Le => (4, 2, 16, [0, 1, 2, 3]),
            other => {
                return Err(CoreError::unsupported(format!(
                    "oxideav-jpeg2000 encoder: pixel format {other:?} is not a packed Gray / RGB / RGBA layout"
                )))
            }
        };
        let row = ncomp * bytes * width as usize;
        if plane.stride < row || plane.data.len() < plane.stride * (height as usize - 1) + row {
            return Err(CoreError::invalid(
                "oxideav-jpeg2000 encoder: plane is smaller than the declared size",
            ));
        }
        let params = self.encode_params(ncomp)?;
        let jp2 = match self.params.options.get("container") {
            None | Some("j2k") | Some("j2c") => false,
            Some("jp2") | Some("jph") => true,
            Some(v) => {
                return Err(CoreError::invalid(format!(
                    "oxideav-jpeg2000 encoder: option container={v:?} is invalid"
                )))
            }
        };
        let n = (width * height) as usize;
        let bytes_out = if bytes == 1 {
            let mut planes: Vec<Vec<u8>> = vec![Vec::with_capacity(n); ncomp];
            for y in 0..height as usize {
                let r = &plane.data[y * plane.stride..y * plane.stride + row];
                for px in r.chunks_exact(ncomp) {
                    for (c, plane) in planes.iter_mut().enumerate() {
                        plane.push(px[order[c]]);
                    }
                }
            }
            let refs: Vec<&[u8]> = planes.iter().map(Vec::as_slice).collect();
            if jp2 {
                crate::encode::encode_jp2(&refs, width, height, &params)?
            } else {
                crate::encode::encode_j2k(&refs, width, height, &params)?
            }
        } else {
            let max = (1u32 << depth) - 1;
            let mut planes: Vec<Vec<u16>> = vec![Vec::with_capacity(n); ncomp];
            for y in 0..height as usize {
                let r = &plane.data[y * plane.stride..y * plane.stride + row];
                for px in r.chunks_exact(ncomp * 2) {
                    for (c, plane) in planes.iter_mut().enumerate() {
                        let k = order[c] * 2;
                        let v = u16::from_le_bytes([px[k], px[k + 1]]);
                        if u32::from(v) > max {
                            return Err(CoreError::invalid(format!(
                                "oxideav-jpeg2000 encoder: sample {v} exceeds {depth} bits"
                            )));
                        }
                        plane.push(v);
                    }
                }
            }
            let refs: Vec<&[u16]> = planes.iter().map(Vec::as_slice).collect();
            if jp2 {
                crate::encode::encode_jp2_u16(
                    &refs,
                    width,
                    height,
                    depth,
                    &params,
                    &crate::jp2::Jp2WriteOptions::for_components(ncomp),
                )?
            } else {
                crate::encode::encode_j2k_u16(&refs, width, height, depth, &params)?
            }
        };
        self.params.pixel_format = Some(format);
        let mut pkt = Packet::new(0, TimeBase::new(1, 1), bytes_out);
        pkt.pts = v.pts;
        pkt.dts = v.pts;
        pkt.flags.keyframe = true; // intra-only
        self.pending = Some(pkt);
        Ok(())
    }

    fn receive_packet(&mut self) -> oxideav_core::Result<Packet> {
        match self.pending.take() {
            Some(pkt) => Ok(pkt),
            None if self.eof => Err(CoreError::Eof),
            None => Err(CoreError::NeedMore),
        }
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        Ok(())
    }
}

/// Register the JPEG 2000 decoder + encoder factories into a
/// [`CodecRegistry`].
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
            .decoder(make_decoder)
            .encoder(make_encoder),
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
        assert!(
            ctx.codecs.has_encoder(&id),
            "jpeg2000 encoder factory not installed via RuntimeContext"
        );
        assert_eq!(
            ctx.containers.container_for_extension("j2k"),
            Some(CODEC_ID_STR)
        );
        assert_eq!(
            ctx.containers.container_for_extension("j2c"),
            Some(CODEC_ID_STR)
        );
    }

    #[test]
    fn encoder_round_trips_through_decoder() {
        // Drive the Encoder trait impl with a packed Rgb24 frame, then
        // feed the produced packet to the Decoder trait impl and assert
        // the pixels round-trip bit-exactly (the lossless 5-3 path).
        let (w, h) = (10u32, 7u32);
        let ncomp = 3usize;
        let data: Vec<u8> = (0..(w * h) as usize * ncomp)
            .map(|i| (i * 37 % 256) as u8)
            .collect();
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(w);
        params.height = Some(h);
        let mut enc = make_encoder(&params).expect("encoder factory");
        let frame = Frame::Video(VideoFrame {
            pts: Some(42),
            planes: vec![VideoPlane {
                stride: w as usize * ncomp,
                data: data.clone(),
            }],
        });
        enc.send_frame(&frame).expect("send_frame");
        let pkt = enc.receive_packet().expect("receive_packet");
        assert!(pkt.flags.keyframe);
        assert_eq!(pkt.pts, Some(42));

        let mut dec =
            make_decoder(&CodecParameters::video(CodecId::new(CODEC_ID_STR))).expect("factory");
        dec.send_packet(&pkt).expect("send_packet");
        let Frame::Video(out) = dec.receive_frame().expect("receive_frame") else {
            panic!("expected a video frame");
        };
        assert_eq!(out.planes.len(), 1);
        assert_eq!(out.planes[0].data, data, "registry round-trip pixels");
    }

    fn drive(params: &CodecParameters, stride: usize, data: Vec<u8>) -> (Packet, VideoFrame) {
        let mut enc = make_encoder(params).expect("encoder factory");
        enc.send_frame(&Frame::Video(VideoFrame {
            pts: Some(1),
            planes: vec![VideoPlane { stride, data }],
        }))
        .expect("send_frame");
        let pkt = enc.receive_packet().expect("receive_packet");
        let mut dec =
            make_decoder(&CodecParameters::video(CodecId::new(CODEC_ID_STR))).expect("factory");
        dec.send_packet(&pkt).expect("send_packet");
        let Frame::Video(out) = dec.receive_frame().expect("receive_frame") else {
            panic!("expected a video frame");
        };
        (pkt, out)
    }

    #[test]
    fn encoder_honours_packed_pixel_formats_both_depths() {
        let (w, h) = (9u32, 6u32);
        let n = (w * h) as usize;
        // 8-bit layouts: BGR / BGRA inputs come back as RGB / RGBA.
        for (format, ncomp, swap) in [
            (PixelFormat::Gray8, 1usize, false),
            (PixelFormat::Rgb24, 3, false),
            (PixelFormat::Bgr24, 3, true),
            (PixelFormat::Rgba, 4, false),
            (PixelFormat::Bgra, 4, true),
        ] {
            let data: Vec<u8> = (0..n * ncomp).map(|i| (i * 53 % 256) as u8).collect();
            let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
            params.width = Some(w);
            params.height = Some(h);
            params.pixel_format = Some(format);
            let (_, out) = drive(&params, w as usize * ncomp, data.clone());
            let mut want = data.clone();
            if swap {
                for px in want.chunks_exact_mut(ncomp) {
                    px.swap(0, 2);
                }
            }
            assert_eq!(out.planes[0].data, want, "{format:?}");
        }
        // 16-bit layouts round-trip through the u16 path and come back
        // as the same little-endian format.
        for (format, ncomp, depth) in [
            (PixelFormat::Gray16Le, 1usize, 16u32),
            (PixelFormat::Gray12Le, 1, 12),
            (PixelFormat::Gray10Le, 1, 10),
            (PixelFormat::Rgb48Le, 3, 16),
            (PixelFormat::Rgba64Le, 4, 16),
        ] {
            let max = (1u32 << depth) - 1;
            let data: Vec<u8> = (0..n * ncomp)
                .map(|i| ((i as u32 * 2_749) % (max + 1)) as u16)
                .flat_map(u16::to_le_bytes)
                .collect();
            let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
            params.width = Some(w);
            params.height = Some(h);
            params.pixel_format = Some(format);
            let (pkt, out) = drive(&params, w as usize * ncomp * 2, data.clone());
            assert_eq!(out.planes[0].data, data, "{format:?}");
            let hdr = crate::parse_j2k_header(&pkt.data).expect("header");
            assert_eq!(u32::from(hdr.siz.components[0].precision_bits), depth);
        }
    }

    #[test]
    fn encoder_options_select_kernel_budget_container_and_markers() {
        let (w, h) = (32u32, 24u32);
        let n = (w * h) as usize;
        let data: Vec<u8> = (0..n * 3).map(|i| (i * 131 % 251) as u8).collect();
        let base = |opts: oxideav_core::CodecOptions| {
            let mut p = CodecParameters::video(CodecId::new(CODEC_ID_STR));
            p.width = Some(w);
            p.height = Some(h);
            p.pixel_format = Some(PixelFormat::Rgb24);
            p.options = opts;
            p
        };
        // Lossless default: bit-exact, RCT signalled.
        let (pkt, out) = drive(
            &base(oxideav_core::CodecOptions::new()),
            w as usize * 3,
            data.clone(),
        );
        assert_eq!(out.planes[0].data, data);
        let hdr = crate::parse_j2k_header(&pkt.data).expect("header");
        assert_eq!(hdr.cod.multi_component_transform, 1);
        // Lossy 9-7 with a PSNR floor, layers, progression, tiles, PLT.
        let opts = oxideav_core::CodecOptions::new()
            .set("lossless", "false")
            .set("fine_bits", "3")
            .set("psnr", "34")
            .set("layers", "2")
            .set("progression", "rpcl")
            .set("tile", "16x16")
            .set("plt", "true")
            .set("comment", "registry");
        let (pkt, out) = drive(&base(opts), w as usize * 3, data.clone());
        let hdr = crate::parse_j2k_header(&pkt.data).expect("header");
        assert_eq!(hdr.cod.progression, crate::ProgressionOrder::Rpcl);
        assert_eq!(hdr.cod.layers, 2);
        assert_eq!(hdr.siz.tile_width, 16);
        assert!(pkt.data.windows(2).any(|x| x == [0xFF, 0x58]), "PLT");
        assert!(pkt.data.windows(2).any(|x| x == [0xFF, 0x64]), "COM");
        let sse: f64 = out.planes[0]
            .data
            .iter()
            .zip(&data)
            .map(|(&g, &wv)| (f64::from(g) - f64::from(wv)).powi(2))
            .sum();
        let psnr = 10.0 * (255.0f64 * 255.0 / (sse / (n * 3) as f64)).log10();
        assert!(psnr >= 34.0, "{psnr}");
        // A bit_rate with a frame rate is a per-frame budget.
        let mut p = base(oxideav_core::CodecOptions::new());
        p.bit_rate = Some(8 * 600 * 25);
        p.frame_rate = Some(oxideav_core::Rational::new(25, 1));
        let (pkt, _) = drive(&p, w as usize * 3, data.clone());
        assert!(pkt.data.len() <= 600, "{}", pkt.data.len());
        // JP2 container + HT block coder.
        let opts = oxideav_core::CodecOptions::new()
            .set("container", "jp2")
            .set("ht", "true");
        let (pkt, out) = drive(&base(opts), w as usize * 3, data.clone());
        assert!(crate::looks_like_jp2(&pkt.data));
        assert_eq!(out.planes[0].data, data);
        let c = crate::jp2::parse_jp2(&pkt.data).expect("parse");
        assert!(c.ftyp.is_jph_compatible());
        // Malformed options surface a clean error.
        let opts = oxideav_core::CodecOptions::new().set("levels", "many");
        let mut enc = make_encoder(&base(opts)).expect("factory");
        assert!(enc
            .send_frame(&Frame::Video(VideoFrame {
                pts: None,
                planes: vec![VideoPlane {
                    stride: w as usize * 3,
                    data: data.clone(),
                }],
            }))
            .is_err());
    }
}
