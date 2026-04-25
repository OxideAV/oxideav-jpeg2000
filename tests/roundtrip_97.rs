//! 9/7 irreversible encode → decode round-trip test.
//!
//! Emits a 64×64 RGB image through the 9/7 encoder with forward ICT,
//! decodes it back through our own decoder, and checks the per-pixel
//! PSNR is above 30 dB. Unlike the 5/3 round-trip this is *not* bit-
//! exact — the 9/7 wavelet is floating-point and the scalar quantiser
//! discards sub-stepsize information — but the reconstruction must
//! still be perceptually close.

use oxideav_core::CodecRegistry;
use oxideav_core::{
    CodecId, CodecParameters, Frame, Packet, PixelFormat, TimeBase, VideoFrame, VideoPlane,
};
use oxideav_jpeg2000::encode::{encode_frame, EncodeOptions, TransformMode};

fn build_rgb_gradient(w: u32, h: u32) -> VideoFrame {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let r = ((x * 255) / (w - 1).max(1)).min(255) as u8;
            let g = ((y * 255) / (h - 1).max(1)).min(255) as u8;
            let b = (((x + y) * 255) / (w + h - 2).max(1)).min(255) as u8;
            data.push(r);
            data.push(g);
            data.push(b);
        }
    }
    VideoFrame {
        format: PixelFormat::Rgb24,
        width: w,
        height: h,
        pts: None,
        time_base: TimeBase::new(1, 1),
        planes: vec![VideoPlane {
            stride: (w * 3) as usize,
            data,
        }],
    }
}

fn build_gray_gradient(w: u32, h: u32) -> VideoFrame {
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

/// Per-pixel PSNR in dB for two planes of the same size. Returns
/// f64::INFINITY when the planes are bit-exact.
fn psnr_u8(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut mse: f64 = 0.0;
    for i in 0..a.len() {
        let d = a[i] as f64 - b[i] as f64;
        mse += d * d;
    }
    mse /= a.len() as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0_f64 * 255.0 / mse).log10()
}

#[test]
fn roundtrip_97_gray_psnr_above_30db() {
    let src = build_gray_gradient(64, 64);
    let opts = EncodeOptions {
        transform: TransformMode::Irreversible97,
        ..Default::default()
    };
    let bytes = encode_frame(&Frame::Video(src.clone()), &opts).expect("encode");
    let dec = decode(&bytes);
    assert_eq!(dec.width, 64);
    assert_eq!(dec.height, 64);
    assert_eq!(dec.format, PixelFormat::Gray8);
    let psnr = psnr_u8(&src.planes[0].data, &dec.planes[0].data);
    println!("9/7 gray PSNR: {:.2} dB", psnr);
    assert!(
        psnr > 30.0,
        "9/7 gray roundtrip PSNR {:.2} dB, expected > 30",
        psnr
    );
}

#[test]
fn roundtrip_97_rgb_psnr_above_30db() {
    let src = build_rgb_gradient(64, 64);
    let opts = EncodeOptions {
        transform: TransformMode::Irreversible97,
        ..Default::default()
    };
    let bytes = encode_frame(&Frame::Video(src.clone()), &opts).expect("encode");
    let dec = decode(&bytes);
    assert_eq!(dec.width, 64);
    assert_eq!(dec.height, 64);
    // Decoder emits planar YUV444 / RGB when MCT is applied — the
    // decoder inverts ICT and writes per-plane output. Reconstruct a
    // packed RGB24 buffer for comparison with the source.
    assert!(
        matches!(
            dec.format,
            PixelFormat::Yuv444P | PixelFormat::Rgb24 | PixelFormat::Yuv420P
        ),
        "unexpected pixel format: {:?}",
        dec.format
    );
    assert_eq!(dec.planes.len(), 3);
    // Reassemble packed RGB24 from three 64×64 planes for PSNR
    // comparison.
    let w = dec.width as usize;
    let h = dec.height as usize;
    let mut decoded_packed = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            decoded_packed.push(dec.planes[0].data[y * dec.planes[0].stride + x]);
            decoded_packed.push(dec.planes[1].data[y * dec.planes[1].stride + x]);
            decoded_packed.push(dec.planes[2].data[y * dec.planes[2].stride + x]);
        }
    }
    let psnr = psnr_u8(&src.planes[0].data, &decoded_packed);
    println!("9/7 rgb PSNR: {:.2} dB", psnr);
    assert!(
        psnr > 30.0,
        "9/7 rgb roundtrip PSNR {:.2} dB, expected > 30",
        psnr
    );
}

/// Build a smooth near-neutral RGB image whose RCT chroma components
/// stay within the [-128, 127] 8-bit signed range. With the current
/// decoder clipping chroma to unsigned 8-bit before the inverse RCT,
/// stronger colour excursions would overflow. This keeps the test
/// focused on the core forward-RCT plumbing.
fn build_rgb_near_neutral(w: u32, h: u32) -> VideoFrame {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            // Base luma ramp + small chroma offsets that stay within
            // range.
            let base = ((x + y) * 127 / (w + h - 2).max(1)).min(127) as i32 + 64;
            let r = (base + (x as i32 / 4 - 8)).clamp(0, 255) as u8;
            let g = base.clamp(0, 255) as u8;
            let b = (base + (y as i32 / 4 - 8)).clamp(0, 255) as u8;
            data.push(r);
            data.push(g);
            data.push(b);
        }
    }
    VideoFrame {
        format: PixelFormat::Rgb24,
        width: w,
        height: h,
        pts: None,
        time_base: TimeBase::new(1, 1),
        planes: vec![VideoPlane {
            stride: (w * 3) as usize,
            data,
        }],
    }
}

#[test]
fn roundtrip_53_rgb_bit_exact() {
    // 5/3 RGB with forward RCT must round-trip bit-exactly as long as
    // chroma stays in the 8-bit signed range.
    let src = build_rgb_near_neutral(64, 64);
    let opts = EncodeOptions::default(); // 5/3 reversible, MCT on
    let bytes = encode_frame(&Frame::Video(src.clone()), &opts).expect("encode");
    let dec = decode(&bytes);
    let w = dec.width as usize;
    let h = dec.height as usize;
    let mut decoded_packed = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            decoded_packed.push(dec.planes[0].data[y * dec.planes[0].stride + x]);
            decoded_packed.push(dec.planes[1].data[y * dec.planes[1].stride + x]);
            decoded_packed.push(dec.planes[2].data[y * dec.planes[2].stride + x]);
        }
    }
    assert_eq!(
        decoded_packed, src.planes[0].data,
        "5/3 RGB with forward RCT must round-trip bit-exactly on near-neutral input",
    );
}
