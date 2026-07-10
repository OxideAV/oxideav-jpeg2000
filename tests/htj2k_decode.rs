//! End-to-end HTJ2K (ITU-T T.814 | ISO/IEC 15444-15) decode tests.
//!
//! Each fixture is a `.j2c` HT codestream produced by the black-box
//! validator `ojph_compress` and a reference reconstruction produced by
//! `ojph_expand`; the test decodes the codestream with this crate and
//! asserts the recovered samples match the reference bit-for-bit (for
//! reversible streams) or exactly (these lossy fixtures reconstruct
//! identically here). The validators are opaque processes — their source
//! is never consulted.

/// Locate the start of the binary raster in a Netpbm file (byte after
/// the third newline) and return `(width, height, data_start)`.
fn pnm_geometry(b: &[u8]) -> (usize, usize, usize) {
    let mut nl = 0;
    let mut i = 0;
    while nl < 3 {
        if b[i] == b'\n' {
            nl += 1;
        }
        i += 1;
    }
    let header = std::str::from_utf8(&b[3..i]).unwrap();
    let mut it = header.split_whitespace();
    let w: usize = it.next().unwrap().parse().unwrap();
    let h: usize = it.next().unwrap().parse().unwrap();
    (w, h, i)
}

/// Parse a binary `P5` (grayscale) PGM into `(w, h, samples)`.
fn parse_pgm(b: &[u8]) -> (usize, usize, Vec<i32>) {
    let (w, h, start) = pnm_geometry(b);
    let data = b[start..start + w * h].iter().map(|&x| x as i32).collect();
    (w, h, data)
}

/// Parse a binary `P6` (RGB) PPM into `(w, h, interleaved_samples)`.
fn parse_ppm(b: &[u8]) -> (usize, usize, Vec<i32>) {
    let (w, h, start) = pnm_geometry(b);
    let data = b[start..start + w * h * 3]
        .iter()
        .map(|&x| x as i32)
        .collect();
    (w, h, data)
}

#[test]
fn ht_8x8_rev_1decomp_matches_ojph() {
    let bytes = include_bytes!("fixtures/ht_8x8_rev_1decomp.j2c");
    let refpgm = include_bytes!("fixtures/ht_8x8_rev_1decomp.pgm");
    let (rw, rh, rdata) = parse_pgm(refpgm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    let c = &img.components[0];
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    assert_eq!(c.samples, rdata, "grayscale 8x8 reconstruction differs");
}

#[test]
fn ht_gray32_d3_matches_ojph() {
    let bytes = include_bytes!("fixtures/ht_gray32_d3.j2c");
    let refpgm = include_bytes!("fixtures/ht_gray32_d3_ref.pgm");
    let (rw, rh, rdata) = parse_pgm(refpgm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    assert_eq!(img.components.len(), 1);
    let c = &img.components[0];
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    assert_eq!(
        c.samples, rdata,
        "grayscale 32x24 / 3-decomp reconstruction differs"
    );
}

#[test]
fn ht_rgb24_rev_matches_ojph() {
    let bytes = include_bytes!("fixtures/ht_rgb24_rev.j2c");
    let refppm = include_bytes!("fixtures/ht_rgb24_rev_ref.ppm");
    let (rw, rh, rdata) = parse_ppm(refppm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    assert_eq!(img.components.len(), 3);
    // ojph PPM is interleaved RGB; our components are planar.
    for (comp, c) in img.components.iter().enumerate() {
        assert_eq!((c.width as usize, c.height as usize), (rw, rh));
        let de_interleaved: Vec<i32> = rdata.iter().skip(comp).step_by(3).copied().collect();
        assert_eq!(
            c.samples, de_interleaved,
            "RGB component {comp} reconstruction differs"
        );
    }
}

#[test]
fn ht_gray32_irreversible_matches_ojph() {
    // Lossy irreversible (9-7) HT: our reconstruction must match the
    // ojph reference reconstruction exactly (both apply the same §E.1
    // midpoint reconstruction to the same decoded coefficients).
    let bytes = include_bytes!("fixtures/ht_gray32_irv.j2c");
    let refpgm = include_bytes!("fixtures/ht_gray32_irv_ref.pgm");
    let (rw, rh, rdata) = parse_pgm(refpgm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    let c = &img.components[0];
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    assert_eq!(
        c.samples, rdata,
        "irreversible reconstruction differs from ojph"
    );
}

#[test]
fn ht_gray64_d1_multiblock_matches_ojph() {
    // 64×64, one decomposition, 16×16 code-blocks — every sub-band carries
    // **multiple** HT code-blocks (a 32×32 band tiles into four 16×16
    // blocks), exercising the per-block §B.2 HT-segment routing across a
    // full precinct of code-blocks rather than the one-block-per-band
    // geometry of the earlier fixtures.
    let bytes = include_bytes!("fixtures/ht_gray64_d1_multiblock.j2c");
    let refpgm = include_bytes!("fixtures/ht_gray64_d1_multiblock_ref.pgm");
    let (rw, rh, rdata) = parse_pgm(refpgm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    let c = &img.components[0];
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    assert_eq!(
        c.samples, rdata,
        "64×64 multi-code-block HT reconstruction differs"
    );
}

#[test]
fn ht_gray128_d4_multiblock_matches_ojph() {
    // 128×128, four decompositions, 32×32 code-blocks. The deep
    // decomposition spans five resolution levels, each high-pass sub-band
    // tiling into several HT code-blocks, so the resolution→sub-band→
    // code-block enumeration and the HT block-coder run end-to-end at
    // scale.
    let bytes = include_bytes!("fixtures/ht_gray128_d4_multiblock.j2c");
    let refpgm = include_bytes!("fixtures/ht_gray128_d4_multiblock_ref.pgm");
    let (rw, rh, rdata) = parse_pgm(refpgm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    let c = &img.components[0];
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    assert_eq!(
        c.samples, rdata,
        "128×128 / 4-decomp multi-code-block HT reconstruction differs"
    );
}

#[test]
fn ht_gray64_d3_irreversible_multiblock_matches_ojph() {
    // Irreversible (9-7) HT with three decompositions and 32×32 blocks:
    // the lossy reconstruction path combined with multiple code-blocks per
    // band. Our coefficients match ojph's exactly (identical §E.1
    // reconstruction over identical decoded coefficients).
    let bytes = include_bytes!("fixtures/ht_gray64_d3_irv_multiblock.j2c");
    let refpgm = include_bytes!("fixtures/ht_gray64_d3_irv_multiblock_ref.pgm");
    let (rw, rh, rdata) = parse_pgm(refpgm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    let c = &img.components[0];
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    assert_eq!(
        c.samples, rdata,
        "irreversible multi-code-block HT reconstruction differs"
    );
}

#[test]
fn fuzz_regressions_error_cleanly() {
    // Corrupt HT codestreams found by the decode_j2k fuzz harness. Both
    // drove the §7.3.8 decodeMagSgnValue width past the 32-bit
    // magnitude lane (an m_n no conformant stream in this crate's
    // precision range can signal) and must surface a clean error — the
    // decoder previously panicked on the left shift.
    for bytes in [
        &include_bytes!("fixtures/fuzz_ht_magsgn_width.j2c")[..],
        &include_bytes!("fixtures/fuzz_ht_emb_shift.j2c")[..],
    ] {
        assert!(oxideav_jpeg2000::decode_j2k(bytes).is_err());
    }
}

// ---------------------------------------------------------------------------
// Whole-codestream HT depth (round 410): real HT codestreams from the
// black-box encoder exercising the *codestream* machinery around the HT
// block decoder — multi-tile grids, image/tile offsets, tile-part
// divisions, TLM pointer markers, position-keyed progression, and
// 16-bit depth — each asserted bit-exact against the black-box
// reference reconstruction. (A 46-case sweep across both kernels, all
// five progression orders, precinct/block shapes, tile-part R/C/RC
// splits, offsets and 12/16-bit depths decodes byte-identical on every
// reversible case; the irreversible cases sit within the ±1
// half-integer rounding latitude between conforming decoders, per the
// ISO/IEC 15444-4 allowances.)
// ---------------------------------------------------------------------------

/// Parse a binary `P5` PGM with a 16-bit (`65535`) maxval into
/// `(w, h, samples)` — big-endian two-byte samples per the Netpbm spec.
fn parse_pgm16(b: &[u8]) -> (usize, usize, Vec<i32>) {
    let (w, h, start) = pnm_geometry(b);
    let data = b[start..start + 2 * w * h]
        .chunks_exact(2)
        .map(|p| i32::from(u16::from_be_bytes([p[0], p[1]])))
        .collect();
    (w, h, data)
}

/// Shared body: decode a single-component HT codestream and assert the
/// samples match a PGM reference bit-for-bit.
fn assert_ht_gray_matches(j2c: &[u8], ref_pgm: &[u8], what: &str) {
    let (rw, rh, rdata) = parse_pgm(ref_pgm);
    let img = oxideav_jpeg2000::decode_j2k(j2c).expect("decode");
    assert_eq!(img.components.len(), 1);
    let c = &img.components[0];
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    assert_eq!(c.samples, rdata, "{what} reconstruction differs");
}

/// Shared body: decode a 3-component HT codestream and assert the
/// planes match an interleaved PPM reference bit-for-bit.
fn assert_ht_rgb_matches(j2c: &[u8], ref_ppm: &[u8], what: &str) {
    let (rw, rh, rdata) = parse_ppm(ref_ppm);
    let img = oxideav_jpeg2000::decode_j2k(j2c).expect("decode");
    assert_eq!(img.components.len(), 3);
    for (comp, c) in img.components.iter().enumerate() {
        assert_eq!((c.width as usize, c.height as usize), (rw, rh));
        for (i, &s) in c.samples.iter().enumerate() {
            assert_eq!(
                s,
                rdata[i * 3 + comp],
                "{what}: component {comp} sample {i} differs"
            );
        }
    }
}

#[test]
fn ht_multi_tile_grid_matches_ojph() {
    // 100×80 gray, reversible, 32×24 tile grid (4×4 = 16 tiles, ragged
    // right/bottom edges): every tile runs its own HT block schedule
    // and the §B.3 / Equation B-12 plane placement stitches them.
    assert_ht_gray_matches(
        include_bytes!("fixtures/ht_tiles_rev.j2c"),
        include_bytes!("fixtures/ht_tiles_rev_ref.pgm"),
        "multi-tile HT",
    );
}

#[test]
fn ht_multi_tile_with_image_and_tile_offsets_matches_ojph() {
    // Same grid *plus* a (5, 5) image origin offset (SIZ XOsiz/YOsiz)
    // and a (2, 3) tile origin offset (XTOsiz/YTOsiz): the §B.3
    // reference-grid anchoring (Equations B-1/B-7) shifts every
    // tile-component region and the odd-anchored DWT parity with it.
    // First committed fixture with non-zero SIZ offsets.
    assert_ht_gray_matches(
        include_bytes!("fixtures/ht_tiles_offsets_rev.j2c"),
        include_bytes!("fixtures/ht_tiles_offsets_rev_ref.pgm"),
        "offset-anchored multi-tile HT",
    );
}

#[test]
fn ht_irreversible_multi_tile_grid_matches_ojph() {
    // The 9-7 lane through the same 16-tile grid — the black-box
    // reference reconstructs identically here (no half-integer
    // boundary sample in this stream).
    assert_ht_gray_matches(
        include_bytes!("fixtures/ht_tiles_irv.j2c"),
        include_bytes!("fixtures/ht_tiles_irv_ref.pgm"),
        "irreversible multi-tile HT",
    );
}

#[test]
fn ht_tileparts_by_resolution_matches_ojph() {
    // 48×40 tiles divided into tile-parts at each resolution
    // (TPsot > 0 chains): the §A.4.2 SOT walk must concatenate each
    // tile's parts in TPsot order before the packet walk.
    assert_ht_gray_matches(
        include_bytes!("fixtures/ht_tileparts_r_rev.j2c"),
        include_bytes!("fixtures/ht_tileparts_r_rev_ref.pgm"),
        "resolution-split tile-part HT",
    );
}

#[test]
fn ht_tileparts_by_resolution_and_component_rgb_matches_ojph() {
    // RGB (RCT), 24×16 tiles, tile-parts split on both the resolution
    // and component axes ("RC") — several TPsot > 0 parts per tile
    // interleaved with the colour transform.
    assert_ht_rgb_matches(
        include_bytes!("fixtures/ht_tileparts_rc_rgb_rev.j2c"),
        include_bytes!("fixtures/ht_tileparts_rc_rgb_rev_ref.ppm"),
        "RC tile-part RGB HT",
    );
}

#[test]
fn ht_tlm_marker_matches_ojph() {
    // Main-header TLM pointer marker (§A.7.1): tile-part lengths
    // signalled up front. The decoder's SOT walk must stay consistent
    // with (and be untroubled by) the pointer segment.
    assert_ht_gray_matches(
        include_bytes!("fixtures/ht_tlm_rev.j2c"),
        include_bytes!("fixtures/ht_tlm_rev_ref.pgm"),
        "TLM-indexed HT",
    );
}

#[test]
fn ht_pcrl_rgb_matches_ojph() {
    // Position-keyed §B.12.1.4 PCRL order over RGB with 16×16
    // precincts and code-blocks — the position-major packet interleave
    // through the HT segment-length reader.
    assert_ht_rgb_matches(
        include_bytes!("fixtures/ht_pcrl_rgb_rev.j2c"),
        include_bytes!("fixtures/ht_pcrl_rgb_rev_ref.ppm"),
        "PCRL RGB HT",
    );
}

#[test]
fn ht_16bit_reversible_matches_ojph() {
    // 16-bit-per-sample grayscale, reversible: the full-depth §7.3.8
    // MagSgn magnitude lane and the 16-bit output surface, bit-exact
    // against the black-box 16-bit PGM reference.
    let bytes = include_bytes!("fixtures/ht_deep16_rev.j2c");
    let refpgm = include_bytes!("fixtures/ht_deep16_rev_ref.pgm");
    let (rw, rh, rdata) = parse_pgm16(refpgm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    assert_eq!(img.components.len(), 1);
    let c = &img.components[0];
    assert_eq!(c.precision_bits, 16);
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    assert_eq!(c.samples, rdata, "16-bit HT reconstruction differs");
}
