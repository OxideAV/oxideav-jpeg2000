//! End-to-end encode → decode round-trip test on the 5/3 reversible
//! lossless path. Exercises the complete write-side pipeline
//! (codestream markers, tier-2 packets, tier-1 EBCOT, forward DWT) and
//! then verifies the resulting bytes reload through the matching
//! decoder into a bit-exact copy of the original planar image.

use oxideav_codec::CodecRegistry;
use oxideav_core::{
    CodecId, CodecParameters, Frame, Packet, PixelFormat, TimeBase, VideoFrame, VideoPlane,
};
use oxideav_jpeg2000::encode::{encode_frame, EncodeOptions};

fn build_gradient(w: u32, h: u32) -> VideoFrame {
    let mut data = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            // Diagonal gradient 0..=255 clipped.
            let v = ((x + y) * 255 / (w + h - 2)).min(255) as u8;
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

fn build_constant(w: u32, h: u32, value: u8) -> VideoFrame {
    VideoFrame {
        format: PixelFormat::Gray8,
        width: w,
        height: h,
        pts: None,
        time_base: TimeBase::new(1, 1),
        planes: vec![VideoPlane {
            stride: w as usize,
            data: vec![value; (w * h) as usize],
        }],
    }
}

fn decode(bytes: &[u8]) -> VideoFrame {
    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.make_decoder(&params).expect("factory");
    let pkt = Packet::new(0, TimeBase::new(1, 1), bytes.to_vec());
    dec.send_packet(&pkt).expect("send_packet");
    match dec.receive_frame().expect("receive_frame") {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    }
}

#[test]
fn roundtrip_constant_gray_is_bit_exact() {
    let src = build_constant(64, 64, 137);
    let bytes =
        encode_frame(&Frame::Video(src.clone()), &EncodeOptions::default()).expect("encode");
    let dec = decode(&bytes);
    assert_eq!(dec.width, src.width);
    assert_eq!(dec.height, src.height);
    assert_eq!(dec.format, PixelFormat::Gray8);
    assert_eq!(dec.planes.len(), 1);
    assert_eq!(
        dec.planes[0].data, src.planes[0].data,
        "constant image must round-trip bit-exactly on 5/3 lossless",
    );
}

#[test]
fn roundtrip_16x16_one_decomp_level_is_bit_exact() {
    // Mirror the `opj16_l1.j2k` fixture's layout (16x16 Gray8, 1-level
    // DWT) so the round-trip exercises the same decoder code path used
    // at OpenJPEG interop.
    let src = build_gradient(16, 16);
    let opts = EncodeOptions {
        num_decomp: 1,
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(&Frame::Video(src.clone()), &opts).expect("encode");
    let dec = decode(&bytes);
    assert_eq!(
        dec.planes[0].data, src.planes[0].data,
        "16x16 gradient at 1 DWT level must round-trip bit-exactly"
    );
}

#[test]
fn roundtrip_gradient_is_bit_exact() {
    let src = build_gradient(64, 64);
    let bytes =
        encode_frame(&Frame::Video(src.clone()), &EncodeOptions::default()).expect("encode");
    let dec = decode(&bytes);
    assert_eq!(dec.width, src.width);
    assert_eq!(dec.height, src.height);
    // Compare a few sample points first — makes failures easier to
    // read than a giant `assert_eq` on 4096 bytes.
    for y in (0..src.height as usize).step_by(8) {
        for x in (0..src.width as usize).step_by(8) {
            let i = y * src.planes[0].stride + x;
            assert_eq!(
                dec.planes[0].data[i], src.planes[0].data[i],
                "mismatch at ({x}, {y})"
            );
        }
    }
    assert_eq!(
        dec.planes[0].data, src.planes[0].data,
        "gradient image must round-trip bit-exactly on 5/3 lossless",
    );
}
