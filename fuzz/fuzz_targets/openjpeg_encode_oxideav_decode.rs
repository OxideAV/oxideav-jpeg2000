#![no_main]

//! Cross-decode fuzz: encode an arbitrary RGB image with OpenJPEG's
//! lossless 5/3 path, then decode the resulting `.j2k` codestream
//! through oxideav-jpeg2000. The decoded RGB must match the original
//! input byte-for-byte (5/3 reversible is bit-exact).
//!
//! Skips silently when libopenjp2 isn't installed on the host.

use libfuzzer_sys::fuzz_target;
use oxideav_core::{CodecId, CodecParameters, CodecRegistry, Frame, Packet, TimeBase, VideoFrame};
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

    let Some(encoded) = openjpeg::encode_lossless_rgb(rgb, width, height) else {
        // OpenJPEG legitimately rejects some inputs (e.g. very small
        // dimensions for the chosen DWT depth). Don't treat those as
        // harness failures.
        return;
    };

    let Some(decoded) = decode_with_oxideav(&encoded) else {
        // Our decoder failing on a libopenjp2-encoded codestream is a
        // real bug — but only if the bytes are actually valid. We
        // assume libopenjp2 produced a conforming codestream when it
        // returned success, so the decode failure is the bug.
        panic!("oxideav-jpeg2000 failed to decode an OpenJPEG-emitted lossless codestream");
    };

    assert_eq!(decoded.planes.len(), 3, "decoded must have 3 planes");
    let plane_len = (width as usize) * (height as usize);
    for p in &decoded.planes {
        assert_eq!(p.data.len(), plane_len, "plane size mismatch");
    }

    let mut got = Vec::with_capacity(rgb.len());
    for i in 0..plane_len {
        got.push(decoded.planes[0].data[i]);
        got.push(decoded.planes[1].data[i]);
        got.push(decoded.planes[2].data[i]);
    }
    assert_eq!(
        got.as_slice(),
        rgb,
        "OpenJPEG lossless encode → oxideav decode must be bit-exact",
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

fn decode_with_oxideav(bytes: &[u8]) -> Option<VideoFrame> {
    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.make_decoder(&params).ok()?;
    let pkt = Packet::new(0u32, TimeBase::new(1, 1), bytes.to_vec());
    dec.send_packet(&pkt).ok()?;
    match dec.receive_frame().ok()? {
        Frame::Video(v) => Some(v),
        _ => None,
    }
}
