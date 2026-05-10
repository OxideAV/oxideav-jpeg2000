//! End-to-end encode → decode round-trip test on the 5/3 reversible
//! lossless path. Exercises the complete write-side pipeline
//! (codestream markers, tier-2 packets, tier-1 EBCOT, forward DWT) and
//! then verifies the resulting bytes reload through the matching
//! decoder into a bit-exact copy of the original planar image.

use oxideav_core::CodecRegistry;
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
        pts: None,
        planes: vec![VideoPlane {
            stride: w as usize,
            data,
        }],
    }
}

fn build_constant(w: u32, h: u32, value: u8) -> VideoFrame {
    VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: w as usize,
            data: vec![value; (w * h) as usize],
        }],
    }
}

fn decode(bytes: &[u8]) -> VideoFrame {
    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register_codecs(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.first_decoder(&params).expect("factory");
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
    let bytes = encode_frame(
        &Frame::Video(src.clone()),
        64,
        64,
        PixelFormat::Gray8,
        &EncodeOptions::default(),
    )
    .expect("encode");
    let dec = decode(&bytes);
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
    let bytes = encode_frame(
        &Frame::Video(src.clone()),
        16,
        16,
        PixelFormat::Gray8,
        &opts,
    )
    .expect("encode");
    let dec = decode(&bytes);
    assert_eq!(
        dec.planes[0].data, src.planes[0].data,
        "16x16 gradient at 1 DWT level must round-trip bit-exactly"
    );
}

/// Round-1 fuzz regressions — degenerate single-row / single-column
/// inputs at `num_decomp = 1`. Before the fix, the encoder skipped the
/// forward DWT entirely whenever **either** axis was < 2 while still
/// signalling `num_decomp = 1` in COD; the decoder then ran a full
/// inverse DWT on raw level-shifted samples and produced garbage —
/// caught by both `oxideav_encode_openjpeg_decode` and
/// `jp2_lossless_self_roundtrip` fuzz harnesses.
///
/// Per T.800 §F.4.2 the 1-D analysis on a length-1 axis is a no-op (the
/// HP band is empty), so the fix is to keep applying the level as long
/// as **either** axis has length >= 2 — the canonical band layout
/// (`[LL, HL]` for 2x1, `[LL; LH]` for 1xN) is what the matching
/// `build_subbands` produces.
#[test]
fn roundtrip_2x1_rgb_one_decomp_is_bit_exact() {
    // Reproducer bytes from
    // fuzz/artifacts/oxideav_encode_openjpeg_decode/crash-29ee71...
    let rgb: &[u8] = &[0x04, 0x2c, 0xc4, 0x71, 0x04, 0x2c];
    let src = VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: 6,
            data: rgb.to_vec(),
        }],
    };
    let opts = EncodeOptions {
        num_decomp: 1,
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(&Frame::Video(src), 2, 1, PixelFormat::Rgb24, &opts).expect("encode");
    let dec = decode(&bytes);
    assert_eq!(dec.planes.len(), 3);
    let mut got = Vec::with_capacity(rgb.len());
    for i in 0..2 {
        got.push(dec.planes[0].data[i]);
        got.push(dec.planes[1].data[i]);
        got.push(dec.planes[2].data[i]);
    }
    assert_eq!(got.as_slice(), rgb, "2x1 RGB at NL=1 must round-trip");
}

#[test]
fn roundtrip_1x2_rgb_one_decomp_is_bit_exact() {
    let rgb: &[u8] = &[0x12, 0x34, 0x56, 0xab, 0xcd, 0xef];
    let src = VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: 3,
            data: rgb.to_vec(),
        }],
    };
    let opts = EncodeOptions {
        num_decomp: 1,
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(&Frame::Video(src), 1, 2, PixelFormat::Rgb24, &opts).expect("encode");
    let dec = decode(&bytes);
    let mut got = Vec::with_capacity(rgb.len());
    for i in 0..2 {
        got.push(dec.planes[0].data[i]);
        got.push(dec.planes[1].data[i]);
        got.push(dec.planes[2].data[i]);
    }
    assert_eq!(got.as_slice(), rgb, "1x2 RGB at NL=1 must round-trip");
}

/// Round-1 fuzz regression — `1×129` RGB at `num_decomp = 1` produces
/// a band where the LL_1 sub-band is `1×65`, which spans **two**
/// `64×64` code-blocks vertically. The previous tier-2 emitter wrote
/// (i) ALL inclusion bits → ALL zero-bitplane bits → ALL num-passes
/// bits, while the decoder reads them INTERLEAVED per cblk; and (ii) a
/// per-cblk `OneLeafTree` for the zero-bitplane tag tree, while the
/// decoder uses a SHARED per-precinct tag tree (T.800 §B.10.4 +
/// §B.10.7). With ≥ 2 cblks per precinct, both shortcuts produced a
/// stream that decoded to all-`0x80` (the level-shift constant —
/// every packet read as empty) — caught by `jp2_lossless_self_roundtrip`
/// fuzz at minimised 388-byte input. Fixed by interleaving the four
/// per-cblk bit streams and replacing `OneLeafTree` with the shared
/// `TagTreeEnc`.
#[test]
fn roundtrip_1x129_rgb_one_decomp_two_cblks_per_band() {
    let rgb: Vec<u8> = (0..(129u32 * 3)).map(|i| (i % 256) as u8).collect();
    let src = VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: 3,
            data: rgb.clone(),
        }],
    };
    let opts = EncodeOptions {
        num_decomp: 1,
        ..EncodeOptions::default()
    };
    let bytes =
        encode_frame(&Frame::Video(src), 1, 129, PixelFormat::Rgb24, &opts).expect("encode");
    let dec = decode(&bytes);
    let mut got = Vec::with_capacity(rgb.len());
    for i in 0..129 {
        got.push(dec.planes[0].data[i]);
        got.push(dec.planes[1].data[i]);
        got.push(dec.planes[2].data[i]);
    }
    assert_eq!(
        got.as_slice(),
        rgb.as_slice(),
        "1x129 RGB at NL=1 (LL_1=1x65 -> 2 cblks vertically) must round-trip"
    );
}

#[test]
fn roundtrip_129x1_rgb_one_decomp_two_cblks_per_band() {
    let rgb: Vec<u8> = (0..(129u32 * 3)).map(|i| (i % 256) as u8).collect();
    let src = VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: 129 * 3,
            data: rgb.clone(),
        }],
    };
    let opts = EncodeOptions {
        num_decomp: 1,
        ..EncodeOptions::default()
    };
    let bytes =
        encode_frame(&Frame::Video(src), 129, 1, PixelFormat::Rgb24, &opts).expect("encode");
    let dec = decode(&bytes);
    let mut got = Vec::with_capacity(rgb.len());
    for i in 0..129 {
        got.push(dec.planes[0].data[i]);
        got.push(dec.planes[1].data[i]);
        got.push(dec.planes[2].data[i]);
    }
    assert_eq!(
        got.as_slice(),
        rgb.as_slice(),
        "129x1 RGB at NL=1 must round-trip"
    );
}

/// Force ≥ 2 cblks per band via a small explicit `cblk_*_log2` (16x16
/// blocks) so the multi-cblk path runs at modest image sizes and the
/// regression coverage doesn't depend on the default 64x64 cblk size.
#[test]
fn roundtrip_small_cblk_multi_cblks_per_band() {
    let rgb: Vec<u8> = (0..(40u32 * 3)).map(|i| (i % 256) as u8).collect();
    let src = VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: 3,
            data: rgb.clone(),
        }],
    };
    let opts = EncodeOptions {
        num_decomp: 1,
        cblk_w_log2: 4,
        cblk_h_log2: 4,
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(&Frame::Video(src), 1, 40, PixelFormat::Rgb24, &opts).expect("encode");
    let dec = decode(&bytes);
    let mut got = Vec::with_capacity(rgb.len());
    for i in 0..40 {
        got.push(dec.planes[0].data[i]);
        got.push(dec.planes[1].data[i]);
        got.push(dec.planes[2].data[i]);
    }
    assert_eq!(got.as_slice(), rgb.as_slice());
}

#[test]
fn roundtrip_2x1_gray_one_decomp_is_bit_exact() {
    let src = VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: 2,
            data: vec![0x42, 0xa7],
        }],
    };
    let opts = EncodeOptions {
        num_decomp: 1,
        ..EncodeOptions::default()
    };
    let bytes =
        encode_frame(&Frame::Video(src.clone()), 2, 1, PixelFormat::Gray8, &opts).expect("encode");
    let dec = decode(&bytes);
    assert_eq!(dec.planes[0].data, src.planes[0].data);
}

#[test]
fn roundtrip_gradient_is_bit_exact() {
    let src = build_gradient(64, 64);
    let bytes = encode_frame(
        &Frame::Video(src.clone()),
        64,
        64,
        PixelFormat::Gray8,
        &EncodeOptions::default(),
    )
    .expect("encode");
    let dec = decode(&bytes);
    // Compare a few sample points first — makes failures easier to
    // read than a giant `assert_eq` on 4096 bytes.
    for y in (0..64usize).step_by(8) {
        for x in (0..64usize).step_by(8) {
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
