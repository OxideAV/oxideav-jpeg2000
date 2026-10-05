//! JPEG 2000 containers for the framework: the bare codestream
//! (`jpeg2000` — `.j2k` / `.j2c`) and the JP2 / JPH file (`jp2` —
//! `.jp2` / `.jph`). Both are single-image containers in the same
//! shape as `oxideav-farbfeld`: one video stream, one packet holding
//! the whole file, the codec sniffing the framing.
//!
//! The demuxer declares the stream the way [`crate::info`] describes
//! the file — width / height, the **native** contract layout as
//! `pixel_format`, the JP2 palette as RGB triples in `extradata` for
//! `Pal8`, and `color_signal` only when the JP2 header carries a
//! `colr` box (a bare codestream signals no colour; the registry
//! ruling forbids inventing one). A component set with no contract
//! layout (signed / mixed-depth / ≥ 5 components, [`crate::image`]
//! docs) still opens: the stream's `pixel_format` is `None` and the
//! decoder reports `Unsupported` for its packet, so a gateway can at
//! least name the file and its geometry. A multi-codestream (`jpx`)
//! file yields the first `jp2c` payload, which is all the Part 1
//! reader exposes.
//!
//! The two muxers take the registry encoder's packets (a bare
//! codestream by default, a JP2 file with `container = jp2`) and write
//! what their name says: `jpeg2000` strips a JP2 wrapper down to the
//! codestream, `jp2` wraps a bare codestream in a JP2 / JPH header
//! built from the stream parameters (layout, palette, colour signal).
//! One picture per file — a second packet is refused.
//!
//! Lives behind the `registry` feature (every type here is
//! `oxideav-core`'s).

use std::io::{Read, SeekFrom, Write};

use oxideav_core::{
    CodecId, CodecParameters, CodecResolver, ContainerRegistry, Demuxer, Error, MediaType, Muxer,
    Packet, PixelFormat, ProbeData, ProbeScore, ReadSeek, Result, StreamInfo, TimeBase, WriteSeek,
    MAX_PROBE_SCORE, PROBE_SCORE_EXTENSION,
};

use crate::api::{describe, is_jp2_file, jp2_options_parts};
use crate::image::{ColorInfo, Jpeg2000PixelFormat, Palette};
use crate::jp2::{parse_jp2, write_jp2, Jp2WriteOptions};
use crate::registry::{from_color_signal, to_color_signal, CODEC_ID_STR};
use crate::{MARKER_SIZ, MARKER_SOC};

/// Container name of the bare codestream (`.j2k` / `.j2c`).
pub const CONTAINER_J2K: &str = "jpeg2000";
/// Container name of the JP2 / JPH file (`.jp2` / `.jph`).
pub const CONTAINER_JP2: &str = "jp2";

/// Register both containers: the shared demuxer under each name, the
/// two muxers, the extension table and the probes.
pub fn register(reg: &mut ContainerRegistry) {
    reg.register_demuxer(CONTAINER_J2K, open_demuxer);
    reg.register_demuxer(CONTAINER_JP2, open_demuxer);
    reg.register_muxer(CONTAINER_J2K, open_muxer_j2k);
    reg.register_muxer(CONTAINER_JP2, open_muxer_jp2);
    reg.register_extension("j2k", CONTAINER_J2K);
    reg.register_extension("j2c", CONTAINER_J2K);
    reg.register_extension("jp2", CONTAINER_JP2);
    reg.register_extension("jph", CONTAINER_JP2);
    reg.register_probe(CONTAINER_J2K, probe_j2k);
    reg.register_probe(CONTAINER_JP2, probe_jp2);
}

/// `true` when `bytes` opens with the `SOC` + `SIZ` marker pair of an
/// Annex A codestream.
fn is_codestream(bytes: &[u8]) -> bool {
    bytes.len() >= 4
        && bytes[0..2] == MARKER_SOC.to_be_bytes()
        && bytes[2..4] == MARKER_SIZ.to_be_bytes()
}

/// Bare-codestream probe: the `SOC` / `SIZ` pair scores full marks, the
/// `.j2k` / `.j2c` extension the conventional weak score.
pub fn probe_j2k(data: &ProbeData) -> ProbeScore {
    if is_codestream(data.buf) {
        return MAX_PROBE_SCORE;
    }
    if matches!(data.ext, Some("j2k") | Some("j2c")) {
        PROBE_SCORE_EXTENSION
    } else {
        0
    }
}

/// JP2 / JPH probe: the 12-byte Signature box scores full marks, the
/// `.jp2` / `.jph` extension the conventional weak score.
pub fn probe_jp2(data: &ProbeData) -> ProbeScore {
    if is_jp2_file(data.buf) {
        return MAX_PROBE_SCORE;
    }
    if matches!(data.ext, Some("jp2") | Some("jph")) {
        PROBE_SCORE_EXTENSION
    } else {
        0
    }
}

// ---- Demuxer ----------------------------------------------------------------

/// Open a bare codestream or a JP2 / JPH file as a one-stream,
/// one-packet container. The header is walked eagerly (the same
/// accept / reject verdict as [`crate::info`], minus the layout
/// requirement) so the stream carries accurate geometry before the
/// decoder runs.
pub fn open_demuxer(
    mut input: Box<dyn ReadSeek>,
    _codecs: &dyn CodecResolver,
) -> Result<Box<dyn Demuxer>> {
    input.seek(SeekFrom::Start(0))?;
    let mut buf = Vec::new();
    input.read_to_end(&mut buf)?;
    drop(input);
    if !is_jp2_file(&buf) && !is_codestream(&buf) {
        return Err(Error::invalid(
            "jpeg2000 demuxer: neither a JP2 Signature box nor an SOC / SIZ codestream",
        ));
    }
    let d = describe(&buf)?;
    let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    params.width = Some(d.width);
    params.height = Some(d.height);
    params.pixel_format = d.format.map(PixelFormat::from);
    if let (Some(Jpeg2000PixelFormat::Pal8), Some(p)) = (d.format, &d.palette) {
        params.extradata = p.entries.iter().flat_map(|e| [e[0], e[1], e[2]]).collect();
    }
    if d.color.is_signalled() {
        params.color_signal = to_color_signal(&d.color);
    }
    let mut metadata = Vec::new();
    if d.has_icc {
        metadata.push(("icc".to_owned(), "present".to_owned()));
    }
    let stream = StreamInfo {
        index: 0,
        params,
        time_base: TimeBase::new(1, 1),
        start_time: Some(0),
        duration: None,
    };
    Ok(Box::new(Jpeg2000Demuxer {
        name: if d.jp2 { CONTAINER_JP2 } else { CONTAINER_J2K },
        streams: vec![stream],
        metadata,
        data: Some(buf),
    }))
}

struct Jpeg2000Demuxer {
    name: &'static str,
    streams: Vec<StreamInfo>,
    metadata: Vec<(String, String)>,
    data: Option<Vec<u8>>,
}

impl Demuxer for Jpeg2000Demuxer {
    fn format_name(&self) -> &str {
        self.name
    }
    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }
    fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }
    fn next_packet(&mut self) -> Result<Packet> {
        match self.data.take() {
            Some(bytes) => {
                let mut pkt = Packet::new(0, TimeBase::new(1, 1), bytes);
                pkt.pts = Some(0);
                pkt.dts = Some(0);
                pkt.flags.keyframe = true;
                Ok(pkt)
            }
            None => Err(Error::Eof),
        }
    }
}

// ---- Muxers -----------------------------------------------------------------

/// Muxer for the bare codestream (`.j2k` / `.j2c`): the encoder's
/// packet is written as is, or unwrapped when it is a JP2 file. A
/// palettised (`Pal8`) picture cannot be carried and is `Unsupported`.
pub fn open_muxer_j2k(
    output: Box<dyn WriteSeek>,
    streams: &[StreamInfo],
) -> Result<Box<dyn Muxer>> {
    Jpeg2000Muxer::open(output, streams, false)
}

/// Muxer for the JP2 / JPH file (`.jp2` / `.jph`): a JP2 packet is
/// written as is; a bare codestream is wrapped in a JP2 header built
/// from the stream parameters — layout from `pixel_format` (else from
/// the `SIZ`), `Pal8` palette from `extradata`, `colr` from
/// `color_signal` (the layout's conventional colourspace when
/// unspecified), JPH brand for HT codestreams.
pub fn open_muxer_jp2(
    output: Box<dyn WriteSeek>,
    streams: &[StreamInfo],
) -> Result<Box<dyn Muxer>> {
    Jpeg2000Muxer::open(output, streams, true)
}

struct Jpeg2000Muxer {
    output: Box<dyn WriteSeek>,
    wrap: bool,
    format: Option<Jpeg2000PixelFormat>,
    palette: Option<Palette>,
    color: ColorInfo,
    written: bool,
}

impl Jpeg2000Muxer {
    fn open(
        output: Box<dyn WriteSeek>,
        streams: &[StreamInfo],
        wrap: bool,
    ) -> Result<Box<dyn Muxer>> {
        let name = if wrap { CONTAINER_JP2 } else { CONTAINER_J2K };
        let [stream] = streams else {
            return Err(Error::invalid(format!(
                "{name} muxer: expected exactly one video stream, got {}",
                streams.len()
            )));
        };
        let p = &stream.params;
        if p.media_type != MediaType::Video {
            return Err(Error::invalid(format!(
                "{name} muxer: stream must be video"
            )));
        }
        if p.codec_id.as_str() != CODEC_ID_STR {
            return Err(Error::unsupported(format!(
                "{name} muxer: stream codec {} is not {CODEC_ID_STR}",
                p.codec_id
            )));
        }
        let format = p.pixel_format.and_then(|f| match f {
            // The registry encoder re-orders these into RGB(A).
            PixelFormat::Bgr24 => Some(Jpeg2000PixelFormat::Rgb24),
            PixelFormat::Bgra => Some(Jpeg2000PixelFormat::Rgba),
            other => Jpeg2000PixelFormat::try_from(other).ok(),
        });
        let palette =
            (format == Some(Jpeg2000PixelFormat::Pal8) && !p.extradata.is_empty()).then(|| {
                Palette::new(
                    p.extradata
                        .chunks_exact(3)
                        .map(|c| [c[0], c[1], c[2], 255])
                        .collect(),
                )
            });
        let color = if p.color_signal.is_unspecified() {
            ColorInfo::unspecified()
        } else {
            from_color_signal(&p.color_signal)
        };
        Ok(Box::new(Jpeg2000Muxer {
            output,
            wrap,
            format,
            palette,
            color,
            written: false,
        }))
    }

    /// The JP2 header for a bare codestream: the stream's layout when
    /// it is one of ours, else the layout the `SIZ` implies, else the
    /// component-count default (`UnkC` for exotic sets).
    fn wrap_options(&self, codestream: &[u8]) -> Result<Jp2WriteOptions> {
        let header = crate::parse_j2k_header(codestream)?;
        let siz = &header.siz;
        let jph = siz.rsiz & 0x4000 != 0;
        let n = siz.components.len();
        let format = self.format.filter(|f| f.components() == n).or_else(|| {
            let c0 = siz.components.first()?;
            if siz.components.iter().any(|c| c.is_signed) {
                return None;
            }
            let chroma = siz
                .components
                .get(1)
                .map(|c1| {
                    (
                        c1.h_separation.max(1) / c0.h_separation.max(1),
                        c1.v_separation.max(1) / c0.v_separation.max(1),
                    )
                })
                .unwrap_or((1, 1));
            Jpeg2000PixelFormat::for_layout(n, c0.precision_bits, chroma, chroma != (1, 1))
        });
        match format {
            Some(Jpeg2000PixelFormat::Pal8) if self.palette.is_none() => Err(Error::invalid(
                "jp2 muxer: Pal8 stream without a palette in extradata",
            )),
            Some(f) => Ok(jp2_options_parts(
                f,
                self.palette.as_ref(),
                self.color,
                None,
                jph,
            )),
            None => Ok(Jp2WriteOptions::for_components(n)),
        }
    }
}

impl Muxer for Jpeg2000Muxer {
    fn format_name(&self) -> &str {
        if self.wrap {
            CONTAINER_JP2
        } else {
            CONTAINER_J2K
        }
    }
    fn write_header(&mut self) -> Result<()> {
        Ok(())
    }
    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let name = self.format_name();
        if self.written {
            return Err(Error::unsupported(format!(
                "{name} muxer: a JPEG 2000 file holds one picture; second packet refused"
            )));
        }
        let data = packet.data.as_slice();
        let out: std::borrow::Cow<'_, [u8]> = match (is_jp2_file(data), is_codestream(data)) {
            (true, _) if self.wrap => data.into(),
            (true, _) => {
                let c = parse_jp2(data)?;
                if c.header.pclr.is_some() {
                    return Err(Error::unsupported(
                        "jpeg2000 muxer: a palettised (Pal8) picture needs the jp2 container",
                    ));
                }
                let end = c
                    .codestream_offset
                    .checked_add(c.codestream_len)
                    .filter(|&e| e <= data.len())
                    .ok_or_else(|| Error::invalid("jpeg2000 muxer: truncated jp2c box"))?;
                data[c.codestream_offset..end].into()
            }
            (_, true) if self.wrap => write_jp2(data, &self.wrap_options(data)?)?.into(),
            (_, true) => {
                if self.format == Some(Jpeg2000PixelFormat::Pal8) {
                    return Err(Error::unsupported(
                        "jpeg2000 muxer: a palettised (Pal8) picture needs the jp2 container",
                    ));
                }
                data.into()
            }
            _ => {
                return Err(Error::invalid(format!(
                    "{name} muxer: packet is neither a JPEG 2000 codestream nor a JP2 file"
                )))
            }
        };
        self.output.write_all(&out)?;
        self.written = true;
        Ok(())
    }
    fn write_trailer(&mut self) -> Result<()> {
        self.output.flush()?;
        Ok(())
    }
}
