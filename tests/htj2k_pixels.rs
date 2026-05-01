//! End-to-end pixel-level decode of a hand-built HTJ2K codestream.
//!
//! Round 3 of the ISO/IEC 15444-15 effort wires the FBCOT entropy
//! decoder (round 2) into the existing Part-1 tier-2 packet walker.
//! These tests build a complete `.j2k` codestream by hand — no
//! third-party encoder is involved (no OpenJPH, no Kakadu) — and run
//! it through the public `J2kDecoder::send_packet` /
//! `receive_frame` API.
//!
//! # Fixture A — 8x8 grayscale, all-zero AZC code-block
//!
//! Smallest meaningful end-to-end test:
//!
//! - SIZ: 8x8 grayscale, 8-bit unsigned, no sub-sampling.
//! - CAP: Pcap15 set, Ccap15 = 0x0000 (HTONLY, single HT set, no RGN,
//!   homogeneous, reversible, magnitude bound = 8).
//! - COD: LRCP, 1 quality layer, MCT=0, num_decomp=0 (identity DWT),
//!   cblk_w_log2=cblk_h_log2=3 (one 8x8 codeblock covers the band),
//!   cblk_style with bit 6 set (HT codeblocks per Annex A.4 / Table A.3),
//!   reversible 5/3 transform.
//! - QCD: reversible quantisation, 0 guard bits, single LL band.
//! - SOT/SOD: one tile-part covering the single tile.
//! - Body: one packet with one included codeblock carrying the
//!   hand-built 3-byte HT cleanup segment that decodes to 16 zero
//!   quads (= 32 zero samples, the whole 8x8 block).
//!
//! After decode, every sample in the wavelet domain is 0; the
//! DC-level shift adds `2^(8-1) = 128`; the final 8-bit plane is
//! filled with 128 across all 64 pixels.

#![cfg(feature = "htj2k")]

use oxideav_core::{CodecId, Decoder, Frame, Packet, TimeBase};
use oxideav_jpeg2000::{J2kDecoder, CODEC_ID_STR};

/// Build the SIZ marker payload for a single-component, 8-bit unsigned
/// grayscale image of the given square dimension.
fn build_siz(dim: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0xFF, 0x51]); // SIZ marker
    v.extend_from_slice(&41u16.to_be_bytes()); // Lsiz: fixed 41 for 1-comp
                                               // Rsiz: per ISO/IEC 15444-15 §A.2, bit 14 must be 1 for HTJ2K.
    v.extend_from_slice(&0x4000u16.to_be_bytes());
    v.extend_from_slice(&dim.to_be_bytes()); // Xsiz
    v.extend_from_slice(&dim.to_be_bytes()); // Ysiz
    v.extend_from_slice(&0u32.to_be_bytes()); // XOsiz
    v.extend_from_slice(&0u32.to_be_bytes()); // YOsiz
    v.extend_from_slice(&dim.to_be_bytes()); // XTsiz (1 tile = full image)
    v.extend_from_slice(&dim.to_be_bytes()); // YTsiz
    v.extend_from_slice(&0u32.to_be_bytes()); // XTOsiz
    v.extend_from_slice(&0u32.to_be_bytes()); // YTOsiz
    v.extend_from_slice(&1u16.to_be_bytes()); // Csiz = 1 component
    v.extend_from_slice(&[7, 1, 1]); // Ssiz = 7 (8-bit unsigned), XRsiz=YRsiz=1
    v
}

/// CAP marker segment for the simplest HTJ2K profile: Pcap with only
/// bit 15 set, single Ccap15 = 0 (HTONLY, single HT set, no RGN,
/// homogeneous, reversible, MAGB = 8).
fn build_cap() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0xFF, 0x50]);
    v.extend_from_slice(&8u16.to_be_bytes()); // Lcap = 6 + 2*1
    v.extend_from_slice(&0x0002_0000u32.to_be_bytes()); // Pcap with Pcap15 set
    v.extend_from_slice(&0x0000u16.to_be_bytes()); // Ccap15
    v
}

/// COD marker for an HT codestream: LRCP, 1 layer, no MCT, NL=0
/// (identity transform), 8x8 code-blocks (cblk_log2=3),
/// SPcod=0x40 (bit 6 set per Table A.3 — all blocks HT, bit 7=0),
/// reversible 5/3 transform (SGcod transform byte = 1).
fn build_cod_ht_8x8_nl0() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0xFF, 0x52]);
    v.extend_from_slice(&12u16.to_be_bytes()); // Lcod = 12
                                               // SGcod: Scod=0 (no SOP/EPH, default precincts), prog=0 (LRCP),
                                               //        layers=1 (BE 16-bit), MCT=0 — total 5 bytes.
    v.extend_from_slice(&[0u8, 0, 0x00, 0x01, 0]);
    // SPcod fields (5 bytes):
    //   num_decomp = 0
    //   cblk_w  = log2 - 2 = 1 → cblk = 8 wide
    //   cblk_h  = log2 - 2 = 1 → cblk = 8 tall
    //   cblk_style = 0x40 (bit 6 = "HT codeblocks", bit 7 = 0 per Table A.3)
    //   transform = 1 (5/3 reversible)
    v.extend_from_slice(&[0, 1, 1, 0x40, 1]);
    v
}

/// QCD marker for reversible quantisation with 0 guard bits and a
/// single LL band (matching NL=0 in COD).
fn build_qcd_reversible_nl0() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0xFF, 0x5C]);
    v.extend_from_slice(&4u16.to_be_bytes()); // Lqcd = 4: Sqcd + 1 band entry
                                              // Sqcd: qntsty=0 (reversible), guard_bits = 0 (top 3 bits = 0).
    v.push(0);
    // Single band exponent (LL): Annex E reversible quantisation byte
    // is `eps << 3` in the lower 5 bits. Use eps = 8 (matches the
    // band's M_b for 8-bit input).
    v.push(8u8 << 3);
    v
}

/// Build the HT cleanup segment `Dcup` for an 8x8 all-zero codeblock.
///
/// Layout (Lcup = 3, Scup = 3, Pcup = 0):
///
/// - Byte 0 (`0xFF`): MEL byte. 8 leading 1-bits cause the MEL
///   decoder to emit 17 cumulative zero symbols (k climbs 0..7,
///   emit-counts 1+1+1+2+2+2+4+4 = 17). Only 16 are needed for the
///   16 quads of an 8x8 (= 4x4 quads) block.
/// - Byte 1 (`0x03`): per the spec NOTE 4 of §7.1.2 the byte after
///   `0xFF` must have its MSB = 0 (it does — top nibble 0x0). The
///   low nibble encodes the Scup low half: `Scup & 0x0F` = 3.
/// - Byte 2 (`0x00`): Scup high half = 0. Recovered Scup =
///   `16*0 + (0x03 & 0x0F) = 3` → Pcup = Lcup - Scup = 0.
///
/// Constraint check: §7.1.1 forbids consecutive bytes whose
/// big-endian 16-bit value exceeds `0xFF8F`. Pairs in this fixture:
/// `0xFF03` (≤ 0xFF8F ✓), `0x0300` (✓). Trailing byte ≠ 0xFF ✓.
fn build_dcup_8x8_azc() -> Vec<u8> {
    vec![0xFFu8, 0x03, 0x00]
}

/// Splice the per-tile-part body together: one packet whose header
/// declares the single 8x8 code-block as included with one coding pass
/// (Z_blk = 1), `missing_msb = 0`, and a length field equal to
/// `Lcup = 3`. Append the 3-byte Dcup.
///
/// Packet header bit-stream (MSB-first, per the §B.10 schema):
///
/// 1. Packet-non-empty flag    : `1`
/// 2. Inclusion tag-tree       : single leaf, threshold 1 → emits
///    `0` (root-level lo == 0 < 1) then `1` (terminator) — a single
///    quasi-bit "1" because the 1x1 tag-tree degenerates: the root
///    leaf's "value < threshold" check fires on the first `0`-bit,
///    then the terminator `1` is written. The encoder's
///    `OneLeafTree::emit` produces "10" — for value=0, threshold=1 it
///    writes the terminator `1` directly. (We mimic that here.)
/// 3. Zero-bitplanes tag-tree  : value 0, encoded as a single `1`
///    terminator at threshold 1.
/// 4. Num-passes               : 1 → bit `0`.
/// 5. Lblock growth            : 0 increments → bit `0`. (Initial
///    Lblock = 3.)
/// 6. Length field             : `Lblock + ilog2(num_passes) = 3 + 0`
///    = 3 bits → write 3 = `011`.
/// 7. Byte align (`inalign`).
///
/// Concatenated bits: `1` `1` `1` `0` `0` `011` = 8 bits = one byte
/// `0b1110_0011` = 0xE3. After alignment, this single byte forms the
/// packet header. Then we append the 3-byte cleanup segment.
fn build_tile_body_8x8_azc() -> Vec<u8> {
    let mut v = Vec::new();
    v.push(0xE3); // packet header: see comments above
    v.extend_from_slice(&build_dcup_8x8_azc());
    v
}

/// Assemble the full 8x8 AZC HTJ2K codestream.
fn build_htj2k_8x8_azc() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0xFF, 0x4F]); // SOC
    v.extend_from_slice(&build_siz(8));
    v.extend_from_slice(&build_cap());
    v.extend_from_slice(&build_cod_ht_8x8_nl0());
    v.extend_from_slice(&build_qcd_reversible_nl0());
    // SOT
    let sot_off = v.len();
    v.extend_from_slice(&[0xFF, 0x90]);
    v.extend_from_slice(&10u16.to_be_bytes()); // Lsot = 10
    v.extend_from_slice(&0u16.to_be_bytes()); // Isot
    let psot_pos = v.len();
    v.extend_from_slice(&0u32.to_be_bytes()); // Psot — patched
    v.extend_from_slice(&[0, 1]); // TPsot=0, TNsot=1
    v.extend_from_slice(&[0xFF, 0x93]); // SOD
    v.extend_from_slice(&build_tile_body_8x8_azc());
    let tile_part_end = v.len();
    let psot = (tile_part_end - sot_off) as u32;
    v[psot_pos..psot_pos + 4].copy_from_slice(&psot.to_be_bytes());
    v.extend_from_slice(&[0xFF, 0xD9]); // EOC
    v
}

#[test]
fn decodes_hand_built_8x8_azc_htj2k_codestream_to_solid_dc_shift() {
    let buf = build_htj2k_8x8_azc();
    let mut dec = J2kDecoder::new(CodecId::new(CODEC_ID_STR));
    let pkt = Packet::new(0u32, TimeBase::new(1, 1), buf);
    dec.send_packet(&pkt)
        .expect("HTJ2K 8x8 AZC fixture must decode end-to-end");
    let frame = dec.receive_frame().expect("frame must be pending");

    // The 8x8 single-component output, after FBCOT decode (all-zero
    // wavelet samples) + DC level shift (+128 for 8-bit unsigned),
    // is a solid 0x80 plane.
    let video = match frame {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    };
    assert_eq!(video.planes.len(), 1, "single-component frame");
    let plane = &video.planes[0];
    assert_eq!(plane.stride, 8);
    assert_eq!(plane.data.len(), 64);
    for (i, &b) in plane.data.iter().enumerate() {
        assert_eq!(
            b, 0x80,
            "pixel {i} expected 0x80 (DC-shifted zero), got {b:#x}"
        );
    }
}

/// Verify the codestream parses as HTJ2K (CAP→Pcap15) so the dispatch
/// in `J2kDecoder::send_packet` actually routes through the FBCOT
/// driver and not the classic-EBCOT one.
#[test]
fn fixture_is_recognised_as_htj2k_via_probe() {
    use oxideav_jpeg2000::{probe, J2kFlavour};
    let buf = build_htj2k_8x8_azc();
    let p = probe(&buf).expect("probe");
    assert_eq!(p.flavour, J2kFlavour::HighThroughput);
    assert_eq!(p.width, 8);
    assert_eq!(p.height, 8);
    assert_eq!(p.num_components, 1);
    assert_eq!(p.pcap, Some(0x0002_0000));
    assert_eq!(p.ccap15, Some(0x0000));
}
