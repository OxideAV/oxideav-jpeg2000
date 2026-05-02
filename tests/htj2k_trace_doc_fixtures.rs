//! Byte-fixture tests built from the clean-room behavioural-trace
//! report at `docs/image/jpeg2000/openjph-htj2k-trace-analysis.md`.
//!
//! Each fixture is a complete HTJ2K codestream (not a JP2 wrapper)
//! recorded by the report author against an instrumented OpenJPH
//! `ojph_compress` build. The bytes in this file come exclusively from
//! that report (which is itself source-quote-free per its preamble);
//! no OpenJPH source code is consulted here.
//!
//! - **§12.1** — 1×1 reversible (117 bytes). Single-pixel input value
//!   128, encoded with NL=0 (no DWT) and an empty packet. Decodes to a
//!   solid 0x80 plane.
//! - **§12.2** — 8×8 reversible (160 bytes, full marker tour). The
//!   block-coder fixtures inside it (HL_R1, LH_R1, HH_R1) are the
//!   smallest non-trivial HT block traces. Decode must produce a
//!   sensible 8×8 grayscale plane that round-trips bit-exact through
//!   OpenJPH (input was the trace report's `ramp8.pgm`).
//! - **§12.3** — 7×7 reversible 2-decomp (boundary parity case). The
//!   strictest test of T.800 §F.3.8.1 whole-sample symmetric extension.

#![cfg(feature = "htj2k")]

use oxideav_core::{CodecId, Decoder, Frame, Packet, TimeBase};
use oxideav_jpeg2000::{J2kDecoder, CODEC_ID_STR};

/// §12.1 — 117-byte 1×1 reversible HTJ2K codestream.
///
/// Per the trace report: single packet body `00` (no codeblock
/// inclusion). Decoded coefficient = 0; DC level shift adds +128;
/// final pixel = 128.
const TRACE_DOC_1X1_117B: &[u8] = &[
    0xff, 0x4f, 0xff, 0x51, 0x00, 0x29, 0x40, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x01, 0xff, 0x50, 0x00,
    0x08, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0xff, 0x52, 0x00, 0x0c, 0x00, 0x02, 0x00, 0x01, 0x00,
    0x00, 0x03, 0x03, 0x40, 0x01, 0xff, 0x5c, 0x00, 0x04, 0x20, 0x38, 0xff, 0x64, 0x00, 0x17, 0x00,
    0x01, 0x4f, 0x70, 0x65, 0x6e, 0x4a, 0x50, 0x48, 0x20, 0x56, 0x65, 0x72, 0x20, 0x30, 0x2e, 0x32,
    0x37, 0x2e, 0x30, 0x2e, 0xff, 0x90, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x01,
    0xff, 0x93, 0x00, 0xff, 0xd9,
];

/// §12.3 — 7×7 reversible 2-decomp HTJ2K codestream (149 bytes). The
/// **boundary-parity** test from the trace doc — odd dimensions force
/// every level to produce odd-length subbands, which exposes the
/// whole-sample-symmetric extension rule on every IDWT pass. Generated
/// by `ojph_compress` on the synthetic ramp `pixel(x, y) = 16y + 4x`
/// (range 0..120) with `-num_decomps 2 -block_size {32,32}`.
///
/// Round-7 pin: this fixture must round-trip byte-exact through our
/// HTJ2K decoder once it is taught about multi-precinct / multi-level
/// `(2-decomp)` HF subbands. Currently our walker accepts the layout
/// (2 decomp levels, 1 codeblock per band) and the cleanup formula is
/// shared with §12.2, so the assertion is expected to pass.
const TRACE_DOC_7X7_149B: &[u8] = &[
    0xff, 0x4f, 0xff, 0x51, 0x00, 0x29, 0x40, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x07,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x07,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x01, 0xff, 0x50, 0x00,
    0x08, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0xff, 0x52, 0x00, 0x0c, 0x00, 0x02, 0x00, 0x01, 0x00,
    0x02, 0x03, 0x03, 0x40, 0x01, 0xff, 0x5c, 0x00, 0x0a, 0x20, 0x48, 0x50, 0x50, 0x50, 0x48, 0x48,
    0x48, 0xff, 0x64, 0x00, 0x17, 0x00, 0x01, 0x4f, 0x70, 0x65, 0x6e, 0x4a, 0x50, 0x48, 0x20, 0x56,
    0x65, 0x72, 0x20, 0x30, 0x2e, 0x32, 0x35, 0x2e, 0x33, 0x2e, 0xff, 0x90, 0x00, 0x0a, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x29, 0x00, 0x01, 0xff, 0x93, 0xc0, 0x2a, 0x00, 0xff, 0x77, 0xed, 0xf2, 0x00,
    0x80, 0xb4, 0x00, 0xc0, 0x12, 0xc0, 0x12, 0x80, 0xf6, 0x00, 0x42, 0x74, 0x00, 0xbe, 0x00, 0x03,
    0x54, 0x00, 0x00, 0xff, 0xd9,
];

/// §12.2 — 160-byte 8×8 reversible HTJ2K codestream (the report's
/// "canonical HTJ2K skeleton"). Carries three non-empty 4×4 codeblocks
/// (HL_R1, LH_R1, HH_R1) plus an LL_R0 packet.
const TRACE_DOC_8X8_160B: &[u8] = &[
    0xff, 0x4f, 0xff, 0x51, 0x00, 0x29, 0x40, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x01, 0xff, 0x50, 0x00,
    0x08, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0xff, 0x52, 0x00, 0x0c, 0x00, 0x02, 0x00, 0x01, 0x00,
    0x01, 0x03, 0x03, 0x40, 0x01, 0xff, 0x5c, 0x00, 0x07, 0x20, 0x48, 0x48, 0x48, 0x48, 0xff, 0x64,
    0x00, 0x17, 0x00, 0x01, 0x4f, 0x70, 0x65, 0x6e, 0x4a, 0x50, 0x48, 0x20, 0x56, 0x65, 0x72, 0x20,
    0x30, 0x2e, 0x32, 0x37, 0x2e, 0x30, 0x2e, 0xff, 0x90, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x37, 0x00, 0x01, 0xff, 0x93, 0xc0, 0x2d, 0x60, 0xff, 0x5f, 0xef, 0xaf, 0xef, 0xa7, 0xd9, 0xf8,
    0xdf, 0xde, 0xa7, 0xef, 0xa5, 0x59, 0xf8, 0x00, 0x1a, 0x00, 0x00, 0x31, 0xb7, 0x00, 0xc0, 0x25,
    0x80, 0x4e, 0xaa, 0xa9, 0xe2, 0x74, 0x00, 0xde, 0xdd, 0xc1, 0xc9, 0xec, 0xb5, 0x00, 0xff, 0xd9,
];

fn decode_htj2k(buf: &[u8]) -> oxideav_core::VideoFrame {
    let mut dec = J2kDecoder::new(CodecId::new(CODEC_ID_STR));
    let pkt = Packet::new(0u32, TimeBase::new(1, 1), buf.to_vec());
    dec.send_packet(&pkt)
        .expect("HTJ2K trace-doc fixture must decode end-to-end");
    let frame = dec.receive_frame().expect("frame must be pending");
    match frame {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    }
}

#[test]
fn trace_doc_section_12_1_1x1_decodes_to_dc_pixel() {
    let frame = decode_htj2k(TRACE_DOC_1X1_117B);
    assert_eq!(frame.planes.len(), 1, "single-component grayscale");
    let plane = &frame.planes[0];
    assert_eq!(plane.stride, 1);
    assert_eq!(plane.data.len(), 1);
    assert_eq!(
        plane.data[0], 0x80,
        "1×1 trace-doc fixture must decode to DC-shifted zero (0x80); got {:#x}",
        plane.data[0]
    );
}

/// §12.2 8×8 fixture probes as a HighThroughput codestream.
#[test]
fn trace_doc_section_12_2_8x8_codestream_probes_as_htj2k() {
    use oxideav_jpeg2000::{probe, J2kFlavour};
    let p = probe(TRACE_DOC_8X8_160B).expect("probe");
    assert_eq!(p.flavour, J2kFlavour::HighThroughput);
    assert_eq!(p.width, 8);
    assert_eq!(p.height, 8);
    assert_eq!(p.num_components, 1);
}

/// §12.2 8×8 fixture must decode **byte-exact** to OpenJPH's reference
/// expansion of the same codestream — the input was `ramp8.pgm` with
/// the synthetic ramp `pixel(x, y) = 16y + 4x`, confirmed by encoding
/// the same pattern with `ojph_compress` and verifying byte-for-byte
/// codestream equality. The expected 8×8 grid is therefore the
/// `16y + 4x` integer ramp, range 0..140.
///
/// Round-6.5 fix that closed this assertion: the spec entry
/// `{0, 0xC, 0x1, 0xC, 0xC, 0x17, 7}` of CxtVLC_table_0 was originally
/// transcribed as `(0, 0xC, 0x1, 0xC, 0x0, 0x17, 7)` (the `ε^1_q`
/// nibble dropped from `C` to `0`). That left `ibit = 0` for samples
/// j=2,3 of the right-column HL_R1 quad, so the cleanup decoded
/// magnitude 2 instead of magnitude 4 — the IDWT then produced
/// pixel column 7 = `[26, 42, 58, 74, 90, …]` rather than the correct
/// `[28, 44, 60, 76, 92, …]`. Both the decoded magnitude AND the IDWT
/// output line up after the table is fixed.
#[test]
fn trace_doc_section_12_2_8x8_decodes_byte_exact_to_ramp() {
    let frame = decode_htj2k(TRACE_DOC_8X8_160B);
    assert_eq!(frame.planes.len(), 1);
    let plane = &frame.planes[0];
    assert_eq!(plane.stride, 8, "8x8 single-component stride");
    assert_eq!(plane.data.len(), 64, "8x8 single-component pixel count");
    let mut expected = [0u8; 64];
    for y in 0..8usize {
        for x in 0..8usize {
            expected[y * 8 + x] = (16 * y + 4 * x) as u8;
        }
    }
    if plane.data.as_slice() != expected.as_slice() {
        eprintln!("§12.2 8x8 decoded plane (FAILED — diff vs expected):");
        for y in 0..8 {
            let got: Vec<i32> = (0..8).map(|x| plane.data[y * 8 + x] as i32).collect();
            let exp: Vec<i32> = (0..8).map(|x| expected[y * 8 + x] as i32).collect();
            eprintln!("  got: {got:?}");
            eprintln!("  exp: {exp:?}");
        }
    }
    assert_eq!(
        plane.data.as_slice(),
        expected.as_slice(),
        "§12.2 8x8 trace-doc fixture must decode byte-exact to the 16y+4x ramp"
    );
}

/// §12.2 per-block trace of HL_R1 (the smallest HT block). Decodes
/// the 5-byte cleanup segment and dumps `(mag, sign)` for each of the
/// 16 sample positions. Used as a single-block oracle when the full
/// 8×8 fixture diverges from the OpenJPH ramp pattern.
#[test]
fn trace_doc_section_12_2_hl_r1_block_unpack() {
    use oxideav_jpeg2000::decode::htj2k::{decode_codeblock, ZBlk};
    let dcup: Vec<u8> = vec![0xaa, 0xa9, 0xe2, 0x74, 0x00];
    let out = decode_codeblock(4, 4, ZBlk::One, &dcup, &[]).unwrap();
    eprintln!("§12.2 HL_R1 4x4 sample decode:");
    for q in 0..4 {
        eprint!("  quad {q}:");
        for j in 0..4 {
            let n = 4 * q + j;
            eprint!(
                " (n={n} m={} s={})",
                out.mag[n], out.sign[n]
            );
        }
        eprintln!();
    }
}

/// §12.2 LH_R1 trace.
#[test]
fn trace_doc_section_12_2_lh_r1_block_unpack() {
    use oxideav_jpeg2000::decode::htj2k::{decode_codeblock, ZBlk};
    let dcup: Vec<u8> = vec![
        0xff, 0x5f, 0xef, 0xaf, 0xef, 0xa7, 0xd9, 0xf8, 0xdf, 0xde, 0xa7, 0xef, 0xa5, 0x59, 0xf8,
        0x00, 0x1a, 0x00, 0x00, 0x31, 0xb7, 0x00,
    ];
    let out = decode_codeblock(4, 4, ZBlk::One, &dcup, &[]).unwrap();
    eprintln!("§12.2 LH_R1 4x4 sample decode:");
    for q in 0..4 {
        eprint!("  quad {q}:");
        for j in 0..4 {
            let n = 4 * q + j;
            eprint!(
                " (n={n} m={} s={})",
                out.mag[n], out.sign[n]
            );
        }
        eprintln!();
    }
}

/// §12.2 HH_R1 trace.
#[test]
fn trace_doc_section_12_2_hh_r1_block_unpack() {
    use oxideav_jpeg2000::decode::htj2k::{decode_codeblock, ZBlk};
    let dcup: Vec<u8> = vec![0xde, 0xdd, 0xc1, 0xc9, 0xec, 0xb5, 0x00];
    let out = decode_codeblock(4, 4, ZBlk::One, &dcup, &[]).unwrap();
    eprintln!("§12.2 HH_R1 4x4 sample decode:");
    for q in 0..4 {
        eprint!("  quad {q}:");
        for j in 0..4 {
            let n = 4 * q + j;
            eprint!(
                " (n={n} m={} s={})",
                out.mag[n], out.sign[n]
            );
        }
        eprintln!();
    }
}

/// §12.3 — 7×7 reversible 2-decomp fixture probes as HighThroughput.
#[test]
fn trace_doc_section_12_3_7x7_codestream_probes_as_htj2k() {
    use oxideav_jpeg2000::{probe, J2kFlavour};
    let p = probe(TRACE_DOC_7X7_149B).expect("probe");
    assert_eq!(p.flavour, J2kFlavour::HighThroughput);
    assert_eq!(p.width, 7);
    assert_eq!(p.height, 7);
    assert_eq!(p.num_components, 1);
}

/// §12.3 — 7×7 reversible 2-decomp fixture must decode byte-exact to
/// the `pixel(x, y) = 16y + 4x` integer ramp (range 0..120). This is
/// the strictest test of T.800 §F.3.8.1 whole-sample symmetric
/// extension because each level's subbands are odd-length.
#[test]
fn trace_doc_section_12_3_7x7_decodes_byte_exact_to_ramp() {
    let frame = decode_htj2k(TRACE_DOC_7X7_149B);
    assert_eq!(frame.planes.len(), 1);
    let plane = &frame.planes[0];
    assert_eq!(plane.stride, 7, "7x7 single-component stride");
    assert_eq!(plane.data.len(), 49, "7x7 single-component pixel count");
    let mut expected = [0u8; 49];
    for y in 0..7usize {
        for x in 0..7usize {
            expected[y * 7 + x] = (16 * y + 4 * x) as u8;
        }
    }
    if plane.data.as_slice() != expected.as_slice() {
        eprintln!("§12.3 7x7 decoded plane (FAILED — diff vs expected):");
        for y in 0..7 {
            let got: Vec<i32> = (0..7).map(|x| plane.data[y * 7 + x] as i32).collect();
            let exp: Vec<i32> = (0..7).map(|x| expected[y * 7 + x] as i32).collect();
            eprintln!("  got: {got:?}");
            eprintln!("  exp: {exp:?}");
        }
    }
    assert_eq!(
        plane.data.as_slice(),
        expected.as_slice(),
        "§12.3 7x7 trace-doc fixture must decode byte-exact to 16y+4x"
    );
}

/// §12.1 fixture probes match the trace report's canonical HTJ2K
/// skeleton declaration.
#[test]
fn trace_doc_section_12_1_codestream_probes_as_htj2k() {
    use oxideav_jpeg2000::{probe, J2kFlavour};
    let p = probe(TRACE_DOC_1X1_117B).expect("probe");
    assert_eq!(p.flavour, J2kFlavour::HighThroughput);
    assert_eq!(p.width, 1);
    assert_eq!(p.height, 1);
    assert_eq!(p.num_components, 1);
}
