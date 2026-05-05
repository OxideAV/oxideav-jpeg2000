//! Multi-layer decode interop (T.800 §B.10).
//!
//! JPEG 2000 supports progressive quality refinement via "layers". Each
//! layer adds further coding-pass contributions (per code-block) on top
//! of the previous layers. The codestream still has one compressed
//! body — the packets are interleaved in progression order, and the
//! decoder accumulates pass counts and byte data per code-block across
//! layers. The tier-1 decoder is run once at the end with the full
//! accumulated stream.
//!
//! Per Table D.8 default ("termination only on last pass"), no MQ
//! termination occurs at intermediate layer boundaries — so a single
//! concatenated MQ stream works for the lossless multi-layer case.
//! Each layer's contribution is signalled as ONE codeword segment in
//! the packet header (B.10.7.1).
//!
//! Fixtures (all 32x32 testsrc, default code-block geometry, generated
//! with `opj_compress`; the `.pgm` reference is `opj_decompress` of the
//! same codestream — i.e. the bit-exact "all layers" result):
//!
//! - `opj32_3lyr.j2k`       — 4-res LRCP, 5/3 lossless, 3 layers.
//! - `opj32_5lyr.j2k`       — 4-res LRCP, 5/3 lossless, 5 layers.
//! - `opj32_3lyr_rlcp.j2k`  — 4-res RLCP, 5/3 lossless, 3 layers
//!   (resolution-major outside of layer).
//! - `opj32_3lyr_lossy.j2k` — 4-res LRCP, 9/7 irreversible, 3 layers.

use oxideav_core::CodecRegistry;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};

const ML3_J2K: &[u8] = include_bytes!("fixtures/opj32_3lyr.j2k");
const ML3_PGM: &[u8] = include_bytes!("fixtures/opj32_3lyr.pgm");
const ML5_J2K: &[u8] = include_bytes!("fixtures/opj32_5lyr.j2k");
const ML5_PGM: &[u8] = include_bytes!("fixtures/opj32_5lyr.pgm");
const ML_RLCP_J2K: &[u8] = include_bytes!("fixtures/opj32_3lyr_rlcp.j2k");
const ML_RLCP_PGM: &[u8] = include_bytes!("fixtures/opj32_3lyr_rlcp.pgm");
const ML_LOSSY_J2K: &[u8] = include_bytes!("fixtures/opj32_3lyr_lossy.j2k");
const ML_LOSSY_PGM: &[u8] = include_bytes!("fixtures/opj32_3lyr_lossy.pgm");

fn parse_pgm(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    assert_eq!(&bytes[0..2], b"P5");
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

fn psnr(src: &[u8], dec: &[u8]) -> f64 {
    assert_eq!(src.len(), dec.len());
    let mut se: f64 = 0.0;
    for (a, b) in src.iter().zip(dec.iter()) {
        let d = *a as f64 - *b as f64;
        se += d * d;
    }
    let mse = se / src.len() as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0 * 255.0 / mse).log10()
}

fn assert_bit_exact(name: &str, j2k: &[u8], pgm: &[u8]) {
    let (w, h, expected) = parse_pgm(pgm);
    assert_eq!((w, h), (32, 32), "{name}: unexpected fixture dims");
    let vf = decode_j2k(j2k);
    let got = &vf.planes[0].data;
    let p = psnr(&expected, got);
    let nmismatch = expected
        .iter()
        .zip(got.iter())
        .filter(|(a, b)| a != b)
        .count();
    eprintln!(
        "{name} PSNR = {p:.2} dB, {nmismatch}/{} mismatches",
        expected.len()
    );
    assert_eq!(
        nmismatch, 0,
        "{name}: not bit-exact ({nmismatch} mismatches, PSNR={p:.2} dB)"
    );
}

/// 3-layer lossless LRCP — exercises the basic layer-accumulation logic
/// in `parse_precinct_packet`. The default arithmetic-coder termination
/// pattern (Table D.8 "termination only on last pass") does not break
/// the MQ stream at layer boundaries, so the tier-1 decoder still
/// receives a single concatenated codeword segment per code-block.
#[test]
fn opj_multilayer_3layer_lossless_lrcp_decodes_bit_exactly() {
    assert_bit_exact("opj32_3lyr", ML3_J2K, ML3_PGM);
}

/// 5-layer lossless LRCP — pushes the "second-and-later layer"
/// inclusion path (single-bit inclusion, no missing-MSB tag-tree
/// re-decode) on the bulk of code-blocks.
#[test]
fn opj_multilayer_5layer_lossless_lrcp_decodes_bit_exactly() {
    assert_bit_exact("opj32_5lyr", ML5_J2K, ML5_PGM);
}

/// 3-layer lossless RLCP — same accumulation logic as LRCP but with
/// `for resno { for layer { for comp ... } }` packet ordering. Verifies
/// the layer index is plumbed correctly through the alternative
/// progression-order branch.
#[test]
fn opj_multilayer_3layer_lossless_rlcp_decodes_bit_exactly() {
    assert_bit_exact("opj32_3lyr_rlcp", ML_RLCP_J2K, ML_RLCP_PGM);
}

/// 3-layer 9/7 irreversible LRCP — verifies the lossy float synthesis
/// path also handles multi-layer accumulation. PSNR vs. OpenJPEG's
/// own decode of the same codestream must be infinite (we both run
/// the same dequantised samples through the same IDWT).
#[test]
fn opj_multilayer_3layer_lossy_lrcp_decodes_bit_exactly() {
    assert_bit_exact("opj32_3lyr_lossy", ML_LOSSY_J2K, ML_LOSSY_PGM);
}
