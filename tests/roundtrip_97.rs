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
/// stay within the [-128, 127] 8-bit signed range. The fixed inverse
/// RCT (T.800 §G.2.2) handles the full ±255 chroma range, so the
/// "near-neutral" restriction is no longer needed for correctness —
/// but it stays as a sanity-check fixture; see
/// [`roundtrip_53_rgb_full_saturation_bit_exact`] below for the
/// fuller range.
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

/// Build a 32x32 RGB image with deliberately fully-saturated colour
/// blocks (R=255 G=0 B=0 etc.) so the RCT chroma components (Y1=B-G,
/// Y2=R-G — see T.800 §G.2.1 (G-4) and (G-5)) reach ±255 at the
/// extremes. Before the §G.1 ordering fix the inverse RCT operated on
/// chroma planes that had been clamped to [0, 255] after a spurious
/// +128 DC level shift, which truncated those excursions to ±127 and
/// produced ~75 % sample mismatches against `opj_decompress`. With
/// the fix the round-trip is bit-exact (the 5/3 path with MCT=1 is
/// fully reversible per spec).
fn build_rgb_full_saturation(w: u32, h: u32) -> VideoFrame {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let bx = x * 4 / w;
            let by = y * 4 / h;
            let (r, g, b) = match (bx, by) {
                (0, 0) => (255u8, 0, 0), // pure red — Cr extreme
                (3, 0) => (0, 255, 0),   // pure green — Cb=Cr=-255
                (0, 3) => (0, 0, 255),   // pure blue — Cb extreme
                (3, 3) => (255, 255, 0), // yellow — strong negative Cb
                (1, 1) => (0, 255, 255), // cyan
                (2, 2) => (255, 0, 255), // magenta
                _ => {
                    // varied mid-range
                    let r = ((x.wrapping_mul(7)).wrapping_add(y.wrapping_mul(3)) % 256) as u8;
                    let g = ((x.wrapping_mul(5)).wrapping_add(y.wrapping_mul(11)) % 256) as u8;
                    let b = ((x.wrapping_mul(13)).wrapping_add(y.wrapping_mul(2)) % 256) as u8;
                    (r, g, b)
                }
            };
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
fn roundtrip_53_rgb_full_saturation_bit_exact() {
    let src = build_rgb_full_saturation(32, 32);
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
    let nm = decoded_packed
        .iter()
        .zip(src.planes[0].data.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        nm, 0,
        "5/3 RGB roundtrip with fully-saturated colour blocks must be bit-exact \
         (the RCT is reversible per T.800 §G.2): {nm} sample diffs"
    );
}
