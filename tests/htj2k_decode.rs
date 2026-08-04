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

#[test]
fn ht_reduced_resolution_matches_ojph() {
    // The §B.2.3 reduced-resolution surface through the HT block
    // decoder: the multi-tile grid at one discarded level and the
    // offset-anchored grid at two, each byte-exact against the
    // black-box HT decoder's own reduced reconstruction.
    for (j2c, ref_pgm, discard, what) in [
        (
            &include_bytes!("fixtures/ht_tiles_rev.j2c")[..],
            &include_bytes!("fixtures/ht_tiles_rev_r1_ref.pgm")[..],
            1u8,
            "multi-tile HT r1",
        ),
        (
            &include_bytes!("fixtures/ht_tiles_offsets_rev.j2c")[..],
            &include_bytes!("fixtures/ht_tiles_offsets_rev_r2_ref.pgm")[..],
            2,
            "offset-anchored HT r2",
        ),
    ] {
        let (rw, rh, rdata) = parse_pgm(ref_pgm);
        let img = oxideav_jpeg2000::decode_j2k_reduced(j2c, discard).expect("reduced HT decode");
        let c = &img.components[0];
        assert_eq!((c.width as usize, c.height as usize), (rw, rh), "{what}");
        assert_eq!(c.samples, rdata, "{what}: reduced reconstruction differs");
    }
}

/// Round-416 precinct-unaligned-tile shapes through the **HT lane**:
/// 45×39 gray, 15×13 tiles (tile 1's full-resolution edge lands one
/// sample below a precinct-cell boundary), custom 16×16 / 8×8
/// precincts, an XOsiz / YOsiz = 7 / 3 image-origin offset, PCRL, 16×16
/// HT code-blocks, reversible 5-3 — part of an 80-case black-box HT
/// sweep (all five orders × tiling × precincts × offsets, gray and
/// RGB/RCT) that decodes byte-exact against the sources after the
/// §B.6 / §B.12.1.3–5 unaligned-tile fixes. COM markers scrubbed.
#[test]
fn ht_unaligned_tiles_precincts_offset_pcrl_rev() {
    let bytes = include_bytes!("fixtures/ht_t15_prec_off_pcrl_rev.j2c");
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode HT unaligned tiles");
    let c = &img.components[0];
    assert_eq!((c.width, c.height), (45, 39));
    for y in 0..39i32 {
        for x in 0..45i32 {
            assert_eq!(
                c.samples[(y * 45 + x) as usize],
                (x * 7 + y * 13) % 256,
                "pixel ({x}, {y})"
            );
        }
    }
}

/// The RGB / RCT sibling: 45×39 three-component, 15×13 tiles, custom
/// precincts, CPRL, reversible — the component-major order across an
/// unaligned multi-tile grid in the HT lane.
#[test]
fn ht_rgb_unaligned_tiles_precincts_cprl_rev() {
    let bytes = include_bytes!("fixtures/ht_rgb_t15_prec_cprl_rev.j2c");
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode HT RGB unaligned tiles");
    assert_eq!(img.components.len(), 3);
    for (ci, c) in img.components.iter().enumerate() {
        assert_eq!((c.width, c.height), (45, 39));
        for y in 0..39i32 {
            for x in 0..45i32 {
                let want = match ci {
                    0 => (x * 5 + y * 11) % 256,
                    1 => (x * 9 + y * 3) % 256,
                    _ => (x * 2 + y * 7) % 256,
                };
                assert_eq!(
                    c.samples[(y * 45 + x) as usize],
                    want,
                    "comp {ci} pixel ({x}, {y})"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// T.814 §A.3.2 stream-level HT signalling (CAP Ccap15 bits 15-14).
// ---------------------------------------------------------------------------

/// Walk the main-header marker chain of `bytes` and return the offset
/// of the first occurrence of `marker`'s segment (at its 0xFF byte).
fn find_marker(bytes: &[u8], marker: u16) -> usize {
    let mut pos = 2usize; // skip SOC
    while pos + 4 <= bytes.len() {
        let m = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]);
        if m == marker {
            return pos;
        }
        let len = u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]) as usize;
        pos += 2 + len;
    }
    panic!("marker {marker:#06x} not found");
}

/// Return a copy of `bytes` with the main-header COD `SPcod` style
/// byte replaced by `style` (Table A.12 layout: marker(2) + Lcod(2) +
/// Scod(1) + SGcod(4) + NL(1) + xcb(1) + ycb(1) + style(1)).
fn with_cod_style(bytes: &[u8], style: u8) -> Vec<u8> {
    let cod = find_marker(bytes, 0xFF52);
    let mut out = bytes.to_vec();
    out[cod + 12] = style;
    out
}

/// Return a copy of `bytes` with the CAP marker's 16-bit `Ccap15`
/// field replaced (T.814 §A.3: marker(2) + Lcap(2) + Pcap(4) +
/// Ccap15(2)).
fn with_ccap15(bytes: &[u8], ccap15: u16) -> Vec<u8> {
    let cap = find_marker(bytes, 0xFF50);
    let mut out = bytes.to_vec();
    out[cap + 8..cap + 10].copy_from_slice(&ccap15.to_be_bytes());
    out
}

/// T.814 §A.3.2: under a `Ccap15` whose bits 15-14 are `00` the stream
/// is HTONLY — **every** code-block is an HT code-block, whatever the
/// `SPcod` bits 6 / 7 say. The strict §A.3.2 first-branch signalling
/// carries the style bits as `00` (the CAP alone routes the blocks),
/// and the §A.3.2 NOTE admits `11`; both must reconstruct identically
/// to the fixture's native `bit 6 = 1` signalling.
#[test]
fn htonly_cap_routes_blocks_regardless_of_spcod_bits() {
    let bytes = include_bytes!("fixtures/ht_8x8_rev_1decomp.j2c");
    let baseline = oxideav_jpeg2000::decode_j2k(bytes).expect("baseline decode");
    for style in [0x00u8, 0xC0] {
        let patched = with_cod_style(bytes, style);
        let img = oxideav_jpeg2000::decode_j2k(&patched)
            .unwrap_or_else(|e| panic!("HTONLY decode with SPcod style {style:#04x}: {e:?}"));
        assert_eq!(img.components.len(), baseline.components.len());
        for (a, b) in img.components.iter().zip(baseline.components.iter()) {
            assert_eq!(a.samples, b.samples, "style {style:#04x}");
        }
    }
}

/// T.814 §A.3.2: under `Ccap15` bits 15-14 = `10` (HTDECLARED) the
/// per-tile-component bit 6 still selects the lane — and "bit 7 of all
/// SPcod or SPcoc values is equal to 0", so a set bit 7 is rejected.
#[test]
fn htdeclared_ccap_enforces_spcod_bit7_clear() {
    let bytes = include_bytes!("fixtures/ht_8x8_rev_1decomp.j2c");
    let baseline = oxideav_jpeg2000::decode_j2k(bytes).expect("baseline decode");
    let declared = with_ccap15(bytes, 0x8001);
    let img = oxideav_jpeg2000::decode_j2k(&declared).expect("HTDECLARED decode");
    assert_eq!(img.components[0].samples, baseline.components[0].samples);
    // bit 7 set under HTDECLARED → reject.
    let bad = with_cod_style(&declared, 0xC0);
    assert!(oxideav_jpeg2000::decode_j2k(&bad).is_err());
}

/// Table A.2: `Ccap15` bits 15-14 = `01` and bits 10-6 are "Reserved
/// for future use by ITU-T | ISO/IEC" — a stream that sets them
/// signals semantics this decoder does not know, and is rejected
/// rather than mis-decoded.
#[test]
fn reserved_ccap15_encodings_rejected() {
    let bytes = include_bytes!("fixtures/ht_8x8_rev_1decomp.j2c");
    // Reserved quadrant 01.
    assert!(oxideav_jpeg2000::decode_j2k(&with_ccap15(bytes, 0x4001)).is_err());
    // Reserved bit 6.
    assert!(oxideav_jpeg2000::decode_j2k(&with_ccap15(bytes, 0x0041)).is_err());
}

/// `Ccap15` bits 15-14 = `11` (MIXED permitted) with a plain
/// `bit 6 = 1, bit 7 = 0` style byte is still an all-HT tile-component
/// (Table A.3) — the MIXED reading only turns on per tile-component
/// via bits 6 + 7 = 11.
#[test]
fn mixed_permitted_all_ht_component_decodes() {
    let bytes = include_bytes!("fixtures/ht_8x8_rev_1decomp.j2c");
    let baseline = oxideav_jpeg2000::decode_j2k(bytes).expect("baseline decode");
    let mixed_cap = with_ccap15(bytes, 0xC001);
    let img = oxideav_jpeg2000::decode_j2k(&mixed_cap).expect("MIXED-permitted decode");
    assert_eq!(img.components[0].samples, baseline.components[0].samples);
}

/// T.800 Table A.19 reserves `SPcod` bit 7; without a CAP marker (or
/// with bit 6 clear under one that permits MIXED) no reading blesses
/// `bit 7 = 1, bit 6 = 0`.
#[test]
fn spcod_bit7_without_bit6_rejected() {
    let bytes = include_bytes!("fixtures/ht_8x8_rev_1decomp.j2c");
    assert!(oxideav_jpeg2000::decode_j2k(&with_cod_style(bytes, 0x80)).is_err());
}

/// The T.814 §A.6 `CPF` (corresponding profile) and T.800 §A.9.1 `CRG`
/// (component registration) marker segments are informational — CPF
/// describes the clause-8.8 transcoding correspondence and CRG "has no
/// effect on decoding the codestream" — so a main header carrying both
/// decodes identically to one without them.
#[test]
fn cpf_and_crg_markers_are_skipped() {
    let bytes = include_bytes!("fixtures/ht_8x8_rev_1decomp.j2c");
    let baseline = oxideav_jpeg2000::decode_j2k(bytes).expect("baseline decode");
    // Insert CPF (marker + Lcpf=4 + Pcpf1) and CRG (marker + Lcrg=6 +
    // Xcrg + Ycrg for the single component) after the CAP segment.
    let cap = find_marker(bytes, 0xFF50);
    let cap_len = u16::from_be_bytes([bytes[cap + 2], bytes[cap + 3]]) as usize;
    let insert_at = cap + 2 + cap_len;
    let mut patched = bytes[..insert_at].to_vec();
    patched.extend_from_slice(&[0xFF, 0x59, 0x00, 0x04, 0x00, 0x03]); // CPF
    patched.extend_from_slice(&[0xFF, 0x63, 0x00, 0x06, 0x40, 0x00, 0x40, 0x00]); // CRG
    patched.extend_from_slice(&bytes[insert_at..]);
    let img = oxideav_jpeg2000::decode_j2k(&patched).expect("decode with CPF + CRG");
    assert_eq!(img.components[0].samples, baseline.components[0].samples);
}
