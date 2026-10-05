//! The framework containers (`jpeg2000` bare codestream, `jp2` file):
//! probe → demuxer → decoder yields the Layer 1 planes byte for byte on
//! every committed layout, the muxers write files Layer 1 reads back,
//! and the registry round trip `demux(mux(frames)) == frames` holds.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{
    CodecId, CodecOptions, CodecParameters, DecoderLimits, Error, Frame, NullCodecResolver, Packet,
    PixelFormat, ProbeData, ReadSeek, RuntimeContext, StreamInfo, TimeBase, VideoFrame, WriteSeek,
};
use oxideav_jpeg2000::container::{
    open_demuxer, open_muxer_j2k, open_muxer_jp2, probe_j2k, probe_jp2, CONTAINER_J2K,
    CONTAINER_JP2,
};
use oxideav_jpeg2000::jp2::{write_jp2, Colr, ColrMethod, Jp2WriteOptions};
use oxideav_jpeg2000::registry::to_color_signal;
use oxideav_jpeg2000::{
    decode, encode, info, make_encoder, parse_j2k_header, register, ColorInfo, Container,
    EncodeOptions, Jpeg2000Error, Jpeg2000Image, Palette, PixelFormat as J2k, Plane, CODEC_ID_STR,
};

/// A `WriteSeek` sink whose bytes stay reachable after the muxer took
/// ownership of the box.
#[derive(Clone, Default)]
struct SharedSink {
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    pos: u64,
}

impl SharedSink {
    fn bytes(&self) -> Vec<u8> {
        self.buf.lock().unwrap().clone()
    }
}

impl std::io::Write for SharedSink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let mut v = self.buf.lock().unwrap();
        let at = self.pos as usize;
        if v.len() < at + b.len() {
            v.resize(at + b.len(), 0);
        }
        v[at..at + b.len()].copy_from_slice(b);
        self.pos += b.len() as u64;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Seek for SharedSink {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let len = self.buf.lock().unwrap().len() as i64;
        let new = match pos {
            std::io::SeekFrom::Start(n) => n as i64,
            std::io::SeekFrom::End(n) => len + n,
            std::io::SeekFrom::Current(n) => self.pos as i64 + n,
        };
        self.pos = u64::try_from(new).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        Ok(self.pos)
    }
}

const GRAY8: &[u8] = include_bytes!("data/gray-17x13-53.j2k");
const GRAY16: &[u8] = include_bytes!("data/gray-32x32-u16-53.j2k");
const GRAY_S8: &[u8] = include_bytes!("data/gray-32x32-s8-53.j2k");
const RGB_RCT: &[u8] = include_bytes!("data/rgb-16x16-rct-53.j2k");
const RGB_SUB21: &[u8] = include_bytes!("data/rgb-95x48-sub21-53.j2k");
const RGB_SUB3: &[u8] = include_bytes!("data/rgb-24x24-cprl-sub3-53.j2k");
const RGB_JP2: &[u8] = include_bytes!("data/rgb-48x32.jp2");
const BGR_CDEF_JP2: &[u8] = include_bytes!("data/bgr-cdef-16x16.jp2");
const PAL_JP2: &[u8] = include_bytes!("data/pal-32x24.jp2");
const HT_RGB24: &[u8] = include_bytes!("fixtures/ht_rgb24_rev.j2c");

/// Every readable fixture with the container the registry must name.
const FIXTURES: &[(&str, &[u8], &str)] = &[
    ("gray-17x13-53.j2k", GRAY8, CONTAINER_J2K),
    ("gray-32x32-u16-53.j2k", GRAY16, CONTAINER_J2K),
    ("rgb-16x16-rct-53.j2k", RGB_RCT, CONTAINER_J2K),
    ("rgb-95x48-sub21-53.j2k", RGB_SUB21, CONTAINER_J2K),
    ("rgb-24x24-cprl-sub3-53.j2k", RGB_SUB3, CONTAINER_J2K),
    ("rgb-48x32.jp2", RGB_JP2, CONTAINER_JP2),
    ("bgr-cdef-16x16.jp2", BGR_CDEF_JP2, CONTAINER_JP2),
    ("pal-32x24.jp2", PAL_JP2, CONTAINER_JP2),
    ("ht_rgb24_rev.j2c", HT_RGB24, CONTAINER_J2K),
];

fn ctx() -> RuntimeContext {
    let mut ctx = RuntimeContext::new();
    register(&mut ctx);
    ctx
}

fn reader(bytes: &[u8]) -> Box<dyn ReadSeek> {
    Box::new(Cursor::new(bytes.to_vec()))
}

fn probe(ctx: &RuntimeContext, bytes: &[u8], ext: Option<&str>) -> oxideav_core::Result<String> {
    let mut cur = Cursor::new(bytes.to_vec());
    ctx.containers.probe_input(&mut cur, ext)
}

/// Demux `bytes` through the registry and decode its packet with the
/// registry decoder; returns the stream and the frame.
fn demux_decode(ctx: &RuntimeContext, bytes: &[u8]) -> (StreamInfo, VideoFrame) {
    let name = probe(ctx, bytes, None).expect("probe");
    let mut demux = ctx
        .containers
        .open_demuxer(&name, reader(bytes), &ctx.codecs)
        .expect("open_demuxer");
    assert_eq!(demux.format_name(), name);
    let stream = demux.streams()[0].clone();
    let pkt = demux.next_packet().expect("one packet");
    assert_eq!(pkt.stream_index, 0);
    assert!(pkt.flags.keyframe);
    assert_eq!(pkt.pts, Some(0));
    assert!(matches!(demux.next_packet(), Err(Error::Eof)));
    let mut dec = ctx
        .codecs
        .first_decoder(&stream.params)
        .expect("first_decoder");
    dec.send_packet(&pkt).expect("send_packet");
    let Frame::Video(vf) = dec.receive_frame().expect("receive_frame") else {
        panic!("video frame expected");
    };
    (stream, vf)
}

#[test]
fn probe_names_the_container_from_magic_and_extension() {
    let ctx = ctx();
    for (name, bytes, container) in FIXTURES {
        assert_eq!(
            probe(&ctx, bytes, None).unwrap(),
            *container,
            "{name} magic"
        );
        let ext = name.rsplit('.').next().unwrap();
        assert_eq!(
            probe(&ctx, bytes, Some(ext)).unwrap(),
            *container,
            "{name} magic + extension"
        );
    }
    // Magic beats a misleading extension hint.
    assert_eq!(probe(&ctx, RGB_JP2, Some("j2k")).unwrap(), CONTAINER_JP2);
    assert_eq!(probe(&ctx, GRAY8, Some("jp2")).unwrap(), CONTAINER_J2K);
    // Extension alone resolves to the right container; the demuxer then
    // rejects the bytes.
    assert_eq!(
        probe(&ctx, b"not a picture at all", Some("j2c")).unwrap(),
        CONTAINER_J2K
    );
    assert_eq!(
        probe(&ctx, b"not a picture at all", Some("jph")).unwrap(),
        CONTAINER_JP2
    );
    assert!(ctx
        .containers
        .open_demuxer(CONTAINER_J2K, reader(b"not a picture at all"), &ctx.codecs)
        .is_err());
    // Foreign files are rejected.
    for foreign in [
        &b"farbfeld\0\0\0\x01\0\0\0\x01\0\0\0\0\0\0\0\0"[..],
        &b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"[..],
        &b"\xff\xd8\xff\xe0\0\x10JFIF"[..],
        &[][..],
    ] {
        assert!(
            matches!(probe(&ctx, foreign, None), Err(Error::FormatNotFound(_))),
            "{foreign:?}"
        );
    }
    // The probe functions themselves are total on short input.
    for n in 0..16 {
        let data = ProbeData {
            buf: &RGB_JP2[..n],
            ext: None,
        };
        let _ = probe_jp2(&data);
        let _ = probe_j2k(&data);
    }
}

#[test]
fn demuxer_declares_the_layer1_stream() {
    let ctx = ctx();
    for (name, bytes, _) in FIXTURES {
        let i = info(bytes).unwrap();
        let (stream, _) = demux_decode(&ctx, bytes);
        let p = &stream.params;
        assert_eq!(p.codec_id, CodecId::new(CODEC_ID_STR), "{name}");
        assert_eq!(p.width, Some(i.width), "{name} width");
        assert_eq!(p.height, Some(i.height), "{name} height");
        assert_eq!(
            p.pixel_format,
            Some(PixelFormat::from(i.format)),
            "{name} format"
        );
        assert_eq!(
            !p.color_signal.is_unspecified(),
            i.color.is_signalled(),
            "{name}: colour signal only when the file carries colour"
        );
        if i.color.is_signalled() {
            assert_eq!(p.color_signal.primaries.0, i.color.primaries, "{name}");
            assert_eq!(p.color_signal.transfer.0, i.color.transfer, "{name}");
            assert_eq!(p.color_signal.matrix.0, i.color.matrix, "{name}");
        }
        let img = decode(bytes).unwrap();
        if i.format == J2k::Pal8 {
            let rgb: Vec<u8> = img
                .palette
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .flat_map(|e| [e[0], e[1], e[2]])
                .collect();
            assert_eq!(p.extradata, rgb, "{name}: palette in extradata");
        } else {
            assert!(p.extradata.is_empty(), "{name}");
        }
        assert_eq!(stream.time_base, TimeBase::new(1, 1));
        assert_eq!(stream.start_time, Some(0));
    }
    // The committed fixtures pin these layouts (the sub-sampled
    // codestreams are uniformly sub-sampled, hence `Rgb24`); planar
    // YCbCr and the alpha layouts are pinned by the encoder-driven
    // round trip below.
    let formats: Vec<J2k> = FIXTURES
        .iter()
        .map(|(_, b, _)| info(b).unwrap().format)
        .collect();
    for want in [J2k::Gray8, J2k::Gray16Le, J2k::Rgb24, J2k::Pal8] {
        assert!(formats.contains(&want), "{want:?} not covered");
    }
}

#[test]
fn registry_frames_are_byte_identical_to_layer1() {
    let ctx = ctx();
    for (name, bytes, _) in FIXTURES {
        let img = decode(bytes).unwrap();
        let (stream, vf) = demux_decode(&ctx, bytes);
        assert_eq!(vf.pts, Some(0), "{name}");
        let planes = vf.image_planes();
        assert_eq!(planes.len(), img.planes.len(), "{name} plane count");
        for (k, (a, b)) in planes.iter().zip(&img.planes).enumerate() {
            assert_eq!(a.stride, b.stride, "{name} plane {k} stride");
            assert_eq!(a.data, b.data, "{name} plane {k} samples");
        }
        if img.format == J2k::Pal8 {
            assert_eq!(
                vf.palette().map(<[u8]>::to_vec),
                Some(stream.params.extradata.clone()),
                "{name}: frame palette == stream extradata"
            );
        }
        assert_eq!(
            vf.color_signal().is_some(),
            img.color.is_signalled(),
            "{name} colour"
        );
        if let Some(sig) = vf.color_signal() {
            assert_eq!(sig, stream.params.color_signal, "{name}");
        }
    }
}

#[test]
fn unsupported_component_set_opens_without_a_layout() {
    // Signed samples have no contract layout: `info` is `Unsupported`,
    // the demuxer still names the stream and its geometry, and the
    // decoder refuses the packet cleanly.
    let ctx = ctx();
    assert!(matches!(info(GRAY_S8), Err(Jpeg2000Error::Unsupported(_))));
    let hdr = parse_j2k_header(GRAY_S8).unwrap();
    let name = probe(&ctx, GRAY_S8, None).unwrap();
    assert_eq!(name, CONTAINER_J2K);
    let mut demux = ctx
        .containers
        .open_demuxer(&name, reader(GRAY_S8), &ctx.codecs)
        .unwrap();
    let stream = demux.streams()[0].clone();
    assert_eq!(stream.params.width, Some(hdr.image_width()));
    assert_eq!(stream.params.height, Some(hdr.image_height()));
    assert_eq!(stream.params.pixel_format, None);
    assert!(stream.params.color_signal.is_unspecified());
    let pkt = demux.next_packet().unwrap();
    let mut dec = ctx.codecs.first_decoder(&stream.params).unwrap();
    dec.send_packet(&pkt).unwrap();
    assert!(matches!(dec.receive_frame(), Err(Error::Unsupported(_))));
}

#[test]
fn demuxer_reports_icc_presence_in_metadata() {
    // Wrap a codestream in a JP2 header whose `colr` carries an ICC
    // profile: the demuxer flags it (the blob itself has no framework
    // carriage yet).
    let cs = encode(
        &Jpeg2000Image::from_rgb8(4, 2, vec![7; 24]).unwrap(),
        &EncodeOptions::default().with_container(Container::J2k),
    )
    .unwrap();
    let mut opts = Jp2WriteOptions::for_components(3);
    opts.colour = vec![Colr {
        method: ColrMethod::RestrictedIccProfile,
        precedence: 0,
        approximation: 0,
        enumerated: None,
        icc_profile: Some(vec![0u8; 132]),
        parameterized: None,
    }];
    let file = write_jp2(&cs, &opts).unwrap();
    let demux = open_demuxer(reader(&file), &NullCodecResolver).unwrap();
    assert_eq!(
        demux.metadata(),
        &[("icc".to_owned(), "present".to_owned())]
    );
    let plain = open_demuxer(reader(RGB_JP2), &NullCodecResolver).unwrap();
    assert!(plain.metadata().is_empty());
}

// ---- muxers -----------------------------------------------------------------

/// Encode `img` through the registry encoder (bare codestream unless
/// `container` says otherwise) and return the stream it declares plus
/// its packet.
fn encode_via_registry(img: &Jpeg2000Image, container: Option<&str>) -> (StreamInfo, Packet) {
    let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    params.width = Some(img.width);
    params.height = Some(img.height);
    params.pixel_format = Some(img.format.into());
    if let Some(c) = container {
        params.options = CodecOptions::new().set("container", c);
    }
    let mut enc = make_encoder(&params).unwrap();
    let frame = VideoFrame::from(img.clone());
    enc.send_frame(&Frame::Video(frame)).unwrap();
    let pkt = enc.receive_packet().unwrap();
    let mut out = enc.output_params().clone();
    // What the frame carried travels on the stream parameters.
    if img.color.is_signalled() {
        out.color_signal = to_color_signal(&img.color);
    }
    if let Some(p) = &img.palette {
        out.extradata = p.entries.iter().flat_map(|e| [e[0], e[1], e[2]]).collect();
    }
    let stream = StreamInfo {
        index: 0,
        params: out,
        time_base: TimeBase::new(1, 1),
        start_time: Some(0),
        duration: None,
    };
    (stream, pkt)
}

fn mux(
    ctx: &RuntimeContext,
    name: &str,
    stream: &StreamInfo,
    pkt: &Packet,
) -> oxideav_core::Result<Vec<u8>> {
    let sink = SharedSink::default();
    let out: Box<dyn WriteSeek> = Box::new(sink.clone());
    let mut m = ctx
        .containers
        .open_muxer(name, out, std::slice::from_ref(stream))?;
    assert_eq!(m.format_name(), name);
    m.write_header()?;
    m.write_packet(pkt)?;
    m.write_trailer()?;
    Ok(sink.bytes())
}

fn sample_images() -> Vec<Jpeg2000Image> {
    let (w, h) = (9u32, 6u32);
    let n = (w * h) as usize;
    let packed = |f: J2k, bpp: usize| {
        Jpeg2000Image::packed(
            w,
            h,
            f,
            (0..n * bpp).map(|i| (i * 37 % 251) as u8).collect(),
        )
        .unwrap()
    };
    let deep = |f: J2k, comps: usize| {
        Jpeg2000Image::packed(
            w,
            h,
            f,
            (0..n * comps)
                .map(|i| ((i * 2_749) % 65_536) as u16)
                .flat_map(u16::to_le_bytes)
                .collect(),
        )
        .unwrap()
    };
    let yuv420 = Jpeg2000Image::new(
        8,
        6,
        J2k::Yuv420P,
        vec![
            Plane::new(8, (0..48).map(|i| (i * 5) as u8).collect()),
            Plane::new(4, (0..12).map(|i| (100 + i * 7) as u8).collect()),
            Plane::new(4, (0..12).map(|i| (200 - i * 3) as u8).collect()),
        ],
    )
    .unwrap()
    .with_color(ColorInfo::sycc());
    let pal = Jpeg2000Image::packed(w, h, J2k::Pal8, (0..n).map(|i| (i % 4) as u8).collect())
        .unwrap()
        .with_palette(Palette::new(vec![
            [1, 2, 3, 255],
            [4, 5, 6, 255],
            [7, 8, 9, 255],
            [250, 251, 252, 255],
        ]))
        .with_color(ColorInfo::srgb());
    vec![
        packed(J2k::Gray8, 1),
        packed(J2k::Rgb24, 3).with_color(ColorInfo::srgb()),
        packed(J2k::Rgba, 4),
        deep(J2k::Gray16Le, 1),
        deep(J2k::Rgb48Le, 3),
        yuv420,
        pal,
    ]
}

#[test]
fn muxers_write_files_layer1_reads_back_and_the_registry_round_trips() {
    let ctx = ctx();
    for img in sample_images() {
        let f = img.format;
        // Pal8 needs the JP2 wrapper at the encoder already.
        let (stream, pkt) = encode_via_registry(&img, (f == J2k::Pal8).then_some("jp2"));
        // --- jp2: bare packet gets wrapped, JP2 packet passes through.
        let jp2 = mux(&ctx, CONTAINER_JP2, &stream, &pkt).unwrap();
        assert!(
            oxideav_jpeg2000::probe(&jp2) && info(&jp2).unwrap().jp2,
            "{f:?}"
        );
        let back = decode(&jp2).unwrap();
        assert_eq!(back.planes, img.planes, "{f:?} jp2 planes");
        assert_eq!(back.format, f, "{f:?} jp2 layout");
        assert_eq!(back.palette, img.palette, "{f:?} jp2 palette");
        if img.color.is_signalled() {
            assert_eq!(back.color, img.color, "{f:?} jp2 colour from the stream");
        }
        // --- jpeg2000: bare packet passes through, JP2 packet unwraps.
        let bare = mux(&ctx, CONTAINER_J2K, &stream, &pkt);
        if f == J2k::Pal8 {
            assert!(
                matches!(bare, Err(Error::Unsupported(_))),
                "Pal8 cannot ride a bare codestream"
            );
        } else {
            let bare = bare.unwrap();
            assert_eq!(&bare[..2], &[0xFF, 0x4F], "{f:?}");
            assert_eq!(
                decode(&bare).unwrap().planes,
                img.planes,
                "{f:?} bare planes"
            );
            // A JP2 packet through the bare muxer comes out as the
            // codestream the file wrapped.
            let jp2_pkt = Packet::new(0, TimeBase::new(1, 1), jp2.clone());
            let unwrapped = mux(&ctx, CONTAINER_J2K, &stream, &jp2_pkt).unwrap();
            assert_eq!(&unwrapped[..2], &[0xFF, 0x4F], "{f:?}");
            assert_eq!(decode(&unwrapped).unwrap().planes, img.planes, "{f:?}");
        }
        // --- registry round trip: demux(mux(frame)) == frame.
        let (stream2, vf) = demux_decode(&ctx, &jp2);
        assert_eq!(
            stream2.params.pixel_format,
            Some(PixelFormat::from(f)),
            "{f:?}"
        );
        let planes = vf.image_planes();
        assert_eq!(planes.len(), img.planes.len(), "{f:?}");
        for (a, b) in planes.iter().zip(&img.planes) {
            assert_eq!(a.data, b.data, "{f:?} round-trip samples");
        }
    }
}

#[test]
fn muxers_refuse_a_second_picture_and_foreign_packets() {
    let ctx = ctx();
    let img = sample_images().swap_remove(0);
    let (stream, pkt) = encode_via_registry(&img, None);
    for name in [CONTAINER_J2K, CONTAINER_JP2] {
        let out: Box<dyn WriteSeek> = Box::new(Cursor::new(Vec::new()));
        let mut m = ctx
            .containers
            .open_muxer(name, out, std::slice::from_ref(&stream))
            .unwrap();
        m.write_header().unwrap();
        m.write_packet(&pkt).unwrap();
        assert!(
            matches!(m.write_packet(&pkt), Err(Error::Unsupported(_))),
            "{name}"
        );
        // Zero-length and foreign payloads are errors, never panics.
        let out: Box<dyn WriteSeek> = Box::new(Cursor::new(Vec::new()));
        let mut m = ctx
            .containers
            .open_muxer(name, out, std::slice::from_ref(&stream))
            .unwrap();
        assert!(m
            .write_packet(&Packet::new(0, TimeBase::new(1, 1), Vec::new()))
            .is_err());
        assert!(m
            .write_packet(&Packet::new(0, TimeBase::new(1, 1), b"farbfeld".to_vec()))
            .is_err());
    }
    // Stream shape is validated at open.
    let out: Box<dyn WriteSeek> = Box::new(Cursor::new(Vec::new()));
    assert!(ctx.containers.open_muxer(CONTAINER_JP2, out, &[]).is_err());
    let mut other = stream.clone();
    other.params.codec_id = CodecId::new("png");
    let out: Box<dyn WriteSeek> = Box::new(Cursor::new(Vec::new()));
    assert!(ctx
        .containers
        .open_muxer(CONTAINER_J2K, out, std::slice::from_ref(&other))
        .is_err());
}

#[test]
fn jp2_muxer_wraps_a_codestream_whose_stream_names_no_layout() {
    // A bare packet with no usable `pixel_format` on the stream (what a
    // pipeline has before the first frame is encoded): the wrapper is
    // derived from the SIZ.
    let ctx = ctx();
    let img = Jpeg2000Image::from_rgba8(5, 4, (0..80).map(|i| (i * 3) as u8).collect()).unwrap();
    let (mut stream, pkt) = encode_via_registry(&img, None);
    stream.params.pixel_format = None;
    let jp2 = mux(&ctx, CONTAINER_JP2, &stream, &pkt).unwrap();
    let i = info(&jp2).unwrap();
    assert!(i.jp2 && i.format == J2k::Rgba && i.has_alpha);
    assert_eq!(decode(&jp2).unwrap().planes, img.planes);
    stream.params.pixel_format = Some(PixelFormat::Yuyv422);
    let jp2 = mux(&ctx, CONTAINER_JP2, &stream, &pkt).unwrap();
    assert_eq!(decode(&jp2).unwrap().planes, img.planes);
}

// ---- registration ---------------------------------------------------------------

#[test]
fn register_installs_codec_and_containers() {
    let ctx = ctx();
    let id = CodecId::new(CODEC_ID_STR);
    assert!(ctx.codecs.has_decoder(&id) && ctx.codecs.has_encoder(&id));
    for name in [CONTAINER_J2K, CONTAINER_JP2] {
        assert!(ctx.containers.demuxer_names().any(|n| n == name), "{name}");
        assert!(ctx.containers.muxer_names().any(|n| n == name), "{name}");
    }
    for (ext, name) in [
        ("j2k", CONTAINER_J2K),
        ("J2C", CONTAINER_J2K),
        ("jp2", CONTAINER_JP2),
        ("jph", CONTAINER_JP2),
    ] {
        assert_eq!(
            ctx.containers.container_for_extension(ext),
            Some(name),
            "{ext}"
        );
    }
    // The `register!` entry point installs the same set.
    let mut ctx2 = RuntimeContext::new();
    oxideav_jpeg2000::__oxideav_entry(&mut ctx2);
    assert!(ctx2.codecs.has_decoder(&id));
    assert_eq!(probe(&ctx2, RGB_JP2, None).unwrap(), CONTAINER_JP2);
    assert_eq!(probe(&ctx2, GRAY8, None).unwrap(), CONTAINER_J2K);
}

// ---- hostile input ----------------------------------------------------------------

#[test]
fn hostile_input_never_panics() {
    let ctx = ctx();
    // Truncations of every fixture: an error or a stream, never a panic.
    for (name, bytes, _) in FIXTURES {
        for cut in [0usize, 1, 2, 3, 4, 11, 12, 13, 20, 40, 60, bytes.len() / 2] {
            let cut = cut.min(bytes.len());
            let r = open_demuxer(reader(&bytes[..cut]), &NullCodecResolver);
            if let Ok(mut d) = r {
                let pkt = d.next_packet().unwrap();
                let mut p = d.streams()[0].params.clone();
                p.limits = DecoderLimits::default()
                    .with_max_pixels_per_frame(1 << 20)
                    .with_max_alloc_bytes_per_frame(64 << 20);
                if let Ok(mut dec) = ctx.codecs.first_decoder(&p) {
                    let _ = dec.send_packet(&pkt);
                    let _ = dec.receive_frame();
                }
                let _ = mux(&ctx, CONTAINER_JP2, &d.streams()[0], &pkt);
                let _ = mux(&ctx, CONTAINER_J2K, &d.streams()[0], &pkt);
            }
            let _ = name;
        }
    }
    // Absurd geometry: the header walk is header-only, so the stream may
    // open; the decoder then fails on its limits instead of allocating.
    let mut huge = GRAY8.to_vec();
    // SOC (0..2), SIZ marker (2..4), Lsiz (4..6), Rsiz (6..8), Xsiz (8..12),
    // Ysiz (12..16).
    assert_eq!(&huge[2..4], &[0xFF, 0x51]);
    huge[8..12].copy_from_slice(&0xFFFF_FFF0u32.to_be_bytes());
    huge[12..16].copy_from_slice(&0xFFFF_FFF0u32.to_be_bytes());
    if let Ok(mut d) = open_demuxer(reader(&huge), &NullCodecResolver) {
        let pkt = d.next_packet().unwrap();
        let mut p = d.streams()[0].params.clone();
        p.limits = DecoderLimits::default()
            .with_max_pixels_per_frame(1 << 20)
            .with_max_alloc_bytes_per_frame(64 << 20);
        let mut dec = ctx.codecs.first_decoder(&p).unwrap();
        dec.send_packet(&pkt).unwrap();
        assert!(dec.receive_frame().is_err());
    }
    // Zero-length input / packet.
    assert!(open_demuxer(reader(&[]), &NullCodecResolver).is_err());
    let out: Box<dyn WriteSeek> = Box::new(Cursor::new(Vec::new()));
    let (stream, _) = encode_via_registry(&sample_images()[0], None);
    let mut m = open_muxer_jp2(out, std::slice::from_ref(&stream)).unwrap();
    assert!(m
        .write_packet(&Packet::new(0, TimeBase::new(1, 1), Vec::new()))
        .is_err());
    let out: Box<dyn WriteSeek> = Box::new(Cursor::new(Vec::new()));
    let mut m = open_muxer_j2k(out, std::slice::from_ref(&stream)).unwrap();
    assert!(m
        .write_packet(&Packet::new(0, TimeBase::new(1, 1), Vec::new()))
        .is_err());
}
