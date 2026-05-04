//! Multi-band 9/7 HTJ2K fixture covering HF-band decode on the float
//! dequantisation path.
//!
//! # Why this fixture exists
//!
//! Round 8 (commit `7477f9c`) wired `pblk = M_b − S_blk − 1` (T.800
//! Eq E-1) into both the 5/3 integer reconstruction and the 9/7 float
//! reconstruction in `decode_subband_htj2k_97`. The previous round-4
//! interop fixture (`htj2k_lossy97_32x32_nl1_lrcp.j2c`) only had the
//! LL band carry data — the inclusion tag tree marked all HF code-
//! blocks `included = false` for that codestream. The 9/7 float path
//! therefore had **no** integration-level coverage of HF-band decodes
//! at all, even at `pblk = 0`.
//!
//! This fixture is a 64×64 8-bit grayscale gradient compressed with
//! `ojph_compress` (binary, NOT source-consulted) at:
//!
//! - `-reversible false` (irreversible 9/7 transform per T.800 §H.1)
//! - `-qstep 0.005` (fine quantisation so HF bands carry meaningful
//!   energy and the encoder can't drop them entirely)
//! - `-num_decomps 5` (5 decomposition levels → 16 sub-bands total)
//! - `-prog_order LRCP`, default precincts, 32×32 code-blocks
//! - input is a smooth diagonal ramp xor'd with a 5-bit checker so
//!   the high-frequency sub-bands receive non-zero energy
//!
//! All 16 sub-bands (LL_5 + 5×{HL,LH,HH}) are included by tier-2
//! inclusion. The `pblk` value the encoder picks for each codeblock
//! is `M_b − missing_msb`; OpenJPH-encoded HT codeblocks consistently
//! emit `missing_msb = M_b` (single cleanup pass per codeblock,
//! highest-significance plane first), so the integration path here
//! exercises `pblk = 0` across every sub-band. The pblk > 0 / pblk
//! < 0 / z = 1 algebraic cases are unit-tested directly against the
//! `mb_grid_value_97` helper inside `tier2.rs` — see the `tests`
//! module there for the closed-form Eq E-1 sweep.
//!
//! # Cross-checks
//!
//! - **Decode parity** against `ojph_expand`'s output (`ojph_expand` is
//!   used as a black-box validator only — bytes-out only; no source).
//!   The 9/7 inverse is float-arithmetic-bound, so the assertion is a
//!   per-pixel mean-absolute-deviation bound rather than bit-exact
//!   equality.
//! - **HF-band coverage** — the test asserts that the codestream is
//!   recognised as HTJ2K via probe and that the decoded image is the
//!   right shape. The MAD bound (≤ 8 LSB on an 8-bit gradient) is
//!   tight enough to catch silent zero-band fall-throughs (which would
//!   blur the gradient to ~16 LSB MAD), but generous enough to
//!   tolerate the documented 9/7 float drift at qstep 0.005.
//!   At authoring time the observed MAD is ~0.47 with a max
//!   per-pixel deviation of 2.

#![cfg(feature = "htj2k")]

use oxideav_core::{CodecId, Decoder, Frame, Packet, TimeBase};
use oxideav_jpeg2000::{J2kDecoder, CODEC_ID_STR};

const HT_LOSSY97_MULTI_J2C: &[u8] = include_bytes!("fixtures/htj2k_lossy97_64x64_nl5_lrcp.j2c");
const REFERENCE_LOSSY97_MULTI_PGM: &[u8] =
    include_bytes!("fixtures/htj2k_lossy97_64x64_opj_ref.pgm");

fn pgm_pixels(blob: &[u8], expected_count: usize) -> &[u8] {
    let start = blob
        .len()
        .checked_sub(expected_count)
        .expect("PGM smaller than pixels");
    &blob[start..]
}

fn decode_htj2k(buf: &[u8]) -> oxideav_core::VideoFrame {
    let mut dec = J2kDecoder::new(CodecId::new(CODEC_ID_STR));
    let pkt = Packet::new(0u32, TimeBase::new(1, 1), buf.to_vec());
    dec.send_packet(&pkt)
        .expect("HTJ2K codestream must decode end-to-end");
    let frame = dec.receive_frame().expect("frame must be pending");
    match frame {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    }
}

#[test]
fn htj2k_lossy97_multi_band_fixture_is_recognised_as_high_throughput() {
    use oxideav_jpeg2000::{probe, J2kFlavour};
    let p = probe(HT_LOSSY97_MULTI_J2C).expect("probe");
    assert_eq!(p.flavour, J2kFlavour::HighThroughput);
    assert_eq!(p.width, 64);
    assert_eq!(p.height, 64);
    assert_eq!(p.num_components, 1);
}

#[test]
fn htj2k_lossy97_multi_band_decodes_close_to_opj_reference() {
    let frame = decode_htj2k(HT_LOSSY97_MULTI_J2C);
    assert_eq!(frame.planes.len(), 1, "single-component grayscale");
    let plane = &frame.planes[0];
    assert_eq!(plane.stride, 64);
    assert_eq!(plane.data.len(), 64 * 64);
    let reference = pgm_pixels(REFERENCE_LOSSY97_MULTI_PGM, 64 * 64);

    // Per-pixel mean-absolute deviation between our 9/7 multi-band
    // decode and the OpenJPH-binary reference. With every HF band
    // contributing across 5 decomposition levels, the dominant error
    // sources are float-rounding drift in the 9/7 lifting (cumulative
    // across 5 levels) and the qstep 0.005 quantisation floor that
    // both decoders share. An MAD ≤ 8 LSB is the same bound the
    // round-4 nl=1 fixture uses; we keep it here so a regression that
    // silently drops HF bands (which would re-introduce the
    // pre-round-8 blur) trips the assertion.
    let mad: f64 = plane
        .data
        .iter()
        .zip(reference.iter())
        .map(|(&a, &b)| (a as i32 - b as i32).unsigned_abs() as f64)
        .sum::<f64>()
        / (plane.data.len() as f64);
    assert!(
        mad < 8.0,
        "HTJ2K 9/7 multi-band MAD vs ojph_expand = {mad:.3} > 8.0",
    );

    // Tighter sanity check: the maximum per-pixel deviation should not
    // be catastrophically larger than the mean. A blown HF band would
    // typically push max well past 64 LSB while keeping the mean
    // moderate; this catches that pattern.
    let max_dev: u32 = plane
        .data
        .iter()
        .zip(reference.iter())
        .map(|(&a, &b)| (a as i32 - b as i32).unsigned_abs())
        .max()
        .unwrap_or(0);
    assert!(
        max_dev <= 64,
        "HTJ2K 9/7 multi-band max abs deviation = {max_dev} > 64",
    );
}
