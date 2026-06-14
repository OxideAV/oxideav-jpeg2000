//! End-to-end `decode_j2k` tests against committed raw-codestream
//! fixtures (T.800 Annex A `.j2k`).
//!
//! The lossless fixtures were produced by feeding deterministic
//! synthetic rasters (regenerated arithmetically below) to an opaque
//! command-line JPEG 2000 encoder used strictly as a black box; the
//! reversible 5-3 path must reproduce the source samples exactly.
//! The 9-7 irreversible fixture is pinned against a committed
//! black-box reference decode of the same codestream (PGM), with a
//! small tolerance for the floating-point inverse-DWT differences
//! T.800 Annex F permits between conforming decoders.

use oxideav_jpeg2000::{decode_j2k, decode_jpeg2000, parse_codestream, ProgressionOrder};

const GRAY_53: &[u8] = include_bytes!("data/gray-17x13-53.j2k");
const GRAY_53_TILED: &[u8] = include_bytes!("data/gray-17x13-tiled-8x8-53.j2k");
const RGB_RCT_53: &[u8] = include_bytes!("data/rgb-16x16-rct-53.j2k");
const GRAY_97: &[u8] = include_bytes!("data/gray-32x32-97.j2k");
const GRAY_97_REF_PGM: &[u8] = include_bytes!("data/gray-32x32-97-ref.pgm");
const GRAY_97_FULL: &[u8] = include_bytes!("data/gray-32x32-97full.j2k");
const GRAY_97_FULL_REF_PGM: &[u8] = include_bytes!("data/gray-32x32-97full-ref.pgm");

// Position-keyed §B.12.1.3–5 progression-order fixtures: the same
// 48×32 three-component raster, lossless 5-3, MCT off (each plane
// independent), 3 resolution levels, one precinct per level — one
// each in RPCL / PCRL / CPRL order. With three components and three
// resolution levels the three orders' packet interleaves genuinely
// differ (RPCL is resolution-major, PCRL position-major, CPRL
// component-major), so any component- or resolution-ordering slip in
// the wiring would corrupt at least one plane. COM markers scrubbed.
const RGB_RPCL_53: &[u8] = include_bytes!("data/rgb-48x32-rpcl-53.j2k");
const RGB_PCRL_53: &[u8] = include_bytes!("data/rgb-48x32-pcrl-53.j2k");
const RGB_CPRL_53: &[u8] = include_bytes!("data/rgb-48x32-cprl-53.j2k");

// Multi-precinct §B.6 / §B.7 fixture: 40×40 gray, lossless 5-3, NL = 2,
// 8×8 code-blocks (xcb = ycb = 3), precinct exponents PPx = PPy = 4
// (16×16 precinct cells) at every resolution. The precinct cell (16) is
// larger than a code-block (8), so each precinct holds a 2×2 grid of
// code-blocks, and the sub-bands span several precincts — the LRCP walk
// must visit every (precinct, code-block) in §B.10.8 raster order and
// scatter each block at its absolute §B.7 sub-band corner. This pins the
// §B.7 Eq B-17 / B-18 effective code-block exponent (`min(xcb, PPx)` at
// r = 0, `min(xcb, PPx - 1)` at r > 0): an off-by-one in the r = 0 / r > 0
// branch mis-counts the LL-band code-blocks and desyncs the packet walk.
// COM markers scrubbed; encoded with an opaque CLI codec as a black box.
const GRAY_MULTIPRECINCT_53: &[u8] = include_bytes!("data/gray-40x40-multiprecinct-53.j2k");

/// Deterministic 17×13 gray source pattern (the raster the lossless
/// gray fixtures were encoded from).
fn gray_17x13_pattern() -> Vec<i32> {
    let (w, h) = (17i32, 13i32);
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            out.push((x * 7 + y * 13 + (x * y) % 31) % 256);
        }
    }
    out
}

/// Deterministic 40×40 gray source pattern (the raster the
/// multi-precinct fixture was encoded from); same arithmetic family as
/// [`gray_17x13_pattern`].
fn gray_40x40_pattern() -> Vec<i32> {
    let (w, h) = (40i32, 40i32);
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            out.push((x * 7 + y * 13 + (x * y) % 31) % 256);
        }
    }
    out
}

/// Deterministic 16×16 RGB source pattern.
fn rgb_16x16_pattern() -> [Vec<i32>; 3] {
    let (w, h) = (16i32, 16i32);
    let mut r = Vec::new();
    let mut g = Vec::new();
    let mut b = Vec::new();
    for y in 0..h {
        for x in 0..w {
            r.push((x * 16 + 3) % 256);
            g.push((y * 16 + 7) % 256);
            b.push(((x + y) * 8 + 11) % 256);
        }
    }
    [r, g, b]
}

/// Minimal binary-PGM (P5, maxval 255) payload extractor.
fn pgm_payload(bytes: &[u8]) -> (usize, usize, &[u8]) {
    let mut toks: Vec<&[u8]> = Vec::new();
    let mut i = 0usize;
    while toks.len() < 4 {
        while bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes[i] == b'#' {
            while bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        toks.push(&bytes[start..i]);
    }
    assert_eq!(toks[0], b"P5");
    let w: usize = std::str::from_utf8(toks[1]).unwrap().parse().unwrap();
    let h: usize = std::str::from_utf8(toks[2]).unwrap().parse().unwrap();
    assert_eq!(toks[3], b"255");
    // Exactly one whitespace byte separates the header from the payload.
    (w, h, &bytes[i + 1..])
}

#[test]
fn gray_53_lossless_is_pixel_exact() {
    let img = decode_j2k(GRAY_53).expect("decode");
    assert_eq!(img.width, 17);
    assert_eq!(img.height, 13);
    assert_eq!(img.components.len(), 1);
    let c = &img.components[0];
    assert_eq!((c.width, c.height), (17, 13));
    assert_eq!(c.precision_bits, 8);
    assert!(!c.is_signed);
    assert_eq!(c.samples, gray_17x13_pattern());
}

#[test]
fn gray_53_multi_tile_is_pixel_exact() {
    // Same raster, 8×8 tile grid → 3×2 = 6 tiles, exercising the
    // per-tile decode + Equation B-12 plane placement.
    let cs = parse_codestream(GRAY_53_TILED).expect("parse");
    assert!(cs.tile_parts.len() >= 6, "expected one tile-part per tile");
    let img = decode_j2k(GRAY_53_TILED).expect("decode");
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].samples, gray_17x13_pattern());
}

#[test]
fn gray_53_multi_precinct_is_pixel_exact() {
    // 40×40 gray, lossless 5-3, NL = 2, 8×8 code-blocks, 16×16 precinct
    // cells: every sub-band spans several precincts and each precinct
    // holds a 2×2 code-block grid. Exercises the §B.6 precinct partition
    // and the §B.7 Eq B-17 / B-18 effective-exponent branch end-to-end.
    let cs = parse_codestream(GRAY_MULTIPRECINCT_53).expect("parse");
    // Confirm the fixture genuinely carries more than one precinct at
    // some resolution (PPx = PPy = 4 with NL = 2): the COD must define
    // precincts (Scod bit 0).
    assert!(
        cs.header.cod.scod & 0x01 != 0,
        "fixture must define precincts (Scod bit 0)"
    );
    let img = decode_j2k(GRAY_MULTIPRECINCT_53).expect("decode");
    assert_eq!(img.width, 40);
    assert_eq!(img.height, 40);
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].samples, gray_40x40_pattern());
}

#[test]
fn rgb_rct_53_lossless_is_pixel_exact() {
    // 3-component lossless with SGcod MCT = 1 → §G.2.2 inverse RCT.
    let img = decode_j2k(RGB_RCT_53).expect("decode");
    assert_eq!(img.components.len(), 3);
    let expected = rgb_16x16_pattern();
    for (c, exp) in img.components.iter().zip(expected.iter()) {
        assert_eq!((c.width, c.height), (16, 16));
        assert_eq!(c.precision_bits, 8);
        assert_eq!(&c.samples, exp);
    }
}

#[test]
fn rgb_rct_53_interleaved_wrapper_matches_planes() {
    let bytes = decode_jpeg2000(RGB_RCT_53).expect("decode");
    let expected = rgb_16x16_pattern();
    assert_eq!(bytes.len(), 16 * 16 * 3);
    for (i, px) in bytes.chunks_exact(3).enumerate() {
        assert_eq!(px[0] as i32, expected[0][i]);
        assert_eq!(px[1] as i32, expected[1][i]);
        assert_eq!(px[2] as i32, expected[2][i]);
    }
}

/// Decode a 32×32 9-7 fixture and return `(max, mean)` absolute
/// deviation from its committed black-box reference decode.
fn gray_97_deviation(j2k: &[u8], ref_pgm: &[u8]) -> (i32, f64) {
    let img = decode_j2k(j2k).expect("decode");
    assert_eq!(img.components.len(), 1);
    let c = &img.components[0];
    assert_eq!((c.width, c.height), (32, 32));

    let (rw, rh, payload) = pgm_payload(ref_pgm);
    assert_eq!((rw, rh), (32, 32));
    assert_eq!(payload.len(), c.samples.len());

    let mut max_diff = 0i32;
    let mut sum = 0u64;
    for (&ours, &refv) in c.samples.iter().zip(payload.iter()) {
        let d = (ours - refv as i32).abs();
        max_diff = max_diff.max(d);
        sum += d as u64;
    }
    (max_diff, sum as f64 / payload.len() as f64)
}

#[test]
fn gray_97_irreversible_full_quality_matches_black_box_reference() {
    // 9-7 irreversible, scalar-expounded quantisation, 6 resolution
    // levels, every coding pass present (no rate truncation, so
    // Nb = Mb for every code-block). Pinned against a committed
    // black-box decode of the same codestream; ±1 covers the Annex F
    // floating-point latitude between conforming inverse DWTs.
    let (max_diff, _) = gray_97_deviation(GRAY_97_FULL, GRAY_97_FULL_REF_PGM);
    assert!(
        max_diff <= 1,
        "full-quality 9-7 decode deviates from the reference by {max_diff} (> 1)"
    );
}

#[test]
fn gray_97_irreversible_truncated_tracks_black_box_reference() {
    // Same source rate-limited 4:1 — coding passes are truncated
    // mid-bit-plane, so per E.1.1.2 NOTE Nb(u, v) differs across one
    // code-block. The wiring currently models Nb per *block* (the
    // fully-completed bit-plane count), which costs up to one
    // bit-plane of Equation E-6 reconstruction-lift accuracy on the
    // coefficients the partial passes did reach; the deviation bound
    // here pins that approximation until per-coefficient Nb lands.
    let (max_diff, mean) = gray_97_deviation(GRAY_97, GRAY_97_REF_PGM);
    assert!(
        max_diff <= 16,
        "truncated 9-7 decode deviates from the reference by {max_diff} (> 16)"
    );
    assert!(
        mean <= 4.0,
        "truncated 9-7 decode mean deviation {mean} (> 4.0)"
    );
}

/// Deterministic 48×32 three-component source pattern (the raster the
/// position-keyed §B.12.1.3–5 fixtures were encoded from), MCT off so
/// each plane is independent.
fn rgb_48x32_pattern() -> [Vec<i32>; 3] {
    let (w, h) = (48i32, 32i32);
    let mut r = Vec::new();
    let mut g = Vec::new();
    let mut b = Vec::new();
    for y in 0..h {
        for x in 0..w {
            r.push((x * 5 + y * 11 + (x * y) % 37) % 256);
            g.push((x * 9 + y * 3 + (x + y) % 29) % 256);
            b.push((x * 2 + y * 7 + (x * y) % 23) % 256);
        }
    }
    [r, g, b]
}

/// Shared body for the three position-keyed fixtures: assert the COD
/// carries the expected §B.12 progression order, then assert the
/// reversible 5-3 decode reproduces the source raster exactly on
/// every plane.
fn assert_position_keyed_pixel_exact(j2k: &[u8], expected: ProgressionOrder) {
    let cs = parse_codestream(j2k).expect("parse");
    assert_eq!(
        cs.header.cod.progression, expected,
        "fixture COD progression order"
    );
    let img = decode_j2k(j2k).expect("decode");
    assert_eq!((img.width, img.height), (48, 32));
    assert_eq!(img.components.len(), 3);
    let expected_planes = rgb_48x32_pattern();
    for (c, exp) in img.components.iter().zip(expected_planes.iter()) {
        assert_eq!((c.width, c.height), (48, 32));
        assert_eq!(c.precision_bits, 8);
        assert!(!c.is_signed);
        assert_eq!(&c.samples, exp);
    }
}

#[test]
fn rgb_rpcl_53_lossless_is_pixel_exact() {
    // §B.12.1.3 resolution level-position-component-layer order.
    assert_position_keyed_pixel_exact(RGB_RPCL_53, ProgressionOrder::Rpcl);
}

#[test]
fn rgb_pcrl_53_lossless_is_pixel_exact() {
    // §B.12.1.4 position-component-resolution level-layer order.
    assert_position_keyed_pixel_exact(RGB_PCRL_53, ProgressionOrder::Pcrl);
}

#[test]
fn rgb_cprl_53_lossless_is_pixel_exact() {
    // §B.12.1.5 component-position-resolution level-layer order.
    assert_position_keyed_pixel_exact(RGB_CPRL_53, ProgressionOrder::Cprl);
}

#[test]
fn truncated_codestream_is_rejected() {
    let cut = &GRAY_53[..GRAY_53.len() / 2];
    assert!(decode_j2k(cut).is_err());
}
