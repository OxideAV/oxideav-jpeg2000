//! Round-1 HTJ2K encoder integration tests (task #456).
//!
//! Exercises [`encode_image_htj2k`]:
//!
//! - **Self round-trip**: encode → decode through the same crate's
//!   FBCOT decoder. Bit-exact for sparse single-significance fixtures.
//! - **OpenJPH cross-decode**: feed our codestream to the `ojph_expand`
//!   binary (workspace policy bars OpenJPH SOURCE; the CLI is
//!   explicitly allowed as a black-box validator). Skipped silently
//!   when the binary is not on PATH.
//!
//! Round-1 scope: single tile, single Gray8 component, NL=0,
//! 32×32 single code-block, lossless 5/3, single quality layer.

#![cfg(feature = "htj2k")]

use oxideav_jpeg2000::encode::{encode_image_htj2k, EncodeOptionsHt};
use oxideav_jpeg2000::image::{Jpeg2000Image, Jpeg2000PixelFormat as PixelFormat, Jpeg2000Plane};
use oxideav_jpeg2000::{decode_jpeg2000, probe, J2kFlavour};

/// Build a 32x32 Gray8 image populated with `data`.
fn img32(data: Vec<u8>) -> Jpeg2000Image {
    Jpeg2000Image {
        width: 32,
        height: 32,
        pixel_format: PixelFormat::Gray8,
        planes: vec![Jpeg2000Plane { stride: 32, data }],
        pts: None,
    }
}

/// Smoke test: a constant 0x80 plane encodes, the codestream is
/// recognised as HTJ2K, and decode round-trips.
#[test]
fn round1_solid_dc_self_roundtrip() {
    let img = img32(vec![0x80u8; 32 * 32]);
    let cs = encode_image_htj2k(&img, &EncodeOptionsHt::default()).expect("encode");
    let p = probe(&cs).expect("probe");
    assert_eq!(p.flavour, J2kFlavour::HighThroughput);
    assert_eq!(p.width, 32);
    assert_eq!(p.height, 32);
    assert_eq!(p.num_components, 1);
    assert_eq!(p.pcap, Some(0x0002_0000));
    let decoded = decode_jpeg2000(&cs).expect("decode");
    assert_eq!(decoded.planes[0].data, img.planes[0].data);
}

/// Sparse content: two ±1 pixels at scattered positions. Each in its
/// own quad. Verifies the CxtVLC + U-VLC + MagSgn paths self-roundtrip.
#[test]
fn round1_sparse_self_roundtrip() {
    let mut data = vec![0x80u8; 32 * 32];
    data[0] = 0x81;
    data[4 * 32 + 4] = 0x7F;
    let img = img32(data.clone());
    let cs = encode_image_htj2k(&img, &EncodeOptionsHt::default()).expect("encode");
    let decoded = decode_jpeg2000(&cs).expect("decode");
    let n_diff = data
        .iter()
        .zip(decoded.planes[0].data.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(n_diff, 0, "{n_diff} pixels differ after self roundtrip");
}

/// Cross-decode through `ojph_expand` (OpenJPH binary) when available.
/// This is a black-box conformance check — `ojph_expand` is treated
/// purely as an opaque validator, no source code is consulted.
#[test]
fn round1_ojph_expand_cross_decode() {
    if std::process::Command::new("ojph_expand")
        .arg("-h")
        .output()
        .is_err()
    {
        eprintln!("ojph_expand not on PATH; skipping");
        return;
    }
    // Solid DC: easiest fixture.
    let img = img32(vec![0x80u8; 32 * 32]);
    let cs = encode_image_htj2k(&img, &EncodeOptionsHt::default()).expect("encode");

    let tmp_dir = std::env::temp_dir();
    let in_path = tmp_dir.join("oxideav_htj2k_round1_solid.j2c");
    let out_path = tmp_dir.join("oxideav_htj2k_round1_solid.pgm");
    std::fs::write(&in_path, &cs).expect("write codestream");
    let _ = std::fs::remove_file(&out_path);

    let status = std::process::Command::new("ojph_expand")
        .args(["-i", in_path.to_str().unwrap()])
        .args(["-o", out_path.to_str().unwrap()])
        .status()
        .expect("spawn ojph_expand");
    assert!(
        status.success(),
        "ojph_expand failed on our HTJ2K codestream"
    );

    // Parse the resulting PGM and compare pixels.
    let pgm = std::fs::read(&out_path).expect("read pgm");
    let payload = strip_pgm_header(&pgm);
    assert_eq!(payload.len(), 32 * 32);
    for &b in payload.iter() {
        assert_eq!(b, 0x80, "ojph_expand cross-decode produced non-DC pixel");
    }

    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
}

/// Same `ojph_expand` cross-decode against the sparse fixture.
#[test]
fn round1_ojph_expand_sparse_cross_decode() {
    if std::process::Command::new("ojph_expand")
        .arg("-h")
        .output()
        .is_err()
    {
        eprintln!("ojph_expand not on PATH; skipping");
        return;
    }
    let mut data = vec![0x80u8; 32 * 32];
    data[0] = 0x81;
    data[4 * 32 + 4] = 0x7F;
    let img = img32(data.clone());
    let cs = encode_image_htj2k(&img, &EncodeOptionsHt::default()).expect("encode");

    let tmp_dir = std::env::temp_dir();
    let in_path = tmp_dir.join("oxideav_htj2k_round1_sparse.j2c");
    let out_path = tmp_dir.join("oxideav_htj2k_round1_sparse.pgm");
    std::fs::write(&in_path, &cs).expect("write codestream");
    let _ = std::fs::remove_file(&out_path);

    let status = std::process::Command::new("ojph_expand")
        .args(["-i", in_path.to_str().unwrap()])
        .args(["-o", out_path.to_str().unwrap()])
        .status()
        .expect("spawn ojph_expand");
    assert!(status.success(), "ojph_expand failed on sparse codestream");

    let pgm = std::fs::read(&out_path).expect("read pgm");
    let payload = strip_pgm_header(&pgm);
    assert_eq!(payload.len(), 32 * 32);
    let n_diff = data
        .iter()
        .zip(payload.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        n_diff, 0,
        "ojph_expand cross-decode disagrees on {n_diff} pixels"
    );

    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
}

/// Strip the P5 PGM header off a buffer and return the raw 8-bit
/// sample payload. Tolerates the 3-line header layout
/// `P5 / W H / maxval / payload`.
fn strip_pgm_header(buf: &[u8]) -> &[u8] {
    let mut i = 0usize;
    let mut newlines = 0;
    while i < buf.len() && newlines < 3 {
        if buf[i] == b'\n' {
            newlines += 1;
        }
        i += 1;
    }
    &buf[i..]
}

// ----- Round 2 fixtures -----

/// 32×32 sparse pattern at NL=1: forward 5/3 DWT on a Gray8 image,
/// self-roundtrip through the same crate's decoder.
#[test]
fn round2_nl1_sparse_self_roundtrip() {
    let mut data = vec![0x80u8; 32 * 32];
    data[0] = 0x81;
    data[5 * 32 + 5] = 0x7F;
    data[10 * 32 + 10] = 0x82;
    let img = img32(data.clone());
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 1,
        use_color_transform: true,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");
    let p = probe(&cs).expect("probe");
    assert_eq!(p.flavour, J2kFlavour::HighThroughput);
    let decoded = decode_jpeg2000(&cs).expect("decode");
    assert_eq!(decoded.planes[0].data, data);
}

/// 64×64 noise + bright square at NL=2: deeper pyramid with
/// multi-significance per quad in HL/LH/HH bands.
#[test]
fn round2_nl2_noise_self_roundtrip() {
    let mut data = vec![0x40u8; 64 * 64];
    for y in 24..40 {
        for x in 24..40 {
            data[y * 64 + x] = 0xC0;
        }
    }
    let img = Jpeg2000Image {
        width: 64,
        height: 64,
        pixel_format: PixelFormat::Gray8,
        planes: vec![Jpeg2000Plane {
            stride: 64,
            data: data.clone(),
        }],
        pts: None,
    };
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 2,
        use_color_transform: true,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");
    let decoded = decode_jpeg2000(&cs).expect("decode");
    assert_eq!(decoded.planes[0].data, data);
}

/// ojph_expand cross-decode for the round-2 NL=1 sparse fixture.
#[test]
fn round2_nl1_ojph_expand_cross_decode() {
    if std::process::Command::new("ojph_expand")
        .arg("-h")
        .output()
        .is_err()
    {
        eprintln!("ojph_expand not on PATH; skipping");
        return;
    }
    let mut data = vec![0x80u8; 32 * 32];
    data[0] = 0x81;
    data[5 * 32 + 5] = 0x7F;
    data[10 * 32 + 10] = 0x82;
    let img = img32(data.clone());
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 1,
        use_color_transform: true,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");

    let tmp_dir = std::env::temp_dir();
    let in_path = tmp_dir.join("oxideav_htj2k_round2_nl1_sparse.j2c");
    let out_path = tmp_dir.join("oxideav_htj2k_round2_nl1_sparse.pgm");
    std::fs::write(&in_path, &cs).expect("write codestream");
    let _ = std::fs::remove_file(&out_path);

    let status = std::process::Command::new("ojph_expand")
        .args(["-i", in_path.to_str().unwrap()])
        .args(["-o", out_path.to_str().unwrap()])
        .status()
        .expect("spawn ojph_expand");
    assert!(status.success(), "ojph_expand failed on NL=1 codestream");

    let pgm = std::fs::read(&out_path).expect("read pgm");
    let payload = strip_pgm_header(&pgm);
    assert_eq!(payload.len(), 32 * 32);
    let n_diff = data
        .iter()
        .zip(payload.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(n_diff, 0, "ojph_expand disagrees on {n_diff} pixels");

    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
}

/// Print encoded byte sizes across NL values for several fixture
/// shapes. Soft assertion: the output is informational only.
#[test]
fn round2_size_report() {
    fn img(w: u32, h: u32, data: Vec<u8>) -> Jpeg2000Image {
        Jpeg2000Image {
            width: w,
            height: h,
            pixel_format: PixelFormat::Gray8,
            planes: vec![Jpeg2000Plane {
                stride: w as usize,
                data,
            }],
            pts: None,
        }
    }
    let solid = img(32, 32, vec![0x80u8; 32 * 32]);
    let cs0 = encode_image_htj2k(
        &solid,
        &EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 0,
            use_color_transform: true,
            ..Default::default()
        },
    )
    .unwrap();
    let cs1 = encode_image_htj2k(
        &solid,
        &EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 1,
            use_color_transform: true,
            ..Default::default()
        },
    )
    .unwrap();
    eprintln!(
        "solid 32x32 (raw 1024): NL=0 {} bytes, NL=1 {} bytes",
        cs0.len(),
        cs1.len()
    );

    let mut d = Vec::with_capacity(64 * 64);
    for y in 0..64u32 {
        for x in 0..64u32 {
            let v = ((x + y) * 4).min(255) as u8;
            d.push(v);
        }
    }
    let grad = img(64, 64, d);
    let cs0 = encode_image_htj2k(
        &grad,
        &EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 0,
            use_color_transform: true,
            ..Default::default()
        },
    )
    .unwrap();
    let cs2 = encode_image_htj2k(
        &grad,
        &EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 2,
            use_color_transform: true,
            ..Default::default()
        },
    )
    .unwrap();
    let cs3 = encode_image_htj2k(
        &grad,
        &EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 3,
            use_color_transform: true,
            ..Default::default()
        },
    )
    .unwrap();
    eprintln!(
        "64x64 gradient (raw 4096): NL=0 {} bytes, NL=2 {} bytes, NL=3 {} bytes",
        cs0.len(),
        cs2.len(),
        cs3.len()
    );

    let mut d = Vec::with_capacity(64 * 64);
    for i in 0..(64 * 64) {
        d.push(((i * 17) % 251) as u8);
    }
    let noise = img(64, 64, d);
    let cs0 = encode_image_htj2k(
        &noise,
        &EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 0,
            use_color_transform: true,
            ..Default::default()
        },
    )
    .unwrap();
    let cs1 = encode_image_htj2k(
        &noise,
        &EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 1,
            use_color_transform: true,
            ..Default::default()
        },
    )
    .unwrap();
    eprintln!(
        "64x64 noise (raw 4096): NL=0 {} bytes, NL=1 {} bytes",
        cs0.len(),
        cs1.len()
    );
}

/// ojph_expand cross-decode for the NL=2 64x64 fixture.
#[test]
fn round2_nl2_ojph_expand_cross_decode() {
    if std::process::Command::new("ojph_expand")
        .arg("-h")
        .output()
        .is_err()
    {
        eprintln!("ojph_expand not on PATH; skipping");
        return;
    }
    let mut data = vec![0x40u8; 64 * 64];
    for y in 24..40 {
        for x in 24..40 {
            data[y * 64 + x] = 0xC0;
        }
    }
    let img = Jpeg2000Image {
        width: 64,
        height: 64,
        pixel_format: PixelFormat::Gray8,
        planes: vec![Jpeg2000Plane {
            stride: 64,
            data: data.clone(),
        }],
        pts: None,
    };
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 2,
        use_color_transform: true,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");

    let tmp_dir = std::env::temp_dir();
    let in_path = tmp_dir.join("oxideav_htj2k_round2_nl2_square.j2c");
    let out_path = tmp_dir.join("oxideav_htj2k_round2_nl2_square.pgm");
    std::fs::write(&in_path, &cs).expect("write codestream");
    let _ = std::fs::remove_file(&out_path);

    let status = std::process::Command::new("ojph_expand")
        .args(["-i", in_path.to_str().unwrap()])
        .args(["-o", out_path.to_str().unwrap()])
        .status()
        .expect("spawn ojph_expand");
    assert!(status.success(), "ojph_expand failed on NL=2 codestream");

    let pgm = std::fs::read(&out_path).expect("read pgm");
    let payload = strip_pgm_header(&pgm);
    assert_eq!(payload.len(), 64 * 64);
    let n_diff = data
        .iter()
        .zip(payload.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(n_diff, 0, "ojph_expand disagrees on {n_diff} pixels");

    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
}

// ----- Round 3 fixtures -----

/// Strip the P6 PPM header off a buffer and return the raw RGB payload
/// (3 bytes per pixel, packed).
fn strip_ppm_header(buf: &[u8]) -> &[u8] {
    let mut i = 0usize;
    let mut newlines = 0;
    while i < buf.len() && newlines < 3 {
        if buf[i] == b'\n' {
            newlines += 1;
        }
        i += 1;
    }
    &buf[i..]
}

/// 32x32 RGB gradient with MCT — self-roundtrip through this crate's
/// own decoder. Verifies forward RCT + multi-component packet emit +
/// inverse RCT recovers the original RGB.
#[test]
fn round3_rgb_mct_self_roundtrip() {
    let mut data = Vec::with_capacity(32 * 32 * 3);
    for y in 0..32u32 {
        for x in 0..32u32 {
            data.push(((x * 8) & 0xFF) as u8);
            data.push(((y * 8) & 0xFF) as u8);
            data.push((((x + y) * 4) & 0xFF) as u8);
        }
    }
    let img = Jpeg2000Image {
        width: 32,
        height: 32,
        pixel_format: PixelFormat::Rgb24,
        planes: vec![Jpeg2000Plane {
            stride: 96,
            data: data.clone(),
        }],
        pts: None,
    };
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 1,
        use_color_transform: true,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");
    let p = probe(&cs).expect("probe");
    assert_eq!(p.flavour, J2kFlavour::HighThroughput);
    assert_eq!(p.num_components, 3);
    let decoded = decode_jpeg2000(&cs).expect("decode");
    for y in 0..32usize {
        for x in 0..32usize {
            let off = y * 96 + 3 * x;
            assert_eq!(decoded.planes[0].data[y * 32 + x], data[off]);
            assert_eq!(decoded.planes[1].data[y * 32 + x], data[off + 1]);
            assert_eq!(decoded.planes[2].data[y * 32 + x], data[off + 2]);
        }
    }
}

/// 32x32 Yuv444P planar — three independent 8-bit planes, no MCT.
#[test]
fn round3_yuv444_planar_self_roundtrip() {
    let mut y = Vec::with_capacity(32 * 32);
    let mut cb = Vec::with_capacity(32 * 32);
    let mut cr = Vec::with_capacity(32 * 32);
    for i in 0..(32 * 32u32) {
        y.push(((i * 17) % 251) as u8);
        cb.push((128u32 + (i % 64)) as u8);
        cr.push((128u32 + ((i * 3) % 32)) as u8);
    }
    let img = Jpeg2000Image {
        width: 32,
        height: 32,
        pixel_format: PixelFormat::Yuv444P,
        planes: vec![
            Jpeg2000Plane {
                stride: 32,
                data: y.clone(),
            },
            Jpeg2000Plane {
                stride: 32,
                data: cb.clone(),
            },
            Jpeg2000Plane {
                stride: 32,
                data: cr.clone(),
            },
        ],
        pts: None,
    };
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 1,
        use_color_transform: true, // ignored for YUV input
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");
    let decoded = decode_jpeg2000(&cs).expect("decode");
    assert_eq!(decoded.pixel_format, PixelFormat::Yuv444P);
    assert_eq!(decoded.planes[0].data, y);
    assert_eq!(decoded.planes[1].data, cb);
    assert_eq!(decoded.planes[2].data, cr);
}

/// 32x32 RGB sparse with MCT, cross-decoded through `ojph_expand`. The
/// resulting PPM must match the input RGB byte-exact.
#[test]
fn round3_rgb_mct_ojph_expand_cross_decode() {
    if std::process::Command::new("ojph_expand")
        .arg("-h")
        .output()
        .is_err()
    {
        eprintln!("ojph_expand not on PATH; skipping");
        return;
    }
    let mut data = Vec::with_capacity(32 * 32 * 3);
    for y in 0..32u32 {
        for x in 0..32u32 {
            data.push(((x * 8) & 0xFF) as u8);
            data.push(((y * 8) & 0xFF) as u8);
            data.push((((x + y) * 4) & 0xFF) as u8);
        }
    }
    let img = Jpeg2000Image {
        width: 32,
        height: 32,
        pixel_format: PixelFormat::Rgb24,
        planes: vec![Jpeg2000Plane {
            stride: 96,
            data: data.clone(),
        }],
        pts: None,
    };
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 1,
        use_color_transform: true,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");

    let tmp_dir = std::env::temp_dir();
    let in_path = tmp_dir.join("oxideav_htj2k_round3_rgb_mct.j2c");
    let out_path = tmp_dir.join("oxideav_htj2k_round3_rgb_mct.ppm");
    std::fs::write(&in_path, &cs).expect("write codestream");
    let _ = std::fs::remove_file(&out_path);

    let status = std::process::Command::new("ojph_expand")
        .args(["-i", in_path.to_str().unwrap()])
        .args(["-o", out_path.to_str().unwrap()])
        .status()
        .expect("spawn ojph_expand");
    assert!(status.success(), "ojph_expand failed on RGB+MCT codestream");

    let ppm = std::fs::read(&out_path).expect("read ppm");
    let payload = strip_ppm_header(&ppm);
    assert_eq!(payload.len(), 32 * 32 * 3);
    let n_diff = data
        .iter()
        .zip(payload.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(n_diff, 0, "ojph_expand disagrees on {n_diff} bytes");

    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
}

/// 64x64 RGB gradient at NL=2 with MCT — bigger spec exercise + ojph_expand
/// cross-decode.
#[test]
fn round3_rgb_mct_nl2_64_ojph_expand_cross_decode() {
    if std::process::Command::new("ojph_expand")
        .arg("-h")
        .output()
        .is_err()
    {
        eprintln!("ojph_expand not on PATH; skipping");
        return;
    }
    let mut data = Vec::with_capacity(64 * 64 * 3);
    for y in 0..64u32 {
        for x in 0..64u32 {
            data.push(((x * 4) & 0xFF) as u8);
            data.push(((y * 4) & 0xFF) as u8);
            data.push((((x + y) * 2) & 0xFF) as u8);
        }
    }
    let img = Jpeg2000Image {
        width: 64,
        height: 64,
        pixel_format: PixelFormat::Rgb24,
        planes: vec![Jpeg2000Plane {
            stride: 192,
            data: data.clone(),
        }],
        pts: None,
    };
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 2,
        use_color_transform: true,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");

    let tmp_dir = std::env::temp_dir();
    let in_path = tmp_dir.join("oxideav_htj2k_round3_rgb_mct_64.j2c");
    let out_path = tmp_dir.join("oxideav_htj2k_round3_rgb_mct_64.ppm");
    std::fs::write(&in_path, &cs).expect("write codestream");
    let _ = std::fs::remove_file(&out_path);

    let status = std::process::Command::new("ojph_expand")
        .args(["-i", in_path.to_str().unwrap()])
        .args(["-o", out_path.to_str().unwrap()])
        .status()
        .expect("spawn ojph_expand");
    assert!(
        status.success(),
        "ojph_expand failed on 64x64 RGB+MCT NL=2 codestream"
    );

    let ppm = std::fs::read(&out_path).expect("read ppm");
    let payload = strip_ppm_header(&ppm);
    assert_eq!(payload.len(), 64 * 64 * 3);
    let n_diff = data
        .iter()
        .zip(payload.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(n_diff, 0, "ojph_expand disagrees on {n_diff} bytes");

    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);
}

/// Encoder size report for round 3 — RGB gradients with vs without MCT
/// at the same NL. Informational only; printed to stderr.
#[test]
fn round3_rgb_size_report() {
    let mut data = Vec::with_capacity(64 * 64 * 3);
    for y in 0..64u32 {
        for x in 0..64u32 {
            data.push(((x * 4) & 0xFF) as u8);
            data.push(((y * 4) & 0xFF) as u8);
            data.push((((x + y) * 2) & 0xFF) as u8);
        }
    }
    let img = Jpeg2000Image {
        width: 64,
        height: 64,
        pixel_format: PixelFormat::Rgb24,
        planes: vec![Jpeg2000Plane {
            stride: 192,
            data: data.clone(),
        }],
        pts: None,
    };
    let cs_mct = encode_image_htj2k(
        &img,
        &EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 3,
            use_color_transform: true,
            ..Default::default()
        },
    )
    .unwrap();
    let cs_no_mct = encode_image_htj2k(
        &img,
        &EncodeOptionsHt {
            cblk_log2: 5,
            num_decomp: 3,
            use_color_transform: false,
            ..Default::default()
        },
    )
    .unwrap();
    eprintln!(
        "64x64 RGB gradient (raw {} bytes): NL=3 MCT {} bytes, NL=3 no-MCT {} bytes",
        data.len(),
        cs_mct.len(),
        cs_no_mct.len()
    );
}

// ----- Round 4 fixtures -----

use oxideav_jpeg2000::encode::HtTransform;

fn ojph_present() -> bool {
    std::process::Command::new("ojph_expand")
        .arg("-h")
        .output()
        .is_ok()
}

/// Round-4: 9/7 irreversible 32×32 solid DC — `ojph_expand` cross-decodes
/// within ±2 LSB of the original (the DC coefficient lives in LL with a
/// stepsize of 1, so the only loss is float→int rounding). This is the
/// cleanest single-tile round-4 fixture for cross-decode validation.
#[test]
fn round4_9_7_solid_dc_32x32_ojph_cross_decode() {
    if !ojph_present() {
        eprintln!("ojph_expand not on PATH; skipping");
        return;
    }
    let img = img32(vec![0x80u8; 32 * 32]);
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 1,
        transform: HtTransform::Irreversible97,
        use_color_transform: false,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");
    let tmp = std::env::temp_dir();
    let in_p = tmp.join("oxideav_round4_lossy97.j2c");
    let out_p = tmp.join("oxideav_round4_lossy97.pgm");
    std::fs::write(&in_p, &cs).expect("write");
    let _ = std::fs::remove_file(&out_p);
    let st = std::process::Command::new("ojph_expand")
        .args(["-i", in_p.to_str().unwrap()])
        .args(["-o", out_p.to_str().unwrap()])
        .status()
        .expect("ojph spawn");
    assert!(st.success(), "ojph_expand failed on 9/7 codestream");
    let pgm = std::fs::read(&out_p).expect("read");
    let payload = strip_pgm_header(&pgm);
    assert_eq!(payload.len(), 32 * 32);
    let mut max_dev = 0i32;
    for &b in payload {
        max_dev = max_dev.max((b as i32 - 0x80).abs());
    }
    assert!(
        max_dev <= 2,
        "ojph_expand 9/7 cross-decode drift {max_dev} LSB > 2",
    );
    let _ = std::fs::remove_file(&in_p);
    let _ = std::fs::remove_file(&out_p);
}

// ----- Round 6 fixtures: SigProp + MagRef encoder passes -----

use oxideav_jpeg2000::encode::HtPassCount;

/// Round-6: 32×32 sparse Gray8 fixture with `Z_blk = 2` (cleanup +
/// SigProp). Self round-trip through our own decoder must be bit-exact:
/// the cleanup pass already communicates the full sample magnitude in
/// FBCOT round 1, so the additional SigProp pass cannot alter `mag[n]`.
#[test]
fn round6_zblk_2_sparse_self_roundtrip() {
    let mut data = vec![0x80u8; 32 * 32];
    data[0] = 0x90;
    data[5 * 32 + 5] = 0x70;
    data[10 * 32 + 10] = 0xA0;
    let img = img32(data.clone());
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 1,
        pass_count: HtPassCount::CleanupSigprop,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");
    let p = probe(&cs).expect("probe");
    assert_eq!(p.flavour, J2kFlavour::HighThroughput);
    let decoded = decode_jpeg2000(&cs).expect("decode");
    let n_diff = data
        .iter()
        .zip(decoded.planes[0].data.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(n_diff, 0, "{n_diff} pixels differ after Z_blk=2 roundtrip");
}

/// Round-6: same fixture with `Z_blk = 3` (cleanup + SigProp + MagRef).
#[test]
fn round6_zblk_3_sparse_self_roundtrip() {
    let mut data = vec![0x80u8; 32 * 32];
    data[0] = 0x90;
    data[5 * 32 + 5] = 0x70;
    data[10 * 32 + 10] = 0xA0;
    let img = img32(data.clone());
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 1,
        pass_count: HtPassCount::CleanupSigpropMagref,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");
    let decoded = decode_jpeg2000(&cs).expect("decode");
    let n_diff = data
        .iter()
        .zip(decoded.planes[0].data.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(n_diff, 0, "{n_diff} pixels differ after Z_blk=3 roundtrip");
}

/// Round-6: cross-decode our `Z_blk = 2` codestream through
/// `ojph_expand` and check the decoded pixels match the input. Skipped
/// silently when the binary is not on PATH (workspace policy bars
/// OpenJPH source — only the binary is in scope as a black-box
/// validator).
#[test]
fn round6_zblk_2_sparse_ojph_cross_decode() {
    if !ojph_present() {
        eprintln!("ojph_expand not on PATH; skipping");
        return;
    }
    let mut data = vec![0x80u8; 32 * 32];
    data[0] = 0x90;
    data[5 * 32 + 5] = 0x70;
    data[10 * 32 + 10] = 0xA0;
    let img = img32(data.clone());
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 1,
        pass_count: HtPassCount::CleanupSigprop,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");
    let tmp = std::env::temp_dir();
    let in_p = tmp.join("oxideav_round6_zblk2.j2c");
    let out_p = tmp.join("oxideav_round6_zblk2.pgm");
    std::fs::write(&in_p, &cs).expect("write");
    let _ = std::fs::remove_file(&out_p);
    let st = std::process::Command::new("ojph_expand")
        .args(["-i", in_p.to_str().unwrap()])
        .args(["-o", out_p.to_str().unwrap()])
        .status()
        .expect("ojph spawn");
    assert!(st.success(), "ojph_expand failed on Z_blk=2 codestream");
    let pgm = std::fs::read(&out_p).expect("read");
    let payload = strip_pgm_header(&pgm);
    assert_eq!(payload.len(), 32 * 32);
    let n_diff = data
        .iter()
        .zip(payload.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        n_diff, 0,
        "ojph_expand cross-decode disagrees on {n_diff} pixels (Z_blk=2)"
    );
    let _ = std::fs::remove_file(&in_p);
    let _ = std::fs::remove_file(&out_p);
}

/// Round-6: cross-decode our `Z_blk = 3` codestream through
/// `ojph_expand` and check the decoded pixels match the input.
#[test]
fn round6_zblk_3_sparse_ojph_cross_decode() {
    if !ojph_present() {
        eprintln!("ojph_expand not on PATH; skipping");
        return;
    }
    let mut data = vec![0x80u8; 32 * 32];
    data[0] = 0x90;
    data[5 * 32 + 5] = 0x70;
    data[10 * 32 + 10] = 0xA0;
    let img = img32(data.clone());
    let opts = EncodeOptionsHt {
        cblk_log2: 5,
        num_decomp: 1,
        pass_count: HtPassCount::CleanupSigpropMagref,
        ..Default::default()
    };
    let cs = encode_image_htj2k(&img, &opts).expect("encode");
    let tmp = std::env::temp_dir();
    let in_p = tmp.join("oxideav_round6_zblk3.j2c");
    let out_p = tmp.join("oxideav_round6_zblk3.pgm");
    std::fs::write(&in_p, &cs).expect("write");
    let _ = std::fs::remove_file(&out_p);
    let st = std::process::Command::new("ojph_expand")
        .args(["-i", in_p.to_str().unwrap()])
        .args(["-o", out_p.to_str().unwrap()])
        .status()
        .expect("ojph spawn");
    assert!(st.success(), "ojph_expand failed on Z_blk=3 codestream");
    let pgm = std::fs::read(&out_p).expect("read");
    let payload = strip_pgm_header(&pgm);
    assert_eq!(payload.len(), 32 * 32);
    let n_diff = data
        .iter()
        .zip(payload.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        n_diff, 0,
        "ojph_expand cross-decode disagrees on {n_diff} pixels (Z_blk=3)"
    );
    let _ = std::fs::remove_file(&in_p);
    let _ = std::fs::remove_file(&out_p);
}
