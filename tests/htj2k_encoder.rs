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
