//! Region of Interest (RGN) Maxshift decode tests (T.800 §A.6.3 + §H.1).
//!
//! Fixtures were produced by OpenJPEG's `opj_compress -ROI c=<i>,U=<s>`
//! which inserts an `RGN` marker segment (`Srgn=0`, `SPrgn=s`) in the
//! main header that upshifts the quantisation indices of component `i`
//! by `s` bit-planes. The encoder then codes more bit-planes so that
//! ROI coefficients sit above the background bit-plane budget. The
//! Maxshift method is identifier-free: on decode any reconstructed
//! magnitude above `2^Mb` (the background bound) is treated as an
//! ROI coefficient and divided by `2^s`.
//!
//! These fixtures all use full lossless 5/3 transformations, so a
//! correct decoder should round-trip them bit-exactly against the
//! original PGM / PPM input.

use oxideav_core::CodecRegistry;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};
use oxideav_jpeg2000::codestream;

const CONST_PGM: &[u8] = include_bytes!("fixtures/const32.pgm");
const ROI_CONST_U4_J2K: &[u8] = include_bytes!("fixtures/roi_const32_u4.j2k");
const ROI_CONST_U8_J2K: &[u8] = include_bytes!("fixtures/roi_const32_u8.j2k");

const GRAD_INPUT_PGM: &[u8] = include_bytes!("fixtures/roi_gradient32_input.pgm");
const ROI_GRAD_U4_J2K: &[u8] = include_bytes!("fixtures/roi_gradient32_u4.j2k");
const ROI_GRAD_U8_J2K: &[u8] = include_bytes!("fixtures/roi_gradient32_u8.j2k");

const RGB_INPUT_PPM: &[u8] = include_bytes!("fixtures/roi_rgb32_input.ppm");
const ROI_RGB_C0_U4_J2K: &[u8] = include_bytes!("fixtures/roi_rgb32_c0_u4.j2k");

// 9/7 irreversible RGN fixture — the encoder runs the float DWT before
// upshifting quantisation indices for component 0. Decode is lossy by
// design (≤ a few LSB) since the 9/7 transform itself is not bit-exact
// reversible; we just check that the decoder doesn't panic and the
// reconstructed pixels match the opj_decompress reference within 4 LSB.
const ROI_GRAD_97_U4_J2K: &[u8] = include_bytes!("fixtures/roi_gradient32_97_u4.j2k");

/// Parse a binary PGM (P5) or PPM (P6) image. Tolerates `#` comments.
fn parse_pnm(bytes: &[u8]) -> (u32, u32, u32, Vec<u8>) {
    let magic = &bytes[0..2];
    assert!(magic == b"P5" || magic == b"P6");
    let comps = if magic == b"P5" { 1 } else { 3 };
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
    (w, h, comps, bytes[i..].to_vec())
}

fn decode_j2k(bytes: &[u8]) -> oxideav_core::VideoFrame {
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
fn parser_records_main_header_rgn_with_correct_shift() {
    let cs = codestream::parse(ROI_GRAD_U4_J2K).expect("parse");
    assert_eq!(cs.rgn.len(), 1, "expected one main-header RGN");
    let r = cs.rgn[0];
    assert_eq!(r.crgn, 0);
    assert_eq!(r.srgn, 0, "Maxshift");
    assert_eq!(r.sprgn, 4, "U=4 shift");
}

#[test]
fn parser_handles_larger_shift_value() {
    let cs = codestream::parse(ROI_GRAD_U8_J2K).expect("parse");
    assert_eq!(cs.rgn.len(), 1);
    assert_eq!(cs.rgn[0].sprgn, 8);
}

#[test]
fn const_with_rgn_u4_round_trips_bit_exactly() {
    let (w, h, _comps, expected) = parse_pnm(CONST_PGM);
    assert_eq!((w, h), (32, 32));
    let vf = decode_j2k(ROI_CONST_U4_J2K);
    assert_eq!(vf.planes[0].data, expected, "RGN U=4 const round-trip");
}

#[test]
fn const_with_rgn_u8_round_trips_bit_exactly() {
    let (w, h, _comps, expected) = parse_pnm(CONST_PGM);
    assert_eq!((w, h), (32, 32));
    let vf = decode_j2k(ROI_CONST_U8_J2K);
    assert_eq!(vf.planes[0].data, expected, "RGN U=8 const round-trip");
}

#[test]
fn gradient_with_rgn_u4_round_trips_bit_exactly() {
    let (w, h, _comps, expected) = parse_pnm(GRAD_INPUT_PGM);
    assert_eq!((w, h), (32, 32));
    let vf = decode_j2k(ROI_GRAD_U4_J2K);
    assert_eq!(
        vf.planes[0].data, expected,
        "RGN U=4 gradient round-trip (opj_compress lossless 5/3)"
    );
}

#[test]
fn gradient_with_rgn_u8_round_trips_bit_exactly() {
    let (w, h, _comps, expected) = parse_pnm(GRAD_INPUT_PGM);
    assert_eq!((w, h), (32, 32));
    let vf = decode_j2k(ROI_GRAD_U8_J2K);
    assert_eq!(vf.planes[0].data, expected, "RGN U=8 gradient round-trip");
}

#[test]
fn gradient_97_with_rgn_u4_decodes_within_tolerance() {
    // 9/7 irreversible: lossy by definition. Check we get a sensible
    // gradient back (max-abs-deviation ≤ 4 LSB at 8-bit).
    let (w, h, _comps, expected) = parse_pnm(GRAD_INPUT_PGM);
    assert_eq!((w, h), (32, 32));
    let vf = decode_j2k(ROI_GRAD_97_U4_J2K);
    let got = &vf.planes[0].data;
    assert_eq!(got.len(), expected.len());
    let mut mad: u32 = 0;
    let mut max_dev: i32 = 0;
    for i in 0..expected.len() {
        let d = (got[i] as i32 - expected[i] as i32).abs();
        mad += d as u32;
        if d > max_dev {
            max_dev = d;
        }
    }
    let mean = mad as f64 / expected.len() as f64;
    assert!(
        max_dev <= 4,
        "9/7 RGN U=4 max deviation {max_dev} > 4 LSB (mean {mean:.2})"
    );
}

#[test]
fn rgb_with_rgn_on_luma_round_trips_bit_exactly() {
    let (w, h, _comps, expected) = parse_pnm(RGB_INPUT_PPM);
    assert_eq!((w, h), (32, 32));
    let vf = decode_j2k(ROI_RGB_C0_U4_J2K);
    // opj_compress with -ROI c=0,U=4 on RGB triggers RCT (MCT=1) and
    // applies the upshift to the Y0 (luma) component. Decoder must
    // honour the per-component shift and still recover lossless RGB.
    let interleaved: Vec<u8> = (0..(w as usize * h as usize))
        .flat_map(|i| {
            [
                vf.planes[0].data[i],
                vf.planes[1].data[i],
                vf.planes[2].data[i],
            ]
        })
        .collect();
    assert_eq!(
        interleaved, expected,
        "RGN U=4 on luma + RCT lossless round-trip"
    );
}
