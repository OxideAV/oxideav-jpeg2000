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

// Multi-layer §B.10.4 / §B.12 fixture: 64×64 gray, lossless 5-3, NL = 2,
// 16×16 code-blocks, single precinct per resolution, LRCP, FIVE quality
// layers. The rate allocator spreads each code-block's coding passes
// across the five layers, so a block first becomes included in one layer
// (§B.10.4 inclusion tag tree) and is then refined by further coding
// passes in every later layer it contributes to. The per-precinct
// inclusion + Lblock state must persist across the five LRCP layer passes
// and the §C.3 codeword segments concatenate (no segmentation-changing
// Table A.19 bit is set), so the tier-1 decoder sees one segment per
// code-block built from every layer's contribution in order. A
// layer-ordering or per-layer-state slip would corrupt the reconstruction.
// COM markers scrubbed; encoded with an opaque CLI codec as a black box.
const GRAY_MULTILAYER_53: &[u8] = include_bytes!("data/gray-64x64-multilayer-53.j2k");

// §D.4.2 "termination on each coding pass" fixture: 40×40 gray,
// lossless 5-3, NL = 3, 8×8 code-blocks, with the COD Table A.19
// code-block-style bit 2 set. Every coding pass owns its own
// terminated §C.3 codeword segment, so the §B.10.7.2 multi-segment
// length signalling is exercised (one §B.10.7.1 length per pass) and
// the tier-1 driver must open a fresh MQ decoder per pass while the
// Annex D contexts persist across the per-pass segment boundaries. The
// reconstruction must remain pixel-exact versus the source raster. COM
// markers scrubbed; encoded with an opaque CLI codec as a black box.
const GRAY_TERMALL_53: &[u8] = include_bytes!("data/gray-40x40-termall-53.j2k");

/// Deterministic 64×64 gray source pattern (the raster the multi-layer
/// fixture was encoded from); same arithmetic family as
/// [`gray_17x13_pattern`] with an extra high-frequency `(x ^ y)` term so
/// the sub-bands carry energy the rate allocator distributes across the
/// quality layers (forcing genuine cross-layer code-block refinement).
fn gray_64x64_pattern() -> Vec<i32> {
    let (w, h) = (64i32, 64i32);
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            out.push((x * 7 + y * 13 + (x * y) % 31 + (x ^ y) * 5) % 256);
        }
    }
    out
}

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

/// §A.6.5 main-header `QCC` override. Inject a `QCC` segment for
/// component 0 into the gray 5-3 fixture's main header that mirrors the
/// fixture's `QCD` byte-for-byte (same style, guard bits, step sizes).
///
/// Because the override is identical to the default it replaces, the
/// decode must remain **pixel-exact** versus the un-injected stream —
/// which proves the wiring (a) no longer rejects a main-header `QCC`
/// with `Error::NotImplemented`, and (b) parses `Cqcc` / `Sqcc` /
/// `SPqcc` and routes them into the per-component quantisation
/// resolution `resolve_band_quant` consumes. A wrong `Cqcc` width, a
/// mis-read style byte, or a dropped step-size payload would change the
/// reconstructed coefficients and break the equality.
#[test]
fn gray_53_redundant_main_header_qcc_is_pixel_exact() {
    use oxideav_jpeg2000::MARKER_QCC;

    let cs = parse_codestream(GRAY_53).expect("parse");
    let qcd = &cs.header.qcd;
    let insert_at = cs.header.bytes_consumed;

    // Sanity: the un-injected fixture has no QCC (so this test really
    // exercises the new path), and the insertion point is the SOT.
    assert!(!GRAY_53[2..insert_at]
        .windows(2)
        .any(|w| w == MARKER_QCC.to_be_bytes()));
    assert_eq!(
        u16::from_be_bytes([GRAY_53[insert_at], GRAY_53[insert_at + 1]]),
        0xFF90, // SOT
    );

    // Build a QCC for component 0 mirroring the QCD. Csiz = 1 < 257 so
    // Cqcc is 8-bit. Lqcc = 2 (length) + 1 (Cqcc) + 1 (Sqcc) + SPqcc.
    let lqcc = 2 + 1 + 1 + qcd.spqcd.len();
    let mut qcc = Vec::new();
    qcc.extend_from_slice(&MARKER_QCC.to_be_bytes());
    qcc.extend_from_slice(&(lqcc as u16).to_be_bytes());
    qcc.push(0u8); // Cqcc = component 0
    qcc.push(qcd.sqcd); // Sqcc mirrors Sqcd (style + guard bits)
    qcc.extend_from_slice(&qcd.spqcd); // SPqcc mirrors SPqcd

    let mut injected = Vec::with_capacity(GRAY_53.len() + qcc.len());
    injected.extend_from_slice(&GRAY_53[..insert_at]);
    injected.extend_from_slice(&qcc);
    injected.extend_from_slice(&GRAY_53[insert_at..]);

    // The injected QCC must now be parsed (not skipped-then-rejected).
    let parsed = parse_codestream(&injected).expect("parse with main-header QCC");
    assert_eq!(parsed.header.siz.components.len(), 1);

    let img = decode_j2k(&injected).expect("decode with main-header QCC");
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].samples, gray_17x13_pattern());
}

/// Injects a redundant main-header `COC` (T.800 §A.6.2) for component
/// 0 into the single-component `gray-17x13-53` fixture, restating the
/// fixture's `COD` per-component coding style byte-for-byte (same NL,
/// code-block size, code-block style, kernel and precinct mode).
///
/// Because the override is identical to the default it replaces, the
/// decode must remain **pixel-exact** versus the un-injected stream —
/// which proves the wiring (a) no longer rejects a main-header `COC`
/// with `Error::NotImplemented`, and (b) parses `Ccoc` / `Scoc` /
/// `SPcoc` and routes them into the per-component coding resolution
/// (`resolve_component_coding`) the geometry + tier-1 + IDWT cascade
/// consume. A wrong `Ccoc` width, a mis-read `SPcoc` field, or a
/// dropped precinct payload would change the geometry and break the
/// equality.
#[test]
fn gray_53_redundant_main_header_coc_is_pixel_exact() {
    use oxideav_jpeg2000::{WaveletTransform, MARKER_COC};

    let cs = parse_codestream(GRAY_53).expect("parse");
    let cod = &cs.header.cod;
    let insert_at = cs.header.bytes_consumed;

    // Sanity: the un-injected fixture has no COC (so this test really
    // exercises the new path), and the insertion point is the SOT.
    assert!(!GRAY_53[2..insert_at]
        .windows(2)
        .any(|w| w == MARKER_COC.to_be_bytes()));
    assert_eq!(
        u16::from_be_bytes([GRAY_53[insert_at], GRAY_53[insert_at + 1]]),
        0xFF90, // SOT
    );

    // Build a COC for component 0 mirroring the COD. Csiz = 1 < 257 so
    // Ccoc is 8-bit. Scoc carries only the precinct-defined low bit
    // (Table A.23); SPcoc = NL, xcb, ycb, style, kernel, then NL+1
    // precinct bytes when user-defined precincts are signalled.
    let scoc = u8::from(cod.user_defined_precincts);
    let kernel_byte = match cod.transform {
        WaveletTransform::Irreversible9x7 => 0x00u8,
        WaveletTransform::Reversible5x3 => 0x01u8,
        WaveletTransform::Reserved(b) => b,
    };
    // Lcoc = 2 (length) + 1 (Ccoc) + 1 (Scoc) + 5 (SPcoc fixed) + precincts.
    let lcoc = 2 + 1 + 1 + 5 + cod.precincts.len();
    let mut coc = Vec::new();
    coc.extend_from_slice(&MARKER_COC.to_be_bytes());
    coc.extend_from_slice(&(lcoc as u16).to_be_bytes());
    coc.push(0u8); // Ccoc = component 0
    coc.push(scoc);
    coc.push(cod.decomposition_levels);
    coc.push(cod.code_block_width_exp);
    coc.push(cod.code_block_height_exp);
    coc.push(cod.code_block_style);
    coc.push(kernel_byte);
    coc.extend_from_slice(&cod.precincts);

    let mut injected = Vec::with_capacity(GRAY_53.len() + coc.len());
    injected.extend_from_slice(&GRAY_53[..insert_at]);
    injected.extend_from_slice(&coc);
    injected.extend_from_slice(&GRAY_53[insert_at..]);

    // The injected COC must now be parsed (not skipped-then-rejected).
    let parsed = parse_codestream(&injected).expect("parse with main-header COC");
    assert_eq!(parsed.header.siz.components.len(), 1);

    let img = decode_j2k(&injected).expect("decode with main-header COC");
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].samples, gray_17x13_pattern());
}

/// Injects a main-header `RGN` (T.800 §A.6.3) for component 0 with
/// `Srgn = 0` (implicit ROI / Maxshift) and `SPrgn = 0` (zero shift)
/// into the single-component `gray-17x13-53` fixture.
///
/// A zero scaling value makes the §H.1 Maxshift decode an exact
/// identity: the coded bit budget `M'b = Mb + 0 = Mb` is unchanged and
/// every §H.1 branch reduces to a no-op, so the reconstruction must
/// stay **pixel-exact** versus the un-injected stream. This proves the
/// wiring (a) no longer rejects a main-header `RGN` with
/// `Error::NotImplemented`, and (b) parses `Crgn` / `Srgn` / `SPrgn`
/// and routes the resolved shift through `resolve_component_roi_shift`
/// into the tier-1 budget and the §H.1 de-scaling without disturbing a
/// non-ROI decode.
#[test]
fn gray_53_main_header_rgn_zero_shift_is_pixel_exact() {
    use oxideav_jpeg2000::MARKER_RGN;

    let cs = parse_codestream(GRAY_53).expect("parse");
    let insert_at = cs.header.bytes_consumed;

    // Sanity: the un-injected fixture has no RGN, and the insertion
    // point is the SOT.
    assert!(!GRAY_53[2..insert_at]
        .windows(2)
        .any(|w| w == MARKER_RGN.to_be_bytes()));
    assert_eq!(
        u16::from_be_bytes([GRAY_53[insert_at], GRAY_53[insert_at + 1]]),
        0xFF90, // SOT
    );

    // Build an RGN for component 0. Csiz = 1 < 257 so Crgn is 8-bit.
    // Lrgn = 2 (length) + 1 (Crgn) + 1 (Srgn) + 1 (SPrgn) = 5.
    let mut rgn = Vec::new();
    rgn.extend_from_slice(&MARKER_RGN.to_be_bytes());
    rgn.extend_from_slice(&5u16.to_be_bytes());
    rgn.push(0u8); // Crgn = component 0
    rgn.push(0u8); // Srgn = 0 (implicit ROI / Maxshift)
    rgn.push(0u8); // SPrgn = 0 (zero shift ⇒ §H.1 identity)

    let mut injected = Vec::with_capacity(GRAY_53.len() + rgn.len());
    injected.extend_from_slice(&GRAY_53[..insert_at]);
    injected.extend_from_slice(&rgn);
    injected.extend_from_slice(&GRAY_53[insert_at..]);

    // The injected RGN must now be parsed (not skipped-then-rejected).
    let parsed = parse_codestream(&injected).expect("parse with main-header RGN");
    assert_eq!(parsed.header.siz.components.len(), 1);

    let img = decode_j2k(&injected).expect("decode with main-header RGN");
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].samples, gray_17x13_pattern());
}

/// A main-header `RGN` with a non-zero `Srgn` (any style other than the
/// Table A.25 implicit-ROI / Maxshift `Srgn = 0`) is reserved and not
/// wired; the decoder must surface a clean `Error::NotImplemented`
/// rather than mis-decode.
#[test]
fn gray_53_main_header_rgn_non_maxshift_style_is_rejected() {
    use oxideav_jpeg2000::{Error, MARKER_RGN};

    let cs = parse_codestream(GRAY_53).expect("parse");
    let insert_at = cs.header.bytes_consumed;

    let mut rgn = Vec::new();
    rgn.extend_from_slice(&MARKER_RGN.to_be_bytes());
    rgn.extend_from_slice(&5u16.to_be_bytes());
    rgn.push(0u8); // Crgn = component 0
    rgn.push(1u8); // Srgn = 1 (reserved / not Maxshift)
    rgn.push(3u8); // SPrgn

    let mut injected = Vec::with_capacity(GRAY_53.len() + rgn.len());
    injected.extend_from_slice(&GRAY_53[..insert_at]);
    injected.extend_from_slice(&rgn);
    injected.extend_from_slice(&GRAY_53[insert_at..]);

    assert_eq!(decode_j2k(&injected), Err(Error::NotImplemented));
}

/// Splice `seg` (a complete marker segment, marker code + length +
/// payload) into the first tile-part header of `stream`, immediately
/// before that tile-part's `SOD`, and grow the tile-part's `Psot`
/// length field by `seg.len()` so the §A.4.2 framing stays consistent.
///
/// The first tile-part's `SOT` is located via the parsed codestream so
/// the helper does not re-implement marker scanning; the `Psot` field
/// lives at `sot_offset + 6` (T.800 Table A.5: `SOT` marker (2) +
/// `Lsot` (2) + `Isot` (2) + `Psot` (4)).
fn inject_into_first_tile_part_header(stream: &[u8], seg: &[u8]) -> Vec<u8> {
    let cs = parse_codestream(stream).expect("parse for injection");
    let tp = cs
        .tile_parts
        .iter()
        .find(|tp| tp.sot.tile_part_index == 0)
        .expect("a TPsot = 0 tile-part");
    let sot_offset = tp.sot_offset;
    let sod_offset = tp.sod_offset;

    let mut out = Vec::with_capacity(stream.len() + seg.len());
    out.extend_from_slice(&stream[..sod_offset]);
    out.extend_from_slice(seg);
    out.extend_from_slice(&stream[sod_offset..]);

    // Grow Psot (4 bytes at sot_offset + 6) unless it is 0 ("until EOC",
    // which needs no adjustment).
    let psot_at = sot_offset + 6;
    let psot = u32::from_be_bytes([
        out[psot_at],
        out[psot_at + 1],
        out[psot_at + 2],
        out[psot_at + 3],
    ]);
    if psot != 0 {
        let grown = psot + seg.len() as u32;
        out[psot_at..psot_at + 4].copy_from_slice(&grown.to_be_bytes());
    }
    out
}

/// §A.6.1 tile-part `COD` override. Inject a redundant `COD` into the
/// first tile-part header of the gray 5-3 fixture restating the main
/// `COD` byte-for-byte. Because the tile override equals the main
/// default it supersedes, the decode must stay **pixel-exact** — which
/// proves the tile-part `COD` is parsed, routed through the §A.6
/// `Tile COD > Main COD` precedence, and drives the per-tile geometry +
/// tier-1 + IDWT instead of being rejected with `Error::NotImplemented`.
#[test]
fn gray_53_redundant_tile_part_cod_is_pixel_exact() {
    use oxideav_jpeg2000::{WaveletTransform, MARKER_COD};

    let cs = parse_codestream(GRAY_53).expect("parse");
    let cod = &cs.header.cod;

    // Rebuild the COD segment from the parsed fields (T.800 Table A.12).
    let scod = (u8::from(cod.user_defined_precincts))
        | (u8::from(cod.sop_marker_allowed) << 1)
        | (u8::from(cod.eph_marker_used) << 2);
    let kernel_byte = match cod.transform {
        WaveletTransform::Irreversible9x7 => 0x00u8,
        WaveletTransform::Reversible5x3 => 0x01u8,
        WaveletTransform::Reserved(b) => b,
    };
    // Lcod = 2 + 1 (Scod) + 4 (SGcod: prog, layers(2), mct) + 5 (SPcod
    // fixed: NL, xcb, ycb, style, kernel) + precincts.
    let lcod = 2 + 1 + 4 + 5 + cod.precincts.len();
    let mut seg = Vec::new();
    seg.extend_from_slice(&MARKER_COD.to_be_bytes());
    seg.extend_from_slice(&(lcod as u16).to_be_bytes());
    seg.push(scod);
    seg.push(0u8); // SGcod progression order (LRCP = 0)
    seg.extend_from_slice(&cod.layers.to_be_bytes());
    seg.push(cod.multi_component_transform);
    seg.push(cod.decomposition_levels);
    seg.push(cod.code_block_width_exp);
    seg.push(cod.code_block_height_exp);
    seg.push(cod.code_block_style);
    seg.push(kernel_byte);
    seg.extend_from_slice(&cod.precincts);

    let injected = inject_into_first_tile_part_header(GRAY_53, &seg);

    // The injected COD must now appear in the tile-part markers (parsed,
    // not skipped-then-rejected).
    let parsed = parse_codestream(&injected).expect("parse with tile-part COD");
    assert!(parsed.tile_parts.iter().any(|tp| tp
        .markers
        .iter()
        .any(|m| matches!(m, oxideav_jpeg2000::TilePartMarker::Cod(_)))));

    let img = decode_j2k(&injected).expect("decode with tile-part COD");
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].samples, gray_17x13_pattern());
}

/// §A.6.4 tile-part `QCD` override. Inject a redundant `QCD` into the
/// first tile-part header restating the main `QCD` byte-for-byte. The
/// decode must stay **pixel-exact**, proving the tile-part `QCD` is
/// routed through the §A.6 `Tile QCD > Main QCD` precedence into the
/// per-component quantisation `resolve_band_quant` consumes.
#[test]
fn gray_53_redundant_tile_part_qcd_is_pixel_exact() {
    use oxideav_jpeg2000::MARKER_QCD;

    let cs = parse_codestream(GRAY_53).expect("parse");
    let qcd = &cs.header.qcd;

    // Lqcd = 2 + 1 (Sqcd) + SPqcd.
    let lqcd = 2 + 1 + qcd.spqcd.len();
    let mut seg = Vec::new();
    seg.extend_from_slice(&MARKER_QCD.to_be_bytes());
    seg.extend_from_slice(&(lqcd as u16).to_be_bytes());
    seg.push(qcd.sqcd);
    seg.extend_from_slice(&qcd.spqcd);

    let injected = inject_into_first_tile_part_header(GRAY_53, &seg);

    let parsed = parse_codestream(&injected).expect("parse with tile-part QCD");
    assert!(parsed.tile_parts.iter().any(|tp| tp
        .markers
        .iter()
        .any(|m| matches!(m, oxideav_jpeg2000::TilePartMarker::Qcd(_)))));

    let img = decode_j2k(&injected).expect("decode with tile-part QCD");
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].samples, gray_17x13_pattern());
}

/// §A.6.3 tile-part `RGN` with `Srgn = SPrgn = 0` (Maxshift, zero
/// shift) is a §H.1 identity, so injecting it into the first tile-part
/// header must leave the decode **pixel-exact** — proving the tile-part
/// `RGN` is parsed and routed through the per-tile ROI resolution.
#[test]
fn gray_53_tile_part_rgn_zero_shift_is_pixel_exact() {
    use oxideav_jpeg2000::MARKER_RGN;

    let mut seg = Vec::new();
    seg.extend_from_slice(&MARKER_RGN.to_be_bytes());
    seg.extend_from_slice(&5u16.to_be_bytes());
    seg.push(0u8); // Crgn = component 0
    seg.push(0u8); // Srgn = 0 (Maxshift)
    seg.push(0u8); // SPrgn = 0 (zero shift ⇒ §H.1 identity)

    let injected = inject_into_first_tile_part_header(GRAY_53, &seg);
    let img = decode_j2k(&injected).expect("decode with tile-part RGN");
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].samples, gray_17x13_pattern());
}

/// A tile-part `COD` that sets the §D.6 selective-arithmetic-coding
/// bypass style bit (Table A.19 bit 0) — unwired in the tier-1 driver —
/// must surface a clean `Error::NotImplemented` for that tile even
/// though the main `COD` did not set it.
#[test]
fn gray_53_tile_part_cod_bypass_style_is_rejected() {
    use oxideav_jpeg2000::{Error, WaveletTransform, MARKER_COD};

    let cs = parse_codestream(GRAY_53).expect("parse");
    let cod = &cs.header.cod;
    let kernel_byte = match cod.transform {
        WaveletTransform::Irreversible9x7 => 0x00u8,
        WaveletTransform::Reversible5x3 => 0x01u8,
        WaveletTransform::Reserved(b) => b,
    };
    let lcod = 2 + 1 + 4 + 5 + cod.precincts.len();
    let mut seg = Vec::new();
    seg.extend_from_slice(&MARKER_COD.to_be_bytes());
    seg.extend_from_slice(&(lcod as u16).to_be_bytes());
    seg.push(0u8); // Scod = 0 (maximum precincts)
    seg.push(0u8); // progression LRCP
    seg.extend_from_slice(&cod.layers.to_be_bytes());
    seg.push(cod.multi_component_transform);
    seg.push(cod.decomposition_levels);
    seg.push(cod.code_block_width_exp);
    seg.push(cod.code_block_height_exp);
    seg.push(0x01u8); // code-block style: §D.6 bypass bit set
    seg.push(kernel_byte);
    seg.extend_from_slice(&cod.precincts);

    let injected = inject_into_first_tile_part_header(GRAY_53, &seg);
    assert_eq!(decode_j2k(&injected), Err(Error::NotImplemented));
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
fn gray_53_multi_layer_is_pixel_exact() {
    // 64×64 gray, lossless 5-3, NL = 2, 16×16 code-blocks, single
    // precinct, LRCP with five quality layers. Each code-block's coding
    // passes are distributed across the five layers, so blocks first
    // become included in one layer (§B.10.4) and refine in every later
    // layer they contribute to — the §B.12 walk visits all five layers
    // and the per-code-block contributions accumulate into one §C.3
    // codeword segment. This pins the multi-layer reassembly path under a
    // single precinct.
    let cs = parse_codestream(GRAY_MULTILAYER_53).expect("parse");
    // Confirm the fixture genuinely carries more than one quality layer.
    assert!(
        cs.header.cod.layers >= 2,
        "fixture must define multiple quality layers (got {})",
        cs.header.cod.layers
    );
    assert_eq!(cs.header.cod.progression, ProgressionOrder::Lrcp);
    let img = decode_j2k(GRAY_MULTILAYER_53).expect("decode");
    assert_eq!(img.width, 64);
    assert_eq!(img.height, 64);
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].precision_bits, 8);
    assert!(!img.components[0].is_signed);
    assert_eq!(img.components[0].samples, gray_64x64_pattern());
}

#[test]
fn gray_53_termination_on_each_pass_is_pixel_exact() {
    // 40×40 gray, lossless 5-3, NL = 3, 8×8 code-blocks, COD Table A.19
    // code-block-style bit 2 ("termination on each coding pass", §D.4.2)
    // set. Each coding pass is flushed into its own terminated §C.3
    // codeword segment, so:
    //
    //   * the packet header signals one §B.10.7.1 length per pass
    //     (§B.10.7.2 multi-segment case, K = passes), and
    //   * the tier-1 driver opens a fresh MQ decoder over each pass's
    //     segment (§D.4.1 0xFF-fill synthesised per segment) while the
    //     Annex D contexts persist across the per-pass boundaries.
    //
    // A single-segment driver (concatenating every pass into one MQ run)
    // would desync at the first termination boundary, so pixel-exactness
    // here pins the whole §D.4.2 path.
    let cs = parse_codestream(GRAY_TERMALL_53).expect("parse");
    assert!(
        cs.header
            .cod
            .code_block_style_flags()
            .termination_on_each_coding_pass(),
        "fixture must set the termination-on-each-coding-pass style bit"
    );
    // And it must NOT set the AC-bypass bit (that path is still rejected).
    assert!(
        !cs.header
            .cod
            .code_block_style_flags()
            .selective_arithmetic_coding_bypass(),
        "fixture must not set the selective-arithmetic-coding-bypass bit"
    );
    let img = decode_j2k(GRAY_TERMALL_53).expect("decode termination-on-each-pass");
    assert_eq!(img.width, 40);
    assert_eq!(img.height, 40);
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].precision_bits, 8);
    assert!(!img.components[0].is_signed);
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

/// Multi-component §A.6.2 `COC`: inject a redundant `COC` for
/// component 1 into the 3-component RGB/RCT fixture, restating the
/// `COD`'s per-component coding style. The MCT (`SGcod` MCT = 1, stays
/// in `COD`) and the two un-targeted components are untouched, so the
/// decode stays pixel-exact — proving the per-component coding
/// resolution routes a single-component `COC` correctly while the
/// global progression / MCT path keeps consuming the `COD`.
#[test]
fn rgb_rct_53_redundant_main_header_coc_component1_is_pixel_exact() {
    use oxideav_jpeg2000::{WaveletTransform, MARKER_COC};

    let cs = parse_codestream(RGB_RCT_53).expect("parse");
    let cod = &cs.header.cod;
    let insert_at = cs.header.bytes_consumed;
    assert_eq!(cs.header.siz.components.len(), 3);

    let scoc = u8::from(cod.user_defined_precincts);
    let kernel_byte = match cod.transform {
        WaveletTransform::Irreversible9x7 => 0x00u8,
        WaveletTransform::Reversible5x3 => 0x01u8,
        WaveletTransform::Reserved(b) => b,
    };
    // Csiz = 3 < 257 → Ccoc is 8-bit.
    let lcoc = 2 + 1 + 1 + 5 + cod.precincts.len();
    let mut coc = Vec::new();
    coc.extend_from_slice(&MARKER_COC.to_be_bytes());
    coc.extend_from_slice(&(lcoc as u16).to_be_bytes());
    coc.push(1u8); // Ccoc = component 1
    coc.push(scoc);
    coc.push(cod.decomposition_levels);
    coc.push(cod.code_block_width_exp);
    coc.push(cod.code_block_height_exp);
    coc.push(cod.code_block_style);
    coc.push(kernel_byte);
    coc.extend_from_slice(&cod.precincts);

    let mut injected = Vec::with_capacity(RGB_RCT_53.len() + coc.len());
    injected.extend_from_slice(&RGB_RCT_53[..insert_at]);
    injected.extend_from_slice(&coc);
    injected.extend_from_slice(&RGB_RCT_53[insert_at..]);

    let img = decode_j2k(&injected).expect("decode with main-header COC for component 1");
    assert_eq!(img.components.len(), 3);
    let expected = rgb_16x16_pattern();
    for (c, exp) in img.components.iter().zip(expected.iter()) {
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
    // mid-bit-plane, so per the §E.1.1.2 NOTE Nb(u, v) differs across
    // one code-block: the coefficients the final partial pass reached
    // carry one more decoded magnitude bit than those it did not. The
    // tier-1 decoder now tracks the §D.2.1 per-coefficient decoded-bit
    // count and the §E.1.1.2 reconstruction lifts each coefficient by
    // its own `r · 2^(Mb − Nb(u, v))` midpoint (round 302). With the
    // per-coefficient Nb the truncated decode tracks the black-box
    // reference within the same ±1 floating-point latitude as the
    // full-quality decode — a step down from the max ≤ 16 / mean ≤ 4
    // the per-block-Nb approximation pinned through round 295.
    let (max_diff, mean) = gray_97_deviation(GRAY_97, GRAY_97_REF_PGM);
    assert!(
        max_diff <= 1,
        "truncated 9-7 decode deviates from the reference by {max_diff} (> 1)"
    );
    assert!(
        mean <= 0.05,
        "truncated 9-7 decode mean deviation {mean} (> 0.05)"
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
