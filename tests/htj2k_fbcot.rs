//! FBCOT block-decoder fixture tests.
//!
//! These tests build a complete HT cleanup segment by hand (no
//! third-party encoder is involved per the task brief — the spec PDF
//! at docs/image/jpeg2000/ISO_IEC_15444-15-HTJ2K-2019.pdf is the only
//! source) and feed it through the public [`decode_codeblock`]
//! entry point of the FBCOT decoder. They cover:
//!
//! * `Z_blk = 0` — skipped block per §7.1.1.
//! * `Z_blk = 3` placeholder passes — Cleanup with no significant
//!   samples, plus zero-length SigProp/MagRef segments.
//! * A 32×32 single-component block whose cleanup pass is a long
//!   MEL run (every quad falls into the AZC short-circuit).

#![cfg(feature = "htj2k")]

use oxideav_jpeg2000::decode::htj2k::{decode_codeblock, ZBlk};

#[test]
fn fixture_zblk_zero_returns_empty_block() {
    // §7.1.1: "If Z_blk equals 0, no HT segments are available for
    // the code-block, and so all sample output values for the block
    // shall be 0."
    let out = decode_codeblock(32, 32, ZBlk::Zero, &[], &[]).unwrap();
    assert_eq!(out.width, 32);
    assert_eq!(out.height, 32);
    assert_eq!(out.mag.len(), 32 * 32);
    assert!(out.mag.iter().all(|&m| m == 0));
    assert!(out.sign.iter().all(|&s| s == 0));
    assert!(out.refinement.iter().all(|&r| r == 0));
}

/// Build a hand-rolled 32x32 HT cleanup segment whose MEL stream
/// is a long run of `1` bits (the spec's AZC encoding when every
/// quad has all zero magnitudes). With every quad short-circuited,
/// no MagSgn nor VLC bits are consumed, all sample magnitudes
/// reconstruct to zero.
///
/// 32×32 samples → 16×16 = 256 quads. Each `1` bit at MEL state k
/// emits `1 << MEL_E[k]` zero-symbols (Table 2 of §7.3.3, page 13
/// of the FDIS). Cumulative emit-0 count vs leading `1` count:
///
/// | leading 1s | cum emits |
/// |-----------:|----------:|
/// |  3         |  3        |
/// |  6         |  9        |
/// |  9         |  21       |
/// | 11         |  37       |
/// | 12         |  53       |
/// | 13         |  85 (k saturates at 12 → 32 each)
/// | 19         |  277      |
///
/// So 19 consecutive `1` bits suffice. With bit-stuffing (a `0x7F`
/// after a `0xFF` only contributes 7 bits because the spec mandates
/// MSB=0 after a `0xFF`), we pack them into:
///
/// ```text
///  byte 0: 0xFF  -- 8 leading `1`s
///  byte 1: 0x7F  -- stuff bit MSB=0, then 7 `1`s
///  byte 2: 0xFF  -- 8 more `1`s   (cumulative emit count: 277)
///  byte 3: 0x75  -- modDcup-overlaid low nibble forced to F (=0x7F);
///                   raw byte's low nibble (5) encodes Scup low half
///  byte 4: 0x00  -- Scup high half (Scup = 16*0 + 5 = 5)
/// ```
///
/// `Lcup = 5`, `Scup = 5`, `Pcup = 0`. MagSgn substream is empty;
/// MEL substream is the first 5 bytes (modDcup-overlaid for the
/// last two); VLC substream runs backward from byte 2 — never read
/// because every quad is AZC. The constraint
/// `D[i] D[i+1] (big-endian u16) <= 0xFF8F` from §7.1.1 holds for
/// every consecutive pair: `0xFF7F`, `0x7FFF`, `0xFF75`, `0x7500`.
fn build_azc32x32_cleanup() -> Vec<u8> {
    vec![0xFFu8, 0x7F, 0xFF, 0x75, 0x00]
}

#[test]
fn fixture_32x32_azc_cleanup_only_decodes_to_zero_magnitudes() {
    let dcup = build_azc32x32_cleanup();
    let out = decode_codeblock(32, 32, ZBlk::One, &dcup, &[]).unwrap();
    assert_eq!(out.width, 32);
    assert_eq!(out.height, 32);
    assert_eq!(out.mag.len(), 32 * 32);
    for (i, &m) in out.mag.iter().enumerate() {
        assert_eq!(m, 0, "sample {i} expected magnitude 0, got {m}");
    }
    for (i, &s) in out.sign.iter().enumerate() {
        assert_eq!(s, 0, "sample {i} expected sign 0, got {s}");
    }
}

#[test]
fn fixture_32x32_azc_three_passes_with_placeholder_refinement() {
    // Same cleanup segment, but request Z_blk = 3 (Cleanup + SigProp
    // + MagRef) with a *placeholder* refinement segment (zero
    // bytes). Per Annex B.3, a placeholder refinement segment is
    // legal and produces no state change in the SigProp/MagRef
    // passes. Verifies the dual-bitstream split correctly handles
    // empty Dref.
    let dcup = build_azc32x32_cleanup();
    let out = decode_codeblock(32, 32, ZBlk::Three, &dcup, &[]).unwrap();
    assert_eq!(out.mag.len(), 32 * 32);
    assert!(out.mag.iter().all(|&m| m == 0));
    // SigProp must not have set any z[n] bits, since no neighbour
    // is ever significant.
    assert!(out.z.iter().all(|&z| z == 0));
    // Likewise MagRef must not have set any refinement bits, since
    // every cleanup-significance σ_n is 0.
    assert!(out.refinement.iter().all(|&r| r == 0));
}

#[test]
fn fixture_zero_length_dcup_with_nonzero_zblk_rejected() {
    let err = decode_codeblock(32, 32, ZBlk::One, &[], &[]).unwrap_err();
    assert!(format!("{err}").contains("cleanup segment cannot be empty"));
}

#[test]
fn fixture_pads_oddwidth_codeblock_to_zero() {
    // 5x5 block → padded to 6x6 internally (3x3 quads). The padding
    // samples must yield magnitude 0 with no MagSgn bits consumed.
    // Reuse the AZC encoding: fewer quads → at most 9 emit-0s
    // needed. MEL_k=0..2 give 1 emit each (3 total); MEL_k=3..5
    // give 2 each (6 more, total 9). So 6 leading `1` bits suffice:
    // byte 0 = 0xFC = 0b11111100 (top 6 bits = 1).
    let dcup = vec![0xFCu8, 0x83, 0x00];
    // Lcup=3, Scup = 16*0 + (0x83 & 0xF) = 3, Pcup = 0.
    let out = decode_codeblock(5, 5, ZBlk::One, &dcup, &[]).unwrap();
    assert_eq!(out.mag.len(), 6 * 6); // 3x3 quads, 4 samples each
    assert!(out.mag.iter().all(|&m| m == 0));
}
