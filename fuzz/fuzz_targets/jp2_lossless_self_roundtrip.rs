#![no_main]

//! Self-roundtrip fuzz: encode an arbitrary RGB image with
//! oxideav-jpeg2000's 5/3 reversible (lossless) path, then decode it
//! back through the same crate. The result must be bit-exact.
//!
//! This catches encoder / decoder co-evolution bugs without needing
//! an external oracle, so it runs even on hosts without OpenJPEG.

use libfuzzer_sys::fuzz_target;
use oxideav_core::{
    CodecId, CodecParameters, CodecRegistry, Frame, Packet, PixelFormat, TimeBase, VideoFrame,
    VideoPlane,
};
use oxideav_jpeg2000::encode::{encode_frame, EncodeOptions, TransformMode};

const MAX_WIDTH: usize = 64;
const MAX_PIXELS: usize = 2048;

fuzz_target!(|data: &[u8]| {
    let Some((width, height, rgb)) = image_from_fuzz_input(data) else {
        return;
    };

    let frame = build_rgb_frame(width, height, rgb);
    let opts = EncodeOptions {
        // 5/3 integer reversible — bit-exact lossless.
        transform: TransformMode::Reversible53,
        // One DWT level keeps tiny inputs (1×N or N×1) valid; the
        // default 5 levels would fail the "image must be at least
        // 2^num_decomp on each side after sub-sampling" check that
        // both encoder and decoder enforce.
        num_decomp: 1,
        ..EncodeOptions::default()
    };
    let Ok(bytes) = encode_frame(&frame, width, height, PixelFormat::Rgb24, &opts) else {
        // The encoder may legitimately reject some inputs (e.g. 1×1
        // with num_decomp=1). Skip silently — the harness's job is to
        // find unexpected panics, not to assert encoding always
        // succeeds.
        return;
    };

    let decoded = decode_with_oxideav(&bytes);
    let dec_vf = match decoded {
        Some(v) => v,
        None => return,
    };

    assert_eq!(dec_vf.planes.len(), 3, "decoded must have 3 planes");
    let plane_len = (width as usize) * (height as usize);
    for p in &dec_vf.planes {
        assert_eq!(p.data.len(), plane_len, "plane size mismatch");
    }

    // Decoder emits planar R / G / B; rebuild the interleaved RGB and
    // compare against the input.
    let mut got = Vec::with_capacity(rgb.len());
    for i in 0..plane_len {
        got.push(dec_vf.planes[0].data[i]);
        got.push(dec_vf.planes[1].data[i]);
        got.push(dec_vf.planes[2].data[i]);
    }
    assert_eq!(
        got.as_slice(),
        rgb,
        "5/3 lossless self-roundtrip must be bit-exact",
    );
});

fn image_from_fuzz_input(data: &[u8]) -> Option<(u32, u32, &[u8])> {
    let (&shape, rgb) = data.split_first()?;

    let pixel_count = (rgb.len() / 3).min(MAX_PIXELS);
    if pixel_count == 0 {
        return None;
    }

    let width = ((shape as usize) % MAX_WIDTH) + 1;
    let width = width.min(pixel_count);
    let height = pixel_count / width;
    if height == 0 {
        return None;
    }
    let used_len = width * height * 3;
    let rgb = &rgb[..used_len];

    Some((width as u32, height as u32, rgb))
}

fn build_rgb_frame(width: u32, height: u32, rgb: &[u8]) -> Frame {
    Frame::Video(VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: (width as usize) * 3,
            data: rgb.to_vec(),
        }],
    })
}

fn decode_with_oxideav(bytes: &[u8]) -> Option<VideoFrame> {
    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.first_decoder(&params).ok()?;
    let pkt = Packet::new(0u32, TimeBase::new(1, 1), bytes.to_vec());
    dec.send_packet(&pkt).ok()?;
    match dec.receive_frame().ok()? {
        Frame::Video(v) => Some(v),
        _ => None,
    }
}
