//! Audit: measure the RGB MCT (RCT) bit-exactness gap before / after
//! the §G.1 ordering fix. Decodes the same `.j2k` file with our crate
//! and compares each component plane byte-for-byte against the
//! reference PPM produced by `opj_decompress`. The fixture is a 16x16
//! RGB image deliberately containing fully-saturated R/G/B/Y blocks so
//! the inverse-RCT chroma excursions reach ±255.

use oxideav_core::CodecRegistry;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};

const RGB_J2K: &[u8] = include_bytes!("fixtures/rgb_test.j2k");
const RGB_OPJ_PPM: &[u8] = include_bytes!("fixtures/rgb_test_opj.ppm");
const RGB64_J2K: &[u8] = include_bytes!("fixtures/rgb_test_64x64.j2k");
const RGB64_OPJ_PPM: &[u8] = include_bytes!("fixtures/rgb_test_64x64_opj.ppm");

fn parse_ppm(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    assert_eq!(&bytes[0..2], b"P6", "expected P6 (binary RGB) PPM");
    let mut i = 2usize;
    let mut toks: Vec<String> = Vec::new();
    while toks.len() < 3 {
        while i < bytes.len() && (bytes[i] == b'\n' || bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < bytes.len()
            && bytes[i] != b'\n'
            && bytes[i] != b' '
            && bytes[i] != b'\t'
            && bytes[i] != b'#'
        {
            i += 1;
        }
        toks.push(String::from_utf8_lossy(&bytes[start..i]).into_owned());
    }
    i += 1;
    let w: u32 = toks[0].parse().unwrap();
    let h: u32 = toks[1].parse().unwrap();
    (w, h, bytes[i..].to_vec())
}

fn decode_j2k(bytes: &[u8]) -> oxideav_core::VideoFrame {
    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register_codecs(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.make_decoder(&params).expect("factory");
    let pkt = Packet::new(0, TimeBase::new(1, 1), bytes.to_vec());
    dec.send_packet(&pkt).expect("send_packet");
    match dec.receive_frame().expect("receive_frame") {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    }
}

fn assert_rgb_bit_exact(name: &str, j2k: &[u8], ppm: &[u8], expected_w: u32, expected_h: u32) {
    let (w, h, expected_interleaved) = parse_ppm(ppm);
    assert_eq!((w, h), (expected_w, expected_h), "{name}: dims");
    assert_eq!(
        expected_interleaved.len(),
        (w * h * 3) as usize,
        "{name}: ppm payload size"
    );

    let vf = decode_j2k(j2k);
    assert_eq!(vf.planes.len(), 3, "{name}: RGB → 3 planes");

    let wu = w as usize;
    let hu = h as usize;
    let mut got = vec![0u8; wu * hu * 3];
    for y in 0..hu {
        for x in 0..wu {
            for c in 0..3 {
                let plane = &vf.planes[c];
                got[(y * wu + x) * 3 + c] = plane.data[y * plane.stride + x];
            }
        }
    }

    let total = got.len();
    let mismatches: Vec<(usize, u8, u8)> = expected_interleaved
        .iter()
        .zip(got.iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (&a, &b))| (i, a, b))
        .collect();

    let nm = mismatches.len();
    eprintln!(
        "{name}: {nm}/{total} sample mismatches ({:.2}%)",
        100.0 * nm as f64 / total as f64
    );
    if !mismatches.is_empty() {
        for (i, a, b) in mismatches.iter().take(20) {
            let pix = i / 3;
            let chan = ["R", "G", "B"][i % 3];
            let py = pix / wu;
            let px = pix % wu;
            eprintln!(
                "  [pix ({px:2}, {py:2}) {chan}] expected {a:3}, got {b:3} (diff {})",
                *a as i32 - *b as i32
            );
        }
    }
    assert_eq!(nm, 0, "{name}: not bit-exact: {nm} sample diffs");
}

#[test]
fn opj_rgb_16x16_roundtrip_bit_exact_against_opj_decompress() {
    // 16x16 fixture: extreme R/G/B/Y corners with mid-saturation
    // patterns elsewhere — stresses the inverse-RCT chroma excursions
    // (Y1, Y2 reach ±255 on the saturated edges).
    assert_rgb_bit_exact("rgb_test_16x16", RGB_J2K, RGB_OPJ_PPM, 16, 16);
}

#[test]
fn opj_rgb_64x64_roundtrip_bit_exact_against_opj_decompress() {
    // 64x64 fixture: smooth gradient body with extreme R/G/B/Y corner
    // blocks. Larger image exercises multiple wavelet levels per OPJ
    // default (`-n` defaults to 6; here OPJ-encoded with default 5
    // decomposition levels and the 5/3 reversible filter via MCT=1).
    assert_rgb_bit_exact("rgb_test_64x64", RGB64_J2K, RGB64_OPJ_PPM, 64, 64);
}
