//! Behavioural fixtures from `docs/image/jpeg2000/openjph-htj2k-trace-analysis.md`
//! §12, used to pin the HTJ2K decoder against concrete reference
//! codestreams whose decoded pixel values are spelled out in the
//! trace-analysis document.
//!
//! These tests do not consult any external library source; they only
//! consume the spec-PDF Annex C tables (transcribed verbatim in
//! `cxt_vlc_tables.rs`) and the trace-doc behavioural fixtures. The
//! `ojph_compress` binary is used as a black-box codestream
//! constructor — its output bytes are pinned in this crate's
//! `tests/fixtures/` and the source is never consulted.
//!
//! - §12.1: 1×1 reversible smallest-possible (117 bytes). Single
//!   sample with DC level-shift round-trip → decoded pixel = 128.
//! - §12.2: 8×8 reversible single-component (160 bytes). Three
//!   non-empty codeblocks (HL_R1, LH_R1, HH_R1) on a `pixel = 16y + 4x`
//!   ramp; the decoded image must equal the ramp byte-exactly. Round 6
//!   exposed two transcription typos in `CXT_VLC_TABLE_0` against the
//!   spec-PDF Annex C listing: the right column drifted by 2 (the
//!   column read `26, 42, 58, ...` instead of `28, 44, 60, ...`).
//! - §12.3: 7×7 reversible 2-decomp (boundary-parity case). Seven
//!   codeblocks across two resolution levels; the decoded image must
//!   round-trip the same `pixel = 16y + 4x` ramp.
//!
//! The fixtures bundled here use the LRCP progression order rather
//! than the trace-doc inline listing's RPCL — for single-component,
//! single-layer codestreams the two orders produce identical packet
//! content and identical FBCOT codeblock byte streams; only the COD
//! progression-order byte differs. The round-4 HTJ2K tier-2 walker
//! only accepts LRCP today.
//!
//! These tests are gated behind the `htj2k` feature so they only run
//! when the High-Throughput block coder is compiled in.

#![cfg(feature = "htj2k")]

use oxideav_core::{CodecId, Decoder, Frame, Packet, TimeBase};
use oxideav_jpeg2000::{J2kDecoder, CODEC_ID_STR};

/// §12.1 — the 1×1 reversible 117-byte fixture, copied verbatim from
/// the trace doc.
const TRACE_1X1_117B: &[u8] = include_bytes!("fixtures/htj2k_trace_1x1_117b.j2c");

/// §12.2 — the 8×8 reversible 160-byte fixture, copied verbatim from
/// the trace doc §2 hex listing.
const TRACE_8X8_160B: &[u8] = include_bytes!("fixtures/htj2k_trace_8x8_160b.j2c");

/// §12.3 — the 7×7 reversible 2-decomp fixture (boundary parity case).
/// Encoded with `ojph_compress -num_decomps 2 -reversible true` from a
/// 7×7 ramp PGM (`pixel = 16y + 4x`); used here as a black-box
/// codestream — `ojph_compress` is a permitted binary validator.
const TRACE_7X7_2DECOMP: &[u8] = include_bytes!("fixtures/htj2k_trace_7x7_2decomp.j2c");

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

/// §12.1 — single pixel decodes to 128 (the DC level-shift midpoint).
#[test]
fn trace_doc_12_1_1x1_117b_decodes_to_128() {
    let frame = decode_htj2k(TRACE_1X1_117B);
    assert_eq!(frame.planes.len(), 1, "single-component grayscale");
    let plane = &frame.planes[0];
    assert_eq!(plane.data.len(), 1);
    assert_eq!(
        plane.data[0], 128,
        "§12.1: DC-level-shift midpoint round-trips to 128"
    );
}

/// §12.2 — 8×8 ramp `pixel = 16y + 4x` decodes byte-exactly.
///
/// Round 6 fixed two transcription typos in `CXT_VLC_TABLE_0` against
/// the spec-PDF Annex C listing — these were a necessary precondition
/// for byte-exact §12.2 decode. Round 7 fixed three additional bugs
/// in the cleanup pass that affected non-first-line-pair quads:
///   1. Eq (2) of T.814 §7.3.5 was implemented with a typo —
///      `2*(σ^n | σ^nw)` instead of `2*(σ^w | σ^sw)` — so the cq
///      context for non-first-line-pair quads omitted the same-row
///      left-neighbour samples.
///   2. The exponent predictor κ_q (Eq 5 of T.814 §7.3.7) was missing
///      the γ_q multiplier defined in Eq (6).
///   3. The U-VLC quad-pair decoding was sequential per quad
///      (pfx1+sfx1+ext1, then pfx2+sfx2+ext2) rather than interleaved
///      as required by T.814 §7.3.4 / Figure 4 (pfx1, pfx2, sfx1,
///      sfx2, ext1, ext2). This caused suffix bits for q1 to be read
///      from positions intended for q2's prefix.
#[test]
fn trace_doc_12_2_8x8_160b_decodes_ramp_byte_exact() {
    let frame = decode_htj2k(TRACE_8X8_160B);
    assert_eq!(frame.planes.len(), 1);
    let plane = &frame.planes[0];
    assert_eq!(plane.stride, 8);
    assert_eq!(plane.data.len(), 8 * 8);

    let expected: [u8; 64] = std::array::from_fn(|i| {
        let x = i % 8;
        let y = i / 8;
        (16 * y + 4 * x) as u8
    });

    let mismatches: Vec<(usize, usize, u8, u8)> = plane
        .data
        .iter()
        .zip(expected.iter())
        .enumerate()
        .filter_map(|(i, (&a, &b))| {
            if a != b {
                Some((i % 8, i / 8, a, b))
            } else {
                None
            }
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "§12.2 8×8 ramp decode mismatches at {} pixels (first 8: {:?})",
        mismatches.len(),
        &mismatches[..mismatches.len().min(8)]
    );
}

/// §12.3 — 7×7 ramp `pixel = 16y + 4x` decodes byte-exactly with two
/// decomposition levels (boundary parity case).
#[test]
fn trace_doc_12_3_7x7_2decomp_decodes_ramp_byte_exact() {
    let frame = decode_htj2k(TRACE_7X7_2DECOMP);
    assert_eq!(frame.planes.len(), 1);
    let plane = &frame.planes[0];
    assert_eq!(plane.stride, 7);
    assert_eq!(plane.data.len(), 7 * 7);

    let expected: [u8; 49] = std::array::from_fn(|i| {
        let x = i % 7;
        let y = i / 7;
        (16 * y + 4 * x) as u8
    });

    let mismatches: Vec<(usize, usize, u8, u8)> = plane
        .data
        .iter()
        .zip(expected.iter())
        .enumerate()
        .filter_map(|(i, (&a, &b))| {
            if a != b {
                Some((i % 7, i / 7, a, b))
            } else {
                None
            }
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "§12.3 7×7 2-decomp ramp decode mismatches at {} pixels (first 8: {:?})",
        mismatches.len(),
        &mismatches[..mismatches.len().min(8)]
    );
}
