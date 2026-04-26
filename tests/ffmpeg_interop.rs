//! Round-6 diagnostic. Encode the `opj16.pgm` reference with our
//! encoder and have `ffmpeg` (spec-conformant decoder) decode it back.
//! If our encoder is spec-compliant, the ffmpeg-decoded PGM matches
//! the source byte-for-byte.

use std::process::Command;

use oxideav_core::{Frame, PixelFormat, TimeBase, VideoFrame, VideoPlane};
use oxideav_jpeg2000::encode::{encode_frame, EncodeOptions};

fn parse_pgm(bytes: &[u8]) -> Vec<u8> {
    let mut i = 0;
    let mut nl = 0;
    while i < bytes.len() && nl < 3 {
        if bytes[i] == b'\n' {
            nl += 1;
        }
        i += 1;
    }
    bytes[i..].to_vec()
}

#[test]
fn encode_opj16_decodes_with_ffmpeg() {
    // Skip gracefully if ffmpeg isn't on PATH (sandboxed CI).
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        eprintln!("ffmpeg not available on PATH — skipping");
        return;
    }
    let pgm = include_bytes!("fixtures/opj16.pgm");
    let pixels = parse_pgm(pgm);
    assert_eq!(pixels.len(), 16 * 16);
    let vf = VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: 16,
            data: pixels.clone(),
        }],
    };
    let frame = Frame::Video(vf);
    let opts = EncodeOptions::default();
    let j2k = encode_frame(&frame, 16, 16, PixelFormat::Gray8, &opts).unwrap();
    let tmp_j2k = "/tmp/our_opj16.j2k";
    let tmp_dec = "/tmp/our_opj16_dec.pgm";
    std::fs::write(tmp_j2k, &j2k).unwrap();
    eprintln!("wrote {} bytes to {}", j2k.len(), tmp_j2k);

    let out = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            tmp_j2k,
            "-pix_fmt",
            "gray",
            "-update",
            "1",
            "-frames:v",
            "1",
            tmp_dec,
        ])
        .output()
        .unwrap();
    if !out.status.success() {
        eprintln!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
        panic!("ffmpeg decode failed");
    }

    let dec_pgm = std::fs::read(tmp_dec).unwrap();
    let dec_pixels = parse_pgm(&dec_pgm);
    assert_eq!(dec_pixels.len(), pixels.len(), "ffmpeg output size");
    let mut diffs = 0;
    let mut first_diff: Option<(usize, u8, u8)> = None;
    for (i, (a, b)) in pixels.iter().zip(dec_pixels.iter()).enumerate() {
        if a != b {
            diffs += 1;
            if first_diff.is_none() {
                first_diff = Some((i, *a, *b));
            }
        }
    }
    eprintln!(
        "ffmpeg decode of our encoded opj16: {} mismatches out of {}",
        diffs,
        pixels.len()
    );
    if let Some((i, a, b)) = first_diff {
        eprintln!(
            "first diff at ({}, {}): expected {a}, got {b}",
            i % 16,
            i / 16
        );
    }
    assert_eq!(diffs, 0, "ffmpeg decode of our output mismatches source");
}
