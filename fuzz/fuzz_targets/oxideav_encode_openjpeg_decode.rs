#![no_main]

//! Cross-decode fuzz: encode an arbitrary RGB image with
//! oxideav-jpeg2000's lossless 5/3 path, then decode it through
//! OpenJPEG. The decoded RGB must match the original input byte-for-
//! byte (5/3 reversible is bit-exact).
//!
//! Skips silently when libopenjp2 isn't installed on the host.

use libfuzzer_sys::fuzz_target;
use oxideav_core::{Frame, PixelFormat, VideoFrame, VideoPlane};
use oxideav_jpeg2000::encode::{encode_frame, EncodeOptions, TransformMode};
use oxideav_jpeg2000_fuzz::openjpeg;

const MAX_WIDTH: usize = 64;
const MAX_PIXELS: usize = 2048;

fuzz_target!(|data: &[u8]| {
    if !openjpeg::available() {
        return;
    }

    let Some((width, height, rgb)) = image_from_fuzz_input(data) else {
        return;
    };

    let frame = build_rgb_frame(width, height, rgb);
    let opts = EncodeOptions {
        transform: TransformMode::Reversible53,
        // One DWT level keeps tiny inputs valid (see
        // jp2_lossless_self_roundtrip for the rationale).
        num_decomp: 1,
        ..EncodeOptions::default()
    };
    let Ok(encoded) = encode_frame(&frame, width, height, PixelFormat::Rgb24, &opts) else {
        // Encoder rejected the input — not a harness failure.
        return;
    };

    let Some(decoded) = openjpeg::decode_to_rgb(&encoded) else {
        // OpenJPEG failing to decode an oxideav-emitted lossless
        // codestream is a real bug.
        panic!("OpenJPEG failed to decode an oxideav-jpeg2000-emitted lossless codestream");
    };

    assert_eq!(decoded.width, width);
    assert_eq!(decoded.height, height);
    assert_eq!(
        decoded.rgb.as_slice(),
        rgb,
        "oxideav lossless encode → OpenJPEG decode must be bit-exact",
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

fn build_rgb_frame(width: u32, _height: u32, rgb: &[u8]) -> Frame {
    Frame::Video(VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: (width as usize) * 3,
            data: rgb.to_vec(),
        }],
    })
}
