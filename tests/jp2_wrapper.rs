//! JP2 wrapper round-trip test. Encodes a small image with the JP2
//! ISOBMFF boxes enabled, parses the box structure by hand to verify
//! the required boxes are present in the right order, extracts the
//! inner `.j2k` codestream via `extract_jp2_codestream`, and decodes
//! it through our own decoder for an end-to-end sanity check.

use oxideav_codec::CodecRegistry;
use oxideav_core::{
    CodecId, CodecParameters, Frame, Packet, PixelFormat, TimeBase, VideoFrame, VideoPlane,
};
use oxideav_jpeg2000::encode::{
    encode_frame, extract_jp2_codestream, EncodeOptions, TransformMode,
};

fn build_gray(w: u32, h: u32) -> VideoFrame {
    let mut data = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = (((x + y) * 255) / (w + h - 2).max(1)).min(255) as u8;
            data.push(v);
        }
    }
    VideoFrame {
        format: PixelFormat::Gray8,
        width: w,
        height: h,
        pts: None,
        time_base: TimeBase::new(1, 1),
        planes: vec![VideoPlane {
            stride: w as usize,
            data,
        }],
    }
}

/// Very simple ISOBMFF box walker. Returns `(type, payload)` tuples.
fn walk_boxes(buf: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 <= buf.len() {
        let size = u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) as usize;
        let ty = std::str::from_utf8(&buf[i + 4..i + 8])
            .unwrap_or("????")
            .to_string();
        if size < 8 || i + size > buf.len() {
            break;
        }
        out.push((ty, buf[i + 8..i + size].to_vec()));
        i += size;
    }
    out
}

#[test]
fn jp2_wrapper_has_required_boxes() {
    let src = build_gray(64, 64);
    let opts = EncodeOptions {
        jp2_wrapper: true,
        ..Default::default()
    };
    let bytes = encode_frame(&Frame::Video(src.clone()), &opts).expect("encode");

    // The first 12 bytes must be the JP2 signature box:
    //   00 00 00 0C 6A 50 20 20 0D 0A 87 0A
    assert!(bytes.len() >= 12, "jp2 too small");
    assert_eq!(
        &bytes[..12],
        &[0, 0, 0, 12, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A],
        "jp2 signature box mismatch"
    );

    let boxes = walk_boxes(&bytes);
    let types: Vec<&str> = boxes.iter().map(|(t, _)| t.as_str()).collect();
    assert!(
        types.contains(&"jP  "),
        "missing signature box: {:?}",
        types
    );
    assert!(types.contains(&"ftyp"), "missing ftyp box: {:?}", types);
    assert!(types.contains(&"jp2h"), "missing jp2h box: {:?}", types);
    assert!(types.contains(&"jp2c"), "missing jp2c box: {:?}", types);

    // ftyp payload: major brand "jp2 " + minor version 0 + compat
    // list containing "jp2 ".
    let ftyp = boxes.iter().find(|(t, _)| t == "ftyp").expect("ftyp box");
    assert!(ftyp.1.starts_with(b"jp2 "), "ftyp major brand not jp2");

    // Walk jp2h super-box: must contain ihdr and colr.
    let jp2h = boxes.iter().find(|(t, _)| t == "jp2h").expect("jp2h box");
    let sub = walk_boxes(&jp2h.1);
    let sub_types: Vec<&str> = sub.iter().map(|(t, _)| t.as_str()).collect();
    assert!(
        sub_types.contains(&"ihdr"),
        "jp2h missing ihdr: {:?}",
        sub_types
    );
    assert!(
        sub_types.contains(&"colr"),
        "jp2h missing colr: {:?}",
        sub_types
    );

    // ihdr encodes height × width and component count.
    let ihdr = sub.iter().find(|(t, _)| t == "ihdr").expect("ihdr box");
    let hp = u32::from_be_bytes([ihdr.1[0], ihdr.1[1], ihdr.1[2], ihdr.1[3]]);
    let wp = u32::from_be_bytes([ihdr.1[4], ihdr.1[5], ihdr.1[6], ihdr.1[7]]);
    let nc = u16::from_be_bytes([ihdr.1[8], ihdr.1[9]]);
    assert_eq!(hp, 64);
    assert_eq!(wp, 64);
    assert_eq!(nc, 1);
    assert_eq!(ihdr.1[11], 7, "ihdr compression type must be 7 (JPEG 2000)");

    // colr: method=1, enum CS = 17 (greyscale).
    let colr = sub.iter().find(|(t, _)| t == "colr").expect("colr box");
    assert_eq!(colr.1[0], 1, "colr method must be enumerated (1)");
    let enum_cs = u32::from_be_bytes([colr.1[3], colr.1[4], colr.1[5], colr.1[6]]);
    assert_eq!(enum_cs, 17, "colr enum CS for gray must be 17");
}

#[test]
fn jp2_inner_codestream_decodes_back() {
    let src = build_gray(64, 64);
    let opts = EncodeOptions {
        jp2_wrapper: true,
        ..Default::default()
    };
    let bytes = encode_frame(&Frame::Video(src.clone()), &opts).expect("encode");

    // Extract and feed the inner j2k to the decoder.
    let cs = extract_jp2_codestream(&bytes).expect("extract jp2c");
    assert_eq!(
        &cs[..2],
        &[0xFF, 0x4F],
        "extracted stream must start with SOC"
    );

    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.make_decoder(&params).expect("factory");
    let pkt = Packet::new(0, TimeBase::new(1, 1), cs);
    dec.send_packet(&pkt).expect("send_packet");
    let frame = dec.receive_frame().expect("receive_frame");
    let vf = match frame {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    };
    assert_eq!(vf.width, 64);
    assert_eq!(vf.height, 64);
    assert_eq!(vf.format, PixelFormat::Gray8);
    assert_eq!(vf.planes[0].data, src.planes[0].data);
}

#[test]
fn jp2_full_wrapper_decodes_transparently() {
    // Feed the entire JP2 container (including the signature box) to
    // the decoder — it must auto-detect the wrapper, extract the
    // inner j2k codestream, and decode bit-exactly.
    let src = build_gray(64, 64);
    let opts = EncodeOptions {
        jp2_wrapper: true,
        ..Default::default()
    };
    let bytes = encode_frame(&Frame::Video(src.clone()), &opts).expect("encode");

    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.make_decoder(&params).expect("factory");
    let pkt = Packet::new(0, TimeBase::new(1, 1), bytes);
    dec.send_packet(&pkt).expect("send_packet");
    let frame = dec.receive_frame().expect("receive_frame");
    let vf = match frame {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    };
    assert_eq!(vf.planes[0].data, src.planes[0].data);
}

#[test]
fn jp2_wrapper_on_9p7_rgb_encodes_and_decodes() {
    // Build an RGB image.
    let mut data = Vec::with_capacity(32 * 32 * 3);
    for y in 0..32 {
        for x in 0..32 {
            data.push(((x + y) * 4) as u8);
            data.push((x * 4) as u8);
            data.push((y * 4) as u8);
        }
    }
    let src = VideoFrame {
        format: PixelFormat::Rgb24,
        width: 32,
        height: 32,
        pts: None,
        time_base: TimeBase::new(1, 1),
        planes: vec![VideoPlane {
            stride: 32 * 3,
            data,
        }],
    };
    let opts = EncodeOptions {
        transform: TransformMode::Irreversible97,
        jp2_wrapper: true,
        ..Default::default()
    };
    let bytes = encode_frame(&Frame::Video(src), &opts).expect("encode");
    assert_eq!(
        &bytes[..12],
        &[0, 0, 0, 12, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A]
    );
    let cs = extract_jp2_codestream(&bytes).expect("extract jp2c");
    assert_eq!(&cs[..2], &[0xFF, 0x4F]);

    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.make_decoder(&params).expect("factory");
    let pkt = Packet::new(0, TimeBase::new(1, 1), cs);
    dec.send_packet(&pkt).expect("send_packet");
    let frame = dec.receive_frame().expect("receive_frame");
    let vf = match frame {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    };
    assert_eq!(vf.width, 32);
    assert_eq!(vf.height, 32);
}
